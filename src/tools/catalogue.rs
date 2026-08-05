//! Fixed JavaScript tool catalogue ported from Codex's `code_mode_only` plan at
//! `1669c2403f793d0230065397dfc25f52b844244e`. bettercodex exposes this one
//! execution path unconditionally; it has no tool-mode selector.
//!
//! The schemas mirror `core/src/tools/handlers/{apply_patch_spec,plan_spec,
//! shell_spec,view_image_spec}.rs`; the `exec` and `wait` wrappers mirror
//! `core/src/tools/code_mode/{execute_spec,wait_spec}.rs`. bettercodex retains
//! Codex's schema-to-TypeScript renderer but uses a concise fixed-catalogue
//! preamble instead of documenting integrations this binary does not expose.

use super::code_runtime;
use super::code_runtime::CodeModeToolKind as ToolKind;
use super::code_runtime::ToolDefinition;
use super::code_runtime::ToolNamespaceDescription;
use codex_protocol::ToolName;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::LazyLock;

const EXEC_SOURCE_GRAMMAR: &str = r#"
start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE

PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;
const EXEC_RUNTIME_GUIDANCE: &str = r#"Execute raw JavaScript to orchestrate tool calls.
- Input JavaScript directly, without JSON wrapping, quotes, or Markdown fences. A fresh V8 isolate supports top-level `await` but has no Node.js, filesystem, direct network access, console, or persistent global state.
- Call the typed methods below as `await tools.name(args)`. Use `Promise.all` for independent calls. Tool results are strings or the documented objects; await all work before the script ends.
- Emit output with `text(value)` or `image(dataUrlOrItem, detail?)`; `notify(value)` emits an interim tool output. `yield_control()` yields accumulated output while the script continues.
- `store(key, value)` and `load(key)` persist serializable values across exec cells. `exit()` finishes successfully. `setTimeout` and `clearTimeout` are available.
- An optional first line `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}` controls early yielding and the direct-output token budget; defaults are 10000 for both."#;
const WAIT_DESCRIPTION: &str = "Resume a yielded `exec` cell. Use only the `cell_id` returned by `exec`; call `wait` again while the cell remains active. Each call returns only new output. `terminate: true` stops the cell. Waiting and output default to 10000 ms and 10000 tokens.";

static CORE_TOOLS: LazyLock<Vec<ToolDefinition>> = LazyLock::new(|| {
    vec![
        freeform_tool(
            "apply_patch",
            "The `apply_patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.",
        ),
        function_tool(
            "exec_command",
            "Runs a command in a PTY, returning output or a session ID for ongoing interaction.",
            exec_command_input_schema(),
            Some(unified_exec_output_schema()),
        ),
        function_tool(
            "log_papercut",
            "Appends one papercut note to `PAPERCUTS.md` at the Git repository root, creating the file on first use.",
            log_papercut_input_schema(),
            Some(log_papercut_output_schema()),
        ),
        function_tool(
            "update_plan",
            "Updates the task plan.\nProvide an optional explanation and a list of plan items, each with a step and status.\nAt most one step can be in_progress at a time.",
            update_plan_input_schema(),
            None,
        ),
        function_tool(
            "view_image",
            "View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk.",
            view_image_input_schema(),
            Some(view_image_output_schema()),
        ),
        function_tool(
            "write_stdin",
            "Writes characters to an existing unified exec session and returns recent output.",
            write_stdin_input_schema(),
            Some(unified_exec_output_schema()),
        ),
        namespaced_function_tool(
            crate::web_search::JAVASCRIPT_NAME,
            crate::web_search::NAMESPACE,
            crate::web_search::TOOL_NAME,
            crate::web_search::DESCRIPTION,
            crate::web_search::input_schema().clone(),
        ),
    ]
});

