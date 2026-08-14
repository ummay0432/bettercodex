use super::context_window::format_tokens;
use super::view::Action;
use super::view::View;
use crate::context::ContextKind;
use crate::context::ContextSection;
use crate::context::ContextSnapshot;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use crate::context::estimated_tokens;
use crate::events::AgentEvent;
use crate::model::AUTO_COMPACT_TOKEN_LIMIT;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::Path;

fn context_snapshot(used_tokens: u64) -> ContextSnapshot {
    let tool_tokens = estimated_tokens(crate::tools::responses_api_specifications());
    ContextSnapshot {
        used_tokens,
        context_window: EFFECTIVE_CONTEXT_WINDOW,
        compact_at_tokens: AUTO_COMPACT_TOKEN_LIMIT,
        measured: false,
        sections: vec![ContextSection {
            kind: ContextKind::ToolCatalogue,
            tokens: tool_tokens,
            items: crate::tools::responses_api_specifications().len(),
        }],
        total_usage: Default::default(),
        rate_limits: Vec::new(),
    }
}

fn press(view: &mut View, code: KeyCode) -> Action {
    view.handle_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn render(view: &mut View, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| view.render(frame))
        .expect("render view");
    let buffer = terminal.backend().buffer();
    (buffer.area.y..buffer.area.bottom())
        .map(|y| {
            (buffer.area.x..buffer.area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn tools_command_renders_the_live_catalogue_and_context_costs() {
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    assert_eq!(
        view.handle_terminal_event(Event::Paste("/tools".to_string())),
        Action::None
    );
    assert_eq!(press(&mut view, KeyCode::Enter), Action::None);

    let rendered = render(&mut view, 80, 24);
    let specifications = crate::tools::responses_api_specifications();
    let total_tokens = estimated_tokens(specifications);
    let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(rendered.contains("Tools"), "{rendered}");
    assert!(
        normalized.contains(&format!(
            "4 direct functions + hosted web search · ~{} estimated context tokens per request",
            format_tokens(total_tokens)
        )),
        "{rendered}"
    );
    assert!(
        normalized.contains("fixed function schemas and compact hosted-search declaration"),
        "{rendered}"
    );
    assert!(
        normalized.contains("Responses API executes hosted web search"),
        "{rendered}"
    );
    for specification in specifications {
        let hosted = specification["type"] == "web_search";
        let name = if hosted {
            "web_search"
        } else {
            specification["name"].as_str().expect("tool name")
        };
        let tokens = estimated_tokens(std::slice::from_ref(specification));
        let description = if hosted {
            "Search and browse the live web using text and image results."
        } else {
            specification["description"]
                .as_str()
                .expect("tool description")
                .lines()
                .find(|line| !line.trim().is_empty())
                .expect("brief tool description")
        };
        let description = description
            .find(". ")
            .map_or(description, |end| &description[..=end]);
        let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(rendered.contains(name), "missing {name:?}\n{rendered}");
        assert!(
            normalized.contains(&description),
            "missing description for {name:?}\n{rendered}"
        );
        assert!(
            rendered.contains(&format!("~{} tokens", format_tokens(tokens))),
            "missing token cost for {name:?}\n{rendered}"
        );
    }
    assert!(rendered.contains("Press esc to close"), "{rendered}");
}

#[test]
fn context_opens_tools_as_a_child_and_returns_to_the_updated_context() {
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.show_context(context_snapshot(1_000));
    let context = render(&mut view, 80, 24);
    assert!(context.contains("Context"), "{context}");
    assert!(
        context.contains("Tools  bash · read · write · edit · web_search  ·  t for details"),
        "{context}"
    );
    assert!(
        context.contains("Press t for tool details; esc to close"),
        "{context}"
    );

    assert_eq!(press(&mut view, KeyCode::Char('t')), Action::None);
    let tools = render(&mut view, 80, 24);
    assert!(tools.contains("Tools"), "{tools}");
    assert!(tools.contains("Press esc to go back to context"), "{tools}");

    view.handle_agent_event(AgentEvent::ContextUpdated(context_snapshot(2_000)));
    assert_eq!(press(&mut view, KeyCode::Esc), Action::None);
    let returned = render(&mut view, 80, 24);
    assert!(returned.contains("Context"), "{returned}");
    assert!(returned.contains("2K / 258.4K tokens"), "{returned}");
}
