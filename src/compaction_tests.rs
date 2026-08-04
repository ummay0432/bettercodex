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
            "# Repository onboarding from AGENTS.md for /repo\n\nstale instructions",
        ),
        user.clone(),
        message("assistant", "assistant output is summarized"),
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
        vec![user, delegated, compaction]
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
}
