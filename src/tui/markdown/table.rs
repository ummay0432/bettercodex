//! Width-aware rendering for GitHub-flavored Markdown tables.

use pulldown_cmark::Alignment;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const CELL_PADDING: usize = 1;
const COLUMN_GAP: usize = 2;
const HEADER_SEPARATOR: char = '━';
const ROW_SEPARATOR: char = '─';
const MIN_COLUMN_WIDTH: usize = 3;
const MIN_SCANNABLE_EXPANSIVE_WIDTH: usize = 12;
const CRAMPED_EXPANSIVE_CELL_LINES: usize = 4;
const CATASTROPHIC_NARRATIVE_CELL_LINES: usize = 7;

#[derive(Clone, Default)]
struct Cell {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
}

impl Cell {
    fn push_span(&mut self, span: Span<'static>) {
        self.current.push(span);
    }

    fn hard_break(&mut self) {
        self.lines
            .push(Line::from(std::mem::take(&mut self.current)));
    }

    fn rendered_lines(&self) -> Vec<Line<'static>> {
        let mut lines = self.lines.clone();
        if !self.current.is_empty() || lines.is_empty() {
            lines.push(Line::from(self.current.clone()));
        }
        lines
    }

    fn plain_text(&self) -> String {
        self.rendered_lines()
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn display_width(&self) -> usize {
        self.rendered_lines()
            .iter()
            .map(line_width)
            .max()
            .unwrap_or_default()
    }
}

pub(super) struct Table {
    alignments: Vec<Alignment>,
    header: Option<Vec<Cell>>,
    rows: Vec<Vec<Cell>>,
    current_row: Option<Vec<Cell>>,
    current_cell: Option<Cell>,
    in_header: bool,
}

impl Table {
    pub(super) fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            header: None,
            rows: Vec::new(),
            current_row: None,
            current_cell: None,
            in_header: false,
        }
    }

    pub(super) fn start_header(&mut self) {
        self.finish_row();
        self.in_header = true;
        self.current_row = Some(Vec::new());
    }

    pub(super) fn finish_header(&mut self) {
        self.finish_row();
        self.in_header = false;
    }

    pub(super) fn start_row(&mut self) {
        self.finish_row();
        self.current_row = Some(Vec::new());
    }

    pub(super) fn start_cell(&mut self) {
        self.finish_cell();
        self.current_cell = Some(Cell::default());
    }

    pub(super) fn finish_cell(&mut self) {
        if let Some(cell) = self.current_cell.take() {
            self.current_row.get_or_insert_with(Vec::new).push(cell);
        }
    }

    pub(super) fn finish_row(&mut self) {
        self.finish_cell();
        let Some(row) = self.current_row.take() else {
            return;
        };
        if self.in_header {
            self.header = Some(row);
        } else {
            self.rows.push(row);
        }
    }

    pub(super) fn is_in_cell(&self) -> bool {
        self.current_cell.is_some()
    }

    pub(super) fn push_span(&mut self, span: Span<'static>) {
        if let Some(cell) = self.current_cell.as_mut() {
            cell.push_span(span);
        }
    }

    pub(super) fn hard_break(&mut self) {
        if let Some(cell) = self.current_cell.as_mut() {
            cell.hard_break();
        }
    }

    pub(super) fn render(mut self, width: usize) -> Vec<Line<'static>> {
        self.finish_row();
        let column_count = self
            .header
            .as_ref()
            .map(Vec::len)
            .into_iter()
            .chain(self.rows.iter().map(Vec::len))
            .chain(std::iter::once(self.alignments.len()))
            .max()
            .unwrap_or_default();
        if column_count == 0 {
            return Vec::new();
        }

        self.alignments.resize(column_count, Alignment::None);
        let mut header = self
            .header
            .take()
            .unwrap_or_else(|| vec![Cell::default(); column_count]);
        normalize_row(&mut header, column_count);
        for row in &mut self.rows {
            normalize_row(row, column_count);
        }

        let metrics = column_metrics(&header, &self.rows, column_count);
        let reserved = column_count.saturating_mul(CELL_PADDING * 2)
            + column_count.saturating_sub(1) * COLUMN_GAP;
        let content_width = width.saturating_sub(reserved);
        let Some(column_widths) = allocate_column_widths(&metrics, content_width) else {
            return render_records(&header, &self.rows, &metrics, width);
        };
        if should_render_records(&metrics, &column_widths, &self.rows) {
            return render_records(&header, &self.rows, &metrics, width);
        }

        let mut output = Vec::with_capacity(2 + self.rows.len().saturating_mul(2));
        output.extend(render_row(
            &header,
            &column_widths,
            &self.alignments,
            Style::default().fg(Color::Yellow).bold(),
        ));
        output.push(separator_line(&column_widths, HEADER_SEPARATOR));
        for (index, row) in self.rows.iter().enumerate() {
            output.extend(render_row(
                row,
                &column_widths,
                &self.alignments,
                Style::default(),
            ));
            if index + 1 < self.rows.len() {
                output.push(separator_line(&column_widths, ROW_SEPARATOR));
            }
        }
        output
    }
}

