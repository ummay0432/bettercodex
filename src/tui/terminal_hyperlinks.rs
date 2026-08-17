//! Semantic terminal hyperlinks carried separately from visible TUI text.
//!
//! Layout code measures and wraps ordinary ratatui lines. Hyperlink annotations are applied only
//! when text reaches a terminal buffer or scrollback writer so OSC 8 bytes never affect geometry.

use std::borrow::Cow;
use std::num::NonZeroU16;
use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::buffer::CellDiffOption;
use ratatui::buffer::CellWidth;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use unicode_segmentation::UnicodeSegmentation;
use url::Url;

use crate::tui::render::line_utils::line_to_borrowed;
use crate::tui::width::display_width;
use crate::tui::width::line_width;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalHyperlink {
    pub(crate) columns: Range<usize>,
    pub(crate) destination: String,
}

impl TerminalHyperlink {
    pub(crate) fn web(columns: Range<usize>, destination: String) -> Self {
        Self {
            columns,
            destination,
        }
    }

    fn with_columns(&self, columns: Range<usize>) -> Self {
        Self {
            columns,
            destination: self.destination.clone(),
        }
    }

    fn terminal_destination(&self) -> Option<Cow<'_, str>> {
        safe_web_destination(&self.destination)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HyperlinkLine {
    pub(crate) line: Line<'static>,
    pub(crate) hyperlinks: Vec<TerminalHyperlink>,
}

impl HyperlinkLine {
    pub(crate) fn new(line: Line<'static>) -> Self {
        Self {
            line,
            hyperlinks: Vec::new(),
        }
    }

    pub(crate) fn width(&self) -> usize {
        line_width(&self.line)
    }

    pub(crate) fn push_span(&mut self, span: Span<'static>, destination: Option<&str>) {
        let start = self.width();
        let end = start + display_width(span.content.as_ref());
        self.line.push_span(span);
        if end > start
            && let Some(destination) = destination.and_then(web_destination)
        {
            self.hyperlinks
                .push(TerminalHyperlink::web(start..end, destination));
        }
    }

    pub(crate) fn style(mut self, style: ratatui::style::Style) -> Self {
        self.line = self.line.style(style);
        self
    }
}

impl From<Line<'static>> for HyperlinkLine {
    fn from(line: Line<'static>) -> Self {
        Self::new(line)
    }
}

impl From<&'static str> for HyperlinkLine {
    fn from(text: &'static str) -> Self {
        Self::new(Line::from(text))
    }
}

impl From<String> for HyperlinkLine {
    fn from(text: String) -> Self {
        Self::new(Line::from(text))
    }
}

impl std::ops::Deref for HyperlinkLine {
    type Target = Line<'static>;

    fn deref(&self) -> &Self::Target {
        &self.line
    }
}

pub(crate) fn plain_hyperlink_lines(lines: Vec<Line<'static>>) -> Vec<HyperlinkLine> {
    lines.into_iter().map(HyperlinkLine::new).collect()
}

pub(crate) fn prefix_hyperlink_lines(
    lines: Vec<HyperlinkLine>,
    initial_prefix: Span<'static>,
    subsequent_prefix: Span<'static>,
) -> Vec<HyperlinkLine> {
    lines
        .into_iter()
        .enumerate()
        .map(|(index, mut line)| {
            let prefix = if index == 0 {
                initial_prefix.clone()
            } else {
                subsequent_prefix.clone()
            };
            let shift = display_width(prefix.content.as_ref());
            let mut spans = Vec::with_capacity(line.line.spans.len() + 1);
            spans.push(prefix);
            spans.extend(line.line.spans);
            line.line = Line::from(spans).style(line.line.style);
            for hyperlink in &mut line.hyperlinks {
                hyperlink.columns = hyperlink.columns.start + shift..hyperlink.columns.end + shift;
            }
            line
        })
        .collect()
}

