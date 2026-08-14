use crate::assistant_message::AssistantMessage;
use crate::auth::Auth;
use crate::auth::AuthSnapshot;
use crate::auth::SharedAuth;
use crate::compaction;
use crate::compaction::CompactionRequest;
use crate::context::HistoryCursor;
use crate::context::estimated_tokens;
use crate::events::AgentEvent;
use crate::events::ModelTextDelta;
use crate::http_client::backoff;
use crate::http_client::bounded_error_body;
use crate::model::ModelSelection;
use crate::model::ReasoningEffort;
use crate::model::SharedModelSelection;
use crate::rollout::SessionIdentity;
use crate::service_tier::ServiceTier;
use crate::time::unix_timestamp_millis;
use crate::tools;
use crate::tools::ToolCall;
use crate::usage::TokenUsage;
use crate::web_search::WebSearchCall;
use anyhow::Context;
use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header::CONTENT_ENCODING;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use serde::Serialize;
use serde::ser::SerializeMap;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::sleep;

#[path = "api_websocket.rs"]
mod websocket;
use websocket::WebSocketConnection;

#[path = "api_sse.rs"]
mod sse;
use sse::SseDecoder;

const BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");
const MAX_HTTP_RETRIES: usize = 4;
const MAX_REMOTE_COMPACTION_V2_STREAM_RETRIES: usize = 2;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(200);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const WEBSOCKET_PREWARM_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_STREAM_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 16_000;
const MAX_ERROR_BODY_CHARS: usize = 4_000;
// Encoding is on the critical path before network I/O. Level 1 retains strong compression for
// long-context JSON while reducing CPU time versus zstd's level-3 default.
const REQUEST_COMPRESSION_LEVEL: i32 = 1;
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const REMOTE_COMPACTION_V2_FEATURE: &str = "remote_compaction_v2";
const X_CODEX_ROUTING_HINT: &str = "x-codex-routing-hint";
const X_CODEX_TURN_STATE: &str = "x-codex-turn-state";
static STABLE_HARNESS_TOKEN_ESTIMATES: OnceLock<[u64; 2]> = OnceLock::new();
static RESPONSES_API_SPECIFICATIONS_JSON: OnceLock<Box<RawValue>> = OnceLock::new();

pub(crate) type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApiErrorKind {
    Fatal,
    Retryable,
    StreamIdle,
    Unauthorized,
    PreviousResponseNotFound,
    WebSocketUnavailable,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    kind: ApiErrorKind,
    message: String,
    retry_after: Option<Duration>,
}

impl ApiError {
    fn new(kind: ApiErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retry_after: None,
        }
    }

    pub(super) fn fatal(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::Fatal, message)
    }

    pub(super) fn retryable(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::Retryable, message)
    }

    pub(super) fn stream_idle(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::StreamIdle, message)
    }

    fn retryable_after(message: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self {
            kind: ApiErrorKind::Retryable,
            message: message.into(),
            retry_after,
        }
    }

    pub(super) fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::Unauthorized, message)
    }

    pub(super) fn websocket_unavailable(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::WebSocketUnavailable, message)
    }

    pub(crate) fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            ApiErrorKind::Retryable
                | ApiErrorKind::StreamIdle
                | ApiErrorKind::PreviousResponseNotFound
                | ApiErrorKind::WebSocketUnavailable
        )
    }

    pub(crate) fn is_stream_idle(&self) -> bool {
        self.kind == ApiErrorKind::StreamIdle
    }

    fn into_retryable(mut self) -> Self {
        self.kind = ApiErrorKind::Retryable;
        self
    }

    pub(crate) fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

pub(crate) struct ApiClient {
    client: reqwest::Client,
    auth: SharedAuth,
    base_url: String,
    installation_id: String,
    session_id: String,
    thread_id: String,
    turn_id: String,
    turn_started_at_unix_ms: u64,
    turn_state: Option<String>,
    window: u64,
    model_selection: SharedModelSelection,
    service_tier: ServiceTier,
    prefer_websocket: bool,
    websocket_prewarm_attempted: bool,
    websocket: Option<WebSocketConnection>,
    startup_websocket_pending_first_event: bool,
    websocket_reasoning_included: bool,
    websocket_server_model: Option<String>,
    websocket_rate_limits: Vec<crate::rate_limits::RateLimitSnapshot>,
    websocket_baseline: Option<WebSocketBaseline>,
    server_model_warning_emitted: bool,
    stream_idle_timeout: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct HarnessInstructions;

impl Serialize for HarnessInstructions {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(harness_instructions())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ResponsesApiTools;

impl Serialize for ResponsesApiTools {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        responses_api_specifications_json().serialize(serializer)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
struct RequestReasoning {
    effort: ReasoningEffort,
    context: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
struct RequestText {
    verbosity: &'static str,
}

#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    instructions: HarnessInstructions,
    tools: ResponsesApiTools,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: RequestReasoning,
    store: bool,
    stream: bool,
    include: [&'static str; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<&'static str>,
    prompt_cache_key: String,
    text: RequestText,
    client_metadata: Map<String, Value>,
    input: Vec<Value>,
}

/// Small owned identity for the request properties covered by `previous_response_id`.
///
/// The input and client metadata vary independently. Everything else is copied without retaining
/// conversation history; fixed instructions and tools are zero-sized handles to shared data.
struct RequestPropertyIdentity {
    model: String,
    instructions: HarnessInstructions,
    tools: ResponsesApiTools,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: RequestReasoning,
    store: bool,
    stream: bool,
    include: [&'static str; 1],
    service_tier: Option<&'static str>,
    prompt_cache_key: String,
    text: RequestText,
}

impl RequestPropertyIdentity {
    fn new(request: &ResponsesRequest) -> Self {
        Self {
            model: request.model.clone(),
            instructions: request.instructions,
            tools: request.tools,
            tool_choice: request.tool_choice,
            parallel_tool_calls: request.parallel_tool_calls,
            reasoning: request.reasoning,
            store: request.store,
            stream: request.stream,
            include: request.include,
            service_tier: request.service_tier,
            prompt_cache_key: request.prompt_cache_key.clone(),
            text: request.text,
        }
    }

    fn matches(&self, request: &ResponsesRequest) -> bool {
        let ResponsesRequest {
            model,
            instructions,
            input: _,
            tools,
            tool_choice,
            parallel_tool_calls,
            reasoning,
            store,
            stream,
            include,
            service_tier,
            prompt_cache_key,
            text,
            client_metadata: _,
        } = request;
        self.model == *model
            && self.instructions == *instructions
            && self.tools == *tools
            && self.tool_choice == *tool_choice
            && self.parallel_tool_calls == *parallel_tool_calls
            && self.reasoning == *reasoning
            && self.store == *store
            && self.stream == *stream
            && self.include == *include
            && self.service_tier == *service_tier
            && self.prompt_cache_key == *prompt_cache_key
            && self.text == *text
    }
}

struct WebSocketClientMetadata<'a> {
    values: &'a Map<String, Value>,
    turn_state: Option<String>,
}

impl Serialize for WebSocketClientMetadata<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let replaced_turn_state = usize::from(self.values.contains_key(X_CODEX_TURN_STATE));
        let inserted_turn_state = usize::from(self.turn_state.is_some());
        let mut metadata = serializer.serialize_map(Some(
            self.values
                .len()
                .saturating_sub(replaced_turn_state)
                .saturating_add(inserted_turn_state),
        ))?;
        for (name, value) in self.values {
            if name != X_CODEX_TURN_STATE {
                metadata.serialize_entry(name, value)?;
            }
        }
        if let Some(turn_state) = &self.turn_state {
            metadata.serialize_entry(X_CODEX_TURN_STATE, turn_state)?;
        }
        metadata.end()
    }
}

// WebSocket `response.create` omits HTTP transport switches such as `stream` and
// `background`; streaming is implicit in the socket protocol.
#[derive(Serialize)]
struct WebSocketRequest<'a> {
    #[serde(rename = "type")]
    request_type: &'static str,
    model: &'a str,
    instructions: HarnessInstructions,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<String>,
    tools: ResponsesApiTools,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: RequestReasoning,
    store: bool,
    include: [&'static str; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<&'static str>,
    prompt_cache_key: &'a str,
    text: RequestText,
    #[serde(skip_serializing_if = "Option::is_none")]
    generate: Option<bool>,
    client_metadata: WebSocketClientMetadata<'a>,
    input: &'a [Value],
}

struct WebSocketBaseline {
    properties: RequestPropertyIdentity,
    input: WebSocketBaselineInput,
    response_id: String,
}

enum WebSocketBaselineInput {
    Exact {
        request: Vec<Value>,
        output: Vec<Value>,
    },
    AppendOnly {
        cursor: HistoryCursor,
        request_len: usize,
        response_items: usize,
    },
}

#[derive(Clone, Copy)]
enum RequestInputIdentity {
    Exact,
    AppendOnly {
        cursor: HistoryCursor,
        trailing_items: usize,
    },
}

#[derive(Clone, Copy)]
enum OutputItemMode {
    /// Keep output in the response and publish a copy to the consumer.
    RetainAndEmit,
    /// Keep output only in the response (compaction and connection warmup).
    Retain,
    /// Move output directly to conversation history and retain only response metadata.
    Transfer,
}

struct ResponseValidation<'a> {
    expected_model: &'a str,
    server_model_warning_emitted: &'a mut bool,
}

impl OutputItemMode {
    fn for_http(request_kind: RequestKind) -> Self {
        match request_kind {
            RequestKind::Turn => Self::Transfer,
            RequestKind::Prewarm | RequestKind::Compaction(_) => Self::Retain,
        }
    }

