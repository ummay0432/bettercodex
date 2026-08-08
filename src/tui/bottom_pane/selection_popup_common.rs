// Ported from OpenAI Codex rust-v0.147.0 (be6e8eac), chiefly
// codex-rs/tui/src/bottom_pane/selection_popup_common.rs. The local API only
// retains the row fields used by bettercodex's deliberately smaller command set.

use super::scroll_state::ScrollState;
use crate::tui::palette;
use crate::tui::width::display_width;
use crate::tui::width::line_width;
use crate::tui::wrapping::RtOptions;
use crate::tui::wrapping::word_wrap_line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

pub(in crate::tui) const MAX_POPUP_ROWS: usize = 8;

const FIXED_LEFT_COLUMN_NUMERATOR: usize = 3;
const FIXED_LEFT_COLUMN_DENOMINATOR: usize = 10;
const MENU_SURFACE_INSET_V: u16 = 1;
const MENU_SURFACE_INSET_H: u16 = 2;

#[derive(Default)]
pub(in crate::tui) struct GenericDisplayRow {
    pub(in crate::tui) name: String,
    pub(in crate::tui) description: Option<String>,
    pub(in crate::tui) wrap_indent: Option<usize>,
}

pub(in crate::tui) fn menu_surface_inset(area: Rect) -> Rect {
    let horizontal = MENU_SURFACE_INSET_H.min(area.width / 2);
    let vertical = MENU_SURFACE_INSET_V.min(area.height / 2);
    Rect::new(
        area.x.saturating_add(horizontal),
        area.y.saturating_add(vertical),
        area.width.saturating_sub(horizontal.saturating_mul(2)),
        area.height.saturating_sub(vertical.saturating_mul(2)),
    )
}

pub(in crate::tui) const fn menu_surface_padding_height() -> u16 {
    MENU_SURFACE_INSET_V * 2
}

pub(in crate::tui) fn render_menu_surface(area: Rect, buffer: &mut Buffer, style: Style) -> Rect {
    if area.is_empty() {
        return area;
    }
    Block::default().style(style).render(area, buffer);
    menu_surface_inset(area)
}

pub(in crate::tui) fn measure_text_height(lines: &[Line<'static>], width: u16) -> u16 {
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width.max(1))
        .try_into()
        .unwrap_or(u16::MAX)
}

pub(in crate::tui) fn render_rows(
    area: Rect,
    buffer: &mut Buffer,
    rows: &[GenericDisplayRow],
    state: &ScrollState,
    empty_message: &str,
) -> u16 {
    if rows.is_empty() {
        if area.height > 0 {
            Line::from(empty_message.dim().italic()).render(area, buffer);
        }
        return u16::from(area.height > 0);
    }

    let max_items = MAX_POPUP_ROWS.min(rows.len());
    let visible_items = max_items.min(area.height.max(1) as usize);
    let start = adjusted_start(rows, state, max_items, area.width, area.height);
    let description_column = compute_description_column(rows, start, visible_items, area.width);
    let mut y = area.y;
    let mut rendered = 0_u16;
    for (index, row) in rows.iter().enumerate().skip(start).take(max_items) {
        if y >= area.bottom() {
            break;
        }
        let mut lines = wrap_row_lines(row, description_column, area.width);
        if Some(index) == state.selected_idx {
            for line in &mut lines {
                for span in &mut line.spans {
                    span.style = palette::accent_style();
                }
            }
        }
        for line in lines {
            if y >= area.bottom() {
                break;
            }
            line.render(Rect::new(area.x, y, area.width, 1), buffer);
            y = y.saturating_add(1);
            rendered = rendered.saturating_add(1);
        }
    }
    rendered
}

pub(in crate::tui) fn measure_rows_height(
    rows: &[GenericDisplayRow],
    state: &ScrollState,
    width: u16,
) -> u16 {
    if rows.is_empty() {
        return 1;
    }
    let content_width = width.saturating_sub(1).max(1);
    let visible_items = MAX_POPUP_ROWS.min(rows.len());
    let start = item_window_start(rows, state, visible_items);
    let description_column = compute_description_column(rows, start, visible_items, content_width);
    rows.iter()
        .skip(start)
        .take(visible_items)
        .map(|row| {
            u16::try_from(wrap_row_lines(row, description_column, content_width).len())
                .unwrap_or(u16::MAX)
        })
        .fold(0_u16, u16::saturating_add)
        .max(1)
}

