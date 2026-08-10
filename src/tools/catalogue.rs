//! Fixed JavaScript tool catalogue ported from Codex's `code_mode_only` plan at
//! `1669c2403f793d0230065397dfc25f52b844244e` and rechecked against upstream at
//! `a16863f8704831d13e041ed7dba2c4a57a2a940b`. bettercodex exposes this one
//! execution path unconditionally; it has no tool-mode selector.
//!
//! The schemas mirror `core/src/tools/handlers/{apply_patch_spec,plan_spec,
//! shell_spec,view_image_spec}.rs`; the `exec` and `wait` wrappers mirror
//! `core/src/tools/code_mode/{execute_spec,wait_spec}.rs`. Upstream renders a
//! prose section and complete declaration wrapper per tool because its surface
//! is dynamic. bettercodex's fixed surface instead renders one concise guide
//! and one schema-derived declaration block, retaining the callable contract
//! without repeating renderer scaffolding or JSON Schema descriptions.

use super::code_runtime;
use super::code_runtime::CodeModeToolKind as ToolKind;
use super::code_runtime::ToolDefinition;
use crate::protocol::ToolName;
use serde_json::Value;
use serde_json::json;
use std::sync::LazyLock;

const EXEC_SOURCE_GRAMMAR: &str = r#"start: SOURCE
SOURCE: /[\s\S]+/"#;
// Upstream also advertises `generatedImage` and `ALL_TOOLS`. bettercodex has no
// MCP or image-generation integrations, and its fixed nested surface is fully
// declared below, so those compatibility globals are deliberately omitted from
// model-facing guidance.
const EXEC_RUNTIME_GUIDANCE: &str = r#"Input raw JavaScript directly (no JSON, string, or Markdown wrapper) into fresh V8: top-level `await`; no Node.js/filesystem/network/console. Call `await tools.name(args)`; errors reject. Use `Promise.all` for independent calls and await all work. Emit with `text(value)`/`image(item,detail?)`; `notify` is interim; `yield_control` yields while code continues; `store`/`load` persist serializable values across cells; `exit`, `setTimeout`, and `clearTimeout` exist. Optional first line `// @exec:{"yield_time_ms":10000,"max_output_tokens":1000}`; both default 10000."#;
const TOOL_DEFAULTS: &str = "Defaults: command cwd=turn, shell=user, `login:true`, `tty:false`, yield=10s; stdin yield=.25s after writes/5s polling; outer exec/wait output=10k tokens; nested command output=collected raw unless `max_output_tokens` is set; image detail=`high`. Nested command and non-empty write yields clamp to .25–30s; empty polls to 5–300s; top-level `exec`/`wait` yields are not clamped. Process: `output`+`wall_time_seconds` always; `session_id`=running, `exit_code`=done, `original_token_count`=before truncation, `chunk_id`=output chunk.";
const WAIT_DESCRIPTION: &str = "Wait on a yielded top-level `exec` cell using its string `cell_id`; process `session_id` values from `exec_command` belong to `tools.write_stdin`. Returns only new output; repeat while active or use `terminate:true` to stop. `yield_time_ms`/`max_tokens` default 10000.";
#[cfg(not(windows))]
const EXEC_COMMAND_DESCRIPTION: &str = "Runs shell. Long commands return `session_id` for `write_stdin`; `tty:true` keeps stdin writable.";
#[cfg(windows)]
const EXEC_COMMAND_DESCRIPTION: &str = r#"Runs PowerShell by default on native Windows. Long commands return `session_id` for `write_stdin`; `tty:true` keeps stdin writable.

Examples of valid PowerShell command strings:
- ls -a (show hidden): `Get-ChildItem -Force`
- recursive find by name: `Get-ChildItem -Recurse -Filter *.py`
- recursive grep: `Get-ChildItem -Path C:\myrepo -Recurse | Select-String -Pattern 'TODO' -CaseSensitive`
- ps aux | grep python: `Get-Process | Where-Object { $_.ProcessName -like '*python*' }`
- setting an env var: `$env:FOO='bar'; echo $env:FOO`
- running an inline Python script: `@'\nprint('Hello, world!')\n'@ | python -`

