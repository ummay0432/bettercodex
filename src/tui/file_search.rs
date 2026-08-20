//! Codex-style `@` file completion backed by the pinned `codex-file-search` crate.
//!
//! One session walks the working tree while the token is active; query edits reuse that index and
//! stream ranked matches with the character positions needed for fuzzy highlighting.

use crate::file_search::FileMatch;
use crate::file_search::FileSearchOptions;
use crate::file_search::FileSearchSession;
use crate::file_search::FileSearchSnapshot;
use crate::file_search::MatchType;
use crate::file_search::SessionReporter;
use crate::tui::palette;
use crate::tui::width::display_width;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use tokio::sync::mpsc::UnboundedSender;
use unicode_segmentation::UnicodeSegmentation;

pub(super) const MAX_POPUP_ROWS: usize = 8;

#[derive(Debug)]
pub(super) enum FileSearchUpdate {
    Matches {
        query: String,
        matches: Vec<FileMatch>,
    },
    Failed {
        query: String,
        message: String,
    },
}

pub(super) struct FileSearchManager {
    state: Arc<Mutex<SearchState>>,
    search_root: PathBuf,
    updates: UnboundedSender<FileSearchUpdate>,
}

struct SearchState {
    latest_query: String,
    /// Last result set sent for this query. Reset on every query transition so revisiting a query
    /// still publishes a fresh result after any intermediate edits.
    reported_matches: Option<Vec<FileMatch>>,
    session: Option<FileSearchSession>,
    session_token: usize,
}

impl FileSearchManager {
    pub(super) fn new(search_root: PathBuf, updates: UnboundedSender<FileSearchUpdate>) -> Self {
        Self {
            state: Arc::new(Mutex::new(SearchState {
                latest_query: String::new(),
                reported_matches: None,
                session: None,
                session_token: 0,
            })),
            search_root,
            updates,
        }
    }

    /// Updates the active fuzzy query. An empty query releases the filesystem index.
    pub(super) fn on_query_changed(&self, query: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if query == state.latest_query {
            return;
        }
        state.latest_query.clear();
        state.latest_query.push_str(query);
        state.reported_matches = None;

        if query.is_empty() {
            state.session.take();
            return;
        }

        if state.session.is_none()
            && let Err(error) = self.start_session(&mut state)
        {
            let message = bounded_error(&error.to_string());
            let _ = self.updates.send(FileSearchUpdate::Failed {
                query: query.to_string(),
                message,
            });
            return;
        }
        if let Some(session) = state.session.as_ref() {
            session.update_query(query);
        }
    }

    fn start_session(&self, state: &mut SearchState) -> anyhow::Result<()> {
        state.session_token = state.session_token.wrapping_add(1);
        let reporter = Arc::new(SearchReporter {
            state: Arc::downgrade(&self.state),
            updates: self.updates.clone(),
            session_token: state.session_token,
        });
        let limit = NonZeroUsize::new(MAX_POPUP_ROWS)
            .ok_or_else(|| anyhow::anyhow!("the file-search result limit must be nonzero"))?;
        let session = crate::file_search::create_session(
            vec![self.search_root.clone()],
            FileSearchOptions {
                limit,
                compute_indices: true,
                ..FileSearchOptions::default()
            },
            reporter,
            None,
        )?;
        state.session = Some(session);
        Ok(())
    }
}

struct SearchReporter {
    state: Weak<Mutex<SearchState>>,
    updates: UnboundedSender<FileSearchUpdate>,
    session_token: usize,
}

impl SessionReporter for SearchReporter {
    fn on_update(&self, snapshot: &FileSearchSnapshot) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.session_token != self.session_token
            || state.latest_query.is_empty()
            || state.latest_query != snapshot.query
        {
            return;
        }
        if state.reported_matches.as_deref() == Some(snapshot.matches.as_slice()) {
            return;
        }
        state
            .reported_matches
            .get_or_insert_default()
            .clone_from(&snapshot.matches);
        drop(state);
        let _ = self.updates.send(FileSearchUpdate::Matches {
            query: snapshot.query.clone(),
            matches: snapshot.matches.clone(),
        });
    }

    fn on_complete(&self) {}
}

