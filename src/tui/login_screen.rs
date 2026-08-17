// Ported from OpenAI Codex 266c6920d9, chiefly
// codex-rs/tui/src/onboarding/auth.rs and onboarding_screen.rs. bettercodex
// retains only the ChatGPT browser and device-code modes used by its fixed
// runtime.

use super::markdown;
use super::palette;
use super::startup_art;
use super::terminal;
use super::terminal::TerminalSession;
use super::terminal_hyperlinks::HyperlinkLine;
use crate::login::LoginInstructions;
use crate::login::LoginMode;
use anyhow::Result;
use crossterm::event::Event;
use crossterm::event::EventStream;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use futures_util::StreamExt;
use ratatui::Frame;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::io;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinError;
use tokio::task::JoinHandle;

const MIN_ART_HEIGHT: u16 = 37;

pub(super) enum LoginScreenOutcome {
    Continue,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignInOption {
    ChatGpt,
    DeviceCode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LoginState {
    PickMode,
    Starting(LoginMode),
    Active(LoginInstructions),
    Success,
}

#[derive(Debug, PartialEq, Eq)]
enum KeyAction {
    None,
    Start(LoginMode),
    Cancel,
    Continue,
    Exit,
}

struct LoginScreen {
    highlighted: SignInOption,
    state: LoginState,
    error: Option<String>,
}

impl Default for LoginScreen {
    fn default() -> Self {
        Self {
            highlighted: SignInOption::ChatGpt,
            state: LoginState::PickMode,
            error: None,
        }
    }
}

impl LoginScreen {
    fn begin(&mut self, mode: LoginMode) {
        self.error = None;
        self.state = LoginState::Starting(mode);
    }

    fn show_instructions(&mut self, instructions: LoginInstructions) {
        self.state = LoginState::Active(instructions);
    }

    fn show_error(&mut self, error: String) {
        self.state = LoginState::PickMode;
        self.error = Some(error);
    }

    fn cancel(&mut self) {
        self.state = LoginState::PickMode;
        self.error = None;
    }

    fn succeed(&mut self) {
        self.state = LoginState::Success;
        self.error = None;
    }

    fn handle_key(&mut self, key: KeyEvent) -> KeyAction {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return KeyAction::None;
        }
        if is_quit_key(key) {
            return KeyAction::Exit;
        }

        match &self.state {
            LoginState::PickMode => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.highlighted = SignInOption::ChatGpt;
                    KeyAction::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.highlighted = SignInOption::DeviceCode;
                    KeyAction::None
                }
                KeyCode::Char('1') => KeyAction::Start(LoginMode::Browser),
                KeyCode::Char('2') => KeyAction::Start(LoginMode::DeviceCode),
                KeyCode::Enter => KeyAction::Start(self.highlighted.login_mode()),
                KeyCode::Esc => KeyAction::Exit,
                _ => KeyAction::None,
            },
            LoginState::Starting(_) | LoginState::Active(_) => {
                if key.code == KeyCode::Esc {
                    KeyAction::Cancel
                } else {
                    KeyAction::None
                }
            }
            LoginState::Success => {
                if key.code == KeyCode::Enter {
                    KeyAction::Continue
                } else {
                    KeyAction::None
                }
            }
        }
    }

    fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let lines = self.lines(area.width, area.height);
        terminal::render_transcript_lines(&lines, area, /*scroll_rows*/ 0, frame.buffer_mut());
    }

    fn lines(&self, width: u16, height: u16) -> Vec<HyperlinkLine> {
        let mut lines = Vec::new();
        if height >= MIN_ART_HEIGHT {
            lines.extend(
                startup_art::lines(width, height)
                    .into_iter()
                    .map(HyperlinkLine::from),
            );
            if !lines.is_empty() {
                lines.push("".into());
            }
        }
        lines.push(
            Line::from(vec![
                "  Welcome to ".into(),
                "bettercodex".bold(),
                ", a focused Codex CLI".into(),
            ])
            .into(),
        );
        lines.push("".into());

        match &self.state {
            LoginState::PickMode => self.push_pick_mode(&mut lines),
            LoginState::Starting(mode) => {
                let message = match mode {
                    LoginMode::Browser => "  Starting browser login...",
                    LoginMode::DeviceCode => "  Preparing device code login...",
                };
                lines.push(Line::from(message).into());
                lines.push("".into());
                lines.push(Line::from("  Press Esc to cancel").dim().into());
            }
            LoginState::Active(LoginInstructions::Browser { auth_url, .. }) => {
                lines.push("  Finish signing in via your browser".into());
                lines.push("".into());
                lines.push(
                    "  If the link doesn't open automatically, open the following link to authenticate:"
                        .into(),
                );
                lines.push("".into());
                lines.push(url_line(auth_url));
                lines.push("".into());
                lines.push(
                    Line::from(vec![
                        "  On a remote or headless machine? Press ".into(),
                        Span::styled("Esc", palette::accent_text_style()),
                        " and choose ".into(),
                        Span::styled("Sign in with Device Code", palette::accent_text_style()),
                        ".".into(),
                    ])
                    .into(),
                );
                lines.push("".into());
                lines.push(Line::from("  Press Esc to cancel").dim().into());
            }
            LoginState::Active(LoginInstructions::DeviceCode {
                verification_url,
                user_code,
            }) => {
                lines.push("  Finish signing in via your browser".into());
                lines.push("".into());
                lines.push("  1. Open this link in your browser and sign in".into());
                lines.push("".into());
                lines.push(url_line(verification_url));
                lines.push("".into());
                lines.push(
                    "  2. Enter this one-time code after you are signed in (expires in 15 minutes)"
                        .into(),
                );
                lines.push("".into());
                lines.push(
                    Line::from(vec![
                        "  ".into(),
                        Span::styled(user_code.to_string(), palette::accent_style()),
                    ])
                    .into(),
                );
                lines.push("".into());
                lines.push(
                    Line::from("  Continue only if you started this login in bettercodex. If a website or another person gave you this code, cancel.")
                        .dim()
                        .into(),
                );
                lines.push("".into());
                lines.push(Line::from("  Press Esc to cancel").dim().into());
            }
            LoginState::Success => self.push_success(&mut lines),
        }
        lines
    }

    fn push_pick_mode(&self, lines: &mut Vec<HyperlinkLine>) {
        lines.push("  Sign in with ChatGPT to use bettercodex as part of your paid plan".into());
        lines.push("".into());
        lines.extend(option_lines(
            1,
            "Sign in with ChatGPT",
            "Usage included with Plus, Pro, Business, and Enterprise plans",
            self.highlighted == SignInOption::ChatGpt,
        ));
        lines.push("".into());
        lines.extend(option_lines(
            2,
            "Sign in with Device Code",
            "Sign in from another device with a one-time code",
            self.highlighted == SignInOption::DeviceCode,
        ));
        lines.push("".into());
        lines.push(Line::from("  Press Enter to continue").dim().into());

        if let Some(error) = &self.error {
            lines.push("".into());
            let error = markdown::sanitize(error);
            lines.extend(
                error
                    .lines()
                    .map(|line| Line::from(format!("  {line}")).red().into()),
            );
        }
    }

    fn push_success(&self, lines: &mut Vec<HyperlinkLine>) {
        lines.push(
            Line::from("✓ Signed in with your ChatGPT account")
                .fg(Color::Green)
                .into(),
        );
        lines.push("".into());
        lines.push("  Before you start:".into());
        lines.push("".into());
        lines.push("  Commands and patches run with your user permissions".into());
        lines.push(
            Line::from("  Review the code bettercodex writes and commands it runs")
                .dim()
                .into(),
        );
        lines.push("".into());
        lines.push("  Powered by your ChatGPT account".into());
        lines.push("".into());
        lines.push(
            Line::from(Span::styled(
                "  Press Enter to continue",
                palette::accent_text_style(),
            ))
            .into(),
        );
    }
}

