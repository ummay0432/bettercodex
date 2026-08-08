use super::markdown;
use unicode_segmentation::UnicodeSegmentation;

const MAX_SUMMARY_PREFIX_BYTES: usize = 1_024;
const MAX_HEADING_GRAPHEMES: usize = 120;

/// Extracts the model-authored heading from one streamed reasoning-summary section.
///
/// Sol emits each summary section as Markdown beginning with a bold heading. Only
/// that bounded heading is retained for the activity row; the complete reasoning
/// item still arrives through the Responses output and is preserved in history.
#[derive(Default)]
pub(super) struct ReasoningStatus {
    summary_prefix: String,
    heading: Option<String>,
}

impl ReasoningStatus {
    pub(super) fn reset(&mut self) {
        self.summary_prefix.clear();
        self.heading = None;
    }

    pub(super) fn push_delta(&mut self, delta: &str) {
        if self.heading.is_some() || self.summary_prefix.len() >= MAX_SUMMARY_PREFIX_BYTES {
            return;
        }

        let remaining = MAX_SUMMARY_PREFIX_BYTES - self.summary_prefix.len();
        let mut end = remaining.min(delta.len());
        while !delta.is_char_boundary(end) {
            end -= 1;
        }
        self.summary_prefix.push_str(&delta[..end]);
        self.heading = extract_heading(&self.summary_prefix);
    }

    pub(super) fn heading(&self) -> Option<&str> {
        self.heading.as_deref()
    }
}

fn extract_heading(summary: &str) -> Option<String> {
    let (_, after_opening) = summary.split_once("**")?;
    let (heading, _) = after_opening.split_once("**")?;
    let heading = markdown::sanitize(heading)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if heading.is_empty() {
        return None;
    }

    let mut graphemes = heading.graphemes(true);
    let mut bounded = graphemes
        .by_ref()
        .take(MAX_HEADING_GRAPHEMES)
        .collect::<String>();
    if graphemes.next().is_some() {
        bounded.push('…');
    }
    Some(bounded)
}
