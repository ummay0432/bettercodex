//! The local `exec`/`wait` runtime and shared direct-tool adapter.
//!
//! The implementations live outside V8 so a native Responses tool call and a
//! Code Mode nested call execute exactly the same operation.

use super::ProcessManager;
use super::ToolImplementations;
use super::ToolResult;
use super::catalogue;
use super::code_runtime;
use super::code_runtime::CellId;
use super::code_runtime::CodeModeNestedToolCall as NestedToolCall;
use super::code_runtime::CodeModeSessionDelegate as SessionDelegate;
use super::code_runtime::ExecuteRequest;
use super::code_runtime::InProcessCodeModeSession as JavaScriptSession;
use super::code_runtime::NotificationFuture;
use super::code_runtime::RuntimeResponse;
use super::code_runtime::ToolDefinition;
use super::code_runtime::ToolInvocationFuture;
use super::code_runtime::WaitRequest;
use super::image_preparation::prepare_tool_output_images;
use crate::events::AgentEvent;
use crate::openai_docs::OpenAiDocsClient;
use crate::protocol::DEFAULT_IMAGE_DETAIL;
use crate::protocol::FunctionCallOutputContentItem;
use crate::protocol::ImageDetail;
use crate::truncation::TruncationPolicy;
use crate::truncation::formatted_truncate_text;
use crate::truncation::formatted_truncate_text_content_items;
use crate::truncation::truncate_function_output_items;
use crate::web_search::ToolTurnContext;
use crate::web_search::WebSearchClient;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

// Current Codex model metadata applies this policy to direct process output independently of a
// caller's larger request. Code Mode keeps the caller's explicit outer-output budget instead.
const DIRECT_PROCESS_OUTPUT_TOKEN_LIMIT: usize = super::MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS;

pub(crate) struct ToolRuntime {
    session: JavaScriptSession,
    state: Arc<RuntimeState>,
}

impl ToolRuntime {
    pub(crate) fn new(
        cwd: PathBuf,
        web_search: WebSearchClient,
        openai_docs: OpenAiDocsClient,
    ) -> Self {
        let state = Arc::new(RuntimeState {
            tools: ToolImplementations::new(cwd, web_search, openai_docs),
            step_dispatch: StepDispatch::default(),
            notifications: Notifications::default(),
            ui_events: UiEvents::default(),
        });
        let delegate: Arc<dyn SessionDelegate> = state.clone();
        let session = JavaScriptSession::with_delegate(delegate);
        Self { session, state }
    }

    pub(super) fn prepare_turn(&self, context: ToolTurnContext) {
        self.state.tools.prepare_turn(context);
    }

    pub(crate) fn begin_step(
        &self,
        context: ToolTurnContext,
        events: Option<UnboundedSender<AgentEvent>>,
    ) -> StepGuard {
        self.state.tools.prepare_turn(context);
        self.state.step_dispatch.begin(events)
    }

    pub(crate) fn has_notifications(&self) -> Result<bool> {
        self.state.notifications.has_pending()
    }

    pub(crate) fn take_notifications(&self) -> Result<Vec<Value>> {
        self.state.notifications.take()
    }

    pub(super) async fn execute_direct(
        &self,
        call_id: &str,
        definition: &ToolDefinition,
        mut input: Option<Value>,
        truncation_policy: TruncationPolicy,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        let tool_name = display_tool_name(&definition.tool_name);
        if let Some(events) = &events {
            let _ = events.send(AgentEvent::ToolStarted {
                call_id: call_id.to_string(),
                name: tool_name.clone(),
                input: input.clone(),
            });
        }
        apply_direct_process_output_policy(definition, &mut input, truncation_policy);

        let started_at = Instant::now();
        let result = self
            .state
            .tools
            .invoke(&definition.tool_name, definition.kind, input, cancellation)
            .await;
        match result {
            Ok(value) => {
                let direct_result = direct_tool_result(definition, &value, truncation_policy);
                if let Some(events) = events {
                    let output = direct_result
                        .as_ref()
                        .map(|_| direct_event_output(definition, &value))
                        .map_err(|error| bounded_runtime_text(&format!("{error:#}")));
                    let _ = events.send(AgentEvent::ToolCompleted {
                        call_id: call_id.to_string(),
                        output,
                        duration: started_at.elapsed(),
                    });
                }
                direct_result
            }
            Err(error) => {
                if let Some(events) = events {
                    let _ = events.send(AgentEvent::ToolCompleted {
                        call_id: call_id.to_string(),
                        output: Err(bounded_runtime_text(&format!("{error:#}"))),
                        duration: started_at.elapsed(),
                    });
                }
                Err(error)
            }
        }
    }

    pub(crate) fn background_processes(&self) -> ProcessManager {
        self.state.tools.processes.clone()
    }

    pub(crate) fn prewarm(&self) {
        code_runtime::prewarm();
    }

