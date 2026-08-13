use crate::assistant_message::AssistantMessage;
use crate::auth::Auth;
use crate::auth::AuthSnapshot;
use crate::auth::SharedAuth;
use crate::compaction;
use crate::compaction::CompactionRequest;
use crate::context::HistoryCursor;
use crate::context::estimated_tokens;
use crate::events::AgentEvent;
use crate::http_client::backoff;
use crate::http_client::bounded_error_body;
use crate::model::ModelSelection;
use crate::model::SharedModelSelection;
use crate::rollout::SessionIdentity;
use crate::service_tier::ServiceTier;
use crate::time::unix_timestamp_millis;
use crate::tools;
use crate::tools::ToolCall;
use crate::usage::TokenUsage;
use crate::web_search::ToolTurnContext;
use crate::web_search::WebSearchClient;
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
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;
use std::io;
use std::io::Write;
use std::sync::LazyLock;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::sleep;

#[path = "api_websocket.rs"]
mod websocket;
use websocket::WebSocketConnection;

#[path = "api_sse.rs"]
mod sse;
use sse::SseDecoder;

const BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const SYSTEM_PROMPT_TEMPLATE: &str = include_str!("../prompts/system.md");
const PLATFORM_SHELL_GUIDANCE_MARKER: &str = "{{platform_shell_guidance}}";
#[cfg(any(not(windows), test))]
const UNIX_PLATFORM_SHELL_GUIDANCE: &str = include_str!("../prompts/system-unix.md");
#[cfg(any(windows, test))]
const WINDOWS_PLATFORM_SHELL_GUIDANCE: &str = include_str!("../prompts/system-windows.md");
#[cfg(not(windows))]
const PLATFORM_SHELL_GUIDANCE: &str = UNIX_PLATFORM_SHELL_GUIDANCE;
#[cfg(windows)]
const PLATFORM_SHELL_GUIDANCE: &str = WINDOWS_PLATFORM_SHELL_GUIDANCE;
const MAX_HTTP_RETRIES: usize = 4;
const MAX_REMOTE_COMPACTION_V2_STREAM_RETRIES: usize = 2;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(200);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const WEBSOCKET_PREWARM_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_STREAM_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 16_000;
const MAX_ERROR_BODY_CHARS: usize = 4_000;
// Encoding is on the critical path before network I/O. Level 1 retains strong compression for the
// long-context JSON workloads in `benchmark_responses_request_encoding` while materially reducing
// CPU time versus zstd's level-3 default.
const REQUEST_COMPRESSION_LEVEL: i32 = 1;
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const REMOTE_COMPACTION_V2_FEATURE: &str = "remote_compaction_v2";
const X_CODEX_ROUTING_HINT: &str = "x-codex-routing-hint";
const X_CODEX_TURN_STATE: &str = "x-codex-turn-state";
const WS_RESPONSES_LITE_CLIENT_METADATA: &str =
    "ws_request_header_x_openai_internal_codex_responses_lite";
static SYSTEM_PROMPT: LazyLock<String> =
    LazyLock::new(|| render_system_prompt(SYSTEM_PROMPT_TEMPLATE, PLATFORM_SHELL_GUIDANCE));
static DEFAULT_STABLE_INPUT_PREFIX_ITEMS: OnceLock<[Value; 2]> = OnceLock::new();
static PAPERCUT_STABLE_INPUT_PREFIX_ITEMS: OnceLock<[Value; 2]> = OnceLock::new();
static DEFAULT_STABLE_HARNESS_TOKEN_ESTIMATES: OnceLock<[u64; 2]> = OnceLock::new();
static PAPERCUT_STABLE_HARNESS_TOKEN_ESTIMATES: OnceLock<[u64; 2]> = OnceLock::new();

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
                | ApiErrorKind::PreviousResponseNotFound
                | ApiErrorKind::WebSocketUnavailable
        )
    }

    pub(crate) fn is_stream_idle(&self) -> bool {
        self.kind == ApiErrorKind::StreamIdle
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
    tool_configuration: tools::ToolConfiguration,
    prefer_websocket: bool,
    websocket_prewarm_attempted: bool,
    websocket: Option<WebSocketConnection>,
    websocket_reasoning_included: bool,
    websocket_server_model: Option<String>,
    websocket_rate_limits: Vec<crate::rate_limits::RateLimitSnapshot>,
    websocket_baseline: Option<WebSocketBaseline>,
    server_model_warning_emitted: bool,
    stream_idle_timeout: Duration,
}

