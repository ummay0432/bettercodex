use super::MAX_READY_AGENT_EVENTS;
use super::ReceiverState;
use super::drain_ready_agent_events;
use super::prompt_history_for_session;
use super::terminal;
use super::terminal_hyperlinks;
use super::view::View;
use crate::events::AgentEvent;
use crate::rollout::SessionTranscriptItem;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc::unbounded_channel;

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
            .send(AgentEvent::ModelTextDelta(format!(
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
            .send(AgentEvent::ModelItemCompleted)
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
    view.handle_agent_event(AgentEvent::ModelTextDelta("**alpha**".to_string()));
    assert!(render_view(&mut view, WIDTH, SCREEN_HEIGHT).contains("alpha"));

    view.handle_agent_event(AgentEvent::ModelTextDelta(" omega".to_string()));
    let updated = render_view(&mut view, WIDTH, SCREEN_HEIGHT);
    assert!(updated.contains("alpha omega"), "{updated}");

    view.handle_agent_event(AgentEvent::ModelItemCompleted);
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
fn prepared_layout_reflows_if_the_terminal_resizes_before_draw() {
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("render across a resize");
    let _ = view.take_pending_history_lines(80);
    view.handle_agent_event(AgentEvent::ModelTextDelta(
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
fn active_assistant_links_reach_the_ratatui_terminal_buffer_as_osc8() {
    const DESTINATION: &str = "https://example.com/docs";
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("render a link");
    let _ = view.take_pending_history_lines(64);
    view.handle_agent_event(AgentEvent::ModelTextDelta(format!(
        "Read [the docs]({DESTINATION}) and {DESTINATION}."
    )));

    let rendered = render_view(&mut view, 64, 24);
    assert!(
        rendered.contains(&format!("\x1b]8;;{DESTINATION}\x07")),
        "{rendered:?}"
    );
    let visible = terminal_hyperlinks::strip_osc8(&rendered);
    assert!(visible.contains("the docs"), "{visible}");
    assert!(visible.contains(DESTINATION), "{visible}");
}

#[test]
fn finalized_scrollback_resize_and_resume_preserve_link_destinations() {
    const DESTINATION: &str = "https://example.com/a_(b)";
    let mut view = View::new(Path::new("/workspace/project"));
    view.start_turn("render finalized links");
    let _ = view.take_pending_history_lines(52);
    view.handle_agent_event(AgentEvent::ModelTextDelta(format!(
        "A wrapped destination ({DESTINATION})."
    )));
    view.handle_agent_event(AgentEvent::ModelItemCompleted);

    let finalized = view.take_pending_history_lines(52);
    assert!(
        finalized
            .iter()
            .flat_map(|line| &line.hyperlinks)
            .any(|link| { link.destination == DESTINATION })
    );
    let buffer = terminal::render_history_lines(&finalized, 52);
    assert!(render_buffer(&buffer).contains(&format!("\x1b]8;;{DESTINATION}\x07")));

    let resized = view.history_lines_for_resize_reflow(24);
    assert!(
        resized
            .iter()
            .flat_map(|line| &line.hyperlinks)
            .any(|link| { link.destination == DESTINATION })
    );

    let mut resumed = View::new(Path::new("/workspace/project"));
    resumed.replay_transcript([SessionTranscriptItem::Assistant {
        text: format!("Resumed [{DESTINATION}]({DESTINATION})"),
    }]);
    let replay = resumed.take_pending_history_lines(36);
    assert!(
        replay
            .iter()
            .flat_map(|line| &line.hyperlinks)
            .any(|link| { link.destination == DESTINATION })
    );
    let replay_buffer = terminal::render_history_lines(&replay, 36);
    assert!(render_buffer(&replay_buffer).contains("\x1b]8;;https://example.com/a_(b)\x07"));
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
            .send(AgentEvent::ModelTextDelta(format!(
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
    let height = prepared.height();
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render_prepared(frame, prepared))
        .unwrap();
    render_buffer(terminal.backend().buffer())
}