    pub(super) async fn execute(
        &self,
        call_id: &str,
        source: &str,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        let started_at = Instant::now();
        if cancellation.is_cancelled() {
            return Ok(aborted_tool_result(started_at));
        }
        let _implicit_step = self.ensure_step(events.clone())?;
        let parsed = code_runtime::parse_exec_source(source).map_err(|error| anyhow!(error))?;
        let max_output_tokens = resolve_max_tokens(parsed.max_output_tokens);
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(aborted_tool_result(started_at)),
            initialized = code_runtime::ensure_initialized() => {
                initialized.map_err(|error| anyhow!(error))?;
            }
        }
        self.state.ui_events.prepare(events);
        let supports_image_detail_original = self.state.tools.supports_image_detail_original()?;
        let request = ExecuteRequest {
            tool_call_id: call_id.to_string(),
            enabled_tools: catalogue::runtime_tools(supports_image_detail_original).to_vec(),
            source: parsed.code,
            yield_time_ms: parsed.yield_time_ms,
            max_output_tokens: Some(max_output_tokens),
        };
        let started = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                self.state.ui_events.clear_pending();
                return Ok(aborted_tool_result(started_at));
            }
            started = self.session.execute(request) => started.map_err(|error| {
                    self.state.ui_events.clear_pending();
                    anyhow!(error)
                })?,
        };
        let cell_id = started.cell_id.clone();
        self.state.ui_events.bind(&cell_id);
        self.state.step_dispatch.mark_cell_ready(&cell_id);
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(aborted_tool_result(started_at)),
            response = started.initial_response() => response.map_err(|error| anyhow!(error))?,
        };
        self.format_response(response, Some(max_output_tokens), started_at.elapsed())
    }

    pub(super) async fn wait(
        &self,
        input: &str,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        let started_at = Instant::now();
        if cancellation.is_cancelled() {
            return Ok(aborted_tool_result(started_at));
        }
        let _implicit_step = self.ensure_step(events.clone())?;
        let arguments: WaitArgs = serde_json::from_str(input)
            .map_err(|error| anyhow!("failed to parse function arguments: {error}"))?;
        let max_tokens = resolve_max_tokens(arguments.max_tokens);
        let cell_id = CellId::new(arguments.cell_id);
        self.state.ui_events.attach(&cell_id, events);
        let terminate = arguments.terminate.unwrap_or(false);
        let yield_time_ms = arguments
            .yield_time_ms
            .unwrap_or(code_runtime::DEFAULT_WAIT_YIELD_TIME_MS);
        let operation = async {
            if terminate {
                self.session.terminate(cell_id).await
            } else {
                self.session
                    .wait(WaitRequest {
                        cell_id,
                        yield_time_ms,
                    })
                    .await
            }
        };
        tokio::pin!(operation);
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(aborted_tool_result(started_at)),
            response = &mut operation => response.map_err(|error| anyhow!(error))?,
        };
        self.format_response(response.into(), Some(max_tokens), started_at.elapsed())
    }

    fn ensure_step(
        &self,
        events: Option<UnboundedSender<AgentEvent>>,
    ) -> Result<Option<StepGuard>> {
        if self.state.step_dispatch.is_active() {
            return Ok(None);
        }
        let context = self.state.tools.turn_context()?;
        Ok(Some(self.begin_step(context, events)))
    }

    fn format_response(
        &self,
        response: RuntimeResponse,
        max_tokens: Option<usize>,
        wall_time: Duration,
    ) -> Result<ToolResult> {
        let (status, cell_id, items, error, yielded) = match response {
            RuntimeResponse::Yielded {
                cell_id,
                content_items,
            } => (
                format!("Script running with cell ID {cell_id}"),
                cell_id,
                content_items,
                None,
                true,
            ),
            RuntimeResponse::Terminated {
                cell_id,
                content_items,
            } => (
                "Script terminated".to_string(),
                cell_id,
                content_items,
                None,
                false,
            ),
            RuntimeResponse::Result {
                cell_id,
                content_items,
                error_text,
            } => (
                if error_text.is_some() {
                    "Script failed".to_string()
                } else {
                    "Script completed".to_string()
                },
                cell_id,
                content_items,
                error_text,
                false,
            ),
        };

        let mut items = into_protocol_items(items);
        if let Some(error) = error {
            items.push(FunctionCallOutputContentItem::InputText {
                text: format!("Script error:\n{error}"),
            });
        }
        sanitize_original_image_detail(
            self.state.tools.supports_image_detail_original()?,
            &mut items,
        );
        let mut items = truncate_items(items, max_tokens);
        let wall_time_seconds = ((wall_time.as_secs_f32()) * 10.0).round() / 10.0;
        items.insert(
            0,
            FunctionCallOutputContentItem::InputText {
                text: format!("{status}\nWall time {wall_time_seconds:.1} seconds\nOutput:\n"),
            },
        );
        prepare_tool_output_images(&mut items);
        #[cfg(test)]
        let preview = items
            .iter()
            .filter_map(|item| match item {
                FunctionCallOutputContentItem::InputText { text } => Some(text.as_str()),
                FunctionCallOutputContentItem::InputImage { .. }
                | FunctionCallOutputContentItem::EncryptedContent { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let body = output_body(items)?;
        if !yielded {
            self.state.ui_events.close(&cell_id);
        }
        Ok(ToolResult {
            body,
            #[cfg(test)]
            preview,
        })
    }
}

#[derive(Deserialize)]
struct WaitArgs {
    cell_id: String,
    yield_time_ms: Option<u64>,
    max_tokens: Option<usize>,
    terminate: Option<bool>,
}

struct RuntimeState {
    tools: ToolImplementations,
    step_dispatch: StepDispatch,
    notifications: Notifications,
    ui_events: UiEvents,
}

impl SessionDelegate for RuntimeState {
    fn invoke_tool<'a>(
        &'a self,
        invocation: NestedToolCall,
        cancellation_token: CancellationToken,
    ) -> ToolInvocationFuture<'a> {
        Box::pin(async move {
            let step = self
                .step_dispatch
                .wait_until_active(&cancellation_token, "code mode nested tool call cancelled")
                .await?;
            self.step_dispatch
                .wait_until_cell_ready(
                    &invocation.cell_id,
                    &cancellation_token,
                    "code mode nested tool call cancelled",
                )
                .await?;
            let call_id = format!("{}:{}", invocation.cell_id, invocation.runtime_tool_call_id);
            let tool_name = display_tool_name(&invocation.tool_name);
            let sender = step
                .events
                .clone()
                .or_else(|| self.ui_events.sender_for(&invocation.cell_id));
            if let Some(sender) = &sender {
                let _ = sender.send(AgentEvent::ToolStarted {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                    input: invocation.input.clone(),
                });
            }
            let started_at = Instant::now();
            let supports_parallel = nested_tool_supports_parallel_execution(&invocation.tool_name);
            let result = if supports_parallel {
                let _execution = Arc::clone(&step.execution_gate).read_owned().await;
                self.tools
                    .execute_nested(invocation, cancellation_token)
                    .await
            } else {
                let _execution = Arc::clone(&step.execution_gate).write_owned().await;
                self.tools
                    .execute_nested(invocation, cancellation_token)
                    .await
            }
            .map_err(|error| bounded_runtime_text(&format!("{error:#}")));
            if let Some(sender) = sender {
                let output = match &result {
                    Ok(_) if tool_name == "view_image" => Ok(json!({})),
                    Ok(value) => Ok(value.clone()),
                    Err(error) => Err(error.clone()),
                };
                let _ = sender.send(AgentEvent::ToolCompleted {
                    call_id,
                    output,
                    duration: started_at.elapsed(),
                });
            }
            result
        })
    }

    fn notify<'a>(
        &'a self,
        call_id: String,
        cell_id: CellId,
        text: String,
        cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a> {
        Box::pin(async move {
            if cancellation_token.is_cancelled() {
                Err("code mode notification cancelled".to_string())
            } else if text.trim().is_empty() {
                Ok(())
            } else {
                self.step_dispatch
                    .wait_until_active(&cancellation_token, "code mode notification cancelled")
                    .await?;
                self.step_dispatch
                    .wait_until_cell_ready(
                        &cell_id,
                        &cancellation_token,
                        "code mode notification cancelled",
                    )
                    .await?;
                self.notifications.push(call_id, text)
            }
        })
    }

    fn cell_closed(&self, cell_id: &CellId) {
        self.step_dispatch.close_cell(cell_id);
        self.ui_events.close(cell_id);
    }
}

