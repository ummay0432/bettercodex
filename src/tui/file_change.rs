//! Codex-style rendering for authoritative direct-tool file changes.

use super::view::truncate_line;
use super::view::wrap_styled_line;
use crate::protocol::FileChange;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

const MAX_PREVIEW_ROWS: usize = 1_000;
const MAX_RENDERED_PREVIEW_ROWS: usize = 1_000;
const MAX_PREVIEW_ROW_BYTES: usize = 2 * 1024;

#[derive(Clone, Copy)]
enum DiffRowKind {
    Add,
    Delete,
    Separator,
}

struct DiffRow {
    number: usize,
    kind: DiffRowKind,
    text: String,
}

pub(super) fn lines(
    path: &str,
    change: &FileChange,
    width: u16,
    user_style: Style,
) -> Vec<Line<'static>> {
    let verb = match change {
        FileChange::Add { .. } => "Added",
        FileChange::Delete { .. } => "Deleted",
        FileChange::Update { .. } => "Edited",
    };
    let (rows, omitted, added, removed) = preview_rows(change);
    let mut header = vec!["• ".dim(), verb.bold(), " ".into(), path.to_string().into()];
    header.push(" ".into());
    header.extend(line_count_spans(added, removed));
    let mut output = vec![truncate_line(Line::from(header), usize::from(width))];

    let number_width = rows
        .iter()
        .filter(|row| !matches!(row.kind, DiffRowKind::Separator))
        .map(|row| row.number)
        .max()
        .unwrap_or(1)
        .to_string()
        .len();
    let mut rendered_rows = 0_usize;
    let mut rendered_content_omitted = false;
    for row in &rows {
        if rendered_rows == MAX_RENDERED_PREVIEW_ROWS {
            rendered_content_omitted = true;
            break;
        }
        let row_lines = diff_row_lines(row, width, number_width, user_style);
        let available = MAX_RENDERED_PREVIEW_ROWS.saturating_sub(rendered_rows);
        rendered_content_omitted |= row_lines.len() > available;
        let retained = row_lines.len().min(available);
        output.extend(row_lines.into_iter().take(retained));
        rendered_rows = rendered_rows.saturating_add(retained);
        if rendered_content_omitted {
            break;
        }
    }
    if rendered_content_omitted {
        output.push(truncate_line(
            Line::from("    … additional diff content omitted …").dim(),
            usize::from(width),
        ));
    } else if omitted > 0 {
        output.push(truncate_line(
            Line::from(format!("    … {omitted} diff rows omitted …")).dim(),
            usize::from(width),
        ));
    }
    output
}

fn line_count_spans(added: usize, removed: usize) -> Vec<Span<'static>> {
    vec![
        "(".into(),
        format!("+{added}").green(),
        " ".into(),
        format!("-{removed}").red(),
        ")".into(),
    ]
}

fn preview_rows(change: &FileChange) -> (Vec<DiffRow>, usize, usize, usize) {
    let mut rows = Vec::new();
    let mut omitted = 0;
    let mut added = 0_usize;
    let mut removed = 0_usize;
    match change {
        FileChange::Add { content } => {
            for (index, text) in content.lines().enumerate() {
                added = added.saturating_add(1);
                push_row(
                    &mut rows,
                    &mut omitted,
                    DiffRow {
                        number: index.saturating_add(1),
                        kind: DiffRowKind::Add,
                        text: bounded_row(text),
                    },
                );
            }
        }
        FileChange::Delete { content } => {
            for (index, text) in content.lines().enumerate() {
                removed = removed.saturating_add(1);
                push_row(
                    &mut rows,
                    &mut omitted,
                    DiffRow {
                        number: index.saturating_add(1),
                        kind: DiffRowKind::Delete,
                        text: bounded_row(text),
                    },
                );
            }
        }
        FileChange::Update { unified_diff, .. } => {
            if let Ok(patch) = diffy::Patch::from_str(unified_diff) {
                for (hunk_index, hunk) in patch.hunks().iter().enumerate() {
                    if hunk_index > 0 {
                        push_row(
                            &mut rows,
                            &mut omitted,
                            DiffRow {
                                number: 0,
                                kind: DiffRowKind::Separator,
                                text: String::new(),
                            },
                        );
                    }
                    let mut old_line = hunk.old_range().start();
                    let mut new_line = hunk.new_range().start();
                    for line in hunk.lines() {
                        match line {
                            diffy::Line::Insert(text) => {
                                added = added.saturating_add(1);
                                push_row(
                                    &mut rows,
                                    &mut omitted,
                                    DiffRow {
                                        number: new_line,
                                        kind: DiffRowKind::Add,
                                        text: bounded_row(text.trim_end_matches(['\r', '\n'])),
                                    },
                                );
                                new_line = new_line.saturating_add(1);
                            }
                            diffy::Line::Delete(text) => {
                                removed = removed.saturating_add(1);
                                push_row(
                                    &mut rows,
                                    &mut omitted,
                                    DiffRow {
                                        number: old_line,
                                        kind: DiffRowKind::Delete,
                                        text: bounded_row(text.trim_end_matches(['\r', '\n'])),
                                    },
                                );
                                old_line = old_line.saturating_add(1);
                            }
                            diffy::Line::Context(_) => {
                                // Context affects coordinates but is not part of the change preview.
                                old_line = old_line.saturating_add(1);
                                new_line = new_line.saturating_add(1);
                            }
                        }
                    }
                }
            }
        }
    }
    (rows, omitted, added, removed)
}

