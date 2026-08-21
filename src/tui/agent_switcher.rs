use super::palette;
use crate::deepwork::SpecialistRole;
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
use unicode_segmentation::UnicodeSegmentation;

const SELECTOR_WIDTH: usize = 2;
const ROOT_ROWS: usize = 1;
const TREE_CONNECTOR: &str = "├── ";
const TREE_LAST_CONNECTOR: &str = "└── ";
const ROLE_SEPARATOR: &str = " | ";
const STATUS_SEPARATOR: &str = " · ";
const SELECTION_BACKGROUND: Color = Color::Rgb(38, 38, 42);
const INACTIVE_GUNMETAL: (u8, u8, u8) = (98, 108, 113);
const ACTIVE_GUNMETAL: (u8, u8, u8) = (166, 171, 174);
const INACTIVE_GUNMETAL_ANSI: u8 = 245;
const ACTIVE_GUNMETAL_ANSI: u8 = 250;
const SHIMMER_PERIOD: Duration = Duration::from_secs(2);
const SHIMMER_PADDING: usize = 10;
const SHIMMER_HALF_WIDTH: f32 = 5.0;

static SHIMMER_START: OnceLock<Instant> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentSwitcherStatus {
    Working(Duration),
    Cancelling(Duration),
    Paused,
    AwaitingReview,
    Waiting,
    Queued,
    Skipped,
    Accepted,
}

impl AgentSwitcherStatus {
    fn label(self) -> String {
        match self {
            Self::Working(elapsed) => {
                format!("Working ({})", format_elapsed(elapsed.as_secs()))
            }
            Self::Cancelling(elapsed) => {
                format!("Cancelling ({})", format_elapsed(elapsed.as_secs()))
            }
            Self::Paused => "Paused".to_string(),
            Self::AwaitingReview => "Awaiting review".to_string(),
            Self::Waiting => "Waiting".to_string(),
            Self::Queued => "Queued".to_string(),
            Self::Skipped => "Skipped".to_string(),
            Self::Accepted => "Accepted".to_string(),
        }
    }

    const fn should_shimmer(self) -> bool {
        matches!(self, Self::Working(_) | Self::Cancelling(_))
    }