struct StepDispatch {
    active: tokio::sync::watch::Sender<Option<Arc<StepState>>>,
    cell_gates: Mutex<HashMap<CellId, tokio::sync::watch::Sender<bool>>>,
}

struct StepState {
    execution_gate: Arc<tokio::sync::RwLock<()>>,
    events: Option<UnboundedSender<AgentEvent>>,
}

impl Default for StepDispatch {
    fn default() -> Self {
        let (active, _) = tokio::sync::watch::channel(None);
        Self {
            active,
            cell_gates: Mutex::new(HashMap::new()),
        }
    }
}

impl StepDispatch {
    fn begin(&self, events: Option<UnboundedSender<AgentEvent>>) -> StepGuard {
        let state = Arc::new(StepState {
            execution_gate: Arc::new(tokio::sync::RwLock::new(())),
            events,
        });
        self.active.send_replace(Some(Arc::clone(&state)));
        StepGuard {
            active: self.active.clone(),
            state,
        }
    }

    fn is_active(&self) -> bool {
        self.active.borrow().is_some()
    }

    fn mark_cell_ready(&self, cell_id: &CellId) {
        self.cell_gate(cell_id).send_replace(true);
    }

    fn close_cell(&self, cell_id: &CellId) {
        self.cell_gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(cell_id);
    }

    async fn wait_until_active(
        &self,
        cancellation: &CancellationToken,
        cancellation_error: &'static str,
    ) -> std::result::Result<Arc<StepState>, String> {
        let mut active = self.active.subscribe();
        loop {
            if let Some(state) = active.borrow_and_update().clone() {
                return Ok(state);
            }
            tokio::select! {
                changed = active.changed() => {
                    if changed.is_err() {
                        return Err("code mode nested tool dispatcher is unavailable".to_string());
                    }
                }
                _ = cancellation.cancelled() => return Err(cancellation_error.to_string()),
            }
        }
    }

    async fn wait_until_cell_ready(
        &self,
        cell_id: &CellId,
        cancellation: &CancellationToken,
        cancellation_error: &'static str,
    ) -> std::result::Result<(), String> {
        if cancellation.is_cancelled() {
            self.close_cell(cell_id);
            return Err(cancellation_error.to_string());
        }
        let mut ready = self.cell_gate(cell_id).subscribe();
        loop {
            if *ready.borrow_and_update() {
                return Ok(());
            }
            tokio::select! {
                changed = ready.changed() => {
                    if changed.is_err() {
                        return Err("code mode nested tool dispatcher is unavailable".to_string());
                    }
                }
                _ = cancellation.cancelled() => {
                    self.close_cell(cell_id);
                    return Err(cancellation_error.to_string());
                }
            }
        }
    }

    fn cell_gate(&self, cell_id: &CellId) -> tokio::sync::watch::Sender<bool> {
        self.cell_gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(cell_id.clone())
            .or_insert_with(|| tokio::sync::watch::channel(false).0)
            .clone()
    }
}

pub(crate) struct StepGuard {
    active: tokio::sync::watch::Sender<Option<Arc<StepState>>>,
    state: Arc<StepState>,
}

impl Drop for StepGuard {
    fn drop(&mut self) {
        let is_current = self
            .active
            .borrow()
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &self.state));
        if is_current {
            self.active.send_replace(None);
        }
    }
}

fn nested_tool_supports_parallel_execution(tool_name: &crate::protocol::ToolName) -> bool {
    tool_name.namespace.is_some()
        || matches!(
            tool_name.name.as_str(),
            "exec_command" | "view_image" | "write_stdin"
        )
}

fn sanitize_original_image_detail(
    supports_image_detail_original: bool,
    items: &mut [FunctionCallOutputContentItem],
) {
    if supports_image_detail_original {
        return;
    }
    for item in items {
        if let FunctionCallOutputContentItem::InputImage { detail, .. } = item
            && matches!(detail, Some(ImageDetail::Original))
        {
            *detail = Some(DEFAULT_IMAGE_DETAIL);
        }
    }
}

fn display_tool_name(tool_name: &crate::protocol::ToolName) -> String {
    match tool_name.namespace.as_deref() {
        Some(namespace) => format!("{namespace}.{}", tool_name.name),
        None => tool_name.name.clone(),
    }
}

