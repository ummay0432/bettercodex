use super::*;
use crate::assistant_message::AssistantMessage;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use crate::events::AgentEvent;
use crate::events::ModelTextDelta;
use crate::protocol::MessagePhase;
use crate::rollout::SessionTranscriptToolOrigin;
use crate::rollout::SessionTranscriptToolOutput;
use crate::skills::SkillUpdate;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::Path;
use tokio::sync::mpsc::unbounded_channel;

fn runtime_without_agent() -> Runtime {
    crate::http_client::ensure_rustls_crypto_provider();
    let cwd = PathBuf::from("/tmp/bettercodex");
    let (diff_updates_tx, diff_updates) = unbounded_channel();
    let (file_search_updates_tx, file_search_updates) = unbounded_channel();
    let (operator_command_updates_tx, operator_command_updates) = unbounded_channel();
    let rate_limit_client = RateLimitClient::new(
        reqwest::Client::new(),
        crate::auth::SharedAuth::new(crate::auth::Auth::for_test("test-token")),
        "http://127.0.0.1",
    );
    Runtime {
        clipboard_lease: None,
        cwd: cwd.clone(),
        agent: None,
        turn: TurnTaskState::Idle,
        turn_events: None,
        turn_handle: None,
        exit_after_work: false,
        context_snapshot: ContextSnapshot {
            used_tokens: 0,
            context_window: EFFECTIVE_CONTEXT_WINDOW,
            compact_at_tokens: 0,
            measured: false,
            sections: Vec::new(),
            total_usage: Default::default(),
            rate_limits: Vec::new(),
        },
        session_id: "00000000-0000-0000-0000-000000000000".to_string(),
        forked_from: None,
        instruction_source_paths: Vec::new(),
        rate_limit_client,
        rate_limit_task: None,
        rate_limit_prefetch_started: false,
        status_rate_limits: BTreeMap::new(),
        diff_task: None,
        diff_updates,
        diff_updates_tx,
        file_search: FileSearchManager::new(cwd.clone(), file_search_updates_tx),
        file_search_updates,
        prompt_history: None,
        prompt_history_reader: None,
        prompt_history_task: None,
        prompt_history_exclusions: HashSet::new(),
        model_selection: ModelSelection::default(),
        service_tier: ServiceTier::default(),
        session_scan: None,
        resume_task: None,
        resume_submission: None,
        notifier: None,
        operator_command_tasks: HashMap::new(),
        operator_command_cancellations: HashMap::new(),
        pending_operator_contexts: VecDeque::new(),
        operator_context_steers: Vec::new(),
        operator_command_updates,
        operator_command_updates_tx,
        terminal_focused: true,
        terminal_title: TerminalTitle::new(),
        turn_started_at: None,
        update_check: None,
        update_check_started: false,
        worker_handoff: None,
        view: View::new(&cwd),
    }
}

fn composer_action(view: &mut View, text: &str, key: KeyCode) -> Action {
    assert_eq!(
        view.handle_terminal_event(Event::Paste(text.to_string())),
        Action::None
    );
    view.handle_terminal_event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)))
}

fn submitted_prompt(action: Action) -> UserPrompt {
    match action {
        Action::Submit(submission) => submission.into_prompt(),
        action => panic!("expected a composer submission, got {action:?}"),
    }
}

fn rendered_history(view: &mut View) -> String {
    view.take_pending_history_lines(80, 24)
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect()
}