    const fn is_queued(self) -> bool {
        matches!(self, Self::Queued)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum AgentSwitcherSelection {
    Main,
    Specialist(SpecialistRole),
}

impl AgentSwitcherSelection {
    pub(super) const ROWS: [Self; 5] = [
        Self::Main,
        Self::Specialist(SpecialistRole::Acceptance),
        Self::Specialist(SpecialistRole::Manifest),
        Self::Specialist(SpecialistRole::Worker),
        Self::Specialist(SpecialistRole::Reviewer),
    ];
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentSwitcherRow {
    selection: AgentSwitcherSelection,
    session_id: Option<SessionId>,
    model_label: String,
    role_label: String,
    status: AgentSwitcherStatus,
}

impl AgentSwitcherRow {
    pub(super) fn main(
        session_id: SessionId,
        selection: &ModelSelection,
        status: AgentSwitcherStatus,
    ) -> Self {
        Self {
            selection: AgentSwitcherSelection::Main,
            session_id: Some(session_id),
            model_label: model_profile_label(selection),
            role_label: "Main".to_string(),
            status,
        }
    }

    pub(super) fn specialist(
        session_id: Option<SessionId>,
        selection: &ModelSelection,
        role: &str,
        status: AgentSwitcherStatus,
    ) -> Self {
        let role = SpecialistRole::parse(role)
            .unwrap_or_else(|_| unreachable!("agent switcher roles are fixed specialists"));
        Self {
            selection: AgentSwitcherSelection::Specialist(role),
            session_id,
            model_label: model_profile_label(selection),
            role_label: role.label().to_string(),
            status,
        }
    }

    fn has_session(&self, session_id: &SessionId) -> bool {
        self.session_id.as_ref() == Some(session_id)
    }

    const fn uses_bright_baseline(&self) -> bool {
        matches!(self.selection, AgentSwitcherSelection::Main) || !self.status.is_queued()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct AgentSwitcher {
    rows: Vec<AgentSwitcherRow>,
    active: Option<SessionId>,
    selected: Option<AgentSwitcherSelection>,
}

impl AgentSwitcher {
    pub(super) fn new(
        rows: Vec<AgentSwitcherRow>,
        active: Option<SessionId>,
        selected: Option<AgentSwitcherSelection>,
    ) -> Self {
        let active = active.filter(|active| rows.iter().any(|row| row.has_session(active)));
        let selected =
            selected.filter(|selected| rows.iter().any(|row| row.selection == *selected));
        Self {
            rows,
            active,
            selected,
        }
    }

    pub(super) fn is_selecting(&self) -> bool {
        self.selected.is_some()
    }

    pub(super) fn preferred_height(&self) -> u16 {
        if self.rows.is_empty() {
            return 0;
        }
        u16::try_from(self.rows.len().saturating_add(ROOT_ROWS)).unwrap_or(u16::MAX)
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() || self.rows.is_empty() {
            return;
        }
        let total_rows = self.rows.len().saturating_add(ROOT_ROWS);
        let visible = usize::from(area.height).min(total_rows);
        let range = self.visible_range(visible);
        let role_width = self
            .rows
            .iter()
            .map(|row| crate::tui::width::display_width(&row.role_label))
            .max()
            .unwrap_or_default();
        let model_width = self
            .rows
            .iter()
            .map(|row| crate::tui::width::display_width(&row.model_label))
            .max()
            .unwrap_or_default();
        for (offset, tree_index) in range.enumerate() {
            let Ok(offset) = u16::try_from(offset) else {
                break;
            };
            let row_area = Rect::new(area.x, area.y.saturating_add(offset), area.width, 1);
            if tree_index == 0 {
                frame.render_widget(
                    Paragraph::new(render_root(usize::from(row_area.width))),
                    row_area,
                );
                continue;
            }
            let row_index = tree_index - ROOT_ROWS;
            let row = &self.rows[row_index];
            let selected = self.selected == Some(row.selection);
            if selected {
                frame.render_widget(
                    Block::default().style(Style::default().bg(SELECTION_BACKGROUND)),
                    row_area,
                );
            }
            frame.render_widget(
                Paragraph::new(render_agent_row(
                    row,
                    row_index + 1 == self.rows.len(),
                    role_width,
                    model_width,
                    usize::from(row_area.width),
                    selected,
                    shimmer_elapsed(),
                )),
                row_area,
            );
        }
    }

    fn visible_range(&self, visible: usize) -> std::ops::Range<usize> {
        let total_rows = self.rows.len().saturating_add(ROOT_ROWS);
        if visible >= total_rows {
            return 0..total_rows;
        }
        let selected = self
            .selected
            .and_then(|selected| self.rows.iter().position(|row| row.selection == selected));
        let active = self
            .active
            .as_ref()
            .and_then(|session_id| self.rows.iter().position(|row| row.has_session(session_id)));
        let anchor = selected
            .or(active)
            .map_or(0, |index| index.saturating_add(ROOT_ROWS));
        let start = anchor
            .saturating_sub(visible / 2)
            .min(total_rows.saturating_sub(visible));
        start..start.saturating_add(visible)
    }
}

pub(super) fn move_selection(
    rows: &[AgentSwitcherSelection],
    selected: Option<AgentSwitcherSelection>,
    forward: bool,
) -> Option<AgentSwitcherSelection> {
    if rows.is_empty() {
        return None;
    }
    let selected = selected.and_then(|selected| rows.iter().position(|row| *row == selected));
    let index = match (selected, forward) {
        (None, true) => Some(0),
        (None, false) => Some(rows.len() - 1),
        (Some(index), true) if index + 1 < rows.len() => Some(index + 1),
        (Some(index), false) if index > 0 => Some(index - 1),
        (Some(_), _) => None,
    };
    index.and_then(|index| rows.get(index).copied())
}

fn render_root(available_width: usize) -> Line<'static> {
    let content = truncate_text("$deepwork", available_width.saturating_sub(SELECTOR_WIDTH));
    Line::from(vec![
        Span::from("  "),
        Span::styled(content, palette::soft_accent_style()),
    ])
}

#[allow(clippy::too_many_arguments)]
fn render_agent_row(
    row: &AgentSwitcherRow,
    last: bool,
    role_width: usize,
    model_width: usize,
    available_width: usize,
    selected: bool,
    elapsed: Duration,
) -> Line<'static> {
    let connector = if last {
        TREE_LAST_CONNECTOR
    } else {
        TREE_CONNECTOR
    };
    let content = format!(
        "{connector}{}{}{}{}{}",
        pad_to_width(&row.role_label, role_width),
        ROLE_SEPARATOR,
        pad_to_width(&row.model_label, model_width),
        STATUS_SEPARATOR,
        row.status.label(),
    );
    let content = truncate_text(&content, available_width.saturating_sub(SELECTOR_WIDTH));
    let mut spans = Vec::new();
    spans.push(if selected {
        Span::styled("› ", Style::default().fg(Color::White).bold())
    } else {
        Span::from("  ")
    });
    if row.status.should_shimmer() {
        spans.extend(shimmer_spans_at(
            &content,
            elapsed,
            palette::terminal_colors().map(|colors| colors.foreground),
            crate::terminal_color::stdout_supports_truecolor(),
        ));
    } else if row.uses_bright_baseline() {
        spans.push(Span::styled(content, bright_gunmetal_style()));
    } else {
        spans.push(Span::styled(content, inactive_gunmetal_style()));
    }
    let line_style = if selected {
        Style::default().bg(SELECTION_BACKGROUND)
    } else {
        Style::default()
    };
    Line::from(spans).style(line_style)
}

fn shimmer_elapsed() -> Duration {
    SHIMMER_START.get_or_init(Instant::now).elapsed()
}

fn shimmer_spans_at(
    text: &str,
    elapsed: Duration,
    terminal_foreground: Option<(u8, u8, u8)>,
    true_color: bool,
) -> Vec<Span<'static>> {
    let width = crate::tui::width::display_width(text);
    if width == 0 {
        return Vec::new();
    }
    let period = width.saturating_add(SHIMMER_PADDING.saturating_mul(2));
    let progress = elapsed.as_secs_f32() % SHIMMER_PERIOD.as_secs_f32();
    let position = progress / SHIMMER_PERIOD.as_secs_f32() * period as f32;
    let highlight = terminal_foreground.unwrap_or((235, 238, 240));
    let mut cell_offset = 0_usize;

    text.graphemes(true)
        .map(|grapheme| {
            let grapheme_width = crate::tui::width::display_width(grapheme);
            let center = cell_offset as f32 + grapheme_width as f32 / 2.0;
            cell_offset = cell_offset.saturating_add(grapheme_width);
            let distance = (center + SHIMMER_PADDING as f32 - position).abs();
            let intensity = if distance <= SHIMMER_HALF_WIDTH {
                let phase = std::f32::consts::PI * (distance / SHIMMER_HALF_WIDTH);
                0.5 * (1.0 + phase.cos())
            } else {
                0.0
            };
            Span::styled(
                grapheme.to_string(),
                shimmer_style(intensity, highlight, true_color),
            )
        })
        .collect()
}

fn shimmer_style(intensity: f32, highlight: (u8, u8, u8), true_color: bool) -> Style {
    if true_color {
        let color = palette::blend(highlight, ACTIVE_GUNMETAL, intensity.clamp(0.0, 1.0) * 0.9);
        return Style::default().fg(Color::Rgb(color.0, color.1, color.2));
    }
    let style = Style::default().fg(Color::Indexed(ACTIVE_GUNMETAL_ANSI));
    if intensity < 0.25 {
        style
    } else if intensity < 0.7 {
        style.add_modifier(Modifier::BOLD)
    } else {
        style.fg(Color::White).add_modifier(Modifier::BOLD)
    }
}

fn bright_gunmetal_style() -> Style {
    let color = if crate::terminal_color::stdout_supports_truecolor() {
        Color::Rgb(ACTIVE_GUNMETAL.0, ACTIVE_GUNMETAL.1, ACTIVE_GUNMETAL.2)
    } else {
        Color::Indexed(ACTIVE_GUNMETAL_ANSI)
    };
    Style::default().fg(color)
}

fn inactive_gunmetal_style() -> Style {
    let color = if crate::terminal_color::stdout_supports_truecolor() {
        Color::Rgb(
            INACTIVE_GUNMETAL.0,
            INACTIVE_GUNMETAL.1,
            INACTIVE_GUNMETAL.2,
        )
    } else {
        Color::Indexed(INACTIVE_GUNMETAL_ANSI)
    };
    Style::default().fg(color).add_modifier(Modifier::DIM)
}

fn model_profile_label(selection: &ModelSelection) -> String {
    let model = if selection.model.contains("luna") {
        "luna"
    } else if selection.model.contains("terra") {
        "terra"
    } else {
        "sol"
    };
    let effort = match selection.reasoning_effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::XHigh => "xhigh",
        ReasoningEffort::Max => "max",
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
    for grapheme in text.graphemes(true) {
        let grapheme_width = crate::tui::width::display_width(grapheme);
        if used.saturating_add(grapheme_width) > limit {
            break;
        }
        output.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::UserPrompt;
    use crate::tui::view::View;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::collections::HashSet;
    use std::path::Path;

    fn session_id(value: u128) -> SessionId {
        let Ok(session_id) =
            SessionId::parse(uuid::Uuid::from_u128(value).hyphenated().to_string())
        else {
            panic!("test UUID should be a canonical session ID");
        };
        session_id
    }

    fn pipeline_rows() -> Vec<AgentSwitcherRow> {
        vec![
            AgentSwitcherRow::main(
                session_id(1),
                &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
                AgentSwitcherStatus::Waiting,
            ),
            AgentSwitcherRow::specialist(
                None,
                &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
                "acceptance",
                AgentSwitcherStatus::Accepted,
            ),
            AgentSwitcherRow::specialist(
                None,
                &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
                "manifest",
                AgentSwitcherStatus::Skipped,
            ),
            AgentSwitcherRow::specialist(
                Some(session_id(2)),
                &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
                "worker",
                AgentSwitcherStatus::Working(Duration::from_secs(134)),
            ),
            AgentSwitcherRow::specialist(
                None,
                &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::Max),
                "reviewer",
                AgentSwitcherStatus::Queued,
            ),
        ]
    }

    fn view_with_tree(active: SessionId) -> View {
        let mut view = View::new(Path::new("/tmp/bettercodex-switcher-render"));
        view.start_turn(&UserPrompt::text("keep Main working"));
        view.set_agent_switcher(AgentSwitcher::new(pipeline_rows(), Some(active), None));
        view
    }

    fn render(view: &mut View, width: u16, height: u16) -> (ratatui::buffer::Buffer, Vec<String>) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap_or_else(|error| match error {});
        let prepared = view.prepare(width, height);
        if let Err(error) = terminal.draw(|frame| view.render_prepared(frame, prepared)) {
            panic!("test terminal should render: {error}");
        }
        let buffer = terminal.backend().buffer().clone();
        let lines = (buffer.area.y..buffer.area.bottom())
            .map(|y| {
                (buffer.area.x..buffer.area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        (buffer, lines)
    }

    #[test]
    fn persistent_pipeline_tree_renders_between_activity_and_composer() {
        let mut view = view_with_tree(session_id(1));
        let (buffer, lines) = render(&mut view, 80, 24);
        let Some(activity) = lines
            .iter()
            .position(|line| line.contains("Working") && line.contains("esc to interrupt"))
        else {
            panic!("activity row missing: {lines:#?}");
        };
        let expected = [
            "$deepwork",
            "├── Main        | sol xhigh · Waiting",
            "├── $acceptance | sol xhigh · Accepted",
            "├── $manifest   | sol xhigh · Skipped",
            "├── $worker     | sol xhigh · Working (2m 14s)",
            "└── $reviewer   | sol max   · Queued",
        ];
        let positions = expected
            .iter()
            .map(|expected| {
                lines
                    .iter()
                    .position(|line| line.contains(expected))
                    .unwrap_or_else(|| panic!("tree row `{expected}` missing: {lines:#?}"))
            })
            .collect::<Vec<_>>();
        let Some(composer) = lines.iter().position(|line| line.trim() == "›") else {
            panic!("composer row missing: {lines:#?}");
        };

        assert_eq!(positions[0], activity + 2, "{lines:#?}");
        assert!(lines[activity + 1].trim().is_empty(), "{lines:#?}");
        for pair in positions.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "{lines:#?}");
        }
        assert_eq!(composer, positions[5] + 3, "{lines:#?}");
        let gap = positions[5] + 1;
        assert!(lines[gap].trim().is_empty(), "{lines:#?}");
        let Ok(gap_y) = u16::try_from(gap) else {
            panic!("tree-to-composer gap should fit in the test terminal");
        };
        for x in buffer.area.x..buffer.area.right() {
            let cell = &buffer[(x, gap_y)];
            assert_eq!(cell.symbol(), " ");
            assert_eq!(cell.fg, Color::Reset);
            assert_eq!(cell.bg, Color::Reset);
            assert!(cell.modifier.is_empty());
        }
        let Ok(root_y) = u16::try_from(positions[0]) else {
            panic!("tree root should fit in the test terminal");
        };
        assert_eq!(
            buffer[(2, root_y)].fg,
            palette::soft_accent_style().fg.unwrap_or(Color::Reset)
        );
        assert!(buffer[(2, root_y)].modifier.contains(Modifier::DIM));
        let Ok(main_y) = u16::try_from(positions[1]) else {
            panic!("tree row should fit in the test terminal");
        };
        assert_eq!(
            buffer[(2, main_y)].fg,
            bright_gunmetal_style().fg.unwrap_or(Color::Reset)
        );
        assert!(!buffer[(2, main_y)].modifier.contains(Modifier::DIM));
        let Ok(accepted_y) = u16::try_from(positions[2]) else {
            panic!("accepted row should fit in the test terminal");
        };
        assert_eq!(
            buffer[(2, accepted_y)].fg,
            bright_gunmetal_style().fg.unwrap_or(Color::Reset)
        );
        assert!(!buffer[(2, accepted_y)].modifier.contains(Modifier::DIM));
        let Ok(skipped_y) = u16::try_from(positions[3]) else {
            panic!("skipped row should fit in the test terminal");
        };
        assert_eq!(
            buffer[(2, skipped_y)].fg,
            bright_gunmetal_style().fg.unwrap_or(Color::Reset)
        );
        assert!(!buffer[(2, skipped_y)].modifier.contains(Modifier::DIM));
        let Ok(queued_y) = u16::try_from(positions[5]) else {
            panic!("queued row should fit in the test terminal");
        };
        assert_eq!(
            buffer[(2, queued_y)].fg,
            inactive_gunmetal_style().fg.unwrap_or(Color::Reset)
        );
        assert!(buffer[(2, queued_y)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn cramped_tree_scrolls_around_the_active_session() {
        let mut view = view_with_tree(session_id(2));
        let (_, lines) = render(&mut view, 80, 6);

        assert!(
            lines.iter().any(|line| line.contains("$manifest")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|line| line.trim() == "›"), "{lines:#?}");
    }

    #[test]
    fn lifecycle_statuses_are_explicit() {
        assert_eq!(
            AgentSwitcherStatus::Cancelling(Duration::from_secs(72)).label(),
            "Cancelling (1m 12s)"
        );
        assert_eq!(AgentSwitcherStatus::Paused.label(), "Paused");
        assert_eq!(
            AgentSwitcherStatus::AwaitingReview.label(),
            "Awaiting review"
        );
        assert_eq!(AgentSwitcherStatus::Waiting.label(), "Waiting");
        assert_eq!(AgentSwitcherStatus::Queued.label(), "Queued");
        assert_eq!(AgentSwitcherStatus::Skipped.label(), "Skipped");
        assert_eq!(AgentSwitcherStatus::Accepted.label(), "Accepted");
    }

    #[test]
    fn switcher_navigation_includes_all_rows_and_the_composer() {
        let rows = AgentSwitcherSelection::ROWS;

        assert_eq!(
            move_selection(&rows, None, true),
            Some(AgentSwitcherSelection::Main)
        );
        assert_eq!(
            move_selection(&rows, Some(AgentSwitcherSelection::Main), true),
            Some(AgentSwitcherSelection::Specialist(
                SpecialistRole::Acceptance
            ))
        );
        assert_eq!(
            move_selection(
                &rows,
                Some(AgentSwitcherSelection::Specialist(SpecialistRole::Reviewer)),
                true,
            ),
            None,
            "moving below the final tree row should return focus to the composer"
        );
        assert_eq!(
            move_selection(&rows, None, false),
            Some(AgentSwitcherSelection::Specialist(SpecialistRole::Reviewer))
        );
        assert_eq!(
            move_selection(&rows, Some(AgentSwitcherSelection::Main), false),
            None,
            "moving above Main should return focus to the composer"
        );
    }

    #[test]
    fn queued_and_current_rows_can_be_selected() {
        let active = session_id(1);
        let queued = AgentSwitcherSelection::Specialist(SpecialistRole::Worker);
        let switcher = AgentSwitcher::new(pipeline_rows(), Some(active.clone()), Some(queued));

        assert_eq!(switcher.active, Some(active.clone()));
        assert_eq!(switcher.selected, Some(queued));
        assert_eq!(switcher.preferred_height(), 6);

        let current = AgentSwitcher::new(
            pipeline_rows(),
            Some(active),
            Some(AgentSwitcherSelection::Main),
        );
        assert_eq!(current.selected, Some(AgentSwitcherSelection::Main));
    }

    #[test]
    fn rows_use_measured_role_first_columns() {
        let row = AgentSwitcherRow::specialist(
            None,
            &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
            "acceptance",
            AgentSwitcherStatus::Accepted,
        );
        let line = render_agent_row(&row, false, 11, 9, 80, false, Duration::ZERO);
        let rendered = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(rendered, "  ├── $acceptance | sol xhigh · Accepted");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[1].style, bright_gunmetal_style());
    }

    #[test]
    fn working_main_and_cancelling_specialist_rows_shimmer() {
        let main = AgentSwitcherRow::main(
            session_id(1),
            &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
            AgentSwitcherStatus::Working(Duration::from_secs(8)),
        );
        let cancelling = AgentSwitcherRow::specialist(
            Some(session_id(2)),
            &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
            "acceptance",
            AgentSwitcherStatus::Cancelling(Duration::from_secs(9)),
        );

        for row in [&main, &cancelling] {
            let line = render_agent_row(row, false, 9, 9, 80, false, Duration::from_secs(1));
            assert!(
                line.spans.len() > 2,
                "active work should use row-wide shimmer spans"
            );
        }
    }

    #[test]
    fn working_row_uses_one_continuous_display_cell_shimmer() {
        let text = "├── $manifest | sol xhigh · Working (2m 14s)";
        let spans = shimmer_spans_at(
            text,
            Duration::from_millis(1_000),
            Some((240, 240, 240)),
            true,
        );
        let rendered = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let styles = spans.iter().map(|span| span.style).collect::<HashSet<_>>();

        assert_eq!(rendered, text);
        assert!(
            styles.len() > 1,
            "the shimmer should cross the assembled row"
        );
        assert_eq!(
            spans.len(),
            text.graphemes(true).count(),
            "the row should be styled from one display-cell sequence"
        );
    }
}
