use super::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

fn rendered_rows(input: &PendingInput, width: u16) -> Vec<String> {
    let lines = input.lines(width);
    let height = u16::try_from(lines.len()).unwrap();
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(lines).render(area, &mut buffer);

    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn long_url_like_message_stays_on_one_clipped_preview_row() {
    let mut input = PendingInput::default();
    input.queue_follow_up(UserPrompt::text(
        "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/session_id=abc123def456ghi789",
    ));

    let rows = rendered_rows(&input, 36);

    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows[1].starts_with("  ↳ example.test/api/"), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains('…')), "{rows:?}");
}

#[test]
fn prose_preview_uses_three_rows_then_an_overflow_marker() {
    let mut input = PendingInput::default();
    input.queue_follow_up(UserPrompt::text(
        "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen",
    ));

    let rows = rendered_rows(&input, 20);

    assert_eq!(rows.len(), 6, "{rows:?}");
    assert!(rows[1].starts_with("  ↳ "), "{rows:?}");
    assert!(rows[2].starts_with("    "), "{rows:?}");
    assert!(rows[3].starts_with("    "), "{rows:?}");
    assert_eq!(rows[4].trim(), "…", "{rows:?}");
}
