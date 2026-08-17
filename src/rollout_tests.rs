use super::*;
use serde_json::json;

fn temporary_directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "bettercodex-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&path).expect("create temporary directory");
    path
}

struct DirectoryCleanup(PathBuf);

impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn resume_reconstructs_function_search_and_citation_transcript() {
    let root = temporary_directory("rollout-tool-search-transcript");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    rollout
        .append_history(&[
            json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "run the checks"}],
            }),
            json!({
                "type": "function_call",
                "call_id": "call-1",
                "namespace": "functions",
                "name": "bash",
                "arguments": "{\"command\":\"cargo test\"}",
            }),
            json!({
                "type": "function_call",
                "call_id": "call-2",
                "namespace": "functions",
                "name": "read",
                "arguments": "{\"path\":\"missing.txt\"}",
            }),
        ])
        .unwrap();
    rollout
        .append_tool_results(
            &[
                json!({
                    "type": "function_call_output",
                    "call_id": "call-1",
                    "output": "{\"stdout\":\"test result: ok\\n\",\"stderr\":\"\",\"exit_code\":0}",
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call-2",
                    "output": "unable to read `missing.txt`",
                }),
            ],
            vec![SessionTranscriptToolOutcome {
                call_id: "call-2".to_string(),
                output: None,
                error: Some("unable to read `missing.txt`".to_string()),
                file_change: None,
            }],
        )
        .unwrap();
    rollout
        .append_history(&[
            json!({
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {"type": "search", "query": "cargo test"},
            }),
            json!({
                "type": "message",
                "role": "assistant",
                "phase": "final_answer",
                "content": [{
                    "type": "output_text",
                    "text": "All checks pass.[1]",
                    "annotations": [{
                        "type": "url_citation",
                        "start_index": 16,
                        "end_index": 19,
                        "url": "https://example.com/checks",
                        "title": "Checks",
                    }],
                }],
            }),
        ])
        .unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(
        loaded.transcript,
        vec![
            SessionTranscriptItem::User {
                text: "run the checks".to_string(),
                image_count: 0,
            },
            SessionTranscriptItem::Tool {
                tool: SessionTranscriptTool {
                    call_id: "call-1".to_string(),
                    origin: SessionTranscriptToolOrigin::Agent,
                    name: "bash".to_string(),
                    input: Some(json!({"command": "cargo test"})),
                    output: Some(SessionTranscriptToolOutput::Success(json!({
                        "stdout": "test result: ok\n",
                        "stderr": "",
                        "exit_code": 0,
                    }))),
                    file_change: None,
                },
            },
            SessionTranscriptItem::Tool {
                tool: SessionTranscriptTool {
                    call_id: "call-2".to_string(),
                    origin: SessionTranscriptToolOrigin::Agent,
                    name: "read".to_string(),
                    input: Some(json!({"path": "missing.txt"})),
                    output: Some(SessionTranscriptToolOutput::Error(
                        "unable to read `missing.txt`".to_string(),
                    )),
                    file_change: None,
                },
            },
            SessionTranscriptItem::WebSearch {
                search: crate::web_search::WebSearchCall {
                    id: "ws_1".to_string(),
                    status: Some("completed".to_string()),
                    action: Some(crate::web_search::WebSearchAction::Search {
                        query: Some("cargo test".to_string()),
                        queries: None,
                    }),
                },
            },
            SessionTranscriptItem::Assistant {
                text: "All checks pass.[1]".to_string(),
                phase: Some(MessagePhase::FinalAnswer),
                citations: vec![crate::web_search::UrlCitation {
                    start_index: 16,
                    end_index: 19,
                    url: "https://example.com/checks".to_string(),
                    title: "Checks".to_string(),
                }],
            },
        ]
    );
    assert_eq!(loaded.transcript_checkpoint, None);
}

#[test]
fn legacy_code_mode_resume_reconstructs_tool_transcript() {
    let root = temporary_directory("rollout-legacy-code-mode-transcript");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let source =
        "const result = await tools.exec_command({cmd:\"cargo test\"}); text(result.output);";
    rollout
        .append_history(&[
            json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "run the checks"}],
            }),
            json!({
                "type": "custom_tool_call",
                "call_id": "call-legacy",
                "name": "exec",
                "input": source,
            }),
            json!({
                "type": "custom_tool_call_output",
                "call_id": "call-legacy",
                "name": "exec",
                "output": "still running",
            }),
            json!({
                "type": "custom_tool_call_output",
                "call_id": "call-legacy",
                "output": [
                    {
                        "type": "input_text",
                        "text": "Script completed\nWall time 0.1 seconds\nOutput:\n",
                    },
                    {"type": "input_text", "text": "test result: ok\n"},
                ],
            }),
            json!({
                "type": "message",
                "role": "assistant",
                "phase": "final_answer",
                "content": [{"type": "output_text", "text": "All checks pass."}],
            }),
        ])
        .unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();

    assert_eq!(
        loaded.transcript,
        vec![
            SessionTranscriptItem::User {
                text: "run the checks".to_string(),
                image_count: 0,
            },
            SessionTranscriptItem::Tool {
                tool: SessionTranscriptTool {
                    call_id: "call-legacy".to_string(),
                    origin: SessionTranscriptToolOrigin::Agent,
                    name: "exec".to_string(),
                    input: Some(Value::String(source.to_string())),
                    output: Some(SessionTranscriptToolOutput::Success(Value::String(
                        "Script completed\nWall time 0.1 seconds\nOutput:\ntest result: ok\n"
                            .to_string(),
                    ))),
                    file_change: None,
                },
            },
            SessionTranscriptItem::Assistant {
                text: "All checks pass.".to_string(),
                phase: Some(MessagePhase::FinalAnswer),
                citations: Vec::new(),
            },
        ]
    );
    assert_eq!(loaded.transcript_checkpoint, None);
}

