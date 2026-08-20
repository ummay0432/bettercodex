use crate::input::MAX_TOTAL_IMAGE_BYTES;
use crate::input::PromptFileAttachment;
use crate::input::PromptImage;
use crate::input::PromptImageAttachment;
use crate::input::UserPrompt;
use crate::input::file_attachment_text;
use crate::skills::SkillMention;
use crate::skills::SkillSelection;
use crate::tui::width::display_width;
use std::borrow::Cow;
use std::cell::Ref;
use std::cell::RefCell;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";
/// Match Codex's threshold for replacing a paste with a compact composer element.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;

#[derive(Debug, Default)]
pub(super) struct Editor {
    text: String,
    cursor: usize,
    wrap_cache: RefCell<Option<WrapCache>>,
    preferred_column: Option<usize>,
    history: VecDeque<EditorHistoryEntry>,
    history_index: Option<usize>,
    saved_draft: Option<EditorHistoryEntry>,
    pending_pastes: Vec<PendingPaste>,
    skill_mentions: Vec<SkillMention>,
    file_attachments: Vec<PromptFileAttachment>,
    image_attachments: Vec<PromptImageAttachment>,
    history_search: Option<HistorySearchSession>,
    history_has_older: bool,
    history_load_in_flight: bool,
    pending_history_load: Option<HistoryLoadIntent>,
}