    fn for_websocket(request_kind: RequestKind, input_identity: RequestInputIdentity) -> Self {
        match (request_kind, input_identity) {
            (
                RequestKind::Turn,
                RequestInputIdentity::AppendOnly {
                    trailing_items: 0, ..
                },
            ) => Self::Transfer,
            (RequestKind::Turn, _) => Self::RetainAndEmit,
            (RequestKind::Prewarm | RequestKind::Compaction(_), _) => Self::Retain,
        }
    }
}

pub(crate) struct SamplingRequest {
    request: ResponsesRequest,
    cursor: HistoryCursor,
}

impl WebSocketBaseline {
    fn new(
        request: &ResponsesRequest,
        response: &ModelResponse,
        input_identity: RequestInputIdentity,
    ) -> ApiResult<Self> {
        let input = match input_identity {
            RequestInputIdentity::AppendOnly {
                cursor,
                trailing_items: 0,
            } => WebSocketBaselineInput::AppendOnly {
                cursor,
                request_len: request.input.len(),
                response_items: response.output_item_count,
            },
            RequestInputIdentity::Exact | RequestInputIdentity::AppendOnly { .. } => {
                if response.items.len() != response.output_item_count {
                    return Err(ApiError::fatal(
                        "an exact WebSocket baseline did not retain its output items",
                    ));
                }
                WebSocketBaselineInput::Exact {
                    request: request.input.clone(),
                    output: response.items.clone(),
                }
            }
        };
        Ok(Self {
            properties: RequestPropertyIdentity::new(request),
            input,
            response_id: response.response_id.clone(),
        })
    }
}

impl SamplingRequest {
    pub(crate) fn into_history(self) -> (Vec<Value>, HistoryCursor) {
        (self.request.input, self.cursor)
    }
}

#[derive(Clone, Copy)]
enum WebSocketRequestMode {
    Inference,
    Warmup,
}

enum WebSocketPrewarmOutcome {
    Ready { refreshed_auth: bool },
    Failed(ApiError),
}

pub(crate) struct ModelResponse {
    // Compaction and exact WebSocket-baseline responses retain their output. Normal sampling moves
    // those values into conversation history and leaves this vector empty.
    pub(crate) items: Vec<Value>,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) final_answer: Option<String>,
    pub(crate) end_turn: Option<bool>,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) rate_limits: Vec<crate::rate_limits::RateLimitSnapshot>,
    pub(crate) server_reasoning_included: bool,
    response_id: String,
    output_item_count: usize,
    has_assistant_text: bool,
}

impl ModelResponse {
    pub(crate) fn has_assistant_text(&self) -> bool {
        self.has_assistant_text
    }
}

#[derive(Debug)]
pub(crate) struct CompactionResult {
    pub(crate) items: Vec<Value>,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) rate_limits: Vec<crate::rate_limits::RateLimitSnapshot>,
}

impl ApiClient {
    pub(crate) fn new(
        auth: Auth,
        identity: &SessionIdentity,
        compaction_count: u64,
        model_selection: ModelSelection,
        service_tier: ServiceTier,
    ) -> anyhow::Result<Self> {
        Self::new_with_base_url(
            auth,
            identity,
            compaction_count,
            model_selection,
            service_tier,
            BASE_URL.to_string(),
        )
    }