fn compute_description_column(
    rows: &[GenericDisplayRow],
    start: usize,
    visible_items: usize,
    width: u16,
) -> usize {
    if width <= 1 {
        return 0;
    }
    let maximum = width.saturating_sub(1) as usize;
    let maximum_auto = maximum.min(
        ((width as usize * (FIXED_LEFT_COLUMN_DENOMINATOR - FIXED_LEFT_COLUMN_NUMERATOR))
            / FIXED_LEFT_COLUMN_DENOMINATOR)
            .max(1),
    );
    rows.iter()
        .skip(start)
        .take(visible_items)
        .map(|row| display_width(&row.name))
        .max()
        .unwrap_or_default()
        .saturating_add(2)
        .min(maximum_auto)
}

fn build_name_spans(name: &str, limit: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut used_width = 0_usize;
    let mut truncated = false;
    for grapheme in name.graphemes(true) {
        let next_width = used_width.saturating_add(display_width(grapheme));
        if next_width > limit {
            truncated = true;
            break;
        }
        used_width = next_width;
        spans.push(Span::from(grapheme.to_string()));
    }
    if truncated {
        spans.push(Span::from("…"));
    }
    spans
}

fn build_full_line(row: &GenericDisplayRow, description_column: usize) -> Line<'static> {
    let name_limit = row
        .description
        .as_ref()
        .map(|_| description_column.saturating_sub(2))
        .unwrap_or(usize::MAX);
    let mut spans = build_name_spans(&row.name, name_limit);
    let name_width = line_width(&Line::from(spans.clone()));
    if let Some(description) = &row.description {
        let gap = description_column.saturating_sub(name_width);
        if gap > 0 {
            spans.push(Span::from(" ".repeat(gap)));
        }
        spans.push(Span::from(description.clone()).dim());
    }
    Line::from(spans)
}

fn wrap_row_lines(
    row: &GenericDisplayRow,
    description_column: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let full_line = build_full_line(row, description_column);
    let maximum_indent = width.saturating_sub(1) as usize;
    let continuation_indent = row
        .wrap_indent
        .unwrap_or_else(|| {
            if row.description.is_some() {
                description_column
            } else {
                0
            }
        })
        .min(maximum_indent);
    let options = RtOptions::new(width.max(1) as usize)
        .initial_indent(Line::default())
        .subsequent_indent(Line::from(" ".repeat(continuation_indent)));
    word_wrap_line(&full_line, options)
        .into_iter()
        .map(line_to_owned)
        .collect()
}

fn line_to_owned(line: Line<'_>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .into_iter()
            .map(|span| Span {
                style: span.style,
                content: Cow::Owned(span.content.into_owned()),
            })
            .collect(),
    }
}

fn item_window_start(
    rows: &[GenericDisplayRow],
    state: &ScrollState,
    maximum_items: usize,
) -> usize {
    if rows.is_empty() || maximum_items == 0 {
        return 0;
    }
    let mut start = state.scroll_top.min(rows.len().saturating_sub(1));
    if let Some(selected) = state.selected_idx {
        if selected < start {
            start = selected;
        } else if selected > start.saturating_add(maximum_items.saturating_sub(1)) {
            start = selected + 1 - maximum_items;
        }
    }
    start
}

fn adjusted_start(
    rows: &[GenericDisplayRow],
    state: &ScrollState,
    maximum_items: usize,
    width: u16,
    height: u16,
) -> usize {
    let mut start = item_window_start(rows, state, maximum_items);
    let Some(selected) = state.selected_idx else {
        return start;
    };
    while start < selected {
        let description_column = compute_description_column(
            rows,
            start,
            maximum_items.min(height.max(1) as usize),
            width,
        );
        if selected_is_visible(
            rows,
            start,
            maximum_items,
            selected,
            description_column,
            width,
            height,
        ) {
            break;
        }
        start = start.saturating_add(1);
    }
    start
}

fn selected_is_visible(
    rows: &[GenericDisplayRow],
    start: usize,
    maximum_items: usize,
    selected: usize,
    description_column: usize,
    width: u16,
    height: u16,
) -> bool {
    if height == 0 {
        return false;
    }
    let mut used_lines = 0_usize;
    for (index, row) in rows.iter().enumerate().skip(start).take(maximum_items) {
        let row_lines = wrap_row_lines(row, description_column, width).len().max(1);
        if used_lines > 0 && used_lines.saturating_add(row_lines) > height as usize {
            break;
        }
        if index == selected {
            return true;
        }
        used_lines = used_lines.saturating_add(row_lines);
        if used_lines >= height as usize {
            break;
        }
    }
    false
}
