//! Tool catalogue following Codex's GPT-5.6 `code_mode_only` exposure.
//!
//! The schemas mirror `core/src/tools/handlers/{apply_patch_spec,plan_spec,
//! shell_spec,view_image_spec}.rs`; the `exec` and `wait` wrappers mirror
//! `core/src/tools/code_mode/{execute_spec,wait_spec}.rs`. The model-facing Code
//! Mode descriptions retain upstream's schema-derived declaration format.

use super::ToolConfiguration;
use super::code_runtime;
use super::code_runtime::CodeModeToolKind as ToolKind;
use super::code_runtime::ToolDefinition;
use crate::protocol::ToolName;
use serde_json::Value;
use serde_json::json;
use std::sync::LazyLock;

const EXEC_SOURCE_GRAMMAR: &str = r#"
start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE

PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;
const EXEC_DESCRIPTION_TEMPLATE: &str = r#"Run JavaScript code to orchestrate/compose tool calls
- Submit raw JavaScript source—not JSON, a quoted string, or a Markdown code fence. It runs in a fresh V8 isolate as an async module; the runtime has no Node, file system, network access, or console.
- All nested tools are available on the global `tools` object under normalized JavaScript identifiers. Call them as `await tools.exec_command(...)`; MCP names normalize to identifiers such as `await tools.mcp__ologs__get_profile(...)`.
- Nested tool methods accept a string or object and return an object or string, according to the tool description.
- Await every operation. When JavaScript finishes evaluating, the isolate ends and unawaited promises are silently discarded.
- Optional first line: `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}`. `yield_time_ms` asks `exec` to yield early if the script is still running (default: 10000 ms); `max_output_tokens` sets the token budget for direct `exec` results (default: 10000 tokens).

Global helpers:
- `exit()`: Immediately ends the current script successfully, like an early return from the top level.
- `text(value: string | number | boolean | undefined | null)`: Appends a text item; non-string values are stringified with `JSON.stringify(...)` when possible.
- `image(imageUrlOrItem: string | { image_url: string; detail?: "auto" | "low" | "high" | "original" | null } | ImageContent, detail?: "auto" | "low" | "high" | "original" | null)`: Appends an image item. `image_url` should be a base64-encoded `data:` URL. Forward an MCP tool image by passing an individual `ImageContent` block from `result.content`, for example `image(result.content[0])`. MCP image blocks may request detail with `_meta: { "codex/imageDetail": "original" }`; when provided, the second `detail` argument overrides detail embedded in the first argument.
- `generatedImage(result: { image_url: string; output_hint?: string })`: Appends an image-generation result and its optional output hint; HTTP(S) URLs are not supported.
- `store(key: string, value: any)`: Stores a serializable value under a string key for later `exec` calls in the same session.
- `load(key: string)`: Returns the stored value, or `undefined` if missing.
- `notify(value: string | number | boolean | undefined | null)`: Immediately injects an extra `custom_tool_call_output` for the current `exec` call; values are stringified like `text(...)`.
- `setTimeout(callback: () => void, delayMs?: number)`: Schedules a callback and returns a timeout id. Pending timeouts do not keep `exec` alive by themselves; await an explicit promise to wait for one.
- `clearTimeout(timeoutId?: number)`: Cancels a timeout created by `setTimeout`.
- `ALL_TOOLS`: Metadata for enabled nested tools as `{ name, description }` entries.
- `yield_control()`: Yields accumulated output to the model immediately while the script keeps running."#;
const WAIT_DESCRIPTION_TEMPLATE: &str = r#"- Use `wait` only after `exec` returns `Script running with cell ID ...`.
- `cell_id` identifies the running `exec` cell to resume.
- `yield_time_ms` controls how long to wait for more output before yielding again. Defaults to 10000 ms.
- `max_tokens` limits how much new output this wait call returns. Defaults to 10000 tokens.
- `terminate: true` stops the running cell; false or omitted waits for output.
- `wait` returns only the new output since the last yield, or the final completion or termination result for that cell.
- If the cell is still running, `wait` may yield again with the same `cell_id`.
- If the cell has already finished, `wait` returns the completed result and closes the cell."#;
#[cfg(not(windows))]
const EXEC_COMMAND_DESCRIPTION: &str =
    "Runs a command in a PTY, returning output or a session ID for ongoing interaction.";
