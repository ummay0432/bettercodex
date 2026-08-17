use crate::assistant_message::AssistantMessage;
use crate::auth::Auth;
use crate::auth::AuthSnapshot;
use crate::auth::SharedAuth;
use crate::compaction;
use crate::compaction::CompactionRequest;
use crate::context::HistoryCursor;
use crate::context::ResponseItemForRequest;
use crate::context::estimated_tokens;
use crate::context::response_item_id_is_prefixed;
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
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use serde::Serialize;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use serde_json::value::RawValue;
use sha2::Digest as _;
use sha2::Sha256;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
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
const MAX_WEBSOCKET_RESPONSE_PRELUDE_EVENTS: usize = 64;
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
    completed_response: Option<Box<CompletedResponseMetadata>>,
}

#[derive(Debug, Default)]
pub(crate) struct CompletedResponseMetadata {
    usage: Option<TokenUsage>,
    rate_limits: Vec<crate::rate_limits::RateLimitSnapshot>,
}

impl CompletedResponseMetadata {
    fn merge(
        &mut self,
        usage: Option<TokenUsage>,
        rate_limits: impl IntoIterator<Item = crate::rate_limits::RateLimitSnapshot>,
    ) {
        if let Some(usage) = usage {
            if let Some(total) = &mut self.usage {
                total.add_assign(&usage);
            } else {
                self.usage = Some(usage);
            }
        }
        for snapshot in rate_limits {
            upsert_rate_limit(&mut self.rate_limits, snapshot);
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Option<TokenUsage>,
        Vec<crate::rate_limits::RateLimitSnapshot>,
    ) {
        (self.usage, self.rate_limits)
    }

    fn attach_to(self, error: ApiError) -> ApiError {
        if self.usage.is_none() && self.rate_limits.is_empty() {
            error
        } else {
            error.with_completed_response(self.usage, self.rate_limits)
        }
    }
}

impl ApiError {
    fn new(kind: ApiErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retry_after: None,
            completed_response: None,
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
            completed_response: None,
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

    fn without_transparent_recovery(mut self) -> Self {
        if matches!(
            self.kind,
            ApiErrorKind::StreamIdle
                | ApiErrorKind::Unauthorized
                | ApiErrorKind::PreviousResponseNotFound
                | ApiErrorKind::WebSocketUnavailable
        ) {
            self.kind = ApiErrorKind::Retryable;
        }
        self
    }

    pub(crate) fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    fn with_completed_response(
        mut self,
        usage: Option<TokenUsage>,
        rate_limits: Vec<crate::rate_limits::RateLimitSnapshot>,
    ) -> Self {
        self.completed_response = Some(Box::new(CompletedResponseMetadata { usage, rate_limits }));
        self
    }

    pub(crate) fn take_completed_response(
        &mut self,
    ) -> Option<(
        Option<TokenUsage>,
        Vec<crate::rate_limits::RateLimitSnapshot>,
    )> {
        self.completed_response
            .take()
            .map(|metadata| metadata.into_parts())
    }

    fn has_completed_usage(&self) -> bool {
        self.completed_response
            .as_ref()
            .is_some_and(|metadata| metadata.usage.is_some())
    }

    fn take_response_rate_limits(&mut self) -> Vec<crate::rate_limits::RateLimitSnapshot> {
        let Some(mut metadata) = self.completed_response.take() else {
            return Vec::new();
        };
        let rate_limits = std::mem::take(&mut metadata.rate_limits);
        if metadata.usage.is_some() {
            self.completed_response = Some(metadata);
        }
        rate_limits
    }

    fn add_response_rate_limit_fallbacks(
        &mut self,
        rate_limits: impl IntoIterator<Item = crate::rate_limits::RateLimitSnapshot>,
    ) {
        let mut rate_limits = rate_limits.into_iter().peekable();
        if rate_limits.peek().is_none() {
            return;
        }
        let metadata = self.completed_response.get_or_insert_with(|| {
            Box::new(CompletedResponseMetadata {
                usage: None,
                rate_limits: Vec::new(),
            })
        });
        for snapshot in rate_limits {
            if let Some(existing) = metadata
                .rate_limits
                .iter_mut()
                .find(|existing| existing.limit_id == snapshot.limit_id)
            {
                crate::rate_limits::fill_missing_rate_limit_fields(existing, &snapshot);
            } else {
                metadata.rate_limits.push(snapshot);
            }
        }
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
    websocket_tls_config: Option<Arc<rustls::ClientConfig>>,
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
    // Compaction clears the incremental baseline while the same socket still needs a stale-frame
    // boundary for its last generation.
    websocket_last_response_id: Option<String>,
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

fn serialize_response_items<S>(
    items: &[Value],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut serialized = serializer.serialize_seq(Some(items.len()))?;
    for item in items {
        serialized.serialize_element(&ResponseItemForRequest::new(item))?;
    }
    serialized.end()
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
    #[serde(serialize_with = "serialize_response_items")]
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
    #[serde(serialize_with = "serialize_response_items")]
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
    /// Keep output only in the response (connection warmup).
    Retain,
    /// Count every output but retain only compaction candidates.
    Compaction,
    /// Move output directly to conversation history and retain only response metadata.
    Transfer,
}

struct ResponseValidation<'a> {
    expected_model: &'a str,
    server_model_warning_emitted: &'a mut bool,
}

struct WebSocketResponseBoundary {
    previous_response_id: Option<String>,
    prelude_events: usize,
    started: bool,
}

impl WebSocketResponseBoundary {
    fn new(previous_response_id: Option<String>) -> Self {
        Self {
            previous_response_id,
            prelude_events: 0,
            started: false,
        }
    }

    fn accepts(&mut self, event: &Value) -> ApiResult<bool> {
        let kind = event.get("type").and_then(Value::as_str);
        let repeats_previous_response =
            self.previous_response_id
                .as_deref()
                .is_some_and(|previous_response_id| {
                    (matches!(
                        kind,
                        Some(
                            "response.created"
                                | "response.completed"
                                | "response.failed"
                                | "response.incomplete"
                        )
                    ) && event.pointer("/response/id").and_then(Value::as_str)
                        == Some(previous_response_id))
                        || (matches!(kind, Some("response.metadata" | "codex.response.metadata"))
                            && event.get("response_id").and_then(Value::as_str)
                                == Some(previous_response_id))
                });
        if repeats_previous_response {
            return Ok(false);
        }
        if self.started {
            return Ok(true);
        }
        self.prelude_events = self.prelude_events.saturating_add(1);
        if self.prelude_events > MAX_WEBSOCKET_RESPONSE_PRELUDE_EVENTS {
            return Err(ApiError::retryable(
                "Responses WebSocket sent too many events before response.created",
            ));
        }
        match kind {
            Some("response.created") => {
                event
                    .pointer("/response/id")
                    .and_then(Value::as_str)
                    .filter(|response_id| !response_id.is_empty())
                    .ok_or_else(|| ApiError::fatal("response.created omitted the response ID"))?;
                self.started = true;
                Ok(true)
            }
            Some("response.completed") => {
                let response = event
                    .get("response")
                    .ok_or_else(|| ApiError::fatal("response.completed omitted its response"))?;
                response
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|response_id| !response_id.is_empty())
                    .ok_or_else(|| ApiError::fatal("response.completed omitted the response ID"))?;
                let usage = parse_response_usage(response)?;
                Err(ApiError::fatal(
                    "Responses WebSocket completed a response before response.created",
                )
                .with_completed_response(usage, Vec::new()))
            }
            Some("response.failed" | "response.incomplete") => Ok(true),
            Some("response.metadata" | "codex.response.metadata") => Ok(event
                .get("response_id")
                .and_then(Value::as_str)
                .is_none_or(|response_id| {
                    self.previous_response_id.as_deref() != Some(response_id)
                })),
            Some("error" | "codex.rate_limits") => Ok(true),
            Some(kind) if kind.starts_with("response.") => Ok(false),
            _ => Ok(false),
        }
    }
}

impl OutputItemMode {
    fn for_http(request_kind: RequestKind) -> Self {
        match request_kind {
            RequestKind::Turn => Self::Transfer,
            RequestKind::Prewarm => Self::Retain,
            RequestKind::Compaction(_) => Self::Compaction,
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
            (RequestKind::Prewarm, _) => Self::Retain,
            (RequestKind::Compaction(_), _) => Self::Compaction,
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
        response: &mut ModelResponse,
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
                    output: std::mem::take(&mut response.items),
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
    // Compaction retains its candidate here. Normal sampling moves output into conversation history;
    // exact WebSocket responses move their retained copy into the connection baseline.
    pub(crate) items: Vec<Value>,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) final_answer: Option<String>,
    pub(crate) end_turn: Option<bool>,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) rate_limits: Vec<crate::rate_limits::RateLimitSnapshot>,
    pub(crate) server_reasoning_included: bool,
    response_id: String,
    output_item_count: usize,
    compaction_item_count: usize,
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
        let websocket_tls_config = crate::http_client::build_websocket_tls_config()
            .context("failed to configure Responses WebSocket TLS")?;
        Ok(Self {
            client,
            websocket_tls_config,
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
            websocket_last_response_id: None,
            server_model_warning_emitted: false,
            stream_idle_timeout: STREAM_IDLE_TIMEOUT,
        })
    }