struct WebSocketBaseline {
    properties_fingerprint: RequestPropertiesFingerprint,
    input: WebSocketBaselineInput,
    response_id: String,
}

/// Collision-resistant identity for fields that must stay fixed across an incremental request.
///
/// A digest keeps baseline changes conservative without retaining another full property tree for
/// the active WebSocket connection.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RequestPropertiesFingerprint([u8; 32]);

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
    request: Value,
    cursor: HistoryCursor,
    input_restoration: SamplingInputRestoration,
}

struct WebSocketRequestRestoration {
    input_prefix: Option<Vec<Value>>,
    stream: Option<Value>,
    client_turn_state: Option<Value>,
}

impl WebSocketRequestRestoration {
    fn restore(self, request: &mut Value) -> ApiResult<()> {
        let object = request
            .as_object_mut()
            .ok_or_else(|| ApiError::fatal("Responses request was not an object"))?;
        object.remove("type");
        object.remove("generate");
        object.remove("previous_response_id");
        if let Some(stream) = self.stream {
            object.insert("stream".to_string(), stream);
        }
        let client_metadata = object
            .get_mut("client_metadata")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| ApiError::fatal("Responses request omitted client metadata"))?;
        client_metadata.remove(X_CODEX_TURN_STATE);
        if let Some(turn_state) = self.client_turn_state {
            client_metadata.insert(X_CODEX_TURN_STATE.to_string(), turn_state);
        }
        client_metadata.remove(WS_RESPONSES_LITE_CLIENT_METADATA);
        if let Some(mut input_prefix) = self.input_prefix {
            let incremental_input = object
                .remove("input")
                .ok_or_else(|| ApiError::fatal("WebSocket delta request omitted input"))?;
            let Value::Array(incremental_input) = incremental_input else {
                return Err(ApiError::fatal(
                    "WebSocket delta request input was not an array",
                ));
            };
            input_prefix.extend(incremental_input);
            object.insert("input".to_string(), Value::Array(input_prefix));
        }
        Ok(())
    }
}

struct WebSocketRequestGuard<'a> {
    request: &'a mut Value,
    restoration: Option<WebSocketRequestRestoration>,
}

impl<'a> WebSocketRequestGuard<'a> {
    fn new(request: &'a mut Value, restoration: WebSocketRequestRestoration) -> Self {
        Self {
            request,
            restoration: Some(restoration),
        }
    }

    fn request(&self) -> &Value {
        self.request
    }

    fn restore(mut self) -> ApiResult<()> {
        self.restore_inner()
    }

    fn restore_inner(&mut self) -> ApiResult<()> {
        let Some(restoration) = self.restoration.take() else {
            return Ok(());
        };
        restoration.restore(self.request)
    }
}

impl Drop for WebSocketRequestGuard<'_> {
    fn drop(&mut self) {
        let _ = self.restore_inner();
    }
}

struct SamplingInputRestoration {
    expected_prefix: [Value; 2],
    stripped_image_details: Vec<StrippedImageDetail>,
}

struct StrippedImageDetail {
    item_index: usize,
    content_index: usize,
    detail: Value,
}

impl WebSocketBaseline {
    fn new(
        request: &Value,
        response: &ModelResponse,
        input_identity: RequestInputIdentity,
    ) -> ApiResult<Self> {
        let request_input = request
            .get("input")
            .and_then(Value::as_array)
            .ok_or_else(|| ApiError::fatal("Responses request omitted input"))?;
        let input = match input_identity {
            RequestInputIdentity::AppendOnly {
                cursor,
                trailing_items: 0,
            } => WebSocketBaselineInput::AppendOnly {
                cursor,
                request_len: request_input.len(),
                response_items: response.output_item_count,
            },
            RequestInputIdentity::Exact | RequestInputIdentity::AppendOnly { .. } => {
                if response.items.len() != response.output_item_count {
                    return Err(ApiError::fatal(
                        "an exact WebSocket baseline did not retain its output items",
                    ));
                }
                WebSocketBaselineInput::Exact {
                    request: request_input.clone(),
                    output: response.items.clone(),
                }
            }
        };
        Ok(Self {
            properties_fingerprint: reusable_request_properties_fingerprint(request)?,
            input,
            response_id: response.response_id.clone(),
        })
    }
}

