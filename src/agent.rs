use crate::api::ApiClient;
use crate::api::ApiError;
use crate::api::CompactionResult;
use crate::api::CompletedResponseMetadata;
use crate::api::ModelResponse;
use crate::api::retry_delay;
use crate::auth::Auth;
use crate::compaction::CompactionPhase;
use crate::compaction::CompactionRequest;
use crate::compaction::InitialContextInjection;
use crate::context::ActiveTurnContext;
use crate::context::ContextSnapshot;
use crate::context::Conversation;
use crate::deepwork::DeepworkRequester;
use crate::deepwork::HarnessProfile;
use crate::deepwork::SpecialistRole;
use crate::events::AgentEvent;
use crate::events::SteerId;
use crate::input::UserInput;
use crate::model::ModelSelection;
use crate::rollout::LoadedRollout;
use crate::rollout::ResumeSelector;
use crate::rollout::Rollout;
use crate::rollout::SessionTranscriptItem;
use crate::rollout::SessionTranscriptToolOutcome;
use crate::rollout::SessionTranscriptToolOutput;
use crate::rollout::TurnOutcome;
use crate::service_tier::ServiceTier;
use crate::skills::DEEPWORK_SYSTEM_SKILL_NAME;
use crate::tools::ToolCall;
use crate::tools::ToolCompletion;
use crate::tools::ToolRuntime;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use futures_util::future::join_all;
use serde_json::Value;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const MAX_STREAM_RETRIES_PER_TRANSPORT: usize = 5;

