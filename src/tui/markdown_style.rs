use super::palette;
use ratatui::style::Color;
use ratatui::style::Style;

const TABLE_SEPARATOR_FG_ALPHA: f32 = 0.20;

/// Keep table structure visible without letting rules compete with cell content.
pub(super) fn table_separator_style() -> Style {
    let (Some(foreground), Some(background)) =
        (palette::default_foreground(), palette::default_background())
    else {
        return Style::default().dim();
    };
    if !supports_color::on_cached(supports_color::Stream::Stdout).is_some_and(|level| level.has_16m)
    {
        return Style::default().dim();
    }

    let (red, green, blue) = palette::blend(foreground, background, TABLE_SEPARATOR_FG_ALPHA);
    Style::default().fg(Color::Rgb(red, green, blue))
}
