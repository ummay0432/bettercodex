use crate::context::estimated_tokens;
use crate::context::is_contextual_user_message;
use crate::context::is_user_shell_command_message;
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
            !is_agent_completion(item)
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
                && (!is_contextual_user_message(item) || is_user_shell_command_message(item))
        }
        Some("agent_message") => true,
        _ => false,
    }
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
            continue;
        }
        let token_count = retained_item_token_count(&item).max(1);
        if token_count <= remaining {
            truncated_reversed.push(item);
            remaining = remaining.saturating_sub(token_count);
            continue;
        }

        let is_user_message = item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("role").and_then(Value::as_str) == Some("user");
        if let Some(item) = truncate_message_to_token_budget(item, remaining) {
            let token_count = retained_item_token_count(&item).max(1);
            debug_assert!(token_count <= remaining);
            if token_count <= remaining {
                truncated_reversed.push(item);
                remaining = remaining.saturating_sub(token_count);
            }
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

fn retained_item_token_count(item: &Value) -> usize {
    usize::try_from(estimated_tokens(std::slice::from_ref(item))).unwrap_or(usize::MAX)
}

fn truncate_message_to_token_budget(mut item: Value, max_tokens: usize) -> Option<Value> {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let content = item.get_mut("content")?.as_array_mut()?;
    let original = std::mem::take(content);
    let mut retained = vec![false; original.len()];
    let mut replacements = std::iter::repeat_with(|| None)
        .take(original.len())
        .collect::<Vec<Option<Value>>>();

    let base_tokens = retained_item_token_count(&item);
    let mut remaining = max_tokens.saturating_sub(base_tokens);

    // Preserve operator text before supplemental media. The opaque compaction item already carries
    // the server's interpretation of older images, while dropping user text here can erase the
    // instruction that the retained image was meant to support.
    for (index, content_item) in original.iter().enumerate() {
        let Some(text) = content_text(content_item) else {
            continue;
        };
        if text.is_empty() || remaining <= 1 {
            continue;
        }
        let available = remaining - 1;
        let token_count = retained_item_token_count(content_item);
        if token_count <= available {
            retained[index] = true;
            remaining = remaining.saturating_sub(token_count.saturating_add(1));
        } else if let Some(truncated) =
            truncate_text_content_to_token_budget(content_item, available)
        {
            let token_count = retained_item_token_count(&truncated);
            retained[index] = true;
            replacements[index] = Some(truncated);
            remaining = remaining.saturating_sub(token_count.saturating_add(1));
        }
    }

    for (index, content_item) in original.iter().enumerate() {
        if content_text(content_item).is_some() || remaining <= 1 {
            continue;
        }
        let token_count = retained_item_token_count(content_item);
        if token_count.saturating_add(1) <= remaining {
            retained[index] = true;
            remaining = remaining.saturating_sub(token_count.saturating_add(1));
        }
    }

    let retained_content = original
        .into_iter()
        .enumerate()
        .filter_map(|(index, content_item)| {
            retained[index].then(|| replacements[index].take().unwrap_or(content_item))
        })
        .collect::<Vec<_>>();
    if retained_content.is_empty() {
        return None;
    }
    *item.get_mut("content")?.as_array_mut()? = retained_content;
    if retained_item_token_count(&item) > max_tokens {
        let content = item.get_mut("content")?.as_array_mut()?;
        content.retain(|content_item| content_text(content_item).is_some());
        if content.is_empty() || retained_item_token_count(&item) > max_tokens {
            return None;
        }
    }
    Some(item)
}

fn truncate_text_content_to_token_budget(item: &Value, max_tokens: usize) -> Option<Value> {
    let text = content_text(item)?;
    let mut low = 1_usize;
    let mut high = approx_token_count(text).min(max_tokens);
    let mut best = None;
    while low <= high {
        let budget = low + (high - low) / 2;
        let mut candidate = item.clone();
        *content_text_mut(&mut candidate)? = truncate_text(text, budget);
        if retained_item_token_count(&candidate) <= max_tokens {
            best = Some(candidate);
            low = budget.saturating_add(1);
        } else {
            high = budget.saturating_sub(1);
        }
    }
    best
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
