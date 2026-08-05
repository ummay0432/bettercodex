use super::markdown;
use crate::rollout::SessionSummary;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use std::cmp::Ordering;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

const MUTED: Color = Color::Indexed(245);
const RULE: Color = Color::Indexed(8);
const SELECTED: Color = Color::Yellow;
const MAX_QUERY_CHARS: usize = 256;
const HORIZONTAL_CHROME_INSET: u16 = 1;
const LIST_HORIZONTAL_INSET: u16 = 2;
const FOOTER_HEIGHT: u16 = 3;
const SESSION_ROW_HEIGHT: u16 = 3;
const METADATA_DATE_WIDTH: usize = 12;

pub(super) struct ResumePicker {
    cwd: PathBuf,
    current_session: Uuid,
    sessions: Option<Vec<SessionSummary>>,
    filtered: Vec<usize>,
    query: String,
    filter: SessionFilter,
    sort: SessionSort,
    toolbar_focus: ToolbarControl,
    selected: usize,
    status: Option<PickerStatus>,
}

enum PickerStatus {
    Resuming(Uuid),
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionFilter {
    Cwd,
    All,
}

impl SessionFilter {
    fn toggle(self) -> Self {
        match self {
            Self::Cwd => Self::All,
            Self::All => Self::Cwd,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionSort {
    Updated,
    Created,
}

impl SessionSort {
    fn toggle(self) -> Self {
        match self {
            Self::Updated => Self::Created,
            Self::Created => Self::Updated,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarControl {
    Filter,
    Sort,
}

impl ToolbarControl {
    fn next(self) -> Self {
        match self {
            Self::Filter => Self::Sort,
            Self::Sort => Self::Filter,
        }
    }
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
            filter: SessionFilter::Cwd,
            sort: SessionSort::Updated,
            toolbar_focus: ToolbarControl::Filter,
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
            KeyCode::Tab | KeyCode::BackTab => {
                self.toolbar_focus = self.toolbar_focus.next();
                self.clear_error();
            }
            KeyCode::Left | KeyCode::Right => {
                match self.toolbar_focus {
                    ToolbarControl::Filter => self.filter = self.filter.toggle(),
                    ToolbarControl::Sort => self.sort = self.sort.toggle(),
                }
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
                self.selected = self.selected.saturating_sub(10);
                self.clear_error();
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + 10).min(self.filtered.len().saturating_sub(1));
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
                remaining = remaining.saturating_sub(1);
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

        let header = inset_row(area, area.y, HORIZONTAL_CHROME_INSET);
        frame.render_widget(
            Paragraph::new(Line::from("Resume a previous session").cyan().bold()),
            header,
        );

        let search_y = area.y.saturating_add(2);
        if search_y < area.bottom() {
            let search = inset_row(area, search_y, HORIZONTAL_CHROME_INSET);
            frame.render_widget(Paragraph::new(self.search_line(search.width)), search);
        }

        let list_y = area.y.saturating_add(4).min(area.bottom());
        let available_after_chrome = area.bottom().saturating_sub(list_y);
        let footer_height = FOOTER_HEIGHT.min(available_after_chrome);
        let footer_y = area.bottom().saturating_sub(footer_height);
        let list = Rect::new(
            area.x.saturating_add(LIST_HORIZONTAL_INSET),
            list_y,
            area.width
                .saturating_sub(LIST_HORIZONTAL_INSET.saturating_mul(2)),
            footer_y.saturating_sub(list_y),
        );
        self.render_sessions(frame, list);

        if footer_height > 0 {
            self.render_footer(
                frame,
                Rect::new(area.x, footer_y, area.width, footer_height),
                list.height,
            );
        }
    }

    fn search_line(&self, width: u16) -> Line<'static> {
        if let Some(status) = &self.status {
            return match status {
                PickerStatus::Resuming(id) => Line::from(vec![
                    Span::from("Resuming ").bold(),
                    Span::from(id.to_string()).dim(),
                    Span::from("…").dim(),
                ]),
                PickerStatus::Error(error) => Line::from(markdown::sanitize(error)).red(),
            };
        }

        let search = if self.query.is_empty() {
            Span::from("Type to search").dim()
        } else {
            Span::from(format!("Search: {}", markdown::sanitize(&self.query)))
        };
        let mut toolbar = self.toolbar_line(false);
        let search_width = search.width();
        if search_width
            .saturating_add(toolbar.width())
            .saturating_add(2)
            > usize::from(width)
        {
            toolbar = self.toolbar_line(true);
        }
        let toolbar_width = toolbar.width();
        if toolbar_width.saturating_add(2) >= usize::from(width) {
            return Line::from(truncate_text(search.content.as_ref(), usize::from(width)));
        }
        let available_search = usize::from(width).saturating_sub(toolbar_width + 2);
        let search = if search_width > available_search {
            let text = truncate_text(search.content.as_ref(), available_search);
            if self.query.is_empty() {
                Span::from(text).dim()
            } else {
                Span::from(text)
            }
        } else {
            search
        };
        let gap = usize::from(width).saturating_sub(search.width() + toolbar_width);
        let mut spans = vec![search, Span::from(" ".repeat(gap))];
        spans.extend(toolbar.spans);
        Line::from(spans)
    }

    fn toolbar_line(&self, compact: bool) -> Line<'static> {
        let mut spans = Vec::new();
        if compact {
            spans.push(Span::from("Filter:").dim());
            spans.push(toolbar_value(
                match self.filter {
                    SessionFilter::Cwd => "Cwd",
                    SessionFilter::All => "All",
                },
                true,
                self.toolbar_focus == ToolbarControl::Filter,
            ));
            spans.push(Span::from("   "));
            spans.push(Span::from("Sort:").dim());
            spans.push(toolbar_value(
                match self.sort {
                    SessionSort::Updated => "Updated",
                    SessionSort::Created => "Created",
                },
                true,
                self.toolbar_focus == ToolbarControl::Sort,
            ));
        } else {
            spans.push(Span::from("Filter: ").dim());
            spans.push(toolbar_value(
                "Cwd",
                self.filter == SessionFilter::Cwd,
                self.toolbar_focus == ToolbarControl::Filter,
            ));
            spans.push(toolbar_value(
                "All",
                self.filter == SessionFilter::All,
                self.toolbar_focus == ToolbarControl::Filter,
            ));
            spans.push(Span::from("   "));
            spans.push(Span::from("Sort: ").dim());
            spans.push(toolbar_value(
                "Updated",
                self.sort == SessionSort::Updated,
                self.toolbar_focus == ToolbarControl::Sort,
            ));
            spans.push(toolbar_value(
                "Created",
                self.sort == SessionSort::Created,
                self.toolbar_focus == ToolbarControl::Sort,
            ));
        }
        Line::from(spans)
    }

    fn render_sessions(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() || matches!(self.status, Some(PickerStatus::Resuming(_))) {
            return;
        }
        let Some(sessions) = &self.sessions else {
            frame.render_widget(
                Paragraph::new(Line::from("Loading sessions…").italic().dim()),
                area,
            );
            return;
        };
        if self.filtered.is_empty() {
            let message = if self.query.is_empty() {
                match self.filter {
                    SessionFilter::Cwd => "No sessions for this directory",
                    SessionFilter::All => "No sessions yet",
                }
            } else {
                "No results for your search"
            };
            frame.render_widget(Paragraph::new(Line::from(message).italic().dim()), area);
            return;
        }

        let visible = visible_session_count(area.height);
        let start = self
            .selected
            .saturating_add(1)
            .saturating_sub(visible)
            .min(self.filtered.len().saturating_sub(visible));
        let now = unix_timestamp_millis();
        let mut y = area.y;
        for (position, index) in self.filtered.iter().enumerate().skip(start).take(visible) {
            if y >= area.bottom() {
                break;
            }
            let session = &sessions[*index];
            let selected = position == self.selected;
            frame.render_widget(
                Paragraph::new(session_title_line(
                    session,
                    selected,
                    session.id == self.current_session,
                    area.width,
                )),
                Rect::new(area.x, y, area.width, 1),
            );
            y = y.saturating_add(1);
            if y >= area.bottom() {
                break;
            }
            frame.render_widget(
                Paragraph::new(session_metadata_line(
                    session,
                    now,
                    self.filter == SessionFilter::All,
                    area.width,
                )),
                Rect::new(area.x, y, area.width, 1),
            );
            y = y.saturating_add(2);
        }
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect, list_height: u16) {
        if area.is_empty() {
            return;
        }
        let visible = visible_session_count(list_height);
        frame.render_widget(
            Paragraph::new(self.footer_separator(area.width, visible)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        if area.height == 1 {
            return;
        }
        if matches!(self.status, Some(PickerStatus::Resuming(_))) {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::from(" resuming").bold(),
                    Span::from(" selected session…").dim(),
                ])),
                Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
            );
            return;
        }

        let escape_label = if self.query.is_empty() {
            "exit"
        } else {
            "clear"
        };
        let first = [
            ("enter", "resume"),
            ("esc", escape_label),
            ("ctrl+c", "exit"),
            ("tab", "focus sort/filter"),
            ("←/→", "change option"),
        ];
        frame.render_widget(
            Paragraph::new(footer_hints(&first, area.width)),
            Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        );
        if area.height > 2 {
            let second = [("↑/↓", "browse"), ("home/end", "jump"), ("type", "search")];
            frame.render_widget(
                Paragraph::new(footer_hints(&second, area.width)),
                Rect::new(area.x, area.y.saturating_add(2), area.width, 1),
            );
        }
    }

    fn footer_separator(&self, width: u16, visible: usize) -> Line<'static> {
        let total = self.filtered.len();
        let position = if total == 0 { 0 } else { self.selected + 1 };
        let start = self
            .selected
            .saturating_add(1)
            .saturating_sub(visible)
            .min(total.saturating_sub(visible));
        let percent = if total <= visible {
            100
        } else {
            let maximum = total.saturating_sub(visible);
            start.saturating_mul(100) / maximum.max(1)
        };
        let label = format!(" {position} / {total} · {percent}% ");
        let label_width = UnicodeWidthStr::width(label.as_str());
        if label_width.saturating_add(1) >= usize::from(width) {
            return Line::from(truncate_text(&label, usize::from(width))).dim();
        }
        let rule_width = usize::from(width).saturating_sub(label_width + 1);
        Line::from(format!("{}{}─", "─".repeat(rule_width), label)).fg(RULE)
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
            .filter(|(_, session)| self.filter == SessionFilter::All || session.cwd == self.cwd)
            .filter(|(_, session)| session_matches(session, &query))
            .map(|(index, _)| index)
            .collect();
        if let Some(sessions) = &self.sessions {
            self.filtered.sort_unstable_by(|left, right| {
                compare_sessions(&sessions[*left], &sessions[*right], self.sort)
            });
        }
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

fn toolbar_value(label: &'static str, active: bool, focused: bool) -> Span<'static> {
    if active {
        let style = if focused {
            Style::default().fg(Color::Magenta)
        } else {
            Style::default()
        };
        Span::styled(format!("[{label}]"), style)
    } else {
        Span::from(format!(" {label} ")).dim()
    }
}

fn inset_row(area: Rect, y: u16, inset: u16) -> Rect {
    Rect::new(
        area.x.saturating_add(inset),
        y,
        area.width.saturating_sub(inset.saturating_mul(2)),
        1,
    )
}

fn visible_session_count(height: u16) -> usize {
    usize::from(height.saturating_add(SESSION_ROW_HEIGHT - 1) / SESSION_ROW_HEIGHT).max(1)
}

fn session_title_line(
    session: &SessionSummary,
    selected: bool,
    current: bool,
    width: u16,
) -> Line<'static> {
    let marker = if selected { "❯ " } else { "  " };
    let current = if current { "  current" } else { "" };
    let title = session
        .preview
        .as_deref()
        .map(markdown::sanitize)
        .unwrap_or_else(|| "New session".to_string());
    let title_width = usize::from(width)
        .saturating_sub(UnicodeWidthStr::width(marker))
        .saturating_sub(UnicodeWidthStr::width(current));
    let title = truncate_text(&title, title_width);
    let selected_style = Style::default().fg(SELECTED).bold();
    let mut spans = vec![
        Span::styled(
            marker.to_string(),
            if selected {
                selected_style
            } else {
                Style::default()
            },
        ),
        Span::styled(
            title,
            if selected {
                selected_style
            } else {
                Style::default()
            },
        ),
    ];
    if !current.is_empty() {
        spans.push(Span::from(current.to_string()).yellow().dim());
    }
    Line::from(spans)
}

fn session_metadata_line(
    session: &SessionSummary,
    now_unix_ms: u64,
    show_cwd: bool,
    width: u16,
) -> Line<'static> {
    let age = format_relative_time(now_unix_ms, session.updated_at_unix_ms);
    let prefix = format!("  {age:<METADATA_DATE_WIDTH$}");
    let suffix = format!("  {}", &session.id.to_string()[..8]);
    let metadata = if show_cwd {
        let cwd_prefix = "  ⌁ ";
        let cwd_width = usize::from(width)
            .saturating_sub(UnicodeWidthStr::width(prefix.as_str()))
            .saturating_sub(UnicodeWidthStr::width(cwd_prefix))
            .saturating_sub(UnicodeWidthStr::width(suffix.as_str()));
        format!(
            "{prefix}{cwd_prefix}{}{suffix}",
            truncate_text(&display_cwd(&session.cwd), cwd_width)
        )
    } else {
        format!("{prefix}{suffix}")
    };
    Line::from(truncate_text(&metadata, usize::from(width)))
        .fg(MUTED)
        .dim()
}

fn footer_hints(hints: &[(&str, &str)], width: u16) -> Line<'static> {
    let mut spans = vec![Span::from(" ")];
    let mut used = 1_usize;
    for (key, label) in hints {
        let gap = if used == 1 { 0 } else { 3 };
        let hint_width = UnicodeWidthStr::width(*key) + 1 + UnicodeWidthStr::width(*label);
        if used.saturating_add(gap).saturating_add(hint_width) > usize::from(width) {
            break;
        }
        if gap > 0 {
            spans.push(Span::from(" ".repeat(gap)).dim());
            used += gap;
        }
        spans.push(Span::from((*key).to_string()));
        spans.push(Span::from(" "));
        spans.push(Span::from((*label).to_string()).dim());
        used += hint_width;
    }
    Line::from(spans)
}

fn compare_sessions(left: &SessionSummary, right: &SessionSummary, sort: SessionSort) -> Ordering {
    let order = match sort {
        SessionSort::Updated => right.updated_at_unix_ms.cmp(&left.updated_at_unix_ms),
        SessionSort::Created => right.created_at_unix_ms.cmp(&left.created_at_unix_ms),
    };
    order.then_with(|| right.id.cmp(&left.id))
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
        0 => "now".to_string(),
        1..=59 => format!("{seconds}s ago"),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn display_cwd(path: &Path) -> String {
    let display = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf))
        .map_or_else(
            || path.display().to_string(),
            |relative| {
                if relative.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", relative.display())
                }
            },
        );
    markdown::sanitize(&display)
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
