use super::ToolCall;
use super::ToolResult;
use serde_json::json;

#[test]
fn parses_exec_and_wait_calls() {
    assert_eq!(
        ToolCall::from_response_item(&json!({
            "type": "custom_tool_call",
            "call_id": "call-1",
            "name": "exec",
            "input": "text('done')"
        })),
        Some(ToolCall::Custom {
            call_id: "call-1".to_string(),
            name: "exec".to_string(),
            input: "text('done')".to_string(),
        })
    );
    assert_eq!(
        ToolCall::from_response_item(&json!({
            "type": "function_call",
            "call_id": "call-2",
            "name": "wait",
            "arguments": "{\"cell_id\":\"cell-1\"}"
        })),
        Some(ToolCall::Function {
            call_id: "call-2".to_string(),
            name: "wait".to_string(),
            arguments: "{\"cell_id\":\"cell-1\"}".to_string(),
        })
    );
}

#[test]
fn custom_outputs_preserve_structured_content_items() {
    let call = ToolCall::Custom {
        call_id: "call-1".to_string(),
        name: "exec".to_string(),
        input: "text('done')".to_string(),
    };
    let output = ToolResult {
        body: json!([{"type": "input_text", "text": "done"}]),
        preview: "done".to_string(),
        preceding_items: Vec::new(),
    };

    assert_eq!(
        call.output_items(&output),
        vec![json!({
            "type": "custom_tool_call_output",
            "call_id": "call-1",
            "output": [{"type": "input_text", "text": "done"}],
        })]
    );
}