fn rendered_view(view: &mut View) -> String {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| view.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();
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

#[test]
fn interrupted_steering_replay_preserves_operator_and_context_order() {
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn(&UserPrompt::text("active turn"));
    view.add_pending_steer(SteerId(1), UserPrompt::text("first operator steer"));
    view.add_pending_steer(SteerId(3), UserPrompt::text("second operator steer"));
    view.set_interrupting(InterruptIntent::SubmitSteering);

    let mut user_steers = view
        .finish_turn(Ok(SubmitOutcome::Cancelled))
        .expect("interrupt should retain submitted steering");
    let first_user_steer = user_steers.pop().expect("at least one steering input");
    let replay = interrupted_steering_replay(
        first_user_steer,
        user_steers,
        vec![
            (SteerId(0), "context before first".to_string()),
            (SteerId(2), "context between operators".to_string()),
            (SteerId(4), "context after second".to_string()),
        ],
    );

    assert_eq!(replay.leading_contexts, ["context before first"]);
    assert_eq!(replay.first_prompt.as_str(), "first operator steer");
    assert!(matches!(
        replay.trailing.as_slice(),
        [
            InterruptedSteering::Context(first),
            InterruptedSteering::Operator(second),
            InterruptedSteering::Context(third),
        ] if first == "context between operators"
            && second.as_str() == "second operator steer"
            && third == "context after second"
    ));

    let mut replay_view = View::new(Path::new("/tmp/bettercodex"));
    replay_view.start_turn(&replay.first_prompt);
    let (turn_handle, _turn_control) = crate::agent::TurnControl::channel();
    let mut context_steers = Vec::new();
    let unqueued = enqueue_initial_steering(
        &turn_handle,
        &mut replay_view,
        &mut context_steers,
        replay.trailing,
    );
    assert!(unqueued.is_empty());
    assert_eq!(
        context_steers
            .iter()
            .map(|(id, context)| (*id, context.as_str()))
            .collect::<Vec<_>>(),
        [
            (SteerId(0), "context between operators"),
            (SteerId(2), "context after second"),
        ]
    );
    let pending = rendered_view(&mut replay_view);
    assert!(pending.contains("second operator steer"), "{pending}");
    assert!(pending.contains("1 steering"), "{pending}");

    let rendered = rendered_history(&mut view);
    assert!(
        rendered.contains("Model interrupted to submit steering input"),
        "{rendered}"
    );
}

#[test]
fn completed_turn_drain_renders_events_beyond_the_fairness_batch() {
    const WIDTH: u16 = 80;
    const HEIGHT: u16 = 24;
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.start_turn(&UserPrompt::text("render every queued delta"));
    let _ = view.take_pending_history_lines(WIDTH, HEIGHT);
    let (events, mut ready) = unbounded_channel();
    let mut answer = "x".repeat(MAX_READY_AGENT_EVENTS);
    answer.push_str(" tail-marker");
    for _ in 0..MAX_READY_AGENT_EVENTS {
        events
            .send(AgentEvent::ModelMessageDelta(ModelTextDelta::now("x")))
            .unwrap();
    }
    events
        .send(AgentEvent::ModelMessageDelta(ModelTextDelta::now(
            " tail-marker",
        )))
        .unwrap();
    events
        .send(AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: answer,
            phase: Some(MessagePhase::FinalAnswer),
            citations: Vec::new(),
        }))
        .unwrap();
    drop(events);

    drain_completed_agent_events(&mut ready, |event| view.handle_agent_event(event));
    assert!(view.has_pending_presentation());
    view.advance_presentation(Instant::now());
    assert!(view.has_pending_presentation());
    view.flush_presentation();

    let rendered = view
        .take_pending_history_lines(WIDTH, HEIGHT)
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(rendered.contains("tail-marker"), "{rendered}");
}

#[test]
fn unavailable_agent_restores_an_idle_submission() {
    let mut runtime = runtime_without_agent();
    let action = composer_action(&mut runtime.view, "keep this draft", KeyCode::Enter);

    assert!(!runtime.handle_action(action));
    assert_eq!(
        submitted_prompt(runtime.view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))),
        UserPrompt::text("keep this draft"),
    );
    let rendered = rendered_history(&mut runtime.view);
    assert!(
        rendered.contains("■ Could not start turn: the active agent is unavailable"),
        "{rendered}"
    );
}

#[test]
fn failed_queued_follow_up_start_restores_the_prompt_and_reports_no_start() {
    let mut runtime = runtime_without_agent();
    runtime
        .view
        .queue_follow_up(UserPrompt::text("keep this queued prompt"));

    assert!(!runtime.start_next_queued_follow_up());
    assert_eq!(
        submitted_prompt(runtime.view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))),
        UserPrompt::text("keep this queued prompt")
    );
    let rendered = rendered_history(&mut runtime.view);
    assert!(
        rendered.contains("■ Could not start turn: the active agent is unavailable"),
        "{rendered}"
    );
}