fn normalize_row(row: &mut Vec<Cell>, column_count: usize) {
    row.truncate(column_count);
    row.resize(column_count, Cell::default());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColumnKind {
    TokenHeavy,
    Narrative,
    Compact,
}

struct ColumnMetrics {
    natural_width: usize,
    header_token_width: usize,
    body_token_width: usize,
    kind: ColumnKind,
}

fn column_metrics(header: &[Cell], rows: &[Vec<Cell>], column_count: usize) -> Vec<ColumnMetrics> {
    (0..column_count)
        .map(|column| {
            let header_text = header[column].plain_text();
            let header_token_width = longest_token_width(&header_text);
            let mut natural_width = header[column].display_width();
            let mut body_token_width = 0;
            let mut token_count = 0;
            let mut long_token_count = 0;
            let mut total_words = 0;
            let mut populated_cells = 0;
            let mut total_cell_width = 0;
            for row in rows {
                let cell = &row[column];
                natural_width = natural_width.max(cell.display_width());
                let text = cell.plain_text();
                let words = text.split_whitespace().collect::<Vec<_>>();
                for word in &words {
                    let token_width = UnicodeWidthStr::width(*word);
                    body_token_width = body_token_width.max(token_width);
                    long_token_count += usize::from(token_width >= 20);
                }
                if !words.is_empty() {
                    token_count += words.len();
                    total_words += words.len();
                    populated_cells += 1;
                    total_cell_width += UnicodeWidthStr::width(text.as_str());
                }
            }
            let average_words = if populated_cells == 0 {
                header_text.split_whitespace().count() as f64
            } else {
                total_words as f64 / populated_cells as f64
            };
            let average_width = if populated_cells == 0 {
                UnicodeWidthStr::width(header_text.as_str()) as f64
            } else {
                total_cell_width as f64 / populated_cells as f64
            };
            let kind = if long_token_count > 0
                && long_token_count >= token_count.saturating_sub(long_token_count)
            {
                ColumnKind::TokenHeavy
            } else if average_words >= 4.0 || average_width >= 28.0 {
                ColumnKind::Narrative
            } else {
                ColumnKind::Compact
            };
            ColumnMetrics {
                natural_width,
                header_token_width,
                body_token_width,
                kind,
            }
        })
        .collect()
}

fn allocate_column_widths(metrics: &[ColumnMetrics], available_width: usize) -> Option<Vec<usize>> {
    let minimum_total = metrics.len().saturating_mul(MIN_COLUMN_WIDTH);
    if available_width < minimum_total {
        return None;
    }
    let mut widths = metrics
        .iter()
        .map(|column| column.natural_width.max(MIN_COLUMN_WIDTH))
        .collect::<Vec<_>>();
    let mut floors = metrics
        .iter()
        .map(preferred_column_floor)
        .collect::<Vec<_>>();
    let floor_total = floors.iter().sum::<usize>();
    if floor_total > available_width {
        let minimums = vec![MIN_COLUMN_WIDTH; floors.len()];
        shrink_columns(
            &mut floors,
            &minimums,
            metrics,
            floor_total - available_width,
        );
    }
    let total = widths.iter().sum::<usize>();
    if total > available_width
        && shrink_columns(&mut widths, &floors, metrics, total - available_width) > 0
    {
        return None;
    }
    Some(widths)
}

fn preferred_column_floor(column: &ColumnMetrics) -> usize {
    let token_target = match column.kind {
        ColumnKind::TokenHeavy | ColumnKind::Narrative => 16,
        ColumnKind::Compact => column
            .header_token_width
            .max(column.body_token_width.min(16)),
    };
    token_target
        .max(MIN_COLUMN_WIDTH)
        .min(column.natural_width.max(MIN_COLUMN_WIDTH))
}

/// Shrink the widest columns in each priority class without iterating once per display cell.
fn shrink_columns(
    widths: &mut [usize],
    floors: &[usize],
    metrics: &[ColumnMetrics],
    mut amount: usize,
) -> usize {
    for kind in [
        ColumnKind::TokenHeavy,
        ColumnKind::Narrative,
        ColumnKind::Compact,
    ] {
        let slack_total = widths
            .iter()
            .enumerate()
            .filter(|(index, _)| metrics[*index].kind == kind)
            .map(|(index, width)| width.saturating_sub(floors[index]))
            .sum::<usize>();
        let to_remove = amount.min(slack_total);
        if to_remove == 0 {
            continue;
        }

        let mut low = 0;
        let mut high = widths
            .iter()
            .enumerate()
            .filter(|(index, _)| metrics[*index].kind == kind)
            .map(|(index, width)| width.saturating_sub(floors[index]))
            .max()
            .unwrap_or_default();
        while low < high {
            let cap = low + (high - low) / 2;
            let removed = widths
                .iter()
                .enumerate()
                .filter(|(index, _)| metrics[*index].kind == kind)
                .map(|(index, width)| width.saturating_sub(floors[index]).saturating_sub(cap))
                .sum::<usize>();
            if removed > to_remove {
                low = cap + 1;
            } else {
                high = cap;
            }
        }

        let cap = low;
        let mut removed = 0;
        for (index, width) in widths.iter_mut().enumerate() {
            if metrics[index].kind != kind {
                continue;
            }
            let reduction = width.saturating_sub(floors[index]).saturating_sub(cap);
            *width -= reduction;
            removed += reduction;
        }
        let mut remainder = to_remove - removed;
        for (index, width) in widths.iter_mut().enumerate() {
            if remainder == 0 {
                break;
            }
            if metrics[index].kind == kind && width.saturating_sub(floors[index]) == cap {
                *width -= 1;
                remainder -= 1;
            }
        }
        amount -= to_remove;
        if amount == 0 {
            break;
        }
    }
    amount
}

fn should_render_records(metrics: &[ColumnMetrics], widths: &[usize], rows: &[Vec<Cell>]) -> bool {
    if rows.is_empty() {
        return false;
    }
    let affected_rows = rows
        .iter()
        .filter(|row| {
            let fragmented = row
                .iter()
                .zip(widths)
                .zip(metrics)
                .any(|((cell, width), metrics)| {
                    let fragmented_token = cell
                        .plain_text()
                        .split_whitespace()
                        .any(|token| UnicodeWidthStr::width(token) > *width);
                    match metrics.kind {
                        ColumnKind::Compact => fragmented_token,
                        ColumnKind::TokenHeavy => {
                            *width < MIN_SCANNABLE_EXPANSIVE_WIDTH && fragmented_token
                        }
                        ColumnKind::Narrative => false,
                    }
                });
            fragmented || expansive_cells_are_starved(row, widths, metrics)
        })
        .count();
    let threshold = if rows.len() == 1 {
        1
    } else {
        2.max(rows.len().div_ceil(3))
    };
    affected_rows >= threshold
}

fn expansive_cells_are_starved(row: &[Cell], widths: &[usize], metrics: &[ColumnMetrics]) -> bool {
    let expansive = row
        .iter()
        .zip(widths)
        .zip(metrics)
        .filter(|(_, metrics)| metrics.kind != ColumnKind::Compact)
        .map(|((cell, width), metrics)| (metrics.kind, *width, wrap_cell(cell, *width).len()))
        .collect::<Vec<_>>();
    expansive
        .iter()
        .filter(|(_, _, height)| *height >= CRAMPED_EXPANSIVE_CELL_LINES)
        .count()
        >= 2
        || expansive.iter().any(|(kind, width, height)| {
            *kind == ColumnKind::Narrative
                && *width < MIN_SCANNABLE_EXPANSIVE_WIDTH
                && *height >= CATASTROPHIC_NARRATIVE_CELL_LINES
        })
}

fn render_row(
    row: &[Cell],
    widths: &[usize],
    alignments: &[Alignment],
    row_style: Style,
) -> Vec<Line<'static>> {
    let wrapped = row
        .iter()
        .zip(widths)
        .map(|(cell, width)| wrap_cell(cell, *width))
        .collect::<Vec<_>>();
    let row_height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    (0..row_height)
        .map(|line_index| {
            let mut spans = Vec::new();
            for (column, width) in widths.iter().copied().enumerate() {
                spans.push(Span::raw(" ".repeat(CELL_PADDING)));
                let mut content = wrapped[column].get(line_index).cloned().unwrap_or_default();
                let content_width = line_width(&content);
                let remaining = width.saturating_sub(content_width);
                let (left_padding, right_padding) = match alignments[column] {
                    Alignment::Right => (remaining, 0),
                    Alignment::Center => (remaining / 2, remaining - (remaining / 2)),
                    Alignment::Left | Alignment::None => (0, remaining),
                };
                if left_padding > 0 {
                    spans.push(Span::raw(" ".repeat(left_padding)));
                }
                spans.append(&mut content.spans);
                if right_padding > 0 {
                    spans.push(Span::raw(" ".repeat(right_padding)));
                }
                spans.push(Span::raw(" ".repeat(CELL_PADDING)));
                if column + 1 < widths.len() {
                    spans.push(Span::raw(" ".repeat(COLUMN_GAP)));
                }
            }
            Line::from(spans).style(row_style)
        })
        .collect()
}

