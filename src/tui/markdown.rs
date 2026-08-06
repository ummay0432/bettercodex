mod table;

use self::table::Table;
use pulldown_cmark::CodeBlockKind;
use pulldown_cmark::Event;
use pulldown_cmark::HeadingLevel;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

pub(super) fn render(source: &str, width: u16) -> Vec<Line<'static>> {
    let source = sanitize(source);
    let parser = Parser::new_ext(
        &source,
        Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
    );
    let mut writer = MarkdownWriter::new(usize::from(width.max(1)));
    for event in parser {
        writer.event(event);
    }
    writer.finish()
}

pub(super) fn sanitize(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\x1b' {
            match characters.next() {
                Some('[') => {
                    let _ = characters.find(|character| ('@'..='~').contains(character));
                }
                Some(']') | Some('P' | '^' | '_') => loop {
                    match characters.next() {
                        Some('\x07') | None => break,
                        Some('\x1b') if characters.next_if_eq(&'\\').is_some() => break,
                        _ => {}
                    }
                },
                Some(_) | None => {}
            }
        } else if matches!(character, '\n' | '\t') || !character.is_control() {
            sanitized.push(character);
        }
    }
    sanitized
}

struct MarkdownWriter {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<ListState>,
    quote_depth: usize,
    code_block: bool,
    table: Option<Table>,
    width: usize,
}

struct ListState {
    next: Option<u64>,
}

impl MarkdownWriter {
    fn new(width: usize) -> Self {
        Self {
            lines: Vec::new(),
            current: Vec::new(),
            styles: Vec::new(),
            lists: Vec::new(),
            quote_depth: 0,
            code_block: false,
            table: None,
            width,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => {
                self.push_span(text.into_string(), Style::default().fg(Color::Cyan));
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                self.push_span(text.into_string(), Style::default().dim());
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.hard_break(),
            Event::Rule => {
                self.flush_nonempty();
                self.lines.push(Line::from(Span::from("────────").dim()));
            }
            Event::TaskListMarker(checked) => {
                self.push_span(
                    if checked { "[x] " } else { "[ ] " },
                    Style::default().dim(),
                );
            }
            Event::FootnoteReference(reference) => {
                self.push_span(format!("[{reference}]"), Style::default().fg(Color::Cyan));
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => self.styles.push(heading_style(level)),
            Tag::BlockQuote => self.quote_depth += 1,
            Tag::CodeBlock(kind) => {
                self.flush_nonempty();
                self.code_block = true;
                if let CodeBlockKind::Fenced(language) = kind
                    && !language.is_empty()
                {
                    self.push_span(language.into_string(), Style::default().dim());
                    self.flush_line();
                }
            }
            Tag::List(first) => self.lists.push(ListState { next: first }),
            Tag::Item => {
                self.flush_nonempty();
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                self.push_span(indent, Style::default());
                let marker = match self.lists.last_mut() {
                    Some(ListState { next: Some(next) }) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        marker
                    }
                    _ => "- ".to_string(),
                };
                self.push_span(marker, Style::default().dim());
            }
            Tag::Emphasis => self.styles.push(Style::default().italic()),
            Tag::Strong => self.styles.push(Style::default().bold()),
            Tag::Strikethrough => self.styles.push(Style::default().crossed_out()),
            Tag::Link { .. } => self
                .styles
                .push(Style::default().fg(Color::Cyan).underlined()),
            Tag::Image { dest_url, .. } => {
                self.push_span("[image: ", Style::default().dim());
                self.push_span(dest_url.into_string(), Style::default().fg(Color::Cyan));
                self.push_span("]", Style::default().dim());
            }
            Tag::FootnoteDefinition(name) => {
                self.flush_nonempty();
                self.push_span(format!("[{name}] "), Style::default().fg(Color::Cyan));
            }
            Tag::Table(alignments) => {
                self.flush_nonempty();
                self.table = Some(Table::new(alignments));
            }
            Tag::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.start_header();
                }
            }
            Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.start_row();
                }
            }
            Tag::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.start_cell();
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph if self.table.is_none() => {
                self.flush_line();
                if self.lists.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::Paragraph => {}
            TagEnd::Heading(_) => {
                self.styles.pop();
                self.flush_line();
                self.blank_line();
            }
            TagEnd::BlockQuote => {
                self.flush_nonempty();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.flush_nonempty();
                self.code_block = false;
                self.blank_line();
            }
            TagEnd::List(_) => {
                self.flush_nonempty();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::Item => self.flush_nonempty(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.styles.pop();
            }
            TagEnd::FootnoteDefinition => {
                self.flush_nonempty();
                self.blank_line();
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_header();
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_row();
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_cell();
                }
            }
            TagEnd::Table => self.finish_table(),
            TagEnd::Image => {}
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        for part in text.split_inclusive('\n') {
            let (content, newline) = part
                .strip_suffix('\n')
                .map_or((part, false), |content| (content, true));
            if !content.is_empty() {
                let style = if self.code_block {
                    Style::default().fg(Color::Cyan)
                } else {
                    self.styles
                        .iter()
                        .copied()
                        .fold(Style::default(), Style::patch)
                };
                self.push_span(content.to_string(), style);
            }
            if newline {
                self.hard_break();
            }
        }
    }

    fn push_span(&mut self, content: impl Into<String>, style: Style) {
        let span = Span::styled(content.into(), style);
        if let Some(table) = self.table.as_mut()
            && table.is_in_cell()
        {
            table.push_span(span);
            return;
        }
        if self.current.is_empty() && self.quote_depth > 0 {
            self.current.push(Span::styled(
                "│ ".repeat(self.quote_depth),
                Style::default().fg(Color::Green),
            ));
        }
        self.current.push(span);
    }

    fn hard_break(&mut self) {
        if let Some(table) = self.table.as_mut()
            && table.is_in_cell()
        {
            table.hard_break();
        } else {
            self.flush_line();
        }
    }

    fn finish_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let quote_width = self.quote_depth.saturating_mul(2);
        let table_width = self.width.saturating_sub(quote_width).max(1);
        let mut rendered = table.render(table_width);
        if self.quote_depth > 0 {
            let prefix = "│ ".repeat(self.quote_depth);
            for line in &mut rendered {
                line.spans.insert(
                    0,
                    Span::styled(prefix.clone(), Style::default().fg(Color::Green)),
                );
            }
        }
        self.lines.append(&mut rendered);
        self.blank_line();
    }

    fn flush_nonempty(&mut self) {
        if !self.current.is_empty() {
            self.flush_line();
        }
    }

    fn flush_line(&mut self) {
        self.lines
            .push(Line::from(std::mem::take(&mut self.current)));
    }

    fn blank_line(&mut self) {
        if self.lines.last().is_some_and(|line| !line.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if self.table.is_some() {
            self.finish_table();
        }
        self.flush_nonempty();
        while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        self.lines
    }
}

