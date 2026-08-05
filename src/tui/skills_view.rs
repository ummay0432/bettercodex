use super::markdown;
use crate::skills::Skill;
use crate::skills::SkillUpdate;
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
use std::path::PathBuf;

const MUTED: Color = Color::Indexed(245);
const RULE: Color = Color::Indexed(8);
const PREFERRED_WIDTH: u16 = 100;
const MAX_VISIBLE_SKILLS: usize = 8;
const PANEL_CHROME_HEIGHT: u16 = 5;

pub(super) struct SkillsView {
    selected: usize,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SkillsViewAction {
    None,
    Close,
    Update { path: PathBuf, update: SkillUpdate },
}

impl SkillsView {
    pub(super) fn new() -> Self {
        Self {
            selected: 0,
            error: None,
        }
    }

    pub(super) fn preferred_height(&self, skills: &[Skill]) -> u16 {
        let rows = if skills.is_empty() {
            2
        } else {
            skills.len().min(MAX_VISIBLE_SKILLS).saturating_mul(2)
        };
        PANEL_CHROME_HEIGHT.saturating_add(u16::try_from(rows).unwrap_or(u16::MAX))
    }

    pub(super) fn set_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent, skills: &[Skill]) -> SkillsViewAction {
        self.selected = self.selected.min(skills.len().saturating_sub(1));
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return SkillsViewAction::Close,
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.error = None;
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(skills.len().saturating_sub(1));
                self.error = None;
            }
            KeyCode::Home => {
                self.selected = 0;
                self.error = None;
            }
            KeyCode::End => {
                self.selected = skills.len().saturating_sub(1);
                self.error = None;
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(MAX_VISIBLE_SKILLS);
                self.error = None;
            }
            KeyCode::PageDown => {
                self.selected =
                    (self.selected + MAX_VISIBLE_SKILLS).min(skills.len().saturating_sub(1));
                self.error = None;
            }
            KeyCode::Enter | KeyCode::Char(' ')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(skill) = skills.get(self.selected) {
                    self.error = None;
                    return SkillsViewAction::Update {
                        path: skill.path().to_path_buf(),
                        update: SkillUpdate::Enabled(!skill.is_enabled()),
                    };
                }
            }
            KeyCode::Char('i' | 'I')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(skill) = skills.get(self.selected) {
                    self.error = None;
                    return SkillsViewAction::Update {
                        path: skill.path().to_path_buf(),
                        update: SkillUpdate::AllowImplicitInvocation(
                            !skill.allows_implicit_invocation(),
                        ),
                    };
                }
            }
            _ => {}
        }
        SkillsViewAction::None
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect, skills: &[Skill]) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        let panel = Rect::new(
            area.x,
            area.y,
            PREFERRED_WIDTH.min(area.width),
            self.preferred_height(skills).min(area.height),
        );
        let block = Block::default()
            .title(" Skills ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(RULE));
        let inner = block.inner(panel);
        frame.render_widget(block, panel);
        if inner.is_empty() {
            return;
        }

        let status_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let footer_height = u16::from(inner.height > 2);
        let list_y = status_area.bottom().saturating_add(1).min(inner.bottom());
        let list_area = Rect::new(
            inner.x,
            list_y,
            inner.width,
            inner
                .bottom()
                .saturating_sub(list_y)
                .saturating_sub(footer_height),
        );
        let footer_area = Rect::new(
            inner.x,
            inner.bottom().saturating_sub(footer_height),
            inner.width,
            footer_height,
        );

        let status = self.error.as_ref().map_or_else(
            || {
                Line::from(
                    "Enable skills or restrict them to explicit $ mentions. Changes save automatically.",
                )
                .dim()
            },
            |error| Line::from(markdown::sanitize(error)).red(),
        );
        frame.render_widget(Paragraph::new(status), status_area);
        self.render_skills(frame, list_area, skills);
        if !footer_area.is_empty() {
            frame.render_widget(
                Paragraph::new("space/enter enabled · i implicit · esc close · ↑/↓ browse")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(MUTED)),
                footer_area,
            );
        }
    }

    fn render_skills(&self, frame: &mut Frame<'_>, area: Rect, skills: &[Skill]) {
        if area.is_empty() {
            return;
        }
        if skills.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("No skills installed.").yellow(),
                    Line::from(
                        "Add SKILL.md under .bcodex/skills or ${BCODEX_HOME:-$HOME/.bcodex}/skills.",
                    )
                    .dim(),
                ]),
                area,
            );
            return;
        }

        let visible = usize::from(area.height / 2).clamp(1, MAX_VISIBLE_SKILLS);
        let selected = self.selected.min(skills.len() - 1);
        let start = selected
            .saturating_add(1)
            .saturating_sub(visible)
            .min(skills.len().saturating_sub(visible));
        let mut lines = Vec::with_capacity(visible.saturating_mul(2));
        for (index, skill) in skills.iter().enumerate().skip(start).take(visible) {
            let is_selected = index == selected;
            let marker = if is_selected { "› " } else { "  " };
            let enabled = if skill.is_enabled() { "x" } else { " " };
            let implicit = if skill.allows_implicit_invocation() {
                "x"
            } else {
                " "
            };
            let mut heading = Line::from(vec![
                Span::from(marker),
                Span::from(format!("[{enabled}] enabled  ")),
                Span::from(format!("[{implicit}] implicit  ")),
                Span::from(skill.display_name().to_string()),
            ]);
            let mut details = Line::from(vec![
                Span::from("    "),
                Span::from(skill.display_description().to_string()),
                Span::from(" · ").dim(),
                Span::from(skill.path().display().to_string()).dim(),
            ]);
            if is_selected {
                let selected_style = Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD);
                for span in heading.spans.iter_mut().chain(&mut details.spans) {
                    span.style = selected_style;
                }
            } else if !skill.is_enabled() {
                heading = heading.dim();
                details = details.dim();
            }
            lines.push(heading);
            lines.push(details);
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
}

#[cfg(test)]
#[path = "skills_view_tests.rs"]
mod tests;
