mod bottom_pane;
mod clipboard;
mod clipboard_paste;
mod context_window;
mod editor;
mod file_change;
mod file_search;
mod git_diff;
mod login_screen;
mod markdown;
mod markdown_cache;
mod markdown_render;
mod markdown_style;
mod markdown_text_merge;
mod model_picker;
mod notifications;
mod palette;
mod pending_input;
mod presentation;
mod render;
mod resume_picker;
#[cfg(test)]
#[path = "review_tests.rs"]
mod review_tests;
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod runtime_tests;
mod skill_popup;
mod skills_view;
mod startup_art;
#[cfg(test)]
#[path = "startup_art_tests.rs"]
mod startup_art_tests;
mod status;
#[cfg(test)]
#[path = "status_tests.rs"]
mod status_tests;
mod table_detect;
mod terminal;
mod terminal_hyperlinks;
mod terminal_title;
mod tools_view;
#[cfg(test)]
#[path = "tools_view_tests.rs"]
mod tools_view_tests;
mod view;
mod width;
mod wrapping;

use crate::agent::Agent;
use crate::agent::CompactionOutcome;
use crate::agent::SubmitOutcome;
use crate::agent::TurnHandle;
use crate::context::ContextSnapshot;
use crate::events::AgentEvent;
use crate::events::SteerId;
use crate::input::UserInput;
use crate::input::UserPrompt;
use crate::model::ModelSelection;
use crate::prompt_history::PromptHistory;
use crate::prompt_history::PromptHistoryReader;
use crate::rate_limits::RateLimitClient;
use crate::rate_limits::RateLimitSnapshot;
use crate::rollout::ResumeSelector;
use crate::rollout::Rollout;
use crate::rollout::SessionSummary;
use crate::rollout::SessionTranscriptItem;
use crate::rollout::SessionTranscriptToolOutput;
use crate::service_tier::ServiceTier;
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
use pending_input::QueuedFollowUp;
use presentation::MIN_FRAME_INTERVAL;
use serde_json::Value;
use status::StatusSnapshot;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::future::pending;
use std::path::Path;
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
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use view::Action;
use view::ComposerSubmission;
use view::InterruptIntent;
use view::View;

type TurnResult = (Agent, TurnCompletion);
type TurnTask = JoinHandle<TurnResult>;
type SessionScanTask = JoinHandle<Result<Vec<SessionSummary>>>;
type ResumeTask = JoinHandle<Result<ResumedSession>>;
type UpdateCheckTask = JoinHandle<Option<AvailableUpdate>>;
type PromptHistoryTask = JoinHandle<(PromptHistoryReader, Result<Vec<String>>)>;
type RateLimitTask = JoinHandle<Result<Vec<RateLimitSnapshot>>>;
const ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(32);
// Match Codex's settled-size recheck delay. Resize signals are reconciled with the backend
// immediately; one more sample after this quiet period catches terminals whose dimensions settle
// late.
const RESIZE_SETTLE_DELAY: Duration = Duration::from_millis(75);
const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(500);
const LONG_TASK_NOTIFICATION_THRESHOLD: Duration = Duration::from_secs(5);
// Keep ingress batching large enough to amortize channel polling, but return to `select!` before a
// burst can noticeably delay terminal input. Model events are retained in the presentation queue,
// so this is a scheduling budget rather than a throughput limit.
const MAX_READY_AGENT_EVENTS: usize = 256;
const MAX_OPERATOR_LIVE_OUTPUT_BYTES: usize = 128 * 1024;

pub(crate) struct Startup(terminal::TerminalStartup);

pub(crate) fn begin_startup() -> Result<Startup> {
    terminal::TerminalStartup::begin().map(Startup)
}

enum TurnCompletion {
    Submission(Result<SubmitOutcome>),
    Compaction(Result<CompactionOutcome>),
}

enum TurnTaskState {
    Idle,
    Running(TurnTask),
    Presenting(Box<TurnResult>),
}