Windows safety rules:
- Do not compose destructive filesystem commands across shells. Do not enumerate paths in PowerShell and then pass them to `cmd /c`, batch builtins, or another shell for deletion or moving. Use one shell end-to-end, prefer native PowerShell cmdlets such as `Remove-Item` / `Move-Item` with `-LiteralPath`, and avoid string-built shell commands for file operations.
- Before any recursive delete or move on Windows, verify the resolved absolute target paths stay within the intended workspace or explicitly named target directory. Never issue a recursive delete or move against a computed path if the final target has not been checked.
- When using `Start-Process` to launch a background helper or service, pass `-WindowStyle Hidden` unless the user explicitly asked for a visible interactive window. Use visible windows only for interactive tools the user needs to see or control."#;

#[cfg(not(windows))]
const EXEC_LOGIN_DESCRIPTION: &str =
    "True runs the shell with -l/-i semantics; false disables them. Defaults to true.";
#[cfg(windows)]
const EXEC_LOGIN_DESCRIPTION: &str =
    "True loads the PowerShell profile; false passes -NoProfile. Defaults to true.";

#[cfg(not(windows))]
const EXEC_TTY_DESCRIPTION: &str =
    "True allocates a PTY with TERM=xterm-256color; false or omitted uses plain pipes.";
#[cfg(windows)]
const EXEC_TTY_DESCRIPTION: &str =
    "True allocates a ConPTY terminal; false or omitted uses plain pipes.";

static CORE_TOOLS: LazyLock<Vec<ToolDefinition>> = LazyLock::new(|| {
    let mut tools = vec![
        freeform_tool(
            "apply_patch",
            "Validates the whole patch before editing. Pass the patch string directly; paths use turn cwd, not `exec_command.workdir`; absolute paths work.",
        ),
        function_tool(
            "exec_command",
            EXEC_COMMAND_DESCRIPTION,
            exec_command_input_schema(),
            Some(unified_exec_output_schema()),
        ),
        function_tool(
            "log_papercut",
            "Appends one repository-root `PAPERCUTS.md` note: 1–2 sentences on friction and likely fix.",
            log_papercut_input_schema(),
            Some(log_papercut_output_schema()),
        ),
        function_tool(
            "update_plan",
            "Replaces plan; allows one `in_progress` step.",
            update_plan_input_schema(),
            Some(empty_object_output_schema()),
        ),
        function_tool(
            "view_image",
            "Loads a local image.",
            view_image_input_schema(),
            Some(view_image_output_schema()),
        ),
        function_tool(
            "write_stdin",
            "Writes `chars` or, when omitted, polls an `exec_command` session; returns new output.",
            write_stdin_input_schema(),
            Some(unified_exec_output_schema()),
        ),
    ];
    tools.extend(crate::openai_docs::TOOLS.iter().copied().map(|tool| {
        namespaced_function_tool(
            tool.javascript_name(),
            crate::openai_docs::NAMESPACE,
            tool.name(),
            tool.description(),
            tool.input_schema(),
            Some(json!({"type": "string"})),
        )
    }));
    tools.push(namespaced_function_tool(
        crate::web_search::JAVASCRIPT_NAME,
        crate::web_search::NAMESPACE,
        crate::web_search::TOOL_NAME,
        crate::web_search::DESCRIPTION,
        crate::web_search::input_schema().clone(),
        Some(json!({"type": "string"})),
    ));
    tools
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

static EXEC_DESCRIPTION: LazyLock<String> = LazyLock::new(build_exec_description);

pub(super) fn core_tools() -> &'static [ToolDefinition] {
    &CORE_TOOLS
}

pub(super) fn runtime_tools() -> &'static [ToolDefinition] {
    &RUNTIME_TOOLS
}

