use super::*;
use crate::compaction::CompactionPhase;
use crate::compaction::CompactionRequest;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

fn test_client(base_url: String) -> ApiClient {
    let identity = SessionIdentity {
        installation_id: "installation-test".to_string(),
        session_id: "session-test".to_string(),
        thread_id: "thread-test".to_string(),
    };
    ApiClient::new_with_base_url(Auth::for_test("token-test"), &identity, 0, base_url).unwrap()
}

trait TestApiClient {
    async fn respond(
        &mut self,
        history: Vec<Value>,
        completed_items: &UnboundedSender<Value>,
    ) -> ApiResult<ModelResponse>;

    async fn compact(
        &mut self,
        history: &[Value],
        compaction: CompactionRequest,
    ) -> ApiResult<CompactionResult>;
}

impl TestApiClient for ApiClient {
    async fn respond(
        &mut self,
        history: Vec<Value>,
        completed_items: &UnboundedSender<Value>,
    ) -> ApiResult<ModelResponse> {
        let mut request = self.build_request(history, RequestKind::Turn);
        self.respond_request_with_events(
            &mut request,
            completed_items,
            None,
            RequestKind::Turn,
            RequestInputIdentity::Exact,
        )
        .await
    }

    async fn compact(
        &mut self,
        history: &[Value],
        compaction: CompactionRequest,
    ) -> ApiResult<CompactionResult> {
        self.compact_with_identity(history, compaction, RequestInputIdentity::Exact)
            .await
    }
}

fn user_message(text: &str) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": text}],
    })
}

fn assistant_item(text: &str) -> Value {
    json!({
        "id": format!("msg_{text}"),
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": text}],
    })
}

fn assistant_item_with_phase(text: &str, phase: &str) -> Value {
    let mut item = assistant_item(text);
    item["phase"] = json!(phase);
    item
}

fn completed_sse(response_id: &str, item: &Value) -> String {
    completed_sse_with_items(response_id, std::slice::from_ref(item))
}

fn completed_sse_with_items(response_id: &str, items: &[Value]) -> String {
    let mut stream = String::new();
    for (output_index, item) in items.iter().enumerate() {
        let item_done = json!({
            "type": "response.output_item.done",
            "output_index": output_index,
            "item": item,
        });
        stream.push_str(&format!("data: {item_done}\n\n"));
    }
    let mut completed = completed_event(response_id, items.first().unwrap_or(&Value::Null));
    completed["response"]["output"] = Value::Array(items.to_vec());
    stream.push_str(&format!("data: {completed}\n\n"));
    stream
}

fn completed_event(response_id: &str, item: &Value) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "model": MODEL,
            "reasoning": {"context": "all_turns"},
            "output": [item],
            "usage": {
                "input_tokens": 42,
                "input_tokens_details": {
                    "cached_tokens": 30,
                    "cache_write_tokens": 12,
                },
                "output_tokens": 8,
                "output_tokens_details": {"reasoning_tokens": 5},
                "total_tokens": 50,
            },
        },
    })
}

#[test]
fn completed_event_records_full_cache_usage() {
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();
    process_event_value(
        completed_event("resp_usage", &assistant_item("done")),
        &mut collected,
        &completed_items,
        None,
    )
    .unwrap();

    assert!(collected.completed);
    assert_eq!(
        collected.usage,
        Some(TokenUsage {
            input_tokens: 42,
            cached_input_tokens: 30,
            cache_write_input_tokens: 12,
            output_tokens: 8,
            reasoning_output_tokens: 5,
            total_tokens: 50,
        })
    );
}

#[test]
fn completed_items_are_emitted_once_in_api_order() {
    let (completed_items, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();
    let first = json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "cipher"});
    let second = assistant_item("done");

    process_event_value(
        json!({"type": "response.output_item.done", "output_index": 1, "item": second}),
        &mut collected,
        &completed_items,
        None,
    )
    .unwrap();
    assert!(received.try_recv().is_err());
    process_event_value(
        json!({"type": "response.output_item.done", "output_index": 0, "item": first}),
        &mut collected,
        &completed_items,
        None,
    )
    .unwrap();

    assert_eq!(received.try_recv().unwrap()["id"], "rs_1");
    assert_eq!(received.try_recv().unwrap()["id"], "msg_done");
    assert!(received.try_recv().is_err());
}