#[test]
fn incomplete_fork_snapshot_recovers_direct_tools_from_matching_history() {
    let root = temporary_directory("rollout-direct-fork-transcript");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let user = SessionTranscriptItem::User {
        text: "inspect the repository".to_string(),
        image_count: 0,
    };
    let assistant = SessionTranscriptItem::Assistant {
        text: "Inspection complete.".to_string(),
        phase: Some(MessagePhase::FinalAnswer),
        citations: Vec::new(),
    };
    rollout.record_fork("source-session", 0, None).unwrap();
    rollout
        .write_record(&RolloutRecord::TranscriptSnapshot {
            items: vec![user.clone(), assistant.clone()],
            complete: false,
        })
        .unwrap();
    rollout
        .replace_history(
            &[
                json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "inspect the repository"}],
                }),
                json!({
                    "type": "function_call",
                    "call_id": "call-1",
                    "namespace": "functions",
                    "name": "bash",
                    "arguments": "{\"command\":\"pwd\"}",
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call-1",
                    "output": "{\"stdout\":\"/repo\\n\",\"stderr\":\"\",\"exit_code\":0}",
                }),
                json!({
                    "type": "message",
                    "role": "assistant",
                    "phase": "final_answer",
                    "content": [{"type": "output_text", "text": "Inspection complete."}],
                }),
            ],
            HistoryReplacement::Initial,
        )
        .unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();

    assert_eq!(
        loaded.transcript,
        vec![
            user,
            SessionTranscriptItem::Tool {
                tool: SessionTranscriptTool {
                    call_id: "call-1".to_string(),
                    origin: SessionTranscriptToolOrigin::Agent,
                    name: "bash".to_string(),
                    input: Some(json!({"command": "pwd"})),
                    output: Some(SessionTranscriptToolOutput::Success(json!({
                        "stdout": "/repo\n",
                        "stderr": "",
                        "exit_code": 0,
                    }))),
                    file_change: None,
                },
            },
            assistant,
        ]
    );
    assert_eq!(loaded.transcript_checkpoint, None);
}

#[test]
fn legacy_fork_snapshot_recovers_exec_command_from_matching_history() {
    let root = temporary_directory("rollout-legacy-fork-transcript");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let user = SessionTranscriptItem::User {
        text: "inspect the repository".to_string(),
        image_count: 0,
    };
    let assistant = SessionTranscriptItem::Assistant {
        text: "Inspection complete.".to_string(),
        phase: Some(MessagePhase::FinalAnswer),
        citations: Vec::new(),
    };
    rollout.record_fork("source-session", 0, None).unwrap();
    rollout
        .write_record(&RolloutRecord::TranscriptSnapshot {
            items: vec![user.clone(), assistant.clone()],
            complete: false,
        })
        .unwrap();
    rollout
        .replace_history(
            &[
                json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "inspect the repository"}],
                }),
                json!({
                    "type": "function_call",
                    "call_id": "call-legacy",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"pwd\"}",
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call-legacy",
                    "output": "Chunk ID: abc\nWall time: 0.1000 seconds\nProcess exited with code 0\nOutput:\n/repo\n",
                }),
                json!({
                    "type": "message",
                    "role": "assistant",
                    "phase": "final_answer",
                    "content": [{"type": "output_text", "text": "Inspection complete."}],
                }),
            ],
            HistoryReplacement::Initial,
        )
        .unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();

    assert_eq!(
        loaded.transcript,
        vec![
            user,
            SessionTranscriptItem::Tool {
                tool: SessionTranscriptTool {
                    call_id: "call-legacy".to_string(),
                    origin: SessionTranscriptToolOrigin::Agent,
                    name: "exec_command".to_string(),
                    input: Some(json!({"cmd": "pwd"})),
                    output: Some(SessionTranscriptToolOutput::Success(json!({
                        "chunk_id": "abc",
                        "exit_code": 0,
                        "output": "/repo\n",
                        "wall_time_seconds": 0.1,
                    }))),
                    file_change: None,
                },
            },
            assistant,
        ]
    );
    assert_eq!(loaded.transcript_checkpoint, None);
}

