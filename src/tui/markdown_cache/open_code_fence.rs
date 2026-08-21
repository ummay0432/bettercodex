//! Conservative append-only handling for one open, top-level, language-tagged code fence.

use crate::tui::render::highlight::MAX_HIGHLIGHT_LINE_BYTES;
use crate::tui::render::highlight::StreamingCodeHighlighter;
use crate::tui::terminal_hyperlinks::HyperlinkLine;
use ratatui::text::Span;

/// An open fence whose source is identical to the code text seen by the canonical renderer.
#[derive(Debug)]
pub(super) struct OpenCodeFence {
    marker: u8,
    marker_len: usize,
    language: String,
    content_start: usize,
    source_len: usize,
    highlighter: Option<StreamingCodeHighlighter>,
}

impl OpenCodeFence {
    /// Recognize the final mutable top-level block after its canonical render.
    pub(super) fn detect(source: &str, source_len: usize) -> Option<Self> {
        let marker = *source.as_bytes().first()?;
        if marker != b'`' && marker != b'~' {
            return None;
        }
        let (opening, code) = source.split_once('\n')?;
        let marker_len = opening.bytes().take_while(|byte| *byte == marker).count();
        if marker_len < 3 || !source.ends_with('\n') || source.contains(['\r', '\0']) {
            return None;
        }

        let info = opening[marker_len..].trim_matches([' ', '\t', '\u{b}', '\u{c}']);
        if info.contains(['&', '\\']) || (marker == b'`' && info.contains('`')) {
            return None;
        }
        let language = info
            .split([',', ' ', '\t'])
            .next()
            .filter(|language| !language.is_empty())?;
        if language.len() > MAX_HIGHLIGHT_LINE_BYTES
            || has_possible_closing_line(code, marker, marker_len)
        {
            return None;
        }

        Some(Self {
            marker,
            marker_len,
            language: language.to_string(),
            content_start: source_len.checked_sub(code.len())?,
            source_len,
            highlighter: None,
        })
    }

    /// Append code while the newline-committed source extends this same fence exactly.
    pub(super) fn append(
        mut self,
        source: &str,
        appended: &str,
    ) -> Option<(Self, Vec<HyperlinkLine>)> {
        if self.source_len.checked_add(appended.len()) != Some(source.len())
            || !source.ends_with(appended)
            || appended.contains(['\r', '\0'])
            || has_possible_closing_line(appended, self.marker, self.marker_len)
        {
            return None;
        }

        let highlighter = match self.highlighter.take() {
            Some(highlighter) => highlighter,
            None => StreamingCodeHighlighter::new(
                &source[self.content_start..self.source_len],
                &self.language,
            )?,
        };
        let (highlighter, lines) = highlighter.append(appended)?;
        self.highlighter = Some(highlighter);
        self.source_len = source.len();
        let lines = lines
            .into_iter()
            .map(|mut line| {
                line.spans.insert(0, Span::default());
                HyperlinkLine::new(line)
            })
            .collect();
        Some((self, lines))
    }
}

/// Deliberately over-detect possible closers so ambiguous input uses CommonMark parsing.
fn has_possible_closing_line(source: &str, marker: u8, marker_len: usize) -> bool {
    source.lines().any(|line| {
        line.trim_start_matches([' ', '\t'])
            .bytes()
            .take_while(|byte| *byte == marker)
            .count()
            >= marker_len
    })
}
