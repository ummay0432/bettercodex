use crate::protocol::MessagePhase;
use crate::web_search::UrlCitation;
use serde_json::Value;
use std::collections::HashSet;

const CITATION_START: char = '\u{e200}';
const CITATION_STOP: char = '\u{e201}';
const CITATION_DELIMITER: char = '\u{e202}';
const CITATION_PREFIX: [char; 5] = ['c', 'i', 't', 'e', CITATION_DELIMITER];
const MAX_RAW_CITATION_MARKER_BYTES: usize = 4 * 1024;

#[derive(Debug, Default)]
pub(crate) struct CitationMarkerFilter {
    state: CitationMarkerState,
    pending: String,
}

#[derive(Debug, Default)]
enum CitationMarkerState {
    #[default]
    Text,
    Prefix(usize),
    Body {
        field_len: usize,
    },
}

impl CitationMarkerFilter {
    pub(crate) fn push(&mut self, text: &str, mut emit: impl FnMut(char)) {
        for character in text.chars() {
            self.push_character(character, &mut emit);
        }
    }

    fn push_character(&mut self, character: char, emit: &mut impl FnMut(char)) {
        let state = std::mem::take(&mut self.state);
        match state {
            CitationMarkerState::Text if character == CITATION_START => {
                self.pending.push(character);
                self.state = CitationMarkerState::Prefix(0);
            }
            CitationMarkerState::Text => emit(character),
            CitationMarkerState::Prefix(index) => {
                self.pending.push(character);
                if CITATION_PREFIX.get(index) == Some(&character) {
                    self.state = if index + 1 == CITATION_PREFIX.len() {
                        CitationMarkerState::Body { field_len: 0 }
                    } else {
                        CitationMarkerState::Prefix(index + 1)
                    };
                } else {
                    self.flush_pending(emit);
                }
            }
            CitationMarkerState::Body { mut field_len } => {
                self.pending.push(character);
                match character {
                    CITATION_STOP if field_len > 0 => {
                        self.pending.clear();
                        self.state = CitationMarkerState::Text;
                    }
                    CITATION_DELIMITER if field_len > 0 => {
                        field_len = 0;
                        self.state = CitationMarkerState::Body { field_len };
                    }
                    character
                        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') =>
                    {
                        field_len = field_len.saturating_add(1);
                        self.state = CitationMarkerState::Body { field_len };
                    }
                    _ => self.flush_pending(emit),
                }
            }
        }
        if !matches!(self.state, CitationMarkerState::Text)
            && self.pending.len() > MAX_RAW_CITATION_MARKER_BYTES
        {
            self.flush_pending(emit);
        }
    }

    pub(crate) fn finish(&mut self, emit: &mut impl FnMut(char)) {
        self.flush_pending(emit);
    }

    fn flush_pending(&mut self, emit: &mut impl FnMut(char)) {
        for character in self.pending.drain(..) {
            emit(character);
        }
        self.state = CitationMarkerState::Text;
    }
}

pub(crate) fn contains_raw_citation_marker(text: &str) -> bool {
    text.contains(CITATION_START)
}

pub(crate) fn strip_raw_citation_markers(text: &str) -> String {
    if !contains_raw_citation_marker(text) {
        return text.to_string();
    }
    let mut stripped = String::with_capacity(text.len());
    let mut filter = CitationMarkerFilter::default();
    filter.push(text, |character| stripped.push(character));
    filter.finish(&mut |character| stripped.push(character));
    stripped
}

/// Assistant text together with the Responses output phase that defines its lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AssistantMessage {
    pub(crate) text: String,
    pub(crate) phase: Option<MessagePhase>,
    pub(crate) citations: Vec<UrlCitation>,
}

pub(crate) fn with_citation_sources(text: &str, citations: &[UrlCitation]) -> String {
    let text = strip_raw_citation_markers(text);
    let mut seen = HashSet::new();
    let sources = citations
        .iter()
        .filter_map(|citation| {
            let url = citation.validated_url()?;
            seen.insert(url.clone()).then_some((citation, url))
        })
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return text;
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
