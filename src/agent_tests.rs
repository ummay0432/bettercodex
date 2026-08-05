use super::*;
use crate::auth::Auth;
use crate::context::AUTO_COMPACT_TOKEN_LIMIT;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use crate::context::estimated_tokens;
use crate::input::ImageDetail;
use crate::input::UserPrompt;
use crate::rollout::SessionIdentity;
use crate::skills::SkillMention;
use crate::skills::SkillSelection;
use crate::usage::TokenUsage;
use futures::SinkExt;
use futures::StreamExt;
use serde_json::json;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;
use tokio::net::TcpListener as TokioTcpListener;
use tokio::net::TcpStream as TokioTcpStream;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::extensions::ExtensionsConfig;
use tokio_tungstenite::tungstenite::extensions::compression::deflate::DeflateConfig;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_waits_for_the_response_boundary_and_enters_the_next_request() {
    let mut fixture = SteeringServer::spawn();
    let (root, mut agent) = test_agent(&fixture.base_url);
    let (events_tx, mut events_rx) = unbounded_channel();
    let (handle, control) = TurnControl::channel();

    let task = tokio::spawn(async move {
        let outcome = agent
            .submit_with_control(UserInput::text("initial prompt"), events_tx, control)
            .await;
        (agent, outcome)
    });

    let first_request = fixture.requests.recv().await.expect("first request");
    let steer_id = handle
        .steer(UserInput::text("steering instruction"))
        .expect("active turn accepts steering");
    fixture
        .release_first
        .send(())
        .expect("release first response");
    let second_request = fixture.requests.recv().await.expect("second request");

    let (agent, outcome) = task.await.expect("agent task");
    assert_eq!(
        outcome.unwrap(),
        SubmitOutcome::Completed("first answer\nsecond answer".to_string())
    );
    assert_eq!(
        agent.prompt_history(),
        vec![
            "initial prompt".to_string(),
            "steering instruction".to_string()
        ]
    );
    assert!(
        handle.steer(UserInput::text("too late")).is_err(),
        "the final empty-queue check must atomically close steering admission"
    );

    assert_eq!(
        conversation_text(&first_request),
        vec![("user".to_string(), "initial prompt".to_string())]
    );
    assert_eq!(
        conversation_text(&second_request),
        vec![
            ("user".to_string(), "initial prompt".to_string()),
            ("assistant".to_string(), "first answer".to_string()),
            ("user".to_string(), "steering instruction".to_string()),
        ]
    );

    let events = std::iter::from_fn(|| events_rx.try_recv().ok()).collect::<Vec<_>>();
    let response_boundary = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ModelResponseCompleted))
        .expect("first response boundary event");
    let committed = events
        .iter()
        .position(|event| *event == AgentEvent::SteeringCommitted(steer_id))
        .expect("steering commit event");
    assert!(response_boundary < committed, "{events:#?}");

    fixture.server.join().expect("server thread");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_compacts_before_an_instruction_boundary_restores_omitted_reasoning() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, mut requests) = unbounded_channel();
    let (release_first, release_first_rx) = std_mpsc::channel();
    let reasoning = json!({
        "type": "reasoning",
        "id": "rs_before_steering",
        "encrypted_content": "x".repeat(110_000),
    });
    let measured_tokens = AUTO_COMPACT_TOKEN_LIMIT - 10_000;
    let projected_with_reasoning =
        measured_tokens.saturating_add(estimated_tokens(std::slice::from_ref(&reasoning)));
    assert!(projected_with_reasoning > AUTO_COMPACT_TOKEN_LIMIT);
    assert!(projected_with_reasoning < EFFECTIVE_CONTEXT_WINDOW);

    let server_reasoning = reasoning.clone();
    let server = thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        requests_tx.send(read_request(&mut first)).unwrap();
        release_first_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first response release");
        write_sse_items_response_with_usage(
            &mut first,
            "resp_before_steering",
            &[
                server_reasoning,
                json!({
                    "id": "msg_before_steering",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "first answer"}],
                }),
            ],
            json!({
                "input_tokens": measured_tokens - 100,
                "output_tokens": 100,
                "total_tokens": measured_tokens,
            }),
        );

        let (mut compact, _) = listener.accept().unwrap();
        requests_tx.send(read_request(&mut compact)).unwrap();
        write_sse_items_response(
            &mut compact,
            "resp_compact_before_steering",
            &[json!({
                "type": "compaction_summary",
                "id": "cmp_before_steering",
                "encrypted_content": "opaque",
            })],
        );

        let (mut continuation, _) = listener.accept().unwrap();
        requests_tx.send(read_request(&mut continuation)).unwrap();
        write_sse_response(&mut continuation, "second answer");
    });

    let (root, mut agent) = test_agent(&format!("http://{address}"));
    let (events_tx, mut events_rx) = unbounded_channel();
    let (handle, control) = TurnControl::channel();
    let task = tokio::spawn(async move {
        let outcome = agent
            .submit_with_control(UserInput::text("initial prompt"), events_tx, control)
            .await;
        (agent, outcome)
    });

    let first_request = timeout(Duration::from_secs(5), requests.recv())
        .await
        .expect("first request timed out")
        .expect("first request");
    let steer_id = handle
        .steer(UserInput::text("steering instruction"))
        .expect("active turn accepts steering");
    release_first.send(()).unwrap();
    let compact_request = timeout(Duration::from_secs(5), requests.recv())
        .await
        .expect("compaction request timed out")
        .expect("compaction request");
    let continuation_request = timeout(Duration::from_secs(5), requests.recv())
        .await
        .expect("continuation request timed out")
        .expect("continuation request");

    let (agent, outcome) = task.await.expect("agent task");
    assert_eq!(
        outcome.unwrap(),
        SubmitOutcome::Completed("first answer\nsecond answer".to_string())
    );
    assert_eq!(
        conversation_text(&first_request),
        vec![("user".to_string(), "initial prompt".to_string())]
    );

    let compact_request: Value = serde_json::from_slice(&compact_request).unwrap();
    assert_eq!(
        compact_request["input"].as_array().unwrap().last().unwrap(),
        &json!({"type": "compaction_trigger"})
    );
    let metadata: Value = serde_json::from_str(
        compact_request["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["compaction"]["phase"], "mid_turn");

    let continuation: Value = serde_json::from_slice(&continuation_request).unwrap();
    let input = continuation["input"].as_array().unwrap();
    let compacted = input
        .iter()
        .position(|item| item["type"] == "compaction_summary")
        .expect("opaque compaction item");
    let steering = input
        .iter()
        .position(|item| {
            item["role"] == "user"
                && item.pointer("/content/0/text").and_then(Value::as_str)
                    == Some("steering instruction")
        })
        .expect("steering instruction");
    assert!(compacted < steering);
    assert!(
        input.iter().all(|item| item["type"] != "reasoning"),
        "uncompacted reasoning reached the continuation request"
    );
    assert_eq!(
        agent.prompt_history(),
        vec![
            "initial prompt".to_string(),
            "steering instruction".to_string(),
        ]
    );

    let events = std::iter::from_fn(|| events_rx.try_recv().ok()).collect::<Vec<_>>();
    let compaction_started = events
        .iter()
        .position(|event| matches!(event, AgentEvent::CompactionStarted))
        .expect("compaction start event");
    let committed = events
        .iter()
        .position(|event| *event == AgentEvent::SteeringCommitted(steer_id))
        .expect("steering commit event");
    assert!(compaction_started < committed, "{events:#?}");

    server.join().expect("server thread");
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_skill_path_drives_the_recorded_history_and_outgoing_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        write_sse_response(&mut stream, "skill accepted");
        request
    });

    let root = std::env::temp_dir().join(format!(
        "bettercodex-agent-skill-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let repository = root.join("repo");
    let cwd = repository.join("service");
    std::fs::create_dir_all(repository.join(".git")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let repository_skill = repository.join(".bcodex/skills/demo/SKILL.md");
    let service_skill = cwd.join(".bcodex/skills/demo/SKILL.md");
    for (path, body) in [
        (&repository_skill, "REPOSITORY SKILL BODY"),
        (&service_skill, "SERVICE SKILL BODY"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!("---\nname: demo\ndescription: Demo workflow\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let identity: SessionIdentity = rollout.identity().clone();
    let conversation = Conversation::new(&cwd, rollout).unwrap();
    let mut api = ApiClient::new_with_base_url(
        Auth::for_test("token-test"),
        &identity,
        0,
        format!("http://{address}"),
    )
    .unwrap();
    assert!(api.fall_back_to_http());
    let tools = ToolRuntime::new(cwd.clone(), api.web_search_client());
    let mut agent = Agent {
        cwd,
        api,
        conversation,
        tools,
    };
    let prompt = UserPrompt::with_skill_mentions(
        "use $demo now",
        vec![SkillMention::new(
            SkillSelection::new("demo", service_skill.canonicalize().unwrap()),
            4..9,
        )],
    );
    let (events_tx, _events_rx) = unbounded_channel();
    let (_handle, control) = TurnControl::channel();

    assert_eq!(
        agent
            .submit_with_control(UserInput::prompt(prompt), events_tx, control)
            .await
            .unwrap(),
        SubmitOutcome::Completed("skill accepted".to_string())
    );
    assert_eq!(agent.prompt_history(), ["use $demo now"]);

    let request: Value = serde_json::from_slice(&server.join().unwrap()).unwrap();
    let input = request["input"].as_array().unwrap();
    fn text(item: &Value) -> &str {
        item.pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
    let catalog = input
        .iter()
        .find(|item| item["role"] == "developer" && text(item).starts_with("<skills>"))
        .expect("model-visible skills catalogue");
    assert_eq!(text(catalog).matches("- demo:").count(), 2);
    let user_index = input
        .iter()
        .position(|item| item["role"] == "user" && text(item) == "use $demo now")
        .expect("ordinary user prompt");
    let injection_index = input
        .iter()
        .position(|item| item["role"] == "user" && text(item).starts_with("<skill>"))
        .expect("selected skill injection");
    assert!(user_index < injection_index);
    assert!(text(&input[injection_index]).contains("SERVICE SKILL BODY"));
    assert!(!text(&input[injection_index]).contains("REPOSITORY SKILL BODY"));

    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelling_compaction_reconnects_before_the_next_turn() {
    let listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (compaction_started_tx, compaction_started_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut first = accept_websocket(&listener).await;
        let warmup = read_websocket_request(&mut first).await.unwrap();
        assert_eq!(warmup["generate"], false);
        send_websocket_completion(&mut first, "resp_warm", &[]).await;

        let compaction = read_websocket_request(&mut first).await.unwrap();
        assert_eq!(
            compaction["input"].as_array().unwrap().last().unwrap()["type"],
            "compaction_trigger"
        );
        compaction_started_tx.send(()).unwrap();

        let (fresh_connection, next_request, mut websocket) =
            match read_websocket_request(&mut first).await {
                Some(request) => (false, request, first),
                None => {
                    let mut fresh = timeout(Duration::from_secs(5), accept_websocket(&listener))
                        .await
                        .expect("agent did not reconnect after cancellation");
                    let request = read_websocket_request(&mut fresh).await.unwrap();
                    (true, request, fresh)
                }
            };
        send_websocket_text_response(&mut websocket, "resp_next", "next answer").await;
        (fresh_connection, next_request)
    });

    let (root, mut agent) = test_websocket_agent(&format!("http://{address}"));
    let large_text =
        "x".repeat(usize::try_from(AUTO_COMPACT_TOKEN_LIMIT.saturating_mul(4)).unwrap());
    let large_message = UserInput::text(large_text.clone())
        .into_message_and_skills()
        .0;
    let incoming_tokens = estimated_tokens(std::slice::from_ref(&large_message));
    assert!(incoming_tokens >= AUTO_COMPACT_TOKEN_LIMIT);
    assert!(incoming_tokens <= EFFECTIVE_CONTEXT_WINDOW);

    let (events, _event_rx) = unbounded_channel();
    let (handle, control) = TurnControl::channel();
    let cancelled = {
        let submission = agent.submit_with_control(UserInput::text(large_text), events, control);
        tokio::pin!(submission);
        tokio::select! {
            result = &mut submission => panic!("turn finished before compaction cancellation: {result:?}"),
            signal = compaction_started_rx => signal.unwrap(),
        }
        handle.cancel();
        submission.await.unwrap()
    };
    assert_eq!(cancelled, SubmitOutcome::Cancelled);

    assert_eq!(
        timeout(Duration::from_secs(5), agent.submit("next turn"))
            .await
            .expect("next turn timed out")
            .unwrap(),
        "next answer"
    );
    let (fresh_connection, next_request) = server.await.unwrap();
    assert!(
        fresh_connection,
        "cancelled compaction reused its busy socket"
    );
    assert!(next_request.get("previous_response_id").is_none());
    assert!(next_request.to_string().contains("next turn"));
    assert_eq!(agent.prompt_history(), vec!["next turn".to_string()]);

    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn manual_compaction_uses_a_standalone_turn_and_replaces_saved_history() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, requests_rx) = std_mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        requests_tx.send(read_request(&mut stream)).unwrap();
        write_sse_items_response(
            &mut stream,
            "resp_manual_compact",
            &[json!({
                "type": "compaction_summary",
                "id": "cmp_manual",
                "encrypted_content": "opaque",
            })],
        );
    });

    let (root, mut agent) = test_agent(&format!("http://{address}"));
    let retained_user = UserInput::text("retain this user request")
        .into_message_and_skills()
        .0;
    agent
        .conversation
        .extend([
            retained_user,
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "earlier answer"}],
            }),
        ])
        .unwrap();
    let session_id = agent.session_id().to_string();
    let (events_tx, mut events_rx) = unbounded_channel();
    let (handle, control) = TurnControl::non_steerable_channel();
    assert!(
        handle
            .steer(UserInput::text("cannot steer compaction"))
            .is_err(),
        "manual compaction is a non-steerable standalone turn"
    );

    assert_eq!(
        timeout(
            Duration::from_secs(5),
            agent.compact_with_control(events_tx, control),
        )
        .await
        .expect("manual compaction timed out")
        .unwrap(),
        CompactionOutcome::Completed
    );
    server.join().unwrap();

    let request: Value = serde_json::from_slice(
        &requests_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("manual compaction request"),
    )
    .unwrap();
    assert_eq!(
        request["input"].as_array().unwrap().last().unwrap(),
        &json!({"type": "compaction_trigger"})
    );
    let metadata: Value = serde_json::from_str(
        request["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["request_kind"], "compaction");
    assert_eq!(
        metadata["compaction"],
        json!({
            "trigger": "manual",
            "reason": "user_requested",
            "implementation": "responses_compaction_v2",
            "phase": "standalone_turn",
            "strategy": "memento",
        })
    );

    assert_eq!(
        agent.prompt_history(),
        vec!["retain this user request".to_string()]
    );
    assert_eq!(
        agent
            .conversation
            .items()
            .iter()
            .filter(|item| item["type"] == "compaction_summary")
            .count(),
        1
    );
    assert!(
        agent
            .conversation
            .items()
            .iter()
            .any(|item| item["role"] == "developer"),
        "current environment context must be reinjected"
    );
    assert!(matches!(
        events_rx.try_recv(),
        Ok(AgentEvent::CompactionStarted)
    ));
    assert!(matches!(
        events_rx.try_recv(),
        Ok(AgentEvent::ContextUpdated(_))
    ));
    assert!(matches!(
        events_rx.try_recv(),
        Ok(AgentEvent::CompactionCompleted)
    ));

    let journal = std::fs::read_to_string(
        root.join("state")
            .join("sessions")
            .join(format!("{session_id}.jsonl")),
    )
    .unwrap();
    let records = journal
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let started = records
        .iter()
        .position(|record| record["type"] == "turn_started")
        .unwrap();
    let replaced = records
        .iter()
        .position(|record| record["type"] == "history_replace" && record["reason"] == "compaction")
        .unwrap();
    let finished = records
        .iter()
        .position(|record| record["type"] == "turn_finished" && record["outcome"] == "completed")
        .unwrap();
    assert!(started < replaced && replaced < finished, "{records:#?}");
    assert_eq!(records[started]["turn_id"], records[finished]["turn_id"]);

    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn input_is_rechecked_after_successful_compaction_before_sampling() {
    let listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut websocket = accept_websocket(&listener).await;
        let warmup = read_websocket_request(&mut websocket).await.unwrap();
        assert_eq!(warmup["generate"], false);
        send_websocket_completion(&mut websocket, "resp_warm", &[]).await;

        let compaction_request = read_websocket_request(&mut websocket).await.unwrap();
        assert_eq!(
            compaction_request["input"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["type"],
            "compaction_trigger"
        );
        let compaction = json!({
            "type": "compaction_summary",
            "id": "cmp_boundary",
            "encrypted_content": "opaque",
        });
        send_websocket_completion(&mut websocket, "resp_compact", &[compaction]).await;
        match timeout(
            Duration::from_secs(2),
            read_websocket_request(&mut websocket),
        )
        .await
        {
            Ok(Some(_)) => {
                send_websocket_text_response(&mut websocket, "resp_sampled", "should not sample")
                    .await;
                true
            }
            Ok(None) | Err(_) => false,
        }
    });

    let (root, mut agent) = test_websocket_agent(&format!("http://{address}"));
    let text =
        "x".repeat(usize::try_from((EFFECTIVE_CONTEXT_WINDOW - 1_000).saturating_mul(4)).unwrap());
    let message = UserInput::text(text.clone()).into_message_and_skills().0;
    let incoming_tokens = estimated_tokens(std::slice::from_ref(&message));
    assert!(incoming_tokens <= EFFECTIVE_CONTEXT_WINDOW);
    assert!(incoming_tokens >= AUTO_COMPACT_TOKEN_LIMIT);
    assert!(
        agent
            .conversation
            .projected_tokens(std::slice::from_ref(&message))
            > EFFECTIVE_CONTEXT_WINDOW
    );

    let error = timeout(Duration::from_secs(5), agent.submit(&text))
        .await
        .expect("input admission did not finish")
        .unwrap_err();
    assert!(error.to_string().contains("after compaction"), "{error:#}");
    assert!(agent.prompt_history().is_empty());
    assert!(!server.await.unwrap(), "oversized input reached sampling");

    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn individually_oversized_text_and_image_batches_are_rejected_without_compaction() {
    let (root, mut agent) = test_agent("http://127.0.0.1:1");
    let text = "x".repeat(usize::try_from(EFFECTIVE_CONTEXT_WINDOW.saturating_mul(4)).unwrap());
    let error = agent.submit(&text).await.unwrap_err();
    assert!(error.to_string().contains("input alone"), "{error:#}");

    let images_directory = root.join("images");
    std::fs::create_dir_all(&images_directory).unwrap();
    let mut png = vec![0_u8; 24];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[16..20].copy_from_slice(&3_200_u32.to_be_bytes());
    png[20..24].copy_from_slice(&3_200_u32.to_be_bytes());
    let paths = (0..36)
        .map(|index| {
            let path = images_directory.join(format!("large-{index}.png"));
            std::fs::write(&path, &png).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let images = UserInput::from_paths("", &paths, ImageDetail::Original).unwrap();
    let error = agent.submit_user_input(images).await.unwrap_err();
    assert!(error.to_string().contains("input alone"), "{error:#}");
    assert!(agent.prompt_history().is_empty());

    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_usage_is_not_overridden_by_a_larger_full_history_estimate() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("failed to accept sampling request: {error}"),
            }
        };
        let request = read_request(&mut stream);
        write_sse_response(&mut stream, "measured context accepted");
        Some(request)
    });
    let (root, mut agent) = test_agent(&format!("http://{address}"));

    let prefix_tokens = estimated_tokens(crate::api::context_prefix_items());
    let baseline_text_tokens = EFFECTIVE_CONTEXT_WINDOW
        .saturating_sub(prefix_tokens)
        .saturating_sub(2_000);
    let baseline = UserInput::text(
        "x".repeat(usize::try_from(baseline_text_tokens.saturating_mul(4)).unwrap()),
    )
    .into_message_and_skills()
    .0;
    let call = json!({
        "type": "custom_tool_call",
        "call_id": "call_measured_context",
        "name": "exec",
        "input": "text(true)",
    });
    agent.conversation.extend([baseline, call]).unwrap();
    // The server's count includes the full request and is authoritative for existing history.
    // This deliberately models a session where conservative bytes/4 replay accounting is higher.
    let measured_tokens = AUTO_COMPACT_TOKEN_LIMIT - 15_000;
    agent
        .conversation
        .record_usage(
            Some(TokenUsage {
                input_tokens: measured_tokens,
                total_tokens: measured_tokens,
                ..TokenUsage::default()
            }),
            true,
        )
        .unwrap();
    agent
        .conversation
        .extend([json!({
            "type": "custom_tool_call_output",
            "call_id": "call_measured_context",
            "output": [{"type": "input_text", "text": "x".repeat(12_000)}],
        })])
        .unwrap();

    let heuristic_request_tokens =
        prefix_tokens.saturating_add(estimated_tokens(agent.conversation.items()));
    assert!(heuristic_request_tokens > EFFECTIVE_CONTEXT_WINDOW);
    assert!(agent.conversation.projected_tokens(&[]) < AUTO_COMPACT_TOKEN_LIMIT);

    let (_handle, control) = TurnControl::channel();
    let outcome = timeout(
        Duration::from_secs(5),
        agent.sample_with_recovery(&None, &control),
    )
    .await;
    let request = server.join().expect("single-response server");
    let response = match outcome.expect("sampling timed out").unwrap() {
        SamplingOutcome::Response(response) => response,
        SamplingOutcome::Cancelled => panic!("sampling was cancelled"),
    };
    assert_eq!(response.text, "measured context accepted");
    let request: Value = serde_json::from_slice(&request.expect("sampling request")).unwrap();
    assert!(
        estimated_tokens(request["input"].as_array().unwrap()) > EFFECTIVE_CONTEXT_WINDOW,
        "the fixture must cover the heuristic false positive"
    );

    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

fn test_agent(base_url: &str) -> (PathBuf, Agent) {
    test_agent_with_transport(base_url, TestTransport::Http)
}

fn test_websocket_agent(base_url: &str) -> (PathBuf, Agent) {
    test_agent_with_transport(base_url, TestTransport::WebSocket)
}

#[derive(Clone, Copy)]
enum TestTransport {
    Http,
    WebSocket,
}

fn test_agent_with_transport(base_url: &str, transport: TestTransport) -> (PathBuf, Agent) {
    let root = std::env::temp_dir().join(format!(
        "bettercodex-agent-steering-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let cwd = root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let identity: SessionIdentity = rollout.identity().clone();
    let conversation = Conversation::new(&cwd, rollout).unwrap();
    let mut api = ApiClient::new_with_base_url(
        Auth::for_test("token-test"),
        &identity,
        0,
        base_url.to_string(),
    )
    .unwrap();
    match transport {
        TestTransport::Http => assert!(api.fall_back_to_http()),
        TestTransport::WebSocket => {}
    }
    let tools = ToolRuntime::new(cwd.clone(), api.web_search_client());
    (
        root,
        Agent {
            cwd,
            api,
            conversation,
            tools,
        },
    )
}

type TestWebSocket = WebSocketStream<TokioTcpStream>;

async fn accept_websocket(listener: &TokioTcpListener) -> TestWebSocket {
    let (stream, _) = listener.accept().await.unwrap();
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());
    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    accept_async_with_config(stream, Some(config))
        .await
        .unwrap()
}

async fn read_websocket_request(websocket: &mut TestWebSocket) -> Option<Value> {
    loop {
        match websocket.next().await? {
            Ok(Message::Text(text)) => return Some(serde_json::from_str(&text).unwrap()),
            Ok(Message::Ping(payload)) => {
                websocket.send(Message::Pong(payload)).await.unwrap();
            }
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(Message::Binary(_) | Message::Pong(_) | Message::Frame(_)) => {}
        }
    }
}

async fn send_websocket_text_response(
    websocket: &mut TestWebSocket,
    response_id: &str,
    text: &str,
) {
    let item = json!({
        "id": format!("msg_{}", text.replace(' ', "_")),
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": text}],
    });
    send_websocket_completion(websocket, response_id, &[item]).await;
}

async fn send_websocket_completion(
    websocket: &mut TestWebSocket,
    response_id: &str,
    items: &[Value],
) {
    for (output_index, item) in items.iter().enumerate() {
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
    }
    websocket
        .send(Message::Text(
            json!({
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "model": crate::MODEL,
                    "reasoning": {"context": "all_turns"},
                    "output": items,
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 2,
                        "total_tokens": 12,
                    },
                },
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
}

fn conversation_text(request: &[u8]) -> Vec<(String, String)> {
    let request: Value = serde_json::from_slice(request).unwrap();
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| {
            let role = item.get("role")?.as_str()?;
            if !matches!(role, "user" | "assistant") {
                return None;
            }
            let text = item
                .get("content")?
                .as_array()?
                .iter()
                .find_map(|content| {
                    content
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })?;
            (!text.starts_with("# Repository onboarding")).then(|| (role.to_string(), text))
        })
        .collect()
}

struct SteeringServer {
    base_url: String,
    requests: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    release_first: std_mpsc::Sender<()>,
    server: thread::JoinHandle<()>,
}

impl SteeringServer {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests) = unbounded_channel();
        let (release_first, release_first_rx) = std_mpsc::channel();
        let server = thread::spawn(move || {
            for (index, answer) in ["first answer", "second answer"].into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                requests_tx.send(read_request(&mut stream)).unwrap();
                if index == 0 {
                    release_first_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("first response release");
                }
                write_sse_response(&mut stream, answer);
            }
        });
        Self {
            base_url: format!("http://{address}"),
            requests,
            release_first,
            server,
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let header_end = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "request ended before its headers");
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
        .unwrap();
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "request ended before its body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = &bytes[header_end..header_end + content_length];
    if headers.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("content-encoding") && value.trim() == "zstd"
        })
    }) {
        zstd::stream::decode_all(std::io::Cursor::new(body)).unwrap()
    } else {
        body.to_vec()
    }
}