fn bounded_error(error: &str) -> String {
    let mut characters = error.chars();
    let shortened = characters.by_ref().take(160).collect::<String>();
    if characters.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveToken {
    range: Range<usize>,
    query: String,
}

#[derive(Debug, Default)]
pub(super) struct FileSearchPopup {
    token: Option<ActiveToken>,
    dismissed_token: Option<ActiveToken>,
    display_query: String,
    waiting: bool,
    matches: Vec<FileMatch>,
    selected: Option<usize>,
    error: Option<String>,
}

impl FileSearchPopup {
    pub(super) fn sync(&mut self, text: &str, cursor: usize) {
        let mut token = active_token(text, cursor);
        if self.dismissed_token.as_ref() == token.as_ref() {
            token = None;
        } else {
            self.dismissed_token = None;
        }
        if token == self.token {
            return;
        }

        match token.as_ref() {
            Some(token) if token.query.is_empty() => {
                self.display_query.clear();
                self.waiting = false;
                self.matches.clear();
                self.selected = None;
                self.error = None;
            }
            Some(token) => {
                self.waiting = true;
                self.error = None;
                if self
                    .token
                    .as_ref()
                    .is_none_or(|previous| previous.range.start != token.range.start)
                    && self.display_query != token.query
                {
                    self.matches.clear();
                    self.selected = None;
                    self.display_query.clear();
                }
            }
            None => {
                self.display_query.clear();
                self.waiting = false;
                self.matches.clear();
                self.selected = None;
                self.error = None;
            }
        }
        self.token = token;
    }

    pub(super) fn query(&self) -> &str {
        self.token
            .as_ref()
            .map(|token| token.query.as_str())
            .filter(|query| !query.is_empty())
            .unwrap_or_default()
    }

    pub(super) fn is_active(&self) -> bool {
        self.token.is_some()
    }

    pub(super) fn dismiss(&mut self) {
        self.dismissed_token = self.token.clone();
        self.hide();
    }

    pub(super) fn hide(&mut self) {
        self.token = None;
        self.display_query.clear();
        self.waiting = false;
        self.matches.clear();
        self.selected = None;
        self.error = None;
    }

    pub(super) fn apply_update(&mut self, update: FileSearchUpdate) {
        match update {
            FileSearchUpdate::Matches { query, matches }
                if self
                    .token
                    .as_ref()
                    .is_some_and(|token| token.query == query) =>
            {
                self.display_query = query;
                self.matches = matches.into_iter().take(MAX_POPUP_ROWS).collect();
                self.waiting = false;
                self.error = None;
                self.selected = (!self.matches.is_empty())
                    .then(|| self.selected.unwrap_or(0).min(self.matches.len() - 1));
            }
            FileSearchUpdate::Failed { query, message }
                if self
                    .token
                    .as_ref()
                    .is_some_and(|token| token.query == query) =>
            {
                self.display_query = query;
                self.waiting = false;
                self.matches.clear();
                self.selected = None;
                self.error = Some(format!("file search unavailable: {message}"));
            }
            FileSearchUpdate::Matches { .. } | FileSearchUpdate::Failed { .. } => {}
        }
    }

    pub(super) fn move_up(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = Some(match self.selected {
            Some(0) | None => self.matches.len() - 1,
            Some(selected) => selected - 1,
        });
    }

    pub(super) fn move_down(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = Some(match self.selected {
            Some(selected) if selected + 1 < self.matches.len() => selected + 1,
            Some(_) | None => 0,
        });
    }

    pub(super) fn selected_path(&self) -> Option<(Range<usize>, String, MatchType)> {
        let token = self.token.as_ref()?;
        let selected = self.selected?;
        let selected = self.matches.get(selected)?;
        let path = selected.path.to_string_lossy().into_owned();
        Some((token.range.clone(), path, selected.match_type))
    }

    pub(super) fn height(&self) -> u16 {
        if !self.is_active() {
            return 0;
        }
        let rows = self.matches.len().clamp(1, MAX_POPUP_ROWS) as u16;
        rows.saturating_add(2)
    }

    pub(super) fn lines(&self, width: u16) -> Vec<Line<'static>> {
        if !self.is_active() {
            return Vec::new();
        }
        let mut lines = if self.matches.is_empty() {
            vec![
                Line::from(format!("  {}", self.empty_message()))
                    .dim()
                    .italic(),
            ]
        } else {
            self.matches
                .iter()
                .enumerate()
                .map(|(index, file_match)| {
                    file_match_line(file_match, Some(index) == self.selected, usize::from(width))
                })
                .collect()
        };
        lines.push(Line::default());
        lines.push(Line::from("  enter insert · esc close · ↑/↓ select").dim());
        lines
    }

    fn empty_message(&self) -> &str {
        if self
            .token
            .as_ref()
            .is_some_and(|token| token.query.is_empty())
        {
            "type to search files"
        } else if let Some(error) = self.error.as_deref() {
            error
        } else if self.waiting {
            "searching files…"
        } else {
            "no matches"
        }
    }
}