#[test]
fn legacy_normalization_replaces_a_stale_wrong_kind_transcript_output() {
    let root = temporary_directory("rollout-legacy-normalization-transcript");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let call = json!({
        "type": "custom_tool_call",
        "call_id": "call-legacy-normalization",
        "name": "opaque_tool",
        "input": "run",
    });
    rollout
        .append_history(&[
            call.clone(),
            json!({
                "type": "function_call_output",
                "call_id": "call-legacy-normalization",
                "output": "stale wrong-kind result",
            }),
        ])
        .unwrap();
    rollout
        .replace_history(
            &[
                call,
                json!({
                    "type": "custom_tool_call_output",
                    "call_id": "call-legacy-normalization",
                    "output": SYNTHETIC_ABORT_OUTPUT,
                }),
            ],
            HistoryReplacement::Normalization,
        )
        .unwrap();
    rollout
        .append_history(&[
            json!({
                "type": "custom_tool_call",
                "call_id": "call-legacy-normalization",
                "name": "opaque_tool",
                "input": "run again",
            }),
            json!({
                "type": "custom_tool_call_output",
                "call_id": "call-legacy-normalization",
                "output": "later completed result",
            }),
        ])
        .unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let tools = loaded
        .transcript
        .iter()
        .filter_map(|item| match item {
            SessionTranscriptItem::Tool { tool } if tool.call_id == "call-legacy-normalization" => {
                Some(tool)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(tools.len(), 2);
    assert!(matches!(
        tools[0].output.as_ref(),
        Some(SessionTranscriptToolOutput::Error(error)) if error == SYNTHETIC_ABORT_OUTPUT
    ));
    assert!(matches!(
        tools[1].output.as_ref(),
        Some(SessionTranscriptToolOutput::Success(Value::Null))
    ));
}

#[test]
fn explicit_transcript_records_replace_fallback_history_and_append_incrementally() {
    let root = temporary_directory("rollout-transcript-checkpoints");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let user = SessionTranscriptItem::User {
        text: "inspect this".to_string(),
        image_count: 0,
    };
    let assistant = SessionTranscriptItem::Assistant {
        text: "Inspection complete.".to_string(),
        phase: Some(MessagePhase::FinalAnswer),
        citations: Vec::new(),
    };
    let operator = SessionTranscriptItem::Tool {
        tool: SessionTranscriptTool {
            call_id: "operator:1".to_string(),
            origin: SessionTranscriptToolOrigin::Operator,
            name: "bash".to_string(),
            input: Some(json!({"command": "git status --short"})),
            output: None,
            file_change: None,
        },
    };
    let operator_output = SessionTranscriptToolOutput::Success(json!({
        "stdout": " M src/main.rs\n",
        "stderr": "",
        "exit_code": 0,
    }));
    rollout
        .append_history(&[json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "inspect this"}],
        })])
        .unwrap();
    rollout.snapshot_transcript(vec![user.clone()]).unwrap();
    rollout
        .append_history(&[json!({
            "type": "message",
            "role": "assistant",
            "phase": "final_answer",
            "content": [{"type": "output_text", "text": "history fallback"}],
        })])
        .unwrap();
    rollout.append_transcript(vec![assistant.clone()]).unwrap();
    rollout.append_transcript(vec![operator.clone()]).unwrap();
    rollout
        .record_tool_outcomes(vec![SessionTranscriptToolOutcome {
            call_id: "operator:1".to_string(),
            output: Some(operator_output.clone()),
            error: None,
            file_change: None,
        }])
        .unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();

    let mut completed_operator = operator;
    let SessionTranscriptItem::Tool { tool } = &mut completed_operator else {
        unreachable!();
    };
    tool.output = Some(operator_output);
    assert_eq!(
        loaded.transcript,
        vec![user.clone(), assistant.clone(), completed_operator.clone()]
    );
    assert_eq!(loaded.transcript_checkpoint, Some(3));

    let recovered = SessionTranscriptItem::Assistant {
        text: "Recovered after interruption.".to_string(),
        phase: Some(MessagePhase::Commentary),
        citations: Vec::new(),
    };
    let mut rollout = loaded.rollout;
    rollout
        .append_history(&[json!({
            "type": "message",
            "role": "assistant",
            "phase": "commentary",
            "content": [{"type": "output_text", "text": "Recovered after interruption."}],
        })])
        .unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();

    assert_eq!(
        loaded.transcript,
        vec![user, assistant, completed_operator, recovered]
    );
    assert_eq!(loaded.transcript_checkpoint, None);
}

