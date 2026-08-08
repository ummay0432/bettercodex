use super::palette;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

// Verbatim Pi Black raven artwork, used under its MIT license. See
// `startup_art.LICENSE` and https://github.com/paoloanzn/pi-black.
pub(super) const ART_WIDTH: u16 = 28;
pub(super) const ART_HEIGHT: u16 = 15;
pub(super) const MIN_TERMINAL_WIDTH: u16 = ART_WIDTH + 2;
pub(super) const MIN_TERMINAL_HEIGHT: u16 = ART_HEIGHT + 13;

const STARTUP_ART: &str = include_str!("startup_art.txt");
const DARK_BACKGROUND_COLOR: Color = Color::Rgb(167, 199, 231);
const LIGHT_BACKGROUND_COLOR: Color = Color::Rgb(36, 79, 112);

pub(super) fn lines(terminal_width: u16, terminal_height: u16) -> Vec<Line<'static>> {
    if terminal_width < MIN_TERMINAL_WIDTH || terminal_height < MIN_TERMINAL_HEIGHT {
        return Vec::new();
    }

    let style = style_for_background(palette::default_background());
    STARTUP_ART
        .lines()
        .map(|row| Line::from(Span::styled(row, style)))
        .collect()
}

pub(super) fn style_for_background(background: Option<(u8, u8, u8)>) -> Style {
    let color = if background.is_some_and(palette::is_light) {
        LIGHT_BACKGROUND_COLOR
    } else {
        DARK_BACKGROUND_COLOR
    };
    Style::default().fg(color)
}
