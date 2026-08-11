//! Standalone Codex web search for the selected Responses model.
//!
//! The wire client and request types come from OpenAI Codex commit
//! `1669c2403f793d0230065397dfc25f52b844244e`. The focused adapter here mirrors
//! `codex-rs/ext/web-search`: it exposes `web.run` inside the exec runtime and
//! sends search, fetch/navigation, and image commands to the same `alpha/search`
//! endpoint used by Codex.

use crate::auth::SharedAuth;
use crate::http_client::backoff;
use crate::model::SharedModelSelection;
use crate::protocol::ContentItem;
use crate::protocol::ImageDetail;
use crate::protocol::InternalChatMessageMetadataPassthrough;
use crate::protocol::MessagePhase;
use crate::protocol::ResponseItem;
use crate::truncation::TruncationPolicy;
use crate::truncation::approx_bytes_for_tokens;
use crate::truncation::approx_token_count;
use crate::truncation::formatted_truncate_text;
use crate::truncation::truncate_text;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(crate) const JAVASCRIPT_NAME: &str = "web__run";
pub(crate) const NAMESPACE: &str = "web";
pub(crate) const TOOL_NAME: &str = "run";
pub(crate) const DESCRIPTION: &str = include_str!("tools/web_run_description.md");

const ASSISTANT_CONTEXT_TOKEN_LIMIT: usize = 1_000;
const SEARCH_PATH: &str = "alpha/search";
const REQUEST_MAX_RETRIES: u64 = 4;
const REQUEST_RETRY_DELAY: Duration = Duration::from_millis(200);

static INPUT_SCHEMA: LazyLock<Value> =
    LazyLock::new(
        || match serde_json::from_str(include_str!("web_search_schema.json")) {
            Ok(schema) => schema,
            Err(error) => panic!("invalid built-in web search schema: {error}"),
        },
    );

#[derive(Clone)]
pub(crate) struct WebSearchClient {
    client: reqwest::Client,
    auth: SharedAuth,
    base_url: String,
    session_id: String,
    model_selection: SharedModelSelection,
}

#[derive(Clone)]
pub(crate) struct ToolTurnContext {
    input: Option<Arc<SearchInput>>,
    turn_metadata: String,
    truncation_policy: TruncationPolicy,
}

impl Default for ToolTurnContext {
    fn default() -> Self {
        Self {
            input: None,
            turn_metadata: String::new(),
            truncation_policy: TruncationPolicy::Tokens(10_000),
        }
    }
}

#[derive(Debug, Serialize)]
struct SearchRequest<'a> {
    id: &'a str,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<&'a SearchInput>,
    commands: &'a SearchCommands,
    settings: SearchSettings,
    max_output_tokens: u64,
}