    pub(crate) fn startup_prewarm_client(&self) -> Self {
        Self {
            client: self.client.clone(),
            websocket_tls_config: self.websocket_tls_config.clone(),
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
            websocket_last_response_id: None,
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
        if prewarmed.websocket.is_some() {
            let current_request = self.build_request(Vec::new(), RequestKind::Prewarm);
            let matches_current_request = prewarmed
                .websocket_baseline
                .as_ref()
                .is_some_and(|baseline| baseline.properties.matches(&current_request));
            if !matches_current_request {
                return;
            }
        }
        self.startup_websocket_pending_first_event = prewarmed.websocket.is_some();
        self.prefer_websocket = prewarmed.prefer_websocket;
        self.websocket_prewarm_attempted = prewarmed.websocket_prewarm_attempted;
        self.websocket = prewarmed.websocket.take();
        self.websocket_reasoning_included = prewarmed.websocket_reasoning_included;
        self.websocket_server_model = prewarmed.websocket_server_model.take();
        self.websocket_rate_limits = std::mem::take(&mut prewarmed.websocket_rate_limits);
        self.websocket_baseline = prewarmed.websocket_baseline.take();
        self.websocket_last_response_id = prewarmed.websocket_last_response_id.take();
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
        // from the compaction request is a valid baseline for the next sample. Keep the socket's
        // last response ID so delayed compaction frames cannot enter that full request's response.
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
        self.websocket_last_response_id = None;
    }

    fn retain_error_rate_limits(&mut self, error: &mut ApiError) {
        for snapshot in error.take_response_rate_limits() {
            upsert_rate_limit(&mut self.websocket_rate_limits, snapshot);
        }
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
            WebSocketPrewarmOutcome::Failed(mut error) => {
                self.retain_error_rate_limits(&mut error);
                false
            }
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
                    Ok(mut response) => {
                        // Compaction consumes the previous turn baseline but immediately replaces
                        // conversation lineage. Retaining its full request here would deep-clone a
                        // near-window history only to discard it after output validation.
                        if matches!(request_kind, RequestKind::Turn) {
                            self.websocket_baseline = Some(WebSocketBaseline::new(
                                request,
                                &mut response,
                                input_identity,
                            )?);
                        }
                        return Ok(response);
                    }
                    Err(mut error)
                        if error.kind == ApiErrorKind::Unauthorized
                            && !refreshed_websocket_auth =>
                    {
                        let refresh = self.force_refresh_auth().await;
                        self.abandon_response();
                        self.retain_error_rate_limits(&mut error);
                        if let Err(mut refresh_error) = refresh {
                            refresh_error.add_response_rate_limit_fallbacks(std::mem::take(
                                &mut self.websocket_rate_limits,
                            ));
                            return Err(refresh_error);
                        }
                        refreshed_websocket_auth = true;
                        continue;
                    }
                    Err(mut error)
                        if error.kind == ApiErrorKind::PreviousResponseNotFound
                            && !error.has_completed_usage()
                            && !retried_full_websocket_request =>
                    {
                        self.retain_error_rate_limits(&mut error);
                        self.websocket_baseline = None;
                        retried_full_websocket_request = true;
                        continue;
                    }
                    Err(mut error) if error.kind == ApiErrorKind::WebSocketUnavailable => {
                        self.fall_back_to_http();
                        self.retain_error_rate_limits(&mut error);
                    }
                    Err(mut error) if error.is_stream_idle() => {
                        if !self.recover_websocket_inactivity(events) {
                            return Err(error);
                        }
                        self.retain_error_rate_limits(&mut error);
                    }
                    Err(mut error) => {
                        let rate_limits = std::mem::take(&mut self.websocket_rate_limits);
                        self.abandon_response();
                        error.add_response_rate_limit_fallbacks(rate_limits);
                        return Err(error);
                    }
                }
            }

