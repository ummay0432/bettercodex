// Ported from OpenAI Codex rust-v0.147.0 (be6e8eac),
// codex-rs/tui/src/bottom_pane/experimental_features_view.rs.

use super::bottom_pane::scroll_state::ScrollState;
use super::bottom_pane::selection_popup_common::GenericDisplayRow;
use super::bottom_pane::selection_popup_common::measure_rows_height;
use super::bottom_pane::selection_popup_common::measure_text_height;
use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use super::bottom_pane::selection_popup_common::render_rows;
use super::markdown;
use crate::operator_settings::TmuxMode;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

const FOOTER_HEIGHT: u16 = 1;

pub(super) struct TmuxView {
    state: ScrollState,
    mode: TmuxMode,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TmuxViewAction {
    None,
    Save(TmuxMode),
}

impl TmuxView {
    pub(super) fn new(mode: TmuxMode) -> Self {
        let mut state = ScrollState::new();
        state.clamp_selection(1);
        Self {
            state,
            mode,
            error: None,
        }
    }

    pub(super) fn preferred_height(&self, width: u16) -> u16 {
        let header = self.header_lines();
        let rows = self.rows();
        let row_width = width.saturating_sub(2);
        measure_text_height(&header, width.saturating_sub(4))
            .saturating_add(measure_rows_height(
                &rows,
                &self.state,
                row_width.saturating_add(1),
            ))
            .saturating_add(menu_surface_padding_height())
            .saturating_add(1)
            .saturating_add(FOOTER_HEIGHT)
    }

    pub(super) fn set_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> TmuxViewAction {
        match key {
            KeyEvent {
                code: KeyCode::Esc | KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => TmuxViewAction::Save(self.mode),
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.error = None;
                self.mode = self.mode.toggled();
                TmuxViewAction::None
            }
            _ => TmuxViewAction::None,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect, surface_style: Style) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        let footer_height = FOOTER_HEIGHT.min(area.height);
        let content_height = area.height.saturating_sub(footer_height);
        let content_area = Rect::new(area.x, area.y, area.width, content_height);
        let footer_area = Rect::new(area.x, content_area.bottom(), area.width, footer_height);
        let inner = render_menu_surface(content_area, frame.buffer_mut(), surface_style);
        if !inner.is_empty() {
            let header = self.header_lines();
            let header_height = measure_text_height(&header, inner.width).min(inner.height);
            let header_area = Rect::new(inner.x, inner.y, inner.width, header_height);
            frame.render_widget(
                Paragraph::new(header).wrap(Wrap { trim: false }),
                header_area,
            );

            let rows = self.rows();
            let row_width = content_area.width.saturating_sub(2).max(1);
            let requested_rows =
                measure_rows_height(&rows, &self.state, row_width.saturating_add(1));
            let list_y = header_area.bottom().saturating_add(1).min(inner.bottom());
            let list_area = Rect::new(
                inner.x.saturating_sub(2),
                list_y,
                row_width,
                requested_rows.min(inner.bottom().saturating_sub(list_y)),
            );
            render_rows(
                list_area,
                frame.buffer_mut(),
                &rows,
                &self.state,
                "  No tmux settings available",
            );
        }

        if !footer_area.is_empty() {
            let hint_area = Rect::new(
                footer_area.x.saturating_add(2),
                footer_area.y,
                footer_area.width.saturating_sub(2),
                footer_area.height,
            );
            frame.render_widget(
                Paragraph::new("Press space to toggle or enter to save for next launch").dim(),
                hint_area,
            );
        }
    }

    fn header_lines(&self) -> Vec<Line<'static>> {
        vec![
            Line::from("Tmux").bold(),
            self.error.as_ref().map_or_else(
                || {
                    Line::from(
                        "Toggle automatic tmux sessions. Changes are saved to settings.json.",
                    )
                    .dim()
                },
                |error| Line::from(markdown::sanitize(error)).red(),
            ),
        ]
    }

    fn rows(&self) -> Vec<GenericDisplayRow> {
        let marker = if self.mode.is_on() { 'x' } else { ' ' };
        vec![GenericDisplayRow {
            name: format!("› [{marker}] Automatic tmux sessions"),
            description: Some(
                "Start interactive launches in detachable c1, c2, … sessions.".to_string(),
            ),
            ..Default::default()
        }]
    }
}

#[cfg(test)]
#[path = "tmux_view_tests.rs"]
mod tests;
