//! Pipe-table and Markdown-fence parsing for the renderer.
//!
//! The table helpers identify header and delimiter rows while the fence helpers let the Markdown
//! renderer conservatively unwrap fenced tables. Cross-line state stays in the renderer, which is
//! the only bettercodex caller that needs it.

/// Split a pipe-delimited line into trimmed segments.
///
/// Returns `None` if the line is empty or has no unescaped separator marker.
/// Leading/trailing pipes are stripped before splitting.
///
/// This is intentionally a structural parser, not a renderer. It preserves
/// escaped pipes inside the returned segments because callers only care about
/// whether the line can participate in a table, not how the cell text should
/// finally be displayed.
pub(crate) fn parse_table_segments(line: &str) -> Option<Vec<&str>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let has_outer_pipe = trimmed.starts_with('|') || trimmed.ends_with('|');
    let content = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let content = content.strip_suffix('|').unwrap_or(content);
    let raw_segments = split_unescaped_pipe(content);
    if !has_outer_pipe && raw_segments.len() <= 1 {
        return None;
    }

    let segments: Vec<&str> = raw_segments.into_iter().map(str::trim).collect();
    (!segments.is_empty()).then_some(segments)
}

/// Split `content` on unescaped `|` characters.
///
/// A pipe preceded by `\` is treated as literal text, not a column separator.
/// The backslash remains in the segment (this is structure detection, not
/// rendering).
fn split_unescaped_pipe(content: &str) -> Vec<&str> {
    let mut segments = Vec::with_capacity(8);
    let mut start = 0;
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            // Skip the escaped character.
            i += 2;
        } else if bytes[i] == b'|' {
            segments.push(&content[start..i]);
            start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    segments.push(&content[start..]);
    segments
}

// Small table-detection helpers inlined for the streaming hot path — they are
// called on every source line during incremental holdback scanning.

/// Whether `line` looks like a table header row (has pipe-separated
/// segments with at least one non-empty cell).
#[inline]
pub(crate) fn is_table_header_line(line: &str) -> bool {
    parse_table_segments(line).is_some_and(|segments| segments.iter().any(|s| !s.is_empty()))
}

/// Whether a single segment matches the `---`, `:---`, `---:`, or `:---:`
/// alignment-colon syntax used in markdown table delimiter rows.
#[inline]
fn is_table_delimiter_segment(segment: &str) -> bool {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return false;
    }
    let without_leading = trimmed.strip_prefix(':').unwrap_or(trimmed);
    let without_ends = without_leading.strip_suffix(':').unwrap_or(without_leading);
    without_ends.len() >= 3 && without_ends.chars().all(|c| c == '-')
}

/// Whether `line` is a valid table delimiter row (every segment passes
/// [`is_table_delimiter_segment`]).
#[inline]
pub(crate) fn is_table_delimiter_line(line: &str) -> bool {
    parse_table_segments(line)
        .is_some_and(|segments| segments.into_iter().all(is_table_delimiter_segment))
}

/// Return fence marker character and run length for a potential fence line.
///
/// Recognises backtick and tilde fences with a minimum run of 3.
/// The input should already have leading whitespace and blockquote prefixes
/// stripped.
#[inline]
pub(crate) fn parse_fence_marker(line: &str) -> Option<(char, usize)> {
    let first = line.as_bytes().first().copied()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let len = line.bytes().take_while(|&b| b == first).count();
    if len < 3 {
        return None;
    }
    Some((first as char, len))
}

/// Whether the info string after a fence marker indicates markdown content.
///
/// Matches `md` and `markdown` (case-insensitive).
#[inline]
pub(crate) fn is_markdown_fence_info(trimmed_line: &str, marker_len: usize) -> bool {
    let info = trimmed_line[marker_len..]
        .split_whitespace()
        .next()
        .unwrap_or_default();
    info.eq_ignore_ascii_case("md") || info.eq_ignore_ascii_case("markdown")
}