pub(crate) fn text() -> &'static str {
    &EXEC_DESCRIPTION
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
                    "cell_id": {"type": "string"},
                    "yield_time_ms": {"type": "number"},
                    "max_tokens": {"type": "number"},
                    "terminate": {"type": "boolean"}
                },
                "required": ["cell_id"],
                "additionalProperties": false
            }
        }),
    ]
}

fn build_exec_description() -> String {
    let process_output_schema = unified_exec_output_schema();
    let tool_reference = core_tools()
        .iter()
        .map(render_tool_reference)
        .collect::<Vec<_>>()
        .join("\n");
    let declarations = core_tools()
        .iter()
        .map(|tool| render_tool_signature(tool, &process_output_schema))
        .collect::<Vec<_>>()
        .join("\n");
    let process_result = render_compact_schema(&process_output_schema);
    format!(
        "{EXEC_RUNTIME_GUIDANCE}\n\nTools:\n{tool_reference}\n\n{TOOL_DEFAULTS}\n\n```ts\ntype ProcessResult = {process_result};\ndeclare const tools: {{\n{declarations}\n}};\n```"
    )
}

fn render_tool_reference(tool: &ToolDefinition) -> String {
    let name = match tool.tool_name.namespace.as_deref() {
        Some(namespace) => format!("`{}` (`{namespace}.{}`)", tool.name, tool.tool_name.name),
        None => format!("`{}`", tool.name),
    };
    let description = tool
        .description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!("- {name}: {description}")
}

fn render_tool_signature(tool: &ToolDefinition, process_output_schema: &Value) -> String {
    let (input_name, input_type) = match tool.kind {
        ToolKind::Function => (
            "args",
            tool.input_schema
                .as_ref()
                .map(render_compact_schema)
                .unwrap_or_else(|| "unknown".to_string()),
        ),
        ToolKind::Freeform => ("input", "string".to_string()),
    };
    let output_type = match tool.output_schema.as_ref() {
        Some(schema) if schema == process_output_schema => "ProcessResult".to_string(),
        Some(schema) => render_compact_schema(schema),
        None => "unknown".to_string(),
    };
    let name = code_runtime::normalize_code_mode_identifier(&tool.name);
    format!("  {name}({input_name}: {input_type}): Promise<{output_type}>;")
}

fn render_compact_schema(schema: &Value) -> String {
    // Upstream renders schema annotations as line comments. Strip those from
    // the rendered declaration so argument properties named `description`
    // remain part of the schema.
    compact_typescript(&code_runtime::render_json_schema_to_typescript(schema))
}

fn compact_typescript(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut characters = source.chars().peekable();
    let mut previous = None;
    let mut pending_whitespace = false;
    let mut in_string = false;
    let mut escaped = false;
    while let Some(character) = characters.next() {
        if in_string {
            output.push(character);
            previous = Some(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        if character == '/' && characters.peek() == Some(&'/') {
            characters.next();
            for comment_character in characters.by_ref() {
                if matches!(comment_character, '\n' | '\r') {
                    break;
                }
            }
            pending_whitespace = true;
            continue;
        }

        if character.is_whitespace() {
            pending_whitespace = true;
            continue;
        }

        if pending_whitespace
            && previous.is_some_and(|character| !is_compact_typescript_punctuation(character))
            && !is_compact_typescript_punctuation(character)
        {
            output.push(' ');
        }
        if character == '}' && previous == Some(';') {
            output.pop();
        }
        output.push(character);
        previous = Some(character);
        pending_whitespace = false;
        in_string = character == '"';
    }
    output
}

fn is_compact_typescript_punctuation(character: char) -> bool {
    matches!(character, '{' | '}' | ':' | ';' | ',' | '|' | '&')
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
        output_schema: Some(empty_object_output_schema()),
    }
}

fn empty_object_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
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
                "description": "Wait before yielding output. Defaults to 10000 ms; effective range is 250-30000 ms."
            },
            "max_output_tokens": {
                "type": "number",
                "description": "Optional output token budget for this command result. Omit it to preserve collected output for JavaScript processing."
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
                "description": "Optional output token budget for this process result. Omit it to preserve collected output for JavaScript processing."
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
