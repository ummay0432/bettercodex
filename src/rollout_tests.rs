use super::*;
use serde_json::json;

fn temporary_directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "bettercodex-{name}-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&path).expect("create temporary directory");
    path
}

#[test]
fn legacy_history_replacements_default_missing_response_usage() {
    let record: RolloutRecord = serde_json::from_value(json!({
        "type": "history_replace",
        "reason": "compaction",
        "items": [],
    }))
    .unwrap();

    assert!(matches!(
        record,
        RolloutRecord::HistoryReplace {
            response_usage: None,
            ..
        }
    ));
}

#[test]
fn failed_streamed_record_is_rolled_back_before_later_appends() {
    struct PartialRecord;

    impl serde::Serialize for PartialRecord {
        fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            use serde::ser::SerializeSeq;

            let mut sequence = serializer.serialize_seq(Some(2))?;
            sequence.serialize_element(&"x".repeat(JOURNAL_BUFFER_BYTES * 2))?;
            Err(serde::ser::Error::custom(
                "intentional serialization failure",
            ))
        }
    }

    let root = temporary_directory("rollout-stream-rollback");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.clone();
    let valid_length = rollout.file.metadata().unwrap().len();

    let error = rollout.write_record(&PartialRecord).unwrap_err();
    assert!(error.to_string().contains("failed to append"));
    assert_eq!(rollout.file.metadata().unwrap().len(), valid_length);

    let item = json!({"type": "message", "role": "user"});
    rollout.append_history(std::slice::from_ref(&item)).unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(
        &root,
        ResumeSelector::Id(Uuid::parse_str(&session_id).unwrap()),
        &cwd,
    )
    .unwrap();
    assert_eq!(loaded.history, vec![item]);

    std::fs::remove_dir_all(root).unwrap();
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
fn session_listing_drives_resume_and_uses_the_first_real_user_message() {
    let root = temporary_directory("rollout-list");
    let first_cwd = root.join("first");
    let second_cwd = root.join("second");
    std::fs::create_dir_all(&first_cwd).unwrap();
    std::fs::create_dir_all(&second_cwd).unwrap();

    let mut first = Rollout::create_in(&root, &first_cwd).unwrap();
    let first_id = Uuid::parse_str(&first.identity().session_id).unwrap();
    let first_created = first.metadata.created_at_unix_ms;
    let first_path = first.path.clone();
    let initial = json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": "# Repository onboarding from AGENTS.md for /tmp\nignored",
        }],
    });
    let interruption = json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": "<turn_aborted>ignored</turn_aborted>"}],
    });
    let first_prompt = json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": "  inspect\n  the   picker  "}],
    });
    first
        .replace_history(std::slice::from_ref(&initial), HistoryReplacement::Initial)
        .unwrap();
    first
        .append_history(std::slice::from_ref(&interruption))
        .unwrap();
    first
        .append_history(std::slice::from_ref(&first_prompt))
        .unwrap();
    drop(first);

    let mut second = Rollout::create_in(&root, &second_cwd).unwrap();
    let second_id = Uuid::parse_str(&second.identity().session_id).unwrap();
    let second_created = second.metadata.created_at_unix_ms;
    let second_path = second.path.clone();
    let image_prompt = json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_image", "image_url": "data:image/png;base64,fixture"}],
    });
    second
        .append_history(std::slice::from_ref(&image_prompt))
        .unwrap();
    drop(second);

    for (path, seconds) in [(&first_path, 1_000), (&second_path, 2_000)] {
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
        )
        .unwrap();
    }

    assert_eq!(
        list_sessions_in(&root).unwrap(),
        [
            SessionSummary {
                id: second_id,
                cwd: second_cwd.clone(),
                created_at_unix_ms: second_created,
                updated_at_unix_ms: 2_000_000,
                preview: Some("Image attachment".to_string()),
            },
            SessionSummary {
                id: first_id,
                cwd: first_cwd.clone(),
                created_at_unix_ms: first_created,
                updated_at_unix_ms: 1_000_000,
                preview: Some("inspect the picker".to_string()),
            },
        ]
    );

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(first_id), &second_cwd).unwrap();
    assert_eq!(loaded.metadata.cwd, first_cwd);
    assert_eq!(loaded.history, [initial, interruption, first_prompt]);
    assert_eq!(
        loaded.transcript,
        [SessionTranscriptItem::User {
            text: "  inspect\n  the   picker  ".to_string(),
            image_count: 0,
        }]
    );
    drop(loaded);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn resume_reconstructs_the_visible_transcript_across_compaction() {
    let root = temporary_directory("rollout-transcript");
    let cwd = root.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rollout = Rollout::create_in(&root, &cwd).unwrap();
    let session_id = Uuid::parse_str(&rollout.identity().session_id).unwrap();
    let contextual = json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": "<turn_aborted>hidden</turn_aborted>"}],
    });
    let user = json!({
        "type": "message",
        "role": "user",
        "content": [
            {"type": "input_text", "text": "inspect this"},
            {"type": "input_image", "image_url": "data:image/png;base64,fixture"},
        ],
    });
    let first_answer = json!({
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": "First answer"}],
        "phase": "commentary",
    });
    let final_answer = json!({
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": "After compaction"}],
        "phase": "final_answer",
    });
    rollout
        .append_history(&[contextual, user.clone(), first_answer.clone()])
        .unwrap();
    rollout
        .replace_compacted_history(&[user, first_answer], None)
        .unwrap();
    rollout
        .append_history(std::slice::from_ref(&final_answer))
        .unwrap();
    drop(rollout);

    let loaded = Rollout::resume_in(&root, ResumeSelector::Id(session_id), &cwd).unwrap();

    assert_eq!(
        loaded.transcript,
        [
            SessionTranscriptItem::User {
                text: "inspect this".to_string(),
                image_count: 1,
            },
            SessionTranscriptItem::Assistant {
                text: "First answer".to_string(),
                phase: Some(MessagePhase::Commentary),
            },
            SessionTranscriptItem::Assistant {
                text: "After compaction".to_string(),
                phase: Some(MessagePhase::FinalAnswer),
            },
        ]
    );
    assert_eq!(loaded.history.last(), Some(&final_answer));

    drop(loaded);
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
