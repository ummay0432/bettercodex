use crate::model::ModelSelection;
use crate::model::ReasoningEffort;
use crate::session_group::SessionId;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

const PREFIX_WIDTH: usize = 2;
const COLUMN_GAP: usize = 2;
const ROLE_SEPARATOR: &str = "  · ";
const SELECTION_BACKGROUND: Color = Color::Rgb(38, 38, 42);
const MAIN_DIM: (u8, u8, u8) = (132, 132, 138);
const MAIN_BRIGHT: (u8, u8, u8) = (205, 205, 212);
const SOL_DIM: (u8, u8, u8) = (126, 91, 48);
const SOL_BRIGHT: (u8, u8, u8) = (211, 157, 83);
const LUNA_DIM: (u8, u8, u8) = (78, 70, 116);
const LUNA_BRIGHT: (u8, u8, u8) = (145, 130, 202);
const TERRA_DIM: (u8, u8, u8) = (70, 104, 91);
const TERRA_BRIGHT: (u8, u8, u8) = (116, 178, 151);

static ANIMATION_START: OnceLock<Instant> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentSwitcherStatus {
    Working(Duration),
    AwaitingReview,
    Idle,
}

impl AgentSwitcherStatus {
    fn label(self) -> String {
        match self {
            Self::Working(elapsed) => {
                format!("Working ({})", format_elapsed(elapsed.as_secs()))
            }
            Self::AwaitingReview => "Awaiting review".to_string(),
            Self::Idle => "Idle".to_string(),
        }
    }

