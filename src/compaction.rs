use crate::context::estimated_tokens;
use crate::context::is_contextual_user_message;
use crate::truncation::approx_token_count;
use crate::truncation::truncate_text;
use serde_json::Value;
use serde_json::json;

// These match Codex's remote compaction v2 retention policy. The opaque
// compaction item summarizes everything else in the discarded transcript.
const RETAINED_MESSAGE_TOKEN_BUDGET: usize = 64_000;
const MAX_RETAINED_AGENT_MESSAGE_TOKENS: u64 = 10_000;
const CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE: &str =
    "Output exceeded the available model context and was truncated";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompactionPhase {
    PreTurn,
    MidTurn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompactionRequest {
    Automatic(CompactionPhase),
    Manual,
}

impl CompactionRequest {
    pub(crate) fn trigger(self) -> &'static str {
        match self {
            Self::Automatic(_) => "auto",
            Self::Manual => "manual",
        }
    }

    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Automatic(_) => "context_limit",
            Self::Manual => "user_requested",
        }
    }

    pub(crate) fn phase(self) -> &'static str {
        match self {
            Self::Automatic(CompactionPhase::PreTurn) => "pre_turn",
            Self::Automatic(CompactionPhase::MidTurn) => "mid_turn",
            Self::Manual => "standalone_turn",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InitialContextInjection {
    AfterCompaction,
    BeforeLastUserMessage,
}

pub(crate) fn compaction_trigger() -> Value {
    json!({"type": "compaction_trigger"})
}

pub(crate) fn opaque_compaction_item(
    items: Vec<Value>,
    compaction_item_count: usize,
    output_item_count: usize,
) -> Result<Value, String> {
    if compaction_item_count != 1 {
        return Err(format!(
            "remote compaction v2 expected exactly one compaction output item, got {compaction_item_count} from {output_item_count} output items"
        ));
    }
    let compaction = items.into_iter().next().ok_or_else(|| {
        format!(
            "remote compaction v2 expected exactly one compaction output item, got {compaction_item_count} from {output_item_count} output items"
        )
    })?;
    let valid = matches!(
        compaction.get("type").and_then(Value::as_str),
        Some("compaction" | "compaction_summary")
    ) && compaction
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|encrypted| !encrypted.trim().is_empty());
    if !valid {
        return Err(
            "remote compaction v2 output omitted its non-empty encrypted_content".to_string(),
        );
    }
    // The opaque item is a continuation token. Preserve every returned field exactly so the next
    // request replays the server-owned state without client-side reinterpretation.
    Ok(compaction)
}

pub(crate) fn retained_compacted_history(prompt_history: Vec<Value>) -> Vec<Value> {
    let retained = prompt_history
        .into_iter()
        .filter(is_retained_for_remote_compaction_v2)
        .filter(should_keep_compacted_history_item)
        .collect::<Vec<_>>();
    truncate_retained_messages(retained, RETAINED_MESSAGE_TOKEN_BUDGET)
}

pub(crate) fn trim_tool_outputs_to_fit(history: &mut [Value], max_tokens: u64) -> usize {
    let mut estimated = estimated_tokens(history);
    let mut rewritten = 0_usize;

    for item in history.iter_mut().rev() {
        if estimated <= max_tokens {
            break;
        }
        if let Some(original_tokens) = rewrite_output_for_context_window(item) {
            estimated = estimated
                .saturating_sub(original_tokens)
                .saturating_add(estimated_tokens(std::slice::from_ref(item)));
            rewritten = rewritten.saturating_add(1);
            continue;
        }
        // Recovery and other harness context can trail completed outputs without starting a new
        // model turn. Scan across those items so an interrupted saved session remains compactable,
        // but stop at every ordinary history boundary.
        if !is_contextual_user_message(item) {
            break;
        }
    }
    rewritten
}

fn rewrite_output_for_context_window(item: &mut Value) -> Option<u64> {
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("function_call_output" | "custom_tool_call_output")
    ) {
        return None;
    }
    let original_tokens = estimated_tokens(std::slice::from_ref(item));
    item["output"] = Value::String(CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE.to_string());
    Some(original_tokens)
}

fn is_retained_for_remote_compaction_v2(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => {
            !is_descendant_agent_progress(item)
                && !is_agent_completion(item)
                && estimated_tokens(std::slice::from_ref(item)) <= MAX_RETAINED_AGENT_MESSAGE_TOKENS
        }
        Some("message") => matches!(
            item.get("role").and_then(Value::as_str),
            Some("user" | "developer" | "system")
        ),
        _ => false,
    }
}

fn should_keep_compacted_history_item(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => {
            item.get("role").and_then(Value::as_str) == Some("user")
                && !is_contextual_user_message(item)
        }
        Some("agent_message") => true,
        _ => false,
    }
}