    pub(crate) fn new_with_base_url(
        auth: Auth,
        identity: &SessionIdentity,
        compaction_count: u64,
        model_selection: ModelSelection,
        service_tier: ServiceTier,
        base_url: String,
    ) -> anyhow::Result<Self> {
        crate::http_client::ensure_rustls_crypto_provider();
        let mut default_headers = HeaderMap::new();
        default_headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));
        let client = crate::http_client::build_client(
            crate::http_client::with_chatgpt_cloudflare_cookie_store(reqwest::Client::builder())
                .default_headers(default_headers)
                .user_agent(concat!("bettercodex/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(20)),
        )
        .context("failed to create HTTP client")?;
        Ok(Self {
            client,
            auth: SharedAuth::new(auth),
            base_url,
            installation_id: identity.installation_id.clone(),
            session_id: identity.session_id.clone(),
            thread_id: identity.thread_id.clone(),
            turn_id: uuid::Uuid::new_v4().to_string(),
            turn_started_at_unix_ms: unix_timestamp_millis(),
            turn_state: None,
            window: compaction_count,
            model_selection: SharedModelSelection::new(model_selection),
            service_tier,
            prefer_websocket: true,
            websocket_prewarm_attempted: false,
            websocket: None,
            startup_websocket_pending_first_event: false,
            websocket_reasoning_included: false,
            websocket_server_model: None,
            websocket_rate_limits: Vec::new(),
            websocket_baseline: None,
            server_model_warning_emitted: false,
            stream_idle_timeout: STREAM_IDLE_TIMEOUT,
        })
    }

    pub(crate) fn startup_prewarm_client(&self) -> Self {
        Self {
            client: self.client.clone(),
            auth: self.auth.clone(),
            base_url: self.base_url.clone(),
            installation_id: self.installation_id.clone(),
            session_id: self.session_id.clone(),
            thread_id: self.thread_id.clone(),
            turn_id: uuid::Uuid::new_v4().to_string(),
            turn_started_at_unix_ms: unix_timestamp_millis(),
            turn_state: None,
            window: self.window,
            model_selection: self.model_selection.clone(),
            service_tier: self.service_tier,
            prefer_websocket: self.prefer_websocket,
            websocket_prewarm_attempted: false,
            websocket: None,
            startup_websocket_pending_first_event: false,
            websocket_reasoning_included: false,
            websocket_server_model: None,
            websocket_rate_limits: Vec::new(),
            websocket_baseline: None,
            server_model_warning_emitted: false,
            stream_idle_timeout: self.stream_idle_timeout,
        }
    }

    pub(crate) async fn prewarm_for_startup(mut self) -> ApiResult<Self> {
        match self.attempt_websocket_prewarm(None).await {
            Ok(WebSocketPrewarmOutcome::Ready { .. }) => Ok(self),
            Ok(WebSocketPrewarmOutcome::Failed(error)) | Err(error) => Err(error),
        }
    }

    pub(crate) fn adopt_startup_prewarm(&mut self, mut prewarmed: Self) {
        self.startup_websocket_pending_first_event = prewarmed.websocket.is_some();
        self.prefer_websocket = prewarmed.prefer_websocket;
        self.websocket_prewarm_attempted = prewarmed.websocket_prewarm_attempted;
        self.websocket = prewarmed.websocket.take();
        self.websocket_reasoning_included = prewarmed.websocket_reasoning_included;
        self.websocket_server_model = prewarmed.websocket_server_model.take();
        self.websocket_rate_limits = std::mem::take(&mut prewarmed.websocket_rate_limits);
        self.websocket_baseline = prewarmed.websocket_baseline.take();
        self.turn_state = prewarmed.turn_state.take();
    }

    pub(crate) fn mark_websocket_prewarm_attempted(&mut self) {
        self.websocket_prewarm_attempted = true;
    }

    pub(crate) fn begin_turn(&mut self) -> &str {
        self.turn_id = uuid::Uuid::new_v4().to_string();
        self.turn_started_at_unix_ms = unix_timestamp_millis();
        self.turn_state = None;
        self.server_model_warning_emitted = false;
        &self.turn_id
    }

    pub(crate) fn compaction_count(&self) -> u64 {
        self.window
    }

    pub(crate) fn set_service_tier(&mut self, service_tier: ServiceTier) {
        if self.service_tier == service_tier {
            return;
        }
        self.service_tier = service_tier;
        // A routing change invalidates both the incremental request baseline and the routing hint
        // attached to the existing WebSocket handshake. Reconnect on the next request.
        self.abandon_response();
    }

    pub(crate) fn set_model_selection(&mut self, selection: ModelSelection) {
        if self.model_selection.get() == selection {
            return;
        }
        self.prefer_websocket = true;
        self.model_selection.set(selection);
        self.websocket_prewarm_attempted = false;
        // Model, tool transport, reasoning fields, and routing headers are all
        // connection/baseline properties.
        self.abandon_response();
    }

    pub(crate) fn commit_compaction(&mut self) {
        self.window = self.window.saturating_add(1);
        // Compaction replaces history instead of appending to it, so no prefix
        // from the compaction request is a valid baseline for the next sample.
        self.websocket_baseline = None;
    }

    pub(crate) fn rate_limit_client(&self) -> crate::rate_limits::RateLimitClient {
        crate::rate_limits::RateLimitClient::new(
            self.client.clone(),
            self.auth.clone(),
            &self.base_url,
        )
    }

    pub(crate) fn abandon_response(&mut self) {
        self.websocket = None;
        self.startup_websocket_pending_first_event = false;
        self.websocket_reasoning_included = false;
        self.websocket_server_model = None;
        self.websocket_rate_limits.clear();
        self.websocket_baseline = None;
    }

    pub(crate) fn fall_back_to_http(&mut self) -> bool {
        if !self.prefer_websocket {
            return false;
        }
        self.prefer_websocket = false;
        self.abandon_response();
        true
    }

    fn recover_websocket_inactivity(
        &mut self,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> bool {
        if !self.fall_back_to_http() {
            return false;
        }
        if let Some(events) = events {
            let _ = events.send(AgentEvent::Warning(
                "Responses WebSocket became inactive; retrying once over HTTPS".to_string(),
            ));
        }
        true
    }

    pub(crate) async fn respond_sampling(
        &mut self,
        request: &SamplingRequest,
        completed_items: &UnboundedSender<Value>,
    ) -> ApiResult<ModelResponse> {
        self.respond_sampling_with_events(request, completed_items, None)
            .await
    }

    pub(crate) async fn respond_sampling_streaming(
        &mut self,
        request: &SamplingRequest,
        completed_items: &UnboundedSender<Value>,
        events: &UnboundedSender<AgentEvent>,
    ) -> ApiResult<ModelResponse> {
        self.respond_sampling_with_events(request, completed_items, Some(events))
            .await
    }

    async fn respond_sampling_with_events(
        &mut self,
        request: &SamplingRequest,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> ApiResult<ModelResponse> {
        self.respond_request_with_events(
            &request.request,
            completed_items,
            events,
            RequestKind::Turn,
            RequestInputIdentity::AppendOnly {
                cursor: request.cursor,
                trailing_items: 0,
            },
        )
        .await
    }

    async fn respond_request_with_events(
        &mut self,
        request: &ResponsesRequest,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
        request_kind: RequestKind,
        input_identity: RequestInputIdentity,
    ) -> ApiResult<ModelResponse> {
        let mut refreshed_websocket_auth = match self.attempt_websocket_prewarm(events).await? {
            WebSocketPrewarmOutcome::Ready { refreshed_auth } => refreshed_auth,
            WebSocketPrewarmOutcome::Failed(_) => false,
        };
        let mut retried_full_websocket_request = false;
        loop {
            if self.prefer_websocket {
                match self
                    .respond_websocket(
                        request,
                        completed_items,
                        events,
                        request_kind,
                        WebSocketRequestMode::Inference,
                        input_identity,
                    )
                    .await
                {
                    Ok(response) => {
                        // Compaction consumes the previous turn baseline but immediately replaces
                        // conversation lineage. Retaining its full request here would deep-clone a
                        // near-window history only to discard it after output validation.
                        if matches!(request_kind, RequestKind::Turn) {
                            self.websocket_baseline =
                                Some(WebSocketBaseline::new(request, &response, input_identity)?);
                        }
                        return Ok(response);
                    }
                    Err(error)
                        if error.kind == ApiErrorKind::Unauthorized
                            && !refreshed_websocket_auth =>
                    {
                        self.auth
                            .force_refreshed_snapshot(&self.client)
                            .await
                            .map_err(|error| ApiError::fatal(format!("{error:#}")))?;
                        refreshed_websocket_auth = true;
                        self.abandon_response();
                        continue;
                    }
                    Err(error)
                        if error.kind == ApiErrorKind::PreviousResponseNotFound
                            && !retried_full_websocket_request =>
                    {
                        self.websocket_baseline = None;
                        retried_full_websocket_request = true;
                        continue;
                    }
                    Err(error) if error.kind == ApiErrorKind::WebSocketUnavailable => {
                        self.fall_back_to_http();
                    }
                    Err(error) if error.is_stream_idle() => {
                        if !self.recover_websocket_inactivity(events) {
                            return Err(error);
                        }
                    }
                    Err(error) => {
                        self.abandon_response();
                        return Err(error);
                    }
                }
            }

            return self
                .respond_http(request, completed_items, events, request_kind)
                .await;
        }
    }

    async fn attempt_websocket_prewarm(
        &mut self,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> ApiResult<WebSocketPrewarmOutcome> {
        if !self.prefer_websocket || self.websocket_prewarm_attempted {
            return Ok(WebSocketPrewarmOutcome::Ready {
                refreshed_auth: false,
            });
        }
        self.websocket_prewarm_attempted = true;
        match self.prewarm_websocket().await {
            Ok(()) => Ok(WebSocketPrewarmOutcome::Ready {
                refreshed_auth: false,
            }),
            Err(error) if error.kind == ApiErrorKind::Unauthorized => {
                self.auth
                    .force_refreshed_snapshot(&self.client)
                    .await
                    .map_err(|error| ApiError::fatal(format!("{error:#}")))?;
                self.abandon_response();
                Ok(WebSocketPrewarmOutcome::Ready {
                    refreshed_auth: true,
                })
            }
            Err(error) if error.kind == ApiErrorKind::WebSocketUnavailable => {
                self.fall_back_to_http();
                Ok(WebSocketPrewarmOutcome::Ready {
                    refreshed_auth: false,
                })
            }
            Err(error) if error.is_stream_idle() => {
                if !self.recover_websocket_inactivity(events) {
                    return Err(error);
                }
                Ok(WebSocketPrewarmOutcome::Ready {
                    refreshed_auth: false,
                })
            }
            Err(error) => {
                // Warmup is an optimization. A normal full request remains the
                // authoritative path when the connection or warmup response fails.
                self.abandon_response();
                Ok(WebSocketPrewarmOutcome::Failed(error))
            }
        }
    }

    async fn respond_http(
        &mut self,
        request: &ResponsesRequest,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
        request_kind: RequestKind,
    ) -> ApiResult<ModelResponse> {
        let expected_model = request.model.clone();
        let response = self
            .post("responses", request, "text/event-stream", request_kind)
            .await?;
        let rate_limits = crate::rate_limits::parse_all_rate_limits(response.headers());
        observe_server_model(
            response
                .headers()
                .get("openai-model")
                .and_then(|value| value.to_str().ok()),
            &expected_model,
            events,
            &mut self.server_model_warning_emitted,
        );
        let server_reasoning_included = response.headers().contains_key("x-reasoning-included");
        self.capture_turn_state(response.headers());
        let mut response = collect_http_stream(
            response,
            completed_items,
            events,
            self.stream_idle_timeout,
            ResponseValidation {
                expected_model: &expected_model,
                server_model_warning_emitted: &mut self.server_model_warning_emitted,
            },
            OutputItemMode::for_http(request_kind),
        )
        .await?;
        response.server_reasoning_included = server_reasoning_included;
        response.rate_limits = rate_limits;
        Ok(response)
    }

    async fn prewarm_websocket(&mut self) -> ApiResult<()> {
        let request = self.build_request(Vec::new(), RequestKind::Prewarm);
        let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
        let response = self
            .respond_websocket(
                &request,
                &completed_items,
                None,
                RequestKind::Prewarm,
                WebSocketRequestMode::Warmup,
                RequestInputIdentity::Exact,
            )
            .await?;
        self.websocket_baseline = Some(WebSocketBaseline::new(
            &request,
            &response,
            RequestInputIdentity::Exact,
        )?);
        Ok(())
    }

    async fn respond_websocket(
        &mut self,
        logical_request: &ResponsesRequest,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
        request_kind: RequestKind,
        mode: WebSocketRequestMode,
        input_identity: RequestInputIdentity,
    ) -> ApiResult<ModelResponse> {
        let expected_model = logical_request.model.clone();
        self.ensure_websocket(request_kind).await?;
        let websocket_server_model = self.websocket_server_model.clone();
        observe_server_model(
            websocket_server_model.as_deref(),
            &expected_model,
            events,
            &mut self.server_model_warning_emitted,
        );
        let startup_websocket_pending_first_event = matches!(mode, WebSocketRequestMode::Inference)
            && self.startup_websocket_pending_first_event;
        let initial_idle_timeout = match mode {
            WebSocketRequestMode::Inference if startup_websocket_pending_first_event => {
                self.stream_idle_timeout.min(WEBSOCKET_PREWARM_IDLE_TIMEOUT)
            }
            WebSocketRequestMode::Inference => self.stream_idle_timeout,
            WebSocketRequestMode::Warmup => {
                self.stream_idle_timeout.min(WEBSOCKET_PREWARM_IDLE_TIMEOUT)
            }
        };
        // The wire request borrows either the full input or its incremental suffix. This keeps the
        // logical request intact for retries and baseline retention without moving or cloning its
        // potentially near-window history.
        let request = self.prepare_websocket_request(logical_request, mode, input_identity);
        self.websocket
            .as_mut()
            .ok_or_else(|| ApiError::websocket_unavailable("Responses WebSocket is unavailable"))?
            .send(&request, initial_idle_timeout)
            .await?;

        let mut collected =
            CollectedResponse::new(OutputItemMode::for_websocket(request_kind, input_identity));
        // A socket completed during startup can become half-open before the operator submits the
        // first turn. Bound only its first real send/event by the startup timeout; after any server
        // event proves the connection active, retain the ordinary timeout for long inference.
        let mut awaiting_startup_first_event = startup_websocket_pending_first_event;
        let mut next_event_idle_timeout = initial_idle_timeout;
        loop {
            let next_text = self
                .websocket
                .as_mut()
                .ok_or_else(|| {
                    ApiError::websocket_unavailable("Responses WebSocket is unavailable")
                })?
                .next_text(next_event_idle_timeout)
                .await;
            let text = match next_text {
                // Replaying the original request over HTTPS is safe only before a completed model
                // item enters history. After that, let the agent record the interrupted stream and
                // retry from the updated history instead of merging two generations.
                Err(error) if error.is_stream_idle() && collected.item_count() != 0 => {
                    return Err(error.into_retryable());
                }
                result => result?,
            }
            .ok_or_else(|| ApiError::retryable("Responses WebSocket ended unexpectedly"))?;
            if awaiting_startup_first_event {
                awaiting_startup_first_event = false;
                self.startup_websocket_pending_first_event = false;
                next_event_idle_timeout = self.stream_idle_timeout;
            }
            if text.len() > MAX_STREAM_EVENT_BYTES {
                return Err(ApiError::fatal("model sent an oversized WebSocket event"));
            }
            let event: Value = serde_json::from_str(text.as_str()).map_err(|error| {
                ApiError::fatal(format!("failed to decode WebSocket event: {error}"))
            })?;
            self.capture_event_turn_state(&event);
            process_event_value(
                event,
                &mut collected,
                completed_items,
                events,
                &expected_model,
                &mut self.server_model_warning_emitted,
            )?;
            if collected.completed {
                break;
            }
        }
        let mut response = collected.finish()?;
        response.server_reasoning_included = self.websocket_reasoning_included;
        let streamed_rate_limits = std::mem::take(&mut response.rate_limits);
        response.rate_limits.clone_from(&self.websocket_rate_limits);
        for snapshot in streamed_rate_limits {
            upsert_rate_limit(&mut response.rate_limits, snapshot);
        }
        self.websocket_rate_limits.clone_from(&response.rate_limits);
        Ok(response)
    }

    async fn ensure_websocket(&mut self, request_kind: RequestKind) -> ApiResult<()> {
        if self.websocket.is_some() {
            return Ok(());
        }
        let auth = self
            .auth
            .refreshed_snapshot(&self.client)
            .await
            .map_err(|error| ApiError::fatal(format!("{error:#}")))?;
        let mut headers = self.request_headers("*/*", request_kind, &auth)?;
        insert_header(&mut headers, "originator", "codex_cli_rs")?;
        insert_header(&mut headers, "openai-beta", RESPONSES_WEBSOCKET_BETA)?;
        let url = websocket_url(&self.base_url, "responses")?;
        let (websocket, response_headers) = WebSocketConnection::connect(&url, &headers).await?;
        self.websocket_reasoning_included = response_headers.contains_key("x-reasoning-included");
        self.websocket_server_model = response_headers
            .get("openai-model")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        self.websocket_rate_limits = crate::rate_limits::parse_all_rate_limits(&response_headers);
        self.capture_turn_state(&response_headers);
        self.websocket = Some(websocket);
        Ok(())
    }

    fn prepare_websocket_request<'a>(
        &self,
        request: &'a ResponsesRequest,
        mode: WebSocketRequestMode,
        input_identity: RequestInputIdentity,
    ) -> WebSocketRequest<'a> {
        let current_input = request.input.as_slice();
        let incremental = match mode {
            WebSocketRequestMode::Warmup => None,
            WebSocketRequestMode::Inference => {
                if let Some(baseline) = &self.websocket_baseline {
                    if !baseline.properties.matches(request) {
                        None
                    } else {
                        match &baseline.input {
                            WebSocketBaselineInput::Exact {
                                request: previous_input,
                                output,
                            } => {
                                let baseline_length =
                                    previous_input.len().saturating_add(output.len());
                                let expected = previous_input.iter().chain(output);
                                (current_input.len() >= baseline_length
                                    && expected
                                        .zip(current_input.iter().take(baseline_length))
                                        .all(|(previous, current)| previous == current))
                                .then(|| (baseline.response_id.clone(), baseline_length))
                            }
                            WebSocketBaselineInput::AppendOnly {
                                cursor: previous_cursor,
                                request_len,
                                response_items,
                            } => match input_identity {
                                RequestInputIdentity::AppendOnly {
                                    cursor,
                                    trailing_items,
                                } => {
                                    let baseline_length = request_len.checked_add(*response_items);
                                    let appended_items = previous_cursor
                                        .len()
                                        .checked_add(*response_items)
                                        .and_then(|minimum_len| {
                                            cursor.len().checked_sub(minimum_len)
                                        });
                                    let expected_len = baseline_length
                                        .and_then(|len| len.checked_add(appended_items?))
                                        .and_then(|len| len.checked_add(trailing_items));
                                    if cursor
                                        .includes_response_after(*previous_cursor, *response_items)
                                        && expected_len == Some(current_input.len())
                                    {
                                        baseline_length.map(|baseline_length| {
                                            (baseline.response_id.clone(), baseline_length)
                                        })
                                    } else {
                                        None
                                    }
                                }
                                RequestInputIdentity::Exact => None,
                            },
                        }
                    }
                } else {
                    None
                }
            }
        };

        let (previous_response_id, input) = match incremental {
            Some((response_id, baseline_length)) => {
                (Some(response_id), &current_input[baseline_length..])
            }
            None => (None, current_input),
        };
        let client_metadata = WebSocketClientMetadata {
            values: &request.client_metadata,
            turn_state: self.turn_state.clone(),
        };
        WebSocketRequest {
            request_type: "response.create",
            model: &request.model,
            instructions: request.instructions,
            previous_response_id,
            tools: request.tools,
            tool_choice: request.tool_choice,
            parallel_tool_calls: request.parallel_tool_calls,
            reasoning: request.reasoning,
            store: request.store,
            include: request.include,
            service_tier: request.service_tier,
            prompt_cache_key: &request.prompt_cache_key,
            text: request.text,
            generate: matches!(mode, WebSocketRequestMode::Warmup).then_some(false),
            client_metadata,
            input,
        }
    }

    pub(crate) async fn compact_append_only(
        &mut self,
        history: &[Value],
        cursor: HistoryCursor,
        compaction: CompactionRequest,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> ApiResult<CompactionResult> {
        self.compact_with_identity(
            history,
            compaction,
            RequestInputIdentity::AppendOnly {
                cursor,
                trailing_items: 1,
            },
            events,
        )
        .await
    }

    async fn compact_with_identity(
        &mut self,
        history: &[Value],
        compaction: CompactionRequest,
        mut input_identity: RequestInputIdentity,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> ApiResult<CompactionResult> {
        if history.is_empty() {
            return Err(ApiError::fatal("cannot compact an empty conversation"));
        }
        let trigger = compaction::compaction_trigger();
        let selection = self.model_selection.get();
        let [tool_prefix_tokens, instruction_tokens] = estimated_harness_tokens();
        let prefix_tokens = tool_prefix_tokens
            .saturating_add(instruction_tokens)
            .saturating_add(estimated_tokens(std::slice::from_ref(&trigger)));
        let mut prompt_history = history.to_vec();
        let effective_context_window = selection.effective_context_window();
        let rewritten_outputs = compaction::trim_tool_outputs_to_fit(
            &mut prompt_history,
            effective_context_window.saturating_sub(prefix_tokens),
        );
        if rewritten_outputs > 0 {
            input_identity = RequestInputIdentity::Exact;
        }
        // Retained context is at most 64k tokens. Build that small result now so ownership of the
        // full prompt can move directly into one retryable request instead of keeping two complete
        // history clones alive throughout compaction.
        let mut items = compaction::retained_compacted_history(&prompt_history);
        prompt_history.push(trigger);
        let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
        let request_kind = RequestKind::Compaction(compaction);
        let mut retries = 0_usize;
        let request = self.build_request(prompt_history, request_kind);
        let response = loop {
            match self
                .respond_request_with_events(
                    &request,
                    &completed_items,
                    events,
                    request_kind,
                    input_identity,
                )
                .await
            {
                Ok(response) => break response,
                Err(error) if error.is_retryable() => {
                    if retries >= MAX_REMOTE_COMPACTION_V2_STREAM_RETRIES {
                        if self.fall_back_to_http() {
                            retries = 0;
                            continue;
                        }
                        return Err(error);
                    }
                    let delay = error.retry_after().unwrap_or_else(|| retry_delay(retries));
                    retries = retries.saturating_add(1);
                    sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        };
        let compaction_output = match compaction::opaque_compaction_item(&response.items) {
            Ok(compaction_output) => compaction_output,
            Err(error) => {
                // A completed but unusable response must not become the baseline
                // for a later request using the unchanged conversation.
                self.abandon_response();
                return Err(ApiError::fatal(error));
            }
        };
        items.push(compaction_output);
        Ok(CompactionResult {
            items,
            usage: response.usage,
            rate_limits: response.rate_limits,
        })
    }

    fn build_request(&self, history: Vec<Value>, request_kind: RequestKind) -> ResponsesRequest {
        let selection = self.model_selection.get();
        self.build_request_from_input(history, request_kind, selection)
    }

    pub(crate) fn build_sampling_request(
        &self,
        history: Vec<Value>,
        cursor: HistoryCursor,
    ) -> SamplingRequest {
        let selection = self.model_selection.get();
        SamplingRequest {
            request: self.build_request_from_input(history, RequestKind::Turn, selection),
            cursor,
        }
    }

    fn build_request_from_input(
        &self,
        input: Vec<Value>,
        request_kind: RequestKind,
        selection: ModelSelection,
    ) -> ResponsesRequest {
        ResponsesRequest {
            model: selection.model,
            instructions: HarnessInstructions,
            tools: ResponsesApiTools,
            tool_choice: "auto",
            parallel_tool_calls: true,
            reasoning: RequestReasoning {
                effort: selection.reasoning_effort,
                context: "all_turns",
            },
            store: false,
            stream: true,
            include: ["reasoning.encrypted_content"],
            service_tier: self.effective_service_tier(),
            prompt_cache_key: self.session_id.clone(),
            text: RequestText { verbosity: "low" },
            client_metadata: self.client_metadata(request_kind),
            input,
        }
    }

    fn client_metadata(&self, request_kind: RequestKind) -> Map<String, Value> {
        let turn_metadata = self.turn_metadata(request_kind).to_string();
        Map::from_iter([
            (
                "x-codex-installation-id".to_string(),
                Value::String(self.installation_id.clone()),
            ),
            (
                "session_id".to_string(),
                Value::String(self.session_id.clone()),
            ),
            (
                "thread_id".to_string(),
                Value::String(self.thread_id.clone()),
            ),
            ("turn_id".to_string(), Value::String(self.turn_id.clone())),
            (
                "x-codex-window-id".to_string(),
                Value::String(self.window_id()),
            ),
            (
                "x-codex-turn-metadata".to_string(),
                Value::String(turn_metadata),
            ),
        ])
    }

    fn turn_metadata(&self, request_kind: RequestKind) -> Value {
        let mut metadata = json!({
            "installation_id": self.installation_id,
            "session_id": self.session_id,
            "thread_id": self.thread_id,
            "turn_id": self.turn_id,
            "window_id": self.window_id(),
            "request_kind": request_kind.as_str(),
            "sandbox": "danger-full-access",
            "turn_started_at_unix_ms": self.turn_started_at_unix_ms,
        });
        if let RequestKind::Compaction(compaction) = request_kind {
            metadata["compaction"] = json!({
                "trigger": compaction.trigger(),
                "reason": compaction.reason(),
                "implementation": "responses_compaction_v2",
                "phase": compaction.phase(),
                "strategy": "memento",
            });
        }
        metadata
    }

    fn window_id(&self) -> String {
        format!("{}:{}", self.thread_id, self.window)
    }

    fn effective_service_tier(&self) -> Option<&'static str> {
        self.service_tier.request_value()
    }

    fn request_headers(
        &self,
        accept: &str,
        request_kind: RequestKind,
        auth: &AuthSnapshot,
    ) -> ApiResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        insert_header(&mut headers, "accept", accept)?;
        headers.insert("authorization", auth.authorization.clone());
        if let Some(account_id) = &auth.account_id {
            headers.insert("chatgpt-account-id", account_id.clone());
        }
        insert_header(&mut headers, "session-id", &self.session_id)?;
        insert_header(&mut headers, "thread-id", &self.thread_id)?;
        insert_header(&mut headers, "x-client-request-id", &self.thread_id)?;
        insert_header(
            &mut headers,
            "x-codex-installation-id",
            &self.installation_id,
        )?;
        insert_header(&mut headers, "x-codex-window-id", &self.window_id())?;
        insert_header(
            &mut headers,
            "x-codex-turn-metadata",
            &self.turn_metadata(request_kind).to_string(),
        )?;
        let selection = self.model_selection.get();
        let routing_hint = match self.effective_service_tier() {
            Some(service_tier) => format!("model={};tier={service_tier}", selection.model),
            None => format!("model={}", selection.model),
        };
        insert_header(&mut headers, X_CODEX_ROUTING_HINT, &routing_hint)?;
        insert_header(
            &mut headers,
            "x-codex-beta-features",
            REMOTE_COMPACTION_V2_FEATURE,
        )?;
        if let Some(turn_state) = &self.turn_state {
            insert_header(&mut headers, X_CODEX_TURN_STATE, turn_state)?;
        }
        Ok(headers)
    }

    async fn post(
        &mut self,
        path: &str,
        body: &ResponsesRequest,
        accept: &str,
        request_kind: RequestKind,
    ) -> ApiResult<reqwest::Response> {
        let compressed_body = encode_request_body(body)?;
        let mut auth = self
            .auth
            .refreshed_snapshot(&self.client)
            .await
            .map_err(|error| ApiError::fatal(format!("{error:#}")))?;
        let url = format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut refreshed_after_unauthorized = false;
        let mut attempt = 0_usize;
        loop {
            let mut headers = self.request_headers(accept, request_kind, &auth)?;
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(CONTENT_ENCODING, HeaderValue::from_static("zstd"));
            let response = self
                .client
                .post(&url)
                .headers(headers)
                .body(compressed_body.clone())
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(_) if attempt < MAX_HTTP_RETRIES => {
                    sleep(retry_delay(attempt)).await;
                    attempt = attempt.saturating_add(1);
                    continue;
                }
                Err(error) => {
                    return Err(ApiError::retryable(format!(
                        "Responses request failed: {error}"
                    )));
                }
            };
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            if status == StatusCode::UNAUTHORIZED && !refreshed_after_unauthorized {
                auth = self
                    .auth
                    .refreshed_snapshot_after_unauthorized(&self.client, &auth)
                    .await
                    .map_err(|error| ApiError::fatal(format!("{error:#}")))?;
                refreshed_after_unauthorized = true;
                // Codex's unauthorized recovery wraps the transport retry loop,
                // so a refreshed request receives a fresh transport budget.
                attempt = 0;
                continue;
            }
            let retry_after = parse_retry_after(response.headers());
            let transport_retryable = status.is_server_error();
            if transport_retryable && attempt < MAX_HTTP_RETRIES {
                sleep(
                    retry_after
                        .unwrap_or_else(|| retry_delay(attempt))
                        .min(Duration::from_secs(30)),
                )
                .await;
                attempt = attempt.saturating_add(1);
                continue;
            }
            let body =
                bounded_error_body(response, MAX_ERROR_BODY_BYTES, MAX_ERROR_BODY_CHARS).await;
            let message = format!("Responses request failed with {status}: {body}");
            if status == StatusCode::TOO_MANY_REQUESTS || transport_retryable {
                return Err(ApiError::retryable_after(message, retry_after));
            }
            return Err(ApiError::fatal(message));
        }
    }

    fn capture_turn_state(&mut self, headers: &HeaderMap) {
        if self.turn_state.is_some() {
            return;
        }
        if let Some(value) = headers
            .get(X_CODEX_TURN_STATE)
            .and_then(|value| value.to_str().ok())
        {
            self.turn_state = Some(value.to_string());
        }
    }

    fn capture_event_turn_state(&mut self, event: &Value) {
        if self.turn_state.is_some()
            || event.get("type").and_then(Value::as_str) != Some("response.metadata")
        {
            return;
        }
        let value = event
            .get("headers")
            .and_then(Value::as_object)
            .and_then(|headers| json_header_value(headers, &[X_CODEX_TURN_STATE]));
        if let Some(value) = value {
            self.turn_state = Some(value.to_string());
        }
    }
}

fn encode_request_body<T: Serialize + ?Sized>(body: &T) -> ApiResult<Bytes> {
    // Keep serde and zstd connected: staging through `serde_json::to_vec` would grow and retain a
    // complete uncompressed request before `encode_all` copied it through `std::io::copy`.
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), REQUEST_COMPRESSION_LEVEL)
        .map_err(|error| {
            ApiError::fatal(format!(
                "failed to initialize Responses request compression: {error}"
            ))
        })?;
    serde_json::to_writer(&mut encoder, body).map_err(|error| {
        ApiError::fatal(format!(
            "failed to encode compressed Responses request: {error}"
        ))
    })?;
    encoder
        .finish()
        .map(Bytes::from)
        .map_err(|error| ApiError::fatal(format!("failed to compress Responses request: {error}")))
}

