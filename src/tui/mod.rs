mod agent_switcher;
mod ask_user_question;
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
mod session_group;
mod skill_popup;
mod skills_view;
mod startup_art;
mod status;
mod table_detect;
mod terminal;
mod terminal_hyperlinks;
mod terminal_title;
mod tools_view;
mod view;
mod width;
mod wrapping;

use crate::agent::Agent;
use crate::agent::CompactionOutcome;
use crate::agent::SubmitOutcome;
use crate::agent::TurnHandle;
use crate::ask_user_question::AskUserQuestionRequest;
use crate::ask_user_question::AskUserQuestionRequester;
use crate::ask_user_question::AskUserQuestionResponse;
use crate::deepwork::CoordinateSpecialistArgs;
use crate::deepwork::CoordinateSpecialistResponse;
use crate::deepwork::DeepworkQuestionBatch;
use crate::deepwork::DeepworkRequest;
use crate::deepwork::DeepworkRequester;
use crate::deepwork::DeepworkStatus;
use crate::deepwork::SpecialistEvent;
use crate::deepwork::SpecialistEventKind;
use crate::deepwork::SpecialistRole;
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
use crate::rollout::SessionTranscriptToolOutput;
use crate::session_group::ChildLifecycle;
use crate::session_group::SessionId;
use crate::update::AvailableUpdate;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clipboard::ClipboardLease;
use crossterm::event::Event;
use crossterm::event::EventStream;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use file_search::FileSearchManager;
use file_search::FileSearchUpdate;
use futures_util::StreamExt;
use notifications::Notifier;
use presentation::MIN_FRAME_INTERVAL;
use serde_json::Value;
use status::StatusSnapshot;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::future::pending;
use std::ops::Deref;
use std::ops::DerefMut;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use terminal_title::TerminalTitle;
use terminal_title::TerminalTitleState;
use tokio::sync::mpsc::UnboundedReceiver;
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

use agent_switcher::AgentSwitcher;
use agent_switcher::AgentSwitcherRow;
use agent_switcher::AgentSwitcherSelection;
use agent_switcher::AgentSwitcherStatus;
use agent_switcher::move_selection;
use session_group::AgentSlot;
use session_group::SessionGroup;

type TurnResult = (Agent, TurnCompletion);
type TurnTask = JoinHandle<()>;
type SessionScanTask = JoinHandle<Result<Vec<SessionSummary>>>;
type ResumeTask = JoinHandle<Result<ResumedSession>>;
type UpdateCheckTask = JoinHandle<Option<AvailableUpdate>>;
type PromptHistoryTask = JoinHandle<(PromptHistoryReader, Result<Vec<String>>)>;
type PatchNotesAckTask = JoinHandle<Result<()>>;
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
    let foreground = session.default_foreground();
    let background = session.default_background();
    for session_id in runtime.sessions.live_session_ids() {
        if let Some(slot) = runtime.sessions.slot_mut(&session_id) {
            slot.view.set_terminal_colors(foreground, background);
        }
    }
    let result = runtime.event_loop(&mut session).await;
    drop(session);
    result
}

struct LoadedAgent {
    agent: Agent,
    patch_notes: Result<Option<crate::patch_notes::Startup>>,
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
    let patch_notes = had_saved_sessions.map_or(Ok(None), |had_saved_sessions| {
        crate::patch_notes::for_startup(had_saved_sessions).map(Some)
    });
    Ok(LoadedAgent { agent, patch_notes })
}

struct Runtime {
    clipboard_lease: Option<ClipboardLease>,
    cwd: PathBuf,
    sessions: SessionGroup,
    agent_events: UnboundedReceiver<session_group::RoutedAgentEvent>,
    turn_results: UnboundedReceiver<session_group::RoutedTurnResult>,
    deepwork_requester: DeepworkRequester,
    deepwork_requests: UnboundedReceiver<DeepworkRequest>,
    ask_user_question_requester: AskUserQuestionRequester,
    ask_user_question_requests: UnboundedReceiver<AskUserQuestionRequest>,
    pending_ask_user_question: Option<AskUserQuestionRequest>,
    switcher_selection: Option<AgentSwitcherSelection>,
    exit_after_work: bool,
    rate_limit_client: RateLimitClient,
    rate_limit_task: Option<RateLimitTask>,
    rate_limit_prefetch_started: bool,
    status_rate_limits: BTreeMap<String, RateLimitSnapshot>,
    diff_task: Option<JoinHandle<()>>,
    diff_updates: UnboundedReceiver<(SessionId, std::result::Result<String, String>)>,
    diff_updates_tx:
        tokio::sync::mpsc::UnboundedSender<(SessionId, std::result::Result<String, String>)>,
    file_search: FileSearchManager,
    file_search_updates: UnboundedReceiver<FileSearchUpdate>,
    patch_notes_startup: Option<crate::patch_notes::Startup>,
    patch_notes_ack_task: Option<PatchNotesAckTask>,
    session_scan: Option<SessionScanTask>,
    resume_task: Option<ResumeTask>,
    resume_submission: Option<ComposerSubmission>,
    notifier: Option<Notifier>,
    operator_command_tasks: HashMap<String, JoinHandle<()>>,
    operator_command_cancellations: HashMap<String, CancellationToken>,
    operator_command_owners: HashMap<String, SessionId>,
    operator_command_updates: UnboundedReceiver<OperatorCommandUpdate>,
    operator_command_updates_tx: tokio::sync::mpsc::UnboundedSender<OperatorCommandUpdate>,
    terminal_focused: bool,
    terminal_title: TerminalTitle,
    update_check: Option<UpdateCheckTask>,
    update_check_started: bool,
    worker_handoff: Option<crate::managed_session::WorkerHandoff>,
}

impl Deref for Runtime {
    type Target = AgentSlot;

    fn deref(&self) -> &Self::Target {
        self.sessions.active()
    }
}

impl DerefMut for Runtime {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.sessions.active_mut()
    }
}

struct ResumedSession {
    agent: Agent,
}

struct SessionPromptHistory {
    writer: PromptHistory,
    reader: PromptHistoryReader,
    exclusions: HashSet<String>,
    composer_history: Vec<String>,
}

enum OperatorCommandUpdate {
    Output {
        session_id: SessionId,
        call_id: String,
        chunk: String,
    },
    Completed {
        session_id: SessionId,
        call_id: String,
        output: std::result::Result<Value, String>,
        context: String,
    },
}

#[derive(Debug)]
enum InterruptedSteering {
    Operator(UserPrompt),
    Context(String),
}

#[derive(Debug)]
struct InterruptedSteeringReplay {
    leading_contexts: Vec<String>,
    first_prompt: UserPrompt,
    trailing: Vec<InterruptedSteering>,
}

fn interrupted_steering_replay(
    mut first_user_steer: (SteerId, UserPrompt),
    user_steers: Vec<(SteerId, UserPrompt)>,
    context_steers: Vec<(SteerId, String)>,
) -> InterruptedSteeringReplay {
    let mut trailing = Vec::with_capacity(user_steers.len().saturating_add(context_steers.len()));
    for user_steer in user_steers {
        if user_steer.0.0 < first_user_steer.0.0 {
            trailing.push((
                first_user_steer.0,
                InterruptedSteering::Operator(first_user_steer.1),
            ));
            first_user_steer = user_steer;
        } else {
            trailing.push((user_steer.0, InterruptedSteering::Operator(user_steer.1)));
        }
    }

    let mut leading_contexts = Vec::new();
    for (id, context) in context_steers {
        if id.0 < first_user_steer.0.0 {
            leading_contexts.push((id, context));
        } else {
            trailing.push((id, InterruptedSteering::Context(context)));
        }
    }
    leading_contexts.sort_by_key(|(id, _)| id.0);
    trailing.sort_by_key(|(id, _)| id.0);
    InterruptedSteeringReplay {
        leading_contexts: leading_contexts
            .into_iter()
            .map(|(_, context)| context)
            .collect(),
        first_prompt: first_user_steer.1,
        trailing: trailing.into_iter().map(|(_, input)| input).collect(),
    }
}

fn coordinate_response(
    action: String,
    message: impl Into<String>,
    status: DeepworkStatus,
    session_id: Option<String>,
    event: Option<SpecialistEvent>,
    state: Option<DeepworkStatus>,
) -> CoordinateSpecialistResponse {
    CoordinateSpecialistResponse {
        action,
        stage: status.stage,
        run_index: status.run_index,
        workspace: status.workspace,
        message: message.into(),
        session_id,
        event,
        state,
    }
}

