use super::MAX_READY_AGENT_EVENTS;
use super::ReceiverState;
use super::drain_ready_agent_events;
use super::prompt_history_for_session;
use super::view::View;
use crate::assistant_message::AssistantMessage;
use crate::events::AgentEvent;
use codex_protocol::models::MessagePhase;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc::unbounded_channel;

fn completed_message(text: impl Into<String>) -> AgentEvent {
    AgentEvent::ModelMessageCompleted(AssistantMessage {
        text: text.into(),
        phase: Some(MessagePhase::FinalAnswer),
    })
}

#[test]
fn resumed_prompts_precede_global_duplicates_during_recall() {
    let persistent = vec![
        "global older".to_string(),
        "resumed older".to_string(),
        "global newer".to_string(),
        "resumed newer".to_string(),
    ];
    let resumed = vec!["resumed older".to_string(), "resumed newer".to_string()];

    assert_eq!(
        prompt_history_for_session(&persistent, resumed),
        [
            "global older",
            "global newer",
            "resumed older",
            "resumed newer",
        ]
    );
}

#[test]
fn ready_stream_events_are_drained_before_the_terminal_frame() {
    const DELTAS: usize = 128;
    const WIDTH: u16 = 80;
    const SCREEN_HEIGHT: u16 = 24;

    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("render a response");
    let _ = view.take_pending_history_lines(WIDTH);
    let (events, mut ready) = unbounded_channel();
    for index in 0..DELTAS {
        events
            .send(AgentEvent::ModelMessageDelta(format!(
                "streamed line {index}\n"
            )))
            .unwrap();
    }
    drop(events);

    let mut applied = 0;
    let state = drain_ready_agent_events(&mut ready, |event| {
        applied += 1;
        view.handle_agent_event(event);
    });

    assert_eq!((state, applied), (ReceiverState::Closed, DELTAS));
    let prepared = view.prepare(WIDTH, SCREEN_HEIGHT);
    let height = prepared.height();
    let backend = TestBackend::new(WIDTH, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render_prepared(frame, prepared))
        .unwrap();
    let rendered = render_buffer(terminal.backend().buffer());
    let visible_text = rendered
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(visible_text.contains("streamedline127"), "{rendered}");
}

#[test]
fn ready_event_drain_yields_to_the_event_loop_at_its_bound() {
    let (events, mut ready) = unbounded_channel();
    for _ in 0..=MAX_READY_AGENT_EVENTS {
        events
            .send(completed_message(String::new()))
            .expect("receiver remains open");
    }
    drop(events);

    let mut applied = 0;
    let first = drain_ready_agent_events(&mut ready, |_| applied += 1);
    assert_eq!(
        (first, applied),
        (ReceiverState::Open, MAX_READY_AGENT_EVENTS)
    );

    let second = drain_ready_agent_events(&mut ready, |_| applied += 1);
    assert_eq!(
        (second, applied),
        (ReceiverState::Closed, MAX_READY_AGENT_EVENTS + 1)
    );
}

#[test]
fn prepared_streaming_layout_tracks_new_deltas_and_finalizes_to_history() {
    const WIDTH: u16 = 80;
    const SCREEN_HEIGHT: u16 = 24;

    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("render a cached response");
    let _ = view.take_pending_history_lines(WIDTH);
    view.handle_agent_event(AgentEvent::ModelMessageDelta("**alpha**".to_string()));
    assert!(render_view(&mut view, WIDTH, SCREEN_HEIGHT).contains("alpha"));

    view.handle_agent_event(AgentEvent::ModelMessageDelta(" omega".to_string()));
    let updated = render_view(&mut view, WIDTH, SCREEN_HEIGHT);
    assert!(updated.contains("alpha omega"), "{updated}");

    view.handle_agent_event(completed_message("**alpha** omega"));
    let finalized = view
        .take_pending_history_lines(WIDTH)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(finalized.contains("alpha omega"), "{finalized}");
}

