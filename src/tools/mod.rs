mod catalogue;
mod code_mode;
mod code_runtime;
mod executor;
mod patch;

pub(crate) use catalogue::CatalogueRoute;
pub(crate) use catalogue::CatalogueTool;
pub(crate) use executor::command_argv_for_display;

use self::code_runtime::CodeModeNestedToolCall;
use self::code_runtime::CodeModeToolKind;
use crate::events::AgentEvent;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_utils_image::data_url_from_bytes;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
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
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> ToolResult {
        self.try_execute(runtime, events, cancellation)
            .await
            .unwrap_or_else(|error| ToolResult::text(format!("{error:#}")))
    }

    pub(crate) fn output_items(&self, output: &ToolResult) -> Vec<Value> {
        let mut items = output.preceding_items.clone();
        items.push(match self {
            Self::Function { call_id, .. } => json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output.body,
            }),
            Self::Custom { call_id, .. } => json!({
                "type": "custom_tool_call_output",
                "call_id": call_id,
                "output": output.body,
            }),
        });
        items
    }

    async fn try_execute(
        &self,
        runtime: &ToolRuntime,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        match self {
            Self::Custom {
                call_id,
                name,
                input,
            } if name == "exec" => {
                runtime
                    .code_mode
                    .execute(call_id, input, events, cancellation)
                    .await
            }
            Self::Function {
                name, arguments, ..
            } if name == "wait" => {
                runtime
                    .code_mode
                    .wait(arguments, events, cancellation)
                    .await
            }
            Self::Function { name, .. } | Self::Custom { name, .. } => {
                Err(anyhow!("unknown top-level tool `{name}`"))
            }
        }
    }
}

pub(crate) struct ToolRuntime {
    code_mode: code_mode::CodeMode,
}

impl ToolRuntime {
    pub(crate) fn new(cwd: PathBuf) -> Self {
        let nested = Arc::new(NestedTools::new(cwd));
        Self {
            code_mode: code_mode::CodeMode::new(nested),
        }
    }
}

pub(crate) struct ToolResult {
    body: Value,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) preview: String,
    preceding_items: Vec<Value>,
}

impl ToolResult {
    pub(crate) fn text(text: String) -> Self {
        Self {
            body: Value::String(text.clone()),
            preview: text,
            preceding_items: Vec::new(),
        }
    }
}

struct NestedTools {
    cwd: PathBuf,
    processes: executor::ProcessManager,
    plan: Mutex<Option<UpdatePlanArgs>>,
}

impl NestedTools {
    fn new(cwd: PathBuf) -> Self {
        Self {
            processes: executor::ProcessManager::new(cwd.clone()),
            cwd,
            plan: Mutex::new(None),
        }
    }

    async fn execute_nested(
        &self,
        invocation: CodeModeNestedToolCall,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        if invocation.tool_name.namespace.is_some() {
            return Err(anyhow!("unknown nested tool `{}`", invocation.tool_name));
        }
        let name = invocation.tool_name.name.as_str();
        match (name, invocation.tool_kind) {
            ("exec_command", CodeModeToolKind::Function) => {
                self.processes
                    .exec_command(function_input(name, invocation.input)?, cancellation)
                    .await
            }
            ("write_stdin", CodeModeToolKind::Function) => {
                self.processes
                    .write_stdin(function_input(name, invocation.input)?, cancellation)
                    .await
            }
            ("apply_patch", CodeModeToolKind::Freeform) => {
                let input = freeform_input(name, invocation.input)?;
                let cwd = self.cwd.clone();
                let _output = tokio::task::spawn_blocking(move || patch::apply(&cwd, &input))
                    .await
                    .context("apply_patch task failed")??;
                Ok(json!({}))
            }
            ("update_plan", CodeModeToolKind::Function) => {
                self.update_plan(function_input(name, invocation.input)?)
            }
            ("view_image", CodeModeToolKind::Function) => {
                let cwd = self.cwd.clone();
                let input = function_input(name, invocation.input)?;
                tokio::task::spawn_blocking(move || view_image(&cwd, input))
                    .await
                    .context("view_image task failed")?
            }
            _ => Err(anyhow!("unknown nested tool `{name}`")),
        }
    }

    fn update_plan(&self, input: Value) -> Result<Value> {
        let arguments: UpdatePlanArgs = serde_json::from_value(input)
            .map_err(|error| anyhow!("failed to parse function arguments: {error}"))?;
        *self
            .plan
            .lock()
            .map_err(|_| anyhow!("plan lock was poisoned"))? = Some(arguments);
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

pub(crate) fn code_mode_tool_names() -> Value {
    catalogue::code_mode_tool_names()
}

#[cfg(test)]
#[path = "../tools_tests.rs"]
mod tests;