impl TurnTaskState {
    fn is_active(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    fn take_presenting(&mut self) -> TurnResult {
        match std::mem::replace(self, Self::Idle) {
            Self::Presenting(completion) => *completion,
            Self::Idle | Self::Running(_) => {
                unreachable!("a ready turn always owns its deferred result")
            }
        }
    }
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
    requested_cwd: PathBuf,
    resume: Option<ResumeSelector>,
    worker_handoff: Option<crate::managed_session::WorkerHandoff>,
    startup: Startup,
    login_status: crate::login::LoginStatus,
) -> Result<()> {
    let (mut runtime, mut session) = if login_status == crate::login::LoginStatus::NotLoggedIn {
        let mut session = startup.0.enter()?;
        palette::set_terminal_colors(session.default_foreground(), session.default_background());
        if matches!(
            login_screen::run(&mut session).await?,
            login_screen::LoginScreenOutcome::Exit
        ) {
            drop(session);
            return Ok(());
        }
        session.terminal_mut().clear_screen()?;
        let loaded = load_agent(&requested_cwd, resume).await?;
        let cwd = loaded.agent.cwd().to_path_buf();
        (Runtime::new(loaded, cwd, worker_handoff)?, session)
    } else {
        let loaded = load_agent(&requested_cwd, resume).await?;
        let cwd = loaded.agent.cwd().to_path_buf();
        let runtime = Runtime::new(loaded, cwd, worker_handoff)?;
        (runtime, startup.0.enter()?)
    };
    runtime
        .view
        .set_terminal_colors(session.default_foreground(), session.default_background());
    let result = runtime.event_loop(&mut session).await;
    drop(session);
    result
}

struct LoadedAgent {
    agent: Agent,
    patch_notes: Result<Option<String>>,
}

async fn load_agent(requested_cwd: &Path, resume: Option<ResumeSelector>) -> Result<LoadedAgent> {
    let had_saved_sessions = if resume.is_none() {
        Some(
            tokio::task::spawn_blocking(Rollout::has_saved_sessions)
                .await
                .context("saved session discovery task failed")??,
        )
    } else {
        None
    };
    let agent = match resume {
        Some(selector) => {
            let requested_cwd = requested_cwd.to_path_buf();
            tokio::task::spawn_blocking(move || Agent::resume(requested_cwd, selector))
                .await
                .context("agent resume task failed")??
        }
        None => Agent::new(requested_cwd)?,
    };
    let patch_notes = had_saved_sessions.map_or(Ok(None), crate::patch_notes::for_startup);
    Ok(LoadedAgent { agent, patch_notes })
}

struct Runtime {
    clipboard_lease: Option<ClipboardLease>,
    cwd: PathBuf,
    agent: Option<Agent>,
    turn: TurnTaskState,
    turn_events: Option<UnboundedReceiver<AgentEvent>>,
    turn_handle: Option<TurnHandle>,
    exit_after_work: bool,
    context_snapshot: ContextSnapshot,
    session_id: String,
    forked_from: Option<String>,
    instruction_source_paths: Vec<PathBuf>,
    rate_limit_client: RateLimitClient,
    rate_limit_task: Option<RateLimitTask>,
    rate_limit_prefetch_started: bool,
    status_rate_limits: BTreeMap<String, RateLimitSnapshot>,
    diff_task: Option<JoinHandle<()>>,
    diff_updates: UnboundedReceiver<std::result::Result<String, String>>,
    diff_updates_tx: tokio::sync::mpsc::UnboundedSender<std::result::Result<String, String>>,
    file_search: FileSearchManager,
    file_search_updates: UnboundedReceiver<FileSearchUpdate>,
    prompt_history: Option<PromptHistory>,
    prompt_history_reader: Option<PromptHistoryReader>,
    prompt_history_task: Option<PromptHistoryTask>,
    prompt_history_exclusions: HashSet<String>,
    model_selection: ModelSelection,
    service_tier: ServiceTier,
    session_scan: Option<SessionScanTask>,
    resume_task: Option<ResumeTask>,
    notifier: Option<Notifier>,
    operator_command_tasks: HashMap<String, JoinHandle<()>>,
    operator_command_cancellations: HashMap<String, CancellationToken>,
    pending_operator_contexts: VecDeque<String>,
    operator_context_steers: Vec<(SteerId, String)>,
    operator_command_updates: UnboundedReceiver<OperatorCommandUpdate>,
    operator_command_updates_tx: tokio::sync::mpsc::UnboundedSender<OperatorCommandUpdate>,
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
    prompt_history_reader: PromptHistoryReader,
    prompt_history_exclusions: HashSet<String>,
    composer_history: Vec<String>,
    transcript: Vec<SessionTranscriptItem>,
}

struct SessionPromptHistory {
    writer: PromptHistory,
    reader: PromptHistoryReader,
    exclusions: HashSet<String>,
    composer_history: Vec<String>,
}

enum OperatorCommandUpdate {
    Output {
        call_id: String,
        chunk: String,
    },
    Completed {
        call_id: String,
        output: std::result::Result<Value, String>,
        context: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveSubmissionRoute {
    QueueNextTurn,
    SteerOrdinary,
}

fn active_submission_route(prompt: &UserPrompt) -> ActiveSubmissionRoute {
    let text = prompt.text_without_image_placeholders();
    let invokes_review = prompt
        .skill_mentions()
        .iter()
        .any(|mention| mention.selection().name() == "review")
        || crate::skills::explicitly_invokes_review(&text);
    if invokes_review {
        ActiveSubmissionRoute::QueueNextTurn
    } else {
        ActiveSubmissionRoute::SteerOrdinary
    }
}

impl Runtime {
    fn new(
        loaded: LoadedAgent,
        cwd: PathBuf,
        worker_handoff: Option<crate::managed_session::WorkerHandoff>,
    ) -> Result<Self> {
        let LoadedAgent {
            mut agent,
            patch_notes,
        } = loaded;
        let mut view = View::new(&cwd);
        let model_selection = agent.model_selection().clone();
        view.set_model_selection(model_selection.clone());
        let service_tier = agent.service_tier();
        view.set_service_tier(service_tier);
        view.replay_transcript(agent.take_resumed_transcript());
        match patch_notes {
            Ok(Some(markdown)) => view.add_patch_notes(markdown),
            Ok(None) => {}
            Err(error) => view.add_notice(format!("Patch notes could not be loaded: {error:#}")),
        }
        view.set_skills(agent.skills().to_vec());
        for warning in agent.skill_warnings() {
            view.add_notice(format!("Skill warning: {warning}"));
        }
        view.set_context_tokens(agent.context_tokens());
        let session_id = agent.session_id().to_string();
        let forked_from = agent.forked_from().map(str::to_string);
        let instruction_source_paths = agent.instruction_source_paths().to_vec();
        let rate_limit_client = agent.rate_limit_client();
        let SessionPromptHistory {
            writer: prompt_history,
            reader: prompt_history_reader,
            exclusions: mut prompt_history_exclusions,
            composer_history,
        } = prompt_history_for_agent(&agent)?;
        let has_persistent_history = prompt_history_reader.has_more();
        if !has_persistent_history {
            prompt_history_exclusions.clear();
        }
        view.seed_prompt_history(composer_history, has_persistent_history);
        let context_snapshot = agent.context_snapshot();
        let status_rate_limits = context_snapshot
            .rate_limits
            .iter()
            .cloned()
            .map(|snapshot| (snapshot.limit_id.clone(), snapshot))
            .collect();
        let (file_search_updates_tx, file_search_updates) = unbounded_channel();
        let file_search = FileSearchManager::new(cwd.clone(), file_search_updates_tx);
        let (operator_command_updates_tx, operator_command_updates) = unbounded_channel();
        let (diff_updates_tx, diff_updates) = unbounded_channel();
        Ok(Self {
            clipboard_lease: None,
            view,
            cwd,
            agent: Some(agent),
            turn: TurnTaskState::Idle,
            turn_events: None,
            turn_handle: None,
            exit_after_work: false,
            context_snapshot,
            session_id,
            forked_from,
            instruction_source_paths,
            rate_limit_client,
            rate_limit_task: None,
            rate_limit_prefetch_started: false,
            status_rate_limits,
            diff_task: None,
            diff_updates,
            diff_updates_tx,
            file_search,
            file_search_updates,
            prompt_history: Some(prompt_history),
            prompt_history_reader: Some(prompt_history_reader),
            prompt_history_task: None,
            prompt_history_exclusions,
            model_selection,
            service_tier,
            session_scan: None,
            resume_task: None,
            notifier: Some(Notifier::detect()),
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
            worker_handoff,
        })
    }

    async fn event_loop(&mut self, session: &mut terminal::TerminalSession) -> Result<()> {
        let mut input = EventStream::new();
        let mut animation_ticks = tokio::time::interval_at(
            tokio::time::Instant::now() + ANIMATION_FRAME_INTERVAL,
            ANIMATION_FRAME_INTERVAL,
        );
        animation_ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut maintenance_ticks = tokio::time::interval(MAINTENANCE_INTERVAL);
        maintenance_ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut screen_size_recheck_at = None;
        let mut stream_frames = StreamFramePacer::default();
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
                stream_frames.frame_started();
                let terminal = session.terminal_mut();
                let clear_requested = self.view.take_clear_request();
                let mut resize_reflow_requested = self.view.take_resize_reflow_request();
                let screen_size = terminal.screen_size();
                let width = screen_size.width;
                let screen_height = screen_size.height;
                resize_reflow_requested |= self.view.streamed_history_needs_reflow(width);
                let rebuild_surface = clear_requested || resize_reflow_requested;
                let mut history = if resize_reflow_requested && !clear_requested {
                    self.view
                        .history_lines_for_resize_reflow(width, screen_height)
                } else {
                    self.view.take_pending_history_lines(width, screen_height)
                };
                let mut prepared = self.view.prepare(width, screen_height);
                history.extend(prepared.take_history_lines());
                let height = prepared.height();
                terminal.synchronized_update(|terminal| {
                    if rebuild_surface {
                        terminal.clear_screen()?;
                    }
                    terminal.insert_history_lines(history, height)?;
                    terminal.draw(height, |frame| {
                        self.view.render_prepared(frame, prepared);
                    })
                })?;
                redraw = false;
                self.start_update_check_after_startup();
                self.start_rate_limit_prefetch_after_startup();
                if self.view.has_pending_presentation() {
                    // Route even an already-due follow-up frame through `select!`. Chaining draws
                    // here would bypass terminal input whenever rendering itself occupied the
                    // minimum frame interval, precisely when the UI is under the most pressure.
                    stream_frames.schedule_frame();
                }
            }
            let animate = self.has_foreground_activity();

            tokio::select! {
                terminal_event = input.next() => {
                    let Some(terminal_event) = terminal_event else {
                        self.cancel_turn(InterruptIntent::StopTurn);
                        break;
                    };
                    let event = terminal_event.context("failed to read terminal input")?;
                    match &event {
                        Event::FocusGained => self.terminal_focused = true,
                        Event::FocusLost => self.terminal_focused = false,
                        Event::Resize(_, _) => {
                            screen_size_recheck_at =
                                Some(tokio::time::Instant::now() + RESIZE_SETTLE_DELAY);
                        }
                        _ => {}
                    }
                    if matches!(
                        &event,
                        Event::FocusGained | Event::FocusLost | Event::Resize(_, _)
                    ) {
                        // A rapid grow followed by a shrink can leave both notifications queued
                        // after the terminal has already reached its final size. Rendering the
                        // stale grow dimensions into the narrower surface wraps full-width rows
                        // and scrolls the inline viewport. Treat resize and adjacent focus changes
                        // as surface-change signals, then sample one current size for the frame.
                        self.reconcile_terminal_geometry(session)?;
                    }
                    if self.exit_after_work {
                        continue;
                    }
                    let action = self.view.handle_terminal_event(event);
                    self.file_search
                        .on_query_changed(self.view.file_search_query());
                    redraw = true;
                    if matches!(action, Action::EnterTmux) {
                        self.enter_tmux(session).await?;
                        continue;
                    }
                    if matches!(action, Action::Logout) {
                        match crate::login::logout().await {
                            Ok(_) => break,
                            Err(error) => self.view.add_notice(format!("Logout failed: {error:#}")),
                        }
                        continue;
                    }
                    if self.handle_action(action) {
                        break;
                    }
                }
                event = receive_agent_event(&mut self.turn_events) => {
                    if let Some(event) = event {
                        // Ingress remains fully drained and ordered, but presentation advances at
                        // most once for each terminal frame. This keeps input responsive without
                        // collapsing a burst of model deltas into one visible plop.
                        self.apply_agent_event(event);
                        self.drain_agent_events();
                        if stream_frames.request_frame() {
                            self.view.advance_presentation(Instant::now());
                            redraw = true;
                        }
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
                completion = receive_rate_limit_refresh(&mut self.rate_limit_task) => {
                    self.rate_limit_task = None;
                    match completion {
                        Ok(Ok(snapshots)) => self.cache_status_rate_limits(snapshots),
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "account rate-limit refresh failed");
                        }
                        Err(error) => {
                            tracing::warn!(%error, "account rate-limit refresh task stopped unexpectedly");
                        }
                    }
                }
                completion = receive_prompt_history(&mut self.prompt_history_task) => {
                    self.prompt_history_task = None;
                    match completion {
                        Ok((reader, Ok(mut entries))) => {
                            let has_more = reader.has_more();
                            self.prompt_history_reader = Some(reader);
                            entries.retain(|entry| {
                                !self.prompt_history_exclusions.contains(entry)
                            });
                            if !has_more {
                                self.prompt_history_exclusions.clear();
                            }
                            if self.view.prompt_history_loaded(entries, has_more) {
                                self.start_prompt_history_load();
                            }
                        }
                        Ok((_reader, Err(error))) => {
                            self.prompt_history_reader = None;
                            self.prompt_history_exclusions.clear();
                            self.view.prompt_history_failed();
                            self.view.add_notice(format!(
                                "Older prompt history could not be loaded: {error:#}"
                            ));
                        }
                        Err(error) => {
                            self.prompt_history_reader = None;
                            self.prompt_history_exclusions.clear();
                            self.view.prompt_history_failed();
                            self.view.add_notice(format!(
                                "Older prompt history task stopped unexpectedly: {error}"
                            ));
                        }
                    }
                    redraw = true;
                }
                completion = receive_session_scan(&mut self.session_scan) => {
                    self.session_scan = None;
                    match completion {
                        Ok(Ok(sessions)) => self.view.set_resume_sessions(sessions),
                        Ok(Err(error)) => self.view.resume_listing_failed(format!(
                            "Could not list saved bettercodex sessions: {error:#}"
                        )),
                        Err(error) => self.view.resume_listing_failed(format!(
                            "Could not list saved bettercodex sessions: listing task stopped unexpectedly: {error}"
                        )),
                    }
                    redraw = true;
                }
                completion = receive_resume_completion(&mut self.resume_task) => {
                    self.resume_task = None;
                    match completion {
                        Ok(Ok(session)) => self.activate_resumed_session(session),
                        Ok(Err(error)) => self.view.resume_failed(format!(
                            "Could not resume the selected bettercodex session: {error:#}"
                        )),
                        Err(error) => self.view.resume_failed(format!(
                            "Could not resume the selected bettercodex session: resume task stopped unexpectedly: {error}"
                        )),
                    }
                    redraw = true;
                }
                completion = receive_turn_completion(&mut self.turn), if !self.view.has_pending_presentation() => {
                    let task_just_completed = completion.context("agent task stopped unexpectedly")?;
                    if task_just_completed {
                        self.drain_completed_turn_events();
                        if self.view.has_pending_presentation() {
                            self.view.advance_presentation(Instant::now());
                            redraw = true;
                            continue;
                        }
                    }
                    let (mut agent, completion) = self.turn.take_presenting();
                    self.sync_model_selection_to_agent(&mut agent);
                    self.sync_service_tier_to_agent(&mut agent);
                    self.pending_operator_contexts.extend(
                        self.operator_context_steers
                            .drain(..)
                            .map(|(_, context)| context),
                    );
                    if let Err(error) =
                        flush_operator_contexts(&mut self.pending_operator_contexts, &mut agent)
                    {
                        self.view.add_notice(format!(
                            "Operator shell output could not be added to model context: {error:#}"
                        ));
                    }
                    self.context_snapshot = agent.context_snapshot();
                    self.cache_status_rate_limits(self.context_snapshot.rate_limits.clone());
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
                    if let Err(error) = persist_session_transcript(&self.view, &mut agent) {
                        self.view.add_notice(format!(
                            "Session transcript could not be saved: {error:#}"
                        ));
                    }
                    self.agent = Some(agent);
                    redraw = true;

                    if self.exit_ready() {
                        break;
                    }
                    if self.exit_after_work {
                        continue;
                    }
                    if completed {
                        let started_follow_up = self.operator_command_tasks.is_empty()
                            && self.start_next_queued_follow_up();
                        if !started_follow_up
                            && let (Some(message), Some(elapsed)) = (notification, elapsed)
                            && should_notify_turn_completion(self.terminal_focused, elapsed)
                        {
                            self.post_notification(&message);
                        }
                    } else if let Some(prompt) = steering_after_interrupt {
                        self.start_turn(prompt);
                    } else {
                        self.view.restore_pending_input_to_composer();
                    }
                }
                update = self.operator_command_updates.recv() => {
                    if let Some(update) = update {
                        self.apply_operator_command_update(update);
                        redraw = true;
                        if self.exit_ready() {
                            break;
                        }
                    }
                }
                result = self.diff_updates.recv() => {
                    if let Some(result) = result {
                        self.diff_task = None;
                        self.view.add_git_diff_result(result);
                        redraw = true;
                    }
                }
                _ = maintenance_ticks.tick() => {
                    // Resize notifications may be coalesced or missed while a terminal rearranges
                    // panes. A cheap maintenance sample bounds stale geometry without restoring a
                    // backend query to every animation or streaming frame.
                    redraw |= self.reconcile_terminal_geometry(session)?;
                }
                _ = receive_deadline(screen_size_recheck_at) => {
                    screen_size_recheck_at = None;
                    redraw |= self.reconcile_terminal_geometry(session)?;
                }
                _ = receive_deadline(stream_frames.scheduled_at()) => {
                    stream_frames.clear_schedule();
                    self.view.advance_presentation(Instant::now());
                    redraw = true;
                }
                _ = receive_frame_tick(animate, &mut animation_ticks) => {
                    if stream_frames.request_frame() {
                        self.view.advance_presentation(Instant::now());
                        redraw = true;
                    }
                }
            }
        }
        Ok(())
    }

