use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[test]
fn renders_a_compact_catalogue_attached_to_the_left_edge() {
    let catalogue = ToolCatalogueView::new();
    let backend = TestBackend::new(88, VIEWPORT_HEIGHT);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| catalogue.render(frame, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();

    assert_eq!(buffer[(0, 0)].symbol(), "┌");
    assert_eq!(buffer[(PREFERRED_WIDTH - 1, 0)].symbol(), "┐");
    assert_eq!(buffer[(PREFERRED_WIDTH, 0)].symbol(), " ");
    assert_eq!(
        render_buffer(buffer),
        [
            "┌ Tools ───────────────────────┐",
            "│Request tools                 │",
            "│  ├─ ● exec                   │",
            "│  └─ ● wait                   │",
            "│                              │",
            "│Inside exec                   │",
            "│  ├─ ● apply_patch            │",
            "│  ├─ ● exec_command           │",
            "│  ├─ ● log_papercut           │",
            "│  ├─ ● update_plan            │",
            "│  ├─ ● view_image             │",
            "│  ├─ ● write_stdin            │",
            "│  └─ ● web__run               │",
            "└──────────────────────────────┘",
        ]
        .join("\n")
    );
}

#[test]
fn only_escape_and_q_close_the_catalogue() {
    let catalogue = ToolCatalogueView::new();
    for key in [KeyCode::Down, KeyCode::Enter, KeyCode::Char('x')] {
        assert_eq!(catalogue.handle_key(key), CatalogueAction::StayOpen);
    }
    for key in [KeyCode::Esc, KeyCode::Char('q')] {
        assert_eq!(catalogue.handle_key(key), CatalogueAction::Close);
    }
}

#[test]
fn rendering_is_bounded_for_tiny_terminals() {
    for (width, height) in [(1, 1), (2, 3), (7, 6), (20, 8)] {
        let catalogue = ToolCatalogueView::new();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| catalogue.render(frame, frame.area()))
            .unwrap();
    }
}

fn render_buffer(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
