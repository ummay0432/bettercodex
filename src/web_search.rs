//! Standalone Codex web search for Responses Lite.
//!
//! The wire client and request types come from OpenAI Codex commit
//! `1669c2403f793d0230065397dfc25f52b844244e`. The focused adapter here mirrors
//! `codex-rs/ext/web-search`: it exposes `web.run` inside the exec runtime and
//! sends search, fetch/navigation, image, finance, weather, sports, and time
//! commands to the same `alpha/search` endpoint used by Codex.

use crate::MODEL;
use crate::auth::SharedAuth;
use crate::http_client::backoff;
use crate::protocol::ContentItem;
use crate::protocol::ImageDetail;
use crate::protocol::InternalChatMessageMetadataPassthrough;
use crate::protocol::MessagePhase;
use crate::protocol::ResponseItem;
use crate::truncation::TruncationPolicy;
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
use schemars::JsonSchema;
use schemars::r#gen::SchemaSettings;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
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
const MAX_OUTPUT_TOKENS: usize = 10_000;
const SEARCH_PATH: &str = "alpha/search";
const REQUEST_MAX_RETRIES: u64 = 4;
const REQUEST_RETRY_DELAY: Duration = Duration::from_millis(200);

static INPUT_SCHEMA: LazyLock<Value> = LazyLock::new(commands_schema);

#[derive(Clone)]
pub(crate) struct WebSearchClient {
    client: reqwest::Client,
    auth: SharedAuth,
    base_url: String,
    session_id: String,
}

#[derive(Clone, Default)]
pub(crate) struct ToolTurnContext {
    input: Option<Arc<SearchInput>>,
    turn_metadata: String,
}