#[cfg(windows)]
const EXEC_COMMAND_DESCRIPTION: &str = r#"Runs a command in a PTY, returning output or a session ID for ongoing interaction.

Windows safety rules:
- Do not compose destructive filesystem commands across shells. Do not enumerate paths in PowerShell and then pass them to `cmd /c`, batch builtins, or another shell for deletion or moving. Use one shell end-to-end, prefer native PowerShell cmdlets such as `Remove-Item` / `Move-Item` with `-LiteralPath`, and avoid string-built shell commands for file operations.
- Before any recursive delete or move on Windows, verify the resolved absolute target paths stay within the intended workspace or explicitly named target directory. Never issue a recursive delete or move against a computed path if the final target has not been checked.
- When using `Start-Process` to launch a background helper or service, pass `-WindowStyle Hidden` unless the user explicitly asked for a visible interactive window. Use visible windows only for interactive tools the user needs to see or control."#;

const EXEC_LOGIN_DESCRIPTION: &str =
    "True runs the shell with -l/-i semantics; false disables them. Defaults to true.";
const EXEC_TTY_DESCRIPTION: &str =
    "True allocates a PTY for the command; false or omitted uses plain pipes.";

#[cfg(not(windows))]
const EXEC_YIELD_TIME_DESCRIPTION: &str =
    "Wait before yielding output. Defaults to 10000 ms; effective range is 250-30000 ms.";
#[cfg(windows)]
const EXEC_YIELD_TIME_DESCRIPTION: &str = "Maximum time to wait before returning a session ID for a still-running command. Commands that finish sooner return immediately. For ordinary commands, omit this parameter to use the 10000 ms default. Effective range on Windows is 10000-30000 ms.";

struct ToolCatalogue {
    runtime_tools: Vec<ToolDefinition>,
    exec_description: String,
    request_specifications: Vec<Value>,
}

static DEFAULT_TOOL_CATALOGUE: LazyLock<ToolCatalogue> =
    LazyLock::new(|| build_tool_catalogue(ToolConfiguration::default()));
static PAPERCUT_TOOL_CATALOGUE: LazyLock<ToolCatalogue> =
    LazyLock::new(|| build_tool_catalogue(ToolConfiguration::with_papercut()));

fn build_tool_catalogue(configuration: ToolConfiguration) -> ToolCatalogue {
    let core_tools = build_core_tools(configuration);
    let runtime_tools = build_runtime_tools(&core_tools);
    let exec_description = build_exec_description(&core_tools);
    let request_specifications = build_responses_lite_specifications(&exec_description);
    ToolCatalogue {
        runtime_tools,
        exec_description,
        request_specifications,
    }
}

fn tool_catalogue(configuration: ToolConfiguration) -> &'static ToolCatalogue {
    if configuration.papercut_enabled() {
        &PAPERCUT_TOOL_CATALOGUE
    } else {
        &DEFAULT_TOOL_CATALOGUE
    }
}