#[derive(Clone, Copy)]
enum RequestKind {
    Turn,
    Prewarm,
    Compaction(CompactionRequest),
}

impl RequestKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Prewarm => "prewarm",
            Self::Compaction(_) => "compaction",
        }
    }
}

pub(crate) fn harness_instructions() -> &'static str {
    SYSTEM_PROMPT.trim()
}

fn responses_api_specifications_json() -> &'static RawValue {
    // Match Codex's typed request path: serialize the fixed tool catalogue once, then transfer its
    // raw JSON directly into every HTTP and WebSocket request.
    RESPONSES_API_SPECIFICATIONS_JSON.get_or_init(|| {
        serde_json::value::to_raw_value(tools::responses_api_specifications()).unwrap_or_else(
            |error| panic!("failed to encode Responses tool specifications: {error}"),
        )
    })
}

pub(crate) fn estimated_harness_tokens() -> [u64; 2] {
    *STABLE_HARNESS_TOKEN_ESTIMATES.get_or_init(|| {
        [
            estimated_tokens(tools::responses_api_specifications()),
            estimated_tokens(std::slice::from_ref(&Value::String(
                harness_instructions().to_string(),
            ))),
        ]
    })
}

async fn collect_http_stream(
    mut response: reqwest::Response,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
    idle_timeout: Duration,
    validation: ResponseValidation<'_>,
    output_item_mode: OutputItemMode,
) -> ApiResult<ModelResponse> {
    let mut decoder = SseDecoder::default();
    let mut collected = CollectedResponse::new(output_item_mode);
    let mut decoded = Vec::new();
    let mut event_deadline = tokio::time::Instant::now() + idle_timeout;
    loop {
        let chunk = tokio::time::timeout_at(event_deadline, response.chunk())
            .await
            .map_err(|_| ApiError::stream_idle("model response was inactive for too long"))?
            .map_err(|error| {
                ApiError::retryable(format!("failed to read model response: {error}"))
            })?;
        let Some(chunk) = chunk else {
            decoder.finish(&mut decoded)?;
            let received_at = Instant::now();
            for data in decoded.drain(..) {
                process_event_at(
                    &data,
                    &mut collected,
                    completed_items,
                    events,
                    validation.expected_model,
                    &mut *validation.server_model_warning_emitted,
                    received_at,
                )?;
            }
            break;
        };
        let received_at = Instant::now();
        decoder.push(&chunk, &mut decoded)?;
        if !decoded.is_empty() {
            event_deadline = tokio::time::Instant::now() + idle_timeout;
        }
        for data in decoded.drain(..) {
            process_event_at(
                &data,
                &mut collected,
                completed_items,
                events,
                validation.expected_model,
                &mut *validation.server_model_warning_emitted,
                received_at,
            )?;
        }
        if collected.completed {
            break;
        }
    }
    if !collected.completed {
        return Err(ApiError::retryable(
            "model stream closed before response.completed",
        ));
    }
    collected.finish()
}

