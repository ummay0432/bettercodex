use super::markdown;
use crate::operator_settings::TmuxMode;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
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
const PREFERRED_WIDTH: u16 = 88;
const PANEL_HEIGHT: u16 = 7;

pub(super) struct TmuxView {
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TmuxViewAction {
    None,
    Close,
    SetMode(TmuxMode),
}

impl TmuxView {
    pub(super) fn new() -> Self {
        Self { error: None }
    }

    pub(super) const fn preferred_height(&self) -> u16 {
        PANEL_HEIGHT
    }

    pub(super) fn set_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
    }

    pub(super) fn clear_error(&mut self) {
        self.error = None;
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent, mode: TmuxMode) -> TmuxViewAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => TmuxViewAction::Close,
            KeyCode::Enter | KeyCode::Char(' ')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.error = None;
                TmuxViewAction::SetMode(mode.toggled())
            }
            _ => TmuxViewAction::None,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect, mode: TmuxMode) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        let panel = Rect::new(
            area.x,
            area.y,
            PREFERRED_WIDTH.min(area.width),
            self.preferred_height().min(area.height),
        );
        let block = Block::default()
            .title(" Tmux ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(RULE));
        let inner = block.inner(panel);
        frame.render_widget(block, panel);
        if inner.is_empty() {
            return;
        }

        let status_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let footer_height = u16::from(inner.height > 2);
        let setting_y = status_area.bottom().saturating_add(1).min(inner.bottom());
        let setting_area = Rect::new(
            inner.x,
            setting_y,
            inner.width,
            inner
                .bottom()
                .saturating_sub(setting_y)
                .saturating_sub(footer_height),
        );
        let footer_area = Rect::new(
            inner.x,
            inner.bottom().saturating_sub(footer_height),
            inner.width,
            footer_height,
        );

        let status = self.error.as_ref().map_or_else(
            || Line::from("Changes save automatically and apply on the next launch.").dim(),
            |error| Line::from(markdown::sanitize(error)).red(),
        );
        frame.render_widget(Paragraph::new(status), status_area);

        if !setting_area.is_empty() {
            let enabled = if mode.is_on() { "x" } else { " " };
            let selected = Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD);
            let lines = vec![
                Line::from(vec![
                    Span::styled("› ", selected),
                    Span::styled(
                        format!("[{enabled}] Automatic tmux sessions ({})", mode.label()),
                        selected,
                    ),
                ]),
                Line::from("    Start interactive launches in detachable c1, c2, … sessions.")
                    .style(selected),
            ];
            frame.render_widget(Paragraph::new(lines), setting_area);
        }

        if !footer_area.is_empty() {
            frame.render_widget(
                Paragraph::new("space/enter toggle · esc close")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(MUTED)),
                footer_area,
            );
        }
    }
}

#[cfg(test)]
#[path = "tmux_view_tests.rs"]
mod tests;