#[test]
fn rollout_replays_replacements_usage_and_turn_state() {
    let root = temporary_directory("rollout-replay");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.clone();
    rollout
        .replace_history(
            &[json!({"type": "message", "role": "user"})],
            HistoryReplacement::Initial,
        )
        .unwrap();
    rollout.start_turn("turn-1").unwrap();
    rollout
        .append_history(&[json!({"type": "reasoning", "encrypted_content": "cipher"})])
        .unwrap();
    let usage = TokenUsage {
        input_tokens: 10,
        output_tokens: 4,
        total_tokens: 14,
        ..TokenUsage::default()
    };
    rollout.record_usage(&usage, 9, true).unwrap();
    let latest_usage = TokenUsage {
        input_tokens: 20,
        cached_input_tokens: 5,
        output_tokens: 6,
        total_tokens: 26,
        ..TokenUsage::default()
    };
    rollout.record_usage(&latest_usage, 18, false).unwrap();
    rollout.record_service_tier(ServiceTier::Fast).unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(
        &root,
        ResumeSelector::Id(Uuid::parse_str(&session_id).unwrap()),
        &cwd,
    )
    .unwrap();
    assert_eq!(loaded.history.len(), 2);
    assert_eq!(loaded.usage, Some(latest_usage));
    assert_eq!(loaded.usage_history_estimate, Some(18));
    assert!(!loaded.server_reasoning_included);
    assert_eq!(
        loaded.total_usage,
        TokenUsage {
            input_tokens: 30,
            cached_input_tokens: 5,
            output_tokens: 10,
            total_tokens: 40,
            ..TokenUsage::default()
        }
    );
    assert_eq!(loaded.unfinished_turn.as_deref(), Some("turn-1"));
    assert_eq!(loaded.compaction_count, 0);
    assert_eq!(loaded.service_tier, ServiceTier::Fast);
}

#[test]
fn fork_rollout_replays_parent_identity_and_cumulative_usage() {
    let root = temporary_directory("rollout-fork-usage");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let prior_usage = TokenUsage {
        input_tokens: 100,
        cached_input_tokens: 80,
        output_tokens: 10,
        total_tokens: 110,
        ..TokenUsage::default()
    };
    let latest_usage = TokenUsage {
        input_tokens: 40,
        cached_input_tokens: 20,
        output_tokens: 5,
        total_tokens: 45,
        ..TokenUsage::default()
    };
    rollout
        .record_fork("parent-session", 2, Some(prior_usage))
        .unwrap();
    rollout.record_usage(&latest_usage, 35, true).unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();

    assert_eq!(loaded.forked_from.as_deref(), Some("parent-session"));
    assert_eq!(loaded.compaction_count, 2);
    assert_eq!(
        loaded.total_usage,
        TokenUsage {
            input_tokens: 140,
            cached_input_tokens: 100,
            output_tokens: 15,
            total_tokens: 155,
            ..TokenUsage::default()
        }
    );
}

#[test]
fn rollout_replays_the_latest_service_tier_selection() {
    let root = temporary_directory("rollout-service-tier");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    rollout.record_service_tier(ServiceTier::Fast).unwrap();
    rollout.record_service_tier(ServiceTier::Standard).unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.service_tier, ServiceTier::Standard);
}

#[test]
fn rollout_replays_the_latest_gpt_5_6_selection() {
    let root = temporary_directory("rollout-model-selection");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let initial = crate::model::available_models()[0].selection(ReasoningEffort::Low);
    let mut rollout = Rollout::create_in_with_selection(&root, &cwd, &initial).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let selected = crate::model::available_models()[1].selection(ReasoningEffort::XHigh);
    rollout.record_model_selection(&selected).unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.model_selection, selected);
}

#[test]
fn legacy_non_gpt_5_6_selection_normalizes_to_sol() {
    let root = temporary_directory("rollout-legacy-model");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let path = rollout.file.path.clone();
    drop(rollout);
    let mut journal = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        journal,
        "{}",
        json!({
            "type": "model_changed",
            "selection": {
                "model": "retired-model",
                "reasoning_effort": "high",
            }
        })
    )
    .unwrap();
    drop(journal);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.model_selection.model, crate::model::DEFAULT_MODEL);
    assert_eq!(
        loaded.model_selection.reasoning_effort,
        ReasoningEffort::High
    );
}

#[test]
fn standalone_tool_registrations_from_refactor_rollouts_are_replayed() {
    let root = temporary_directory("rollout-standalone-tool-registration");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let path = rollout.file.path.clone();
    let call = json!({
        "type": "function_call",
        "call_id": "call-legacy-registration",
        "name": "write",
        "arguments": "{\"path\":\"sample.txt\",\"content\":\"hello\"}",
    });
    rollout.start_turn("turn-legacy-registration").unwrap();
    rollout
        .write_record(&RolloutRecord::HistoryAppend {
            items: vec![call],
            outcomes: None,
            tool_calls: None,
        })
        .unwrap();
    drop(rollout);
    let mut journal = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        journal,
        "{}",
        json!({
            "type": "tool_calls_registered",
            "calls": [{
                "call_id": "call-legacy-registration",
                "name": "write",
                "effect": "atomic_mutation",
            }],
        })
    )
    .unwrap();
    drop(journal);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let recovery = &loaded.tool_recoveries["call-legacy-registration"];
    assert!(
        recovery
            .output
            .as_str()
            .unwrap()
            .contains("was registered but did not start")
    );
    assert!(!recovery.requires_inspection);
    assert!(!loaded.crash_recovery_requires_inspection);
}