#[test]
fn failed_fork_and_clear_restore_the_command_draft() {
    let mut runtime = runtime_without_agent();
    let fork = composer_action(&mut runtime.view, "/fork", KeyCode::Enter);
    assert!(!runtime.handle_action(fork));
    assert!(matches!(
        runtime.view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))),
        Action::Fork(_)
    ));
    let rendered = rendered_history(&mut runtime.view);
    assert!(
        rendered.contains("■ Could not fork this session:"),
        "{rendered}"
    );

    let mut runtime = runtime_without_agent();
    runtime.cwd = PathBuf::from(format!(
        "/tmp/bettercodex-missing-session-{}",
        Uuid::new_v4()
    ));
    let clear = composer_action(&mut runtime.view, "/clear", KeyCode::Enter);
    assert!(!runtime.handle_action(clear));
    assert!(matches!(
        runtime.view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))),
        Action::Clear(_)
    ));
    let rendered = rendered_history(&mut runtime.view);
    assert!(
        rendered.contains("■ Could not start a fresh session:"),
        "{rendered}"
    );
}

#[test]
fn resume_and_skill_failures_are_rendered_without_exiting() {
    let mut runtime = runtime_without_agent();
    let resume = composer_action(
        &mut runtime.view,
        "/resume 123e4567-e89b-12d3-a456-426614174000",
        KeyCode::Enter,
    );
    assert!(!runtime.handle_action(resume));
    assert!(!runtime.handle_action(Action::UpdateSkill {
        path: PathBuf::from("/tmp/missing/SKILL.md"),
        update: SkillUpdate::Enabled(false),
    }));

    let rendered = rendered_history(&mut runtime.view);
    assert!(
        rendered.contains("■ Could not resume this session:"),
        "{rendered}"
    );
    assert!(rendered.contains("■ Could not update skill:"), "{rendered}");
}

#[tokio::test]
async fn closing_the_resume_picker_cancels_the_scan_and_restores_the_command_draft() {
    for key in [
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('\u{3}'), KeyModifiers::NONE),
    ] {
        let mut runtime = runtime_without_agent();
        let open = composer_action(&mut runtime.view, "/resume", KeyCode::Enter);
        assert!(!runtime.handle_action(open));
        assert!(runtime.session_scan.is_some());

        let close = runtime.view.handle_terminal_event(Event::Key(key));
        assert_eq!(close, Action::CloseResumePicker);
        assert!(!runtime.handle_action(close));
        assert!(runtime.session_scan.is_none());

        assert!(matches!(
            runtime.view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::OpenResumePicker(_)
        ));
    }
}

#[tokio::test]
async fn same_session_resume_consumes_the_command_and_closes_the_picker() {
    let mut runtime = runtime_without_agent();
    let target = Uuid::parse_str(&runtime.session_id).unwrap();
    let open = composer_action(&mut runtime.view, "/resume", KeyCode::Enter);
    assert!(!runtime.handle_action(open));
    assert!(runtime.session_scan.is_some());

    runtime.complete_same_session_resume(target);

    assert!(runtime.session_scan.is_none());
    assert!(runtime.resume_submission.is_none());
    assert_eq!(
        runtime.view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))),
        Action::None
    );
    let rendered = rendered_history(&mut runtime.view);
    assert!(
        rendered.contains(&format!("Already viewing bettercodex session {target}")),
        "{rendered}"
    );
}

#[tokio::test]
async fn resume_progress_can_be_cancelled_without_switching_or_losing_the_draft() {
    const DRAFT: &str = "/resume";
    let target = Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").unwrap();

    for key in [
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('\u{3}'), KeyModifiers::NONE),
    ] {
        let mut runtime = runtime_without_agent();
        let original_session = runtime.session_id.clone();
        let open = composer_action(&mut runtime.view, DRAFT, KeyCode::Enter);
        assert!(!runtime.handle_action(open));
        runtime.view.show_resume_progress(target);

        let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_completed = std::sync::Arc::clone(&completed);
        let task_release = std::sync::Arc::clone(&release);
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        runtime.resume_task = Some(tokio::task::spawn_blocking(move || {
            started_tx.send(()).expect("resume task start signal");
            while !task_release.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            task_completed.store(true, std::sync::atomic::Ordering::Release);
            Err(anyhow::anyhow!("late cancelled resume result"))
        }));
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("resume task should start");

        let progress = rendered_view(&mut runtime.view);
        assert!(progress.contains("esc cancel"), "{progress}");
        assert!(progress.contains("ctrl+c cancel"), "{progress}");

        let action = runtime.view.handle_terminal_event(Event::Key(key));
        assert_eq!(action, Action::CancelResumeLoad);
        assert!(!runtime.handle_action(action));
        assert!(runtime.resume_task.is_none());
        release.store(true, std::sync::atomic::Ordering::Release);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !completed.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("cancelled blocking resume should finish without leaking its result");

        assert_eq!(runtime.session_id, original_session);
        let resumed_view = rendered_view(&mut runtime.view);
        assert!(!resumed_view.contains("Resuming"));
        assert!(!resumed_view.contains("Resume a previous session"));
        assert!(!resumed_view.contains("late cancelled resume result"));
        assert!(matches!(
            runtime.view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::OpenResumePicker(_)
        ));
    }
}