impl SamplingRequest {
    pub(crate) fn into_history(mut self) -> ApiResult<(Vec<Value>, HistoryCursor)> {
        let input = self
            .request
            .as_object_mut()
            .and_then(|request| request.remove("input"))
            .ok_or_else(|| ApiError::fatal("Responses request omitted input"))?;
        let Value::Array(input) = input else {
            return Err(ApiError::fatal("Responses request input was not an array"));
        };
        Ok((self.input_restoration.restore(input)?, self.cursor))
    }
}

impl SamplingInputRestoration {
    fn restore(self, mut input: Vec<Value>) -> ApiResult<Vec<Value>> {
        let expected = &self.expected_prefix;
        if input.get(..expected.len()) != Some(expected.as_slice()) {
            return Err(ApiError::fatal(
                "sampling request lost its inserted Responses Lite prefix",
            ));
        }
        input.drain(..expected.len());
        for stripped in self.stripped_image_details {
            let content = input
                .get_mut(stripped.item_index)
                .and_then(image_content_items_mut)
                .ok_or_else(|| {
                    ApiError::fatal("sampling request lost image content while it was in flight")
                })?;
            let image = content
                .get_mut(stripped.content_index)
                .and_then(Value::as_object_mut)
                .filter(|image| image.get("type").and_then(Value::as_str) == Some("input_image"))
                .ok_or_else(|| {
                    ApiError::fatal("sampling request changed an image while it was in flight")
                })?;
            image.insert("detail".to_string(), stripped.detail);
        }
        Ok(input)
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
        tool_configuration: tools::ToolConfiguration,
    ) -> anyhow::Result<Self> {
        Self::new_with_base_url(
            auth,
            identity,
            compaction_count,
            model_selection,
            service_tier,
            tool_configuration,
            BASE_URL.to_string(),
        )
    }