fn active_token(text: &str, cursor: usize) -> Option<ActiveToken> {
    let cursor = text.floor_char_boundary(cursor);
    let before_cursor = &text[..cursor];
    let after_cursor = &text[cursor..];
    let at_whitespace = after_cursor.chars().next().is_some_and(char::is_whitespace);
    let after_horizontal_whitespace = before_cursor
        .chars()
        .next_back()
        .is_some_and(is_horizontal_whitespace);
    let cursor_starts_token = after_horizontal_whitespace && !at_whitespace;
    let next_non_separator = after_cursor
        .chars()
        .find(|character| !is_horizontal_whitespace(*character));
    let separator_precedes_token =
        next_non_separator.is_some_and(|character| !character.is_whitespace());
    let separator_precedes_completion = next_non_separator == Some('@');
    let at_separator = (at_whitespace || after_horizontal_whitespace) && separator_precedes_token;

    let left_end = if at_separator {
        before_cursor
            .trim_end_matches(is_horizontal_whitespace)
            .len()
    } else {
        cursor
            + after_cursor
                .char_indices()
                .find(|(_, character)| character.is_whitespace())
                .map_or(after_cursor.len(), |(index, _)| index)
    };
    let left_start = text[..left_end]
        .char_indices()
        .rfind(|(_, character)| character.is_whitespace())
        .map_or(0, |(index, character)| index + character.len_utf8());
    let right_start = cursor
        + after_cursor
            .chars()
            .take_while(|character| is_horizontal_whitespace(*character))
            .map(char::len_utf8)
            .sum::<usize>();
    let right_end = right_start
        + text[right_start..]
            .char_indices()
            .find(|(_, character)| character.is_whitespace())
            .map_or(text.len() - right_start, |(index, _)| index);

    let left = token_in_range(text, left_start..left_end);
    let right = token_in_range(text, right_start..right_end);
    if cursor_starts_token {
        return right;
    }
    if at_separator {
        if after_horizontal_whitespace && !separator_precedes_completion {
            return right;
        }
        return right.or(left);
    }
    if after_cursor.starts_with('@') {
        let prefix_starts_token = before_cursor
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        return if prefix_starts_token {
            right.or(left)
        } else {
            left
        };
    }
    left.or(right)
}

fn token_in_range(text: &str, range: Range<usize>) -> Option<ActiveToken> {
    let query = text.get(range.clone())?.strip_prefix('@')?;
    Some(ActiveToken {
        range,
        query: query.to_string(),
    })
}

