use super::*;
use crate::compaction::CompactionPhase;
use crate::compaction::CompactionRequest;
use crate::model::DEFAULT_MODEL as MODEL;
use futures_util::SinkExt;
use futures_util::StreamExt;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::pem::PemObject;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tokio::io::AsyncWriteExt as _;

fn test_client(base_url: String) -> ApiClient {
    let identity = SessionIdentity {
        installation_id: "installation-test".to_string(),
        session_id: "session-test".to_string(),
        thread_id: "thread-test".to_string(),
    };
    ApiClient::new_with_base_url(
        Auth::for_test("token-test"),
        &identity,
        0,
        ModelSelection::default(),
        ServiceTier::default(),
        base_url,
    )
    .unwrap()
}

trait TestApiClient {
    async fn respond(
        &mut self,
        history: Vec<Value>,
        completed_items: &UnboundedSender<Value>,
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
        let request = self.build_request(history, RequestKind::Turn);
        self.respond_request_with_events(
            &request,
            completed_items,
            None,
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
        let mut completed = CompletedResponseMetadata::default();
        self.compact_with_identity(
            history,
            compaction,
            RequestInputIdentity::Exact,
            None,
            &mut completed,
        )
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

fn serialized_value(value: &impl Serialize) -> Value {
    serde_json::from_slice(&serde_json::to_vec(value).unwrap()).unwrap()
}

#[test]
fn websocket_incremental_input_requires_matching_request_properties() {
    let mut client = test_client("http://127.0.0.1:1".to_string());
    let first = user_message("first");
    let second = user_message("second");
    let first_request = client.build_request(vec![first.clone()], RequestKind::Turn);
    let warmup = serialized_value(&client.prepare_websocket_request(
        &first_request,
        WebSocketRequestMode::Warmup,
        RequestInputIdentity::Exact,
    ));
    assert_eq!(warmup["type"], "response.create");
    assert_eq!(warmup["generate"], false);
    assert_eq!(warmup["instructions"], harness_instructions());
    assert_eq!(warmup["parallel_tool_calls"], true);
    assert_eq!(
        warmup["reasoning"],
        json!({"effort": "xhigh", "context": "all_turns"})
    );
    assert_eq!(
        warmup["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| {
                tool.get("name")
                    .or_else(|| tool.get("type"))
                    .and_then(Value::as_str)
                    .unwrap()
            })
            .collect::<Vec<_>>(),
        vec!["bash", "read", "write", "edit", "web_search"]
    );
    assert!(warmup.get("stream").is_none());
    let mut response = ModelResponse {
        items: Vec::new(),
        tool_calls: Vec::new(),
        final_answer: None,
        end_turn: None,
        usage: None,
        rate_limits: Vec::new(),
        server_reasoning_included: false,
        response_id: "response-first".to_string(),
        output_item_count: 0,
        compaction_item_count: 0,
        has_assistant_text: false,
    };
    client.websocket_baseline = Some(
        WebSocketBaseline::new(&first_request, &mut response, RequestInputIdentity::Exact).unwrap(),
    );

    let incremental_request =
        client.build_request(vec![first.clone(), second.clone()], RequestKind::Turn);
    let incremental = client.prepare_websocket_request(
        &incremental_request,
        WebSocketRequestMode::Inference,
        RequestInputIdentity::Exact,
    );
    let incremental = serialized_value(&incremental);
    assert_eq!(incremental["previous_response_id"], "response-first");
    assert_eq!(incremental["input"], json!([second]));

    let mut changed_request =
        client.build_request(vec![first, user_message("changed")], RequestKind::Turn);
    changed_request.parallel_tool_calls = false;
    let changed = client.prepare_websocket_request(
        &changed_request,
        WebSocketRequestMode::Inference,
        RequestInputIdentity::Exact,
    );
    let changed = serialized_value(&changed);
    assert!(changed.get("previous_response_id").is_none());
    assert_eq!(
        changed["input"],
        json!([user_message("first"), user_message("changed")])
    );
}

#[test]
fn request_serialization_omits_local_metadata_and_unprefixed_ids_without_mutating_history() {
    let client = test_client("http://127.0.0.1:1".to_string());
    let mut input = vec![
        json!({
            "id": "msg_existing",
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "prefixed"}],
        }),
        json!({
            "id": "018f9e15-7a6a-7000-8000-000000000001",
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "legacy UUID"}],
        }),
        json!({
            "id": "",
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "empty ID"}],
        }),
        json!({
            "id": null,
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "null ID"}],
        }),
        json!({
            "id": 7,
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "non-string ID"}],
        }),
    ];
    crate::context::mark_operator_user_message(&mut input[0]);
    let request = client.build_request(input.clone(), RequestKind::Turn);
    let http = serialized_value(&request);
    let websocket = serialized_value(&client.prepare_websocket_request(
        &request,
        WebSocketRequestMode::Warmup,
        RequestInputIdentity::Exact,
    ));

    for serialized in [&http, &websocket] {
        let serialized_input = serialized["input"].as_array().unwrap();
        assert_eq!(serialized_input.len(), input.len());
        assert_eq!(serialized_input[0]["id"], "msg_existing");
        assert!(
            serialized_input
                .iter()
                .all(|item| { item.get(crate::context::USER_MESSAGE_KIND_FIELD).is_none() })
        );
        for item in &serialized_input[1..] {
            assert!(item.get("id").is_none());
        }
    }
    let mut common_http = http;
    common_http.as_object_mut().unwrap().remove("stream");
    let mut common_websocket = websocket;
    common_websocket.as_object_mut().unwrap().remove("type");
    common_websocket.as_object_mut().unwrap().remove("generate");
    assert_eq!(common_websocket, common_http);
    assert_eq!(request.input, input);
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
fn sse_decoder_accepts_standard_line_endings_and_a_split_leading_bom() {
    let mut decoder = SseDecoder::default();
    let mut events = Vec::new();

    decoder.push(b"\xef", &mut events).unwrap();
    decoder.push(b"\xbb\xbfdata: first\r", &mut events).unwrap();
    decoder.push(b"", &mut events).unwrap();
    decoder
        .push(b"\n\rdata: second\r\rdata: third\n\n", &mut events)
        .unwrap();
    decoder.finish(&mut events).unwrap();

    assert_eq!(events, ["first", "second", "third"]);
}

#[test]
fn completed_event_records_full_cache_usage() {
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::Retain);
    let mut server_model_warning_emitted = false;
    let item = assistant_item("done");
    process_event_value(
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 1,
            "item": item,
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    process_event_value(
        completed_event("resp_usage", &item),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
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
fn completed_response_id_must_be_present_and_match_created() {
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    for (completed_id, expected_error) in [
        (Some("resp_completed"), "changed response IDs"),
        (None, "omitted the response ID"),
    ] {
        let mut collected = CollectedResponse::new(OutputItemMode::Retain);
        let mut server_model_warning_emitted = false;
        process_event_value(
            json!({
                "type": "response.created",
                "sequence_number": 1,
                "response": {"id": "resp_created"},
            }),
            &mut collected,
            &completed_items,
            None,
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap();
        let mut completed = completed_event(completed_id.unwrap_or_default(), &Value::Null);
        completed["sequence_number"] = json!(2);
        completed["response"]["output"] = json!([]);
        if completed_id.is_none() {
            completed["response"].as_object_mut().unwrap().remove("id");
        }

        let error = process_event_value(
            completed,
            &mut collected,
            &completed_items,
            None,
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap_err();

        assert!(error.to_string().contains(expected_error), "{error}");
        assert!(!collected.completed);
    }
}

#[test]
fn websocket_response_boundary_requires_a_new_response_identity() {
    let mut boundary = WebSocketResponseBoundary::new(None);
    let error = boundary
        .accepts(&json!({"type": "response.created", "response": {}}))
        .unwrap_err();
    assert!(error.to_string().contains("response.created omitted"));

    let mut boundary = WebSocketResponseBoundary::new(None);
    let mut completed = completed_event("resp_without_created", &Value::Null);
    completed["response"]["output"] = json!([]);
    let mut error = boundary.accepts(&completed).unwrap_err();
    assert!(error.to_string().contains("before response.created"));
    let (usage, rate_limits) = error
        .take_completed_response()
        .expect("completed response metadata");
    assert_eq!(usage.unwrap().total_tokens, 50);
    assert!(rate_limits.is_empty());
}

#[test]
fn malformed_completed_metadata_cannot_finish_a_response() {
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    for (field, value, expected_error) in [
        ("usage", json!("not usage"), "usage was not an object"),
        ("end_turn", json!("yes"), "invalid end_turn"),
    ] {
        let mut collected = CollectedResponse::new(OutputItemMode::Retain);
        let mut server_model_warning_emitted = false;
        let mut completed = completed_event("resp_malformed_metadata", &Value::Null);
        completed["response"]["output"] = json!([]);
        completed["response"][field] = value;

        let error = process_event_value(
            completed,
            &mut collected,
            &completed_items,
            None,
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap_err();

        assert!(error.to_string().contains(expected_error), "{error}");
        assert!(!collected.completed);
    }
}

#[test]
fn decoded_http_events_precede_a_later_chunk_failure() {
    let (completed_items, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::Transfer);
    let mut server_model_warning_emitted = false;
    let item = assistant_item("before trailing failure");
    let mut completed = completed_event("resp_before_trailing_failure", &item);
    completed["sequence_number"] = json!(2);
    let mut decoded = vec![
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 1,
            "item": item,
        })
        .to_string(),
        completed.to_string(),
    ];
    let mut validation = ResponseValidation {
        expected_model: MODEL,
        server_model_warning_emitted: &mut server_model_warning_emitted,
    };

    let complete = process_decoded_http_events(
        &mut decoded,
        Err(ApiError::fatal("malformed trailing SSE bytes")),
        &mut collected,
        &completed_items,
        None,
        &mut validation,
        Instant::now(),
    )
    .unwrap();

    assert!(complete);
    assert_eq!(received.try_recv().unwrap(), item);
    assert!(received.try_recv().is_err());
    assert_eq!(
        collected.finish().unwrap().response_id,
        "resp_before_trailing_failure"
    );
}

#[test]
fn response_rate_limit_events_are_retained_for_status() {
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::Retain);
    let mut server_model_warning_emitted = false;
    process_event_value(
        json!({
            "type": "codex.rate_limits",
            "metered_limit_name": "codex_other",
            "rate_limits": {
                "primary": {
                    "used_percent": 12.5,
                    "window_minutes": 300,
                    "reset_at": 1_704_069_000_i64,
                },
            },
            "credits": {
                "has_credits": true,
                "unlimited": false,
                "balance": "42.4",
            },
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    let mut completed = completed_event("resp_limits", &Value::Null);
    completed["response"]["output"] = json!([]);
    process_event_value(
        completed,
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();

    let response = collected.finish().unwrap();
    let snapshot = response.rate_limits.first().expect("rate-limit snapshot");
    assert_eq!(snapshot.limit_id, "codex_other");
    assert_eq!(snapshot.primary.as_ref().unwrap().used_percent, 12.5);
    assert_eq!(snapshot.primary.as_ref().unwrap().window_minutes, Some(300));
    assert_eq!(
        snapshot.credits.as_ref().unwrap().balance.as_deref(),
        Some("42.4")
    );
}

#[test]
fn response_rate_limit_headers_are_parsed_for_status() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-codex-primary-used-percent",
        reqwest::header::HeaderValue::from_static("19"),
    );
    headers.insert(
        "x-codex-primary-window-minutes",
        reqwest::header::HeaderValue::from_static("300"),
    );
    headers.insert(
        "x-codex-primary-reset-at",
        reqwest::header::HeaderValue::from_static("1704069000"),
    );

    let snapshots = crate::rate_limits::parse_all_rate_limits(&headers);

    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].limit_id, "codex");
    assert_eq!(snapshots[0].primary.as_ref().unwrap().used_percent, 19.0);
    assert_eq!(
        snapshots[0].primary.as_ref().unwrap().window_minutes,
        Some(300)
    );
}

#[tokio::test]
async fn status_rate_limit_prefetch_reads_chatgpt_usage_with_account_auth() {
    let body = json!({
        "plan_type": "pro",
        "rate_limit": {
            "secondary_window": {
                "used_percent": 9,
                "limit_window_seconds": 604_800,
                "reset_after_seconds": 0,
                "reset_at": 1_704_069_000,
            }
        },
        "additional_rate_limits": [{
            "limit_name": "GPT-5.3-Codex-Spark",
            "metered_feature": "codex_spark",
            "rate_limit": {
                "secondary_window": {
                    "used_percent": 0,
                    "limit_window_seconds": 604_800,
                    "reset_after_seconds": 0,
                    "reset_at": 1_704_069_900,
                }
            }
        }]
    })
    .to_string();
    let (origin, requests, server) =
        spawn_http_server(vec![HttpReply::ok("application/json", body)]);
    let client = test_client(format!("{origin}/backend-api/codex"));

    let snapshots = client.rate_limit_client().fetch().await.unwrap();

    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].limit_id, "codex");
    assert_eq!(
        snapshots[0].secondary.as_ref().unwrap().window_minutes,
        Some(10_080)
    );
    assert_eq!(snapshots[1].limit_id, "codex_spark");
    assert_eq!(
        snapshots[1].limit_name.as_deref(),
        Some("GPT-5.3-Codex-Spark")
    );
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request.path, "/backend-api/wham/usage");
    let headers = request.headers.to_ascii_lowercase();
    assert!(headers.contains("authorization: bearer token-test"));
    assert!(headers.contains("chatgpt-account-id: test-account"));
    server.join().unwrap();
}

