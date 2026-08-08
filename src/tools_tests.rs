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
    let payload_allocation = output.body[0]["text"].as_str().unwrap().as_ptr();
    let items = call.into_output_items(output);

    assert_eq!(
        items,
        vec![json!({
            "type": "custom_tool_call_output",
            "call_id": "call-1",
            "output": [{"type": "input_text", "text": "done"}],
        })]
    );
    assert_eq!(
        items[0]["output"][0]["text"].as_str().unwrap().as_ptr(),
        payload_allocation,
        "tool payloads must move into history instead of being deep-cloned"
    );
}

#[test]
fn direct_tool_errors_are_bounded_before_history_insertion() {
    let result = ToolResult::text("x".repeat(50_000));

    assert!(result.preview.starts_with("Warning: truncated output"));
    assert!(result.preview.len() < 50_000);
}

#[test]
fn view_image_rejects_oversized_files_before_loading_them() {
    let cwd = std::env::temp_dir().join(format!(
        "bettercodex-view-image-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&cwd).unwrap();
    let path = cwd.join("oversized.png");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(u64::try_from(crate::input::MAX_TOTAL_IMAGE_BYTES).unwrap() + 1)
        .unwrap();

    let error = super::view_image(&cwd, json!({"path": "oversized.png"})).unwrap_err();

    assert!(error.to_string().contains("50 MiB view_image limit"));
    std::fs::remove_dir_all(cwd).unwrap();
}
