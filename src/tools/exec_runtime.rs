//! The fixed `exec`/`wait` runtime used by every bettercodex turn.
//!
//! Upstream Codex calls this mechanism Code Mode because its tool planner can
//! choose other modes. bettercodex has no planner or selector: this module is
//! the only top-level tool execution path.

use super::NestedTools;
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
use super::code_runtime::ToolInvocationFuture;
use super::code_runtime::WaitRequest;
use super::image_preparation::prepare_tool_output_images;
use crate::events::AgentEvent;
use crate::web_search::ToolTurnContext;
use crate::web_search::WebSearchClient;
use anyhow::Result;
use anyhow::anyhow;
use codex_protocol::models::DEFAULT_IMAGE_DETAIL;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ImageDetail;
use codex_utils_audio::estimate_audio_token_count;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::formatted_truncate_text;
use codex_utils_output_truncation::formatted_truncate_text_content_items_with_policy;
use codex_utils_output_truncation::truncate_function_output_items_with_policy;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

pub(crate) struct ToolRuntime {
    session: JavaScriptSession,
    state: Arc<RuntimeState>,
}

impl ToolRuntime {
    pub(crate) fn new(cwd: PathBuf, web_search: WebSearchClient) -> Self {
        let state = Arc::new(RuntimeState {
            tools: NestedTools::with_web_search(cwd, web_search),
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

    pub(super) async fn execute(
        &self,
        call_id: &str,
        source: &str,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        let parsed = code_runtime::parse_exec_source(source).map_err(|error| anyhow!(error))?;
        let max_output_tokens = output_token_budget(parsed.max_output_tokens);
        let started_at = Instant::now();
        self.state.ui_events.prepare(events);
        let started = self
            .session
            .execute(ExecuteRequest {
                tool_call_id: call_id.to_string(),
                enabled_tools: catalogue::runtime_tools().to_vec(),
                source: parsed.code,
                yield_time_ms: parsed.yield_time_ms,
                max_output_tokens: Some(max_output_tokens),
            })
            .await
            .map_err(|error| {
                self.state.ui_events.clear_pending();
                anyhow!(error)
            })?;
        let cell_id = started.cell_id.clone();
        self.state.ui_events.bind(&cell_id);
        let response = tokio::select! {
            response = started.initial_response() => response.map_err(|error| anyhow!(error))?,
            _ = cancellation.cancelled() => {
                self.session
                    .terminate(cell_id)
                    .await
                    .map_err(|error| anyhow!(error))?
                    .into()
            }
        };
        self.format_response(response, Some(max_output_tokens), started_at.elapsed())
    }

    pub(super) async fn wait(
        &self,
        input: &str,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        let arguments: WaitArgs = serde_json::from_str(input)
            .map_err(|error| anyhow!("failed to parse function arguments: {error}"))?;
        let max_tokens = output_token_budget(arguments.max_tokens);
        let cell_id = CellId::new(arguments.cell_id);
        self.state.ui_events.attach(&cell_id, events);
        let started_at = Instant::now();
        let response = if arguments.terminate.unwrap_or(false) {
            self.session
                .terminate(cell_id)
                .await
                .map_err(|error| anyhow!(error))?
        } else {
            let yield_time_ms = arguments
                .yield_time_ms
                .unwrap_or(code_runtime::DEFAULT_WAIT_YIELD_TIME_MS);
            tokio::select! {
                response = self.session.wait(WaitRequest { cell_id: cell_id.clone(), yield_time_ms }) => {
                    response.map_err(|error| anyhow!(error))?
                }
                _ = cancellation.cancelled() => {
                    self.session
                        .terminate(cell_id)
                        .await
                        .map_err(|error| anyhow!(error))?
                }
            }
        };
        self.format_response(response.into(), Some(max_tokens), started_at.elapsed())
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

        let notifications = self.state.notifications.take(&cell_id)?;
        let mut output_items = Vec::new();
        let mut items = into_protocol_items(items);
        if let Some(error) = error {
            items.push(FunctionCallOutputContentItem::InputText {
                text: format!("Script error:\n{error}"),
            });
        }
        let mut items = truncate_items(items, max_tokens);
        let wall_time_seconds = ((wall_time.as_secs_f32()) * 10.0).round() / 10.0;
        items.insert(
            0,
            FunctionCallOutputContentItem::InputText {
                text: format!("{status}\nWall time {wall_time_seconds:.1} seconds\nOutput:\n"),
            },
        );
        prepare_tool_output_images(&mut items);
        for notification in notifications {
            output_items.push(json!({
                "type": "custom_tool_call_output",
                "call_id": notification.call_id,
                "name": "exec",
                "output": notification.text,
            }));
        }
        let preview = items
            .iter()
            .filter_map(|item| match item {
                FunctionCallOutputContentItem::InputText { text } => Some(text.as_str()),
                FunctionCallOutputContentItem::InputImage { .. }
                | FunctionCallOutputContentItem::InputAudio { .. }
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
            preview,
            preceding_items: output_items,
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
    tools: NestedTools,
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
            let call_id = format!("{}:{}", invocation.cell_id, invocation.runtime_tool_call_id);
            let tool_name = display_tool_name(&invocation.tool_name);
            let sender = self.ui_events.sender_for(&invocation.cell_id);
            if let Some(sender) = &sender {
                let _ = sender.send(AgentEvent::ToolStarted {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                    input: invocation.input.clone(),
                });
            }
            let started_at = Instant::now();
            let result = self
                .tools
                .execute_nested(invocation, cancellation_token)
                .await
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
        _cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a> {
        Box::pin(async move { self.notifications.push(cell_id, call_id, text) })
    }

    fn cell_closed(&self, cell_id: &CellId) {
        // The observer drains notifications after it receives the terminal event.
        self.ui_events.close(cell_id);
    }
}

fn display_tool_name(tool_name: &codex_protocol::ToolName) -> String {
    match tool_name.namespace.as_deref() {
        Some(namespace) => format!("{namespace}.{}", tool_name.name),
        None => tool_name.name.clone(),
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
    by_cell: Mutex<HashMap<CellId, Vec<Notification>>>,
}

struct Notification {
    call_id: String,
    text: String,
}

impl Notifications {
    fn push(
        &self,
        cell_id: CellId,
        call_id: String,
        text: String,
    ) -> std::result::Result<(), String> {
        let text = bounded_runtime_text(&text);
        self.by_cell
            .lock()
            .map_err(|_| "notification lock was poisoned".to_string())?
            .entry(cell_id)
            .or_default()
            .push(Notification { call_id, text });
        Ok(())
    }

    fn take(&self, cell_id: &CellId) -> Result<Vec<Notification>> {
        Ok(self
            .by_cell
            .lock()
            .map_err(|_| anyhow!("notification lock was poisoned"))?
            .remove(cell_id)
            .unwrap_or_default())
    }
}

fn truncate_items(
    items: Vec<FunctionCallOutputContentItem>,
    max_tokens: Option<usize>,
) -> Vec<FunctionCallOutputContentItem> {
    let policy = TruncationPolicy::Tokens(output_token_budget(max_tokens));
    if items
        .iter()
        .all(|item| matches!(item, FunctionCallOutputContentItem::InputText { .. }))
    {
        return formatted_truncate_text_content_items_with_policy(&items, policy).0;
    }

    truncate_function_output_items_with_policy(&items, policy, estimate_audio_token_count)
}

fn output_token_budget(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(super::MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS)
        .min(super::MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS)
}

fn bounded_runtime_text(text: &str) -> String {
    formatted_truncate_text(
        text,
        TruncationPolicy::Tokens(super::MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
    )
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
            code_runtime::FunctionCallOutputContentItem::InputAudio { audio_url } => {
                FunctionCallOutputContentItem::InputAudio { audio_url }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("bettercodex-exec-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn runtime(cwd: PathBuf) -> ToolRuntime {
        ToolRuntime::new(
            cwd,
            crate::web_search::WebSearchClient::new(
                reqwest::Client::new(),
                crate::auth::SharedAuth::new(crate::auth::Auth::for_test("test-token")),
                "http://127.0.0.1:1".to_string(),
                "test-session".to_string(),
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
    async fn nested_command_default_is_bounded_before_javascript_receives_it() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-bounded-command",
                r#"
const result = await tools.exec_command({
  cmd: "dd if=/dev/zero bs=50000 count=1 2>/dev/null | tr '\\000' x",
});
text(`${result.output.startsWith("Warning: truncated output")}:${result.original_token_count}`);
"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.preview.contains("true:12500"), "{}", result.preview);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_outer_budget_cannot_exceed_the_history_item_ceiling() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-bounded-outer",
                "// @exec: {\"max_output_tokens\": 50000}\ntext(\"x\".repeat(50000));",
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(
            result.preview.contains("Warning: truncated output"),
            "{}",
            result.preview
        );
        assert!(result.preview.len() < 50_000);
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
    async fn patch_and_plan_tools_dispatch_through_v8() {
        let cwd = temporary_directory("patch-plan");
        let runtime = runtime(cwd.clone());
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
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn papercut_tool_creates_and_appends_the_git_root_log() {
        let root = temporary_directory("papercut");
        let cwd = root.join("src/nested");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let runtime = runtime(cwd);
        assert!(!root.join("PAPERCUTS.md").exists());
        let result = runtime
            .execute(
                "call-papercut",
                r#"
const first = await tools.log_papercut({
  message: "While running tests,\n  the documented path was stale.",
});
const second = await tools.log_papercut({
  message: "The formatter emitted a misleading error.",
});
text(`${first.path}:${second.path}`);
"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(
            result.preview.contains("PAPERCUTS.md:PAPERCUTS.md"),
            "{}",
            result.preview
        );
        assert_eq!(
            std::fs::read_to_string(root.join("PAPERCUTS.md")).unwrap(),
            "# Papercuts\n\n- While running tests, the documented path was stale.\n- The formatter emitted a misleading error.\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_a_papercut_waiting_for_the_log_lock_stops_without_writing() {
        let root = temporary_directory("papercut-cancel");
        let cwd = root.join("src/nested");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let log_path = root.join("PAPERCUTS.md");
        let initial = "# Papercuts\n\n- Existing note.\n";
        std::fs::write(&log_path, initial).unwrap();
        let held_log = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(&log_path)
            .unwrap();
        held_log.lock().unwrap();

        let runtime = runtime(cwd);
        let cancellation = CancellationToken::new();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let execution = runtime.execute(
            "call-cancel-papercut",
            r#"await tools.log_papercut({message: "This must not be written."});"#,
            Some(events_tx),
            cancellation.clone(),
        );
        tokio::pin!(execution);

        let started = tokio::select! {
            _ = &mut execution => panic!("papercut finished before reaching the held lock"),
            event = events_rx.recv() => event.expect("papercut start event"),
        };
        assert!(matches!(
            started,
            AgentEvent::ToolStarted { name, .. } if name == "log_papercut"
        ));
        // Give the blocking worker time to reach the contended lock; the old
        // blocking File::lock implementation could not return after this point.
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), &mut execution)
            .await
            .expect("cancelled papercut remained blocked on the file lock")
            .unwrap();
        assert!(
            result.preview.starts_with("Script terminated"),
            "{}",
            result.preview
        );
        assert_eq!(std::fs::read_to_string(&log_path).unwrap(), initial);

        held_log.unlock().unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(std::fs::read_to_string(&log_path).unwrap(), initial);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_exec_stops_in_flight_patch_mutations() {
        let cwd = temporary_directory("cancel-patch");
        let runtime = runtime(cwd.clone());
        let cancellation = CancellationToken::new();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let execution = runtime.execute(
            "call-cancel-patch",
            r#"
const repeated = "*** Add File: scratch.txt\n+partial\n".repeat(10000);
const patch = `*** Begin Patch
${repeated}*** Add File: completed.txt
+done
*** End Patch`;
await tools.apply_patch(patch);
"#,
            Some(events_tx),
            cancellation.clone(),
        );
        tokio::pin!(execution);

        let started = tokio::select! {
            _ = &mut execution => panic!("patch finished before cancellation"),
            event = events_rx.recv() => event.expect("patch start event"),
        };
        assert!(matches!(
            started,
            AgentEvent::ToolStarted { name, .. } if name == "apply_patch"
        ));
        cancellation.cancel();

        let result = tokio::time::timeout(Duration::from_secs(5), &mut execution)
            .await
            .expect("cancelled patch did not stop")
            .unwrap();
        assert!(
            result.preview.starts_with("Script terminated"),
            "{}",
            result.preview
        );
        assert!(!cwd.join("completed.txt").exists());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !cwd.join("completed.txt").exists(),
            "patch continued mutating files after exec termination"
        );
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn view_image_dispatches_structured_image_content() {
        let cwd = temporary_directory("view-image");
        let image_path = cwd.join("sample.png");
        let image = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap();
        std::fs::write(&image_path, image).unwrap();
        let runtime = runtime(cwd.clone());
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
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalid_view_image_is_replaced_at_the_model_facing_boundary() {
        let cwd = temporary_directory("invalid-view-image");
        std::fs::write(cwd.join("broken.png"), b"\x89PNG\r\n\x1a\nnot-an-image").unwrap();
        let runtime = runtime(cwd.clone());
        let result = runtime
            .execute(
                "call-invalid-image",
                r#"const result = await tools.view_image({path: "broken.png"}); image(result);"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let items = result.body.as_array().unwrap();
        assert!(items.iter().any(|item| {
            item == &json!({
                "type": "input_text",
                "text": "image content omitted because it could not be processed",
            })
        }));
        assert!(!items.iter().any(|item| item["type"] == "input_image"));
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn text_only_single_item_result_uses_codex_plain_string_shape() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-no-output",
                r#"store("answer", 42);"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(
            result
                .body
                .as_str()
                .is_some_and(|body| body.starts_with("Script completed\nWall time ")),
            "{}",
            result.body
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
                "text(ALL_TOOLS.map(tool => tool.name).join(','));",
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            result.preview.contains(
                "apply_patch,exec_command,log_papercut,update_plan,view_image,write_stdin,web__run"
            ),
            "{}",
            result.preview
        );
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
    async fn notify_injects_a_preceding_output_item() {
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
            result.preceding_items,
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
    async fn notify_output_is_bounded_before_history_insertion() {
        let runtime = runtime(PathBuf::from("."));
        let result = runtime
            .execute(
                "call-large-notify",
                r#"notify("x".repeat(50000)); text("done");"#,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let output = result.preceding_items[0]["output"].as_str().unwrap();
        assert!(output.starts_with("Warning: truncated output"));
        assert!(output.len() < 50_000);
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