fn apply_direct_process_output_policy(
    definition: &ToolDefinition,
    input: &mut Option<Value>,
    truncation_policy: TruncationPolicy,
) {
    if definition.tool_name.namespace.is_some()
        || !matches!(
            definition.tool_name.name.as_str(),
            "exec_command" | "write_stdin"
        )
    {
        return;
    }
    let Some(Value::Object(arguments)) = input else {
        return;
    };
    let requested = arguments
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .and_then(|tokens| usize::try_from(tokens).ok());
    let model_output_limit = truncation_policy
        .token_budget()
        .min(DIRECT_PROCESS_OUTPUT_TOKEN_LIMIT);
    if requested.is_none_or(|tokens| tokens > model_output_limit) {
        arguments.insert("max_output_tokens".to_string(), json!(model_output_limit));
    }
}

fn direct_tool_result(
    definition: &ToolDefinition,
    value: &Value,
    truncation_policy: TruncationPolicy,
) -> Result<ToolResult> {
    match (
        definition.tool_name.namespace.as_deref(),
        definition.tool_name.name.as_str(),
    ) {
        (None, "exec_command" | "write_stdin") => Ok(ToolResult::untruncated_text(
            format_direct_process_output(value)?,
        )),
        (None, "update_plan") => Ok(ToolResult::text("Plan updated".to_string())),
        (None, "view_image") => direct_image_result(value),
        (Some(crate::web_search::NAMESPACE), crate::web_search::TOOL_NAME) => {
            direct_web_search_result(value)
        }
        _ => {
            let text = match value {
                Value::String(text) => text.clone(),
                value => serde_json::to_string(value)?,
            };
            Ok(ToolResult::text_with_policy(text, truncation_policy))
        }
    }
}

fn direct_web_search_result(value: &Value) -> Result<ToolResult> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("web.run returned a non-text result"))?
        .to_string();
    #[cfg(test)]
    let preview = text.clone();
    Ok(ToolResult {
        body: json!([{
            "type": "input_text",
            "text": text,
        }]),
        #[cfg(test)]
        preview,
    })
}

fn format_direct_process_output(value: &Value) -> Result<String> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("unified exec returned a non-object result"))?;
    let mut sections = Vec::new();
    if let Some(chunk_id) = object.get("chunk_id").and_then(Value::as_str)
        && !chunk_id.is_empty()
    {
        sections.push(format!("Chunk ID: {chunk_id}"));
    }
    let wall_time_seconds = object
        .get("wall_time_seconds")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("unified exec result omitted wall_time_seconds"))?;
    sections.push(format!("Wall time: {wall_time_seconds:.4} seconds"));
    if let Some(exit_code) = object.get("exit_code").and_then(Value::as_i64) {
        sections.push(format!("Process exited with code {exit_code}"));
    }
    if let Some(session_id) = object.get("session_id") {
        let session_id = session_id
            .as_i64()
            .map(|session_id| session_id.to_string())
            .or_else(|| session_id.as_str().map(str::to_string))
            .ok_or_else(|| anyhow!("unified exec returned an invalid session_id"))?;
        sections.push(format!("Process running with session ID {session_id}"));
    }
    if let Some(original_token_count) = object.get("original_token_count").and_then(Value::as_u64) {
        sections.push(format!("Original token count: {original_token_count}"));
    }
    let output = object
        .get("output")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("unified exec result omitted output"))?;
    sections.push("Output:".to_string());
    sections.push(output.to_string());
    Ok(sections.join("\n"))
}

fn direct_image_result(value: &Value) -> Result<ToolResult> {
    let image_url = value
        .get("image_url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("view_image result omitted image_url"))?
        .to_string();
    let detail = match value.get("detail").and_then(Value::as_str) {
        None | Some("high") => ImageDetail::High,
        Some("original") => ImageDetail::Original,
        Some(detail) => return Err(anyhow!("view_image returned unsupported detail `{detail}`")),
    };
    let mut items = vec![FunctionCallOutputContentItem::InputImage {
        image_url,
        detail: Some(detail),
    }];
    prepare_tool_output_images(&mut items);
    let body = output_body(items)?;
    Ok(ToolResult {
        body,
        #[cfg(test)]
        preview: "<image>".to_string(),
    })
}

fn direct_event_output(definition: &ToolDefinition, value: &Value) -> Value {
    if definition.tool_name.namespace.is_none() && definition.tool_name.name == "view_image" {
        json!({})
    } else {
        value.clone()
    }
}

#[derive(Default)]
struct UiEvents {
    state: Mutex<UiEventState>,
}

#[derive(Default)]
struct UiEventState {
    pending: Option<UnboundedSender<AgentEvent>>,
    by_cell: HashMap<String, UnboundedSender<AgentEvent>>,
}

impl UiEvents {
    fn prepare(&self, events: Option<UnboundedSender<AgentEvent>>) {
        if let Ok(mut state) = self.state.lock() {
            state.pending = events;
        }
    }

    fn bind(&self, cell_id: &CellId) {
        if let Ok(mut state) = self.state.lock() {
            let key = cell_id.to_string();
            if !state.by_cell.contains_key(&key)
                && let Some(events) = state.pending.clone()
            {
                state.by_cell.insert(key, events);
            }
            state.pending = None;
        }
    }

    fn attach(&self, cell_id: &CellId, events: Option<UnboundedSender<AgentEvent>>) {
        if let (Some(events), Ok(mut state)) = (events, self.state.lock()) {
            state.by_cell.insert(cell_id.to_string(), events);
        }
    }

    fn sender_for(&self, cell_id: &CellId) -> Option<UnboundedSender<AgentEvent>> {
        let mut state = self.state.lock().ok()?;
        let key = cell_id.to_string();
        if let Some(events) = state.by_cell.get(&key) {
            return Some(events.clone());
        }
        let events = state.pending.clone()?;
        state.by_cell.insert(key, events.clone());
        Some(events)
    }

    fn close(&self, cell_id: &CellId) {
        if let Ok(mut state) = self.state.lock() {
            state.by_cell.remove(&cell_id.to_string());
        }
    }

    fn clear_pending(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.pending = None;
        }
    }
}