pub(crate) fn annotate_web_urls_in_line(line: Line<'static>) -> HyperlinkLine {
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let mut out = HyperlinkLine::new(line);
    out.hyperlinks = web_links_in_text(&text);
    out
}

/// Re-attach source hyperlink ranges after visible-text wrapping has split a line.
///
/// Link text is matched in display order so a URL split across table rows retains the complete
/// destination on every rendered fragment. Whitespace inserted or removed at line boundaries is
/// ignored while matching; hyperlink destinations themselves are never reconstructed from output.
pub(crate) fn remap_wrapped_line(
    source: &HyperlinkLine,
    wrapped: Vec<Line<'static>>,
) -> Vec<HyperlinkLine> {
    let mut out = plain_hyperlink_lines(wrapped);
    if source.hyperlinks.is_empty() {
        return out;
    }

    let source_text = line_text(&source.line);
    let mut source_byte = 0usize;
    let mut source_column = 0usize;
    let mut link_index = 0usize;
    for (index, line) in out.iter_mut().enumerate() {
        if index > 0 {
            let trimmed = source_text[source_byte..].trim_start_matches(char::is_whitespace);
            let skipped = source_text[source_byte..].len() - trimmed.len();
            source_column += source_text[source_byte..source_byte + skipped]
                .graphemes(/*is_extended*/ true)
                .map(display_width)
                .sum::<usize>();
            source_byte += skipped;
        }

        let rendered = line_text(&line.line);
        let remaining = &source_text[source_byte..];
        let Some(rendered_start) = longest_suffix_matching_prefix(&rendered, remaining) else {
            continue;
        };
        let mapped = &rendered[rendered_start..];
        let mut output_column = display_width(&rendered[..rendered_start]);
        for grapheme in mapped.graphemes(/*is_extended*/ true) {
            let width = display_width(grapheme);
            while source
                .hyperlinks
                .get(link_index)
                .is_some_and(|link| link.columns.end <= source_column)
            {
                link_index += 1;
            }
            if let Some(link) = source
                .hyperlinks
                .get(link_index)
                .filter(|link| link.columns.contains(&source_column))
            {
                push_link_range(line, output_column..output_column + width, link);
            }
            source_column += width;
            output_column += width;
        }
        source_byte += mapped.len();
    }
    out
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn longest_suffix_matching_prefix(rendered: &str, source: &str) -> Option<usize> {
    rendered
        .grapheme_indices(/*is_extended*/ true)
        .map(|(index, _)| index)
        .find(|index| source.starts_with(&rendered[*index..]))
}

fn push_link_range(line: &mut HyperlinkLine, range: Range<usize>, link: &TerminalHyperlink) {
    if range.is_empty() {
        return;
    }
    if let Some(previous) = line.hyperlinks.last_mut()
        && previous.destination == link.destination
        && previous.columns.end == range.start
    {
        previous.columns.end = range.end;
        return;
    }
    line.hyperlinks.push(link.with_columns(range));
}

pub(crate) fn web_links_in_text(text: &str) -> Vec<TerminalHyperlink> {
    let mut links = Vec::new();
    let mut search_from = 0usize;
    let mut search_column = 0usize;
    for raw_token in text.split_ascii_whitespace() {
        let Some(relative_start) = text[search_from..].find(raw_token) else {
            continue;
        };
        let raw_start = search_from + relative_start;
        search_column += display_width(&text[search_from..raw_start]);
        let raw_column = search_column;
        search_from = raw_start + raw_token.len();
        search_column += display_width(raw_token);

        let trimmed_start = raw_token
            .find(|ch: char| !is_leading_punctuation(ch))
            .unwrap_or(raw_token.len());
        let trimmed_end = trailing_url_end(&raw_token[trimmed_start..]) + trimmed_start;
        if trimmed_start >= trimmed_end {
            continue;
        }
        let candidate = &raw_token[trimmed_start..trimmed_end];
        let Some(destination) = web_destination(candidate) else {
            continue;
        };
        let start = raw_column + display_width(&raw_token[..trimmed_start]);
        let end = start + display_width(candidate);
        links.push(TerminalHyperlink::web(start..end, destination));
    }
    links
}

fn is_leading_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | '.' | ';' | '!' | '\'' | '"'
    )
}

