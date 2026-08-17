//! OSC 9/BEL completion notifications for an unfocused terminal.

use crate::text::is_invisible_format_character;
use crossterm::Command;
use crossterm::execute;
use std::fmt;
use std::io;
use std::io::stdout;

const MAX_NOTIFICATION_CHARS: usize = 200;

pub(super) struct Notifier {
    backend: Backend,
}

enum Backend {
    Osc9 { tmux: bool },
    Bel,
}

impl Notifier {
    pub(super) fn detect() -> Self {
        let backend = if terminal_supports_osc9() {
            Backend::Osc9 {
                tmux: crate::managed_session::is_tmux_active(),
            }
        } else {
            Backend::Bel
        };
        Self { backend }
    }

    pub(super) fn notify_turn_complete(&mut self, final_answer: &str) -> io::Result<()> {
        let preview = sanitize_notification(final_answer);
        let message = if preview.is_empty() {
            "bettercodex turn complete".to_string()
        } else {
            preview
        };
        match self.backend {
            Backend::Osc9 { tmux } => execute!(stdout(), PostOsc9Notification { message, tmux }),
            Backend::Bel => execute!(stdout(), PostBelNotification),
        }
    }
}

fn terminal_supports_osc9() -> bool {
    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let term_program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    term.contains("kitty")
        || term_program.contains("ghostty")
        || term_program.contains("iterm")
        || term_program.contains("kitty")
        || term_program.contains("warp")
        || term_program.contains("wezterm")
        || std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
        || std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var_os("WEZTERM_PANE").is_some()
}

fn sanitize_notification(message: &str) -> String {
    let mut sanitized = String::new();
    let mut chars_written = 0;
    let mut pending_space = false;
    for character in message.chars() {
        if character.is_whitespace() {
            pending_space = !sanitized.is_empty();
            continue;
        }
        if character.is_control() || is_invisible_format_character(character) {
            continue;
        }
        if pending_space && chars_written + 1 < MAX_NOTIFICATION_CHARS {
            sanitized.push(' ');
            chars_written += 1;
        }
        pending_space = false;
        if chars_written >= MAX_NOTIFICATION_CHARS {
            break;
        }
        sanitized.push(character);
        chars_written += 1;
    }
    sanitized
}

#[derive(Debug)]
struct PostOsc9Notification {
    message: String,
    tmux: bool,
}

impl Command for PostOsc9Notification {
    fn write_ansi(&self, output: &mut impl fmt::Write) -> fmt::Result {
        if self.tmux {
            let message = self.message.replace('\u{1b}', "\u{1b}\u{1b}");
            write!(output, "\x1bPtmux;\x1b\x1b]9;{message}\x07\x1b\\")
        } else {
            write!(output, "\x1b]9;{}\x07", self.message)
        }
    }
}

#[derive(Debug)]
struct PostBelNotification;

impl Command for PostBelNotification {
    fn write_ansi(&self, output: &mut impl fmt::Write) -> fmt::Result {
        output.write_str("\x07")
    }
}