            return self
                .respond_http(request, completed_items, events, request_kind)
                .await;
        }
    }

    async fn force_refresh_auth(&self) -> ApiResult<()> {
        self.auth
            .force_refreshed_snapshot(&self.client)
            .await
            .map(|_| ())
            .map_err(|error| ApiError::fatal(format!("{error:#}")))
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
            Err(mut error) if error.kind == ApiErrorKind::Unauthorized => {
                let refresh = self.force_refresh_auth().await;
                self.abandon_response();
                self.retain_error_rate_limits(&mut error);
                if let Err(mut refresh_error) = refresh {
                    refresh_error.add_response_rate_limit_fallbacks(std::mem::take(
                        &mut self.websocket_rate_limits,
                    ));
                    return Err(refresh_error);
                }
                Ok(WebSocketPrewarmOutcome::Ready {
                    refreshed_auth: true,
                })
            }
            Err(mut error) if error.kind == ApiErrorKind::WebSocketUnavailable => {
                self.fall_back_to_http();
                self.retain_error_rate_limits(&mut error);
                Ok(WebSocketPrewarmOutcome::Ready {
                    refreshed_auth: false,
                })
            }
            Err(mut error) if error.is_stream_idle() => {
                if !self.recover_websocket_inactivity(events) {
                    return Err(error);
                }
                self.retain_error_rate_limits(&mut error);
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
        // A WebSocket attempt can fail with useful account headers before HTTP fallback. Keep that
        // older snapshot until this request returns a response or an error to the conversation.
        let mut rate_limits = self.websocket_rate_limits.clone();
        let (response, retry_rate_limits) = match self
            .post("responses", request, "text/event-stream", request_kind)
            .await
        {
            Ok(response) => response,
            Err(mut error) => {
                error.add_response_rate_limit_fallbacks(rate_limits);
                self.websocket_rate_limits.clear();
                return Err(error);
            }
        };
        for snapshot in retry_rate_limits {
            upsert_rate_limit(&mut rate_limits, snapshot);
        }
        for snapshot in crate::rate_limits::parse_all_rate_limits(response.headers()) {
            upsert_rate_limit(&mut rate_limits, snapshot);
        }
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
        let mut response = match collect_http_stream(
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
        .await
        {
            Ok(response) => response,
            Err(mut error) => {
                error.add_response_rate_limit_fallbacks(rate_limits);
                self.websocket_rate_limits.clear();
                return Err(error);
            }
        };
        response.server_reasoning_included = server_reasoning_included;
        let streamed_rate_limits = std::mem::take(&mut response.rate_limits);
        response.rate_limits = rate_limits;
        for snapshot in streamed_rate_limits {
            upsert_rate_limit(&mut response.rate_limits, snapshot);
        }
        self.websocket_rate_limits.clear();
        Ok(response)
    }

    async fn prewarm_websocket(&mut self) -> ApiResult<()> {
        let request = self.build_request(Vec::new(), RequestKind::Prewarm);
        let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut response = self
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
            &mut response,
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
        let previous_response_id = self.websocket_last_response_id.clone();
        let request_history_identities = RequestHistoryIdentities::new(&logical_request.input);
        let request = self.prepare_websocket_request(logical_request, mode, input_identity);
        self.websocket
            .as_mut()
            .ok_or_else(|| ApiError::websocket_unavailable("Responses WebSocket is unavailable"))?
            .send(&request, initial_idle_timeout)
            .await?;

        let mut response_boundary = WebSocketResponseBoundary::new(previous_response_id);
        let mut collected =
            CollectedResponse::new(OutputItemMode::for_websocket(request_kind, input_identity));
        // A socket completed during startup can become half-open before the operator submits the
        // first turn. Bound only its first real send/event by the startup timeout; after any server
        // event proves the connection active, retain the ordinary timeout for long inference.
        let mut awaiting_startup_first_event = startup_websocket_pending_first_event;
        let mut next_event_idle_timeout = initial_idle_timeout;
        let response_start_deadline = tokio::time::Instant::now() + initial_idle_timeout;
        loop {
            let read_timeout = if response_boundary.started {
                next_event_idle_timeout
            } else {
                response_start_deadline.saturating_duration_since(tokio::time::Instant::now())
            };
            let next_text = self
                .websocket
                .as_mut()
                .ok_or_else(|| {
                    ApiError::websocket_unavailable("Responses WebSocket is unavailable")
                })?
                .next_text(read_timeout)
                .await;
            let text = match next_text {
                // Replaying the original request over HTTPS is safe only before model activity is
                // exposed. After that, return the interruption to the agent instead of merging two
                // generations into one UI or history stream.
                Err(mut error) => {
                    if collected.has_model_activity() {
                        error = error.without_transparent_recovery();
                    }
                    return Err(report_websocket_error(
                        error,
                        &mut collected,
                        &self.websocket_rate_limits,
                        events,
                    ));
                }
                Ok(text) => text,
            };
            if text.len() > MAX_STREAM_EVENT_BYTES {
                return Err(report_websocket_error(
                    ApiError::fatal("model sent an oversized WebSocket event"),
                    &mut collected,
                    &self.websocket_rate_limits,
                    events,
                ));
            }
            let event: Value = match serde_json::from_str(text.as_str()) {
                Ok(event) => event,
                Err(error) => {
                    return Err(report_websocket_error(
                        ApiError::fatal(format!("failed to decode WebSocket event: {error}")),
                        &mut collected,
                        &self.websocket_rate_limits,
                        events,
                    ));
                }
            };
            if let Some(snapshot) = crate::rate_limits::parse_rate_limit_event(&event) {
                upsert_rate_limit(&mut self.websocket_rate_limits, snapshot);
            }
            // The Responses WebSocket protocol starts every generation with response.created. A
            // reused connection can still deliver duplicated terminal frames from its previous
            // generation, so keep those prelude frames out of this response's sequence, UI, and
            // conversation history.
            match response_boundary.accepts(&event) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    return Err(report_websocket_error(
                        error,
                        &mut collected,
                        &self.websocket_rate_limits,
                        events,
                    ));
                }
            }
            if matches!(
                event.get("type").and_then(Value::as_str),
                Some(
                    "response.created"
                        | "response.completed"
                        | "response.failed"
                        | "response.incomplete"
                )
            ) && let Some(response_id) = event
                .pointer("/response/id")
                .and_then(Value::as_str)
                .filter(|response_id| !response_id.is_empty())
            {
                self.websocket_last_response_id = Some(response_id.to_string());
            }
            if request_history_identities.event_references(&event) {
                continue;
            }
            if matches!(
                event.get("type").and_then(Value::as_str),
                Some("response.output_item.added" | "response.output_item.done")
            ) && let Some(item) = event.get("item")
            {
                let item_match = match request_history_identities.output_item_match(item) {
                    Ok(item_match) => item_match,
                    Err(error) => {
                        return Err(report_websocket_error(
                            error,
                            &mut collected,
                            &self.websocket_rate_limits,
                            events,
                        ));
                    }
                };
                match (event.get("type").and_then(Value::as_str), item_match) {
                    (
                        Some("response.output_item.added"),
                        RequestHistoryItemMatch::Exact | RequestHistoryItemMatch::Conflicting,
                    )
                    | (Some("response.output_item.done"), RequestHistoryItemMatch::Exact) => {
                        continue;
                    }
                    (Some("response.output_item.done"), RequestHistoryItemMatch::Conflicting) => {
                        return Err(report_websocket_error(
                            ApiError::fatal(
                                "model reused an output item identity from request history",
                            ),
                            &mut collected,
                            &self.websocket_rate_limits,
                            events,
                        ));
                    }
                    _ => {}
                }
            }
            if awaiting_startup_first_event {
                awaiting_startup_first_event = false;
                self.startup_websocket_pending_first_event = false;
                next_event_idle_timeout = self.stream_idle_timeout;
            }
            self.capture_event_turn_state(&event);
            if let Err(mut error) = process_event_value(
                event,
                &mut collected,
                completed_items,
                events,
                &expected_model,
                &mut self.server_model_warning_emitted,
            ) {
                if collected.has_model_activity() {
                    if error.kind == ApiErrorKind::Unauthorized
                        && let Err(refresh_error) = self.force_refresh_auth().await
                    {
                        return Err(report_websocket_error(
                            refresh_error,
                            &mut collected,
                            &self.websocket_rate_limits,
                            events,
                        ));
                    }
                    error = error.without_transparent_recovery();
                }
                return Err(report_websocket_error(
                    error,
                    &mut collected,
                    &self.websocket_rate_limits,
                    events,
                ));
            }
            if collected.completed {
                break;
            }
        }
        let mut response = collected.finish()?;
        response.server_reasoning_included = self.websocket_reasoning_included;
        let streamed_rate_limits = std::mem::take(&mut response.rate_limits);
        response.rate_limits = std::mem::take(&mut self.websocket_rate_limits);
        for snapshot in streamed_rate_limits {
            upsert_rate_limit(&mut response.rate_limits, snapshot);
        }
        self.websocket_rate_limits.clone_from(&response.rate_limits);
        Ok(response)
    }

    async fn ensure_websocket(&mut self, request_kind: RequestKind) -> ApiResult<()> {
        if let Some(websocket) = &self.websocket {
            if !websocket.is_closed() {
                return Ok(());
            }
            // A reconnect cannot reuse connection-local `previous_response_id` state.
            self.abandon_response();
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
        let (websocket, response_headers) =
            WebSocketConnection::connect(&url, &headers, self.websocket_tls_config.clone()).await?;
        self.websocket_reasoning_included = response_headers.contains_key("x-reasoning-included");
        self.websocket_server_model = response_headers
            .get("openai-model")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        for snapshot in crate::rate_limits::parse_all_rate_limits(&response_headers) {
            upsert_rate_limit(&mut self.websocket_rate_limits, snapshot);
        }
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
        completed: &mut CompletedResponseMetadata,
    ) -> ApiResult<CompactionResult> {
        self.compact_with_identity(
            history,
            compaction,
            RequestInputIdentity::AppendOnly {
                cursor,
                trailing_items: 1,
            },
            events,
            completed,
        )
        .await
    }

    async fn compact_with_identity(
        &mut self,
        history: &[Value],
        compaction: CompactionRequest,
        mut input_identity: RequestInputIdentity,
        events: Option<&UnboundedSender<AgentEvent>>,
        completed: &mut CompletedResponseMetadata,
    ) -> ApiResult<CompactionResult> {
        if history.is_empty() {
            return Err(ApiError::fatal("cannot compact an empty conversation"));
        }
        let trigger = compaction::compaction_trigger();
        let selection = self.model_selection.get();
        let [tool_tokens, instruction_tokens] = estimated_harness_tokens();
        let fixed_request_tokens = tool_tokens
            .saturating_add(instruction_tokens)
            .saturating_add(estimated_tokens(std::slice::from_ref(&trigger)));
        let mut prompt_history = history.to_vec();
        let effective_context_window = selection.effective_context_window();
        let rewritten_outputs = compaction::trim_tool_outputs_to_fit(
            &mut prompt_history,
            effective_context_window.saturating_sub(fixed_request_tokens),
        );
        if rewritten_outputs > 0 {
            input_identity = RequestInputIdentity::Exact;
        }
        prompt_history.push(trigger);
        let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
        let request_kind = RequestKind::Compaction(compaction);
        let mut retries = 0_usize;
        let request = self.build_request(prompt_history, request_kind);
        let mut response = loop {
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
                Err(mut error) if error.is_retryable() => {
                    if let Some((usage, rate_limits)) = error.take_completed_response() {
                        completed.merge(usage, rate_limits);
                    }
                    if retries >= MAX_REMOTE_COMPACTION_V2_STREAM_RETRIES {
                        if self.fall_back_to_http() {
                            retries = 0;
                            continue;
                        }
                        return Err(std::mem::take(completed).attach_to(error));
                    }
                    let delay = error.retry_after().unwrap_or_else(|| retry_delay(retries));
                    retries = retries.saturating_add(1);
                    sleep(delay).await;
                }
                Err(mut error) => {
                    if let Some((usage, rate_limits)) = error.take_completed_response() {
                        completed.merge(usage, rate_limits);
                    }
                    return Err(std::mem::take(completed).attach_to(error));
                }
            }
        };
        completed.merge(
            response.usage.take(),
            std::mem::take(&mut response.rate_limits),
        );
        (response.usage, response.rate_limits) = std::mem::take(completed).into_parts();
        let compaction_output = match compaction::opaque_compaction_item(
            response.items,
            response.compaction_item_count,
            response.output_item_count,
        ) {
            Ok(compaction_output) => compaction_output,
            Err(error) => {
                // A completed but unusable response must not become the baseline
                // for a later request using the unchanged conversation.
                self.abandon_response();
                return Err(ApiError::fatal(error)
                    .with_completed_response(response.usage, response.rate_limits));
            }
        };
        let mut prompt_history = request.input;
        let _trigger = prompt_history.pop();
        let mut items = compaction::retained_compacted_history(prompt_history);
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
    ) -> ApiResult<(
        reqwest::Response,
        Vec<crate::rate_limits::RateLimitSnapshot>,
    )> {
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
        let mut retry_rate_limits = Vec::new();
        loop {
            let mut headers = self.request_headers(accept, request_kind, &auth)?;
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(CONTENT_ENCODING, HeaderValue::from_static("zstd"));
            let response = tokio::time::timeout(
                self.stream_idle_timeout,
                self.client
                    .post(&url)
                    .headers(headers)
                    .body(compressed_body.clone())
                    .send(),
            )
            .await;
            let response = match response {
                Ok(Ok(response)) => response,
                Ok(Err(_)) if attempt < MAX_HTTP_RETRIES => {
                    sleep(retry_delay(attempt)).await;
                    attempt = attempt.saturating_add(1);
                    continue;
                }
                Ok(Err(error)) => {
                    return Err(CompletedResponseMetadata {
                        usage: None,
                        rate_limits: retry_rate_limits,
                    }
                    .attach_to(ApiError::retryable(format!(
                        "Responses request failed: {error}"
                    ))));
                }
                Err(_) => {
                    return Err(CompletedResponseMetadata {
                        usage: None,
                        rate_limits: retry_rate_limits,
                    }
                    .attach_to(ApiError::stream_idle(
                        "Responses HTTP request was inactive before response headers",
                    )));
                }
            };
            let status = response.status();
            if status.is_success() {
                return Ok((response, retry_rate_limits));
            }
            let retry_after = parse_retry_after(response.headers());
            let rate_limits = crate::rate_limits::parse_all_rate_limits(response.headers());
            if status == StatusCode::UNAUTHORIZED && !refreshed_after_unauthorized {
                for snapshot in rate_limits {
                    upsert_rate_limit(&mut retry_rate_limits, snapshot);
                }
                auth = match self
                    .auth
                    .refreshed_snapshot_after_unauthorized(&self.client, &auth)
                    .await
                {
                    Ok(auth) => auth,
                    Err(error) => {
                        return Err(CompletedResponseMetadata {
                            usage: None,
                            rate_limits: retry_rate_limits,
                        }
                        .attach_to(ApiError::fatal(format!("{error:#}"))));
                    }
                };
                refreshed_after_unauthorized = true;
                // Codex's unauthorized recovery wraps the transport retry loop,
                // so a refreshed request receives a fresh transport budget.
                attempt = 0;
                continue;
            }
            let transport_retryable = status.is_server_error();
            if transport_retryable && attempt < MAX_HTTP_RETRIES {
                for snapshot in rate_limits {
                    upsert_rate_limit(&mut retry_rate_limits, snapshot);
                }
                sleep(
                    retry_after
                        .unwrap_or_else(|| retry_delay(attempt))
                        .min(Duration::from_secs(30)),
                )
                .await;
                attempt = attempt.saturating_add(1);
                continue;
            }
            let body = tokio::time::timeout(
                self.stream_idle_timeout,
                bounded_error_body(response, MAX_ERROR_BODY_BYTES, MAX_ERROR_BODY_CHARS),
            )
            .await
            .unwrap_or_else(|_| "timed out reading the response body".to_string());
            let message = format!("Responses request failed with {status}: {body}");
            let error = if status == StatusCode::TOO_MANY_REQUESTS || transport_retryable {
                ApiError::retryable_after(message, retry_after)
            } else {
                ApiError::fatal(message)
            };
            for snapshot in rate_limits {
                upsert_rate_limit(&mut retry_rate_limits, snapshot);
            }
            return Err(error.with_completed_response(None, retry_rate_limits));
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
    mut validation: ResponseValidation<'_>,
    output_item_mode: OutputItemMode,
) -> ApiResult<ModelResponse> {
    let mut decoder = SseDecoder::default();
    let mut collected = CollectedResponse::new(output_item_mode);
    let mut decoded = Vec::new();
    let mut event_deadline = tokio::time::Instant::now() + idle_timeout;
    loop {
        let chunk = match tokio::time::timeout_at(event_deadline, response.chunk()).await {
            Ok(Ok(chunk)) => chunk,
            Ok(Err(error)) => {
                let error = ApiError::retryable(format!("failed to read model response: {error}"));
                return Err(report_model_activity_error(error, &mut collected, events));
            }
            Err(_) => {
                let error = ApiError::stream_idle("model response was inactive for too long");
                return Err(report_model_activity_error(error, &mut collected, events));
            }
        };
        let Some(chunk) = chunk else {
            let decode_result = decoder.finish(&mut decoded);
            if let Err(error) = process_decoded_http_events(
                &mut decoded,
                decode_result,
                &mut collected,
                completed_items,
                events,
                &mut validation,
                Instant::now(),
            ) {
                return Err(report_model_activity_error(error, &mut collected, events));
            }
            break;
        };
        let received_at = Instant::now();
        let decode_result = decoder.push(&chunk, &mut decoded);
        if !decoded.is_empty() {
            event_deadline = tokio::time::Instant::now() + idle_timeout;
        }
        match process_decoded_http_events(
            &mut decoded,
            decode_result,
            &mut collected,
            completed_items,
            events,
            &mut validation,
            received_at,
        ) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => {
                return Err(report_model_activity_error(error, &mut collected, events));
            }
        }
    }
    if !collected.completed {
        let error = ApiError::retryable("model stream closed before response.completed");
        return Err(report_model_activity_error(error, &mut collected, events));
    }
    collected.finish()
}