fn write_sse_response(stream: &mut TcpStream, answer: &str) {
    let item = json!({
        "id": format!("msg_{}", answer.replace(' ', "_")),
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": answer}],
    });
    write_sse_items_response(
        stream,
        &format!("resp_{}", answer.replace(' ', "_")),
        &[item],
    );
}

fn write_sse_items_response(stream: &mut TcpStream, response_id: &str, items: &[Value]) {
    write_sse_items_response_with_usage(
        stream,
        response_id,
        items,
        json!({
            "input_tokens": 10,
            "output_tokens": 2,
            "total_tokens": 12,
        }),
    );
}

fn write_sse_items_response_with_usage(
    stream: &mut TcpStream,
    response_id: &str,
    items: &[Value],
    usage: Value,
) {
    let item_events = items
        .iter()
        .enumerate()
        .map(|(output_index, item)| {
            format!(
                "data: {}\n\n",
                json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": item,
                })
            )
        })
        .collect::<String>();
    let completed = json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "model": crate::MODEL,
            "reasoning": {"context": "all_turns"},
            "output": items,
            "usage": usage,
        },
    });
    let body = format!("{item_events}data: {completed}\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nopenai-model: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        crate::MODEL,
        body,
    )
    .unwrap();
    stream.flush().unwrap();
}
