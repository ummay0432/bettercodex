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
async fn direct_model_executes_native_apply_patch_and_returns_its_output_to_the_model() -> Result<()>
{
    let root = temporary_root("direct-apply-patch");
    let _cleanup = DirectoryCleanup(root.clone());
    let patch = "*** Begin Patch\n*** Add File: direct.txt\n+direct tool route\n*** End Patch";
    let tool_call = json!({
        "type": "custom_tool_call",
        "id": "ctc_direct_patch",
        "call_id": "call_direct_patch",
        "name": "apply_patch",
        "input": patch,
    });
    let answer = json!({
        "type": "message",
        "id": "msg_direct_patch",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "patched directly"}],
    });
    let (base_url, requests, server) = serve_responses(vec![
        (
            200,
            completed_sse("resp_direct_patch", "gpt-5.5", &tool_call, false),
        ),
        (
            200,
            completed_sse("resp_direct_answer", "gpt-5.5", &answer, false),
        ),
    ]);
    let (mut agent, _previous, target) = model_switch_agent(&root, base_url, TestHistory::Fresh)?;
    assert_eq!(target.tool_mode(), crate::model::ToolMode::Direct);

    let result = agent.submit("create a file with apply_patch").await?;

    assert_eq!(result, "patched directly");
    assert_eq!(
        std::fs::read_to_string(agent.cwd().join("direct.txt"))?,
        "direct tool route\n"
    );

    let first_request = requests.recv_timeout(Duration::from_secs(2))?;
    let second_request = requests.recv_timeout(Duration::from_secs(2))?;
    server
        .join()
        .map_err(|_| anyhow!("direct apply_patch test server panicked"))?;

    let advertised_tools = first_request["tools"]
        .as_array()
        .ok_or_else(|| anyhow!("direct Responses request omitted tools"))?;
    assert!(
        advertised_tools
            .iter()
            .any(|tool| { tool["type"] == "custom" && tool["name"] == "apply_patch" })
    );
    assert!(!advertised_tools.iter().any(|tool| tool["name"] == "exec"));
    let follow_up_input = second_request["input"]
        .as_array()
        .ok_or_else(|| anyhow!("direct follow-up request omitted input"))?;
    assert!(follow_up_input.iter().any(|item| item == &tool_call));
    let output = follow_up_input
        .iter()
        .find(|item| {
            item["type"] == "custom_tool_call_output" && item["call_id"] == "call_direct_patch"
        })
        .ok_or_else(|| anyhow!("direct follow-up request omitted apply_patch output"))?;
    assert!(
        output["output"]
            .as_str()
            .is_some_and(|output| output.contains("Success"))
    );
    assert!(agent.conversation.items().contains(output));
    Ok(())
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
    let selection = ModelSelection {
        prefer_websocket: false,
        ..ModelSelection::default()
    };
    let (base_url, requests, server) = serve_responses(vec![
        (
            200,
            completed_sse("resp_notify_exec", &selection.model, &tool_call, true),
        ),
        (
            200,
            completed_sse("resp_notify_answer", &selection.model, &answer, true),
        ),
    ]);
    let (mut agent, _previous, _direct) = model_switch_agent(&root, base_url, TestHistory::Fresh)?;
    agent.set_model_selection(selection)?;

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
    assert_eq!(agent.conversation.items().last(), Some(&answer));
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
async fn model_switch_compacts_when_only_the_effective_context_window_shrinks() -> Result<()> {
    let root = temporary_root("effective-model-downshift");
    let _cleanup = DirectoryCleanup(root.clone());
    let compacted = json!({
        "type": "compaction",
        "id": "cmp_effective_downshift",
        "encrypted_content": "opaque effective-window summary",
    });
    let (base_url, requests, server) = serve_responses(vec![(
        200,
        completed_sse(
            "resp_effective_downshift",
            &ModelSelection::default().model,
            &compacted,
            true,
        ),
    )]);
    let (mut agent, previous, mut target) =
        model_switch_agent(&root, base_url, TestHistory::PreviousModel)?;
    agent.conversation.record_usage(
        Some(crate::usage::TokenUsage {
            input_tokens: 20_000,
            total_tokens: 20_000,
            ..Default::default()
        }),
        true,
    )?;
    target.comp_hash.clone_from(&previous.comp_hash);
    target.raw_context_window = previous.raw_context_window;
    target.effective_context_window_percent = 50;
    target.configured_auto_compact_token_limit = Some(10_000);
    agent.set_model_selection(target.clone())?;

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

    let compact_request = requests.recv_timeout(Duration::from_secs(2))?;
    server
        .join()
        .map_err(|_| anyhow!("effective model-downshift test server panicked"))?;
    assert_eq!(compact_request["model"], previous.model);
    let metadata: Value = serde_json::from_str(
        compact_request["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .ok_or_else(|| anyhow!("compaction request omitted turn metadata"))?,
    )?;
    assert_eq!(metadata["compaction"]["reason"], "model_downshift");
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
        transcript_checkpoint: None,
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