fn trailing_url_end(candidate: &str) -> usize {
    let mut end = candidate.len();
    while end > 0 {
        let remaining = &candidate[..end];
        let Some(ch) = remaining.chars().next_back() else {
            break;
        };
        let trim = matches!(ch, ',' | '.' | ';' | '!' | '\'' | '"')
            || matches!(ch, ')' | ']' | '}' | '>')
                && has_unmatched_closing_delimiter(remaining, ch);
        if !trim {
            break;
        }
        end -= ch.len_utf8();
    }
    end
}

fn has_unmatched_closing_delimiter(candidate: &str, closing: char) -> bool {
    let opening = match closing {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        '>' => '<',
        _ => return false,
    };
    candidate.chars().filter(|ch| *ch == closing).count()
        > candidate.chars().filter(|ch| *ch == opening).count()
}

pub(crate) fn web_destination(destination: &str) -> Option<String> {
    safe_web_destination(destination).map(Cow::into_owned)
}

fn safe_web_destination(destination: &str) -> Option<Cow<'_, str>> {
    let safe_destination = sanitized_destination(destination);
    {
        let parsed = Url::parse(&safe_destination).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return None;
        }
    }
    Some(safe_destination)
}

fn sanitized_destination(destination: &str) -> Cow<'_, str> {
    if destination.chars().any(char::is_control) {
        Cow::Owned(destination.chars().filter(|ch| !ch.is_control()).collect())
    } else {
        Cow::Borrowed(destination)
    }
}

