mod context_window;
mod editor;
mod file_search;
mod markdown;
mod markdown_cache;
mod markdown_render;
mod markdown_style;
mod markdown_text_merge;
mod palette;
mod pending_input;
mod reasoning_status;
mod render;
mod resume_picker;
mod skill_popup;
mod skills_view;
mod table_detect;
mod terminal;
mod terminal_hyperlinks;
mod tool_catalogue;
mod view;
mod width;
mod wrapping;

use crate::agent::Agent;
use crate::agent::CompactionOutcome;
use crate::agent::SubmitOutcome;
use crate::agent::TurnHandle;
use crate::context::ContextSnapshot;
use crate::events::AgentEvent;
use crate::input::UserInput;
use crate::input::UserPrompt;
use crate::prompt_history::PromptHistory;
use crate::rollout::ResumeSelector;
use crate::rollout::Rollout;
use crate::rollout::SessionSummary;
use crate::rollout::SessionTranscriptItem;
use anyhow::Context;
use anyhow::Result;
use crossterm::event::EventStream;
use file_search::FileSearchManager;
use file_search::FileSearchUpdate;
use futures::StreamExt;
use std::collections::HashSet;
use std::future::pending;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use tokio::time::Interval;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;
use view::Action;
use view::View;

type TurnResult = (Agent, TurnCompletion);
type TurnTask = JoinHandle<TurnResult>;
type SessionScanTask = JoinHandle<Result<Vec<SessionSummary>>>;
type ResumeTask = JoinHandle<Result<ResumedSession>>;
const FRAME_INTERVAL: Duration = Duration::from_millis(32);
const MAX_READY_AGENT_EVENTS: usize = 4_096;

enum TurnCompletion {
    Submission(Result<SubmitOutcome>),
    Compaction(Result<CompactionOutcome>),
}

impl TurnCompletion {
    fn completed(&self) -> bool {
        matches!(
            self,
            Self::Submission(Ok(SubmitOutcome::Completed(_)))
                | Self::Compaction(Ok(CompactionOutcome::Completed))
        )
    }
}

pub(crate) async fn run(agent: Agent, cwd: PathBuf) -> Result<()> {
    let mut runtime = Runtime::new(agent, cwd)?;
    let mut session = terminal::TerminalSession::enter()?;
    runtime
        .view
        .set_terminal_colors(session.default_foreground(), session.default_background());
    let result = runtime.event_loop(session.terminal_mut()).await;
    drop(session);
    result
}

struct Runtime {
    cwd: PathBuf,
    agent: Option<Agent>,
    turn: Option<TurnTask>,
    turn_events: Option<UnboundedReceiver<AgentEvent>>,
    turn_handle: Option<TurnHandle>,
    submit_steers_after_interrupt: bool,
    exit_after_turn: bool,
    context_snapshot: ContextSnapshot,
    file_search: FileSearchManager,
    file_search_updates: UnboundedReceiver<FileSearchUpdate>,
    prompt_history: Option<PromptHistory>,
    session_scan: Option<SessionScanTask>,
    resume_task: Option<ResumeTask>,
    view: View,
}

struct ResumedSession {
    agent: Agent,
    prompt_history: PromptHistory,
    composer_history: Vec<String>,
    transcript: Vec<SessionTranscriptItem>,
}

impl Runtime {
    fn new(mut agent: Agent, cwd: PathBuf) -> Result<Self> {
        let mut view = View::new(&cwd);
        view.replay_transcript(agent.take_resumed_transcript());
        view.set_skills(agent.skills().to_vec());
        for warning in agent.skill_warnings() {
            view.add_notice(format!("Skill warning: {warning}"));
        }
        view.set_context_tokens(agent.context_tokens());
        let (prompt_history, composer_history) = prompt_history_for_agent(&agent)?;
        view.seed_prompt_history(composer_history);
        let context_snapshot = agent.context_snapshot();
        let (file_search_updates_tx, file_search_updates) = unbounded_channel();
        let file_search = FileSearchManager::new(cwd.clone(), file_search_updates_tx);
        Ok(Self {
            view,
            cwd,
            agent: Some(agent),
            turn: None,
            turn_events: None,
            turn_handle: None,
            submit_steers_after_interrupt: false,
            exit_after_turn: false,
            context_snapshot,
            file_search,
            file_search_updates,
            prompt_history: Some(prompt_history),
            session_scan: None,
            resume_task: None,
        })
    }

