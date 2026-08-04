use super::ToolCall;
use super::ToolResult;
use super::ToolRuntime;
use crate::auth::Auth;
use crate::auth::SharedAuth;
use crate::events::AgentEvent;
use crate::web_search::ToolTurnContext;
use crate::web_search::WebSearchClient;
use serde_json::json;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[test]
fn parses_exec_and_wait_calls() {
    assert_eq!(
        ToolCall::from_response_item(&json!({
            "type": "custom_tool_call",
            "call_id": "call-1",
            "name": "exec",
            "input": "text('done')"
        })),
        Some(ToolCall::Custom {
            call_id: "call-1".to_string(),
            name: "exec".to_string(),
            input: "text('done')".to_string(),
        })
    );
    assert_eq!(
        ToolCall::from_response_item(&json!({
            "type": "function_call",
            "call_id": "call-2",
            "name": "wait",
            "arguments": "{\"cell_id\":\"cell-1\"}"
        })),
        Some(ToolCall::Function {
            call_id: "call-2".to_string(),
            name: "wait".to_string(),
            arguments: "{\"cell_id\":\"cell-1\"}".to_string(),
        })
    );
}

#[test]
fn custom_outputs_preserve_structured_content_items() {
    let call = ToolCall::Custom {
        call_id: "call-1".to_string(),
        name: "exec".to_string(),
        input: "text('done')".to_string(),
    };
    let output = ToolResult {
        body: json!([{"type": "input_text", "text": "done"}]),
        preview: "done".to_string(),
        preceding_items: Vec::new(),
    };

    assert_eq!(
        call.output_items(&output),
        vec![json!({
            "type": "custom_tool_call_output",
            "call_id": "call-1",
            "output": [{"type": "input_text", "text": "done"}],
        })]
    );
}

#[tokio::test]
async fn web_search_runs_through_code_mode_and_posts_the_codex_alpha_contract() {
    let (base_url, requests, server) = spawn_search_server();
    let client = reqwest::Client::builder()
        .default_headers(reqwest::header::HeaderMap::from_iter([(
            reqwest::header::HeaderName::from_static("originator"),
            reqwest::header::HeaderValue::from_static("codex_cli_rs"),
        )]))
        .build()
        .unwrap();
    let web_search = WebSearchClient::new(
        client,
        SharedAuth::new(Auth::for_test("token-test")),
        base_url,
        "session-test".to_string(),
    );
    let runtime = ToolRuntime::new(PathBuf::from("."), web_search);
    let call = ToolCall::Custom {
        call_id: "call-web".to_string(),
        name: "exec".to_string(),
        input: r#"
const result = await tools.web__run({
  search_query: [{ q: "standalone web search", domains: ["openai.com"] }],
  open: [{ ref_id: "https://openai.com", lineno: 12 }],
});
text(result);
"#
        .to_string(),
    };
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let output = call
        .execute(
            &runtime,
            ToolTurnContext::from_history(
                &[json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "search now"}],
                })],
                r#"{"turn_id":"turn-test"}"#.to_string(),
            ),
            Some(events_tx),
            CancellationToken::new(),
        )
        .await;

    let nested_call_id = match events_rx.recv().await.unwrap() {
        AgentEvent::ToolStarted {
            call_id,
            name,
            input,
        } => {
            assert_eq!(name, "web.run");
            assert_eq!(
                input,
                Some(json!({
                    "search_query": [{
                        "q": "standalone web search",
                        "domains": ["openai.com"],
                    }],
                    "open": [{"ref_id": "https://openai.com", "lineno": 12}],
                }))
            );
            call_id
        }
        event => panic!("expected web search start event, got {event:?}"),
    };
    assert!(matches!(
        events_rx.recv().await.unwrap(),
        AgentEvent::ToolCompleted {
            call_id,
            output: Ok(output),
            ..
        } if call_id == nested_call_id && output == json!("Search result")
    ));
    assert!(events_rx.try_recv().is_err());

    assert!(
        output.preview.contains("Search result"),
        "{}",
        output.preview
    );
    assert_eq!(
        call.output_items(&output),
        vec![json!({
            "type": "custom_tool_call_output",
            "call_id": "call-web",
            "output": output.body.clone(),
        })]
    );
    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.path, "/alpha/search");
    assert!(request.headers.contains("authorization: Bearer token-test"));
    assert!(request.headers.contains("chatgpt-account-id: test-account"));
    assert!(request.headers.contains("originator: codex_cli_rs"));
    assert!(
        request
            .headers
            .contains(r#"x-codex-turn-metadata: {"turn_id":"turn-test"}"#)
    );
    assert!(
        !request
            .headers
            .contains("x-openai-internal-codex-responses-lite")
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
        json!({
            "id": "session-test",
            "model": crate::MODEL,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "search now"}],
            }],
            "commands": {
                "search_query": [{
                    "q": "standalone web search",
                    "domains": ["openai.com"],
                }],
                "open": [{"ref_id": "https://openai.com", "lineno": 12}],
            },
            "settings": {
                "allowed_callers": ["direct"],
                "external_web_access": true,
            },
            "max_output_tokens": 10_000,
        })
    );
    server.join().unwrap();
}

struct CapturedRequest {
    path: String,
    headers: String,
    body: Vec<u8>,
}

fn spawn_search_server() -> (
    String,
    mpsc::Receiver<CapturedRequest>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, requests_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        requests_tx.send(read_request(&mut stream)).unwrap();
        let body = r#"{"encrypted_output":null,"output":"Search result","results":[]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body,
        )
        .unwrap();
        stream.flush().unwrap();
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
