// Ported from OpenAI Codex 85e0661c3b, chiefly
// codex-rs/tui/src/resume_picker.rs. bettercodex keeps its local rollout listing
// and resume lifecycle while retaining Codex's picker layout and row rendering.

use super::markdown;
use super::palette;
use super::width::display_width;
use super::width::line_width;
use super::width::prefix_fitting_width;
use crate::rollout::SessionSummary;
use crate::time::unix_timestamp_millis;
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
use uuid::Uuid;

const RULE: Color = Color::Indexed(8);
const MAX_QUERY_CHARS: usize = 256;
const HORIZONTAL_CHROME_INSET: u16 = 1;
const LIST_HORIZONTAL_INSET: u16 = 2;
const FOOTER_HEIGHT: u16 = 3;
const METADATA_DATE_WIDTH: usize = 12;

pub(super) struct ResumePicker {
    cwd: PathBuf,
    sessions: Option<Vec<SessionSummary>>,
    filtered: Vec<usize>,
    query: String,
    filter: SessionFilter,
    sort: SessionSort,
    density: SessionListDensity,
    toolbar_focus: ToolbarControl,
    selected: usize,
    status: Option<PickerStatus>,
    relative_time_reference_unix_ms: u64,
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
enum SessionListDensity {
    Dense,
    Comfortable,
}

impl SessionListDensity {
    fn toggle(self) -> Self {
        match self {
            Self::Dense => Self::Comfortable,
            Self::Comfortable => Self::Dense,
        }
    }