pub(crate) fn mark_buffer_hyperlinks(
    buf: &mut Buffer,
    area: Rect,
    lines: &[HyperlinkLine],
    scroll_rows: usize,
) {
    if area.is_empty() {
        return;
    }
    // Hyperlink marking is this pass's only side effect. Most transcript frames contain ordinary
    // prose or code, so do not run Ratatui's word wrapper over every line a second time when there
    // is nothing to annotate. Lines after the final hyperlink cannot affect linked row offsets.
    let Some(last_linked_line) = lines.iter().rposition(|line| !line.hyperlinks.is_empty()) else {
        return;
    };

    let viewport_end = scroll_rows.saturating_add(usize::from(area.height));
    let mut logical_row = 0usize;
    for line in &lines[..=last_linked_line] {
        if logical_row >= viewport_end {
            break;
        }
        let paragraph =
            Paragraph::new(Text::from(line_to_borrowed(&line.line))).wrap(Wrap { trim: false });
        let rendered_height = paragraph.line_count(area.width).max(/*other*/ 1);
        let next_logical_row = logical_row.saturating_add(rendered_height);
        if line.hyperlinks.is_empty() || next_logical_row <= scroll_rows {
            logical_row = next_logical_row;
            continue;
        }

        let required_height = rendered_height.min(viewport_end.saturating_sub(logical_row));
        let layout_area = Rect::new(
            /*x*/ 0,
            /*y*/ 0,
            area.width,
            u16::try_from(required_height).unwrap_or(u16::MAX),
        );
        let mut layout = Buffer::empty(layout_area);
        paragraph.render(layout_area, &mut layout);
        let rendered_lines = (0..layout_area.height)
            .map(|row| {
                let mut trailing_columns = 0usize;
                let text = (0..layout_area.width)
                    .filter_map(|column| {
                        if trailing_columns > 0 {
                            trailing_columns -= 1;
                            return None;
                        }
                        let cell = &layout[(column, row)];
                        if cell.diff_option == CellDiffOption::Skip {
                            return None;
                        }
                        trailing_columns = usize::from(cell.cell_width()).saturating_sub(1);
                        Some(cell.symbol())
                    })
                    .collect::<String>();
                Line::from(text.trim_end_matches(' ').to_string())
            })
            .collect();
        for (row, rendered) in remap_wrapped_line(line, rendered_lines).iter().enumerate() {
            for link in &rendered.hyperlinks {
                let Some(terminal_destination) = link.terminal_destination() else {
                    continue;
                };
                let mut trailing_columns = 0usize;
                for column in link.columns.clone() {
                    if trailing_columns > 0 {
                        trailing_columns -= 1;
                        continue;
                    }
                    let row = logical_row + row;
                    if row < scroll_rows || row - scroll_rows >= usize::from(area.height) {
                        continue;
                    }
                    let Ok(column) = u16::try_from(column) else {
                        continue;
                    };
                    let Some(x) = area.x.checked_add(column).filter(|x| *x < area.right()) else {
                        continue;
                    };
                    let y = area.y.saturating_add((row - scroll_rows) as u16);
                    let cell = &mut buf[(x, y)];
                    if cell.diff_option == CellDiffOption::Skip {
                        continue;
                    }
                    trailing_columns = usize::from(cell.cell_width()).saturating_sub(1);
                    let symbol = format!(
                        "\x1b]8;;{terminal_destination}\x07{}\x1b]8;;\x07",
                        cell.symbol()
                    );
                    let width = NonZeroU16::new(cell.cell_width()).unwrap_or(NonZeroU16::MIN);
                    cell.set_symbol(&symbol)
                        .set_diff_option(CellDiffOption::ForcedWidth(width));
                }
            }
        }
        logical_row = next_logical_row;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn strip_osc8(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut stripped = String::with_capacity(text.len());
        let mut index = 0usize;

        while index < bytes.len() {
            if bytes[index..].starts_with(b"\x1b]8;;") {
                index += 5;
                while index < bytes.len() {
                    if bytes[index] == b'\x07' {
                        index += 1;
                        break;
                    }
                    if index + 1 < bytes.len()
                        && bytes[index] == b'\x1b'
                        && bytes[index + 1] == b'\\'
                    {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
                continue;
            }
            let ch = text[index..]
                .chars()
                .next()
                .expect("current byte index starts a character");
            stripped.push(ch);
            index += ch.len_utf8();
        }

        stripped
    }

    #[test]
    fn accepts_only_sanitized_web_destinations() {
        let unsafe_destination = "https://example.com/\u{7}safe";
        let safe_destination = "https://example.com/safe";
        assert_eq!(
            web_destination(unsafe_destination),
            Some(safe_destination.to_string())
        );
        assert_eq!(web_destination("mailto:a@example.com"), None);

        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 4, /*height*/ 1,
        );
        let mut line = HyperlinkLine::new(Line::from("safe"));
        line.hyperlinks.push(TerminalHyperlink::web(
            /*columns*/ 0..4,
            unsafe_destination.to_string(),
        ));
        let mut buf = Buffer::empty(area);
        Paragraph::new(Text::from(line.line.clone())).render(area, &mut buf);
        mark_buffer_hyperlinks(&mut buf, area, &[line], /*scroll_rows*/ 0);
        let rendered = area
            .positions()
            .map(|position| buf[position].symbol())
            .collect::<String>();

        assert!(
            rendered.contains(&format!("\x1b]8;;{safe_destination}\x07")),
            "{rendered:?}"
        );
        assert!(!rendered.contains(unsafe_destination), "{rendered:?}");
    }

    #[test]
    fn discovers_punctuated_web_url_columns() {
        assert_eq!(
            web_links_in_text("See (https://example.com/a)."),
            vec![TerminalHyperlink::web(
                /*columns*/ 5..26,
                "https://example.com/a".to_string(),
            )]
        );
    }

    #[test]
    fn hyperlink_columns_follow_a_long_prefix_without_wrapping() {
        let prefix = "a".repeat(65_536);
        let destination = "https://example.com/long-prefix";
        let text = format!("{prefix} {destination}");

        assert_eq!(
            HyperlinkLine::new(Line::from(text.clone())).width(),
            text.len()
        );
        assert_eq!(
            web_links_in_text(&text),
            vec![TerminalHyperlink::web(
                /*columns*/ 65_537..65_537 + destination.len(),
                destination.to_string(),
            )]
        );
    }

    #[test]
    fn preserves_balanced_parentheses_in_bare_web_urls() {
        let destination = "https://en.wikipedia.org/wiki/Function_(mathematics)";
        assert_eq!(
            web_links_in_text(&format!("See ({destination}).")),
            vec![TerminalHyperlink::web(
                /*columns*/ 5..5 + usize::from(destination.cell_width()),
                destination.to_string(),
            )]
        );
    }

    #[test]
    fn wrapping_maps_repeated_link_labels_by_source_position() {
        let mut source = HyperlinkLine::new(Line::from("here here"));
        source.hyperlinks.push(TerminalHyperlink::web(
            /*columns*/ 5..9,
            "https://example.com".to_string(),
        ));

        let wrapped = remap_wrapped_line(&source, vec![Line::from("here here")]);

        assert_eq!(
            wrapped[0].hyperlinks,
            vec![TerminalHyperlink::web(
                /*columns*/ 5..9,
                "https://example.com".to_string(),
            )]
        );
    }

    #[test]
    fn wrapping_maps_multiple_links_across_indented_unicode_lines() {
        let text = "alpha \u{ff21}here middle there end";
        let first_start = text.find("here").expect("first link");
        let second_start = text.find("there").expect("second link");
        let first_column = usize::from(text[..first_start].cell_width());
        let second_column = usize::from(text[..second_start].cell_width());
        let mut source = HyperlinkLine::new(Line::from(text));
        source.hyperlinks.push(TerminalHyperlink::web(
            first_column..first_column + usize::from("here".cell_width()),
            "https://example.com/first".to_string(),
        ));
        source.hyperlinks.push(TerminalHyperlink::web(
            second_column..second_column + usize::from("there".cell_width()),
            "https://example.com/second".to_string(),
        ));

        let wrapped = remap_wrapped_line(
            &source,
            vec![
                Line::from("  alpha \u{ff21}here"),
                Line::from("    middle there end"),
            ],
        );

        assert_eq!(
            wrapped,
            vec![
                HyperlinkLine {
                    line: Line::from("  alpha \u{ff21}here"),
                    hyperlinks: vec![TerminalHyperlink::web(
                        /*columns*/ 10..14,
                        "https://example.com/first".to_string(),
                    )],
                },
                HyperlinkLine {
                    line: Line::from("    middle there end"),
                    hyperlinks: vec![TerminalHyperlink::web(
                        /*columns*/ 11..16,
                        "https://example.com/second".to_string(),
                    )],
                },
            ]
        );
    }

    #[test]
    fn buffer_hyperlinks_follow_word_wrapping() {
        let destination = "https://example.com/path";
        let mut line = HyperlinkLine::new(Line::from(format!("See {destination} now")));
        line.hyperlinks.push(TerminalHyperlink::web(
            /*columns*/ 4..4 + usize::from(destination.cell_width()),
            destination.to_string(),
        ));
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 18, /*height*/ 4,
        );
        let mut buf = Buffer::empty(area);

        Paragraph::new(Text::from(line.line.clone()))
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        mark_buffer_hyperlinks(&mut buf, area, &[line], /*scroll_rows*/ 0);

        let linked_text = area
            .positions()
            .filter_map(|position| {
                let symbol = buf[position].symbol();
                symbol
                    .contains(&format!("\x1b]8;;{destination}\x07"))
                    .then(|| strip_osc8(symbol))
            })
            .collect::<String>();
        assert_eq!(linked_text, destination);
    }

    #[test]
    fn buffer_hyperlinks_follow_wrapped_wide_glyphs() {
        let destination = "https://example.com/wide";
        let mut line = HyperlinkLine::new(Line::from("前文 "));
        line.push_span("漢字漢字".into(), Some(destination));
        line.push_span(" 後文".into(), /*destination*/ None);
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 6, /*height*/ 4,
        );
        let mut buf = Buffer::empty(area);

        Paragraph::new(Text::from(line.line.clone()))
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        mark_buffer_hyperlinks(&mut buf, area, &[line], /*scroll_rows*/ 0);

        let linked_text = area
            .positions()
            .filter_map(|position| {
                let symbol = buf[position].symbol();
                symbol
                    .contains(&format!("\x1b]8;;{destination}\x07"))
                    .then(|| strip_osc8(symbol))
            })
            .collect::<String>();
        assert_eq!(linked_text, "漢字漢字");
    }

    #[test]
    fn buffer_hyperlinks_follow_emoji_and_combining_graphemes() {
        let destination = "https://example.com/graphemes";
        let mut line = HyperlinkLine::new(Line::from("👩‍💻 e\u{301} "));
        line.push_span("linked\u{a0}".into(), Some(destination));
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 12, /*height*/ 2,
        );
        let mut buf = Buffer::empty(area);

        Paragraph::new(Text::from(line.line.clone()))
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        mark_buffer_hyperlinks(&mut buf, area, &[line], /*scroll_rows*/ 0);

        let linked_text = area
            .positions()
            .filter_map(|position| {
                let symbol = buf[position].symbol();
                symbol
                    .contains(&format!("\x1b]8;;{destination}\x07"))
                    .then(|| strip_osc8(symbol))
            })
            .collect::<String>();
        assert_eq!(linked_text, "linked\u{a0}");
    }

    #[test]
    fn buffer_hyperlinks_follow_wrapped_halfwidth_dakuten() {
        let destination = "https://example.com/dakuten";
        let mut line = HyperlinkLine::new(Line::from("ｶﾞ "));
        line.push_span("ﾊﾟlink".into(), Some(destination));
        line.push_span(" tail".into(), /*destination*/ None);
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 5, /*height*/ 4,
        );
        let mut buf = Buffer::empty(area);

        Paragraph::new(Text::from(line.line.clone()))
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        mark_buffer_hyperlinks(&mut buf, area, &[line], /*scroll_rows*/ 0);

        let linked_text = area
            .positions()
            .filter_map(|position| {
                let symbol = buf[position].symbol();
                symbol
                    .contains(&format!("\x1b]8;;{destination}\x07"))
                    .then(|| strip_osc8(symbol))
            })
            .collect::<String>();
        assert_eq!(linked_text, "ﾊﾟlink");
    }

    #[test]
    fn buffer_hyperlinks_preserve_visible_cell_width_for_ratatui_diff() {
        let destination = "https://example.com/dakuten";
        let mut line = HyperlinkLine::new(Line::from("ｶﾞ tail"));
        line.hyperlinks.push(TerminalHyperlink::web(
            /*columns*/ 0..2,
            destination.to_string(),
        ));
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 7, /*height*/ 1,
        );
        let previous = Buffer::with_lines(["       "]);
        let mut next = Buffer::empty(area);

        Paragraph::new(Text::from(line.line.clone())).render(area, &mut next);
        mark_buffer_hyperlinks(&mut next, area, &[line], /*scroll_rows*/ 0);

        assert_eq!(next[(0, 0)].cell_width(), 2);
        assert!(matches!(
            next[(0, 0)].diff_option,
            CellDiffOption::ForcedWidth(width) if width.get() == 2
        ));
        assert_eq!(
            previous
                .diff_iter(&next)
                .map(|(x, _, cell)| (x, strip_osc8(cell.symbol())))
                .collect::<Vec<_>>(),
            vec![
                (0, "ｶﾞ".to_string()),
                (3, "t".to_string()),
                (4, "a".to_string()),
                (5, "i".to_string()),
                (6, "l".to_string()),
            ]
        );
    }
}