#[test]
fn completed_item_retains_its_stream_allocation_in_the_collector() {
    let (completed_items, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();
    let event = json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {
            "type": "reasoning",
            "id": "rs_move",
            "encrypted_content": "cipher payload",
        },
    });
    let stream_allocation = event["item"]["encrypted_content"]
        .as_str()
        .unwrap()
        .as_ptr();

    process_event_value(event, &mut collected, &completed_items, None).unwrap();

    assert_eq!(
        collected.items[0]["encrypted_content"]
            .as_str()
            .unwrap()
            .as_ptr(),
        stream_allocation,
        "stream payloads must move into the response collector instead of being deep-cloned"
    );
    assert_eq!(received.try_recv().unwrap(), collected.items[0]);
}

#[test]
fn extracts_text_and_forwards_streaming_events() {
    assert_eq!(
        terminal_answer(&[assistant_item("done")]).as_deref(),
        Some("done")
    );
    assert_eq!(
        terminal_answer(&[
            assistant_item_with_phase("working", "commentary"),
            assistant_item_with_phase("done", "final_answer"),
        ])
        .as_deref(),
        Some("done")
    );
    assert_eq!(
        terminal_answer(&[assistant_item_with_phase("future", "unknown")]),
        None
    );
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();
    process_event(
        r#"{"type":"response.output_text.delta","delta":"hello"}"#,
        &mut collected,
        &completed_items,
        Some(&events),
    )
    .unwrap();
    process_event(
        r#"{"type":"response.reasoning_summary_part.added","summary_index":0,"part":{"type":"summary_text","text":""}}"#,
        &mut collected,
        &completed_items,
        Some(&events),
    )
    .unwrap();
    process_event(
        r#"{"type":"response.reasoning_summary_text.delta","delta":"checking"}"#,
        &mut collected,
        &completed_items,
        Some(&events),
    )
    .unwrap();

    assert_eq!(
        received.try_recv().unwrap(),
        AgentEvent::ModelMessageDelta("hello".to_string())
    );
    assert_eq!(
        received.try_recv().unwrap(),
        AgentEvent::ReasoningSummarySectionStarted
    );
    assert_eq!(
        received.try_recv().unwrap(),
        AgentEvent::ReasoningSummaryDelta("checking".to_string())
    );
}

#[test]
fn stream_errors_classify_websocket_recovery_cases() {
    let previous = error_event(&json!({
        "type": "error",
        "error": {"code": "previous_response_not_found", "message": "expired"},
    }));
    assert_eq!(previous.kind, ApiErrorKind::PreviousResponseNotFound);
    let overloaded = error_event(&json!({
        "type": "error",
        "status_code": 503,
        "error": {"message": "busy"},
    }));
    assert!(overloaded.is_retryable());
    assert!(classify_stream_error("future_transient_error", "try again").is_retryable());
    assert!(!classify_stream_error("insufficient_quota", "quota exhausted").is_retryable());
    assert_eq!(
        classify_stream_error(
            "rate_limit_exceeded",
            "Rate limit reached. Please try again in 1.898s.",
        )
        .retry_after(),
        Some(Duration::from_secs_f64(1.898))
    );
    assert_eq!(
        parse_rate_limit_delay("Rate limit exceeded. Try again in 28ms."),
        Some(Duration::from_millis(28))
    );
    assert_eq!(
        parse_rate_limit_delay("Rate limit exceeded. Try again in 35 seconds."),
        Some(Duration::from_secs(35))
    );
}

#[tokio::test]
async fn http_transport_sends_the_contract_and_collects_the_response() {
    let item = assistant_item("http");
    let (base_url, requests, server) = spawn_http_server(vec![
        HttpReply::ok("text/event-stream", completed_sse("resp_http", &item))
            .with_header("x-reasoning-included", "true")
            .with_header("openai-model", MODEL),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    client.turn_started_at_unix_ms = 1_700_000_000_123;
    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let response = client
        .respond(vec![user_message("hello")], &completed_items)
        .await
        .unwrap();
    assert_eq!(response.final_answer.as_deref(), Some("http"));
    assert!(response.server_reasoning_included);
    assert_eq!(response.usage.unwrap().cached_input_tokens, 30);
    assert_eq!(completed_rx.try_recv().unwrap(), item);
    assert!(completed_rx.try_recv().is_err());

    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.path, "/responses");
    assert!(request.headers.contains("authorization: Bearer token-test"));
    assert!(
        request
            .headers
            .contains("x-openai-internal-codex-responses-lite: true")
    );
    assert!(request.headers.contains("content-encoding: zstd"));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], MODEL);
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(
        body["input"].as_array().unwrap().last().unwrap(),
        &user_message("hello")
    );
    let client_turn_metadata: Value = serde_json::from_str(
        body["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        client_turn_metadata["turn_started_at_unix_ms"],
        1_700_000_000_123_u64
    );
    assert!(client_turn_metadata.get("code_mode_tool_names").is_some());
    let header_turn_metadata: Value = serde_json::from_str(
        request
            .headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("x-codex-turn-metadata")
                    .then_some(value.trim())
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        header_turn_metadata["turn_started_at_unix_ms"],
        client_turn_metadata["turn_started_at_unix_ms"]
    );
    assert!(header_turn_metadata.get("code_mode_tool_names").is_none());
    server.join().unwrap();
}

