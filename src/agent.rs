use crate::api::ApiClient;
use crate::api::ApiError;
use crate::api::CompactionResult;
use crate::api::ModelResponse;
use crate::api::retry_delay;
use crate::auth::Auth;
use crate::compaction::CompactionPhase;
use crate::compaction::CompactionRequest;
use crate::compaction::InitialContextInjection;
use crate::context::ActiveTurnContext;
use crate::context::ContextSnapshot;
use crate::context::Conversation;
use crate::events::AgentEvent;
use crate::events::SteerId;
use crate::input::UserInput;
use crate::model::ModelSelection;
use crate::rollout::LoadedRollout;
use crate::rollout::ResumeSelector;
use crate::rollout::Rollout;
use crate::rollout::SessionTranscriptItem;
use crate::rollout::TurnOutcome;
use crate::service_tier::ServiceTier;
use crate::tools::ProcessManager;
use crate::tools::ToolRuntime;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const MAX_STREAM_RETRIES_PER_TRANSPORT: usize = 5;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SubmitOutcome {
    Completed(String),
    Cancelled,
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
}

pub(crate) struct TurnControl {
    cancellation: CancellationToken,
    steering: Option<Arc<Mutex<SteeringState>>>,
}

struct SteeringState {
    accepting: bool,
    next_id: u64,
    queued: VecDeque<SteeringInput>,
}

struct SteeringInput {
    id: SteerId,
    input: UserInput,
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

    pub(crate) fn steer(&self, input: UserInput) -> Result<SteerId> {
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
        steering.queued.push_back(SteeringInput { id, input });
        Ok(id)
    }
}

impl TurnControl {
    pub(crate) fn channel() -> (TurnHandle, Self) {
        let cancellation = CancellationToken::new();
        let steering = Arc::new(Mutex::new(SteeringState {
            accepting: true,
            next_id: 0,
            queued: VecDeque::new(),
        }));
        (
            TurnHandle {
                cancellation: cancellation.clone(),
                steering: Some(Arc::clone(&steering)),
            },
            Self {
                cancellation,
                steering: Some(steering),
            },
        )
    }

    pub(crate) fn non_steerable_channel() -> (TurnHandle, Self) {
        let cancellation = CancellationToken::new();
        (
            TurnHandle {
                cancellation: cancellation.clone(),
                steering: None,
            },
            Self::cancellation_only(cancellation),
        )
    }