pub(super) fn is_horizontal_whitespace(character: char) -> bool {
    character.is_whitespace()
        && !matches!(
            character,
            '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
        )
}

fn file_match_line(file_match: &FileMatch, selected: bool, width: usize) -> Line<'static> {
    let path = file_match.path.to_string_lossy();
    let display_path = sanitized_display_path(&path);
    let (parent, name, name_char_start) = split_path(&display_path);
    let indices = file_match.indices.as_deref().unwrap_or_default();
    let mut content = matched_spans(name, name_char_start, indices, palette::accent_text_style());
    content.push(Span::from("  "));
    if name_char_start == 0 {
        content.push(Span::styled(
            parent.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ));
    } else {
        content.extend(matched_spans(
            parent,
            0,
            indices,
            Style::default().add_modifier(Modifier::DIM),
        ));
    }

    let tag = match file_match.match_type {
        MatchType::File => "File",
        MatchType::Directory => "Dir",
    };
    let gutter_width = 2;
    let tag_width = display_width(tag);
    let tagged = width >= gutter_width + tag_width + 2;
    let content_limit = if tagged {
        width - gutter_width - tag_width - 2
    } else {
        width.saturating_sub(gutter_width)
    };
    let mut spans = vec![Span::from(if selected { "> " } else { "  " })];
    let content = truncate_spans(content, content_limit);
    let content_width = spans_width(&content);
    spans.extend(content);
    if tagged {
        spans.push(Span::from(
            " ".repeat(width - gutter_width - content_width - tag_width),
        ));
        spans.push(Span::from(tag).dim());
    }
    if selected {
        let selected_style = palette::accent_style();
        for span in &mut spans {
            span.style = selected_style;
        }
    }
    Line::from(spans)
}

fn sanitized_display_path(path: &str) -> Cow<'_, str> {
    if path.chars().any(char::is_control) {
        // Replace one scalar with one scalar so fuzzy-match character indices still align with the
        // visible label. The original `FileMatch.path` remains untouched for composer insertion.
        Cow::Owned(
            path.chars()
                .map(|character| {
                    if character.is_control() {
                        '�'
                    } else {
                        character
                    }
                })
                .collect(),
        )
    } else {
        Cow::Borrowed(path)
    }
}

fn split_path(path: &str) -> (&str, &str, usize) {
    let Some(separator) = path.rfind('/') else {
        return ("./", path, 0);
    };
    let name_start = separator + 1;
    let name_char_start = path[..name_start].chars().count();
    (&path[..name_start], &path[name_start..], name_char_start)
}

fn matched_spans(
    text: &str,
    char_offset: usize,
    indices: &[u32],
    base: Style,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut character_index = 0usize;
    for grapheme in text.graphemes(/*is_extended*/ true) {
        let character_count = grapheme.chars().count();
        let matched =
            (character_index..character_index.saturating_add(character_count)).any(|index| {
                indices
                    .binary_search(&((char_offset + index) as u32))
                    .is_ok()
            });
        let style = if matched {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        if let Some(previous) = spans.last_mut()
            && previous.style == style
        {
            previous.content.to_mut().push_str(grapheme);
        } else {
            spans.push(Span::styled(grapheme.to_string(), style));
        }
        character_index = character_index.saturating_add(character_count);
    }
    spans
}

fn truncate_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= width {
        return spans;
    }
    if width == 0 {
        return Vec::new();
    }
    let target = width.saturating_sub(1);
    let mut used = 0;
    let mut shortened = Vec::new();
    'spans: for span in spans {
        let mut content = String::new();
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = display_width(grapheme);
            if used + grapheme_width > target {
                if !content.is_empty() {
                    shortened.push(Span::styled(content, span.style));
                }
                break 'spans;
            }
            content.push_str(grapheme);
            used += grapheme_width;
        }
        if !content.is_empty() {
            shortened.push(Span::styled(content, span.style));
        }
    }
    shortened.push(Span::from("…").dim());
    shortened
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum()
}
