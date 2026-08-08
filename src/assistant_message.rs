use crate::protocol::MessagePhase;
use serde_json::Value;

/// Assistant text together with the Responses output phase that defines its lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AssistantMessage {
    pub(crate) text: String,
    pub(crate) phase: Option<MessagePhase>,
}

impl AssistantMessage {
    pub(crate) fn from_response_item(item: &Value) -> Option<Self> {
        if item.get("type").and_then(Value::as_str) != Some("message")
            || item.get("role").and_then(Value::as_str) != Some("assistant")
        {
            return None;
        }

        let content = item.get("content").and_then(Value::as_array)?;
        let text = content
            .iter()
            .filter(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("input_text" | "output_text")
                )
            })
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<String>();
        let phase = match item.get("phase") {
            None | Some(Value::Null) => None,
            Some(phase) => match phase.as_str() {
                Some("commentary") => Some(MessagePhase::Commentary),
                Some("final_answer") => Some(MessagePhase::FinalAnswer),
                _ => return None,
            },
        };
        Some(Self { text, phase })
    }

    /// Missing phase retains Codex's compatibility behavior for legacy model output.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.phase, Some(MessagePhase::FinalAnswer) | None)
    }
}

pub(crate) fn terminal_answer(items: &[Value]) -> Option<String> {
    items
        .iter()
        .rev()
        .filter_map(AssistantMessage::from_response_item)
        .find(|message| message.is_terminal() && !message.text.trim().is_empty())
        .map(|message| message.text)
}

pub(crate) fn has_assistant_text(items: &[Value]) -> bool {
    items
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("assistant")
        })
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("input_text" | "output_text")
            )
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .any(|text| !text.trim().is_empty())
}