impl SignInOption {
    fn login_mode(self) -> LoginMode {
        match self {
            Self::ChatGpt => LoginMode::Browser,
            Self::DeviceCode => LoginMode::DeviceCode,
        }
    }
}

fn option_lines(
    index: usize,
    label: &str,
    description: &str,
    selected: bool,
) -> [HyperlinkLine; 2] {
    if selected {
        [
            Line::from(vec![
                Span::styled(
                    format!("> {index}. "),
                    palette::accent_text_style().add_modifier(Modifier::DIM),
                ),
                Span::styled(label.to_string(), palette::accent_text_style()),
            ])
            .into(),
            Line::from(Span::styled(
                format!("     {description}"),
                palette::soft_accent_style(),
            ))
            .into(),
        ]
    } else {
        [
            Line::from(format!("  {index}. {label}")).into(),
            Line::from(format!("     {description}"))
                .style(Style::default().add_modifier(Modifier::DIM))
                .into(),
        ]
    }
}

fn url_line(url: &str) -> HyperlinkLine {
    let mut line = HyperlinkLine::new(Line::from("  "));
    line.push_span(
        Span::styled(url.to_string(), palette::accent_link_style()),
        Some(url),
    );
    line
}

fn is_quit_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'd')))
}

struct ActiveLogin {
    task: JoinHandle<Result<()>>,
    updates: UnboundedReceiver<LoginInstructions>,
    updates_open: bool,
}

impl ActiveLogin {
    fn start(mode: LoginMode) -> Self {
        let (updates_tx, updates) = unbounded_channel();
        let task = tokio::spawn(crate::login::login_with_updates(mode, updates_tx));
        Self {
            task,
            updates,
            updates_open: true,
        }
    }