fn transcript_tool_outcome(completion: ToolCompletion) -> Option<SessionTranscriptToolOutcome> {
    let output = completion
        .inspection
        .map(SessionTranscriptToolOutput::recovered_file_state);
    let has_file_change = completion.file_change.is_some();
    if completion.error.is_none() && output.is_none() && !has_file_change {
        return None;
    }
    Some(SessionTranscriptToolOutcome {
        call_id: completion.call_id,
        output,
        error: completion.error,
        file_change: completion.file_change,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SubmitOutcome {
    Completed(String),
    Cancelled,
    CancelledBeforeProcessing,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CompactionOutcome {
    Completed,
    Cancelled,
}

#[derive(Clone)]
pub(crate) struct TurnHandle {
    cancellation: CancellationToken,
    steering: Option<Arc<Mutex<SteeringState>>>,
    input_acceptance: Option<Arc<InputAcceptance>>,
}

pub(crate) struct TurnControl {
    cancellation: CancellationToken,
    steering: Option<Arc<Mutex<SteeringState>>>,
    input_acceptance: Option<Arc<InputAcceptance>>,
}

struct SteeringState {
    accepting: bool,
    // Set atomically with dequeue and cleared after sampling, so Esc-to-send cannot cancel in the
    // gap where the UI still owns the prompt but admission has already removed it from the queue.
    operator_input_in_flight: bool,
    next_id: u64,
    queued: VecDeque<SteeringInput>,
}

struct SteeringInput {
    id: SteerId,
    payload: SteeringPayload,
}

enum SteeringPayload {
    Operator(UserInput),
    Context(String),
}

const INPUT_PENDING: u8 = 0;
const INPUT_EDIT_REQUESTED: u8 = 1;
const INPUT_ACCEPTED: u8 = 2;

#[derive(Debug)]
struct InputAcceptance {
    state: AtomicU8,
}

impl InputAcceptance {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(INPUT_PENDING),
        }
    }

    fn request_edit(&self) {
        let _ = self.state.compare_exchange(
            INPUT_PENDING,
            INPUT_EDIT_REQUESTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn edit_requested(&self) -> bool {
        self.state.load(Ordering::Acquire) == INPUT_EDIT_REQUESTED
    }

    fn accept(&self) -> bool {
        self.state
            .compare_exchange(
                INPUT_PENDING,
                INPUT_ACCEPTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

impl SteeringState {
    fn has_queued_operator_input(&self) -> bool {
        self.queued
            .iter()
            .any(|input| matches!(&input.payload, SteeringPayload::Operator(_)))
    }

    fn drain_into(&mut self, pending: &mut VecDeque<SteeringInput>) {
        self.operator_input_in_flight |= self.has_queued_operator_input();
        pending.append(&mut self.queued);
    }
}

struct StartupPrewarm {
    task: Option<JoinHandle<std::result::Result<ApiClient, ApiError>>>,
    started_at: Instant,
}

enum StartupPrewarmResolution {
    Cancelled,
    Ready(Box<ApiClient>),
    Unavailable,
}

impl StartupPrewarm {
    fn schedule(api: &ApiClient) -> Option<Self> {
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let client = api.startup_prewarm_client();
        Some(Self {
            task: Some(runtime.spawn(client.prewarm_for_startup())),
            started_at: Instant::now(),
        })
    }

    async fn resolve(mut self, cancellation: &CancellationToken) -> StartupPrewarmResolution {
        let Some(mut task) = self.task.take() else {
            tracing::warn!("startup websocket prewarm task was unavailable");
            return StartupPrewarmResolution::Unavailable;
        };
        let remaining =
            crate::api::WEBSOCKET_CONNECT_TIMEOUT.saturating_sub(self.started_at.elapsed());
        let result = if task.is_finished() {
            Ok((&mut task).await)
        } else {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    task.abort();
                    return StartupPrewarmResolution::Cancelled;
                }
                result = timeout(remaining, &mut task) => result,
            }
        };
        match result {
            Ok(Ok(Ok(api))) => StartupPrewarmResolution::Ready(Box::new(api)),
            Ok(Ok(Err(error))) => {
                tracing::warn!(%error, "startup websocket prewarm failed");
                StartupPrewarmResolution::Unavailable
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "startup websocket prewarm task failed");
                StartupPrewarmResolution::Unavailable
            }
            Err(_) => {
                task.abort();
                tracing::info!("startup websocket prewarm timed out before the first turn");
                StartupPrewarmResolution::Unavailable
            }
        }
    }
}

impl Drop for StartupPrewarm {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl TurnHandle {
    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub(crate) fn cancel_and_edit_prompt(&self) {
        if let Some(input_acceptance) = &self.input_acceptance {
            input_acceptance.request_edit();
        }
        self.cancellation.cancel();
    }

    /// Interrupt current work only while the operator input still needs to be dequeued.
    pub(crate) fn interrupt_for_steering(&self) {
        let should_cancel = self.steering.as_ref().is_none_or(|steering| {
            let steering = lock_steering(steering);
            steering.has_queued_operator_input() || !steering.operator_input_in_flight
        });
        if should_cancel {
            self.cancellation.cancel();
        }
    }

    pub(crate) fn steer(&self, input: UserInput) -> Result<SteerId> {
        self.enqueue(SteeringPayload::Operator(input))
    }

    pub(crate) fn inject_context(&self, text: String) -> Result<SteerId> {
        self.enqueue(SteeringPayload::Context(text))
    }

    fn enqueue(&self, payload: SteeringPayload) -> Result<SteerId> {
        let steering = self
            .steering
            .as_ref()
            .ok_or_else(|| anyhow!("the active task does not accept steering"))?;
        let mut steering = lock_steering(steering);
        if !steering.accepting {
            return Err(anyhow!("the active turn already finished"));
        }
        let id = SteerId(steering.next_id);
        steering.next_id = steering.next_id.wrapping_add(1);
        steering.queued.push_back(SteeringInput { id, payload });
        Ok(id)
    }
}

impl Drop for TurnControl {
    fn drop(&mut self) {
        self.close();
    }
}

impl TurnControl {
    pub(crate) fn channel() -> (TurnHandle, Self) {
        let cancellation = CancellationToken::new();
        let input_acceptance = Arc::new(InputAcceptance::new());
        let steering = Arc::new(Mutex::new(SteeringState {
            accepting: true,
            operator_input_in_flight: false,
            next_id: 0,
            queued: VecDeque::new(),
        }));
        (
            TurnHandle {
                cancellation: cancellation.clone(),
                steering: Some(Arc::clone(&steering)),
                input_acceptance: Some(Arc::clone(&input_acceptance)),
            },
            Self {
                cancellation,
                steering: Some(steering),
                input_acceptance: Some(input_acceptance),
            },
        )
    }

    pub(crate) fn non_steerable_channel() -> (TurnHandle, Self) {
        let cancellation = CancellationToken::new();
        (
            TurnHandle {
                cancellation: cancellation.clone(),
                steering: None,
                input_acceptance: None,
            },
            Self {
                cancellation,
                steering: None,
                input_acceptance: None,
            },
        )
    }

    fn cancellation_only(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            steering: None,
            input_acceptance: None,
        }
    }

    fn edit_requested(&self) -> bool {
        self.input_acceptance
            .as_ref()
            .is_some_and(|input_acceptance| input_acceptance.edit_requested())
    }

    fn accept_input(&self) -> bool {
        self.input_acceptance
            .as_ref()
            .is_none_or(|input_acceptance| input_acceptance.accept())
    }

    fn drain_steering(&self, pending: &mut VecDeque<SteeringInput>) {
        let Some(steering) = &self.steering else {
            return;
        };
        let mut steering = lock_steering(steering);
        if self.cancellation.is_cancelled() {
            return;
        }
        steering.drain_into(pending);
    }

    fn finish_sampling(&self) {
        if let Some(steering) = &self.steering {
            lock_steering(steering).operator_input_in_flight = false;
        }
    }

    /// Atomically stop accepting steering if no input is waiting.
    ///
    /// This closes the race between the final queue check and turn completion:
    /// a concurrent sender either lands in `pending` or observes a closed turn.
    fn close_if_idle(&self, pending: &mut VecDeque<SteeringInput>) -> bool {
        let Some(steering) = &self.steering else {
            return true;
        };
        let mut steering = lock_steering(steering);
        if self.cancellation.is_cancelled() {
            return false;
        }
        if steering.queued.is_empty() {
            steering.accepting = false;
            true
        } else {
            steering.drain_into(pending);
            false
        }
    }

    fn close(&self) {
        if let Some(steering) = &self.steering {
            lock_steering(steering).accepting = false;
        }
    }
}

pub(crate) struct Agent {
    cwd: PathBuf,
    api: ApiClient,
    startup_prewarm: Option<StartupPrewarm>,
    conversation: Conversation,
    tools: ToolRuntime,
    resumed_transcript: Vec<SessionTranscriptItem>,
    transcript_checkpoint: Option<usize>,
    forked_from: Option<String>,
}

impl Agent {
    pub(crate) fn new(cwd: impl AsRef<Path>) -> Result<Self> {
        let model_selection = crate::model::load_default_selection().unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load saved model selection; using the default");
            ModelSelection::default()
        });
        Self::new_with_selection(cwd, model_selection)
    }

    /// Starts a fresh independent session with an explicit model profile.
    ///
    /// This deliberately creates a new rollout instead of forking another Agent's conversation.
    /// The current working directory and repository instructions are discovered normally for the
    /// child, while the selected model and reasoning effort never inherit Main's `/model` choice.
    pub(crate) fn new_with_selection(
        cwd: impl AsRef<Path>,
        model_selection: ModelSelection,
    ) -> Result<Self> {
        let cwd = canonical_directory(cwd.as_ref())?;
        model_selection.validate()?;
        let auth = Auth::load()?;
        let service_tier = crate::service_tier::load_default().unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load saved Fast mode preference; using off");
            ServiceTier::default()
        });
        let mut conversation = Conversation::create_with_selection(&cwd, model_selection)?;
        conversation.set_service_tier(service_tier)?;
        let identity = conversation.identity().clone();
        let api = ApiClient::new(
            auth,
            &identity,
            0,
            conversation.model_selection().clone(),
            conversation.service_tier(),
        )?;
        let tools = ToolRuntime::new(cwd.clone());
        let startup_prewarm = StartupPrewarm::schedule(&api);
        Ok(Self {
            cwd,
            api,
            startup_prewarm,
            conversation,
            tools,
            resumed_transcript: Vec::new(),
            transcript_checkpoint: None,
            forked_from: None,
        })
    }

    pub(crate) fn new_specialist(cwd: impl AsRef<Path>, role: SpecialistRole) -> Result<Self> {
        let mut agent = Self::new_with_selection(cwd, role.model_selection())?;
        agent.set_specialist_role(role)?;
        Ok(agent)
    }

    pub(crate) fn resume(cwd: impl AsRef<Path>, selector: ResumeSelector) -> Result<Self> {
        let requested_cwd = canonical_directory(cwd.as_ref())?;
        let loaded = Rollout::resume(selector, &requested_cwd)?;
        Self::from_loaded_rollout(loaded)
    }

    pub(crate) fn fork(&self, transcript: Vec<SessionTranscriptItem>) -> Result<Self> {
        let auth = Auth::load()?;
        let mut rollout =
            Rollout::create_with_selection(&self.cwd, self.conversation.model_selection())?;
        let identity = rollout.identity().clone();
        let compaction_count = self.api.compaction_count();
        rollout.record_fork(
            self.session_id(),
            compaction_count,
            self.conversation.prior_usage_for_fork(),
        )?;
        let transcript_checkpoint = Some(transcript.len());
        rollout.snapshot_transcript(transcript)?;
        let conversation = self.conversation.fork(rollout)?;
        let api = ApiClient::new(
            auth,
            &identity,
            compaction_count,
            conversation.model_selection().clone(),
            conversation.service_tier(),
        )?;
        let tools = ToolRuntime::new(self.cwd.clone());
        let startup_prewarm = StartupPrewarm::schedule(&api);
        Ok(Self {
            cwd: self.cwd.clone(),
            api,
            startup_prewarm,
            conversation,
            tools,
            resumed_transcript: Vec::new(),
            transcript_checkpoint,
            forked_from: Some(self.session_id().to_string()),
        })
    }

    fn from_loaded_rollout(mut loaded: LoadedRollout) -> Result<Self> {
        let cwd = canonical_directory(&loaded.metadata.cwd)?;
        let identity = loaded.metadata.identity.clone();
        let compaction_count = loaded.compaction_count;
        let resumed_transcript = std::mem::take(&mut loaded.transcript);
        let transcript_checkpoint = loaded.transcript_checkpoint;
        let forked_from = loaded.forked_from.clone();
        let conversation = Conversation::resume(&cwd, loaded)?;
        let auth = Auth::load()?;
        let api = ApiClient::new(
            auth,
            &identity,
            compaction_count,
            conversation.model_selection().clone(),
            conversation.service_tier(),
        )?;
        let mut tools = ToolRuntime::new(cwd.clone());
        let deepwork_active = conversation.has_injected_skill(DEEPWORK_SYSTEM_SKILL_NAME);
        tools.set_ask_user_question_enabled(deepwork_active);
        tools.set_specialist_coordination_enabled(deepwork_active);
        let startup_prewarm = StartupPrewarm::schedule(&api);
        Ok(Self {
            cwd,
            api,
            startup_prewarm,
            conversation,
            tools,
            resumed_transcript,
            transcript_checkpoint,
            forked_from,
        })
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn session_id(&self) -> &str {
        self.conversation.session_id()
    }

    pub(crate) fn forked_from(&self) -> Option<&str> {
        self.forked_from.as_deref()
    }

    pub(crate) fn instruction_source_paths(&self) -> &[PathBuf] {
        self.conversation.instruction_source_paths()
    }

    pub(crate) fn model_selection(&self) -> &ModelSelection {
        self.conversation.model_selection()
    }

    pub(crate) fn set_model_selection(&mut self, selection: ModelSelection) -> Result<()> {
        self.conversation.set_model_selection(selection.clone())?;
        self.api.set_model_selection(selection);
        Ok(())
    }

    pub(crate) fn set_ask_user_question_requester(
        &mut self,
        requester: crate::ask_user_question::AskUserQuestionRequester,
    ) {
        self.tools.set_ask_user_question_requester(requester);
        self.sync_deepwork_tool_access();
    }

    pub(crate) fn set_deepwork_requester(&mut self, requester: DeepworkRequester) {
        self.tools.set_deepwork_requester(requester);
        self.sync_deepwork_tool_access();
    }

    pub(crate) fn restore_deepwork_access(&mut self) {
        self.tools.set_ask_user_question_enabled(true);
        self.tools.set_specialist_coordination_enabled(true);
        self.sync_deepwork_tool_access();
    }

    pub(crate) fn set_specialist_role(&mut self, role: SpecialistRole) -> Result<()> {
        self.startup_prewarm = None;
        let profile = HarnessProfile::Specialist(role);
        self.conversation.set_harness_profile(profile)?;
        self.api.set_harness_profile(profile);
        self.tools.set_ask_user_question_enabled(false);
        self.tools.set_specialist_coordination_enabled(false);
        self.sync_deepwork_tool_access();
        Ok(())
    }

    async fn apply_skill_tool_access(
        &mut self,
        skill_context: &[Value],
        original_task: Option<String>,
    ) -> Result<()> {
        let activates_deepwork = skill_context.iter().any(|item| {
            crate::context::injected_skill_name(item) == Some(DEEPWORK_SYSTEM_SKILL_NAME)
        });
        if !activates_deepwork {
            return Ok(());
        }
        let requester = self
            .tools
            .deepwork_requester()
            .ok_or_else(|| anyhow!("$deepwork requires the interactive session coordinator"))?;
        let status = requester
            .activate(original_task.context("$deepwork activation lost the user's task")?)
            .await?;
        self.conversation
            .extend([crate::context::deepwork_runtime_context(
                status.run_index,
                &status.workspace,
                status.stage,
            )])?;
        self.tools.set_ask_user_question_enabled(true);
        self.tools.set_specialist_coordination_enabled(true);
        self.sync_deepwork_tool_access();
        Ok(())
    }

    fn sync_deepwork_tool_access(&mut self) {
        let ask_enabled = self.tools.ask_user_question_enabled();
        let coordinate_enabled = self.tools.specialist_coordination_enabled();
        if self.api.ask_user_question_enabled() != ask_enabled
            || self.api.specialist_coordination_enabled() != coordinate_enabled
        {
            self.startup_prewarm = None;
            self.api.set_ask_user_question_enabled(ask_enabled);
            self.api
                .set_specialist_coordination_enabled(coordinate_enabled);
        }
        self.conversation.set_ask_user_question_enabled(ask_enabled);
        self.conversation
            .set_specialist_coordination_enabled(coordinate_enabled);
    }

    pub(crate) fn service_tier(&self) -> ServiceTier {
        self.conversation.service_tier()
    }

    pub(crate) fn set_service_tier(&mut self, service_tier: ServiceTier) -> Result<()> {
        self.conversation.set_service_tier(service_tier)?;
        self.api.set_service_tier(service_tier);
        Ok(())
    }

    pub(crate) fn context_snapshot(&self) -> ContextSnapshot {
        self.conversation.context_snapshot()
    }

    pub(crate) fn rate_limit_client(&self) -> crate::rate_limits::RateLimitClient {
        self.api.rate_limit_client()
    }

    pub(crate) fn prompt_history(&self) -> Vec<String> {
        self.conversation.prompt_history()
    }

    pub(crate) fn record_operator_shell_context(&mut self, text: String) -> Result<()> {
        debug_assert!(crate::context::is_contextual_user_text(&text));
        let mut message = crate::context::message("user", text);
        crate::context::mark_contextual_user_message(&mut message);
        self.conversation.extend([message])
    }

    pub(crate) fn take_resumed_transcript(&mut self) -> Vec<SessionTranscriptItem> {
        std::mem::take(&mut self.resumed_transcript)
    }

    pub(crate) fn transcript_checkpoint(&self) -> Option<usize> {
        self.transcript_checkpoint
    }

    pub(crate) fn invalidate_transcript_checkpoint(&mut self) {
        self.transcript_checkpoint = None;
    }

    pub(crate) fn persist_transcript_tool_outcome(
        &mut self,
        call_id: String,
        output: SessionTranscriptToolOutput,
    ) -> Result<()> {
        self.conversation
            .record_tool_outcomes(vec![SessionTranscriptToolOutcome {
                call_id,
                output: Some(output),
                error: None,
                file_change: None,
            }])
    }

    pub(crate) fn persist_transcript(
        &mut self,
        items: Vec<SessionTranscriptItem>,
        total_items: usize,
    ) -> Result<()> {
        match self.transcript_checkpoint {
            Some(checkpoint) if checkpoint.saturating_add(items.len()) == total_items => {
                self.conversation.append_transcript(items)?;
            }
            _ if items.len() == total_items => {
                self.conversation.snapshot_transcript(items)?;
            }
            _ => {
                return Err(anyhow!(
                    "complete transcript is required to replace its saved checkpoint"
                ));
            }
        }
        self.transcript_checkpoint = Some(total_items);
        Ok(())
    }

    pub(crate) fn skills(&self) -> &[crate::skills::Skill] {
        self.conversation.skill_catalog().skills()
    }

    pub(crate) fn skill_warnings(&self) -> &[String] {
        self.conversation.skill_catalog().warnings()
    }

    pub(crate) fn update_skill(
        &mut self,
        path: &Path,
        update: crate::skills::SkillUpdate,
    ) -> Result<()> {
        let Some(skill) = self.skills().iter().find(|skill| skill.path() == path) else {
            return Err(anyhow!("skill {} is no longer available", path.display()));
        };
        if skill.settings_are_fixed() {
            return Err(anyhow!("skill `{}` has fixed settings", skill.name()));
        }
        crate::skills::save_skill_update(path, update)?;
        self.conversation
            .reload_skills(&self.cwd)
            .context("skill setting was saved, but the active session could not reload skills")?;
        Ok(())
    }

    async fn resolve_startup_prewarm(&mut self, cancellation: &CancellationToken) -> bool {
        let Some(startup_prewarm) = self.startup_prewarm.take() else {
            return true;
        };
        // Startup already spent the one warmup attempt. If it fails or times out, the first turn
        // should proceed with a normal full request rather than paying for a second warmup.
        self.api.mark_websocket_prewarm_attempted();
        match startup_prewarm.resolve(cancellation).await {
            StartupPrewarmResolution::Cancelled => false,
            StartupPrewarmResolution::Ready(api) => {
                self.api.adopt_startup_prewarm(*api);
                true
            }
            StartupPrewarmResolution::Unavailable => true,
        }
    }

    pub(crate) async fn submit(&mut self, prompt: &str) -> Result<String> {
        self.submit_user_input(UserInput::text(prompt)).await
    }

    pub(crate) async fn submit_user_input(&mut self, input: UserInput) -> Result<String> {
        match self
            .submit_input(
                input,
                None,
                TurnControl::cancellation_only(CancellationToken::new()),
            )
            .await?
        {
            SubmitOutcome::Completed(answer) => Ok(answer),
            SubmitOutcome::Cancelled | SubmitOutcome::CancelledBeforeProcessing => {
                Err(anyhow!("turn was cancelled"))
            }
        }
    }

    pub(crate) async fn submit_with_control(
        &mut self,
        input: UserInput,
        events: UnboundedSender<AgentEvent>,
        control: TurnControl,
    ) -> Result<SubmitOutcome> {
        self.submit_input(input, Some(events), control).await
    }

    pub(crate) async fn compact_with_control(
        &mut self,
        events: UnboundedSender<AgentEvent>,
        control: TurnControl,
    ) -> Result<CompactionOutcome> {
        if self.startup_prewarm.take().is_some() {
            // Manual compaction does not consume the startup session, but it must not repeat the
            // discarded warmup inline before sending its own request.
            self.api.mark_websocket_prewarm_attempted();
        }
        let turn_id = self.api.begin_turn().to_string();
        self.conversation.start_turn(&turn_id)?;
        let active_turn_context = ActiveTurnContext::default();
        let result = self
            .run_compaction_inner(
                &Some(events),
                &control.cancellation,
                CompactionRequest::Manual,
                InitialContextInjection::AfterCompaction,
                &active_turn_context,
            )
            .await;
        control.close();
        let outcome = match &result {
            Ok(true) => TurnOutcome::Completed,
            Ok(false) => TurnOutcome::Interrupted,
            Err(_) => TurnOutcome::Failed,
        };
        self.conversation.finish_turn(&turn_id, outcome)?;
        result.map(|completed| {
            if completed {
                CompactionOutcome::Completed
            } else {
                CompactionOutcome::Cancelled
            }
        })
    }

    async fn submit_input(
        &mut self,
        input: UserInput,
        events: Option<UnboundedSender<AgentEvent>>,
        control: TurnControl,
    ) -> Result<SubmitOutcome> {
        if input.is_empty() {
            return Err(anyhow!("prompt and image list are both empty"));
        }
        let turn_id = self.api.begin_turn().to_string();
        self.conversation.start_turn(&turn_id)?;
        let startup_ready = self.resolve_startup_prewarm(&control.cancellation).await;
        let result = if startup_ready {
            self.run_turn(input, &events, &control).await
        } else {
            let mut active_turn_context = ActiveTurnContext::default();
            self.record_incoming_user(
                IncomingUserInput::Initial(input),
                &events,
                IncomingUserAdmission::PreserveAfterCancellation,
                Some(&control),
                &mut active_turn_context,
            )
            .await
            .map(|accepted| {
                if accepted {
                    SubmitOutcome::Cancelled
                } else {
                    SubmitOutcome::CancelledBeforeProcessing
                }
            })
        };
        control.close();
        let outcome = match &result {
            Ok(SubmitOutcome::Completed(_)) => TurnOutcome::Completed,
            Ok(SubmitOutcome::Cancelled) => {
                self.conversation.mark_interrupted()?;
                TurnOutcome::Interrupted
            }
            Ok(SubmitOutcome::CancelledBeforeProcessing) => TurnOutcome::Interrupted,
            Err(_) => {
                self.conversation.normalize()?;
                TurnOutcome::Failed
            }
        };
        self.conversation.finish_turn(&turn_id, outcome)?;
        result
    }

    async fn run_turn(
        &mut self,
        input: UserInput,
        events: &Option<UnboundedSender<AgentEvent>>,
        control: &TurnControl,
    ) -> Result<SubmitOutcome> {
        let mut active_turn_context = ActiveTurnContext::default();
        // Match Codex's pre-sampling boundary: compact only the history that was already recorded.
        // If cancellation interrupts that request, persist the accepted input before ending unless
        // the operator requested it back before admission completed.
        let compaction_cancelled = self.conversation.needs_compaction()
            && !self
                .run_compaction(
                    events,
                    &control.cancellation,
                    CompactionRequest::Automatic(CompactionPhase::PreTurn),
                    InitialContextInjection::AfterCompaction,
                    &active_turn_context,
                )
                .await?;
        let accepted = self
            .record_incoming_user(
                IncomingUserInput::Initial(input),
                events,
                if compaction_cancelled {
                    IncomingUserAdmission::PreserveAfterCancellation
                } else {
                    IncomingUserAdmission::EnforceContextWindow
                },
                Some(control),
                &mut active_turn_context,
            )
            .await?;
        if !accepted {
            return Ok(SubmitOutcome::CancelledBeforeProcessing);
        }
        if compaction_cancelled {
            return Ok(SubmitOutcome::Cancelled);
        }

        let mut pending_steering = VecDeque::new();
        // Sample the fresh turn input first. After a mid-turn compact, sample the
        // compacted tool continuation once before inserting queued steering.
        let mut can_record_pending_steering = false;
        let mut first_sample = true;
        loop {
            if control.cancellation.is_cancelled() {
                return Ok(SubmitOutcome::Cancelled);
            }
            let mut recorded_steering = false;
            if can_record_pending_steering {
                control.drain_steering(&mut pending_steering);
                while let Some(input) = pending_steering.pop_front() {
                    let accepted = self
                        .record_incoming_user(
                            IncomingUserInput::Steering(input),
                            events,
                            IncomingUserAdmission::EnforceContextWindow,
                            None,
                            &mut active_turn_context,
                        )
                        .await?;
                    debug_assert!(accepted);
                    recorded_steering = true;
                }
            }
            can_record_pending_steering = true;

            // Admission captures the world state used by the first sample and any sample with
            // fresh steering. Continuations without new input need their own request-boundary
            // refresh so tool-driven AGENTS.md or skill changes cannot remain stale.
            if !first_sample && !recorded_steering {
                self.reload_world_state_for_admission(
                    IncomingUserAdmission::EnforceContextWindow,
                    &active_turn_context,
                )?;
            }
            first_sample = false;

            let tool_truncation_policy = self.conversation.model_selection().truncation_policy();
            let sampling = self.sample_with_recovery(events, control).await;
            control.finish_sampling();
            let response = match sampling? {
                SamplingOutcome::Response(response) => response,
                SamplingOutcome::Cancelled => return Ok(SubmitOutcome::Cancelled),
            };
            let model_needs_follow_up = response.end_turn == Some(false);
            let has_assistant_text = response.has_assistant_text();
            let final_answer = response.final_answer;
            self.conversation.record_usage(
                response.usage,
                response.server_reasoning_included,
                response.rate_limits,
            )?;
            self.emit_context(events);
            emit(events, AgentEvent::ModelResponseCompleted);
            if control.cancellation.is_cancelled() {
                return Ok(SubmitOutcome::Cancelled);
            }

            let tool_calls = response.tool_calls;
            if tool_calls.is_empty() && !model_needs_follow_up {
                control.drain_steering(&mut pending_steering);
                if pending_steering.is_empty() && control.close_if_idle(&mut pending_steering) {
                    if let Some(final_answer) = final_answer {
                        return Ok(SubmitOutcome::Completed(final_answer.trim().to_string()));
                    }
                    if has_assistant_text {
                        // Explicit commentary remains visible in the transcript but is
                        // not promoted into the terminal answer for line/one-shot mode.
                        return Ok(SubmitOutcome::Completed(String::new()));
                    }
                    return Err(anyhow!("model returned no text or tool call"));
                }
                if self.conversation.needs_compaction()
                    && !self
                        .run_compaction(
                            events,
                            &control.cancellation,
                            CompactionRequest::Automatic(CompactionPhase::MidTurn),
                            InitialContextInjection::BeforeLastUserMessage,
                            &active_turn_context,
                        )
                        .await?
                {
                    return Ok(SubmitOutcome::Cancelled);
                }
                continue;
            }

            let lifecycle = Some(self.conversation.tool_lifecycle_journal());
            let tools = &self.tools;
            let execute = |tool_call: ToolCall| {
                let cancellation = control.cancellation.clone();
                let tool_events = events.clone();
                let lifecycle = lifecycle.clone();
                async move {
                    let output = tool_call
                        .execute(
                            tools,
                            tool_truncation_policy,
                            tool_events,
                            cancellation,
                            lifecycle,
                        )
                        .await;
                    tool_call.into_output_item(output)
                }
            };
            let mut executed_calls = Vec::with_capacity(tool_calls.len());
            let mut tool_calls = tool_calls.into_iter().peekable();
            while let Some(tool_call) = tool_calls.next() {
                if tool_call.supports_parallel_execution() {
                    let mut parallel_calls = vec![tool_call];
                    while let Some(tool_call) =
                        tool_calls.next_if(ToolCall::supports_parallel_execution)
                    {
                        parallel_calls.push(tool_call);
                    }
                    executed_calls.extend(join_all(parallel_calls.into_iter().map(&execute)).await);
                } else {
                    executed_calls.push(execute(tool_call).await);
                }
            }
            let (output_items, completions): (Vec<_>, Vec<_>) = executed_calls.into_iter().unzip();
            let outcomes = completions
                .into_iter()
                .filter_map(transcript_tool_outcome)
                .collect();
            self.conversation
                .extend_tool_results(output_items, outcomes)?;
            self.emit_context(events);
            if control.cancellation.is_cancelled() {
                return Ok(SubmitOutcome::Cancelled);
            }
            if self.conversation.needs_compaction() {
                if !self
                    .run_compaction(
                        events,
                        &control.cancellation,
                        CompactionRequest::Automatic(CompactionPhase::MidTurn),
                        InitialContextInjection::BeforeLastUserMessage,
                        &active_turn_context,
                    )
                    .await?
                {
                    return Ok(SubmitOutcome::Cancelled);
                }
                can_record_pending_steering = false;
            }
        }
    }

    async fn sample_with_recovery(
        &mut self,
        events: &Option<UnboundedSender<AgentEvent>>,
        control: &TurnControl,
    ) -> Result<SamplingOutcome> {
        let mut retries = 0_usize;
        loop {
            if control.cancellation.is_cancelled() {
                return Ok(SamplingOutcome::Cancelled);
            }
            self.conversation.normalize()?;
            // Preserve the backend usage baseline for old history. Re-estimating the complete
            // rendered input here can substantially overcount a valid long-running session.
            let active_context_tokens = self.conversation.active_context_tokens();
            let effective_context_window = self
                .conversation
                .model_selection()
                .effective_context_window();
            if active_context_tokens > effective_context_window {
                return Err(anyhow!(
                    "active conversation requires {active_context_tokens} tokens, exceeding bettercodex's {effective_context_window}-token effective context window"
                ));
            }
            let (completed_tx, mut completed_rx) = unbounded_channel::<Value>();
            let mut observed_item = false;
            let (history, cursor) = self.conversation.take_history_for_sampling();
            let request = self.api.build_sampling_request(history, cursor);
            let wait: Result<SamplingWait> = {
                let api = &mut self.api;
                let conversation = &mut self.conversation;
                let response = async {
                    match events {
                        Some(events) => {
                            api.respond_sampling_streaming(&request, &completed_tx, events)
                                .await
                        }
                        None => api.respond_sampling(&request, &completed_tx).await,
                    }
                };
                tokio::pin!(response);
                let mut completed_closed = false;
                loop {
                    tokio::select! {
                        biased;
                        // If terminal metadata and cancellation become ready together, install the
                        // completed response so usage and cache lineage cannot be discarded after
                        // its output items have already entered history.
                        result = &mut response => break Ok(SamplingWait::Finished(result)),
                        _ = control.cancellation.cancelled() => break Ok(SamplingWait::Cancelled),
                        item = completed_rx.recv(), if !completed_closed => {
                            match item {
                                Some(item) => {
                                    if let Err(error) = conversation.extend([item]) {
                                        break Err(error);
                                    }
                                    observed_item = true;
                                }
                                None => completed_closed = true,
                            }
                        }
                    }
                }
            };
            let mut trailing_error = None;
            while wait.is_ok()
                && let Ok(item) = completed_rx.try_recv()
            {
                match self.conversation.extend([item]) {
                    Ok(()) => observed_item = true,
                    Err(error) => {
                        trailing_error = Some(error);
                        break;
                    }
                }
            }
            let (history, cursor) = request.into_history();
            if let Err(error) = self
                .conversation
                .restore_history_after_sampling(history, cursor)
            {
                self.api.abandon_response();
                return Err(error);
            }
            let wait = match wait {
                Ok(wait) => wait,
                Err(error) => {
                    self.api.abandon_response();
                    return Err(error);
                }
            };
            if let Some(error) = trailing_error {
                self.api.abandon_response();
                return Err(error);
            }

            match wait {
                SamplingWait::Finished(Ok(response)) => {
                    return Ok(SamplingOutcome::Response(response));
                }
                SamplingWait::Cancelled => {
                    self.api.abandon_response();
                    return Ok(SamplingOutcome::Cancelled);
                }
                SamplingWait::Finished(Err(mut error)) => {
                    self.api.abandon_response();
                    if observed_item {
                        self.conversation
                            .mark_stream_interrupted(&error.to_string())?;
                    }
                    if let Some((usage, rate_limits)) = error.take_completed_response() {
                        self.conversation
                            .record_uninstalled_response(usage, rate_limits)?;
                    }
                    if error.is_context_window_exceeded() {
                        // Backend tokenization can outrun the local estimate. Match Codex by
                        // forcing the next turn through pre-turn compaction instead of repeating
                        // the same oversized request indefinitely.
                        self.conversation.mark_context_window_full()?;
                        self.emit_context(events);
                    }
                    if !error.is_retryable() {
                        return Err(error.into());
                    }
                    if retries >= MAX_STREAM_RETRIES_PER_TRANSPORT {
                        if self.api.fall_back_to_http() {
                            retries = 0;
                            continue;
                        }
                        return Err(error.into());
                    }
                    retries += 1;
                    let delay = sleep(
                        error
                            .retry_after()
                            .unwrap_or_else(|| retry_delay(retries.saturating_sub(1))),
                    );
                    tokio::pin!(delay);
                    tokio::select! {
                        biased;
                        _ = control.cancellation.cancelled() => {
                            return Ok(SamplingOutcome::Cancelled);
                        }
                        _ = &mut delay => {}
                    }
                }
            }
        }
    }

    async fn run_compaction(
        &mut self,
        events: &Option<UnboundedSender<AgentEvent>>,
        cancellation: &CancellationToken,
        compaction: CompactionRequest,
        initial_context_injection: InitialContextInjection,
        active_turn_context: &ActiveTurnContext,
    ) -> Result<bool> {
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        self.run_compaction_inner(
            events,
            cancellation,
            compaction,
            initial_context_injection,
            active_turn_context,
        )
        .await
    }

    async fn run_compaction_inner(
        &mut self,
        events: &Option<UnboundedSender<AgentEvent>>,
        cancellation: &CancellationToken,
        compaction: CompactionRequest,
        initial_context_injection: InitialContextInjection,
        active_turn_context: &ActiveTurnContext,
    ) -> Result<bool> {
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        emit(events, AgentEvent::CompactionStarted);
        self.conversation.normalize()?;
        let history_cursor = self.conversation.history_cursor();
        let compacted = self
            .request_compaction(events, cancellation, compaction, history_cursor)
            .await?;
        let Some(compacted) = compacted else {
            // Dropping the request future stops polling, but the server still owns the
            // in-flight response. A Responses WebSocket cannot carry the next request
            // until that response finishes, so discard the connection and its baseline.
            self.api.abandon_response();
            return Ok(false);
        };
        let CompactionResult {
            items,
            usage,
            rate_limits,
        } = match compacted {
            Ok(compacted) => compacted,
            Err(mut error) => {
                if let Some((usage, rate_limits)) = error.take_completed_response() {
                    self.conversation
                        .record_uninstalled_response(usage, rate_limits)?;
                }
                return Err(error.into());
            }
        };
        let replacement = self.conversation.replace_compacted(
            items,
            initial_context_injection,
            active_turn_context,
            usage.as_ref(),
            &rate_limits,
        );
        if let Err(error) = replacement {
            // The server has completed a response that was not installed. Drop its
            // connection-local baseline, but retain account usage and rate-limit status without
            // replacing the unchanged conversation's response baseline.
            self.api.abandon_response();
            self.conversation
                .record_uninstalled_response(usage, rate_limits)?;
            return Err(error);
        }
        self.api.commit_compaction();
        self.emit_context(events);
        emit(events, AgentEvent::CompactionCompleted);
        Ok(true)
    }

    async fn request_compaction(
        &mut self,
        events: &Option<UnboundedSender<AgentEvent>>,
        cancellation: &CancellationToken,
        compaction: CompactionRequest,
        history_cursor: crate::context::HistoryCursor,
    ) -> Result<Option<std::result::Result<CompactionResult, ApiError>>> {
        let mut completed = CompletedResponseMetadata::default();
        let compacted = {
            let request = self.api.compact_append_only(
                self.conversation.items(),
                history_cursor,
                compaction,
                events.as_ref(),
                &mut completed,
            );
            tokio::pin!(request);
            tokio::select! {
                biased;
                // A terminal compaction response owns usage and a potential replacement. If completion
                // and cancellation become ready together, finish the atomic install/reject path rather
                // than discarding completed-response accounting.
                compacted = &mut request => Some(compacted),
                _ = cancellation.cancelled() => None,
            }
        };
        if compacted.is_none() {
            let (usage, rate_limits) = completed.into_parts();
            self.conversation
                .record_uninstalled_response(usage, rate_limits)?;
        }
        Ok(compacted)
    }

    fn reload_world_state_for_admission(
        &mut self,
        admission: IncomingUserAdmission,
        active_turn_context: &ActiveTurnContext,
    ) -> Result<()> {
        if let Err(error) = self
            .conversation
            .reload_world_state_for_active_turn(&self.cwd, active_turn_context)
        {
            if admission == IncomingUserAdmission::EnforceContextWindow {
                return Err(error);
            }
            // Cancellation must still preserve the accepted operator input. Keep the prior
            // context only when a fresh local snapshot cannot be loaded.
            tracing::warn!(%error, "failed to refresh context before preserving cancelled input");
        }
        Ok(())
    }

    async fn record_incoming_user(
        &mut self,
        input: IncomingUserInput,
        events: &Option<UnboundedSender<AgentEvent>>,
        admission: IncomingUserAdmission,
        control: Option<&TurnControl>,
        active_turn_context: &mut ActiveTurnContext,
    ) -> Result<bool> {
        if control.is_some_and(TurnControl::edit_requested) {
            return Ok(false);
        }
        let (payload, steering_id) = match input {
            IncomingUserInput::Initial(input) => (SteeringPayload::Operator(input), None),
            IncomingUserInput::Steering(steering) => (steering.payload, Some(steering.id)),
        };
        let (user_message, operator_input) = match payload {
            SteeringPayload::Operator(input) => {
                if input.is_empty() {
                    return Err(anyhow!("prompt and attachment list are both empty"));
                }
                let has_blocking_attachments = input.has_blocking_attachments();
                let cwd = self.cwd.clone();
                let file_context_token_budget = self
                    .conversation
                    .model_selection()
                    .effective_context_window();
                let prepare = move || {
                    let (user_message, prompt_text, selected_skills, selected_files) =
                        input.into_message_and_attachments()?;
                    let file_context = crate::file_context::inject_selected_files(
                        &cwd,
                        &selected_files,
                        file_context_token_budget,
                    );
                    Ok::<_, anyhow::Error>((
                        user_message,
                        prompt_text,
                        selected_skills,
                        file_context,
                    ))
                };
                let prepared = if has_blocking_attachments {
                    tokio::task::spawn_blocking(prepare)
                        .await
                        .context("attachment preprocessing task failed")?
                } else {
                    prepare()
                };
                if control.is_some_and(TurnControl::edit_requested) {
                    return Ok(false);
                }
                let (mut user_message, prompt_text, selected_skills, file_context) = prepared?;
                crate::context::mark_operator_user_message(&mut user_message);
                (
                    user_message,
                    Some((prompt_text, selected_skills, file_context)),
                )
            }
            SteeringPayload::Context(text) => {
                if text.is_empty() {
                    return Err(anyhow!("prompt and attachment list are both empty"));
                }
                let mut user_message = crate::context::message("user", text);
                crate::context::mark_contextual_user_message(&mut user_message);
                (user_message, None)
            }
        };
        // Capture filesystem-derived context after potentially expensive attachment preprocessing,
        // so the catalogue used for skill matching and the next sampling request share one
        // snapshot.
        let reloaded = self.reload_world_state_for_admission(admission, active_turn_context);
        if control.is_some_and(TurnControl::edit_requested) {
            return Ok(false);
        }
        reloaded?;
        let (
            turn_context,
            skill_context_len,
            injected_files,
            warnings,
            real_user_message,
            deepwork_original_task,
        ) = match operator_input {
            Some((prompt_text, selected_skills, file_context)) => {
                let skill_injections = self
                    .conversation
                    .skill_catalog()
                    .explicit_injections(&prompt_text, &selected_skills);
                let activates_deepwork = skill_injections.items.iter().any(|item| {
                    crate::context::injected_skill_name(item) == Some(DEEPWORK_SYSTEM_SKILL_NAME)
                });
                let crate::file_context::FileContextInjectionOutcome {
                    items: file_items,
                    injected: injected_files,
                    warnings: file_warnings,
                } = file_context;
                let skill_context_len = skill_injections.items.len();
                let mut turn_context = skill_injections.items;
                turn_context.extend(file_items);
                let mut warnings = skill_injections.warnings;
                warnings.extend(file_warnings);
                (
                    turn_context,
                    skill_context_len,
                    injected_files,
                    warnings,
                    true,
                    activates_deepwork.then_some(prompt_text),
                )
            }
            None => (Vec::new(), 0, Vec::new(), Vec::new(), false, None),
        };
        let mut projected = Vec::with_capacity(turn_context.len().saturating_add(1));
        projected.extend(turn_context.iter().cloned());
        projected.push(user_message);
        let projection = self.conversation.project_append(projected);
        let incoming_tokens = projection.additional_tokens();
        let effective_context_window = self
            .conversation
            .model_selection()
            .effective_context_window();
        if admission == IncomingUserAdmission::EnforceContextWindow
            && incoming_tokens > effective_context_window
        {
            return Err(anyhow!(
                "input alone is estimated at {incoming_tokens} tokens, exceeding bettercodex's {effective_context_window}-token effective context window; shorten the prompt or attach fewer files or images"
            ));
        }
        let projected_tokens = projection.projected_tokens();
        // Codex still records accepted input when pre-turn work is aborted, even if the unchanged
        // history plus that input cannot be sampled until a later compaction succeeds.
        if admission == IncomingUserAdmission::EnforceContextWindow
            && projected_tokens > effective_context_window
        {
            return Err(anyhow!(
                "input would require an estimated {projected_tokens} tokens after compaction, exceeding bettercodex's {effective_context_window}-token effective context window; shorten the prompt or attach fewer files or images"
            ));
        }
        if control.is_some_and(|control| !control.accept_input()) {
            return Ok(false);
        }
        self.conversation.append_projected(projection)?;
        self.apply_skill_tool_access(&turn_context[..skill_context_len], deepwork_original_task)
            .await?;
        if let Some(id) = steering_id {
            emit(events, AgentEvent::SteeringCommitted(id));
        } else {
            emit(events, AgentEvent::UserInputAccepted);
        }
        for injected in injected_files {
            emit(events, AgentEvent::FileContextInjected(injected));
        }
        for warning in warnings {
            emit(events, AgentEvent::Warning(warning));
        }
        if real_user_message {
            active_turn_context.record_real_user_input(turn_context);
        }
        self.emit_context(events);
        Ok(true)
    }

    fn emit_context(&self, events: &Option<UnboundedSender<AgentEvent>>) {
        emit(
            events,
            AgentEvent::ContextUpdated(self.conversation.context_snapshot()),
        );
    }
}

enum SamplingWait {
    Finished(std::result::Result<ModelResponse, ApiError>),
    Cancelled,
}

enum SamplingOutcome {
    Response(ModelResponse),
    Cancelled,
}

enum IncomingUserInput {
    Initial(UserInput),
    Steering(SteeringInput),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IncomingUserAdmission {
    EnforceContextWindow,
    PreserveAfterCancellation,
}

fn lock_steering(steering: &Mutex<SteeringState>) -> std::sync::MutexGuard<'_, SteeringState> {
    steering
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    if !path.is_dir() {
        return Err(anyhow!("{} is not a directory", path.display()));
    }
    Ok(path)
}

fn emit(events: &Option<UnboundedSender<AgentEvent>>, event: AgentEvent) {
    if let Some(events) = events {
        let _ = events.send(event);
    }
}