// The in-process V8 runtime needs names, descriptions, and calling kinds for
// its globals and ALL_TOOLS. Input and output schemas are request-prompt data;
// the session adapter immediately discards them. Strip those trees once rather
// than cloning the complete catalogue for every exec cell.
static RUNTIME_TOOLS: LazyLock<Vec<ToolDefinition>> = LazyLock::new(|| {
    CORE_TOOLS
        .iter()
        .map(|tool| ToolDefinition {
            name: tool.name.clone(),
            tool_name: tool.tool_name.clone(),
            description: tool.description.clone(),
            kind: tool.kind,
            input_schema: None,
            output_schema: None,
        })
        .collect()
});

static NAMESPACE_DESCRIPTIONS: LazyLock<BTreeMap<String, ToolNamespaceDescription>> =
    LazyLock::new(|| {
        BTreeMap::from([(
            crate::web_search::NAMESPACE.to_string(),
            ToolNamespaceDescription {
                name: crate::web_search::NAMESPACE.to_string(),
                description: format!("Tools in the {} namespace.", crate::web_search::NAMESPACE),
            },
        )])
    });

static EXEC_DESCRIPTION: LazyLock<String> = LazyLock::new(build_exec_description);

static CATALOGUE_INSPECTION: LazyLock<CatalogueInspection> = LazyLock::new(|| {
    let specifications = specifications();
    let mut tools = specifications
        .iter()
        .map(|specification| {
            let name = specification
                .get("name")
                .and_then(Value::as_str)
                .expect("request tool specifications always have a name");
            CatalogueTool {
                name: name.to_string(),
                route: CatalogueRoute::Request,
            }
        })
        .collect::<Vec<_>>();

    tools.extend(core_tools().iter().map(|tool| CatalogueTool {
        name: tool.name.clone(),
        route: CatalogueRoute::InsideExec,
    }));
    let item = json!({
        "type": "additional_tools",
        "role": "developer",
        "tools": specifications,
    });
    let request_bytes = u64::try_from(
        serde_json::to_vec(&item)
            .expect("the fixed tool catalogue is JSON serializable")
            .len(),
    )
    .unwrap_or(u64::MAX);
    CatalogueInspection {
        tools,
        metrics: CatalogueMetrics {
            description_bytes: u64::try_from(text().len()).unwrap_or(u64::MAX),
            request_bytes,
            estimated_tokens: request_bytes.div_ceil(4),
        },
    }
});

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogueRoute {
    Request,
    InsideExec,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CatalogueTool {
    pub(crate) name: String,
    pub(crate) route: CatalogueRoute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogueMetrics {
    pub(crate) description_bytes: u64,
    pub(crate) request_bytes: u64,
    pub(crate) estimated_tokens: u64,
}

struct CatalogueInspection {
    tools: Vec<CatalogueTool>,
    metrics: CatalogueMetrics,
}

pub(super) fn core_tools() -> &'static [ToolDefinition] {
    &CORE_TOOLS
}

pub(super) fn runtime_tools() -> &'static [ToolDefinition] {
    &RUNTIME_TOOLS
}

pub(crate) fn text() -> &'static str {
    &EXEC_DESCRIPTION
}

pub(crate) fn display_tools() -> &'static [CatalogueTool] {
    &CATALOGUE_INSPECTION.tools
}

pub(crate) fn metrics() -> CatalogueMetrics {
    CATALOGUE_INSPECTION.metrics
}

pub(crate) fn specifications() -> Vec<Value> {
    vec![
        json!({
            "type": "custom",
            "name": code_runtime::PUBLIC_TOOL_NAME,
            "description": text(),
            "format": {
                "type": "grammar",
                "syntax": "lark",
                "definition": EXEC_SOURCE_GRAMMAR,
            }
        }),
        json!({
            "type": "function",
            "name": code_runtime::WAIT_TOOL_NAME,
            "description": WAIT_DESCRIPTION,
            "strict": false,
            "parameters": {
                "type": "object",
                "properties": {
                    "cell_id": {
                        "type": "string",
                        "description": "Identifier of the running exec cell."
                    },
                    "yield_time_ms": {
                        "type": "number",
                        "description": "Wait before yielding more output. Defaults to 10000 ms."
                    },
                    "max_tokens": {
                        "type": "number",
                        "description": "Output token budget for this wait call. Defaults to 10000 tokens."
                    },
                    "terminate": {
                        "type": "boolean",
                        "description": "True stops the running exec cell; false or omitted waits for output."
                    }
                },
                "required": ["cell_id"],
                "additionalProperties": false
            }
        }),
    ]
}

