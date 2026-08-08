use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use crate::MODEL;
use crate::context::ContextKind;
use crate::context::ContextSnapshot;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

const MUTED: Color = Color::Indexed(245);
const HEADER_HEIGHT: u16 = 4;
const FOOTER_HEIGHT: u16 = 1;
const MIN_GRID_WIDTH: u16 = 58;
const GRID_COLUMNS: usize = 10;
const GRID_ROWS: usize = 10;
const GRID_CELLS: usize = GRID_COLUMNS * GRID_ROWS;
const PANEL_CHROME_HEIGHT: u16 = menu_surface_padding_height() + HEADER_HEIGHT + FOOTER_HEIGHT;

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

    pub(super) fn preferred_height(&self, width: u16) -> u16 {
        panel_height(width, self.segments().len())
    }

    pub(super) fn handle_key(&self, code: KeyCode) -> ContextAction {
        match code {
            KeyCode::Esc => ContextAction::Close,
            _ => ContextAction::StayOpen,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect, surface_style: Style) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        let segments = self.segments();
        let footer_height = FOOTER_HEIGHT.min(area.height);
        let content_area = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.saturating_sub(footer_height),
        );
        let footer_area = Rect::new(area.x, content_area.bottom(), area.width, footer_height);
        let inner = render_menu_surface(content_area, frame.buffer_mut(), surface_style);
        if inner.is_empty() {
            return;
        }

        let header_height = HEADER_HEIGHT.min(inner.height);
        let body_height = inner.height.saturating_sub(header_height);
        let header_area = Rect::new(inner.x, inner.y, inner.width, header_height);
        let body_area = Rect::new(inner.x, header_area.bottom(), inner.width, body_height);

        self.render_header(frame, header_area);
        self.render_body(frame, body_area, &segments);
        if !footer_area.is_empty() {
            let hint_area = Rect::new(
                footer_area.x.saturating_add(2),
                footer_area.y,
                footer_area.width.saturating_sub(2),
                footer_area.height,
            );
            frame.render_widget(
                Paragraph::new("Press esc to go back").style(Style::default().fg(MUTED)),
                hint_area,
            );
        }
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            return;
        }
        let used_percent = format_percent(self.snapshot.used_tokens, self.snapshot.context_window);
        let lines = vec![
            Line::from("Context").bold(),
            Line::from(vec![
                Span::from(MODEL).cyan().bold(),
                Span::from(format!(
                    "  ·  {} / {} tokens  ·  {used_percent} used",
                    format_tokens(self.snapshot.used_tokens),
                    format_tokens(self.snapshot.context_window),
                )),
            ]),
            Line::from(format!(
                "Auto-compact at {}",
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
        if area.width >= MIN_GRID_WIDTH && area.height >= GRID_ROWS as u16 {
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

fn panel_height(width: u16, legend_rows: usize) -> u16 {
    let legend_rows = u16::try_from(legend_rows).unwrap_or(u16::MAX);
    let inner_width = width.saturating_sub(4);
    let body_height = if inner_width >= MIN_GRID_WIDTH {
        legend_rows.max(GRID_ROWS as u16)
    } else {
        legend_rows
    };
    PANEL_CHROME_HEIGHT.saturating_add(body_height)
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
        ContextKind::Skills => "Skills",
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
        ContextKind::Skills => Color::LightCyan,
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
