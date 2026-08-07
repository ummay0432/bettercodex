use ratatui::style::Color;
use ratatui::style::Style;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TerminalColors {
    pub(super) foreground: (u8, u8, u8),
    pub(super) background: (u8, u8, u8),
}

static TERMINAL_FOREGROUND: OnceLock<(u8, u8, u8)> = OnceLock::new();
static TERMINAL_BACKGROUND: OnceLock<(u8, u8, u8)> = OnceLock::new();

pub(super) fn set_terminal_colors(
    foreground: Option<(u8, u8, u8)>,
    background: Option<(u8, u8, u8)>,
) {
    if let Some(foreground) = foreground {
        let _ = TERMINAL_FOREGROUND.set(foreground);
    }
    if let Some(background) = background {
        let _ = TERMINAL_BACKGROUND.set(background);
    }
}

pub(super) fn terminal_colors() -> Option<TerminalColors> {
    Some(TerminalColors {
        foreground: default_foreground()?,
        background: default_background()?,
    })
}

pub(super) fn default_foreground() -> Option<(u8, u8, u8)> {
    TERMINAL_FOREGROUND.get().copied()
}

pub(super) fn default_background() -> Option<(u8, u8, u8)> {
    TERMINAL_BACKGROUND.get().copied()
}

pub(super) fn is_light((red, green, blue): (u8, u8, u8)) -> bool {
    0.299 * red as f32 + 0.587 * green as f32 + 0.114 * blue as f32 > 128.0
}

/// Codex's shared style for the active row in command menus.
pub(super) fn accent_style() -> Style {
    if default_background().is_some_and(is_light) {
        Style::default().fg(Color::Rgb(0, 95, 135)).bold()
    } else {
        Style::default().fg(Color::Cyan).bold()
    }
}

pub(super) fn blend(
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
    alpha: f32,
) -> (u8, u8, u8) {
    (
        (foreground.0 as f32 * alpha + background.0 as f32 * (1.0 - alpha)) as u8,
        (foreground.1 as f32 * alpha + background.1 as f32 * (1.0 - alpha)) as u8,
        (foreground.2 as f32 * alpha + background.2 as f32 * (1.0 - alpha)) as u8,
    )
}
