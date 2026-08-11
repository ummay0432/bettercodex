use super::*;
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

fn temporary_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bettercodex-agent-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

fn test_agent(root: &Path, base_url: String, selection: ModelSelection) -> Result<Agent> {
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
        base_url,
    )?;
    api.fall_back_to_http();
    let tools = ToolRuntime::new(
        cwd.clone(),
        api.web_search_client(),
        api.openai_docs_client(),
    );
    Ok(Agent {
        cwd,
        api,
        conversation,
        tools,
        resumed_transcript: Vec::new(),
        transcript_checkpoint: None,
        forked_from: None,
    })
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