fn process_decoded_http_events(
    decoded: &mut Vec<String>,
    decode_result: ApiResult<()>,
    collected: &mut CollectedResponse,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
    validation: &mut ResponseValidation<'_>,
    received_at: Instant,
) -> ApiResult<bool> {
    // A decoder can return an error after yielding earlier complete SSE events from the same
    // transport chunk. Apply those events first so partial output remains lossless, and let a
    // terminal response.completed make any trailing bytes irrelevant.
    for data in decoded.drain(..) {
        process_event_at(
            &data,
            collected,
            completed_items,
            events,
            validation.expected_model,
            &mut *validation.server_model_warning_emitted,
            received_at,
        )?;
        if collected.completed {
            return Ok(true);
        }
    }
    decode_result?;
    Ok(false)
}

struct CollectedResponse {
    output_items: CollectedOutputItems,
    added_output: OutputItemTracker,
    completed_output: CompletedOutput,
    last_sequence: Option<(u64, JsonFingerprint)>,
    item_summary: ResponseItemSummary,
    usage: Option<TokenUsage>,
    rate_limits: Vec<crate::rate_limits::RateLimitSnapshot>,
    end_turn: Option<bool>,
    response_id: Option<String>,
    model_activity_observed: bool,
    completed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct JsonFingerprint([u8; 32]);

struct ObservedOutputItem {
    id: Option<String>,
    call_id: Option<String>,
    fingerprint: JsonFingerprint,
}

#[derive(Default)]
struct OutputItemTracker {
    by_id: HashMap<String, JsonFingerprint>,
    by_call_id: HashMap<String, JsonFingerprint>,
    by_index: HashMap<u64, JsonFingerprint>,
    anonymous_without_event_identity: HashSet<JsonFingerprint>,
}

#[derive(Default)]
struct CompletedOutput {
    order: Vec<ObservedOutputItem>,
    items: OutputItemTracker,
}

enum CollectedOutputItems {
    RetainedAndEmitted(Vec<Value>),
    Retained(Vec<Value>),
    Compaction {
        candidate: Option<Value>,
        output_count: usize,
        compaction_count: usize,
    },
    Transferred {
        count: usize,
    },
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
            OutputItemMode::Compaction => CollectedOutputItems::Compaction {
                candidate: None,
                output_count: 0,
                compaction_count: 0,
            },
            OutputItemMode::Transfer => CollectedOutputItems::Transferred { count: 0 },
        };
        Self {
            output_items,
            added_output: OutputItemTracker::default(),
            completed_output: CompletedOutput::default(),
            last_sequence: None,
            item_summary: ResponseItemSummary::default(),
            usage: None,
            rate_limits: Vec::new(),
            end_turn: None,
            response_id: None,
            model_activity_observed: false,
            completed: false,
        }
    }

    fn item_count(&self) -> usize {
        match &self.output_items {
            CollectedOutputItems::RetainedAndEmitted(items)
            | CollectedOutputItems::Retained(items) => items.len(),
            CollectedOutputItems::Compaction { output_count, .. } => *output_count,
            CollectedOutputItems::Transferred { count } => *count,
        }
    }

    fn observes_model_output(&self) -> bool {
        matches!(
            &self.output_items,
            CollectedOutputItems::RetainedAndEmitted(_) | CollectedOutputItems::Transferred { .. }
        )
    }

    fn observe_model_activity(&mut self) {
        if self.observes_model_output() {
            self.model_activity_observed = true;
        }
    }

    fn has_model_activity(&self) -> bool {
        self.model_activity_observed
    }

    fn validates_output_content(&self) -> bool {
        !matches!(&self.output_items, CollectedOutputItems::Compaction { .. })
    }

    fn upsert_rate_limit(&mut self, snapshot: crate::rate_limits::RateLimitSnapshot) {
        upsert_rate_limit(&mut self.rate_limits, snapshot);
    }

    fn observe_event_sequence(&mut self, event: &Value) -> ApiResult<bool> {
        let Some(sequence_number) = optional_event_u64(event, "sequence_number")? else {
            return Ok(true);
        };
        match self.last_sequence {
            None => {
                self.last_sequence = Some((sequence_number, json_fingerprint(event)?));
                Ok(true)
            }
            Some((previous, _)) if sequence_number > previous => {
                self.last_sequence = Some((sequence_number, json_fingerprint(event)?));
                Ok(true)
            }
            Some((previous, fingerprint)) if sequence_number == previous => {
                if json_fingerprint(event)? == fingerprint {
                    Ok(false)
                } else {
                    Err(ApiError::fatal(
                        "model stream reused a sequence number for a different event",
                    ))
                }
            }
            Some(_) => Err(ApiError::fatal("model stream sent events out of sequence")),
        }
    }

    fn observe_output_item_added(&mut self, event: &Value, item: &Value) -> ApiResult<bool> {
        self.added_output
            .observe(event, item)
            .map(|observed| observed.is_some())
    }

    fn observe_output_item_done(&mut self, event: &Value, item: &Value) -> ApiResult<bool> {
        self.completed_output.observe_done(event, item)
    }

    fn validate_completed_output(&self, response: &Value) -> ApiResult<()> {
        self.completed_output.validate_response(response)
    }

    fn output_item_was_completed(&self, event: &Value, item: &Value) -> ApiResult<bool> {
        self.completed_output.references_item(event, item)
    }

    fn event_references_completed_output(&self, event: &Value) -> ApiResult<bool> {
        self.completed_output.references_event(event)
    }

    fn observe_response_id(&mut self, response_id: Option<&str>) -> ApiResult<()> {
        let Some(response_id) = response_id.filter(|response_id| !response_id.is_empty()) else {
            return Ok(());
        };
        if self
            .response_id
            .as_deref()
            .is_some_and(|existing| existing != response_id)
        {
            return Err(ApiError::fatal(
                "model stream changed response IDs before completion",
            ));
        }
        if self.response_id.is_none() {
            self.response_id = Some(response_id.to_string());
        }
        Ok(())
    }

    fn reject_completed_response(
        &mut self,
        error: ApiError,
        usage: Option<TokenUsage>,
    ) -> ApiError {
        error.with_completed_response(usage, std::mem::take(&mut self.rate_limits))
    }

    fn observe_terminal_response(&mut self, event: &Value) -> ApiResult<Option<TokenUsage>> {
        let Some(response) = event.get("response") else {
            return Ok(None);
        };
        let usage = match parse_response_usage(response) {
            Ok(usage) => usage,
            Err(error) => return Err(self.reject_completed_response(error, None)),
        };
        if let Err(error) = self.observe_response_id(response.get("id").and_then(Value::as_str)) {
            return Err(self.reject_completed_response(error, usage));
        }
        Ok(usage)
    }

    fn push_item(
        &mut self,
        item: Value,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> ApiResult<()> {
        if self.validates_output_content() {
            validate_assistant_message_phase(&item)?;
        }
        let (completed_message, completed_web_search) = if self.observes_model_output() {
            (
                self.item_summary.observe(&item, events.is_some()),
                WebSearchCall::from_response_item(&item),
            )
        } else {
            (None, None)
        };
        match &mut self.output_items {
            CollectedOutputItems::RetainedAndEmitted(items) => {
                completed_items.send(item.clone()).map_err(|_| {
                    ApiError::fatal("model output consumer closed before response completion")
                })?;
                items.push(item);
            }
            CollectedOutputItems::Retained(items) => items.push(item),
            CollectedOutputItems::Compaction {
                candidate,
                output_count,
                compaction_count,
            } => {
                *output_count = output_count.saturating_add(1);
                if matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("compaction" | "compaction_summary")
                ) {
                    *compaction_count = compaction_count.saturating_add(1);
                    if candidate.is_none() {
                        *candidate = Some(item);
                    }
                }
            }
            CollectedOutputItems::Transferred { count } => {
                completed_items.send(item).map_err(|_| {
                    ApiError::fatal("model output consumer closed before response completion")
                })?;
                *count = count.saturating_add(1);
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
        Ok(())
    }

    fn finish(self) -> ApiResult<ModelResponse> {
        let output_item_count = self.item_count();
        let response_id = self
            .response_id
            .ok_or_else(|| ApiError::fatal("response.completed omitted the response ID"))?;
        let (items, compaction_item_count) = match self.output_items {
            CollectedOutputItems::RetainedAndEmitted(items)
            | CollectedOutputItems::Retained(items) => (items, 0),
            CollectedOutputItems::Compaction {
                candidate,
                compaction_count,
                ..
            } => (candidate.into_iter().collect(), compaction_count),
            CollectedOutputItems::Transferred { .. } => (Vec::new(), 0),
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
            compaction_item_count,
            has_assistant_text: self.item_summary.has_assistant_text,
        })
    }
}

