//! Append-only syntax highlighting for complete lines in an open code fence.

use super::MAX_HIGHLIGHT_LINE_BYTES;
use super::exceeds_highlight_limits;
use super::find_syntax;
use super::highlighted_line_spans;
use super::syntax_set;
use super::theme;
use crate::ansi_escape::expand_tabs;
use ratatui::text::Line;
use syntect::easy::HighlightLines;
use syntect::util::LinesWithEndings;
use two_face::re_exports::syntect;

/// Retains one bounded Syntect line highlighter for an append-only code block.
#[derive(Debug)]
pub(crate) struct StreamingCodeHighlighter {
    state: Option<HighlightedCode>,
}

struct HighlightedCode {
    bytes: usize,
    lines: usize,
    // The local theme is immutable, so retaining this avoids rebuilding its selector tables.
    highlighter: HighlightLines<'static>,
}

impl std::fmt::Debug for HighlightedCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HighlightedCode")
            .field("bytes", &self.bytes)
            .field("lines", &self.lines)
            .finish_non_exhaustive()
    }
}

impl StreamingCodeHighlighter {
    /// Reconstruct highlighting state after the canonical renderer emitted `code`.
    pub(crate) fn new(code: &str, language: &str) -> Option<Self> {
        let line_count = code.lines().count();
        let syntax = find_syntax(language).filter(|_| {
            !exceeds_highlight_limits(code.len(), line_count)
                && !code
                    .lines()
                    .any(|line| line.len() > MAX_HIGHLIGHT_LINE_BYTES)
        });
        let Some(syntax) = syntax else {
            return Some(Self { state: None });
        };

        let mut highlighter = HighlightLines::new(syntax, theme());
        for line in LinesWithEndings::from(code) {
            highlighter.highlight_line(line, syntax_set()).ok()?;
        }
        Some(Self {
            state: Some(HighlightedCode {
                bytes: code.len(),
                lines: line_count,
                highlighter,
            }),
        })
    }

    /// Highlight only newly committed, newline-terminated source lines.
    pub(crate) fn append(mut self, appended: &str) -> Option<(Self, Vec<Line<'static>>)> {
        if !appended.ends_with('\n') {
            return None;
        }
        let appended_line_count = appended.lines().count();
        let Some(mut state) = self.state.take() else {
            let lines = appended
                .lines()
                .map(|line| Line::from(expand_tabs(line).into_owned()))
                .collect();
            return Some((self, lines));
        };

        let bytes = state.bytes.checked_add(appended.len())?;
        let lines = state.lines.checked_add(appended_line_count)?;
        if exceeds_highlight_limits(bytes, lines)
            || appended
                .lines()
                .any(|line| line.len() > MAX_HIGHLIGHT_LINE_BYTES)
        {
            return None;
        }

        let mut rendered = Vec::with_capacity(appended_line_count);
        for line in LinesWithEndings::from(appended) {
            let ranges = state.highlighter.highlight_line(line, syntax_set()).ok()?;
            rendered.push(Line::from(highlighted_line_spans(ranges)));
        }
        state.bytes = bytes;
        state.lines = lines;
        self.state = Some(state);
        Some((self, rendered))
    }
}
