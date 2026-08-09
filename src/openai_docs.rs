//! Focused client for OpenAI's public Developer Docs MCP service.
//!
//! The bundled skill comes from `openai/codex` commit
//! `572954683910555cbbe3034bc8a2a0aa2bc7e66a`. Upstream Codex exposes this
//! service through its general MCP runtime. bettercodex deliberately has no MCP
//! framework, so this module retains only the five read-only documentation tools
//! and the service's stateless Streamable HTTP `tools/call` contract.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use quick_xml::Reader;
use quick_xml::escape::unescape;
use quick_xml::events::Event;
use reqwest::header::ACCEPT;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::USER_AGENT;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(crate) const NAMESPACE: &str = "openaiDeveloperDocs";
pub(crate) const FETCH_OPENAI_DOC: &str = "fetch_openai_doc";
pub(crate) const GET_OPENAPI_SPEC: &str = "get_openapi_spec";
pub(crate) const LIST_API_ENDPOINTS: &str = "list_api_endpoints";
pub(crate) const LIST_OPENAI_DOCS: &str = "list_openai_docs";
pub(crate) const SEARCH_OPENAI_DOCS: &str = "search_openai_docs";

const ENDPOINT: &str = "https://developers.openai.com/mcp";
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const REQUEST_ID: u64 = 1;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
const CHANGELOG_FEED: &str = "https://learn.chatgpt.com/docs/changelog/rss.xml";

#[derive(Clone, Copy)]
pub(crate) struct OpenAiDocsTool {
    javascript_name: &'static str,
    name: &'static str,
    description: &'static str,
}

impl OpenAiDocsTool {
    pub(crate) fn javascript_name(self) -> &'static str {
        self.javascript_name
    }

    pub(crate) fn name(self) -> &'static str {
        self.name
    }

    pub(crate) fn description(self) -> &'static str {
        self.description
    }

    pub(crate) fn input_schema(self) -> Value {
        match self.name {
            FETCH_OPENAI_DOC => json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "minLength": 1},
                    "anchor": {"type": "string"}
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            GET_OPENAPI_SPEC => json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "minLength": 1},
                    "languages": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1}
                    },
                    "codeExamplesOnly": {"type": "boolean"}
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            LIST_API_ENDPOINTS => json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            LIST_OPENAI_DOCS => json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                    "cursor": {"type": "string"}
                },
                "additionalProperties": false
            }),
            SEARCH_OPENAI_DOCS => json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                    "cursor": {"type": "string"}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            _ => unreachable!("OpenAI Docs tool specifications have fixed names"),
        }
    }
}

pub(crate) const TOOLS: &[OpenAiDocsTool] = &[
    OpenAiDocsTool {
        javascript_name: "openaiDeveloperDocs__fetch_openai_doc",
        name: FETCH_OPENAI_DOC,
        description: "Fetch exact Markdown for an official OpenAI documentation URL; `anchor` can select one section. Search or list first when the URL is unknown. Returns the server's text payload.",
    },
    OpenAiDocsTool {
        javascript_name: "openaiDeveloperDocs__get_openapi_spec",
        name: GET_OPENAPI_SPEC,
        description: "Return the OpenAPI specification for one URL from `list_api_endpoints`; optionally filter code samples by language or return only examples. Returns the server's text payload.",
    },
    OpenAiDocsTool {
        javascript_name: "openaiDeveloperDocs__list_api_endpoints",
        name: LIST_API_ENDPOINTS,
        description: "List all OpenAI API endpoint URLs available in the current OpenAPI specification. Returns the server's text payload.",
    },
    OpenAiDocsTool {
        javascript_name: "openaiDeveloperDocs__list_openai_docs",
        name: LIST_OPENAI_DOCS,
        description: "Browse official pages from `platform.openai.com`, `developers.openai.com`, and `learn.chatgpt.com`; use fetch on a result URL for exact Markdown. Returns the server's text payload.",
    },
    OpenAiDocsTool {
        javascript_name: "openaiDeveloperDocs__search_openai_docs",
        name: SEARCH_OPENAI_DOCS,
        description: "Search official OpenAI, ChatGPT, and Codex documentation; then use fetch on the best result URL before quoting or relying on it. Returns the server's text payload.",
    },
];

pub(crate) fn is_tool(name: &str) -> bool {
    TOOLS.iter().any(|tool| tool.name == name)
}

#[derive(Clone)]
pub(crate) struct OpenAiDocsClient {
    client: reqwest::Client,
    endpoint: String,
    changelog_feed: String,
}

impl OpenAiDocsClient {
    pub(crate) fn new(client: reqwest::Client) -> Self {
        Self::with_endpoint(client, ENDPOINT)
    }

