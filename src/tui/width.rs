//! Terminal display-width helpers.
//!
//! These match Ratatui's terminal-cell semantics while retaining `usize` precision for long
//! lines and accounting for halfwidth sound marks that Ratatui renders as visible cells.

use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Returns the display width Ratatui uses for terminal text without its `u16` limit.
pub(crate) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
        + text
            .chars()
            .filter(|ch| matches!(ch, '\u{FF9E}' | '\u{FF9F}'))
            .count()
}

pub(crate) fn line_width(line: &Line<'_>) -> usize {
    line.iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum()
}

/// Return the longest prefix that fits within `max_width` terminal cells without splitting an
/// extended grapheme cluster.
pub(crate) fn prefix_fitting_width(text: &str, max_width: usize) -> &str {
    if max_width == 0 {
        return "";
    }
    let mut end = 0usize;
    let mut width = 0usize;
    for grapheme in text.graphemes(/*is_extended*/ true) {
        let next_width = width.saturating_add(display_width(grapheme));
        if next_width > max_width {
            break;
        }
        width = next_width;
        end = end.saturating_add(grapheme.len());
    }
    &text[..end]
}
