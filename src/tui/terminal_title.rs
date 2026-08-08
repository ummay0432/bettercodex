//! Sanitized OSC 0 activity titles for the interactive TUI.

use crossterm::Command;
use crossterm::execute;
use std::fmt;
use std::io;
use std::io::IsTerminal;
use std::io::stdout;
use std::time::Duration;
use std::time::Instant;

const MAX_TERMINAL_TITLE_CHARS: usize = 240;
const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalTitleState {
    Idle,
    Working,
    ActionRequired,
}

pub(super) struct TerminalTitle {
    animation_origin: Instant,
    last_state: Option<TerminalTitleState>,
    last_title: Option<String>,
}

impl TerminalTitle {
    pub(super) fn new() -> Self {
        Self {
            animation_origin: Instant::now(),
            last_state: None,
            last_title: None,
        }
    }

    pub(super) fn refresh(&mut self, state: TerminalTitleState) -> io::Result<()> {
        if state == TerminalTitleState::Working
            && self.last_state != Some(TerminalTitleState::Working)
        {
            self.animation_origin = Instant::now();
        }
        self.last_state = Some(state);
        let title = match state {
            TerminalTitleState::Idle => "bettercodex".to_string(),
            TerminalTitleState::Working => {
                let elapsed = Instant::now().saturating_duration_since(self.animation_origin);
                let frame = (elapsed.as_millis() / SPINNER_INTERVAL.as_millis()) as usize;
                format!(
                    "{} bettercodex",
                    SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
                )
            }
            TerminalTitleState::ActionRequired => "Action Required".to_string(),
        };
        if self.last_title.as_deref() == Some(title.as_str()) {
            return Ok(());
        }
        if set_terminal_title(&title)? {
            self.last_title = Some(title);
        }
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        if self.last_title.take().is_none() || !stdout().is_terminal() {
            return Ok(());
        }
        execute!(stdout(), SetWindowTitle(String::new()))
    }
}

impl Drop for TerminalTitle {
    fn drop(&mut self) {
        let _ = self.clear();
    }
}

fn set_terminal_title(title: &str) -> io::Result<bool> {
    if !stdout().is_terminal() {
        return Ok(true);
    }
    let title = sanitize_terminal_title(title);
    if title.is_empty() {
        return Ok(false);
    }
    execute!(stdout(), SetWindowTitle(title))?;
    Ok(true)
}

#[derive(Debug, Clone)]
struct SetWindowTitle(String);

impl Command for SetWindowTitle {
    fn write_ansi(&self, output: &mut impl fmt::Write) -> fmt::Result {
        write!(output, "\x1b]0;{}\x07", self.0)
    }
}

fn sanitize_terminal_title(title: &str) -> String {
    let mut sanitized = String::new();
    let mut chars_written = 0;
    let mut pending_space = false;
    for character in title.chars() {
        if character.is_whitespace() {
            pending_space = !sanitized.is_empty();
            continue;
        }
        if is_disallowed_terminal_title_char(character) {
            continue;
        }
        if pending_space && MAX_TERMINAL_TITLE_CHARS.saturating_sub(chars_written) > 1 {
            sanitized.push(' ');
            chars_written += 1;
        }
        pending_space = false;
        if chars_written >= MAX_TERMINAL_TITLE_CHARS {
            break;
        }
        sanitized.push(character);
        chars_written += 1;
    }
    sanitized
}

fn is_disallowed_terminal_title_char(character: char) -> bool {
    character.is_control()
        || matches!(
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
