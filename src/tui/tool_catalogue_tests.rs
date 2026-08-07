use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

#[test]
fn renders_on_the_shared_codex_command_surface() {
    let catalogue = ToolCatalogueView::new();
    let backend = TestBackend::new(88, catalogue.preferred_height());
    let mut terminal = Terminal::new(backend).unwrap();
    let surface = Style::default().bg(Color::Rgb(55, 55, 55));
    terminal
        .draw(|frame| catalogue.render(frame, frame.area(), surface))
        .unwrap();
    let buffer = terminal.backend().buffer();

    assert_eq!(buffer[(0, 0)].bg, Color::Rgb(55, 55, 55));
    assert_eq!(buffer[(87, 0)].bg, Color::Rgb(55, 55, 55));
    assert_eq!(
        buffer[(0, catalogue.preferred_height().saturating_sub(1))].bg,
        Color::Reset
    );
    assert_eq!(
        render_buffer(buffer),
        [
            "",
            "  Tools",
            "",
            "  Request tools",
            "    ├─ ● exec",
            "    └─ ● wait",
            "",
            "  Inside exec",
            "    ├─ ● apply_patch",
            "    ├─ ● exec_command",
            "    ├─ ● log_papercut",
            "    ├─ ● update_plan",
            "    ├─ ● view_image",
            "    ├─ ● write_stdin",
            "    ├─ ● openaiDeveloperDocs__fetch_openai_doc",
            "    ├─ ● openaiDeveloperDocs__get_openapi_spec",
            "    ├─ ● openaiDeveloperDocs__list_api_endpoints",
            "    ├─ ● openaiDeveloperDocs__list_openai_docs",
            "    ├─ ● openaiDeveloperDocs__search_openai_docs",
            "    └─ ● web__run",
            "",
            "  14 tools · ~1.7K prompt tokens",
            "",
            "  Press esc to go back",
        ]
        .join("\n")
    );
}

#[test]
fn preferred_height_is_derived_from_the_active_catalogue() {
    let catalogue = ToolCatalogueView::new();
    assert_eq!(
        catalogue.preferred_height(),
        u16::try_from(catalogue_lines(crate::tools::display_tools(), catalogue.metrics).len())
            .unwrap()
            .saturating_add(5)
    );
}

#[test]
fn only_escape_closes_the_catalogue() {
    let catalogue = ToolCatalogueView::new();
    for key in [
        KeyCode::Down,
        KeyCode::Enter,
        KeyCode::Char('q'),
        KeyCode::Char('x'),
    ] {
        assert_eq!(catalogue.handle_key(key), CatalogueAction::StayOpen);
    }
    assert_eq!(catalogue.handle_key(KeyCode::Esc), CatalogueAction::Close);
}

#[test]
fn rendering_is_bounded_for_tiny_terminals() {
    for (width, height) in [(1, 1), (2, 3), (7, 6), (20, 8)] {
        let catalogue = ToolCatalogueView::new();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| catalogue.render(frame, frame.area(), Style::default()))
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
