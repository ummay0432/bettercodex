use super::*;

fn message(role: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{"type": "input_text", "text": text}],
    })
}

#[test]
fn remote_v2_retains_only_recent_user_and_non_completion_agent_messages() {
    let user = message("user", "real user request");
    let reserved_prefix_user = message(
        "user",
        "<skill_context> is the literal syntax I need you to investigate",
    );
    let delegated = json!({
        "type": "agent_message",
        "author": "worker",
        "recipient": "root",
        "content": [{"type": "input_text", "text": "child completion"}],
    });
    let compaction =
        json!({"type": "compaction_summary", "id": "cmp_new", "encrypted_content": "opaque"});
    let history = vec![
        message("developer", "system prompt"),
        message(
            "user",
            "# Repository onboarding from AGENTS.md for /repo\n\nstale instructions\n# End repository onboarding",
        ),
        message("user", "<turn_aborted>\ninterrupted\n</turn_aborted>"),
        message(
            "user",
            "<response_interrupted>\nretry\n</response_interrupted>",
        ),
        reserved_prefix_user.clone(),
        user.clone(),
        json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "interim output is summarized"}],
            "phase": "commentary",
        }),
        json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "final output is summarized"}],
            "phase": "final_answer",
        }),
        delegated.clone(),
        json!({
            "type": "agent_message",
            "author": "root",
            "recipient": "user",
            "content": [{"type": "input_text", "text": "Message Type: FINAL_ANSWER\nfinished"}],
        }),
        json!({"type": "reasoning", "encrypted_content": "reasoning"}),
        json!({"type": "custom_tool_call", "call_id": "call_1", "name": "exec", "input": "text(true)"}),
        json!({"type": "custom_tool_call_output", "call_id": "call_1", "name": "exec", "output": "true"}),
        json!({"type": "compaction", "id": "cmp_old", "encrypted_content": "old"}),
    ];

    assert_eq!(
        build_compacted_history(&history, compaction.clone()),
        vec![reserved_prefix_user, user, delegated, compaction]
    );
}

#[test]
fn retained_history_budget_keeps_the_newest_messages() {
    let newest = message("user", "new");
    let retained = vec![
        message("user", "old-old"),
        message("user", "middle1234"),
        newest.clone(),
    ];

    assert_eq!(
        truncate_retained_messages(retained, 3),
        vec![message("user", "midd…1 tokens truncated…1234"), newest,]
    );
}

#[test]
fn multimodal_inputs_match_upstream_text_only_retention_budgeting() {
    let images = (0..40).map(|_| {
        json!({
            "type": "input_image",
            "image_url": "data:image/png;base64,AAAA",
            "detail": "low",
        })
    });
    let mut content = vec![json!({"type": "input_text", "text": "inspect every image"})];
    content.extend(images);
    let original = json!({
        "type": "message",
        "role": "user",
        "content": content,
    });
    let budget = 10_000;

    let retained = truncate_retained_messages(vec![original], budget);
    assert_eq!(retained.len(), 1);
    let retained_images = retained[0]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "input_image")
        .count();
    assert_eq!(retained_images, 40);
    assert!(message_text_token_count(&retained[0]) <= budget);
}

#[test]
fn oversized_trailing_tool_outputs_are_rewritten_before_compaction() {
    let call = json!({
        "type": "custom_tool_call",
        "id": "ctc_1",
        "call_id": "call_1",
        "name": "exec",
        "input": "text(true)",
    });
    let mut history = vec![
        message("user", "keep"),
        call.clone(),
        json!({
            "type": "custom_tool_call_output",
            "id": "ctco_1",
            "call_id": "call_1",
            "name": "exec",
            "output": "x".repeat(4_000),
        }),
    ];
    let max_tokens = estimated_tokens(&history[..2]);

    assert_eq!(trim_tool_outputs_to_fit(&mut history, max_tokens), 1);
    assert_eq!(history[1], call);
    assert_eq!(
        history[2]["output"],
        CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE
    );
}

#[test]
fn compaction_output_validation_ignores_other_output_items_but_requires_one_summary() {
    let compaction = json!({"type": "compaction", "encrypted_content": "opaque"});
    let outputs = vec![message("assistant", "ignored"), compaction.clone()];

    assert_eq!(opaque_compaction_item(&outputs), Ok(compaction));
    assert!(
        opaque_compaction_item(&[message("assistant", "missing")])
            .unwrap_err()
            .contains("exactly one")
    );
    assert!(
        opaque_compaction_item(&[
            json!({"type": "compaction", "encrypted_content": "one"}),
            json!({"type": "compaction_summary", "encrypted_content": "two"}),
        ])
        .unwrap_err()
        .contains("got 2")
    );
    for malformed in [
        json!({"type": "compaction"}),
        json!({"type": "compaction", "encrypted_content": ""}),
        json!({"type": "compaction", "encrypted_content": "  \n"}),
        json!({"type": "compaction", "encrypted_content": null}),
    ] {
        assert!(
            opaque_compaction_item(&[malformed])
                .unwrap_err()
                .contains("non-empty encrypted_content")
        );
    }
}