    pub(crate) fn new_with_base_url(
        auth: Auth,
        identity: &SessionIdentity,
        compaction_count: u64,
        model_selection: ModelSelection,
        service_tier: ServiceTier,
        tool_configuration: tools::ToolConfiguration,
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
            tool_configuration,
            prefer_websocket: true,
            websocket_prewarm_attempted: false,
            websocket: None,
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
            tool_configuration: self.tool_configuration,
            prefer_websocket: self.prefer_websocket,
            websocket_prewarm_attempted: false,
            websocket: None,
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

    pub(crate) fn set_tool_configuration(&mut self, tool_configuration: tools::ToolConfiguration) {
        if self.tool_configuration == tool_configuration {
            return;
        }
        self.tool_configuration = tool_configuration;
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

    pub(crate) fn web_search_client(&self) -> WebSearchClient {
        WebSearchClient::new(
            self.client.clone(),
            self.auth.clone(),
            self.base_url.clone(),
            self.session_id.clone(),
            self.model_selection.clone(),
        )
    }

    pub(crate) fn rate_limit_client(&self) -> crate::rate_limits::RateLimitClient {
        crate::rate_limits::RateLimitClient::new(
            self.client.clone(),
            self.auth.clone(),
            &self.base_url,
        )
    }

    pub(crate) fn tool_turn_context(&self, history: &[Value]) -> ToolTurnContext {
        let selection = self.model_selection.get();
        ToolTurnContext::from_history(
            history,
            self.turn_metadata(RequestKind::Turn).to_string(),
            selection.truncation_policy(),
        )
    }

    pub(crate) fn abandon_response(&mut self) {
        self.websocket = None;
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
        request: &mut SamplingRequest,
        completed_items: &UnboundedSender<Value>,
    ) -> ApiResult<ModelResponse> {
        self.respond_sampling_with_events(request, completed_items, None)
            .await
    }

    pub(crate) async fn respond_sampling_streaming(
        &mut self,
        request: &mut SamplingRequest,
        completed_items: &UnboundedSender<Value>,
        events: &UnboundedSender<AgentEvent>,
    ) -> ApiResult<ModelResponse> {
        self.respond_sampling_with_events(request, completed_items, Some(events))
            .await
    }

    async fn respond_sampling_with_events(
        &mut self,
        request: &mut SamplingRequest,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> ApiResult<ModelResponse> {
        self.respond_request_with_events(
            &mut request.request,
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
        request: &mut Value,
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
        request: &Value,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
        request_kind: RequestKind,
    ) -> ApiResult<ModelResponse> {
        let expected_model = request_model(request)?.to_string();
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
        let mut request = self.build_request(Vec::new(), RequestKind::Prewarm);
        let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
        let response = self
            .respond_websocket(
                &mut request,
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
        logical_request: &mut Value,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
        request_kind: RequestKind,
        mode: WebSocketRequestMode,
        input_identity: RequestInputIdentity,
    ) -> ApiResult<ModelResponse> {
        let expected_model = request_model(logical_request)?.to_string();
        self.ensure_websocket(request_kind).await?;
        let websocket_server_model = self.websocket_server_model.clone();
        observe_server_model(
            websocket_server_model.as_deref(),
            &expected_model,
            events,
            &mut self.server_model_warning_emitted,
        );
        let idle_timeout = match mode {
            WebSocketRequestMode::Inference => self.stream_idle_timeout,
            WebSocketRequestMode::Warmup => {
                self.stream_idle_timeout.min(WEBSOCKET_PREWARM_IDLE_TIMEOUT)
            }
        };
        // Serialize a delta in place, then restore the logical request for retries and baseline
        // retention. Input values move between vectors instead of being deep-cloned.
        let restoration = self.prepare_websocket_request(logical_request, mode, input_identity)?;
        let guard = WebSocketRequestGuard::new(logical_request, restoration);
        let send_result = self
            .websocket
            .as_mut()
            .ok_or_else(|| ApiError::websocket_unavailable("Responses WebSocket is unavailable"))?
            .send(guard.request(), idle_timeout)
            .await;
        guard.restore()?;
        send_result?;

        let mut collected =
            CollectedResponse::new(OutputItemMode::for_websocket(request_kind, input_identity));
        loop {
            let text = self
                .websocket
                .as_mut()
                .ok_or_else(|| {
                    ApiError::websocket_unavailable("Responses WebSocket is unavailable")
                })?
                .next_text(idle_timeout)
                .await?
                .ok_or_else(|| ApiError::retryable("Responses WebSocket ended unexpectedly"))?;
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

    fn prepare_websocket_request(
        &self,
        request: &mut Value,
        mode: WebSocketRequestMode,
        input_identity: RequestInputIdentity,
    ) -> ApiResult<WebSocketRequestRestoration> {
        let incremental = match mode {
            WebSocketRequestMode::Warmup => None,
            WebSocketRequestMode::Inference => {
                if let Some(baseline) = &self.websocket_baseline {
                    if !request_properties_match(&baseline.properties_fingerprint, request)? {
                        None
                    } else {
                        let current_input = request
                            .get("input")
                            .and_then(Value::as_array)
                            .ok_or_else(|| ApiError::fatal("Responses request omitted input"))?;
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

        let object = request
            .as_object_mut()
            .ok_or_else(|| ApiError::fatal("Responses request was not an object"))?;
        let stream = object.remove("stream");
        let client_metadata = object
            .get_mut("client_metadata")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| ApiError::fatal("Responses request omitted client metadata"))?;
        let client_turn_state = client_metadata.remove(X_CODEX_TURN_STATE);
        if let Some(turn_state) = &self.turn_state {
            client_metadata.insert(
                X_CODEX_TURN_STATE.to_string(),
                Value::String(turn_state.clone()),
            );
        }
        client_metadata.remove(WS_RESPONSES_LITE_CLIENT_METADATA);
        client_metadata.insert(
            WS_RESPONSES_LITE_CLIENT_METADATA.to_string(),
            Value::String("true".to_string()),
        );
        object.insert(
            "type".to_string(),
            Value::String("response.create".to_string()),
        );
        if matches!(mode, WebSocketRequestMode::Warmup) {
            object.insert("generate".to_string(), Value::Bool(false));
        }
        let Some((previous_response_id, baseline_length)) = incremental else {
            return Ok(WebSocketRequestRestoration {
                input_prefix: None,
                stream,
                client_turn_state,
            });
        };
        let full_input = object
            .remove("input")
            .ok_or_else(|| ApiError::fatal("Responses request omitted input"))?;
        let Value::Array(mut input_prefix) = full_input else {
            return Err(ApiError::fatal("Responses request input was not an array"));
        };
        let incremental_input = input_prefix.split_off(baseline_length);
        object.insert("input".to_string(), Value::Array(incremental_input));
        object.insert(
            "previous_response_id".to_string(),
            Value::String(previous_response_id),
        );
        Ok(WebSocketRequestRestoration {
            input_prefix: Some(input_prefix),
            stream,
            client_turn_state,
        })
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
        let [tool_prefix_tokens, instruction_tokens] =
            estimated_harness_tokens(self.tool_configuration);
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
        let mut request = self.build_request(prompt_history, request_kind);
        let response = loop {
            match self
                .respond_request_with_events(
                    &mut request,
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

    fn build_request(&self, history: Vec<Value>, request_kind: RequestKind) -> Value {
        let selection = self.model_selection.get();
        let input = compose_input(history, self.tool_configuration);
        self.build_request_from_input(input, request_kind, &selection)
    }

    pub(crate) fn build_sampling_request(
        &self,
        history: Vec<Value>,
        cursor: HistoryCursor,
    ) -> SamplingRequest {
        let selection = self.model_selection.get();
        let (input, input_restoration) = compose_sampling_input(history, self.tool_configuration);
        SamplingRequest {
            request: self.build_request_from_input(input, RequestKind::Turn, &selection),
            cursor,
            input_restoration,
        }
    }

    fn build_request_from_input(
        &self,
        input: Vec<Value>,
        request_kind: RequestKind,
        selection: &ModelSelection,
    ) -> Value {
        let reasoning = json!({
            "effort": selection.reasoning_effort.as_str(),
            "summary": "auto",
            "context": "all_turns",
        });
        let mut request = json!({
            "model": selection.model,
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "reasoning": reasoning,
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": self.session_id,
            "text": {"verbosity": "low"},
            "client_metadata": self.client_metadata(request_kind),
        });
        if let Some(service_tier) = self.effective_service_tier() {
            request["service_tier"] = Value::String(service_tier.to_string());
        }
        request["input"] = Value::Array(input);
        request
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
            "x-openai-internal-codex-responses-lite",
            "true",
        )?;
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
        body: &Value,
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
                    .force_refreshed_snapshot(&self.client)
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

fn encode_request_body(body: &Value) -> ApiResult<Bytes> {
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

fn compose_input(history: Vec<Value>, tool_configuration: tools::ToolConfiguration) -> Vec<Value> {
    compose_sampling_input(history, tool_configuration).0
}

fn compose_sampling_input(
    mut history: Vec<Value>,
    tool_configuration: tools::ToolConfiguration,
) -> (Vec<Value>, SamplingInputRestoration) {
    let stripped_image_details = strip_image_details(&mut history);
    let expected_prefix = stable_request_prefix(tool_configuration);
    history.splice(0..0, expected_prefix.clone());
    (
        history,
        SamplingInputRestoration {
            expected_prefix,
            stripped_image_details,
        },
    )
}

fn is_additional_tools_item(item: &Value, expected: &Value) -> bool {
    ["type", "role", "tools"]
        .into_iter()
        .all(|field| item.get(field) == expected.get(field))
}

fn strip_image_details(history: &mut [Value]) -> Vec<StrippedImageDetail> {
    let mut stripped = Vec::new();
    for (item_index, item) in history.iter_mut().enumerate() {
        let Some(content) = image_content_items_mut(item) else {
            continue;
        };
        for (content_index, content_item) in content.iter_mut().enumerate() {
            if content_item.get("type").and_then(Value::as_str) != Some("input_image") {
                continue;
            }
            let Some(detail) = content_item
                .as_object_mut()
                .and_then(|image| image.remove("detail"))
            else {
                continue;
            };
            stripped.push(StrippedImageDetail {
                item_index,
                content_index,
                detail,
            });
        }
    }
    stripped
}

fn image_content_items_mut(item: &mut Value) -> Option<&mut Vec<Value>> {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => item.get_mut("content")?.as_array_mut(),
        Some("function_call_output" | "custom_tool_call_output") => {
            item.get_mut("output")?.as_array_mut()
        }
        _ => None,
    }
}

pub(crate) fn stable_input_prefix_items(
    tool_configuration: tools::ToolConfiguration,
) -> &'static [Value; 2] {
    let items = if tool_configuration.papercut_enabled() {
        &PAPERCUT_STABLE_INPUT_PREFIX_ITEMS
    } else {
        &DEFAULT_STABLE_INPUT_PREFIX_ITEMS
    };
    items.get_or_init(|| {
        [
            json!({
                "type": "additional_tools",
                "role": "developer",
                "tools": tools::responses_lite_specifications(tool_configuration),
            }),
            developer_instructions_item(),
        ]
    })
}

pub(crate) fn stable_request_prefix(tool_configuration: tools::ToolConfiguration) -> [Value; 2] {
    stable_input_prefix_items(tool_configuration).clone()
}

pub(crate) fn is_stable_tools_prefix_item(item: &Value) -> bool {
    if item.get("type").and_then(Value::as_str) != Some("additional_tools")
        || item.get("role").and_then(Value::as_str) != Some("developer")
    {
        return false;
    }
    is_additional_tools_item(
        item,
        &stable_input_prefix_items(tools::ToolConfiguration::default())[0],
    ) || is_additional_tools_item(
        item,
        &stable_input_prefix_items(tools::ToolConfiguration::with_papercut())[0],
    )
}

pub(crate) fn is_stable_instructions_prefix_item(item: &Value) -> bool {
    if item.get("type").and_then(Value::as_str) != Some("message")
        || item.get("role").and_then(Value::as_str) != Some("developer")
    {
        return false;
    }
    let Some([content]) = item
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    else {
        return false;
    };
    content.as_object().is_some_and(|content| {
        content.len() == 2
            && content.get("type").and_then(Value::as_str) == Some("input_text")
            && content.get("text").and_then(Value::as_str) == Some(harness_instructions())
    })
}

pub(crate) fn harness_instructions() -> &'static str {
    SYSTEM_PROMPT.as_str()
}

fn developer_instructions_item() -> Value {
    json!({
        "type": "message",
        "role": "developer",
        "content": [{
            "type": "input_text",
            "text": harness_instructions(),
        }],
    })
}

fn render_system_prompt(template: &str, platform_shell_guidance: &str) -> String {
    assert_eq!(
        template.matches(PLATFORM_SHELL_GUIDANCE_MARKER).count(),
        1,
        "system prompt must contain exactly one platform shell guidance marker"
    );
    template.trim().replace(
        PLATFORM_SHELL_GUIDANCE_MARKER,
        platform_shell_guidance.trim(),
    )
}

pub(crate) fn estimated_harness_tokens(tool_configuration: tools::ToolConfiguration) -> [u64; 2] {
    let estimates = if tool_configuration.papercut_enabled() {
        &PAPERCUT_STABLE_HARNESS_TOKEN_ESTIMATES
    } else {
        &DEFAULT_STABLE_HARNESS_TOKEN_ESTIMATES
    };
    *estimates.get_or_init(|| {
        let [tools_item, instructions_item] = stable_input_prefix_items(tool_configuration);
        [
            estimated_tokens(std::slice::from_ref(tools_item)),
            estimated_tokens(std::slice::from_ref(instructions_item)),
        ]
    })
}

fn is_reusable_request_property(name: &str) -> bool {
    !matches!(name, "input" | "client_metadata" | "stream_options")
}

struct ReusableRequestProperties<'a>(&'a [(&'a str, &'a Value)]);

impl Serialize for ReusableRequestProperties<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut properties = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            properties.serialize_entry(name, value)?;
        }
        properties.end()
    }
}

struct DigestWriter<'a>(&'a mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn reusable_request_properties_fingerprint(
    request: &Value,
) -> ApiResult<RequestPropertiesFingerprint> {
    let request = request
        .as_object()
        .ok_or_else(|| ApiError::fatal("Responses request was not an object"))?;
    // WebSocket request restoration can reinsert fields in a different order, and serde_json's
    // preserve_order feature is enabled in the application dependency graph. Canonicalize only
    // the small reference list rather than cloning the potentially large property values.
    let mut properties = request
        .iter()
        .filter(|(name, _)| is_reusable_request_property(name))
        .map(|(name, value)| (name.as_str(), value))
        .collect::<Vec<_>>();
    properties.sort_unstable_by_key(|(name, _)| *name);
    let mut digest = Sha256::new();
    serde_json::to_writer(
        DigestWriter(&mut digest),
        &ReusableRequestProperties(&properties),
    )
    .map_err(|error| {
        ApiError::fatal(format!(
            "failed to fingerprint reusable Responses request properties: {error}"
        ))
    })?;
    Ok(RequestPropertiesFingerprint(digest.finalize().into()))
}

fn request_properties_match(
    previous: &RequestPropertiesFingerprint,
    current: &Value,
) -> ApiResult<bool> {
    Ok(*previous == reusable_request_properties_fingerprint(current)?)
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
            for data in decoded.drain(..) {
                process_event(
                    &data,
                    &mut collected,
                    completed_items,
                    events,
                    validation.expected_model,
                    &mut *validation.server_model_warning_emitted,
                )?;
            }
            break;
        };
        decoder.push(&chunk, &mut decoded)?;
        if !decoded.is_empty() {
            event_deadline = tokio::time::Instant::now() + idle_timeout;
        }
        for data in decoded.drain(..) {
            process_event(
                &data,
                &mut collected,
                completed_items,
                events,
                validation.expected_model,
                &mut *validation.server_model_warning_emitted,
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
        if let (Some(events), Some(message)) = (events, completed_message) {
            let _ = events.send(AgentEvent::ModelMessageCompleted(message));
        }
    }

    fn finish(self) -> ApiResult<ModelResponse> {
        if !self.pending_items.is_empty() {
            return Err(ApiError::fatal(
                "model response completed with a gap in output item indexes",
            ));
        }
        let output_item_count = self.item_count();
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
            response_id: self
                .response_id
                .ok_or_else(|| ApiError::fatal("response.completed omitted the response ID"))?,
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
                self.final_answer = Some(message.text.clone());
            }
            Some(message)
        } else if is_terminal && has_text {
            self.final_answer = Some(message.text);
            None
        } else {
            None
        }
    }
}

fn process_event(
    data: &str,
    collected: &mut CollectedResponse,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
    expected_model: &str,
    server_model_warning_emitted: &mut bool,
) -> ApiResult<()> {
    if data == "[DONE]" {
        return Ok(());
    }
    let event: Value = serde_json::from_str(data)
        .map_err(|error| ApiError::fatal(format!("failed to decode SSE event: {error}")))?;
    process_event_value(
        event,
        collected,
        completed_items,
        events,
        expected_model,
        server_model_warning_emitted,
    )
}

fn process_event_value(
    mut event: Value,
    collected: &mut CollectedResponse,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
    expected_model: &str,
    server_model_warning_emitted: &mut bool,
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
            if let Some(message) = event
                .get("item")
                .and_then(AssistantMessage::from_response_item)
                && let Some(events) = events
            {
                let _ = events.send(AgentEvent::ModelMessageStarted(message));
            }
        }
        Some("response.output_text.delta") => {
            if let Some(Value::String(delta)) = event.get_mut("delta")
                && let Some(events) = events
            {
                let _ = events.send(AgentEvent::ModelMessageDelta(std::mem::take(delta)));
            }
        }
        Some("response.reasoning_summary_text.delta") => {
            if let Some(Value::String(delta)) = event.get_mut("delta")
                && let Some(events) = events
            {
                let _ = events.send(AgentEvent::ReasoningSummaryDelta(std::mem::take(delta)));
            }
        }
        Some("response.reasoning_summary_part.added") => {
            if let Some(events) = events {
                let _ = events.send(AgentEvent::ReasoningSummarySectionStarted);
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
            if status == Some(429) || status.is_some_and(|status| status >= 500) {
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

fn request_model(request: &Value) -> ApiResult<&str> {
    request
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::fatal("Responses request omitted its model"))
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