#[derive(Debug)]
struct WrapCache {
    width: usize,
    ranges: Vec<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingPaste {
    placeholder: String,
    content: String,
    range: Range<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EditorHistoryEntry {
    text: String,
    pending_pastes: Vec<PendingPaste>,
    skill_mentions: Vec<SkillMention>,
    file_attachments: Vec<PromptFileAttachment>,
    image_attachments: Vec<PromptImageAttachment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HistorySearchStatus {
    Idle,
    Match,
    Searching,
    NoMatch,
}

#[derive(Debug)]
enum HistoryLoadIntent {
    RecallPrevious { steps: usize },
    Search { query: String, target_match: usize },
}

#[derive(Debug)]
struct HistorySearchSession {
    original: EditorSnapshot,
    query: String,
    matches: Vec<usize>,
    seen_texts: HashSet<String>,
    selected: Option<usize>,
    status: HistorySearchStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EditorSnapshot {
    text: String,
    cursor: usize,
    preferred_column: Option<usize>,
    history_index: Option<usize>,
    saved_draft: Option<EditorHistoryEntry>,
    pending_pastes: Vec<PendingPaste>,
    skill_mentions: Vec<SkillMention>,
    file_attachments: Vec<PromptFileAttachment>,
    image_attachments: Vec<PromptImageAttachment>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct EditorLayout {
    pub(super) lines: Vec<String>,
    /// Byte ranges within each visible line that represent compacted pastes.
    pub(super) paste_ranges: Vec<Vec<Range<usize>>>,
    /// Byte ranges within each visible line that represent selected skill mentions.
    pub(super) skill_ranges: Vec<Vec<Range<usize>>>,
    /// Byte ranges within each visible line that represent attached images.
    pub(super) image_ranges: Vec<Vec<Range<usize>>>,
    /// Byte ranges within each visible line matching the active Ctrl+R query.
    pub(super) history_search_ranges: Vec<Vec<Range<usize>>>,
    pub(super) cursor_row: u16,
    pub(super) cursor_column: u16,
    pub(super) total_lines: u16,
}

impl Editor {
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(super) fn history_search_active(&self) -> bool {
        self.history_search.is_some()
    }

    pub(super) fn history_search_query(&self) -> Option<&str> {
        self.history_search
            .as_ref()
            .map(|search| search.query.as_str())
    }

    pub(super) fn history_search_status(&self) -> Option<HistorySearchStatus> {
        self.history_search.as_ref().map(|search| search.status)
    }

    pub(super) fn begin_history_search(&mut self) {
        if self.history_search.is_some() {
            self.history_search_older();
            return;
        }
        self.history_search = Some(HistorySearchSession {
            original: self.snapshot(),
            query: String::new(),
            matches: Vec::new(),
            seen_texts: HashSet::new(),
            selected: None,
            status: HistorySearchStatus::Idle,
        });
        self.history_index = None;
        self.saved_draft = None;
    }

    pub(super) fn history_search_insert(&mut self, value: &str) {
        let Some(search) = self.history_search.as_mut() else {
            return;
        };
        for character in value.chars() {
            if character.is_control() {
                continue;
            }
            search.query.push(character);
        }
        self.restart_history_search();
    }

    pub(super) fn history_search_backspace(&mut self) {
        if let Some(search) = self.history_search.as_mut() {
            search.query.pop();
        }
        self.restart_history_search();
    }

    pub(super) fn history_search_clear(&mut self) {
        if let Some(search) = self.history_search.as_mut() {
            search.query.clear();
        }
        self.restart_history_search();
    }

    pub(super) fn history_search_older(&mut self) {
        let Some((query, target_match, match_count)) =
            self.history_search.as_ref().and_then(|search| {
                (!search.query.is_empty()).then(|| {
                    (
                        search.query.clone(),
                        search
                            .selected
                            .map_or(0, |selected| selected.saturating_add(1)),
                        search.matches.len(),
                    )
                })
            })
        else {
            return;
        };
        if target_match < match_count {
            if let Some(search) = self.history_search.as_mut() {
                search.selected = Some(target_match);
                search.status = HistorySearchStatus::Match;
            }
            self.preview_history_search_match();
        } else if self.history_has_older {
            self.request_history_load(HistoryLoadIntent::Search {
                query,
                target_match,
            });
            if let Some(search) = self.history_search.as_mut() {
                search.status = HistorySearchStatus::Searching;
            }
        } else if let Some(search) = self.history_search.as_mut() {
            search.status = if search.matches.is_empty() {
                HistorySearchStatus::NoMatch
            } else {
                HistorySearchStatus::Match
            };
        }
    }

    pub(super) fn history_search_newer(&mut self) {
        let Some(search) = self.history_search.as_mut() else {
            return;
        };
        if search.query.is_empty() || search.matches.is_empty() {
            return;
        }
        if matches!(
            self.pending_history_load,
            Some(HistoryLoadIntent::Search { .. })
        ) {
            self.pending_history_load = None;
        }
        let selected = search.selected.unwrap_or_default().saturating_sub(1);
        search.selected = Some(selected);
        search.status = HistorySearchStatus::Match;
        self.preview_history_search_match();
    }

    pub(super) fn accept_history_search(&mut self) {
        if self
            .history_search
            .as_ref()
            .is_some_and(|search| search.selected.is_some())
        {
            self.history_search = None;
            self.pending_history_load = None;
            self.history_index = None;
            self.saved_draft = None;
            self.cursor = self.text.len();
            self.preferred_column = None;
        }
    }

    pub(super) fn cancel_history_search(&mut self) {
        self.pending_history_load = None;
        if let Some(search) = self.history_search.take() {
            self.restore_snapshot(search.original);
        }
    }

    pub(super) fn set_text(&mut self, text: impl Into<String>) {
        self.history_search = None;
        self.pending_history_load = None;
        self.text = text.into();
        self.invalidate_wrapping();
        self.cursor = self.text.len();
        self.preferred_column = None;
        self.pending_pastes.clear();
        self.skill_mentions.clear();
        self.file_attachments.clear();
        self.image_attachments.clear();
    }

    pub(super) fn set_user_prompt(&mut self, prompt: &UserPrompt) {
        self.set_text(prompt.as_str());
        self.bind_skill_mentions(prompt.skill_mentions(), 0);
        self.bind_file_attachments(prompt.file_attachments(), 0);
        self.bind_image_attachments(prompt.image_attachments(), 0);
    }

    pub(super) fn prepend_user_prompt(&mut self, prompt: &UserPrompt) {
        if prompt.as_str().is_empty() {
            return;
        }
        let text = format!("{}\n\n", prompt.as_str());
        self.prepend(&text);
        self.bind_skill_mentions(prompt.skill_mentions(), 0);
        self.bind_file_attachments(prompt.file_attachments(), 0);
        self.bind_image_attachments(prompt.image_attachments(), 0);
    }

    pub(super) fn take_prompt(&mut self) -> UserPrompt {
        let (text, mentions, files, images) = self.take_contents();
        UserPrompt::with_all_attachments(text, mentions, files, images)
    }

    pub(super) fn attach_image(&mut self, image: PromptImage) -> Result<(), String> {
        let total = self
            .image_attachments
            .iter()
            .map(|attachment| attachment.image().byte_len())
            .try_fold(image.byte_len(), usize::checked_add)
            .filter(|total| *total <= MAX_TOTAL_IMAGE_BYTES)
            .ok_or_else(|| {
                format!(
                    "attached images exceed bettercodex's {} MiB input limit",
                    MAX_TOTAL_IMAGE_BYTES / (1024 * 1024)
                )
            })?;
        debug_assert!(total <= MAX_TOTAL_IMAGE_BYTES);
        let placeholder = self.next_image_placeholder();
        let start = self.cursor;
        self.insert(&placeholder);
        self.image_attachments.push(PromptImageAttachment::new(
            image,
            start..start.saturating_add(placeholder.len()),
        ));
        Ok(())
    }

    pub(super) fn remember_snapshot(&mut self, snapshot: &EditorSnapshot) {
        self.remember_entry(EditorHistoryEntry {
            text: snapshot.text.clone(),
            pending_pastes: snapshot.pending_pastes.clone(),
            skill_mentions: snapshot.skill_mentions.clone(),
            file_attachments: snapshot.file_attachments.clone(),
            image_attachments: snapshot.image_attachments.clone(),
        });
    }

    fn remember(&mut self, text: &str) {
        self.remember_entry(EditorHistoryEntry {
            text: text.to_string(),
            ..EditorHistoryEntry::default()
        });
    }

    fn remember_entry(&mut self, entry: EditorHistoryEntry) {
        if !entry.text.is_empty() && self.history.back().is_none_or(|last| last != &entry) {
            self.history.push_back(entry);
        }
        self.pending_history_load = None;
        self.history_index = None;
        self.saved_draft = None;
    }

    pub(super) fn seed_history(&mut self, history: impl IntoIterator<Item = String>) {
        for text in history {
            self.remember(&text);
        }
    }

    pub(super) fn set_persistent_history_available(&mut self, available: bool) {
        self.history_has_older = available;
        if !available && !self.history_load_in_flight {
            self.pending_history_load = None;
        }
    }

    /// Marks a pending lazy-history request as started.
    ///
    /// The caller owns the blocking I/O task and returns its newest-first batch through
    /// [`Self::persistent_history_loaded`].
    pub(super) fn begin_history_load(&mut self) -> bool {
        if self.history_has_older
            && !self.history_load_in_flight
            && self.pending_history_load.is_some()
        {
            self.history_load_in_flight = true;
            true
        } else {
            false
        }
    }

    pub(super) fn persistent_history_loaded(
        &mut self,
        newest_first: impl IntoIterator<Item = String>,
        has_more: bool,
    ) {
        self.history_load_in_flight = false;
        self.history_has_older = has_more;
        self.prepend_persistent_history(newest_first);

        let intent = self.pending_history_load.take();
        match intent {
            Some(HistoryLoadIntent::RecallPrevious { mut steps }) => {
                while steps > 0 && self.recall_previous_loaded() {
                    steps = steps.saturating_sub(1);
                }
                if steps > 0 && self.history_has_older {
                    self.pending_history_load = Some(HistoryLoadIntent::RecallPrevious { steps });
                }
            }
            Some(HistoryLoadIntent::Search {
                query,
                target_match,
            }) => self.fulfill_history_search(query, target_match),
            None => self.refresh_active_history_search(),
        }
    }

    pub(super) fn persistent_history_failed(&mut self) {
        self.history_load_in_flight = false;
        self.history_has_older = false;
        self.pending_history_load = None;
        self.refresh_active_history_search();
    }

    pub(super) fn clear_for_ctrl_c(&mut self) {
        if self.is_empty() {
            return;
        }
        let snapshot = self.snapshot();
        self.remember_snapshot(&snapshot);
        self.set_text("");
    }

    fn history_entry(&self) -> EditorHistoryEntry {
        EditorHistoryEntry {
            text: self.text.clone(),
            pending_pastes: self.pending_pastes.clone(),
            skill_mentions: self.skill_mentions.clone(),
            file_attachments: self.file_attachments.clone(),
            image_attachments: self.image_attachments.clone(),
        }
    }

    fn apply_history_entry(&mut self, entry: EditorHistoryEntry) {
        self.text = entry.text;
        self.invalidate_wrapping();
        self.cursor = self.text.len();
        self.preferred_column = None;
        self.pending_pastes = entry.pending_pastes;
        self.skill_mentions = entry.skill_mentions;
        self.file_attachments = entry.file_attachments;
        self.image_attachments = entry.image_attachments;
    }

    pub(super) fn history_previous(&mut self) {
        if self.history.is_empty() {
            if self.history_has_older {
                if self.saved_draft.is_none() {
                    self.saved_draft = Some(self.history_entry());
                }
                self.request_previous_history(1);
            }
            return;
        }
        if !self.recall_previous_loaded() && self.history_has_older {
            self.request_previous_history(1);
        }
    }

    pub(super) fn history_next(&mut self) {
        if matches!(
            self.pending_history_load,
            Some(HistoryLoadIntent::RecallPrevious { .. })
        ) {
            self.pending_history_load = None;
        }
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            let next = index + 1;
            self.history_index = Some(next);
            self.apply_history_entry(self.history[next].clone());
        } else {
            self.history_index = None;
            let draft = self.saved_draft.take().unwrap_or_default();
            self.apply_history_entry(draft);
        }
    }

    pub(super) fn is_browsing_history(&self) -> bool {
        self.history_index.is_some()
    }

    pub(super) fn can_recall_older(&self) -> bool {
        (!self.history.is_empty() || self.history_has_older)
            && (self.text.is_empty()
                || (self.is_browsing_history()
                    && (self.cursor == 0 || self.cursor == self.text.len())))
    }

    pub(super) fn can_recall_newer(&self) -> bool {
        self.is_browsing_history() && (self.cursor == 0 || self.cursor == self.text.len())
    }

    pub(super) fn insert(&mut self, value: &str) {
        self.leave_history();
        self.replace_range_inner(self.cursor..self.cursor, value);
    }

    pub(super) fn prepend(&mut self, value: &str) {
        self.leave_history();
        self.replace_range_inner(0..0, value);
    }

    /// Insert a terminal paste using Codex's compact large-paste representation.
    pub(super) fn insert_paste(&mut self, value: String) {
        let char_count = value.chars().count();
        if char_count <= LARGE_PASTE_CHAR_THRESHOLD {
            self.insert(&value);
            return;
        }

        let placeholder = self.next_large_paste_placeholder(char_count);
        let start = self.cursor;
        self.insert(&placeholder);
        self.pending_pastes.push(PendingPaste {
            range: start..start + placeholder.len(),
            placeholder,
            content: value,
        });
    }

    pub(super) fn replace_range(&mut self, range: Range<usize>, value: &str) {
        self.leave_history();
        self.replace_range_inner(range, value);
    }

    pub(super) fn bind_skill(&mut self, range: Range<usize>, selection: SkillSelection) {
        if self.text.get(range.clone()) != Some(format!("${}", selection.name()).as_str())
            || self
                .atomic_ranges()
                .any(|atomic| ranges_overlap(atomic, &range))
        {
            return;
        }
        self.skill_mentions
            .push(SkillMention::new(selection, range));
    }

    pub(super) fn bind_file(&mut self, range: Range<usize>, path: std::path::PathBuf) {
        if self.text.get(range.clone()) != Some(file_attachment_text(&path).as_str())
            || self
                .atomic_ranges()
                .any(|atomic| ranges_overlap(atomic, &range))
        {
            return;
        }
        self.file_attachments
            .push(PromptFileAttachment::new(path, range));
    }

    pub(super) fn skill_ranges(&self) -> Vec<Range<usize>> {
        let mut ranges = self
            .skill_mentions
            .iter()
            .map(|mention| mention.range().clone())
            .collect::<Vec<_>>();
        ranges.sort_by_key(|range| range.start);
        ranges
    }

    pub(super) fn insert_newline(&mut self) {
        self.insert("\n");
    }

    pub(super) fn backspace(&mut self) {
        self.leave_history();
        let previous = self.previous_atomic_boundary(self.cursor);
        if previous < self.cursor {
            self.replace_range_inner(previous..self.cursor, "");
        }
        self.preferred_column = None;
    }

    pub(super) fn delete(&mut self) {
        self.leave_history();
        let next = self.next_atomic_boundary(self.cursor);
        if next > self.cursor {
            self.replace_range_inner(self.cursor..next, "");
        }
        self.preferred_column = None;
    }

    pub(super) fn delete_previous_word(&mut self) {
        self.leave_history();
        let start = beginning_of_previous_word(&self.text, self.cursor);
        self.replace_range_inner(start..self.cursor, "");
        self.preferred_column = None;
    }

    pub(super) fn kill_to_line_start(&mut self) {
        self.leave_history();
        let start = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.replace_range_inner(start..self.cursor, "");
        self.preferred_column = None;
    }

    pub(super) fn kill_to_line_end(&mut self) {
        self.leave_history();
        let end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
        if end == self.cursor && end < self.text.len() {
            self.replace_range_inner(end..end + 1, "");
        } else {
            self.replace_range_inner(self.cursor..end, "");
        }
        self.preferred_column = None;
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = self.previous_atomic_boundary(self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_right(&mut self) {
        self.cursor = self.next_atomic_boundary(self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_word_left(&mut self) {
        let position = beginning_of_previous_word(&self.text, self.cursor);
        self.cursor = self.atomic_start_boundary(position);
        self.preferred_column = None;
    }

    pub(super) fn move_word_right(&mut self) {
        let position = end_of_next_word(&self.text, self.cursor);
        self.cursor = self.atomic_end_boundary(position);
        self.preferred_column = None;
    }

    pub(super) fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.cursor = self.atomic_start_boundary(self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
        self.cursor = self.atomic_end_boundary(self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_vertical(&mut self, delta: isize, width: u16) {
        let width = usize::from(width.max(1));
        let ranges = self.wrapped_ranges(width);
        let current = line_for_cursor(&self.text, &ranges, self.cursor);
        let target = current.saturating_add_signed(delta).min(ranges.len() - 1);
        if target == current {
            drop(ranges);
            self.cursor = if delta < 0 { 0 } else { self.text.len() };
            self.preferred_column = None;
            return;
        }
        let column = self.preferred_column.unwrap_or_else(|| {
            cursor_column_for_range(&self.text, &ranges[current], self.cursor, width)
        });
        let position = byte_at_column(&self.text, &ranges[target], column);
        drop(ranges);
        self.preferred_column = Some(column);
        self.cursor = self.nearest_atomic_boundary(position);
    }

    pub(super) fn desired_height(&self, width: u16) -> u16 {
        self.wrapped_ranges(usize::from(width.max(1)))
            .len()
            .try_into()
            .unwrap_or(u16::MAX)
    }

    pub(super) fn layout(&self, width: u16, max_height: u16) -> EditorLayout {
        let width = width.max(1) as usize;
        let ranges = self.wrapped_ranges(width);
        let cursor_line = line_for_cursor(&self.text, &ranges, self.cursor);
        let max_height = max_height.max(1) as usize;
        let first = cursor_line
            .saturating_add(1)
            .saturating_sub(max_height)
            .min(ranges.len().saturating_sub(max_height));
        let end = (first + max_height).min(ranges.len());
        let visible_ranges = &ranges[first..end];
        let lines = visible_ranges
            .iter()
            .map(|range| self.text[range.clone()].replace('\t', " "))
            .collect();
        let paste_ranges = visible_ranges
            .iter()
            .map(|line| {
                let mut highlights = self
                    .pending_pastes
                    .iter()
                    .filter_map(|paste| {
                        let start = paste.range.start.max(line.start);
                        let end = paste.range.end.min(line.end);
                        (start < end).then_some(start - line.start..end - line.start)
                    })
                    .collect::<Vec<_>>();
                highlights.sort_by_key(|range| range.start);
                highlights
            })
            .collect();
        let skill_ranges = visible_ranges
            .iter()
            .map(|line| {
                let mut highlights = self
                    .skill_mentions
                    .iter()
                    .filter_map(|mention| {
                        let start = mention.range().start.max(line.start);
                        let end = mention.range().end.min(line.end);
                        (start < end).then_some(start - line.start..end - line.start)
                    })
                    .collect::<Vec<_>>();
                highlights.sort_by_key(|range| range.start);
                highlights
            })
            .collect();
        let image_ranges = visible_ranges
            .iter()
            .map(|line| {
                let mut highlights = self
                    .image_attachments
                    .iter()
                    .filter_map(|attachment| {
                        let start = attachment.range().start.max(line.start);
                        let end = attachment.range().end.min(line.end);
                        (start < end).then_some(start - line.start..end - line.start)
                    })
                    .collect::<Vec<_>>();
                highlights.sort_by_key(|range| range.start);
                highlights
            })
            .collect();
        let search_ranges = self.history_search_match_ranges();
        let history_search_ranges = visible_ranges
            .iter()
            .map(|line| {
                search_ranges
                    .iter()
                    .filter_map(|range| {
                        let start = range.start.max(line.start);
                        let end = range.end.min(line.end);
                        (start < end).then_some(start - line.start..end - line.start)
                    })
                    .collect()
            })
            .collect();
        EditorLayout {
            lines,
            paste_ranges,
            skill_ranges,
            image_ranges,
            history_search_ranges,
            cursor_row: (cursor_line - first) as u16,
            cursor_column: cursor_column_for_range(
                &self.text,
                &ranges[cursor_line],
                self.cursor,
                width,
            ) as u16,
            total_lines: ranges.len().try_into().unwrap_or(u16::MAX),
        }
    }

    fn replace_range_inner(&mut self, range: Range<usize>, value: &str) {
        let range = self.atomic_edit_range(range);
        let start = range.start;
        let removed_len = range.end - range.start;
        let inserted_len = value.len();

        self.pending_pastes.retain_mut(|paste| {
            if paste.range.end <= range.start {
                return true;
            }
            if paste.range.start >= range.end {
                shift_range(&mut paste.range, removed_len, inserted_len);
                return true;
            }
            false
        });
        self.skill_mentions.retain_mut(|mention| {
            if mention.range().end <= range.start {
                return true;
            }
            if mention.range().start >= range.end {
                shift_range(mention.range_mut(), removed_len, inserted_len);
                return true;
            }
            false
        });
        self.file_attachments.retain_mut(|attachment| {
            if attachment.range().end <= range.start {
                return true;
            }
            if attachment.range().start >= range.end {
                shift_range(attachment.range_mut(), removed_len, inserted_len);
                return true;
            }
            false
        });
        self.image_attachments.retain_mut(|attachment| {
            if attachment.range().end <= range.start {
                return true;
            }
            if attachment.range().start >= range.end {
                shift_range(attachment.range_mut(), removed_len, inserted_len);
                return true;
            }
            false
        });
        self.text.replace_range(range, value);
        self.invalidate_wrapping();
        self.cursor = start + inserted_len;
        self.preferred_column = None;
    }

    fn atomic_edit_range(&self, mut range: Range<usize>) -> Range<usize> {
        if range.is_empty() {
            let position = self.nearest_atomic_boundary(range.start);
            return position..position;
        }
        for atomic in self.atomic_ranges() {
            if range.start > atomic.start && range.start < atomic.end {
                range.start = atomic.start;
            }
            if range.end > atomic.start && range.end < atomic.end {
                range.end = atomic.end;
            }
        }
        range
    }

    fn previous_atomic_boundary(&self, position: usize) -> usize {
        self.atomic_ranges()
            .find(|range| position > range.start && position <= range.end)
            .map_or_else(
                || previous_boundary(&self.text, position),
                |range| range.start,
            )
    }

    fn next_atomic_boundary(&self, position: usize) -> usize {
        self.atomic_ranges()
            .find(|range| position >= range.start && position < range.end)
            .map_or_else(|| next_boundary(&self.text, position), |range| range.end)
    }

    fn nearest_atomic_boundary(&self, position: usize) -> usize {
        let Some(range) = self
            .atomic_ranges()
            .find(|range| position > range.start && position < range.end)
        else {
            return position;
        };
        if position - range.start < range.end - position {
            range.start
        } else {
            range.end
        }
    }

    fn atomic_start_boundary(&self, position: usize) -> usize {
        self.atomic_ranges()
            .find(|range| position > range.start && position < range.end)
            .map_or(position, |range| range.start)
    }

    fn atomic_end_boundary(&self, position: usize) -> usize {
        self.atomic_ranges()
            .find(|range| position > range.start && position < range.end)
            .map_or(position, |range| range.end)
    }

    fn next_large_paste_placeholder(&self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let prefix = format!("{base} #");
        let mut max_suffix = 0usize;
        for paste in &self.pending_pastes {
            if paste.placeholder == base {
                max_suffix = max_suffix.max(1);
            } else if let Some(suffix) = paste.placeholder.strip_prefix(&prefix)
                && let Ok(value) = suffix.parse::<usize>()
            {
                max_suffix = max_suffix.max(value);
            }
        }
        if max_suffix == 0 {
            base
        } else {
            format!("{base} #{}", max_suffix + 1)
        }
    }

    fn next_image_placeholder(&self) -> String {
        let mut number = 1_usize;
        loop {
            let placeholder = format!("[Image {number}]");
            if self.image_attachments.iter().all(|attachment| {
                self.text.get(attachment.range().clone()) != Some(placeholder.as_str())
            }) {
                return placeholder;
            }
            number = number.saturating_add(1);
        }
    }

    fn expanded_text(&self) -> String {
        let mut pastes = self.pending_pastes.iter().collect::<Vec<_>>();
        pastes.sort_by_key(|paste| paste.range.start);
        let mut expanded = String::with_capacity(self.text.len());
        let mut cursor = 0;
        for paste in pastes {
            debug_assert_eq!(
                self.text.get(paste.range.clone()),
                Some(paste.placeholder.as_str())
            );
            expanded.push_str(&self.text[cursor..paste.range.start]);
            expanded.push_str(&paste.content);
            cursor = paste.range.end;
        }
        expanded.push_str(&self.text[cursor..]);
        expanded
    }

    fn leave_history(&mut self) {
        self.history_search = None;
        self.pending_history_load = None;
        self.history_index = None;
        self.saved_draft = None;
    }

    fn request_history_load(&mut self, intent: HistoryLoadIntent) {
        if self.history_has_older {
            self.pending_history_load = Some(intent);
        }
    }

    fn request_previous_history(&mut self, steps: usize) {
        if !self.history_has_older {
            return;
        }
        if let Some(HistoryLoadIntent::RecallPrevious {
            steps: pending_steps,
        }) = self.pending_history_load.as_mut()
        {
            *pending_steps = pending_steps.saturating_add(steps);
        } else {
            self.pending_history_load = Some(HistoryLoadIntent::RecallPrevious { steps });
        }
    }

    fn recall_previous_loaded(&mut self) -> bool {
        let Some(index) = self.history_index else {
            let Some(index) = self.history.len().checked_sub(1) else {
                return false;
            };
            if self.saved_draft.is_none() {
                self.saved_draft = Some(self.history_entry());
            }
            self.history_index = Some(index);
            self.apply_history_entry(self.history[index].clone());
            return true;
        };
        let Some(index) = index.checked_sub(1) else {
            return false;
        };
        self.history_index = Some(index);
        self.apply_history_entry(self.history[index].clone());
        true
    }

    fn prepend_persistent_history(&mut self, newest_first: impl IntoIterator<Item = String>) {
        let mut added = 0;
        for text in newest_first {
            let entry = EditorHistoryEntry {
                text,
                ..EditorHistoryEntry::default()
            };
            if !entry.text.is_empty() && self.history.front().is_none_or(|newer| newer != &entry) {
                self.history.push_front(entry);
                added += 1;
            }
        }
        if added == 0 {
            return;
        }

        let shift = |index: &mut Option<usize>| {
            if let Some(index) = index {
                *index = index.saturating_add(added);
            }
        };
        shift(&mut self.history_index);
        if let Some(search) = self.history_search.as_mut() {
            shift(&mut search.original.history_index);
            for index in &mut search.matches {
                *index = index.saturating_add(added);
            }
        }
        self.extend_active_history_search(added);
    }

    fn matching_history(&self, query: &str) -> (Vec<usize>, HashSet<String>) {
        let folded_query = query.to_lowercase();
        let mut unique = HashSet::new();
        let matches = self
            .history
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, entry)| entry.text.to_lowercase().contains(&folded_query))
            .filter(|(_, entry)| unique.insert(entry.text.clone()))
            .map(|(index, _)| index)
            .collect();
        (matches, unique)
    }

    fn extend_active_history_search(&mut self, added: usize) {
        let Some(search) = self.history_search.as_mut() else {
            return;
        };
        if search.query.is_empty() {
            return;
        }
        let folded_query = search.query.to_lowercase();
        for index in (0..added).rev() {
            let entry = &self.history[index];
            if entry.text.to_lowercase().contains(&folded_query)
                && search.seen_texts.insert(entry.text.clone())
            {
                search.matches.push(index);
            }
        }
    }

    fn restart_history_search(&mut self) {
        let Some((query, original)) = self
            .history_search
            .as_ref()
            .map(|search| (search.query.clone(), search.original.clone()))
        else {
            return;
        };
        self.pending_history_load = None;
        self.restore_snapshot(original);
        if query.is_empty() {
            if let Some(search) = self.history_search.as_mut() {
                search.matches.clear();
                search.seen_texts.clear();
                search.selected = None;
                search.status = HistorySearchStatus::Idle;
            }
            return;
        }

        let (matches, seen_texts) = self.matching_history(&query);
        let has_match = !matches.is_empty();
        if let Some(search) = self.history_search.as_mut() {
            search.matches = matches;
            search.seen_texts = seen_texts;
            search.selected = has_match.then_some(0);
            search.status = if has_match {
                HistorySearchStatus::Match
            } else if self.history_has_older {
                HistorySearchStatus::Searching
            } else {
                HistorySearchStatus::NoMatch
            };
        }
        if has_match {
            self.preview_history_search_match();
        } else if self.history_has_older {
            self.request_history_load(HistoryLoadIntent::Search {
                query,
                target_match: 0,
            });
        }
    }

    fn fulfill_history_search(&mut self, query: String, target_match: usize) {
        if self.history_search_query() != Some(query.as_str()) {
            self.refresh_active_history_search();
            return;
        }
        let selected = self
            .history_search
            .as_ref()
            .and_then(|search| (target_match < search.matches.len()).then_some(target_match));
        if let Some(search) = self.history_search.as_mut() {
            if let Some(selected) = selected {
                search.selected = Some(selected);
                search.status = HistorySearchStatus::Match;
            } else if self.history_has_older {
                search.status = HistorySearchStatus::Searching;
                if let Some(previous) = search.selected {
                    search.selected = (!search.matches.is_empty())
                        .then_some(previous.min(search.matches.len() - 1));
                }
            } else if search.matches.is_empty() {
                search.selected = None;
                search.status = HistorySearchStatus::NoMatch;
            } else {
                search.selected = Some(search.matches.len() - 1);
                search.status = HistorySearchStatus::Match;
            }
        }
        if selected.is_some() || !self.history_has_older {
            self.preview_history_search_match();
        } else {
            self.request_history_load(HistoryLoadIntent::Search {
                query,
                target_match,
            });
        }
    }

    fn refresh_active_history_search(&mut self) {
        let Some((query, selected, was_searching)) = self.history_search.as_ref().map(|search| {
            (
                search.query.clone(),
                search.selected,
                search.status == HistorySearchStatus::Searching,
            )
        }) else {
            return;
        };
        if query.is_empty() {
            if let Some(search) = self.history_search.as_mut() {
                search.matches.clear();
                search.seen_texts.clear();
                search.selected = None;
                search.status = HistorySearchStatus::Idle;
            }
            return;
        }
        if let Some(search) = self.history_search.as_mut() {
            search.selected = selected
                .filter(|selected| *selected < search.matches.len())
                .or_else(|| (!search.matches.is_empty()).then_some(0));
            search.status = if search.matches.is_empty() {
                if was_searching && self.history_load_in_flight {
                    HistorySearchStatus::Searching
                } else {
                    HistorySearchStatus::NoMatch
                }
            } else {
                HistorySearchStatus::Match
            };
        }
        self.preview_history_search_match();
    }

    fn preview_history_search_match(&mut self) {
        let Some(history_index) = self.history_search.as_ref().and_then(|search| {
            search
                .selected
                .and_then(|selected| search.matches.get(selected))
                .copied()
        }) else {
            return;
        };
        self.apply_history_entry(self.history[history_index].clone());
    }

    pub(super) fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            preferred_column: self.preferred_column,
            history_index: self.history_index,
            saved_draft: self.saved_draft.clone(),
            pending_pastes: self.pending_pastes.clone(),
            skill_mentions: self.skill_mentions.clone(),
            file_attachments: self.file_attachments.clone(),
            image_attachments: self.image_attachments.clone(),
        }
    }

    pub(super) fn restore_snapshot(&mut self, snapshot: EditorSnapshot) {
        self.text = snapshot.text;
        self.invalidate_wrapping();
        self.cursor = snapshot.cursor;
        self.preferred_column = snapshot.preferred_column;
        self.history_index = snapshot.history_index;
        self.saved_draft = snapshot.saved_draft;
        self.pending_pastes = snapshot.pending_pastes;
        self.skill_mentions = snapshot.skill_mentions;
        self.file_attachments = snapshot.file_attachments;
        self.image_attachments = snapshot.image_attachments;
    }

    fn history_search_match_ranges(&self) -> Vec<Range<usize>> {
        let Some(search) = self
            .history_search
            .as_ref()
            .filter(|search| search.selected.is_some())
        else {
            return Vec::new();
        };
        case_insensitive_match_ranges(&self.text, &search.query)
    }

    fn atomic_ranges(&self) -> impl Iterator<Item = &Range<usize>> {
        self.pending_pastes
            .iter()
            .map(|paste| &paste.range)
            .chain(self.skill_mentions.iter().map(SkillMention::range))
            .chain(
                self.file_attachments
                    .iter()
                    .map(PromptFileAttachment::range),
            )
            .chain(
                self.image_attachments
                    .iter()
                    .map(PromptImageAttachment::range),
            )
    }

    fn bind_skill_mentions(&mut self, mentions: &[SkillMention], offset: usize) {
        for mention in mentions {
            let range = offset.saturating_add(mention.range().start)
                ..offset.saturating_add(mention.range().end);
            self.bind_skill(range, mention.selection().clone());
        }
    }

    fn bind_file_attachments(&mut self, attachments: &[PromptFileAttachment], offset: usize) {
        for attachment in attachments {
            let range = offset.saturating_add(attachment.range().start)
                ..offset.saturating_add(attachment.range().end);
            self.bind_file(range, attachment.path().to_path_buf());
        }
    }

    fn bind_image_attachments(&mut self, attachments: &[PromptImageAttachment], offset: usize) {
        for attachment in attachments {
            let range = offset.saturating_add(attachment.range().start)
                ..offset.saturating_add(attachment.range().end);
            if range.start < range.end
                && range.end <= self.text.len()
                && !self
                    .atomic_ranges()
                    .any(|atomic| ranges_overlap(atomic, &range))
            {
                self.image_attachments.push(PromptImageAttachment::new(
                    attachment.image().clone(),
                    range,
                ));
            }
        }
    }

    fn take_contents(
        &mut self,
    ) -> (
        String,
        Vec<SkillMention>,
        Vec<PromptFileAttachment>,
        Vec<PromptImageAttachment>,
    ) {
        self.skill_mentions
            .sort_by_key(|mention| mention.range().start);
        let mut mentions = std::mem::take(&mut self.skill_mentions);
        self.file_attachments
            .sort_by_key(|attachment| attachment.range().start);
        let mut files = std::mem::take(&mut self.file_attachments);
        self.image_attachments
            .sort_by_key(|attachment| attachment.range().start);
        let mut images = std::mem::take(&mut self.image_attachments);
        let text = if self.pending_pastes.is_empty() {
            std::mem::take(&mut self.text)
        } else {
            for mention in &mut mentions {
                let original_start = mention.range().start;
                for paste in &self.pending_pastes {
                    if paste.range.end <= original_start {
                        shift_range(
                            mention.range_mut(),
                            paste.placeholder.len(),
                            paste.content.len(),
                        );
                    }
                }
            }
            for attachment in &mut files {
                let original_start = attachment.range().start;
                for paste in &self.pending_pastes {
                    if paste.range.end <= original_start {
                        shift_range(
                            attachment.range_mut(),
                            paste.placeholder.len(),
                            paste.content.len(),
                        );
                    }
                }
            }
            for attachment in &mut images {
                let original_start = attachment.range().start;
                for paste in &self.pending_pastes {
                    if paste.range.end <= original_start {
                        shift_range(
                            attachment.range_mut(),
                            paste.placeholder.len(),
                            paste.content.len(),
                        );
                    }
                }
            }
            let expanded = self.expanded_text();
            self.text.clear();
            self.pending_pastes.clear();
            expanded
        };
        self.cursor = 0;
        self.invalidate_wrapping();
        self.preferred_column = None;
        self.history_index = None;
        self.saved_draft = None;
        (text, mentions, files, images)
    }

    fn wrapped_ranges(&self, width: usize) -> Ref<'_, Vec<Range<usize>>> {
        {
            let mut cache = self.wrap_cache.borrow_mut();
            if cache.as_ref().is_none_or(|cache| cache.width != width) {
                *cache = Some(WrapCache {
                    width,
                    ranges: editable_visual_ranges(&self.text, width),
                });
            }
        }

        Ref::map(self.wrap_cache.borrow(), |cache| match cache {
            Some(cache) => &cache.ranges,
            None => unreachable!("editor wrap cache was populated above"),
        })
    }

    fn invalidate_wrapping(&mut self) {
        *self.wrap_cache.get_mut() = None;
    }
}

fn case_insensitive_match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let folded_query = query.to_lowercase();
    if folded_query.is_empty() {
        return Vec::new();
    }

    let mut folded = String::new();
    let mut folded_spans = Vec::new();
    for (original_start, character) in text.char_indices() {
        let original_range = original_start..original_start + character.len_utf8();
        for lowercase in character.to_lowercase() {
            let folded_start = folded.len();
            folded.push(lowercase);
            folded_spans.push((folded_start..folded.len(), original_range.clone()));
        }
    }

    let mut ranges = Vec::new();
    let mut search_from = 0;
    while search_from <= folded.len()
        && let Some(relative_start) = folded[search_from..].find(&folded_query)
    {
        let folded_start = search_from + relative_start;
        let folded_end = folded_start + folded_query.len();
        if let Some((_, first_original)) = folded_spans.iter().find(|(folded_range, _)| {
            folded_range.end > folded_start && folded_range.start < folded_end
        }) {
            let original_end = folded_spans
                .iter()
                .rev()
                .find(|(folded_range, _)| {
                    folded_range.end > folded_start && folded_range.start < folded_end
                })
                .map(|(_, original_range)| original_range.end)
                .unwrap_or(first_original.end);
            ranges.push(first_original.start..original_end);
        }
        search_from = folded_end;
    }
    ranges
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn shift_range(range: &mut Range<usize>, removed_len: usize, inserted_len: usize) {
    if inserted_len >= removed_len {
        let shift = inserted_len - removed_len;
        range.start = range.start.saturating_add(shift);
        range.end = range.end.saturating_add(shift);
    } else {
        let shift = removed_len - inserted_len;
        range.start = range.start.saturating_sub(shift);
        range.end = range.end.saturating_sub(shift);
    }
}

fn is_word_separator(character: char) -> bool {
    WORD_SEPARATORS.contains(character)
}

fn split_word_pieces(run: &str) -> Vec<(usize, &str)> {
    let mut pieces = Vec::new();
    for (segment_start, segment) in run.split_word_bound_indices() {
        let mut piece_start = 0;
        let mut characters = segment.char_indices();
        let Some((_, first_character)) = characters.next() else {
            continue;
        };
        let mut in_separator = is_word_separator(first_character);
        for (index, character) in characters {
            let is_separator = is_word_separator(character);
            if is_separator != in_separator {
                pieces.push((segment_start + piece_start, &segment[piece_start..index]));
                piece_start = index;
                in_separator = is_separator;
            }
        }
        pieces.push((segment_start + piece_start, &segment[piece_start..]));
    }
    pieces
}

fn beginning_of_previous_word(text: &str, cursor: usize) -> usize {
    let prefix = &text[..cursor];
    let Some((first_non_whitespace, character)) = prefix
        .char_indices()
        .rev()
        .find(|(_, character)| !character.is_whitespace())
    else {
        return 0;
    };
    let run_start = prefix[..first_non_whitespace]
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map_or(0, |(index, character)| index + character.len_utf8());
    let run_end = first_non_whitespace + character.len_utf8();
    let mut pieces = split_word_pieces(&prefix[run_start..run_end])
        .into_iter()
        .rev()
        .peekable();
    let Some((piece_start, piece)) = pieces.next() else {
        return run_start;
    };
    let mut start = run_start + piece_start;
    if piece.chars().all(is_word_separator) {
        while let Some((index, piece)) = pieces.peek() {
            if !piece.chars().all(is_word_separator) {
                break;
            }
            start = run_start + *index;
            pieces.next();
        }
    }
    start
}

fn end_of_next_word(text: &str, cursor: usize) -> usize {
    let suffix = &text[cursor..];
    let Some(first_non_whitespace) = suffix.find(|character: char| !character.is_whitespace())
    else {
        return text.len();
    };
    let run = &suffix[first_non_whitespace..];
    let run = &run[..run.find(char::is_whitespace).unwrap_or(run.len())];
    let mut pieces = split_word_pieces(run).into_iter().peekable();
    let Some((piece_start, piece)) = pieces.next() else {
        return cursor + first_non_whitespace;
    };
    let word_start = cursor + first_non_whitespace + piece_start;
    let mut end = word_start + piece.len();
    if piece.chars().all(is_word_separator) {
        while let Some((index, piece)) = pieces.peek() {
            if !piece.chars().all(is_word_separator) {
                break;
            }
            end = cursor + first_non_whitespace + *index + piece.len();
            pieces.next();
        }
    }
    end
}

pub(super) fn visual_ranges(text: &str, width: usize) -> Vec<Range<usize>> {
    let display_text = text_for_display(text);
    visual_ranges_for_display(&display_text, width, false)
}

fn editable_visual_ranges(text: &str, width: usize) -> Vec<Range<usize>> {
    let display_text = text_for_display(text);
    visual_ranges_for_display(&display_text, width, true)
}

fn visual_ranges_for_display(
    text: &str,
    width: usize,
    reserve_cursor_row: bool,
) -> Vec<Range<usize>> {
    if text.is_empty() {
        return std::iter::once(0..0).collect();
    }
    let mut ranges = Vec::new();
    let mut logical_start = 0;
    for segment in text.split_inclusive('\n') {
        let has_newline = segment.ends_with('\n');
        let logical_end = logical_start + segment.len() - usize::from(has_newline);
        wrap_logical_line(
            text,
            logical_start,
            logical_end,
            width,
            reserve_cursor_row,
            &mut ranges,
        );
        logical_start += segment.len();
    }
    if text.ends_with('\n') {
        ranges.push(text.len()..text.len());
    }
    ranges
}

fn is_breakable_space(character: char) -> bool {
    character.is_whitespace() && !matches!(character, '\u{a0}' | '\u{2007}' | '\u{202f}')
}

fn wrap_logical_line(
    text: &str,
    logical_start: usize,
    logical_end: usize,
    width: usize,
    reserve_cursor_row: bool,
    ranges: &mut Vec<Range<usize>>,
) {
    if logical_start == logical_end {
        ranges.push(logical_start..logical_end);
        return;
    }
    let logical_content_end = logical_start
        + text[logical_start..logical_end]
            .trim_end_matches(is_breakable_space)
            .len();
    let mut start = logical_start;
    let mut has_text_before_start = false;
    while start < logical_end {
        if has_text_before_start && start < logical_content_end {
            let mut content_start = start;
            while content_start < logical_content_end {
                let next = next_boundary(text, content_start).min(logical_content_end);
                if !text[content_start..next].chars().all(is_breakable_space) {
                    break;
                }
                content_start = next;
            }
            // Interior separators hang at the previous soft break. Keep indentation and trailing
            // whitespace in explicit ranges so it remains visible and its cursor stays bounded.
            start = content_start;
        }
        let mut used = 0;
        let mut fitted_end = start;
        let mut whitespace_break = None;
        for (offset, grapheme) in text[start..logical_end].grapheme_indices(true) {
            let grapheme_start = start + offset;
            let grapheme_end = grapheme_start + grapheme.len();
            let grapheme_width = display_width(grapheme).max(1);
            if used + grapheme_width > width && fitted_end > start {
                break;
            }
            fitted_end = grapheme_end;
            used += grapheme_width;
            if grapheme.chars().all(is_breakable_space) {
                whitespace_break = Some(grapheme_end);
            }
            if used >= width {
                break;
            }
        }
        if fitted_end >= logical_end {
            ranges.push(start..logical_end);
            if reserve_cursor_row && used >= width {
                ranges.push(logical_end..logical_end);
            }
            break;
        }
        let exact_word_boundary = fitted_end >= logical_end
            || text[fitted_end..logical_end]
                .chars()
                .next()
                .is_some_and(is_breakable_space)
            || text[start..fitted_end]
                .chars()
                .next_back()
                .is_some_and(is_breakable_space);
        let end = if used == width && exact_word_boundary {
            fitted_end
        } else {
            whitespace_break
                .filter(|end| *end > start)
                .unwrap_or(fitted_end)
        };
        has_text_before_start |= text[start..end]
            .chars()
            .any(|character| !is_breakable_space(character));
        ranges.push(start..end);
        start = end;
    }
}

fn line_for_cursor(text: &str, ranges: &[Range<usize>], cursor: usize) -> usize {
    let line = ranges
        .partition_point(|range| range.start <= cursor)
        .saturating_sub(1)
        .min(ranges.len() - 1);
    let Some(next) = ranges.get(line + 1) else {
        return line;
    };
    if cursor >= ranges[line].end
        && cursor < next.start
        && !text[ranges[line].end..next.start].contains('\n')
    {
        line + 1
    } else {
        line
    }
}

fn cursor_column_for_range(text: &str, range: &Range<usize>, cursor: usize, width: usize) -> usize {
    if cursor <= range.start {
        return 0;
    }
    editable_display_width(&text[range.start..cursor.min(range.end)]).min(width.saturating_sub(1))
}

fn byte_at_column(text: &str, range: &Range<usize>, target: usize) -> usize {
    let mut width = 0;
    for (offset, grapheme) in text[range.clone()].grapheme_indices(true) {
        let next_width = width + display_width(grapheme).max(1);
        if next_width > target {
            return range.start + offset;
        }
        width = next_width;
    }
    range.end
}

fn previous_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(text.len(), |(index, _)| cursor + index)
}

/// Tabs occupy one rendered column. Since a tab and a space are both one byte, wrapping the
/// display copy preserves ranges into the editable source.
fn text_for_display(text: &str) -> Cow<'_, str> {
    if text.contains('\t') {
        Cow::Owned(text.replace('\t', " "))
    } else {
        Cow::Borrowed(text)
    }
}

fn editable_display_width(text: &str) -> usize {
    display_width(text_for_display(text).as_ref())
}