#[derive(Default)]
struct Notifications {
    pending: Mutex<VecDeque<Notification>>,
}

struct Notification {
    call_id: String,
    text: String,
}

impl Notifications {
    fn push(&self, call_id: String, text: String) -> std::result::Result<(), String> {
        self.pending
            .lock()
            .map_err(|_| "notification lock was poisoned".to_string())?
            .push_back(Notification { call_id, text });
        Ok(())
    }

    fn has_pending(&self) -> Result<bool> {
        Ok(!self
            .pending
            .lock()
            .map_err(|_| anyhow!("notification lock was poisoned"))?
            .is_empty())
    }

    fn take(&self) -> Result<Vec<Value>> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| anyhow!("notification lock was poisoned"))?;
        Ok(pending
            .drain(..)
            .map(|notification| {
                json!({
                    "type": "custom_tool_call_output",
                    "call_id": notification.call_id,
                    "name": "exec",
                    "output": notification.text,
                })
            })
            .collect())
    }
}

fn truncate_items(
    items: Vec<FunctionCallOutputContentItem>,
    max_tokens: Option<usize>,
) -> Vec<FunctionCallOutputContentItem> {
    let max_tokens = resolve_max_tokens(max_tokens);
    if items
        .iter()
        .all(|item| matches!(item, FunctionCallOutputContentItem::InputText { .. }))
    {
        return formatted_truncate_text_content_items(&items, max_tokens).0;
    }

    truncate_function_output_items(&items, max_tokens)
}

fn resolve_max_tokens(requested: Option<usize>) -> usize {
    requested.unwrap_or(super::MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS)
}

fn bounded_runtime_text(text: &str) -> String {
    formatted_truncate_text(text, super::MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS)
}

fn aborted_tool_result(started_at: Instant) -> ToolResult {
    let seconds = started_at.elapsed().as_secs_f32().max(0.1);
    ToolResult::text(format!("aborted by user after {seconds:.1}s"))
}

fn output_body(items: Vec<FunctionCallOutputContentItem>) -> Result<Value> {
    if let [FunctionCallOutputContentItem::InputText { text }] = items.as_slice() {
        return Ok(Value::String(text.clone()));
    }
    Ok(Value::Array(
        items
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?,
    ))
}

