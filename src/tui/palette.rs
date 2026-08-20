use ratatui::style::Color;
use ratatui::style::Style;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TerminalColors {
    pub(super) foreground: (u8, u8, u8),
    pub(super) background: (u8, u8, u8),
}

const LIGHT_BACKGROUND_ACCENT: (u8, u8, u8) = (0, 95, 135);
const LIGHT_BACKGROUND_WARNING: (u8, u8, u8) = (135, 75, 0);

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

pub(super) fn accent_color_for(background: Option<(u8, u8, u8)>) -> Color {
    if background.is_some_and(is_light) {
        Color::Rgb(
            LIGHT_BACKGROUND_ACCENT.0,
            LIGHT_BACKGROUND_ACCENT.1,
            LIGHT_BACKGROUND_ACCENT.2,
        )
    } else {
        Color::Cyan
    }
}

pub(super) fn accent_color() -> Color {
    accent_color_for(default_background())
}

pub(super) fn warning_color_for(background: Option<(u8, u8, u8)>) -> Color {
    if background.is_some_and(is_light) {
        Color::Rgb(
            LIGHT_BACKGROUND_WARNING.0,
            LIGHT_BACKGROUND_WARNING.1,
            LIGHT_BACKGROUND_WARNING.2,
        )
    } else {
        Color::Yellow
    }
}

pub(super) fn warning_color() -> Color {
    warning_color_for(default_background())
}

pub(super) fn accent_text_style() -> Style {
    Style::default().fg(accent_color())
}

pub(super) fn soft_accent_style() -> Style {
    accent_text_style().dim()
}

pub(super) fn accent_link_style() -> Style {
    accent_text_style().underlined()
}

/// Shared style for active or selected TUI controls.
pub(super) fn accent_style() -> Style {
    accent_text_style().bold()
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