fn heading_style(level: HeadingLevel) -> Style {
    match level {
        HeadingLevel::H1 => Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        HeadingLevel::H2 => Style::default().bold(),
        HeadingLevel::H3 => Style::default().bold().italic(),
        HeadingLevel::H4 | HeadingLevel::H5 | HeadingLevel::H6 => Style::default().italic(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    fn display_width(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    }

    #[test]
    fn markdown_keeps_lists_and_code_as_separate_lines() {
        let lines = render("**Done**\n\n- one\n- two\n\n```rs\nlet x = 1;\n```", 80);
        let plain = plain(&lines);
        assert!(plain.iter().any(|line| line == "- one"));
        assert!(plain.iter().any(|line| line == "let x = 1;"));
    }

    #[test]
    fn sanitizer_removes_terminal_control_sequences() {
        assert_eq!(
            sanitize("ok\u{1b}[31mred\u{1b}[0m\u{1b}]0;secret\u{7}done"),
            "okreddone"
        );
    }

    #[test]
    fn tables_render_as_aligned_row_separated_grids() {
        let lines = render(
            "| Left | Center | Right |\n| :--- | :---: | ---: |\n| **bold** | [docs](https://example.com) | `42` |",
            60,
        );
        let text = plain(&lines);
        assert!(text[0].starts_with(" Left"), "{text:?}");
        assert!(text[0].contains("Center"), "{text:?}");
        assert!(text[0].contains("Right "), "{text:?}");
        assert!(text[1].contains('━'), "{text:?}");
        assert!(text[2].contains("bold"), "{text:?}");
        assert!(lines[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(lines[2].spans.iter().any(|span| {
            span.content.contains("bold") && span.style.add_modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn timeline_table_stays_structured_and_within_the_terminal_width() {
        let source = "| Time | Component | Value observed | Result |\n\
                      | ---: | --- | --- | --- |\n\
                      | -10 ms | API handling R1 | Reads `legacy` once | R1 retains and uses `legacy` throughout its lifetime |\n\
                      | +20 ms | Producer creating J1 | Cached value is still `legacy` | J1 envelope receives `mode=legacy` |\n\
                      | +120 ms | Audit consuming J1 event | Envelope: `legacy`; live configuration: `strict` | Records `(legacy, strict)` |";
        let lines = render(source, 100);
        let text = plain(&lines);
        assert!(text[0].contains("Time") && text[0].contains("Result"));
        assert!(text.iter().any(|line| line.contains('━')));
        assert!(text.iter().any(|line| line.contains('─')));
        assert!(!text.iter().any(|line| line.contains(" │ ")));
        assert!(lines.iter().all(|line| display_width(line) <= 100));
    }

    #[test]
    fn table_widths_follow_terminal_cell_geometry() {
        let lines = render(
            "| Key | Notes |\n| --- | --- |\n| ｶﾞﾊﾟtail | First 漢字 row with an escaped \\| pipe. |\n| short | Final 😀 row. |",
            23,
        );
        let text = plain(&lines).join("\n");
        assert!(text.contains("ｶﾞﾊﾟ") && text.contains("漢字"), "{text}");
        assert!(lines.iter().all(|line| display_width(line) <= 23));
    }

    #[test]
    fn narrow_tables_fall_back_to_key_value_records() {
        let lines = render(
            "| Key | Content | Extra | More |\n| --- | --- | --- | --- |\n| item | linked value | bold | code |",
            16,
        );
        let text = plain(&lines);
        assert!(text.iter().any(|line| line.trim() == "Key"));
        assert!(text.iter().any(|line| line.trim() == "item"));
        assert!(text.iter().any(|line| line.trim() == "Content"));
        assert!(!text.iter().any(|line| line.contains('━')));
        assert!(lines.iter().all(|line| display_width(line) <= 16));
    }
}