struct CollectedResponse {
    output_items: CollectedOutputItems,
    pending_items: BTreeMap<usize, Value>,
    item_summary: ResponseItemSummary,
    usage: Option<TokenUsage>,
    rate_limits: Vec<crate::rate_limits::RateLimitSnapshot>,
    end_turn: Option<bool>,
    response_id: Option<String>,
    completed: bool,
}

enum CollectedOutputItems {
    RetainedAndEmitted(Vec<Value>),
    Retained(Vec<Value>),
    Transferred { count: usize },
}

#[derive(Default)]
struct ResponseItemSummary {
    tool_calls: Vec<ToolCall>,
    final_answer: Option<String>,
    has_assistant_text: bool,
}

impl CollectedResponse {
    fn new(output_item_mode: OutputItemMode) -> Self {
        let output_items = match output_item_mode {
            OutputItemMode::RetainAndEmit => CollectedOutputItems::RetainedAndEmitted(Vec::new()),
            OutputItemMode::Retain => CollectedOutputItems::Retained(Vec::new()),
            OutputItemMode::Transfer => CollectedOutputItems::Transferred { count: 0 },
        };
        Self {
            output_items,
            pending_items: BTreeMap::new(),
            item_summary: ResponseItemSummary::default(),
            usage: None,
            rate_limits: Vec::new(),
            end_turn: None,
            response_id: None,
            completed: false,
        }
    }

