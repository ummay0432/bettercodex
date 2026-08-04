use super::markdown;
use crate::rollout::SessionSummary;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
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
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

const MUTED: Color = Color::Indexed(245);
const RULE: Color = Color::Indexed(8);
const PREFERRED_WIDTH: u16 = 100;
const MAX_VISIBLE_SESSIONS: usize = 6;
const MAX_QUERY_CHARS: usize = 256;
const PANEL_CHROME_HEIGHT: u16 = 5;

pub(super) struct ResumePicker {
    cwd: PathBuf,
    current_session: Uuid,
    sessions: Option<Vec<SessionSummary>>,
    filtered: Vec<usize>,
    query: String,
    show_all: bool,
    selected: usize,
    status: Option<PickerStatus>,
}

enum PickerStatus {
    Resuming(Uuid),
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResumePickerAction {
    None,
    Close,
    Resume(Uuid),
}

impl ResumePicker {
    pub(super) fn loading(cwd: &Path, current_session: Uuid) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            current_session,
            sessions: None,
            filtered: Vec::new(),
            query: String::new(),
            show_all: false,
            selected: 0,
            status: None,
        }
    }

    pub(super) fn resuming(cwd: &Path, current_session: Uuid, target: Uuid) -> Self {
        let mut picker = Self::loading(cwd, current_session);
        picker.sessions = Some(Vec::new());
        picker.status = Some(PickerStatus::Resuming(target));
        picker
    }

    pub(super) fn set_sessions(&mut self, sessions: Vec<SessionSummary>) {
        if matches!(self.status, Some(PickerStatus::Resuming(_))) {
            return;
        }
        self.sessions = Some(sessions);
        self.status = None;
        self.rebuild_filter();
    }

    pub(super) fn set_listing_error(&mut self, error: impl Into<String>) {
        if !matches!(self.status, Some(PickerStatus::Resuming(_))) {
            self.status = Some(PickerStatus::Error(error.into()));
        }
    }

    pub(super) fn set_error(&mut self, error: impl Into<String>) {
        self.status = Some(PickerStatus::Error(error.into()));
    }

    pub(super) fn begin_resume(&mut self, target: Uuid) {
        self.status = Some(PickerStatus::Resuming(target));
    }

    pub(super) fn preferred_height(&self) -> u16 {
        let rows = self
            .sessions
            .as_ref()
            .map_or(1, |_| self.filtered.len().clamp(1, MAX_VISIBLE_SESSIONS))
            .saturating_mul(2);
        PANEL_CHROME_HEIGHT.saturating_add(u16::try_from(rows).unwrap_or(u16::MAX))
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ResumePickerAction {
        if matches!(self.status, Some(PickerStatus::Resuming(_))) {
            return ResumePickerAction::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return ResumePickerAction::Close;
        }
        match key.code {
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.clear_error();
                self.rebuild_filter();
            }
            KeyCode::Esc => return ResumePickerAction::Close,
            KeyCode::Tab if self.sessions.is_some() => {
                self.show_all = !self.show_all;
                self.clear_error();
                self.rebuild_filter();
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.clear_error();
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.filtered.len().saturating_sub(1));
                self.clear_error();
            }
            KeyCode::Home => {
                self.selected = 0;
                self.clear_error();
            }
            KeyCode::End => {
                self.selected = self.filtered.len().saturating_sub(1);
                self.clear_error();
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(MAX_VISIBLE_SESSIONS);
                self.clear_error();
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + MAX_VISIBLE_SESSIONS)
                    .min(self.filtered.len().saturating_sub(1));
                self.clear_error();
            }
            KeyCode::Enter => {
                if let Some(id) = self.selected_session_id() {
                    self.begin_resume(id);
                    return ResumePickerAction::Resume(id);
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.clear_error();
                self.rebuild_filter();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.clear();
                self.clear_error();
                self.rebuild_filter();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if self.query.chars().count() < MAX_QUERY_CHARS {
                    self.query.push(character);
                }
                self.clear_error();
                self.rebuild_filter();
            }
            _ => {}
        }
        ResumePickerAction::None
    }

    pub(super) fn handle_paste(&mut self, text: &str) {
        if matches!(self.status, Some(PickerStatus::Resuming(_))) {
            return;
        }
        let mut remaining = MAX_QUERY_CHARS.saturating_sub(self.query.chars().count());
        for word in text.split_whitespace() {
            if remaining == 0 {
                break;
            }
            if !self.query.is_empty() && !self.query.ends_with(char::is_whitespace) {
                self.query.push(' ');
                remaining -= 1;
            }
            for character in word.chars().take(remaining) {
                self.query.push(character);
                remaining -= 1;
            }
        }
        self.clear_error();
        self.rebuild_filter();
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
            self.preferred_height().min(area.height),
        );
        let block = Block::default()
            .title(" Resume a previous session ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(RULE));
        let inner = block.inner(panel);
        frame.render_widget(block, panel);
        if inner.is_empty() {
            return;
        }

        let search_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let footer_height = u16::from(inner.height > 1);
        let list_y = search_area.bottom().saturating_add(1).min(inner.bottom());
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

        frame.render_widget(
            Paragraph::new(self.search_line(search_area.width)),
            search_area,
        );
        self.render_sessions(frame, list_area);
        if !footer_area.is_empty() {
            let footer = if matches!(self.status, Some(PickerStatus::Resuming(_))) {
                "please wait"
            } else {
                "enter resume · esc close · tab cwd/all · ↑/↓ browse"
            };
            frame.render_widget(
                Paragraph::new(footer)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(MUTED)),
                footer_area,
            );
        }
    }

    fn search_line(&self, width: u16) -> Line<'static> {
        if let Some(status) = &self.status {
            return match status {
                PickerStatus::Resuming(id) => Line::from(vec![
                    Span::from("Resuming ").cyan().bold(),
                    Span::from(id.to_string()).dim(),
                ]),
                PickerStatus::Error(error) => Line::from(markdown::sanitize(error)).red(),
            };
        }
        if self.sessions.is_none() {
            return Line::from("Loading sessions…").dim();
        }
        let search = if self.query.is_empty() {
            "Type to search".to_string()
        } else {
            format!("Search: {}", markdown::sanitize(&self.query))
        };
        let filter = if self.show_all {
            "Filter: Cwd [All]"
        } else {
            "Filter: [Cwd] All"
        };
        let search_width = UnicodeWidthStr::width(search.as_str());
        let filter_width = UnicodeWidthStr::width(filter);
        let gap = usize::from(width).saturating_sub(search_width + filter_width);
        if gap >= 2 {
            Line::from(vec![
                Span::from(search),
                Span::from(" ".repeat(gap)),
                Span::from(filter).dim(),
            ])
        } else {
            Line::from(search)
        }
    }

    fn render_sessions(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            return;
        }
        if matches!(self.status, Some(PickerStatus::Resuming(_))) {
            return;
        }
        let Some(sessions) = &self.sessions else {
            return;
        };
        if self.filtered.is_empty() {
            let message = if self.show_all || !self.query.is_empty() {
                "No matching sessions"
            } else {
                "No sessions for this directory · Tab shows all"
            };
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(MUTED)),
                area,
            );
            return;
        }

        let visible = usize::from(area.height / 2).max(1);
        let start = self
            .selected
            .saturating_add(1)
            .saturating_sub(visible)
            .min(self.filtered.len().saturating_sub(visible));
        let now = unix_timestamp_millis();
        let mut lines = Vec::with_capacity(visible.saturating_mul(2));
        for (position, index) in self.filtered.iter().enumerate().skip(start).take(visible) {
            let session = &sessions[*index];
            let selected = position == self.selected;
            let marker = if selected { "› " } else { "  " };
            let title = session
                .preview
                .as_deref()
                .map(markdown::sanitize)
                .unwrap_or_else(|| "New session".to_string());
            let current_suffix = if session.id == self.current_session {
                "  current"
            } else {
                ""
            };
            let title_width = usize::from(area.width)
                .saturating_sub(UnicodeWidthStr::width(marker))
                .saturating_sub(UnicodeWidthStr::width(current_suffix));
            let title = truncate_text(&title, title_width);
            let mut title_spans = vec![Span::from(marker), Span::from(title)];
            if !current_suffix.is_empty() {
                title_spans.push(Span::from(current_suffix).yellow().dim());
            }
            let mut title_line = Line::from(title_spans);
            if selected {
                title_line = title_line.cyan().bold();
            }
            lines.push(title_line);

            let id = session.id.to_string();
            let age = format_relative_time(now, session.updated_at_unix_ms);
            let cwd = markdown::sanitize(&session.cwd.display().to_string());
            let fixed_width = UnicodeWidthStr::width(age.as_str())
                .saturating_add(UnicodeWidthStr::width(&id[..8]))
                .saturating_add(12);
            let cwd = truncate_text(&cwd, usize::from(area.width).saturating_sub(fixed_width));
            lines.push(Line::from(format!("  {}  ·  {}  ·  {}", age, cwd, &id[..8],)).dim());
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn rebuild_filter(&mut self) {
        let selected_id = self.selected_session_id();
        let query = self.query.to_lowercase();
        self.filtered = self
            .sessions
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter(|(_, session)| self.show_all || session.cwd == self.cwd)
            .filter(|(_, session)| session_matches(session, &query))
            .map(|(index, _)| index)
            .collect();
        self.selected = selected_id
            .and_then(|id| {
                self.filtered.iter().position(|index| {
                    self.sessions
                        .as_ref()
                        .is_some_and(|sessions| sessions[*index].id == id)
                })
            })
            .unwrap_or(0)
            .min(self.filtered.len().saturating_sub(1));
    }

    fn selected_session_id(&self) -> Option<Uuid> {
        let index = *self.filtered.get(self.selected)?;
        self.sessions.as_ref()?.get(index).map(|session| session.id)
    }

    fn clear_error(&mut self) {
        if self.sessions.is_some() && matches!(self.status, Some(PickerStatus::Error(_))) {
            self.status = None;
        }
    }
}

fn session_matches(session: &SessionSummary, query: &str) -> bool {
    query.is_empty()
        || session.id.to_string().to_lowercase().contains(query)
        || session.cwd.to_string_lossy().to_lowercase().contains(query)
        || session
            .preview
            .as_ref()
            .is_some_and(|preview| preview.to_lowercase().contains(query))
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn format_relative_time(now_unix_ms: u64, timestamp_unix_ms: u64) -> String {
    let seconds = now_unix_ms.saturating_sub(timestamp_unix_ms) / 1_000;
    match seconds {
        0..=59 => "now".to_string(),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn truncate_text(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let content_width = max_width.saturating_sub(1);
    let mut truncated = String::new();
    let mut width = 0_usize;
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width.saturating_add(character_width) > content_width {
            break;
        }
        truncated.push(character);
        width += character_width;
    }
    truncated.push('…');
    truncated
}

#[cfg(test)]
#[path = "resume_picker_tests.rs"]
mod tests;
