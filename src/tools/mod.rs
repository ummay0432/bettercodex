mod catalogue;
mod code_runtime;
mod exec_runtime;
mod executor;
mod image_preparation;
mod papercuts;
mod patch;

const MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS: usize =
    code_runtime::DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL;

pub(crate) use catalogue::CatalogueMetrics;
pub(crate) use catalogue::CatalogueRoute;
pub(crate) use catalogue::CatalogueTool;
pub(crate) use exec_runtime::ToolRuntime;
pub(crate) use executor::BackgroundProcess;
pub(crate) use executor::ProcessManager;
pub(crate) use executor::command_argv_for_display;

use self::code_runtime::CodeModeNestedToolCall as NestedToolCall;
use self::code_runtime::CodeModeToolKind as NestedToolKind;
use crate::events::AgentEvent;
use crate::web_search::ToolTurnContext;
use crate::web_search::WebSearchClient;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_utils_image::data_url_from_bytes;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::formatted_truncate_text;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ToolCall {
    Function {
        call_id: String,
        name: String,
        arguments: String,
    },
    Custom {
        call_id: String,
        name: String,
        input: String,
    },
}

impl ToolCall {
    pub(crate) fn from_response_item(item: &Value) -> Option<Self> {
        let item_type = item.get("type")?.as_str()?;
        let call_id = item.get("call_id")?.as_str()?.to_string();
        let name = item.get("name")?.as_str()?.to_string();
        match item_type {
            "function_call" => Some(Self::Function {
                call_id,
                name,
                arguments: item.get("arguments")?.as_str()?.to_string(),
            }),
            "custom_tool_call" => Some(Self::Custom {
                call_id,
                name,
                input: item.get("input")?.as_str()?.to_string(),
            }),
            _ => None,
        }
    }

    pub(crate) async fn execute(
        &self,
        runtime: &ToolRuntime,
        context: ToolTurnContext,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> ToolResult {
        self.try_execute(runtime, context, events, cancellation)
            .await
            .unwrap_or_else(|error| ToolResult::text(format!("{error:#}")))
    }

    pub(crate) fn into_output_items(self, output: ToolResult) -> Vec<Value> {
        let ToolResult {
            body,
            preceding_items,
            ..
        } = output;
        let mut items = preceding_items;
        let (item_type, call_id) = match self {
            Self::Function { call_id, .. } => ("function_call_output", call_id),
            Self::Custom { call_id, .. } => ("custom_tool_call_output", call_id),
        };
        let mut item = Map::new();
        item.insert("type".to_string(), Value::String(item_type.to_string()));
        item.insert("call_id".to_string(), Value::String(call_id));
        item.insert("output".to_string(), body);
        items.push(Value::Object(item));
        items
    }

    async fn try_execute(
        &self,
        runtime: &ToolRuntime,
        context: ToolTurnContext,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        runtime.prepare_turn(context);
        match self {
            Self::Custom {
                call_id,
                name,
                input,
            } if name == "exec" => runtime.execute(call_id, input, events, cancellation).await,
            Self::Function {
                name, arguments, ..
            } if name == "wait" => runtime.wait(arguments, events, cancellation).await,
            Self::Function { name, .. } | Self::Custom { name, .. } => {
                Err(anyhow!("unknown top-level tool `{name}`"))
            }
        }
    }
}

pub(crate) struct ToolResult {
    body: Value,
    // Runtime tests inspect this human-readable projection. Production moves the structured body
    // directly into history and should not duplicate every text result merely for a dead preview.
    #[cfg(test)]
    pub(crate) preview: String,
    preceding_items: Vec<Value>,
}

impl ToolResult {
    pub(crate) fn text(text: String) -> Self {
        let text = formatted_truncate_text(
            &text,
            TruncationPolicy::Tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
        );
        #[cfg(test)]
        let preview = text.clone();
        Self {
            body: Value::String(text),
            #[cfg(test)]
            preview,
            preceding_items: Vec::new(),
        }
    }
}

struct NestedTools {
    cwd: PathBuf,
    processes: ProcessManager,
    web_search: WebSearchClient,
    turn: Mutex<ToolTurnContext>,
}

impl NestedTools {
    fn with_web_search(cwd: PathBuf, web_search: WebSearchClient) -> Self {
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

    async fn execute_nested(
        &self,
        invocation: NestedToolCall,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        let namespace = invocation.tool_name.namespace.as_deref();
        let name = invocation.tool_name.name.as_str();
        match (namespace, name, invocation.tool_kind) {
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
                self.web_search
                    .run(invocation.input, &context, cancellation)
                    .await
            }
            (None, "exec_command", NestedToolKind::Function) => {
                self.processes
                    .exec_command(function_input(name, invocation.input)?, cancellation)
                    .await
            }
            (None, "write_stdin", NestedToolKind::Function) => {
                self.processes
                    .write_stdin(function_input(name, invocation.input)?, cancellation)
                    .await
            }
            (None, "apply_patch", NestedToolKind::Freeform) => {
                let input = freeform_input(name, invocation.input)?;
                let cwd = self.cwd.clone();
                let _output =
                    tokio::task::spawn_blocking(move || patch::apply(&cwd, &input, &cancellation))
                        .await
                        .context("apply_patch task failed")??;
                Ok(json!({}))
            }
            (None, "log_papercut", NestedToolKind::Function) => {
                let cwd = self.cwd.clone();
                let input = function_input(name, invocation.input)?;
                tokio::task::spawn_blocking(move || papercuts::log(&cwd, input, &cancellation))
                    .await
                    .context("log_papercut task failed")?
            }
            (None, "update_plan", NestedToolKind::Function) => {
                self.update_plan(function_input(name, invocation.input)?)
            }
            (None, "view_image", NestedToolKind::Function) => {
                let cwd = self.cwd.clone();
                let input = function_input(name, invocation.input)?;
                tokio::task::spawn_blocking(move || view_image(&cwd, input))
                    .await
                    .context("view_image task failed")?
            }
            _ => Err(anyhow!("unknown nested tool `{}`", invocation.tool_name)),
        }
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
    let requested = PathBuf::from(&arguments.path);
    let path = cwd.join(requested);
    let metadata = path
        .metadata()
        .with_context(|| format!("unable to locate image at `{}`", path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!("image path `{}` is not a file", path.display()));
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("unable to read image at `{}`", path.display()))?;
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

pub(crate) fn specifications() -> Vec<Value> {
    catalogue::specifications()
}

pub(crate) fn catalogue_text() -> &'static str {
    catalogue::text()
}

pub(crate) fn display_tools() -> &'static [CatalogueTool] {
    catalogue::display_tools()
}

pub(crate) fn catalogue_metrics() -> CatalogueMetrics {
    catalogue::metrics()
}

pub(crate) fn nested_tool_name_map() -> Value {
    catalogue::nested_tool_name_map()
}

#[cfg(test)]
#[path = "../tools_tests.rs"]
mod tests;