    async fn event_loop(&mut self, terminal: &mut terminal::AppTerminal) -> Result<()> {
        let mut input = EventStream::new();
        let mut ticks =
            tokio::time::interval_at(tokio::time::Instant::now() + FRAME_INTERVAL, FRAME_INTERVAL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut redraw = true;

        loop {
            if redraw {
                let clear_requested = self.view.take_clear_request();
                let resize_reflow_requested = self.view.take_resize_reflow_request();
                if clear_requested || resize_reflow_requested {
                    terminal.clear_screen()?;
                }
                let width = terminal.width()?;
                let screen_height = terminal.height()?;
                let history = if resize_reflow_requested && !clear_requested {
                    self.view.history_lines_for_resize_reflow(width)
                } else {
                    self.view.take_pending_history_lines(width)
                };
                let prepared = self.view.prepare(width, screen_height);
                let height = prepared.height();
                terminal.insert_history_lines(history, height)?;
                terminal.draw(height, |frame| self.view.render_prepared(frame, prepared))?;
                redraw = false;
            }
            let animate = self.view.is_busy();

            tokio::select! {
                terminal_event = input.next() => {
                    let Some(terminal_event) = terminal_event else {
                        self.cancel_turn();
                        break;
                    };
                    let event = terminal_event.context("failed to read terminal input")?;
                    let action = self.view.handle_terminal_event(event);
                    self.file_search
                        .on_query_changed(self.view.file_search_query());
                    redraw = true;
                    if self.handle_action(action)? {
                        break;
                    }
                }
                event = receive_agent_event(&mut self.turn_events) => {
                    if let Some(event) = event {
                        // Model streams can produce deltas much faster than a terminal can paint
                        // them. Fold every ready event into this frame and let the existing
                        // animation clock bound repaint frequency instead of rendering per token.
                        self.apply_agent_event(event);
                        self.drain_agent_events();
                    } else {
                        self.turn_events = None;
                    }
                }
                update = self.file_search_updates.recv() => {
                    if let Some(update) = update {
                        self.view.handle_file_search_update(update);
                        while let Ok(update) = self.file_search_updates.try_recv() {
                            self.view.handle_file_search_update(update);
                        }
                        redraw = true;
                    }
                }
                completion = receive_session_scan(&mut self.session_scan) => {
                    self.session_scan = None;
                    match completion.context("session listing task stopped unexpectedly")? {
                        Ok(sessions) => self.view.set_resume_sessions(sessions),
                        Err(error) => self.view.resume_listing_failed(format!(
                            "Could not list saved bettercodex sessions: {error:#}"
                        )),
                    }
                    redraw = true;
                }
                completion = receive_resume_completion(&mut self.resume_task) => {
                    self.resume_task = None;
                    match completion.context("session resume task stopped unexpectedly")? {
                        Ok(session) => self.activate_resumed_session(session),
                        Err(error) => self.view.resume_failed(format!(
                            "Could not resume the selected bettercodex session: {error:#}"
                        )),
                    }
                    redraw = true;
                }
                completion = receive_turn_completion(&mut self.turn) => {
                    let (agent, completion) = completion.context("agent task stopped unexpectedly")?;
                    self.turn = None;
                    self.drain_agent_events();
                    self.turn_events = None;
                    self.context_snapshot = agent.context_snapshot();
                    self.agent = Some(agent);
                    self.turn_handle = None;
                    let completed = completion.completed();
                    match completion {
                        TurnCompletion::Submission(result) => self.view.finish_turn(result),
                        TurnCompletion::Compaction(result) => self.view.finish_compaction(result),
                    }
                    redraw = true;

                    if self.exit_after_turn {
                        break;
                    }
                    if completed {
                        self.submit_steers_after_interrupt = false;
                        if let Some(prompt) = self.view.pop_next_queued_follow_up() {
                            self.start_turn(prompt);
                        }
                    } else if self.submit_steers_after_interrupt {
                        self.submit_steers_after_interrupt = false;
                        let steers = self.view.take_pending_steers();
                        if steers.is_empty() {
                            self.view.restore_pending_input_to_composer();
                        } else {
                            self.view.add_notice(
                                "Model interrupted to submit steering input".to_string(),
                            );
                            let prompt = UserPrompt::joined(steers);
                            self.start_turn(prompt);
                        }
                    } else {
                        self.view.restore_pending_input_to_composer();
                    }
                }
                _ = receive_frame_tick(animate, &mut ticks) => redraw = true,
            }
        }
        Ok(())
    }

    fn handle_action(&mut self, action: Action) -> Result<bool> {
        match action {
            Action::None => {}
            Action::Submit(prompt) => {
                self.persist_prompt(prompt.as_str());
                if self.turn.is_some() {
                    let steering = self
                        .turn_handle
                        .as_ref()
                        .and_then(|turn| turn.steer(UserInput::prompt(prompt.clone())).ok());
                    match steering {
                        Some(id) => self.view.add_pending_steer(id, prompt),
                        None => self.view.queue_follow_up(prompt),
                    }
                } else {
                    self.start_turn(prompt);
                }
            }
            Action::Queue(prompt) => {
                self.persist_prompt(prompt.as_str());
                if self.turn.is_some() {
                    self.view.queue_follow_up(prompt);
                } else {
                    self.start_turn(prompt);
                }
            }
            Action::Cancel => self.interrupt_turn(),
            Action::Compact => self.start_compaction(),
            Action::Clear => {
                if self.turn.is_none() {
                    let agent = Agent::new(&self.cwd)?;
                    let prompt_history = PromptHistory::open(agent.session_id())?;
                    self.context_snapshot = agent.context_snapshot();
                    self.agent = Some(agent);
                    self.prompt_history = Some(prompt_history);
                    self.submit_steers_after_interrupt = false;
                    self.view.clear();
                    let agent = self
                        .agent
                        .as_ref()
                        .expect("a cleared runtime owns its replacement agent");
                    self.view.set_skills(agent.skills().to_vec());
                    for warning in agent.skill_warnings() {
                        self.view.add_notice(format!("Skill warning: {warning}"));
                    }
                }
            }
            Action::OpenResumePicker => self.open_resume_picker()?,
            Action::ResumeSession(id) => self.start_resume(id)?,
            Action::ShowContext => {
                if let Some(agent) = &self.agent {
                    self.context_snapshot = agent.context_snapshot();
                }
                self.view.show_context(self.context_snapshot.clone());
            }
            Action::UpdateSkill { path, update } => {
                let result = self
                    .agent
                    .as_mut()
                    .context("skills can only be changed while the agent is idle")?
                    .update_skill(&path, update);
                match result {
                    Ok(()) => {
                        let agent = self
                            .agent
                            .as_ref()
                            .expect("an idle runtime owns its updated agent");
                        self.context_snapshot = agent.context_snapshot();
                        self.view.set_context_tokens(agent.context_tokens());
                        self.view.set_skills(agent.skills().to_vec());
                    }
                    Err(error) => self
                        .view
                        .skill_update_failed(format!("Could not update skill: {error:#}")),
                }
            }
            Action::Quit => {
                if self.turn.is_some() {
                    self.exit_after_turn = true;
                    self.cancel_turn();
                } else {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn persist_prompt(&mut self, prompt: &str) {
        let Some(history) = self.prompt_history.as_mut() else {
            return;
        };
        if let Err(error) = history.append(prompt) {
            self.prompt_history = None;
            self.view
                .add_notice(format!("Prompt history could not be saved: {error:#}"));
        }
    }

    fn open_resume_picker(&mut self) -> Result<()> {
        let current_session = self.current_session_id()?;
        self.view.show_resume_picker(current_session);
        if self.session_scan.is_none() {
            self.session_scan = Some(tokio::task::spawn_blocking(Rollout::list_sessions));
        }
        Ok(())
    }

    fn start_resume(&mut self, target: Uuid) -> Result<()> {
        let current_session = self.current_session_id()?;
        if target == current_session {
            self.view.close_resume_picker();
            return Ok(());
        }
        if self.resume_task.is_some() {
            return Ok(());
        }
        self.view.show_resume_progress(current_session, target);
        let requested_cwd = self.cwd.clone();
        self.resume_task = Some(tokio::task::spawn_blocking(move || {
            let mut agent = Agent::resume(&requested_cwd, ResumeSelector::Id(target))?;
            let (prompt_history, composer_history) = prompt_history_for_agent(&agent)?;
            let transcript = agent.take_resumed_transcript();
            Ok(ResumedSession {
                agent,
                prompt_history,
                composer_history,
                transcript,
            })
        }));
        Ok(())
    }

    fn current_session_id(&self) -> Result<Uuid> {
        let session_id = self
            .agent
            .as_ref()
            .expect("an idle runtime always owns its agent")
            .session_id();
        Uuid::parse_str(session_id).context("the active bettercodex session ID is invalid")
    }

    fn activate_resumed_session(&mut self, session: ResumedSession) {
        let ResumedSession {
            agent,
            prompt_history,
            composer_history,
            transcript,
        } = session;
        let cwd = agent.cwd().to_path_buf();
        let context_snapshot = agent.context_snapshot();
        let context_tokens = agent.context_tokens();
        let skills = agent.skills().to_vec();
        let skill_warnings = agent.skill_warnings().to_vec();
        let (file_search_updates_tx, file_search_updates) = unbounded_channel();
        let file_search = FileSearchManager::new(cwd.clone(), file_search_updates_tx);

        self.cwd = cwd.clone();
        self.context_snapshot = context_snapshot;
        self.agent = Some(agent);
        self.submit_steers_after_interrupt = false;
        self.exit_after_turn = false;
        self.file_search = file_search;
        self.file_search_updates = file_search_updates;
        self.prompt_history = Some(prompt_history);
        self.view
            .switch_session(&cwd, context_tokens, transcript, composer_history, skills);
        for warning in skill_warnings {
            self.view.add_notice(format!("Skill warning: {warning}"));
        }
    }

    fn start_turn(&mut self, prompt: UserPrompt) {
        let mut agent = self
            .agent
            .take()
            .expect("an idle runtime always owns its agent");
        let (events_tx, events_rx) = unbounded_channel();
        let (turn_handle, turn_control) = crate::agent::TurnControl::channel();
        self.view.start_turn(prompt.clone());
        self.turn_events = Some(events_rx);
        self.turn_handle = Some(turn_handle);
        self.turn = Some(tokio::spawn(async move {
            let result = agent
                .submit_with_control(UserInput::prompt(prompt), events_tx, turn_control)
                .await;
            (agent, TurnCompletion::Submission(result))
        }));
    }

    fn start_compaction(&mut self) {
        let mut agent = self
            .agent
            .take()
            .expect("an idle runtime always owns its agent");
        let (events_tx, events_rx) = unbounded_channel();
        let (turn_handle, turn_control) = crate::agent::TurnControl::non_steerable_channel();
        self.view.start_compaction();
        self.turn_events = Some(events_rx);
        self.turn_handle = Some(turn_handle);
        self.turn = Some(tokio::spawn(async move {
            let result = agent.compact_with_control(events_tx, turn_control).await;
            (agent, TurnCompletion::Compaction(result))
        }));
    }

    fn cancel_turn(&mut self) {
        if let Some(turn) = &self.turn_handle {
            turn.cancel();
            self.view.set_interrupting();
        }
    }

    fn interrupt_turn(&mut self) {
        self.submit_steers_after_interrupt = self.view.has_pending_steers();
        self.cancel_turn();
    }

    fn drain_agent_events(&mut self) {
        let Some(mut events) = self.turn_events.take() else {
            return;
        };
        if drain_ready_agent_events(&mut events, |event| self.apply_agent_event(event))
            == ReceiverState::Open
        {
            self.turn_events = Some(events);
        }
    }

    fn apply_agent_event(&mut self, event: AgentEvent) {
        if let AgentEvent::ContextUpdated(snapshot) = &event {
            self.context_snapshot = snapshot.clone();
        }
        self.view.handle_agent_event(event);
    }
}

fn prompt_history_for_session(persistent: &[String], resumed: Vec<String>) -> Vec<String> {
    let resumed_set = resumed.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut history = persistent
        .iter()
        .filter(|prompt| !resumed_set.contains(prompt.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    drop(resumed_set);
    history.extend(resumed);
    history
}

fn prompt_history_for_agent(agent: &Agent) -> Result<(PromptHistory, Vec<String>)> {
    let prompt_history = PromptHistory::open(agent.session_id())?;
    let composer_history =
        prompt_history_for_session(prompt_history.entries(), agent.prompt_history());
    Ok((prompt_history, composer_history))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiverState {
    Open,
    Closed,
}

fn drain_ready_agent_events(
    receiver: &mut UnboundedReceiver<AgentEvent>,
    mut apply: impl FnMut(AgentEvent),
) -> ReceiverState {
    for _ in 0..MAX_READY_AGENT_EVENTS {
        match receiver.try_recv() {
            Ok(event) => apply(event),
            Err(TryRecvError::Empty) => return ReceiverState::Open,
            Err(TryRecvError::Disconnected) => return ReceiverState::Closed,
        }
    }
    // Return to `select!` so input, cancellation, and the frame clock cannot be
    // starved by an unusually large ready backlog.
    ReceiverState::Open
}

async fn receive_frame_tick(animate: bool, ticks: &mut Interval) {
    if animate {
        ticks.tick().await;
    } else {
        pending().await
    }
}

async fn receive_agent_event(
    receiver: &mut Option<UnboundedReceiver<AgentEvent>>,
) -> Option<AgentEvent> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => pending().await,
    }
}

async fn receive_turn_completion(
    turn: &mut Option<TurnTask>,
) -> std::result::Result<TurnResult, tokio::task::JoinError> {
    match turn {
        Some(turn) => turn.await,
        None => pending().await,
    }
}

async fn receive_session_scan(
    task: &mut Option<SessionScanTask>,
) -> std::result::Result<Result<Vec<SessionSummary>>, tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => pending().await,
    }
}

async fn receive_resume_completion(
    task: &mut Option<ResumeTask>,
) -> std::result::Result<Result<ResumedSession>, tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => pending().await,
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