fn build_exec_description() -> String {
    let mut sections = vec![EXEC_RUNTIME_GUIDANCE.to_string()];
    let mut current_namespace = None;
    for tool in core_tools() {
        let namespace = tool.tool_name.namespace.as_deref();
        if namespace != current_namespace {
            if let Some(namespace) = namespace
                && let Some(description) = NAMESPACE_DESCRIPTIONS.get(namespace)
            {
                sections.push(format!(
                    "## {}\n{}",
                    description.name,
                    description.description.trim()
                ));
            }
            current_namespace = namespace;
        }
        let normalized_name = code_runtime::normalize_code_mode_identifier(&tool.name);
        let heading = if normalized_name == tool.name {
            format!("### `{normalized_name}`")
        } else {
            format!("### `{normalized_name}` (`{}`)", tool.name)
        };
        sections.push(format!("{heading}\n{}", render_tool_declaration(tool)));
    }
    sections.join("\n\n")
}

fn render_tool_declaration(tool: &ToolDefinition) -> String {
    let (input_name, input_type) = match tool.kind {
        ToolKind::Function => (
            "args",
            tool.input_schema
                .as_ref()
                .map(code_runtime::render_json_schema_to_typescript)
                .unwrap_or_else(|| "unknown".to_string()),
        ),
        ToolKind::Freeform => ("input", "string".to_string()),
    };
    let output_type = tool
        .output_schema
        .as_ref()
        .map(code_runtime::render_json_schema_to_typescript)
        .unwrap_or_else(|| "unknown".to_string());
    code_runtime::render_code_mode_sample(
        tool.description.trim(),
        &tool.name,
        input_name,
        input_type,
        output_type,
    )
}

pub(crate) fn nested_tool_name_map() -> Value {
    Value::Object(
        core_tools()
            .iter()
            .map(|tool| {
                (
                    code_runtime::normalize_code_mode_identifier(&tool.name),
                    json!({
                        "name": tool.tool_name.name,
                        "namespace": tool.tool_name.namespace,
                    }),
                )
            })
            .collect(),
    )
}

fn function_tool(
    name: &str,
    description: &str,
    input_schema: Value,
    output_schema: Option<Value>,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        tool_name: ToolName::plain(name),
        description: description.to_string(),
        kind: ToolKind::Function,
        input_schema: Some(input_schema),
        output_schema,
    }
}

fn namespaced_function_tool(
    javascript_name: &str,
    namespace: &str,
    name: &str,
    description: &str,
    input_schema: Value,
) -> ToolDefinition {
    ToolDefinition {
        name: javascript_name.to_string(),
        tool_name: ToolName::namespaced(namespace, name),
        description: description.to_string(),
        kind: ToolKind::Function,
        input_schema: Some(input_schema),
        output_schema: None,
    }
}

fn freeform_tool(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        tool_name: ToolName::plain(name),
        description: description.to_string(),
        kind: ToolKind::Freeform,
        input_schema: None,
        output_schema: None,
    }
}

fn exec_command_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "cmd": {
                "type": "string",
                "description": "Shell command to execute."
            },
            "workdir": {
                "type": "string",
                "description": "Working directory for the command. Defaults to the turn cwd."
            },
            "shell": {
                "type": "string",
                "description": "Shell binary to launch. Defaults to the user's default shell."
            },
            "login": {
                "type": "boolean",
                "description": "True runs the shell with -l/-i semantics; false disables them. Defaults to true."
            },
            "tty": {
                "type": "boolean",
                "description": "True allocates a PTY for the command; false or omitted uses plain pipes."
            },
            "yield_time_ms": {
                "type": "number",
                "description": "Wait before yielding output. Defaults to 10000 ms; effective range is 250-30000 ms."
            },
            "max_output_tokens": {
                "type": "number",
                "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."
            }
        },
        "required": ["cmd"],
        "additionalProperties": false
    })
}