    fn item_count(&self) -> usize {
        match &self.output_items {
            CollectedOutputItems::RetainedAndEmitted(items)
            | CollectedOutputItems::Retained(items) => items.len(),
            CollectedOutputItems::Transferred { count } => *count,
        }
    }

    fn upsert_rate_limit(&mut self, snapshot: crate::rate_limits::RateLimitSnapshot) {
        upsert_rate_limit(&mut self.rate_limits, snapshot);
    }

    fn push_item(
        &mut self,
        index: usize,
        item: Value,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> ApiResult<()> {
        validate_assistant_message_phase(&item)?;
        let item_count = self.item_count();
        if index < item_count {
            let conflicts = match &self.output_items {
                CollectedOutputItems::RetainedAndEmitted(items)
                | CollectedOutputItems::Retained(items) => items[index] != item,
                CollectedOutputItems::Transferred { .. } => false,
            };
            if conflicts {
                return Err(ApiError::fatal(format!(
                    "model sent conflicting output items at index {index}"
                )));
            }
            // Match Codex by treating output_item.done as authoritative. A transferred value is
            // already in history, so the duplicate copy carried by response.completed is ignored.
            return Ok(());
        }
        if index == item_count {
            self.accept_item(item, completed_items, events);
            while let Some(item) = self.pending_items.remove(&self.item_count()) {
                self.accept_item(item, completed_items, events);
            }
            return Ok(());
        }
        match self.pending_items.entry(index) {
            Entry::Vacant(entry) => {
                entry.insert(item);
            }
            Entry::Occupied(entry) if entry.get() == &item => {}
            Entry::Occupied(_) => {
                return Err(ApiError::fatal(format!(
                    "model sent conflicting pending output items at index {index}"
                )));
            }
        }
        Ok(())
    }

    fn accept_item(
        &mut self,
        item: Value,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) {
        let completed_message = self.item_summary.observe(&item, events.is_some());
        let completed_web_search = WebSearchCall::from_response_item(&item);
        match &mut self.output_items {
            CollectedOutputItems::RetainedAndEmitted(items) => {
                let _ = completed_items.send(item.clone());
                items.push(item);
            }
            CollectedOutputItems::Retained(items) => items.push(item),
            CollectedOutputItems::Transferred { count } => {
                *count = count.saturating_add(1);
                let _ = completed_items.send(item);
            }
        }
        if let Some(events) = events {
            if let Some(message) = completed_message {
                let _ = events.send(AgentEvent::ModelMessageCompleted(message));
            }
            if let Some(search) = completed_web_search {
                let _ = events.send(AgentEvent::WebSearchCompleted(search));
            }
        }
    }

    fn finish(self) -> ApiResult<ModelResponse> {
        if !self.pending_items.is_empty() {
            return Err(ApiError::fatal(
                "model response completed with a gap in output item indexes",
            ));
        }
        let output_item_count = self.item_count();
        let response_id = self
            .response_id
            .ok_or_else(|| ApiError::fatal("response.completed omitted the response ID"))?;
        let items = match self.output_items {
            CollectedOutputItems::RetainedAndEmitted(items)
            | CollectedOutputItems::Retained(items) => items,
            CollectedOutputItems::Transferred { .. } => Vec::new(),
        };
        Ok(ModelResponse {
            items,
            tool_calls: self.item_summary.tool_calls,
            final_answer: self.item_summary.final_answer,
            end_turn: self.end_turn,
            usage: self.usage,
            rate_limits: self.rate_limits,
            server_reasoning_included: false,
            response_id,
            output_item_count,
            has_assistant_text: self.item_summary.has_assistant_text,
        })
    }
}

fn upsert_rate_limit(
    snapshots: &mut Vec<crate::rate_limits::RateLimitSnapshot>,
    snapshot: crate::rate_limits::RateLimitSnapshot,
) {
    if let Some(existing) = snapshots
        .iter_mut()
        .find(|existing| existing.limit_id == snapshot.limit_id)
    {
        *existing = snapshot;
    } else {
        snapshots.push(snapshot);
    }
}

impl ResponseItemSummary {
    fn observe(&mut self, item: &Value, emit_completed_message: bool) -> Option<AssistantMessage> {
        if let Some(tool_call) = ToolCall::from_response_item(item) {
            self.tool_calls.push(tool_call);
        }
        let message = AssistantMessage::from_response_item(item)?;
        let has_text = !message.text.trim().is_empty();
        let is_terminal = message.is_terminal();
        self.has_assistant_text |= has_text;
        if emit_completed_message {
            if is_terminal && has_text {
                self.final_answer = Some(message.text_with_citation_sources());
            }
            Some(message)
        } else if is_terminal && has_text {
            self.final_answer = Some(message.text_with_citation_sources());
            None
        } else {
            None
        }
    }
}

#[cfg(test)]
fn process_event(
    data: &str,
    collected: &mut CollectedResponse,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
    expected_model: &str,
    server_model_warning_emitted: &mut bool,
) -> ApiResult<()> {
    process_event_at(
        data,
        collected,
        completed_items,
        events,
        expected_model,
        server_model_warning_emitted,
        Instant::now(),
    )
}

fn process_event_at(
    data: &str,
    collected: &mut CollectedResponse,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
    expected_model: &str,
    server_model_warning_emitted: &mut bool,
    received_at: Instant,
) -> ApiResult<()> {
    if data == "[DONE]" {
        return Ok(());
    }
    let event: Value = serde_json::from_str(data)
        .map_err(|error| ApiError::fatal(format!("failed to decode SSE event: {error}")))?;
    process_event_value_at(
        event,
        collected,
        completed_items,
        events,
        expected_model,
        server_model_warning_emitted,
        received_at,
    )
}

fn process_event_value(
    event: Value,
    collected: &mut CollectedResponse,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
    expected_model: &str,
    server_model_warning_emitted: &mut bool,
) -> ApiResult<()> {
    process_event_value_at(
        event,
        collected,
        completed_items,
        events,
        expected_model,
        server_model_warning_emitted,
        Instant::now(),
    )
}

fn process_event_value_at(
    mut event: Value,
    collected: &mut CollectedResponse,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
    expected_model: &str,
    server_model_warning_emitted: &mut bool,
    received_at: Instant,
) -> ApiResult<()> {
    if let Some(snapshot) = crate::rate_limits::parse_rate_limit_event(&event) {
        collected.upsert_rate_limit(snapshot);
        return Ok(());
    }
    observe_server_model(
        event_server_model(&event),
        expected_model,
        events,
        server_model_warning_emitted,
    );
    // Completed items can contain multi-megabyte encrypted reasoning. Move the item out of the
    // event so normal sampling can transfer that allocation directly into conversation history.
    if event.get("type").and_then(Value::as_str) == Some("response.output_item.done") {
        let index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .unwrap_or_else(|| collected.item_count() + collected.pending_items.len());
        let item = event
            .get_mut("item")
            .map(Value::take)
            .ok_or_else(|| ApiError::fatal("output_item.done omitted its item"))?;
        collected.push_item(index, item, completed_items, events)?;
        return Ok(());
    }
    match event.get("type").and_then(Value::as_str) {
        Some("response.output_item.added") => {
            if let Some(item) = event.get("item") {
                validate_assistant_message_phase(item)?;
            }
            if let Some(events) = events {
                if let Some(message) = event
                    .get("item")
                    .and_then(AssistantMessage::from_response_item)
                {
                    let _ = events.send(AgentEvent::ModelMessageStarted(message));
                }
                if let Some(search) = event
                    .get("item")
                    .and_then(WebSearchCall::from_response_item)
                {
                    let _ = events.send(AgentEvent::WebSearchStarted(search));
                }
            }
        }
        Some("response.output_text.delta") => {
            if let Some(Value::String(delta)) = event.get_mut("delta")
                && let Some(events) = events
            {
                let _ = events.send(AgentEvent::ModelMessageDelta(ModelTextDelta::new(
                    std::mem::take(delta),
                    received_at,
                )));
            }
        }
        Some("response.completed") => {
            let response = event
                .get_mut("response")
                .map(Value::take)
                .ok_or_else(|| ApiError::fatal("response.completed omitted its response"))?;
            validate_completed_response(&response)?;
            let mut response = response;
            if let Some(output) = response.get_mut("output").and_then(Value::as_array_mut) {
                for (index, item) in std::mem::take(output).into_iter().enumerate() {
                    collected.push_item(index, item, completed_items, events)?;
                }
            }
            collected.usage = response.get("usage").and_then(parse_usage);
            collected.end_turn = response.get("end_turn").and_then(Value::as_bool);
            collected.response_id = response
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string);
            collected.completed = true;
        }
        Some("response.created") if collected.response_id.is_none() => {
            collected.response_id = event
                .pointer("/response/id")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        Some("response.failed") => {
            let message = event
                .pointer("/response/error/message")
                .and_then(Value::as_str)
                .unwrap_or("model response failed");
            let code = event
                .pointer("/response/error/code")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Err(classify_stream_error(code, message));
        }
        Some("response.incomplete") => {
            let reason = event
                .pointer("/response/incomplete_details/reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Err(ApiError::retryable(format!(
                "model response was incomplete: {reason}"
            )));
        }
        Some("error") => return Err(error_event(&event)),
        _ => {}
    }
    Ok(())
}

