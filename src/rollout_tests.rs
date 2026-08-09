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