#[tokio::test]
async fn operator_command_streams_and_escape_cancels_its_process_tree() {
    let mut runtime = runtime_without_agent();
    runtime.cwd = std::env::current_dir().unwrap();
    let action = composer_action(
        &mut runtime.view,
        "!printf 'local-live-marker\\n'; exec sleep 30",
        KeyCode::Enter,
    );
    assert!(!runtime.handle_action(action));

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = runtime
                .operator_command_updates
                .recv()
                .await
                .expect("operator command update channel");
            let contains_marker = matches!(
                &update,
                OperatorCommandUpdate::Output { chunk, .. }
                    if chunk.contains("local-live-marker")
            );
            runtime.apply_operator_command_update(update);
            if contains_marker {
                break;
            }
        }
    })
    .await
    .expect("operator command should stream before sleeping");

    let live = rendered_view(&mut runtime.view);
    assert!(live.contains("local-live-marker"), "{live}");
    let cancel = runtime
        .view
        .handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert_eq!(cancel, Action::Cancel);
    assert!(!runtime.handle_action(cancel));

    tokio::time::timeout(Duration::from_secs(5), async {
        while !runtime.operator_command_tasks.is_empty() {
            let update = runtime
                .operator_command_updates
                .recv()
                .await
                .expect("operator command completion");
            runtime.apply_operator_command_update(update);
        }
    })
    .await
    .expect("cancelled operator command should finish promptly");

    let transcript = runtime.view.session_transcript();
    let tool = transcript
        .iter()
        .find_map(|item| match item {
            SessionTranscriptItem::Tool { tool } => Some(tool),
            _ => None,
        })
        .expect("operator command transcript cell");
    assert_eq!(tool.origin, SessionTranscriptToolOrigin::Operator);
    let Some(SessionTranscriptToolOutput::Success(output)) = &tool.output else {
        panic!("operator command should retain its structured output");
    };
    assert_eq!(output.get("exit_code").and_then(Value::as_i64), Some(130));
    assert_eq!(runtime.operator_command_cancellations.len(), 0);
    let context = runtime
        .pending_operator_contexts
        .front()
        .expect("cancelled operator output should remain pending without an agent");
    assert!(context.contains("<user_shell_command>"), "{context}");
    assert!(context.contains("Exit code: 130"), "{context}");

    let rendered = rendered_history(&mut runtime.view);
    assert!(rendered.contains("You ran printf"), "{rendered}");
    assert!(rendered.contains("local-live-marker"), "{rendered}");
}

#[tokio::test]
async fn cancel_interrupts_operator_command_even_while_an_agent_turn_is_active() {
    let mut runtime = runtime_without_agent();
    runtime.cwd = std::env::current_dir().unwrap();
    runtime.start_operator_command("exec sleep 30".to_string());
    runtime.turn = TurnTaskState::Running(tokio::spawn(std::future::pending::<(
        Agent,
        TurnCompletion,
    )>()));
    let (turn_handle, _turn_control) = crate::agent::TurnControl::channel();
    runtime.turn_handle = Some(turn_handle);

    assert!(!runtime.handle_action(Action::Cancel));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !runtime.operator_command_tasks.is_empty() {
            let update = runtime
                .operator_command_updates
                .recv()
                .await
                .expect("operator command completion");
            runtime.apply_operator_command_update(update);
        }
    })
    .await
    .expect("concurrent operator command should be interrupted promptly");

    let exit_code = runtime
        .view
        .session_transcript()
        .iter()
        .find_map(|item| match item {
            SessionTranscriptItem::Tool { tool } => {
                tool.output.as_ref().and_then(|output| match output {
                    SessionTranscriptToolOutput::Success(output) => {
                        output.get("exit_code").and_then(Value::as_i64)
                    }
                    SessionTranscriptToolOutput::Error(_) => None,
                })
            }
            _ => None,
        });
    assert_eq!(exit_code, Some(130));
    let [(_, context)] = runtime.operator_context_steers.as_slice() else {
        panic!("active operator output should be retained as model-context steering");
    };
    assert!(context.contains("Exit code: 130"), "{context}");
}