fn build_core_tools(configuration: ToolConfiguration) -> Vec<ToolDefinition> {
    let mut tools = vec![
        function_tool(
            "exec_command",
            EXEC_COMMAND_DESCRIPTION,
            exec_command_input_schema(),
            Some(unified_exec_output_schema()),
        ),
        function_tool(
            "write_stdin",
            "Writes characters to an existing unified exec session and returns recent output.",
            write_stdin_input_schema(),
            Some(unified_exec_output_schema()),
        ),
        function_tool(
            "update_plan",
            "Updates the task plan.\nProvide an optional explanation and a list of plan items, each with a step and status.\nAt most one step can be in_progress at a time.\n",
            update_plan_input_schema(),
            None,
        ),
        freeform_tool(
            "apply_patch",
            "The `apply_patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.",
        ),
        function_tool(
            "view_image",
            "View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk.",
            view_image_input_schema(),
            Some(view_image_output_schema()),
        ),
    ];
    if configuration.papercut_enabled() {
        tools.push(function_tool(
            "log_papercut",
            "Appends one repository-root `PAPERCUTS.md` note: 1–2 sentences on friction and likely fix.",
            log_papercut_input_schema(),
            Some(log_papercut_output_schema()),
        ));
    }
    tools.push(namespaced_function_tool(
        crate::web_search::JAVASCRIPT_NAME,
        crate::web_search::NAMESPACE,
        crate::web_search::TOOL_NAME,
        crate::web_search::DESCRIPTION,
        crate::web_search::input_schema().clone(),
        Some(json!({"type": "string"})),
    ));
    tools
}

// The in-process V8 runtime needs names, descriptions, and calling kinds for
// its globals and ALL_TOOLS. Input and output schemas are request-prompt data;
// the session adapter immediately discards them. Strip those trees once rather
// than cloning the complete catalogue for every exec cell.
fn build_runtime_tools(core_tools: &[ToolDefinition]) -> Vec<ToolDefinition> {
    let mut tools = core_tools
        .iter()
        .map(|tool| {
            // Each runtime definition carries the schema-derived declaration used by a Code Mode
            // cell. Namespaced definitions additionally inherit their namespace guidance.
            let description = render_code_mode_sample_for_definition(tool);
            let description = if let Some(namespace) = tool.tool_name.namespace.as_deref() {
                format!(
                    "{}\n\n{description}",
                    default_namespace_description(namespace)
                )
            } else {
                description
            };
            ToolDefinition {
                name: tool.name.clone(),
                tool_name: tool.tool_name.clone(),
                description,
                kind: tool.kind,
                input_schema: None,
                output_schema: None,
            }
        })
        .collect::<Vec<_>>();
    // Current Codex canonicalizes the definitions sent to each Code Mode cell by global name.
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools
}

pub(super) fn runtime_tools(configuration: ToolConfiguration) -> &'static [ToolDefinition] {
    &tool_catalogue(configuration).runtime_tools
}

pub(crate) fn text(configuration: ToolConfiguration) -> &'static str {
    &tool_catalogue(configuration).exec_description
}

pub(crate) fn responses_lite_specifications(configuration: ToolConfiguration) -> Vec<Value> {
    tool_catalogue(configuration).request_specifications.clone()
}

fn build_responses_lite_specifications(exec_description: &str) -> Vec<Value> {
    vec![json!({
        "type": "namespace",
        "name": "functions",
        "description": "",
        "tools": [
            exec_specification(exec_description),
            wait_specification(),
        ],
    })]
}

fn default_namespace_description(namespace: &str) -> String {
    format!("Tools in the {namespace} namespace.")
}

fn exec_specification(description: &str) -> Value {
    json!({
        "type": "custom",
        "name": code_runtime::PUBLIC_TOOL_NAME,
        "description": description,
        "format": {
            "type": "grammar",
            "syntax": "lark",
            "definition": EXEC_SOURCE_GRAMMAR,
        }
    })
}

fn wait_specification() -> Value {
    json!({
        "type": "function",
        "name": code_runtime::WAIT_TOOL_NAME,
        "description": format!(
            "Waits on a yielded `{}` cell and returns new output or completion.\n{}",
            code_runtime::PUBLIC_TOOL_NAME,
            WAIT_DESCRIPTION_TEMPLATE.trim(),
        ),
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
    })
}

