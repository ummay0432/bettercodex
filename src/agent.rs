use crate::api::ApiClient;
use crate::api::ApiError;
use crate::api::ModelResponse;
use crate::api::retry_delay;
use crate::auth::Auth;
use crate::compaction::CompactionPhase;
use crate::compaction::CompactionRequest;
use crate::compaction::InitialContextInjection;
use crate::context::ActiveTurnContext;
use crate::context::ContextSnapshot;
use crate::context::Conversation;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use crate::context::FrozenLoopContext;
use crate::events::AgentEvent;
use crate::events::SteerId;
use crate::input::UserInput;
use crate::rollout::LoadedRollout;
use crate::rollout::OperatorInputRecord;
use crate::rollout::ResumeSelector;
use crate::rollout::Rollout;
use crate::rollout::SessionTranscriptItem;
use crate::rollout::TurnOutcome;
use crate::tools::ProcessManager;
use crate::tools::ToolResult;
use crate::tools::ToolRuntime;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::sleep;
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

    pub(crate) fn child_non_steerable(&self) -> Self {
        Self::cancellation_only(self.cancellation.clone())
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub(crate) fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
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
    conversation: Conversation,
    tools: ToolRuntime,
    resumed_transcript: Vec<SessionTranscriptItem>,
}

impl Agent {
    pub(crate) fn new(cwd: impl AsRef<Path>) -> Result<Self> {
        let cwd = canonical_directory(cwd.as_ref())?;
        let auth = Auth::load()?;
        let conversation = Conversation::create(&cwd)?;
        let identity = conversation.identity().clone();
        let api = ApiClient::new(auth, &identity, 0)?;
        let tools = ToolRuntime::new(
            cwd.clone(),
            api.web_search_client(),
            api.openai_docs_client(),
        );
        Ok(Self {
            cwd,
            api,
            conversation,
            tools,
            resumed_transcript: Vec::new(),
        })
    }

    pub(crate) fn resume(cwd: impl AsRef<Path>, selector: ResumeSelector) -> Result<Self> {
        let requested_cwd = canonical_directory(cwd.as_ref())?;
        let loaded = Rollout::resume(selector, &requested_cwd)?;
        Self::from_loaded_rollout(loaded)
    }

    pub(crate) fn fork(&self, transcript: Vec<SessionTranscriptItem>) -> Result<Self> {
        let auth = Auth::load()?;
        let mut rollout = Rollout::create(&self.cwd)?;
        let identity = rollout.identity().clone();
        let compaction_count = self.api.compaction_count();
        rollout.record_fork(self.session_id(), compaction_count)?;
        rollout.snapshot_transcript(transcript)?;
        let conversation = self.conversation.fork(rollout)?;
        let api = ApiClient::new(auth, &identity, compaction_count)?;
        let tools = ToolRuntime::new(
            self.cwd.clone(),
            api.web_search_client(),
            api.openai_docs_client(),
        );
        Ok(Self {
            cwd: self.cwd.clone(),
            api,
            conversation,
            tools,
            resumed_transcript: Vec::new(),
        })
    }