#[test]
fn appended_records_are_visible_while_the_rollout_is_open() {
    let root = temporary_directory("rollout-flush");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let item = json!({"type": "message", "role": "user", "content": []});

    rollout.append_history(std::slice::from_ref(&item)).unwrap();

    let journal = std::fs::read_to_string(&rollout.file.path).unwrap();
    let header: Value = serde_json::from_str(journal.lines().next().unwrap()).unwrap();
    assert_eq!(header["metadata"]["cwd"], cwd.to_string_lossy().as_ref());
    let record: RolloutRecord = serde_json::from_str(journal.lines().last().unwrap()).unwrap();
    match record {
        RolloutRecord::HistoryAppend {
            items,
            outcomes,
            tool_calls,
        } => {
            assert_eq!(items, vec![item]);
            assert!(outcomes.is_none());
            assert!(tool_calls.is_none());
        }
        other => panic!("expected a history append, got {other:?}"),
    }
}

#[test]
fn concurrent_lifecycle_records_remain_distinct_and_replayable() {
    let root = temporary_directory("rollout-concurrent-lifecycle");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    rollout.start_turn("turn-concurrent-lifecycle").unwrap();
    let calls = (0..16)
        .map(|index| {
            json!({
                "type": "function_call",
                "call_id": format!("call-{index}"),
                "name": "bash",
                "arguments": "{\"command\":\"true\"}",
            })
        })
        .collect::<Vec<_>>();
    rollout.append_history(&calls).unwrap();
    let lifecycle = rollout.tool_lifecycle_journal();

    std::thread::scope(|scope| {
        for index in 0..16 {
            let lifecycle = lifecycle.clone();
            scope.spawn(move || {
                let call_id = format!("call-{index}");
                lifecycle.record_started(&call_id).unwrap();
                lifecycle
                    .record_finished(
                        &call_id,
                        Value::String(format!("result-{index}")),
                        None,
                        None,
                        false,
                    )
                    .unwrap();
            });
        }
    });
    drop(lifecycle);
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.tool_recoveries.len(), 16);
    for index in 0..16 {
        assert_eq!(
            loaded.tool_recoveries[&format!("call-{index}")].output,
            Value::String(format!("result-{index}"))
        );
    }
}

#[test]
fn a_panicking_append_is_rolled_back_without_poisoning_future_writes() {
    let root = temporary_directory("rollout-panicking-append");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = append_rollout_record(&rollout.file, |file| -> Result<()> {
            file.write_all(b"{\"type\":\"history_append\"")?;
            file.flush()?;
            panic!("simulated serializer panic after a partial record");
        });
    }));
    assert!(panic.is_err());

    let item = json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": "writer recovered"}],
    });
    rollout.append_history(std::slice::from_ref(&item)).unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.history, vec![item]);
}

#[test]
fn unknown_journal_records_are_ignored_when_resuming() {
    let root = temporary_directory("rollout-unknown-record");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let path = rollout.file.path.clone();
    let item = json!({"type": "message", "role": "user", "content": []});
    rollout.append_history(std::slice::from_ref(&item)).unwrap();
    drop(rollout);

    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(b"{\"type\":\"retired_extension\",\"payload\":{\"ignored\":true}}\n")
        .unwrap();
    file.flush().unwrap();
    drop(file);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.history, vec![item]);
}

#[test]
fn latest_resume_is_scoped_to_the_canonical_working_directory() {
    let root = temporary_directory("rollout-latest");
    let _cleanup = DirectoryCleanup(root.clone());
    let first_cwd = root.join("first");
    let second_cwd = root.join("second");
    std::fs::create_dir_all(&first_cwd).unwrap();
    std::fs::create_dir_all(&second_cwd).unwrap();
    let mut first = Rollout::create_in(&root, &first_cwd).unwrap();
    let first_id = first.identity().session_id.clone();
    first
        .append_history(&[json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "first repository"}],
        })])
        .unwrap();
    drop(first);
    let mut second = Rollout::create_in(&root, &second_cwd).unwrap();
    second
        .append_history(&[json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "second repository"}],
        })])
        .unwrap();
    drop(second);

    let loaded = Rollout::resume_in(&root, ResumeSelector::LatestForCwd, &first_cwd).unwrap();
    assert_eq!(loaded.metadata.identity.session_id, first_id);
}

#[test]
fn latest_resume_prefers_the_most_recently_used_matching_session() {
    let root = temporary_directory("rollout-recent");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut first = Rollout::create_in(&root, &cwd).unwrap();
    let first_id = first.identity().session_id.clone();
    let first_path = first.file.path.clone();
    first
        .append_history(&[json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "older session"}],
        })])
        .unwrap();
    drop(first);
    let mut second = Rollout::create_in(&root, &cwd).unwrap();
    second
        .append_history(&[json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "newer session"}],
        })])
        .unwrap();
    drop(second);

    let file = OpenOptions::new().write(true).open(first_path).unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(SystemTime::now() + std::time::Duration::from_secs(5)),
    )
    .unwrap();
    let loaded = Rollout::resume_in(&root, ResumeSelector::LatestForCwd, &cwd).unwrap();
    assert_eq!(loaded.metadata.identity.session_id, first_id);
}