#[test]
fn oversized_stream_moves_old_rows_to_history_before_completion() {
    const WIDTH: u16 = 48;
    const SCREEN_HEIGHT: u16 = 12;

    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("stream beyond the viewport");
    let _ = view.take_pending_history_lines(WIDTH);
    let first = (0..40)
        .map(|index| format!("- stream row {index}\n"))
        .collect::<String>();
    view.handle_agent_event(AgentEvent::ModelMessageDelta(first.clone()));

    let mut prepared = view.prepare(WIDTH, SCREEN_HEIGHT);
    let mut emitted = prepared.take_history_lines();
    let rendered = render_prepared_view(&mut view, WIDTH, prepared);
    assert!(
        emitted
            .iter()
            .any(|line| plain(line).ends_with("stream row 0")),
        "{emitted:?}"
    );
    assert!(!rendered.contains("stream row 0"), "{rendered}");
    assert!(rendered.contains("stream row 39"), "{rendered}");

    let second = (40..45)
        .map(|index| format!("- stream row {index}\n"))
        .collect::<String>();
    view.handle_agent_event(AgentEvent::ModelMessageDelta(second.clone()));
    let mut prepared = view.prepare(WIDTH, SCREEN_HEIGHT);
    let next = prepared.take_history_lines();
    assert!(
        next.first().is_some_and(|line| !plain(line).is_empty()),
        "a streamed continuation must not add another cell separator: {next:?}"
    );
    emitted.extend(next);
    assert!(render_prepared_view(&mut view, WIDTH, prepared).contains("stream row 44"));

    view.handle_agent_event(completed_message(format!("{first}{second}")));
    assert!(!view.streamed_history_needs_reflow(WIDTH));
    emitted.extend(view.take_pending_history_lines(WIDTH));
    for index in 0..45 {
        let suffix = format!("stream row {index}");
        assert_eq!(
            emitted
                .iter()
                .filter(|line| plain(line).ends_with(&suffix))
                .count(),
            1,
            "row {index} was lost or duplicated in {emitted:?}"
        );
    }
}

#[test]
fn completed_stream_requests_replay_if_markdown_rewrites_emitted_rows() {
    const WIDTH: u16 = 48;
    const SCREEN_HEIGHT: u16 = 12;

    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("render a late reference definition");
    let _ = view.take_pending_history_lines(WIDTH);
    let mut initial = String::from("[earlier reference][target]\n\n");
    initial.push_str(
        &(0..40)
            .map(|index| format!("- filler row {index}\n"))
            .collect::<String>(),
    );
    view.handle_agent_event(AgentEvent::ModelMessageDelta(initial.clone()));
    let mut prepared = view.prepare(WIDTH, SCREEN_HEIGHT);
    let emitted = prepared.take_history_lines();
    assert!(
        emitted
            .iter()
            .any(|line| plain(line).contains("[earlier reference][target]")),
        "{emitted:?}"
    );

    view.handle_agent_event(AgentEvent::ModelMessageDelta(
        "\n[target]: https://example.com\n".to_string(),
    ));
    view.handle_agent_event(completed_message(format!(
        "{initial}\n[target]: https://example.com\n"
    )));
    assert!(view.streamed_history_needs_reflow(WIDTH));

    let replay = view
        .history_lines_for_resize_reflow(WIDTH)
        .iter()
        .map(plain)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(replay.contains("earlier reference"), "{replay}");
    assert!(!replay.contains("[earlier reference][target]"), "{replay}");
}

#[test]
fn prepared_layout_reflows_if_the_terminal_resizes_before_draw() {
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("render across a resize");
    let _ = view.take_pending_history_lines(80);
    view.handle_agent_event(AgentEvent::ModelMessageDelta(
        "a response wide enough to reflow at the narrower width resize-marker".to_string(),
    ));
    let prepared = view.prepare(80, 24);
    let backend = TestBackend::new(32, prepared.height());
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| view.render_prepared(frame, prepared))
        .unwrap();

    let rendered = render_buffer(terminal.backend().buffer());
    let visible_text = rendered
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(visible_text.contains("resize-marker"), "{rendered}");
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_streaming_markdown_event_burst() {
    const DELTAS: usize = 2_000;
    const WIDTH: u16 = 100;
    const SCREEN_HEIGHT: u16 = 40;

    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("measure streaming rendering");
    let _ = view.take_pending_history_lines(WIDTH);
    let (events, mut ready) = unbounded_channel();
    for index in 0..DELTAS {
        events
            .send(AgentEvent::ModelMessageDelta(format!(
                "token {index} with **markdown** and `code`\n"
            )))
            .unwrap();
    }
    drop(events);

    let started = Instant::now();
    let state = drain_ready_agent_events(&mut ready, |event| view.handle_agent_event(event));
    let prepared = view.prepare(WIDTH, SCREEN_HEIGHT);
    let height = prepared.height();
    let backend = TestBackend::new(WIDTH, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render_prepared(frame, prepared))
        .unwrap();
    std::hint::black_box(terminal.backend().buffer());
    assert_eq!(state, ReceiverState::Closed);
    eprintln!(
        "{DELTAS} streaming deltas drained into one frame: {:?}",
        started.elapsed()
    );
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

fn render_view(view: &mut View, width: u16, screen_height: u16) -> String {
    let prepared = view.prepare(width, screen_height);
    render_prepared_view(view, width, prepared)
}

fn render_prepared_view(
    view: &mut View,
    width: u16,
    prepared: super::view::PreparedView,
) -> String {
    let height = prepared.height();
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render_prepared(frame, prepared))
        .unwrap();
    render_buffer(terminal.backend().buffer())
}

fn plain(line: &ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}
