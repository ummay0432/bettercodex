use crate::MODEL;
use crate::context::ContextKind;
use crate::context::ContextSnapshot;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

const MUTED: Color = Color::Indexed(245);
const RULE: Color = Color::Indexed(8);
const PREFERRED_WIDTH: u16 = 86;
const PREFERRED_HEIGHT: u16 = 24;
const GRID_COLUMNS: usize = 10;
const GRID_ROWS: usize = 10;
const GRID_CELLS: usize = GRID_COLUMNS * GRID_ROWS;

pub(super) const VIEWPORT_HEIGHT: u16 = PREFERRED_HEIGHT;

pub(super) struct ContextWindowView {
    snapshot: ContextSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ContextAction {
    StayOpen,
    Close,
}

struct Segment {
    label: &'static str,
    color: Color,
    tokens: u64,
    items: Option<usize>,
}

impl ContextWindowView {
    pub(super) fn new(snapshot: ContextSnapshot) -> Self {
        Self { snapshot }
    }

    pub(super) fn update(&mut self, snapshot: ContextSnapshot) {
        self.snapshot = snapshot;
    }

    pub(super) fn handle_key(&self, code: KeyCode) -> ContextAction {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => ContextAction::Close,
            _ => ContextAction::StayOpen,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        let panel = Rect::new(
            area.x,
            area.y,
            PREFERRED_WIDTH.min(area.width),
            PREFERRED_HEIGHT.min(area.height),
        );
        let block = Block::default()
            .title(" Context ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(RULE));
        let inner = block.inner(panel);
        frame.render_widget(block, panel);
        if inner.is_empty() {
            return;
        }

        let footer_height = u16::from(inner.height >= 2);
        let note_height = if inner.height >= 8 { 3 } else { 0 };
        let header_height = if inner.height >= 6 {
            3
        } else {
            inner
                .height
                .saturating_sub(footer_height)
                .saturating_sub(note_height)
        };
        let body_height = inner
            .height
            .saturating_sub(header_height)
            .saturating_sub(note_height)
            .saturating_sub(footer_height);
        let header_area = Rect::new(inner.x, inner.y, inner.width, header_height);
        let body_area = Rect::new(inner.x, header_area.bottom(), inner.width, body_height);
        let note_area = Rect::new(inner.x, body_area.bottom(), inner.width, note_height);
        let footer_area = Rect::new(inner.x, note_area.bottom(), inner.width, footer_height);

        self.render_header(frame, header_area);
        let segments = self.segments();
        self.render_body(frame, body_area, &segments);
        self.render_note(frame, note_area);
        if !footer_area.is_empty() {
            frame.render_widget(
                Paragraph::new("Esc/q close")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(MUTED)),
                footer_area,
            );
        }
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            return;
        }
        let used_percent = format_percent(self.snapshot.used_tokens, self.snapshot.context_window);
        let threshold_status = if self.snapshot.used_tokens < self.snapshot.compact_at_tokens {
            format!(
                "{} free before compact",
                format_tokens(
                    self.snapshot
                        .compact_at_tokens
                        .saturating_sub(self.snapshot.used_tokens)
                )
            )
        } else {
            format!(
                "{} past threshold",
                format_tokens(
                    self.snapshot
                        .used_tokens
                        .saturating_sub(self.snapshot.compact_at_tokens)
                )
            )
        };
        let lines = vec![
            Line::from(vec![
                Span::from(MODEL).cyan().bold(),
                Span::from(format!(
                    "  ·  {} / {} tokens  ·  {used_percent} used",
                    format_tokens(self.snapshot.used_tokens),
                    format_tokens(self.snapshot.context_window),
                )),
            ]),
            Line::from(format!(
                "Auto-compact at {}  ·  {threshold_status}",
                format_tokens(self.snapshot.compact_at_tokens)
            ))
            .dim(),
            Line::default(),
        ];
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_body(&self, frame: &mut Frame<'_>, area: Rect, segments: &[Segment]) {
        if area.is_empty() {
            return;
        }
        if area.width >= 58 && area.height >= GRID_ROWS as u16 {
            let grid_width = 21.min(area.width);
            let grid_area = Rect::new(area.x, area.y, grid_width, area.height);
            let legend_x = grid_area.right().saturating_add(1).min(area.right());
            let legend_area = Rect::new(
                legend_x,
                area.y,
                area.right().saturating_sub(legend_x),
                area.height,
            );
            self.render_grid(frame, grid_area, segments);
            render_legend(frame, legend_area, segments, self.snapshot.context_window);
        } else {
            render_legend(frame, area, segments, self.snapshot.context_window);
        }
    }

    fn render_grid(&self, frame: &mut Frame<'_>, area: Rect, segments: &[Segment]) {
        if area.is_empty() || self.snapshot.context_window == 0 {
            return;
        }
        let colors = grid_colors(segments, self.snapshot.context_window);
        let lines = colors
            .chunks(GRID_COLUMNS)
            .take(usize::from(area.height))
            .map(|row| {
                let spans = row
                    .iter()
                    .enumerate()
                    .map(|(index, color)| {
                        let symbol = if index + 1 == row.len() {
                            "■"
                        } else {
                            "■ "
                        };
                        Span::styled(symbol, Style::default().fg(*color))
                    })
                    .collect::<Vec<_>>();
                Line::from(spans)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_note(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            return;
        }
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(RULE));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.is_empty() {
            return;
        }
        let accounting = if self.snapshot.measured {
            "Total from latest API usage · category split estimated from current request items"
        } else {
            "No API usage yet · all values estimated from current request items"
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(accounting),
                Line::from(format!(
                    "Each square is 1% of the {} context window",
                    format_tokens(self.snapshot.context_window)
                ))
                .dim(),
            ]),
            inner,
        );
    }