#[test]
fn session_discovery_ignores_non_regular_jsonl_entries() {
    let root = temporary_directory("rollout-non-regular-discovery");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let session_path = rollout.file.path.clone();
    rollout
        .append_history(&[json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "valid session"}],
        })])
        .unwrap();
    drop(rollout);

    let sessions = root.join(SESSIONS_DIRECTORY);
    std::fs::create_dir(sessions.join("directory.jsonl")).unwrap();
    let symlink = sessions.join("symlink.jsonl");
    std::os::unix::fs::symlink(&session_path, &symlink).unwrap();
    let fifo = sessions.join("pipe.jsonl");
    use std::os::unix::ffi::OsStrExt as _;
    let fifo_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `fifo_path` is a valid, NUL-terminated pathname that remains alive for the call.
    assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);

    assert!(read_session_summary(&symlink, None).unwrap().is_none());
    assert!(read_session_summary(&fifo, None).unwrap().is_none());
    let summaries = list_sessions_in(&root).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, session_id);
    let latest = latest_rollout_for_cwd(&sessions, &cwd).unwrap().unwrap();
    assert_eq!(latest, session_path);
}

#[test]
fn session_discovery_skips_an_unreadable_journal() {
    let root = temporary_directory("rollout-unreadable-discovery");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let session_path = rollout.file.path.clone();
    rollout
        .append_history(&[json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "healthy session"}],
        })])
        .unwrap();
    drop(rollout);

    let unreadable = root.join(SESSIONS_DIRECTORY).join("unreadable.jsonl");
    std::fs::write(&unreadable, b"unreadable").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

    assert!(has_saved_sessions_in(&root).unwrap());
    let summaries = list_sessions_in(&root).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, session_id);
    assert_eq!(
        latest_rollout_for_cwd(&root.join(SESSIONS_DIRECTORY), &cwd)
            .unwrap()
            .unwrap(),
        session_path
    );
}

#[cfg(not(target_vendor = "apple"))]
#[test]
fn non_utf8_working_directory_round_trips_through_discovery_and_resume() {
    use std::os::unix::ffi::OsStringExt as _;

    let root = temporary_directory("rollout-non-utf8-cwd");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join(std::ffi::OsString::from_vec(b"repo-\xff".to_vec()));
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let session_path = rollout.file.path.clone();
    rollout
        .append_history(&[json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "non-UTF-8 repository"}],
        })])
        .unwrap();
    drop(rollout);

    let journal = std::fs::read_to_string(session_path).unwrap();
    let header: Value = serde_json::from_str(journal.lines().next().unwrap()).unwrap();
    assert!(header["metadata"]["cwd"]["unix_bytes"].is_array());
    let summaries = list_sessions_in(&root).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].cwd, cwd);
    let loaded = Rollout::resume_in(&root, ResumeSelector::LatestForCwd, &cwd).unwrap();
    assert_eq!(loaded.metadata.identity.session_id, session_id.to_string());
    assert_eq!(loaded.metadata.cwd, cwd);
}

#[test]
fn explicit_resume_rejects_a_journal_with_a_mismatched_session_id() {
    let root = temporary_directory("rollout-mismatched-id");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let original_path = rollout.file.path.clone();
    drop(rollout);

    let mismatched_id = Uuid::new_v4();
    let mismatched_path = root
        .join(SESSIONS_DIRECTORY)
        .join(format!("{mismatched_id}.jsonl"));
    std::fs::rename(original_path, mismatched_path).unwrap();

    let error = Rollout::resume_in(&root, ResumeSelector::Id(mismatched_id), &cwd)
        .err()
        .expect("explicit resume must reject a journal whose header has another session ID");
    assert!(format!("{error:#}").contains("does not match journal filename"));
    assert!(list_sessions_in(&root).unwrap().is_empty());
}

#[test]
fn sessions_without_substantive_user_history_are_not_discovered() {
    let root = temporary_directory("rollout-empty");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();

    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let conversation = crate::context::Conversation::new(&cwd, rollout).unwrap();
    drop(conversation);

    assert!(list_sessions_in(&root).unwrap().is_empty());
    assert!(
        latest_rollout_for_cwd(&root.join(SESSIONS_DIRECTORY), &cwd)
            .unwrap()
            .is_none()
    );
}

#[test]
fn session_listing_finds_an_inherited_user_message_in_replaced_history() {
    let root = temporary_directory("rollout-replaced-preview");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    rollout
        .replace_history(
            &[json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "inherited fork prompt"}],
            })],
            HistoryReplacement::Initial,
        )
        .unwrap();
    drop(rollout);

    let summaries = list_sessions_in(&root).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].preview.as_str(), "inherited fork prompt");
}

