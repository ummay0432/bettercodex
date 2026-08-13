use super::*;
use futures_util::SinkExt;
use futures_util::StreamExt;
use serde_json::json;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

struct DirectoryCleanup(PathBuf);

impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_mode_notify_follows_the_terminal_exec_output_in_model_history() -> Result<()> {
    let root = temporary_root("code-mode-notify-order");
    let _cleanup = DirectoryCleanup(root.clone());
    let tool_call = json!({
        "type": "custom_tool_call",
        "id": "ctc_notify",
        "call_id": "call_notify",
        "name": "exec",
        "input": "await tools.update_plan({plan: [{step: 'exercise nested dispatch', status: 'completed'}]}); notify('notification marker'); text('terminal marker');",
    });
    let answer = json!({
        "type": "message",
        "id": "msg_notify_answer",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "notification observed"}],
    });
    let selection = ModelSelection::default();
    let (base_url, requests, server) = serve_responses(vec![
        (
            200,
            completed_sse("resp_notify_exec", &selection.model, &tool_call),
        ),
        (
            200,
            completed_sse("resp_notify_answer", &selection.model, &answer),
        ),
    ]);
    let mut agent = test_agent(&root, base_url, selection)?;

    assert_eq!(
        agent.submit("exercise exec notify").await?,
        "notification observed"
    );

    let _first_request = requests.recv_timeout(Duration::from_secs(2))?;
    let second_request = requests.recv_timeout(Duration::from_secs(2))?;
    server
        .join()
        .map_err(|_| anyhow!("code mode notify test server panicked"))?;

    let input = second_request["input"]
        .as_array()
        .ok_or_else(|| anyhow!("code mode follow-up request omitted input"))?;
    let terminal_index = input
        .iter()
        .position(|item| {
            item["type"] == "custom_tool_call_output"
                && item["call_id"] == "call_notify"
                && item.get("name").is_none()
                && item["output"].to_string().contains("terminal marker")
        })
        .ok_or_else(|| anyhow!("code mode follow-up omitted terminal exec output"))?;
    let notification_index = input
        .iter()
        .position(|item| {
            item["type"] == "custom_tool_call_output"
                && item["call_id"] == "call_notify"
                && item["name"] == "exec"
                && item["output"] == "notification marker"
        })
        .ok_or_else(|| anyhow!("code mode follow-up omitted notify output"))?;
    assert!(
        terminal_index < notification_index,
        "notify output must follow the terminal exec output: {input:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_warmup_starts_before_first_submission() -> Result<()> {
    let root = temporary_root("startup-websocket-prewarm");
    let _cleanup = DirectoryCleanup(root.clone());
    let selection = ModelSelection::default();
    let (base_url, mut requests_rx, server) =
        spawn_successful_startup_websocket_server(selection.model.clone()).await?;
    let mut agent = test_agent_with_transport(&root, base_url, selection, true)?;
    let warmup = tokio::time::timeout(Duration::from_secs(2), requests_rx.recv())
        .await
        .context("startup warmup did not begin before submission")?;
    let Some(warmup) = warmup else {
        return match server.await.context("startup warmup server task failed")? {
            Ok(()) => Err(anyhow!(
                "startup warmup server closed before receiving a request"
            )),
            Err(error) => Err(error.context("startup warmup server failed before receiving")),
        };
    };
    assert_eq!(warmup["type"], "response.create");
    assert_eq!(warmup["generate"], false);

    assert_eq!(agent.submit("hello").await?, "ready");
    let turn = tokio::time::timeout(Duration::from_secs(2), requests_rx.recv())
        .await
        .context("first turn did not reuse the startup connection")?
        .context("startup warmup server closed before the first turn")?;
    assert_eq!(turn["type"], "response.create");
    assert_eq!(turn["previous_response_id"], "warm-1");
    assert_eq!(
        turn["client_metadata"]["x-codex-turn-state"],
        "startup-turn-state"
    );
    server
        .await
        .context("startup warmup server task failed")??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_first_turn_uses_updated_fast_tier_after_startup_prewarm() -> Result<()> {
    let root = temporary_root("startup-websocket-prewarm-fast");
    let _cleanup = DirectoryCleanup(root.clone());
    let selection = ModelSelection::default();
    let (base_url, mut requests_rx, server) =
        spawn_successful_startup_websocket_server(selection.model.clone()).await?;
    let mut agent = test_agent_with_transport(&root, base_url, selection, true)?;

    let warmup = tokio::time::timeout(Duration::from_secs(2), requests_rx.recv())
        .await
        .context("startup warmup did not begin before Fast mode changed")?
        .context("startup warmup server closed before Fast mode changed")?;
    assert!(warmup.get("service_tier").is_none());
    agent.set_service_tier(ServiceTier::Fast)?;

    assert_eq!(agent.submit("hello").await?, "ready");
    let turn = tokio::time::timeout(Duration::from_secs(2), requests_rx.recv())
        .await
        .context("first Fast mode turn did not reuse the startup connection")?
        .context("startup warmup server closed before the first Fast mode turn")?;
    assert_eq!(turn["service_tier"], "priority");
    assert!(turn.get("previous_response_id").is_none());
    assert!(
        turn["input"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    server
        .await
        .context("startup warmup Fast mode server task failed")??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_warmup_failure_is_not_retried_on_first_submission() -> Result<()> {
    let root = temporary_root("startup-websocket-prewarm-failure");
    let _cleanup = DirectoryCleanup(root.clone());
    let selection = ModelSelection::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_model = selection.model.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut websocket =
            tokio_tungstenite::accept_async_with_config(stream, Some(websocket_server_config()))
                .await?;
        let warmup = receive_websocket_json(&mut websocket).await?;
        if warmup["generate"] != false {
            return Err(anyhow!("startup request was not a warmup: {warmup}"));
        }
        send_websocket_json(
            &mut websocket,
            &json!({
                "type": "response.failed",
                "response": {
                    "error": {"code": "server_error", "message": "warmup failed"},
                },
            }),
        )
        .await?;
        drop(websocket);

        let (stream, _) = listener.accept().await?;
        let mut websocket =
            tokio_tungstenite::accept_async_with_config(stream, Some(websocket_server_config()))
                .await?;
        let turn = receive_websocket_json(&mut websocket).await?;
        if turn.get("generate").is_some() {
            return Err(anyhow!("first turn retried startup warmup: {turn}"));
        }
        send_websocket_answer(
            &mut websocket,
            &server_model,
            "resp-after-failed-prewarm",
            "msg_after_failed_prewarm",
            "recovered",
        )
        .await?;
        Ok::<_, anyhow::Error>(())
    });

    let mut agent = test_agent_with_transport(&root, format!("http://{address}"), selection, true)?;
    assert_eq!(agent.submit("hello").await?, "recovered");
    server
        .await
        .context("startup warmup failure server task failed")??;
    Ok(())
}

fn temporary_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bettercodex-agent-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

fn test_agent(root: &Path, base_url: String, selection: ModelSelection) -> Result<Agent> {
    test_agent_with_transport(root, base_url, selection, false)
}

fn test_agent_with_transport(
    root: &Path,
    base_url: String,
    selection: ModelSelection,
    websocket: bool,
) -> Result<Agent> {
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd)?;
    let rollout = Rollout::create_in_with_selection(root, &cwd, &selection)?;
    let mut conversation = Conversation::new(&cwd, rollout)?;
    conversation.set_model_selection(selection.clone())?;
    let mut api = ApiClient::new_with_base_url(
        Auth::for_test("token-test"),
        conversation.identity(),
        0,
        selection,
        ServiceTier::default(),
        crate::tools::ToolConfiguration::default(),
        base_url,
    )?;
    if !websocket {
        api.fall_back_to_http();
    }
    let tools = ToolRuntime::new(
        cwd.clone(),
        api.web_search_client(),
        crate::tools::ToolConfiguration::default(),
    );
    let startup_prewarm = StartupPrewarm::schedule(&api);
    Ok(Agent {
        cwd,
        api,
        startup_prewarm,
        conversation,
        tools,
        resumed_transcript: Vec::new(),
        transcript_checkpoint: None,
        forked_from: None,
    })
}

async fn spawn_successful_startup_websocket_server(
    server_model: String,
) -> Result<(
    String,
    tokio::sync::mpsc::UnboundedReceiver<Value>,
    tokio::task::JoinHandle<Result<()>>,
)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (requests_tx, requests_rx) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut websocket =
            tokio_tungstenite::accept_async_with_config(stream, Some(websocket_server_config()))
                .await?;

        requests_tx
            .send(receive_websocket_json(&mut websocket).await?)
            .map_err(|_| anyhow!("startup warmup receiver closed"))?;
        send_websocket_json(
            &mut websocket,
            &json!({
                "type": "response.metadata",
                "headers": {"x-codex-turn-state": "startup-turn-state"},
            }),
        )
        .await?;
        send_websocket_json(
            &mut websocket,
            &json!({
                "type": "response.completed",
                "response": {
                    "id": "warm-1",
                    "model": server_model,
                    "output": [],
                    "reasoning": {"context": "all_turns"},
                },
            }),
        )
        .await?;

        requests_tx
            .send(receive_websocket_json(&mut websocket).await?)
            .map_err(|_| anyhow!("first-turn request receiver closed"))?;
        send_websocket_answer(
            &mut websocket,
            &server_model,
            "resp-1",
            "msg_startup_prewarm",
            "ready",
        )
        .await
    });
    Ok((format!("http://{address}"), requests_rx, server))
}