    fn reconcile_terminal_geometry(
        &mut self,
        session: &mut terminal::TerminalSession,
    ) -> Result<bool> {
        let changed = session.terminal_mut().refresh_screen_size()?;
        if changed {
            self.view.request_terminal_reflow();
        }
        Ok(changed)
    }

    async fn enter_tmux(&mut self, session: &mut terminal::TerminalSession) -> Result<()> {
        if crate::managed_session::is_tmux_active() {
            self.view
                .add_notice("This session is already running in tmux".to_string());
            return Ok(());
        }
        if self.worker_handoff.is_none() {
            self.view.tmux_handoff_failed(
                "Could not move this session into tmux: the interactive supervisor is unavailable",
            );
            return Ok(());
        }
        let size = {
            let terminal = session.terminal_mut();
            terminal.refresh_screen_size().map(|_| {
                let size = terminal.screen_size();
                (size.width.max(1), size.height.max(1))
            })
        };
        let size = match size {
            Ok(size) => size,
            Err(error) => {
                self.view.tmux_handoff_failed(format!(
                    "Could not move this session into tmux: failed to read terminal size: {error:#}"
                ));
                return Ok(());
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
                return Ok(());
            }
            Err(error) => {
                self.view.tmux_handoff_failed(format!(
                    "Could not move this session into tmux: relay setup task failed: {error}"
                ));
                return Ok(());
            }
        };

        let Some(worker_handoff) = self.worker_handoff.as_mut() else {
            self.view.tmux_handoff_failed(
                "Could not move this session into tmux: the interactive supervisor stopped during setup",
            );
            return Ok(());
        };
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
        // The handoff can replace the display surface. Reconcile once before replaying source-
        // backed transcript history instead of reviving per-frame geometry queries.
        session.terminal_mut().refresh_screen_size()?;
        self.view.request_terminal_reflow();
        Ok(())
    }

