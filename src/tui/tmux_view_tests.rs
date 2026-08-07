use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

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
    assert!(enabled.contains("Tmux"), "{enabled}");
    assert!(
        enabled.contains("[x] Automatic tmux sessions (on)"),
        "{enabled}"
    );
    assert!(enabled.contains("c1, c2, …"), "{enabled}");
    assert!(enabled.contains("next launch"), "{enabled}");
    assert!(enabled.contains("space/enter toggle"), "{enabled}");

    let disabled = render(&view, TmuxMode::Off);
    assert!(
        disabled.contains("[ ] Automatic tmux sessions (off)"),
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

fn render(view: &TmuxView, mode: TmuxMode) -> String {
    let height = view.preferred_height();
    let backend = TestBackend::new(PREFERRED_WIDTH, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), mode))
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
        })
        .collect::<Vec<_>>()
        .join("\n")
}
