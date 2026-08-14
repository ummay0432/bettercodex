use super::*;
use futures_util::SinkExt;
use futures_util::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

struct DirectoryCleanup(PathBuf);

impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::ImageBuffer::from_pixel(width, height, image::Rgba([10_u8, 20, 30, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_bash_output_is_returned_in_ordinary_function_history() -> Result<()> {
    let root = temporary_root("direct-bash-output");
    let _cleanup = DirectoryCleanup(root.clone());
    let tool_call = json!({
        "type": "function_call",
        "id": "fc_bash",
        "call_id": "call_bash",
        "namespace": "functions",
        "name": "bash",
        "arguments": r#"{"command":"printf terminal-marker"}"#,
    });
    let answer = json!({
        "type": "message",
        "id": "msg_bash_answer",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "command observed"}],
    });
    let selection = ModelSelection::default();
    let (base_url, requests, server) = serve_responses(vec![
        (
            200,
            completed_sse("resp_bash", &selection.model, &tool_call),
        ),
        (200, completed_sse("resp_answer", &selection.model, &answer)),
    ]);
    let mut agent = test_agent(&root, base_url, selection)?;

    assert_eq!(agent.submit("run a command").await?, "command observed");

    let _first_request = requests.recv_timeout(Duration::from_secs(2))?;
    let second_request = requests.recv_timeout(Duration::from_secs(2))?.body;
    server
        .join()
        .map_err(|_| anyhow!("direct bash test server panicked"))?;

    let output = second_request["input"]
        .as_array()
        .and_then(|input| {
            input.iter().find(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "call_bash"
            })
        })
        .and_then(|item| item["output"].as_str())
        .ok_or_else(|| anyhow!("follow-up request omitted direct bash output"))?;
    let output: Value = serde_json::from_str(output)?;
    assert_eq!(output["stdout"], "terminal-marker");
    assert_eq!(output["stderr"], "");
    assert_eq!(output["exit_code"], 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_turn_repairs_an_unhandled_function_call() -> Result<()> {
    let root = temporary_root("failed-turn-function-repair");
    let _cleanup = DirectoryCleanup(root.clone());
    let malformed_call = json!({
        "type": "function_call",
        "id": "fc_unhandled",
        "call_id": "call_unhandled",
        "namespace": "functions",
        "name": "bash",
    });
    let selection = ModelSelection::default();
    let (base_url, requests, server) = serve_responses(vec![(
        200,
        completed_sse("resp_unhandled", &selection.model, &malformed_call),
    )]);
    let mut agent = test_agent(&root, base_url, selection)?;

    let error = agent.submit("run an invalid call").await.unwrap_err();
    assert!(error.to_string().contains("no text or tool call"));
    let call_index = agent
        .conversation
        .items()
        .iter()
        .position(|item| item["call_id"] == "call_unhandled")
        .unwrap();
    let output = &agent.conversation.items()[call_index + 1];
    assert_eq!(output["type"], "function_call_output");
    assert_eq!(output["call_id"], "call_unhandled");
    assert_eq!(output["output"], "aborted");

    let _request = requests.recv_timeout(Duration::from_secs(2))?;
    server
        .join()
        .map_err(|_| anyhow!("failed-turn repair test server panicked"))?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hosted_web_search_and_citations_survive_the_next_responses_request() -> Result<()> {
    let root = temporary_root("hosted-web-search-history");
    let _cleanup = DirectoryCleanup(root.clone());
    let search_call = json!({
        "type": "web_search_call",
        "id": "ws_history",
        "status": "completed",
        "action": {
            "type": "search",
            "queries": ["current source"],
        },
    });
    let cited_answer = json!({
        "type": "message",
        "id": "msg_cited_answer",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{
            "type": "output_text",
            "text": "A current answer.[1]",
            "annotations": [{
                "type": "url_citation",
                "start_index": 17,
                "end_index": 20,
                "url": "https://example.com/source",
                "title": "Example source",
            }],
        }],
    });
    let follow_up_answer = json!({
        "type": "message",
        "id": "msg_follow_up",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "history observed"}],
    });
    let selection = ModelSelection::default();
    let (base_url, requests, server) = serve_responses(vec![
        (
            200,
            completed_sse_items(
                "resp_hosted_search",
                &selection.model,
                &[search_call.clone(), cited_answer.clone()],
            ),
        ),
        (
            200,
            completed_sse(
                "resp_hosted_search_follow_up",
                &selection.model,
                &follow_up_answer,
            ),
        ),
    ]);
    let mut agent = test_agent(&root, base_url, selection)?;

    assert_eq!(
        agent.submit("find a current source").await?,
        "A current answer.[1]\n\nSources:\n1. Example source: https://example.com/source"
    );
    assert_eq!(
        agent.submit("use the same source").await?,
        "history observed"
    );

    let _first_request = requests.recv_timeout(Duration::from_secs(2))?;
    let second_request = requests.recv_timeout(Duration::from_secs(2))?.body;
    server
        .join()
        .map_err(|_| anyhow!("hosted web search history test server panicked"))?;
    let input = second_request["input"]
        .as_array()
        .ok_or_else(|| anyhow!("web search follow-up request omitted input"))?;
    let search_index = input
        .iter()
        .position(|item| item == &search_call)
        .ok_or_else(|| anyhow!("follow-up request rewrote or omitted the web search call"))?;
    let answer_index = input
        .iter()
        .position(|item| item == &cited_answer)
        .ok_or_else(|| anyhow!("follow-up request rewrote or omitted citation annotations"))?;
    let follow_up_index = input
        .iter()
        .position(|item| {
            item.get("role").and_then(Value::as_str) == Some("user")
                && item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|content| {
                        content.get("text").and_then(Value::as_str) == Some("use the same source")
                    })
        })
        .ok_or_else(|| anyhow!("web search follow-up request omitted the new prompt"))?;
    assert!(search_index < answer_index && answer_index < follow_up_index);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_shell_output_reaches_the_next_model_turn_as_context() -> Result<()> {
    let root = temporary_root("operator-shell-context");
    let _cleanup = DirectoryCleanup(root.clone());
    let selection = ModelSelection::default();
    let output = Ok(json!({
        "stdout": "operator stdout",
        "stderr": "operator stderr",
        "exit_code": 7,
    }));
    let context = crate::context::user_shell_command_context(
        "printf operator",
        &output,
        selection.truncation_policy(),
    );
    let answer = json!({
        "type": "message",
        "id": "msg_operator_context_answer",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "operator output observed"}],
    });
    let (base_url, requests, server) = serve_responses(vec![(
        200,
        completed_sse("resp_operator_context", &selection.model, &answer),
    )]);
    let mut agent = test_agent(&root, base_url, selection)?;

    agent.record_operator_shell_context(context.clone())?;
    assert!(agent.prompt_history().is_empty());
    assert_eq!(
        agent.submit("explain the operator command").await?,
        "operator output observed"
    );

    let request = requests.recv_timeout(Duration::from_secs(2))?.body;
    server
        .join()
        .map_err(|_| anyhow!("operator context test server panicked"))?;
    let input = request["input"]
        .as_array()
        .ok_or_else(|| anyhow!("operator context request omitted input"))?;
    let contains_text = |item: &Value, expected: &str| {
        item.get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|content| content.get("text").and_then(Value::as_str) == Some(expected))
    };
    let context_index = input
        .iter()
        .position(|item| contains_text(item, &context))
        .ok_or_else(|| anyhow!("operator shell output was absent from model context"))?;
    let prompt_index = input
        .iter()
        .position(|item| contains_text(item, "explain the operator command"))
        .ok_or_else(|| anyhow!("operator context request omitted the user prompt"))?;
    assert!(context_index < prompt_index);
    assert!(context.contains("Exit code: 7"));
    assert!(context.contains("Stdout:\noperator stdout"));
    assert!(context.contains("Stderr:\noperator stderr"));
    assert_eq!(
        agent.prompt_history(),
        vec!["explain the operator command".to_string()]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_tool_calls_execute_concurrently_with_ordered_exclusive_barriers() -> Result<()> {
    let root = temporary_root("parallel-direct-tools");
    let _cleanup = DirectoryCleanup(root.clone());
    let first_call = json!({
        "type": "function_call",
        "id": "fc_first",
        "call_id": "call_first",
        "name": "bash",
        "arguments": r#"{"command":"touch first.started; while [ ! -e second.started ]; do sleep 0.01; done; printf first","timeout":5}"#,
    });
    let second_call = json!({
        "type": "function_call",
        "id": "fc_second",
        "call_id": "call_second",
        "name": "bash",
        "arguments": r#"{"command":"touch second.started; while [ ! -e first.started ]; do sleep 0.01; done; printf second","timeout":5}"#,
    });
    let answer = json!({
        "type": "message",
        "id": "msg_parallel_answer",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "both observed"}],
    });
    let before_mutation = json!({
        "type": "function_call",
        "id": "fc_before_mutation",
        "call_id": "call_before_mutation",
        "name": "bash",
        "arguments": r#"{"command":"sleep 0.2; if [ -e mutation.txt ]; then printf overlap; else printf before; fi","timeout":5}"#,
    });
    let mutation = json!({
        "type": "function_call",
        "id": "fc_mutation",
        "call_id": "call_mutation",
        "name": "write",
        "arguments": r#"{"path":"mutation.txt","content":"done"}"#,
    });
    let after_mutation = json!({
        "type": "function_call",
        "id": "fc_after_mutation",
        "call_id": "call_after_mutation",
        "name": "bash",
        "arguments": r#"{"command":"if [ -e mutation.txt ]; then printf after; else printf bypassed; fi","timeout":5}"#,
    });
    let barrier_answer = json!({
        "type": "message",
        "id": "msg_barrier_answer",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "barrier observed"}],
    });
    let selection = ModelSelection::default();
    let (base_url, requests, server) = serve_responses(vec![
        (
            200,
            completed_sse_items(
                "resp_parallel",
                &selection.model,
                &[first_call, second_call],
            ),
        ),
        (
            200,
            completed_sse("resp_parallel_answer", &selection.model, &answer),
        ),
        (
            200,
            completed_sse_items(
                "resp_barrier",
                &selection.model,
                &[before_mutation, mutation, after_mutation],
            ),
        ),
        (
            200,
            completed_sse("resp_barrier_answer", &selection.model, &barrier_answer),
        ),
    ]);
    let mut agent = test_agent(&root, base_url, selection)?;

    assert_eq!(agent.submit("run both probes").await?, "both observed");

    let _first_request = requests.recv_timeout(Duration::from_secs(2))?;
    let second_request = requests.recv_timeout(Duration::from_secs(2))?.body;

    let outputs = second_request["input"]
        .as_array()
        .ok_or_else(|| anyhow!("parallel follow-up omitted input"))?
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| {
            let output = item["output"]
                .as_str()
                .ok_or_else(|| anyhow!("parallel tool output was not text"))?;
            Ok((
                item["call_id"].clone(),
                serde_json::from_str::<Value>(output)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].0, "call_first");
    assert_eq!(outputs[0].1["stdout"], "first");
    assert_eq!(outputs[0].1["exit_code"], 0);
    assert_eq!(outputs[1].0, "call_second");
    assert_eq!(outputs[1].1["stdout"], "second");
    assert_eq!(outputs[1].1["exit_code"], 0);

    assert_eq!(
        agent.submit("run around one file mutation").await?,
        "barrier observed"
    );
    let _third_request = requests.recv_timeout(Duration::from_secs(2))?;
    let fourth_request = requests.recv_timeout(Duration::from_secs(2))?.body;
    server
        .join()
        .map_err(|_| anyhow!("parallel direct tool test server panicked"))?;
    let outputs = fourth_request["input"]
        .as_array()
        .ok_or_else(|| anyhow!("exclusive follow-up omitted input"))?
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect::<Vec<_>>();
    let before: Value = serde_json::from_str(
        outputs[2]["output"]
            .as_str()
            .ok_or_else(|| anyhow!("before-mutation output was not text"))?,
    )?;
    assert_eq!(before["stdout"], "before");
    assert!(
        outputs[3]["output"]
            .as_str()
            .is_some_and(|output| output.contains("mutation.txt"))
    );
    let after: Value = serde_json::from_str(
        outputs[4]["output"]
            .as_str()
            .ok_or_else(|| anyhow!("after-mutation output was not text"))?,
    )?;
    assert_eq!(after["stdout"], "after");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_parallel_tools_records_every_output_in_model_order() -> Result<()> {
    let root = temporary_root("cancelled-parallel-direct-tools");
    let _cleanup = DirectoryCleanup(root.clone());
    let first_call = json!({
        "type": "function_call",
        "id": "fc_cancel_first",
        "call_id": "call_cancel_first",
        "name": "bash",
        "arguments": r#"{"command":"touch cancel-first.started; while :; do sleep 1; done"}"#,
    });
    let second_call = json!({
        "type": "function_call",
        "id": "fc_cancel_second",
        "call_id": "call_cancel_second",
        "name": "bash",
        "arguments": r#"{"command":"touch cancel-second.started; while :; do sleep 1; done"}"#,
    });
    let trailing_mutation = json!({
        "type": "function_call",
        "id": "fc_cancel_mutation",
        "call_id": "call_cancel_mutation",
        "name": "write",
        "arguments": r#"{"path":"must-not-exist.txt","content":"unexpected"}"#,
    });
    let selection = ModelSelection::default();
    let (base_url, requests, server) = serve_responses(vec![(
        200,
        completed_sse_items(
            "resp_cancel_parallel",
            &selection.model,
            &[first_call, second_call, trailing_mutation],
        ),
    )]);
    let mut agent = test_agent(&root, base_url, selection)?;
    let session_id = agent.session_id().parse::<uuid::Uuid>()?;
    let (handle, control) = TurnControl::channel();
    let cancellation = handle.clone();
    let repo = root.join("repo");
    let cancel_task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if repo.join("cancel-first.started").exists()
                    && repo.join("cancel-second.started").exists()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("parallel tools did not both start before cancellation"))?;
        cancellation.cancel();
        Ok::<_, anyhow::Error>(())
    });
    let (events, _events_rx) = tokio::sync::mpsc::unbounded_channel();

    assert_eq!(
        agent
            .submit_with_control(UserInput::text("run until interrupted"), events, control)
            .await?,
        SubmitOutcome::Cancelled
    );
    cancel_task.await.context("cancellation task failed")??;
    let _request = requests.recv_timeout(Duration::from_secs(2))?;
    server
        .join()
        .map_err(|_| anyhow!("cancelled parallel tool test server panicked"))?;

    let outputs = agent
        .conversation
        .items()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0]["call_id"], "call_cancel_first");
    assert_eq!(outputs[1]["call_id"], "call_cancel_second");
    assert_eq!(outputs[2]["call_id"], "call_cancel_mutation");
    for output in &outputs[..2] {
        let body: Value = serde_json::from_str(
            output["output"]
                .as_str()
                .ok_or_else(|| anyhow!("cancelled Bash output was not text"))?,
        )?;
        assert_eq!(body["exit_code"], 130);
    }
    assert!(
        outputs[2]["output"]
            .as_str()
            .is_some_and(|output| output.contains("interrupted"))
    );
    assert!(!root.join("repo/must-not-exist.txt").exists());
    assert!(
        agent
            .conversation
            .items()
            .last()
            .and_then(|item| item.pointer("/content/0/text"))
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains("<turn_aborted>"))
    );

    drop(agent);
    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &root.join("repo"))?;
    assert!(loaded.unfinished_turn.is_none());
    let resumed_tools = loaded
        .transcript
        .iter()
        .filter_map(|item| match item {
            SessionTranscriptItem::Tool { tool } => Some(tool.call_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        resumed_tools,
        [
            "call_cancel_first",
            "call_cancel_second",
            "call_cancel_mutation"
        ]
    );
    let resumed_outputs = loaded
        .history
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| item["call_id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        resumed_outputs,
        [
            "call_cancel_first",
            "call_cancel_second",
            "call_cancel_mutation"
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_read_image_output_reaches_responses_and_retains_history_detail() -> Result<()> {
    let root = temporary_root("direct-read-image-output");
    let _cleanup = DirectoryCleanup(root.clone());
    std::fs::create_dir_all(root.join("repo"))?;
    std::fs::write(root.join("repo/visual.dat"), png(6401, 1))?;
    let tool_call = json!({
        "type": "function_call",
        "id": "fc_read_image",
        "call_id": "call_read_image",
        "namespace": "functions",
        "name": "read",
        "arguments": r#"{"path":"visual.dat","detail":"original"}"#,
    });
    let answer = json!({
        "type": "message",
        "id": "msg_read_image_answer",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "image observed"}],
    });
    let selection = ModelSelection::default();
    let (base_url, requests, server) = serve_responses(vec![
        (
            200,
            completed_sse("resp_read_image", &selection.model, &tool_call),
        ),
        (
            200,
            completed_sse("resp_read_image_answer", &selection.model, &answer),
        ),
    ]);
    let mut agent = test_agent(&root, base_url, selection)?;

    assert_eq!(agent.submit("inspect the image").await?, "image observed");

    let _first_request = requests.recv_timeout(Duration::from_secs(2))?;
    let second_request = requests.recv_timeout(Duration::from_secs(2))?.body;
    server
        .join()
        .map_err(|_| anyhow!("direct read image test server panicked"))?;

    let request_image = second_request["input"]
        .as_array()
        .and_then(|input| {
            input.iter().find(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "call_read_image"
            })
        })
        .and_then(|item| item["output"].as_array())
        .and_then(|items| items.first())
        .ok_or_else(|| anyhow!("follow-up request omitted direct read image output"))?;
    assert_eq!(request_image["type"], "input_image");
    assert_eq!(request_image["detail"], "original");
    let request_image_url = request_image["image_url"]
        .as_str()
        .filter(|url| url.starts_with("data:image/png;base64,"))
        .ok_or_else(|| anyhow!("direct read image output was not a PNG data URL"))?;
    let (_, request_image_payload) = request_image_url
        .split_once(',')
        .ok_or_else(|| anyhow!("direct read image output omitted its payload"))?;
    use base64::Engine as _;
    use image::GenericImageView as _;
    let request_image_bytes = base64::engine::general_purpose::STANDARD
        .decode(request_image_payload)
        .context("decode direct read image output")?;
    assert_eq!(
        image::load_from_memory(&request_image_bytes)
            .context("decode direct read prepared image")?
            .dimensions(),
        (6401, 1)
    );

    let history_image = agent
        .conversation
        .items()
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "call_read_image")
        .and_then(|item| item["output"].as_array())
        .and_then(|items| items.first())
        .ok_or_else(|| anyhow!("conversation history omitted direct read image output"))?;
    assert_eq!(history_image["detail"], "original");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_input_after_cancelled_startup_prewarm_closes_the_saved_turn() -> Result<()> {
    let root = temporary_root("cancelled-prewarm-input-failure");
    let _cleanup = DirectoryCleanup(root.clone());
    let selection = ModelSelection::default();
    let mut agent = test_agent(&root, "http://127.0.0.1:1".to_string(), selection)?;
    agent.startup_prewarm = Some(StartupPrewarm {
        task: Some(tokio::spawn(std::future::pending::<
            std::result::Result<ApiClient, ApiError>,
        >())),
        started_at: Instant::now(),
    });
    let image = crate::input::PromptImage::from_bytes(
        Path::new("corrupt.png"),
        b"\x89PNG\r\n\x1a\ncorrupt".to_vec(),
        crate::input::ImageDetail::High,
    )?;
    let prompt = crate::input::UserPrompt::with_attachments(
        "[image]",
        Vec::new(),
        vec![crate::input::PromptImageAttachment::new(image, 0..7)],
    );
    let session_id = agent.session_id().parse::<uuid::Uuid>()?;
    let (handle, control) = TurnControl::channel();
    handle.cancel();
    let (events, _events_rx) = tokio::sync::mpsc::unbounded_channel();

    assert!(
        agent
            .submit_with_control(UserInput::prompt(prompt), events, control)
            .await
            .is_err()
    );
    assert!(handle.steer(UserInput::text("late steering")).is_err());

    drop(agent);
    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &root.join("repo"))?;
    assert!(loaded.unfinished_turn.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_pre_turn_compaction_retains_the_submitted_input() -> Result<()> {
    let root = temporary_root("cancelled-pre-turn-compaction-input");
    let _cleanup = DirectoryCleanup(root.clone());
    let selection = ModelSelection::default();
    let mut agent = test_agent(&root, "http://127.0.0.1:1".to_string(), selection)?;
    agent.startup_prewarm = None;
    let compact_at = agent
        .conversation
        .model_selection()
        .auto_compact_token_limit();
    agent.conversation.record_usage(
        Some(crate::usage::TokenUsage {
            input_tokens: compact_at,
            total_tokens: compact_at,
            ..crate::usage::TokenUsage::default()
        }),
        false,
        Vec::new(),
    )?;
    assert!(agent.conversation.needs_compaction());

    let session_id = agent.session_id().parse::<uuid::Uuid>()?;
    let (handle, control) = TurnControl::channel();
    handle.cancel();
    let (events, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let submitted = "retain this input after interrupted compaction";

    assert_eq!(
        agent
            .submit_with_control(UserInput::text(submitted), events, control)
            .await?,
        SubmitOutcome::Cancelled
    );
    assert!(agent.prompt_history().iter().any(|text| text == submitted));

    drop(agent);
    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &root.join("repo"))?;
    assert!(loaded.unfinished_turn.is_none());
    assert!(loaded.history.iter().any(|item| {
        item.get("role").and_then(Value::as_str) == Some("user")
            && item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|content| content.get("text").and_then(Value::as_str) == Some(submitted))
    }));
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
        base_url,
    )?;
    if !websocket {
        api.fall_back_to_http();
    }
    let tools = ToolRuntime::new(cwd.clone());
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
    completed_sse_items(response_id, model, std::slice::from_ref(item))
}

