mod catalogue;
mod code_runtime;
mod exec_runtime;
mod executor;
mod papercuts;
mod patch;
mod process_session;

const MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS: usize =
    code_runtime::DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL;
const VIEW_IMAGE_INVALID_MESSAGE: &str =
    "unable to process image: invalid or unsupported image data";

pub(crate) use code_runtime::package_smoke_test;
pub(crate) use exec_runtime::ToolRuntime;
pub(crate) use executor::BackgroundProcess;
pub(crate) use executor::ProcessManager;
pub(crate) use executor::command_argv_for_display;

use self::code_runtime::CodeModeNestedToolCall as NestedToolCall;
use self::code_runtime::CodeModeToolKind as NestedToolKind;
use crate::events::AgentEvent;
use crate::image::data_url_from_bytes;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolConfiguration {
    papercut: bool,
}

impl ToolConfiguration {
    pub(crate) const fn with_papercut() -> Self {
        Self { papercut: true }
    }

    pub(crate) const fn papercut_enabled(self) -> bool {
        self.papercut
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ToolCall {
    Exec { call_id: String, input: String },
    Wait { call_id: String, arguments: String },
}

impl ToolCall {
    pub(crate) fn from_response_item(item: &Value) -> Option<Self> {
        let item_type = item.get("type")?.as_str()?;
        let call_id = item.get("call_id")?.as_str()?.to_string();
        match item.get("namespace") {
            None | Some(Value::Null) => {}
            Some(Value::String(namespace)) if namespace.is_empty() || namespace == "functions" => {}
            Some(_) => return None,
        };
        let name = item.get("name")?.as_str()?;
        match (item_type, name) {
            ("custom_tool_call", "exec") => Some(Self::Exec {
                call_id,
                input: item.get("input")?.as_str()?.to_string(),
            }),
            ("function_call", "wait") => Some(Self::Wait {
                call_id,
                arguments: item.get("arguments")?.as_str()?.to_string(),
            }),
            _ => None,
        }
    }

    pub(crate) async fn execute(
        &self,
        runtime: &ToolRuntime,
        truncation_policy: TruncationPolicy,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> ToolResult {
        let result = match self {
            Self::Exec { call_id, input } => {
                runtime.execute(call_id, input, events, cancellation).await
            }
            Self::Wait { arguments, .. } => runtime.wait(arguments, events, cancellation).await,
        };
        result.unwrap_or_else(|error| {
            ToolResult::text_with_policy(format!("{error:#}"), truncation_policy)
        })
    }

    pub(crate) fn into_output_items(self, output: ToolResult) -> Vec<Value> {
        let ToolResult { body, .. } = output;
        let (item_type, call_id) = match self {
            Self::Exec { call_id, .. } => ("custom_tool_call_output", call_id),
            Self::Wait { call_id, .. } => ("function_call_output", call_id),
        };
        let mut item = Map::new();
        item.insert("type".to_string(), Value::String(item_type.to_string()));
        item.insert("call_id".to_string(), Value::String(call_id));
        item.insert("output".to_string(), body);
        vec![Value::Object(item)]
    }
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
    processes: ProcessManager,
    web_search: WebSearchClient,
    turn: Mutex<ToolTurnContext>,
}

impl ToolImplementations {
    fn new(cwd: PathBuf, web_search: WebSearchClient) -> Self {
        Self {
            processes: ProcessManager::new(cwd.clone()),
            cwd,
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
                tokio::task::spawn_blocking(move || view_image(&cwd, input))
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
        // Match upstream's Code Mode result contract for side-effecting tools.
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

fn view_image(cwd: &Path, input: Value) -> Result<Value> {
    let arguments: ViewImageArgs = serde_json::from_value(input)
        .map_err(|error| anyhow!("failed to parse function arguments: {error}"))?;
    let detail = match arguments.detail.as_deref() {
        None | Some("high") => "high",
        Some("original") => "original",
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
    // Match current Codex: prevent arbitrary local file bytes from reaching a Code Mode cell
    // through view_image while leaving valid image bytes unchanged for centralized preparation.
    image::load_from_memory(&bytes).map_err(|_| anyhow!(VIEW_IMAGE_INVALID_MESSAGE))?;
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

pub(crate) fn responses_lite_specifications(configuration: ToolConfiguration) -> Vec<Value> {
    catalogue::responses_lite_specifications(configuration)
}

pub(crate) fn catalogue_text(configuration: ToolConfiguration) -> &'static str {
    catalogue::text(configuration)
}

#[cfg(test)]
#[path = "../tools_tests.rs"]
mod tests;