    fn cancellation_only(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            steering: None,
        }
    }

    fn drain_steering(&self, pending: &mut VecDeque<SteeringInput>) {
        let Some(steering) = &self.steering else {
            return;
        };
        pending.append(&mut lock_steering(steering).queued);
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
        if steering.queued.is_empty() {
            steering.accepting = false;
            true
        } else {
            pending.append(&mut steering.queued);
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
        let cwd = canonical_directory(cwd.as_ref())?;
        let auth = Auth::load()?;
        let model_selection = crate::model::load_default_selection().unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load saved model selection; using the default");
            ModelSelection::default()
        });
        let service_tier = crate::service_tier::load_default().unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load saved Fast mode preference; using off");
            ServiceTier::default()
        });
        let mut conversation = Conversation::create_with_selection(&cwd, model_selection)?;
        conversation.set_service_tier(service_tier)?;
        let identity = conversation.identity().clone();
        let tool_configuration = conversation.tool_configuration();
        let api = ApiClient::new(
            auth,
            &identity,
            0,
            conversation.model_selection().clone(),
            conversation.service_tier(),
            tool_configuration,
        )?;
        let tools = ToolRuntime::new(cwd.clone(), api.web_search_client(), tool_configuration);
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
        let tool_configuration = conversation.tool_configuration();
        let api = ApiClient::new(
            auth,
            &identity,
            compaction_count,
            conversation.model_selection().clone(),
            conversation.service_tier(),
            tool_configuration,
        )?;
        let tools = ToolRuntime::new(
            self.cwd.clone(),
            api.web_search_client(),
            tool_configuration,
        );
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
        let tool_configuration = conversation.tool_configuration();
        let api = ApiClient::new(
            auth,
            &identity,
            compaction_count,
            conversation.model_selection().clone(),
            conversation.service_tier(),
            tool_configuration,
        )?;
        let tools = ToolRuntime::new(cwd.clone(), api.web_search_client(), tool_configuration);
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

    pub(crate) fn service_tier(&self) -> ServiceTier {
        self.conversation.service_tier()
    }

    pub(crate) fn set_service_tier(&mut self, service_tier: ServiceTier) -> Result<()> {
        self.conversation.set_service_tier(service_tier)?;
        self.api.set_service_tier(service_tier);
        Ok(())
    }

    pub(crate) fn context_tokens(&self) -> Option<u64> {
        self.conversation.context_tokens()
    }

    pub(crate) fn context_snapshot(&self) -> ContextSnapshot {
        self.conversation.context_snapshot()
    }

    pub(crate) fn rate_limit_client(&self) -> crate::rate_limits::RateLimitClient {
        self.api.rate_limit_client()
    }

    pub(crate) fn background_processes(&self) -> ProcessManager {
        self.tools.background_processes()
    }

    pub(crate) fn prompt_history(&self) -> Vec<String> {
        self.conversation.prompt_history()
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
        if !self.skills().iter().any(|skill| skill.path() == path) {
            return Err(anyhow!("skill {} is no longer available", path.display()));
        }
        crate::skills::save_skill_update(path, update)?;
        self.conversation
            .reload_skills(&self.cwd)
            .context("skill setting was saved, but the active session could not reload skills")?;
        let configuration = self.conversation.tool_configuration();
        self.api.set_tool_configuration(configuration);
        self.tools.set_configuration(configuration);
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
            SubmitOutcome::Cancelled => Err(anyhow!("turn was cancelled")),
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
            let _ = self
                .record_incoming_user(
                    IncomingUserInput::Initial(input),
                    &events,
                    &control.cancellation,
                    CompactionPhase::PreTurn,
                    &mut active_turn_context,
                )
                .await?;
            Ok(SubmitOutcome::Cancelled)
        };
        control.close();
        let outcome = match &result {
            Ok(SubmitOutcome::Completed(_)) => TurnOutcome::Completed,
            Ok(SubmitOutcome::Cancelled) => {
                self.conversation.mark_interrupted()?;
                TurnOutcome::Interrupted
            }
            Err(_) => TurnOutcome::Failed,
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
        if !self
            .record_incoming_user(
                IncomingUserInput::Initial(input),
                events,
                &control.cancellation,
                CompactionPhase::PreTurn,
                &mut active_turn_context,
            )
            .await?
        {
            return Ok(SubmitOutcome::Cancelled);
        }

        // Hide the one-time ICU decompression and V8 platform initialization behind the model's
        // first sampling request.
        self.tools.prewarm();
        // Standalone search excludes model and tool output after the newest operator input. Keep
        // that potentially large projection until steering or compaction changes its visible tail.
        let mut tool_context = self.api.tool_turn_context(self.conversation.items());
        let mut pending_steering = VecDeque::new();
        // Sample the fresh turn input first. After a mid-turn compact, sample the
        // compacted tool continuation once before inserting queued steering.
        let mut can_record_pending_steering = false;
        loop {
            let mut tool_context_changed = false;
            if can_record_pending_steering {
                let notifications = self.tools.take_notifications()?;
                if !notifications.is_empty() {
                    self.conversation.extend(notifications)?;
                    self.emit_context(events);
                }
                control.drain_steering(&mut pending_steering);
                while let Some(input) = pending_steering.pop_front() {
                    if !self
                        .record_incoming_user(
                            IncomingUserInput::Steering(input),
                            events,
                            &control.cancellation,
                            CompactionPhase::MidTurn,
                            &mut active_turn_context,
                        )
                        .await?
                    {
                        return Ok(SubmitOutcome::Cancelled);
                    }
                    tool_context_changed = true;
                }
            }
            can_record_pending_steering = true;
            if tool_context_changed {
                tool_context = self.api.tool_turn_context(self.conversation.items());
            }

            let tool_truncation_policy = tool_context.truncation_policy();
            let code_mode_step = self.tools.begin_step(tool_context.clone(), events.clone());
            let response = match self.sample_with_recovery(events, control).await? {
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

            let tool_calls = response.tool_calls;
            if tool_calls.is_empty() && !model_needs_follow_up {
                drop(code_mode_step);
                control.drain_steering(&mut pending_steering);
                if !pending_steering.is_empty() {
                    continue;
                }
                if self.tools.has_notifications()? {
                    continue;
                }
                if !control.close_if_idle(&mut pending_steering) {
                    continue;
                }
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

            for tool_call in tool_calls {
                let cancellation = control.cancellation.clone();
                let tool_events = events.clone();
                let output = tool_call
                    .execute(
                        &self.tools,
                        tool_truncation_policy,
                        tool_events,
                        cancellation,
                    )
                    .await;
                self.conversation
                    .extend(tool_call.into_output_items(output))?;
                self.emit_context(events);
            }
            drop(code_mode_step);
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
                tool_context = self.api.tool_turn_context(self.conversation.items());
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
            let mut request = self.api.build_sampling_request(history, cursor);
            let wait: Result<SamplingWait> = {
                let api = &mut self.api;
                let conversation = &mut self.conversation;
                let response = async {
                    match events {
                        Some(events) => {
                            api.respond_sampling_streaming(&mut request, &completed_tx, events)
                                .await
                        }
                        None => api.respond_sampling(&mut request, &completed_tx).await,
                    }
                };
                tokio::pin!(response);
                let mut completed_closed = false;
                loop {
                    tokio::select! {
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
            let (history, cursor) = request.into_history()?;
            self.conversation
                .restore_history_after_sampling(history, cursor)?;
            let wait = wait?;
            if let Some(error) = trailing_error {
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
                SamplingWait::Finished(Err(error)) => {
                    self.api.abandon_response();
                    if observed_item {
                        self.conversation
                            .mark_stream_interrupted(&error.to_string())?;
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
                        _ = &mut delay => {}
                        _ = control.cancellation.cancelled() => {
                            return Ok(SamplingOutcome::Cancelled);
                        }
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
            .await;
        let Some(compacted) = compacted else {
            // Dropping the request future stops polling, but the server still owns the
            // in-flight response. A Responses WebSocket cannot carry the next request
            // until that response finishes, so discard the connection and its baseline.
            self.api.abandon_response();
            return Ok(false);
        };
        let compacted = compacted?;
        let replacement = self.conversation.replace_compacted(
            compacted.items,
            initial_context_injection,
            active_turn_context,
            compacted.usage,
            compacted.rate_limits,
        );
        if let Err(error) = replacement {
            // The server has completed a response that was not installed. Drop
            // its connection-local baseline before any unchanged history is sent.
            self.api.abandon_response();
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
    ) -> Option<std::result::Result<CompactionResult, ApiError>> {
        let request = self.api.compact_append_only(
            self.conversation.items(),
            history_cursor,
            compaction,
            events.as_ref(),
        );
        tokio::pin!(request);
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            compacted = &mut request => Some(compacted),
        }
    }

    async fn record_incoming_user(
        &mut self,
        input: IncomingUserInput,
        events: &Option<UnboundedSender<AgentEvent>>,
        cancellation: &CancellationToken,
        phase: CompactionPhase,
        active_turn_context: &mut ActiveTurnContext,
    ) -> Result<bool> {
        let (input, steering_id) = match input {
            IncomingUserInput::Initial(input) => (input, None),
            IncomingUserInput::Steering(steering) => (steering.input, Some(steering.id)),
        };
        if input.is_empty() {
            return Err(anyhow!("prompt and image list are both empty"));
        }
        let (user_message, prompt_text, selected_skills) = if input.has_images() {
            tokio::task::spawn_blocking(move || input.into_message_and_skills())
                .await
                .context("attached image preprocessing task failed")??
        } else {
            input.into_message_and_skills()?
        };
        let injections = self
            .conversation
            .skill_catalog()
            .explicit_injections(&prompt_text, &selected_skills);
        for warning in injections.warnings {
            emit(events, AgentEvent::Warning(warning));
        }
        let skill_context = injections.items;
        let mut projected = Vec::with_capacity(skill_context.len().saturating_add(1));
        projected.extend(skill_context.iter().cloned());
        projected.push(user_message);
        let mut projection = self.conversation.project_append(projected);
        let incoming_tokens = projection.additional_tokens();
        let effective_context_window = self
            .conversation
            .model_selection()
            .effective_context_window();
        if incoming_tokens > effective_context_window {
            return Err(anyhow!(
                "input alone is estimated at {incoming_tokens} tokens, exceeding bettercodex's {effective_context_window}-token effective context window; shorten the prompt or attach fewer images"
            ));
        }
        if projection.needs_compaction() {
            let projected = projection.into_items();
            if !self
                .run_compaction(
                    events,
                    cancellation,
                    CompactionRequest::Automatic(phase),
                    InitialContextInjection::AfterCompaction,
                    active_turn_context,
                )
                .await?
            {
                return Ok(false);
            }
            projection = self.conversation.project_append(projected);
        }
        let projected_tokens = projection.projected_tokens();
        if projected_tokens > effective_context_window {
            return Err(anyhow!(
                "input would require an estimated {projected_tokens} tokens after compaction, exceeding bettercodex's {effective_context_window}-token effective context window; shorten the prompt or attach fewer images"
            ));
        }
        self.conversation.append_projected(projection)?;
        active_turn_context.record_input(skill_context);
        if let Some(id) = steering_id {
            emit(events, AgentEvent::SteeringCommitted(id));
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

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
