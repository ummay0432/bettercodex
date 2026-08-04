use crate::api::ApiClient;
use crate::api::ApiError;
use crate::api::ModelResponse;
use crate::auth::Auth;
use crate::compaction::CompactionPhase;
use crate::compaction::InitialContextInjection;
use crate::context::ContextSnapshot;
use crate::context::Conversation;
use crate::events::AgentEvent;
use crate::input::UserInput;
use crate::rollout::LoadedRollout;
use crate::rollout::ResumeSelector;
use crate::rollout::Rollout;
use crate::rollout::TurnOutcome;
use crate::tools::ToolResult;
use crate::tools::ToolRuntime;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value;
use std::collections::VecDeque;
use std::future::pending;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
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

#[derive(Clone)]
pub(crate) struct TurnHandle {
    cancellation: CancellationToken,
    steering: UnboundedSender<UserInput>,
}

pub(crate) struct TurnControl {
    cancellation: CancellationToken,
    steering: Option<UnboundedReceiver<UserInput>>,
}

impl TurnHandle {
    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub(crate) fn steer(&self, input: UserInput) -> Result<()> {
        self.steering
            .send(input)
            .map_err(|_| anyhow!("the active turn already finished"))
    }
}

impl TurnControl {
    pub(crate) fn channel() -> (TurnHandle, Self) {
        let cancellation = CancellationToken::new();
        let (steering, steering_rx) = unbounded_channel();
        (
            TurnHandle {
                cancellation: cancellation.clone(),
                steering,
            },
            Self {
                cancellation,
                steering: Some(steering_rx),
            },
        )
    }

    fn cancellation_only(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            steering: None,
        }
    }
}

pub(crate) struct Agent {
    cwd: PathBuf,
    api: ApiClient,
    conversation: Conversation,
    tools: ToolRuntime,
}

impl Agent {
    pub(crate) fn new(cwd: impl AsRef<Path>) -> Result<Self> {
        let cwd = canonical_directory(cwd.as_ref())?;
        let rollout = Rollout::create(&cwd)?;
        let identity = rollout.identity().clone();
        let conversation = Conversation::new(&cwd, rollout)?;
        let auth = Auth::load()?;
        let api = ApiClient::new(auth, &identity, 0)?;
        let tools = ToolRuntime::new(cwd.clone(), api.web_search_client());
        Ok(Self {
            cwd,
            api,
            conversation,
            tools,
        })
    }

    pub(crate) fn resume(cwd: impl AsRef<Path>, selector: ResumeSelector) -> Result<Self> {
        let requested_cwd = canonical_directory(cwd.as_ref())?;
        let loaded = Rollout::resume(selector, &requested_cwd)?;
        Self::from_loaded_rollout(loaded)
    }