fn build_exec_description(core_tools: &[ToolDefinition]) -> String {
    let exec_description = EXEC_DESCRIPTION_TEMPLATE.replace(
        "default: 10000 ms",
        &format!(
            "default: {} ms",
            code_runtime::DEFAULT_CODE_MODE_EXEC_YIELD_TIME_MS
        ),
    );
    let mut tools = core_tools.iter().collect::<Vec<_>>();
    tools.sort_by(|left, right| {
        left.tool_name
            .namespace
            .cmp(&right.tool_name.namespace)
            .then_with(|| left.tool_name.name.cmp(&right.tool_name.name))
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut current_namespace = None;
    let mut nested_tool_sections = Vec::new();
    for tool in tools {
        let next_namespace = tool.tool_name.namespace.as_deref();
        if next_namespace != current_namespace {
            if let Some(namespace) = next_namespace {
                nested_tool_sections.push(format!(
                    "## {namespace}\n{}",
                    default_namespace_description(namespace)
                ));
            }
            current_namespace = next_namespace;
        }
        let global_name = code_runtime::normalize_code_mode_identifier(&tool.name);
        let heading = if global_name == tool.name {
            format!("### `{global_name}`")
        } else {
            format!("### `{global_name}` (`{}`)", tool.name)
        };
        nested_tool_sections.push(format!(
            "{heading}\n{}",
            render_catalogue_sample_for_definition(tool).trim()
        ));
    }
    let nested_tools = nested_tool_sections.join("\n\n");
    format!(
        "{exec_description}\n\nThe following TypeScript blocks are `exec` tool declarations.\n\n{nested_tools}"
    )
}

fn render_code_mode_sample_for_definition(tool: &ToolDefinition) -> String {
    let declaration = render_code_mode_declaration(tool, None);
    format!(
        "{}\n\nexec tool declaration:\n```ts\n{declaration}\n```",
        tool.description
    )
}

fn render_catalogue_sample_for_definition(tool: &ToolDefinition) -> String {
    let uses_command_result = tool.tool_name.namespace.is_none()
        && matches!(tool.name.as_str(), "exec_command" | "write_stdin");
    let declaration =
        render_code_mode_declaration(tool, uses_command_result.then_some("CommandResult"));
    let type_declaration = if tool.tool_name.namespace.is_none() && tool.name == "exec_command" {
        let output_type = tool
            .output_schema
            .as_ref()
            .map(code_runtime::render_json_schema_to_typescript)
            .unwrap_or_else(|| "unknown".to_string());
        format!("type CommandResult = {output_type};\n\n")
    } else {
        String::new()
    };
    format!(
        "{}\n\n```ts\n{type_declaration}{declaration}\n```",
        tool.description
    )
}

fn render_code_mode_declaration(
    tool: &ToolDefinition,
    output_type_override: Option<&str>,
) -> String {
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
    let output_type = output_type_override.map(str::to_owned).unwrap_or_else(|| {
        tool.output_schema
            .as_ref()
            .map(code_runtime::render_json_schema_to_typescript)
            .unwrap_or_else(|| "unknown".to_string())
    });
    let name = code_runtime::normalize_code_mode_identifier(&tool.name);
    format!(
        "declare const tools: {{ {name}({input_name}: {input_type}): Promise<{output_type}>; }};"
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
    output_schema: Option<Value>,
) -> ToolDefinition {
    ToolDefinition {
        name: javascript_name.to_string(),
        tool_name: ToolName::namespaced(namespace, name),
        description: description.to_string(),
        kind: ToolKind::Function,
        input_schema: Some(input_schema),
        output_schema,
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
                "description": EXEC_LOGIN_DESCRIPTION
            },
            "tty": {
                "type": "boolean",
                "description": EXEC_TTY_DESCRIPTION
            },
            "yield_time_ms": {
                "type": "number",
                "description": EXEC_YIELD_TIME_DESCRIPTION
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
