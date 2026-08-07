//! OSC 9/BEL completion notifications for an unfocused terminal.

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
        if character.is_control() || is_invisible_format(character) {
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

fn is_invisible_format(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{E0100}'..='\u{E01EF}'
    )
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

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(io::Error::other("OSC notifications require ANSI output"))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug)]
struct PostBelNotification;

impl Command for PostBelNotification {
    fn write_ansi(&self, output: &mut impl fmt::Write) -> fmt::Result {
        output.write_str("\x07")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(io::Error::other("BEL notifications require ANSI output"))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_preview_is_single_line_bounded_and_safe() {
        let message = format!(
            " Done\n\u{202e}\x1b{} ",
            "x".repeat(MAX_NOTIFICATION_CHARS + 20)
        );
        let sanitized = sanitize_notification(&message);
        assert!(sanitized.starts_with("Done "));
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\x1b'));
        assert!(!sanitized.contains('\u{202e}'));
        assert_eq!(sanitized.chars().count(), MAX_NOTIFICATION_CHARS);
    }

    #[test]
    fn osc9_supports_plain_and_tmux_output() {
        let mut plain = String::new();
        PostOsc9Notification {
            message: "done".to_string(),
            tmux: false,
        }
        .write_ansi(&mut plain)
        .unwrap();
        assert_eq!(plain, "\x1b]9;done\x07");

        let mut tmux = String::new();
        PostOsc9Notification {
            message: "done".to_string(),
            tmux: true,
        }
        .write_ansi(&mut tmux)
        .unwrap();
        assert_eq!(tmux, "\x1bPtmux;\x1b\x1b]9;done\x07\x1b\\");
    }

    #[test]
    fn bel_output_is_one_control_byte() {
        let mut encoded = String::new();
        PostBelNotification.write_ansi(&mut encoded).unwrap();
        assert_eq!(encoded, "\x07");
    }
}
