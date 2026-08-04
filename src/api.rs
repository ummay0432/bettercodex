use crate::MODEL;
use crate::auth::Auth;
use crate::events::AgentEvent;
use crate::rollout::SessionIdentity;
use crate::tools;
use crate::tools::ToolCall;
use crate::usage::TokenUsage;
use anyhow::Context;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt;
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

const BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");
const MAX_HTTP_ATTEMPTS: usize = 3;
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_STREAM_EVENT_BYTES: usize = 2 * 1024 * 1024;
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";

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
    auth: Auth,
    base_url: String,
    installation_id: String,
    session_id: String,
    thread_id: String,
    turn_id: String,
    turn_state: Option<String>,
    window: u64,
    explicit_cache: bool,
    prefer_websocket: bool,
    websocket: Option<WebSocketConnection>,
    websocket_baseline: Option<WebSocketBaseline>,
}

#[derive(Clone)]
struct WebSocketBaseline {
    request: Value,
    response_id: String,
    output: Vec<Value>,
}

pub(crate) struct ModelResponse {
    pub(crate) items: Vec<Value>,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) text: String,
    pub(crate) usage: Option<TokenUsage>,
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

    fn new_with_base_url(
        auth: Auth,
        identity: &SessionIdentity,
        compaction_count: u64,
        base_url: String,
    ) -> anyhow::Result<Self> {
        codex_utils_rustls_provider::ensure_rustls_crypto_provider();
        let mut default_headers = HeaderMap::new();
        default_headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));
        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .user_agent(concat!("bettercodex/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .context("failed to create HTTP client")?;
        Ok(Self {
            client,
            auth,
            base_url,
            installation_id: identity.installation_id.clone(),
            session_id: identity.session_id.clone(),
            thread_id: identity.thread_id.clone(),
            turn_id: Uuid::new_v4().to_string(),
            turn_state: None,
            window: compaction_count,
            explicit_cache: true,
            prefer_websocket: true,
            websocket: None,
            websocket_baseline: None,
        })
    }

    pub(crate) fn begin_turn(&mut self) -> &str {
        self.turn_id = Uuid::new_v4().to_string();
        self.turn_state = None;
        &self.turn_id
    }

    pub(crate) fn abandon_response(&mut self) {
        self.websocket = None;
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
        history: &[Value],
        completed_items: &UnboundedSender<Value>,
    ) -> ApiResult<ModelResponse> {
        self.respond_with_events(history, completed_items, None)
            .await
    }

    pub(crate) async fn respond_streaming(
        &mut self,
        history: &[Value],
        completed_items: &UnboundedSender<Value>,
        events: &UnboundedSender<AgentEvent>,
    ) -> ApiResult<ModelResponse> {
        self.respond_with_events(history, completed_items, Some(events))
            .await
    }

    async fn respond_with_events(
        &mut self,
        history: &[Value],
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> ApiResult<ModelResponse> {
        let mut refreshed_websocket_auth = false;
        let mut retried_full_websocket_request = false;
        loop {
            let request = self.build_request(history);
            if self.prefer_websocket {
                match self
                    .respond_websocket(&request, completed_items, events)
                    .await
                {
                    Ok(response) => return Ok(response),
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

            let request = self.build_request(history);
            match self.respond_http(&request, completed_items, events).await {
                Err(error) if error.kind == ApiErrorKind::CacheUnsupported => {
                    if self.explicit_cache {
                        self.disable_explicit_cache();
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
    ) -> ApiResult<ModelResponse> {
        let response = self
            .post("responses", request, "text/event-stream", RequestKind::Turn)
            .await?;
        self.capture_turn_state(response.headers());
        collect_http_stream(response, completed_items, events).await
    }

    async fn respond_websocket(
        &mut self,
        logical_request: &Value,
        completed_items: &UnboundedSender<Value>,
        events: Option<&UnboundedSender<AgentEvent>>,
    ) -> ApiResult<ModelResponse> {
        self.ensure_websocket().await?;
        let wire_request = self.websocket_request(logical_request)?;
        self.websocket
            .as_mut()
            .ok_or_else(|| ApiError::websocket_unavailable("Responses WebSocket is unavailable"))?
            .send(&wire_request, STREAM_IDLE_TIMEOUT)
            .await?;

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
        let response = collected.finish(events)?;
        self.websocket_baseline = Some(WebSocketBaseline {
            request: logical_request.clone(),
            response_id: response.response_id.clone(),
            output: response.items.clone(),
        });
        Ok(response)
    }

    async fn ensure_websocket(&mut self) -> ApiResult<()> {
        if self.websocket.is_some() {
            return Ok(());
        }
        self.auth
            .refresh_if_needed(&self.client)
            .await
            .map_err(|error| ApiError::fatal(format!("{error:#}")))?;
        let mut headers = self.request_headers("*/*", RequestKind::Turn)?;
        insert_header(&mut headers, "originator", "codex_cli_rs")?;
        insert_header(&mut headers, "openai-beta", RESPONSES_WEBSOCKET_BETA)?;
        let url = websocket_url(&self.base_url, "responses")?;
        let (websocket, response_headers) = WebSocketConnection::connect(&url, &headers).await?;
        self.capture_turn_state(&response_headers);
        self.websocket = Some(websocket);
        Ok(())
    }

    fn websocket_request(&self, request: &Value) -> ApiResult<Value> {
        let mut wire = request.clone();
        let object = wire
            .as_object_mut()
            .ok_or_else(|| ApiError::fatal("Responses request was not an object"))?;
        object.insert(
            "type".to_string(),
            Value::String("response.create".to_string()),
        );

        let Some(baseline) = &self.websocket_baseline else {
            return Ok(wire);
        };
        if !request_properties_match(&baseline.request, request) {
            return Ok(wire);
        }
        let previous_input = baseline
            .request
            .get("input")
            .and_then(Value::as_array)
            .ok_or_else(|| ApiError::fatal("previous Responses request omitted input"))?;
        let current_input = request
            .get("input")
            .and_then(Value::as_array)
            .ok_or_else(|| ApiError::fatal("Responses request omitted input"))?;
        let baseline_length = previous_input.len().saturating_add(baseline.output.len());
        if current_input.len() <= baseline_length {
            return Ok(wire);
        }
        let expected = previous_input.iter().chain(&baseline.output);
        if !expected
            .zip(current_input.iter().take(baseline_length))
            .all(|(previous, current)| previous == current)
        {
            return Ok(wire);
        }
        object.insert(
            "previous_response_id".to_string(),
            Value::String(baseline.response_id.clone()),
        );
        object.insert(
            "input".to_string(),
            Value::Array(current_input[baseline_length..].to_vec()),
        );
        Ok(wire)
    }

    pub(crate) async fn compact(&mut self, history: &[Value]) -> ApiResult<CompactionResult> {
        if history.is_empty() {
            return Ok(CompactionResult {
                items: Vec::new(),
                usage: None,
            });
        }
        let body = json!({
            "model": MODEL,
            "input": compose_input(history, false),
            "instructions": "",
            "parallel_tool_calls": false,
            "reasoning": {"effort": "max", "context": "all_turns"},
            "text": {"verbosity": "low"},
        });
        let response = self
            .post(
                "responses/compact",
                &body,
                "application/json",
                RequestKind::Compaction,
            )
            .await?;
        self.capture_turn_state(response.headers());
        let payload: Value = response.json().await.map_err(|error| {
            ApiError::fatal(format!("failed to decode compacted conversation: {error}"))
        })?;
        let output = payload
            .get("output")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| ApiError::fatal("compaction response did not contain output items"))?;
        let compaction_items = output
            .iter()
            .filter(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("compaction" | "compaction_summary")
                )
            })
            .count();
        if compaction_items != 1 {
            return Err(ApiError::fatal(format!(
                "compaction response contained {compaction_items} compaction items; expected exactly one"
            )));
        }
        let usage = payload.get("usage").and_then(parse_usage);
        self.window = self.window.saturating_add(1);
        self.websocket_baseline = None;
        Ok(CompactionResult {
            items: output,
            usage,
        })
    }

    fn build_request(&self, history: &[Value]) -> Value {
        let mut request = json!({
            "model": MODEL,
            "instructions": "",
            "input": compose_input(history, self.explicit_cache),
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "reasoning": {"effort": "max", "context": "all_turns"},
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": self.session_id,
            "text": {"verbosity": "low"},
            "client_metadata": self.client_metadata(),
        });
        if self.explicit_cache {
            request["prompt_cache_options"] = json!({"mode": "explicit", "ttl": "30m"});
        }
        request
    }

    fn disable_explicit_cache(&mut self) {
        self.explicit_cache = false;
        self.abandon_response();
    }

    fn client_metadata(&self) -> Map<String, Value> {
        let turn_metadata = self.turn_metadata(RequestKind::Turn).to_string();
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
        json!({
            "installation_id": self.installation_id,
            "session_id": self.session_id,
            "thread_id": self.thread_id,
            "turn_id": self.turn_id,
            "window_id": self.window_id(),
            "request_kind": request_kind.as_str(),
            "sandbox": "danger-full-access",
            "code_mode_tool_names": tools::code_mode_tool_names(),
            "turn_started_at_unix_ms": unix_timestamp_millis(),
        })
    }

    fn window_id(&self) -> String {
        format!("{}:{}", self.thread_id, self.window)
    }

    fn request_headers(&self, accept: &str, request_kind: RequestKind) -> ApiResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        insert_header(&mut headers, "accept", accept)?;
        insert_header(
            &mut headers,
            "authorization",
            &format!("Bearer {}", self.auth.access_token()),
        )?;
        if let Some(account_id) = self.auth.account_id() {
            insert_header(&mut headers, "chatgpt-account-id", account_id)?;
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
        insert_header(
            &mut headers,
            "x-openai-internal-codex-responses-lite",
            "true",
        )?;
        if let Some(turn_state) = &self.turn_state {
            insert_header(&mut headers, "x-codex-turn-state", turn_state)?;
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
        self.auth
            .refresh_if_needed(&self.client)
            .await
            .map_err(|error| ApiError::fatal(format!("{error:#}")))?;
        let url = format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut refreshed_after_unauthorized = false;
        let mut last_error = None;
        for attempt in 0..MAX_HTTP_ATTEMPTS {
            let response = self
                .client
                .post(&url)
                .headers(self.request_headers(accept, request_kind)?)
                .json(body)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) if attempt + 1 < MAX_HTTP_ATTEMPTS => {
                    last_error = Some(error.to_string());
                    sleep(retry_delay(attempt)).await;
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
                refreshed_after_unauthorized = true;
                continue;
            }
            let retry_after = parse_retry_after(response.headers());
            let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            if retryable && attempt + 1 < MAX_HTTP_ATTEMPTS {
                sleep(
                    retry_after
                        .unwrap_or_else(|| retry_delay(attempt))
                        .min(Duration::from_secs(30)),
                )
                .await;
                continue;
            }
            let body = bounded_error_body(response).await;
            let message = format!("Responses request failed with {status}: {body}");
            if explicit_cache_unsupported(&body) {
                return Err(ApiError::new(ApiErrorKind::CacheUnsupported, message));
            }
            if retryable {
                return Err(ApiError::retryable_after(message, retry_after));
            }
            return Err(ApiError::fatal(message));
        }
        Err(ApiError::retryable(last_error.unwrap_or_else(|| {
            "Responses request exhausted its retries".to_string()
        })))
    }

    fn capture_turn_state(&mut self, headers: &HeaderMap) {
        if let Some(value) = headers
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok())
        {
            self.turn_state = Some(value.to_string());
        }
    }

    fn capture_event_turn_state(&mut self, event: &Value) {
        let headers = event.get("headers").and_then(Value::as_object);
        if let Some(value) = headers
            .and_then(|headers| headers.get("x-codex-turn-state"))
            .and_then(Value::as_str)
        {
            self.turn_state = Some(value.to_string());
        }
    }
}

#[derive(Clone, Copy)]
enum RequestKind {
    Turn,
    Compaction,
}

impl RequestKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Compaction => "compaction",
        }
    }
}