fn wrap_cell(cell: &Cell, width: usize) -> Vec<Line<'static>> {
    cell.rendered_lines()
        .iter()
        .flat_map(|line| wrap_styled_words(line, width))
        .collect()
}

fn render_records(
    header: &[Cell],
    rows: &[Vec<Cell>],
    metrics: &[ColumnMetrics],
    width: usize,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    if rows.is_empty() {
        return header
            .iter()
            .flat_map(Cell::rendered_lines)
            .flat_map(|line| wrap_styled_words(&line, width))
            .collect();
    }

    let label_width = header
        .iter()
        .map(Cell::plain_text)
        .map(|label| UnicodeWidthStr::width(label.as_str()))
        .max()
        .unwrap_or_default();
    let minimum_value_width = if metrics
        .iter()
        .any(|metrics| metrics.kind != ColumnKind::Compact)
    {
        24
    } else {
        12
    };
    let align_values = CELL_PADDING + label_width + COLUMN_GAP + minimum_value_width <= width;
    let label_style = Style::default().fg(Color::Yellow).bold();
    let mut output = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        for (label, value) in header.iter().zip(row) {
            if align_values {
                render_aligned_record_field(
                    &mut output,
                    label,
                    value,
                    label_width,
                    width,
                    label_style,
                );
            } else {
                render_stacked_record_field(&mut output, label, value, width, label_style);
            }
        }
        if row_index + 1 < rows.len() {
            output.push(Line::from(Span::styled(
                ROW_SEPARATOR.to_string().repeat(width),
                Style::default().dim(),
            )));
        }
    }
    output
}

