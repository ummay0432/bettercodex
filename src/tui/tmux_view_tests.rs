use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::style::Modifier;

#[test]
fn space_stages_the_mode_and_accept_keys_save_it() {
    let mut view = TmuxView::new(TmuxMode::On);
    assert_eq!(
        view.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
        TmuxViewAction::None
    );
    assert_eq!(
        view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        TmuxViewAction::Save(TmuxMode::Off)
    );

    let mut escape = TmuxView::new(TmuxMode::On);
    escape.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    assert_eq!(
        escape.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        TmuxViewAction::Save(TmuxMode::Off)
    );

    let mut control_c = TmuxView::new(TmuxMode::Off);
    assert_eq!(
        control_c.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        TmuxViewAction::Save(TmuxMode::Off)
    );
}

#[test]
fn rendered_toggle_shows_state_timing_and_controls() {
    let mut view = TmuxView::new(TmuxMode::On);
    let enabled = render(&view);
    assert_eq!(
        enabled,
        [
            "",
            "  Tmux",
            "  Toggle automatic tmux sessions. Changes are saved to settings.json.",
            "",
            "› [x] Automatic tmux sessions  Start interactive launches in detachable c1,",
            "                               c2, … sessions.",
            "",
            "  Press space to toggle or enter to save for next launch",
        ]
        .join("\n")
    );

    view.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    let disabled = render(&view);
    assert!(
        disabled.contains("› [ ] Automatic tmux sessions"),
        "{disabled}"
    );
}

#[test]
fn save_error_replaces_the_timing_hint() {
    let mut view = TmuxView::new(TmuxMode::On);
    view.set_error("Could not save <tmux>");
    let rendered = render(&view);
    assert!(rendered.contains("Could not save <tmux>"), "{rendered}");
    assert!(
        !rendered.contains("Changes are saved to settings.json"),
        "{rendered}"
    );
}

#[test]
fn rendered_menu_uses_the_codex_surface_and_active_row_styles() {
    const WIDTH: u16 = 80;
    let view = TmuxView::new(TmuxMode::On);
    let height = view.preferred_height(WIDTH);
    let backend = TestBackend::new(WIDTH, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let surface = Style::default().bg(Color::Rgb(55, 55, 55));
    terminal
        .draw(|frame| view.render(frame, frame.area(), surface))
        .unwrap();
    let buffer = terminal.backend().buffer();

    assert_eq!(buffer[(0, 0)].bg, Color::Rgb(55, 55, 55));
    assert_eq!(buffer[(WIDTH - 1, 0)].bg, Color::Rgb(55, 55, 55));
    assert_eq!(buffer[(0, height - 1)].bg, Color::Reset);
    assert!(buffer[(0, 4)].modifier.contains(Modifier::BOLD));
    assert_ne!(buffer[(0, 4)].fg, Color::Reset);
}

fn render(view: &TmuxView) -> String {
    const WIDTH: u16 = 80;
    let height = view.preferred_height(WIDTH);
    let backend = TestBackend::new(WIDTH, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), Style::default()))
        .unwrap();
    render_buffer(terminal.backend().buffer())
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
