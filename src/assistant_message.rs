use crate::protocol::MessagePhase;
use crate::web_search::UrlCitation;
use serde_json::Value;
use std::collections::HashSet;

/// Assistant text together with the Responses output phase that defines its lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AssistantMessage {
    pub(crate) text: String,
    pub(crate) phase: Option<MessagePhase>,
    pub(crate) citations: Vec<UrlCitation>,
}

pub(crate) fn with_citation_sources(text: &str, citations: &[UrlCitation]) -> String {
    let mut seen = HashSet::new();
    let sources = citations
        .iter()
        .filter_map(|citation| {
            let url = citation.validated_url()?;
            seen.insert(url.clone()).then_some((citation, url))
        })
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return text.to_string();
    }
    let mut text = text.trim_end().to_string();
    text.push_str("\n\nSources:\n");
    for (index, (citation, url)) in sources.into_iter().enumerate() {
        let title = citation
            .title
            .chars()
            .filter(|ch| !ch.is_control())
            .collect::<String>();
        text.push_str(&format!("{}. ", index + 1));
        if !title.trim().is_empty() && title != url {
            text.push_str(title.trim());
            text.push_str(": ");
        }
        text.push_str(&url);
        text.push('\n');
    }
    text.pop();
    text
}

impl AssistantMessage {
    pub(crate) fn from_response_item(item: &Value) -> Option<Self> {
        if item.get("type").and_then(Value::as_str) != Some("message")
            || item.get("role").and_then(Value::as_str) != Some("assistant")
        {
            return None;
        }

        let content = item.get("content").and_then(Value::as_array)?;
        let mut text = String::new();
        let mut citations = Vec::new();
        let mut character_offset = 0_usize;
        for part in content.iter().filter(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("input_text" | "output_text")
            )
        }) {
            let Some(part_text) = part.get("text").and_then(Value::as_str) else {
                continue;
            };
            citations.extend(
                part.get("annotations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|annotation| {
                        UrlCitation::from_annotation(annotation, character_offset)
                    }),
            );
            text.push_str(part_text);
            character_offset = character_offset.saturating_add(part_text.chars().count());
        }
        let phase = match item.get("phase") {
            None | Some(Value::Null) => None,
            Some(phase) => match phase.as_str() {
                Some("commentary") => Some(MessagePhase::Commentary),
                Some("final_answer") => Some(MessagePhase::FinalAnswer),
                _ => return None,
            },
        };
        Some(Self {
            text,
            phase,
            citations,
        })
    }

    pub(crate) fn text_with_citation_sources(&self) -> String {
        with_citation_sources(&self.text, &self.citations)
    }

    /// Missing phase retains Codex's compatibility behavior for legacy model output.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.phase, Some(MessagePhase::FinalAnswer) | None)
    }
}
