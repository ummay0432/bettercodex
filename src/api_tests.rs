use super::*;
use crate::compaction::CompactionPhase;
use crate::compaction::CompactionRequest;
use crate::context::RAW_CONTEXT_WINDOW;
use futures::SinkExt;
use futures::StreamExt;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;
use tokio_tungstenite::accept_async_with_config;
use tokio_tungstenite::tungstenite::Message;

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

    async fn respond_streaming(
        &mut self,
        history: Vec<Value>,
        completed_items: &UnboundedSender<Value>,
        events: &UnboundedSender<AgentEvent>,
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

    async fn respond_streaming(
        &mut self,
        history: Vec<Value>,
        completed_items: &UnboundedSender<Value>,
        events: &UnboundedSender<AgentEvent>,
    ) -> ApiResult<ModelResponse> {
        let mut request = self.build_request(history, RequestKind::Turn);
        self.respond_request_with_events(
            &mut request,
            completed_items,
            Some(events),
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
fn streaming_delta_payloads_move_from_decoding_into_tui_events() {
    for (event_type, reasoning) in [
        ("response.output_text.delta", false),
        ("response.reasoning_summary_text.delta", true),
    ] {
        let payload = format!("{event_type}-payload");
        let event = json!({"type": event_type, "delta": payload});
        let decoded_allocation = event["delta"].as_str().unwrap().as_ptr();
        let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();

        process_event_value(
            event,
            &mut CollectedResponse::default(),
            &completed_items,
            Some(&events),
        )
        .unwrap();

        let received = received.try_recv().unwrap();
        let received_text = match &received {
            AgentEvent::ModelMessageDelta(text) | AgentEvent::ReasoningSummaryDelta(text) => text,
            event => panic!("expected a streaming delta event, got {event:?}"),
        };
        assert_eq!(received_text, &payload);
        assert_eq!(
            received_text.as_ptr(),
            decoded_allocation,
            "decoded delta payloads must move into TUI events instead of being copied"
        );
        assert_eq!(
            matches!(received, AgentEvent::ReasoningSummaryDelta(_)),
            reasoning
        );
    }
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_completed_output_handoff() {
    const ITEMS: usize = 64;
    const PAYLOAD_BYTES: usize = 1024 * 1024;

    let events = (0..ITEMS)
        .map(|index| {
            json!({
                "type": "response.output_item.done",
                "output_index": index,
                "item": {
                    "type": "reasoning",
                    "id": format!("rs_{index}"),
                    "encrypted_content": "x".repeat(PAYLOAD_BYTES),
                },
            })
        })
        .collect::<Vec<_>>();
    let (completed_items, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();

    let started = std::time::Instant::now();
    for event in events {
        process_event_value(event, &mut collected, &completed_items, None).unwrap();
    }
    let elapsed = started.elapsed();
    for _ in 0..ITEMS {
        std::hint::black_box(received.try_recv().unwrap());
    }

    eprintln!(
        "{ITEMS} completed {} MiB output items handed to history in {elapsed:?}",
        PAYLOAD_BYTES / (1024 * 1024)
    );
    assert_eq!(collected.items.len(), ITEMS);
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_websocket_event_handoff() {
    const EVENTS: usize = 100_000;
    const DELTA_BYTES: usize = 512;

    let wire = json!({
        "type": "response.output_text.delta",
        "delta": "x".repeat(DELTA_BYTES),
    })
    .to_string();
    let frame = tungstenite::Utf8Bytes::from(wire);
    let frames = std::iter::repeat_n(frame, EVENTS).collect::<Vec<_>>();
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();

    let started = std::time::Instant::now();
    for frame in frames {
        let event = serde_json::from_str::<Value>(frame.as_str()).unwrap();
        process_event_value(event, &mut collected, &completed_items, Some(&events)).unwrap();
    }
    drop(events);
    let received = std::iter::from_fn(|| received.try_recv().ok())
        .map(std::hint::black_box)
        .count();
    let elapsed = started.elapsed();

    eprintln!(
        "{EVENTS} WebSocket events with {DELTA_BYTES}-byte deltas decoded and handed to the TUI without intermediate payload copies in {elapsed:?}"
    );
    assert_eq!(received, EVENTS);
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_http_sse_event_handoff() {
    const EVENTS: usize = 100_000;
    const DELTA_BYTES: usize = 512;

    let wire = json!({
        "type": "response.output_text.delta",
        "delta": "x".repeat(DELTA_BYTES),
    })
    .to_string();
    let frame = format!("data: {wire}\n\n");
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();
    let mut decoder = SseDecoder::default();
    let mut decoded = Vec::new();

    let started = std::time::Instant::now();
    for _ in 0..EVENTS {
        decoder
            .push(std::hint::black_box(frame.as_bytes()), &mut decoded)
            .unwrap();
        for data in decoded.drain(..) {
            process_event(&data, &mut collected, &completed_items, Some(&events)).unwrap();
        }
    }
    drop(events);
    let received = std::iter::from_fn(|| received.try_recv().ok())
        .map(std::hint::black_box)
        .count();
    let elapsed = started.elapsed();

    eprintln!(
        "{EVENTS} HTTP SSE events with {DELTA_BYTES}-byte deltas decoded and handed to the TUI in {elapsed:?}"
    );
    assert_eq!(received, EVENTS);
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
fn assistant_item_events_preserve_phase_and_authoritative_text() {
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();

    let item = assistant_item_with_phase("still working", "commentary");
    process_event_value(
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": item,
        }),
        &mut collected,
        &completed_items,
        Some(&events),
    )
    .unwrap();
    process_event_value(
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": item,
        }),
        &mut collected,
        &completed_items,
        Some(&events),
    )
    .unwrap();

    let message = AssistantMessage {
        text: "still working".to_string(),
        phase: Some(codex_protocol::models::MessagePhase::Commentary),
    };
    assert_eq!(
        received.try_recv().unwrap(),
        AgentEvent::ModelMessageStarted(message.clone())
    );
    assert_eq!(
        received.try_recv().unwrap(),
        AgentEvent::ModelMessageCompleted(message)
    );
}

#[test]
fn response_completion_preserves_end_turn_and_recovers_missing_item_events() {
    let (events, mut received_events) = tokio::sync::mpsc::unbounded_channel();
    let (completed_items, mut received_items) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();
    let item = assistant_item_with_phase("continuing", "commentary");
    let mut completed = completed_event("resp_continue", &item);
    completed["response"]["end_turn"] = json!(false);

    process_event_value(completed, &mut collected, &completed_items, Some(&events)).unwrap();

    assert_eq!(received_items.try_recv().unwrap(), item);
    assert_eq!(
        received_events.try_recv().unwrap(),
        AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: "continuing".to_string(),
            phase: Some(codex_protocol::models::MessagePhase::Commentary),
        })
    );
    let response = collected.finish().unwrap();
    assert_eq!(response.end_turn, Some(false));
    assert_eq!(response.final_answer, None);
    assert!(response.has_assistant_text());
}

#[test]
fn unsupported_assistant_message_phases_are_rejected_at_the_stream_boundary() {
    let (completed_items, _received_items) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();
    let error = process_event_value(
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": assistant_item_with_phase("future", "unknown"),
        }),
        &mut collected,
        &completed_items,
        None,
    )
    .unwrap_err();

    assert!(error.to_string().contains("unsupported phase"));
}

