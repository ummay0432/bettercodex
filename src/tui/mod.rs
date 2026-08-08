mod bottom_pane;
mod clipboard;
mod clipboard_paste;
mod context_window;
mod editor;
mod file_search;
mod git_diff;
mod markdown;
mod markdown_cache;
mod markdown_render;
mod markdown_style;
mod markdown_text_merge;
mod notifications;
mod palette;
mod pending_input;
mod reasoning_status;
mod render;
mod resume_picker;
#[cfg(test)]
#[path = "review_tests.rs"]
mod review_tests;
mod skill_popup;
mod skills_view;
mod table_detect;
mod terminal;
mod terminal_hyperlinks;
mod terminal_title;
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
use crate::tools::ProcessManager;
use crate::update::AvailableUpdate;
use anyhow::Context;
use anyhow::Result;
use clipboard::ClipboardLease;
use crossterm::event::Event;
use crossterm::event::EventStream;
use file_search::FileSearchManager;
use file_search::FileSearchUpdate;
use futures_util::StreamExt;
use notifications::Notifier;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::future::pending;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use terminal_title::TerminalTitle;
use terminal_title::TerminalTitleState;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use tokio::time::Interval;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;
use view::Action;
use view::InterruptIntent;
use view::View;

type TurnResult = (Agent, TurnCompletion);
type TurnTask = JoinHandle<TurnResult>;
type SessionScanTask = JoinHandle<Result<Vec<SessionSummary>>>;
type ResumeTask = JoinHandle<Result<ResumedSession>>;
type UpdateCheckTask = JoinHandle<Option<AvailableUpdate>>;
const FRAME_INTERVAL: Duration = Duration::from_millis(32);
const PROCESS_STATUS_INTERVAL: Duration = Duration::from_millis(500);
const LONG_TASK_NOTIFICATION_THRESHOLD: Duration = Duration::from_secs(5);
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

pub(crate) async fn run(
    agent: Agent,
    cwd: PathBuf,
    worker_handoff: Option<crate::managed_session::WorkerHandoff>,
) -> Result<()> {
    let mut runtime = Runtime::new(agent, cwd, worker_handoff)?;
    let mut session = terminal::TerminalSession::enter()?;
    runtime
        .view
        .set_terminal_colors(session.default_foreground(), session.default_background());
    let result = runtime.event_loop(&mut session).await;
    drop(session);
    result
}

struct Runtime {
    clipboard_lease: Option<ClipboardLease>,
    cwd: PathBuf,
    agent: Option<Agent>,
    turn: Option<TurnTask>,
    turn_events: Option<UnboundedReceiver<AgentEvent>>,
    turn_handle: Option<TurnHandle>,
    exit_after_turn: bool,
    context_snapshot: ContextSnapshot,
    diff_task: Option<JoinHandle<()>>,
    diff_updates: UnboundedReceiver<std::result::Result<String, String>>,
    diff_updates_tx: tokio::sync::mpsc::UnboundedSender<std::result::Result<String, String>>,
    file_search: FileSearchManager,
    file_search_updates: UnboundedReceiver<FileSearchUpdate>,
    prompt_history: Option<PromptHistory>,
    processes: ProcessManager,
    session_scan: Option<SessionScanTask>,
    resume_task: Option<ResumeTask>,
    notifier: Option<Notifier>,
    operator_command_tasks: HashMap<String, JoinHandle<()>>,
    operator_command_updates: UnboundedReceiver<OperatorCommandCompletion>,
    operator_command_updates_tx: tokio::sync::mpsc::UnboundedSender<OperatorCommandCompletion>,
    terminal_focused: bool,
    terminal_title: TerminalTitle,
    turn_started_at: Option<Instant>,
    update_check: Option<UpdateCheckTask>,
    update_check_started: bool,
    worker_handoff: Option<crate::managed_session::WorkerHandoff>,
    view: View,
}

