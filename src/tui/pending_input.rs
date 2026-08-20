use super::wrapping::RtOptions;
use super::wrapping::adaptive_wrap_line;
use crate::events::SteerId;
use crate::input::UserPrompt;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::collections::VecDeque;

const MAX_VISIBLE_MESSAGES_PER_SECTION: usize = 3;
const MAX_PREVIEW_ROWS: usize = 3;
const MAX_PREVIEW_CHARS: usize = 240;

#[derive(Debug)]
struct PendingSteer {
    id: SteerId,
    prompt: UserPrompt,
}

#[derive(Debug, Default)]
pub(super) struct PendingInput {
    steers: VecDeque<PendingSteer>,
    follow_ups: VecDeque<UserPrompt>,
}

impl PendingInput {
    pub(super) fn add_steer(&mut self, id: SteerId, prompt: UserPrompt) {
        self.steers.push_back(PendingSteer { id, prompt });
    }

    pub(super) fn commit_steer(&mut self, id: SteerId) -> Option<UserPrompt> {
        let index = self.steers.iter().position(|steer| steer.id == id)?;
        self.steers.remove(index).map(|steer| steer.prompt)
    }

    pub(super) fn queue_follow_up(&mut self, prompt: UserPrompt) {
        self.follow_ups.push_back(prompt);
    }

    pub(super) fn pop_next_follow_up(&mut self) -> Option<UserPrompt> {
        self.follow_ups.pop_front()
    }

    pub(super) fn pop_latest_follow_up(&mut self) -> Option<UserPrompt> {
        self.follow_ups.pop_back()
    }

    pub(super) fn has_steers(&self) -> bool {
        !self.steers.is_empty()
    }

    pub(super) fn has_follow_ups(&self) -> bool {
        !self.follow_ups.is_empty()
    }

    pub(super) fn steer_count(&self) -> usize {
        self.steers.len()
    }

    pub(super) fn follow_up_count(&self) -> usize {
        self.follow_ups.len()
    }

    pub(super) fn take_steers(&mut self) -> Vec<(SteerId, UserPrompt)> {
        self.steers
            .drain(..)
            .map(|steer| (steer.id, steer.prompt))
            .collect()
    }

    pub(super) fn take_all(&mut self) -> Vec<UserPrompt> {
        let mut prompts = self
            .take_steers()
            .into_iter()
            .map(|(_, prompt)| prompt)
            .collect::<Vec<_>>();
        prompts.extend(self.follow_ups.drain(..));
        prompts
    }

    pub(super) fn clear(&mut self) {
        self.steers.clear();
        self.follow_ups.clear();
    }

    pub(super) fn lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if !self.steers.is_empty() {
            lines.push(Line::from(vec![
                "• ".dim(),
                "Steering after the current model step".into(),
                " (Esc sends now)".dim(),
            ]));
            append_messages(
                &mut lines,
                self.steers.iter().map(|steer| steer.prompt.as_str()),
                self.steers.len(),
                width,
                false,
            );
        }
        if !self.follow_ups.is_empty() {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::from(vec![
                "• ".dim(),
                "Queued follow-up inputs".into(),
            ]));
            append_messages(
                &mut lines,
                self.follow_ups.iter().map(UserPrompt::as_str),
                self.follow_ups.len(),
                width,
                true,
            );
            lines.push(Line::from("    Alt+Up / Shift+Left edit last queued message").dim());
        }
        lines
    }
}

fn append_messages<'a>(
    lines: &mut Vec<Line<'static>>,
    messages: impl Iterator<Item = &'a str>,
    count: usize,
    width: u16,
    italic: bool,
) {
    for message in messages.take(MAX_VISIBLE_MESSAGES_PER_SECTION) {
        append_message_preview(lines, message, width, italic);
    }
    let hidden = count.saturating_sub(MAX_VISIBLE_MESSAGES_PER_SECTION);
    if hidden > 0 {
        lines.push(Line::from(format!("    … {hidden} more")).dim());
    }
}

fn append_message_preview(lines: &mut Vec<Line<'static>>, message: &str, width: u16, italic: bool) {
    let preview = Line::from(bounded_preview(message));
    let wrapped = adaptive_wrap_line(
        &preview,
        RtOptions::new(usize::from(width))
            .initial_indent(Line::from("  ↳ "))
            .subsequent_indent(Line::from("    ")),
    );
    let wrapped_len = wrapped.len();
    for wrapped_line in wrapped.into_iter().take(MAX_PREVIEW_ROWS) {
        let content = wrapped_line
            .spans
            .into_iter()
            .map(|span| span.content)
            .collect::<String>();
        let line = Line::from(content);
        lines.push(if italic {
            line.italic().dim()
        } else {
            line.dim()
        });
    }
    if wrapped_len > MAX_PREVIEW_ROWS {
        let line = Line::from("    …");
        lines.push(if italic {
            line.italic().dim()
        } else {
            line.dim()
        });
    }
}

fn bounded_preview(prompt: &str) -> String {
    let mut preview = String::new();
    let mut preview_chars = 0_usize;
    let mut previous_was_space = false;
    let mut truncated = false;
    for character in prompt.chars() {
        let character = if character.is_whitespace() {
            if previous_was_space {
                continue;
            }
            previous_was_space = true;
            ' '
        } else {
            previous_was_space = false;
            character
        };
        if preview_chars == MAX_PREVIEW_CHARS {
            truncated = true;
            break;
        }
        preview.push(character);
        preview_chars += 1;
    }
    if truncated {
        preview.push('…');
    }
    preview.trim().to_string()
}