    fn from_loaded_rollout(mut loaded: LoadedRollout) -> Result<Self> {
        let cwd = canonical_directory(&loaded.metadata.cwd)?;
        let identity = loaded.metadata.identity.clone();
        let compaction_count = loaded.compaction_count;
        let resumed_transcript = std::mem::take(&mut loaded.transcript);
        let conversation = Conversation::resume(&cwd, loaded)?;
        let auth = Auth::load()?;
        let api = ApiClient::new(auth, &identity, compaction_count)?;
        let tools = ToolRuntime::new(
            cwd.clone(),
            api.web_search_client(),
            api.openai_docs_client(),
        );
        Ok(Self {
            cwd,
            api,
            conversation,
            tools,
            resumed_transcript,
        })
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn session_id(&self) -> &str {
        self.conversation.session_id()
    }

    pub(crate) fn latest_usage(&self) -> Option<crate::usage::TokenUsage> {
        self.conversation.latest_usage().cloned()
    }

    pub(crate) fn context_tokens(&self) -> Option<u64> {
        self.conversation.context_tokens()
    }

    pub(crate) fn context_snapshot(&self) -> ContextSnapshot {
        self.conversation.context_snapshot()
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

    pub(crate) fn skills(&self) -> &[crate::skills::Skill] {
        self.conversation.skill_catalog().skills()
    }

    pub(crate) fn skill_warnings(&self) -> &[String] {
        self.conversation.skill_catalog().warnings()
    }

    pub(crate) fn record_loop_invocation(
        &mut self,
        input: UserInput,
        events: &Option<UnboundedSender<AgentEvent>>,
    ) -> Result<FrozenLoopContext> {
        if input.is_empty() {
            return Err(anyhow!("prompt and image list are both empty"));
        }
        let existing = self.conversation.operator_inputs().ok_or_else(|| {
            anyhow!(
                "this resumed session predates exact operator-input capture; start a fresh session before invoking the quality loop"
            )
        })?;
        let (message, prompt_text, selected_skills) = input.into_message_and_skills();
        let injections = self
            .conversation
            .skill_catalog()
            .explicit_injections(&prompt_text, &selected_skills);
        for warning in &injections.warnings {
            emit(events, AgentEvent::Warning(warning.clone()));
        }
        let record = OperatorInputRecord {
            message: message.clone(),
            prompt_text,
            selected_skills,
            skill_context: injections.items.clone(),
        };
        let mut records = existing.to_vec();
        records.push(record.clone());
        let (frozen, warnings) = FrozenLoopContext::capture(&self.cwd, &records)?;
        for warning in warnings {
            emit(events, AgentEvent::Warning(warning));
        }
        self.conversation.record_operator_input(record)?;
        self.conversation
            .extend(injections.items.into_iter().chain([message]))?;
        self.emit_context(events);
        Ok(frozen)
    }

    pub(crate) fn start_loop_turn(&mut self) -> Result<String> {
        let turn_id = self.api.begin_turn().to_string();
        self.conversation.start_turn(&turn_id)?;
        Ok(turn_id)
    }

    pub(crate) fn finish_loop_turn(
        &mut self,
        turn_id: &str,
        answer: Option<&str>,
        outcome: TurnOutcome,
    ) -> Result<()> {
        if let Some(answer) = answer {
            self.conversation.extend([serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": answer}],
            })])?;
        }
        if outcome == TurnOutcome::Interrupted {
            self.conversation.mark_interrupted()?;
        }
        self.conversation.finish_turn(turn_id, outcome)
    }

    pub(crate) fn new_frozen_loop_session(
        cwd: &Path,
        local_state_root: &Path,
        frozen: &FrozenLoopContext,
        phase_prompt: &str,
        command_environment: HashMap<String, String>,
    ) -> Result<Self> {
        let auth = Auth::load()?;
        let rollout = Rollout::create_in(local_state_root, cwd)?;
        let identity = rollout.identity().clone();
        let conversation = Conversation::from_frozen_loop(frozen, rollout)?;
        let instructions = format!(
            "{}\n\n{}",
            crate::api::harness_instructions(),
            phase_prompt.trim()
        );
        let api = ApiClient::new_with_instructions(auth, &identity, 0, instructions)?;
        let tools = ToolRuntime::with_environment(
            cwd.to_path_buf(),
            api.web_search_client(),
            api.openai_docs_client(),
            command_environment,
        );
        Ok(Self {
            cwd: cwd.to_path_buf(),
            api,
            conversation,
            tools,
            resumed_transcript: Vec::new(),
        })
    }

