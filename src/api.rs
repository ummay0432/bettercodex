use crate::MODEL;
use crate::auth::Auth;
use crate::auth::AuthSnapshot;
use crate::auth::SharedAuth;
use crate::compaction;
use crate::compaction::CompactionRequest;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use crate::context::estimated_tokens;
use crate::events::AgentEvent;
use crate::rollout::SessionIdentity;
use crate::tools;
use crate::tools::ToolCall;
use crate::usage::TokenUsage;
use crate::web_search::ToolTurnContext;
use crate::web_search::WebSearchClient;
use anyhow::Context;
use bytes::Bytes;
use codex_client::backoff;
use reqwest::StatusCode;
use reqwest::header::CONTENT_ENCODING;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::sleep;
use tokio::time::timeout;
use uuid::Uuid;

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
const MAX_STREAM_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 16_000;
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const REMOTE_COMPACTION_V2_FEATURE: &str = "remote_compaction_v2";
const X_CODEX_TURN_STATE: &str = "x-codex-turn-state";
const WS_RESPONSES_LITE_CLIENT_METADATA: &str =
    "ws_request_header_x_openai_internal_codex_responses_lite";
static CONTEXT_PREFIX_ITEMS: LazyLock<[Value; 2]> = LazyLock::new(|| {
    [
        json!({
            "type": "additional_tools",
            "role": "developer",
            "tools": tools::specifications(),
        }),
        json!({
            "type": "message",
            "role": "developer",
            "content": [{"type": "input_text", "text": SYSTEM_PROMPT.trim()}],
        }),
    ]
});

pub(crate) type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApiErrorKind {
    Fatal,
    Retryable,
    Unauthorized,
    CacheUnsupported,
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
    explicit_cache: bool,
    prefer_websocket: bool,
    websocket_prewarm_attempted: bool,
    websocket: Option<WebSocketConnection>,
    websocket_reasoning_included: bool,
    websocket_baseline: Option<WebSocketBaseline>,
}

#[derive(Clone)]
struct WebSocketBaseline {
    request: Value,
    response_id: String,
    output: Vec<Value>,
}

struct WebSocketRequestRestoration {
    input_prefix: Option<Vec<Value>>,
    stream: Option<Value>,
    client_turn_state: Option<Value>,
    client_responses_lite: Option<Value>,
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
        if let Some(responses_lite) = self.client_responses_lite {
            client_metadata.insert(
                WS_RESPONSES_LITE_CLIENT_METADATA.to_string(),
                responses_lite,
            );
        }
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

#[derive(Clone, Copy)]
enum WebSocketRequestMode {
    Inference,
    Warmup,
}

pub(crate) struct ModelResponse {
    pub(crate) items: Vec<Value>,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) text: String,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) server_reasoning_included: bool,
    response_id: String,
}

#[derive(Debug)]
pub(crate) struct CompactionResult {
    pub(crate) items: Vec<Value>,
    pub(crate) usage: Option<TokenUsage>,
}

impl ApiClient {
    pub(crate) fn new(
        auth: Auth,
        identity: &SessionIdentity,
        compaction_count: u64,
    ) -> anyhow::Result<Self> {
        Self::new_with_base_url(auth, identity, compaction_count, BASE_URL.to_string())
    }