#[derive(Debug, Serialize)]
struct SearchSettings {
    allowed_callers: [&'static str; 1],
    external_web_access: bool,
}

impl<'a> SearchRequest<'a> {
    fn new(
        id: &'a str,
        model: &'a str,
        input: Option<&'a SearchInput>,
        commands: &'a SearchCommands,
        max_output_tokens: usize,
    ) -> Self {
        Self {
            id,
            model,
            input,
            commands,
            settings: SearchSettings {
                allowed_callers: ["direct"],
                external_web_access: true,
            },
            max_output_tokens: u64::try_from(max_output_tokens).unwrap_or(u64::MAX),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum SearchInput {
    Items(Vec<ResponseItem>),
}

/// Borrowed projection of a history message used to build standalone-search context.
///
/// Deserializing from `&Value` validates the same wire fields as `ResponseItem` without deep-
/// cloning payloads such as input images that the search endpoint never receives.
#[derive(Deserialize)]
struct HistoryMessage<'a> {
    #[serde(default, rename = "id")]
    _id: Option<&'a str>,
    role: &'a str,
    #[serde(borrow)]
    content: Vec<HistoryContent<'a>>,
    #[serde(default)]
    phase: Option<MessagePhase>,
    #[serde(default)]
    internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HistoryContent<'a> {
    InputText {
        text: &'a str,
    },
    InputImage {
        image_url: &'a str,
        #[serde(default)]
        detail: Option<ImageDetail>,
    },
    OutputText {
        text: &'a str,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct SearchCommands {
    /// Query the internet; at most 4 queries per call.
    #[serde(skip_serializing_if = "Option::is_none")]
    search_query: Option<Vec<SearchQuery>>,
    /// Query the image search engine for a given list of queries.
    #[serde(skip_serializing_if = "Option::is_none")]
    image_query: Option<Vec<SearchQuery>>,
    /// Open pages by reference id or URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<Vec<OpenOperation>>,
    /// Open links from previously opened pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    click: Option<Vec<ClickOperation>>,
    /// Find text patterns in pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    find: Option<Vec<FindOperation>>,
    /// Take screenshots of PDF pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot: Option<Vec<ScreenshotOperation>>,
    /// Response length; omit for `short`. Use `medium` or `long` when `search_query` contains 4
    /// queries.
    #[serde(skip_serializing_if = "Option::is_none")]
    response_length: Option<SearchResponseLength>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SearchQuery {
    /// Search query.
    q: String,
    /// Whether to filter by recency, as a number of recent days.
    #[serde(skip_serializing_if = "Option::is_none")]
    recency: Option<u64>,
    /// Whether to filter by a specific list of domains.
    #[serde(skip_serializing_if = "Option::is_none")]
    domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct OpenOperation {
    /// Reference id or URL to open.
    ref_id: String,
    /// Line number to position the page at.
    #[serde(skip_serializing_if = "Option::is_none")]
    lineno: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ClickOperation {
    /// Reference id containing the numbered link.
    ref_id: String,
    /// Numbered link id to open.
    id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FindOperation {
    /// Reference id or URL to search within.
    ref_id: String,
    /// Text pattern to find.
    pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ScreenshotOperation {
    /// Reference id or URL to screenshot.
    ref_id: String,
    /// Zero-indexed PDF page number.
    pageno: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum SearchResponseLength {
    Short,
    Medium,
    Long,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    output: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WebSearchAction {
    Search {
        query: Option<String>,
        queries: Option<Vec<String>>,
    },
    OpenPage {
        url: Option<String>,
    },
    FindInPage {
        url: Option<String>,
        pattern: Option<String>,
    },
    Other,
}

impl WebSearchClient {
    pub(crate) fn new(
        client: reqwest::Client,
        auth: SharedAuth,
        base_url: String,
        session_id: String,
        model_selection: SharedModelSelection,
    ) -> Self {
        Self {
            client,
            auth,
            base_url,
            session_id,
            model_selection,
        }
    }

    pub(crate) async fn run(
        &self,
        input: Option<Value>,
        context: &ToolTurnContext,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        if cancellation.is_cancelled() {
            return Err(anyhow!("web search cancelled"));
        }
        let commands = parse_commands(input)?;
        let model_selection = self.model_selection.get();
        let max_output_tokens = model_selection.truncation_policy().token_budget();
        let request = SearchRequest::new(
            &self.session_id,
            &model_selection.model,
            context.input.as_deref(),
            &commands,
            max_output_tokens,
        );
        let body = Bytes::from(
            serde_json::to_vec(&request)
                .context("failed to encode standalone web search request")?,
        );
        let mut auth = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(anyhow!("web search cancelled")),
            auth = self.auth.refreshed_snapshot(&self.client) => auth,
        }
        .context("failed to refresh ChatGPT credentials for web search")?;
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), SEARCH_PATH);
        let mut refreshed_after_unauthorized = false;
        let response = loop {
            let headers = search_headers(&context.turn_metadata, &auth)?;
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(anyhow!("web search cancelled")),
                response = send_search_request(&self.client, &url, &headers, body.clone()) => response,
            };
            match result {
                Err(SearchTransportError::Http { status, .. })
                    if status == StatusCode::UNAUTHORIZED && !refreshed_after_unauthorized =>
                {
                    auth = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Err(anyhow!("web search cancelled")),
                        refreshed = self.auth.force_refreshed_snapshot(&self.client) => refreshed,
                    }
                    .context(
                        "web search authentication was rejected and ChatGPT credential refresh failed",
                    )?;
                    refreshed_after_unauthorized = true;
                }
                result => break result,
            }
        }
        .map_err(|error| anyhow!("standalone web search request failed: {error}"))?;
        let response: SearchResponse = serde_json::from_slice(&response)
            .context("failed to decode standalone web search response")?;
        Ok(Value::String(bounded_search_output(
            response.output,
            max_output_tokens,
        )))
    }
}

fn bounded_search_output(output: String, max_output_tokens: usize) -> String {
    if output.len() <= approx_bytes_for_tokens(max_output_tokens) {
        output
    } else {
        formatted_truncate_text(&output, max_output_tokens)
    }
}

impl ToolTurnContext {
    pub(crate) fn from_history(
        history: &[Value],
        turn_metadata: String,
        truncation_policy: TruncationPolicy,
    ) -> Self {
        Self {
            input: recent_input(history).map(Arc::new),
            turn_metadata,
            truncation_policy,
        }
    }

    pub(crate) fn truncation_policy(&self) -> TruncationPolicy {
        self.truncation_policy
    }
}

pub(crate) fn input_schema() -> &'static Value {
    &INPUT_SCHEMA
}

pub(crate) fn action_for_display(input: Option<Value>) -> WebSearchAction {
    parse_commands(input)
        .map(|commands| command_action(&commands))
        .unwrap_or(WebSearchAction::Other)
}

fn parse_commands(input: Option<Value>) -> Result<SearchCommands> {
    match input {
        None => Ok(SearchCommands::default()),
        Some(Value::Object(arguments)) => serde_json::from_value(Value::Object(arguments))
            .map_err(|error| anyhow!("failed to parse web.run arguments: {error}")),
        Some(_) => Err(anyhow!(
            "tool `web.run` expects a JSON object for arguments"
        )),
    }
}

fn command_action(commands: &SearchCommands) -> WebSearchAction {
    commands
        .search_query
        .as_deref()
        .and_then(query_action)
        .or_else(|| commands.image_query.as_deref().and_then(query_action))
        .or_else(|| {
            commands
                .open
                .as_deref()
                .and_then(|operations| operations.first())
                .and_then(|operation| {
                    literal_url(&operation.ref_id)
                        .map(|url| WebSearchAction::OpenPage { url: Some(url) })
                })
        })
        .or_else(|| {
            commands
                .find
                .as_deref()
                .and_then(|operations| operations.first())
                .map(|operation| WebSearchAction::FindInPage {
                    url: literal_url(&operation.ref_id),
                    pattern: Some(operation.pattern.clone()),
                })
        })
        .unwrap_or(WebSearchAction::Other)
}

fn query_action(queries: &[SearchQuery]) -> Option<WebSearchAction> {
    match queries {
        [] => None,
        [query] => Some(WebSearchAction::Search {
            query: Some(query.q.clone()),
            queries: None,
        }),
        queries => Some(WebSearchAction::Search {
            query: None,
            queries: Some(queries.iter().map(|query| query.q.clone()).collect()),
        }),
    }
}

fn literal_url(ref_id: &str) -> Option<String> {
    reqwest::Url::parse(ref_id)
        .is_ok()
        .then(|| ref_id.to_string())
}

fn search_headers(turn_metadata: &str, auth: &crate::auth::AuthSnapshot) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", auth.authorization.clone());
    if let Some(account_id) = &auth.account_id {
        headers.insert("chatgpt-account-id", account_id.clone());
    }
    headers.insert(
        "version",
        HeaderValue::from_str(env!("CARGO_PKG_VERSION"))
            .context("bettercodex package version is not a valid HTTP header")?,
    );
    if !turn_metadata.is_empty() {
        headers.insert(
            "x-codex-turn-metadata",
            HeaderValue::from_str(turn_metadata)
                .context("x-codex-turn-metadata is not a valid HTTP header")?,
        );
    }
    Ok(headers)
}

#[derive(Debug)]
enum SearchTransportError {
    Http {
        status: StatusCode,
        body: Option<String>,
    },
    Timeout,
    Network(String),
}

impl SearchTransportError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Http { status, .. } => status.is_server_error(),
            Self::Timeout | Self::Network(_) => true,
        }
    }
}

impl fmt::Display for SearchTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, body } => write!(formatter, "http {status}: {body:?}"),
            Self::Timeout => formatter.write_str("timeout"),
            Self::Network(error) => write!(formatter, "network error: {error}"),
        }
    }
}

