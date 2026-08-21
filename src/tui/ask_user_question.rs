use super::bottom_pane::selection_popup_common::MAX_POPUP_ROWS;
use super::bottom_pane::selection_popup_common::measure_text_height;
use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use super::editor::Editor;
use super::markdown;
use super::palette;
use super::view::truncate_line;
use super::width::display_width;
use super::width::prefix_fitting_width;
use super::wrapping::RtOptions;
use super::wrapping::word_wrap_line;
use crate::ask_user_question::AskUserQuestionAnswer;
use crate::ask_user_question::AskUserQuestionArgs;
use crate::ask_user_question::AskUserQuestionResponse;
use crate::ask_user_question::MAX_FREE_TEXT_BYTES;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;

const TOP_GAP_HEIGHT: u16 = 1;
const FOOTER_HEIGHT: u16 = 1;
const SECTION_GAP_HEIGHT: u16 = 1;
const MAX_DETAIL_HEIGHT: u16 = 7;
const MAX_EDITOR_HEIGHT: u16 = 5;

fn primary_style() -> Style {
    Style::default()
}

fn secondary_style() -> Style {
    primary_style().dim()
}

fn selected_style() -> Style {
    primary_style().bold()
}

fn focused_style() -> Style {
    palette::accent_style()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AskUserQuestionCardAction {
    None,
    Submit {
        call_id: String,
        response: AskUserQuestionResponse,
    },
    Cancel {
        call_id: String,
    },
    Interrupt,
}

#[derive(Debug)]
struct AnswerState {
    selected: Vec<bool>,
    other: String,
    cursor: usize,
}

pub(super) struct AskUserQuestionCard {
    call_id: String,
    arguments: AskUserQuestionArgs,
    answers: Vec<AnswerState>,
    // The question count is also the review tab index.
    current: usize,
    editing_other: bool,
    editor: Editor,
    validation: Option<String>,
}

impl AskUserQuestionCard {
    pub(super) fn new(call_id: String, arguments: AskUserQuestionArgs) -> Self {
        let answers = arguments
            .questions
            .iter()
            .map(|question| AnswerState {
                selected: question
                    .options
                    .iter()
                    .map(|option| question.multi_select && option.default_selected)
                    .collect(),
                other: String::new(),
                cursor: 0,
            })
            .collect();
        Self {
            call_id,
            arguments,
            answers,
            current: 0,
            editing_other: false,
            editor: Editor::default(),
            validation: None,
        }
    }

    pub(super) fn call_id(&self) -> &str {
        &self.call_id
    }

    pub(super) fn preferred_height(&self, width: u16, cwd: &Path) -> u16 {
        let inner_width = width.saturating_sub(4).max(1);
        let body_height = if self.review_active() {
            self.review_lines(inner_width).len() as u16
        } else {
            let question_height = measure_text_height(&self.question_lines(), inner_width);
            let choices_height = self.choices_preferred_height(inner_width);
            let detail_height = self.detail_preferred_height(inner_width, cwd);
            question_height
                .saturating_add(SECTION_GAP_HEIGHT)
                .saturating_add(choices_height)
                .saturating_add(u16::from(detail_height > 0).saturating_mul(SECTION_GAP_HEIGHT))
                .saturating_add(detail_height)
        };
        TOP_GAP_HEIGHT
            .saturating_add(menu_surface_padding_height())
            .saturating_add(1) // question tabs
            .saturating_add(SECTION_GAP_HEIGHT)
            .saturating_add(body_height)
            .saturating_add(u16::from(self.validation.is_some()))
            .saturating_add(FOOTER_HEIGHT)
    }

    pub(super) fn handle_paste(&mut self, text: &str) {
        if !self.editing_other {
            return;
        }
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        self.insert_bounded(&markdown::sanitize(&normalized));
    }

    pub(super) fn handle_key(
        &mut self,
        key: KeyEvent,
        available_width: u16,
    ) -> AskUserQuestionCardAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return AskUserQuestionCardAction::Interrupt;
        }
        if self.editing_other {
            return self.handle_editor_key(key, available_width);
        }
        if self.review_active() {
            return self.handle_review_key(key);
        }

        let row_count = self.row_count();
        let visible = MAX_POPUP_ROWS.min(row_count);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                return AskUserQuestionCardAction::Cancel {
                    call_id: self.call_id.clone(),
                };
            }
            KeyCode::Up if !control && !alt => self.move_cursor_up(row_count),
            KeyCode::Down if !control && !alt => self.move_cursor_down(row_count),
            KeyCode::PageUp => self.move_cursor_page(row_count, visible, false),
            KeyCode::PageDown => self.move_cursor_page(row_count, visible, true),
            KeyCode::Home => self.set_cursor(0),
            KeyCode::End => self.set_cursor(row_count.saturating_sub(1)),
            KeyCode::BackTab if !control && !alt => self.move_question(false),
            KeyCode::Tab if !control && !alt => self.move_question(true),
            KeyCode::Left if !control && !alt && !shift => self.move_question(false),
            KeyCode::Right if !control && !alt && !shift => self.move_question(true),
            KeyCode::Enter if control => return self.submit_all(),
            KeyCode::Enter => return self.activate_cursor(),
            KeyCode::Char(character)
                if !control && !alt && !character.is_control() && self.other_row_focused() =>
            {
                self.begin_editing_other();
                let mut encoded = [0_u8; 4];
                self.insert_bounded(character.encode_utf8(&mut encoded));
                return AskUserQuestionCardAction::None;
            }
            KeyCode::Char(' ')
                if !control && !alt && self.arguments.questions[self.current].multi_select =>
            {
                return self.activate_cursor();
            }
            KeyCode::Char(number)
                if key.modifiers.is_empty()
                    && number.to_digit(10).is_some_and(|number| {
                        number > 0 && number as usize <= self.numbered_row_count()
                    }) =>
            {
                let selected = number.to_digit(10).unwrap_or_default() as usize - 1;
                self.set_cursor(selected);
            }
            _ => return AskUserQuestionCardAction::None,
        }
        self.validation = None;
        AskUserQuestionCardAction::None
    }

    pub(super) fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        surface_style: Style,
        cwd: &Path,
    ) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        let top_gap_height = TOP_GAP_HEIGHT.min(area.height);
        let card_area = Rect::new(
            area.x,
            area.y.saturating_add(top_gap_height),
            area.width,
            area.height.saturating_sub(top_gap_height),
        );
        let footer_height = FOOTER_HEIGHT.min(card_area.height);
        let content_area = Rect::new(
            card_area.x,
            card_area.y,
            card_area.width,
            card_area.height.saturating_sub(footer_height),
        );
        let footer_area = Rect::new(area.x, content_area.bottom(), area.width, footer_height);
        let inner = render_menu_surface(content_area, frame.buffer_mut(), surface_style);
        if !inner.is_empty() {
            self.render_content(frame, inner, cwd);
        }
        if !footer_area.is_empty() {
            let hint_area = Rect::new(
                footer_area.x.saturating_add(2),
                footer_area.y,
                footer_area.width.saturating_sub(2),
                footer_area.height,
            );
            frame.render_widget(
                Paragraph::new(self.footer_hint(hint_area.width)).style(secondary_style()),
                hint_area,
            );
        }
    }

    fn handle_review_key(&mut self, key: KeyEvent) -> AskUserQuestionCardAction {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => AskUserQuestionCardAction::Cancel {
                call_id: self.call_id.clone(),
            },
            KeyCode::Enter => self.submit_all(),
            KeyCode::Left | KeyCode::BackTab if !control && !alt => {
                self.move_question(false);
                AskUserQuestionCardAction::None
            }
            _ => AskUserQuestionCardAction::None,
        }
    }

    fn handle_editor_key(
        &mut self,
        key: KeyEvent,
        available_width: u16,
    ) -> AskUserQuestionCardAction {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => {
                self.save_editor();
                self.editing_other = false;
            }
            KeyCode::Enter if control => {
                self.save_editor();
                self.editing_other = false;
                return self.submit_all();
            }
            KeyCode::Enter if shift || alt => {
                self.insert_bounded("\n");
                return AskUserQuestionCardAction::None;
            }
            KeyCode::Enter => {
                self.save_editor();
                if self.answers[self.current].other.trim().is_empty() {
                    self.validation = Some("Type an answer before continuing.".to_string());
                    return AskUserQuestionCardAction::None;
                }
                self.editing_other = false;
                if self.arguments.questions[self.current].multi_select {
                    self.validation = None;
                    return AskUserQuestionCardAction::None;
                }
                return self.advance_or_review();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.save_editor();
                self.editing_other = false;
                self.move_question(matches!(key.code, KeyCode::Tab));
            }
            KeyCode::Backspace if control => self.editor.delete_previous_word(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Left if control || alt => self.editor.move_word_left(),
            KeyCode::Left => self.editor.move_left(),
            KeyCode::Right if control || alt => self.editor.move_word_right(),
            KeyCode::Right => self.editor.move_right(),
            KeyCode::Up => self.move_editor_or_choice(false, available_width),
            KeyCode::Down => self.move_editor_or_choice(true, available_width),
            KeyCode::Home => self.editor.move_home(),
            KeyCode::End => self.editor.move_end(),
            KeyCode::Char('u') if control => self.editor.kill_to_line_start(),
            KeyCode::Char('k') if control => self.editor.kill_to_line_end(),
            KeyCode::Char(character) if !control => {
                let mut encoded = [0_u8; 4];
                self.insert_bounded(character.encode_utf8(&mut encoded));
                return AskUserQuestionCardAction::None;
            }
            _ => return AskUserQuestionCardAction::None,
        }
        self.validation = None;
        AskUserQuestionCardAction::None
    }

    fn move_editor_or_choice(&mut self, forward: bool, available_width: u16) {
        let width = self.other_text_width(available_width);
        let layout = self.editor.layout(width, u16::MAX);
        let at_boundary = if forward {
            layout.cursor_row.saturating_add(1) >= layout.total_lines
        } else {
            layout.cursor_row == 0
        };
        if !at_boundary {
            self.editor
                .move_vertical(if forward { 1 } else { -1 }, width);
            return;
        }

        self.save_editor();
        self.editing_other = false;
        let row_count = self.row_count();
        if forward {
            self.move_cursor_down(row_count);
        } else {
            self.move_cursor_up(row_count);
        }
    }

    fn activate_cursor(&mut self) -> AskUserQuestionCardAction {
        let question = &self.arguments.questions[self.current];
        let options_len = question.options.len();
        let cursor = self.answers[self.current]
            .cursor
            .min(self.row_count().saturating_sub(1));
        if cursor < options_len {
            if question.multi_select {
                self.answers[self.current].selected[cursor] =
                    !self.answers[self.current].selected[cursor];
                self.validation = None;
                return AskUserQuestionCardAction::None;
            }
            self.answers[self.current].selected.fill(false);
            self.answers[self.current].selected[cursor] = true;
            self.answers[self.current].other.clear();
            self.validation = None;
            return self.advance_or_review();
        }
        if cursor == options_len {
            self.begin_editing_other();
            return AskUserQuestionCardAction::None;
        }
        self.advance_or_review()
    }

    fn advance_or_review(&mut self) -> AskUserQuestionCardAction {
        if !self.current_answered() {
            self.validation = Some("Choose an option or type your own answer.".to_string());
            return AskUserQuestionCardAction::None;
        }
        if self.hide_submit_tab() {
            return self.submit_all();
        }
        self.current = (self.current + 1).min(self.arguments.questions.len());
        self.editing_other = false;
        self.validation = None;
        AskUserQuestionCardAction::None
    }

    fn submit_all(&mut self) -> AskUserQuestionCardAction {
        self.save_editor_if_active();
        if let Some(index) =
            (0..self.arguments.questions.len()).find(|index| !self.answered(*index))
        {
            self.current = index;
            self.editing_other = false;
            self.validation = Some("Answer every question before submitting.".to_string());
            return AskUserQuestionCardAction::None;
        }
        let answers = self
            .arguments
            .questions
            .iter()
            .zip(&self.answers)
            .map(|(question, answer)| AskUserQuestionAnswer {
                question: question.question.clone(),
                selected_options: question
                    .options
                    .iter()
                    .zip(&answer.selected)
                    .filter(|(_, selected)| **selected)
                    .map(|(option, _)| option.label.clone())
                    .collect(),
                free_text: (!answer.other.trim().is_empty()).then(|| answer.other.clone()),
            })
            .collect();
        AskUserQuestionCardAction::Submit {
            call_id: self.call_id.clone(),
            response: AskUserQuestionResponse::answered(answers),
        }
    }

    fn save_editor_if_active(&mut self) {
        if self.editing_other {
            self.save_editor();
        }
    }

    fn save_editor(&mut self) {
        if self.current < self.answers.len() {
            self.answers[self.current].other = self.editor.text().to_string();
        }
    }

    fn current_answered(&self) -> bool {
        self.current < self.answers.len() && self.answered(self.current)
    }

    fn other_row_focused(&self) -> bool {
        self.arguments
            .questions
            .get(self.current)
            .zip(self.answers.get(self.current))
            .is_some_and(|(question, answer)| answer.cursor == question.options.len())
    }

    fn begin_editing_other(&mut self) {
        let question = &self.arguments.questions[self.current];
        if !question.multi_select {
            self.answers[self.current].selected.fill(false);
        }
        self.editor
            .set_text(self.answers[self.current].other.clone());
        self.editing_other = true;
        self.validation = None;
    }

    fn answered(&self, index: usize) -> bool {
        self.answers[index]
            .selected
            .iter()
            .any(|selected| *selected)
            || !self.answers[index].other.trim().is_empty()
    }

    fn review_active(&self) -> bool {
        self.current >= self.arguments.questions.len()
    }

    fn hide_submit_tab(&self) -> bool {
        self.arguments.questions.len() == 1 && !self.arguments.questions[0].multi_select
    }

    fn row_count(&self) -> usize {
        let question = &self.arguments.questions[self.current];
        question
            .options
            .len()
            .saturating_add(1) // Type something.
            .saturating_add(usize::from(question.multi_select))
    }

    fn numbered_row_count(&self) -> usize {
        self.arguments.questions[self.current]
            .options
            .len()
            .saturating_add(1)
    }

    fn set_cursor(&mut self, cursor: usize) {
        self.answers[self.current].cursor = cursor.min(self.row_count().saturating_sub(1));
    }

    fn move_cursor_up(&mut self, len: usize) {
        let cursor = self.answers[self.current].cursor;
        self.answers[self.current].cursor = if cursor == 0 {
            len.saturating_sub(1)
        } else {
            cursor - 1
        };
    }

    fn move_cursor_down(&mut self, len: usize) {
        self.answers[self.current].cursor =
            self.answers[self.current].cursor.saturating_add(1) % len.max(1);
    }

    fn move_cursor_page(&mut self, len: usize, visible: usize, down: bool) {
        let cursor = self.answers[self.current].cursor;
        self.answers[self.current].cursor = if down {
            cursor
                .saturating_add(visible.max(1))
                .min(len.saturating_sub(1))
        } else {
            cursor.saturating_sub(visible.max(1))
        };
    }

    fn move_question(&mut self, forward: bool) {
        let question_count = self.arguments.questions.len();
        if self.hide_submit_tab() {
            return;
        }
        self.save_editor_if_active();
        self.editing_other = false;
        if forward {
            self.current = self.current.saturating_add(1).min(question_count);
        } else {
            self.current = self.current.saturating_sub(1);
        }
        self.validation = None;
    }

    fn insert_bounded(&mut self, value: &str) {
        let remaining = MAX_FREE_TEXT_BYTES.saturating_sub(self.editor.text().len());
        if remaining == 0 {
            self.validation = Some(format!(
                "Other answers are limited to {MAX_FREE_TEXT_BYTES} bytes."
            ));
            return;
        }
        let original_len = value.len();
        let value = utf8_prefix(value, remaining);
        if value.is_empty() {
            return;
        }
        self.editor.insert(value);
        if value.len() < original_len {
            self.validation = Some(format!(
                "Other answers are limited to {MAX_FREE_TEXT_BYTES} bytes."
            ));
        } else {
            self.validation = None;
        }
    }

    fn tabs_line(&self, width: u16) -> Line<'static> {
        let hide_navigation = self.hide_submit_tab();
        let show_submit = !hide_navigation;
        let question_count = self.arguments.questions.len();
        let fixed_width = usize::from(!hide_navigation)
            .saturating_mul(4)
            .saturating_add(usize::from(show_submit).saturating_mul(10))
            .saturating_add(question_count.saturating_mul(4));
        let labels_budget = usize::from(width).saturating_sub(fixed_width);
        let label_budget = labels_budget
            .checked_div(question_count)
            .unwrap_or_default();
        let mut spans = Vec::new();
        if !hide_navigation {
            let left_style = if self.current == 0 {
                secondary_style()
            } else {
                primary_style()
            };
            spans.push(Span::styled("← ", left_style));
        }
        for (index, question) in self.arguments.questions.iter().enumerate() {
            let header = markdown::sanitize_inline(&question.header);
            let header = compact_label(&header, label_budget);
            let content = format!(" • {header} ");
            let style = if index == self.current {
                focused_style()
            } else if self.answered(index) {
                selected_style()
            } else {
                secondary_style()
            };
            spans.push(Span::styled(content, style));
            spans.push(Span::from(" "));
        }
        if show_submit {
            let style = if self.review_active() {
                focused_style()
            } else {
                secondary_style()
            };
            spans.push(Span::styled(" • Submit ", style));
            spans.push(Span::from(" "));
        }
        if !hide_navigation {
            let right_style = if self.review_active() {
                secondary_style()
            } else {
                primary_style()
            };
            spans.push(Span::styled("→", right_style));
        }
        truncate_line(Line::from(spans), usize::from(width))
    }

    fn question_lines(&self) -> Vec<Line<'static>> {
        let question = &self.arguments.questions[self.current];
        vec![Line::from(markdown::sanitize(&question.question)).style(selected_style())]
    }

    fn review_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from("Review your answers").style(selected_style()),
            Line::default(),
        ];
        for (index, question) in self.arguments.questions.iter().enumerate() {
            let marker = if self.answered(index) { "✓" } else { "□" };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {marker} "),
                    if self.answered(index) {
                        palette::accent_style()
                    } else {
                        palette::warning_color().into()
                    },
                ),
                Span::styled(
                    markdown::sanitize_inline(&question.header),
                    selected_style(),
                ),
            ]));
            let summary = self.answer_summary(index);
            let style = if self.answered(index) {
                secondary_style()
            } else {
                Style::default().fg(palette::warning_color())
            };
            let source = Line::from(Span::styled(summary, style));
            let options = RtOptions::new(usize::from(width.max(1)))
                .initial_indent(Line::from("      "))
                .subsequent_indent(Line::from("      "));
            lines.extend(
                word_wrap_line(&source, options)
                    .into_iter()
                    .map(line_to_owned),
            );
            if index + 1 < self.arguments.questions.len() {
                lines.push(Line::default());
            }
        }
        lines
    }

    fn answer_summary(&self, index: usize) -> String {
        let question = &self.arguments.questions[index];
        let answer = &self.answers[index];
        let mut values = question
            .options
            .iter()
            .zip(&answer.selected)
            .filter(|(_, selected)| **selected)
            .map(|(option, _)| markdown::sanitize_inline(&option.label))
            .collect::<Vec<_>>();
        if !answer.other.trim().is_empty() {
            values.push(concise_other(&answer.other));
        }
        if values.is_empty() {
            "Unanswered".to_string()
        } else {
            values.join(", ")
        }
    }

    fn choice_lines(&self, index: usize, width: u16) -> Vec<Line<'static>> {
        let question = &self.arguments.questions[self.current];
        let answer = &self.answers[self.current];
        let options_len = question.options.len();
        let focused = answer.cursor == index;
        if index < options_len {
            let option = &question.options[index];
            let selected = answer.selected[index];
            let (label_line, label_indent) = self.choice_label_line(
                index + 1,
                &markdown::sanitize_inline(&option.label),
                focused,
                selected,
                question.multi_select,
            );
            let options = RtOptions::new(usize::from(width.max(1)))
                .subsequent_indent(Line::from(" ".repeat(label_indent)));
            let mut lines = word_wrap_line(&label_line, options)
                .into_iter()
                .map(line_to_owned)
                .collect::<Vec<_>>();
            if !option.description.trim().is_empty() {
                let description = Line::from(Span::styled(
                    markdown::sanitize(&option.description),
                    secondary_style(),
                ));
                let indent = Line::from(" ".repeat(label_indent));
                let options = RtOptions::new(usize::from(width.max(1)))
                    .initial_indent(indent.clone())
                    .subsequent_indent(indent);
                lines.extend(
                    word_wrap_line(&description, options)
                        .into_iter()
                        .map(line_to_owned),
                );
            }
            lines.push(Line::default());
            return lines;
        }
        if index == options_len {
            let text = if answer.other.trim().is_empty() {
                "Type something.".to_string()
            } else {
                concise_other(&answer.other)
            };
            let (line, indent) = self.choice_label_line(
                options_len + 1,
                &text,
                focused,
                !answer.other.trim().is_empty(),
                question.multi_select,
            );
            let options = RtOptions::new(usize::from(width.max(1)))
                .subsequent_indent(Line::from(" ".repeat(indent)));
            return word_wrap_line(&line, options)
                .into_iter()
                .map(line_to_owned)
                .collect();
        }

        let label = if self.current + 1 < self.arguments.questions.len() {
            "Next"
        } else {
            "Submit"
        };
        let pointer_style = if focused {
            focused_style()
        } else {
            primary_style()
        };
        let label_style = if focused {
            focused_style()
        } else {
            selected_style()
        };
        vec![
            Line::default(),
            Line::from(vec![
                Span::styled(if focused { "›    " } else { "     " }, pointer_style),
                Span::styled(label, label_style),
            ]),
        ]
    }

    fn choice_label_line(
        &self,
        number: usize,
        label: &str,
        focused: bool,
        selected: bool,
        multi_select: bool,
    ) -> (Line<'static>, usize) {
        let number_width = self.number_width();
        let pointer_style = if focused {
            focused_style()
        } else {
            primary_style()
        };
        let number_style = secondary_style();
        let marker_style = if focused || selected {
            focused_style()
        } else {
            secondary_style()
        };
        let label_style = if focused {
            focused_style()
        } else if selected {
            selected_style()
        } else {
            primary_style()
        };
        let mut spans = vec![
            Span::styled(if focused { "› " } else { "  " }, pointer_style),
            Span::styled(format!("{number:>number_width$}. "), number_style),
        ];
        let indent = 2_usize.saturating_add(number_width).saturating_add(2);
        if multi_select {
            spans.push(Span::styled(
                if selected { "[✓] " } else { "[ ] " },
                marker_style,
            ));
        }
        spans.push(Span::styled(label.to_string(), label_style));
        (
            Line::from(spans),
            indent.saturating_add(if multi_select { 4 } else { 0 }),
        )
    }

    fn number_width(&self) -> usize {
        self.numbered_row_count().to_string().len()
    }

    fn choices_preferred_height(&self, width: u16) -> u16 {
        let count = self.row_count();
        let start = self
            .answers
            .get(self.current)
            .map(|answer| {
                answer
                    .cursor
                    .saturating_add(1)
                    .saturating_sub(MAX_POPUP_ROWS.min(count))
            })
            .unwrap_or_default();
        (start..count)
            .take(MAX_POPUP_ROWS)
            .map(|index| self.choice_height(index, width))
            .fold(0_u16, u16::saturating_add)
            .max(1)
    }

    fn choice_height(&self, index: usize, width: u16) -> u16 {
        let options_len = self.arguments.questions[self.current].options.len();
        if self.editing_other && index == options_len {
            return self
                .editor
                .desired_height(self.other_text_width(width))
                .clamp(1, MAX_EDITOR_HEIGHT);
        }
        u16::try_from(self.choice_lines(index, width).len()).unwrap_or(u16::MAX)
    }

    fn visible_choice_range(&self, width: u16, height: u16) -> Range<usize> {
        let count = self.row_count();
        if count == 0 {
            return 0..0;
        }
        let cursor = self.answers[self.current].cursor.min(count - 1);
        let maximum_items = MAX_POPUP_ROWS.min(count);
        let mut start = cursor.saturating_add(1).saturating_sub(maximum_items);
        while start < cursor {
            let used = (start..=cursor)
                .map(|index| self.choice_height(index, width))
                .fold(0_u16, u16::saturating_add);
            if used <= height.max(1) {
                break;
            }
            start = start.saturating_add(1);
        }
        start..start.saturating_add(maximum_items).min(count)
    }

    fn other_text_width(&self, width: u16) -> u16 {
        width
            .saturating_sub(u16::try_from(self.other_text_indent()).unwrap_or(u16::MAX))
            .max(1)
    }

    fn other_text_indent(&self) -> usize {
        2_usize
            .saturating_add(self.number_width())
            .saturating_add(2)
            .saturating_add(if self.arguments.questions[self.current].multi_select {
                4
            } else {
                0
            })
    }

    fn detail_preferred_height(&self, width: u16, cwd: &Path) -> u16 {
        let Some(preview) = self.selected_preview() else {
            return 0;
        };
        let lines = markdown::render_markdown_agent_with_links_and_cwd(
            preview,
            Some(usize::from(width.max(1))),
            Some(cwd),
        )
        .into_iter()
        .map(|line| line.line)
        .collect::<Vec<_>>();
        measure_text_height(&lines, width.max(1)).min(MAX_DETAIL_HEIGHT)
    }

    fn selected_preview(&self) -> Option<&str> {
        if self.review_active() || self.editing_other {
            return None;
        }
        let question = &self.arguments.questions[self.current];
        let cursor = self.answers[self.current].cursor;
        question
            .options
            .get(cursor)
            .and_then(|option| option.preview.as_deref())
            .filter(|preview| !preview.trim().is_empty())
    }

    fn render_content(&self, frame: &mut Frame<'_>, inner: Rect, cwd: &Path) {
        let tabs_area = Rect::new(inner.x, inner.y, inner.width, inner.height.min(1));
        frame.render_widget(Paragraph::new(self.tabs_line(inner.width)), tabs_area);
        if inner.height <= 1 {
            return;
        }
        let body_y = tabs_area
            .bottom()
            .saturating_add(SECTION_GAP_HEIGHT)
            .min(inner.bottom());
        let body_area = Rect::new(
            inner.x,
            body_y,
            inner.width,
            inner.bottom().saturating_sub(body_y),
        );
        if body_area.is_empty() {
            return;
        }
        if self.review_active() {
            let mut lines = self.review_lines(body_area.width);
            if let Some(validation) = &self.validation {
                lines.push(Line::from(markdown::sanitize(validation)).fg(palette::warning_color()));
            }
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body_area);
            return;
        }

        let question = self.question_lines();
        let question_height = measure_text_height(&question, body_area.width).min(body_area.height);
        let question_area = Rect::new(body_area.x, body_area.y, body_area.width, question_height);
        frame.render_widget(
            Paragraph::new(question).wrap(Wrap { trim: false }),
            question_area,
        );

        let choices_y = question_area
            .bottom()
            .saturating_add(SECTION_GAP_HEIGHT)
            .min(body_area.bottom());
        let available = body_area.bottom().saturating_sub(choices_y);
        if available == 0 {
            return;
        }
        let requested_detail = self.detail_preferred_height(body_area.width, cwd);
        let validation_height = u16::from(self.validation.is_some());
        let detail_gap = u16::from(requested_detail > 0).saturating_mul(SECTION_GAP_HEIGHT);
        let reserved_detail = requested_detail
            .min(available.saturating_sub(validation_height.saturating_add(detail_gap)));
        let choices_height = self
            .choices_preferred_height(body_area.width)
            .min(
                available
                    .saturating_sub(validation_height)
                    .saturating_sub(detail_gap)
                    .saturating_sub(reserved_detail),
            )
            .max(1);
        let choices_area = Rect::new(body_area.x, choices_y, body_area.width, choices_height);
        self.render_choices(frame, choices_area);

        let mut next_y = choices_area.bottom();
        if reserved_detail > 0 {
            next_y = next_y.saturating_add(detail_gap).min(body_area.bottom());
            let detail_area = Rect::new(
                body_area.x,
                next_y,
                body_area.width,
                reserved_detail.min(body_area.bottom().saturating_sub(next_y)),
            );
            if let Some(preview) = self.selected_preview() {
                self.render_preview(frame, detail_area, preview, cwd);
            }
            next_y = detail_area.bottom();
        }
        if let Some(validation) = &self.validation
            && next_y < body_area.bottom()
        {
            frame.render_widget(
                Paragraph::new(
                    Line::from(markdown::sanitize(validation)).fg(palette::warning_color()),
                ),
                Rect::new(body_area.x, next_y, body_area.width, 1),
            );
        }
    }

    fn render_choices(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            return;
        }
        let range = self.visible_choice_range(area.width, area.height);
        let options_len = self.arguments.questions[self.current].options.len();
        let mut y = area.y;
        for index in range {
            if y >= area.bottom() {
                break;
            }
            if self.editing_other && index == options_len {
                let used = self.render_other_editor(
                    frame,
                    Rect::new(area.x, y, area.width, area.bottom().saturating_sub(y)),
                );
                y = y.saturating_add(used);
                continue;
            }
            for line in self.choice_lines(index, area.width) {
                if y >= area.bottom() {
                    break;
                }
                frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
                y = y.saturating_add(1);
            }
        }
    }

    fn render_other_editor(&self, frame: &mut Frame<'_>, area: Rect) -> u16 {
        if area.is_empty() {
            return 0;
        }
        let question = &self.arguments.questions[self.current];
        let number = question.options.len() + 1;
        let number_width = self.number_width();
        let mut prefix = vec![
            Span::styled("› ", focused_style()),
            Span::styled(format!("{number:>number_width$}. "), secondary_style()),
        ];
        if question.multi_select {
            let selected = !self.editor.text().trim().is_empty();
            prefix.push(
                Span::styled(if selected { "[✓] " } else { "[ ] " }, focused_style()).not_dim(),
            );
        }
        frame.render_widget(
            Paragraph::new(Line::from(prefix)),
            Rect::new(area.x, area.y, area.width, 1),
        );

        let indent = u16::try_from(self.other_text_indent()).unwrap_or(u16::MAX);
        let text_x = area.x.saturating_add(indent).min(area.right());
        let text_width = area.right().saturating_sub(text_x).max(1);
        let editor_height = self
            .editor
            .desired_height(text_width)
            .clamp(1, MAX_EDITOR_HEIGHT)
            .min(area.height);
        let layout = self.editor.layout(text_width, editor_height);
        for (index, text) in layout
            .lines
            .iter()
            .take(usize::from(editor_height))
            .enumerate()
        {
            let Ok(index) = u16::try_from(index) else {
                break;
            };
            let line = if text.is_empty() && self.editor.text().is_empty() && index == 0 {
                Line::from("Type something.").style(secondary_style())
            } else {
                Line::from(markdown::sanitize(text)).style(selected_style())
            };
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(text_x, area.y.saturating_add(index), text_width, 1),
            );
        }
        let cursor_x = text_x
            .saturating_add(layout.cursor_column)
            .min(area.right().saturating_sub(1));
        let cursor_y = area
            .y
            .saturating_add(layout.cursor_row)
            .min(area.bottom().saturating_sub(1));
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
        editor_height
    }

    fn render_preview(&self, frame: &mut Frame<'_>, area: Rect, preview: &str, cwd: &Path) {
        if area.is_empty() {
            return;
        }
        let mut lines = markdown::render_markdown_agent_with_links_and_cwd(
            preview,
            Some(usize::from(area.width.max(1))),
            Some(cwd),
        )
        .into_iter()
        .map(|line| line.line)
        .collect::<Vec<_>>();
        let height = usize::from(area.height);
        if lines.len() > height {
            lines.truncate(height);
            if let Some(last) = lines.last_mut() {
                *last = Line::from("… preview truncated")
                    .style(secondary_style())
                    .italic();
            }
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn footer_hint(&self, width: u16) -> Line<'static> {
        let narrow = width < 42;
        let compact = width < 54;
        if self.editing_other {
            if narrow {
                Line::from("enter accept  ·  esc choices")
            } else if compact {
                Line::from("enter accept  ·  shift+enter newline  ·  esc choices")
            } else {
                Line::from("enter to accept  ·  shift+enter for newline  ·  esc to choices")
            }
        } else if self.review_active() {
            if narrow {
                Line::from("enter submit  ·  ← back  ·  esc cancel")
            } else if compact {
                Line::from("enter submit  ·  shift+tab/← back  ·  esc cancel")
            } else {
                Line::from("enter to submit  ·  shift+tab/← to go back  ·  esc to cancel")
            }
        } else if self.arguments.questions.len() == 1 {
            if narrow {
                Line::from("enter  ·  ↑/↓  ·  esc cancel")
            } else if compact {
                Line::from("enter select  ·  ↑/↓ navigate  ·  esc cancel")
            } else {
                Line::from("enter to select  ·  ↑/↓ to navigate  ·  esc to cancel")
            }
        } else if narrow {
            Line::from("enter  ·  tab/arrows  ·  esc cancel")
        } else if compact {
            Line::from("enter select  ·  tab/arrows  ·  esc cancel")
        } else {
            Line::from("enter to select  ·  tab/arrow keys to navigate  ·  esc to cancel")
        }
    }
}

