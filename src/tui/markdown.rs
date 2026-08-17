//! Markdown-to-ratatui rendering entry points.
//!
//! Ported from Codex CLI revision `1669c2403f793d0230065397dfc25f52b844244e`. It keeps source
//! text separate from rendered lines and hyperlink metadata, and conservatively unwraps Markdown
//! fences only when they contain a real table.
//!
//! ## Why fence unwrapping exists
//!
//! LLM agents frequently wrap tables in `` ```markdown `` fences, treating
//! them as code.  Without unwrapping, `pulldown-cmark` parses those lines
//! as a fenced code block and renders them as monospace code rather than a
//! structured table.  The unwrapper is intentionally conservative: it
//! buffers the entire fence body before deciding, only unwraps fences whose
//! info string is `md` or `markdown` AND whose body contains a
//! header+delimiter pair, and degrades gracefully on unclosed fences.
use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;

use super::markdown_render;
use super::table_detect;
use super::terminal_hyperlinks::HyperlinkLine;

pub(super) fn render_markdown_agent_with_links_and_cwd(
    markdown_source: &str,
    width: Option<usize>,
    cwd: Option<&Path>,
) -> Vec<HyperlinkLine> {
    let normalized = unwrap_markdown_fences(markdown_source);
    markdown_render::render_markdown_lines_with_width_and_cwd(&normalized, width, cwd)
}

/// Render an agent message and collect the block metadata needed for incremental rendering.
///
/// Block offsets are mapped back to `markdown_source` after Markdown table fences are unwrapped.
/// If a normalized boundary cannot be expressed as a raw-source suffix, it is discarded so the
/// transformed block remains mutable.
pub(super) fn render_streaming_markdown_agent_with_links_and_cwd(
    markdown_source: &str,
    width: Option<usize>,
    cwd: Option<&Path>,
) -> markdown_render::StreamingMarkdownRender {
    let normalized = unwrap_markdown_fences(markdown_source);
    let mut rendered = markdown_render::render_streaming_markdown_lines_with_width_and_cwd(
        &normalized,
        width,
        cwd,
    );
    if normalized != markdown_source {
        // Fence unwrapping removes opening/closing lines. A normalized tail that is still a raw
        // suffix necessarily begins after those removed lines, so its boundary can safely be
        // mapped back to the raw source; otherwise leave the transformed block mutable.
        rendered.last_top_level_block_start = rendered
            .last_top_level_block_start
            .and_then(|boundary| markdown_source.strip_suffix(&normalized[boundary..]))
            .map(str::len);
    }
    rendered
}

/// Stateful control-sequence filter for text split across streaming deltas.
///
/// Keeping the parser state between calls ensures an unterminated CSI or OSC sequence cannot leak
/// its continuation when the next model delta arrives. A one-shot [`sanitize`] call uses the same
/// implementation so streamed and finalized text have identical filtering semantics.
#[derive(Debug, Default)]
pub(super) struct StreamingSanitizer {
    state: SanitizerState,
}

#[derive(Clone, Copy, Debug, Default)]
enum SanitizerState {
    #[default]
    Text,
    Escape,
    Csi,
    ControlString,
    ControlStringEscape,
}

impl StreamingSanitizer {
    pub(super) fn push(&mut self, text: &str, sanitized: &mut String) {
        for character in text.chars() {
            let mut pending = Some(character);
            while let Some(character) = pending.take() {
                match self.state {
                    SanitizerState::Text => {
                        if character == '\x1b' {
                            self.state = SanitizerState::Escape;
                        } else if matches!(character, '\n' | '\t') || !character.is_control() {
                            sanitized.push(character);
                        }
                    }
                    SanitizerState::Escape => {
                        self.state = match character {
                            '[' => SanitizerState::Csi,
                            ']' | 'P' | '^' | '_' => SanitizerState::ControlString,
                            _ => SanitizerState::Text,
                        };
                    }
                    SanitizerState::Csi => {
                        if ('@'..='~').contains(&character) {
                            self.state = SanitizerState::Text;
                        }
                    }
                    SanitizerState::ControlString => match character {
                        '\x07' => self.state = SanitizerState::Text,
                        '\x1b' => self.state = SanitizerState::ControlStringEscape,
                        _ => {}
                    },
                    SanitizerState::ControlStringEscape => {
                        if character == '\\' {
                            self.state = SanitizerState::Text;
                        } else {
                            // The one-shot parser leaves a non-terminating character for the
                            // control-string loop to consume, so do the same across chunk edges.
                            self.state = SanitizerState::ControlString;
                            pending = Some(character);
                        }
                    }
                }
            }
        }
    }
}