    async fn cancel(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for ActiveLogin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum NextEvent {
    Terminal(Option<io::Result<Event>>),
    Instructions(Option<LoginInstructions>),
    LoginFinished(std::result::Result<Result<()>, JoinError>),
}

pub(super) async fn run(session: &mut TerminalSession) -> Result<LoginScreenOutcome> {
    let mut screen = LoginScreen::default();
    let mut events = EventStream::new();
    let mut active_login: Option<ActiveLogin> = None;
    redraw(session, &screen)?;

    loop {
        let next = if let Some(active) = active_login.as_mut() {
            let updates_open = active.updates_open;
            tokio::select! {
                event = events.next() => NextEvent::Terminal(event),
                instructions = active.updates.recv(), if updates_open => {
                    NextEvent::Instructions(instructions)
                }
                result = &mut active.task => NextEvent::LoginFinished(result),
            }
        } else {
            NextEvent::Terminal(events.next().await)
        };

        match next {
            NextEvent::Terminal(Some(Ok(Event::Key(key)))) => match screen.handle_key(key) {
                KeyAction::None => {}
                KeyAction::Start(mode) => {
                    screen.begin(mode);
                    active_login = Some(ActiveLogin::start(mode));
                }
                KeyAction::Cancel => {
                    if let Some(active) = active_login.take() {
                        active.cancel().await;
                    }
                    screen.cancel();
                }
                KeyAction::Continue => return Ok(LoginScreenOutcome::Continue),
                KeyAction::Exit => {
                    if let Some(active) = active_login.take() {
                        active.cancel().await;
                    }
                    return Ok(LoginScreenOutcome::Exit);
                }
            },
            NextEvent::Terminal(Some(Ok(_))) => {}
            NextEvent::Terminal(Some(Err(error))) => return Err(error.into()),
            NextEvent::Terminal(None) => {
                if let Some(active) = active_login.take() {
                    active.cancel().await;
                }
                return Ok(LoginScreenOutcome::Exit);
            }
            NextEvent::Instructions(Some(instructions)) => {
                screen.show_instructions(instructions);
            }
            NextEvent::Instructions(None) => {
                if let Some(active) = active_login.as_mut() {
                    active.updates_open = false;
                }
            }
            NextEvent::LoginFinished(result) => {
                active_login = None;
                match result {
                    Ok(Ok(())) => screen.succeed(),
                    Ok(Err(error)) => screen.show_error(format!("{error:#}")),
                    Err(error) => {
                        screen.show_error(format!("login task stopped unexpectedly: {error}"));
                    }
                }
            }
        }
        redraw(session, &screen)?;
    }
}

fn redraw(session: &mut TerminalSession, screen: &LoginScreen) -> Result<()> {
    let terminal = session.terminal_mut();
    // Login is event-driven rather than animation-driven, so sample here to keep this standalone
    // surface resize-safe without restoring backend queries to the main redraw loop.
    terminal.refresh_screen_size()?;
    let height = terminal.screen_size().height;
    terminal.draw(height, |frame| screen.render(frame))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn render(screen: &LoginScreen) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| screen.render(frame)).unwrap();
        render_buffer(terminal.backend().buffer())
    }

    fn render_buffer(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn signed_out_screen_renders_upstream_chatgpt_choices() {
        let rendered = render(&LoginScreen::default());

        assert!(rendered.contains("Welcome to bettercodex"), "{rendered}");
        assert!(rendered.contains("> 1. Sign in with ChatGPT"), "{rendered}");
        assert!(
            rendered.contains("2. Sign in with Device Code"),
            "{rendered}"
        );
        assert!(rendered.contains("Press Enter to continue"), "{rendered}");
        assert!(!rendered.contains("API key"), "{rendered}");
    }

    #[test]
    fn device_code_screen_keeps_login_material_visible() {
        let screen = LoginScreen {
            state: LoginState::Active(LoginInstructions::DeviceCode {
                verification_url: "https://auth.openai.com/codex/device".to_string(),
                user_code: "ABCD-EFGH".to_string(),
            }),
            ..LoginScreen::default()
        };
        let rendered = render(&screen);

        assert!(
            rendered.contains("https://auth.openai.com/codex/device"),
            "{rendered}"
        );
        assert!(rendered.contains("ABCD-EFGH"), "{rendered}");
        assert!(rendered.contains("expires in 15 minutes"), "{rendered}");
    }

    #[test]
    fn keyboard_navigation_selects_both_login_modes_and_can_exit() {
        let mut screen = LoginScreen::default();
        assert_eq!(
            screen.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            KeyAction::None
        );
        assert_eq!(
            screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            KeyAction::Start(LoginMode::DeviceCode)
        );
        assert_eq!(
            screen.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)),
            KeyAction::Start(LoginMode::Browser)
        );
        assert_eq!(
            screen.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            KeyAction::Exit
        );
    }
}