#[derive(Debug, Serialize)]
struct SearchRequest<'a> {
    id: &'a str,
    model: &'static str,
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
    fn new(id: &'a str, input: Option<&'a SearchInput>, commands: &'a SearchCommands) -> Self {
        Self {
            id,
            model: MODEL,
            input,
            commands,
            settings: SearchSettings {
                allowed_callers: ["direct"],
                external_web_access: true,
            },
            max_output_tokens: u64::try_from(MAX_OUTPUT_TOKENS).unwrap_or(u64::MAX),
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
    InputAudio {
        audio_url: &'a str,
    },
    OutputText {
        text: &'a str,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, JsonSchema)]
struct SearchCommands {
    /// Query the internet search engine for a given list of queries.
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
    /// Look up prices for the given stock symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    finance: Option<Vec<FinanceOperation>>,
    /// Look up weather forecasts.
    #[serde(skip_serializing_if = "Option::is_none")]
    weather: Option<Vec<WeatherOperation>>,
    /// Look up sports schedules and standings.
    #[serde(skip_serializing_if = "Option::is_none")]
    sports: Option<Vec<SportsOperation>>,
    /// Get time for the given UTC offsets.
    #[serde(skip_serializing_if = "Option::is_none")]
    time: Option<Vec<TimeOperation>>,
    /// Set the length of the response to be returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    response_length: Option<SearchResponseLength>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
struct OpenOperation {
    /// Reference id or URL to open.
    ref_id: String,
    /// Line number to position the page at.
    #[serde(skip_serializing_if = "Option::is_none")]
    lineno: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
struct ClickOperation {
    /// Reference id containing the numbered link.
    ref_id: String,
    /// Numbered link id to open.
    id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
struct FindOperation {
    /// Reference id or URL to search within.
    ref_id: String,
    /// Text pattern to find.
    pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
struct ScreenshotOperation {
    /// Reference id or URL to screenshot.
    ref_id: String,
    /// Zero-indexed PDF page number.
    pageno: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
struct FinanceOperation {
    /// Ticker symbol to look up.
    ticker: String,
    /// Asset type to look up.
    r#type: FinanceAssetType,
    /// ISO 3166-1 alpha-3 country code, "OTC", or "" for cryptocurrency.
    #[serde(skip_serializing_if = "Option::is_none")]
    market: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum FinanceAssetType {
    Equity,
    Fund,
    Crypto,
    Index,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
struct WeatherOperation {
    /// Location in "Country, Area, City" format.
    location: String,
    /// Start date in YYYY-MM-DD format. Defaults to today.
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<String>,
    /// Number of days to return. Defaults to 7.
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
struct SportsOperation {
    /// Tool name for sports requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<SportsToolName>,
    /// Sports function to call.
    r#fn: SportsFunction,
    /// League to look up.
    league: SportsLeague,
    /// Team to look up, using the common 3 or 4 letter alias used in broadcasts.
    #[serde(skip_serializing_if = "Option::is_none")]
    team: Option<String>,
    /// Opponent to use with `team` when narrowing the lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    opponent: Option<String>,
    /// Start date in YYYY-MM-DD format.
    #[serde(skip_serializing_if = "Option::is_none")]
    date_from: Option<String>,
    /// End date in YYYY-MM-DD format.
    #[serde(skip_serializing_if = "Option::is_none")]
    date_to: Option<String>,
    /// Number of games to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    num_games: Option<u64>,
    /// Locale for the lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    locale: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum SportsToolName {
    Sports,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum SportsFunction {
    Schedule,
    Standings,
}

impl SportsFunction {
    fn display_name(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Standings => "standings",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum SportsLeague {
    Nba,
    Wnba,
    Nfl,
    Nhl,
    Mlb,
    Epl,
    Ncaamb,
    Ncaawb,
    Ipl,
}

impl SportsLeague {
    fn display_name(self) -> &'static str {
        match self {
            Self::Nba => "NBA",
            Self::Wnba => "WNBA",
            Self::Nfl => "NFL",
            Self::Nhl => "NHL",
            Self::Mlb => "MLB",
            Self::Epl => "EPL",
            Self::Ncaamb => "NCAAMB",
            Self::Ncaawb => "NCAAWB",
            Self::Ipl => "IPL",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
struct TimeOperation {
    /// UTC offset formatted like "+03:00".
    utc_offset: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
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

/// One concise row in the TUI's grouped web-activity tree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WebActivity {
    pub(crate) verb: &'static str,
    pub(crate) detail: String,
}

impl WebActivity {
    fn new(verb: &'static str, detail: impl Into<String>) -> Self {
        Self {
            verb,
            detail: detail.into(),
        }
    }
}

impl WebSearchClient {
    pub(crate) fn new(
        client: reqwest::Client,
        auth: SharedAuth,
        base_url: String,
        session_id: String,
    ) -> Self {
        Self {
            client,
            auth,
            base_url,
            session_id,
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
        let request = SearchRequest::new(&self.session_id, context.input.as_deref(), &commands);
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
        Ok(Value::String(bounded_search_output(response.output)))
    }
}

fn bounded_search_output(output: String) -> String {
    let policy = TruncationPolicy::Tokens(MAX_OUTPUT_TOKENS);
    if output.len() <= policy.byte_budget() {
        output
    } else {
        formatted_truncate_text(&output, policy)
    }
}

impl ToolTurnContext {
    pub(crate) fn from_history(history: &[Value], turn_metadata: String) -> Self {
        Self {
            input: recent_input(history).map(Arc::new),
            turn_metadata,
        }
    }
}

pub(crate) fn input_schema() -> &'static Value {
    &INPUT_SCHEMA
}

pub(crate) fn activities_for_display(input: Option<Value>) -> Vec<WebActivity> {
    let Ok(commands) = parse_commands(input) else {
        return vec![WebActivity::new("Browse", String::new())];
    };
    let SearchCommands {
        search_query,
        image_query,
        open,
        click,
        find,
        screenshot,
        finance,
        weather,
        sports,
        time,
        response_length: _,
    } = commands;
    let mut activities = Vec::new();

    if let Some(queries) = search_query {
        activities.extend(
            queries
                .into_iter()
                .map(|query| WebActivity::new("Search", query.q)),
        );
    }
    if let Some(queries) = image_query {
        activities.extend(
            queries
                .into_iter()
                .map(|query| WebActivity::new("Image search", query.q)),
        );
    }
    if let Some(operations) = open {
        activities.extend(operations.into_iter().map(|operation| {
            let detail = match operation.lineno {
                Some(line) => format!("{} at line {line}", operation.ref_id),
                None => operation.ref_id,
            };
            WebActivity::new("Open", detail)
        }));
    }
    if let Some(operations) = click {
        activities.extend(operations.into_iter().map(|operation| {
            WebActivity::new(
                "Open",
                format!("link {} in {}", operation.id, operation.ref_id),
            )
        }));
    }
    if let Some(operations) = find {
        activities.extend(operations.into_iter().map(|operation| {
            WebActivity::new(
                "Find",
                format!("'{}' in {}", operation.pattern, operation.ref_id),
            )
        }));
    }
    if let Some(operations) = screenshot {
        activities.extend(operations.into_iter().map(|operation| {
            WebActivity::new(
                "Screenshot",
                format!("page index {} of {}", operation.pageno, operation.ref_id),
            )
        }));
    }
    if let Some(operations) = finance {
        activities.extend(operations.into_iter().map(|operation| {
            let ticker = operation.ticker;
            let detail = match operation.market.filter(|market| !market.is_empty()) {
                Some(market) => format!("{ticker} ({market})"),
                None => ticker,
            };
            WebActivity::new("Finance", detail)
        }));
    }
    if let Some(operations) = weather {
        activities.extend(operations.into_iter().map(|operation| {
            let mut detail = operation.location;
            if let Some(start) = operation.start {
                detail.push_str(&format!(" from {start}"));
            }
            if let Some(duration) = operation.duration {
                let plural = if duration == 1 { "" } else { "s" };
                detail.push_str(&format!(" for {duration} day{plural}"));
            }
            WebActivity::new("Weather", detail)
        }));
    }
    if let Some(operations) = sports {
        activities.extend(operations.into_iter().map(|operation| {
            let mut detail = format!(
                "{} {}",
                operation.league.display_name(),
                operation.r#fn.display_name()
            );
            if let Some(team) = operation.team {
                detail.push_str(&format!(" for {team}"));
            }
            if let Some(opponent) = operation.opponent {
                detail.push_str(&format!(" vs {opponent}"));
            }
            match (operation.date_from, operation.date_to) {
                (Some(from), Some(to)) => detail.push_str(&format!(" from {from} to {to}")),
                (Some(from), None) => detail.push_str(&format!(" from {from}")),
                (None, Some(to)) => detail.push_str(&format!(" through {to}")),
                (None, None) => {}
            }
            WebActivity::new("Sports", detail)
        }));
    }
    if let Some(operations) = time {
        activities.extend(
            operations
                .into_iter()
                .map(|operation| WebActivity::new("Time", operation.utc_offset)),
        );
    }

    if activities.is_empty() {
        activities.push(WebActivity::new("Browse", String::new()));
    }
    activities
}

fn commands_schema() -> Value {
    let schema = SchemaSettings::draft2019_09()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.option_add_null_type = false;
        })
        .into_generator()
        .into_root_schema_for::<SearchCommands>();
    let Value::Object(mut schema) =
        serde_json::to_value(schema).expect("web search command schema should serialize")
    else {
        unreachable!("web search command schema must be an object");
    };

    let mut tool_schema = Map::new();
    for key in [
        "properties",
        "required",
        "type",
        "additionalProperties",
        "$defs",
        "definitions",
    ] {
        if let Some(value) = schema.remove(key) {
            tool_schema.insert(key.to_string(), value);
        }
    }
    Value::Object(tool_schema)
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
        let content = self
            .content
            .into_iter()
            .filter_map(|item| {
                if is_user {
                    return match item {
                        HistoryContent::InputText { text } => Some(ContentItem::InputText {
                            text: text.to_string(),
                        }),
                        HistoryContent::InputImage { .. }
                        | HistoryContent::InputAudio { .. }
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
                    HistoryContent::InputAudio { audio_url } => Some(ContentItem::InputAudio {
                        audio_url: audio_url.to_string(),
                    }),
                    HistoryContent::OutputText { .. } if *assistant_tokens == 0 => None,
                    HistoryContent::OutputText { text } => {
                        let tokens = approx_token_count(text);
                        let text = if tokens <= *assistant_tokens {
                            *assistant_tokens = assistant_tokens.saturating_sub(tokens);
                            text.to_string()
                        } else {
                            let text =
                                truncate_text(text, TruncationPolicy::Tokens(*assistant_tokens));
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