async fn receive_websocket_json(
    websocket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) -> Result<Value> {
    let message = websocket
        .next()
        .await
        .context("websocket closed before the next request")??;
    let tungstenite::Message::Text(text) = message else {
        return Err(anyhow!(
            "expected a text websocket request, got {message:?}"
        ));
    };
    serde_json::from_str(text.as_str()).context("failed to decode websocket request")
}

async fn send_websocket_answer(
    websocket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    server_model: &str,
    response_id: &str,
    message_id: &str,
    text: &str,
) -> Result<()> {
    let answer = json!({
        "type": "message",
        "id": message_id,
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": text}],
    });
    send_websocket_json(
        websocket,
        &json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": answer,
        }),
    )
    .await?;
    send_websocket_json(
        websocket,
        &json!({
            "type": "response.completed",
            "response": {
                "id": response_id,
                "model": server_model,
                "output": [answer],
                "reasoning": {"context": "all_turns"},
            },
        }),
    )
    .await
}

async fn send_websocket_json(
    websocket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    event: &Value,
) -> Result<()> {
    websocket
        .send(tungstenite::Message::Text(event.to_string().into()))
        .await
        .context("failed to send websocket event")
}

fn websocket_server_config() -> tungstenite::protocol::WebSocketConfig {
    let mut extensions = tungstenite::extensions::ExtensionsConfig::default();
    extensions.permessage_deflate =
        Some(tungstenite::extensions::compression::deflate::DeflateConfig::default());
    let mut config = tungstenite::protocol::WebSocketConfig::default();
    config.extensions = extensions;
    config
}

