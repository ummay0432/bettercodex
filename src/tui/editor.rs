use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

#[derive(Debug, Default)]
pub(super) struct Editor {
    text: String,
    cursor: usize,
    preferred_column: Option<usize>,
    history: Vec<String>,
    history_index: Option<usize>,
    saved_draft: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct EditorLayout {
    pub(super) lines: Vec<String>,
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

    pub(super) fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.preferred_column = None;
    }

    pub(super) fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.preferred_column = None;
        self.history_index = None;
        self.saved_draft.clear();
        text
    }

    pub(super) fn remember(&mut self, text: &str) {
        if !text.is_empty() && self.history.last().is_none_or(|last| last != text) {
            self.history.push(text.to_string());
        }
        self.history_index = None;
        self.saved_draft.clear();
    }

    pub(super) fn seed_history(&mut self, history: impl IntoIterator<Item = String>) {
        for text in history {
            self.remember(&text);
        }
    }

    pub(super) fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.saved_draft = self.text.clone();
                self.history.len() - 1
            }
        };
        self.history_index = Some(index);
        self.set_text(self.history[index].clone());
    }

    pub(super) fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            let next = index + 1;
            self.history_index = Some(next);
            self.set_text(self.history[next].clone());
        } else {
            self.history_index = None;
            let draft = std::mem::take(&mut self.saved_draft);
            self.set_text(draft);
        }
    }

    pub(super) fn is_browsing_history(&self) -> bool {
        self.history_index.is_some()
    }

    pub(super) fn can_recall_older(&self) -> bool {
        !self.history.is_empty()
            && (self.text.is_empty()
                || (self.is_browsing_history()
                    && (self.cursor == 0 || self.cursor == self.text.len())))
    }

    pub(super) fn can_recall_newer(&self) -> bool {
        self.is_browsing_history() && (self.cursor == 0 || self.cursor == self.text.len())
    }

    pub(super) fn insert(&mut self, value: &str) {
        self.leave_history();
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.preferred_column = None;
    }

    pub(super) fn replace_range(&mut self, range: Range<usize>, value: &str) {
        self.leave_history();
        let start = range.start;
        self.text.replace_range(range, value);
        self.cursor = start + value.len();
        self.preferred_column = None;
    }

    pub(super) fn insert_newline(&mut self) {
        self.insert("\n");
    }

    pub(super) fn backspace(&mut self) {
        self.leave_history();
        let previous = previous_boundary(&self.text, self.cursor);
        if previous < self.cursor {
            self.text.replace_range(previous..self.cursor, "");
            self.cursor = previous;
        }
        self.preferred_column = None;
    }

    pub(super) fn delete(&mut self) {
        self.leave_history();
        let next = next_boundary(&self.text, self.cursor);
        if next > self.cursor {
            self.text.replace_range(self.cursor..next, "");
        }
        self.preferred_column = None;
    }

    pub(super) fn delete_previous_word(&mut self) {
        self.leave_history();
        let start = beginning_of_previous_word(&self.text, self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.preferred_column = None;
    }

    pub(super) fn kill_to_line_start(&mut self) {
        self.leave_history();
        let start = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.preferred_column = None;
    }

    pub(super) fn kill_to_line_end(&mut self) {
        self.leave_history();
        let end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
        if end == self.cursor && end < self.text.len() {
            self.text.replace_range(end..=end, "");
        } else {
            self.text.replace_range(self.cursor..end, "");
        }
        self.preferred_column = None;
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = previous_boundary(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_right(&mut self) {
        self.cursor = next_boundary(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_word_left(&mut self) {
        self.cursor = beginning_of_previous_word(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_word_right(&mut self) {
        self.cursor = end_of_next_word(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.preferred_column = None;
    }

    pub(super) fn move_end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
        self.preferred_column = None;
    }

    pub(super) fn move_vertical(&mut self, delta: isize, width: u16) {
        let ranges = visual_ranges(&self.text, width.max(1) as usize);
        let current = line_for_cursor(&ranges, self.cursor);
        let target = current.saturating_add_signed(delta).min(ranges.len() - 1);
        if target == current {
            self.cursor = if delta < 0 { 0 } else { self.text.len() };
            self.preferred_column = None;
            return;
        }
        let column = self
            .preferred_column
            .unwrap_or_else(|| display_width(&self.text[ranges[current].start..self.cursor]));
        self.preferred_column = Some(column);
        self.cursor = byte_at_column(&self.text, &ranges[target], column);
    }

    pub(super) fn layout(&self, width: u16, max_height: u16) -> EditorLayout {
        let width = width.max(1) as usize;
        let ranges = visual_ranges(&self.text, width);
        let cursor_line = line_for_cursor(&ranges, self.cursor);
        let max_height = max_height.max(1) as usize;
        let first = cursor_line
            .saturating_add(1)
            .saturating_sub(max_height)
            .min(ranges.len().saturating_sub(max_height));
        let end = (first + max_height).min(ranges.len());
        let lines = ranges[first..end]
            .iter()
            .map(|range| self.text[range.clone()].replace('\t', " "))
            .collect();
        EditorLayout {
            lines,
            cursor_row: (cursor_line - first) as u16,
            cursor_column: display_width(&self.text[ranges[cursor_line].start..self.cursor]) as u16,
            total_lines: ranges.len().try_into().unwrap_or(u16::MAX),
        }
    }

    fn leave_history(&mut self) {
        self.history_index = None;
        self.saved_draft.clear();
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

fn visual_ranges(text: &str, width: usize) -> Vec<Range<usize>> {
    if text.is_empty() {
        return std::iter::once(0..0).collect();
    }
    let mut ranges = Vec::new();
    let mut logical_start = 0;
    for segment in text.split_inclusive('\n') {
        let has_newline = segment.ends_with('\n');
        let logical_end = logical_start + segment.len() - usize::from(has_newline);
        wrap_logical_line(text, logical_start, logical_end, width, &mut ranges);
        logical_start += segment.len();
    }
    if text.ends_with('\n') {
        ranges.push(text.len()..text.len());
    }
    ranges
}

pub(super) fn wrap_text(text: &str, width: u16) -> Vec<String> {
    visual_ranges(text, usize::from(width.max(1)))
        .into_iter()
        .map(|range| text[range].replace('\t', " "))
        .collect()
}

fn wrap_logical_line(
    text: &str,
    logical_start: usize,
    logical_end: usize,
    width: usize,
    ranges: &mut Vec<Range<usize>>,
) {
    if logical_start == logical_end {
        ranges.push(logical_start..logical_end);
        return;
    }
    let mut start = logical_start;
    while start < logical_end {
        if start > logical_start {
            while start < logical_end {
                let next = next_boundary(text, start).min(logical_end);
                if !text[start..next].chars().all(char::is_whitespace) {
                    break;
                }
                start = next;
            }
            if start == logical_end {
                break;
            }
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
            if grapheme.chars().all(char::is_whitespace) {
                whitespace_break = Some(grapheme_end);
            }
            if used >= width {
                break;
            }
        }
        if fitted_end >= logical_end {
            ranges.push(start..logical_end);
            break;
        }
        let exact_word_boundary = fitted_end >= logical_end
            || text[fitted_end..logical_end]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
            || text[start..fitted_end]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        let end = if used == width && exact_word_boundary {
            fitted_end
        } else {
            whitespace_break
                .filter(|end| *end > start)
                .unwrap_or(fitted_end)
        };
        ranges.push(start..end);
        start = end;
    }
}

fn line_for_cursor(ranges: &[Range<usize>], cursor: usize) -> usize {
    ranges
        .partition_point(|range| range.start <= cursor)
        .saturating_sub(1)
        .min(ranges.len() - 1)
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

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text.replace('\t', " ").as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_moves_by_grapheme_clusters() {
        let mut editor = Editor::default();
        editor.insert("a👩‍💻b");
        editor.move_left();
        editor.backspace();
        assert_eq!(editor.text(), "ab");
    }

    #[test]
    fn layout_wraps_and_keeps_cursor_visible() {
        let mut editor = Editor::default();
        editor.insert("one two three four");
        let layout = editor.layout(7, 2);
        assert_eq!(layout.total_lines, 3);
        assert_eq!(layout.lines, ["three ", "four"]);
        assert_eq!(layout.cursor_row, 1);
        assert_eq!(layout.cursor_column, 4);
    }

    #[test]
    fn history_restores_the_unsubmitted_draft() {
        let mut editor = Editor::default();
        editor.remember("first");
        editor.set_text("draft");
        editor.history_previous();
        assert_eq!(editor.text(), "first");
        editor.history_next();
        assert_eq!(editor.text(), "draft");
    }

    #[test]
    fn seeded_multiline_history_cycles_at_text_boundaries() {
        let mut editor = Editor::default();
        editor.seed_history([
            "older\nmultiline".to_string(),
            "newer\nmultiline".to_string(),
        ]);

        assert!(editor.can_recall_older());
        editor.history_previous();
        assert_eq!(editor.text(), "newer\nmultiline");
        assert!(editor.can_recall_older());
        editor.history_previous();
        assert_eq!(editor.text(), "older\nmultiline");
        assert!(editor.can_recall_newer());
        editor.history_next();
        assert_eq!(editor.text(), "newer\nmultiline");
    }

    #[test]
    fn word_navigation_uses_codex_unicode_and_separator_boundaries() {
        let mut editor = Editor::default();
        editor.insert("alpha.beta gamma");

        editor.move_word_left();
        assert_eq!(editor.cursor(), 11);
        editor.move_word_left();
        assert_eq!(editor.cursor(), 6);
        editor.move_word_left();
        assert_eq!(editor.cursor(), 5);
        editor.move_word_left();
        assert_eq!(editor.cursor(), 0);

        editor.move_word_right();
        assert_eq!(editor.cursor(), 5);
        editor.move_word_right();
        assert_eq!(editor.cursor(), 6);
        editor.move_word_right();
        assert_eq!(editor.cursor(), 10);
        editor.move_word_right();
        assert_eq!(editor.cursor(), editor.text().len());

        editor.set_text("你好世界");
        editor.move_word_left();
        assert_eq!(editor.cursor(), 9);
        editor.move_word_right();
        assert_eq!(editor.cursor(), editor.text().len());
    }

    #[test]
    fn replacing_a_completion_moves_the_cursor_after_it() {
        let mut editor = Editor::default();
        editor.insert("inspect @vie next");
        editor.replace_range("inspect ".len().."inspect @vie".len(), "src/tui/view.rs");
        assert_eq!(editor.text(), "inspect src/tui/view.rs next");
        assert_eq!(editor.cursor(), "inspect src/tui/view.rs".len());
    }
}