fn log_papercut_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "maxLength": super::papercuts::MAX_MESSAGE_CHARS,
                "description": "One or two sentences describing what caused friction and the likely fix when known."
            }
        },
        "required": ["message"],
        "additionalProperties": false
    })
}

fn log_papercut_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Repository-relative path to the papercut log."
            }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn write_stdin_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session_id": {
                "type": "number",
                "description": "Identifier of the running unified exec session."
            },
            "chars": {
                "type": "string",
                "description": "Bytes to write to stdin. Defaults to empty, which polls without writing."
            },
            "yield_time_ms": {
                "type": "number",
                "description": "Wait before yielding output. Non-empty writes default to 250 ms and cap at 30000 ms; empty polls wait 5000-300000 ms by default."
            },
            "max_output_tokens": {
                "type": "number",
                "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."
            }
        },
        "required": ["session_id"],
        "additionalProperties": false
    })
}

fn unified_exec_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "chunk_id": {
                "type": "string",
                "description": "Chunk identifier included when the response reports one."
            },
            "wall_time_seconds": {
                "type": "number",
                "description": "Elapsed wall time spent waiting for output in seconds."
            },
            "exit_code": {
                "type": "number",
                "description": "Process exit code when the command finished during this call."
            },
            "session_id": {
                "type": "number",
                "description": "Session identifier to pass to write_stdin when the process is still running."
            },
            "original_token_count": {
                "type": "number",
                "description": "Approximate token count before output truncation."
            },
            "output": {
                "type": "string",
                "description": "Command output text, possibly truncated."
            }
        },
        "required": ["wall_time_seconds", "output"],
        "additionalProperties": false
    })
}

fn update_plan_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "explanation": {
                "type": "string",
                "description": "Optional explanation for this plan update."
            },
            "plan": {
                "type": "array",
                "description": "The list of steps",
                "items": {
                    "type": "object",
                    "properties": {
                        "step": {
                            "type": "string",
                            "description": "Task step text."
                        },
                        "status": {
                            "type": "string",
                            "enum": ["pending", "in_progress", "completed"],
                            "description": "Step status."
                        }
                    },
                    "required": ["step", "status"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["plan"],
        "additionalProperties": false
    })
}