impl OutputItemTracker {
    fn observe(&mut self, event: &Value, item: &Value) -> ApiResult<Option<ObservedOutputItem>> {
        let fingerprint = json_fingerprint(item)?;
        let id = optional_output_item_identity(item, "id")?;
        let call_id = optional_output_item_identity(item, "call_id")?;
        let output_index = optional_event_u64(event, "output_index")?;
        let has_sequence_number = event.get("sequence_number").is_some();

        let id_fingerprint = id.as_ref().and_then(|id| self.by_id.get(id));
        let call_id_fingerprint = call_id
            .as_ref()
            .and_then(|call_id| self.by_call_id.get(call_id));
        let index_fingerprint = output_index.and_then(|index| self.by_index.get(&index));
        if [id_fingerprint, call_id_fingerprint, index_fingerprint]
            .into_iter()
            .flatten()
            .any(|existing| *existing != fingerprint)
        {
            return Err(ApiError::fatal(
                "model sent conflicting duplicate output items",
            ));
        }

        let anonymous_without_event_identity =
            id.is_none() && call_id.is_none() && output_index.is_none() && !has_sequence_number;
        let duplicate = id_fingerprint.is_some()
            || call_id_fingerprint.is_some()
            || index_fingerprint.is_some()
            || (anonymous_without_event_identity
                && self.anonymous_without_event_identity.contains(&fingerprint));

        if let Some(id) = &id {
            self.by_id.entry(id.clone()).or_insert(fingerprint);
        }
        if let Some(call_id) = &call_id {
            self.by_call_id
                .entry(call_id.clone())
                .or_insert(fingerprint);
        }
        if let Some(output_index) = output_index {
            self.by_index.entry(output_index).or_insert(fingerprint);
        }
        if anonymous_without_event_identity {
            self.anonymous_without_event_identity.insert(fingerprint);
        }
        Ok((!duplicate).then_some(ObservedOutputItem {
            id,
            call_id,
            fingerprint,
        }))
    }
}