fn compose_input(history: &[Value], explicit_cache: bool) -> Vec<Value> {
    let tools_item = json!({
        "type": "additional_tools",
        "role": "developer",
        "tools": tools::specifications(),
    });
    let mut input = history.to_vec();
    let tools_position = input
        .iter()
        .position(|item| is_additional_tools_item(item, &tools_item));
    if tools_position.is_none() {
        input.insert(0, tools_item);
    }

    let system_position = input.iter().position(is_system_prompt_item);
    let system_item = system_prompt_item(explicit_cache);
    match system_position {
        Some(index) if explicit_cache => mark_cache_breakpoint(&mut input[index]),
        Some(_) => {}
        None => input.insert(1, system_item),
    }
    input
}

fn is_additional_tools_item(item: &Value, expected: &Value) -> bool {
    ["type", "role", "tools"]
        .into_iter()
        .all(|field| item.get(field) == expected.get(field))
}

fn system_prompt_item(explicit_cache: bool) -> Value {
    let mut item = json!({
        "type": "message",
        "role": "developer",
        "content": [{"type": "input_text", "text": SYSTEM_PROMPT.trim()}],
    });
    if explicit_cache {
        mark_cache_breakpoint(&mut item);
    }
    item
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

fn request_properties_match(previous: &Value, current: &Value) -> bool {
    let mut previous = previous.clone();
    let mut current = current.clone();
    for request in [&mut previous, &mut current] {
        if let Some(object) = request.as_object_mut() {
            object.remove("input");
            object.remove("client_metadata");
            object.remove("stream_options");
        }
    }
    previous == current
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
                let _ = events.send(AgentEvent::ReasoningDelta(delta.to_string()));
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
    } else if code.contains("rate_limit") || code.contains("server") || code.contains("overload") {
        ApiError::retryable(message)
    } else {
        ApiError::fatal(message)
    }
}