fn is_descendant_agent_progress(item: &Value) -> bool {
    let Some(author) = item.get("author").and_then(Value::as_str) else {
        return false;
    };
    let Some(recipient) = item.get("recipient").and_then(Value::as_str) else {
        return false;
    };
    author
        .strip_prefix(recipient)
        .is_some_and(|suffix| suffix.starts_with('/'))
        && item
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.starts_with("Message Type: MESSAGE\n"))
}

fn is_agent_completion(item: &Value) -> bool {
    item.pointer("/content/0/text")
        .and_then(Value::as_str)
        .is_some_and(|text| text.starts_with("Message Type: FINAL_ANSWER\n"))
}

fn truncate_retained_messages(items: Vec<Value>, max_tokens: usize) -> Vec<Value> {
    let mut remaining = max_tokens;
    let mut truncated_reversed = Vec::with_capacity(items.len());
    for item in items.into_iter().rev() {
        if remaining == 0 {
            break;
        }
        // Match Codex's remote-v2 retention policy: the 64k budget limits message text, while
        // user images remain attached to any retained message.
        let token_count = message_text_token_count(&item).max(1);
        if token_count <= remaining {
            truncated_reversed.push(item);
            remaining = remaining.saturating_sub(token_count);
            continue;
        }

        let is_user_message = item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("role").and_then(Value::as_str) == Some("user");
        if let Some(item) = truncate_message_text_to_token_budget(item, remaining) {
            truncated_reversed.push(item);
            remaining = 0;
        } else if is_user_message {
            // Retained user messages are a newest-first suffix. Once the boundary message cannot
            // retain any model-visible content, admitting an older user message would misalign the
            // active skill blocks that are restored newest-to-newest after compaction.
            remaining = 0;
        }
    }
    truncated_reversed.reverse();
    truncated_reversed
}

fn message_text_token_count(item: &Value) -> usize {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return usize::try_from(estimated_tokens(std::slice::from_ref(item))).unwrap_or(usize::MAX);
    }
    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return usize::try_from(estimated_tokens(std::slice::from_ref(item))).unwrap_or(usize::MAX);
    };
    content
        .iter()
        .filter_map(content_text)
        .map(approx_token_count)
        .sum()
}

fn truncate_message_text_to_token_budget(mut item: Value, max_tokens: usize) -> Option<Value> {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let content = item.get_mut("content")?.as_array_mut()?;
    let mut remaining = max_tokens;
    let mut retained = Vec::with_capacity(content.len());
    for mut content_item in std::mem::take(content) {
        if let Some(text) = content_text_mut(&mut content_item) {
            if remaining == 0 {
                continue;
            }
            let token_count = approx_token_count(text);
            if token_count <= remaining {
                remaining = remaining.saturating_sub(token_count);
            } else {
                *text = truncate_text(text, remaining);
                remaining = 0;
            }
            if !text.is_empty() {
                retained.push(content_item);
            }
        } else {
            retained.push(content_item);
        }
    }
    if retained.is_empty() {
        return None;
    }
    *content = retained;
    Some(item)
}

fn content_text(item: &Value) -> Option<&str> {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("input_text" | "output_text")
    )
    .then(|| item.get("text")?.as_str())?
}