#[test]
fn completed_items_ignore_sparse_indexes_and_are_emitted_once_in_stream_order() {
    let (completed_items, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut received_events) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::Transfer);
    let mut server_model_warning_emitted = false;
    let first = json!({
        "type": "function_call",
        "call_id": "call_1",
        "namespace": "functions",
        "name": "bash",
        "arguments": "{\"command\":\"printf done\"}",
    });
    let second = assistant_item_with_phase("working", "commentary");
    let third = assistant_item_with_phase("done", "final_answer");

    for (output_index, sequence_number, item) in [
        (Some(0), 1, &first),
        (None, 3, &second),
        (Some(5), 5, &third),
    ] {
        let mut done = json!({
            "type": "response.output_item.done",
            "sequence_number": sequence_number,
            "item": item,
        });
        if let Some(output_index) = output_index {
            done["output_index"] = json!(output_index);
        }
        process_event_value(
            done.clone(),
            &mut collected,
            &completed_items,
            Some(&events),
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap();
        assert_eq!(received.try_recv().unwrap(), *item);
        done["sequence_number"] = json!(sequence_number + 1);
        process_event_value(
            done,
            &mut collected,
            &completed_items,
            Some(&events),
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap();
        assert!(received.try_recv().is_err());
    }

    assert!(received.try_recv().is_err());
    let mut completed = completed_event("resp_summary", &Value::Null);
    completed["sequence_number"] = json!(7);
    completed["response"]["output"] = json!([first, second, third,]);
    process_event_value(
        completed,
        &mut collected,
        &completed_items,
        Some(&events),
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    process_event_value(
        json!({
            "type": "response.output_item.done",
            "output_index": 8,
            "sequence_number": 8,
            "item": assistant_item("late duplicate"),
        }),
        &mut collected,
        &completed_items,
        Some(&events),
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    assert!(received.try_recv().is_err());
    assert_eq!(
        received_events.try_recv().unwrap(),
        AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: "working".to_string(),
            phase: Some(crate::protocol::MessagePhase::Commentary),
            citations: Vec::new(),
        })
    );
    assert_eq!(
        received_events.try_recv().unwrap(),
        AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: "done".to_string(),
            phase: Some(crate::protocol::MessagePhase::FinalAnswer),
            citations: Vec::new(),
        })
    );
    assert!(received_events.try_recv().is_err());
    let response = collected.finish().unwrap();
    assert_eq!(
        response.tool_calls,
        vec![ToolCall::from_response_item(&first).unwrap()]
    );
    assert_eq!(response.final_answer.as_deref(), Some("done"));
    assert!(response.has_assistant_text());
}