fn render_aligned_record_field(
    output: &mut Vec<Line<'static>>,
    label: &Cell,
    value: &Cell,
    label_width: usize,
    width: usize,
    label_style: Style,
) {
    let value_indent = CELL_PADDING + label_width + COLUMN_GAP;
    let value_width = width.saturating_sub(value_indent).max(1);
    for (index, mut line) in wrap_cell(value, value_width).into_iter().enumerate() {
        let mut spans = Vec::new();
        if index == 0 {
            let label = label.plain_text();
            spans.push(Span::raw(" ".repeat(CELL_PADDING)));
            spans.push(Span::styled(label.clone(), label_style));
            spans.push(Span::raw(" ".repeat(
                label_width.saturating_sub(UnicodeWidthStr::width(label.as_str())) + COLUMN_GAP,
            )));
        } else {
            spans.push(Span::raw(" ".repeat(value_indent)));
        }
        spans.append(&mut line.spans);
        output.push(Line::from(spans));
    }
}

fn render_stacked_record_field(
    output: &mut Vec<Line<'static>>,
    label: &Cell,
    value: &Cell,
    width: usize,
    label_style: Style,
) {
    let label_width = width.saturating_sub(CELL_PADDING).max(1);
    let label = Line::from(Span::styled(label.plain_text(), label_style));
    for mut line in wrap_styled_words(&label, label_width) {
        let mut spans = vec![Span::raw(" ".repeat(CELL_PADDING))];
        spans.append(&mut line.spans);
        output.push(Line::from(spans));
    }
    let value_indent = 2;
    let value_width = width.saturating_sub(value_indent).max(1);
    for mut line in wrap_cell(value, value_width) {
        let mut spans = vec![Span::raw(" ".repeat(value_indent))];
        spans.append(&mut line.spans);
        output.push(Line::from(spans));
    }
}