fn view_image_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Local filesystem path to an image file."
            },
            "detail": {
                "type": "string",
                "enum": ["high", "original"],
                "description": "Image detail level. Defaults to `high`; use `original` to preserve exact resolution."
            }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn view_image_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "image_url": {
                "type": "string",
                "description": "Data URL for the loaded image."
            },
            "detail": {
                "type": "string",
                "enum": ["high", "original"],
                "description": "Image detail hint returned by view_image. Returns `high` for default resized behavior or `original` when original resolution is preserved."
            }
        },
        "required": ["image_url", "detail"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_contains_the_fixed_tools_and_codex_web_namespace() {
        assert_eq!(
            core_tools()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [
                "apply_patch",
                "exec_command",
                "log_papercut",
                "update_plan",
                "view_image",
                "write_stdin",
                "web__run",
            ]
        );
    }

    #[test]
    fn runtime_catalogue_keeps_metadata_but_drops_prompt_only_schemas() {
        assert_eq!(runtime_tools().len(), core_tools().len());
        for (runtime, complete) in runtime_tools().iter().zip(core_tools()) {
            assert_eq!(runtime.name, complete.name);
            assert_eq!(runtime.tool_name, complete.tool_name);
            assert_eq!(runtime.description, complete.description);
            assert_eq!(runtime.kind, complete.kind);
            assert_eq!(
                (
                    runtime.input_schema.as_ref(),
                    runtime.output_schema.as_ref()
                ),
                (None, None)
            );
        }
        let runtime_bytes = serde_json::to_vec(runtime_tools()).unwrap().len();
        let complete_bytes = serde_json::to_vec(core_tools()).unwrap().len();
        assert!(
            runtime_bytes < complete_bytes,
            "runtime metadata was {runtime_bytes} bytes versus {complete_bytes} complete bytes"
        );
    }

    #[test]
    fn model_visible_catalogue_contains_typed_declarations() {
        let text = text();
        assert!(text.contains("fresh V8 isolate"));
        assert!(text.contains("### `exec_command`"));
        assert!(text.contains("exec_command(args:"));
        assert!(text.contains("### `log_papercut`"));
        assert!(text.contains("log_papercut(args:"));
        assert!(text.contains("Promise<{"));
        assert!(text.contains("### `apply_patch`"));
        assert!(text.contains("## web\nTools in the web namespace."));
        assert!(text.contains("### `web__run`"));
    }

    #[test]
    fn fixed_catalogue_omits_unavailable_integration_guidance() {
        let text = text();
        for omitted in [
            "MCP tool",
            "generatedImage",
            "audio(",
            "Examples of different commands",
        ] {
            assert!(!text.contains(omitted), "unexpected `{omitted}` in {text}");
        }
        for retained in [
            "Promise.all",
            "store(key, value)",
            "yield_control()",
            "primary sources",
            "direct, descriptive Markdown links",
            "Quote at most 25 words",
        ] {
            assert!(text.contains(retained), "missing `{retained}` in {text}");
        }
    }

    #[test]
    fn readable_catalogue_snapshot_matches_the_request() {
        assert_eq!(
            text().trim_end(),
            include_str!("../../prompts/tool-catalogue.md").trim_end()
        );
    }

    #[test]
    fn request_exposes_only_exec_and_wait() {
        let tools = specifications();
        let names = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names, ["exec", "wait"]);
    }

    #[test]
    fn display_catalogue_matches_the_request_and_nested_definitions() {
        let request = specifications();
        let expected_request = request
            .iter()
            .map(|tool| (tool["name"].as_str().unwrap(), CatalogueRoute::Request))
            .collect::<Vec<_>>();
        let displayed_request = display_tools()
            .iter()
            .filter(|tool| tool.route == CatalogueRoute::Request)
            .map(|tool| (tool.name.as_str(), tool.route))
            .collect::<Vec<_>>();
        assert_eq!(displayed_request, expected_request);

        let expected_nested = core_tools()
            .iter()
            .map(|tool| (tool.name.as_str(), CatalogueRoute::InsideExec))
            .collect::<Vec<_>>();
        let displayed_nested = display_tools()
            .iter()
            .filter(|tool| tool.route == CatalogueRoute::InsideExec)
            .map(|tool| (tool.name.as_str(), tool.route))
            .collect::<Vec<_>>();
        assert_eq!(displayed_nested, expected_nested);
    }

    #[test]
    fn documented_tool_context_byte_counts_do_not_drift() {
        let tools = specifications();
        let update = "run ./scripts/dev.py tool-context --update";
        assert_eq!(text().len(), 9_727, "{update}");
        assert_eq!(
            serde_json::to_string(&tools[0]).unwrap().len(),
            10_354,
            "{update}"
        );
        assert_eq!(
            serde_json::to_string(&tools[1]).unwrap().len(),
            826,
            "{update}"
        );
        let item = json!({
            "type": "additional_tools",
            "role": "developer",
            "tools": tools,
        });
        assert_eq!(
            serde_json::to_string(&item).unwrap().len(),
            11_238,
            "{update}"
        );
        assert_eq!(
            metrics(),
            CatalogueMetrics {
                description_bytes: 9_727,
                request_bytes: 11_238,
                estimated_tokens: 2_810,
            }
        );
    }
}
