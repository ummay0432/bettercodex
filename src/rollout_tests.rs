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
fn legacy_resume_reconstructs_user_tool_and_assistant_transcript() {
    let root = temporary_directory("rollout-complete-transcript");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let source =
        "const result = await tools.exec_command({cmd:\"cargo test\"}); text(result.output);";
    rollout
        .append_history(&[json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "run the checks"}],
        })])
        .unwrap();
    rollout
        .append_history(&[json!({
            "type": "custom_tool_call",
            "call_id": "call-1",
            "name": "exec",
            "input": source,
        })])
        .unwrap();
    rollout
        .append_history(&[json!({
            "type": "custom_tool_call_output",
            "call_id": "call-1",
            "name": "exec",
            "output": "still running",
        })])
        .unwrap();
    rollout
        .append_history(&[json!({
            "type": "custom_tool_call_output",
            "call_id": "call-1",
            "output": [
                {
                    "type": "input_text",
                    "text": "Script completed\nWall time 0.1 seconds\nOutput:\n",
                },
                {"type": "input_text", "text": "test result: ok\n"},
            ],
        })])
        .unwrap();
    rollout
        .append_history(&[json!({
            "type": "message",
            "role": "assistant",
            "phase": "final_answer",
            "content": [{"type": "output_text", "text": "All checks pass."}],
        })])
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
                    name: "exec".to_string(),
                    input: Some(Value::String(source.to_string())),
                    output: Some(SessionTranscriptToolOutput::Success(Value::String(
                        "Script completed\nWall time 0.1 seconds\nOutput:\ntest result: ok\n"
                            .to_string(),
                    ))),
                },
            },
            SessionTranscriptItem::Assistant {
                text: "All checks pass.".to_string(),
                phase: Some(MessagePhase::FinalAnswer),
            },
        ]
    );
    assert_eq!(loaded.transcript_checkpoint, None);
}

#[test]
fn legacy_fork_snapshot_recovers_tools_from_matching_history() {
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
    };
    rollout.record_fork("source-session", 0).unwrap();
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
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"pwd\"}",
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call-1",
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
                    call_id: "call-1".to_string(),
                    name: "exec_command".to_string(),
                    input: Some(json!({"cmd": "pwd"})),
                    output: Some(SessionTranscriptToolOutput::Success(json!({
                        "chunk_id": "abc",
                        "exit_code": 0,
                        "output": "/repo\n",
                        "wall_time_seconds": 0.1,
                    }))),
                },
            },
            assistant,
        ]
    );
    assert_eq!(loaded.transcript_checkpoint, None);
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
    };
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
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();

    assert_eq!(loaded.transcript, vec![user.clone(), assistant.clone()]);
    assert_eq!(loaded.transcript_checkpoint, Some(2));

    let recovered = SessionTranscriptItem::Assistant {
        text: "Recovered after interruption.".to_string(),
        phase: Some(MessagePhase::Commentary),
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

    assert_eq!(loaded.transcript, vec![user, assistant, recovered]);
    assert_eq!(loaded.transcript_checkpoint, None);
}

