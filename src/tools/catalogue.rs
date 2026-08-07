//! Fixed JavaScript tool catalogue ported from Codex's `code_mode_only` plan at
//! `1669c2403f793d0230065397dfc25f52b844244e` and rechecked against upstream at
//! `3b366654f1de1b77587ffb026c8f35507f3fe4ef`. bettercodex exposes this one
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
use codex_protocol::ToolName;
use serde_json::Value;
use serde_json::json;
use std::sync::LazyLock;

const EXEC_SOURCE_GRAMMAR: &str = r#"start: SOURCE
SOURCE: /[\s\S]+/"#;
const EXEC_RUNTIME_GUIDANCE: &str = r#"Input raw JavaScript directly (no JSON, string, or Markdown wrapper) into fresh V8: top-level `await`; no Node.js/filesystem/network/console. Call `await tools.name(args)`; errors reject. Use `Promise.all` for independent calls and await all work. Emit with `text(value)`/`image(item,detail?)`; `notify` is interim; `yield_control` yields while code continues; `store`/`load` persist serializable values across cells; `exit`, `setTimeout`, and `clearTimeout` exist. Optional first line `// @exec:{"yield_time_ms":10000,"max_output_tokens":1000}`; both default 10000."#;
const TOOL_DEFAULTS: &str = "Defaults: command cwd=turn, shell=user, `login:true`, `tty:false`, yield=10s; stdin yield=.25s after writes/5s polling; output=10k tokens; image detail=`high`. Yields clamp to .25–30s (poll 5–300s). Process: `output`+`wall_time_seconds` always; `session_id`=running, `exit_code`=done, `original_token_count`=before truncation, `chunk_id`=output chunk.";
const WAIT_DESCRIPTION: &str = "Continue yielded `exec` by `cell_id`; returns only new output. Repeat while active; `terminate:true` stops. `yield_time_ms`/`max_tokens` default 10000.";

