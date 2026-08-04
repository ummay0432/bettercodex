use super::*;
use crate::context::ContextSection;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn snapshot(measured: bool) -> ContextSnapshot {
    ContextSnapshot {
        used_tokens: 74_400,
        context_window: 372_000,
        compact_at_tokens: 353_400,
        measured,
        sections: vec![
            ContextSection {
                kind: ContextKind::SystemPrompt,
                tokens: 3_720,
                items: 1,
            },
            ContextSection {
                kind: ContextKind::ToolCatalogue,
                tokens: 7_440,
                items: 1,
            },
            ContextSection {
                kind: ContextKind::RepositoryInstructions,
                tokens: 3_720,
                items: 1,
            },
            ContextSection {
                kind: ContextKind::UserMessages,
                tokens: 11_160,
                items: 3,
            },
            ContextSection {
                kind: ContextKind::AssistantMessages,
                tokens: 14_880,
                items: 2,
            },
            ContextSection {
                kind: ContextKind::ToolActivity,
                tokens: 11_160,
                items: 4,
            },
            ContextSection {
                kind: ContextKind::Reasoning,
                tokens: 22_320,
                items: 2,
            },
        ],
    }
}

#[test]
fn renders_capacity_grid_categories_and_compaction_headroom() {
    let context = ContextWindowView::new(snapshot(true));
    let segments = context.segments();
    let colors = grid_colors(&segments, context.snapshot.context_window);
    assert_eq!(
        [
            colors
                .iter()
                .filter(|color| **color == Color::Magenta)
                .count(),
            colors.iter().filter(|color| **color == Color::Cyan).count(),
            colors
                .iter()
                .filter(|color| **color == Color::Yellow)
                .count(),
            colors
                .iter()
                .filter(|color| **color == Color::LightYellow)
                .count(),
            colors
                .iter()
                .filter(|color| **color == Color::Green)
                .count(),
            colors
                .iter()
                .filter(|color| **color == Color::LightRed)
                .count(),
            colors
                .iter()
                .filter(|color| **color == Color::LightMagenta)
                .count(),
            colors
                .iter()
                .filter(|color| **color == Color::Indexed(238))
                .count(),
            colors
                .iter()
                .filter(|color| **color == Color::Indexed(245))
                .count(),
        ],
        [1, 2, 1, 3, 4, 3, 6, 75, 5]
    );
    let backend = TestBackend::new(92, VIEWPORT_HEIGHT);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| context.render(frame, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = render_buffer(buffer);

    assert_eq!(buffer[(0, 0)].symbol(), "┌");
    assert_eq!(buffer[(PREFERRED_WIDTH - 1, 0)].symbol(), "┐");
    assert_eq!(buffer[(PREFERRED_WIDTH, 0)].symbol(), " ");
    assert!(rendered.contains("Context"), "{rendered}");
    assert!(rendered.contains("74.4K / 372K tokens"), "{rendered}");
    assert!(rendered.contains("20.0% used"), "{rendered}");
    assert!(rendered.contains("Auto-compact at 353.4K"), "{rendered}");
    assert!(rendered.contains("279K free before compact"), "{rendered}");
    assert!(rendered.contains("System prompt"), "{rendered}");
    assert!(rendered.contains("Tool catalogue"), "{rendered}");
    assert!(rendered.contains("AGENTS.md instructions"), "{rendered}");
    assert!(rendered.contains("Tool calls & results"), "{rendered}");
    assert!(rendered.contains("Free before compact"), "{rendered}");
    assert!(rendered.contains("Auto-compact reserve"), "{rendered}");
    assert!(
        rendered.contains("Total from latest API usage"),
        "{rendered}"
    );
    assert!(rendered.matches('■').count() >= GRID_CELLS, "{rendered}");
    assert_eq!(context.handle_key(KeyCode::Esc), ContextAction::Close);
    assert_eq!(context.handle_key(KeyCode::Down), ContextAction::StayOpen);
}

#[test]
fn explains_when_all_accounting_is_estimated() {
    let context = ContextWindowView::new(snapshot(false));
    let backend = TestBackend::new(92, VIEWPORT_HEIGHT);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| context.render(frame, frame.area()))
        .unwrap();
    let rendered = render_buffer(terminal.backend().buffer());

    assert!(rendered.contains("No API usage yet"), "{rendered}");
    assert!(rendered.contains("all values estimated"), "{rendered}");
}

#[test]
fn rendering_is_bounded_for_tiny_terminals() {
    for (width, height) in [(1, 1), (2, 3), (7, 6), (20, 8)] {
        let context = ContextWindowView::new(snapshot(false));
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| context.render(frame, frame.area()))
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
        })
        .collect::<Vec<_>>()
        .join("\n")
}
