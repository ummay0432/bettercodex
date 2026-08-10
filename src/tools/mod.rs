mod catalogue;
mod code_runtime;
mod exec_runtime;
mod executor;
mod image_preparation;
mod papercuts;
mod patch;
mod process_session;

const MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS: usize =
    code_runtime::DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL;

pub(crate) use code_runtime::package_smoke_test;
pub(crate) use exec_runtime::ToolRuntime;
pub(crate) use executor::BackgroundProcess;
pub(crate) use executor::ProcessManager;
pub(crate) use executor::command_argv_for_display;

use self::code_runtime::CodeModeNestedToolCall as NestedToolCall;
use self::code_runtime::CodeModeToolKind as NestedToolKind;
use crate::events::AgentEvent;
use crate::image::data_url_from_bytes;
use crate::model::ToolMode;
use crate::openai_docs::OpenAiDocsClient;
use crate::protocol::UpdatePlanArgs;
use crate::truncation::TruncationPolicy;
use crate::truncation::formatted_truncate_text;
use crate::truncation::formatted_truncate_text_with_policy;
use crate::web_search::ToolTurnContext;
use crate::web_search::WebSearchClient;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ToolCall {
    Function {
        call_id: String,
        namespace: Option<String>,
        name: String,
        arguments: String,
    },
    Custom {
        call_id: String,
        namespace: Option<String>,
        name: String,
        input: String,
    },
}

impl ToolCall {
    pub(crate) fn from_response_item(item: &Value) -> Option<Self> {
        let item_type = item.get("type")?.as_str()?;
        let call_id = item.get("call_id")?.as_str()?.to_string();
        let namespace = match item.get("namespace") {
            None | Some(Value::Null) => None,
            Some(Value::String(namespace)) if namespace.is_empty() || namespace == "functions" => {
                None
            }
            Some(Value::String(namespace)) => Some(namespace.clone()),
            Some(_) => return None,
        };
        let name = item.get("name")?.as_str()?.to_string();
        match item_type {
            "function_call" => Some(Self::Function {
                call_id,
                namespace,
                name,
                arguments: item.get("arguments")?.as_str()?.to_string(),
            }),
            "custom_tool_call" => Some(Self::Custom {
                call_id,
                namespace,
                name,
                input: item.get("input")?.as_str()?.to_string(),
            }),
            _ => None,
        }
    }

    pub(crate) async fn execute(
        &self,
        runtime: &ToolRuntime,
        tool_mode: ToolMode,
        context: ToolTurnContext,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> ToolResult {
        let truncation_policy = context.truncation_policy();
        self.try_execute(runtime, tool_mode, context, events, cancellation)
            .await
            .unwrap_or_else(|error| {
                ToolResult::text_with_policy(format!("{error:#}"), truncation_policy)
            })
    }

    pub(crate) fn into_output_items(self, output: ToolResult) -> Vec<Value> {
        let ToolResult { body, .. } = output;
        let (item_type, call_id) = match self {
            Self::Function { call_id, .. } => ("function_call_output", call_id),
            Self::Custom { call_id, .. } => ("custom_tool_call_output", call_id),
        };
        let mut item = Map::new();
        item.insert("type".to_string(), Value::String(item_type.to_string()));
        item.insert("call_id".to_string(), Value::String(call_id));
        item.insert("output".to_string(), body);
        vec![Value::Object(item)]
    }

    pub(crate) fn supports_parallel_execution(&self) -> bool {
        match self {
            Self::Function {
                namespace, name, ..
            } => find_direct_tool_definition(namespace.as_deref(), name, NestedToolKind::Function)
                .is_some_and(|tool| {
                    tool.tool_name.namespace.is_some()
                        || matches!(name.as_str(), "exec_command" | "view_image" | "write_stdin")
                }),
            Self::Custom { .. } => false,
        }
    }

    async fn try_execute(
        &self,
        runtime: &ToolRuntime,
        tool_mode: ToolMode,
        context: ToolTurnContext,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        let truncation_policy = context.truncation_policy();
        runtime.prepare_turn(context);
        match self {
            Self::Custom {
                call_id,
                namespace,
                name,
                input,
            } if namespace.is_none() && name == "exec" && tool_mode.includes_code_mode() => {
                runtime.execute(call_id, input, events, cancellation).await
            }
            Self::Function {
                namespace,
                name,
                arguments,
                ..
            } if namespace.is_none() && name == "wait" && tool_mode.includes_code_mode() => {
                runtime.wait(arguments, events, cancellation).await
            }
            Self::Function {
                call_id,
                namespace,
                name,
                arguments,
            } => {
                let input = serde_json::from_str(arguments)
                    .map_err(|error| anyhow!("failed to parse function arguments: {error}"))?;
                let definition =
                    direct_tool_definition(namespace.as_deref(), name, NestedToolKind::Function)?;
                runtime
                    .execute_direct(
                        call_id,
                        definition,
                        Some(input),
                        truncation_policy,
                        events,
                        cancellation,
                    )
                    .await
            }
            Self::Custom {
                call_id,
                namespace,
                name,
                input,
            } => {
                let definition =
                    direct_tool_definition(namespace.as_deref(), name, NestedToolKind::Freeform)?;
                runtime
                    .execute_direct(
                        call_id,
                        definition,
                        Some(Value::String(input.clone())),
                        truncation_policy,
                        events,
                        cancellation,
                    )
                    .await
            }
        }
    }
}

fn direct_tool_definition(
    namespace: Option<&str>,
    name: &str,
    kind: NestedToolKind,
) -> Result<&'static code_runtime::ToolDefinition> {
    find_direct_tool_definition(namespace, name, kind).ok_or_else(|| {
        let name = namespace.map_or_else(
            || name.to_string(),
            |namespace| format!("{namespace}.{name}"),
        );
        anyhow!("unknown top-level tool `{name}`")
    })
}

