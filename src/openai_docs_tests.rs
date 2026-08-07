use super::*;
use serde_json::json;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[tokio::test]
async fn stateless_tool_call_posts_mcp_contract_and_unwraps_sse_text() {
    let response = concat!(
        "event: message\n",
        "data: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"official markdown\"}]},\"jsonrpc\":\"2.0\",\"id\":1}\n\n"
    );
    let (endpoint, requests, server) = spawn_server("200 OK", "text/event-stream", response, None);
    let client = OpenAiDocsClient::with_endpoint(reqwest::Client::new(), endpoint);

    let output = client
        .call(
            SEARCH_OPENAI_DOCS,
            json!({"query": "Responses streaming", "limit": 3}),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(output, json!("official markdown"));
    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.path, "/mcp");
    let headers = request.headers.to_ascii_lowercase();
    assert!(
        headers.contains("accept: application/json, text/event-stream"),
        "{}",
        request.headers
    );
    assert!(
        headers.contains("content-type: application/json"),
        "{}",
        request.headers
    );
    assert!(
        headers.contains("mcp-protocol-version: 2025-03-26"),
        "{}",
        request.headers
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "search_openai_docs",
                "arguments": {"query": "Responses streaming", "limit": 3}
            }
        })
    );
    server.join().unwrap();
}

#[tokio::test]
async fn application_json_and_multiple_text_blocks_are_supported() {
    let response = r#"{"result":{"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]},"jsonrpc":"2.0","id":1}"#;
    let (endpoint, _requests, server) = spawn_server("200 OK", "application/json", response, None);
    let client = OpenAiDocsClient::with_endpoint(reqwest::Client::new(), endpoint);

    let output = client
        .call(LIST_API_ENDPOINTS, json!({}), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(output, json!("first\nsecond"));
    server.join().unwrap();
}

#[tokio::test]
async fn tool_and_json_rpc_errors_are_model_readable() {
    let tool_error = r#"{"result":{"content":[{"type":"text","text":"bad URL"}],"isError":true},"jsonrpc":"2.0","id":1}"#;
    let (endpoint, _requests, server) =
        spawn_server("200 OK", "application/json", tool_error, None);
    let client = OpenAiDocsClient::with_endpoint(reqwest::Client::new(), endpoint);
    let error = client
        .call(
            FETCH_OPENAI_DOC,
            json!({"url": "invalid"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "OpenAI Developer Docs tool failed: bad URL"
    );
    server.join().unwrap();

    let rpc_error =
        r#"{"error":{"code":-32602,"message":"Invalid arguments"},"jsonrpc":"2.0","id":1}"#;
    let (endpoint, _requests, server) = spawn_server("200 OK", "application/json", rpc_error, None);
    let client = OpenAiDocsClient::with_endpoint(reqwest::Client::new(), endpoint);
    let error = client
        .call(
            FETCH_OPENAI_DOC,
            json!({"url": "invalid"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "OpenAI Developer Docs returned JSON-RPC error -32602: Invalid arguments"
    );
    server.join().unwrap();
}

#[tokio::test]
async fn declared_oversized_responses_are_rejected_before_the_body_is_read() {
    let declared_length = MAX_RESPONSE_BYTES + 1;
    let (endpoint, _requests, server) =
        spawn_server("200 OK", "application/json", "", Some(declared_length));
    let client = OpenAiDocsClient::with_endpoint(reqwest::Client::new(), endpoint);

    let error = client
        .call(LIST_API_ENDPOINTS, json!({}), CancellationToken::new())
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        format!("OpenAI Developer Docs response exceeds the {MAX_RESPONSE_BYTES}-byte limit")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn unknown_tools_are_rejected_without_network_access() {
    let client =
        OpenAiDocsClient::with_endpoint(reqwest::Client::new(), "http://127.0.0.1:1/unreachable");

    let error = client
        .call("not_a_docs_tool", json!({}), CancellationToken::new())
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "unknown OpenAI Developer Docs tool `not_a_docs_tool`"
    );
}

struct CapturedRequest {
    path: String,
    headers: String,
    body: Vec<u8>,
}

fn spawn_server(
    status: &'static str,
    content_type: &'static str,
    body: &'static str,
    declared_length: Option<usize>,
) -> (
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
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            declared_length.unwrap_or(body.len()),
        )
        .unwrap();
        stream.flush().unwrap();
    });
    (format!("http://{address}/mcp"), requests_rx, server)
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
    CapturedRequest {
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}