    pub(crate) fn with_endpoint(client: reqwest::Client, endpoint: impl Into<String>) -> Self {
        Self {
            client,
            endpoint: endpoint.into(),
            changelog_feed: CHANGELOG_FEED.to_string(),
        }
    }

    pub(crate) async fn call(
        &self,
        tool_name: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        if !is_tool(tool_name) {
            return Err(anyhow!("unknown OpenAI Developer Docs tool `{tool_name}`"));
        }
        if !arguments.is_object() {
            return Err(anyhow!(
                "OpenAI Developer Docs tool `{tool_name}` expects a JSON object"
            ));
        }
        if tool_name == FETCH_OPENAI_DOC
            && let Some(request) = changelog_request(&arguments)?
        {
            let markdown = self.fetch_changelog(request, &cancellation).await?;
            return Ok(Value::String(markdown));
        }

        let request = json!({
            "jsonrpc": "2.0",
            "id": REQUEST_ID,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            }
        });
        let response = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(anyhow!("OpenAI Developer Docs request cancelled"));
            }
            response = self.client
                .post(&self.endpoint)
                .header(ACCEPT, "application/json, text/event-stream")
                .header(CONTENT_TYPE, "application/json")
                .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
                .header(USER_AGENT, concat!("bettercodex/", env!("CARGO_PKG_VERSION")))
                .timeout(REQUEST_TIMEOUT)
                .json(&request)
                .send() => response,
        }
        .context("OpenAI Developer Docs request failed")?;

        let status = response.status();
        if !status.is_success() {
            let body = read_bounded_body(response, MAX_ERROR_BODY_BYTES, &cancellation)
                .await
                .unwrap_or_else(|_| b"unreadable response".to_vec());
            let body = String::from_utf8_lossy(&body);
            return Err(anyhow!(
                "OpenAI Developer Docs request failed with HTTP {status}: {}",
                body.chars().take(4_000).collect::<String>()
            ));
        }

        let is_event_stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let body = read_bounded_body(response, MAX_RESPONSE_BYTES, &cancellation).await?;
        let response = decode_response(&body, is_event_stream)?;
        Ok(Value::String(extract_text_result(response)?))
    }

    async fn fetch_changelog(
        &self,
        request: ChangelogRequest,
        cancellation: &CancellationToken,
    ) -> Result<String> {
        let response = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(anyhow!("OpenAI Developer Docs request cancelled"));
            }
            response = self.client
                .get(&self.changelog_feed)
                .header(ACCEPT, "application/rss+xml, application/xml")
                .header(USER_AGENT, concat!("bettercodex/", env!("CARGO_PKG_VERSION")))
                .timeout(REQUEST_TIMEOUT)
                .send() => response,
        }
        .context("OpenAI changelog request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = read_bounded_body(response, MAX_ERROR_BODY_BYTES, cancellation)
                .await
                .unwrap_or_else(|_| b"unreadable response".to_vec());
            return Err(anyhow!(
                "OpenAI changelog request failed with HTTP {status}: {}",
                String::from_utf8_lossy(&body)
                    .chars()
                    .take(4_000)
                    .collect::<String>()
            ));
        }
        let body = read_bounded_body(response, MAX_RESPONSE_BYTES, cancellation).await?;
        changelog_markdown(&body, request.anchor.as_deref())
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ChangelogRequest {
    anchor: Option<String>,
}

fn changelog_request(arguments: &Value) -> Result<Option<ChangelogRequest>> {
    let Some(raw_url) = arguments.get("url").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Ok(url) = url::Url::parse(raw_url) else {
        return Ok(None);
    };
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("learn.chatgpt.com" | "developers.openai.com")
        )
        || !matches!(
            url.path().trim_end_matches('/'),
            "/docs/changelog" | "/codex/changelog"
        )
    {
        return Ok(None);
    }
    let anchor = arguments
        .get("anchor")
        .map(|anchor| {
            anchor
                .as_str()
                .ok_or_else(|| anyhow!("OpenAI changelog anchor must be a string"))
        })
        .transpose()?
        .or_else(|| url.fragment())
        .map(|anchor| anchor.trim_start_matches('#').to_string())
        .filter(|anchor| !anchor.is_empty() && anchor != "changelog-content");
    Ok(Some(ChangelogRequest { anchor }))
}

#[derive(Default)]
struct ChangelogFeed {
    title: String,
    description: String,
    items: Vec<ChangelogItem>,
}

#[derive(Default)]
struct ChangelogItem {
    title: String,
    link: String,
    published: String,
    content: String,
}

