// Ported from OpenAI Codex rust-v0.147.0 (be6e8eac),
// codex-rs/tui/src/bottom_pane/skills_toggle_view.rs.

use super::bottom_pane::scroll_state::ScrollState;
use super::bottom_pane::selection_popup_common::GenericDisplayRow;
use super::bottom_pane::selection_popup_common::MAX_POPUP_ROWS;
use super::bottom_pane::selection_popup_common::measure_rows_height;
use super::bottom_pane::selection_popup_common::measure_text_height;
use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use super::bottom_pane::selection_popup_common::render_rows;
use super::markdown;
use crate::skills::Skill;
use crate::skills::SkillUpdate;
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
use std::path::PathBuf;

const FOOTER_HEIGHT: u16 = 1;

pub(super) struct SkillsView {
    state: ScrollState,
    error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SkillsViewAction {
    None,
    Close,
    Update { path: PathBuf, update: SkillUpdate },
}

impl SkillsView {
    pub(super) fn new() -> Self {
        let mut state = ScrollState::new();
        state.clamp_selection(1);
        Self { state, error: None }
    }

    pub(super) fn preferred_height(&self, skills: &[Skill], width: u16) -> u16 {
        let header = self.header_lines();
        let rows = self.rows(skills);
        let row_width = width.saturating_sub(2);
        measure_text_height(&header, width.saturating_sub(4))
            .saturating_add(measure_rows_height(
                &rows,
                &self.render_state(skills),
                row_width.saturating_add(1),
            ))
            .saturating_add(menu_surface_padding_height())
            .saturating_add(1)
            .saturating_add(FOOTER_HEIGHT)
    }

    pub(super) fn set_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
    }

    pub(super) fn clear_error(&mut self) {
        self.error = None;
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent, skills: &[Skill]) -> SkillsViewAction {
        let len = skills.len();
        self.state.clamp_selection(len);
        let visible = MAX_POPUP_ROWS.min(len);
        match key {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => return SkillsViewAction::Close,
            KeyEvent {
                code: KeyCode::Up, ..
            } => self.state.move_up_wrap(len),
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => self.state.move_down_wrap(len),
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => self.state.page_up_clamped(len, visible),
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => self.state.page_down_clamped(len, visible),
            KeyEvent {
                code: KeyCode::Home,
                ..
            } => self.state.jump_top(len, visible),
            KeyEvent {
                code: KeyCode::End, ..
            } => self.state.jump_bottom(len, visible),
            KeyEvent {
                code: KeyCode::Enter | KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let Some(skill) = self.selected_skill(skills) {
                    self.error = None;
                    return SkillsViewAction::Update {
                        path: skill.path().to_path_buf(),
                        update: SkillUpdate::Enabled(!skill.is_enabled()),
                    };
                }
            }
            KeyEvent {
                code: KeyCode::Char('i' | 'I'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let Some(skill) = self.selected_skill(skills) {
                    self.error = None;
                    return SkillsViewAction::Update {
                        path: skill.path().to_path_buf(),
                        update: SkillUpdate::AllowImplicitInvocation(
                            !skill.allows_implicit_invocation(),
                        ),
                    };
                }
            }
            _ => return SkillsViewAction::None,
        }
        self.state.ensure_visible(len, visible);
        self.error = None;
        SkillsViewAction::None
    }

    pub(super) fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        skills: &[Skill],
        surface_style: Style,
    ) {
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

            let rows = self.rows(skills);
            let state = self.render_state(skills);
            let row_width = content_area.width.saturating_sub(2).max(1);
            let requested_rows = measure_rows_height(&rows, &state, row_width.saturating_add(1));
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
                &state,
                "  No skills installed",
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
                Paragraph::new(
                    "Press space or enter to toggle enabled; i to toggle implicit; esc to close",
                )
                .dim(),
                hint_area,
            );
        }
    }

    fn selected_skill<'a>(&self, skills: &'a [Skill]) -> Option<&'a Skill> {
        self.state.selected_idx.and_then(|index| skills.get(index))
    }

    fn render_state(&self, skills: &[Skill]) -> ScrollState {
        let mut state = self.state;
        state.clamp_selection(skills.len());
        state
    }

    fn header_lines(&self) -> Vec<Line<'static>> {
        vec![
            Line::from("Enable/Disable Skills").bold(),
            self.error.as_ref().map_or_else(
                || {
                    Line::from(
                        "Enable skills or restrict them to explicit $ mentions. Changes are saved automatically.",
                    )
                    .dim()
                },
                |error| Line::from(markdown::sanitize(error)).red(),
            ),
        ]
    }

    fn rows(&self, skills: &[Skill]) -> Vec<GenericDisplayRow> {
        if skills.is_empty() {
            return vec![GenericDisplayRow {
                name: "  No skills installed.".to_string(),
                description: Some(
                    "Add SKILL.md under .bcodex/skills or ${BCODEX_HOME:-$HOME/.bcodex}/skills."
                        .to_string(),
                ),
                ..Default::default()
            }];
        }
        let selected = self
            .state
            .selected_idx
            .unwrap_or_default()
            .min(skills.len() - 1);
        skills
            .iter()
            .enumerate()
            .map(|(index, skill)| {
                let prefix = if index == selected { '›' } else { ' ' };
                let enabled = if skill.is_enabled() { 'x' } else { ' ' };
                let implicit = if skill.allows_implicit_invocation() {
                    'x'
                } else {
                    ' '
                };
                GenericDisplayRow {
                    name: format!(
                        "{prefix} [{enabled}] enabled  [{implicit}] implicit  {}",
                        skill.display_name()
                    ),
                    description: Some(skill.display_description().to_string()),
                    ..Default::default()
                }
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "skills_view_tests.rs"]
mod tests;