    fn handle_action(&mut self, action: Action) -> bool {
        if self.exit_after_work {
            return false;
        }
        match action {
            Action::None => {}
            Action::LoadPromptHistory => self.start_prompt_history_load(),
            Action::Submit(submission) => {
                if self.turn.is_active() {
                    let history_text = submission.prompt().text_without_image_placeholders();
                    match active_submission_route(submission.prompt()) {
                        ActiveSubmissionRoute::QueueNextTurn => {
                            self.persist_prompt(&history_text);
                            self.view.queue_follow_up(submission.into_prompt());
                        }
                        ActiveSubmissionRoute::SteerOrdinary => {
                            self.persist_prompt(&history_text);
                            let prompt = submission.into_prompt();
                            let steering = self
                                .turn_handle
                                .as_ref()
                                .and_then(|turn| turn.steer(UserInput::prompt_ref(&prompt)).ok());
                            match steering {
                                Some(id) => self.view.add_pending_steer(id, prompt),
                                None => self.view.queue_follow_up(prompt),
                            }
                        }
                    }
                } else if self.operator_command_tasks.is_empty() {
                    self.start_composer_turn(submission);
                } else {
                    self.queue_composer_follow_up(submission);
                }
            }
            Action::Queue(submission) => {
                self.queue_composer_follow_up(submission);
                if !self.turn.is_active() && self.operator_command_tasks.is_empty() {
                    self.start_next_queued_follow_up();
                }
            }
            Action::Cancel => self.cancel_active_work(),
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
            Action::ToggleFast => self.toggle_fast_mode(),
            Action::SelectModel(selection) => self.select_model(selection),
            Action::Fork(submission) => {
                if self.has_local_session_activity() {
                    self.view.defer_composer_action(
                        submission,
                        "Wait for the local command or Git diff before forking".to_string(),
                    );
                } else {
                    drop(submission);
                    if let Err(error) = self.fork_session() {
                        self.view
                            .add_error(format!("Could not fork this session: {error:#}"));
                    }
                }
            }
            Action::Clear(submission) => {
                if self.has_local_session_activity() {
                    self.view.defer_composer_action(
                        submission,
                        "Wait for the local command or Git diff before clearing".to_string(),
                    );
                } else if self.turn.is_active() {
                    self.view.defer_composer_action(
                        submission,
                        "Interrupt the active turn before starting a fresh session".to_string(),
                    );
                } else {
                    drop(submission);
                    if let Err(error) = self.clear_session() {
                        self.view
                            .add_error(format!("Could not start a fresh session: {error:#}"));
                    }
                }
            }
            Action::OpenResumePicker(submission) => {
                if self.has_local_session_activity() {
                    self.view.defer_composer_action(
                        submission,
                        "Wait for the local command or Git diff before resuming".to_string(),
                    );
                } else {
                    drop(submission);
                    self.open_resume_picker();
                }
            }
            Action::ResumeSession { id, submission } => {
                if self.has_local_session_activity() {
                    let notice = "Wait for the local command or Git diff before resuming";
                    if let Some(submission) = submission {
                        self.view.defer_composer_action(submission, notice);
                    } else {
                        self.view.add_notice(notice.to_string());
                    }
                } else {
                    drop(submission);
                    if let Err(error) = self.start_resume(id) {
                        self.view
                            .resume_failed(format!("Could not resume this session: {error:#}"));
                    }
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
            Action::ShowStatus => {
                if let Some(agent) = &self.agent {
                    self.context_snapshot = agent.context_snapshot();
                    self.session_id = agent.session_id().to_string();
                    self.forked_from = agent.forked_from().map(str::to_string);
                    self.instruction_source_paths = agent.instruction_source_paths().to_vec();
                }
                self.cache_status_rate_limits(self.context_snapshot.rate_limits.clone());
                let account = self.rate_limit_client.account().unwrap_or_else(|error| {
                    tracing::warn!(%error, "failed to read ChatGPT account metadata");
                    crate::auth::ChatGptAccount::default()
                });
                self.view.add_status(StatusSnapshot {
                    model: self.model_selection.clone(),
                    directory: self.cwd.clone(),
                    instruction_source_paths: self.instruction_source_paths.clone(),
                    session_id: self.session_id.clone(),
                    forked_from: self.forked_from.clone(),
                    account,
                    context: self.context_snapshot.clone(),
                    rate_limits: self.status_rate_limits.values().cloned().collect(),
                    refreshing_rate_limits: true,
                });
                self.start_rate_limit_refresh();
            }
            Action::ShowDiff => self.start_git_diff(),
            Action::EnterTmux => unreachable!("tmux handoffs are handled by the event loop"),
            Action::Logout => unreachable!("logout is handled by the event loop"),
            Action::UpdateSkill { path, update } => match self.agent.as_mut() {
                Some(agent) => {
                    let result = agent.update_skill(&path, update).map(|()| {
                        (
                            agent.context_snapshot(),
                            agent.context_tokens(),
                            agent.skills().to_vec(),
                        )
                    });
                    match result {
                        Ok((context_snapshot, context_tokens, skills)) => {
                            self.context_snapshot = context_snapshot;
                            self.view.set_context_tokens(context_tokens);
                            self.view.set_skills(skills);
                        }
                        Err(error) => self
                            .view
                            .skill_update_failed(format!("Could not update skill: {error:#}")),
                    }
                }
                None => self.view.skill_update_failed(
                    "Could not update skill: skills can only be changed while the agent is idle",
                ),
            },
            Action::Quit => return self.request_exit(),
        }
        false
    }

    fn toggle_fast_mode(&mut self) {
        let service_tier = self.service_tier.toggled();
        if let Some(agent) = self.agent.as_mut() {
            if let Err(error) = agent.set_service_tier(service_tier) {
                self.view
                    .add_error(format!("Could not change Fast mode: {error:#}"));
                return;
            }
        } else if self.turn.is_idle() {
            self.view
                .add_error("Could not change Fast mode: the active agent is unavailable");
            return;
        }
        self.service_tier = service_tier;
        self.view.set_service_tier(service_tier);
        if let Err(error) = crate::service_tier::save_default(service_tier) {
            self.view.add_error(format!(
                "Fast mode changed for this session, but the preference could not be saved: {error:#}"
            ));
        }
    }

    fn select_model(&mut self, selection: ModelSelection) {
        if let Err(error) = selection.validate() {
            self.view
                .add_error(format!("Could not change model: {error:#}"));
            return;
        }
        if let Some(agent) = self.agent.as_mut() {
            if let Err(error) = agent.set_model_selection(selection.clone()) {
                self.view
                    .add_error(format!("Could not change model: {error:#}"));
                return;
            }
        } else if self.turn.is_idle() {
            self.view
                .add_error("Could not change model: the active agent is unavailable");
            return;
        }
        self.apply_model_selection(selection.clone());
        self.view.add_notice(format!(
            "Model changed to {} {}",
            selection.model, selection.reasoning_effort
        ));
        if let Err(error) = crate::model::save_default_selection(&selection) {
            self.view.add_error(format!(
                "Model changed for this session, but the default could not be saved: {error:#}"
            ));
        }
    }

    fn apply_model_selection(&mut self, selection: ModelSelection) {
        self.model_selection = selection.clone();
        self.view.set_model_selection(selection.clone());
        self.context_snapshot.context_window = selection.effective_context_window();
        self.context_snapshot.compact_at_tokens = selection.auto_compact_token_limit();
    }

    fn sync_model_selection_to_agent(&mut self, agent: &mut Agent) {
        if agent.model_selection() == &self.model_selection {
            return;
        }
        if let Err(error) = agent.set_model_selection(self.model_selection.clone()) {
            self.model_selection = agent.model_selection().clone();
            self.view.set_model_selection(self.model_selection.clone());
            self.view
                .add_error(format!("Could not change model: {error:#}"));
        }
    }

    fn sync_service_tier_to_agent(&mut self, agent: &mut Agent) {
        if agent.service_tier() == self.service_tier {
            return;
        }
        if let Err(error) = agent.set_service_tier(self.service_tier) {
            self.service_tier = agent.service_tier();
            self.view.set_service_tier(self.service_tier);
            self.view
                .add_error(format!("Could not change Fast mode: {error:#}"));
        }
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

    fn start_prompt_history_load(&mut self) {
        if self.prompt_history_task.is_some() {
            return;
        }
        let Some(mut reader) = self.prompt_history_reader.take() else {
            self.view.prompt_history_failed();
            return;
        };
        self.prompt_history_task = Some(tokio::task::spawn_blocking(move || {
            let result = reader.read_older();
            (reader, result)
        }));
    }

    fn clear_session(&mut self) -> Result<()> {
        let mut agent = Agent::new(&self.cwd)?;
        agent.set_model_selection(self.model_selection.clone())?;
        agent.set_service_tier(self.service_tier)?;
        let prompt_history = PromptHistory::open(agent.session_id())?;
        let context_snapshot = agent.context_snapshot();
        let session_id = agent.session_id().to_string();
        let forked_from = agent.forked_from().map(str::to_string);
        let instruction_source_paths = agent.instruction_source_paths().to_vec();
        let skills = agent.skills().to_vec();
        let skill_warnings = agent.skill_warnings().to_vec();

        self.context_snapshot = context_snapshot;
        self.session_id = session_id;
        self.forked_from = forked_from;
        self.instruction_source_paths = instruction_source_paths;
        self.agent = Some(agent);
        self.prompt_history = Some(prompt_history);
        self.view.clear();
        self.view.set_skills(skills);
        for warning in skill_warnings {
            self.view.add_notice(format!("Skill warning: {warning}"));
        }
        Ok(())
    }

    fn fork_session(&mut self) -> Result<()> {
        let source = self
            .agent
            .as_ref()
            .context("a session can only be forked while the agent is idle")?;
        let agent = source.fork(self.view.session_transcript())?;
        let prompt_history = PromptHistory::open(agent.session_id())?;
        let session_id = agent.session_id().to_string();
        let forked_from = agent.forked_from().map(str::to_string);
        let instruction_source_paths = agent.instruction_source_paths().to_vec();
        self.context_snapshot = agent.context_snapshot();
        self.session_id.clone_from(&session_id);
        self.forked_from = forked_from;
        self.instruction_source_paths = instruction_source_paths;
        self.view.set_context_tokens(agent.context_tokens());
        self.view.set_skills(agent.skills().to_vec());
        self.agent = Some(agent);
        self.prompt_history = Some(prompt_history);
        self.view
            .add_notice(format!("Forked conversation into session {session_id}"));
        Ok(())
    }

    fn open_resume_picker(&mut self) {
        self.view.show_resume_picker();
        if self.session_scan.is_none() {
            self.session_scan = Some(tokio::task::spawn_blocking(Rollout::list_sessions));
        }
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
            let SessionPromptHistory {
                writer: prompt_history,
                reader: prompt_history_reader,
                exclusions: prompt_history_exclusions,
                composer_history,
            } = prompt_history_for_agent(&agent)?;
            let transcript = agent.take_resumed_transcript();
            Ok(ResumedSession {
                agent,
                prompt_history,
                prompt_history_reader,
                prompt_history_exclusions,
                composer_history,
                transcript,
            })
        }));
        Ok(())
    }