    fn from_loaded_rollout(loaded: LoadedRollout) -> Result<Self> {
        let cwd = canonical_directory(&loaded.metadata.cwd)?;
        let identity = loaded.metadata.identity.clone();
        let compaction_count = loaded.compaction_count;
        let conversation = Conversation::resume(&cwd, loaded)?;
        let auth = Auth::load()?;
        let api = ApiClient::new(auth, &identity, compaction_count)?;
        let tools = ToolRuntime::new(cwd.clone(), api.web_search_client());
        Ok(Self {
            cwd,
            api,
            conversation,
            tools,
        })
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn session_id(&self) -> &str {
        self.conversation.session_id()
    }

    pub(crate) fn context_tokens(&self) -> Option<u64> {
        self.conversation.context_tokens()
    }

    pub(crate) fn context_snapshot(&self) -> ContextSnapshot {
        self.conversation.context_snapshot()
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

    async fn submit_input(
        &mut self,
        input: UserInput,
        events: Option<UnboundedSender<AgentEvent>>,
        mut control: TurnControl,
    ) -> Result<SubmitOutcome> {
        if input.is_empty() {
            return Err(anyhow!("prompt and image list are both empty"));
        }
        let turn_id = self.api.begin_turn().to_string();
        self.conversation.start_turn(&turn_id)?;
        let result = self.run_turn(input, &events, &mut control).await;
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
        control: &mut TurnControl,
    ) -> Result<SubmitOutcome> {
        if !self
            .record_incoming_user(
                input,
                events,
                &control.cancellation,
                CompactionPhase::PreTurn,
            )
            .await?
        {
            return Ok(SubmitOutcome::Cancelled);
        }

        let mut transcript = Vec::new();
        let mut pending_steering = VecDeque::new();
        // Sample the fresh turn input first. After a mid-turn compact, sample the
        // compacted tool continuation once before inserting queued steering.
        let mut can_record_pending_steering = false;
        loop {
            if can_record_pending_steering {
                drain_available_steering(&mut control.steering, &mut pending_steering);
                while let Some(input) = pending_steering.pop_front() {
                    if !self
                        .record_incoming_user(
                            input,
                            events,
                            &control.cancellation,
                            CompactionPhase::MidTurn,
                        )
                        .await?
                    {
                        return Ok(SubmitOutcome::Cancelled);
                    }
                }
            }
            can_record_pending_steering = true;

            let response = match self
                .sample_with_recovery(events, control, &mut pending_steering)
                .await?
            {
                SamplingOutcome::Response(response) => response,
                SamplingOutcome::Steered => continue,
                SamplingOutcome::Cancelled => return Ok(SubmitOutcome::Cancelled),
            };
            if !response.text.trim().is_empty() {
                transcript.push(response.text.trim().to_string());
            }
            self.conversation
                .record_usage(response.usage, response.server_reasoning_included)?;
            self.emit_context(events);
            emit(events, AgentEvent::ModelResponseCompleted);

            let mut tool_calls = response.tool_calls.into_iter();
            if tool_calls.len() == 0 {
                drain_available_steering(&mut control.steering, &mut pending_steering);
                if !pending_steering.is_empty() {
                    continue;
                }
                if transcript.is_empty() {
                    return Err(anyhow!("model returned no text or tool call"));
                }
                return Ok(SubmitOutcome::Completed(transcript.join("\n")));
            }

            while let Some(tool_call) = tool_calls.next() {
                let output = self
                    .execute_tool(&tool_call, events, control, &mut pending_steering)
                    .await;
                self.conversation.extend(tool_call.output_items(&output))?;
                self.emit_context(events);
                if control.cancellation.is_cancelled() {
                    for pending in tool_calls {
                        let output = ToolResult::text(
                            "tool error: skipped after user interruption".to_string(),
                        );
                        self.conversation.extend(pending.output_items(&output))?;
                    }
                    return Ok(SubmitOutcome::Cancelled);
                }
            }
            if self.conversation.needs_compaction() {
                if !self
                    .run_compaction(
                        events,
                        &control.cancellation,
                        CompactionPhase::MidTurn,
                        InitialContextInjection::BeforeLastUserMessage,
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
        control: &mut TurnControl,
        pending_steering: &mut VecDeque<UserInput>,
    ) -> Result<SamplingOutcome> {
        let mut retries = 0_usize;
        loop {
            self.conversation.normalize()?;
            let (completed_tx, mut completed_rx) = unbounded_channel::<Value>();
            let mut observed_item = false;
            let wait = {
                let api = &mut self.api;
                let conversation = &mut self.conversation;
                // Keep one stable snapshot while completed stream items are journaled into the
                // live conversation. The API consumes this allocation through request assembly.
                let history = conversation.items().to_vec();
                let response = async {
                    match events {
                        Some(events) => api.respond_streaming(history, &completed_tx, events).await,
                        None => api.respond(history, &completed_tx).await,
                    }
                };
                tokio::pin!(response);
                let mut completed_closed = false;
                loop {
                    tokio::select! {
                        result = &mut response => break SamplingWait::Finished(result),
                        _ = control.cancellation.cancelled() => break SamplingWait::Cancelled,
                        steering = receive_steering(&mut control.steering) => {
                            match steering {
                                Some(input) => break SamplingWait::Steered(input),
                                None => control.steering = None,
                            }
                        }
                        item = completed_rx.recv(), if !completed_closed => {
                            match item {
                                Some(item) => {
                                    conversation.extend([item])?;
                                    observed_item = true;
                                }
                                None => completed_closed = true,
                            }
                        }
                    }
                }
            };
            while let Ok(item) = completed_rx.try_recv() {
                self.conversation.extend([item])?;
                observed_item = true;
            }

            match wait {
                SamplingWait::Finished(Ok(response)) => {
                    return Ok(SamplingOutcome::Response(response));
                }
                SamplingWait::Cancelled => {
                    self.api.abandon_response();
                    return Ok(SamplingOutcome::Cancelled);
                }
                SamplingWait::Steered(input) => {
                    self.api.abandon_response();
                    if observed_item {
                        self.conversation.mark_stream_interrupted(
                            "the user sent steering input while the response was still streaming",
                        )?;
                    }
                    pending_steering.push_back(input);
                    drain_available_steering(&mut control.steering, pending_steering);
                    return Ok(SamplingOutcome::Steered);
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
                    let delay = sleep(error.retry_after().unwrap_or_else(|| retry_delay(retries)));
                    tokio::pin!(delay);
                    loop {
                        tokio::select! {
                            _ = &mut delay => break,
                            _ = control.cancellation.cancelled() => {
                                return Ok(SamplingOutcome::Cancelled);
                            }
                            steering = receive_steering(&mut control.steering), if control.steering.is_some() => {
                                match steering {
                                    Some(input) => {
                                        pending_steering.push_back(input);
                                        drain_available_steering(&mut control.steering, pending_steering);
                                        return Ok(SamplingOutcome::Steered);
                                    }
                                    None => control.steering = None,
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    async fn execute_tool(
        &self,
        tool_call: &crate::tools::ToolCall,
        events: &Option<UnboundedSender<AgentEvent>>,
        control: &mut TurnControl,
        pending_steering: &mut VecDeque<UserInput>,
    ) -> ToolResult {
        let context = self.api.tool_turn_context(self.conversation.items());
        let execution = tool_call.execute(
            &self.tools,
            context,
            events.clone(),
            control.cancellation.clone(),
        );
        tokio::pin!(execution);
        loop {
            tokio::select! {
                output = &mut execution => return output,
                steering = receive_steering(&mut control.steering) => {
                    match steering {
                        Some(input) => pending_steering.push_back(input),
                        None => control.steering = None,
                    }
                }
            }
        }
    }

    async fn run_compaction(
        &mut self,
        events: &Option<UnboundedSender<AgentEvent>>,
        cancellation: &CancellationToken,
        phase: CompactionPhase,
        initial_context_injection: InitialContextInjection,
    ) -> Result<bool> {
        emit(events, AgentEvent::CompactionStarted);
        self.conversation.normalize()?;
        let compacted = tokio::select! {
            _ = cancellation.cancelled() => return Ok(false),
            compacted = self.api.compact(self.conversation.items(), phase) => compacted?,
        };
        let _compaction_usage = compacted.usage;
        self.conversation
            .replace_compacted(compacted.items, initial_context_injection)?;
        self.emit_context(events);
        emit(events, AgentEvent::CompactionCompleted);
        Ok(true)
    }

    async fn record_incoming_user(
        &mut self,
        input: UserInput,
        events: &Option<UnboundedSender<AgentEvent>>,
        cancellation: &CancellationToken,
        phase: CompactionPhase,
    ) -> Result<bool> {
        let projected = input.clone().into_message();
        if self
            .conversation
            .needs_compaction_with(std::slice::from_ref(&projected))
            && !self
                .run_compaction(
                    events,
                    cancellation,
                    phase,
                    InitialContextInjection::AfterCompaction,
                )
                .await?
        {
            return Ok(false);
        }
        self.conversation.push_user(input)?;
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
    Steered(UserInput),
}

enum SamplingOutcome {
    Response(ModelResponse),
    Cancelled,
    Steered,
}

async fn receive_steering(
    steering: &mut Option<UnboundedReceiver<UserInput>>,
) -> Option<UserInput> {
    match steering {
        Some(steering) => steering.recv().await,
        None => pending().await,
    }
}

fn drain_available_steering(
    steering: &mut Option<UnboundedReceiver<UserInput>>,
    pending: &mut VecDeque<UserInput>,
) {
    let Some(receiver) = steering.as_mut() else {
        return;
    };
    let mut disconnected = false;
    loop {
        match receiver.try_recv() {
            Ok(input) => pending.push_back(input),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }
    if disconnected {
        *steering = None;
    }
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

fn retry_delay(retry: usize) -> Duration {
    Duration::from_secs(1_u64 << retry.saturating_sub(1).min(4))
}

fn emit(events: &Option<UnboundedSender<AgentEvent>>, event: AgentEvent) {
    if let Some(events) = events {
        let _ = events.send(event);
    }
}
