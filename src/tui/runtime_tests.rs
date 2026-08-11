use super::*;
use crate::assistant_message::AssistantMessage;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use crate::events::AgentEvent;
use crate::protocol::MessagePhase;
use crate::skills::SkillUpdate;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
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
        turn: None,
        turn_events: None,
        turn_handle: None,
        exit_after_turn: false,
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
        processes: ProcessManager::new(cwd.clone()),
        model_selection: ModelSelection::default(),
        service_tier: ServiceTier::default(),
        session_scan: None,
        resume_task: None,
        notifier: None,
        operator_command_tasks: HashMap::new(),
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
        Action::Submit(submission) | Action::Queue(submission) => submission.into_prompt(),
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
            .send(AgentEvent::ModelMessageDelta("x".to_string()))
            .unwrap();
    }
    events
        .send(AgentEvent::ModelMessageDelta(" tail-marker".to_string()))
        .unwrap();
    events
        .send(AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: answer,
            phase: Some(MessagePhase::FinalAnswer),
        }))
        .unwrap();
    drop(events);

    drain_completed_agent_events(&mut ready, |event| view.handle_agent_event(event));

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
fn session_action_failures_are_rendered_without_exiting() {
    let mut runtime = runtime_without_agent();

    let fork = composer_action(&mut runtime.view, "/fork", KeyCode::Enter);
    assert!(!runtime.handle_action(fork));
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
        rendered.contains("■ Could not fork this session:"),
        "{rendered}"
    );
    assert!(
        rendered.contains("■ Could not resume this session:"),
        "{rendered}"
    );
    assert!(rendered.contains("■ Could not update skill:"), "{rendered}");
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
    let Action::ResumeSession { id, .. } = restored else {
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