    fn start_operator_command(&mut self, command: String) {
        // Resolve agent events that were already ready when the terminal action won `select!`.
        // The view then flushes their paced presentation before inserting this local boundary.
        self.drain_agent_events();
        let call_id = format!("operator:{}", uuid::Uuid::new_v4());
        self.view.start_operator_command(call_id.clone(), &command);
        if let Some(agent) = self.agent.as_mut()
            && let Err(error) = persist_session_transcript(&self.view, agent)
        {
            self.view
                .add_notice(format!("Session transcript could not be saved: {error:#}"));
        }
        let cwd = self.cwd.clone();
        let cancellation = CancellationToken::new();
        let updates = self.operator_command_updates_tx.clone();
        let task_call_id = call_id.clone();
        let task_cancellation = cancellation.clone();
        let truncation_policy = self.model_selection.truncation_policy();
        let task = tokio::spawn(async move {
            let output_updates = updates.clone();
            let output_call_id = task_call_id.clone();
            let mut forwarded_bytes = 0_usize;
            let mut forward_output = move |_stream, mut chunk: String| {
                let omitted = crate::process_runtime::fit_live_output_budget(
                    &mut chunk,
                    &mut forwarded_bytes,
                    MAX_OPERATOR_LIVE_OUTPUT_BYTES,
                );
                if !chunk.is_empty()
                    && output_updates
                        .send(OperatorCommandUpdate::Output {
                            call_id: output_call_id.clone(),
                            chunk,
                        })
                        .is_err()
                {
                    return crate::process_runtime::LiveOutputAction::Stop;
                }
                if omitted {
                    let _ = output_updates.send(OperatorCommandUpdate::Output {
                        call_id: output_call_id.clone(),
                        chunk: "\n… additional live output omitted …\n".to_string(),
                    });
                    crate::process_runtime::LiveOutputAction::Stop
                } else {
                    crate::process_runtime::LiveOutputAction::Continue
                }
            };
            let output = crate::process_runtime::run_user_shell(
                &command,
                &cwd,
                task_cancellation,
                Some(&mut forward_output),
            )
            .await
            .map(|output| {
                serde_json::json!({
                    "stdout": output.stdout,
                    "stderr": output.stderr,
                    "exit_code": output.exit_code,
                })
            })
            .map_err(|error| format!("{error:#}"));
            let context =
                crate::context::user_shell_command_context(&command, &output, truncation_policy);
            let _ = updates.send(OperatorCommandUpdate::Completed {
                call_id: task_call_id,
                output,
                context,
            });
        });
        self.operator_command_cancellations
            .insert(call_id.clone(), cancellation);
        self.operator_command_tasks.insert(call_id, task);
    }