fn changelog_markdown(body: &[u8], anchor: Option<&str>) -> Result<String> {
    let feed = parse_changelog_feed(body)?;
    let matching_items = feed
        .items
        .iter()
        .filter(|item| {
            anchor.is_none_or(|anchor| {
                url::Url::parse(&item.link)
                    .ok()
                    .is_some_and(|url| url.fragment() == Some(anchor))
            })
        })
        .collect::<Vec<_>>();
    if let Some(anchor) = anchor
        && matching_items.is_empty()
    {
        return Err(anyhow!("OpenAI changelog has no section `{anchor}`"));
    }
    if feed.title.is_empty() || matching_items.is_empty() {
        return Err(anyhow!("OpenAI changelog feed contained no entries"));
    }

    let mut markdown = format!("# {}\n", feed.title);
    if anchor.is_none() && !feed.description.is_empty() {
        markdown.push('\n');
        markdown.push_str(&feed.description);
        markdown.push('\n');
    }
    for item in matching_items {
        markdown.push_str("\n## [");
        markdown.push_str(&item.title);
        markdown.push_str("](");
        markdown.push_str(&item.link);
        markdown.push_str(")\n");
        if !item.published.is_empty() {
            markdown.push_str("\nPublished: ");
            markdown.push_str(&item.published);
            markdown.push('\n');
        }
        if !item.content.is_empty() {
            markdown.push('\n');
            markdown.push_str(item.content.trim());
            markdown.push('\n');
        }
    }
    Ok(markdown)
}

