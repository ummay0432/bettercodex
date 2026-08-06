use super::*;
use crate::context::AUTO_COMPACT_TOKEN_LIMIT;
use crate::context::ContextSection;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn snapshot(measured: bool) -> ContextSnapshot {
    ContextSnapshot {
        used_tokens: 51_680,
        context_window: EFFECTIVE_CONTEXT_WINDOW,
        compact_at_tokens: AUTO_COMPACT_TOKEN_LIMIT,
        measured,
        sections: vec![
            ContextSection {
                kind: ContextKind::SystemPrompt,
                tokens: 2_584,
                items: 1,
            },
            ContextSection {
                kind: ContextKind::ToolCatalogue,
                tokens: 5_168,
                items: 1,
            },
            ContextSection {
                kind: ContextKind::RepositoryInstructions,
                tokens: 2_584,
                items: 1,
            },
            ContextSection {
                kind: ContextKind::UserMessages,
                tokens: 7_752,
                items: 3,
            },
            ContextSection {
                kind: ContextKind::AssistantMessages,
                tokens: 10_336,
                items: 2,
            },
            ContextSection {
                kind: ContextKind::ToolActivity,
                tokens: 7_752,
                items: 4,
            },
            ContextSection {
                kind: ContextKind::Reasoning,
                tokens: 15_504,
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
    assert_eq!(context.preferred_height(92), 16);
    assert_eq!(context.preferred_height(40), 15);
    let backend = TestBackend::new(92, context.preferred_height(92));
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
    assert!(rendered.contains("51.7K / 258.4K tokens"), "{rendered}");
    assert!(rendered.contains("20.0% used"), "{rendered}");
    assert!(rendered.contains("Auto-compact at 244.8K"), "{rendered}");
    assert!(
        !rendered.contains("Auto-compact at 244.8K  ·"),
        "{rendered}"
    );
    assert!(rendered.contains("System prompt"), "{rendered}");
    assert!(rendered.contains("Tool catalogue"), "{rendered}");
    assert!(rendered.contains("AGENTS.md instructions"), "{rendered}");
    assert!(rendered.contains("Tool calls & results"), "{rendered}");
    assert!(rendered.contains("Free before compact"), "{rendered}");
    assert!(rendered.contains("Auto-compact reserve"), "{rendered}");
    assert!(rendered.matches('■').count() >= GRID_CELLS, "{rendered}");
    assert_eq!(context.handle_key(KeyCode::Esc), ContextAction::Close);
    assert_eq!(context.handle_key(KeyCode::Down), ContextAction::StayOpen);
}

#[test]
fn omits_accounting_notes() {
    for measured in [false, true] {
        let context = ContextWindowView::new(snapshot(measured));
        let backend = TestBackend::new(92, context.preferred_height(92));
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| context.render(frame, frame.area()))
            .unwrap();
        let rendered = render_buffer(terminal.backend().buffer());

        assert!(!rendered.contains("No API usage yet"), "{rendered}");
        assert!(
            !rendered.contains("Total from latest API usage"),
            "{rendered}"
        );
        assert!(!rendered.contains("Each square is 1%"), "{rendered}");
    }
}

#[test]
fn height_expands_to_fit_a_legend_larger_than_the_grid() {
    let mut snapshot = snapshot(true);
    snapshot.sections.extend([
        ContextSection {
            kind: ContextKind::Environment,
            tokens: 1,
            items: 1,
        },
        ContextSection {
            kind: ContextKind::Compaction,
            tokens: 1,
            items: 1,
        },
        ContextSection {
            kind: ContextKind::Other,
            tokens: 1,
            items: 1,
        },
    ]);
    let context = ContextWindowView::new(snapshot);
    assert_eq!(context.segments().len(), 12);
    assert_eq!(context.preferred_height(92), 18);

    let backend = TestBackend::new(92, context.preferred_height(92));
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| context.render(frame, frame.area()))
        .unwrap();
    let rendered = render_buffer(terminal.backend().buffer());
    assert!(rendered.contains("Compacted history"), "{rendered}");
    assert!(rendered.contains("Other"), "{rendered}");
    assert!(rendered.contains("Auto-compact reserve"), "{rendered}");
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