struct ResumedSession {
    agent: Agent,
    prompt_history: PromptHistory,
    composer_history: Vec<String>,
    transcript: Vec<SessionTranscriptItem>,
}

struct OperatorCommandCompletion {
    call_id: String,
    output: std::result::Result<Value, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveSubmissionRoute {
    QueueNextTurn,
    SteerOrdinary,
}

fn active_submission_route(prompt: &UserPrompt) -> Result<ActiveSubmissionRoute> {
    let text = prompt.text_without_image_placeholders();
    let invocation =
        crate::quality_loop::parse_invocation_with_mode(&text, prompt.image_count() > 0, true)?;
    let invokes_review = prompt
        .skill_mentions()
        .iter()
        .any(|mention| mention.selection().name() == "review")
        || crate::skills::explicitly_invokes_review(&text);
    Ok(if invocation.is_some() || invokes_review {
        ActiveSubmissionRoute::QueueNextTurn
    } else {
        ActiveSubmissionRoute::SteerOrdinary
    })
}

impl Runtime {
    fn new(
        mut agent: Agent,
        cwd: PathBuf,
        worker_handoff: Option<crate::managed_session::WorkerHandoff>,
    ) -> Result<Self> {
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
        let processes = agent.background_processes();
        let (file_search_updates_tx, file_search_updates) = unbounded_channel();
        let file_search = FileSearchManager::new(cwd.clone(), file_search_updates_tx);
        let (operator_command_updates_tx, operator_command_updates) = unbounded_channel();
        let (diff_updates_tx, diff_updates) = unbounded_channel();
        Ok(Self {
            clipboard_lease: None,
            view,
            cwd,
            agent: Some(agent),
            turn: None,
            turn_events: None,
            turn_handle: None,
            exit_after_turn: false,
            context_snapshot,
            diff_task: None,
            diff_updates,
            diff_updates_tx,
            file_search,
            file_search_updates,
            prompt_history: Some(prompt_history),
            processes,
            session_scan: None,
            resume_task: None,
            notifier: Some(Notifier::detect()),
            operator_command_tasks: HashMap::new(),
            operator_command_updates,
            operator_command_updates_tx,
            terminal_focused: true,
            terminal_title: TerminalTitle::new(),
            turn_started_at: None,
            update_check: None,
            update_check_started: false,
            worker_handoff,
        })
    }