/// Whether [`sanitize`] would alter `text` when the parser starts in ordinary text mode.
pub(super) fn requires_sanitization(text: &str) -> bool {
    text.chars()
        .any(|character| !matches!(character, '\n' | '\t') && character.is_control())
}

/// Remove control sequences before model output reaches Ratatui or a terminal escape writer.
pub(super) fn sanitize(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    StreamingSanitizer::default().push(text, &mut sanitized);
    sanitized
}

/// Strip `` ```md ``/`` ```markdown `` fences that contain tables, emitting their content as bare
/// markdown so `pulldown-cmark` parses the tables natively.
///
/// Fences whose info string is not `md` or `markdown` are passed through unchanged.  Markdown
/// fences that do *not* contain a table (detected by checking for a header row + delimiter row)
/// are also passed through so that non-table markdown inside a fence still renders as a code
/// block.
///
/// The fence unwrapping is intentionally conservative: it buffers the entire fence body before
/// deciding, and an unclosed fence at end-of-input is re-emitted with its opening line so partial
/// streams degrade to code display.
fn unwrap_markdown_fences<'a>(markdown_source: &'a str) -> Cow<'a, str> {
    // Zero-copy fast path: most messages contain no fences at all.
    if !markdown_source.contains("```") && !markdown_source.contains("~~~") {
        return Cow::Borrowed(markdown_source);
    }

    #[derive(Clone, Copy)]
    struct Fence {
        marker: u8,
        len: usize,
        is_blockquoted: bool,
    }

    // Strip a trailing newline and up to 3 leading spaces, returning the
    // trimmed slice.  Returns `None` when the line has 4+ leading spaces
    // (which makes it an indented code line per CommonMark).
    fn strip_line_indent(line: &str) -> Option<&str> {
        let without_newline = line.strip_suffix('\n').unwrap_or(line);
        let mut byte_idx = 0usize;
        let mut column = 0usize;
        for b in without_newline.as_bytes() {
            match b {
                b' ' => {
                    byte_idx += 1;
                    column += 1;
                }
                b'\t' => {
                    byte_idx += 1;
                    column += 4;
                }
                _ => break,
            }
            if column >= 4 {
                return None;
            }
        }
        Some(&without_newline[byte_idx..])
    }

    // Parse an opening fence line, returning the fence metadata and whether
    // the fence info string indicates markdown content.
    fn parse_open_fence(line: &str) -> Option<(Fence, bool)> {
        let trimmed = strip_line_indent(line)?;
        let is_blockquoted = trimmed.trim_start().starts_with('>');
        let fence_scan_text = table_detect::strip_blockquote_prefix(trimmed);
        let (marker, len) = table_detect::parse_fence_marker(fence_scan_text)?;
        let is_markdown = table_detect::is_markdown_fence_info(fence_scan_text, len);
        Some((
            Fence {
                marker: marker as u8,
                len,
                is_blockquoted,
            },
            is_markdown,
        ))
    }

    fn is_close_fence(line: &str, fence: Fence) -> bool {
        let Some(trimmed) = strip_line_indent(line) else {
            return false;
        };
        let fence_scan_text = if fence.is_blockquoted {
            if !trimmed.trim_start().starts_with('>') {
                return false;
            }
            table_detect::strip_blockquote_prefix(trimmed)
        } else {
            trimmed
        };
        if let Some((marker, len)) = table_detect::parse_fence_marker(fence_scan_text) {
            marker as u8 == fence.marker
                && len >= fence.len
                && fence_scan_text[len..].trim().is_empty()
        } else {
            false
        }
    }

    fn markdown_fence_contains_table(content: &str, is_blockquoted_fence: bool) -> bool {
        let mut previous_line: Option<&str> = None;
        for line in content.lines() {
            let text = if is_blockquoted_fence {
                table_detect::strip_blockquote_prefix(line)
            } else {
                line
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                previous_line = None;
                continue;
            }

            if let Some(previous) = previous_line
                && table_detect::is_table_header_line(previous)
                && !table_detect::is_table_delimiter_line(previous)
                && table_detect::is_table_delimiter_line(trimmed)
            {
                return true;
            }

            previous_line = Some(trimmed);
        }
        false
    }

    fn content_from_ranges(source: &str, ranges: &[Range<usize>]) -> String {
        let total_len: usize = ranges.iter().map(ExactSizeIterator::len).sum();
        let mut content = String::with_capacity(total_len);
        for range in ranges {
            content.push_str(&source[range.start..range.end]);
        }
        content
    }

    struct MarkdownCandidateData {
        fence: Fence,
        opening_range: Range<usize>,
        content_ranges: Vec<Range<usize>>,
    }

    // Box the large variant to keep ActiveFence small (~pointer-sized).
    enum ActiveFence {
        Passthrough(Fence),
        MarkdownCandidate(Box<MarkdownCandidateData>),
    }

    let mut out = String::with_capacity(markdown_source.len());
    let mut active_fence: Option<ActiveFence> = None;
    let mut source_offset = 0usize;

    let mut push_source_range = |range: Range<usize>| {
        if !range.is_empty() {
            out.push_str(&markdown_source[range]);
        }
    };

    for line in markdown_source.split_inclusive('\n') {
        let line_start = source_offset;
        source_offset += line.len();
        let line_range = line_start..source_offset;

        if let Some(active) = active_fence.take() {
            match active {
                ActiveFence::Passthrough(fence) => {
                    push_source_range(line_range);
                    if !is_close_fence(line, fence) {
                        active_fence = Some(ActiveFence::Passthrough(fence));
                    }
                }
                ActiveFence::MarkdownCandidate(mut data) => {
                    if is_close_fence(line, data.fence) {
                        if markdown_fence_contains_table(
                            &content_from_ranges(markdown_source, &data.content_ranges),
                            data.fence.is_blockquoted,
                        ) {
                            for range in data.content_ranges {
                                push_source_range(range);
                            }
                        } else {
                            push_source_range(data.opening_range);
                            for range in data.content_ranges {
                                push_source_range(range);
                            }
                            push_source_range(line_range);
                        }
                    } else {
                        data.content_ranges.push(line_range);
                        active_fence = Some(ActiveFence::MarkdownCandidate(data));
                    }
                }
            }
            continue;
        }

        if let Some((fence, is_markdown)) = parse_open_fence(line) {
            if is_markdown {
                active_fence = Some(ActiveFence::MarkdownCandidate(Box::new(
                    MarkdownCandidateData {
                        fence,
                        opening_range: line_range,
                        content_ranges: Vec::new(),
                    },
                )));
            } else {
                push_source_range(line_range);
                active_fence = Some(ActiveFence::Passthrough(fence));
            }
            continue;
        }

        push_source_range(line_range);
    }

    if let Some(active) = active_fence {
        match active {
            ActiveFence::Passthrough(_) => {}
            ActiveFence::MarkdownCandidate(data) => {
                push_source_range(data.opening_range);
                for range in data.content_ranges {
                    push_source_range(range);
                }
            }
        }
    }

    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn visible_text(lines: Vec<HyperlinkLine>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| {
                line.line
                    .spans
                    .into_iter()
                    .map(|span| span.content)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn markdown_table_fences_render_as_tables() {
        let source = "```markdown\n| A | B |\n|---|---|\n| 1 | 2 |\n```\n";
        let rendered = visible_text(render_markdown_agent_with_links_and_cwd(
            source, /*width*/ None, /*cwd*/ None,
        ));

        assert!(rendered.iter().any(|line| line.contains('━')));
        assert!(rendered.iter().any(|line| line.contains(" 1      2")));
    }

    #[test]
    fn unwraps_supported_markdown_table_fences() {
        for (source, expected) in [
            (
                "```md\nA | B\n--- | ---\nleft | right\n```\n",
                "A | B\n--- | ---\nleft | right\n",
            ),
            (
                "> ```markdown\n> | A | B |\n> |---|---|\n> | 1 | 2 |\n> ```\n",
                "> | A | B |\n> |---|---|\n> | 1 | 2 |\n",
            ),
        ] {
            assert_eq!(unwrap_markdown_fences(source), expected);
        }
    }

    #[test]
    fn preserves_fences_that_are_not_table_wrappers() {
        for source in [
            "```rust\n| A | B |\n|---|---|\n| 1 | 2 |\n```\n",
            "```markdown\n**bold**\n```\n",
            "```markdown\n> | A | B |\n> |---|---|\n> | 1 | 2 |\n```\n",
            "```markdown\n| A | B |\nnot a delimiter row\n| --- | --- |\n# Heading\n```\n",
            "```markdown\n| A | B |\n\n|---|---|\n| 1 | 2 |\n```\n",
        ] {
            assert_eq!(unwrap_markdown_fences(source), source);
        }
    }
}
