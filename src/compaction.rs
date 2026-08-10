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
    ModelSwitch(ModelSwitchCompactionReason),
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelSwitchCompactionReason {
    CompHashChanged,
    ModelDownshift,
}

impl CompactionRequest {
    pub(crate) fn trigger(self) -> &'static str {
        match self {
            Self::Automatic(_) | Self::ModelSwitch(_) => "auto",
            Self::Manual => "manual",
        }
    }

    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Automatic(_) => "context_limit",
            Self::ModelSwitch(ModelSwitchCompactionReason::CompHashChanged) => "comp_hash_changed",
            Self::ModelSwitch(ModelSwitchCompactionReason::ModelDownshift) => "model_downshift",
            Self::Manual => "user_requested",
        }
    }

    pub(crate) fn phase(self) -> &'static str {
        match self {
            Self::Automatic(CompactionPhase::PreTurn) => "pre_turn",
            Self::Automatic(CompactionPhase::MidTurn) => "mid_turn",
            Self::ModelSwitch(_) => "pre_turn",
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

pub(crate) fn opaque_compaction_item(items: &[Value]) -> Result<Value, String> {
    let mut compaction = None;
    let mut count = 0_usize;
    for item in items {
        if is_compaction_item(item) {
            count = count.saturating_add(1);
            if compaction.is_none() {
                compaction = Some(item.clone());
            }
        }
    }
    if count != 1 {
        return Err(format!(
            "remote compaction v2 expected exactly one compaction output item, got {count} from {} output items",
            items.len()
        ));
    }
    let compaction = compaction.ok_or_else(|| {
        format!(
            "remote compaction v2 expected exactly one compaction output item, got {count} from {} output items",
            items.len()
        )
    })?;
    if compaction
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_none_or(|encrypted| encrypted.trim().is_empty())
    {
        return Err(
            "remote compaction v2 output omitted its non-empty encrypted_content".to_string(),
        );
    }
    Ok(compaction)
}

pub(crate) fn retained_compacted_history(prompt_history: &[Value]) -> Vec<Value> {
    let retained = prompt_history
        .iter()
        .filter(|item| is_retained_for_remote_compaction_v2(item))
        .filter(|item| should_keep_compacted_history_item(item))
        .cloned()
        .collect::<Vec<_>>();
    truncate_retained_messages(retained, RETAINED_MESSAGE_TOKEN_BUDGET)
}

pub(crate) fn trim_tool_outputs_to_fit(history: &mut [Value], max_tokens: u64) -> usize {
    let item_token_estimates = history
        .iter()
        .map(|item| estimated_tokens(std::slice::from_ref(item)))
        .collect::<Vec<_>>();
    let mut estimated = item_token_estimates
        .iter()
        .copied()
        .fold(0_u64, u64::saturating_add);
    let mut rewritten = 0_usize;

    for (item, item_tokens) in history.iter_mut().zip(item_token_estimates).rev() {
        if estimated <= max_tokens {
            break;
        }
        let Some(replacement) = rewritten_output_for_context_window(item) else {
            break;
        };
        estimated = estimated
            .saturating_sub(item_tokens)
            .saturating_add(estimated_tokens(std::slice::from_ref(&replacement)));
        *item = replacement;
        rewritten = rewritten.saturating_add(1);
    }
    rewritten
}

fn rewritten_output_for_context_window(item: &Value) -> Option<Value> {
    let mut rewritten = item.clone();
    match item.get("type").and_then(Value::as_str) {
        Some("function_call_output" | "custom_tool_call_output") => {
            rewritten["output"] =
                Value::String(CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE.to_string());
        }
        Some("tool_search_output") => {
            rewritten["tools"] = Value::Array(Vec::new());
        }
        _ => return None,
    }
    Some(rewritten)
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
                && !is_contextual_user_message(item)
        }
        Some("agent_message" | "compaction" | "compaction_summary") => true,
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
        let token_count = message_text_token_count(&item).max(1);
        if token_count <= remaining {
            truncated_reversed.push(item);
            remaining = remaining.saturating_sub(token_count);
        } else if let Some(item) = truncate_message_text(item, remaining) {
            truncated_reversed.push(item);
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

fn truncate_message_text(mut item: Value, max_tokens: usize) -> Option<Value> {
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

fn is_compaction_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("compaction" | "compaction_summary")
    )
}