/// Peel all leading `>` blockquote markers from a line.
///
/// Tables can appear inside blockquotes (`> | A | B |`), so the holdback
/// scanner must strip these markers before checking for table syntax.
#[inline]
pub(crate) fn strip_blockquote_prefix(line: &str) -> &str {
    let mut rest = line.trim_start();
    loop {
        let Some(stripped) = rest.strip_prefix('>') else {
            return rest;
        };
        rest = stripped.strip_prefix(' ').unwrap_or(stripped).trim_start();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_table_segments_basic() {
        assert_eq!(
            parse_table_segments("| A | B | C |"),
            Some(vec!["A", "B", "C"])
        );
    }

    #[test]
    fn parse_table_segments_no_outer_pipes() {
        assert_eq!(parse_table_segments("A | B | C"), Some(vec!["A", "B", "C"]));
    }

    #[test]
    fn parse_table_segments_no_leading_pipe() {
        assert_eq!(
            parse_table_segments("A | B | C |"),
            Some(vec!["A", "B", "C"])
        );
    }

    #[test]
    fn parse_table_segments_no_trailing_pipe() {
        assert_eq!(
            parse_table_segments("| A | B | C"),
            Some(vec!["A", "B", "C"])
        );
    }

    #[test]
    fn parse_table_segments_single_segment_is_allowed() {
        assert_eq!(parse_table_segments("| only |"), Some(vec!["only"]));
    }

    #[test]
    fn parse_table_segments_without_pipe_returns_none() {
        assert_eq!(parse_table_segments("just text"), None);
    }

    #[test]
    fn parse_table_segments_empty_returns_none() {
        assert_eq!(parse_table_segments(""), None);
        assert_eq!(parse_table_segments("   "), None);
    }

    #[test]
    fn parse_table_segments_escaped_pipe() {
        // Escaped pipe should NOT split — stays inside the segment.
        assert_eq!(
            parse_table_segments(r"| A \| B | C |"),
            Some(vec![r"A \| B", "C"])
        );
    }

    #[test]
    fn is_table_delimiter_segment_valid() {
        assert!(is_table_delimiter_segment("---"));
        assert!(is_table_delimiter_segment(":---"));
        assert!(is_table_delimiter_segment("---:"));
        assert!(is_table_delimiter_segment(":---:"));
        assert!(is_table_delimiter_segment(":-------:"));
    }

    #[test]
    fn is_table_delimiter_segment_invalid() {
        assert!(!is_table_delimiter_segment(""));
        assert!(!is_table_delimiter_segment("--"));
        assert!(!is_table_delimiter_segment("abc"));
        assert!(!is_table_delimiter_segment(":--"));
    }

    #[test]
    fn is_table_delimiter_line_valid() {
        assert!(is_table_delimiter_line("| --- | --- |"));
        assert!(is_table_delimiter_line("|:---:|---:|"));
        assert!(is_table_delimiter_line("--- | --- | ---"));
    }

    #[test]
    fn is_table_delimiter_line_invalid() {
        assert!(!is_table_delimiter_line("| A | B |"));
        assert!(!is_table_delimiter_line("| -- | -- |"));
    }

    #[test]
    fn is_table_header_line_valid() {
        assert!(is_table_header_line("| A | B |"));
        assert!(is_table_header_line("Name | Value"));
    }

    #[test]
    fn is_table_header_line_all_empty_segments() {
        assert!(!is_table_header_line("| | |"));
    }

    // Fence helper function tests.

    #[test]
    fn parse_fence_marker_backtick() {
        assert_eq!(parse_fence_marker("```rust"), Some(('`', 3)));
        assert_eq!(parse_fence_marker("````"), Some(('`', 4)));
    }

    #[test]
    fn parse_fence_marker_tilde() {
        assert_eq!(parse_fence_marker("~~~python"), Some(('~', 3)));
    }

    #[test]
    fn parse_fence_marker_too_short() {
        assert_eq!(parse_fence_marker("``"), None);
        assert_eq!(parse_fence_marker("~~"), None);
    }

    #[test]
    fn parse_fence_marker_not_fence() {
        assert_eq!(parse_fence_marker("hello"), None);
        assert_eq!(parse_fence_marker(""), None);
    }

    #[test]
    fn is_markdown_fence_info_basic() {
        assert!(is_markdown_fence_info("```md", /*marker_len*/ 3));
        assert!(is_markdown_fence_info("```markdown", /*marker_len*/ 3));
        assert!(is_markdown_fence_info("```MD", /*marker_len*/ 3));
        assert!(!is_markdown_fence_info("```rust", /*marker_len*/ 3));
        assert!(!is_markdown_fence_info("```", /*marker_len*/ 3));
    }

    #[test]
    fn strip_blockquote_prefix_basic() {
        assert_eq!(strip_blockquote_prefix("> hello"), "hello");
        assert_eq!(strip_blockquote_prefix("> > nested"), "nested");
        assert_eq!(strip_blockquote_prefix("no prefix"), "no prefix");
    }
}