#[test]
fn session_listing_streams_past_large_ignored_payloads_to_the_user_preview() {
    let root = temporary_directory("rollout-preview");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    rollout
        .append_history(&[
            json!({
                "type": "function_call_output",
                "call_id": "large-output",
                "output": "x".repeat(JOURNAL_BUFFER_BYTES * 4),
            }),
            json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "inspect the persisted session without loading tool payloads",
                }],
            }),
        ])
        .unwrap();
    drop(rollout);

    let summaries = list_sessions_in(&root).unwrap();

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, session_id);
    assert_eq!(
        summaries[0].preview.as_str(),
        "inspect the persisted session without loading tool payloads"
    );
}

#[test]
fn an_active_session_cannot_be_resumed_concurrently() {
    let root = temporary_directory("rollout-exclusive");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();

    let error = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd)
        .err()
        .expect("a second owner must not open an active session journal");
    assert!(
        error
            .to_string()
            .contains("is already open in another bettercodex process")
    );

    let lifecycle = rollout.tool_lifecycle_journal();
    let inherited_descriptor = rollout
        .file
        .file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .try_clone()
        .unwrap();
    drop(rollout);
    let error = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd)
        .err()
        .expect("a lifecycle journal must retain ownership of its active session");
    assert!(
        error
            .to_string()
            .contains("is already open in another bettercodex process")
    );
    drop(lifecycle);
    let resumed = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    drop(inherited_descriptor);
    let error = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd)
        .err()
        .expect("a resumed session must retain ownership of its journal");
    assert!(
        error
            .to_string()
            .contains("is already open in another bettercodex process")
    );
    drop(resumed);
    let resumed = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    drop(resumed);
}

#[test]
fn installation_identity_is_stable_and_state_is_private() {
    let root = temporary_directory("rollout-private");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let installation_path = root.join(INSTALLATION_ID_FILE);
    std::fs::write(&installation_path, "invalid legacy contents").unwrap();
    let first = Rollout::create_in(&root, &cwd).unwrap();
    let installation_id = first.identity().installation_id.clone();
    let first_path = first.file.path.clone();
    drop(first);
    let second = Rollout::create_in(&root, &cwd).unwrap();
    assert_eq!(second.identity().installation_id, installation_id);
    assert_eq!(
        std::fs::read_to_string(&installation_path).unwrap().trim(),
        installation_id
    );

    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(first_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(installation_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(root.join(SESSIONS_DIRECTORY))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn installation_identity_rejects_a_symbolic_link() {
    use std::os::unix::fs::symlink;

    let root = temporary_directory("rollout-installation-symlink");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let target = root.join("unrelated-installation-id");
    let target_id = Uuid::new_v4().to_string();
    std::fs::write(&target, format!("{target_id}\n")).unwrap();
    symlink(&target, root.join(INSTALLATION_ID_FILE)).unwrap();

    let error = Rollout::create_in(&root, &cwd)
        .err()
        .expect("installation identity must not follow a symbolic link");

    assert!(format!("{error:#}").contains("failed to open installation ID"));
    assert_eq!(std::fs::read_to_string(target).unwrap().trim(), target_id);
}

#[test]
fn a_partial_final_record_is_ignored_after_a_crash() {
    let root = temporary_directory("rollout-partial");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.clone();
    let path = rollout.file.path.clone();
    drop(rollout);
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(b"{\"type\":\"history_append\"").unwrap();
    file.flush().unwrap();

    let mut loaded = Rollout::resume_in(
        &root,
        ResumeSelector::Id(Uuid::parse_str(&session_id).unwrap()),
        &cwd,
    )
    .unwrap();
    assert!(loaded.history.is_empty());
    loaded
        .rollout
        .append_history(&[json!({"type": "message", "role": "user"})])
        .unwrap();
    drop(loaded);

    let loaded = Rollout::resume_in(
        &root,
        ResumeSelector::Id(Uuid::parse_str(&session_id).unwrap()),
        &cwd,
    )
    .unwrap();
    assert_eq!(loaded.history.len(), 1);
}

#[test]
fn a_complete_record_without_a_final_newline_remains_appendable() {
    let root = temporary_directory("rollout-missing-newline");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.clone();
    let path = rollout.file.path.clone();
    drop(rollout);
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    let length = file.metadata().unwrap().len();
    file.set_len(length - 1).unwrap();

    let mut loaded = Rollout::resume_in(
        &root,
        ResumeSelector::Id(Uuid::parse_str(&session_id).unwrap()),
        &cwd,
    )
    .unwrap();
    loaded
        .rollout
        .append_history(&[json!({"type": "message", "role": "user"})])
        .unwrap();
    drop(loaded);

    let loaded = Rollout::resume_in(
        &root,
        ResumeSelector::Id(Uuid::parse_str(&session_id).unwrap()),
        &cwd,
    )
    .unwrap();
    assert_eq!(loaded.history.len(), 1);
}

#[test]
fn explicit_resume_requires_the_session_header_to_be_first() {
    let root = temporary_directory("rollout-header-order");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let path = rollout.file.path.clone();
    drop(rollout);

    let existing = std::fs::read(&path).unwrap();
    let mut reordered = b"{\"type\":\"history_append\",\"items\":[]}\n".to_vec();
    reordered.extend(existing);
    std::fs::write(path, reordered).unwrap();

    let error = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd)
        .err()
        .expect("records before the session header must be rejected");
    assert!(format!("{error:#}").contains("session header must be the first record"));
}

#[test]
fn known_records_after_recovery_checkpoint_are_rejected_as_interior_corruption() {
    let root = temporary_directory("rollout-record-after-recovery-checkpoint");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    rollout.start_turn("turn-recovery").unwrap();
    rollout
        .replace_recovered_history(
            &[json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<turn_aborted>\nrecovered\n</turn_aborted>",
                }],
            })],
            Vec::new(),
            "turn-recovery",
            false,
        )
        .unwrap();
    rollout
        .tool_lifecycle_journal()
        .record_started("call-after-recovery")
        .unwrap();
    drop(rollout);

    let error = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd)
        .err()
        .expect("known work after a recovery checkpoint must not be hidden");
    assert!(
        format!("{error:#}").contains("follows a completed turn recovery checkpoint"),
        "{error:#}"
    );
}