fn compact_label(label: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(label) <= width {
        return label.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    format!("{}…", prefix_fitting_width(label, width - 1))
}

fn line_to_owned(line: Line<'_>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .into_iter()
            .map(|span| Span {
                style: span.style,
                content: Cow::Owned(span.content.into_owned()),
            })
            .collect(),
    }
}

fn concise_other(text: &str) -> String {
    const LIMIT: usize = 120;
    let sanitized = markdown::sanitize(text).replace('\n', " ");
    if sanitized.chars().count() <= LIMIT {
        sanitized
    } else {
        format!("{}…", sanitized.chars().take(LIMIT).collect::<String>())
    }
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &value[..boundary]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask_user_question::AskUserQuestion;
    use crate::ask_user_question::AskUserQuestionOption;
    use ratatui::style::Modifier;

    fn card() -> AskUserQuestionCard {
        AskUserQuestionCard::new(
            "call-1".to_string(),
            AskUserQuestionArgs {
                questions: vec![
                    AskUserQuestion {
                        question: "Which outcome should the pipeline produce?".to_string(),
                        header: "Outcome".to_string(),
                        options: vec![
                            AskUserQuestionOption {
                                label: "Small focused change".to_string(),
                                description: "Keep the implementation narrow and easy to review."
                                    .to_string(),
                                preview: None,
                                default_selected: false,
                            },
                            AskUserQuestionOption {
                                label: "Broader rewrite".to_string(),
                                description: "Replace the surrounding implementation as well."
                                    .to_string(),
                                preview: None,
                                default_selected: false,
                            },
                        ],
                        multi_select: false,
                    },
                    AskUserQuestion {
                        question: "Which quality targets matter?".to_string(),
                        header: "Criteria".to_string(),
                        options: vec![
                            AskUserQuestionOption {
                                label: "Simpler".to_string(),
                                description: "Prefer the smallest sufficient solution.".to_string(),
                                preview: None,
                                default_selected: true,
                            },
                            AskUserQuestionOption {
                                label: "Faster".to_string(),
                                description: "Keep interactive work responsive.".to_string(),
                                preview: None,
                                default_selected: true,
                            },
                        ],
                        multi_select: true,
                    },
                ],
            },
        )
    }

    fn edit_criteria_other(card: &mut AskUserQuestionCard, text: &str) {
        card.current = 1;
        card.answers[1].cursor = card.arguments.questions[1].options.len();
        card.begin_editing_other();
        card.insert_bounded(text);
    }

    #[test]
    fn focused_choice_uses_accent_without_reverse_video() {
        let card = card();
        let lines = card.choice_lines(0, 80);
        let label_line = &lines[0];

        assert_eq!(label_line.spans[0].style.fg, Some(palette::accent_color()));
        assert_eq!(label_line.spans[2].style.fg, Some(palette::accent_color()));
        assert!(
            label_line
                .spans
                .iter()
                .all(|span| !span.style.add_modifier.contains(Modifier::REVERSED))
        );
    }

    #[test]
    fn answer_blocks_have_a_blank_row_between_them() {
        let card = card();
        let lines = card.choice_lines(0, 80);

        assert!(lines.last().is_some_and(|line| line.spans.is_empty()));
    }

    #[test]
    fn active_tab_uses_accent_without_reverse_video() {
        let card = card();
        let tabs = card.tabs_line(100);
        let active = tabs
            .spans
            .iter()
            .find(|span| span.content.contains("Outcome"))
            .unwrap_or_else(|| panic!("active question tab should be rendered"));
        let text = tabs
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(active.style.fg, Some(palette::accent_color()));
        assert!(!active.style.add_modifier.contains(Modifier::REVERSED));
        assert!(text.contains("• Outcome"));
        assert!(text.contains("• Criteria"));
        assert!(text.contains("• Submit"));
        assert!(!text.contains('✓'));
        assert!(!text.contains('□'));
    }

    #[test]
    fn arrow_up_leaves_single_line_other_editor_and_moves_to_previous_choice() {
        let mut card = card();
        edit_criteria_other(&mut card, "Custom criterion");

        let action = card.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 80);

        assert_eq!(action, AskUserQuestionCardAction::None);
        assert!(!card.editing_other);
        assert_eq!(card.answers[1].cursor, 1);
        assert_eq!(card.answers[1].other, "Custom criterion");
    }

    #[test]
    fn arrow_down_leaves_single_line_other_editor_and_moves_to_submit() {
        let mut card = card();
        edit_criteria_other(&mut card, "Custom criterion");

        let action = card.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 80);

        assert_eq!(action, AskUserQuestionCardAction::None);
        assert!(!card.editing_other);
        assert_eq!(card.answers[1].cursor, 3);
        assert_eq!(card.answers[1].other, "Custom criterion");
    }

    #[test]
    fn arrows_still_move_inside_multiline_other_text_before_leaving() {
        let mut card = card();
        edit_criteria_other(&mut card, "First line\nSecond line");

        let first = card.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 80);
        assert_eq!(first, AskUserQuestionCardAction::None);
        assert!(card.editing_other);

        let second = card.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 80);
        assert_eq!(second, AskUserQuestionCardAction::None);
        assert!(!card.editing_other);
        assert_eq!(card.answers[1].cursor, 1);
        assert_eq!(card.answers[1].other, "First line\nSecond line");
    }

    #[test]
    fn escape_cancels_without_selecting_a_highlighted_option() {
        let mut card = card();

        let action = card.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 80);

        assert_eq!(
            action,
            AskUserQuestionCardAction::Cancel {
                call_id: "call-1".to_string(),
            }
        );
    }

    #[test]
    fn multi_select_defaults_are_submitted_when_the_user_accepts_them() {
        let mut card = card();
        assert_eq!(
            card.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 80),
            AskUserQuestionCardAction::None
        );

        let action = card.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL), 80);
        let AskUserQuestionCardAction::Submit { response, .. } = action else {
            panic!("completed answers should submit");
        };

        assert!(!response.cancelled);
        assert_eq!(response.answers.len(), 2);
        assert_eq!(
            response.answers[1].selected_options,
            vec!["Simpler".to_string(), "Faster".to_string()]
        );
        assert!(response.answers[1].free_text.is_none());
    }
}