    const fn is_working(self) -> bool {
        matches!(self, Self::Working(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentSwitcherRow {
    pub(super) session_id: SessionId,
    model_label: String,
    role_label: String,
    hue: IdentityHue,
    status: AgentSwitcherStatus,
}

impl AgentSwitcherRow {
    pub(super) fn main(
        session_id: SessionId,
        selection: &ModelSelection,
        status: AgentSwitcherStatus,
    ) -> Self {
        Self {
            session_id,
            model_label: model_profile_label(selection),
            role_label: "Main".to_string(),
            hue: IdentityHue::Main,
            status,
        }
    }

    pub(super) fn specialist(
        session_id: SessionId,
        selection: &ModelSelection,
        role: &str,
        status: AgentSwitcherStatus,
    ) -> Self {
        let role = role.trim().trim_start_matches('$');
        Self {
            session_id,
            model_label: model_profile_label(selection),
            role_label: format!("${role}"),
            hue: IdentityHue::for_model(&selection.model),
            status,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdentityHue {
    Main,
    Sol,
    Luna,
    Terra,
}

impl IdentityHue {
    fn for_model(model: &str) -> Self {
        if model.contains("luna") {
            Self::Luna
        } else if model.contains("terra") {
            Self::Terra
        } else {
            Self::Sol
        }
    }

    const fn colors(self) -> ((u8, u8, u8), (u8, u8, u8)) {
        match self {
            Self::Main => (MAIN_DIM, MAIN_BRIGHT),
            Self::Sol => (SOL_DIM, SOL_BRIGHT),
            Self::Luna => (LUNA_DIM, LUNA_BRIGHT),
            Self::Terra => (TERRA_DIM, TERRA_BRIGHT),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct AgentSwitcher {
    rows: Vec<AgentSwitcherRow>,
    selected: Option<SessionId>,
}

impl AgentSwitcher {
    pub(super) fn new(rows: Vec<AgentSwitcherRow>, selected: Option<SessionId>) -> Self {
        let selected =
            selected.filter(|selected| rows.iter().any(|row| &row.session_id == selected));
        Self { rows, selected }
    }

    pub(super) fn is_selecting(&self) -> bool {
        self.selected.is_some()
    }

    pub(super) fn preferred_height(&self) -> u16 {
        u16::try_from(self.rows.len()).unwrap_or(u16::MAX)
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() || self.rows.is_empty() {
            return;
        }
        let visible = usize::from(area.height).min(self.rows.len());
        let range = self.visible_range(visible);
        let rows = &self.rows[range];
        let model_width = self
            .rows
            .iter()
            .map(|row| crate::tui::width::display_width(&row.model_label))
            .max()
            .unwrap_or_default();
        let status_width = self
            .rows
            .iter()
            .map(|row| crate::tui::width::display_width(&row.status.label()))
            .max()
            .unwrap_or_default();

        for (offset, row) in rows.iter().enumerate() {
            let status = row.status.label();
            let Ok(offset) = u16::try_from(offset) else {
                break;
            };
            let row_area = Rect::new(area.x, area.y.saturating_add(offset), area.width, 1);
            let selected = self.selected.as_ref() == Some(&row.session_id);
            if selected {
                frame.render_widget(
                    Block::default().style(Style::default().bg(SELECTION_BACKGROUND)),
                    row_area,
                );
            }
            frame.render_widget(
                Paragraph::new(render_row(
                    row,
                    &status,
                    model_width,
                    status_width,
                    usize::from(row_area.width),
                    selected,
                )),
                row_area,
            );
        }
    }

    fn visible_range(&self, visible: usize) -> std::ops::Range<usize> {
        if visible >= self.rows.len() {
            return 0..self.rows.len();
        }
        let anchor = self
            .selected
            .as_ref()
            .and_then(|selected| self.rows.iter().position(|row| &row.session_id == selected))
            .unwrap_or_default();
        let start = anchor
            .saturating_sub(visible / 2)
            .min(self.rows.len().saturating_sub(visible));
        start..start.saturating_add(visible)
    }
}

fn render_row(
    row: &AgentSwitcherRow,
    status: &str,
    model_width: usize,
    status_width: usize,
    available_width: usize,
    selected: bool,
) -> Line<'static> {
    let full_width = PREFIX_WIDTH
        .saturating_add(model_width)
        .saturating_add(COLUMN_GAP)
        .saturating_add(status_width)
        .saturating_add(crate::tui::width::display_width(ROLE_SEPARATOR))
        .saturating_add(crate::tui::width::display_width(&row.role_label));
    let identity_width = PREFIX_WIDTH
        .saturating_add(model_width)
        .saturating_add(crate::tui::width::display_width(ROLE_SEPARATOR))
        .saturating_add(crate::tui::width::display_width(&row.role_label));

    let mut spans = vec![if selected {
        Span::styled("› ", Style::default().fg(Color::White).bold())
    } else {
        Span::from("  ")
    }];
    let model = pad_to_width(&row.model_label, model_width);
    if available_width >= full_width {
        spans.extend(styled_identity_spans(row, model, selected));
        spans.push(Span::from(" ".repeat(COLUMN_GAP)));
        spans.extend(styled_identity_spans(
            row,
            pad_to_width(status, status_width),
            selected,
        ));
        spans.push(Span::from(ROLE_SEPARATOR));
        spans.extend(styled_identity_spans(row, row.role_label.clone(), selected));
    } else if available_width >= identity_width {
        spans.extend(styled_identity_spans(row, model, selected));
        spans.push(Span::from(ROLE_SEPARATOR));
        spans.extend(styled_identity_spans(row, row.role_label.clone(), selected));
    } else {
        let available = available_width.saturating_sub(PREFIX_WIDTH);
        let separator_width = crate::tui::width::display_width(ROLE_SEPARATOR);
        let role_width = crate::tui::width::display_width(&row.role_label);
        if available > separator_width.saturating_add(role_width) {
            let model_width = available
                .saturating_sub(separator_width)
                .saturating_sub(role_width);
            spans.extend(styled_identity_spans(
                row,
                truncate_text(&row.model_label, model_width),
                selected,
            ));
            spans.push(Span::from(ROLE_SEPARATOR));
            spans.extend(styled_identity_spans(row, row.role_label.clone(), selected));
        } else {
            spans.extend(styled_identity_spans(
                row,
                truncate_text(&row.role_label, available),
                selected,
            ));
        }
    }

    let line_style = if selected {
        Style::default().bg(SELECTION_BACKGROUND)
    } else {
        Style::default()
    };
    Line::from(spans).style(line_style)
}

fn styled_identity_spans(
    row: &AgentSwitcherRow,
    text: String,
    selected: bool,
) -> Vec<Span<'static>> {
    let (dim, bright) = row.hue.colors();
    if row.status.is_working() && !selected {
        glow_spans(&text, dim, bright)
    } else {
        vec![Span::styled(
            text,
            Style::default()
                .fg(Color::Rgb(dim.0, dim.1, dim.2))
                .add_modifier(Modifier::DIM),
        )]
    }
}

fn glow_spans(text: &str, dim: (u8, u8, u8), bright: (u8, u8, u8)) -> Vec<Span<'static>> {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return Vec::new();
    }
    let elapsed = ANIMATION_START.get_or_init(Instant::now).elapsed();
    let padding = 8_usize;
    let period = characters.len().saturating_add(padding.saturating_mul(2));
    let position = ((elapsed.as_secs_f32() % 2.4) / 2.4 * period as f32) as isize;
    let band_half_width = 4.0_f32;
    characters
        .into_iter()
        .enumerate()
        .map(|(index, character)| {
            let distance = ((index + padding) as isize - position).unsigned_abs() as f32;
            let intensity = if distance <= band_half_width {
                let phase = std::f32::consts::PI * (distance / band_half_width);
                0.5 * (1.0 + phase.cos())
            } else {
                0.0
            };
            let color = blend(bright, dim, 0.35 + intensity * 0.65);
            Span::styled(
                character.to_string(),
                Style::default()
                    .fg(Color::Rgb(color.0, color.1, color.2))
                    .bold(),
            )
        })
        .collect()
}

fn model_profile_label(selection: &ModelSelection) -> String {
    let model = if selection.model.contains("luna") {
        "Luna"
    } else if selection.model.contains("terra") {
        "Terra"
    } else {
        "Sol"
    };
    let effort = match selection.reasoning_effort {
        ReasoningEffort::Low => "Low",
        ReasoningEffort::Medium => "Medium",
        ReasoningEffort::High => "High",
        ReasoningEffort::XHigh => "XHigh",
        ReasoningEffort::Max => "Max",
    };
    format!("{model} {effort}")
}

fn pad_to_width(text: &str, width: usize) -> String {
    let padding = width.saturating_sub(crate::tui::width::display_width(text));
    format!("{text}{}", " ".repeat(padding))
}

fn truncate_text(text: &str, width: usize) -> String {
    if crate::tui::width::display_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut output = String::new();
    let mut used = 0_usize;
    let limit = width - 1;
    for character in text.chars() {
        let character_width = crate::tui::width::display_width(&character.to_string());
        if used.saturating_add(character_width) > limit {
            break;
        }
        output.push(character);
        used = used.saturating_add(character_width);
    }
    output.push('…');
    output
}

fn format_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!(
            "{}h {:02}m {:02}s",
            seconds / 3_600,
            (seconds % 3_600) / 60,
            seconds % 60
        )
    }
}

fn blend(foreground: (u8, u8, u8), background: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    (
        (foreground.0 as f32 * alpha + background.0 as f32 * (1.0 - alpha)) as u8,
        (foreground.1 as f32 * alpha + background.1 as f32 * (1.0 - alpha)) as u8,
        (foreground.2 as f32 * alpha + background.2 as f32 * (1.0 - alpha)) as u8,
    )
}