#[test]
fn incomplete_recovery_checkpoints_are_rejected_as_interior_corruption() {
    let root = temporary_directory("rollout-incomplete-recovery-checkpoint");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    rollout.start_turn("turn-recovery").unwrap();
    rollout
        .replace_recovered_history(&[], Vec::new(), "turn-recovery", false)
        .unwrap();
    drop(rollout);

    let error = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd)
        .err()
        .expect("a recovery checkpoint without its notice must not be adopted");
    assert!(format!("{error:#}").contains("invalid turn recovery checkpoint"));

    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let prior_notice = json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": "<turn_aborted>\nprior turn\n</turn_aborted>",
        }],
    });
    rollout
        .replace_history(
            std::slice::from_ref(&prior_notice),
            HistoryReplacement::Initial,
        )
        .unwrap();
    rollout.start_turn("turn-prior-notice").unwrap();
    rollout
        .replace_recovered_history(
            std::slice::from_ref(&prior_notice),
            Vec::new(),
            "turn-prior-notice",
            false,
        )
        .unwrap();
    drop(rollout);

    let error = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd)
        .err()
        .expect("a prior turn's notice must not validate a recovery checkpoint");
    assert!(format!("{error:#}").contains("invalid turn recovery checkpoint"));

    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let call = json!({
        "type": "function_call",
        "call_id": "call-missing-outcome",
        "name": "bash",
        "arguments": "{\"command\":\"true\"}",
    });
    rollout.start_turn("turn-missing-outcome").unwrap();
    rollout.append_history(std::slice::from_ref(&call)).unwrap();
    let recovered = vec![
        call,
        json!({
            "type": "function_call_output",
            "call_id": "call-missing-outcome",
            "output": "Recovery: durable result",
        }),
        json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "<turn_aborted>\ncurrent turn\n</turn_aborted>",
            }],
        }),
    ];
    rollout
        .replace_recovered_history(&recovered, Vec::new(), "turn-missing-outcome", false)
        .unwrap();
    drop(rollout);

    let error = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd)
        .err()
        .expect("a recovery checkpoint must include presentation outcomes for repaired calls");
    assert!(format!("{error:#}").contains("invalid turn recovery checkpoint"));
}

#[test]
fn mismatched_complete_turn_records_are_rejected_as_interior_corruption() {
    let root = temporary_directory("rollout-mismatched-turn");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let path = rollout.file.path.clone();
    rollout.start_turn("turn-a").unwrap();
    drop(rollout);

    let mut journal = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        journal,
        "{}",
        json!({
            "type": "turn_finished",
            "turn_id": "turn-b",
            "outcome": "completed",
        })
    )
    .unwrap();
    drop(journal);

    let error = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd)
        .err()
        .expect("a mismatched complete turn record must not be ignored");
    assert!(format!("{error:#}").contains("without a matching active turn"));
}

#[test]
fn adjacent_records_without_a_jsonl_newline_are_rejected() {
    for terminated in [false, true] {
        let root = temporary_directory(&format!(
            "rollout-missing-separator-{}",
            if terminated { "terminated" } else { "tail" }
        ));
        let _cleanup = DirectoryCleanup(root.clone());
        let cwd = root.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let rollout = Rollout::create_in(&root, &cwd).unwrap();
        let session_id = rollout.identity().session_id.clone();
        let path = rollout.file.path.clone();
        drop(rollout);

        let file = OpenOptions::new().write(true).open(&path).unwrap();
        let length = file.metadata().unwrap().len();
        file.set_len(length - 1).unwrap();
        drop(file);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"type\":\"history_append\",\"items\":[]}")
            .unwrap();
        if terminated {
            file.write_all(b"\n").unwrap();
        }
        drop(file);

        let error = Rollout::resume_in(
            &root,
            ResumeSelector::Id(Uuid::parse_str(&session_id).unwrap()),
            &cwd,
        )
        .err()
        .expect("missing JSONL framing must be rejected");
        assert!(error.to_string().contains("invalid session record"));
    }
}