#[test]
fn rollout_replays_replacements_usage_and_turn_state() {
    let root = temporary_directory("rollout-replay");
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
    rollout.record_service_tier(ServiceTier::Fast).unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(
        &root,
        ResumeSelector::Id(Uuid::parse_str(&session_id).unwrap()),
        &cwd,
    )
    .unwrap();
    assert_eq!(loaded.history.len(), 2);
    assert_eq!(loaded.usage, Some(usage));
    assert_eq!(loaded.usage_history_estimate, Some(9));
    assert!(loaded.server_reasoning_included);
    assert_eq!(loaded.unfinished_turn.as_deref(), Some("turn-1"));
    assert_eq!(loaded.compaction_count, 0);
    assert_eq!(loaded.service_tier, ServiceTier::Fast);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollout_replays_the_latest_service_tier_selection() {
    let root = temporary_directory("rollout-service-tier");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    rollout.record_service_tier(ServiceTier::Fast).unwrap();
    rollout.record_service_tier(ServiceTier::Standard).unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.service_tier, ServiceTier::Standard);
    drop(loaded);
    std::fs::remove_dir_all(root).unwrap();
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
    let path = rollout.path.clone();
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
fn appended_records_are_visible_while_the_rollout_is_open() {
    let root = temporary_directory("rollout-flush");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let item = json!({"type": "message", "role": "user", "content": []});

    rollout.append_history(std::slice::from_ref(&item)).unwrap();

    let journal = std::fs::read_to_string(&rollout.path).unwrap();
    let record: RolloutRecord = serde_json::from_str(journal.lines().last().unwrap()).unwrap();
    match record {
        RolloutRecord::HistoryAppend { items } => assert_eq!(items, vec![item]),
        other => panic!("expected a history append, got {other:?}"),
    }

    drop(rollout);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unknown_journal_records_are_ignored_when_resuming() {
    let root = temporary_directory("rollout-unknown-record");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let path = rollout.path.clone();
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

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn latest_resume_is_scoped_to_the_canonical_working_directory() {
    let root = temporary_directory("rollout-latest");
    let first_cwd = root.join("first");
    let second_cwd = root.join("second");
    std::fs::create_dir_all(&first_cwd).unwrap();
    std::fs::create_dir_all(&second_cwd).unwrap();
    let first = Rollout::create_in(&root, &first_cwd).unwrap();
    let first_id = first.identity().session_id.clone();
    drop(first);
    let second = Rollout::create_in(&root, &second_cwd).unwrap();
    drop(second);

    let loaded = Rollout::resume_in(&root, ResumeSelector::LatestForCwd, &first_cwd).unwrap();
    assert_eq!(loaded.metadata.identity.session_id, first_id);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn latest_resume_prefers_the_most_recently_used_matching_session() {
    let root = temporary_directory("rollout-recent");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let first = Rollout::create_in(&root, &cwd).unwrap();
    let first_id = first.identity().session_id.clone();
    let first_path = first.path.clone();
    drop(first);
    let second = Rollout::create_in(&root, &cwd).unwrap();
    drop(second);

    let file = OpenOptions::new().write(true).open(first_path).unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(SystemTime::now() + std::time::Duration::from_secs(5)),
    )
    .unwrap();
    let loaded = Rollout::resume_in(&root, ResumeSelector::LatestForCwd, &cwd).unwrap();
    assert_eq!(loaded.metadata.identity.session_id, first_id);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_resume_rejects_a_journal_with_a_mismatched_session_id() {
    let root = temporary_directory("rollout-mismatched-id");
    let _cleanup = DirectoryCleanup(root.clone());
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let original_path = rollout.path.clone();
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
fn session_listing_streams_past_large_ignored_payloads_to_the_user_preview() {
    let root = temporary_directory("rollout-preview");
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
        summaries[0].preview.as_deref(),
        Some("inspect the persisted session without loading tool payloads")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_active_session_cannot_be_resumed_concurrently() {
    let root = temporary_directory("rollout-exclusive");
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

    let inherited_descriptor = rollout.file.try_clone().unwrap();
    drop(rollout);
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
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn installation_identity_is_stable_and_state_is_private() {
    let root = temporary_directory("rollout-private");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let first = Rollout::create_in(&root, &cwd).unwrap();
    let installation_id = first.identity().installation_id.clone();
    let first_path = first.path.clone();
    drop(first);
    let second = Rollout::create_in(&root, &cwd).unwrap();
    assert_eq!(second.identity().installation_id, installation_id);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(first_path).unwrap().permissions().mode() & 0o777,
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
    #[cfg(windows)]
    assert!(std::fs::metadata(first_path).unwrap().is_file());

    drop(second);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_partial_final_record_is_ignored_after_a_crash() {
    let root = temporary_directory("rollout-partial");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.clone();
    let path = rollout.path.clone();
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

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_complete_record_without_a_final_newline_remains_appendable() {
    let root = temporary_directory("rollout-missing-newline");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.clone();
    let path = rollout.path.clone();
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

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn adjacent_records_without_a_jsonl_newline_are_rejected() {
    let root = temporary_directory("rollout-missing-separator");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.clone();
    let path = rollout.path.clone();
    drop(rollout);

    let file = OpenOptions::new().write(true).open(&path).unwrap();
    let length = file.metadata().unwrap().len();
    file.set_len(length - 1).unwrap();
    drop(file);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"type\":\"history_append\",\"items\":[]}\n")
        .unwrap();
    drop(file);

    let error = Rollout::resume_in(
        &root,
        ResumeSelector::Id(Uuid::parse_str(&session_id).unwrap()),
        &cwd,
    )
    .err()
    .expect("missing JSONL framing must be rejected");
    assert!(error.to_string().contains("invalid session record"));

    std::fs::remove_dir_all(root).unwrap();
}
