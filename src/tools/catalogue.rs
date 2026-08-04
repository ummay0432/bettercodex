//! Fixed Code Mode tool plan ported from Codex at
//! `1669c2403f793d0230065397dfc25f52b844244e`.
//!
//! The schemas mirror `core/src/tools/handlers/{apply_patch_spec,plan_spec,
//! shell_spec,view_image_spec}.rs`; the `exec` and `wait` wrappers mirror
//! `core/src/tools/code_mode/{execute_spec,wait_spec}.rs`. The shared renderer
//! remains in `codex-code-mode-protocol` so model-facing TypeScript generation
//! cannot drift into a BetterCodex-specific format.

use super::code_runtime;
use super::code_runtime::CodeModeToolKind;
use super::code_runtime::ToolDefinition;
use codex_protocol::ToolName;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::LazyLock;

const CODE_MODE_FREEFORM_GRAMMAR: &str = r#"
start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE

PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;

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
    ]
});

static EXEC_DESCRIPTION: LazyLock<String> = LazyLock::new(|| {
    code_runtime::build_exec_tool_description(
        &CORE_TOOLS,
        &[],
        &BTreeMap::new(),
        code_runtime::DEFAULT_EXEC_YIELD_TIME_MS,
        true,
    )
});

pub(super) fn core_tools() -> &'static [ToolDefinition] {
    &CORE_TOOLS
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
                "definition": CODE_MODE_FREEFORM_GRAMMAR,
            }
        }),
        json!({
            "type": "function",
            "name": code_runtime::WAIT_TOOL_NAME,
            "description": format!(
                "Waits on a yielded `exec` cell and returns new output or completion.\n{}",
                code_runtime::build_wait_tool_description().trim(),
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
        }),
    ]
}

pub(crate) fn code_mode_tool_names() -> Value {
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
        kind: CodeModeToolKind::Function,
        input_schema: Some(input_schema),
        output_schema,
    }
}

fn freeform_tool(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        tool_name: ToolName::plain(name),
        description: description.to_string(),
        kind: CodeModeToolKind::Freeform,
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
    fn catalogue_contains_only_the_fixed_core_tools() {
        assert_eq!(
            core_tools()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [
                "apply_patch",
                "exec_command",
                "update_plan",
                "view_image",
                "write_stdin",
            ]
        );
    }

    #[test]
    fn model_visible_catalogue_contains_typed_declarations() {
        let text = text();
        assert!(text.contains("fresh V8 isolate"));
        assert!(text.contains("### `exec_command`"));
        assert!(text.contains("exec_command(args:"));
        assert!(text.contains("Promise<{"));
        assert!(text.contains("### `apply_patch`"));
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
    fn documented_tool_context_byte_counts_do_not_drift() {
        let tools = specifications();
        assert_eq!(text().len(), 7_307, "update prompts/tool-context.md");
        assert_eq!(
            serde_json::to_string(&tools[0]).unwrap().len(),
            7_783,
            "update prompts/tool-context.md"
        );
        assert_eq!(
            serde_json::to_string(&tools[1]).unwrap().len(),
            1_356,
            "update prompts/tool-context.md"
        );
        let item = json!({
            "type": "additional_tools",
            "role": "developer",
            "tools": tools,
        });
        assert_eq!(
            serde_json::to_string(&item).unwrap().len(),
            9_197,
            "update prompts/tool-context.md"
        );
    }
}