impl CompletedOutput {
    fn observe_done(&mut self, event: &Value, item: &Value) -> ApiResult<bool> {
        validate_output_item(item)?;
        let Some(observed) = self.items.observe(event, item)? else {
            return Ok(false);
        };
        self.order.push(observed);
        Ok(true)
    }

    fn references_item(&self, event: &Value, item: &Value) -> ApiResult<bool> {
        let id = output_item_identity(item, "id")?;
        let call_id = output_item_identity(item, "call_id")?;
        let output_index = optional_event_u64(event, "output_index")?;
        Ok(id.is_some_and(|id| self.items.by_id.contains_key(id))
            || call_id.is_some_and(|call_id| self.items.by_call_id.contains_key(call_id))
            || output_index.is_some_and(|index| self.items.by_index.contains_key(&index)))
    }

    fn references_event(&self, event: &Value) -> ApiResult<bool> {
        let item_id = event_identity(event, "item_id")?;
        let call_id = event_identity(event, "call_id")?;
        let output_index = optional_event_u64(event, "output_index")?;
        Ok(
            item_id.is_some_and(|item_id| self.items.by_id.contains_key(item_id))
                || call_id.is_some_and(|call_id| self.items.by_call_id.contains_key(call_id))
                || output_index.is_some_and(|index| self.items.by_index.contains_key(&index)),
        )
    }

    fn validate_response(&self, response: &Value) -> ApiResult<()> {
        let Some(output) = response.get("output") else {
            // The ChatGPT backend can omit the full output array. In that compatibility shape,
            // completed `output_item.done` events remain the only available source of truth.
            return Ok(());
        };
        let output = output
            .as_array()
            .ok_or_else(|| ApiError::fatal("response.completed output was not an array"))?;
        if output.len() != self.order.len() {
            return Err(ApiError::fatal(
                "response.completed did not match completed output items",
            ));
        }
        for (expected, item) in self.order.iter().zip(output) {
            validate_output_item(item)?;
            let id = optional_output_item_identity(item, "id")?;
            let call_id = optional_output_item_identity(item, "call_id")?;
            let strong_identity_matches = expected
                .id
                .as_ref()
                .is_none_or(|expected_id| id.as_ref() == Some(expected_id))
                && expected
                    .call_id
                    .as_ref()
                    .is_none_or(|expected_call_id| call_id.as_ref() == Some(expected_call_id));
            let identity_matches = if expected.id.is_none() && expected.call_id.is_none() {
                json_fingerprint(item)? == expected.fingerprint
            } else {
                strong_identity_matches
            };
            if !identity_matches {
                return Err(ApiError::fatal(
                    "response.completed did not match completed output items",
                ));
            }
        }
        Ok(())
    }
}

