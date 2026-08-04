mod context_window;
mod editor;
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
use anyhow::Context;
use anyhow::Result;
use crossterm::event::EventStream;
use futures::StreamExt;
use std::collections::VecDeque;
use std::future::pending;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use tokio::time::Interval;
use tokio::time::MissedTickBehavior;
use view::Action;
use view::View;

type TurnResult = (Agent, Result<SubmitOutcome>);
type TurnTask = JoinHandle<TurnResult>;

pub(crate) async fn run(agent: Agent, cwd: PathBuf) -> Result<()> {
    let mut runtime = Runtime::new(agent, cwd);
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
    view: View,
}

impl Runtime {
    fn new(agent: Agent, cwd: PathBuf) -> Self {
        let mut view = View::new(&cwd);
        view.set_context_tokens(agent.context_tokens());
        let context_snapshot = agent.context_snapshot();
        Self {
            view,
            cwd,
            agent: Some(agent),
            turn: None,
            turn_events: None,
            turn_handle: None,
            queued: VecDeque::new(),
            exit_after_turn: false,
            context_snapshot,
        }
    }

    async fn event_loop(&mut self, terminal: &mut terminal::AppTerminal) -> Result<()> {
        let mut input = EventStream::new();
        let mut ticks = tokio::time::interval(Duration::from_millis(32));
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut dirty = true;

        loop {
            if dirty {
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
                let height = self.view.desired_height(width, screen_height);
                terminal.insert_history_lines(history, height)?;
                terminal.draw(height, |frame| self.view.render(frame))?;
                dirty = false;
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
                    dirty = true;
                    if self.handle_action(action)? {
                        break;
                    }
                }
                event = receive_agent_event(&mut self.turn_events) => {
                    if let Some(event) = event {
                        if let AgentEvent::ContextUpdated(snapshot) = &event {
                            self.context_snapshot = snapshot.clone();
                        }
                        self.view.handle_agent_event(event);
                        dirty = true;
                    } else {
                        self.turn_events = None;
                    }
                }
                completion = receive_turn_completion(&mut self.turn) => {
                    let (agent, result) = completion.context("agent task stopped unexpectedly")?;
                    self.turn = None;
                    self.drain_agent_events();
                    self.context_snapshot = agent.context_snapshot();
                    self.agent = Some(agent);
                    self.turn_handle = None;
                    self.view.finish_turn(result);
                    dirty = true;

                    if self.exit_after_turn {
                        break;
                    }
                    if let Some(prompt) = self.queued.pop_front() {
                        self.view.set_queued(self.queued.len());
                        self.start_turn(prompt);
                    }
                }
                _ = receive_animation_tick(animate, &mut ticks) => dirty = true,
            }
        }
        Ok(())
    }

    fn handle_action(&mut self, action: Action) -> Result<bool> {
        match action {
            Action::None => {}
            Action::Submit(prompt) => {
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
            Action::Queue(prompt) => {
                if self.turn.is_some() {
                    self.queued.push_back(prompt);
                    self.view.set_queued(self.queued.len());
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
        if let Some(events) = self.turn_events.as_mut() {
            while let Ok(event) = events.try_recv() {
                self.view.handle_agent_event(event);
            }
        }
        self.turn_events = None;
    }
}

async fn receive_animation_tick(animate: bool, ticks: &mut Interval) {
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