    pub(crate) async fn submit_preloaded_with_control(
        &mut self,
        events: Option<UnboundedSender<AgentEvent>>,
        control: TurnControl,
    ) -> Result<SubmitOutcome> {
        let turn_id = self.api.begin_turn().to_string();
        self.conversation.start_turn(&turn_id)?;
        let result = self
            .run_active_turn(ActiveTurnContext::default(), &events, &control)
            .await;
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
            .context("skill setting was saved, but the active session could not reload skills")
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
        let turn_id = self.api.begin_turn().to_string();
        self.conversation.start_turn(&turn_id)?;
        let active_turn_context = ActiveTurnContext::default();
        let result = self
            .run_compaction(
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
        let result = self.run_turn(input, &events, &control).await;
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

        self.run_active_turn(active_turn_context, events, control)
            .await
    }

    async fn run_active_turn(
        &mut self,
        mut active_turn_context: ActiveTurnContext,
        events: &Option<UnboundedSender<AgentEvent>>,
        control: &TurnControl,
    ) -> Result<SubmitOutcome> {
        // Hide the one-time ICU decompression and V8 platform initialization behind the model's
        // first sampling request instead of paying it after the first exec call arrives.
        self.tools.prewarm();
        let mut pending_steering = VecDeque::new();
        // Sample the fresh turn input first. After a mid-turn compact, sample the
        // compacted tool continuation once before inserting queued steering.
        let mut can_record_pending_steering = false;
        loop {
            if can_record_pending_steering {
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
                }
            }
            can_record_pending_steering = true;

            let response = match self.sample_with_recovery(events, control).await? {
                SamplingOutcome::Response(response) => response,
                SamplingOutcome::Cancelled => return Ok(SubmitOutcome::Cancelled),
            };
            let model_needs_follow_up = response.end_turn == Some(false);
            let has_assistant_text = response.has_assistant_text();
            let final_answer = response.final_answer;
            self.conversation
                .record_usage(response.usage, response.server_reasoning_included)?;
            self.emit_context(events);
            emit(events, AgentEvent::ModelResponseCompleted);

            let mut tool_calls = response.tool_calls.into_iter();
            if tool_calls.len() == 0 && !model_needs_follow_up {
                control.drain_steering(&mut pending_steering);
                if !pending_steering.is_empty() {
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

            while let Some(tool_call) = tool_calls.next() {
                let context = self.api.tool_turn_context(self.conversation.items());
                let output = tool_call
                    .execute(
                        &self.tools,
                        context,
                        events.clone(),
                        control.cancellation.clone(),
                    )
                    .await;
                self.conversation
                    .extend(tool_call.into_output_items(output))?;
                self.emit_context(events);
                if control.cancellation.is_cancelled() {
                    for pending in tool_calls {
                        let output = ToolResult::text(
                            "tool error: skipped after user interruption".to_string(),
                        );
                        self.conversation
                            .extend(pending.into_output_items(output))?;
                    }
                    return Ok(SubmitOutcome::Cancelled);
                }
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
            self.conversation.normalize()?;
            // Preserve the backend usage baseline for old history. Re-estimating the complete
            // rendered input here can substantially overcount a valid long-running session.
            let active_context_tokens = self.conversation.active_context_tokens();
            if active_context_tokens > EFFECTIVE_CONTEXT_WINDOW {
                return Err(anyhow!(
                    "active conversation requires {active_context_tokens} tokens, exceeding bettercodex's {EFFECTIVE_CONTEXT_WINDOW}-token effective context window"
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
        emit(events, AgentEvent::CompactionStarted);
        self.conversation.normalize()?;
        let history_cursor = self.conversation.history_cursor();
        let compacted = {
            let request =
                self.api
                    .compact_append_only(self.conversation.items(), history_cursor, compaction);
            tokio::pin!(request);
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => None,
                compacted = &mut request => Some(compacted),
            }
        };
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
        let (user_message, prompt_text, selected_skills) = input.into_message_and_skills();
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
        projected.push(user_message.clone());
        let mut projection = self.conversation.project_append(projected);
        let incoming_tokens = projection.additional_tokens();
        if incoming_tokens > EFFECTIVE_CONTEXT_WINDOW {
            return Err(anyhow!(
                "input alone is estimated at {incoming_tokens} tokens, exceeding bettercodex's {EFFECTIVE_CONTEXT_WINDOW}-token effective context window; shorten the prompt or attach fewer images"
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
        if projected_tokens > EFFECTIVE_CONTEXT_WINDOW {
            return Err(anyhow!(
                "input would require an estimated {projected_tokens} tokens after compaction, exceeding bettercodex's {EFFECTIVE_CONTEXT_WINDOW}-token effective context window; shorten the prompt or attach fewer images"
            ));
        }
        self.conversation
            .record_operator_input(OperatorInputRecord {
                message: user_message,
                prompt_text,
                selected_skills,
                skill_context: skill_context.clone(),
            })?;
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