fn validate_output_item(item: &Value) -> ApiResult<()> {
    let item = item
        .as_object()
        .ok_or_else(|| ApiError::fatal("model output contained a non-object item"))?;
    if item
        .get("type")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(ApiError::fatal(
            "model output contained an item without a valid type",
        ));
    }
    Ok(())
}

fn optional_output_item_identity(item: &Value, name: &str) -> ApiResult<Option<String>> {
    output_item_identity(item, name).map(|identity| identity.map(str::to_owned))
}

fn output_item_identity<'a>(item: &'a Value, name: &str) -> ApiResult<Option<&'a str>> {
    string_identity(item, name, "model output")
}

fn event_identity<'a>(event: &'a Value, name: &str) -> ApiResult<Option<&'a str>> {
    string_identity(event, name, "model event")
}

fn string_identity<'a>(
    value: &'a Value,
    name: &str,
    description: &str,
) -> ApiResult<Option<&'a str>> {
    let Some(value) = value.get(name) else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(value) if value.is_empty() => Ok(None),
        Value::String(value) => Ok(Some(value)),
        _ => Err(ApiError::fatal(format!(
            "{description} contained an invalid {name}"
        ))),
    }
}

enum RequestHistoryItemMatch {
    None,
    Exact,
    Conflicting,
}

struct RequestHistoryIdentities<'a> {
    by_id: HashMap<&'a str, &'a Value>,
    by_type_and_call_id: HashMap<(&'a str, &'a str), &'a Value>,
    call_ids: HashSet<&'a str>,
}

impl<'a> RequestHistoryIdentities<'a> {
    fn new(history: &'a [Value]) -> Self {
        let mut identities = Self {
            by_id: HashMap::new(),
            by_type_and_call_id: HashMap::new(),
            call_ids: HashSet::new(),
        };
        for item in history {
            if let Some(id) = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| response_item_id_is_prefixed(id))
            {
                identities.by_id.entry(id).or_insert(item);
            }
            if let Some(call_id) = item
                .get("call_id")
                .and_then(Value::as_str)
                .filter(|call_id| !call_id.is_empty())
            {
                identities.call_ids.insert(call_id);
                if let Some(kind) = item.get("type").and_then(Value::as_str) {
                    identities
                        .by_type_and_call_id
                        .entry((kind, call_id))
                        .or_insert(item);
                }
            }
        }
        identities
    }

    fn output_item_match(&self, item: &Value) -> ApiResult<RequestHistoryItemMatch> {
        validate_output_item(item)?;
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let id = output_item_identity(item, "id")?;
        let call_id = output_item_identity(item, "call_id")?;
        let previous_by_id = id.and_then(|id| self.by_id.get(id).copied());
        let previous_by_call_id =
            call_id.and_then(|call_id| self.by_type_and_call_id.get(&(kind, call_id)).copied());
        let mut matched = false;
        for previous in previous_by_id.into_iter().chain(previous_by_call_id) {
            matched = true;
            if previous != item {
                return Ok(RequestHistoryItemMatch::Conflicting);
            }
        }
        Ok(if matched {
            RequestHistoryItemMatch::Exact
        } else {
            RequestHistoryItemMatch::None
        })
    }

    fn event_references(&self, event: &Value) -> bool {
        event
            .get("item_id")
            .and_then(Value::as_str)
            .is_some_and(|item_id| self.by_id.contains_key(item_id))
            || event
                .get("call_id")
                .and_then(Value::as_str)
                .is_some_and(|call_id| self.call_ids.contains(call_id))
    }
}

struct Sha256Writer<'a>(&'a mut Sha256);

impl std::io::Write for Sha256Writer<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn json_fingerprint(value: &Value) -> ApiResult<JsonFingerprint> {
    let mut digest = Sha256::new();
    serde_json::to_writer(Sha256Writer(&mut digest), value)
        .map_err(|error| ApiError::fatal(format!("failed to fingerprint model JSON: {error}")))?;
    let mut fingerprint = [0_u8; 32];
    fingerprint.copy_from_slice(&digest.finalize());
    Ok(JsonFingerprint(fingerprint))
}

fn optional_event_u64(event: &Value, name: &str) -> ApiResult<Option<u64>> {
    let Some(value) = event.get(name) else {
        return Ok(None);
    };
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| ApiError::fatal(format!("model event contained an invalid {name}")))
}

fn report_model_activity_error(
    mut error: ApiError,
    collected: &mut CollectedResponse,
    events: Option<&UnboundedSender<AgentEvent>>,
) -> ApiError {
    error.add_response_rate_limit_fallbacks(std::mem::take(&mut collected.rate_limits));
    if error.is_retryable()
        && collected.has_model_activity()
        && let Some(events) = events
    {
        let _ = events.send(AgentEvent::Warning(
            "Model response streaming was interrupted after partial output; recovery will continue from completed response items only."
                .to_string(),
        ));
    }
    error
}

fn report_websocket_error(
    error: ApiError,
    collected: &mut CollectedResponse,
    connection_rate_limits: &[crate::rate_limits::RateLimitSnapshot],
    events: Option<&UnboundedSender<AgentEvent>>,
) -> ApiError {
    let mut error = report_model_activity_error(error, collected, events);
    error.add_response_rate_limit_fallbacks(connection_rate_limits.iter().cloned());
    error
}

