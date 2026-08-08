use super::startup_art;
use ratatui::style::Color;
use unicode_width::UnicodeWidthStr;

#[test]
fn artwork_has_the_declared_terminal_footprint() {
    let lines = startup_art::lines(
        startup_art::MIN_TERMINAL_WIDTH,
        startup_art::MIN_TERMINAL_HEIGHT,
    );

    assert_eq!(lines.len(), usize::from(startup_art::ART_HEIGHT));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .flat_map(|span| span.content.chars())
            .any(|character| ('\u{2800}'..='\u{28ff}').contains(&character))
    }));
    assert!(lines.iter().all(|line| {
        line.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum::<usize>()
            == usize::from(startup_art::ART_WIDTH)
    }));
}

#[test]
fn artwork_yields_to_small_terminal_layouts() {
    assert!(
        startup_art::lines(
            startup_art::MIN_TERMINAL_WIDTH - 1,
            startup_art::MIN_TERMINAL_HEIGHT,
        )
        .is_empty()
    );
    assert!(
        startup_art::lines(
            startup_art::MIN_TERMINAL_WIDTH,
            startup_art::MIN_TERMINAL_HEIGHT - 1,
        )
        .is_empty()
    );
}

#[test]
fn artwork_color_stays_legible_on_light_and_dark_backgrounds() {
    assert_eq!(
        startup_art::style_for_background(Some((18, 18, 18))).fg,
        Some(Color::Rgb(167, 199, 231))
    );
    assert_eq!(
        startup_art::style_for_background(Some((245, 245, 245))).fg,
        Some(Color::Rgb(36, 79, 112))
    );
    assert_eq!(
        startup_art::style_for_background(None).fg,
        Some(Color::Rgb(167, 199, 231))
    );
}