fn into_protocol_items(
    items: Vec<code_runtime::FunctionCallOutputContentItem>,
) -> Vec<FunctionCallOutputContentItem> {
    items
        .into_iter()
        .map(|item| match item {
            code_runtime::FunctionCallOutputContentItem::InputText { text } => {
                FunctionCallOutputContentItem::InputText { text }
            }
            code_runtime::FunctionCallOutputContentItem::InputImage { image_url, detail } => {
                FunctionCallOutputContentItem::InputImage {
                    image_url,
                    detail: Some(match detail {
                        Some(code_runtime::ImageDetail::Auto) => ImageDetail::Auto,
                        Some(code_runtime::ImageDetail::Low) => ImageDetail::Low,
                        Some(code_runtime::ImageDetail::High) => ImageDetail::High,
                        Some(code_runtime::ImageDetail::Original) => ImageDetail::Original,
                        None => DEFAULT_IMAGE_DETAIL,
                    }),
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::ops::Deref;
    use std::path::PathBuf;

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("bettercodex-exec-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Deref for TemporaryDirectory {
        type Target = PathBuf;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn runtime(cwd: PathBuf) -> ToolRuntime {
        let http_client = || {
            crate::http_client::build_client(reqwest::Client::builder())
                .expect("build test HTTP client")
        };
        ToolRuntime::new(
            cwd,
            crate::web_search::WebSearchClient::new(
                http_client(),
                crate::auth::SharedAuth::new(crate::auth::Auth::for_test("test-token")),
                "http://127.0.0.1:1".to_string(),
                "test-session".to_string(),
                crate::model::SharedModelSelection::new(crate::model::ModelSelection::default()),
            ),
            crate::openai_docs::OpenAiDocsClient::with_endpoint(
                http_client(),
                "http://127.0.0.1:1",
            ),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn composes_parallel_nested_calls_in_v8() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-1",
                r#"
const [left, right] = await Promise.all([
  tools.exec_command({cmd: "printf left"}),
  tools.exec_command({cmd: "printf right"}),
]);
text(`${left.output}:${right.output}`);
"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.preview.contains("left:right"), "{}", result.preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn direct_function_calls_bypass_javascript_and_emit_native_tool_events() {
        let runtime = runtime(PathBuf::from("."));
        let command = if cfg!(windows) {
            "Write-Output -NoNewline direct"
        } else {
            "printf direct"
        };
        let call = crate::tools::ToolCall::Function {
            call_id: "call-direct".to_string(),
            namespace: None,
            name: "exec_command".to_string(),
            arguments: json!({"cmd": command, "login": false}).to_string(),
        };
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();

        let result = call
            .execute(
                &runtime,
                crate::model::ToolMode::Direct,
                ToolTurnContext::default(),
                Some(events),
                CancellationToken::new(),
            )
            .await;
        assert!(result.preview.contains("Chunk ID: "));
        assert!(result.preview.contains("Wall time: "));
        assert!(result.preview.contains("Process exited with code 0"));
        assert!(result.preview.contains("Original token count: "));
        assert!(result.preview.ends_with("Output:\ndirect"));
        assert!(matches!(
            received.recv().await.unwrap(),
            AgentEvent::ToolStarted { call_id, name, input: Some(input) }
                if call_id == "call-direct"
                    && name == "exec_command"
                    && input["cmd"] == command
                    && input.get("max_output_tokens").is_none()
        ));
        assert!(matches!(
            received.recv().await.unwrap(),
            AgentEvent::ToolCompleted { call_id, output: Ok(output), .. }
                if call_id == "call-direct" && output["output"] == "direct"
        ));
        assert!(received.try_recv().is_err());

        let output = result.preview.clone();
        assert_eq!(
            call.into_output_items(result),
            vec![json!({
                "type": "function_call_output",
                "call_id": "call-direct",
                "output": output,
            })]
        );

        let long_command = if cfg!(windows) {
            "Write-Output -NoNewline ('x' * 20000)"
        } else {
            "yes x | tr -d '\\n' | head -c 20000"
        };
        let byte_limited = crate::tools::ToolCall::Function {
            call_id: "call-byte-limited".to_string(),
            namespace: None,
            name: "exec_command".to_string(),
            arguments: json!({"cmd": long_command, "login": false}).to_string(),
        }
        .execute(
            &runtime,
            crate::model::ToolMode::Direct,
            ToolTurnContext::with_truncation_policy(TruncationPolicy::Bytes(10_000)),
            None,
            CancellationToken::new(),
        )
        .await;
        assert!(byte_limited.preview.contains("Warning: truncated output"));
        assert!(
            byte_limited
                .preview
                .chars()
                .filter(|character| *character == 'x')
                .count()
                < 20_000
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn direct_mode_rejects_unadvertised_code_mode_entrypoints() {
        let runtime = runtime(PathBuf::from("."));
        let exec = crate::tools::ToolCall::Custom {
            call_id: "call-unadvertised-exec".to_string(),
            namespace: None,
            name: "exec".to_string(),
            input: "text('must not run')".to_string(),
        }
        .execute(
            &runtime,
            crate::model::ToolMode::Direct,
            ToolTurnContext::default(),
            None,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(exec.preview, "unknown top-level tool `exec`");

        let wait = crate::tools::ToolCall::Function {
            call_id: "call-unadvertised-wait".to_string(),
            namespace: None,
            name: "wait".to_string(),
            arguments: json!({"cell_id": "missing"}).to_string(),
        }
        .execute(
            &runtime,
            crate::model::ToolMode::Direct,
            ToolTurnContext::default(),
            None,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(wait.preview, "unknown top-level tool `wait`");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn direct_mode_dispatches_canonical_namespaces_without_flat_aliases() {
        let runtime = runtime(PathBuf::from("."));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let canonical = crate::tools::ToolCall::Function {
            call_id: "call-web-run".to_string(),
            namespace: Some(crate::web_search::NAMESPACE.to_string()),
            name: crate::web_search::TOOL_NAME.to_string(),
            arguments: json!({}).to_string(),
        }
        .execute(
            &runtime,
            crate::model::ToolMode::Direct,
            ToolTurnContext::default(),
            None,
            cancellation,
        )
        .await;
        assert_eq!(canonical.preview, "web search cancelled");

        let flattened_alias = crate::tools::ToolCall::Function {
            call_id: "call-flattened-web-run".to_string(),
            namespace: None,
            name: crate::web_search::JAVASCRIPT_NAME.to_string(),
            arguments: json!({}).to_string(),
        }
        .execute(
            &runtime,
            crate::model::ToolMode::Direct,
            ToolTurnContext::default(),
            None,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(flattened_alias.preview, "unknown top-level tool `web__run`");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nested_exec_preserves_data_until_javascript_emits_it() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-raw-output",
                r#"
const command = "yes x | tr -d '\\n' | head -c 50000";
const raw = await tools.exec_command({cmd: command, login: false, shell: "/bin/sh"});
const explicit = await tools.exec_command({
  cmd: command,
  login: false,
  shell: "/bin/sh",
  max_output_tokens: 20000,
});
const limited = await tools.exec_command({
  cmd: command,
  login: false,
  shell: "/bin/sh",
  max_output_tokens: 10,
});
text(JSON.stringify({
  raw: raw.output.length,
  explicit: explicit.output.length,
  limited: limited.output.startsWith("Warning: truncated output"),
}));
"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(
            result
                .preview
                .contains(r#"{"raw":50000,"explicit":50000,"limited":true}"#),
            "{}",
            result.preview
        );

        let expanded_outer_budget = runtime
            .execute(
                "call-expanded-output",
                "// @exec: {\"max_output_tokens\": 20000}\ntext('x'.repeat(50000));",
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!expanded_outer_budget.preview.contains("truncated output"));
        assert_eq!(
            expanded_outer_budget
                .preview
                .chars()
                .filter(|character| *character == 'x')
                .count(),
            50_000
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nested_calls_emit_codex_tui_events_instead_of_outer_exec_events() {
        let runtime = runtime(PathBuf::from("."));
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        runtime.execute(
            "call-events",
            r#"const result = await tools.exec_command({cmd: "printf ready"}); text(result.output);"#,
            Some(events_tx),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let started = events_rx.recv().await.unwrap();
        let completed = events_rx.recv().await.unwrap();
        assert!(matches!(
            started,
            AgentEvent::ToolStarted { name, input: Some(input), .. }
                if name == "exec_command" && input["cmd"] == "printf ready"
        ));
        assert!(matches!(
            completed,
            AgentEvent::ToolCompleted { output: Ok(output), .. }
                if output["output"] == "ready"
        ));
        assert!(events_rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn patch_and_plan_tools_share_implementations_across_routes() {
        let cwd = TemporaryDirectory::new("patch-plan");
        let runtime = runtime(cwd.to_path_buf());
        let result = runtime
            .execute(
                "call-patch-plan",
                r#"
const patch = await tools.apply_patch(`*** Begin Patch
*** Add File: made.txt
+hello
*** End Patch`);
const plan = await tools.update_plan({plan: [
  {step: "port tools", status: "completed"},
]});
text(`${JSON.stringify(patch)}:${JSON.stringify(plan)}`);
"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("made.txt")).unwrap(),
            "hello\n"
        );
        assert!(result.preview.contains("{}:{}"), "{}", result.preview);

        let direct = crate::tools::ToolCall::Custom {
            call_id: "call-direct-patch".to_string(),
            namespace: None,
            name: "apply_patch".to_string(),
            input: "*** Begin Patch\n*** Update File: made.txt\n@@\n-hello\n+direct\n*** End Patch"
                .to_string(),
        }
        .execute(
            &runtime,
            crate::model::ToolMode::Direct,
            ToolTurnContext::default(),
            None,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(cwd.join("made.txt")).unwrap(),
            "direct\n"
        );
        assert!(
            direct.preview.contains("Success. Updated"),
            "{}",
            direct.preview
        );

        let direct_plan = crate::tools::ToolCall::Function {
            call_id: "call-direct-plan".to_string(),
            namespace: None,
            name: "update_plan".to_string(),
            arguments: json!({
                "plan": [{"step": "Verify direct tools", "status": "completed"}]
            })
            .to_string(),
        }
        .execute(
            &runtime,
            crate::model::ToolMode::Direct,
            ToolTurnContext::default(),
            None,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(direct_plan.preview, "Plan updated");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn view_image_dispatches_structured_image_content() {
        let cwd = TemporaryDirectory::new("view-image");
        let image_path = cwd.join("sample.png");
        let image = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap();
        std::fs::write(&image_path, image).unwrap();
        let runtime = runtime(cwd.to_path_buf());
        let result = runtime
            .execute(
                "call-image",
                r#"const result = await tools.view_image({path: "sample.png", detail: "original"}); image(result);"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let items = result.body.as_array().unwrap();
        assert!(items.iter().any(|item| {
            item["type"] == "input_image"
                && item["detail"] == "original"
                && item["image_url"]
                    .as_str()
                    .is_some_and(|url| url.starts_with("data:image/png;base64,"))
        }));

        let direct = crate::tools::ToolCall::Function {
            call_id: "call-direct-image".to_string(),
            namespace: None,
            name: "view_image".to_string(),
            arguments: json!({"path": "sample.png", "detail": "original"}).to_string(),
        }
        .execute(
            &runtime,
            crate::model::ToolMode::Direct,
            ToolTurnContext::default(),
            None,
            CancellationToken::new(),
        )
        .await;
        let direct_items = direct.body.as_array().unwrap();
        assert!(direct_items.iter().any(|item| {
            item["type"] == "input_image"
                && item["detail"] == "original"
                && item["image_url"]
                    .as_str()
                    .is_some_and(|url| url.starts_with("data:image/png;base64,"))
        }));

        let unsupported = crate::tools::ToolCall::Function {
            call_id: "call-sanitized-image".to_string(),
            namespace: None,
            name: "view_image".to_string(),
            // Keep accepting a hint saved or generated against an older schema, but sanitize it
            // exactly as upstream does when the selected model lacks original-detail support.
            arguments: json!({"path": "sample.png", "detail": "original"}).to_string(),
        }
        .execute(
            &runtime,
            crate::model::ToolMode::Direct,
            ToolTurnContext::without_original_image_detail(),
            None,
            CancellationToken::new(),
        )
        .await;
        let unsupported_items = unsupported.body.as_array().unwrap();
        assert!(
            unsupported_items
                .iter()
                .any(|item| { item["type"] == "input_image" && item["detail"] == "high" })
        );

        let nested_unsupported = runtime
            .execute(
                "call-nested-sanitized-image",
                r#"
const view = ALL_TOOLS.find(({ name }) => name === "view_image");
text(String(view.description.includes('detail?: "high" | "original";')));
const result = await tools.view_image({path: "sample.png", detail: "original"});
image(result.image_url, "original");
"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(nested_unsupported.preview.contains("false"));
        let nested_unsupported_items = nested_unsupported.body.as_array().unwrap();
        assert!(
            nested_unsupported_items
                .iter()
                .any(|item| { item["type"] == "input_image" && item["detail"] == "high" })
        );
        assert!(
            !nested_unsupported_items
                .iter()
                .any(|item| { item["type"] == "input_image" && item["detail"] == "original" })
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn javascript_has_no_node_or_console_globals() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-1",
                "text(`${typeof process}:${typeof require}:${typeof console}`);",
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            result.preview.contains("undefined:undefined:undefined"),
            "{}",
            result.preview
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runtime_exposes_the_fixed_catalogue() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-tools",
                r#"
const view = ALL_TOOLS.find(({ name }) => name === "view_image");
const web = ALL_TOOLS.find(({ name }) => name === "web__run");
text(JSON.stringify({
  names: ALL_TOOLS.map(tool => tool.name).join(','),
  viewDeclaration: view.description.includes("declare const tools: { view_image(args:"),
  viewOutput: view.description.includes("image_url: string;"),
  webNamespace: web.description.startsWith("Tools in the web namespace.\n\n"),
  webDeclaration: web.description.includes("declare const tools: { web__run(args:"),
  webOutput: web.description.includes("): Promise<unknown>; };"),
}));
"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            result.preview.contains(
                r#"{"names":"apply_patch,exec_command,log_papercut,openaiDeveloperDocs__fetch_openai_doc,openaiDeveloperDocs__get_openapi_spec,openaiDeveloperDocs__list_api_endpoints,openaiDeveloperDocs__list_openai_docs,openaiDeveloperDocs__search_openai_docs,update_plan,view_image,web__run,write_stdin","viewDeclaration":true,"viewOutput":true,"webNamespace":true,"webDeclaration":true,"webOutput":true}"#
            ),
            "{}",
            result.preview
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clear_timeout_cancels_the_pending_callback() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-clear-timeout",
                r#"
let fired = false;
const cancelled = setTimeout(() => { fired = true; }, 0);
clearTimeout(cancelled);
await new Promise(resolve => setTimeout(resolve, 20));
text(String(fired));
"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.preview.contains("false"), "{}", result.preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yielded_cells_resume_with_wait() {
        let runtime = runtime(PathBuf::from("."));
        let yielded = runtime
            .execute(
                "call-yield",
                r#"
text("before");
yield_control();
await new Promise(resolve => setTimeout(resolve, 20));
text("after");
"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let first_line = yielded.preview.lines().next().unwrap();
        let cell_id = first_line
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        assert!(yielded.preview.contains("before"), "{}", yielded.preview);

        let completed = runtime
            .wait(
                &json!({"cell_id": cell_id, "yield_time_ms": 1000}).to_string(),
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            completed.preview.starts_with("Script completed"),
            "{}",
            completed.preview
        );
        assert!(completed.preview.contains("after"), "{}", completed.preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_can_terminate_a_yielded_cell() {
        let runtime = runtime(PathBuf::from("."));
        let yielded = runtime
            .execute(
                "call-terminate",
                "yield_control(); await new Promise(resolve => setTimeout(resolve, 5000));",
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let cell_id = yielded
            .preview
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        let terminated = runtime
            .wait(
                &json!({"cell_id": cell_id, "terminate": true}).to_string(),
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            terminated.preview.starts_with("Script terminated"),
            "{}",
            terminated.preview
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_wait_leaves_the_yielded_cell_running() {
        let runtime = runtime(PathBuf::from("."));
        let yielded = runtime
            .execute(
                "call-cancelled-wait",
                "yield_control(); await new Promise(resolve => setTimeout(resolve, 20)); text('done');",
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let cell_id = yielded
            .preview
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let aborted = runtime
            .wait(
                &json!({"cell_id": cell_id, "yield_time_ms": 1000}).to_string(),
                None,
                cancellation,
            )
            .await
            .unwrap();
        assert_eq!(aborted.preview, "aborted by user after 0.1s");

        let completed = runtime
            .wait(
                &json!({"cell_id": cell_id, "yield_time_ms": 1000}).to_string(),
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            completed.preview.starts_with("Script completed"),
            "{}",
            completed.preview
        );
        assert!(completed.preview.contains("done"), "{}", completed.preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stored_values_survive_between_cells() {
        let runtime = runtime(PathBuf::from("."));
        runtime
            .execute(
                "call-store",
                r#"store("answer", {value: 42});"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let loaded = runtime
            .execute(
                "call-load",
                r#"text(load("answer").value);"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(loaded.preview.contains("42"), "{}", loaded.preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stored_writes_are_visible_within_the_same_cell() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-store-load",
                r#"store("answer", {value: 42}); text(load("answer").value);"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.preview.contains("42"), "{}", result.preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_cells_keep_their_starting_store_snapshot() {
        let runtime = runtime(PathBuf::from("."));
        runtime
            .execute(
                "call-seed",
                r#"store("answer", {value: 1});"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let reader = runtime.execute(
            "call-snapshot-reader",
            r#"await new Promise(resolve => setTimeout(resolve, 200)); text(load("answer").value);"#,
            None,
            CancellationToken::new(),
        );
        let writer = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            runtime
                .execute(
                    "call-concurrent-writer",
                    r#"store("answer", {value: 2});"#,
                    None,
                    CancellationToken::new(),
                )
                .await
                .unwrap();
        };
        let (reader, ()) = tokio::join!(reader, writer);
        let reader = reader.unwrap();

        assert!(reader.preview.contains("1"), "{}", reader.preview);
        let latest = runtime
            .execute(
                "call-latest-reader",
                r#"text(load("answer").value);"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(latest.preview.contains("2"), "{}", latest.preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn notify_queues_an_additional_output_item() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-notify",
                r#"notify({stage: "ready"}); text("done");"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            runtime.take_notifications().unwrap(),
            vec![json!({
                "type": "custom_tool_call_output",
                "call_id": "call-notify",
                "name": "exec",
                "output": "{\"stage\":\"ready\"}",
            })]
        );
        assert!(result.preview.contains("done"), "{}", result.preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn notify_after_yield_is_dispatched_by_the_next_wait_step() {
        let runtime = runtime(PathBuf::from("."));
        let yielded = runtime
            .execute(
                "call-delayed-notify",
                r#"yield_control(); await new Promise(resolve => setTimeout(resolve, 20)); notify("later"); text("done");"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let cell_id = yielded
            .preview
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!runtime.has_notifications().unwrap());

        let completed = runtime
            .wait(
                &json!({"cell_id": cell_id, "yield_time_ms": 1000}).to_string(),
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(completed.preview.contains("done"), "{}", completed.preview);
        assert_eq!(
            runtime.take_notifications().unwrap(),
            vec![json!({
                "type": "custom_tool_call_output",
                "call_id": "call-delayed-notify",
                "name": "exec",
                "output": "later",
            })]
        );
    }

    #[test]
    fn all_text_truncation_matches_codex_head_tail_format() {
        let items = vec![FunctionCallOutputContentItem::InputText {
            text: "0123456789012345678901234567890123456789".to_string(),
        }];
        assert_eq!(
            truncate_items(items, Some(5)),
            vec![FunctionCallOutputContentItem::InputText {
                text: concat!(
                    "Warning: truncated output (original token count: 10)\n",
                    "Total output lines: 1\n\n",
                    "0123456789…5 tokens truncated…0123456789"
                )
                .to_string(),
            }]
        );
    }

    #[test]
    fn truncation_preserves_text_boundaries_and_images() {
        let items = vec![
            FunctionCallOutputContentItem::InputText {
                text: "x".repeat(100),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,AA==".to_string(),
                detail: None,
            },
        ];
        let truncated = truncate_items(items, Some(5));
        assert!(matches!(
            &truncated[0],
            FunctionCallOutputContentItem::InputText { text } if text == "xxxxxxxxxx…20 tokens truncated…xxxxxxxxxx"
        ));
        assert!(matches!(
            &truncated[1],
            FunctionCallOutputContentItem::InputImage { .. }
        ));
    }
}