fn validate_assistant_message_phase(item: &Value) -> ApiResult<()> {
    if item.get("type").and_then(Value::as_str) != Some("message")
        || item.get("role").and_then(Value::as_str) != Some("assistant")
    {
        return Ok(());
    }
    match item.get("phase") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(phase)) if matches!(phase.as_str(), "commentary" | "final_answer") => {
            Ok(())
        }
        _ => Err(ApiError::fatal(
            "model sent an assistant message with an unsupported phase",
        )),
    }
}

fn error_event(event: &Value) -> ApiError {
    let code = event
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = event
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("Responses WebSocket returned an error");
    match code {
        "previous_response_not_found" => {
            ApiError::new(ApiErrorKind::PreviousResponseNotFound, message)
        }
        "websocket_connection_limit_reached" => ApiError::retryable(message),
        _ => {
            let status = event
                .get("status")
                .or_else(|| event.get("status_code"))
                .and_then(Value::as_u64);
            if status == Some(401) {
                ApiError::unauthorized(message)
            } else if status == Some(429) || status.is_some_and(|status| status >= 500) {
                ApiError::retryable(message)
            } else {
                ApiError::fatal(message)
            }
        }
    }
}

fn classify_stream_error(code: &str, message: &str) -> ApiError {
    match code {
        "previous_response_not_found" => {
            ApiError::new(ApiErrorKind::PreviousResponseNotFound, message)
        }
        "context_length_exceeded"
        | "insufficient_quota"
        | "usage_not_included"
        | "cyber_policy"
        | "invalid_prompt"
        | "bio_policy" => ApiError::fatal(message),
        // Codex treats other response.failed errors as retryable, including
        // server_is_overloaded, slow_down, and future transient codes.
        _ => ApiError::retryable_after(
            message,
            (code == "rate_limit_exceeded")
                .then(|| parse_rate_limit_delay(message))
                .flatten(),
        ),
    }
}