    async fn event_loop(&mut self, session: &mut terminal::TerminalSession) -> Result<()> {
        let mut input = EventStream::new();
        let mut ticks =
            tokio::time::interval_at(tokio::time::Instant::now() + FRAME_INTERVAL, FRAME_INTERVAL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut process_ticks = tokio::time::interval(PROCESS_STATUS_INTERVAL);
        process_ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut redraw = true;

        loop {
            let title_state = if self.view.action_required() {
                TerminalTitleState::ActionRequired
            } else if self.has_foreground_activity() {
                TerminalTitleState::Working
            } else {
                TerminalTitleState::Idle
            };
            self.terminal_title.refresh(title_state)?;
            if redraw {
                let terminal = session.terminal_mut();
                let clear_requested = self.view.take_clear_request();
                let mut resize_reflow_requested = self.view.take_resize_reflow_request();
                let width = terminal.width()?;
                let screen_height = terminal.height()?;
                resize_reflow_requested |= self.view.streamed_history_needs_reflow(width);
                if clear_requested || resize_reflow_requested {
                    terminal.clear_screen()?;
                }
                let mut history = if resize_reflow_requested && !clear_requested {
                    self.view.history_lines_for_resize_reflow(width)
                } else {
                    self.view.take_pending_history_lines(width)
                };
                let mut prepared = self.view.prepare(width, screen_height);
                history.extend(prepared.take_history_lines());
                let height = prepared.height();
                terminal.insert_history_lines(history, height)?;
                terminal.draw(height, |frame| self.view.render_prepared(frame, prepared))?;
                redraw = false;
                self.start_update_check_after_startup();
            }
            let animate = self.has_foreground_activity();

            tokio::select! {
                terminal_event = input.next() => {
                    let Some(terminal_event) = terminal_event else {
                        self.cancel_turn(InterruptIntent::StopTurn);
                        break;
                    };
                    let event = terminal_event.context("failed to read terminal input")?;
                    match event {
                        Event::FocusGained => self.terminal_focused = true,
                        Event::FocusLost => self.terminal_focused = false,
                        _ => {}
                    }
                    let action = self.view.handle_terminal_event(event);
                    self.file_search
                        .on_query_changed(self.view.file_search_query());
                    redraw = true;
                    if matches!(action, Action::EnterTmux) {
                        self.enter_tmux(session).await;
                        continue;
                    }
                    if matches!(action, Action::Logout) {
                        match crate::login::logout().await {
                            Ok(_) => break,
                            Err(error) => self.view.add_notice(format!("Logout failed: {error:#}")),
                        }
                        continue;
                    }
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
                completion = receive_update_check(&mut self.update_check) => {
                    self.update_check = None;
                    if let Ok(Some(update)) = completion {
                        self.view.add_update_available(update);
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
                    let elapsed = self.turn_started_at.take().map(|started| started.elapsed());
                    let notification = match &completion {
                        TurnCompletion::Submission(Ok(SubmitOutcome::Completed(answer))) => {
                            Some(answer.clone())
                        }
                        TurnCompletion::Compaction(Ok(CompactionOutcome::Completed)) => {
                            Some("Context compacted".to_string())
                        }
                        _ => None,
                    };
                    let completed = completion.completed();
                    let steering_after_interrupt = match completion {
                        TurnCompletion::Submission(result) => self.view.finish_turn(result),
                        TurnCompletion::Compaction(result) => {
                            self.view.finish_compaction(result);
                            None
                        }
                    };
                    redraw = true;

                    if self.exit_after_turn {
                        break;
                    }
                    if completed {
                        if let Some(prompt) = self.view.pop_next_queued_follow_up() {
                            self.start_turn(prompt);
                        } else if let (Some(message), Some(elapsed)) = (notification, elapsed)
                            && should_notify_turn_completion(
                                self.terminal_focused,
                                elapsed,
                            )
                        {
                            self.post_notification(&message);
                        }
                    } else if let Some(prompt) = steering_after_interrupt {
                        self.start_turn(prompt);
                    } else {
                        self.view.restore_pending_input_to_composer();
                    }
                }
                completion = self.operator_command_updates.recv() => {
                    if let Some(completion) = completion {
                        self.operator_command_tasks.remove(&completion.call_id);
                        self.view.finish_operator_command(
                            &completion.call_id,
                            completion.output,
                        );
                        redraw = true;
                    }
                }
                result = self.diff_updates.recv() => {
                    if let Some(result) = result {
                        self.diff_task = None;
                        self.view.add_git_diff_result(result);
                        redraw = true;
                    }
                }
                _ = process_ticks.tick() => {
                    redraw |= self.refresh_background_processes();
                }
                _ = receive_frame_tick(animate, &mut ticks) => redraw = true,
            }
        }
        Ok(())
    }

    async fn enter_tmux(&mut self, session: &mut terminal::TerminalSession) {
        if crate::managed_session::is_tmux_active() {
            self.view
                .add_notice("This session is already running in tmux".to_string());
            return;
        }
        if self.worker_handoff.is_none() {
            self.view.tmux_handoff_failed(
                "Could not move this session into tmux: the interactive supervisor is unavailable",
            );
            return;
        }
        let size = {
            let terminal = session.terminal_mut();
            terminal.width().and_then(|width| {
                terminal
                    .height()
                    .map(|height| (width.max(1), height.max(1)))
            })
        };
        let size = match size {
            Ok(size) => size,
            Err(error) => {
                self.view.tmux_handoff_failed(format!(
                    "Could not move this session into tmux: failed to read terminal size: {error:#}"
                ));
                return;
            }
        };
        let cwd = self.cwd.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            crate::managed_session::prepare_tmux_session(&cwd, size)
        })
        .await;
        let prepared = match prepared {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => {
                self.view.tmux_handoff_failed(format!(
                    "Could not move this session into tmux: {error:#}"
                ));
                return;
            }
            Err(error) => {
                self.view.tmux_handoff_failed(format!(
                    "Could not move this session into tmux: relay setup task failed: {error}"
                ));
                return;
            }
        };

        let worker_handoff = self
            .worker_handoff
            .as_mut()
            .expect("the interactive supervisor was checked before tmux setup");
        match session.migrate_to_tmux(prepared, worker_handoff) {
            Ok(session_name) => {
                self.worker_handoff = None;
                self.notifier = Some(Notifier::detect());
                self.view.tmux_handoff_succeeded(&session_name);
            }
            Err(error) => self
                .view
                .tmux_handoff_failed(format!("Could not move this session into tmux: {error:#}")),
        }
        self.view.request_terminal_reflow();
    }

    fn handle_action(&mut self, action: Action) -> Result<bool> {
        match action {
            Action::None => {}
            Action::Submit(prompt) => {
                self.persist_prompt(&prompt.text_without_image_placeholders());
                if self.turn.is_some() {
                    match active_submission_route(&prompt) {
                        Ok(ActiveSubmissionRoute::QueueNextTurn) => {
                            self.view.queue_follow_up(prompt)
                        }
                        Err(error) => self
                            .view
                            .add_notice(format!("Invalid quality loop request: {error:#}")),
                        Ok(ActiveSubmissionRoute::SteerOrdinary) => {
                            let steering = self.turn_handle.as_ref().and_then(|turn| {
                                turn.steer(UserInput::prompt(prompt.clone())).ok()
                            });
                            match steering {
                                Some(id) => self.view.add_pending_steer(id, prompt),
                                None => self.view.queue_follow_up(prompt),
                            }
                        }
                    }
                } else {
                    self.start_turn(prompt);
                }
            }
            Action::Queue(prompt) => {
                self.persist_prompt(&prompt.text_without_image_placeholders());
                if self.turn.is_some() {
                    self.view.queue_follow_up(prompt);
                } else {
                    self.start_turn(prompt);
                }
            }
            Action::Cancel => self.interrupt_turn(),
            Action::Copy(text) => match clipboard::copy_to_clipboard(&text) {
                Ok(lease) => {
                    self.clipboard_lease = lease;
                    self.view
                        .add_notice("Copied latest final response as Markdown".to_string());
                }
                Err(error) => self
                    .view
                    .add_notice(format!("Could not copy final response: {error}")),
            },
            Action::Compact => self.start_compaction(),
            Action::Fork => {
                if self.has_local_session_activity() {
                    self.view.add_notice(
                        "Wait for the local command or Git diff before forking".to_string(),
                    );
                } else {
                    self.fork_session()?;
                }
            }
            Action::ListBackgroundProcesses => {
                let processes = self.processes.list_background_processes();
                self.view.set_background_processes(processes.clone());
                self.view.add_background_process_list(processes);
            }
            Action::Clear => {
                if self.has_local_session_activity() {
                    self.view.add_notice(
                        "Wait for the local command or Git diff before clearing".to_string(),
                    );
                } else if self.turn.is_none() {
                    let agent = Agent::new(&self.cwd)?;
                    let prompt_history = PromptHistory::open(agent.session_id())?;
                    let processes = agent.background_processes();
                    self.processes.stop_all_background_processes();
                    self.processes = processes;
                    self.context_snapshot = agent.context_snapshot();
                    self.agent = Some(agent);
                    self.prompt_history = Some(prompt_history);
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
            Action::OpenResumePicker => {
                if self.has_local_session_activity() {
                    self.view.add_notice(
                        "Wait for the local command or Git diff before resuming".to_string(),
                    );
                } else {
                    self.open_resume_picker()?;
                }
            }
            Action::ResumeSession(id) => {
                if self.has_local_session_activity() {
                    self.view.add_notice(
                        "Wait for the local command or Git diff before resuming".to_string(),
                    );
                } else {
                    self.start_resume(id)?;
                }
            }
            Action::RunShellCommand {
                command,
                history_text,
            } => {
                self.persist_prompt(&history_text);
                self.start_operator_command(command);
            }
            Action::ShowContext => {
                if let Some(agent) = &self.agent {
                    self.context_snapshot = agent.context_snapshot();
                }
                self.view.show_context(self.context_snapshot.clone());
            }
            Action::ShowDiff => self.start_git_diff(),
            Action::StopBackgroundProcesses => {
                let count = self.processes.stop_all_background_processes();
                self.view.set_background_processes(Vec::new());
                let plural = if count == 1 { "" } else { "s" };
                self.view
                    .add_notice(format!("Stopped {count} background terminal{plural}"));
            }
            Action::EnterTmux => unreachable!("tmux handoffs are handled by the event loop"),
            Action::Logout => unreachable!("logout is handled by the event loop"),
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
                    self.cancel_turn(InterruptIntent::StopTurn);
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

    fn fork_session(&mut self) -> Result<()> {
        let source = self
            .agent
            .as_ref()
            .context("a session can only be forked while the agent is idle")?;
        let agent = source.fork(self.view.session_transcript())?;
        let prompt_history = PromptHistory::open(agent.session_id())?;
        let session_id = agent.session_id().to_string();
        self.processes.stop_all_background_processes();
        self.processes = agent.background_processes();
        self.context_snapshot = agent.context_snapshot();
        self.view.set_context_tokens(agent.context_tokens());
        self.view.set_skills(agent.skills().to_vec());
        self.view.set_background_processes(Vec::new());
        self.agent = Some(agent);
        self.prompt_history = Some(prompt_history);
        self.view
            .add_notice(format!("Forked conversation into session {session_id}"));
        Ok(())
    }

    fn open_resume_picker(&mut self) -> Result<()> {
        self.view.show_resume_picker();
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
        self.view.show_resume_progress(target);
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

    fn start_operator_command(&mut self, command: String) {
        let call_id = format!("operator:{}", uuid::Uuid::new_v4());
        self.view.start_operator_command(call_id.clone(), &command);
        let processes = self.processes.clone();
        let updates = self.operator_command_updates_tx.clone();
        let task_call_id = call_id.clone();
        let task = tokio::spawn(async move {
            let output = processes
                .run_operator_command(command)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = updates.send(OperatorCommandCompletion {
                call_id: task_call_id,
                output,
            });
        });
        self.operator_command_tasks.insert(call_id, task);
    }

    fn start_git_diff(&mut self) {
        if self.diff_task.is_some() {
            self.view
                .add_notice("A Git diff is already being computed".to_string());
            return;
        }
        let cwd = self.cwd.clone();
        let updates = self.diff_updates_tx.clone();
        self.diff_task = Some(tokio::spawn(async move {
            let result = git_diff::get_git_diff(cwd).await;
            let _ = updates.send(result);
        }));
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
        let processes = agent.background_processes();
        let (file_search_updates_tx, file_search_updates) = unbounded_channel();
        let file_search = FileSearchManager::new(cwd.clone(), file_search_updates_tx);

        self.cwd = cwd.clone();
        self.processes.stop_all_background_processes();
        self.processes = processes;
        self.context_snapshot = context_snapshot;
        self.agent = Some(agent);
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
        let invocation = match crate::quality_loop::parse_invocation_with_mode(
            &prompt.text_without_image_placeholders(),
            prompt.image_count() > 0,
            true,
        ) {
            Ok(invocation) => invocation,
            Err(error) => {
                self.view
                    .add_notice(format!("Invalid quality loop request: {error:#}"));
                return;
            }
        };
        let mut agent = self
            .agent
            .take()
            .expect("an idle runtime always owns its agent");
        let (events_tx, events_rx) = unbounded_channel();
        let (turn_handle, turn_control) = if invocation.is_some() {
            crate::agent::TurnControl::non_steerable_channel()
        } else {
            crate::agent::TurnControl::channel()
        };
        self.view.start_turn(prompt.clone());
        self.turn_started_at = Some(Instant::now());
        self.turn_events = Some(events_rx);
        self.turn_handle = Some(turn_handle);
        self.turn = Some(tokio::spawn(async move {
            let input = UserInput::prompt(prompt);
            let result = if let Some(invocation) = invocation {
                crate::quality_loop::submit_with_control(
                    &mut agent,
                    input,
                    invocation,
                    events_tx,
                    turn_control,
                )
                .await
            } else {
                agent
                    .submit_with_control(input, events_tx, turn_control)
                    .await
            };
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
        self.turn_started_at = Some(Instant::now());
        self.turn_events = Some(events_rx);
        self.turn_handle = Some(turn_handle);
        self.turn = Some(tokio::spawn(async move {
            let result = agent.compact_with_control(events_tx, turn_control).await;
            (agent, TurnCompletion::Compaction(result))
        }));
    }

    fn cancel_turn(&mut self, intent: InterruptIntent) {
        if let Some(turn) = &self.turn_handle {
            turn.cancel();
            self.view.set_interrupting(intent);
        }
    }

    fn interrupt_turn(&mut self) {
        let intent = if self.view.has_pending_steers() {
            InterruptIntent::SubmitSteering
        } else {
            InterruptIntent::StopTurn
        };
        self.cancel_turn(intent);
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

    fn refresh_background_processes(&mut self) -> bool {
        self.view
            .set_background_processes(self.processes.list_background_processes())
    }

    fn has_foreground_activity(&self) -> bool {
        self.view.is_busy()
            || !self.operator_command_tasks.is_empty()
            || self.diff_task.is_some()
            || self.resume_task.is_some()
            || self.session_scan.is_some()
    }

    fn has_local_session_activity(&self) -> bool {
        !self.operator_command_tasks.is_empty() || self.diff_task.is_some()
    }

    fn post_notification(&mut self, message: &str) {
        let Some(notifier) = self.notifier.as_mut() else {
            return;
        };
        if let Err(error) = notifier.notify_turn_complete(message) {
            self.notifier = None;
            self.view.add_notice(format!(
                "Terminal notifications were disabled after an output error: {error}"
            ));
        }
    }

    fn start_update_check_after_startup(&mut self) {
        if self.update_check_started {
            return;
        }
        self.update_check_started = true;
        self.update_check = Some(tokio::spawn(crate::update::check_for_update()));
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(turn) = self.turn_handle.take() {
            turn.cancel();
        }
        abort_join_task(&mut self.turn);
        abort_join_task(&mut self.session_scan);
        abort_join_task(&mut self.resume_task);
        abort_join_task(&mut self.update_check);
        for (_, task) in self.operator_command_tasks.drain() {
            task.abort();
        }
        abort_join_task(&mut self.diff_task);
        self.processes.stop_all_background_processes();
    }
}

fn abort_join_task<T>(task: &mut Option<JoinHandle<T>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

fn should_notify_turn_completion(terminal_focused: bool, elapsed: Duration) -> bool {
    !terminal_focused && elapsed >= LONG_TASK_NOTIFICATION_THRESHOLD
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

async fn receive_update_check(
    task: &mut Option<UpdateCheckTask>,
) -> std::result::Result<Option<AvailableUpdate>, tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => pending().await,
    }
}
