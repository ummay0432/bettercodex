use super::*;
use futures::SinkExt;
use futures::StreamExt;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

fn test_client(base_url: String) -> ApiClient {
    let identity = SessionIdentity {
        installation_id: "installation-test".to_string(),
        session_id: "session-test".to_string(),
        thread_id: "thread-test".to_string(),
    };
    ApiClient::new_with_base_url(Auth::for_test("token-test"), &identity, 0, base_url).unwrap()
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

fn completed_sse(response_id: &str, item: &Value) -> String {
    let item_done = json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": item,
    });
    let completed = completed_event(response_id, item);
    format!("data: {item_done}\n\ndata: {completed}\n\n")
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
fn sse_decoder_handles_split_crlf_and_multiline_frames() {
    let mut decoder = SseDecoder::default();
    assert_eq!(
        decoder.push(b"event: ignored\r\nda").unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        decoder
            .push(b"ta: {\"type\":\r\ndata: \"response.created\"}\r\n\r\n")
            .unwrap(),
        vec!["{\"type\":\n\"response.created\"}".to_string()]
    );
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
fn extracts_text_and_forwards_streaming_events() {
    assert_eq!(text_from_items(&[assistant_item("done")]), "done");
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
        r#"{"type":"response.reasoning_summary_text.delta","delta":"checking"}"#,
        &mut collected,
        &completed_items,
        Some(&events),
    )
    .unwrap();

    assert_eq!(
        received.try_recv().unwrap(),
        AgentEvent::ModelTextDelta("hello".to_string())
    );
    assert_eq!(
        received.try_recv().unwrap(),
        AgentEvent::ReasoningDelta("checking".to_string())
    );
}

#[test]
fn output_item_completion_closes_the_visible_stream() {
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();

    process_event_value(
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": assistant_item("done"),
        }),
        &mut collected,
        &completed_items,
        Some(&events),
    )
    .unwrap();

    assert_eq!(received.try_recv().unwrap(), AgentEvent::ModelItemCompleted);
}

#[test]
fn request_has_one_stable_prefix_and_explicit_cache_breakpoint() {
    let client = test_client("http://127.0.0.1:1".to_string());
    let first = client.build_request(&[user_message("one")]);
    let second = client.build_request(&[user_message("one"), user_message("two")]);
    let first_input = first["input"].as_array().unwrap();
    let second_input = second["input"].as_array().unwrap();

    assert_eq!(first["model"], MODEL);
    assert_eq!(
        first["reasoning"],
        json!({"effort": "max", "context": "all_turns"})
    );
    assert_eq!(
        first["prompt_cache_options"],
        json!({"mode": "explicit", "ttl": "30m"})
    );
    assert_eq!(first["prompt_cache_key"], "session-test");
    assert_eq!(first_input[0]["type"], "additional_tools");
    assert_eq!(first_input[1]["role"], "developer");
    assert_eq!(
        first_input[1]["content"][0]["prompt_cache_breakpoint"],
        json!({"mode": "explicit"})
    );
    assert_eq!(&first_input[..2], &second_input[..2]);
    assert_eq!(
        serde_json::to_string(&first_input[..2]).unwrap().len(),
        11_920,
        "update prompts/tool-context.md"
    );

    let mut retained_prefix = first_input.to_vec();
    retained_prefix[0]["id"] = json!("at_compacted");
    retained_prefix[0]["status"] = json!("completed");
    let recomposed = compose_input(&retained_prefix, true);
    assert_eq!(
        recomposed
            .iter()
            .filter(|item| item["type"] == "additional_tools")
            .count(),
        1
    );
    assert_eq!(
        recomposed
            .iter()
            .filter(|item| is_system_prompt_item(item))
            .count(),
        1
    );
}

#[test]
fn websocket_delta_requires_an_exact_prefix_and_new_input() {
    let mut client = test_client("http://127.0.0.1:1".to_string());
    let first_request = client.build_request(&[user_message("one")]);
    let output = vec![assistant_item("first")];
    client.websocket_baseline = Some(WebSocketBaseline {
        request: first_request.clone(),
        response_id: "resp_1".to_string(),
        output: output.clone(),
    });
    let next_history = [vec![user_message("one")], output, vec![user_message("two")]].concat();
    let next_request = client.build_request(&next_history);
    let delta = client.websocket_request(&next_request).unwrap();
    assert_eq!(delta["previous_response_id"], "resp_1");
    assert_eq!(delta["input"], json!([user_message("two")]));

    let unchanged = client.websocket_request(&first_request).unwrap();
    assert!(unchanged.get("previous_response_id").is_none());

    let mut changed = next_request;
    changed["text"]["verbosity"] = json!("medium");
    let full = client.websocket_request(&changed).unwrap();
    assert!(full.get("previous_response_id").is_none());
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
}