fn completed_sse_items(response_id: &str, model: &str, items: &[Value]) -> String {
    let mut stream = String::new();
    for (output_index, item) in items.iter().enumerate() {
        let item_done = json!({
            "type": "response.output_item.done",
            "output_index": output_index,
            "item": item,
        });
        stream.push_str(&format!("data: {item_done}\n\n"));
    }
    let response = json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "model": model,
            "output": items,
            "reasoning": {"context": "all_turns"},
            "usage": {
                "input_tokens": 10,
                "output_tokens": 2,
                "total_tokens": 12,
            },
        },
    });
    stream.push_str(&format!("data: {response}\n\n"));
    stream
}

fn serve_responses(
    replies: Vec<(u16, String)>,
) -> (
    String,
    mpsc::Receiver<CapturedAgentRequest>,
    thread::JoinHandle<()>,
) {
    serve_agent_http(
        replies
            .into_iter()
            .map(|(status, body)| AgentHttpReply {
                expected_path: "/responses",
                status,
                content_type: "text/event-stream",
                body,
            })
            .collect(),
    )
}

struct AgentHttpReply {
    expected_path: &'static str,
    status: u16,
    content_type: &'static str,
    body: String,
}

struct CapturedAgentRequest {
    body: Value,
}

fn serve_agent_http(
    replies: Vec<AgentHttpReply>,
) -> (
    String,
    mpsc::Receiver<CapturedAgentRequest>,
    thread::JoinHandle<()>,
) {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind test server");
    let address = server.server_addr().to_ip().expect("test server address");
    let (requests_tx, requests_rx) = mpsc::channel();
    let task = thread::spawn(move || {
        for reply in replies {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .expect("receive agent HTTP request")
                .expect("agent HTTP request timed out");
            let path = request.url().to_string();
            assert_eq!(path, reply.expected_path);
            let headers = request
                .headers()
                .iter()
                .map(|header| {
                    (
                        header.field.as_str().to_string().to_ascii_lowercase(),
                        header.value.as_str().to_string(),
                    )
                })
                .collect::<HashMap<_, _>>();
            let compressed = headers
                .get("content-encoding")
                .is_some_and(|value| value == "zstd");
            let mut body = Vec::new();
            request
                .as_reader()
                .read_to_end(&mut body)
                .expect("read Responses request");
            if compressed {
                body = zstd::stream::decode_all(std::io::Cursor::new(body))
                    .expect("decode compressed agent HTTP request");
            }
            requests_tx
                .send(CapturedAgentRequest {
                    body: serde_json::from_slice(&body).expect("decode agent HTTP request JSON"),
                })
                .expect("capture agent HTTP request");
            let content_type = tiny_http::Header::from_bytes(
                b"content-type".as_slice(),
                reply.content_type.as_bytes(),
            )
            .expect("build content-type header");
            request
                .respond(
                    tiny_http::Response::from_string(reply.body)
                        .with_status_code(tiny_http::StatusCode(reply.status))
                        .with_header(content_type),
                )
                .expect("send agent HTTP reply");
        }
    });
    (format!("http://{address}"), requests_rx, task)
}
