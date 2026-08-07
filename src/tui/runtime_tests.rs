use super::ActiveSubmissionRoute;
use super::MAX_READY_AGENT_EVENTS;
use super::ReceiverState;
use super::abort_join_task;
use super::active_submission_route;
use super::drain_ready_agent_events;
use super::prompt_history_for_session;
use super::terminal;
use super::terminal_hyperlinks;
use super::view::View;
use crate::assistant_message::AssistantMessage;
use crate::events::AgentEvent;
use crate::input::UserInput;
use crate::input::UserPrompt;
use crate::rollout::SessionTranscriptItem;
use crate::update::AvailableUpdate;
use codex_protocol::models::MessagePhase;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::oneshot;

fn completed_message(text: impl Into<String>) -> AgentEvent {
    AgentEvent::ModelMessageCompleted(AssistantMessage {
        text: text.into(),
        phase: Some(MessagePhase::FinalAnswer),
    })
}

#[test]
fn loop_submissions_queue_instead_of_steering_an_active_turn() {
    assert_eq!(
        active_submission_route(&UserPrompt::text("/loop improve startup"))
            .expect("valid slash loop"),
        ActiveSubmissionRoute::QueueLoop
    );
    assert_eq!(
        active_submission_route(&UserPrompt::text("improve startup $loop"))
            .expect("valid inline loop"),
        ActiveSubmissionRoute::QueueLoop
    );
    assert_eq!(
        active_submission_route(&UserPrompt::text("ordinary follow-up")).expect("ordinary prompt"),
        ActiveSubmissionRoute::SteerOrdinary
    );
    assert!(active_submission_route(&UserPrompt::text("/loop 0x invalid")).is_err());

    let (non_steerable, _control) = crate::agent::TurnControl::non_steerable_channel();
    assert!(
        non_steerable
            .steer(UserInput::text("wait until the loop ends"))
            .is_err()
    );
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

#[tokio::test]
async fn terminal_shutdown_aborts_owned_tasks() {
    struct DropSignal(Option<oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
        }
    }

    let (started_tx, started_rx) = oneshot::channel();
    let (dropped_tx, dropped_rx) = oneshot::channel();
    let mut task = Some(tokio::spawn(async move {
        let _drop_signal = DropSignal(Some(dropped_tx));
        started_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    }));
    started_rx.await.unwrap();

    abort_join_task(&mut task);

    tokio::time::timeout(Duration::from_secs(1), dropped_rx)
        .await
        .expect("terminal-owned task remained detached after shutdown")
        .unwrap();
    assert!(task.is_none());
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
fn update_available_card_renders_the_release_and_standalone_command() {
    const WIDTH: u16 = 80;
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    let _ = view.take_pending_history_lines(WIDTH);
    view.add_update_available(AvailableUpdate {
        current_version: "1.2.2".to_string(),
        latest_version: "1.2.3".to_string(),
    });

    let rendered = view
        .take_pending_history_lines(WIDTH)
        .iter()
        .map(plain)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Update available"), "{rendered}");
    assert!(
        rendered.contains("bettercodex 1.2.2 -> 1.2.3"),
        "{rendered}"
    );
    assert!(
        rendered.lines().any(|line| line.contains("bcodex update")),
        "{rendered}"
    );
    assert!(
        rendered.contains('╭') && rendered.contains('╰'),
        "{rendered}"
    );
}

#[test]
fn asynchronous_update_card_does_not_split_a_streaming_assistant_message() {
    const WIDTH: u16 = 80;
    const SCREEN_HEIGHT: u16 = 24;
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("stream around an update check");
    let _ = view.take_pending_history_lines(WIDTH);
    view.handle_agent_event(AgentEvent::ModelMessageDelta("alpha".to_string()));
    view.add_update_available(AvailableUpdate {
        current_version: "1.2.2".to_string(),
        latest_version: "1.2.3".to_string(),
    });
    view.handle_agent_event(AgentEvent::ModelMessageDelta(" omega".to_string()));

    let rendered = render_view(&mut view, WIDTH, SCREEN_HEIGHT);
    assert!(rendered.contains("alpha omega"), "{rendered}");
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
fn active_assistant_links_reach_the_ratatui_terminal_buffer_as_osc8() {
    const DESTINATION: &str = "https://example.com/docs";
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn("render a link");
    let _ = view.take_pending_history_lines(64);
    view.handle_agent_event(AgentEvent::ModelMessageDelta(format!(
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
    let response = format!("A wrapped destination ({DESTINATION}).");
    view.handle_agent_event(AgentEvent::ModelMessageDelta(response.clone()));
    view.handle_agent_event(completed_message(response));

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
        phase: Some(MessagePhase::FinalAnswer),
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

fn plain(line: &terminal_hyperlinks::HyperlinkLine) -> String {
    line.line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}