fn find_direct_tool_definition(
    namespace: Option<&str>,
    name: &str,
    kind: NestedToolKind,
) -> Option<&'static code_runtime::ToolDefinition> {
    catalogue::core_tools(/*supports_image_detail_original*/ true)
        .iter()
        .find(|tool| {
            tool.tool_name.namespace.as_deref() == namespace
                && tool.tool_name.name == name
                && tool.kind == kind
        })
}

pub(crate) struct ToolResult {
    body: Value,
    // Runtime tests inspect this human-readable projection. Production moves the structured body
    // directly into history and should not duplicate every text result merely for a dead preview.
    #[cfg(test)]
    pub(crate) preview: String,
}

impl ToolResult {
    pub(crate) fn text(text: String) -> Self {
        let text = formatted_truncate_text(&text, MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS);
        Self::untruncated_text(text)
    }

    fn text_with_policy(text: String, policy: TruncationPolicy) -> Self {
        Self::untruncated_text(formatted_truncate_text_with_policy(&text, policy))
    }

    fn untruncated_text(text: String) -> Self {
        #[cfg(test)]
        let preview = text.clone();
        Self {
            body: Value::String(text),
            #[cfg(test)]
            preview,
        }
    }
}

struct ToolImplementations {
    cwd: PathBuf,
    openai_docs: OpenAiDocsClient,
    processes: ProcessManager,
    web_search: WebSearchClient,
    turn: Mutex<ToolTurnContext>,
}

impl ToolImplementations {
    fn new(cwd: PathBuf, web_search: WebSearchClient, openai_docs: OpenAiDocsClient) -> Self {
        Self {
            processes: ProcessManager::new(cwd.clone()),
            cwd,
            openai_docs,
            web_search,
            turn: Mutex::new(ToolTurnContext::default()),
        }
    }

    fn prepare_turn(&self, context: ToolTurnContext) {
        if let Ok(mut turn) = self.turn.lock() {
            *turn = context;
        }
    }

    fn turn_context(&self) -> Result<ToolTurnContext> {
        self.turn
            .lock()
            .map(|turn| turn.clone())
            .map_err(|_| anyhow!("tool turn context lock was poisoned"))
    }

    fn supports_image_detail_original(&self) -> Result<bool> {
        self.turn
            .lock()
            .map(|turn| turn.supports_image_detail_original())
            .map_err(|_| anyhow!("tool turn context lock was poisoned"))
    }

    async fn invoke(
        &self,
        tool_name: &crate::protocol::ToolName,
        tool_kind: NestedToolKind,
        input: Option<Value>,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        let namespace = tool_name.namespace.as_deref();
        let name = tool_name.name.as_str();
        match (namespace, name, tool_kind) {
            (Some(crate::openai_docs::NAMESPACE), name, NestedToolKind::Function)
                if crate::openai_docs::is_tool(name) =>
            {
                self.openai_docs
                    .call(name, function_input(name, input)?, cancellation)
                    .await
            }
            (
                Some(crate::web_search::NAMESPACE),
                crate::web_search::TOOL_NAME,
                NestedToolKind::Function,
            ) => {
                let context = self
                    .turn
                    .lock()
                    .map_err(|_| anyhow!("web search turn context lock was poisoned"))?
                    .clone();
                self.web_search.run(input, &context, cancellation).await
            }
            (None, "exec_command", NestedToolKind::Function) => {
                self.processes
                    .exec_command(function_input(name, input)?, cancellation)
                    .await
            }
            (None, "write_stdin", NestedToolKind::Function) => {
                self.processes
                    .write_stdin(function_input(name, input)?, cancellation)
                    .await
            }
            (None, "apply_patch", NestedToolKind::Freeform) => {
                let input = freeform_input(name, input)?;
                let cwd = self.cwd.clone();
                let output =
                    tokio::task::spawn_blocking(move || patch::apply(&cwd, &input, &cancellation))
                        .await
                        .context("apply_patch task failed")??;
                Ok(Value::String(output))
            }
            (None, "log_papercut", NestedToolKind::Function) => {
                let cwd = self.cwd.clone();
                let input = function_input(name, input)?;
                tokio::task::spawn_blocking(move || papercuts::log(&cwd, input, &cancellation))
                    .await
                    .context("log_papercut task failed")?
            }
            (None, "update_plan", NestedToolKind::Function) => {
                self.update_plan(function_input(name, input)?)
            }
            (None, "view_image", NestedToolKind::Function) => {
                let cwd = self.cwd.clone();
                let input = function_input(name, input)?;
                let supports_image_detail_original = self.supports_image_detail_original()?;
                tokio::task::spawn_blocking(move || {
                    view_image(&cwd, input, supports_image_detail_original)
                })
                .await
                .context("view_image task failed")?
            }
            _ => Err(anyhow!("unknown tool `{tool_name}`")),
        }
    }