fn enqueue_initial_steering(
    turn: &TurnHandle,
    view: &mut View,
    context_steers: &mut Vec<(SteerId, String)>,
    steering: Vec<InterruptedSteering>,
) -> Vec<InterruptedSteering> {
    // Queue replay before the task is spawned, so a fast model response cannot close the new turn
    // between adjacent operator and generated-context inputs.
    let mut steering = steering.into_iter();
    while let Some(input) = steering.next() {
        match input {
            InterruptedSteering::Operator(prompt) => {
                let Ok(id) = turn.steer(UserInput::prompt_ref(&prompt)) else {
                    let mut unqueued = Vec::with_capacity(steering.len().saturating_add(1));
                    unqueued.push(InterruptedSteering::Operator(prompt));
                    unqueued.extend(steering);
                    return unqueued;
                };
                view.add_pending_steer(id, prompt);
            }
            InterruptedSteering::Context(context) => {
                let Ok(id) = turn.inject_context(context.clone()) else {
                    let mut unqueued = Vec::with_capacity(steering.len().saturating_add(1));
                    unqueued.push(InterruptedSteering::Context(context));
                    unqueued.extend(steering);
                    return unqueued;
                };
                context_steers.push((id, context));
            }
        }
    }
    Vec::new()
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
        let (ask_user_question_requester, ask_user_question_requests) =
            crate::ask_user_question::channel();
        let (deepwork_requester, deepwork_requests) = crate::deepwork::channel();
        agent.set_ask_user_question_requester(ask_user_question_requester.clone());
        agent.set_deepwork_requester(deepwork_requester.clone());
        let rate_limit_client = agent.rate_limit_client();
        let mut main = AgentSlot::main(agent)?;
        let mut patch_notes_startup = match patch_notes {
            Ok(startup) => startup,
            Err(error) => {
                main.view
                    .add_notice(format!("Patch notes could not be loaded: {error:#}"));
                None
            }
        };
        if let Some(startup) = &mut patch_notes_startup
            && let Some(markdown) = startup.take_notes()
        {
            main.view.add_patch_notes(markdown);
        }
        let status_rate_limits = main
            .context_snapshot
            .rate_limits
            .iter()
            .cloned()
            .map(|snapshot| (snapshot.limit_id.clone(), snapshot))
            .collect();
        let (sessions, agent_events, turn_results) = SessionGroup::new(main)?;
        let (file_search_updates_tx, file_search_updates) = unbounded_channel();
        let file_search = FileSearchManager::new(cwd.clone(), file_search_updates_tx);
        let (operator_command_updates_tx, operator_command_updates) = unbounded_channel();
        let (diff_updates_tx, diff_updates) = unbounded_channel();
        Ok(Self {
            clipboard_lease: None,
            cwd,
            sessions,
            agent_events,
            turn_results,
            deepwork_requester,
            deepwork_requests,
            ask_user_question_requester,
            ask_user_question_requests,
            pending_ask_user_question: None,
            switcher_selection: None,
            exit_after_work: false,
            rate_limit_client,
            rate_limit_task: None,
            rate_limit_prefetch_started: false,
            status_rate_limits,
            diff_task: None,
            diff_updates,
            diff_updates_tx,
            file_search,
            file_search_updates,
            patch_notes_startup,
            patch_notes_ack_task: None,
            session_scan: None,
            resume_task: None,
            resume_submission: None,
            notifier: Some(Notifier::detect()),
            operator_command_tasks: HashMap::new(),
            operator_command_cancellations: HashMap::new(),
            operator_command_owners: HashMap::new(),
            operator_command_updates,
            operator_command_updates_tx,
            terminal_focused: true,
            terminal_title: TerminalTitle::new(),
            update_check: None,
            update_check_started: false,
            worker_handoff,
        })
    }

    fn refresh_agent_switcher(&mut self) {
        const PIPELINE: [SpecialistRole; 4] = [
            SpecialistRole::Acceptance,
            SpecialistRole::Manifest,
            SpecialistRole::Worker,
            SpecialistRole::Reviewer,
        ];

        let Some((accepted, skipped)) = self
            .sessions
            .linkage
            .deepwork
            .as_ref()
            .filter(|state| state.stage != crate::deepwork::DeepworkStage::Completed)
            .map(|state| {
                (
                    PIPELINE.map(|role| state.accepted_stages.contains_key(&role)),
                    PIPELINE.map(|role| state.skipped_stages.contains_key(&role)),
                )
            })
        else {
            self.switcher_selection = None;
            self.sessions
                .active_mut()
                .view
                .set_agent_switcher(AgentSwitcher::default());
            return;
        };

        let active_id = self.sessions.active_id().clone();
        let main_id = self.sessions.main_id().clone();
        let active_specialist = self
            .sessions
            .linkage
            .children
            .iter()
            .filter(|child| child.lifecycle.is_live())
            .find_map(|child| {
                self.sessions
                    .slot(&child.session_id)
                    .filter(|slot| slot.turn.is_active() || slot.view.is_busy())
                    .map(|_| child.session_id.clone())
            });
        let main_is_working = active_specialist.is_none()
            && self
                .sessions
                .slot(&main_id)
                .is_some_and(|slot| slot.turn.is_active() || slot.view.is_busy());

        let main = self.sessions.main();
        let main_elapsed = main
            .turn_started_at
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let mut rows = vec![AgentSwitcherRow::main(
            main_id,
            &main.model_selection,
            if main_is_working {
                AgentSwitcherStatus::Working(main_elapsed)
            } else {
                AgentSwitcherStatus::Waiting
            },
        )];

        for (index, role) in PIPELINE.into_iter().enumerate() {
            let live = self
                .sessions
                .linkage
                .children
                .iter()
                .rev()
                .find(|child| {
                    child.lifecycle.is_live()
                        && SpecialistRole::parse(&child.role).ok() == Some(role)
                })
                .map(|child| (child.session_id.clone(), child.lifecycle));
            let (session_id, status) = if let Some((session_id, lifecycle)) = live {
                let slot = self.sessions.slot(&session_id);
                let elapsed = slot
                    .and_then(|slot| slot.turn_started_at)
                    .map(|started| started.elapsed())
                    .unwrap_or_default();
                let status = match lifecycle {
                    ChildLifecycle::Cancelling => AgentSwitcherStatus::Cancelling(elapsed),
                    ChildLifecycle::Paused => AgentSwitcherStatus::Paused,
                    ChildLifecycle::AwaitingReview => AgentSwitcherStatus::AwaitingReview,
                    ChildLifecycle::Working | ChildLifecycle::Revived
                        if active_specialist.as_ref() == Some(&session_id) =>
                    {
                        AgentSwitcherStatus::Working(elapsed)
                    }
                    ChildLifecycle::Active | ChildLifecycle::Working | ChildLifecycle::Revived => {
                        AgentSwitcherStatus::Waiting
                    }
                    ChildLifecycle::Retired | ChildLifecycle::Replaced => {
                        unreachable!("retired and replaced specialist sessions are not live")
                    }
                };
                (slot.map(|_| session_id), status)
            } else if accepted[index] {
                (None, AgentSwitcherStatus::Accepted)
            } else if skipped[index] {
                (None, AgentSwitcherStatus::Skipped)
            } else {
                (None, AgentSwitcherStatus::Queued)
            };
            let selection = role.model_selection();
            rows.push(AgentSwitcherRow::specialist(
                session_id,
                &selection,
                role.as_str(),
                status,
            ));
        }

        self.sessions
            .active_mut()
            .view
            .set_agent_switcher(AgentSwitcher::new(
                rows,
                Some(active_id),
                self.switcher_selection,
            ));
    }

    fn switcher_session_id(&self, selection: AgentSwitcherSelection) -> Option<SessionId> {
        match selection {
            AgentSwitcherSelection::Main => Some(self.sessions.main_id().clone()),
            AgentSwitcherSelection::Specialist(role) => self
                .sessions
                .linkage
                .children
                .iter()
                .rev()
                .find(|child| {
                    child.lifecycle.is_live()
                        && SpecialistRole::parse(&child.role).ok() == Some(role)
                        && self.sessions.slot(&child.session_id).is_some()
                })
                .map(|child| child.session_id.clone()),
        }
    }

    fn handle_terminal_event(&mut self, event: Event) -> Option<Action> {
        if self.handle_agent_switcher_event(&event) {
            return None;
        }
        let action = self.view.handle_terminal_event(event);
        self.file_search
            .on_query_changed(self.view.file_search_query());
        Some(action)
    }

    fn handle_agent_switcher_event(&mut self, event: &Event) -> bool {
        if self.view.session_switcher_blocked() {
            self.switcher_selection = None;
            return false;
        }
        if !self.deepwork_in_progress() {
            self.switcher_selection = None;
            return false;
        }
        if self.switcher_selection.is_some() && matches!(event, Event::Paste(_)) {
            return true;
        }
        let Event::Key(key) = event else {
            return false;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return false;
        }
        let switch_key = key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && !key.modifiers.contains(KeyModifiers::ALT);
        let direction = match key.code {
            KeyCode::Up if switch_key => Some(false),
            KeyCode::Down if switch_key => Some(true),
            _ => None,
        };
        if let Some(forward) = direction {
            self.switcher_selection = move_selection(
                &AgentSwitcherSelection::ROWS,
                self.switcher_selection,
                forward,
            );
            self.refresh_agent_switcher();
            return true;
        }
        let Some(selected) = self.switcher_selection else {
            return false;
        };
        match key.code {
            KeyCode::Enter => {
                let Some(session_id) = self.switcher_session_id(selected) else {
                    return true;
                };
                self.switcher_selection = None;
                match self.sessions.activate(&session_id) {
                    Ok(()) => {
                        self.sessions.active_mut().view.request_terminal_reflow();
                        let query = self.sessions.active().view.file_search_query().to_string();
                        self.file_search.on_query_changed(&query);
                    }
                    Err(error) => self
                        .sessions
                        .active_mut()
                        .view
                        .add_notice(format!("Could not switch sessions: {error:#}")),
                }
                self.refresh_agent_switcher();
            }
            KeyCode::Esc => {
                self.switcher_selection = None;
                self.refresh_agent_switcher();
            }
            _ => {}
        }
        true
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
                self.refresh_agent_switcher();
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
                self.start_patch_notes_acknowledgement();
                self.start_update_check_after_startup();
                self.start_rate_limit_prefetch_after_startup();
                if self.view.has_pending_presentation() {
                    // Route even an already-due follow-up frame through `select!`. Chaining draws
                    // here would bypass terminal input whenever rendering itself occupied the
                    // minimum frame interval, precisely when the UI is under the most pressure.
                    stream_frames.schedule_frame();
                }
            }
            if self.finish_ready_turns()? {
                redraw = true;
                continue;
            }
            // Completion handlers finalize transcript entries and request a redraw. Let that frame
            // move the final rows into terminal history before teardown clears the mutable viewport.
            if self.exit_ready() {
                break;
            }
            let animate = self.has_foreground_activity();

            tokio::select! {
                terminal_event = input.next() => {
                    let Some(terminal_event) = terminal_event else {
                        self.cancel_all_turns(InterruptIntent::StopTurn);
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
                    let Some(action) = self.handle_terminal_event(event) else {
                        redraw = true;
                        continue;
                    };
                    redraw = true;
                    if matches!(action, Action::EnterTmux) {
                        if self.sessions.is_main_active() {
                            self.enter_tmux(session).await?;
                        } else {
                            self.view.add_notice(
                                "Switch to Main before moving the session group into tmux"
                                    .to_string(),
                            );
                        }
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
                request = self.deepwork_requests.recv() => {
                    if let Some(request) = request {
                        self.handle_deepwork_request(request);
                        redraw = true;
                    }
                }
                request = self.ask_user_question_requests.recv() => {
                    if let Some(request) = request {
                        if self.pending_ask_user_question.is_some() {
                            let _ = request.respond(AskUserQuestionResponse::cancelled());
                            self.sessions.main_mut().view.add_notice(
                                "A second AskUserQuestion request was cancelled while another was active"
                                    .to_string(),
                            );
                        } else {
                            self.switcher_selection = None;
                            let main_id = self.sessions.main_id().clone();
                            if let Err(error) = self.sessions.activate(&main_id) {
                                self.sessions.main_mut().view.add_notice(format!(
                                    "The question is ready, but Main could not be displayed: {error:#}"
                                ));
                            }
                            self.sessions.main_mut().view.show_ask_user_question(
                                request.call_id().to_string(),
                                request.arguments().clone(),
                            );
                            self.pending_ask_user_question = Some(request);
                        }
                        redraw = true;
                    }
                }
                routed = self.agent_events.recv() => {
                    if let Some((session_id, event)) = routed {
                        // AgentEvent stays session-agnostic. The supervisor that owns this slot's
                        // private event receiver attaches stable identity only at group ingress.
                        self.apply_agent_event(&session_id, event);
                        self.drain_agent_events();
                        if stream_frames.request_frame() {
                            self.view.advance_presentation(Instant::now());
                            redraw = true;
                        }
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
                completion = receive_patch_notes_acknowledgement(&mut self.patch_notes_ack_task) => {
                    self.patch_notes_ack_task = None;
                    match completion {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            self.view.add_notice(format!(
                                "Patch notes could not be marked as seen: {error:#}"
                            ));
                            redraw = true;
                        }
                        Err(error) => {
                            self.view.add_notice(format!(
                                "Patch notes could not be marked as seen: task stopped unexpectedly: {error}"
                            ));
                            redraw = true;
                        }
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
                completion = receive_prompt_history(
                    &mut self.sessions.active_mut().prompt_history_task
                ) => {
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
                        Ok(Ok(sessions)) => self.sessions.main_mut().view.set_resume_sessions(sessions),
                        Ok(Err(error)) => self.sessions.main_mut().view.resume_listing_failed(format!(
                            "Could not list saved bettercodex sessions: {error:#}"
                        )),
                        Err(error) => self.sessions.main_mut().view.resume_listing_failed(format!(
                            "Could not list saved bettercodex sessions: listing task stopped unexpectedly: {error}"
                        )),
                    }
                    redraw = true;
                }
                completion = receive_resume_completion(&mut self.resume_task) => {
                    self.resume_task = None;
                    match completion {
                        Ok(Ok(session)) => {
                            self.resume_submission = None;
                            self.activate_resumed_session(session);
                        }
                        Ok(Err(error)) => {
                            self.restore_resume_submission();
                            self.sessions.main_mut().view.resume_failed(format!(
                                "Could not resume the selected bettercodex session: {error:#}"
                            ));
                        }
                        Err(error) => {
                            self.restore_resume_submission();
                            self.sessions.main_mut().view.resume_failed(format!(
                                "Could not resume the selected bettercodex session: resume task stopped unexpectedly: {error}"
                            ));
                        }
                    }
                    redraw = true;
                }
                result = self.turn_results.recv() => {
                    if let Some((session_id, result)) = result {
                        self.sessions.install_turn_result(&session_id, result)?;
                        // The supervisor sends completion only after forwarding every ordinary
                        // event. Drain the routed ingress now; presentation still retains Main's
                        // established frame pacing before finalization.
                        self.drain_agent_events();
                        redraw = true;
                    }
                }
                update = self.operator_command_updates.recv() => {
                    if let Some(update) = update {
                        self.apply_operator_command_update(update);
                        redraw = true;
                    }
                }
                result = self.diff_updates.recv() => {
                    if let Some((session_id, result)) = result {
                        self.diff_task = None;
                        if let Some(slot) = self.sessions.slot_mut(&session_id) {
                            slot.view.add_git_diff_result(result);
                        }
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

    fn finish_ready_turns(&mut self) -> Result<bool> {
        let active_id = self.sessions.active_id().clone();
        let ready = self
            .sessions
            .live_session_ids()
            .into_iter()
            .filter(|session_id| {
                self.sessions.slot(session_id).is_some_and(|slot| {
                    matches!(slot.turn, TurnTaskState::Presenting(_))
                        && (session_id != &active_id || !slot.view.has_pending_presentation())
                })
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        for session_id in ready {
            if session_id == *self.sessions.main_id() {
                self.finish_main_turn()?;
            } else {
                self.finish_child_turn(&session_id)?;
            }
            changed = true;
        }
        Ok(changed)
    }

    fn finish_main_turn(&mut self) -> Result<()> {
        let (mut agent, completion, pending_context_steers, elapsed) = {
            let main = self.sessions.main_mut();
            let (agent, completion) = main.turn.take_presenting();
            main.turn_handle = None;
            (
                agent,
                completion,
                std::mem::take(&mut main.operator_context_steers),
                main.turn_started_at.take().map(|started| started.elapsed()),
            )
        };
        if let Some(request) = self.pending_ask_user_question.take() {
            self.sessions
                .main_mut()
                .view
                .dismiss_ask_user_question(request.call_id());
        }
        self.sync_main_model_selection_to_agent(&mut agent);
        self.sync_main_service_tier_to_agent(&mut agent);
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
        let interrupted_user_steers = {
            let view = &mut self.sessions.main_mut().view;
            match completion {
                TurnCompletion::Submission(result) => view.finish_turn(result),
                TurnCompletion::Compaction(result) => {
                    view.finish_compaction(result);
                    None
                }
            }
        };
        let mut interrupted_steering = match interrupted_user_steers {
            Some(mut user_steers) => match user_steers.pop() {
                Some(first_user_steer) => Some(interrupted_steering_replay(
                    first_user_steer,
                    user_steers,
                    pending_context_steers,
                )),
                None => {
                    self.sessions.main_mut().pending_operator_contexts.extend(
                        pending_context_steers
                            .into_iter()
                            .map(|(_, context)| context),
                    );
                    None
                }
            },
            None => {
                self.sessions.main_mut().pending_operator_contexts.extend(
                    pending_context_steers
                        .into_iter()
                        .map(|(_, context)| context),
                );
                None
            }
        };
        if let Some(replay) = interrupted_steering.as_mut() {
            self.sessions
                .main_mut()
                .pending_operator_contexts
                .extend(replay.leading_contexts.drain(..));
        }
        let flush_result = {
            let pending = &mut self.sessions.main_mut().pending_operator_contexts;
            flush_operator_contexts(pending, &mut agent)
        };
        if let Err(error) = flush_result {
            self.sessions.main_mut().view.add_notice(format!(
                "Operator shell output could not be added to model context: {error:#}"
            ));
        }
        if !self.deepwork_in_progress() {
            agent.disable_deepwork_access();
        }
        let context_snapshot = agent.context_snapshot();
        let rate_limits = context_snapshot.rate_limits.clone();
        let instruction_source_paths = agent.instruction_source_paths().to_vec();
        let skills = agent.skills().to_vec();
        {
            let main = self.sessions.main_mut();
            main.set_context_snapshot(context_snapshot);
            main.instruction_source_paths = instruction_source_paths;
            main.view.refresh_skills(&skills);
            if let Err(error) = persist_session_transcript(&main.view, &mut agent) {
                main.view
                    .add_notice(format!("Session transcript could not be saved: {error:#}"));
            }
            main.agent = Some(agent);
        }
        self.cache_status_rate_limits(rate_limits);

        if self.exit_after_work {
            return Ok(());
        }
        if completed {
            let main_id = self.sessions.main_id().clone();
            let started_follow_up = !self.has_operator_command_for(&main_id)
                && self.start_next_queued_follow_up_for(&main_id);
            if !started_follow_up
                && let (Some(message), Some(elapsed)) = (notification, elapsed)
                && should_notify_turn_completion(self.terminal_focused, elapsed)
            {
                self.post_notification(&message);
            }
        } else if let Some(replay) = interrupted_steering {
            let main_id = self.sessions.main_id().clone();
            self.start_interrupted_turn_for(&main_id, replay);
        } else {
            self.sessions
                .main_mut()
                .view
                .restore_pending_input_to_composer();
        }
        Ok(())
    }

    fn finish_child_turn(&mut self, session_id: &SessionId) -> Result<()> {
        let cancellation_requested = self
            .sessions
            .linkage
            .child(session_id)
            .is_some_and(|child| child.lifecycle == ChildLifecycle::Cancelling);
        let (mut agent, completion, role, stage_attempt) = {
            let slot = self
                .sessions
                .slot_mut(session_id)
                .context("specialist turn completed for a missing slot")?;
            slot.view.flush_presentation();
            let (agent, completion) = slot.turn.take_presenting();
            slot.turn_handle = None;
            slot.turn_started_at = None;
            let (role, stage_attempt, _) = slot
                .child_identity()
                .unwrap_or_else(|| unreachable!("child slot identity"));
            (agent, completion, role, stage_attempt)
        };
        let (lifecycle, mut event) = match completion {
            TurnCompletion::Submission(result) => {
                let event = match &result {
                    Ok(SubmitOutcome::Completed(answer)) => SpecialistEvent {
                        session_id: session_id.to_string(),
                        role,
                        stage_attempt,
                        kind: SpecialistEventKind::Completed,
                        status: ChildLifecycle::AwaitingReview,
                        message: "specialist turn completed and is awaiting orchestrator review"
                            .to_string(),
                        final_result: Some(answer.clone()),
                    },
                    Ok(SubmitOutcome::Cancelled | SubmitOutcome::CancelledBeforeProcessing) => {
                        SpecialistEvent {
                            session_id: session_id.to_string(),
                            role,
                            stage_attempt,
                            kind: SpecialistEventKind::Interrupted,
                            status: if cancellation_requested {
                                ChildLifecycle::Paused
                            } else {
                                ChildLifecycle::Active
                            },
                            message: if cancellation_requested {
                                "specialist turn was cancelled; the pipeline stage is paused"
                                    .to_string()
                            } else {
                                "specialist turn was interrupted".to_string()
                            },
                            final_result: None,
                        }
                    }
                    Err(error) => SpecialistEvent {
                        session_id: session_id.to_string(),
                        role,
                        stage_attempt,
                        kind: SpecialistEventKind::Failed,
                        status: if cancellation_requested {
                            ChildLifecycle::Paused
                        } else {
                            ChildLifecycle::Active
                        },
                        message: if cancellation_requested {
                            format!(
                                "specialist turn failed while cancellation was pending: {error:#}"
                            )
                        } else {
                            format!("specialist turn failed: {error:#}")
                        },
                        final_result: None,
                    },
                };
                let lifecycle = event.status;
                self.sessions
                    .slot_mut(session_id)
                    .unwrap_or_else(|| unreachable!("live specialist slot"))
                    .view
                    .finish_turn(result);
                (lifecycle, event)
            }
            TurnCompletion::Compaction(result) => {
                let event = match &result {
                    Ok(CompactionOutcome::Completed) => SpecialistEvent {
                        session_id: session_id.to_string(),
                        role,
                        stage_attempt,
                        kind: SpecialistEventKind::Completed,
                        status: ChildLifecycle::Active,
                        message: "specialist context compacted".to_string(),
                        final_result: None,
                    },
                    Ok(CompactionOutcome::Cancelled) => SpecialistEvent {
                        session_id: session_id.to_string(),
                        role,
                        stage_attempt,
                        kind: SpecialistEventKind::Interrupted,
                        status: if cancellation_requested {
                            ChildLifecycle::Paused
                        } else {
                            ChildLifecycle::Active
                        },
                        message: if cancellation_requested {
                            "specialist compaction was cancelled; the pipeline stage is paused"
                                .to_string()
                        } else {
                            "specialist compaction was interrupted".to_string()
                        },
                        final_result: None,
                    },
                    Err(error) => SpecialistEvent {
                        session_id: session_id.to_string(),
                        role,
                        stage_attempt,
                        kind: SpecialistEventKind::Failed,
                        status: if cancellation_requested {
                            ChildLifecycle::Paused
                        } else {
                            ChildLifecycle::Active
                        },
                        message: if cancellation_requested {
                            format!(
                                "specialist compaction failed while cancellation was pending: {error:#}"
                            )
                        } else {
                            format!("specialist compaction failed: {error:#}")
                        },
                        final_result: None,
                    },
                };
                self.sessions
                    .slot_mut(session_id)
                    .unwrap_or_else(|| unreachable!("live specialist slot"))
                    .view
                    .finish_compaction(result);
                (event.status, event)
            }
        };
        let context_snapshot = agent.context_snapshot();
        let instruction_source_paths = agent.instruction_source_paths().to_vec();
        let skills = agent.skills().to_vec();
        {
            let slot = self
                .sessions
                .slot_mut(session_id)
                .unwrap_or_else(|| unreachable!("live specialist slot"));
            slot.set_context_snapshot(context_snapshot);
            slot.instruction_source_paths = instruction_source_paths;
            slot.view.refresh_skills(&skills);
            if let Err(error) = persist_session_transcript(&slot.view, &mut agent) {
                slot.view
                    .add_notice(format!("Session transcript could not be saved: {error:#}"));
            }
            slot.agent = Some(agent);
        }
        if let Err(error) = self.sessions.set_lifecycle(session_id, lifecycle) {
            if let Some(link) = self.sessions.linkage.child_mut(session_id) {
                link.lifecycle = lifecycle;
            }
            event.kind = SpecialistEventKind::Failed;
            event.message = format!(
                "specialist turn finished, but its lifecycle could not be persisted: {error:#}"
            );
            self.sessions.main_mut().view.add_notice(format!(
                "Child session {session_id} finished, but its lifecycle could not be saved: {error:#}"
            ));
        }
        self.sessions
            .slot_mut(session_id)
            .unwrap_or_else(|| unreachable!("live specialist slot"))
            .deliver_meaningful_event(event);
        Ok(())
    }

    fn handle_deepwork_request(&mut self, request: DeepworkRequest) {
        let active_before = self.sessions.active_id().clone();
        match request {
            DeepworkRequest::Activate {
                original_task,
                response,
            } => {
                let repository_root =
                    crate::repository::find_root(&self.cwd).unwrap_or_else(|| self.cwd.clone());
                let result = self
                    .sessions
                    .activate_deepwork(&repository_root, original_task);
                let _ = response.send(result);
            }
            DeepworkRequest::Coordinate {
                arguments,
                cancellation,
                response,
            } => {
                self.handle_coordinate_specialist(arguments, cancellation, response);
            }
        }
        if self.sessions.active_id() != &active_before {
            self.switcher_selection = None;
            self.sessions.active_mut().view.request_terminal_reflow();
        }
        self.refresh_agent_switcher();
    }

    fn handle_coordinate_specialist(
        &mut self,
        arguments: CoordinateSpecialistArgs,
        cancellation: CancellationToken,
        response: tokio::sync::oneshot::Sender<Result<CoordinateSpecialistResponse>>,
    ) {
        if cancellation.is_cancelled() {
            let _ = response.send(Err(anyhow!("coordinate_specialist was interrupted")));
            return;
        }
        let action = arguments.action_name().to_string();
        if let CoordinateSpecialistArgs::Wait { session_id } = arguments {
            let result = (|| {
                let session_id = SessionId::parse(session_id)?;
                let (stage, run_index, workspace) = self.sessions.deepwork_state_fields()?;
                let (event_response, event_receiver) = tokio::sync::oneshot::channel();
                self.sessions.wait_child(&session_id, event_response);
                Ok((session_id, stage, run_index, workspace, event_receiver))
            })();
            match result {
                Ok((session_id, stage, run_index, workspace, event_receiver)) => {
                    tokio::spawn(async move {
                        let result = tokio::select! {
                            _ = cancellation.cancelled() => return,
                            event = event_receiver => match event {
                                Ok(result) => result.map(|event| CoordinateSpecialistResponse {
                                    action,
                                    stage,
                                    run_index,
                                    workspace,
                                    message: "meaningful specialist event received".to_string(),
                                    session_id: Some(session_id.to_string()),
                                    event: Some(event),
                                    state: None,
                                }),
                                Err(_) => Err(anyhow!(
                                    "specialist wait stopped before replying"
                                )),
                            }
                        };
                        let _ = response.send(result);
                    });
                }
                Err(error) => {
                    let _ = response.send(Err(error));
                }
            }
            return;
        }

        let result = (|| match arguments {
            CoordinateSpecialistArgs::Status => {
                let status = self.sessions.deepwork_status()?;
                Ok(coordinate_response(
                    action,
                    "returned canonical deepwork state",
                    status.clone(),
                    None,
                    None,
                    Some(status),
                ))
            }
            CoordinateSpecialistArgs::ApproveInterview { contract } => {
                let status = self.sessions.approve_deepwork_interview(contract)?;
                Ok(coordinate_response(
                    action,
                    "interview contract approved; `$acceptance` may start",
                    status,
                    None,
                    None,
                    None,
                ))
            }
            CoordinateSpecialistArgs::SkipManifest { reason } => {
                let status = self.sessions.skip_deepwork_manifest(reason)?;
                Ok(coordinate_response(
                    action,
                    "`$manifest` skipped; `$worker` may start",
                    status,
                    None,
                    None,
                    None,
                ))
            }
            CoordinateSpecialistArgs::Start {
                specialist,
                handoff,
            } => {
                let session_id = self
                    .sessions
                    .start_deepwork_child(&self.cwd, specialist, handoff)?;
                let status = self.sessions.deepwork_status()?;
                Ok(coordinate_response(
                    action,
                    format!("{} started", specialist.label()),
                    status,
                    Some(session_id.to_string()),
                    None,
                    None,
                ))
            }
            CoordinateSpecialistArgs::Send {
                session_id,
                message,
            } => {
                let session_id = SessionId::parse(session_id)?;
                self.sessions.send_child(&session_id, message)?;
                let status = self.sessions.deepwork_status()?;
                Ok(coordinate_response(
                    action,
                    "follow-up sent to the existing specialist session",
                    status,
                    Some(session_id.to_string()),
                    None,
                    None,
                ))
            }
            CoordinateSpecialistArgs::Cancel { session_id } => {
                let session_id = SessionId::parse(session_id)?;
                let status = self.sessions.cancel_deepwork_child(&session_id)?;
                Ok(coordinate_response(
                    action,
                    "specialist cancellation requested; the current pipeline stage remains paused",
                    status,
                    Some(session_id.to_string()),
                    None,
                    None,
                ))
            }
            CoordinateSpecialistArgs::Retire {
                session_id,
                accepted_handoff,
                artifacts,
                remaining_risks,
            } => {
                let session_id = SessionId::parse(session_id)?;
                let status = self.sessions.accept_and_retire_deepwork_child(
                    &session_id,
                    accepted_handoff,
                    artifacts,
                    remaining_risks,
                )?;
                Ok(coordinate_response(
                    action,
                    "specialist stage accepted and session retired",
                    status,
                    Some(session_id.to_string()),
                    None,
                    None,
                ))
            }
            CoordinateSpecialistArgs::Revive {
                session_id,
                message,
            } => {
                let session_id = SessionId::parse(session_id)?;
                if self
                    .sessions
                    .linkage
                    .children
                    .iter()
                    .any(|child| child.lifecycle.is_live() && child.session_id != session_id)
                {
                    return Err(anyhow!(
                        "deepwork is strictly sequential; another specialist is live"
                    ));
                }
                let role = SpecialistRole::parse(
                    &self
                        .sessions
                        .linkage
                        .child(&session_id)
                        .context("specialist session is not linked to this group")?
                        .role,
                )?;
                self.sessions
                    .revive_child(&self.cwd, &session_id, message)?;
                let status = self.sessions.deepwork_status()?;
                Ok(coordinate_response(
                    action,
                    format!("{} revived", role.label()),
                    status,
                    Some(session_id.to_string()),
                    None,
                    None,
                ))
            }
            CoordinateSpecialistArgs::Replace {
                session_id,
                message,
            } => {
                let session_id = SessionId::parse(session_id)?;
                if self
                    .sessions
                    .linkage
                    .children
                    .iter()
                    .any(|child| child.lifecycle.is_live() && child.session_id != session_id)
                {
                    return Err(anyhow!(
                        "deepwork is strictly sequential; another specialist is live"
                    ));
                }
                let role = SpecialistRole::parse(
                    &self
                        .sessions
                        .linkage
                        .child(&session_id)
                        .context("specialist session is not linked to this group")?
                        .role,
                )?;
                let replacement = self
                    .sessions
                    .replace_child(&self.cwd, &session_id, message)?;
                let status = self.sessions.deepwork_status()?;
                Ok(coordinate_response(
                    action,
                    format!("{} replaced with a fresh session", role.label()),
                    status,
                    Some(replacement.to_string()),
                    None,
                    None,
                ))
            }
            CoordinateSpecialistArgs::Wait { .. } => {
                unreachable!("wait is handled before synchronous coordination actions")
            }
        })();
        let _ = response.send(result);
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
                } else if !self.has_operator_command_for(self.sessions.active_id()) {
                    self.start_composer_turn(submission);
                } else {
                    self.queue_composer_follow_up(submission);
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
                if !self.sessions.is_main_active() {
                    self.view.defer_composer_action(
                        submission,
                        "Switch to Main before forking the session group".to_string(),
                    );
                } else if self.deepwork_in_progress() {
                    self.view.defer_composer_action(
                        submission,
                        "Finish or exit the active `$deepwork` run before forking".to_string(),
                    );
                } else if self.has_local_session_activity() {
                    self.view.defer_composer_action(
                        submission,
                        "Wait for the local command or Git diff before forking".to_string(),
                    );
                } else if let Err(error) = self.fork_session() {
                    self.view.reject_composer_submission(
                        submission,
                        format!("Could not fork this session: {error:#}"),
                    );
                }
            }
            Action::Clear(submission) => {
                if !self.sessions.is_main_active() {
                    self.view.defer_composer_action(
                        submission,
                        "Switch to Main before starting a fresh session".to_string(),
                    );
                } else if self.deepwork_in_progress() {
                    self.view.defer_composer_action(
                        submission,
                        "Finish or exit the active `$deepwork` run before clearing".to_string(),
                    );
                } else if self.has_local_session_activity() {
                    self.view.defer_composer_action(
                        submission,
                        "Wait for the local command or Git diff before clearing".to_string(),
                    );
                } else if self.turn.is_active() {
                    self.view.defer_composer_action(
                        submission,
                        "Interrupt the active turn before starting a fresh session".to_string(),
                    );
                } else if let Err(error) = self.clear_session() {
                    self.view.reject_composer_submission(
                        submission,
                        format!("Could not start a fresh session: {error:#}"),
                    );
                }
            }
            Action::OpenResumePicker(submission) => {
                if !self.sessions.is_main_active() {
                    self.view.defer_composer_action(
                        submission,
                        "Switch to Main before resuming another saved session".to_string(),
                    );
                } else if self.deepwork_in_progress() {
                    self.view.defer_composer_action(
                        submission,
                        "Finish or exit the active `$deepwork` run before resuming another session"
                            .to_string(),
                    );
                } else if self.has_local_session_activity() {
                    self.view.defer_composer_action(
                        submission,
                        "Wait for the local command or Git diff before resuming".to_string(),
                    );
                } else {
                    self.resume_submission = Some(submission);
                    self.open_resume_picker();
                }
            }
            Action::CloseResumePicker => self.close_resume_picker(),
            Action::CancelResumeLoad => self.cancel_resume_load(),
            Action::ResumeSessionFromPicker(id) => {
                if self.deepwork_in_progress() {
                    self.view.add_notice(
                        "Finish or exit the active `$deepwork` run before resuming another session"
                            .to_string(),
                    );
                    self.close_resume_picker();
                } else if self.has_local_session_activity() {
                    self.view.add_notice(
                        "Wait for the local command or Git diff before resuming".to_string(),
                    );
                    self.close_resume_picker();
                } else if let Err(error) = self.start_resume(id) {
                    self.restore_resume_submission();
                    self.view
                        .resume_failed(format!("Could not resume this session: {error:#}"));
                }
            }
            Action::ResumeSessionFromComposer { id, submission } => {
                if self.deepwork_in_progress() {
                    self.view.defer_composer_action(
                        submission,
                        "Finish or exit the active `$deepwork` run before resuming another session",
                    );
                } else if self.has_local_session_activity() {
                    self.view.defer_composer_action(
                        submission,
                        "Wait for the local command or Git diff before resuming",
                    );
                } else {
                    self.resume_submission = Some(submission);
                    if let Err(error) = self.start_resume(id) {
                        self.restore_resume_submission();
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
                let snapshot = self.sessions.active().agent.as_ref().map_or_else(
                    || self.sessions.active().context_snapshot.clone(),
                    Agent::context_snapshot,
                );
                let slot = self.sessions.active_mut();
                slot.set_context_snapshot(snapshot.clone());
                slot.view.show_context(snapshot);
            }
            Action::ShowStatus => {
                if let Some((context_snapshot, session_id, forked_from, instruction_paths)) =
                    self.sessions.active().agent.as_ref().map(|agent| {
                        (
                            agent.context_snapshot(),
                            agent.session_id().to_string(),
                            agent.forked_from().map(str::to_string),
                            agent.instruction_source_paths().to_vec(),
                        )
                    })
                {
                    let slot = self.sessions.active_mut();
                    slot.set_context_snapshot(context_snapshot);
                    slot.session_id = session_id;
                    slot.forked_from = forked_from;
                    slot.instruction_source_paths = instruction_paths;
                }
                let context = self.sessions.active().context_snapshot.clone();
                self.cache_status_rate_limits(context.rate_limits.clone());
                let account = self.rate_limit_client.account().unwrap_or_else(|error| {
                    tracing::warn!(%error, "failed to read ChatGPT account metadata");
                    crate::auth::ChatGptAccount::default()
                });
                let snapshot = StatusSnapshot {
                    model: self.sessions.active().model_selection.clone(),
                    directory: self.cwd.clone(),
                    instruction_source_paths: self
                        .sessions
                        .active()
                        .instruction_source_paths
                        .clone(),
                    session_id: self.sessions.active().session_id.clone(),
                    forked_from: self.sessions.active().forked_from.clone(),
                    account,
                    context,
                    rate_limits: self.status_rate_limits.values().cloned().collect(),
                    refreshing_rate_limits: true,
                };
                self.sessions.active_mut().view.add_status(snapshot);
                self.start_rate_limit_refresh();
            }
            Action::ShowDiff => self.start_git_diff(),
            Action::EnterTmux => unreachable!("tmux handoffs are handled by the event loop"),
            Action::Logout => unreachable!("logout is handled by the event loop"),
            Action::UpdateSkill { path, update } => match self.agent.as_mut() {
                Some(agent) => {
                    let result = agent
                        .update_skill(&path, update)
                        .map(|()| (agent.context_snapshot(), agent.skills().to_vec()));
                    match result {
                        Ok((context_snapshot, skills)) => {
                            self.set_context_snapshot(context_snapshot);
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
            Action::ResolveAskUserQuestion { call_id, response } => {
                let matches_pending = self
                    .pending_ask_user_question
                    .as_ref()
                    .is_some_and(|request| request.call_id() == call_id);
                if matches_pending && let Some(request) = self.pending_ask_user_question.take() {
                    self.sessions
                        .main_mut()
                        .view
                        .dismiss_ask_user_question(&call_id);
                    if self.sessions.linkage.deepwork.is_some() {
                        let persisted =
                            DeepworkQuestionBatch::from_response(request.arguments(), &response)
                                .and_then(|batch| {
                                    self.sessions.record_deepwork_question_batch(batch)
                                });
                        if let Err(error) = persisted {
                            self.sessions.main_mut().view.add_notice(format!(
                                "The answer was returned, but canonical deepwork state could not be saved: {error:#}"
                            ));
                        }
                    }
                    if !request.respond(response) {
                        self.sessions.main_mut().view.add_notice(
                            "AskUserQuestion response arrived after the turn stopped".to_string(),
                        );
                    }
                }
            }
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
        if self.sessions.active().is_child() {
            self.view.add_notice(
                "Specialist model profiles are fixed; switch to Main to change models".to_string(),
            );
            return;
        }
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
        let slot = self.sessions.active_mut();
        slot.model_selection = selection.clone();
        slot.view.set_model_selection(selection.clone());
        slot.context_snapshot.context_window = selection.effective_context_window();
        slot.context_snapshot.compact_at_tokens = selection.auto_compact_token_limit();
    }

    fn sync_main_model_selection_to_agent(&mut self, agent: &mut Agent) {
        let selection = self.sessions.main().model_selection.clone();
        if agent.model_selection() == &selection {
            return;
        }
        if let Err(error) = agent.set_model_selection(selection) {
            let selection = agent.model_selection().clone();
            let main = self.sessions.main_mut();
            main.model_selection = selection.clone();
            main.view.set_model_selection(selection);
            main.view
                .add_error(format!("Could not change model: {error:#}"));
        }
    }

    fn sync_main_service_tier_to_agent(&mut self, agent: &mut Agent) {
        let service_tier = self.sessions.main().service_tier;
        if agent.service_tier() == service_tier {
            return;
        }
        if let Err(error) = agent.set_service_tier(service_tier) {
            let service_tier = agent.service_tier();
            let main = self.sessions.main_mut();
            main.service_tier = service_tier;
            main.view.set_service_tier(service_tier);
            main.view
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
        let selection = self.sessions.main().model_selection.clone();
        let service_tier = self.sessions.main().service_tier;
        let mut agent = Agent::new(&self.cwd)?;
        agent.set_ask_user_question_requester(self.ask_user_question_requester.clone());
        agent.set_deepwork_requester(self.deepwork_requester.clone());
        agent.set_model_selection(selection)?;
        agent.set_service_tier(service_tier)?;
        let mut slot = AgentSlot::main(agent)?;
        slot.view.clear();
        slot.view.sync_context_snapshot(&slot.context_snapshot);
        self.install_main_slot(slot)
    }

    fn fork_session(&mut self) -> Result<()> {
        let source = self
            .sessions
            .main()
            .agent
            .as_ref()
            .context("a session can only be forked while the agent is idle")?;
        let transcript = self.sessions.main().view.session_transcript();
        let mut agent = source.fork(transcript)?;
        agent.set_ask_user_question_requester(self.ask_user_question_requester.clone());
        agent.set_deepwork_requester(self.deepwork_requester.clone());
        let session_id = agent.session_id().to_string();
        let cwd = self.cwd.clone();
        let retained_view = std::mem::replace(&mut self.sessions.main_mut().view, View::new(&cwd));
        let mut slot = AgentSlot::main(agent)?;
        slot.view = retained_view;
        slot.view.sync_context_snapshot(&slot.context_snapshot);
        slot.view.set_model_selection(slot.model_selection.clone());
        slot.view.set_service_tier(slot.service_tier);
        slot.view
            .add_notice(format!("Forked conversation into session {session_id}"));
        self.install_main_slot(slot)
    }

    fn open_resume_picker(&mut self) {
        self.view.show_resume_picker();
        if self.session_scan.is_none() {
            self.session_scan = Some(tokio::task::spawn_blocking(Rollout::list_sessions));
        }
    }

    fn dismiss_resume_picker(&mut self) {
        abort_join_task(&mut self.session_scan);
        self.view.close_resume_picker();
    }

    fn close_resume_picker(&mut self) {
        self.dismiss_resume_picker();
        self.restore_resume_submission();
    }

    fn cancel_resume_load(&mut self) {
        // `spawn_blocking` work that has started cannot be preempted, but dropping its result
        // guarantees that a cancelled resume can never replace the current session.
        abort_join_task(&mut self.resume_task);
        self.close_resume_picker();
    }

    fn restore_resume_submission(&mut self) {
        if let Some(submission) = self.resume_submission.take() {
            self.view.restore_composer_submission(submission);
        }
    }

    fn complete_same_session_resume(&mut self, target: Uuid) {
        self.resume_submission = None;
        self.dismiss_resume_picker();
        self.view
            .add_notice(format!("Already viewing bettercodex session {target}"));
    }

    fn start_resume(&mut self, target: Uuid) -> Result<()> {
        let current_session = self.current_session_id()?;
        if target == current_session {
            self.complete_same_session_resume(target);
            return Ok(());
        }
        if self.resume_task.is_some() {
            return Ok(());
        }
        self.view.show_resume_progress(target);
        let requested_cwd = self.cwd.clone();
        self.resume_task = Some(tokio::task::spawn_blocking(move || {
            Agent::resume(&requested_cwd, ResumeSelector::Id(target))
                .map(|agent| ResumedSession { agent })
        }));
        Ok(())
    }

    fn start_operator_command(&mut self, command: String) {
        // Resolve agent events that were already ready when the terminal action won `select!`.
        // The initiating slot then owns the complete local-command presentation even if the user
        // watches another session before the command exits.
        self.drain_agent_events();
        let session_id = self.sessions.active_id().clone();
        let call_id = format!("operator:{}", uuid::Uuid::new_v4());
        {
            let slot = self
                .sessions
                .slot_mut(&session_id)
                .unwrap_or_else(|| unreachable!("active session stayed live"));
            slot.view.start_operator_command(call_id.clone(), &command);
            if let Some(agent) = slot.agent.as_mut()
                && let Err(error) = persist_session_transcript(&slot.view, agent)
            {
                slot.view
                    .add_notice(format!("Session transcript could not be saved: {error:#}"));
            }
        }
        let cwd = self.cwd.clone();
        let cancellation = CancellationToken::new();
        let updates = self.operator_command_updates_tx.clone();
        let task_call_id = call_id.clone();
        let task_session_id = session_id.clone();
        let task_cancellation = cancellation.clone();
        let truncation_policy = self
            .sessions
            .slot(&session_id)
            .unwrap_or_else(|| unreachable!("active session stayed live"))
            .model_selection
            .truncation_policy();
        let task = tokio::spawn(async move {
            let output_updates = updates.clone();
            let output_call_id = task_call_id.clone();
            let output_session_id = task_session_id.clone();
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
                            session_id: output_session_id.clone(),
                            call_id: output_call_id.clone(),
                            chunk,
                        })
                        .is_err()
                {
                    return crate::process_runtime::LiveOutputAction::Stop;
                }
                if omitted {
                    let _ = output_updates.send(OperatorCommandUpdate::Output {
                        session_id: output_session_id.clone(),
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
                session_id: task_session_id,
                call_id: task_call_id,
                output,
                context,
            });
        });
        self.operator_command_cancellations
            .insert(call_id.clone(), cancellation);
        self.operator_command_owners
            .insert(call_id.clone(), session_id);
        self.operator_command_tasks.insert(call_id, task);
    }

    fn apply_operator_command_update(&mut self, update: OperatorCommandUpdate) {
        match update {
            OperatorCommandUpdate::Output {
                session_id,
                call_id,
                chunk,
            } => {
                if let Some(slot) = self.sessions.slot_mut(&session_id) {
                    slot.view.append_operator_command_output(&call_id, &chunk);
                }
            }
            OperatorCommandUpdate::Completed {
                session_id,
                call_id,
                output,
                context,
            } => {
                self.operator_command_tasks.remove(&call_id);
                self.operator_command_cancellations.remove(&call_id);
                self.operator_command_owners.remove(&call_id);
                let transcript_output = match &output {
                    Ok(output) => SessionTranscriptToolOutput::Success(output.clone()),
                    Err(error) => SessionTranscriptToolOutput::Error(error.clone()),
                };
                if let Some(slot) = self.sessions.slot_mut(&session_id) {
                    slot.view.finish_operator_command(&call_id, output);
                }
                self.record_operator_context(&session_id, context);
                if let Some(slot) = self.sessions.slot_mut(&session_id)
                    && let Some(agent) = slot.agent.as_mut()
                {
                    // Persist the cell first in case its start checkpoint failed, then patch its
                    // bounded outcome without replacing the complete transcript snapshot.
                    if let Err(error) = persist_session_transcript(&slot.view, agent) {
                        slot.view.add_notice(format!(
                            "Session transcript could not be saved: {error:#}"
                        ));
                    }
                    if let Err(error) =
                        agent.persist_transcript_tool_outcome(call_id.clone(), transcript_output)
                    {
                        // The saved cell may still be incomplete. Force the next checkpoint to
                        // replace it from the view instead of appending past stale state.
                        agent.invalidate_transcript_checkpoint();
                        slot.view.add_notice(format!(
                            "Session transcript could not be saved: {error:#}"
                        ));
                    }
                }
                let slot_idle = self
                    .sessions
                    .slot(&session_id)
                    .is_some_and(|slot| !slot.turn.is_active());
                if !self.has_operator_command_for(&session_id) && slot_idle && !self.exit_after_work
                {
                    self.start_next_queued_follow_up_for(&session_id);
                }
            }
        }
    }

    fn record_operator_context(&mut self, session_id: &SessionId, context: String) {
        let Some(slot) = self.sessions.slot_mut(session_id) else {
            return;
        };
        if slot.turn.is_active()
            && let Some(turn) = &slot.turn_handle
            && let Ok(id) = turn.inject_context(context.clone())
        {
            slot.operator_context_steers.push((id, context));
            return;
        }

        slot.pending_operator_contexts.push_back(context);
        let Some(mut agent) = slot.agent.take() else {
            return;
        };
        if let Err(error) = flush_operator_contexts(&mut slot.pending_operator_contexts, &mut agent)
        {
            slot.view.add_notice(format!(
                "Operator shell output could not be added to model context: {error:#}"
            ));
        }
        slot.set_context_snapshot(agent.context_snapshot());
        slot.agent = Some(agent);
    }

    fn start_git_diff(&mut self) {
        if self.diff_task.is_some() {
            self.view
                .add_notice("A Git diff is already being computed".to_string());
            return;
        }
        let session_id = self.sessions.active_id().clone();
        let cwd = self.cwd.clone();
        let updates = self.diff_updates_tx.clone();
        self.diff_task = Some(tokio::spawn(async move {
            let result = git_diff::get_git_diff(cwd).await;
            let _ = updates.send((session_id, result));
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
        let mut agent = session.agent;
        agent.set_ask_user_question_requester(self.ask_user_question_requester.clone());
        agent.set_deepwork_requester(self.deepwork_requester.clone());
        match AgentSlot::main(agent).and_then(|slot| self.install_main_slot(slot)) {
            Ok(()) => self.exit_after_work = false,
            Err(error) => self.sessions.main_mut().view.add_notice(format!(
                "The selected session loaded, but its runtime could not be activated: {error:#}"
            )),
        }
    }

    fn install_main_slot(&mut self, slot: AgentSlot) -> Result<()> {
        let cwd = slot.cwd.clone();
        let rate_limits = slot.context_snapshot.rate_limits.clone();
        let (sessions, agent_events, turn_results) = SessionGroup::new(slot)?;
        let (file_search_updates_tx, file_search_updates) = unbounded_channel();
        self.file_search = FileSearchManager::new(cwd.clone(), file_search_updates_tx);
        self.file_search_updates = file_search_updates;
        self.cwd = cwd;
        self.sessions = sessions;
        self.switcher_selection = None;
        self.agent_events = agent_events;
        self.turn_results = turn_results;
        self.cache_status_rate_limits(rate_limits);
        Ok(())
    }

    fn queue_composer_follow_up(&mut self, submission: ComposerSubmission) {
        let history_text = submission.prompt().text_without_image_placeholders();
        self.persist_prompt(&history_text);
        self.view.queue_follow_up(submission.into_prompt());
    }

    fn start_next_queued_follow_up_for(&mut self, session_id: &SessionId) -> bool {
        let Some(prompt) = self
            .sessions
            .slot_mut(session_id)
            .and_then(|slot| slot.view.pop_next_queued_follow_up())
        else {
            return false;
        };
        self.start_turn_for(session_id, prompt)
    }

    fn start_composer_turn(&mut self, submission: ComposerSubmission) {
        let session_id = self.sessions.active_id().clone();
        match self.prepare_turn_start_for(&session_id) {
            Ok(agent) => {
                let history_text = submission.prompt().text_without_image_placeholders();
                self.persist_prompt(&history_text);
                self.spawn_turn_for(&session_id, agent, submission.into_prompt(), Vec::new());
            }
            Err(error) => {
                self.view.reject_composer_submission(submission, error);
            }
        }
    }

    fn start_turn_for(&mut self, session_id: &SessionId, prompt: UserPrompt) -> bool {
        match self.prepare_turn_start_for(session_id) {
            Ok(agent) => {
                self.spawn_turn_for(session_id, agent, prompt, Vec::new());
                true
            }
            Err(error) => {
                if let Some(slot) = self.sessions.slot_mut(session_id) {
                    slot.view.reject_prompt(prompt, error);
                }
                false
            }
        }
    }

    fn start_interrupted_turn_for(
        &mut self,
        session_id: &SessionId,
        replay: InterruptedSteeringReplay,
    ) {
        match self.prepare_turn_start_for(session_id) {
            Ok(agent) => {
                self.spawn_turn_for(session_id, agent, replay.first_prompt, replay.trailing)
            }
            Err(error) => {
                if let Some(slot) = self.sessions.slot_mut(session_id) {
                    slot.view.reject_prompt(replay.first_prompt, error);
                }
                self.restore_interrupted_steering_for(session_id, replay.trailing);
            }
        }
    }

    fn restore_interrupted_steering_for(
        &mut self,
        session_id: &SessionId,
        steering: Vec<InterruptedSteering>,
    ) {
        let Some(slot) = self.sessions.slot_mut(session_id) else {
            return;
        };
        for input in steering {
            match input {
                InterruptedSteering::Operator(prompt) => slot.view.queue_follow_up(prompt),
                InterruptedSteering::Context(context) => {
                    slot.pending_operator_contexts.push_back(context);
                }
            }
        }
    }

    fn prepare_turn_start_for(
        &mut self,
        session_id: &SessionId,
    ) -> std::result::Result<Agent, String> {
        let mut agent = self
            .sessions
            .slot_mut(session_id)
            .and_then(|slot| slot.agent.take())
            .ok_or_else(|| "Could not start turn: the active agent is unavailable".to_string())?;
        let flush_result = {
            let slot = self
                .sessions
                .slot_mut(session_id)
                .unwrap_or_else(|| unreachable!("turn session stayed live"));
            flush_operator_contexts(&mut slot.pending_operator_contexts, &mut agent)
        };
        if let Err(error) = flush_result {
            self.sessions
                .slot_mut(session_id)
                .unwrap_or_else(|| unreachable!("turn session stayed live"))
                .agent = Some(agent);
            return Err(format!(
                "Could not start turn: operator shell output could not be added to model context: {error:#}"
            ));
        }
        if self
            .sessions
            .slot(session_id)
            .is_some_and(AgentSlot::is_child)
            && let Err(error) = self
                .sessions
                .set_lifecycle(session_id, ChildLifecycle::Working)
        {
            self.sessions
                .slot_mut(session_id)
                .unwrap_or_else(|| unreachable!("turn session stayed live"))
                .agent = Some(agent);
            return Err(format!(
                "Could not start turn: specialist lifecycle could not be saved: {error:#}"
            ));
        }
        Ok(agent)
    }

    fn spawn_turn_for(
        &mut self,
        session_id: &SessionId,
        agent: Agent,
        prompt: UserPrompt,
        initial_steering: Vec<InterruptedSteering>,
    ) {
        let (turn_handle, turn_control) = crate::agent::TurnControl::channel();
        let unqueued = {
            let slot = self
                .sessions
                .slot_mut(session_id)
                .unwrap_or_else(|| unreachable!("turn session stayed live"));
            slot.view.start_turn(&prompt);
            enqueue_initial_steering(
                &turn_handle,
                &mut slot.view,
                &mut slot.operator_context_steers,
                initial_steering,
            )
        };
        self.restore_interrupted_steering_for(session_id, unqueued);
        let task = session_group::spawn_main_submission_supervisor(
            session_id.clone(),
            agent,
            prompt,
            turn_control,
            self.sessions.agent_events_tx.clone(),
            self.sessions.turn_results_tx.clone(),
        );
        let slot = self
            .sessions
            .slot_mut(session_id)
            .unwrap_or_else(|| unreachable!("turn session stayed live"));
        slot.turn_started_at = Some(Instant::now());
        slot.turn_handle = Some(turn_handle);
        slot.turn = TurnTaskState::Running(task);
    }

    fn start_compaction(&mut self) {
        let session_id = self.sessions.active_id().clone();
        let Ok(agent) = self.prepare_turn_start_for(&session_id) else {
            self.view.add_notice(
                "Could not compact conversation: the active agent is unavailable".to_string(),
            );
            return;
        };
        let (turn_handle, turn_control) = crate::agent::TurnControl::non_steerable_channel();
        let task = session_group::spawn_compaction_supervisor(
            session_id.clone(),
            agent,
            turn_control,
            self.sessions.agent_events_tx.clone(),
            self.sessions.turn_results_tx.clone(),
        );
        let slot = self
            .sessions
            .slot_mut(&session_id)
            .unwrap_or_else(|| unreachable!("compaction session stayed live"));
        slot.view.start_compaction();
        slot.turn_started_at = Some(Instant::now());
        slot.turn_handle = Some(turn_handle);
        slot.turn = TurnTaskState::Running(task);
    }

    fn request_exit(&mut self) -> bool {
        if !self.any_turn_active() && self.operator_command_tasks.is_empty() {
            return true;
        }
        self.exit_after_work = true;
        self.cancel_all_turns(InterruptIntent::StopTurn);
        for cancellation in self.operator_command_cancellations.values() {
            cancellation.cancel();
        }
        false
    }

    fn exit_ready(&self) -> bool {
        self.exit_after_work && !self.any_turn_active() && self.operator_command_tasks.is_empty()
    }

    fn deepwork_in_progress(&self) -> bool {
        self.sessions
            .linkage
            .deepwork
            .as_ref()
            .is_some_and(|state| state.stage != crate::deepwork::DeepworkStage::Completed)
    }

    fn any_turn_active(&self) -> bool {
        self.sessions.live_session_ids().iter().any(|session_id| {
            self.sessions
                .slot(session_id)
                .is_some_and(|slot| slot.turn.is_active())
        })
    }

    fn cancel_all_turns(&mut self, intent: InterruptIntent) {
        let session_ids = self.sessions.live_session_ids();
        for session_id in session_ids {
            if let Some(slot) = self.sessions.slot(&session_id)
                && slot.is_child()
            {
                if matches!(slot.turn, TurnTaskState::Running(_)) {
                    self.cancel_child_turn(&session_id);
                }
                continue;
            }
            let Some(slot) = self.sessions.slot_mut(&session_id) else {
                continue;
            };
            if let Some(turn) = &slot.turn_handle {
                match intent {
                    InterruptIntent::StopTurn => turn.cancel(),
                    InterruptIntent::EditPrompt => turn.cancel_and_edit_prompt(),
                    InterruptIntent::SubmitSteering => turn.interrupt_for_steering(),
                }
                slot.view.set_interrupting(intent);
            }
        }
    }

    fn cancel_turn(&mut self, intent: InterruptIntent) {
        let session_id = self.sessions.active_id().clone();
        if self
            .sessions
            .slot(&session_id)
            .is_some_and(AgentSlot::is_child)
        {
            self.cancel_child_turn(&session_id);
            return;
        }
        if let Some(turn) = &self.turn_handle {
            match intent {
                InterruptIntent::StopTurn => turn.cancel(),
                InterruptIntent::EditPrompt => turn.cancel_and_edit_prompt(),
                InterruptIntent::SubmitSteering => turn.interrupt_for_steering(),
            }
            self.view.set_interrupting(intent);
        }
    }

    fn cancel_child_turn(&mut self, session_id: &SessionId) {
        if let Err(error) = self.sessions.cancel_deepwork_child(session_id) {
            let Some(slot) = self.sessions.slot_mut(session_id) else {
                return;
            };
            if matches!(slot.turn, TurnTaskState::Running(_))
                && let Some(turn) = &slot.turn_handle
            {
                turn.cancel();
                slot.view.set_interrupting(InterruptIntent::StopTurn);
                slot.view.add_notice(format!(
                    "The specialist was interrupted, but its paused lifecycle could not be saved: {error:#}"
                ));
            } else {
                slot.view
                    .add_notice(format!("Could not cancel the specialist: {error:#}"));
            }
        }
    }

    fn cancel_active_work(&mut self) {
        if let Some(request) = self.pending_ask_user_question.take() {
            self.view.dismiss_ask_user_question(request.call_id());
        }
        let submitting_steering = self.turn.is_active() && self.view.has_pending_steers();
        if self.turn.is_active() {
            self.interrupt_turn();
        }
        if !submitting_steering {
            let active_id = self.sessions.active_id().clone();
            for (call_id, cancellation) in &self.operator_command_cancellations {
                if self.operator_command_owners.get(call_id) == Some(&active_id) {
                    cancellation.cancel();
                }
            }
        }
    }

    fn interrupt_turn(&mut self) {
        let intent = if self.view.has_pending_steers() {
            InterruptIntent::SubmitSteering
        } else if !self.has_operator_command_for(self.sessions.active_id())
            && self.operator_context_steers.is_empty()
            && self.pending_operator_contexts.is_empty()
            && self.view.can_edit_submitted_prompt()
        {
            InterruptIntent::EditPrompt
        } else {
            InterruptIntent::StopTurn
        };
        self.cancel_turn(intent);
    }

    fn drain_agent_events(&mut self) {
        for _ in 0..MAX_READY_AGENT_EVENTS {
            let Ok((session_id, event)) = self.agent_events.try_recv() else {
                return;
            };
            self.apply_agent_event(&session_id, event);
        }
    }

    fn apply_agent_event(&mut self, session_id: &SessionId, event: AgentEvent) {
        let rate_limits = match &event {
            AgentEvent::ContextUpdated(snapshot) => Some(snapshot.rate_limits.clone()),
            _ => None,
        };
        let is_active = session_id == self.sessions.active_id();
        let Some(slot) = self.sessions.slot_mut(session_id) else {
            return;
        };
        if let AgentEvent::ContextUpdated(snapshot) = &event {
            slot.context_snapshot = snapshot.clone();
        } else if let AgentEvent::SteeringCommitted(id) = &event
            && let Some(index) = slot
                .operator_context_steers
                .iter()
                .position(|(candidate, _)| candidate == id)
        {
            slot.operator_context_steers.remove(index);
        }
        slot.view.handle_agent_event(event);
        if !is_active {
            // Hidden sessions do not consume terminal animation frames. Their presentation state
            // still advances so the retained transcript is current when the user enters the slot.
            slot.view.flush_presentation();
        }
        if let Some(rate_limits) = rate_limits {
            self.cache_status_rate_limits(rate_limits);
        }
    }

    fn has_foreground_activity(&self) -> bool {
        self.sessions.live_session_ids().iter().any(|session_id| {
            self.sessions
                .slot(session_id)
                .is_some_and(|slot| slot.view.is_busy())
        }) || !self.operator_command_tasks.is_empty()
            || self.diff_task.is_some()
            || self.resume_task.is_some()
            || self.session_scan.is_some()
    }

    fn has_operator_command_for(&self, session_id: &SessionId) -> bool {
        self.operator_command_owners
            .values()
            .any(|owner| owner == session_id)
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

    fn start_patch_notes_acknowledgement(&mut self) {
        let Some(startup) = self.patch_notes_startup.take() else {
            return;
        };
        self.patch_notes_ack_task = Some(tokio::task::spawn_blocking(move || startup.mark_seen()));
    }

    fn start_update_check_after_startup(&mut self) {
        if self.update_check_started {
            return;
        }
        self.update_check_started = true;
        if let Some(check) = crate::update::background_update_check() {
            self.update_check = Some(tokio::spawn(check));
        }
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
        // A successful first frame owns this persistence attempt. Detach it on immediate exit
        // rather than cancelling a queued blocking write.
        drop(self.patch_notes_ack_task.take());
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

async fn receive_patch_notes_acknowledgement(
    task: &mut Option<PatchNotesAckTask>,
) -> std::result::Result<Result<()>, tokio::task::JoinError> {
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
