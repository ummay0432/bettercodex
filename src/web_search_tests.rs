use super::*;
use crate::auth::Auth;
use crate::auth::SharedAuth;
use serde_json::json;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn message(role: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{
            "type": if role == "assistant" { "output_text" } else { "input_text" },
            "text": text,
        }],
    })
}

#[test]
fn command_schema_matches_the_codex_alpha_search_surface() {
    let properties = input_schema()["properties"].as_object().unwrap();
    assert_eq!(
        properties.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "click",
            "finance",
            "find",
            "image_query",
            "open",
            "response_length",
            "screenshot",
            "search_query",
            "sports",
            "time",
            "weather",
        ]
    );
    assert_eq!(
        properties["response_length"]["enum"],
        json!(["short", "medium", "long"])
    );
    assert_eq!(
        properties["sports"]["items"]["properties"]["league"]["enum"],
        json!([
            "nba", "wnba", "nfl", "nhl", "mlb", "epl", "ncaamb", "ncaawb", "ipl"
        ])
    );
    assert_eq!(DESCRIPTION.len(), 1_180);
}

#[test]
fn recent_input_keeps_two_operator_turns_and_drops_images_and_world_state() {
    let history = vec![
        message(
            "developer",
            "<environment_context>ignored</environment_context>",
        ),
        message(
            "user",
            "<repository_context>\nignored\n</repository_context>",
        ),
        message(
            "user",
            "<available_skills>\n- demo: ignored\n</available_skills>",
        ),
        message("user", "old user"),
        message("assistant", "old assistant"),
        json!({
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "previous user"},
                {"type": "input_image", "image_url": "data:image/png;base64,AA=="},
            ],
        }),
        json!({
            "id": "msg_previous",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "previous assistant"}],
        }),
        message("user", "current user"),
        message("assistant", "not sent after the current user"),
    ];

    assert_eq!(
        serde_json::to_value(recent_input(&history)).unwrap(),
        json!([
            message("user", "previous user"),
            message("assistant", "previous assistant"),
            message("user", "current user"),
        ])
    );
}

#[test]
fn recent_input_excludes_assistant_text_outside_the_selected_user_turns() {
    let history = vec![
        message("assistant", "before the only user"),
        message("user", "current user"),
        message("assistant", "after the current user"),
    ];

    assert_eq!(
        serde_json::to_value(recent_input(&history)).unwrap(),
        json!([message("user", "current user")])
    );
}

#[test]
fn recent_input_borrows_multimodal_payloads_before_discarding_them() {
    let history_message = json!({
        "type": "message",
        "role": "user",
        "content": [
            {"type": "input_text", "text": "inspect the image"},
            {"type": "input_image", "image_url": "data:image/png;base64,payload"},
        ],
    });
    let source = history_message["content"][1]["image_url"]
        .as_str()
        .unwrap()
        .as_ptr();

    let projected = HistoryMessage::deserialize(&history_message).unwrap();
    let HistoryContent::InputImage { image_url, .. } = &projected.content[1] else {
        panic!("expected the input image")
    };

    assert_eq!(image_url.as_ptr(), source);
    let mut assistant_tokens = ASSISTANT_CONTEXT_TOKEN_LIMIT;
    assert_eq!(
        serde_json::to_value(projected.into_search_item(&mut assistant_tokens)).unwrap(),
        message("user", "inspect the image")
    );
}