impl std::error::Error for SearchTransportError {}

async fn send_search_request(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> std::result::Result<Bytes, SearchTransportError> {
    for attempt in 0..=REQUEST_MAX_RETRIES {
        let response = client
            .post(url)
            .headers(headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(body.clone())
            .send()
            .await;
        let result = match response {
            Ok(response) => {
                let status = response.status();
                let bytes = response.bytes().await.map_err(map_search_reqwest_error)?;
                if status.is_success() {
                    Ok(bytes)
                } else {
                    Err(SearchTransportError::Http {
                        status,
                        body: String::from_utf8(bytes.to_vec()).ok(),
                    })
                }
            }
            Err(error) => Err(map_search_reqwest_error(error)),
        };
        match result {
            Ok(response) => return Ok(response),
            Err(error) if attempt < REQUEST_MAX_RETRIES && error.is_retryable() => {
                tokio::time::sleep(backoff(REQUEST_RETRY_DELAY, attempt + 1)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the bounded retry loop always returns")
}

fn map_search_reqwest_error(error: reqwest::Error) -> SearchTransportError {
    if error.is_timeout() {
        SearchTransportError::Timeout
    } else {
        SearchTransportError::Network(error.to_string())
    }
}

fn recent_input(history: &[Value]) -> Option<SearchInput> {
    let mut messages = Vec::new();
    let mut user_messages = 0;
    for value in history.iter().rev() {
        let Some(role) = value
            .get("type")
            .and_then(Value::as_str)
            .filter(|kind| *kind == "message")
            .and_then(|_| value.get("role"))
            .and_then(Value::as_str)
            .filter(|role| matches!(*role, "assistant" | "user"))
        else {
            continue;
        };
        // Search context ends at the newest operator input. Avoid even walking the assistant
        // response that triggered the current tool call, because it is discarded by contract.
        if role == "assistant" && user_messages == 0 {
            continue;
        }
        let Ok(message) = HistoryMessage::deserialize(value) else {
            continue;
        };
        if role == "user" {
            if !message.is_operator_message() {
                continue;
            }
            user_messages += 1;
            messages.push(message);
            if user_messages == 2 {
                break;
            }
        } else if user_messages > 0 {
            messages.push(message);
        }
    }
    // With only one operator message, reverse traversal can encounter assistant messages that
    // predate it. They are outside the selected turn just as trailing assistant output is.
    if user_messages == 1 {
        messages.truncate(1);
    }
    messages.reverse();
    let mut assistant_tokens = ASSISTANT_CONTEXT_TOKEN_LIMIT;
    let messages = messages
        .into_iter()
        .filter_map(|message| message.into_search_item(&mut assistant_tokens))
        .collect::<Vec<_>>();
    (!messages.is_empty()).then_some(SearchInput::Items(messages))
}

impl HistoryMessage<'_> {
    fn is_operator_message(&self) -> bool {
        let mut text = self.content.iter().filter_map(|item| match item {
            HistoryContent::InputText { text } => Some(*text),
            _ => None,
        });
        let Some(first) = text.next() else {
            return false;
        };
        let Some(second) = text.next() else {
            return !crate::context::is_contextual_user_text(first);
        };
        let mut joined =
            String::with_capacity(first.len().saturating_add(second.len()).saturating_add(1));
        joined.push_str(first);
        joined.push('\n');
        joined.push_str(second);
        for part in text {
            joined.push('\n');
            joined.push_str(part);
        }
        !crate::context::is_contextual_user_text(&joined)
    }

    fn into_search_item(self, assistant_tokens: &mut usize) -> Option<ResponseItem> {
        let is_user = self.role == "user";
        let content =
            self.content
                .into_iter()
                .filter_map(|item| {
                    if is_user {
                        return match item {
                            HistoryContent::InputText { text } => Some(ContentItem::InputText {
                                text: text.to_string(),
                            }),
                            HistoryContent::InputImage { .. }
                            | HistoryContent::OutputText { .. } => None,
                        };
                    }
                    match item {
                        HistoryContent::InputText { text } => Some(ContentItem::InputText {
                            text: text.to_string(),
                        }),
                        HistoryContent::InputImage { image_url, detail } => {
                            Some(ContentItem::InputImage {
                                image_url: image_url.to_string(),
                                detail,
                            })
                        }
                        HistoryContent::OutputText { .. } if *assistant_tokens == 0 => None,
                        HistoryContent::OutputText { text } => {
                            let tokens = approx_token_count(text);
                            let text = if tokens <= *assistant_tokens {
                                *assistant_tokens = assistant_tokens.saturating_sub(tokens);
                                text.to_string()
                            } else {
                                let text = truncate_text(text, *assistant_tokens);
                                *assistant_tokens = 0;
                                text
                            };
                            Some(ContentItem::OutputText { text })
                        }
                    }
                })
                .collect::<Vec<_>>();
        (!content.is_empty()).then_some(ResponseItem::Message {
            id: None,
            role: self.role.to_string(),
            content,
            phase: self.phase,
            internal_chat_message_metadata_passthrough: self
                .internal_chat_message_metadata_passthrough,
        })
    }
}