    fn row_height(self) -> u16 {
        match self {
            Self::Dense => 1,
            Self::Comfortable => 3,
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
    CancelResume,
    Resume(Uuid),
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')
        || key.modifiers.is_empty() && key.code == KeyCode::Char('\u{3}')
}

impl ResumePicker {
    pub(super) fn loading(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            sessions: None,
            filtered: Vec::new(),
            query: String::new(),
            filter: SessionFilter::Cwd,
            sort: SessionSort::Updated,
            density: SessionListDensity::Dense,
            toolbar_focus: ToolbarControl::Filter,
            selected: 0,
            status: None,
            relative_time_reference_unix_ms: unix_timestamp_millis(),
        }
    }

    pub(super) fn resuming(cwd: &Path, target: Uuid) -> Self {
        let mut picker = Self::loading(cwd);
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
            return if key.code == KeyCode::Esc || is_ctrl_c(&key) {
                ResumePickerAction::CancelResume
            } else {
                ResumePickerAction::None
            };
        }
        if is_ctrl_c(&key) {
            return ResumePickerAction::Close;
        }
        match key.code {
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.density = self.density.toggle();
                self.clear_error();
            }
            KeyCode::Char('\u{000f}') if key.modifiers.is_empty() => {
                self.density = self.density.toggle();
                self.clear_error();
            }
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
                if !character.is_control()
                    && !key
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
        let title = Span::styled("Resume a previous session", palette::accent_style());
        frame.render_widget(Paragraph::new(Line::from(title)), header);

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
        let search_width = display_width(search.content.as_ref());
        if search_width
            .saturating_add(line_width(&toolbar))
            .saturating_add(2)
            > usize::from(width)
        {
            toolbar = self.toolbar_line(true);
        }
        let toolbar_width = line_width(&toolbar);
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
        let gap = usize::from(width)
            .saturating_sub(display_width(search.content.as_ref()) + toolbar_width);
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

        let visible = visible_session_count(area.height, self.density);
        let start = self
            .selected
            .saturating_add(1)
            .saturating_sub(visible)
            .min(self.filtered.len().saturating_sub(visible));
        let mut y = area.y;
        for (position, index) in self.filtered.iter().enumerate().skip(start).take(visible) {
            if y >= area.bottom() {
                break;
            }
            let session = &sessions[*index];
            let selected = position == self.selected;
            let zebra = position.is_multiple_of(2);
            let lines = match self.density {
                SessionListDensity::Dense => vec![dense_session_line(
                    session,
                    self.relative_time_reference_unix_ms,
                    self.sort,
                    selected,
                    zebra,
                    area.width,
                )],
                SessionListDensity::Comfortable => comfortable_session_lines(
                    session,
                    self.relative_time_reference_unix_ms,
                    self.sort,
                    self.filter == SessionFilter::All,
                    selected,
                    zebra,
                    area.width,
                ),
            };
            for line in lines {
                if y >= area.bottom() {
                    break;
                }
                frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
                y = y.saturating_add(1);
            }
            if self.density == SessionListDensity::Comfortable {
                y = y.saturating_add(1);
            }
        }
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect, list_height: u16) {
        if area.is_empty() {
            return;
        }
        let visible = visible_session_count(list_height, self.density);
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
            if area.height > 2 {
                frame.render_widget(
                    Paragraph::new(footer_hints(
                        &[("esc", "cancel"), ("ctrl+c", "cancel")],
                        area.width,
                    )),
                    Rect::new(area.x, area.y.saturating_add(2), area.width, 1),
                );
            }
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
            let density_label = match self.density {
                SessionListDensity::Dense => "comfortable view",
                SessionListDensity::Comfortable => "dense view",
            };
            let second = [
                ("ctrl+o", density_label),
                ("↑/↓", "browse"),
                ("home/end", "jump"),
                ("type", "search"),
            ];
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
        let label_width = display_width(&label);
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

fn visible_session_count(height: u16, density: SessionListDensity) -> usize {
    let row_height = density.row_height();
    usize::from(height.saturating_add(row_height - 1) / row_height).max(1)
}

fn dense_session_line(
    session: &SessionSummary,
    now_unix_ms: u64,
    sort: SessionSort,
    selected: bool,
    zebra: bool,
    width: u16,
) -> Line<'static> {
    let date = format_relative_time(now_unix_ms, session_timestamp(session, sort));
    dense_summary_line(DenseSummaryInput {
        marker: selection_marker(selected),
        date: &date,
        title: &session_title(session),
        selected,
        zebra,
        width,
    })
}

struct DenseSummaryInput<'a> {
    marker: Span<'static>,
    date: &'a str,
    title: &'a str,
    selected: bool,
    zebra: bool,
    width: u16,
}

fn dense_summary_line(input: DenseSummaryInput<'_>) -> Line<'static> {
    let marker_width = display_width(input.marker.content.as_ref());
    let available = usize::from(input.width).saturating_sub(marker_width);
    let title_width = available.saturating_sub(METADATA_DATE_WIDTH);
    let title = dense_column_text(input.title, title_width);
    let title = if input.selected {
        Span::styled(title, selected_session_style())
    } else {
        Span::from(title)
    };
    let mut line = Line::from(vec![
        input.marker,
        Span::from(dense_column_text(input.date, METADATA_DATE_WIDTH)).dim(),
        title,
    ]);
    let style = if input.selected {
        Some(selected_row_style())
    } else if input.zebra {
        Some(zebra_row_style())
    } else {
        None
    };
    if let Some(style) = style {
        line = apply_line_background(line, style, input.width);
    }
    line
}

fn dense_column_text(text: &str, width: usize) -> String {
    let text = truncate_text(text, width.saturating_sub(1));
    let padding = width.saturating_sub(display_width(&text));
    format!("{text}{}", " ".repeat(padding))
}

fn comfortable_session_lines(
    session: &SessionSummary,
    now_unix_ms: u64,
    sort: SessionSort,
    show_cwd: bool,
    selected: bool,
    zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        session_title_line(session, selected, width),
        session_metadata_line(session, now_unix_ms, sort, show_cwd, width),
    ];
    let style = if selected {
        Some(selected_row_style())
    } else if zebra {
        Some(zebra_row_style())
    } else {
        None
    };
    if let Some(style) = style {
        lines = lines
            .into_iter()
            .map(|line| apply_line_background(line, style, width))
            .collect();
    }
    lines
}

fn session_title_line(session: &SessionSummary, selected: bool, width: u16) -> Line<'static> {
    let marker = selection_marker(selected);
    let title = session_title(session);
    let title_width = usize::from(width).saturating_sub(display_width(marker.content.as_ref()));
    let title = truncate_text(&title, title_width);
    let title = if selected {
        Span::styled(title, selected_session_style())
    } else {
        Span::from(title)
    };
    Line::from(vec![marker, title])
}