fn completed_sse(response_id: &str, model: &str, item: &Value) -> String {
    let item_done = json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": item,
    });
    let response = json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "model": model,
            "output": [item],
            "reasoning": {"context": "all_turns"},
            "usage": {
                "input_tokens": 10,
                "output_tokens": 2,
                "total_tokens": 12,
            },
        },
    });
    format!("data: {item_done}\n\ndata: {response}\n\n")
}

fn serve_responses(
    replies: Vec<(u16, String)>,
) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind test server");
    let address = server.server_addr().to_ip().expect("test server address");
    let (requests_tx, requests_rx) = mpsc::channel();
    let task = thread::spawn(move || {
        for (status, reply) in replies {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .expect("receive Responses request")
                .expect("Responses request timed out");
            let compressed = request.headers().iter().any(|header| {
                header.field.equiv("content-encoding") && header.value.as_str() == "zstd"
            });
            let mut body = Vec::new();
            request
                .as_reader()
                .read_to_end(&mut body)
                .expect("read Responses request");
            if compressed {
                body = zstd::stream::decode_all(std::io::Cursor::new(body))
                    .expect("decode Responses request");
            }
            requests_tx
                .send(serde_json::from_slice(&body).expect("decode Responses request JSON"))
                .expect("capture Responses request");
            let content_type = tiny_http::Header::from_bytes(
                b"content-type".as_slice(),
                b"text/event-stream".as_slice(),
            )
            .expect("build content-type header");
            request
                .respond(
                    tiny_http::Response::from_string(reply)
                        .with_status_code(tiny_http::StatusCode(status))
                        .with_header(content_type),
                )
                .expect("send Responses reply");
        }
    });
    (format!("http://{address}"), requests_rx, task)
}
