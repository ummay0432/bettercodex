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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::text::Line;

    #[test]
    fn display_width_matches_ratatui_halfwidth_sound_marks_without_overflow() {
        assert_eq!(display_width("ｶﾞﾊﾟ"), 4);
        assert_eq!(display_width("ｶﾞﾞ"), 3);
        assert_eq!(display_width("界ﾞ"), 3);

        let text = "a".repeat(65_536);
        assert_eq!(display_width(&text), 65_536);
        assert_eq!(line_width(&Line::from(text)), 65_536);
    }

    #[test]
    fn fitting_prefix_preserves_extended_graphemes() {
        let text = "👩‍💻e\u{301}x";
        assert_eq!(prefix_fitting_width(text, 0), "");
        assert_eq!(prefix_fitting_width(text, 1), "");
        assert_eq!(prefix_fitting_width(text, 2), "👩‍💻");
        assert_eq!(prefix_fitting_width(text, 3), "👩‍💻e\u{301}");
    }
}