fn session_metadata_line(
    session: &SessionSummary,
    now_unix_ms: u64,
    sort: SessionSort,
    show_cwd: bool,
    width: u16,
) -> Line<'static> {
    let age = format_relative_time(now_unix_ms, session_timestamp(session, sort));
    let prefix = format!("  {age:<METADATA_DATE_WIDTH$}");
    let metadata = if show_cwd {
        let cwd_prefix = "  ⌁ ";
        let cwd_width = usize::from(width)
            .saturating_sub(display_width(&prefix))
            .saturating_sub(display_width(cwd_prefix));
        format!(
            "{prefix}{cwd_prefix}{}",
            truncate_text(&display_cwd(&session.cwd), cwd_width)
        )
    } else {
        prefix
    };
    Line::from(truncate_text(&metadata, usize::from(width))).dim()
}

fn session_title(session: &SessionSummary) -> String {
    markdown::sanitize(&session.preview)
}

fn session_timestamp(session: &SessionSummary, sort: SessionSort) -> u64 {
    match sort {
        SessionSort::Updated => session.updated_at_unix_ms,
        SessionSort::Created => session.created_at_unix_ms,
    }
}

fn selection_marker(selected: bool) -> Span<'static> {
    if selected {
        Span::styled("❯ ", selected_session_style().bold())
    } else {
        Span::from("  ")
    }
}

fn selected_session_style() -> Style {
    if palette::default_background().is_some_and(palette::is_light) {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default().fg(Color::Yellow)
    }
}

fn selected_row_style() -> Style {
    selected_session_style().patch(row_background_style(true))
}

fn zebra_row_style() -> Style {
    row_background_style(false)
}

fn row_background_style(selected: bool) -> Style {
    palette::default_background().map_or_else(Style::default, |background| {
        Style::default().bg(row_background_color(background, selected))
    })
}

fn row_background_color(background: (u8, u8, u8), selected: bool) -> Color {
    let (overlay, alpha) = if palette::is_light(background) {
        ((0, 0, 0), if selected { 0.12 } else { 0.04 })
    } else {
        ((255, 255, 255), if selected { 0.12 } else { 0.055 })
    };
    let (red, green, blue) = palette::blend(overlay, background, alpha);
    Color::Rgb(red, green, blue)
}

fn apply_line_background(mut line: Line<'static>, style: Style, width: u16) -> Line<'static> {
    let padding = usize::from(width).saturating_sub(line_width(&line));
    if padding > 0 {
        line.spans.push(Span::styled(" ".repeat(padding), style));
    }
    line.style = line.style.patch(style);
    for span in &mut line.spans {
        span.style = span.style.patch(style);
    }
    line
}

fn footer_hints(hints: &[(&str, &str)], width: u16) -> Line<'static> {
    let mut spans = vec![Span::from(" ")];
    let mut used = 1_usize;
    for (key, label) in hints {
        let gap = if used == 1 { 0 } else { 3 };
        let hint_width = display_width(key) + 1 + display_width(label);
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
        || session.preview.to_lowercase().contains(query)
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
    let display = crate::paths::home_dir()
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
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    format!(
        "{}…",
        prefix_fitting_width(text, max_width.saturating_sub(1))
    )
}