    fn apply_operator_command_update(&mut self, update: OperatorCommandUpdate) {
        match update {
            OperatorCommandUpdate::Output { call_id, chunk } => {
                self.view.append_operator_command_output(&call_id, &chunk);
            }
            OperatorCommandUpdate::Completed {
                call_id,
                output,
                context,
            } => {
                self.operator_command_tasks.remove(&call_id);
                self.operator_command_cancellations.remove(&call_id);
                let transcript_output = match &output {
                    Ok(output) => SessionTranscriptToolOutput::Success(output.clone()),
                    Err(error) => SessionTranscriptToolOutput::Error(error.clone()),
                };
                self.view.finish_operator_command(&call_id, output);
                self.record_operator_context(context);
                if let Some(agent) = self.agent.as_mut() {
                    // Persist the cell first in case its start checkpoint failed, then patch its
                    // bounded outcome without replacing the complete transcript snapshot.
                    if let Err(error) = persist_session_transcript(&self.view, agent) {
                        self.view.add_notice(format!(
                            "Session transcript could not be saved: {error:#}"
                        ));
                    }
                    if let Err(error) =
                        agent.persist_transcript_tool_outcome(call_id.clone(), transcript_output)
                    {
                        // The saved cell may still be incomplete. Force the next checkpoint to
                        // replace it from the view instead of appending past stale state.
                        agent.invalidate_transcript_checkpoint();
                        self.view.add_notice(format!(
                            "Session transcript could not be saved: {error:#}"
                        ));
                    }
                }
                if self.operator_command_tasks.is_empty()
                    && !self.turn.is_active()
                    && !self.exit_after_work
                {
                    self.start_next_queued_follow_up();
                }
            }
        }
    }

    fn record_operator_context(&mut self, context: String) {
        if self.turn.is_active()
            && let Some(turn) = &self.turn_handle
            && let Ok(id) = turn.inject_context(context.clone())
        {
            self.operator_context_steers.push((id, context));
            return;
        }

        self.pending_operator_contexts.push_back(context);
        let Some(mut agent) = self.agent.take() else {
            return;
        };
        if let Err(error) = flush_operator_contexts(&mut self.pending_operator_contexts, &mut agent)
        {
            self.view.add_notice(format!(
                "Operator shell output could not be added to model context: {error:#}"
            ));
        }
        self.context_snapshot = agent.context_snapshot();
        self.agent = Some(agent);
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
            .context("the active agent is unavailable")?
            .session_id();
        Uuid::parse_str(session_id).context("the active bettercodex session ID is invalid")
    }

