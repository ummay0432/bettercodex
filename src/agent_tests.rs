use super::*;
use crate::model::ReasoningEffort;
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

#[tokio::test]
async fn model_switch_compacts_previous_history_before_sampling_with_the_new_model() -> Result<()> {
    let root = temporary_root("model-switch");
    let _cleanup = DirectoryCleanup(root.clone());
    let compacted = json!({
        "type": "compaction",
        "id": "cmp_switch",
        "encrypted_content": "opaque model-switch summary",
    });
    let answer = json!({
        "type": "message",
        "id": "msg_after_switch",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "switched"}],
    });
    let (base_url, requests, server) = serve_responses(vec![
        (
            200,
            completed_sse(
                "resp_compact",
                &ModelSelection::default().model,
                &compacted,
                true,
            ),
        ),
        (200, completed_sse("resp_sample", "gpt-5.5", &answer, false)),
    ]);
    let (mut agent, previous, target) =
        model_switch_agent(&root, base_url, TestHistory::PreviousModel)?;

    let turn_id = agent.api.begin_turn().to_string();
    agent.conversation.start_turn(&turn_id)?;
    assert!(
        agent
            .prepare_model_switch(
                &None,
                &CancellationToken::new(),
                &ActiveTurnContext::default(),
            )
            .await?
    );
    sample_once(&mut agent, "after switch").await?;
    agent
        .conversation
        .finish_turn(&turn_id, TurnOutcome::Completed)?;

    let compact_request = requests.recv_timeout(Duration::from_secs(2))?;
    let sample_request = requests.recv_timeout(Duration::from_secs(2))?;
    server
        .join()
        .map_err(|_| anyhow!("model-switch test server panicked"))?;

    assert_eq!(compact_request["model"], previous.model);
    assert_eq!(sample_request["model"], target.model);
    assert!(!compact_request.to_string().contains("after switch"));
    assert!(sample_request.to_string().contains("after switch"));
    let metadata: Value = serde_json::from_str(
        compact_request["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .ok_or_else(|| anyhow!("compaction request omitted turn metadata"))?,
    )?;
    assert_eq!(metadata["compaction"]["trigger"], "auto");
    assert_eq!(metadata["compaction"]["reason"], "comp_hash_changed");
    assert_eq!(metadata["compaction"]["phase"], "pre_turn");
    assert_eq!(agent.conversation.history_model_selection(), Some(&target));
    Ok(())
}

#[tokio::test]
async fn model_switch_before_the_first_request_does_not_compact_unused_history() -> Result<()> {
    let root = temporary_root("fresh-model-switch");
    let _cleanup = DirectoryCleanup(root.clone());
    let answer = json!({
        "type": "message",
        "id": "msg_first_request",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "first request"}],
    });
    let (base_url, requests, server) = serve_responses(vec![(
        200,
        completed_sse("resp_first", "gpt-5.5", &answer, false),
    )]);
    let (mut agent, _previous, target) = model_switch_agent(&root, base_url, TestHistory::Fresh)?;

    let turn_id = agent.api.begin_turn().to_string();
    agent.conversation.start_turn(&turn_id)?;
    assert!(
        agent
            .prepare_model_switch(
                &None,
                &CancellationToken::new(),
                &ActiveTurnContext::default(),
            )
            .await?
    );
    assert_eq!(agent.conversation.history_model_selection(), None);
    sample_once(&mut agent, "first prompt").await?;
    agent
        .conversation
        .finish_turn(&turn_id, TurnOutcome::Completed)?;

    let request = requests.recv_timeout(Duration::from_secs(2))?;
    server
        .join()
        .map_err(|_| anyhow!("fresh model-switch test server panicked"))?;
    assert_eq!(request["model"], target.model);
    assert!(request.to_string().contains("first prompt"));
    assert_eq!(agent.conversation.history_model_selection(), Some(&target));
    Ok(())
}

#[tokio::test]
async fn model_switch_compaction_retries_a_retired_previous_model_with_the_selected_model()
-> Result<()> {
    let root = temporary_root("model-switch-fallback");
    let _cleanup = DirectoryCleanup(root.clone());
    let compacted = json!({
        "type": "compaction",
        "id": "cmp_fallback",
        "encrypted_content": "opaque fallback summary",
    });
    let (base_url, requests, server) = serve_responses(vec![
        (400, "retired model".to_string()),
        (
            200,
            completed_sse("resp_fallback", "gpt-5.5", &compacted, false),
        ),
    ]);
    let (mut agent, previous, target) =
        model_switch_agent(&root, base_url, TestHistory::PreviousModel)?;

    let turn_id = agent.api.begin_turn().to_string();
    agent.conversation.start_turn(&turn_id)?;
    assert!(
        agent
            .prepare_model_switch(
                &None,
                &CancellationToken::new(),
                &ActiveTurnContext::default(),
            )
            .await?
    );
    agent
        .conversation
        .finish_turn(&turn_id, TurnOutcome::Completed)?;

    let previous_request = requests.recv_timeout(Duration::from_secs(2))?;
    let fallback_request = requests.recv_timeout(Duration::from_secs(2))?;
    server
        .join()
        .map_err(|_| anyhow!("model-switch fallback test server panicked"))?;

    assert_eq!(previous_request["model"], previous.model);
    assert_eq!(fallback_request["model"], target.model);
    assert_eq!(agent.conversation.history_model_selection(), Some(&target));
    Ok(())
}

fn temporary_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bettercodex-agent-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

enum TestHistory {
    Fresh,
    PreviousModel,
}

fn model_switch_agent(
    root: &Path,
    base_url: String,
    history: TestHistory,
) -> Result<(Agent, ModelSelection, ModelSelection)> {
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd)?;
    let previous = ModelSelection {
        prefer_websocket: false,
        ..ModelSelection::default()
    };
    let rollout = Rollout::create_in_with_selection(root, &cwd, &previous)?;
    let mut conversation = Conversation::new(&cwd, rollout)?;
    if matches!(history, TestHistory::PreviousModel) {
        conversation.record_history_model_selection(&previous)?;
    }
    conversation.set_model_selection(previous.clone())?;
    let api = ApiClient::new_with_base_url(
        Auth::for_test("token-test"),
        conversation.identity(),
        0,
        previous.clone(),
        ServiceTier::default(),
        base_url,
    )?;
    let tools = ToolRuntime::new(
        cwd.clone(),
        api.web_search_client(),
        api.openai_docs_client(),
    );
    let mut agent = Agent {
        cwd,
        api,
        conversation,
        tools,
        resumed_transcript: Vec::new(),
    };
    let mut target = crate::model::bundled_models()[3].selection(ReasoningEffort::Medium);
    target.prefer_websocket = false;
    agent.set_model_selection(target.clone())?;
    Ok((agent, previous, target))
}

async fn sample_once(agent: &mut Agent, prompt: &str) -> Result<()> {
    agent.conversation.extend([user_message(prompt)])?;
    let control = TurnControl::cancellation_only(CancellationToken::new());
    match agent.sample_with_recovery(&None, &control).await? {
        SamplingOutcome::Response(_) => Ok(()),
        SamplingOutcome::Cancelled => Err(anyhow!("sampling was unexpectedly cancelled")),
    }
}

fn user_message(text: &str) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": text}],
    })
}

fn completed_sse(response_id: &str, model: &str, item: &Value, lite: bool) -> String {
    let item_done = json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": item,
    });
    let mut response = json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "model": model,
            "output": [item],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 2,
                "total_tokens": 12,
            },
        },
    });
    if lite {
        response["response"]["reasoning"] = json!({"context": "all_turns"});
    }
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