#[tokio::test]
async fn http_transport_sends_the_contract_and_collects_the_response() {
    let item = assistant_item("http");
    let (base_url, requests, server) = spawn_http_server(vec![HttpReply::ok(
        "text/event-stream",
        completed_sse("resp_http", &item),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let response = client
        .respond(&[user_message("hello")], &completed_items)
        .await
        .unwrap();
    assert_eq!(response.text, "http");
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
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], MODEL);
    assert_eq!(
        body["input"].as_array().unwrap().last().unwrap(),
        &user_message("hello")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn unsupported_explicit_cache_retries_without_cache_fields() {
    let item = assistant_item("fallback");
    let (base_url, requests, server) = spawn_http_server(vec![
        HttpReply::status(
            400,
            "application/json",
            r#"{"error":{"message":"Unknown parameter: prompt_cache_options"}}"#,
        ),
        HttpReply::ok("text/event-stream", completed_sse("resp_fallback", &item)),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let response = client
        .respond(&[user_message("hello")], &completed_items)
        .await
        .unwrap();
    assert_eq!(response.text, "fallback");
    assert!(!client.explicit_cache);

    let first: Value =
        serde_json::from_slice(&requests.recv_timeout(Duration::from_secs(2)).unwrap().body)
            .unwrap();
    let second: Value =
        serde_json::from_slice(&requests.recv_timeout(Duration::from_secs(2)).unwrap().body)
            .unwrap();
    assert!(first.get("prompt_cache_options").is_some());
    assert!(
        first["input"][1]["content"][0]
            .get("prompt_cache_breakpoint")
            .is_some()
    );
    assert!(second.get("prompt_cache_options").is_none());
    assert!(
        second["input"][1]["content"][0]
            .get("prompt_cache_breakpoint")
            .is_none()
    );
    server.join().unwrap();
}

#[tokio::test]
async fn repeated_cache_parameter_error_stops_after_the_single_fallback() {
    let error_body = r#"{"error":{"message":"Unsupported prompt_cache_options"}}"#;
    let (base_url, requests, server) = spawn_http_server(vec![
        HttpReply::status(400, "application/json", error_body),
        HttpReply::status(400, "application/json", error_body),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let error = match client
        .respond(&[user_message("hello")], &completed_items)
        .await
    {
        Ok(_) => panic!("the repeated parameter error should be returned"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("Unsupported prompt_cache_options")
    );
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(requests.try_recv().is_err());
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
        .respond(&[user_message("hello")], &completed_items)
        .await
        .unwrap();
    assert_eq!(response.text, "https fallback");
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
async fn compaction_accepts_only_the_canonical_compaction_window() {
    let mut retained = user_message("retained");
    retained["id"] = json!("msg_retained");
    retained["status"] = json!("completed");
    let canonical = vec![
        retained,
        json!({"type": "compaction_summary", "id": "cmp_fixture", "encrypted_content": "opaque"}),
    ];
    let body = json!({
        "output": canonical,
        "usage": {"input_tokens": 100, "output_tokens": 10, "total_tokens": 110},
    });
    let (base_url, requests, server) =
        spawn_http_server(vec![HttpReply::ok("application/json", body.to_string())]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;

    let compacted = client.compact(&[user_message("old")]).await.unwrap();
    assert_eq!(compacted.items, canonical);
    assert_eq!(compacted.usage.unwrap().total_tokens, 110);
    assert_eq!(client.window, 1);
    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.path, "/responses/compact");
    let request_body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(request_body["reasoning"]["context"], "all_turns");
    assert!(request_body.get("prompt_cache_options").is_none());
    server.join().unwrap();
}

#[tokio::test]
async fn compaction_rejects_a_response_without_one_opaque_item() {
    let (base_url, _requests, server) = spawn_http_server(vec![HttpReply::ok(
        "application/json",
        json!({"output": [user_message("not compacted")]}).to_string(),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;

    let error = client.compact(&[user_message("old")]).await.unwrap_err();
    assert!(error.to_string().contains("expected exactly one"));
    assert_eq!(client.window, 0);
    server.join().unwrap();
}

#[tokio::test]
async fn websocket_transport_reuses_a_response_with_an_input_delta() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, mut requests_rx) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        for (index, text) in [Some("first"), None, Some("second")]
            .into_iter()
            .enumerate()
        {
            let request = websocket.next().await.unwrap().unwrap();
            let Message::Text(request) = request else {
                panic!("expected a text request")
            };
            requests_tx
                .send(serde_json::from_str::<Value>(&request).unwrap())
                .unwrap();
            let Some(text) = text else {
                websocket
                    .send(Message::Text(
                        json!({
                            "type": "error",
                            "error": {
                                "code": "previous_response_not_found",
                                "message": "connection-local response expired",
                            },
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                continue;
            };
            let item = assistant_item(text);
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": item,
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            websocket
                .send(Message::Text(
                    completed_event(if index == 0 { "resp_1" } else { "resp_2" }, &item)
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        }
    });
    let mut client = test_client(format!("http://{address}"));
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut history = vec![user_message("one")];
    let first = client.respond(&history, &completed_items).await.unwrap();
    history.extend(first.items);
    history.push(user_message("two"));
    let second = client.respond(&history, &completed_items).await.unwrap();
    assert_eq!(second.text, "second");

    let first_request = requests_rx.recv().await.unwrap();
    let delta_request = requests_rx.recv().await.unwrap();
    let recovered_request = requests_rx.recv().await.unwrap();
    assert_eq!(first_request["type"], "response.create");
    assert!(first_request.get("previous_response_id").is_none());
    assert_eq!(delta_request["previous_response_id"], "resp_1");
    assert_eq!(delta_request["input"], json!([user_message("two")]));
    assert!(recovered_request.get("previous_response_id").is_none());
    assert_eq!(
        recovered_request["input"].as_array().unwrap().last(),
        Some(&user_message("two"))
    );
    server.await.unwrap();
}

struct HttpReply {
    status: u16,
    content_type: &'static str,
    body: String,
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
        }
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
            write!(
                stream,
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.status,
                reason,
                reply.content_type,
                reply.body.len(),
                reply.body,
            )
            .unwrap();
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
    CapturedRequest {
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}