    pub(crate) fn new_with_base_url(
        auth: Auth,
        identity: &SessionIdentity,
        compaction_count: u64,
        base_url: String,
    ) -> anyhow::Result<Self> {
        codex_utils_rustls_provider::ensure_rustls_crypto_provider();
        let mut default_headers = HeaderMap::new();
        default_headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));
        let client = codex_client::with_chatgpt_cloudflare_cookie_store(reqwest::Client::builder())
            .default_headers(default_headers)
            .user_agent(concat!("bettercodex/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .context("failed to create HTTP client")?;
        Ok(Self {
            client,
            auth: SharedAuth::new(auth),
            base_url,
            installation_id: identity.installation_id.clone(),
            session_id: identity.session_id.clone(),
            thread_id: identity.thread_id.clone(),
            turn_id: Uuid::new_v4().to_string(),
            turn_started_at_unix_ms: unix_timestamp_millis(),
            turn_state: None,
            window: compaction_count,
            explicit_cache: true,
            prefer_websocket: true,
            websocket_prewarm_attempted: false,
            websocket: None,
            websocket_reasoning_included: false,
            websocket_baseline: None,
        })
    }

    pub(crate) fn begin_turn(&mut self) -> &str {
        self.turn_id = Uuid::new_v4().to_string();
        self.turn_started_at_unix_ms = unix_timestamp_millis();
        self.turn_state = None;
        &self.turn_id
    }

    pub(crate) fn web_search_client(&self) -> WebSearchClient {
        WebSearchClient::new(
            self.client.clone(),
            self.auth.clone(),
            self.base_url.clone(),
            self.session_id.clone(),
        )
    }

    pub(crate) fn tool_turn_context(&self, history: &[Value]) -> ToolTurnContext {
        ToolTurnContext::from_history(history, self.turn_metadata(RequestKind::Turn).to_string())
    }

    pub(crate) fn abandon_response(&mut self) {
        self.websocket = None;
        self.websocket_reasoning_included = false;
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

    pub(crate) async fn respond(
        &mut self,
        history: Vec<Value>,
        completed_items: &UnboundedSender<Value>,
    ) -> ApiResult<ModelResponse> {
        self.respond_with_events(history, completed_items, None, RequestKind::Turn)
            .await
    }

    pub(crate) async fn respond_streaming(
        &mut self,
        history: Vec<Value>,
        completed_items: &UnboundedSender<Value>,
        events: &UnboundedSender<AgentEvent>,
    ) -> ApiResult<ModelResponse> {
        self.respond_with_events(history, completed_items, Some(events), RequestKind::Turn)
            .await
    }

    async fn respond_with_events(
        &mut self,
        history: Vec<Value>,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
        request_kind: RequestKind,
    ) -> ApiResult<ModelResponse> {
        let mut refreshed_websocket_auth = false;
        let mut retried_full_websocket_request = false;
        // Build once from the owned sampling snapshot. Retries mutate only transport fields, and
        // a successful WebSocket response moves this request into the next delta baseline.
        let mut request = self.build_request(history, request_kind);
        if self.prefer_websocket && !self.websocket_prewarm_attempted {
            self.websocket_prewarm_attempted = true;
            match self.prewarm_websocket().await {
                Ok(()) => {}
                Err(error) if error.kind == ApiErrorKind::Unauthorized => {
                    self.auth
                        .force_refresh(&self.client)
                        .await
                        .map_err(|error| ApiError::fatal(format!("{error:#}")))?;
                    refreshed_websocket_auth = true;
                    self.abandon_response();
                }
                Err(error) if error.kind == ApiErrorKind::CacheUnsupported => {
                    if self.explicit_cache {
                        self.disable_explicit_cache();
                        disable_explicit_cache_for_request(&mut request)?;
                    }
                }
                Err(error) if error.kind == ApiErrorKind::WebSocketUnavailable => {
                    self.fall_back_to_http();
                }
                Err(_) => {
                    // Warmup is an optimization. A normal full request remains the
                    // authoritative path when the connection or warmup response fails.
                    self.abandon_response();
                }
            }
        }
        loop {
            if self.prefer_websocket {
                match self
                    .respond_websocket(
                        &mut request,
                        completed_items,
                        events,
                        request_kind,
                        WebSocketRequestMode::Inference,
                    )
                    .await
                {
                    Ok(response) => {
                        self.websocket_baseline = Some(WebSocketBaseline {
                            request,
                            response_id: response.response_id.clone(),
                            output: response.items.clone(),
                        });
                        return Ok(response);
                    }
                    Err(error)
                        if error.kind == ApiErrorKind::Unauthorized
                            && !refreshed_websocket_auth =>
                    {
                        self.auth
                            .force_refresh(&self.client)
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
                    Err(error) if error.kind == ApiErrorKind::CacheUnsupported => {
                        if self.explicit_cache {
                            self.disable_explicit_cache();
                            disable_explicit_cache_for_request(&mut request)?;
                            continue;
                        }
                        return Err(error);
                    }
                    Err(error) if error.kind == ApiErrorKind::WebSocketUnavailable => {
                        self.fall_back_to_http();
                    }
                    Err(error) => {
                        self.abandon_response();
                        return Err(error);
                    }
                }
            }

            match self
                .respond_http(&request, completed_items, events, request_kind)
                .await
            {
                Err(error) if error.kind == ApiErrorKind::CacheUnsupported => {
                    if self.explicit_cache {
                        self.disable_explicit_cache();
                        disable_explicit_cache_for_request(&mut request)?;
                    } else {
                        return Err(error);
                    }
                }
                result => return result,
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
        let response = self
            .post("responses", request, "text/event-stream", request_kind)
            .await?;
        validate_server_model_header(response.headers())?;
        let server_reasoning_included = response.headers().contains_key("x-reasoning-included");
        self.capture_turn_state(response.headers());
        let mut response = collect_http_stream(response, completed_items, events).await?;
        response.server_reasoning_included = server_reasoning_included;
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
            )
            .await?;
        self.websocket_baseline = Some(WebSocketBaseline {
            request,
            response_id: response.response_id,
            output: response.items,
        });
        Ok(())
    }

    async fn respond_websocket(
        &mut self,
        logical_request: &mut Value,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
        request_kind: RequestKind,
        mode: WebSocketRequestMode,
    ) -> ApiResult<ModelResponse> {
        self.ensure_websocket(request_kind).await?;
        // Serialize a delta in place, then restore the logical request for retries and baseline
        // retention. Input values move between vectors instead of being deep-cloned.
        let restoration = self.prepare_websocket_request(logical_request, mode)?;
        let send_result = self
            .websocket
            .as_mut()
            .ok_or_else(|| ApiError::websocket_unavailable("Responses WebSocket is unavailable"))?
            .send(logical_request, STREAM_IDLE_TIMEOUT)
            .await;
        restoration.restore(logical_request)?;
        send_result?;

        let mut collected = CollectedResponse::default();
        loop {
            let text = self
                .websocket
                .as_mut()
                .ok_or_else(|| {
                    ApiError::websocket_unavailable("Responses WebSocket is unavailable")
                })?
                .next_text(STREAM_IDLE_TIMEOUT)
                .await?
                .ok_or_else(|| ApiError::retryable("Responses WebSocket ended unexpectedly"))?;
            if text.len() > MAX_STREAM_EVENT_BYTES {
                return Err(ApiError::fatal("model sent an oversized WebSocket event"));
            }
            let event: Value = serde_json::from_str(&text).map_err(|error| {
                ApiError::fatal(format!("failed to decode WebSocket event: {error}"))
            })?;
            self.capture_event_turn_state(&event);
            process_event_value(event, &mut collected, completed_items, events)?;
            if collected.completed {
                break;
            }
        }
        let mut response = collected.finish(events)?;
        response.server_reasoning_included = self.websocket_reasoning_included;
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
        validate_server_model_header(&response_headers)?;
        self.websocket_reasoning_included = response_headers.contains_key("x-reasoning-included");
        self.capture_turn_state(&response_headers);
        self.websocket = Some(websocket);
        Ok(())
    }

    fn prepare_websocket_request(
        &self,
        request: &mut Value,
        mode: WebSocketRequestMode,
    ) -> ApiResult<WebSocketRequestRestoration> {
        let incremental = match mode {
            WebSocketRequestMode::Warmup => None,
            WebSocketRequestMode::Inference => {
                if let Some(baseline) = &self.websocket_baseline {
                    if !request_properties_match(&baseline.request, request) {
                        None
                    } else {
                        let previous_input = baseline
                            .request
                            .get("input")
                            .and_then(Value::as_array)
                            .ok_or_else(|| {
                                ApiError::fatal("previous Responses request omitted input")
                            })?;
                        let current_input = request
                            .get("input")
                            .and_then(Value::as_array)
                            .ok_or_else(|| ApiError::fatal("Responses request omitted input"))?;
                        let baseline_length =
                            previous_input.len().saturating_add(baseline.output.len());
                        if current_input.len() < baseline_length {
                            None
                        } else {
                            let expected = previous_input.iter().chain(&baseline.output);
                            expected
                                .zip(current_input.iter().take(baseline_length))
                                .all(|(previous, current)| previous == current)
                                .then(|| (baseline.response_id.clone(), baseline_length))
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
        let client_responses_lite = client_metadata.insert(
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
                client_responses_lite,
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
            client_responses_lite,
        })
    }

    pub(crate) async fn compact(
        &mut self,
        history: &[Value],
        compaction: CompactionRequest,
    ) -> ApiResult<CompactionResult> {
        if history.is_empty() {
            return Ok(CompactionResult {
                items: Vec::new(),
                usage: None,
            });
        }
        let trigger = compaction::compaction_trigger();
        let rendered_tokens =
            estimated_tokens(&compose_input(history.to_vec(), self.explicit_cache));
        let prefix_tokens = rendered_tokens
            .saturating_sub(estimated_tokens(history))
            .saturating_add(estimated_tokens(std::slice::from_ref(&trigger)));
        let mut prompt_history = history.to_vec();
        compaction::trim_tool_outputs_to_fit(
            &mut prompt_history,
            EFFECTIVE_CONTEXT_WINDOW.saturating_sub(prefix_tokens),
        );
        let mut request_history = prompt_history.clone();
        request_history.push(trigger);
        let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
        let request_kind = RequestKind::Compaction(compaction);
        let mut retries = 0_usize;
        let response = loop {
            match self
                .respond_with_events(
                    request_history.clone(),
                    &completed_items,
                    None,
                    request_kind,
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
        let compaction_output =
            compaction::opaque_compaction_item(&response.items).map_err(ApiError::fatal)?;
        let items = compaction::build_compacted_history(&prompt_history, compaction_output);
        self.window = self.window.saturating_add(1);
        self.websocket_baseline = None;
        Ok(CompactionResult {
            items,
            usage: response.usage,
        })
    }

    fn build_request(&self, history: Vec<Value>, request_kind: RequestKind) -> Value {
        let input = compose_input(history, self.explicit_cache);
        let mut request = json!({
            "model": MODEL,
            "instructions": "",
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "reasoning": {"effort": "max", "summary": "auto", "context": "all_turns"},
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": self.session_id,
            "text": {"verbosity": "low"},
            "client_metadata": self.client_metadata(request_kind),
        });
        request["input"] = Value::Array(input);
        if self.explicit_cache {
            request["prompt_cache_options"] = json!({"mode": "explicit", "ttl": "30m"});
        }
        request
    }

    fn disable_explicit_cache(&mut self) {
        self.explicit_cache = false;
        self.abandon_response();
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
            // Responses Lite requires Codex's protocol key even though
            // BetterCodex has one fixed tool runtime and no mode selector.
            "code_mode_tool_names": tools::nested_tool_name_map(),
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

    fn compatibility_turn_metadata(&self, request_kind: RequestKind) -> Value {
        let mut metadata = self.turn_metadata(request_kind);
        if let Some(metadata) = metadata.as_object_mut() {
            // The complete mapping belongs in request client_metadata. Keeping it
            // out of the compatibility header matches Codex and bounds headers
            // independently of the nested catalogue.
            metadata.remove("code_mode_tool_names");
        }
        metadata
    }

    fn window_id(&self) -> String {
        format!("{}:{}", self.thread_id, self.window)
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
            &self.compatibility_turn_metadata(request_kind).to_string(),
        )?;
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
        let encoded_body = serde_json::to_vec(body).map_err(|error| {
            ApiError::fatal(format!("failed to encode Responses request: {error}"))
        })?;
        let compressed_body = Bytes::from(
            zstd::stream::encode_all(std::io::Cursor::new(encoded_body), 3).map_err(|error| {
                ApiError::fatal(format!("failed to compress Responses request: {error}"))
            })?,
        );
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
                self.auth
                    .force_refresh(&self.client)
                    .await
                    .map_err(|error| ApiError::fatal(format!("{error:#}")))?;
                auth = self
                    .auth
                    .snapshot()
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
            let body = bounded_error_body(response).await;
            let message = format!("Responses request failed with {status}: {body}");
            if explicit_cache_unsupported(&body) {
                return Err(ApiError::new(ApiErrorKind::CacheUnsupported, message));
            }
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

fn compose_input(mut history: Vec<Value>, explicit_cache: bool) -> Vec<Value> {
    let [tools_item, mut system_item] = (*context_prefix_items()).clone();
    if explicit_cache {
        mark_cache_breakpoint(&mut system_item);
    }
    let tools_position = history
        .iter()
        .position(|item| is_additional_tools_item(item, &tools_item));
    if tools_position.is_none() {
        history.insert(0, tools_item);
    }

    let system_position = history.iter().position(is_system_prompt_item);
    match system_position {
        Some(index) if explicit_cache => mark_cache_breakpoint(&mut history[index]),
        Some(_) => {}
        None => history.insert(1, system_item),
    }
    history
}

fn is_additional_tools_item(item: &Value, expected: &Value) -> bool {
    ["type", "role", "tools"]
        .into_iter()
        .all(|field| item.get(field) == expected.get(field))
}

pub(crate) fn context_prefix_items() -> &'static [Value; 2] {
    &CONTEXT_PREFIX_ITEMS
}

fn is_system_prompt_item(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("developer")
        && item
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text == SYSTEM_PROMPT.trim())
}

fn mark_cache_breakpoint(item: &mut Value) {
    if let Some(content) = item.pointer_mut("/content/0")
        && let Some(content) = content.as_object_mut()
    {
        content.insert(
            "prompt_cache_breakpoint".to_string(),
            json!({"mode": "explicit"}),
        );
    }
}

fn disable_explicit_cache_for_request(request: &mut Value) -> ApiResult<()> {
    let object = request
        .as_object_mut()
        .ok_or_else(|| ApiError::fatal("Responses request was not an object"))?;
    object.remove("prompt_cache_options");
    let input = object
        .get_mut("input")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| ApiError::fatal("Responses request omitted input"))?;
    for item in input.iter_mut().filter(|item| is_system_prompt_item(item)) {
        if let Some(content) = item
            .pointer_mut("/content/0")
            .and_then(Value::as_object_mut)
        {
            content.remove("prompt_cache_breakpoint");
        }
    }
    Ok(())
}

fn request_properties_match(previous: &Value, current: &Value) -> bool {
    let (Some(previous), Some(current)) = (previous.as_object(), current.as_object()) else {
        return false;
    };
    let is_reusable_property =
        |name: &str| !matches!(name, "input" | "client_metadata" | "stream_options");
    let previous_count = previous
        .keys()
        .filter(|name| is_reusable_property(name))
        .count();
    let current_count = current
        .keys()
        .filter(|name| is_reusable_property(name))
        .count();
    previous_count == current_count
        && previous
            .iter()
            .all(|(name, value)| !is_reusable_property(name) || current.get(name) == Some(value))
}

async fn collect_http_stream(
    mut response: reqwest::Response,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
) -> ApiResult<ModelResponse> {
    let mut decoder = SseDecoder::default();
    let mut collected = CollectedResponse::default();
    loop {
        let chunk = timeout(STREAM_IDLE_TIMEOUT, response.chunk())
            .await
            .map_err(|_| ApiError::retryable("timed out waiting for the model response"))?
            .map_err(|error| {
                ApiError::retryable(format!("failed to read model response: {error}"))
            })?;
        let Some(chunk) = chunk else {
            for data in decoder.finish()? {
                process_event(&data, &mut collected, completed_items, events)?;
            }
            break;
        };
        for data in decoder.push(&chunk)? {
            process_event(&data, &mut collected, completed_items, events)?;
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
    collected.finish(events)
}

#[derive(Default)]
struct CollectedResponse {
    items: Vec<Value>,
    pending_items: BTreeMap<usize, Value>,
    text: String,
    usage: Option<TokenUsage>,
    response_id: Option<String>,
    completed: bool,
    streamed_text: bool,
}

impl CollectedResponse {
    fn push_item(
        &mut self,
        index: usize,
        item: Value,
        completed_items: &UnboundedSender<Value>,
    ) -> ApiResult<()> {
        if index < self.items.len() {
            if self.items[index] == item {
                return Ok(());
            }
            return Err(ApiError::fatal(format!(
                "model sent conflicting output items at index {index}"
            )));
        }
        if let Some(previous) = self.pending_items.insert(index, item.clone())
            && previous != item
        {
            return Err(ApiError::fatal(format!(
                "model sent conflicting pending output items at index {index}"
            )));
        }
        while let Some(item) = self.pending_items.remove(&self.items.len()) {
            let _ = completed_items.send(item.clone());
            self.items.push(item);
        }
        Ok(())
    }

    fn finish(self, events: Option<&UnboundedSender<AgentEvent>>) -> ApiResult<ModelResponse> {
        if !self.pending_items.is_empty() {
            return Err(ApiError::fatal(
                "model response completed with a gap in output item indexes",
            ));
        }
        let tool_calls = self
            .items
            .iter()
            .filter_map(ToolCall::from_response_item)
            .collect();
        let text = if self.text.is_empty() {
            text_from_items(&self.items)
        } else {
            self.text
        };
        if !self.streamed_text
            && !text.is_empty()
            && let Some(events) = events
        {
            let _ = events.send(AgentEvent::ModelTextDelta(text.clone()));
        }
        Ok(ModelResponse {
            items: self.items,
            tool_calls,
            text,
            usage: self.usage,
            server_reasoning_included: false,
            response_id: self
                .response_id
                .ok_or_else(|| ApiError::fatal("response.completed omitted the response ID"))?,
        })
    }
}

fn process_event(
    data: &str,
    collected: &mut CollectedResponse,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
) -> ApiResult<()> {
    if data == "[DONE]" {
        return Ok(());
    }
    let event: Value = serde_json::from_str(data)
        .map_err(|error| ApiError::fatal(format!("failed to decode SSE event: {error}")))?;
    process_event_value(event, collected, completed_items, events)
}

fn process_event_value(
    event: Value,
    collected: &mut CollectedResponse,
    completed_items: &UnboundedSender<Value>,
    events: Option<&UnboundedSender<AgentEvent>>,
) -> ApiResult<()> {
    validate_event_server_model(&event)?;
    match event.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                collected.text.push_str(delta);
                collected.streamed_text = true;
                if let Some(events) = events {
                    let _ = events.send(AgentEvent::ModelTextDelta(delta.to_string()));
                }
            }
        }
        Some("response.reasoning_summary_text.delta") => {
            if let Some(delta) = event.get("delta").and_then(Value::as_str)
                && let Some(events) = events
            {
                let _ = events.send(AgentEvent::ReasoningSummaryDelta(delta.to_string()));
            }
        }
        Some("response.reasoning_summary_part.added") => {
            if let Some(events) = events {
                let _ = events.send(AgentEvent::ReasoningSummarySectionStarted);
            }
        }
        Some("response.output_item.done") => {
            let item = event
                .get("item")
                .cloned()
                .ok_or_else(|| ApiError::fatal("output_item.done omitted its item"))?;
            let index = event
                .get("output_index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .unwrap_or_else(|| collected.items.len() + collected.pending_items.len());
            collected.push_item(index, item, completed_items)?;
            if let Some(events) = events {
                let _ = events.send(AgentEvent::ModelItemCompleted);
            }
        }
        Some("response.completed") => {
            let response = event
                .get("response")
                .ok_or_else(|| ApiError::fatal("response.completed omitted its response"))?;
            validate_completed_response(response)?;
            if let Some(output) = response.get("output").and_then(Value::as_array) {
                for (index, item) in output.iter().cloned().enumerate() {
                    collected.push_item(index, item, completed_items)?;
                }
            }
            collected.usage = response.get("usage").and_then(parse_usage);
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
        _ if explicit_cache_unsupported(message) => {
            ApiError::new(ApiErrorKind::CacheUnsupported, message)
        }
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
    if explicit_cache_unsupported(message) {
        ApiError::new(ApiErrorKind::CacheUnsupported, message)
    } else {
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
}

fn validate_completed_response(response: &Value) -> ApiResult<()> {
    if let Some(model) = response.get("model").and_then(Value::as_str) {
        validate_server_model(model)?;
    }
    if let Some(context) = response
        .pointer("/reasoning/context")
        .and_then(Value::as_str)
        && context != "all_turns"
    {
        return Err(ApiError::fatal(format!(
            "backend used reasoning.context `{context}`; BetterCodex requires `all_turns`"
        )));
    }
    Ok(())
}

fn validate_server_model_header(headers: &HeaderMap) -> ApiResult<()> {
    if let Some(model) = headers
        .get("openai-model")
        .and_then(|value| value.to_str().ok())
    {
        validate_server_model(model)?;
    }
    Ok(())
}

fn validate_event_server_model(event: &Value) -> ApiResult<()> {
    if let Some(model) = event.pointer("/response/model").and_then(Value::as_str) {
        validate_server_model(model)?;
    }
    let response_headers = event
        .pointer("/response/headers")
        .and_then(Value::as_object);
    let event_headers = event.get("headers").and_then(Value::as_object);
    if let Some(model) = response_headers
        .and_then(|headers| json_header_value(headers, &["openai-model", "x-openai-model"]))
        .or_else(|| {
            event_headers
                .and_then(|headers| json_header_value(headers, &["openai-model", "x-openai-model"]))
        })
    {
        validate_server_model(model)?;
    }
    Ok(())
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

fn validate_server_model(model: &str) -> ApiResult<()> {
    if model == MODEL {
        return Ok(());
    }
    Err(ApiError::fatal(format!(
        "backend returned model `{model}` for fixed BetterCodex model `{MODEL}`"
    )))
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

fn text_from_items(items: &[Value]) -> String {
    items
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("assistant")
        })
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
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

fn explicit_cache_unsupported(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    (message.contains("prompt_cache_options") || message.contains("prompt_cache_breakpoint"))
        && (message.contains("unknown")
            || message.contains("unsupported")
            || message.contains("invalid"))
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn bounded_error_body(mut response: reqwest::Response) -> String {
    let mut body = Vec::with_capacity(MAX_ERROR_BODY_BYTES);
    while body.len() < MAX_ERROR_BODY_BYTES {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) if body.is_empty() => return "unreadable response".to_string(),
            Err(_) => break,
        };
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    String::from_utf8_lossy(&body).chars().take(4_000).collect()
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