#[test]
fn request_uses_instructions_and_one_stable_tool_prefix() {
    assert!(
        !SYSTEM_PROMPT.to_ascii_lowercase().contains("papercut"),
        "papercut policy belongs to the toggleable system skill"
    );
    let client = test_client("http://127.0.0.1:1".to_string());
    let first_message = user_message("one");
    let first_text_allocation = first_message["content"][0]["text"]
        .as_str()
        .unwrap()
        .as_ptr();
    let first = client.build_request(vec![first_message], RequestKind::Turn);
    let second = client.build_request(
        vec![user_message("one"), user_message("two")],
        RequestKind::Turn,
    );
    let first_input = first["input"].as_array().unwrap();
    let second_input = second["input"].as_array().unwrap();

    assert_eq!(first["model"], MODEL);
    assert_eq!(first["instructions"], harness_instructions());
    assert!(
        harness_instructions().starts_with(
            "<system_instructions>\nYou are Sol, the coding agent inside bettercodex."
        )
    );
    assert!(harness_instructions().contains("# Engineering ownership"));
    assert!(harness_instructions().ends_with("</system_instructions>"));
    assert!(
        first.get("max_output_tokens").is_none(),
        "ChatGPT Responses Lite rejects the public max_output_tokens field"
    );
    assert_eq!(
        first["reasoning"],
        json!({"effort": "max", "summary": "auto", "context": "all_turns"})
    );
    assert!(first.get("prompt_cache_options").is_none());
    assert_eq!(first["prompt_cache_key"], "session-test");
    assert_eq!(first_input[0]["type"], "additional_tools");
    assert_eq!(
        first_input.last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .as_ptr(),
        first_text_allocation,
        "request assembly must consume the sampling snapshot without cloning its payloads"
    );
    assert_eq!(&first_input[..1], &second_input[..1]);
    assert_eq!(&first_input[..1], stable_request_prefix());
    assert_eq!(
        serde_json::to_string(&first_input[..1]).unwrap().len(),
        11_372,
        "run ./scripts/dev.py tool-context --update"
    );
    assert!(first_input.iter().all(|item| {
        item.pointer("/content/0/text").and_then(Value::as_str) != Some(harness_instructions())
    }));

    let mut retained_prefix = first_input.to_vec();
    retained_prefix[0]["id"] = json!("at_compacted");
    retained_prefix[0]["status"] = json!("completed");
    let recomposed = compose_input(retained_prefix);
    assert_eq!(
        recomposed
            .iter()
            .filter(|item| item["type"] == "additional_tools")
            .count(),
        1
    );
}