fn validate_completed_response(response: &Value) -> ApiResult<()> {
    if let Some(model) = response.get("model").and_then(Value::as_str)
        && model != MODEL
    {
        return Err(ApiError::fatal(format!(
            "backend returned model `{model}` for fixed BetterCodex model `{MODEL}`"
        )));
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

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    data_lines: Vec<String>,
    data_bytes: usize,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> ApiResult<Vec<String>> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > MAX_STREAM_EVENT_BYTES {
            return Err(ApiError::fatal("model sent an oversized SSE event"));
        }
        let mut events = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(&line, &mut events)?;
        }
        Ok(events)
    }

    fn finish(&mut self) -> ApiResult<Vec<String>> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.process_line(&line, &mut events)?;
        }
        if !self.data_lines.is_empty() {
            events.push(self.take_event());
        }
        Ok(events)
    }

    fn process_line(&mut self, line: &[u8], events: &mut Vec<String>) -> ApiResult<()> {
        let line =
            std::str::from_utf8(line).map_err(|_| ApiError::fatal("SSE stream was not UTF-8"))?;
        if line.is_empty() {
            if !self.data_lines.is_empty() {
                events.push(self.take_event());
            }
        } else if let Some(data) = line.strip_prefix("data:") {
            let data = data.strip_prefix(' ').unwrap_or(data);
            self.data_bytes = self
                .data_bytes
                .checked_add(data.len() + usize::from(!self.data_lines.is_empty()))
                .filter(|bytes| *bytes <= MAX_STREAM_EVENT_BYTES)
                .ok_or_else(|| ApiError::fatal("model sent an oversized SSE event"))?;
            self.data_lines.push(data.to_string());
        }
        Ok(())
    }

    fn take_event(&mut self) -> String {
        self.data_bytes = 0;
        std::mem::take(&mut self.data_lines).join("\n")
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

fn retry_delay(attempt: usize) -> Duration {
    Duration::from_secs(1_u64 << attempt.min(4))
}

fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn explicit_cache_unsupported(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    (message.contains("prompt_cache_options") || message.contains("prompt_cache_breakpoint"))
        && (message.contains("unknown")
            || message.contains("unsupported")
            || message.contains("invalid"))
}

fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

async fn bounded_error_body(response: reqwest::Response) -> String {
    response
        .text()
        .await
        .unwrap_or_else(|_| "unreadable response".to_string())
        .chars()
        .take(4_000)
        .collect()
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