fn push_row(rows: &mut Vec<DiffRow>, omitted: &mut usize, row: DiffRow) {
    if rows.len() < MAX_PREVIEW_ROWS {
        rows.push(row);
    } else {
        *omitted = omitted.saturating_add(1);
    }
}

fn bounded_row(text: &str) -> String {
    if text.len() <= MAX_PREVIEW_ROW_BYTES {
        return text.to_string();
    }
    let mut end = MAX_PREVIEW_ROW_BYTES;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &text[..end])
}

fn diff_row_lines(
    row: &DiffRow,
    width: u16,
    number_width: usize,
    user_style: Style,
) -> Vec<Line<'static>> {
    if matches!(row.kind, DiffRowKind::Separator) {
        return vec![truncate_line(
            Line::from(format!("    {:number_width$}  ⋮", "")).dim(),
            usize::from(width),
        )];
    }

    let light_background = matches!(user_style.bg, Some(Color::Rgb(red, green, blue))
        if 0.299 * red as f32 + 0.587 * green as f32 + 0.114 * blue as f32 > 128.0);
    let (marker, line_style, gutter_style, marker_style, content_style) = match row.kind {
        DiffRowKind::Add if light_background => (
            '+',
            Style::default().bg(Color::Rgb(218, 251, 225)),
            Style::default()
                .fg(Color::Rgb(31, 35, 40))
                .bg(Color::Rgb(172, 238, 187)),
            Style::default().fg(Color::Green),
            Style::default(),
        ),
        DiffRowKind::Delete if light_background => (
            '-',
            Style::default().bg(Color::Rgb(255, 235, 233)),
            Style::default()
                .fg(Color::Rgb(31, 35, 40))
                .bg(Color::Rgb(255, 206, 203)),
            Style::default().fg(Color::Red),
            Style::default(),
        ),
        DiffRowKind::Add => {
            let background = Color::Rgb(33, 58, 43);
            (
                '+',
                Style::default().bg(background),
                Style::default().dim(),
                Style::default().fg(Color::Green).bg(background),
                Style::default().fg(Color::Green).bg(background),
            )
        }
        DiffRowKind::Delete => {
            let background = Color::Rgb(74, 34, 29);
            (
                '-',
                Style::default().bg(background),
                Style::default().dim(),
                Style::default().fg(Color::Red).bg(background),
                Style::default().fg(Color::Red).bg(background),
            )
        }
        DiffRowKind::Separator => unreachable!("separators return before diff styling"),
    };
    let prefix_width = 4_usize.saturating_add(number_width).saturating_add(2);
    let content_width = usize::from(width)
        .saturating_sub(prefix_width)
        .max(1)
        .try_into()
        .unwrap_or(u16::MAX);
    let content = row.text.replace('\t', "    ");
    wrap_styled_line(
        &Line::from(Span::styled(content, content_style)),
        content_width,
    )
    .into_iter()
    .enumerate()
    .map(|(index, mut content)| {
        let mut spans = if index == 0 {
            vec![
                Span::styled(format!("    {:>number_width$} ", row.number), gutter_style),
                Span::styled(marker.to_string(), marker_style),
            ]
        } else {
            vec![Span::styled(
                format!("    {:number_width$}  ", ""),
                gutter_style,
            )]
        };
        spans.append(&mut content.spans);
        Line::from(spans).style(line_style)
    })
    .collect()
}
