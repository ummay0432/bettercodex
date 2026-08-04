mod context_window;
mod editor;
mod file_search;
mod markdown;
mod reasoning_status;
mod terminal;
mod tool_catalogue;
mod view;

use crate::agent::Agent;
use crate::agent::SubmitOutcome;
use crate::agent::TurnHandle;
use crate::context::ContextSnapshot;
use crate::events::AgentEvent;
use crate::input::UserInput;
use crate::prompt_history::PromptHistory;
use anyhow::Context;
use anyhow::Result;
use crossterm::event::EventStream;
use file_search::FileSearchManager;
use file_search::FileSearchUpdate;
use futures::StreamExt;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::future::pending;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use tokio::time::Interval;
use tokio::time::MissedTickBehavior;
use view::Action;
use view::View;

type TurnResult = (Agent, Result<SubmitOutcome>);
type TurnTask = JoinHandle<TurnResult>;
const FRAME_INTERVAL: Duration = Duration::from_millis(32);
const MAX_READY_AGENT_EVENTS: usize = 4_096;

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
    queued: VecDeque<String>,
    exit_after_turn: bool,
    context_snapshot: ContextSnapshot,
    file_search: FileSearchManager,
    file_search_updates: UnboundedReceiver<FileSearchUpdate>,
    prompt_history: Option<PromptHistory>,
    view: View,
}

impl Runtime {
    fn new(agent: Agent, cwd: PathBuf) -> Result<Self> {
        let mut view = View::new(&cwd);
        view.set_context_tokens(agent.context_tokens());
        let resumed_prompts = agent.prompt_history();
        let prompt_history = PromptHistory::open(agent.session_id())?;
        view.seed_prompt_history(prompt_history_for_session(
            prompt_history.entries(),
            resumed_prompts,
        ));
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
            queued: VecDeque::new(),
            exit_after_turn: false,
            context_snapshot,
            file_search,
            file_search_updates,
            prompt_history: Some(prompt_history),
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
                completion = receive_turn_completion(&mut self.turn) => {
                    let (agent, result) = completion.context("agent task stopped unexpectedly")?;
                    self.turn = None;
                    self.drain_agent_events();
                    self.turn_events = None;
                    self.context_snapshot = agent.context_snapshot();
                    self.agent = Some(agent);
                    self.turn_handle = None;
                    self.view.finish_turn(result);
                    redraw = true;

                    if self.exit_after_turn {
                        break;
                    }
                    if let Some(prompt) = self.queued.pop_front() {
                        self.view.set_queued(self.queued.len());
                        self.start_turn(prompt);
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
                self.persist_prompt(&prompt);
                if self.turn.is_some() {
                    if self
                        .turn_handle
                        .as_ref()
                        .is_some_and(|turn| turn.steer(UserInput::text(&prompt)).is_ok())
                    {
                        self.view.add_user_message(&prompt);
                    } else {
                        self.queued.push_back(prompt);
                        self.view.set_queued(self.queued.len());
                    }
                } else {
                    self.start_turn(prompt);
                }
            }
            Action::Cancel => self.cancel_turn(),
            Action::Clear => {
                if self.turn.is_none() {
                    let agent = Agent::new(&self.cwd)?;
                    self.context_snapshot = agent.context_snapshot();
                    self.agent = Some(agent);
                    self.queued.clear();
                    self.view.clear();
                }
            }
            Action::ShowContext => {
                if let Some(agent) = &self.agent {
                    self.context_snapshot = agent.context_snapshot();
                }
                self.view.show_context(self.context_snapshot.clone());
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

    fn start_turn(&mut self, prompt: String) {
        let mut agent = self
            .agent
            .take()
            .expect("an idle runtime always owns its agent");
        let (events_tx, events_rx) = unbounded_channel();
        let (turn_handle, turn_control) = crate::agent::TurnControl::channel();
        self.view.start_turn(&prompt);
        self.turn_events = Some(events_rx);
        self.turn_handle = Some(turn_handle);
        self.turn = Some(tokio::spawn(async move {
            let result = agent
                .submit_with_control(UserInput::text(prompt), events_tx, turn_control)
                .await;
            (agent, result)
        }));
    }

    fn cancel_turn(&mut self) {
        if let Some(turn) = &self.turn_handle {
            turn.cancel();
            self.view.set_interrupting();
        }
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

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