#[test]
fn request_bakes_in_the_fixed_exec_runtime() {
    let client = test_client("http://127.0.0.1:1".to_string());
    let request = client.build_request(vec![user_message("run tools")], RequestKind::Turn);
    let request_tools = request["input"][0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            (
                tool["type"].as_str().unwrap(),
                tool["name"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        request_tools,
        vec![("custom", "exec"), ("function", "wait")]
    );
    assert_eq!(request["tool_choice"], "auto");
    assert_eq!(request["parallel_tool_calls"], false);
    assert!(request.get("tool_mode").is_none());

    let turn_metadata: Value = serde_json::from_str(
        request["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        turn_metadata["code_mode_tool_names"],
        json!({
            "apply_patch": {"name": "apply_patch", "namespace": null},
            "exec_command": {"name": "exec_command", "namespace": null},
            "log_papercut": {"name": "log_papercut", "namespace": null},
            "update_plan": {"name": "update_plan", "namespace": null},
            "view_image": {"name": "view_image", "namespace": null},
            "web__run": {"name": "run", "namespace": "web"},
            "write_stdin": {"name": "write_stdin", "namespace": null},
        })
    );
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_long_context_sampling_handoff() {
    const ITEMS: usize = 256;
    const BYTES_PER_ITEM: usize = 8 * 1024;
    const SAMPLES: usize = 32;

    let history = (0..ITEMS)
        .map(|index| user_message(&format!("{index}:{}", "x".repeat(BYTES_PER_ITEM))))
        .collect::<Vec<_>>();
    let clone_started = std::time::Instant::now();
    for _ in 0..SAMPLES {
        std::hint::black_box(history.clone());
    }
    let clone_elapsed = clone_started.elapsed();

    let mut transferred = history;
    let transfer_started = std::time::Instant::now();
    for _ in 0..SAMPLES {
        let (input, restoration) = compose_sampling_input(transferred);
        transferred = restoration.restore(input).unwrap();
        std::hint::black_box(&transferred);
    }
    let transfer_elapsed = transfer_started.elapsed();
    eprintln!(
        "{SAMPLES} handoffs of a {} MiB sampling history: clone {clone_elapsed:?}, transfer {transfer_elapsed:?}",
        ITEMS * BYTES_PER_ITEM / (1024 * 1024),
    );
    assert_eq!(transferred.len(), ITEMS);
}

#[test]
fn websocket_delta_requires_an_exact_prefix_and_allows_empty_input() {
    let mut client = test_client("http://127.0.0.1:1".to_string());
    let first_request = client.build_request(vec![user_message("one")], RequestKind::Turn);
    let output = vec![assistant_item("first")];
    client.websocket_baseline = Some(WebSocketBaseline {
        properties: reusable_request_properties(&first_request).unwrap(),
        input: WebSocketBaselineInput::Exact {
            request: first_request["input"].as_array().unwrap().clone(),
            output: output.clone(),
        },
        response_id: "resp_1".to_string(),
    });
    let next_history = [
        vec![user_message("one")],
        output.clone(),
        vec![user_message("two")],
    ]
    .concat();
    let mut next_request = client.build_request(next_history, RequestKind::Turn);
    let appended_text_allocation =
        next_request["input"].as_array().unwrap().last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .as_ptr();
    let logical_next_request = next_request.clone();
    let restoration = client
        .prepare_websocket_request(
            &mut next_request,
            WebSocketRequestMode::Inference,
            RequestInputIdentity::Exact,
        )
        .unwrap();
    assert_eq!(next_request["previous_response_id"], "resp_1");
    assert_eq!(next_request["input"], json!([user_message("two")]));
    assert!(next_request.get("stream").is_none());
    assert_eq!(
        next_request["client_metadata"][WS_RESPONSES_LITE_CLIENT_METADATA],
        "true"
    );
    assert_eq!(
        next_request["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .as_ptr(),
        appended_text_allocation,
        "delta preparation must move input values instead of cloning their payloads"
    );
    restoration.restore(&mut next_request).unwrap();
    assert_eq!(next_request, logical_next_request);
    assert_eq!(
        next_request["input"].as_array().unwrap().last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .as_ptr(),
        appended_text_allocation,
    );

    let mut empty_delta = client.build_request(
        [vec![user_message("one")], output.clone()].concat(),
        RequestKind::Turn,
    );
    let logical_empty_delta = empty_delta.clone();
    let restoration = client
        .prepare_websocket_request(
            &mut empty_delta,
            WebSocketRequestMode::Inference,
            RequestInputIdentity::Exact,
        )
        .unwrap();
    assert_eq!(empty_delta["previous_response_id"], "resp_1");
    assert_eq!(empty_delta["input"], json!([]));
    assert!(empty_delta.get("stream").is_none());
    restoration.restore(&mut empty_delta).unwrap();
    assert_eq!(empty_delta, logical_empty_delta);

    let mut unchanged = first_request.clone();
    let restoration = client
        .prepare_websocket_request(
            &mut unchanged,
            WebSocketRequestMode::Inference,
            RequestInputIdentity::Exact,
        )
        .unwrap();
    assert!(unchanged.get("previous_response_id").is_none());
    assert!(unchanged.get("stream").is_none());
    restoration.restore(&mut unchanged).unwrap();
    assert_eq!(unchanged, first_request);

    let mut changed = logical_next_request;
    changed["text"]["verbosity"] = json!("medium");
    let logical_changed = changed.clone();
    let restoration = client
        .prepare_websocket_request(
            &mut changed,
            WebSocketRequestMode::Inference,
            RequestInputIdentity::Exact,
        )
        .unwrap();
    assert!(changed.get("previous_response_id").is_none());
    restoration.restore(&mut changed).unwrap();
    assert_eq!(changed, logical_changed);
}

#[test]
fn remote_compaction_v2_reuses_the_turn_websocket_prefix() {
    let mut client = test_client("http://127.0.0.1:1".to_string());
    let user = user_message("one");
    let output = assistant_item("first");
    let first_request = client.build_request(vec![user.clone()], RequestKind::Turn);
    client.websocket_baseline = Some(WebSocketBaseline {
        properties: reusable_request_properties(&first_request).unwrap(),
        input: WebSocketBaselineInput::Exact {
            request: first_request["input"].as_array().unwrap().clone(),
            output: vec![output.clone()],
        },
        response_id: "resp_turn".to_string(),
    });
    let trigger = json!({"type": "compaction_trigger"});
    let mut compact_request = client.build_request(
        vec![user, output, trigger.clone()],
        RequestKind::Compaction(CompactionRequest::Automatic(CompactionPhase::MidTurn)),
    );
    let logical_request = compact_request.clone();

    let restoration = client
        .prepare_websocket_request(
            &mut compact_request,
            WebSocketRequestMode::Inference,
            RequestInputIdentity::Exact,
        )
        .unwrap();

    assert_eq!(compact_request["previous_response_id"], "resp_turn");
    assert_eq!(compact_request["input"], json!([trigger]));
    restoration.restore(&mut compact_request).unwrap();
    assert_eq!(compact_request, logical_request);
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

#[test]
fn websocket_events_validate_model_and_latch_first_metadata_turn_state() {
    let mut client = test_client("http://127.0.0.1:1".to_string());
    client.capture_event_turn_state(&json!({
        "type": "response.metadata",
        "headers": {"X-Codex-Turn-State": ["first"]},
    }));
    client.capture_event_turn_state(&json!({
        "type": "response.metadata",
        "headers": {"x-codex-turn-state": "replacement"},
    }));
    assert_eq!(client.turn_state.as_deref(), Some("first"));

    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::default();
    let error = process_event_value(
        json!({
            "type": "response.metadata",
            "headers": {"X-OpenAI-Model": ["gpt-5.5"]},
        }),
        &mut collected,
        &completed_items,
        None,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("backend returned model `gpt-5.5`")
    );

    let error = process_event_value(
        json!({
            "type": "response.created",
            "response": {"id": "resp_wrong", "model": "gpt-5.5"},
        }),
        &mut collected,
        &completed_items,
        None,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("backend returned model `gpt-5.5`")
    );

    let mut fresh_client = test_client("http://127.0.0.1:1".to_string());
    fresh_client.capture_event_turn_state(&json!({
        "type": "response.completed",
        "headers": {"x-codex-turn-state": "not-metadata"},
    }));
    assert!(fresh_client.turn_state.is_none());
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
async fn http_transport_collects_large_sse_events_from_small_http_chunks() {
    let text = "x".repeat(256 * 1_024);
    let item = json!({
        "id": "msg_fragmented",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": text}],
    });
    let (base_url, requests, server) = spawn_http_server(vec![
        HttpReply::ok("text/event-stream", completed_sse("resp_fragmented", &item))
            .in_chunks_of(1_024),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let response = client
        .respond(vec![user_message("large response")], &completed_items)
        .await
        .unwrap();

    assert_eq!(response.items.as_slice(), std::slice::from_ref(&item));
    assert_eq!(response.final_answer.as_deref(), Some(text.as_str()));
    assert_eq!(completed_rx.try_recv().unwrap(), item);
    assert!(completed_rx.try_recv().is_err());
    assert_eq!(
        requests.recv_timeout(Duration::from_secs(2)).unwrap().path,
        "/responses"
    );
    server.join().unwrap();
}

#[tokio::test]
async fn http_keepalive_comments_do_not_hide_model_event_inactivity() {
    let (base_url, requests, server) = spawn_idle_http_server();
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    client.stream_idle_timeout = Duration::from_millis(50);
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let error = client
        .respond(vec![user_message("idle")], &completed_items)
        .await
        .err()
        .expect("SSE comments must not keep a response alive without model events");

    assert!(error.is_stream_idle());
    assert_eq!(
        requests.recv_timeout(Duration::from_secs(2)).unwrap().path,
        "/responses"
    );
    server.join().unwrap();
}

#[tokio::test]
async fn transport_model_header_cannot_silently_route_to_another_model() {
    let item = assistant_item("wrong model");
    let (base_url, _requests, server) = spawn_http_server(vec![
        HttpReply::ok(
            "text/event-stream",
            completed_sse("resp_wrong_model", &item),
        )
        .with_header("openai-model", "gpt-5.5"),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let error = client
        .respond(vec![user_message("hello")], &completed_items)
        .await
        .err()
        .expect("the mismatched transport model must be rejected");
    assert!(
        error
            .to_string()
            .contains("backend returned model `gpt-5.5`")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn http_transport_uses_the_pinned_four_retry_request_policy() {
    let item = assistant_item("recovered");
    let retry = || HttpReply::status(503, "text/plain", "busy").with_header("retry-after", "0");
    let (base_url, requests, server) = spawn_http_server(vec![
        retry(),
        retry(),
        retry(),
        retry(),
        HttpReply::ok("text/event-stream", completed_sse("resp_recovered", &item)),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let response = client
        .respond(vec![user_message("retry")], &completed_items)
        .await
        .unwrap();
    assert_eq!(response.final_answer.as_deref(), Some("recovered"));
    for _ in 0..5 {
        requests.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    assert!(requests.try_recv().is_err());
    server.join().unwrap();
}

#[tokio::test]
async fn http_rate_limits_are_left_to_the_stream_retry_policy() {
    let (base_url, requests, server) = spawn_http_server(vec![
        HttpReply::status(429, "application/json", "rate limited").with_header("retry-after", "0"),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let error = client
        .respond(vec![user_message("retry")], &completed_items)
        .await
        .err()
        .expect("the rate limit should be returned to the stream retry loop");
    assert!(error.is_retryable());
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(requests.try_recv().is_err());
    server.join().unwrap();
}

#[tokio::test]
async fn http_error_bodies_are_bounded_while_streaming() {
    let (base_url, _requests, server) = spawn_http_server(vec![HttpReply::status(
        400,
        "text/plain",
        "x".repeat(100_000),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let error = client
        .respond(vec![user_message("error")], &completed_items)
        .await
        .err()
        .expect("the HTTP error response must fail");
    assert!(error.to_string().len() < 5_000);
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
async fn remote_compaction_v2_uses_the_responses_stream_and_builds_bounded_history() {
    let compaction =
        json!({"type": "compaction_summary", "id": "cmp_fixture", "encrypted_content": "opaque"});
    let ignored = assistant_item("ignored compact response text");
    let (base_url, requests, server) = spawn_http_server(vec![HttpReply::ok(
        "text/event-stream",
        completed_sse_with_items("resp_compact", &[ignored, compaction.clone()]),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;

    let compacted = client
        .compact(
            &[user_message("old")],
            CompactionRequest::Automatic(CompactionPhase::PreTurn),
        )
        .await
        .unwrap();
    assert_eq!(compacted.items, vec![user_message("old"), compaction]);
    assert_eq!(compacted.usage.unwrap().total_tokens, 50);
    assert_eq!(client.window, 0, "transport success is not installation");
    client.commit_compaction();
    assert_eq!(client.window, 1);
    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.path, "/responses");
    assert!(
        request
            .headers
            .contains("x-codex-beta-features: remote_compaction_v2")
    );
    let request_body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        request_body["reasoning"],
        json!({"effort": "max", "summary": "auto", "context": "all_turns"})
    );
    assert_eq!(request_body["prompt_cache_key"], "session-test");
    assert_eq!(
        request_body["input"].as_array().unwrap().last().unwrap(),
        &json!({"type": "compaction_trigger"})
    );
    let metadata: Value = serde_json::from_str(
        request_body["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        metadata["compaction"],
        json!({
            "trigger": "auto",
            "reason": "context_limit",
            "implementation": "responses_compaction_v2",
            "phase": "pre_turn",
            "strategy": "memento",
        })
    );
    server.join().unwrap();
}

#[tokio::test]
async fn remote_compaction_request_is_bounded_by_the_effective_context_window() {
    let compaction =
        json!({"type": "compaction_summary", "id": "cmp_fixture", "encrypted_content": "opaque"});
    let (base_url, requests, server) = spawn_http_server(vec![HttpReply::ok(
        "text/event-stream",
        completed_sse("resp_compact", &compaction),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;

    let call = json!({
        "type": "custom_tool_call",
        "id": "ctc_1",
        "call_id": "call_1",
        "name": "exec",
        "input": "text(true)",
    });
    let empty_output = json!({
        "type": "custom_tool_call_output",
        "id": "ctco_1",
        "call_id": "call_1",
        "name": "exec",
        "output": "",
    });
    let fixed_history = vec![user_message("keep"), call, empty_output.clone()];
    let trigger = compaction::compaction_trigger();
    let prefix_tokens = estimated_tokens(&compose_input(fixed_history.clone()))
        .saturating_sub(estimated_tokens(&fixed_history))
        .saturating_add(estimated_harness_instruction_tokens())
        .saturating_add(estimated_tokens(std::slice::from_ref(&trigger)));
    let target_request_tokens =
        EFFECTIVE_CONTEXT_WINDOW + (RAW_CONTEXT_WINDOW - EFFECTIVE_CONTEXT_WINDOW) / 2;
    let fixed_tokens = estimated_tokens(&fixed_history);
    let payload_tokens = target_request_tokens
        .saturating_sub(prefix_tokens)
        .saturating_sub(fixed_tokens);
    let mut oversized_output = empty_output;
    oversized_output["output"] =
        Value::String("x".repeat(usize::try_from(payload_tokens.saturating_mul(4)).unwrap()));
    let history = vec![
        fixed_history[0].clone(),
        fixed_history[1].clone(),
        oversized_output,
    ];
    let request_tokens = prefix_tokens.saturating_add(estimated_tokens(&history));
    assert!(request_tokens > EFFECTIVE_CONTEXT_WINDOW);
    assert!(request_tokens <= RAW_CONTEXT_WINDOW);

    client
        .compact(
            &history,
            CompactionRequest::Automatic(CompactionPhase::MidTurn),
        )
        .await
        .unwrap();

    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let request_body: Value = serde_json::from_slice(&request.body).unwrap();
    let sent_output = request_body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("custom_tool_call_output"))
        .unwrap();
    assert_eq!(
        sent_output["output"],
        "Output exceeded the available model context and was truncated"
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

#[tokio::test]
async fn websocket_prewarm_and_continuations_match_the_responses_contract() {
    let websocket_config = websocket::websocket_config();
    assert_eq!(
        websocket_config.max_message_size,
        Some(MAX_STREAM_EVENT_BYTES)
    );
    assert!(websocket_config.extensions.permessage_deflate.is_some());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, mut requests_rx) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async_with_config(stream, Some(websocket::websocket_config()))
            .await
            .unwrap();
        for index in 0..4 {
            let request = websocket.next().await.unwrap().unwrap();
            let Message::Text(request) = request else {
                panic!("expected a text request")
            };
            requests_tx
                .send(serde_json::from_str::<Value>(&request).unwrap())
                .unwrap();
            match index {
                0 => {
                    websocket
                        .send(Message::Text(
                            json!({
                                "type": "response.completed",
                                "response": {
                                    "id": "resp_warm",
                                    "model": MODEL,
                                    "reasoning": {"context": "all_turns"},
                                    "output": [],
                                },
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .unwrap();
                }
                1 | 3 => {
                    let (response_id, text) = if index == 1 {
                        ("resp_1", "first")
                    } else {
                        ("resp_2", "second")
                    };
                    let item = assistant_item(text);
                    if index == 1 {
                        websocket
                            .send(Message::Text(
                                json!({
                                    "type": "response.metadata",
                                    "headers": {
                                        "x-codex-turn-state": "sticky-turn-state",
                                    },
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
                            completed_event(response_id, &item).to_string().into(),
                        ))
                        .await
                        .unwrap();
                }
                2 => {
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
                }
                _ => unreachable!(),
            }
        }
    });
    let mut client = test_client(format!("http://{address}"));
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut history = vec![user_message("one")];
    let first = client
        .respond(history.clone(), &completed_items)
        .await
        .unwrap();
    history.extend(first.items);
    history.push(user_message("two"));
    let second = client.respond(history, &completed_items).await.unwrap();
    assert_eq!(second.final_answer.as_deref(), Some("second"));

    let warmup_request = requests_rx.recv().await.unwrap();
    let first_request = requests_rx.recv().await.unwrap();
    let delta_request = requests_rx.recv().await.unwrap();
    let recovered_request = requests_rx.recv().await.unwrap();

    assert_eq!(warmup_request["type"], "response.create");
    assert_eq!(warmup_request["generate"], false);
    assert!(warmup_request.get("max_output_tokens").is_none());
    assert!(warmup_request.get("stream").is_none());
    assert!(warmup_request.get("background").is_none());
    assert!(warmup_request.get("previous_response_id").is_none());
    assert_eq!(warmup_request["instructions"], harness_instructions());
    assert_eq!(warmup_request["input"].as_array().unwrap().len(), 1);
    assert_eq!(
        warmup_request["client_metadata"][WS_RESPONSES_LITE_CLIENT_METADATA],
        "true"
    );
    let warmup_metadata: Value = serde_json::from_str(
        warmup_request["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(warmup_metadata["request_kind"], "prewarm");

    assert_eq!(first_request["type"], "response.create");
    assert!(first_request.get("max_output_tokens").is_none());
    assert!(first_request.get("stream").is_none());
    assert!(first_request.get("generate").is_none());
    assert_eq!(first_request["previous_response_id"], "resp_warm");
    assert_eq!(first_request["input"], json!([user_message("one")]));
    assert_eq!(
        first_request["client_metadata"][WS_RESPONSES_LITE_CLIENT_METADATA],
        "true"
    );
    assert_eq!(delta_request["previous_response_id"], "resp_1");
    assert_eq!(delta_request["input"], json!([user_message("two")]));
    assert_eq!(
        delta_request["client_metadata"][X_CODEX_TURN_STATE],
        "sticky-turn-state"
    );
    assert!(delta_request.get("stream").is_none());
    assert!(recovered_request.get("previous_response_id").is_none());
    assert_eq!(
        recovered_request["client_metadata"][X_CODEX_TURN_STATE],
        "sticky-turn-state"
    );
    assert!(recovered_request.get("stream").is_none());
    assert_eq!(
        recovered_request["input"].as_array().unwrap().last(),
        Some(&user_message("two"))
    );
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_pings_do_not_hide_inactivity_and_recovery_goes_directly_to_https() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let item = assistant_item("https recovery");
    let response_item = item.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async_with_config(stream, Some(websocket::websocket_config()))
            .await
            .unwrap();

        let Message::Text(warmup) = websocket.next().await.unwrap().unwrap() else {
            panic!("expected warmup request")
        };
        let warmup: Value = serde_json::from_str(&warmup).unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_warm",
                        "model": MODEL,
                        "reasoning": {"context": "all_turns"},
                        "output": [],
                    },
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        let Message::Text(stalled) = websocket.next().await.unwrap().unwrap() else {
            panic!("expected inference request")
        };
        let stalled: Value = serde_json::from_str(&stalled).unwrap();
        for _ in 0..20 {
            if websocket
                .send(Message::Ping(Vec::new().into()))
                .await
                .is_err()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(websocket);

        let (http_stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("client did not fall back to HTTPS")
            .unwrap();
        let mut http_stream = http_stream.into_std().unwrap();
        http_stream.set_nonblocking(false).unwrap();
        let http_request = read_request(&mut http_stream);
        let body = completed_sse("resp_https_recovery", &response_item);
        write!(
            http_stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        )
        .unwrap();
        http_stream.flush().unwrap();
        (warmup, stalled, http_request)
    });

    let mut client = test_client(format!("http://{address}"));
    client.stream_idle_timeout = Duration::from_millis(50);
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        client.respond_streaming(vec![user_message("recover me")], &completed_items, &events),
    )
    .await
    .expect("response recovery timed out")
    .unwrap();

    assert_eq!(response.final_answer.as_deref(), Some("https recovery"));
    assert!(!client.prefer_websocket);
    let warnings = std::iter::from_fn(|| event_rx.try_recv().ok())
        .filter_map(|event| match event {
            AgentEvent::Warning(message) => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        warnings,
        ["Responses WebSocket became inactive; retrying once over HTTPS"]
    );

    let (warmup, stalled, http_request) = server.await.unwrap();
    assert_eq!(warmup["generate"], false);
    assert!(stalled.to_string().contains("recover me"));
    assert_eq!(http_request.path, "/responses");
    let http_body: Value = serde_json::from_slice(&http_request.body).unwrap();
    assert!(http_body.to_string().contains("recover me"));
}

struct HttpReply {
    status: u16,
    content_type: &'static str,
    body: String,
    headers: Vec<(&'static str, &'static str)>,
    body_chunk_bytes: Option<usize>,
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
            body_chunk_bytes: None,
        }
    }

    fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }

    fn in_chunks_of(mut self, bytes: usize) -> Self {
        assert!(bytes > 0);
        self.body_chunk_bytes = Some(bytes);
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
            let body_framing = reply.body_chunk_bytes.map_or_else(
                || format!("Content-Length: {}\r\n", reply.body.len()),
                |_| "Transfer-Encoding: chunked\r\n".to_string(),
            );
            write!(
                stream,
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\n{}{}Connection: close\r\n\r\n",
                reply.status, reason, reply.content_type, body_framing, extra_headers,
            )
            .unwrap();
            if let Some(chunk_bytes) = reply.body_chunk_bytes {
                stream.set_nodelay(true).unwrap();
                for chunk in reply.body.as_bytes().chunks(chunk_bytes) {
                    write!(stream, "{:x}\r\n", chunk.len()).unwrap();
                    stream.write_all(chunk).unwrap();
                    stream.write_all(b"\r\n").unwrap();
                    stream.flush().unwrap();
                }
                stream.write_all(b"0\r\n\r\n").unwrap();
            } else {
                stream.write_all(reply.body.as_bytes()).unwrap();
            }
            stream.flush().unwrap();
        }
    });
    (format!("http://{address}"), requests_rx, server)
}

fn spawn_idle_http_server() -> (
    String,
    std_mpsc::Receiver<CapturedRequest>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, requests_rx) = std_mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        requests_tx.send(read_request(&mut stream)).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        stream.flush().unwrap();
        let keepalive = b": keepalive\n\n";
        for _ in 0..20 {
            if write!(stream, "{:x}\r\n", keepalive.len())
                .and_then(|()| stream.write_all(keepalive))
                .and_then(|()| stream.write_all(b"\r\n"))
                .and_then(|()| stream.flush())
                .is_err()
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = stream.write_all(b"0\r\n\r\n");
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
