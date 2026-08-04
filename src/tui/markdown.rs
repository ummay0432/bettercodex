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

pub(super) fn render(source: &str) -> Vec<Line<'static>> {
    let source = sanitize(source);
    let parser = Parser::new_ext(
        &source,
        Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
    );
    let mut writer = MarkdownWriter::default();
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

#[derive(Default)]
struct MarkdownWriter {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<ListState>,
    quote_depth: usize,
    code_block: bool,
    in_table: bool,
    table_cell: usize,
}

struct ListState {
    next: Option<u64>,
}

impl MarkdownWriter {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => {
                self.push_span(text.into_string(), Style::default().fg(Color::Cyan))
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                self.push_span(text.into_string(), Style::default().dim())
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.flush_line(),
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
            Tag::Table(_) => {
                self.flush_nonempty();
                self.in_table = true;
            }
            Tag::TableHead | Tag::TableRow => self.table_cell = 0,
            Tag::TableCell => {
                if self.table_cell > 0 {
                    self.push_span(" │ ", Style::default().dim());
                }
                self.table_cell += 1;
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line();
                if self.lists.is_empty() && !self.in_table {
                    self.blank_line();
                }
            }
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
            TagEnd::TableHead | TagEnd::TableRow => self.flush_nonempty(),
            TagEnd::Table => {
                self.flush_nonempty();
                self.in_table = false;
                self.blank_line();
            }
            TagEnd::TableCell | TagEnd::Image => {}
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
                self.flush_line();
            }
        }
    }

    fn push_span(&mut self, content: impl Into<String>, style: Style) {
        if self.current.is_empty() && self.quote_depth > 0 {
            self.current.push(Span::styled(
                "│ ".repeat(self.quote_depth),
                Style::default().fg(Color::Green),
            ));
        }
        self.current.push(Span::styled(content.into(), style));
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

    #[test]
    fn markdown_keeps_lists_and_code_as_separate_lines() {
        let lines = render("**Done**\n\n- one\n- two\n\n```rs\nlet x = 1;\n```");
        let plain = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
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
    fn table_cells_have_visible_separators() {
        let lines = render("| name | state |\n| --- | --- |\n| tui | ready |");
        let plain = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(plain.iter().any(|line| line == "name │ state"));
        assert!(plain.iter().any(|line| line == "tui │ ready"));
    }
}
