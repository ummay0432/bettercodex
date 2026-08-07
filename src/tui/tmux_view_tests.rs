use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::style::Modifier;

#[test]
fn space_and_enter_toggle_the_mode_and_escape_closes() {
    let mut view = TmuxView::new();
    assert_eq!(
        view.handle_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            TmuxMode::On
        ),
        TmuxViewAction::SetMode(TmuxMode::Off)
    );
    assert_eq!(
        view.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            TmuxMode::Off
        ),
        TmuxViewAction::SetMode(TmuxMode::On)
    );
    assert_eq!(
        view.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            TmuxMode::On
        ),
        TmuxViewAction::Close
    );
}

#[test]
fn rendered_toggle_shows_state_timing_and_controls() {
    let view = TmuxView::new();
    let enabled = render(&view, TmuxMode::On);
    assert_eq!(
        enabled,
        [
            "",
            "  Tmux",
            "  Changes are saved automatically and apply on the next launch.",
            "",
            "› [x] Automatic tmux sessions  Start interactive launches in detachable c1,",
            "                               c2, … sessions.",
            "",
            "  Press space or enter to toggle; esc to close",
        ]
        .join("\n")
    );

    let disabled = render(&view, TmuxMode::Off);
    assert!(
        disabled.contains("› [ ] Automatic tmux sessions"),
        "{disabled}"
    );
}

#[test]
fn save_error_replaces_the_timing_hint() {
    let mut view = TmuxView::new();
    view.set_error("Could not save <tmux>");
    let rendered = render(&view, TmuxMode::On);
    assert!(rendered.contains("Could not save <tmux>"), "{rendered}");
    assert!(!rendered.contains("next launch"), "{rendered}");
}

#[test]
fn rendered_menu_uses_the_codex_surface_and_active_row_styles() {
    const WIDTH: u16 = 80;
    let view = TmuxView::new();
    let height = view.preferred_height(WIDTH, TmuxMode::On);
    let backend = TestBackend::new(WIDTH, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let surface = Style::default().bg(Color::Rgb(55, 55, 55));
    terminal
        .draw(|frame| view.render(frame, frame.area(), TmuxMode::On, surface))
        .unwrap();
    let buffer = terminal.backend().buffer();

    assert_eq!(buffer[(0, 0)].bg, Color::Rgb(55, 55, 55));
    assert_eq!(buffer[(WIDTH - 1, 0)].bg, Color::Rgb(55, 55, 55));
    assert_eq!(buffer[(0, height - 1)].bg, Color::Reset);
    assert!(buffer[(0, 4)].modifier.contains(Modifier::BOLD));
    assert_ne!(buffer[(0, 4)].fg, Color::Reset);
}

fn render(view: &TmuxView, mode: TmuxMode) -> String {
    const WIDTH: u16 = 80;
    let height = view.preferred_height(WIDTH, mode);
    let backend = TestBackend::new(WIDTH, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), mode, Style::default()))
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