#[test]
fn recent_input_preserves_bounded_assistant_message_metadata() {
    let assistant_text = "x".repeat(ASSISTANT_CONTEXT_TOKEN_LIMIT * 4 + 100);
    let expected_text = truncate_text(
        &assistant_text,
        TruncationPolicy::Tokens(ASSISTANT_CONTEXT_TOKEN_LIMIT),
    );
    let history = vec![
        message("user", "previous user"),
        json!({
            "id": "msg_previous",
            "type": "message",
            "role": "assistant",
            "phase": "commentary",
            "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"},
            "content": [
                {"type": "output_text", "text": assistant_text},
                {"type": "input_image", "image_url": "https://example.com/image.png", "detail": "low"},
            ],
        }),
        message("user", "current user"),
    ];

    assert_eq!(
        serde_json::to_value(recent_input(&history)).unwrap(),
        json!([
            message("user", "previous user"),
            {
                "type": "message",
                "role": "assistant",
                "phase": "commentary",
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"},
                "content": [
                    {"type": "output_text", "text": expected_text},
                    {"type": "input_image", "image_url": "https://example.com/image.png", "detail": "low"},
                ],
            },
            message("user", "current user"),
        ])
    );
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_recent_input_with_discarded_large_payloads() {
    const RUNS: usize = 50;
    const IMAGE_BYTES: usize = 16 * 1024 * 1024;
    const TRAILING_ASSISTANT_BYTES: usize = 4 * 1024 * 1024;

    let history = vec![
        message("user", "previous user"),
        message("assistant", "previous assistant"),
        json!({
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "current user"},
                {
                    "type": "input_image",
                    "image_url": format!("data:image/png;base64,{}", "x".repeat(IMAGE_BYTES)),
                },
            ],
        }),
        message("assistant", &"x".repeat(TRAILING_ASSISTANT_BYTES)),
    ];

    let started = std::time::Instant::now();
    for _ in 0..RUNS {
        std::hint::black_box(recent_input(&history));
    }
    let elapsed = started.elapsed();

    eprintln!(
        "{RUNS} web-search context projections with a {} MiB image and {} MiB trailing assistant message: {elapsed:?}",
        IMAGE_BYTES / (1024 * 1024),
        TRAILING_ASSISTANT_BYTES / (1024 * 1024),
    );
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_search_request_retry_preparation() {
    const RUNS: usize = 100;
    const USER_BYTES: usize = 1024 * 1024;

    let history = vec![message("user", &"x".repeat(USER_BYTES))];
    let context = ToolTurnContext::from_history(&history, "{}".to_string());
    let commands = parse_commands(Some(json!({
        "search_query": [{"q": "request encoding benchmark"}],
    })))
    .unwrap();
    let url = "https://example.com/alpha/search".to_string();
    let started = std::time::Instant::now();
    for _ in 0..RUNS {
        let context = context.clone();
        let request = SearchRequest::new("benchmark-session", context.input.as_deref(), &commands);
        let body = EncodedJsonBody::encode(&request).unwrap();
        let request = Request {
            method: reqwest::Method::POST,
            url: url.clone(),
            headers: HeaderMap::new(),
            body: Some(RequestBody::EncodedJson(body)),
            compression: RequestCompression::None,
            timeout: None,
        }
        .into_prepared()
        .unwrap();
        for _ in 0..=REQUEST_MAX_RETRIES {
            let prepared = request.clone().prepare_body_for_send().unwrap();
            std::hint::black_box(prepared.body);
        }
    }
    let elapsed = started.elapsed();

    eprintln!(
        "{RUNS} web-search requests with {REQUEST_MAX_RETRIES} retry slots and a {} MiB user message: {elapsed:?}",
        USER_BYTES / (1024 * 1024),
    );
}

#[test]
fn command_parser_preserves_search_and_fetch_operations() {
    let commands = parse_commands(Some(json!({
        "search_query": [{"q": "Codex", "domains": ["openai.com"]}],
        "open": [{"ref_id": "turn0search0", "lineno": 12}],
        "click": [{"ref_id": "turn0fetch0", "id": 3}],
        "find": [{"ref_id": "turn0fetch0", "pattern": "Responses"}],
    })))
    .unwrap();

    assert_eq!(
        serde_json::to_value(commands).unwrap(),
        json!({
            "search_query": [{"q": "Codex", "domains": ["openai.com"]}],
            "open": [{"ref_id": "turn0search0", "lineno": 12}],
            "click": [{"ref_id": "turn0fetch0", "id": 3}],
            "find": [{"ref_id": "turn0fetch0", "pattern": "Responses"}],
        })
    );
    assert!(parse_commands(Some(json!([]))).is_err());
}

