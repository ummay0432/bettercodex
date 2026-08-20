use crate::model::ModelSelection;
use crate::model::ReasoningEffort;
use crate::session_group::SessionId;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use std::time::Duration;

const PREFIX_WIDTH: usize = 2;
const ROLE_SEPARATOR: &str = " | ";
const STATUS_SEPARATOR: &str = " · ";
const SELECTION_BACKGROUND: Color = Color::Rgb(38, 38, 42);
const GUNMETAL: (u8, u8, u8) = (98, 108, 113);
const GUNMETAL_ANSI: u8 = 245;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentSwitcherStatus {
    Working(Duration),
    Cancelling(Duration),
    Paused,
    AwaitingReview,
    Idle,
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
            Self::Idle => "Idle".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentSwitcherRow {
    pub(super) session_id: SessionId,
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
            session_id,
            model_label: model_profile_label(selection),
            role_label: "Main".to_string(),
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
            status,
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
                    role_width,
                    model_width,
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
    role_width: usize,
    model_width: usize,
    available_width: usize,
    selected: bool,
) -> Line<'static> {
    let content = format!(
        "{}{}{}{}{}",
        pad_to_width(&row.role_label, role_width),
        ROLE_SEPARATOR,
        pad_to_width(&row.model_label, model_width),
        STATUS_SEPARATOR,
        status,
    );
    let available = available_width.saturating_sub(PREFIX_WIDTH);
    let content = truncate_text(&content, available);
    let line_style = if selected {
        Style::default().bg(SELECTION_BACKGROUND)
    } else {
        Style::default()
    };
    Line::from(vec![
        if selected {
            Span::styled("› ", Style::default().fg(Color::White).bold())
        } else {
            Span::from("  ")
        },
        Span::styled(content, gunmetal_style()),
    ])
    .style(line_style)
}

fn gunmetal_style() -> Style {
    let color = if crate::terminal_color::stdout_supports_truecolor() {
        Color::Rgb(GUNMETAL.0, GUNMETAL.1, GUNMETAL.2)
    } else {
        Color::Indexed(GUNMETAL_ANSI)
    };
    Style::default().fg(color)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::UserPrompt;
    use crate::tui::view::View;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::Path;

    fn session_id(value: u128) -> SessionId {
        let Ok(session_id) =
            SessionId::parse(uuid::Uuid::from_u128(value).hyphenated().to_string())
        else {
            panic!("test UUID should be a canonical session ID");
        };
        session_id
    }

    fn view_with_switcher() -> View {
        let mut view = View::new(Path::new("/tmp/bettercodex-switcher-render"));
        view.start_turn(&UserPrompt::text("keep Main working"));
        view.set_agent_switcher(AgentSwitcher::new(
            vec![AgentSwitcherRow::specialist(
                session_id(1),
                &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
                "evals",
                AgentSwitcherStatus::Working(Duration::from_secs(72)),
            )],
            None,
        ));
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
    fn working_switcher_row_renders_between_activity_and_composer() {
        let mut view = view_with_switcher();
        let (buffer, lines) = render(&mut view, 80, 24);
        let Some(activity) = lines
            .iter()
            .position(|line| line.contains("Working") && line.contains("esc to interrupt"))
        else {
            panic!("activity row missing: {lines:#?}");
        };
        let Some(switcher) = lines
            .iter()
            .position(|line| line.contains("$evals | sol xhigh · Working (1m 12s)"))
        else {
            panic!("switcher row missing: {lines:#?}");
        };
        let Some(composer) = lines.iter().position(|line| line.trim() == "›") else {
            panic!("composer row missing: {lines:#?}");
        };

        assert_eq!(switcher, activity + 2, "{lines:#?}");
        assert!(lines[activity + 1].trim().is_empty(), "{lines:#?}");
        assert!(switcher < composer, "{lines:#?}");
        let Ok(switcher_y) = u16::try_from(switcher) else {
            panic!("switcher row should fit in the test terminal");
        };
        let identity_cell = &buffer[(2, switcher_y)];
        assert_eq!(
            identity_cell.fg,
            gunmetal_style().fg.unwrap_or(Color::Reset)
        );
    }

    #[test]
    fn cramped_bottom_pane_preserves_the_switcher_before_the_activity_row() {
        let mut view = view_with_switcher();
        let (_, lines) = render(&mut view, 80, 6);

        assert!(
            lines
                .iter()
                .any(|line| line.contains("$evals | sol xhigh · Working (1m 12s)")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|line| line.trim() == "›"), "{lines:#?}");
    }

    #[test]
    fn cancellation_statuses_are_explicit() {
        assert_eq!(
            AgentSwitcherStatus::Cancelling(Duration::from_secs(72)).label(),
            "Cancelling (1m 12s)"
        );
        assert_eq!(AgentSwitcherStatus::Paused.label(), "Paused");
    }

    #[test]
    fn switcher_rows_use_role_first_columns_and_one_static_gunmetal_style() {
        let row = AgentSwitcherRow::specialist(
            session_id(1),
            &ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
            "evals",
            AgentSwitcherStatus::Working(Duration::from_secs(155)),
        );
        let line = render_row(&row, "Working (2m 35s)", 9, 9, 80, false);
        let rendered = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(rendered, "  $evals    | sol xhigh · Working (2m 35s)");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[1].style, gunmetal_style());
    }
}
