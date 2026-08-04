use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn summary(id: Uuid, cwd: &str, updated_at_unix_ms: u64, preview: &str) -> SessionSummary {
    SessionSummary {
        id,
        cwd: PathBuf::from(cwd),
        created_at_unix_ms: updated_at_unix_ms.saturating_sub(1_000),
        updated_at_unix_ms,
        preview: Some(preview.to_string()),
    }
}

#[test]
fn search_and_cwd_filter_select_the_matching_session() {
    let current = Uuid::new_v4();
    let other = Uuid::new_v4();
    let mut picker = ResumePicker::loading(Path::new("/work/current"), current);
    picker.set_sessions(vec![
        summary(current, "/work/current", 2_000, "Current work"),
        summary(other, "/work/other", 1_000, "Find the regression"),
    ]);

    picker.handle_paste("regression");
    assert!(picker.filtered.is_empty());
    assert_eq!(
        picker.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        ResumePickerAction::None
    );
    assert_eq!(picker.filtered, vec![1]);
    assert_eq!(
        picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ResumePickerAction::Resume(other)
    );
    assert!(matches!(
        picker.status,
        Some(PickerStatus::Resuming(id)) if id == other
    ));
}

#[test]
fn escape_clears_search_before_closing_the_picker() {
    let current = Uuid::new_v4();
    let mut picker = ResumePicker::loading(Path::new("/work/current"), current);
    picker.set_sessions(vec![summary(
        current,
        "/work/current",
        1_000,
        "Current work",
    )]);
    picker.handle_paste("missing");

    assert_eq!(
        picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ResumePickerAction::None
    );
    assert_eq!(
        (picker.query.as_str(), picker.filtered.clone()),
        ("", vec![0])
    );
    assert_eq!(
        picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ResumePickerAction::Close
    );
}

#[test]
fn a_stale_listing_cannot_replace_direct_resume_progress() {
    let current = Uuid::new_v4();
    let target = Uuid::new_v4();
    let mut picker = ResumePicker::resuming(Path::new("/work/current"), current, target);

    picker.set_sessions(vec![summary(
        target,
        "/work/current",
        1_000,
        "Stale listing",
    )]);
    picker.set_listing_error("stale error");

    assert!(matches!(
        picker.status,
        Some(PickerStatus::Resuming(id)) if id == target
    ));
    assert_eq!(picker.sessions, Some(Vec::new()));

    let backend = TestBackend::new(80, picker.preferred_height());
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| picker.render(frame, frame.area()))
        .unwrap();
    let rendered = render_buffer(terminal.backend().buffer());
    assert!(rendered.contains("Resuming"), "{rendered}");
    assert!(!rendered.contains("No sessions"), "{rendered}");
}

#[test]
fn picker_renders_session_metadata_and_sanitizes_preview_text() {
    let current = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
    let other = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
    let now = unix_timestamp_millis();
    let mut picker = ResumePicker::loading(Path::new("/work/current"), current);
    picker.set_sessions(vec![
        summary(current, "/work/current", now, "Current work"),
        summary(other, "/work/current", now, "\x1b[31mUnsafe title"),
    ]);

    let height = picker.preferred_height();
    let backend = TestBackend::new(100, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| picker.render(frame, frame.area()))
        .unwrap();
    let rendered = render_buffer(terminal.backend().buffer());

    assert!(rendered.contains("Resume a previous session"), "{rendered}");
    assert!(rendered.contains("Type to search"), "{rendered}");
    assert!(rendered.contains("Filter: [Cwd] All"), "{rendered}");
    assert!(rendered.contains("Current work  current"), "{rendered}");
    assert!(rendered.contains("Unsafe title"), "{rendered}");
    assert!(!rendered.contains('\x1b'), "{rendered:?}");
    assert!(rendered.contains("11111111"), "{rendered}");
    assert!(rendered.contains("enter resume"), "{rendered}");
}

#[test]
fn long_rows_keep_the_current_marker_and_short_session_id_visible() {
    let current = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
    let cwd = format!("/{}", "directory".repeat(20));
    let mut picker = ResumePicker::loading(Path::new(&cwd), current);
    picker.set_sessions(vec![summary(
        current,
        &cwd,
        unix_timestamp_millis(),
        &"conversation ".repeat(30),
    )]);

    let backend = TestBackend::new(48, picker.preferred_height());
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| picker.render(frame, frame.area()))
        .unwrap();
    let rendered = render_buffer(terminal.backend().buffer());

    assert!(rendered.contains("current"), "{rendered}");
    assert!(rendered.contains("11111111"), "{rendered}");
    assert!(rendered.contains('…'), "{rendered}");
}

#[test]
fn relative_time_uses_compact_codex_style_units() {
    assert_eq!(format_relative_time(60_000, 59_001), "now");
    assert_eq!(format_relative_time(120_000, 60_000), "1m ago");
    assert_eq!(format_relative_time(7_200_000, 3_600_000), "1h ago");
    assert_eq!(format_relative_time(172_800_000, 86_400_000), "1d ago");
}

#[test]
fn picker_rendering_is_bounded_for_tiny_terminals() {
    let current = Uuid::new_v4();
    let mut picker = ResumePicker::loading(Path::new("/tiny"), current);
    picker.set_sessions(vec![summary(current, "/tiny", 0, "A session")]);

    for (width, height) in [(1, 1), (4, 2), (12, 4)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area()))
            .unwrap();
        assert_eq!(terminal.backend().buffer().area.width, width);
        assert_eq!(terminal.backend().buffer().area.height, height);
    }
}

#[test]
fn pasted_search_queries_are_bounded() {
    let current = Uuid::new_v4();
    let mut picker = ResumePicker::loading(Path::new("/work/current"), current);
    picker.set_sessions(Vec::new());

    picker.handle_paste(&"x".repeat(MAX_QUERY_CHARS * 2));

    assert_eq!(picker.query.chars().count(), MAX_QUERY_CHARS);
}

fn render_buffer(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    let mut rendered = String::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            rendered.push_str(buffer[(x, y)].symbol());
        }
        rendered.push('\n');
    }
    rendered
}