    fn segments(&self) -> Vec<Segment> {
        let mut segments = self
            .snapshot
            .sections
            .iter()
            .filter(|section| section.tokens > 0)
            .map(|section| Segment {
                label: context_label(section.kind),
                color: context_color(section.kind),
                tokens: section.tokens,
                items: Some(section.items),
            })
            .collect::<Vec<_>>();
        let free_before_compact = self
            .snapshot
            .compact_at_tokens
            .saturating_sub(self.snapshot.used_tokens);
        if free_before_compact > 0 {
            segments.push(Segment {
                label: "Free before compact",
                color: Color::Indexed(238),
                tokens: free_before_compact,
                items: None,
            });
        }
        let reserve = self.snapshot.context_window.saturating_sub(
            self.snapshot
                .used_tokens
                .max(self.snapshot.compact_at_tokens),
        );
        if reserve > 0 {
            segments.push(Segment {
                label: "Auto-compact reserve",
                color: Color::Indexed(245),
                tokens: reserve,
                items: None,
            });
        }
        segments
    }
}

fn render_legend(frame: &mut Frame<'_>, area: Rect, segments: &[Segment], total: u64) {
    if area.is_empty() {
        return;
    }
    let lines = segments
        .iter()
        .take(usize::from(area.height))
        .map(|segment| {
            let mut spans = vec![
                Span::styled("■ ", Style::default().fg(segment.color)),
                Span::from(format!("{:<23}", segment.label)),
                Span::from(format!(
                    "{:>7}  {:>6}",
                    format_tokens(segment.tokens),
                    format_percent(segment.tokens, total)
                ))
                .bold(),
            ];
            if let Some(items) = segment.items {
                spans.push(Span::from(format!("  ×{items}")).dim());
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), area);
}

fn grid_colors(segments: &[Segment], total: u64) -> Vec<Color> {
    if total == 0 {
        return Vec::new();
    }
    let mut colors = Vec::with_capacity(GRID_CELLS);
    for cell in 0..GRID_CELLS {
        let point = u128::from((cell * 2 + 1) as u64) * u128::from(total);
        let mut cumulative = 0_u128;
        let color = segments
            .iter()
            .find_map(|segment| {
                cumulative = cumulative
                    .saturating_add(u128::from(segment.tokens) * u128::from(GRID_CELLS as u64) * 2);
                (cumulative > point).then_some(segment.color)
            })
            .unwrap_or(Color::Reset);
        colors.push(color);
    }
    colors
}

fn context_label(kind: ContextKind) -> &'static str {
    match kind {
        ContextKind::SystemPrompt => "System prompt",
        ContextKind::ToolCatalogue => "Tool catalogue",
        ContextKind::RepositoryInstructions => "AGENTS.md instructions",
        ContextKind::Environment => "Environment",
        ContextKind::UserMessages => "User messages",
        ContextKind::AssistantMessages => "Assistant messages",
        ContextKind::ToolActivity => "Tool calls & results",
        ContextKind::Reasoning => "Reasoning",
        ContextKind::Compaction => "Compacted history",
        ContextKind::Other => "Other",
    }
}

fn context_color(kind: ContextKind) -> Color {
    match kind {
        ContextKind::SystemPrompt => Color::Magenta,
        ContextKind::ToolCatalogue => Color::Cyan,
        ContextKind::RepositoryInstructions => Color::Yellow,
        ContextKind::Environment => Color::Blue,
        ContextKind::UserMessages => Color::LightYellow,
        ContextKind::AssistantMessages => Color::Green,
        ContextKind::ToolActivity => Color::LightRed,
        ContextKind::Reasoning => Color::LightMagenta,
        ContextKind::Compaction => Color::LightBlue,
        ContextKind::Other => Color::Gray,
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let tenths = tokens.saturating_add(50_000) / 100_000;
        if tenths.is_multiple_of(10) {
            format!("{}M", tenths / 10)
        } else {
            format!("{}.{}M", tenths / 10, tenths % 10)
        }
    } else if tokens >= 1_000 {
        let tenths = tokens.saturating_add(50) / 100;
        if tenths.is_multiple_of(10) {
            format!("{}K", tenths / 10)
        } else {
            format!("{}.{}K", tenths / 10, tenths % 10)
        }
    } else {
        tokens.to_string()
    }
}

fn format_percent(tokens: u64, total: u64) -> String {
    if total == 0 {
        return "0.0%".to_string();
    }
    let tenths = (u128::from(tokens) * 1_000 / u128::from(total)).min(u128::from(u64::MAX));
    let tenths = u64::try_from(tenths).unwrap_or(u64::MAX);
    format!("{}.{:01}%", tenths / 10, tenths % 10)
}

#[cfg(test)]
#[path = "context_window_tests.rs"]
mod tests;