    async fn execute_nested(
        &self,
        invocation: NestedToolCall,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        let NestedToolCall {
            tool_name,
            tool_kind,
            input,
            ..
        } = invocation;
        let result = self
            .invoke(&tool_name, tool_kind, input, cancellation)
            .await?;
        // Match upstream's Code Mode result contracts for side-effecting tools. Their direct
        // route reports a human-readable acknowledgement instead.
        if tool_name.namespace.is_none()
            && matches!(tool_name.name.as_str(), "apply_patch" | "update_plan")
        {
            return Ok(json!({}));
        }
        Ok(result)
    }

    fn update_plan(&self, input: Value) -> Result<Value> {
        serde_json::from_value::<UpdatePlanArgs>(input)
            .map_err(|error| anyhow!("failed to parse function arguments: {error}"))?;
        Ok(json!({}))
    }
}

fn view_image(cwd: &Path, input: Value, supports_image_detail_original: bool) -> Result<Value> {
    let arguments: ViewImageArgs = serde_json::from_value(input)
        .map_err(|error| anyhow!("failed to parse function arguments: {error}"))?;
    let detail = match arguments.detail.as_deref() {
        None | Some("high") => "high",
        Some("original") if supports_image_detail_original => "original",
        Some("original") => "high",
        Some(detail) => {
            return Err(anyhow!(
                "view_image.detail only supports `high` or `original`; omit `detail` for default high resized behavior, got `{detail}`"
            ));
        }
    };
    let path = cwd.join(PathBuf::from(&arguments.path));
    let file = File::open(&path)
        .with_context(|| format!("unable to locate image at `{}`", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("unable to inspect image at `{}`", path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!("image path `{}` is not a file", path.display()));
    }
    let limit = crate::input::MAX_TOTAL_IMAGE_BYTES;
    if metadata.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(anyhow!(
            "image at `{}` exceeds the {} MiB view_image limit",
            path.display(),
            limit / (1024 * 1024)
        ));
    }
    let capacity = usize::try_from(metadata.len()).unwrap_or(limit).min(limit);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .with_context(|| format!("unable to read image at `{}`", path.display()))?;
    if bytes.len() > limit {
        return Err(anyhow!(
            "image at `{}` exceeds the {} MiB view_image limit",
            path.display(),
            limit / (1024 * 1024)
        ));
    }
    Ok(json!({
        "image_url": data_url_from_bytes("application/octet-stream", &bytes),
        "detail": detail,
    }))
}

fn function_input(name: &str, input: Option<Value>) -> Result<Value> {
    match input {
        None => Ok(json!({})),
        Some(input @ Value::Object(_)) => Ok(input),
        Some(_) => Err(anyhow!("tool `{name}` expects a JSON object for arguments")),
    }
}

fn freeform_input(name: &str, input: Option<Value>) -> Result<String> {
    match input {
        Some(Value::String(input)) => Ok(input),
        _ => Err(anyhow!("tool `{name}` expects a string input")),
    }
}

#[derive(Deserialize)]
struct ViewImageArgs {
    path: String,
    detail: Option<String>,
}

pub(crate) fn specifications(
    tool_mode: crate::model::ToolMode,
    supports_image_detail_original: bool,
) -> Vec<Value> {
    catalogue::specifications(tool_mode, supports_image_detail_original)
}

pub(crate) fn responses_lite_specifications(
    tool_mode: crate::model::ToolMode,
    supports_image_detail_original: bool,
) -> Vec<Value> {
    catalogue::responses_lite_specifications(tool_mode, supports_image_detail_original)
}

pub(crate) fn catalogue_text() -> &'static str {
    catalogue::text()
}

#[cfg(test)]
#[path = "../tools_tests.rs"]
mod tests;
