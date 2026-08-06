//! Standalone Codex web search for Responses Lite.
//!
//! The wire client and request types come from OpenAI Codex commit
//! `1669c2403f793d0230065397dfc25f52b844244e`. The focused adapter here mirrors
//! `codex-rs/ext/web-search`: it exposes `web.run` inside the exec runtime and
//! sends search, fetch/navigation, image, finance, weather, sports, and time
//! commands to the same `alpha/search` endpoint used by Codex.

use crate::MODEL;
use crate::auth::SharedAuth;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_client::HttpTransport;
use codex_client::Request;
use codex_client::RequestBody;
use codex_client::RequestCompression;
use codex_client::ReqwestTransport;
use codex_client::RetryOn;
use codex_client::RetryPolicy;
use codex_client::TransportError;
use codex_client::run_with_retry;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_token_count;
use codex_utils_output_truncation::formatted_truncate_text;
use codex_utils_output_truncation::truncate_text;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use schemars::JsonSchema;
use schemars::r#gen::SchemaSettings;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
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
    input: Option<SearchInput>,
    turn_metadata: String,
}

#[derive(Debug, Serialize)]
struct SearchRequest {
    id: String,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<SearchInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commands: Option<SearchCommands>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum SearchInput {
    Items(Vec<ResponseItem>),
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
    #[serde(rename = "encrypted_output")]
    _encrypted_output: Option<String>,
    output: String,
    #[serde(default, rename = "results")]
    _results: Option<Vec<Value>>,
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
        let commands = parse_commands(input)?;
        let mut auth = self
            .auth
            .refreshed_snapshot(&self.client)
            .await
            .context("failed to refresh ChatGPT credentials for web search")?;
        let request = SearchRequest {
            id: self.session_id.clone(),
            model: MODEL.to_string(),
            input: context.input.clone(),
            commands: Some(commands),
            settings: Some(serde_json::json!({
                "allowed_callers": ["direct"],
                "external_web_access": true,
            })),
            max_output_tokens: Some(u64::try_from(MAX_OUTPUT_TOKENS).unwrap_or(u64::MAX)),
        };
        let body = serde_json::to_value(request)
            .context("failed to encode standalone web search request")?;
        let transport = ReqwestTransport::new(self.client.clone());
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), SEARCH_PATH);
        let mut refreshed_after_unauthorized = false;
        let response = loop {
            let request = Request {
                method: reqwest::Method::POST,
                url: url.clone(),
                headers: search_headers(&context.turn_metadata, &auth)?,
                body: Some(RequestBody::Json(body.clone())),
                compression: RequestCompression::None,
                timeout: None,
            };
            let result = tokio::select! {
                _ = cancellation.cancelled() => return Err(anyhow!("web search cancelled")),
                response = run_with_retry(
                    search_retry_policy(),
                    || request.clone(),
                    |request, _attempt| transport.execute(request),
                ) => response,
            };
            match result {
                Err(TransportError::Http { status, .. })
                    if status.as_u16() == 401 && !refreshed_after_unauthorized =>
                {
                    auth = tokio::select! {
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
        let response: SearchResponse = serde_json::from_slice(&response.body)
            .context("failed to decode standalone web search response")?;
        Ok(Value::String(bounded_search_output(&response.output)))
    }
}

fn bounded_search_output(output: &str) -> String {
    formatted_truncate_text(output, TruncationPolicy::Tokens(MAX_OUTPUT_TOKENS))
}

impl ToolTurnContext {
    pub(crate) fn from_history(history: &[Value], turn_metadata: String) -> Self {
        Self {
            input: recent_input(history),
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

fn search_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: REQUEST_MAX_RETRIES,
        base_delay: REQUEST_RETRY_DELAY,
        retry_on: RetryOn {
            retry_429: false,
            retry_5xx: true,
            retry_transport: true,
        },
    }
}

fn recent_input(history: &[Value]) -> Option<SearchInput> {
    let mut messages = Vec::new();
    let mut user_messages = 0;
    for value in history.iter().rev() {
        let Some(message) = visible_message(value) else {
            continue;
        };
        if message.is_user_message() {
            user_messages += 1;
            messages.push(message);
            if user_messages == 2 {
                break;
            }
        } else if user_messages > 0 {
            messages.push(message);
        }
    }
    messages.reverse();
    retain_recent_user_turns(&mut messages, 2);
    truncate_assistant_text(&mut messages, ASSISTANT_CONTEXT_TOKEN_LIMIT);
    (!messages.is_empty()).then_some(SearchInput::Items(messages))
}

fn visible_message(value: &Value) -> Option<ResponseItem> {
    if value.get("type").and_then(Value::as_str) != Some("message")
        || !matches!(
            value.get("role").and_then(Value::as_str),
            Some("assistant" | "user")
        )
    {
        return None;
    }
    let item = serde_json::from_value::<ResponseItem>(value.clone()).ok()?;
    match item {
        ResponseItem::Message { ref role, .. } if role == "assistant" => {
            let mut message = item;
            message.set_id(None);
            Some(message)
        }
        ResponseItem::Message {
            role,
            content,
            phase,
            internal_chat_message_metadata_passthrough,
            ..
        } if role == "user" && is_operator_message(&content) => {
            let content = content
                .into_iter()
                .filter(|item| matches!(item, ContentItem::InputText { .. }))
                .collect::<Vec<_>>();
            (!content.is_empty()).then_some(ResponseItem::Message {
                id: None,
                role,
                content,
                phase,
                internal_chat_message_metadata_passthrough,
            })
        }
        _ => None,
    }
}

fn is_operator_message(content: &[ContentItem]) -> bool {
    let text = content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    !crate::context::is_contextual_user_text(&text)
}

fn retain_recent_user_turns(items: &mut Vec<ResponseItem>, user_message_count: usize) {
    if user_message_count == 0 {
        items.clear();
        return;
    }
    let Some(latest_user) = items.iter().rposition(ResponseItem::is_user_message) else {
        items.clear();
        return;
    };
    items.truncate(latest_user + 1);
    let earliest_user = items
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, item)| item.is_user_message())
        .take(user_message_count)
        .last()
        .map(|(index, _)| index)
        .unwrap_or(latest_user);
    items.drain(..earliest_user);
}

fn truncate_assistant_text(items: &mut Vec<ResponseItem>, max_tokens: usize) {
    let mut remaining = max_tokens;
    items.retain_mut(|item| {
        let ResponseItem::Message { role, content, .. } = item else {
            return true;
        };
        if role != "assistant" {
            return true;
        }
        content.retain_mut(|item| {
            let ContentItem::OutputText { text } = item else {
                return true;
            };
            if remaining == 0 {
                return false;
            }
            let tokens = approx_token_count(text);
            if tokens <= remaining {
                remaining = remaining.saturating_sub(tokens);
            } else {
                *text = truncate_text(text, TruncationPolicy::Tokens(remaining));
                remaining = 0;
            }
            true
        });
        !content.is_empty()
    });
}

#[cfg(test)]
#[path = "web_search_tests.rs"]
mod tests;