static CORE_TOOLS: LazyLock<Vec<ToolDefinition>> = LazyLock::new(|| {
    vec![
        freeform_tool(
            "apply_patch",
            "Validates the whole patch before editing. Pass the patch string directly; paths use turn cwd, not `exec_command.workdir`; absolute paths work.",
        ),
        function_tool(
            "exec_command",
            "Runs shell. Long commands return `session_id` for `write_stdin`; `tty:true` keeps stdin writable.",
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
        namespaced_function_tool(
            crate::web_search::JAVASCRIPT_NAME,
            crate::web_search::NAMESPACE,
            crate::web_search::TOOL_NAME,
            crate::web_search::DESCRIPTION,
            crate::web_search::input_schema().clone(),
            Some(json!({"type": "string"})),
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
                "description": "True runs the shell with -l/-i semantics; false disables them. Defaults to true."
            },
            "tty": {
                "type": "boolean",
                "description": "True allocates a PTY with TERM=xterm-256color; false or omitted uses plain pipes."
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
        assert!(text.starts_with("Input raw JavaScript directly"));
        assert_eq!(text.matches("declare const tools:").count(), 1);
        for declaration in [
            "apply_patch(input: string): Promise<{}>",
            "exec_command(args: {cmd:string",
            "log_papercut(args: {message:string}): Promise<{path:string}>",
            "update_plan(args: {explanation?:string;plan:Array<",
            "view_image(args: {detail?:\"high\"|\"original\";path:string}",
            "write_stdin(args: {chars?:string;max_output_tokens?:number;session_id:number",
            "web__run(args: {click?:Array<",
            "): Promise<string>",
        ] {
            assert!(
                text.contains(declaration),
                "missing `{declaration}` in {text}"
            );
        }
        let declarations = text.split_once("```ts\n").unwrap().1;
        assert!(!declarations.contains("//"));
        assert!(!text.contains("exec tool declaration"));
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
            "`store`/`load` persist serializable values across cells",
            "`yield_control` yields while code continues",
            "`tty:true` keeps stdin writable",
            "never if forbidden",
            "official only unless asked otherwise",
            "Technical: primary sources",
            "direct descriptive Markdown links",
            "Internal IDs (for example `turn2search5`) are call-only",
            "native cite markers in final answers",
            "`pageno` is zero-based",
            "dates use YYYY-MM-DD",
            "UTC offsets like `+03:00`",
            "`[wordlim N]`",
            "at most 25 non-lyric/10 lyric words",
        ] {
            assert!(text.contains(retained), "missing `{retained}` in {text}");
        }
    }

    #[test]
    fn compact_schema_rendering_preserves_string_literals() {
        assert_eq!(
            render_compact_schema(&json!({
                "type": "object",
                "properties": {
                    "label": {
                        "type": "string",
                        "enum": [
                            "two words",
                            "x,y",
                            "say \"hi\"",
                            "c:\\tmp",
                            "https://example.com/a // b"
                        ],
                        "description": "Removed from the declaration."
                    }
                },
                "required": ["label"],
                "additionalProperties": false
            })),
            r#"{label:"two words"|"x,y"|"say \"hi\""|"c:\\tmp"|"https://example.com/a // b"}"#
        );
    }

    #[test]
    fn compact_schema_rendering_preserves_properties_named_description() {
        assert_eq!(
            render_compact_schema(&json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "Documentation for a real argument."
                    },
                    "nested": {
                        "type": "object",
                        "properties": {
                            "description": {
                                "type": "number",
                                "description": "Documentation for a nested argument."
                            }
                        },
                        "additionalProperties": false
                    }
                },
                "required": ["description"],
                "additionalProperties": false
            })),
            "{description:string;nested?:{description?:number}}"
        );
    }

    #[test]
    fn readable_catalogue_snapshot_matches_the_request() {
        assert_eq!(
            text().trim_end(),
            include_str!("../../prompts/tool-catalogue.md").trim_end()
        );
    }

    #[test]
    fn agent_context_snapshot_matches_the_stable_request_context() {
        fn excerpt<'a>(document: &'a str, start: &str, end: &str) -> &'a str {
            document
                .split_once(start)
                .unwrap_or_else(|| panic!("missing snapshot marker `{start}`"))
                .1
                .split_once(end)
                .unwrap_or_else(|| panic!("missing snapshot marker `{end}`"))
                .0
        }

        let snapshot = include_str!("../../AGENT_CONTEXT.md");
        assert_eq!(
            excerpt(
                snapshot,
                "````text\n",
                "\n````\n\n## 2. `additional_tools` input item",
            ),
            crate::api::harness_instructions()
        );

        let mut exec: Value = serde_json::from_str(excerpt(
            snapshot,
            "### 2.1 `exec`\n\nWire fields and grammar:\n\n```json\n",
            "\n```\n\nExact `description` text:",
        ))
        .expect("exec snapshot must be valid JSON");
        assert_eq!(exec["description"], "Exact text shown below");
        exec["description"] = Value::String(text().to_string());
        assert_eq!(exec, specifications()[0]);
        assert_eq!(
            excerpt(
                snapshot,
                "Exact `description` text:\n\n````markdown\n",
                "\n````\n\n### 2.2 `wait`",
            ),
            text()
        );

        let wait: Value = serde_json::from_str(excerpt(
            snapshot,
            "### 2.2 `wait`\n\nComplete wire definition:\n\n```json\n",
            "\n```\n\n## 3. Environment context",
        ))
        .expect("wait snapshot must be valid JSON");
        assert_eq!(wait, specifications()[1]);
    }

    #[test]
    fn request_exposes_only_exec_and_wait() {
        let tools = specifications();
        let names = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names, ["exec", "wait"]);
        assert_eq!(
            tools[0]["format"]["definition"],
            "start: SOURCE\nSOURCE: /[\\s\\S]+/"
        );
        assert_eq!(
            tools[1]["parameters"],
            json!({
                "type": "object",
                "properties": {
                    "cell_id": {"type": "string"},
                    "yield_time_ms": {"type": "number"},
                    "max_tokens": {"type": "number"},
                    "terminate": {"type": "boolean"}
                },
                "required": ["cell_id"],
                "additionalProperties": false
            })
        );
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
        assert_eq!(text().len(), 4_353, "{update}");
        assert_eq!(
            serde_json::to_string(&tools[0]).unwrap().len(),
            4_577,
            "{update}"
        );
        assert_eq!(
            serde_json::to_string(&tools[1]).unwrap().len(),
            438,
            "{update}"
        );
        let item = json!({
            "type": "additional_tools",
            "role": "developer",
            "tools": tools,
        });
        assert_eq!(
            serde_json::to_string(&item).unwrap().len(),
            5_073,
            "{update}"
        );
        assert_eq!(
            metrics(),
            CatalogueMetrics {
                description_bytes: 4_353,
                request_bytes: 5_073,
                estimated_tokens: 1_269,
            }
        );
    }
}