#[tokio::test]
async fn quit_cancels_and_waits_for_an_active_operator_command() {
    let mut runtime = runtime_without_agent();
    runtime.cwd = std::env::current_dir().unwrap();
    runtime.start_operator_command("printf 'quit-live-marker\\n'; exec sleep 30".to_string());

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = runtime
                .operator_command_updates
                .recv()
                .await
                .expect("operator command update");
            let started = matches!(
                &update,
                OperatorCommandUpdate::Output { chunk, .. }
                    if chunk.contains("quit-live-marker")
            );
            runtime.apply_operator_command_update(update);
            if started {
                break;
            }
        }
    })
    .await
    .expect("operator command should start before quit");

    assert!(!runtime.handle_action(Action::Quit));
    assert!(runtime.exit_after_work);
    assert!(!runtime.exit_ready());

    tokio::time::timeout(Duration::from_secs(5), async {
        while !runtime.operator_command_tasks.is_empty() {
            let update = runtime
                .operator_command_updates
                .recv()
                .await
                .expect("operator command completion");
            runtime.apply_operator_command_update(update);
        }
    })
    .await
    .expect("quit should interrupt the operator command promptly");

    assert!(runtime.exit_ready());
    let exit_code = runtime
        .view
        .session_transcript()
        .iter()
        .find_map(|item| match item {
            SessionTranscriptItem::Tool { tool } => {
                tool.output.as_ref().and_then(|output| match output {
                    SessionTranscriptToolOutput::Success(output) => {
                        output.get("exit_code").and_then(Value::as_i64)
                    }
                    SessionTranscriptToolOutput::Error(_) => None,
                })
            }
            _ => None,
        });
    assert_eq!(exit_code, Some(130));
}

#[tokio::test]
async fn user_message_during_operator_command_is_queued_until_local_work_finishes() {
    let mut runtime = runtime_without_agent();
    runtime
        .view
        .start_operator_command("operator:busy".to_string(), "sleep 30");
    runtime.operator_command_tasks.insert(
        "operator:busy".to_string(),
        tokio::spawn(std::future::pending()),
    );

    let action = composer_action(
        &mut runtime.view,
        "continue after the command",
        KeyCode::Enter,
    );
    assert!(!runtime.handle_action(action));

    let rendered = rendered_view(&mut runtime.view);
    assert!(rendered.contains("Queued follow-up inputs"), "{rendered}");
    assert!(
        rendered.contains("continue after the command"),
        "{rendered}"
    );
    assert_eq!(
        runtime.view.pop_next_queued_follow_up(),
        Some(UserPrompt::text("continue after the command"))
    );
}

#[test]
fn tab_never_submits_or_queues_plain_composer_text() {
    for active in [false, true] {
        let mut runtime = runtime_without_agent();
        if active {
            runtime.view.start_turn(&UserPrompt::text("active turn"));
        }

        let action = composer_action(&mut runtime.view, "keep this draft", KeyCode::Tab);

        assert_eq!(action, Action::None);
        assert_eq!(runtime.view.pop_next_queued_follow_up(), None);
        let rendered = rendered_view(&mut runtime.view);
        assert!(rendered.contains("keep this draft"), "{rendered}");
    }
}

#[tokio::test]
async fn local_activity_defers_and_restores_session_commands() {
    const DRAFT: &str = "/resume 123e4567-e89b-12d3-a456-426614174000";
    let mut runtime = runtime_without_agent();
    runtime.operator_command_tasks.insert(
        "operator:busy".to_string(),
        tokio::spawn(std::future::pending()),
    );
    let action = composer_action(&mut runtime.view, DRAFT, KeyCode::Enter);

    assert!(!runtime.handle_action(action));
    let restored = runtime.view.handle_terminal_event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    let Action::ResumeSessionFromComposer { id, .. } = restored else {
        panic!("restored resume command should remain executable");
    };
    assert_eq!(
        id,
        Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").unwrap()
    );
    let rendered = rendered_history(&mut runtime.view);
    assert!(
        rendered.contains("• Wait for the local command or Git diff before resuming"),
        "{rendered}"
    );
}