#[test]
fn conflicting_reuse_of_a_function_call_id_is_rejected_without_indexes() {
    let (completed_items, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::Transfer);
    let mut server_model_warning_emitted = false;
    let first = json!({
        "type": "function_call",
        "id": "fc_first",
        "call_id": "call_reused",
        "name": "bash",
        "arguments": "{\"command\":\"printf first\"}",
    });
    process_event_value(
        json!({
            "type": "response.output_item.done",
            "sequence_number": 1,
            "item": first,
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    assert_eq!(received.try_recv().unwrap(), first);

    let error = process_event_value(
        json!({
            "type": "response.output_item.done",
            "sequence_number": 2,
            "item": {
                "type": "function_call",
                "id": "fc_second",
                "call_id": "call_reused",
                "name": "bash",
                "arguments": "{\"command\":\"printf second\"}",
            },
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap_err();

    assert!(error.to_string().contains("conflicting duplicate"));
    assert!(received.try_recv().is_err());
}

#[test]
fn completed_response_output_is_not_replayed_or_reconciled() {
    let (completed_items, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::Transfer);
    let mut server_model_warning_emitted = false;
    let interrupted_search = json!({
        "type": "web_search_call",
        "id": "ws_interrupted_completion",
        "status": "in_progress",
    });
    let answer = assistant_item_with_phase("answer", "final_answer");
    process_event_value(
        json!({
            "type": "codex.rate_limits",
            "rate_limits": {
                "primary": {
                    "used_percent": 17.0,
                    "window_minutes": 300,
                    "reset_at": 1_704_069_000_i64,
                },
            },
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    process_event_value(
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "sequence_number": 1,
            "item": interrupted_search,
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    process_event_value(
        json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "sequence_number": 2,
            "item": answer,
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    assert_eq!(received.try_recv().unwrap(), answer);

    let mut completed = completed_event("resp_interrupted_completion", &Value::Null);
    completed["sequence_number"] = json!(3);
    completed["response"]["output"] = json!([interrupted_search, answer]);
    process_event_value(
        completed,
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();

    assert!(received.try_recv().is_err());
    assert!(collected.completed);
    let response = collected.finish().unwrap();
    assert_eq!(response.output_item_count, 1);
    assert_eq!(response.final_answer.as_deref(), Some("answer"));
    assert_eq!(response.usage.unwrap().total_tokens, 50);
    assert_eq!(
        response.rate_limits[0]
            .primary
            .as_ref()
            .unwrap()
            .used_percent,
        17.0
    );
}

#[test]
fn hosted_web_search_and_citations_are_forwarded_without_rewriting_history() {
    let (events, mut received_events) = tokio::sync::mpsc::unbounded_channel();
    let (completed_items, mut received_items) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::Transfer);
    let mut server_model_warning_emitted = false;
    let added = json!({
        "type": "web_search_call",
        "id": "ws_1",
        "status": "in_progress",
    });
    for sequence_number in [1, 2] {
        process_event_value(
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "sequence_number": sequence_number,
                "item": added,
            }),
            &mut collected,
            &completed_items,
            Some(&events),
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap();
    }
    assert_eq!(
        received_events.try_recv().unwrap(),
        AgentEvent::WebSearchStarted(crate::web_search::WebSearchCall {
            id: "ws_1".to_string(),
            status: Some("in_progress".to_string()),
            action: None,
        })
    );
    assert!(received_events.try_recv().is_err());

    let completed_search = json!({
        "type": "web_search_call",
        "id": "ws_1",
        "status": "completed",
        "action": {"type": "search", "query": "current Rust release"},
    });
    process_event_value(
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 3,
            "item": completed_search,
        }),
        &mut collected,
        &completed_items,
        Some(&events),
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    assert_eq!(received_items.try_recv().unwrap(), completed_search);
    assert_eq!(
        received_events.try_recv().unwrap(),
        AgentEvent::WebSearchCompleted(
            crate::web_search::WebSearchCall::from_response_item(&completed_search).unwrap()
        )
    );

    let message = json!({
        "type": "message",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{
            "type": "output_text",
            "text": "Rust is current.\u{e200}cite\u{e202}turn0search2\u{e201}",
            "annotations": [{
                "type": "url_citation",
                "start_index": 16,
                "end_index": 35,
                "url": "https://www.rust-lang.org/",
                "title": "Rust",
            }],
        }],
    });
    process_event_value(
        json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "sequence_number": 4,
            "item": message,
        }),
        &mut collected,
        &completed_items,
        Some(&events),
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();
    assert_eq!(received_items.try_recv().unwrap(), message);
    let AgentEvent::ModelMessageCompleted(message) = received_events.try_recv().unwrap() else {
        panic!("expected completed assistant message");
    };
    assert_eq!(
        message.text,
        "Rust is current.\u{e200}cite\u{e202}turn0search2\u{e201}"
    );
    assert_eq!(message.citations.len(), 1);
    assert_eq!(message.citations[0].url, "https://www.rust-lang.org/");
    assert_eq!(
        collected.item_summary.final_answer.as_deref(),
        Some("Rust is current.\n\nSources:\n1. Rust: https://www.rust-lang.org/")
    );
}

#[test]
fn duplicate_sequence_numbers_require_identical_events() {
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::RetainAndEmit);
    let mut server_model_warning_emitted = false;
    let delta_event =
        r#"{"type":"response.output_text.delta","sequence_number":1,"delta":"hello"}"#;
    for _ in 0..2 {
        process_event(
            delta_event,
            &mut collected,
            &completed_items,
            Some(&events),
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap();
    }
    let AgentEvent::ModelMessageDelta(delta) = received.try_recv().unwrap() else {
        panic!("expected model text delta");
    };
    assert_eq!(delta.text, "hello");
    assert!(received.try_recv().is_err());

    let error = process_event(
        r#"{"type":"response.output_text.delta","sequence_number":1,"delta":"different"}"#,
        &mut collected,
        &completed_items,
        Some(&events),
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap_err();
    assert!(error.to_string().contains("reused a sequence number"));
    assert!(received.try_recv().is_err());
}

#[test]
fn completed_output_rejects_late_lifecycle_events() {
    let item = assistant_item_with_phase("complete", "final_answer");
    let late_events = [
        json!({
            "type": "response.output_item.added",
            "output_index": 4,
            "sequence_number": 2,
            "item": item,
        }),
        json!({
            "type": "response.output_text.delta",
            "item_id": item["id"],
            "output_index": 4,
            "content_index": 0,
            "sequence_number": 2,
            "delta": "late",
        }),
    ];

    for late_event in late_events {
        let (completed_items, mut received_items) = tokio::sync::mpsc::unbounded_channel();
        let (events, mut received_events) = tokio::sync::mpsc::unbounded_channel();
        let mut collected = CollectedResponse::new(OutputItemMode::Transfer);
        let mut server_model_warning_emitted = false;
        process_event_value(
            json!({
                "type": "response.output_item.done",
                "output_index": 4,
                "sequence_number": 1,
                "item": item,
            }),
            &mut collected,
            &completed_items,
            Some(&events),
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap();
        assert_eq!(received_items.try_recv().unwrap(), item);
        assert!(matches!(
            received_events.try_recv().unwrap(),
            AgentEvent::ModelMessageCompleted(_)
        ));

        let error = process_event_value(
            late_event,
            &mut collected,
            &completed_items,
            Some(&events),
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap_err();
        assert!(error.to_string().contains("after it was completed"));
        assert!(received_items.try_recv().is_err());
        assert!(received_events.try_recv().is_err());
    }
}

#[test]
fn failed_response_preserves_usage_and_rate_limits() {
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::Retain);
    let mut server_model_warning_emitted = false;
    process_event_value(
        json!({
            "type": "codex.rate_limits",
            "rate_limits": {
                "primary": {
                    "used_percent": 37.0,
                    "window_minutes": 300,
                    "reset_at": 1_704_069_000_i64,
                },
            },
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap();

    let mut error = process_event_value(
        json!({
            "type": "response.failed",
            "sequence_number": 1,
            "response": {
                "id": "resp_failed_with_usage",
                "error": {"code": "server_error", "message": "failed after billing"},
                "usage": {
                    "input_tokens": 40,
                    "input_tokens_details": {
                        "cached_tokens": 30,
                        "cache_write_tokens": 4,
                    },
                    "output_tokens": 2,
                    "output_tokens_details": {"reasoning_tokens": 1},
                    "total_tokens": 42,
                },
            },
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap_err();

    assert!(error.is_retryable());
    let (usage, rate_limits) = error
        .take_completed_response()
        .expect("failed response metadata");
    assert_eq!(usage.unwrap().total_tokens, 42);
    assert_eq!(rate_limits[0].primary.as_ref().unwrap().used_percent, 37.0);
}

#[test]
fn failed_and_incomplete_response_ids_must_match_the_created_response() {
    for kind in ["response.failed", "response.incomplete"] {
        let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut collected = CollectedResponse::new(OutputItemMode::Retain);
        let mut server_model_warning_emitted = false;
        process_event_value(
            json!({
                "type": "response.created",
                "sequence_number": 1,
                "response": {"id": "resp_created"},
            }),
            &mut collected,
            &completed_items,
            None,
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap();
        let mut terminal = completed_event("resp_other", &Value::Null);
        terminal["type"] = json!(kind);
        terminal["sequence_number"] = json!(2);
        terminal["response"]["error"] =
            json!({"code": "server_error", "message": "failed after creation"});
        terminal["response"]["incomplete_details"] = json!({"reason": "max_output_tokens"});

        let mut error = process_event_value(
            terminal,
            &mut collected,
            &completed_items,
            None,
            MODEL,
            &mut server_model_warning_emitted,
        )
        .unwrap_err();

        assert!(!error.is_retryable(), "{kind} accepted another response ID");
        assert!(error.to_string().contains("changed response IDs"));
        let (usage, rate_limits) = error
            .take_completed_response()
            .expect("terminal response metadata");
        assert_eq!(usage.unwrap().total_tokens, 50);
        assert!(rate_limits.is_empty());
    }
}

#[test]
fn wrapped_websocket_errors_preserve_retry_and_rate_limit_headers() {
    let (completed_items, _completed_items_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut collected = CollectedResponse::new(OutputItemMode::Retain);
    let mut server_model_warning_emitted = false;
    let mut error = process_event_value(
        json!({
            "type": "error",
            "status": 429,
            "error": {"type": "usage_limit_reached", "message": "usage limit reached"},
            "headers": {
                "retry-after": 2,
                "x-codex-primary-used-percent": "91.0",
                "x-codex-primary-window-minutes": 300,
            },
        }),
        &mut collected,
        &completed_items,
        None,
        MODEL,
        &mut server_model_warning_emitted,
    )
    .unwrap_err();

    assert!(error.is_retryable());
    assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));
    let (usage, rate_limits) = error
        .take_completed_response()
        .expect("wrapped WebSocket error metadata");
    assert!(usage.is_none());
    assert_eq!(rate_limits.len(), 1);
    assert_eq!(rate_limits[0].primary.as_ref().unwrap().used_percent, 91.0);
    assert_eq!(
        rate_limits[0].primary.as_ref().unwrap().window_minutes,
        Some(300)
    );
}

#[test]
fn stream_errors_classify_websocket_recovery_cases() {
    let previous = error_event(&json!({
        "type": "error",
        "code": "previous_response_not_found",
        "message": "expired",
    }));
    assert_eq!(previous.kind, ApiErrorKind::PreviousResponseNotFound);
    let unauthorized = error_event(&json!({
        "type": "error",
        "status": 401,
        "error": {"message": "expired token"},
    }));
    assert_eq!(unauthorized.kind, ApiErrorKind::Unauthorized);
    let overloaded = error_event(&json!({
        "type": "error",
        "status_code": 503,
        "error": {"message": "busy"},
    }));
    assert!(overloaded.is_retryable());
    let top_level_rate_limit = error_event(&json!({
        "type": "error",
        "code": "rate_limit_exceeded",
        "message": "Rate limit reached. Please try again in 1.898s.",
    }));
    assert!(top_level_rate_limit.is_retryable());
    assert_eq!(
        top_level_rate_limit.retry_after(),
        Some(Duration::from_secs_f64(1.898))
    );
    let idle = ApiError::stream_idle("idle timeout");
    assert!(idle.is_stream_idle());
    assert!(idle.is_retryable());
    assert!(classify_stream_error("future_transient_error", "try again").is_retryable());
    let context_window = classify_stream_error("context_length_exceeded", "too large");
    assert!(context_window.is_context_window_exceeded());
    assert!(!context_window.is_retryable());
    assert!(!classify_stream_error("insufficient_quota", "quota exhausted").is_retryable());
    assert!(!classify_stream_error("misalignment_policy_violation", "blocked").is_retryable());
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

#[tokio::test]
async fn http_transport_sends_the_contract_and_collects_the_response() {
    let item = assistant_item("http");
    let rate_limit = json!({
        "type": "codex.rate_limits",
        "rate_limits": {
            "primary": {
                "used_percent": 12.5,
                "window_minutes": 300,
                "reset_at": 1_704_069_000_i64,
            },
        },
    });
    let body = format!(
        "data: {rate_limit}\n\n{}",
        completed_sse("resp_http", &item)
    );
    let (base_url, requests, server) = spawn_http_server(vec![
        HttpReply::ok("text/event-stream", body)
            .with_header("x-reasoning-included", "true")
            .with_header("openai-model", MODEL)
            .with_header("x-codex-primary-used-percent", "19")
            .with_header("x-codex-credits-has-credits", "true")
            .with_header("x-codex-credits-unlimited", "false")
            .with_header("x-codex-credits-balance", "8.5"),
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
    let rate_limit = response
        .rate_limits
        .iter()
        .find(|snapshot| snapshot.limit_id == "codex")
        .expect("merged Codex rate limit");
    assert_eq!(rate_limit.primary.as_ref().unwrap().used_percent, 12.5);
    assert_eq!(
        rate_limit.credits.as_ref().unwrap().balance.as_deref(),
        Some("8.5")
    );
    assert_eq!(completed_rx.try_recv().unwrap(), item);
    assert!(completed_rx.try_recv().is_err());

    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.path, "/responses");
    assert!(request.headers.contains("authorization: Bearer token-test"));
    assert!(!request.headers.contains("codex-responses-lite"));
    assert!(
        request
            .headers
            .contains(&format!("x-codex-routing-hint: model={MODEL}"))
    );
    assert!(request.headers.contains("content-encoding: zstd"));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], MODEL);
    assert!(body.get("service_tier").is_none());
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["instructions"], harness_instructions());
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(
        body["reasoning"],
        json!({"effort": "xhigh", "context": "all_turns"})
    );
    let tools = &body["tools"];
    assert_eq!(
        tools
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| {
                tool.get("name")
                    .or_else(|| tool.get("type"))
                    .and_then(Value::as_str)
                    .unwrap()
            })
            .collect::<Vec<_>>(),
        vec!["bash", "read", "write", "edit", "web_search"]
    );
    let bash = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "bash")
        .unwrap();
    let timeout = &bash["parameters"]["properties"]["timeout"];
    assert_eq!(timeout["exclusiveMinimum"], 0);
    assert_eq!(timeout["maximum"], json!(i32::MAX as f64 / 1_000.0));
    assert!(
        timeout["description"]
            .as_str()
            .unwrap()
            .contains("no timeout by default")
    );
    let read = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "read")
        .unwrap();
    assert_eq!(
        read["parameters"]["properties"]["detail"]["enum"],
        json!(["high", "original"])
    );
    assert_eq!(read["parameters"]["properties"]["limit"]["maximum"], 2_000);
    let edit = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "edit")
        .unwrap();
    assert_eq!(
        edit["parameters"]["properties"]["edits"]["items"]["properties"]["oldText"]["minLength"],
        1
    );
    let web_search = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["type"] == "web_search")
        .unwrap();
    assert_eq!(web_search["external_web_access"], true);
    assert_eq!(web_search["search_content_types"], json!(["text", "image"]));
    assert!(web_search.get("name").is_none());
    assert!(web_search.get("parameters").is_none());
    assert!(
        tools
            .as_array()
            .unwrap()
            .iter()
            .filter(|tool| tool["type"] == "function")
            .all(|tool| tool["strict"] == false)
    );
    assert!(
        tools
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool.get("output_schema").is_none())
    );
    assert_eq!(body["input"], json!([user_message("hello")]));
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
    assert_eq!(header_turn_metadata, client_turn_metadata);
    server.join().unwrap();
}

#[tokio::test]
async fn http_error_preserves_rate_limit_headers() {
    let (base_url, _requests, server) = spawn_http_server(vec![
        HttpReply::status(429, "application/json", r#"{"error":"rate limited"}"#)
            .with_header("x-codex-primary-used-percent", "91"),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut error = match client
        .respond(vec![user_message("hello")], &completed_items)
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("rate-limited HTTP response unexpectedly succeeded"),
    };

    assert!(error.is_retryable());
    let (usage, rate_limits) = error
        .take_completed_response()
        .expect("HTTP error response metadata");
    assert!(usage.is_none());
    assert_eq!(rate_limits[0].primary.as_ref().unwrap().used_percent, 91.0);
    server.join().unwrap();
}

#[tokio::test]
async fn http_retry_failure_preserves_rate_limit_headers_from_prior_attempts() {
    let mut replies = vec![
        HttpReply::status(500, "application/json", r#"{"error":"retry"}"#)
            .with_header("retry-after", "0")
            .with_header("x-codex-primary-used-percent", "73"),
    ];
    replies.extend((0..MAX_HTTP_RETRIES).map(|_| {
        HttpReply::status(500, "application/json", r#"{"error":"still retrying"}"#)
            .with_header("retry-after", "0")
    }));
    let (base_url, _requests, server) = spawn_http_server(replies);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut error = match client
        .respond(vec![user_message("hello")], &completed_items)
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("exhausted HTTP retry sequence unexpectedly succeeded"),
    };

    assert!(error.is_retryable());
    let (usage, rate_limits) = error
        .take_completed_response()
        .expect("exhausted HTTP retry response metadata");
    assert!(usage.is_none());
    assert_eq!(rate_limits[0].primary.as_ref().unwrap().used_percent, 73.0);
    server.join().unwrap();
}

#[tokio::test]
async fn http_retry_preserves_rate_limit_headers_from_prior_attempts() {
    let item = assistant_item("recovered");
    let (base_url, _requests, server) = spawn_http_server(vec![
        HttpReply::status(500, "application/json", r#"{"error":"retry"}"#)
            .with_header("retry-after", "0")
            .with_header("x-codex-primary-used-percent", "73"),
        HttpReply::ok(
            "text/event-stream",
            completed_sse("resp_after_http_retry", &item),
        ),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let response = client
        .respond(vec![user_message("hello")], &completed_items)
        .await
        .unwrap();

    assert_eq!(response.final_answer.as_deref(), Some("recovered"));
    assert_eq!(
        response.rate_limits[0]
            .primary
            .as_ref()
            .unwrap()
            .used_percent,
        73.0
    );
    server.join().unwrap();
}

#[tokio::test]
async fn http_response_headers_respect_the_stream_idle_timeout() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4_096];
        let _ = stream.read(&mut request).await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let mut client = test_client(format!("http://{address}"));
    client.prefer_websocket = false;
    client.stream_idle_timeout = Duration::from_millis(50);
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    let error = match client
        .respond(vec![user_message("hello")], &completed_items)
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("inactive HTTP request unexpectedly succeeded"),
    };

    assert!(error.is_stream_idle());
    assert!(error.to_string().contains("before response headers"));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn model_rerouting_warns_once_per_turn_without_rejecting_payload_aliases() {
    const FALLBACK_MODEL: &str = "safety-fallback";
    let first_item = assistant_item("first");
    let mut first_completed = completed_event("resp_rerouted_event", &first_item);
    first_completed["response"]["model"] = Value::String("backend-payload-alias".to_string());
    let first_stream = format!(
        "data: {}\n\ndata: {}\n\ndata: {first_completed}\n\n",
        json!({
            "type": "response.created",
            "response": {
                "id": "resp_rerouted_event",
                "headers": {"OpenAI-Model": FALLBACK_MODEL},
            },
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": first_item,
        }),
    );
    let second_item = assistant_item("second");
    let (base_url, _requests, server) = spawn_http_server(vec![
        HttpReply::ok("text/event-stream", first_stream).with_header("openai-model", MODEL),
        HttpReply::ok(
            "text/event-stream",
            completed_sse("resp_rerouted_header", &second_item),
        )
        .with_header("openai-model", FALLBACK_MODEL),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    client.begin_turn();
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

    for (prompt, expected) in [("first", "first"), ("second", "second")] {
        let request = client.build_request(vec![user_message(prompt)], RequestKind::Turn);
        let response = client
            .respond_request_with_events(
                &request,
                &completed_items,
                Some(&events),
                RequestKind::Turn,
                RequestInputIdentity::Exact,
            )
            .await
            .unwrap();
        assert_eq!(response.final_answer.as_deref(), Some(expected));
    }

    let warnings = std::iter::from_fn(|| event_rx.try_recv().ok())
        .filter_map(|event| match event {
            AgentEvent::Warning(warning) => Some(warning),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(MODEL));
    assert!(warnings[0].contains(FALLBACK_MODEL));
    server.join().unwrap();
}

#[test]
fn completed_events_may_omit_reasoning_context() {
    let response = json!({
        "id": "resp_without_context",
        "model": MODEL,
        "output": [],
    });

    validate_completed_response(&response).unwrap();
}

#[tokio::test]
async fn http_transport_tracks_fast_service_tier_in_body_and_routing_hint() {
    let (base_url, requests, server) = spawn_http_server(vec![
        HttpReply::ok(
            "text/event-stream",
            completed_sse("resp_fast", &assistant_item("fast")),
        ),
        HttpReply::ok(
            "text/event-stream",
            completed_sse("resp_standard", &assistant_item("standard")),
        ),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();

    client.set_service_tier(ServiceTier::Fast);
    client
        .respond(vec![user_message("use Fast mode")], &completed_items)
        .await
        .unwrap();
    client.set_service_tier(ServiceTier::Standard);
    client
        .respond(vec![user_message("use standard mode")], &completed_items)
        .await
        .unwrap();

    let fast_request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let fast_body: Value = serde_json::from_slice(&fast_request.body).unwrap();
    assert_eq!(fast_body["service_tier"], "priority");
    assert!(fast_request.headers.contains(&format!(
        "x-codex-routing-hint: model={MODEL};tier=priority"
    )));

    let standard_request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let standard_body: Value = serde_json::from_slice(&standard_request.body).unwrap();
    assert!(standard_body.get("service_tier").is_none());
    assert!(
        standard_request
            .headers
            .contains(&format!("x-codex-routing-hint: model={MODEL}"))
    );
    assert!(!standard_request.headers.contains(";tier="));
    server.join().unwrap();
}

#[tokio::test]
async fn websocket_upgrade_error_preserves_retry_and_rate_limit_headers() {
    let (base_url, _requests, server) = spawn_http_server(vec![
        HttpReply::status(429, "text/plain", "slow down")
            .with_header("retry-after", "2")
            .with_header("x-codex-primary-used-percent", "88"),
    ]);
    let url = websocket_url(&base_url, "responses").unwrap();

    let mut error = match WebSocketConnection::connect(&url, &HeaderMap::new(), None).await {
        Err(error) => error,
        Ok(_) => panic!("rate-limited WebSocket upgrade unexpectedly succeeded"),
    };

    assert!(error.is_retryable());
    assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));
    let (usage, rate_limits) = error
        .take_completed_response()
        .expect("WebSocket upgrade response metadata");
    assert!(usage.is_none());
    assert_eq!(rate_limits[0].primary.as_ref().unwrap().used_percent, 88.0);
    server.join().unwrap();
}

#[tokio::test]
async fn websocket_upgrade_failure_falls_back_to_http() {
    let item = assistant_item("https fallback");
    let (base_url, requests, server) = spawn_http_server(vec![
        HttpReply::status(404, "text/plain", "no websocket here")
            .with_header("x-codex-primary-used-percent", "64"),
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
    assert_eq!(
        response.rate_limits[0]
            .primary
            .as_ref()
            .unwrap()
            .used_percent,
        64.0
    );
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
async fn websocket_tls_uses_the_configured_custom_root() {
    const HELPER_ENV: &str = "BETTERCODEX_WEBSOCKET_CUSTOM_CA_TEST_HELPER";
    const TEST_NAME: &str = "api::tests::websocket_tls_uses_the_configured_custom_root";

    if std::env::var_os(HELPER_ENV).is_none() {
        let ca_path = std::env::temp_dir().join(format!(
            "bettercodex-websocket-ca-{}-{}.pem",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&ca_path, TEST_WEBSOCKET_CA_CERTIFICATE).unwrap();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
            .env(HELPER_ENV, "1")
            .env_remove("CODEX_CA_CERTIFICATE")
            .env("SSL_CERT_FILE", &ca_path)
            .env_remove("SSL_CERT_DIR");
        let output = tokio::time::timeout(Duration::from_secs(30), command.output()).await;
        let cleanup = std::fs::remove_file(&ca_path);
        let output = output
            .expect("nested custom-CA WebSocket test timed out")
            .unwrap();
        cleanup.unwrap();
        assert!(
            output.status.success(),
            "nested custom-CA WebSocket test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    crate::http_client::ensure_rustls_crypto_provider();
    let server_certificate =
        CertificateDer::from_pem_slice(TEST_WEBSOCKET_CERTIFICATE.as_bytes()).unwrap();
    let private_key = PrivateKeyDer::from_pem_slice(TEST_WEBSOCKET_PRIVATE_KEY.as_bytes()).unwrap();
    let server_config = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![server_certificate], private_key)
            .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let response_item = assistant_item("custom root");
    let server_item = response_item.clone();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let connection = rustls::ServerConnection::new(Arc::clone(&server_config)).unwrap();
        let tls = rustls::StreamOwned::new(connection, stream);
        let mut websocket =
            tungstenite::accept_with_config(tls, Some(super::websocket::websocket_config()))
                .unwrap();

        let tungstenite::Message::Text(warmup) = websocket.read().unwrap() else {
            panic!("expected a text WSS warmup request");
        };
        let warmup: Value = serde_json::from_str(warmup.as_str()).unwrap();
        assert_eq!(warmup["generate"], false);
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_tls_warm"},
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        let mut warmup_completed = completed_event("resp_tls_warm", &Value::Null);
        warmup_completed["sequence_number"] = json!(2);
        warmup_completed["response"]["output"] = json!([]);
        websocket
            .send(tungstenite::Message::Text(
                warmup_completed.to_string().into(),
            ))
            .unwrap();

        let tungstenite::Message::Text(request) = websocket.read().unwrap() else {
            panic!("expected a text WSS inference request");
        };
        let request: Value = serde_json::from_str(request.as_str()).unwrap();
        assert_eq!(request["previous_response_id"], "resp_tls_warm");
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_custom_root"},
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "sequence_number": 2,
                    "item": server_item,
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        let mut completed = completed_event("resp_custom_root", &server_item);
        completed["sequence_number"] = json!(3);
        websocket
            .send(tungstenite::Message::Text(completed.to_string().into()))
            .unwrap();
    });

    let mut client = test_client(format!("https://localhost:{}", address.port()));
    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let response = client
        .respond(vec![user_message("secure websocket")], &completed_items)
        .await
        .unwrap();
    assert_eq!(response.final_answer.as_deref(), Some("custom root"));
    assert_eq!(completed_rx.recv().await.unwrap(), response_item);
    server.join().unwrap();
}

#[tokio::test]
async fn websocket_answers_pings_while_idle_between_requests() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (idle_tx, idle_rx) = tokio::sync::oneshot::channel();
    let (pong_tx, pong_rx) = tokio::sync::oneshot::channel();
    let response_item = assistant_item("after idle ping");
    let server_item = response_item.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = tokio_tungstenite::accept_async_with_config(
            stream,
            Some(super::websocket::websocket_config()),
        )
        .await
        .unwrap();

        let tungstenite::Message::Text(warmup) = websocket.next().await.unwrap().unwrap() else {
            panic!("expected a text WebSocket warmup request");
        };
        let warmup: Value = serde_json::from_str(warmup.as_str()).unwrap();
        assert_eq!(warmup["generate"], false);
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_idle_warm"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut warmup_completed = completed_event("resp_idle_warm", &Value::Null);
        warmup_completed["sequence_number"] = json!(2);
        warmup_completed["response"]["output"] = json!([]);
        websocket
            .send(tungstenite::Message::Text(
                warmup_completed.to_string().into(),
            ))
            .await
            .unwrap();

        idle_rx.await.unwrap();
        websocket
            .send(tungstenite::Message::Ping(
                b"idle keepalive".to_vec().into(),
            ))
            .await
            .unwrap();
        let pong = tokio::time::timeout(Duration::from_secs(1), websocket.next())
            .await
            .expect("idle WebSocket ping was not answered")
            .expect("WebSocket closed before its idle pong")
            .expect("failed to read idle WebSocket pong");
        assert_eq!(
            pong,
            tungstenite::Message::Pong(b"idle keepalive".to_vec().into())
        );
        pong_tx.send(()).unwrap();

        let tungstenite::Message::Text(request) = websocket.next().await.unwrap().unwrap() else {
            panic!("expected a text WebSocket inference request");
        };
        let request: Value = serde_json::from_str(request.as_str()).unwrap();
        assert_eq!(request["previous_response_id"], "resp_idle_warm");
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_after_idle_ping"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "sequence_number": 2,
                    "item": server_item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut completed = completed_event("resp_after_idle_ping", &server_item);
        completed["sequence_number"] = json!(3);
        websocket
            .send(tungstenite::Message::Text(completed.to_string().into()))
            .await
            .unwrap();
    });

    let mut client = test_client(format!("http://{address}"));
    assert!(matches!(
        client.attempt_websocket_prewarm(None).await.unwrap(),
        WebSocketPrewarmOutcome::Ready { .. }
    ));
    idle_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), pong_rx)
        .await
        .expect("idle WebSocket pong confirmation timed out")
        .unwrap();

    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let response = client
        .respond(vec![user_message("continue")], &completed_items)
        .await
        .unwrap();
    assert_eq!(response.final_answer.as_deref(), Some("after idle ping"));
    assert_eq!(completed_rx.recv().await.unwrap(), response_item);
    server.await.unwrap();
}

#[tokio::test]
async fn websocket_reconnects_after_an_idle_server_close() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let response_item = assistant_item("after reconnect");
    let server_item = response_item.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = tokio_tungstenite::accept_async_with_config(
            stream,
            Some(super::websocket::websocket_config()),
        )
        .await
        .unwrap();

        let tungstenite::Message::Text(warmup) = websocket.next().await.unwrap().unwrap() else {
            panic!("expected a text WebSocket warmup request");
        };
        let warmup: Value = serde_json::from_str(warmup.as_str()).unwrap();
        assert_eq!(warmup["generate"], false);
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_closed_warm"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut completed = completed_event("resp_closed_warm", &Value::Null);
        completed["sequence_number"] = json!(2);
        completed["response"]["output"] = json!([]);
        websocket
            .send(tungstenite::Message::Text(completed.to_string().into()))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Close(None))
            .await
            .unwrap();
        drop(websocket);

        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = tokio_tungstenite::accept_async_with_config(
            stream,
            Some(super::websocket::websocket_config()),
        )
        .await
        .unwrap();
        let tungstenite::Message::Text(request) = websocket.next().await.unwrap().unwrap() else {
            panic!("expected a text WebSocket inference request after reconnect");
        };
        let request: Value = serde_json::from_str(request.as_str()).unwrap();
        assert!(request["previous_response_id"].is_null());
        assert_eq!(request["input"].as_array().unwrap().len(), 1);
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_after_reconnect"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "sequence_number": 2,
                    "item": server_item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut completed = completed_event("resp_after_reconnect", &server_item);
        completed["sequence_number"] = json!(3);
        websocket
            .send(tungstenite::Message::Text(completed.to_string().into()))
            .await
            .unwrap();
    });

    let mut client = test_client(format!("http://{address}"));
    assert!(matches!(
        client.attempt_websocket_prewarm(None).await.unwrap(),
        WebSocketPrewarmOutcome::Ready { .. }
    ));
    assert!(client.websocket_baseline.is_some());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if client
                .websocket
                .as_ref()
                .is_none_or(super::websocket::WebSocketConnection::is_closed)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("idle WebSocket close was not observed");

    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let response = client
        .respond(
            vec![user_message("continue on a new socket")],
            &completed_items,
        )
        .await
        .unwrap();
    assert_eq!(response.final_answer.as_deref(), Some("after reconnect"));
    assert_eq!(completed_rx.recv().await.unwrap(), response_item);
    server.await.unwrap();
}

#[tokio::test]
async fn websocket_ignores_trailing_terminal_frames_after_the_next_request() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let first_item = assistant_item("first");
    let second_item = assistant_item("second");
    let server_first_item = first_item.clone();
    let server_second_item = second_item.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut extensions = tungstenite::extensions::ExtensionsConfig::default();
        extensions.permessage_deflate =
            Some(tungstenite::extensions::compression::deflate::DeflateConfig::default());
        let mut config = tungstenite::protocol::WebSocketConfig::default();
        config.extensions = extensions;
        let mut websocket = tokio_tungstenite::accept_async_with_config(stream, Some(config))
            .await
            .unwrap();

        let tungstenite::Message::Text(warmup) = websocket.next().await.unwrap().unwrap() else {
            panic!("expected a text WebSocket warmup request");
        };
        let warmup: Value = serde_json::from_str(warmup.as_str()).unwrap();
        assert_eq!(warmup["generate"], false);
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_warm"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut warmup_completed = completed_event("resp_warm", &Value::Null);
        warmup_completed["sequence_number"] = json!(2);
        warmup_completed["response"]["output"] = json!([]);
        websocket
            .send(tungstenite::Message::Text(
                warmup_completed.to_string().into(),
            ))
            .await
            .unwrap();

        let tungstenite::Message::Text(first_request) = websocket.next().await.unwrap().unwrap()
        else {
            panic!("expected a text first-turn WebSocket request");
        };
        let first_request: Value = serde_json::from_str(first_request.as_str()).unwrap();
        assert_eq!(first_request["previous_response_id"], "resp_warm");
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_first"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "sequence_number": 2,
                    "item": server_first_item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut first_completed = completed_event("resp_first", &server_first_item);
        first_completed["sequence_number"] = json!(3);
        websocket
            .send(tungstenite::Message::Text(
                first_completed.to_string().into(),
            ))
            .await
            .unwrap();

        let tungstenite::Message::Text(second_request) = websocket.next().await.unwrap().unwrap()
        else {
            panic!("expected a text second-turn WebSocket request");
        };
        let second_request: Value = serde_json::from_str(second_request.as_str()).unwrap();
        assert_eq!(second_request["previous_response_id"], "resp_first");
        assert_eq!(second_request["input"], json!([user_message("second")]));

        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_second"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        // Deliver item and terminal duplicates from the prior generation only after the next
        // request and its response.created event are on the wire. Neither a pre-send drain nor an
        // event-start boundary alone can protect this case.
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "sequence_number": 4,
                    "item": server_first_item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "item_id": "msg_first",
                    "output_index": 0,
                    "content_index": 0,
                    "sequence_number": 5,
                    "delta": "first",
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "sequence_number": 6,
                    "item": server_first_item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut stale_completed = completed_event("resp_first", &server_first_item);
        stale_completed["sequence_number"] = json!(7);
        websocket
            .send(tungstenite::Message::Text(
                stale_completed.to_string().into(),
            ))
            .await
            .unwrap();

        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "sequence_number": 2,
                    "item": server_second_item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut second_completed = completed_event("resp_second", &server_second_item);
        second_completed["sequence_number"] = json!(3);
        websocket
            .send(tungstenite::Message::Text(
                second_completed.to_string().into(),
            ))
            .await
            .unwrap();
    });

    let mut client = test_client(format!("http://{address}"));
    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let first_user = user_message("first");
    let first_response = client
        .respond(vec![first_user.clone()], &completed_items)
        .await
        .unwrap();
    assert_eq!(first_response.final_answer.as_deref(), Some("first"));
    assert_eq!(completed_rx.recv().await.unwrap(), first_item);

    let second_response = client
        .respond(
            vec![first_user, first_item, user_message("second")],
            &completed_items,
        )
        .await
        .unwrap();
    assert_eq!(second_response.final_answer.as_deref(), Some("second"));
    assert_eq!(completed_rx.recv().await.unwrap(), second_item);
    assert!(completed_rx.try_recv().is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn websocket_compaction_keeps_the_prior_response_boundary() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let first_user = user_message("before compaction");
    let first_item = assistant_item("before compaction");
    let compaction_item = json!({
        "type": "compaction",
        "id": "cmp_boundary",
        "encrypted_content": "opaque boundary summary",
    });
    let next_user = user_message("after compaction");
    let next_item = assistant_item("after compaction");
    let server_first_item = first_item.clone();
    let server_compaction_item = compaction_item.clone();
    let server_next_item = next_item.clone();
    let server_next_user = next_user.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut extensions = tungstenite::extensions::ExtensionsConfig::default();
        extensions.permessage_deflate =
            Some(tungstenite::extensions::compression::deflate::DeflateConfig::default());
        let mut config = tungstenite::protocol::WebSocketConfig::default();
        config.extensions = extensions;
        let mut websocket = tokio_tungstenite::accept_async_with_config(stream, Some(config))
            .await
            .unwrap();

        let tungstenite::Message::Text(warmup) = websocket.next().await.unwrap().unwrap() else {
            panic!("expected a text WebSocket warmup request");
        };
        let warmup: Value = serde_json::from_str(warmup.as_str()).unwrap();
        assert_eq!(warmup["generate"], false);
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_warm_compaction_boundary"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut warmup_completed = completed_event("resp_warm_compaction_boundary", &Value::Null);
        warmup_completed["sequence_number"] = json!(2);
        warmup_completed["response"]["output"] = json!([]);
        websocket
            .send(tungstenite::Message::Text(
                warmup_completed.to_string().into(),
            ))
            .await
            .unwrap();

        let tungstenite::Message::Text(first_request) = websocket.next().await.unwrap().unwrap()
        else {
            panic!("expected a text first-turn WebSocket request");
        };
        let first_request: Value = serde_json::from_str(first_request.as_str()).unwrap();
        assert_eq!(
            first_request["previous_response_id"],
            "resp_warm_compaction_boundary"
        );
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_before_compaction"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "sequence_number": 2,
                    "item": server_first_item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut first_completed = completed_event("resp_before_compaction", &server_first_item);
        first_completed["sequence_number"] = json!(3);
        websocket
            .send(tungstenite::Message::Text(
                first_completed.to_string().into(),
            ))
            .await
            .unwrap();

        let tungstenite::Message::Text(compaction_request) =
            websocket.next().await.unwrap().unwrap()
        else {
            panic!("expected a text compaction WebSocket request");
        };
        let compaction_request: Value = serde_json::from_str(compaction_request.as_str()).unwrap();
        assert_eq!(
            compaction_request["previous_response_id"],
            "resp_before_compaction"
        );
        assert_eq!(
            compaction_request["input"]
                .as_array()
                .and_then(|items| items.last())
                .and_then(|item| item["type"].as_str()),
            Some("compaction_trigger")
        );
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_compaction_boundary"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "sequence_number": 2,
                    "item": server_compaction_item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut compaction_completed =
            completed_event("resp_compaction_boundary", &server_compaction_item);
        compaction_completed["sequence_number"] = json!(3);
        websocket
            .send(tungstenite::Message::Text(
                compaction_completed.to_string().into(),
            ))
            .await
            .unwrap();

        let tungstenite::Message::Text(next_request) = websocket.next().await.unwrap().unwrap()
        else {
            panic!("expected a text post-compaction WebSocket request");
        };
        let next_request: Value = serde_json::from_str(next_request.as_str()).unwrap();
        assert!(next_request.get("previous_response_id").is_none());
        assert!(
            next_request["input"]
                .as_array()
                .is_some_and(|items| items.contains(&server_next_user))
        );
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "resp_after_compaction"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        // A full request after compaction has no incremental baseline, but it still shares the
        // socket with the completed compaction generation. Its delayed terminal frame must not
        // complete or corrupt the new response.
        let mut stale_compaction_completed =
            completed_event("resp_compaction_boundary", &server_compaction_item);
        stale_compaction_completed["sequence_number"] = json!(4);
        websocket
            .send(tungstenite::Message::Text(
                stale_compaction_completed.to_string().into(),
            ))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "sequence_number": 2,
                    "item": server_next_item,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut next_completed = completed_event("resp_after_compaction", &server_next_item);
        next_completed["sequence_number"] = json!(3);
        websocket
            .send(tungstenite::Message::Text(
                next_completed.to_string().into(),
            ))
            .await
            .unwrap();
    });

    let mut client = test_client(format!("http://{address}"));
    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let first_response = client
        .respond(vec![first_user.clone()], &completed_items)
        .await
        .unwrap();
    assert_eq!(
        first_response.final_answer.as_deref(),
        Some("before compaction")
    );
    assert_eq!(completed_rx.recv().await.unwrap(), first_item.clone());

    let result = client
        .compact(
            &[first_user, first_item],
            CompactionRequest::Automatic(CompactionPhase::MidTurn),
        )
        .await
        .unwrap();
    assert!(result.items.contains(&compaction_item));
    client.commit_compaction();

    let mut next_history = result.items;
    next_history.push(next_user);
    let next_response = client
        .respond(next_history, &completed_items)
        .await
        .unwrap();
    assert_eq!(
        next_response.final_answer.as_deref(),
        Some("after compaction")
    );
    assert_eq!(completed_rx.recv().await.unwrap(), next_item);
    assert!(completed_rx.try_recv().is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn inactive_startup_websocket_falls_back_to_http_promptly() {
    let (base_url, mut websocket_requests, mut http_requests, server) =
        spawn_inactive_startup_websocket_server(InactiveWebSocketResponse::BeforeOutput).await;
    let mut client = test_client(base_url);
    let prewarmed = client
        .startup_prewarm_client()
        .prewarm_for_startup()
        .await
        .unwrap();
    client.adopt_startup_prewarm(prewarmed);
    let (completed_items, _completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let warmup = websocket_requests.recv().await.unwrap();
    assert_eq!(warmup["generate"], false);
    let mut response_task = tokio::spawn(async move {
        let response = client
            .respond(vec![user_message("hello")], &completed_items)
            .await;
        (client, response)
    });
    let first_turn = tokio::time::timeout(Duration::from_secs(2), websocket_requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_turn["previous_response_id"], "warm-inactive");

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::time::resume();
    let (client, response) =
        match tokio::time::timeout(Duration::from_secs(2), &mut response_task).await {
            Ok(joined) => joined.unwrap(),
            Err(_) => {
                response_task.abort();
                server.abort();
                let _ = response_task.await;
                let _ = server.await;
                panic!("startup WebSocket recovery exceeded its bounded idle timeout");
            }
        };
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            server.abort();
            let _ = server.await;
            panic!("startup WebSocket recovery failed: {error}");
        }
    };

    assert_eq!(response.final_answer.as_deref(), Some("https recovery"));
    assert!(!client.prefer_websocket);
    let fallback = http_requests.recv().await.unwrap();
    assert_eq!(fallback.path, "/responses");
    let fallback: Value = serde_json::from_slice(&fallback.body).unwrap();
    assert!(fallback["input"].to_string().contains("hello"));
    server.await.unwrap();
}

#[tokio::test]
async fn partial_websocket_output_is_not_transparently_replayed_over_http() {
    let partial = assistant_item_with_phase("partial", "commentary");
    let (base_url, mut websocket_requests, mut http_requests, server) =
        spawn_inactive_startup_websocket_server(InactiveWebSocketResponse::AfterOutput(
            partial.clone(),
        ))
        .await;
    let mut client = test_client(base_url);
    client.stream_idle_timeout = Duration::from_secs(10);
    let prewarmed = client
        .startup_prewarm_client()
        .prewarm_for_startup()
        .await
        .unwrap();
    client.adopt_startup_prewarm(prewarmed);
    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let warmup = websocket_requests.recv().await.unwrap();
    assert_eq!(warmup["generate"], false);
    let mut response_task = tokio::spawn(async move {
        let response = client
            .respond(vec![user_message("hello")], &completed_items)
            .await;
        (client, response)
    });
    let first_turn = tokio::time::timeout(Duration::from_secs(2), websocket_requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_turn["previous_response_id"], "warm-inactive");
    let completed = tokio::time::timeout(Duration::from_secs(2), completed_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed, partial);

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::time::resume();
    let (client, response) =
        match tokio::time::timeout(Duration::from_secs(2), &mut response_task).await {
            Ok(joined) => joined.unwrap(),
            Err(_) => {
                response_task.abort();
                server.abort();
                let _ = response_task.await;
                let _ = server.await;
                panic!("partial WebSocket inactivity did not return to the agent");
            }
        };

    let error = match response {
        Err(error) => error,
        Ok(_) => panic!("partial WebSocket response was transparently replayed"),
    };
    assert!(error.is_retryable());
    assert!(client.prefer_websocket);
    assert!(completed_rx.try_recv().is_err());
    assert!(http_requests.try_recv().is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn partial_websocket_text_is_not_transparently_replayed_over_http() {
    let (base_url, mut websocket_requests, mut http_requests, server) =
        spawn_inactive_startup_websocket_server(InactiveWebSocketResponse::AfterDelta).await;
    let mut client = test_client(base_url);
    client.stream_idle_timeout = Duration::from_secs(10);
    let prewarmed = client
        .startup_prewarm_client()
        .prewarm_for_startup()
        .await
        .unwrap();
    client.adopt_startup_prewarm(prewarmed);
    let (completed_items, mut completed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let warmup = websocket_requests.recv().await.unwrap();
    assert_eq!(warmup["generate"], false);
    let mut response_task = tokio::spawn(async move {
        let request = client.build_request(vec![user_message("hello")], RequestKind::Turn);
        let response = client
            .respond_request_with_events(
                &request,
                &completed_items,
                Some(&events),
                RequestKind::Turn,
                RequestInputIdentity::Exact,
            )
            .await;
        (client, response)
    });
    let first_turn = tokio::time::timeout(Duration::from_secs(2), websocket_requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_turn["previous_response_id"], "warm-inactive");
    let delta = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let AgentEvent::ModelMessageDelta(delta) = delta else {
        panic!("expected a model text delta");
    };
    assert_eq!(delta.text, "partial");

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::time::resume();
    let (client, response) =
        match tokio::time::timeout(Duration::from_secs(2), &mut response_task).await {
            Ok(joined) => joined.unwrap(),
            Err(_) => {
                response_task.abort();
                server.abort();
                let _ = response_task.await;
                let _ = server.await;
                panic!("partial WebSocket text inactivity did not return to the agent");
            }
        };

    let mut error = match response {
        Err(error) => error,
        Ok(_) => panic!("partial WebSocket text was transparently replayed"),
    };
    assert!(error.is_retryable());
    assert!(client.prefer_websocket);
    assert!(completed_rx.try_recv().is_err());
    assert!(http_requests.try_recv().is_err());
    let AgentEvent::Warning(warning) = event_rx.try_recv().unwrap() else {
        panic!("expected a partial-output warning");
    };
    assert!(warning.contains("partial output"));
    assert!(event_rx.try_recv().is_err());
    let (usage, rate_limits) = error
        .take_completed_response()
        .expect("partial transport response metadata");
    assert!(usage.is_none());
    assert_eq!(rate_limits[0].primary.as_ref().unwrap().used_percent, 44.0);
    server.await.unwrap();
}

#[tokio::test]
async fn compaction_sends_the_trigger_and_accepts_one_opaque_output_amid_noise() {
    let retained = user_message("retain this operator message");
    let retained_commentary =
        assistant_item_with_phase("retain this active-turn commentary", "commentary");
    let discarded = assistant_item("discard this completed answer");
    let noise = assistant_item("ignore this compaction-stream output");
    let legacy_opaque = json!({
        "type": "compaction_summary",
        "id": "cmp_1",
        "encrypted_content": "opaque legacy summary",
        "status": "completed",
        "internal_chat_message_metadata_passthrough": {
            "turn_id": "turn-compact",
            "create_time": 1.25,
            "executed_tool_calls": [{"name": "ignored"}],
            "unknown": true,
        },
    });
    let (base_url, requests, server) = spawn_http_server(vec![HttpReply::ok(
        "text/event-stream",
        completed_sse_with_items("resp_compacted", &[noise, legacy_opaque.clone()]),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;
    client.set_service_tier(ServiceTier::Fast);

    let result = client
        .compact(
            &[
                retained.clone(),
                retained_commentary.clone(),
                discarded.clone(),
            ],
            CompactionRequest::Automatic(CompactionPhase::MidTurn),
        )
        .await
        .unwrap();

    assert_eq!(result.items, [retained.clone(), legacy_opaque]);
    assert_eq!(
        client.window, 0,
        "cache lineage advances only after the replacement is persisted"
    );
    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(request.path, "/responses");
    assert!(
        request
            .headers
            .to_ascii_lowercase()
            .contains("x-codex-beta-features: remote_compaction_v2")
    );
    let request: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(request["service_tier"], "priority");
    let input = request["input"].as_array().unwrap();
    assert!(input.contains(&retained));
    assert!(input.contains(&retained_commentary));
    assert!(input.contains(&discarded));
    assert_eq!(
        input.last().and_then(|item| item["type"].as_str()),
        Some("compaction_trigger")
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
            "trigger": "auto",
            "reason": "context_limit",
            "implementation": "responses_compaction_v2",
            "phase": "mid_turn",
            "strategy": "memento",
        })
    );
    server.join().unwrap();
}

#[tokio::test]
async fn compaction_rewrites_only_the_trailing_output_group() {
    let oversized = "oversized tool output\n".repeat(60_000);
    let interruption = user_message("<turn_aborted>interrupted by operator</turn_aborted>");
    let trailing_interruption =
        user_message("<response_interrupted>stream ended after tool output</response_interrupted>");
    let history = vec![
        user_message("inspect large tool results"),
        json!({
            "type": "function_call",
            "call_id": "call_older",
            "name": "read",
            "arguments": r#"{"path":"older.txt"}"#,
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call_older",
            "output": oversized.clone(),
        }),
        interruption.clone(),
        json!({
            "type": "function_call",
            "call_id": "call_trailing_function",
            "name": "read",
            "arguments": r#"{"path":"current.txt"}"#,
        }),
        json!({
            "type": "custom_tool_call",
            "call_id": "call_trailing_custom",
            "name": "exec",
            "input": "text('current legacy result')",
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call_trailing_function",
            "output": oversized.clone(),
        }),
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_trailing_custom",
            "output": oversized.clone(),
        }),
        trailing_interruption.clone(),
    ];
    let opaque = json!({
        "type": "compaction",
        "id": "cmp_oversized",
        "encrypted_content": "opaque summary",
    });
    let (base_url, requests, server) = spawn_http_server(vec![HttpReply::ok(
        "text/event-stream",
        completed_sse("resp_compacted_oversized", &opaque),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;

    client
        .compact(
            &history,
            CompactionRequest::Automatic(CompactionPhase::PreTurn),
        )
        .await
        .unwrap();

    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let request: Value = serde_json::from_slice(&request.body).unwrap();
    let input = request["input"].as_array().unwrap();
    assert!(input.contains(&interruption));
    assert!(input.contains(&trailing_interruption));
    assert_eq!(
        input
            .iter()
            .find(|item| item["call_id"] == "call_older" && item["type"] == "function_call_output")
            .and_then(|item| item["output"].as_str()),
        Some(oversized.as_str())
    );
    for call_id in ["call_trailing_function", "call_trailing_custom"] {
        assert_eq!(
            input
                .iter()
                .find(|item| item["call_id"] == call_id
                    && item["type"]
                        .as_str()
                        .is_some_and(|kind| kind.ends_with("_call_output")))
                .and_then(|item| item["output"].as_str()),
            Some("Output exceeded the available model context and was truncated"),
            "{call_id} was not rewritten"
        );
    }
    assert_eq!(input.last().unwrap()["type"], "compaction_trigger");
    server.join().unwrap();
}

#[tokio::test]
async fn compaction_budget_counts_the_full_model_visible_request() {
    let selection = ModelSelection::default();
    let effective_window = selection.effective_context_window();
    let [tool_tokens, instruction_tokens] = estimated_harness_tokens();
    let trigger_tokens = estimated_tokens(std::slice::from_ref(&compaction::compaction_trigger()));
    let full_request_history_limit = effective_window.saturating_sub(
        tool_tokens
            .saturating_add(instruction_tokens)
            .saturating_add(trigger_tokens),
    );
    let base_only_history_limit = effective_window.saturating_sub(instruction_tokens);
    assert!(full_request_history_limit < base_only_history_limit);

    let mut history = vec![
        user_message("fit this trailing output"),
        json!({
            "type": "function_call",
            "call_id": "call_fits_upstream",
            "name": "read",
            "arguments": r#"{"path":"large.txt"}"#,
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call_fits_upstream",
            "output": "",
        }),
    ];
    let target_tokens = full_request_history_limit
        .saturating_add(base_only_history_limit)
        .div_ceil(2);
    let base_tokens = estimated_tokens(&history);
    let output = "x".repeat(
        usize::try_from(target_tokens.saturating_sub(base_tokens).saturating_mul(4)).unwrap(),
    );
    history[2]["output"] = Value::String(output.clone());
    let history_tokens = estimated_tokens(&history);
    assert!(
        history_tokens > full_request_history_limit && history_tokens <= base_only_history_limit,
        "fixture estimate {history_tokens} was outside ({full_request_history_limit}, {base_only_history_limit}]"
    );

    let opaque = json!({
        "type": "compaction",
        "id": "cmp_fits_upstream",
        "encrypted_content": "opaque summary",
    });
    let (base_url, requests, server) = spawn_http_server(vec![HttpReply::ok(
        "text/event-stream",
        completed_sse("resp_compacted_fitting", &opaque),
    )]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;

    client
        .compact(
            &history,
            CompactionRequest::Automatic(CompactionPhase::PreTurn),
        )
        .await
        .unwrap();

    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let request: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        request["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "call_fits_upstream"
            })
            .and_then(|item| item["output"].as_str()),
        Some("Output exceeded the available model context and was truncated")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn compaction_rejects_a_response_without_one_opaque_item() {
    let ordinary = assistant_item("not compacted");
    let streamed_rate_limit = json!({
        "type": "codex.rate_limits",
        "rate_limits": {
            "primary": {
                "used_percent": 17.0,
                "window_minutes": 300,
                "reset_at": 1_704_069_000_i64,
            },
        },
    });
    let stream = format!(
        "data: {streamed_rate_limit}\n\n{}",
        completed_sse("resp_not_compacted", &ordinary)
    );
    let (base_url, _requests, server) = spawn_http_server(vec![
        HttpReply::ok("text/event-stream", stream)
            .with_header("x-codex-primary-used-percent", "23"),
    ]);
    let mut client = test_client(base_url);
    client.prefer_websocket = false;

    let mut error = client
        .compact(
            &[user_message("old")],
            CompactionRequest::Automatic(CompactionPhase::MidTurn),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("expected exactly one"));
    let (usage, rate_limits) = error
        .take_completed_response()
        .expect("completed response metadata");
    assert_eq!(usage.unwrap().input_tokens, 42);
    assert_eq!(rate_limits[0].primary.as_ref().unwrap().used_percent, 17.0);
    assert_eq!(client.window, 0);
    server.join().unwrap();
}

struct HttpReply {
    status: u16,
    content_type: &'static str,
    body: String,
    headers: Vec<(&'static str, &'static str)>,
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
        }
    }

    fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
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
            write!(
                stream,
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
                reply.status,
                reason,
                reply.content_type,
                reply.body.len(),
                extra_headers,
            )
            .unwrap();
            stream.write_all(reply.body.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
    });
    (format!("http://{address}"), requests_rx, server)
}

enum InactiveWebSocketResponse {
    BeforeOutput,
    AfterDelta,
    AfterOutput(Value),
}

async fn spawn_inactive_startup_websocket_server(
    response: InactiveWebSocketResponse,
) -> (
    String,
    tokio::sync::mpsc::UnboundedReceiver<Value>,
    tokio::sync::mpsc::UnboundedReceiver<CapturedRequest>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (websocket_requests_tx, websocket_requests_rx) = tokio::sync::mpsc::unbounded_channel();
    let (http_requests_tx, http_requests_rx) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut extensions = tungstenite::extensions::ExtensionsConfig::default();
        extensions.permessage_deflate =
            Some(tungstenite::extensions::compression::deflate::DeflateConfig::default());
        let mut config = tungstenite::protocol::WebSocketConfig::default();
        config.extensions = extensions;
        let mut websocket = tokio_tungstenite::accept_async_with_config(stream, Some(config))
            .await
            .unwrap();

        let tungstenite::Message::Text(warmup) = websocket.next().await.unwrap().unwrap() else {
            panic!("expected a text WebSocket warmup request");
        };
        websocket_requests_tx
            .send(serde_json::from_str(warmup.as_str()).unwrap())
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": {"id": "warm-inactive"},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(tungstenite::Message::Text(
                json!({
                    "type": "response.completed",
                    "sequence_number": 2,
                    "response": {
                        "id": "warm-inactive",
                        "model": MODEL,
                        "output": [],
                        "reasoning": {"context": "all_turns"},
                    },
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        let tungstenite::Message::Text(first_turn) = websocket.next().await.unwrap().unwrap()
        else {
            panic!("expected a text first-turn WebSocket request");
        };
        websocket_requests_tx
            .send(serde_json::from_str(first_turn.as_str()).unwrap())
            .unwrap();

        match response {
            InactiveWebSocketResponse::BeforeOutput => {
                // Keep the accepted WebSocket open but never answer the first turn. This models a
                // half-open warmed connection whose write succeeds while no server events arrive.
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_async_request(&mut stream).await;
                http_requests_tx.send(request).unwrap();
                let body = completed_sse("resp-https-recovery", &assistant_item("https recovery"));
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(body.as_bytes()).await.unwrap();
                stream.flush().await.unwrap();
            }
            InactiveWebSocketResponse::AfterDelta => {
                websocket
                    .send(tungstenite::Message::Text(
                        json!({
                            "type": "response.created",
                            "sequence_number": 1,
                            "response": {"id": "resp-inactive"},
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                websocket
                    .send(tungstenite::Message::Text(
                        json!({
                            "type": "codex.rate_limits",
                            "rate_limits": {
                                "primary": {
                                    "used_percent": 44.0,
                                    "window_minutes": 300,
                                    "reset_at": 1_704_069_000_i64,
                                },
                            },
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                websocket
                    .send(tungstenite::Message::Text(
                        json!({
                            "type": "response.output_text.delta",
                            "item_id": "msg_partial_delta",
                            "output_index": 0,
                            "content_index": 0,
                            "sequence_number": 2,
                            "delta": "partial",
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                let _ = websocket.next().await;
            }
            InactiveWebSocketResponse::AfterOutput(item) => {
                websocket
                    .send(tungstenite::Message::Text(
                        json!({
                            "type": "response.created",
                            "sequence_number": 1,
                            "response": {"id": "resp-inactive"},
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                websocket
                    .send(tungstenite::Message::Text(
                        json!({
                            "type": "response.output_item.done",
                            "output_index": 0,
                            "sequence_number": 2,
                            "item": item,
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                // The client should return the interrupted stream instead of opening an HTTP
                // connection, then drop this socket while abandoning the incomplete response.
                let _ = websocket.next().await;
            }
        }
    });
    (
        format!("http://{address}"),
        websocket_requests_rx,
        http_requests_rx,
        server,
    )
}

async fn read_async_request(stream: &mut tokio::net::TcpStream) -> CapturedRequest {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let header_end = loop {
        let read = stream.read(&mut buffer).await.unwrap();
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
        let read = stream.read(&mut buffer).await.unwrap();
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

const TEST_WEBSOCKET_CA_CERTIFICATE: &str = r#"-----BEGIN CERTIFICATE-----
MIIDQzCCAiugAwIBAgIUX0gCod1/KrjPaMGmvQVTZ0Vh+zYwDQYJKoZIhvcNAQEL
BQAwKDEmMCQGA1UEAwwdYmV0dGVyY29kZXggdGVzdCBXZWJTb2NrZXQgQ0EwIBcN
MjYwODE3MTczODU3WhgPMjEyNjA3MjQxNzM4NTdaMCgxJjAkBgNVBAMMHWJldHRl
cmNvZGV4IHRlc3QgV2ViU29ja2V0IENBMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A
MIIBCgKCAQEA2RwX1XYshDz9a6kn1fzuve6a7KjFDwqHe5s+L1CDi/wn8D0zk/w8
HYytmon253zzEvAzPwW3AubgNOHqAr65ePV1BHHM3ojpKqVydhnKx2VIGU3uRi64
BowEzgjt69FMfUclBVhNdytVZULNCAsBTcNCCmo7XV8n5MJC+8js2szS77r0b98Q
99RW4bHuR+Rn1L3VtKYgTaicjEi9aEoRYzPvoeL+Cr1xWYhtRt3IETexnZPtMEpN
hrujvR6xUgFBbT5/LiJ3VJnd9yCKEhoV4D+aETcEmIzqQQlXCqyPT+I1AVpMKKBB
+M2YwJXcHULbrBnX9zfYhwHedX0IUFkjKQIDAQABo2MwYTAdBgNVHQ4EFgQU9A//
NuAxwms/LxCCSHsX0Bdp4h0wHwYDVR0jBBgwFoAU9A//NuAxwms/LxCCSHsX0Bdp
4h0wDwYDVR0TAQH/BAUwAwEB/zAOBgNVHQ8BAf8EBAMCAQYwDQYJKoZIhvcNAQEL
BQADggEBADM9dMllBGzNuBwM22qObTCtZX/nVHcjt9ewurlFXqG4vu8dhk4o2Vh7
exrWETGgWqybnO3xRyT9UPsoIv4lH1pkIBKRXElhBOmzbRmXCGUBH88tDN+BzqC/
lBcGymMJUMmI40vaKfdwVyNb5+jegDns459wzqe8LB9p2r5U3OZbu5QzWLGSTBaR
yNC9E2sgODrn5G9kIn74U9z0pso5wa1wK4M3HCk8q2UdH7FlUBSmod6MxC0fr5pg
nXeOmJC41BmebWTCD2vcxZPpDVsVpQGK9qAIXjGOnAwvNX8W+e5UH5fQA9Kzls3r
/YW1WWRzo38vQsZgfhxmC+uJCFewgaQ=
-----END CERTIFICATE-----"#;

const TEST_WEBSOCKET_CERTIFICATE: &str = r#"-----BEGIN CERTIFICATE-----
MIIDWTCCAkGgAwIBAgIUIxjMlJkiQ9vkSyamKGPk0614EucwDQYJKoZIhvcNAQEL
BQAwKDEmMCQGA1UEAwwdYmV0dGVyY29kZXggdGVzdCBXZWJTb2NrZXQgQ0EwIBcN
MjYwODE3MTczOTIyWhgPMjEyNjA3MjQxNzM5MjJaMBQxEjAQBgNVBAMMCWxvY2Fs
aG9zdDCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAK58xz1uxQ1C0hzr
lv+MjAuT+D/FY57GD/v/tiv3J+Jqwz17R4YmoviWGGqFFSuSAzthAZF6FYq33+s5
5M8da++XDbBO29I3900uljQgrChMclOIv0m9qSOgXOGyGxKWWfJN7gi/+Gj5YrVN
X2qZNQf6AmIF4EBd0BV1O4gp2qxE4ZOHRie3KtSzxq7zD+b/9YzSPudcAPg/OqBl
dH/xyHoGy/I4i0AfFEsjn8gSPMeH/EourrZS4vEDrE9PPUGgJXGTM5oezlbwluqy
aCyKTpKsmvd4/u4JrNaXKx62qa8ZtQQDN+ZR0OAeXL6aHreNlVskspG/M1s0v/jH
X4NE0RMCAwEAAaOBjDCBiTAMBgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIFoDAT
BgNVHSUEDDAKBggrBgEFBQcDATAUBgNVHREEDTALgglsb2NhbGhvc3QwHQYDVR0O
BBYEFIofbvGLeTuVOCiS6uRuh0I0A7n0MB8GA1UdIwQYMBaAFPQP/zbgMcJrPy8Q
gkh7F9AXaeIdMA0GCSqGSIb3DQEBCwUAA4IBAQB89ALMykfNOeUmx5K9XLIvE6ur
pusAntDhy5bnu0byoCx+PteKywMlynOWJsKbYU4/VoUqSK6KZtdxVDAh5K8a0S11
WWIxXtX3eDO9NPrvBe7Cg/5g1V94dYaKNJnP5Eb3ZW6kcf+QLeboWidzuUjytlt3
muOhTcgpQW/neL8x6GXbPRFCIFUJAEAJp2sPKA66NkBlHTVXTECQYL6nRUP7tk3Z
r1idBEOu9xY/3GtcAw3r08tYTNCVeyxCr1O+h9/824aARRruvk7SmA6iaJel28a9
zW84B7dAZkzUF4YkwUtmC+uWiZYLKJv6zc3k/1i1YvPItXUbVWNB0GsKgcsf
-----END CERTIFICATE-----"#;

const TEST_WEBSOCKET_PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCufMc9bsUNQtIc
65b/jIwLk/g/xWOexg/7/7Yr9yfiasM9e0eGJqL4lhhqhRUrkgM7YQGRehWKt9/r
OeTPHWvvlw2wTtvSN/dNLpY0IKwoTHJTiL9JvakjoFzhshsSllnyTe4Iv/ho+WK1
TV9qmTUH+gJiBeBAXdAVdTuIKdqsROGTh0YntyrUs8au8w/m//WM0j7nXAD4Pzqg
ZXR/8ch6BsvyOItAHxRLI5/IEjzHh/xKLq62UuLxA6xPTz1BoCVxkzOaHs5W8Jbq
smgsik6SrJr3eP7uCazWlysetqmvGbUEAzfmUdDgHly+mh63jZVbJLKRvzNbNL/4
x1+DRNETAgMBAAECggEAE/6sfjexUQG1PicpGIOskK8WJYijD9C2iDQXVhZudZ2y
XdtAqPjIeCALEDnL4UBMKoPFQDxzN4A2oqfxtmIyujPfF7MRsZdEOY37HGIaGEwa
VcQ312VqenCn9B0KySh9iiyv+ES3XKAnVYtWQcrors9RcpYlynp1m9/hQIs7Sb4y
XNU01MIDRGc3rw5ah8QlnrB9drXMo84yAWK4K8NavKFQsSEUzLfwVC1gyFwj+XGa
gYCob9MaVcio4a+7906qQSlH7BuZVAp+b8SZbxmVGu/3PG3eEg0RndaeWOqh1FXJ
/7EXheDdAaDf2TJjrCKQPNT0paocICtE0OSGDrrDIQKBgQDkmHlDblmbA/C2Zj5o
ol46QlgYRInZGboI/3re0uq1McFImvyAlduB0XW7CmvZIs6hU1QV+8o4t+7qNMDg
nDcKRaOkzcNLtiX66zhA0Z9Adgyt2KJR3VrgLbkcHjSmBVXff0+QjVZ6bSe/+9g4
VHH2N4Gav5woPktI3IgU+CLoawKBgQDDZ73Y7v20rzP1r6ZZv4eSsqVef82/GLbD
m3AFDwMsGUvx1qmrs/zoCC02j4gbmIjzxtGIAlavyH0uSXCmLBq4DMuXnSDDEQPK
5Npx8x65sUeQhjk9vKbEG1OWWMMYw8DBOD1TzJg/hnc1rTStpaOwEUHS8TMA8ftc
pxNahaaD+QKBgQCiqUSQkPM99P3SLMr31aHLPu5ExnB4hW/1eyXJbLgKmw74RSCr
tvbtV0i5AV9gsP3rmcnZosNwvKFLEqK0sTQRISCi4q+3LjO0arAqn378dYPsKJzI
OAS0RJTVx0CbamyCjqrlJ02D7Cw+1kwzOROmqjSVEwdhM4KKpDJJCZB9ZQKBgDlS
QG3XxdLwJmTnDvx64/FTuJEdGqT5QfvlqBnDyqFwFkguOX2mAgWrCGBeAIZf26Tv
aN3mGbndLWObpZEJlRjyn/Ks5ER0xFELi00sDZJZf+3UggwrQBx9C6sqBKlKG0xT
DCJ9/Rd9gZDca3yY/4iRt2aC3Pxk/+CxHktKs4s5AoGBALlEGbZSrd1PBKWAJgCz
TYUxjM5eG57JI2cslckmYMaPbu+PkoLpy/sNX3DjMi8cJ52eZWFtwUQnW7r1cJpt
SHZnqPB8tXRFtYEkOW5dSEIM6hCs7yQtQWBFWu0lEnN5UC8Tv2Wo1+K6h69pvgzW
xvbq9gKstf9a/AERJfJX6AA2
-----END PRIVATE KEY-----"#;

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