fn content_text_mut(item: &mut Value) -> Option<&mut String> {
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("input_text" | "output_text")
    ) {
        return None;
    }
    match item.get_mut("text")? {
        Value::String(text) => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, text: &str) -> Value {
        json!({
            "type": "message",
            "role": role,
            "content": [{"type": "input_text", "text": text}],
        })
    }

    fn agent_message(author: &str, recipient: &str, text: &str) -> Value {
        json!({
            "type": "agent_message",
            "author": author,
            "recipient": recipient,
            "content": [{"type": "input_text", "text": text}],
        })
    }

    #[test]
    fn remote_v2_retains_tasks_but_drops_descendant_progress_and_completions() {
        let user = message("user", "real user request");
        let parent_task = agent_message(
            "/root",
            "/root/worker",
            "Message Type: TASK\nPayload:\ndelegated task",
        );
        let descendant_task = agent_message(
            "/root/worker",
            "/root/worker/grandchild",
            "Message Type: TASK\nPayload:\nfollow-up task",
        );
        let peer_progress = agent_message(
            "/peer",
            "/root",
            "Message Type: MESSAGE\nPayload:\npeer update",
        );
        let descendant_progress = agent_message(
            "/root/child",
            "/root",
            "Message Type: MESSAGE\nPayload:\nchild progress",
        );
        let completion = agent_message(
            "/root/child",
            "/root",
            "Message Type: FINAL_ANSWER\nPayload:\nchild completion",
        );
        let history = vec![
            message("developer", "stale developer context"),
            message(
                "user",
                "<environment_context>\nstale\n</environment_context>",
            ),
            user.clone(),
            parent_task.clone(),
            descendant_progress,
            completion,
            descendant_task.clone(),
            peer_progress.clone(),
        ];

        assert_eq!(
            retained_compacted_history(history),
            vec![user, parent_task, descendant_task, peer_progress]
        );
    }

    #[test]
    fn remote_v2_drops_oversized_agent_messages() {
        let oversized = agent_message("/root", "/root/worker", &"x".repeat(50_000));
        assert!(retained_compacted_history(vec![oversized]).is_empty());
    }

    #[test]
    fn remote_v2_drops_generated_user_shell_context() {
        let shell = message(
            "user",
            "<user_shell_command>\n<command>\nprintf huge-output\n</command>\n<result>\nlarge output\n</result>\n</user_shell_command>",
        );
        let operator = message("user", "preserve the actual operator request");

        assert_eq!(
            retained_compacted_history(vec![shell, operator.clone()]),
            vec![operator]
        );
    }

    #[test]
    fn retained_text_budget_keeps_images_and_newest_text() {
        let image = json!({
            "type": "input_image",
            "image_url": "data:image/png;base64,AAAA",
            "detail": "low",
        });
        let older = message("user", "older message");
        let newest = json!({
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "newest message"},
                image,
            ],
        });

        let retained = truncate_retained_messages(vec![older, newest], 2);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0]["content"][1], image);
        assert!(
            retained[0]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("tokens truncated"))
        );
    }

    #[test]
    fn image_only_messages_cost_one_budget_unit_regardless_of_image_count() {
        let image = json!({
            "type": "input_image",
            "image_url": "data:image/png;base64,AAAA",
            "detail": "low",
        });
        let image_only = json!({
            "type": "message",
            "role": "user",
            "content": [image.clone(), image],
        });
        let newest = message("user", "new");

        assert_eq!(
            truncate_retained_messages(vec![image_only.clone(), newest.clone()], 2),
            vec![image_only, newest.clone()]
        );
        assert_eq!(
            truncate_retained_messages(
                vec![
                    json!({
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_image",
                            "image_url": "data:image/png;base64,BBBB",
                            "detail": "low",
                        }],
                    }),
                    newest.clone(),
                ],
                1,
            ),
            vec![newest]
        );
    }

    #[test]
    fn retained_text_truncation_preserves_metadata_images_and_later_text_shape() {
        let item = json!({
            "type": "message",
            "id": "msg_1",
            "role": "user",
            "phase": "commentary",
            "internal_chat_message_metadata_passthrough": {"trace": "keep"},
            "content": [
                {"type": "input_text", "text": "abcdef"},
                {
                    "type": "input_image",
                    "image_url": "data:image/png;base64,AAAA",
                    "detail": "low",
                },
                {"type": "output_text", "text": "uvwxyz"},
            ],
        });

        let retained = truncate_retained_messages(vec![item], 3);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0]["id"], "msg_1");
        assert_eq!(retained[0]["phase"], "commentary");
        assert_eq!(
            retained[0]["internal_chat_message_metadata_passthrough"]["trace"],
            "keep"
        );
        assert_eq!(retained[0]["content"][0]["text"], "abcdef");
        assert_eq!(retained[0]["content"][1]["type"], "input_image");
        assert!(
            retained[0]["content"][2]["text"]
                .as_str()
                .is_some_and(|text| text.contains("tokens truncated"))
        );
    }

    #[test]
    fn tool_output_rewrite_preserves_the_call_pair_and_other_fields() {
        let call = json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "exec",
            "arguments": "{}",
        });
        let mut history = vec![
            message("user", "keep"),
            call.clone(),
            json!({
                "type": "function_call_output",
                "id": "fco_1",
                "call_id": "call_1",
                "name": "exec",
                "namespace": "functions",
                "output": "x".repeat(8_000),
                "success": false,
            }),
        ];
        let max_tokens = estimated_tokens(&history[..2]);

        assert_eq!(trim_tool_outputs_to_fit(&mut history, max_tokens), 1);
        assert_eq!(history[1], call);
        assert_eq!(
            history[2]["output"],
            CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE
        );
        assert_eq!(history[2]["id"], "fco_1");
        assert_eq!(history[2]["name"], "exec");
        assert_eq!(history[2]["namespace"], "functions");
        assert_eq!(history[2]["success"], false);
    }

    #[test]
    fn opaque_compaction_validation_preserves_the_exact_server_item() {
        let compaction = json!({
            "type": "compaction",
            "id": "cmp_1",
            "encrypted_content": "opaque",
            "internal_chat_message_metadata_passthrough": {"trace": "keep"},
        });
        assert_eq!(
            opaque_compaction_item(vec![compaction.clone()], 1, 3),
            Ok(compaction)
        );
        assert!(opaque_compaction_item(Vec::new(), 0, 2).is_err());
        assert!(
            opaque_compaction_item(
                vec![json!({"type": "compaction", "encrypted_content": ""})],
                1,
                1,
            )
            .is_err()
        );
    }
}