fn separator_line(widths: &[usize], character: char) -> Line<'static> {
    let segment = character.to_string();
    let separator = widths
        .iter()
        .map(|width| segment.repeat(width + CELL_PADDING * 2))
        .collect::<Vec<_>>()
        .join(&" ".repeat(COLUMN_GAP));
    Line::from(Span::styled(separator, Style::default().dim()))
}

#[derive(Default)]
struct StyledToken {
    spans: Vec<Span<'static>>,
    width: usize,
    whitespace: bool,
}

fn styled_tokens(line: &Line<'static>) -> Vec<StyledToken> {
    let mut tokens = Vec::<StyledToken>::new();
    for span in &line.spans {
        for grapheme in span.content.graphemes(true) {
            let whitespace = grapheme.chars().all(char::is_whitespace);
            if tokens
                .last()
                .is_none_or(|token| token.whitespace != whitespace)
            {
                tokens.push(StyledToken {
                    whitespace,
                    ..StyledToken::default()
                });
            }
            let token = tokens.last_mut().expect("styled token was inserted");
            push_merged_span(&mut token.spans, grapheme, span.style);
            token.width += UnicodeWidthStr::width(grapheme);
        }
    }
    tokens
}

fn wrap_styled_words(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut output = Vec::new();
    let mut current = Vec::new();
    let mut used = 0;
    let mut pending_space: Option<Style> = None;
    for token in styled_tokens(line) {
        if token.whitespace {
            if used > 0 {
                pending_space = token.spans.first().map(|span| span.style);
            }
            continue;
        }
        let separator_width = usize::from(used > 0 && pending_space.is_some());
        if used > 0 && used + separator_width + token.width > width {
            output.push(Line::from(std::mem::take(&mut current)).style(line.style));
            used = 0;
        }
        if used > 0
            && let Some(style) = pending_space.take()
        {
            push_merged_span(&mut current, " ", style);
            used += 1;
        }
        if token.width <= width.saturating_sub(used) {
            append_spans(&mut current, token.spans);
            used += token.width;
            continue;
        }
        if used > 0 {
            output.push(Line::from(std::mem::take(&mut current)).style(line.style));
        }
        for mut chunk in split_spans(token.spans, width) {
            let chunk_width = spans_width(&chunk);
            if chunk_width == width {
                output.push(Line::from(chunk).style(line.style));
            } else {
                current.append(&mut chunk);
                used = chunk_width;
            }
        }
        pending_space = None;
    }
    if !current.is_empty() || output.is_empty() {
        output.push(Line::from(current).style(line.style));
    }
    output
}

fn split_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut used = 0;
    for span in spans {
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
            if used > 0 && used + grapheme_width > width {
                chunks.push(std::mem::take(&mut current));
                used = 0;
            }
            push_merged_span(&mut current, grapheme, span.style);
            used += grapheme_width;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn append_spans(destination: &mut Vec<Span<'static>>, spans: Vec<Span<'static>>) {
    for span in spans {
        push_merged_span(destination, span.content.as_ref(), span.style);
    }
}

fn push_merged_span(spans: &mut Vec<Span<'static>>, content: &str, style: Style) {
    if content.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push_str(content);
    } else {
        spans.push(Span::styled(content.to_string(), style));
    }
}

fn longest_token_width(text: &str) -> usize {
    text.split_whitespace()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or_default()
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn line_width(line: &Line<'_>) -> usize {
    spans_width(&line.spans)
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}