#[tokio::test]
async fn websocket_upgrade_failure_falls_back_to_http() {
    let item = assistant_item("https fallback");
    let (base_url, requests, server) = spawn_http_server(vec![
        HttpReply::status(404, "text/plain", "no websocket here"),
        HttpReply::ok(
            "text/event-stream",
            completed_sse("resp_https_fallback", &item),
        ),
    ]);
    let mut client = test_client(base_url);
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let response = client
        .respond(vec![user_message("hello")], &completed_items)
        .await
        .unwrap();
    assert_eq!(response.final_answer.as_deref(), Some("https fallback"));
    assert!(!client.prefer_websocket);
    assert_eq!(
        requests.recv_timeout(Duration::from_secs(2)).unwrap().path,
        "/responses"
    );
    assert_eq!(
        requests.recv_timeout(Duration::from_secs(2)).unwrap().path,
        "/responses"
    );
    server.join().unwrap();
}

#[tokio::test]
async fn compaction_sends_the_trigger_and_returns_only_retained_history() {
    let retained = user_message("retain this operator message");
    let discarded = assistant_item("discard this completed answer");
    let opaque = json!({
        "type": "compaction",
        "id": "cmp_1",
        "encrypted_content": "opaque summary",
    });
    let (base_url, requests, server) = spawn_http_server(vec![HttpReply::ok(
        "text/event-stream",
        completed_sse("resp_compacted", &opaque),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;

    let result = client
        .compact(
            &[retained.clone(), discarded.clone()],
            CompactionRequest::Automatic(CompactionPhase::MidTurn),
        )
        .await
        .unwrap();

    assert_eq!(result.items, [retained.clone(), opaque]);
    assert_eq!(
        client.window, 0,
        "cache lineage advances only after the replacement is persisted"
    );
    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.path, "/responses");
    let request: Value = serde_json::from_slice(&request.body).unwrap();
    let input = request["input"].as_array().unwrap();
    assert!(input.contains(&retained));
    assert!(input.contains(&discarded));
    assert_eq!(
        input.last().and_then(|item| item["type"].as_str()),
        Some("compaction_trigger")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn compaction_rejects_a_response_without_one_opaque_item() {
    let ordinary = assistant_item("not compacted");
    let (base_url, _requests, server) = spawn_http_server(vec![HttpReply::ok(
        "text/event-stream",
        completed_sse("resp_not_compacted", &ordinary),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;

    let error = client
        .compact(
            &[user_message("old")],
            CompactionRequest::Automatic(CompactionPhase::MidTurn),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("expected exactly one"));
    assert_eq!(client.window, 0);
    server.join().unwrap();
}

struct HttpReply {
    status: u16,
    content_type: &'static str,
    body: String,
    headers: Vec<(&'static str, &'static str)>,
}

impl HttpReply {
    fn ok(content_type: &'static str, body: String) -> Self {
        Self::status(200, content_type, body)
    }

    fn status(status: u16, content_type: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type,
            body: body.into(),
            headers: Vec::new(),
        }
    }

    fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }
}

struct CapturedRequest {
    path: String,
    headers: String,
    body: Vec<u8>,
}

fn spawn_http_server(
    replies: Vec<HttpReply>,
) -> (
    String,
    std_mpsc::Receiver<CapturedRequest>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, requests_rx) = std_mpsc::channel();
    let server = thread::spawn(move || {
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            requests_tx.send(request).unwrap();
            let reason = match reply.status {
                200 => "OK",
                400 => "Bad Request",
                401 => "Unauthorized",
                429 => "Too Many Requests",
                _ => "Error",
            };
            let extra_headers = reply
                .headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}\r\n"))
                .collect::<String>();
            write!(
                stream,
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
                reply.status,
                reason,
                reply.content_type,
                reply.body.len(),
                extra_headers,
            )
            .unwrap();
            stream.write_all(reply.body.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
    });
    (format!("http://{address}"), requests_rx, server)
}

fn read_request(stream: &mut TcpStream) -> CapturedRequest {
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
    while bytes.len() - header_end < content_length {
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
    let mut body = bytes[header_end..header_end + content_length].to_vec();
    if headers.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("content-encoding") && value.trim() == "zstd"
        })
    }) {
        body = zstd::stream::decode_all(std::io::Cursor::new(body)).unwrap();
    }
    CapturedRequest {
        path,
        headers,
        body,
    }
}