#[test]
fn display_activities_preserve_each_search_and_fetch_row() {
    assert_eq!(
        activities_for_display(Some(json!({
            "search_query": [{"q": "first"}, {"q": "second"}],
            "open": [{"ref_id": "turn0search0", "lineno": 12}],
            "click": [{"ref_id": "turn0fetch0", "id": 3}],
            "find": [{"ref_id": "turn0fetch0", "pattern": "Responses"}],
        }))),
        vec![
            WebActivity::new("Search", "first"),
            WebActivity::new("Search", "second"),
            WebActivity::new("Open", "turn0search0 at line 12"),
            WebActivity::new("Open", "link 3 in turn0fetch0"),
            WebActivity::new("Find", "'Responses' in turn0fetch0"),
        ]
    );
    assert_eq!(
        activities_for_display(Some(json!([]))),
        vec![WebActivity::new("Browse", "")]
    );
}

#[test]
fn search_output_is_bounded_even_if_the_endpoint_exceeds_its_budget() {
    let output = "x".repeat(50_000);
    let bounded = bounded_search_output(output.clone());

    assert!(bounded.starts_with("Warning: truncated output"));
    assert!(bounded.len() < output.len());
}

#[test]
fn search_output_within_budget_reuses_the_response_allocation() {
    let output = "search result".to_string();
    let allocation = output.as_ptr();

    let bounded = bounded_search_output(output);

    assert_eq!(bounded, "search result");
    assert_eq!(bounded.as_ptr(), allocation);
}

#[test]
fn search_response_ignores_unused_endpoint_fields() {
    let response: SearchResponse = serde_json::from_value(json!({
        "encrypted_output": {"future": "shape"},
        "output": "model-visible result",
        "results": "future shape",
    }))
    .unwrap();

    assert_eq!(response.output, "model-visible result");
}

#[tokio::test]
async fn already_cancelled_search_stops_before_transport() {
    let web_search = WebSearchClient::new(
        reqwest::Client::new(),
        SharedAuth::new(Auth::for_test("token")),
        "http://127.0.0.1:1".to_string(),
        "session-test".to_string(),
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = web_search
        .run(
            Some(json!({"search_query": [{"q": "should not run"}]})),
            &ToolTurnContext::default(),
            cancellation,
        )
        .await
        .unwrap_err();

    assert_eq!(error.to_string(), "web search cancelled");
}

#[tokio::test]
async fn rejected_search_auth_refreshes_once_and_retries_with_the_new_token() {
    let (base_url, requests, server) = spawn_auth_recovery_server();
    let client = reqwest::Client::new();
    let auth = Auth::refreshable_for_test(
        "stale-token",
        "refresh-token",
        format!("{base_url}/oauth/token"),
    );
    let web_search = WebSearchClient::new(
        client,
        SharedAuth::new(auth),
        base_url,
        "session-test".to_string(),
    );

    let output = web_search
        .run(
            Some(json!({"search_query": [{"q": "fresh sources"}]})),
            &ToolTurnContext::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(output, Value::String("fresh result".to_string()));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        requests,
        vec![
            RecordedRequest {
                path: "/alpha/search".to_string(),
                authorization: Some("Bearer stale-token".to_string()),
            },
            RecordedRequest {
                path: "/oauth/token".to_string(),
                authorization: None,
            },
            RecordedRequest {
                path: "/alpha/search".to_string(),
                authorization: Some("Bearer fresh-token".to_string()),
            },
        ]
    );
    server.join().unwrap();
}

#[derive(Debug, PartialEq, Eq)]
struct RecordedRequest {
    path: String,
    authorization: Option<String>,
}

fn spawn_auth_recovery_server() -> (
    String,
    mpsc::Receiver<Vec<RecordedRequest>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, requests_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in [
            ("401 Unauthorized", r#"{"error":"revoked"}"#),
            (
                "200 OK",
                r#"{"access_token":"fresh-token","refresh_token":"new-refresh-token"}"#,
            ),
            (
                "200 OK",
                r#"{"encrypted_output":null,"output":"fresh result","results":[]}"#,
            ),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_recorded_request(&mut stream));
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .unwrap();
            stream.flush().unwrap();
        }
        requests_tx.send(requests).unwrap();
    });
    (format!("http://{address}"), requests_rx, server)
}

fn read_recorded_request(stream: &mut TcpStream) -> RecordedRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let header_end = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "request closed before its headers completed");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while bytes.len().saturating_sub(header_end) < content_length {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "request closed before its body completed");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let path = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap()
        .to_string();
    let authorization = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_string())
    });
    RecordedRequest {
        path,
        authorization,
    }
}