fn parse_changelog_feed(body: &[u8]) -> Result<ChangelogFeed> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut feed = ChangelogFeed::default();
    let mut item = None::<ChangelogItem>;
    loop {
        match reader
            .read_event()
            .context("OpenAI changelog returned invalid XML")?
        {
            Event::Start(element) => {
                let local_name = element.local_name();
                match local_name.as_ref() {
                    b"item" => item = Some(ChangelogItem::default()),
                    b"title" | b"description" | b"link" | b"pubDate" | b"encoded" => {
                        let text = reader
                            .read_text(element.name())
                            .context("OpenAI changelog contained invalid text")?
                            .decode()
                            .context("OpenAI changelog contained invalid UTF-8")?;
                        let text = unescape(&text)
                            .context("OpenAI changelog contained invalid XML entities")?
                            .into_owned();
                        match (item.as_mut(), local_name.as_ref()) {
                            (Some(item), b"title") => item.title = text,
                            (Some(item), b"link") => item.link = text,
                            (Some(item), b"pubDate") => item.published = text,
                            (Some(item), b"encoded") => item.content = text,
                            (None, b"title") if feed.title.is_empty() => feed.title = text,
                            (None, b"description") if feed.description.is_empty() => {
                                feed.description = text;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            Event::End(element) if element.local_name().as_ref() == b"item" => {
                if let Some(item) = item.take() {
                    feed.items.push(item);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(feed)
}

async fn read_bounded_body(
    mut response: reqwest::Response,
    maximum: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(maximum).unwrap_or(u64::MAX))
    {
        return Err(anyhow!(
            "OpenAI Developer Docs response exceeds the {maximum}-byte limit"
        ));
    }

    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(anyhow!("OpenAI Developer Docs request cancelled"));
            }
            chunk = response.chunk() => chunk,
        }
        .context("failed to read OpenAI Developer Docs response")?;
        let Some(chunk) = chunk else {
            break;
        };
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(anyhow!(
                "OpenAI Developer Docs response exceeds the {maximum}-byte limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn decode_response(body: &[u8], is_event_stream: bool) -> Result<Value> {
    let first_non_whitespace = body
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());
    if is_event_stream || matches!(first_non_whitespace, Some(b'd' | b'e' | b':')) {
        decode_sse_response(body)
    } else {
        let response: Value =
            serde_json::from_slice(body).context("OpenAI Developer Docs returned invalid JSON")?;
        validate_response_id(response)
    }
}

fn decode_sse_response(body: &[u8]) -> Result<Value> {
    let body =
        std::str::from_utf8(body).context("OpenAI Developer Docs returned non-UTF-8 event data")?;
    let mut data = String::new();
    let mut responses = Vec::new();
    for line in body.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if data.is_empty() {
                continue;
            }
            if data != "[DONE]" {
                responses.push(
                    serde_json::from_str::<Value>(&data)
                        .context("OpenAI Developer Docs returned invalid SSE JSON")?,
                );
            }
            data.clear();
            continue;
        }
        let Some(fragment) = line.strip_prefix("data:") else {
            continue;
        };
        if !data.is_empty() {
            data.push('\n');
        }
        data.push_str(fragment.strip_prefix(' ').unwrap_or(fragment));
    }

    let response = responses
        .into_iter()
        .find(|response| response.get("id") == Some(&json!(REQUEST_ID)))
        .ok_or_else(|| anyhow!("OpenAI Developer Docs SSE response omitted request id"))?;
    validate_response_id(response)
}

fn validate_response_id(response: Value) -> Result<Value> {
    if response.get("id") != Some(&json!(REQUEST_ID)) {
        return Err(anyhow!(
            "OpenAI Developer Docs response used an unexpected request id"
        ));
    }
    Ok(response)
}

fn extract_text_result(response: Value) -> Result<String> {
    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(Value::as_i64);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown JSON-RPC error");
        return Err(match code {
            Some(code) => {
                anyhow!("OpenAI Developer Docs returned JSON-RPC error {code}: {message}")
            }
            None => anyhow!("OpenAI Developer Docs returned a JSON-RPC error: {message}"),
        });
    }

    let result = response
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("OpenAI Developer Docs response omitted its result"))?;
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("OpenAI Developer Docs result omitted text content"))?;
    let mut text = Vec::new();
    for block in content {
        match (
            block.get("type").and_then(Value::as_str),
            block.get("text").and_then(Value::as_str),
        ) {
            (Some("text"), Some(value)) => text.push(value),
            (Some(kind), _) => {
                return Err(anyhow!(
                    "OpenAI Developer Docs returned unsupported `{kind}` content"
                ));
            }
            _ => {
                return Err(anyhow!("OpenAI Developer Docs returned malformed content"));
            }
        }
    }
    if text.is_empty() {
        return Err(anyhow!("OpenAI Developer Docs returned no text content"));
    }
    let text = text.join("\n");
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(anyhow!("OpenAI Developer Docs tool failed: {text}"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::io::Write;
    use std::net::TcpListener;

    const CHANGELOG_RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>ChatGPT &amp; Codex changelog</title>
    <description>Latest updates to ChatGPT and Codex.</description>
    <item>
      <title>Codex CLI Release: 1.2.3</title>
      <link>https://learn.chatgpt.com/docs/changelog#release-one</link>
      <pubDate>Fri, 07 Aug 2026 00:00:00 GMT</pubDate>
      <content:encoded>&lt;h2&gt;Fixes&lt;/h2&gt;&lt;p&gt;One &amp;amp; two&lt;/p&gt;</content:encoded>
    </item>
    <item>
      <title>Another release</title>
      <link>https://learn.chatgpt.com/docs/changelog#release-two</link>
      <pubDate>Thu, 06 Aug 2026 00:00:00 GMT</pubDate>
      <content:encoded>&lt;p&gt;Other&lt;/p&gt;</content:encoded>
    </item>
  </channel>
</rss>"#;

    #[tokio::test]
    async fn fetch_changelog_uses_retrievable_feed_and_honors_anchor() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4_096];
            let length = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..length]).starts_with("GET /feed "));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{CHANGELOG_RSS}",
                CHANGELOG_RSS.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let client = OpenAiDocsClient {
            client: crate::http_client::build_client(reqwest::Client::builder()).unwrap(),
            endpoint: "http://127.0.0.1:1".to_string(),
            changelog_feed: format!("http://{address}/feed"),
        };

        let result = client
            .call(
                FETCH_OPENAI_DOC,
                json!({
                    "url": "https://learn.chatgpt.com/docs/changelog",
                    "anchor": "release-one",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        server.join().unwrap();

        let markdown = result.as_str().unwrap();
        assert!(markdown.starts_with("# ChatGPT & Codex changelog\n"));
        assert!(markdown.contains(
            "## [Codex CLI Release: 1.2.3](https://learn.chatgpt.com/docs/changelog#release-one)"
        ));
        assert!(markdown.contains("<h2>Fixes</h2><p>One &amp; two</p>"));
        assert!(!markdown.contains("Another release"));
    }

    #[test]
    fn changelog_fallback_is_limited_to_the_known_dynamic_page() {
        assert_eq!(
            changelog_request(&json!({
                "url": "https://developers.openai.com/codex/changelog/#release-one"
            }))
            .unwrap(),
            Some(ChangelogRequest {
                anchor: Some("release-one".to_string())
            })
        );
        assert_eq!(
            changelog_request(&json!({
                "url": "https://learn.chatgpt.com/docs/changelog#changelog-content"
            }))
            .unwrap(),
            Some(ChangelogRequest { anchor: None })
        );
        assert_eq!(
            changelog_request(&json!({
                "url": "https://learn.chatgpt.com/docs/developer-commands"
            }))
            .unwrap(),
            None
        );
    }

    #[test]
    fn changelog_feed_formats_the_full_page_and_rejects_unknown_sections() {
        let markdown = changelog_markdown(CHANGELOG_RSS.as_bytes(), None).unwrap();
        assert!(markdown.contains("Latest updates to ChatGPT and Codex."));
        assert!(markdown.contains("Codex CLI Release: 1.2.3"));
        assert!(markdown.contains("Another release"));

        let error = changelog_markdown(CHANGELOG_RSS.as_bytes(), Some("missing")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "OpenAI changelog has no section `missing`"
        );
    }
}