fn validate_completed_response(response: &Value) -> ApiResult<()> {
    if let Some(context) = response
        .pointer("/reasoning/context")
        .and_then(Value::as_str)
        && context != "all_turns"
    {
        return Err(ApiError::fatal(format!(
            "backend used reasoning.context `{context}`; bettercodex requires `all_turns`"
        )));
    }
    Ok(())
}

fn event_server_model(event: &Value) -> Option<&str> {
    let response_headers = event
        .pointer("/response/headers")
        .and_then(Value::as_object);
    let event_headers = event.get("headers").and_then(Value::as_object);
    response_headers
        .and_then(|headers| json_header_value(headers, &["openai-model", "x-openai-model"]))
        .or_else(|| {
            event_headers
                .and_then(|headers| json_header_value(headers, &["openai-model", "x-openai-model"]))
        })
}

fn json_header_value<'a>(
    headers: &'a Map<String, Value>,
    accepted_names: &[&str],
) -> Option<&'a str> {
    headers.iter().find_map(|(name, value)| {
        accepted_names
            .iter()
            .any(|accepted| name.eq_ignore_ascii_case(accepted))
            .then(|| match value {
                Value::String(value) => Some(value.as_str()),
                Value::Array(values) => values.first().and_then(Value::as_str),
                _ => None,
            })
            .flatten()
    })
}

fn observe_server_model(
    server_model: Option<&str>,
    requested_model: &str,
    events: Option<&UnboundedSender<AgentEvent>>,
    warning_emitted: &mut bool,
) {
    let Some(server_model) = server_model else {
        return;
    };
    if server_model.eq_ignore_ascii_case(requested_model) {
        tracing::debug!(%server_model, "server model matches the requested model");
        return;
    }
    tracing::warn!(
        %requested_model,
        %server_model,
        "server reported a different model than requested"
    );
    if *warning_emitted {
        return;
    }
    let Some(events) = events else {
        return;
    };
    *warning_emitted = true;
    let _ = events.send(AgentEvent::Warning(format!(
        "OpenAI routed this request from {requested_model} to {server_model}. This can occur when potentially high-risk cyber activity triggers a safety fallback; the `/model` selection remains {requested_model}."
    )));
}

fn parse_usage(usage: &Value) -> Option<TokenUsage> {
    Some(TokenUsage {
        input_tokens: usage.get("input_tokens")?.as_u64()?,
        cached_input_tokens: usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_input_tokens: usage
            .pointer("/input_tokens_details/cache_write_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        reasoning_output_tokens: usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .saturating_add(
                        usage
                            .get("output_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
            }),
    })
}

fn websocket_url(base_url: &str, path: &str) -> ApiResult<String> {
    let base = base_url.trim_end_matches('/');
    let url = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}/{}", path.trim_start_matches('/'))
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}/{}", path.trim_start_matches('/'))
    } else if base.starts_with("ws://") || base.starts_with("wss://") {
        format!("{base}/{}", path.trim_start_matches('/'))
    } else {
        return Err(ApiError::fatal(format!(
            "unsupported Responses base URL `{base_url}`"
        )));
    };
    Ok(url)
}

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) -> ApiResult<()> {
    headers.insert(
        name,
        HeaderValue::from_str(value)
            .map_err(|error| ApiError::fatal(format!("invalid {name} header: {error}")))?,
    );
    Ok(())
}

pub(crate) fn retry_delay(attempt: usize) -> Duration {
    backoff(
        RETRY_BASE_DELAY,
        u64::try_from(attempt.saturating_add(1)).unwrap_or(u64::MAX),
    )
}

fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn parse_rate_limit_delay(message: &str) -> Option<Duration> {
    const MARKER: &str = "try again in";
    let lowercase = message.to_ascii_lowercase();
    let tail = lowercase
        .get(lowercase.find(MARKER)?.saturating_add(MARKER.len())..)?
        .trim_start();
    let number_end = tail
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit() || *character == '.')
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    let value = tail.get(..number_end)?.parse::<f64>().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let unit = tail.get(number_end..)?.trim_start();
    if unit.starts_with("ms") {
        Duration::try_from_secs_f64(value / 1_000.0).ok()
    } else if unit.starts_with('s') || unit.starts_with("second") {
        Duration::try_from_secs_f64(value).ok()
    } else {
        None
    }
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