fn upsert_rate_limit(
    snapshots: &mut Vec<crate::rate_limits::RateLimitSnapshot>,
    mut snapshot: crate::rate_limits::RateLimitSnapshot,
) {
    if let Some(existing) = snapshots
        .iter_mut()
        .find(|existing| existing.limit_id == snapshot.limit_id)
    {
        crate::rate_limits::fill_missing_rate_limit_fields(&mut snapshot, existing);
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
    if collected.completed {
        return Ok(());
    }
    if let Some(snapshot) = crate::rate_limits::parse_rate_limit_event(&event) {
        collected.upsert_rate_limit(snapshot);
        return Ok(());
    }
    if !collected.observe_event_sequence(&event)? {
        return Ok(());
    }
    observe_server_model(
        event_server_model(&event),
        expected_model,
        events,
        server_model_warning_emitted,
    );
    // Consume completed items in wire order. Hosted items can be added and then interrupted
    // without a done event, so output indexes identify duplicates but never define completeness or
    // ordering. Fingerprints keep duplicate detection bounded even for large encrypted items.
    if event.get("type").and_then(Value::as_str) == Some("response.output_item.done") {
        let item = event
            .get("item")
            .ok_or_else(|| ApiError::fatal("output_item.done omitted its item"))?;
        if !collected.observe_output_item_done(&event, item)? {
            return Ok(());
        }
        collected.observe_model_activity();
        let item = event
            .get_mut("item")
            .map(Value::take)
            .ok_or_else(|| ApiError::fatal("output_item.done omitted its item"))?;
        collected.push_item(item, completed_items, events)?;
        return Ok(());
    }
    match event.get("type").and_then(Value::as_str) {
        Some("response.output_item.added") => {
            if let Some(item) = event.get("item")
                && collected.output_item_was_completed(&event, item)?
            {
                return Err(ApiError::fatal(
                    "model stream added an output item after it was completed",
                ));
            }
            if collected.validates_output_content()
                && let Some(item) = event.get("item")
            {
                validate_assistant_message_phase(item)?;
            }
            if collected.observes_model_output()
                && let Some(item) = event.get("item")
            {
                if !collected.observe_output_item_added(&event, item)? {
                    return Ok(());
                }
                collected.observe_model_activity();
                if let Some(events) = events {
                    if let Some(message) = AssistantMessage::from_response_item(item) {
                        let _ = events.send(AgentEvent::ModelMessageStarted(message));
                    }
                    if let Some(search) = WebSearchCall::from_response_item(item) {
                        let _ = events.send(AgentEvent::WebSearchStarted(search));
                    }
                }
            }
        }
        Some("response.output_text.delta")
            if collected.observes_model_output()
                && event.get("delta").is_some_and(Value::is_string) =>
        {
            if collected.event_references_completed_output(&event)? {
                return Err(ApiError::fatal(
                    "model stream sent text for an output item after it was completed",
                ));
            }
            collected.observe_model_activity();
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
            // `output_item.done` remains authoritative. When the backend also supplies the full
            // output array, use it only to prove that no completed item was omitted or reordered.
            let response = event
                .get_mut("response")
                .map(Value::take)
                .ok_or_else(|| ApiError::fatal("response.completed omitted its response"))?;
            let usage = match parse_response_usage(&response) {
                Ok(usage) => usage,
                Err(error) => {
                    return Err(collected.reject_completed_response(error, None));
                }
            };
            if let Err(error) = validate_completed_response(&response) {
                return Err(collected.reject_completed_response(error, usage));
            }
            if let Err(error) = collected.validate_completed_output(&response) {
                return Err(collected.reject_completed_response(error, usage));
            }
            let Some(response_id) = response
                .get("id")
                .and_then(Value::as_str)
                .filter(|response_id| !response_id.is_empty())
            else {
                return Err(collected.reject_completed_response(
                    ApiError::fatal("response.completed omitted the response ID"),
                    usage,
                ));
            };
            if let Err(error) = collected.observe_response_id(Some(response_id)) {
                return Err(collected.reject_completed_response(error, usage));
            }
            let end_turn = match optional_response_bool(&response, "end_turn") {
                Ok(end_turn) => end_turn,
                Err(error) => {
                    return Err(collected.reject_completed_response(error, usage));
                }
            };
            collected.usage = usage;
            collected.end_turn = end_turn;
            collected.completed = true;
        }
        Some("response.created") => {
            collected.observe_response_id(event.pointer("/response/id").and_then(Value::as_str))?;
        }
        Some("response.failed") => {
            let usage = collected.observe_terminal_response(&event)?;
            let message = event
                .pointer("/response/error/message")
                .and_then(Value::as_str)
                .unwrap_or("model response failed");
            let code = event
                .pointer("/response/error/code")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Err(
                collected.reject_completed_response(classify_stream_error(code, message), usage)
            );
        }
        Some("response.incomplete") => {
            let usage = collected.observe_terminal_response(&event)?;
            let reason = event
                .pointer("/response/incomplete_details/reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Err(collected.reject_completed_response(
                ApiError::retryable(format!("model response was incomplete: {reason}")),
                usage,
            ));
        }
        Some("error") => {
            let headers = event
                .get("headers")
                .and_then(Value::as_object)
                .map(json_headers_to_http_headers);
            if let Some(headers) = &headers {
                for snapshot in crate::rate_limits::parse_all_rate_limits(headers) {
                    collected.upsert_rate_limit(snapshot);
                }
            }
            let mut error = error_event(&event);
            if error.retry_after.is_none() {
                error.retry_after = headers.as_ref().and_then(parse_retry_after);
            }
            return Err(collected.reject_completed_response(error, None));
        }
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
    // ChatGPT currently wraps these fields in `error`; the public Responses event uses the same
    // fields at top level. Accept both without weakening status classification.
    let code = event
        .pointer("/error/code")
        .or_else(|| event.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = event
        .pointer("/error/message")
        .or_else(|| event.get("message"))
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
            } else if matches!(
                code,
                "rate_limit_exceeded" | "server_error" | "server_is_overloaded" | "slow_down"
            ) {
                ApiError::retryable_after(
                    message,
                    (code == "rate_limit_exceeded")
                        .then(|| parse_rate_limit_delay(message))
                        .flatten(),
                )
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
        | "misalignment_policy_violation"
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
    match response.pointer("/reasoning/context") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(context)) if context == "all_turns" => Ok(()),
        Some(Value::String(context)) => Err(ApiError::fatal(format!(
            "backend used reasoning.context `{context}`; bettercodex requires `all_turns`"
        ))),
        Some(_) => Err(ApiError::fatal(
            "response.completed contained an invalid reasoning.context",
        )),
    }
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

fn json_headers_to_http_headers(headers: &Map<String, Value>) -> HeaderMap {
    let mut mapped = HeaderMap::new();
    for (name, value) in headers {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Some(value) = json_value_to_header_value(value) else {
            continue;
        };
        mapped.insert(name, value);
    }
    mapped
}

fn json_value_to_header_value(value: &Value) -> Option<HeaderValue> {
    match value {
        Value::String(value) => HeaderValue::from_str(value).ok(),
        Value::Number(value) => HeaderValue::from_str(&value.to_string()).ok(),
        Value::Bool(value) => Some(HeaderValue::from_static(if *value {
            "true"
        } else {
            "false"
        })),
        Value::Array(values) => values.first().and_then(json_value_to_header_value),
        _ => None,
    }
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

fn optional_response_bool(response: &Value, name: &str) -> ApiResult<Option<bool>> {
    match response.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ApiError::fatal(format!(
            "response.completed contained an invalid {name}"
        ))),
    }
}

fn parse_response_usage(response: &Value) -> ApiResult<Option<TokenUsage>> {
    match response.get("usage") {
        None | Some(Value::Null) => Ok(None),
        Some(usage) => parse_usage(usage).map(Some),
    }
}

fn parse_usage(usage: &Value) -> ApiResult<TokenUsage> {
    let usage = usage
        .as_object()
        .ok_or_else(|| ApiError::fatal("response.completed usage was not an object"))?;
    let input_tokens = required_usage_u64(usage, "input_tokens")?;
    let output_tokens = optional_usage_u64(usage, "output_tokens")?.unwrap_or(0);
    Ok(TokenUsage {
        input_tokens,
        cached_input_tokens: optional_usage_detail_u64(
            usage,
            "input_tokens_details",
            "cached_tokens",
        )?
        .unwrap_or(0),
        cache_write_input_tokens: optional_usage_detail_u64(
            usage,
            "input_tokens_details",
            "cache_write_tokens",
        )?
        .unwrap_or(0),
        output_tokens,
        reasoning_output_tokens: optional_usage_detail_u64(
            usage,
            "output_tokens_details",
            "reasoning_tokens",
        )?
        .unwrap_or(0),
        total_tokens: optional_usage_u64(usage, "total_tokens")?
            .unwrap_or_else(|| input_tokens.saturating_add(output_tokens)),
    })
}

fn required_usage_u64(usage: &Map<String, Value>, name: &str) -> ApiResult<u64> {
    optional_usage_u64(usage, name)?
        .ok_or_else(|| ApiError::fatal(format!("response.completed usage omitted its {name}")))
}

fn optional_usage_u64(usage: &Map<String, Value>, name: &str) -> ApiResult<Option<u64>> {
    match usage.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            ApiError::fatal(format!(
                "response.completed usage contained an invalid {name}"
            ))
        }),
    }
}

fn optional_usage_detail_u64(
    usage: &Map<String, Value>,
    details_name: &str,
    name: &str,
) -> ApiResult<Option<u64>> {
    match usage.get(details_name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(details)) => optional_usage_u64(details, name),
        Some(_) => Err(ApiError::fatal(format!(
            "response.completed usage contained invalid {details_name}"
        ))),
    }
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