    fn activate_resumed_session(&mut self, session: ResumedSession) {
        let ResumedSession {
            agent,
            prompt_history,
            prompt_history_reader,
            mut prompt_history_exclusions,
            composer_history,
            transcript,
        } = session;
        let model_selection = agent.model_selection().clone();
        let service_tier = agent.service_tier();
        let cwd = agent.cwd().to_path_buf();
        let context_snapshot = agent.context_snapshot();
        let session_id = agent.session_id().to_string();
        let forked_from = agent.forked_from().map(str::to_string);
        let instruction_source_paths = agent.instruction_source_paths().to_vec();
        let context_tokens = agent.context_tokens();
        let skills = agent.skills().to_vec();
        let skill_warnings = agent.skill_warnings().to_vec();
        let (file_search_updates_tx, file_search_updates) = unbounded_channel();
        let file_search = FileSearchManager::new(cwd.clone(), file_search_updates_tx);

        self.cwd = cwd.clone();
        self.context_snapshot = context_snapshot;
        self.session_id = session_id;
        self.forked_from = forked_from;
        self.instruction_source_paths = instruction_source_paths;
        self.agent = Some(agent);
        self.exit_after_work = false;
        self.file_search = file_search;
        self.file_search_updates = file_search_updates;
        self.prompt_history = Some(prompt_history);
        abort_join_task(&mut self.prompt_history_task);
        let has_persistent_history = prompt_history_reader.has_more();
        if !has_persistent_history {
            prompt_history_exclusions.clear();
        }
        self.prompt_history_reader = Some(prompt_history_reader);
        self.prompt_history_exclusions = prompt_history_exclusions;
        self.model_selection = model_selection.clone();
        self.service_tier = service_tier;
        self.view.switch_session(
            &cwd,
            context_tokens,
            transcript,
            composer_history,
            has_persistent_history,
            skills,
        );
        self.view.set_service_tier(service_tier);
        self.view.set_model_selection(model_selection);
        for warning in skill_warnings {
            self.view.add_notice(format!("Skill warning: {warning}"));
        }
    }

    fn queue_composer_follow_up(&mut self, submission: ComposerSubmission) {
        let history_text = submission
            .prompt()
            .text_without_image_placeholders()
            .into_owned();
        let shell_command = (submission.prompt().image_count() == 0)
            .then(|| history_text.trim().strip_prefix('!'))
            .flatten()
            .map(str::trim)
            .map(str::to_string);
        if shell_command.as_deref() != Some("") {
            self.persist_prompt(&history_text);
        }
        let prompt = submission.into_prompt();
        if let Some(command) = shell_command {
            self.view.queue_shell_follow_up(prompt, command);
        } else {
            self.view.queue_follow_up(prompt);
        }
    }

    fn start_next_queued_follow_up(&mut self) -> bool {
        loop {
            let Some(follow_up) = self.view.pop_next_queued_follow_up() else {
                return false;
            };
            match follow_up {
                QueuedFollowUp::Prompt(prompt) => {
                    self.start_turn(prompt);
                    return true;
                }
                QueuedFollowUp::Shell { command, .. } if command.is_empty() => {
                    self.view
                        .add_notice("Run an operator shell command with !command".to_string());
                }
                QueuedFollowUp::Shell { command, .. } => {
                    self.start_operator_command(command);
                    return true;
                }
            }
        }
    }

    fn start_composer_turn(&mut self, submission: ComposerSubmission) {
        match self.prepare_turn_start() {
            Ok(agent) => {
                let history_text = submission.prompt().text_without_image_placeholders();
                self.persist_prompt(&history_text);
                self.spawn_turn(agent, submission.into_prompt());
            }
            Err(error) => {
                self.view.reject_composer_submission(submission, error);
            }
        }
    }

    fn start_turn(&mut self, prompt: UserPrompt) {
        match self.prepare_turn_start() {
            Ok(agent) => self.spawn_turn(agent, prompt),
            Err(error) => self.view.reject_prompt(prompt, error),
        }
    }

    fn prepare_turn_start(&mut self) -> std::result::Result<Agent, String> {
        let mut agent = self
            .agent
            .take()
            .ok_or_else(|| "Could not start turn: the active agent is unavailable".to_string())?;
        if let Err(error) = flush_operator_contexts(&mut self.pending_operator_contexts, &mut agent)
        {
            self.agent = Some(agent);
            return Err(format!(
                "Could not start turn: operator shell output could not be added to model context: {error:#}"
            ));
        }
        Ok(agent)
    }

    fn spawn_turn(&mut self, mut agent: Agent, prompt: UserPrompt) {
        let (events_tx, events_rx) = unbounded_channel();
        let (turn_handle, turn_control) = crate::agent::TurnControl::channel();
        self.view.start_turn(&prompt);
        self.turn_started_at = Some(Instant::now());
        self.turn_events = Some(events_rx);
        self.turn_handle = Some(turn_handle);
        self.turn = TurnTaskState::Running(tokio::spawn(async move {
            let input = UserInput::prompt(prompt);
            let result = agent
                .submit_with_control(input, events_tx, turn_control)
                .await;
            (agent, TurnCompletion::Submission(result))
        }));
    }

    fn start_compaction(&mut self) {
        let Some(mut agent) = self.agent.take() else {
            self.view.add_notice(
                "Could not compact conversation: the active agent is unavailable".to_string(),
            );
            return;
        };
        if let Err(error) = flush_operator_contexts(&mut self.pending_operator_contexts, &mut agent)
        {
            self.agent = Some(agent);
            self.view.add_notice(format!(
                "Could not compact conversation: operator shell output could not be added to model context: {error:#}"
            ));
            return;
        }
        let (events_tx, events_rx) = unbounded_channel();
        let (turn_handle, turn_control) = crate::agent::TurnControl::non_steerable_channel();
        self.view.start_compaction();
        self.turn_started_at = Some(Instant::now());
        self.turn_events = Some(events_rx);
        self.turn_handle = Some(turn_handle);
        self.turn = TurnTaskState::Running(tokio::spawn(async move {
            let result = agent.compact_with_control(events_tx, turn_control).await;
            (agent, TurnCompletion::Compaction(result))
        }));
    }

    fn request_exit(&mut self) -> bool {
        if !self.turn.is_active() && self.operator_command_tasks.is_empty() {
            return true;
        }
        self.exit_after_work = true;
        if self.turn.is_active() {
            self.cancel_turn(InterruptIntent::StopTurn);
        }
        for cancellation in self.operator_command_cancellations.values() {
            cancellation.cancel();
        }
        false
    }

    fn exit_ready(&self) -> bool {
        self.exit_after_work && !self.turn.is_active() && self.operator_command_tasks.is_empty()
    }

    fn cancel_turn(&mut self, intent: InterruptIntent) {
        if let Some(turn) = &self.turn_handle {
            turn.cancel();
            self.view.set_interrupting(intent);
        }
    }

    fn cancel_active_work(&mut self) {
        let submitting_steering = self.turn.is_active() && self.view.has_pending_steers();
        if self.turn.is_active() {
            self.interrupt_turn();
        }
        if !submitting_steering {
            for cancellation in self.operator_command_cancellations.values() {
                cancellation.cancel();
            }
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

    fn drain_completed_turn_events(&mut self) {
        let Some(mut events) = self.turn_events.take() else {
            return;
        };
        drain_completed_agent_events(&mut events, |event| self.apply_agent_event(event));
    }

    fn apply_agent_event(&mut self, event: AgentEvent) {
        if let AgentEvent::ContextUpdated(snapshot) = &event {
            self.context_snapshot = snapshot.clone();
            self.cache_status_rate_limits(snapshot.rate_limits.clone());
        } else if let AgentEvent::SteeringCommitted(id) = &event
            && let Some(index) = self
                .operator_context_steers
                .iter()
                .position(|(candidate, _)| candidate == id)
        {
            self.operator_context_steers.remove(index);
        }
        self.view.handle_agent_event(event);
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

    fn start_rate_limit_prefetch_after_startup(&mut self) {
        if self.rate_limit_prefetch_started {
            return;
        }
        self.rate_limit_prefetch_started = true;
        self.start_rate_limit_refresh();
    }

    fn start_rate_limit_refresh(&mut self) {
        if self.rate_limit_task.is_some() {
            return;
        }
        let client = self.rate_limit_client.clone();
        self.rate_limit_task = Some(tokio::spawn(async move { client.fetch().await }));
    }

    fn cache_status_rate_limits(&mut self, snapshots: impl IntoIterator<Item = RateLimitSnapshot>) {
        for snapshot in snapshots {
            self.status_rate_limits
                .insert(snapshot.limit_id.clone(), snapshot);
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(turn) = self.turn_handle.take() {
            turn.cancel();
        }
        if let TurnTaskState::Running(task) = std::mem::replace(&mut self.turn, TurnTaskState::Idle)
        {
            task.abort();
        }
        abort_join_task(&mut self.session_scan);
        abort_join_task(&mut self.resume_task);
        abort_join_task(&mut self.update_check);
        abort_join_task(&mut self.rate_limit_task);
        abort_join_task(&mut self.prompt_history_task);
        for (_, cancellation) in self.operator_command_cancellations.drain() {
            cancellation.cancel();
        }
        for (_, task) in self.operator_command_tasks.drain() {
            task.abort();
        }
        abort_join_task(&mut self.diff_task);
    }
}

fn abort_join_task<T>(task: &mut Option<JoinHandle<T>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

fn persist_session_transcript(view: &View, agent: &mut Agent) -> Result<()> {
    let checkpoint = agent.transcript_checkpoint();
    let (mut total_items, mut items) = view.session_transcript_since(checkpoint);
    if checkpoint.is_some_and(|checkpoint| checkpoint.saturating_add(items.len()) != total_items) {
        (total_items, items) = view.session_transcript_since(None);
    }
    agent.persist_transcript(items, total_items)
}

fn should_notify_turn_completion(terminal_focused: bool, elapsed: Duration) -> bool {
    !terminal_focused && elapsed >= LONG_TASK_NOTIFICATION_THRESHOLD
}

fn flush_operator_contexts(pending: &mut VecDeque<String>, agent: &mut Agent) -> Result<()> {
    while let Some(context) = pending.pop_front() {
        if let Err(error) = agent.record_operator_shell_context(context.clone()) {
            pending.push_front(context);
            return Err(error);
        }
    }
    Ok(())
}

fn prompt_history_for_agent(agent: &Agent) -> Result<SessionPromptHistory> {
    let (writer, reader) = PromptHistory::open_with_reader(agent.session_id())?;
    let composer_history = agent.prompt_history();
    let exclusions = composer_history.iter().cloned().collect();
    Ok(SessionPromptHistory {
        writer,
        reader,
        exclusions,
        composer_history,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiverState {
    Open,
    Closed,
}

#[derive(Debug, Default)]
struct StreamFramePacer {
    last_frame_started_at: Option<tokio::time::Instant>,
    scheduled_at: Option<tokio::time::Instant>,
}

impl StreamFramePacer {
    fn frame_started(&mut self) {
        self.last_frame_started_at = Some(tokio::time::Instant::now());
        self.scheduled_at = None;
    }

    /// Request the earliest frame allowed by the stream-rate ceiling.
    ///
    /// A `true` result means the caller can draw now. Otherwise the request is retained at
    /// [`Self::scheduled_at`] so repeated deltas coalesce behind the same deadline.
    fn request_frame(&mut self) -> bool {
        let now = tokio::time::Instant::now();
        let earliest = self.earliest_frame_at(now);
        if earliest <= now {
            self.scheduled_at = None;
            true
        } else {
            self.retain_schedule(earliest);
            false
        }
    }

    /// Retain a follow-up frame request for event-loop arbitration, even when it is already due.
    fn schedule_frame(&mut self) {
        let now = tokio::time::Instant::now();
        let earliest = self.earliest_frame_at(now);
        self.retain_schedule(earliest);
    }

    fn retain_schedule(&mut self, deadline: tokio::time::Instant) {
        self.scheduled_at = Some(
            self.scheduled_at
                .map_or(deadline, |scheduled| scheduled.min(deadline)),
        );
    }

    fn earliest_frame_at(&self, now: tokio::time::Instant) -> tokio::time::Instant {
        self.last_frame_started_at
            .and_then(|started| started.checked_add(MIN_FRAME_INTERVAL))
            .unwrap_or(now)
    }

    fn scheduled_at(&self) -> Option<tokio::time::Instant> {
        self.scheduled_at
    }

    fn clear_schedule(&mut self) {
        self.scheduled_at = None;
    }
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

fn drain_completed_agent_events(
    receiver: &mut UnboundedReceiver<AgentEvent>,
    mut apply: impl FnMut(AgentEvent),
) {
    // Once the turn task has joined, every response event it emitted is already queued. The
    // normal fairness cap no longer protects input responsiveness and would discard the tail when
    // the receiver is dropped immediately after this drain.
    while let Ok(event) = receiver.try_recv() {
        apply(event);
    }
}

async fn receive_frame_tick(animate: bool, ticks: &mut Interval) {
    if animate {
        ticks.tick().await;
    } else {
        pending().await
    }
}

async fn receive_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => pending().await,
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
    turn: &mut TurnTaskState,
) -> std::result::Result<bool, tokio::task::JoinError> {
    let completion = match turn {
        TurnTaskState::Running(turn) => turn.await?,
        TurnTaskState::Presenting(_) => return Ok(false),
        TurnTaskState::Idle => return pending().await,
    };
    // Store the joined result before this future becomes ready. If another `tokio::select!`
    // branch wins while the task is still pending, dropping this future remains a no-op.
    *turn = TurnTaskState::Presenting(Box::new(completion));
    Ok(true)
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

async fn receive_rate_limit_refresh(
    task: &mut Option<RateLimitTask>,
) -> std::result::Result<Result<Vec<RateLimitSnapshot>>, tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => pending().await,
    }
}

async fn receive_prompt_history(
    task: &mut Option<PromptHistoryTask>,
) -> std::result::Result<(PromptHistoryReader, Result<Vec<String>>), tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => pending().await,
    }
}
