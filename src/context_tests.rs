use super::*;
use crate::compaction::InitialContextInjection;
use crate::input::UserInput;
use crate::rollout::ResumeSelector;

fn temporary_repository(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "bettercodex-context-{name}-{}-{}",
        std::process::id(),
        crate::new_uuid()
    ));
    let cwd = root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    (root, cwd)
}

#[test]
fn normalization_inserts_stable_outputs_and_removes_orphans() {
    let mut history = vec![
        json!({
            "id": "fc_one",
            "type": "function_call",
            "call_id": "call_one",
            "name": "shell_command",
            "arguments": "{}"
        }),
        json!({
            "type": "function_call_output",
            "call_id": "orphan",
            "output": "lost"
        }),
    ];
    assert!(!history_is_normalized(&history));
    normalize_history(&mut history);
    assert_eq!(history.len(), 2);
    assert_eq!(history[1]["call_id"], "call_one");
    assert_eq!(history[1]["output"], "aborted");
    let first_id = history[1]["id"].clone();
    assert!(history_is_normalized(&history));
    normalize_history(&mut history);
    assert_eq!(history[1]["id"], first_id);
}

#[test]
fn normalization_preserves_exec_notifications_and_final_output() {
    let mut history = vec![
        json!({
            "type": "custom_tool_call",
            "call_id": "call_exec",
            "name": "exec",
            "input": "notify('working'); text('done')",
        }),
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_exec",
            "name": "exec",
            "output": "working",
        }),
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_exec",
            "name": "exec",
            "output": [{"type": "input_text", "text": "done"}],
        }),
    ];
    let expected = history.clone();
    assert!(history_is_normalized(&history));
    normalize_history(&mut history);
    assert_eq!(history, expected);
}

#[test]
fn webp_extended_dimensions_are_included_in_image_budgeting() {
    let mut webp = vec![0_u8; 30];
    webp[..4].copy_from_slice(b"RIFF");
    webp[8..12].copy_from_slice(b"WEBP");
    webp[12..16].copy_from_slice(b"VP8X");
    let width_minus_one = 639_u32.to_le_bytes();
    let height_minus_one = 479_u32.to_le_bytes();
    webp[24..27].copy_from_slice(&width_minus_one[..3]);
    webp[27..30].copy_from_slice(&height_minus_one[..3]);

    assert_eq!(image_dimensions(&webp), Some((640, 480)));
}

#[test]
fn project_root_stops_agents_discovery_at_git_boundary() {
    let root = std::env::temp_dir().join(format!("bettercodex-agents-{}", crate::new_uuid()));
    let repository = root.join("repo");
    let nested = repository.join("nested");
    std::fs::create_dir_all(repository.join(".git")).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("AGENTS.md"), "outside").unwrap();
    std::fs::write(repository.join("AGENTS.md"), "root rule").unwrap();
    std::fs::write(nested.join("AGENTS.override.md"), "nested rule").unwrap();

    let world_state = WorldState::load(&nested).unwrap();
    let repository_item = world_state.repository_context.as_ref().unwrap();
    assert_eq!(repository_item["type"], "message");
    assert_eq!(repository_item["role"], "user");
    assert_eq!(repository_item["content"][0]["type"], "input_text");
    let context = message_text(repository_item).unwrap();
    assert!(context.starts_with(&format!(
        "<repository_context>\n{REPOSITORY_CONTEXT_INSTRUCTION}\n\n<repository_instructions path=\""
    )));
    assert!(context.ends_with("</repository_context>"));
    assert!(context.contains("<repository_instructions path=\""));
    assert!(context.contains("<![CDATA["));
    assert!(context.contains("root rule"));
    assert!(context.contains("nested rule"));
    assert!(!context.contains("outside"));
    assert_eq!(context.matches(REPOSITORY_CONTEXT_INSTRUCTION).count(), 1);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_existing_override_suppresses_the_same_directory_agents_file() {
    let root = std::env::temp_dir().join(format!("bettercodex-agents-{}", crate::new_uuid()));
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("AGENTS.override.md"), "\n").unwrap();
    std::fs::write(root.join("AGENTS.md"), "must not be loaded").unwrap();

    let context = repository_context(&root).unwrap();
    assert!(
        context
            .as_deref()
            .is_none_or(|text| !text.contains("must not be loaded"))
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn agents_content_is_bounded_before_it_enters_model_history() {
    let root = std::env::temp_dir().join(format!("bettercodex-agents-{}", crate::new_uuid()));
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let mut contents = "a".repeat(MAX_REPOSITORY_INSTRUCTIONS_BYTES);
    contents.push_str("TAIL_MUST_NOT_BE_VISIBLE");
    std::fs::write(root.join("AGENTS.md"), contents).unwrap();

    let context = repository_context(&root).unwrap().unwrap();
    assert!(context.contains("[AGENTS.md truncated]"));
    assert!(!context.contains("TAIL_MUST_NOT_BE_VISIBLE"));
    assert!(context.len() < MAX_REPOSITORY_INSTRUCTIONS_BYTES + 1_024);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn compaction_replaces_history_canonically_then_reinjects_world_state() {
    let (root, cwd) = temporary_repository("compaction");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let canonical = vec![json!({
        "type": "compaction_summary",
        "id": "cmp_1",
        "encrypted_content": "opaque",
    })];
    let compaction_usage = TokenUsage {
        input_tokens: 320_000,
        cached_input_tokens: 20_000,
        cache_write_input_tokens: 1_000,
        output_tokens: 2_000,
        reasoning_output_tokens: 1_500,
        total_tokens: 322_000,
    };

    conversation
        .replace_compacted(
            canonical.clone(),
            InitialContextInjection::AfterCompaction,
            &ActiveTurnContext::default(),
            Some(compaction_usage.clone()),
        )
        .unwrap();
    assert_eq!(&conversation.items()[..canonical.len()], canonical);
    assert!(conversation.items().len() > canonical.len());
    assert_eq!(
        conversation
            .items()
            .iter()
            .filter(|item| item["type"] == "compaction_summary")
            .count(),
        1
    );
    drop(conversation);

    let journal = std::fs::read_to_string(
        rollout_root
            .join("sessions")
            .join(format!("{session_id}.jsonl")),
    )
    .unwrap();
    let replacement = journal
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record["type"] == "history_replace" && record["reason"] == "compaction")
        .unwrap();
    assert_eq!(
        replacement["response_usage"],
        serde_json::to_value(compaction_usage).unwrap()
    );

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.compaction_count, 1);
    assert_eq!(loaded.history[0], canonical[0]);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn mid_turn_compaction_keeps_the_opaque_summary_last() {
    let (root, cwd) = temporary_repository("mid-turn-compaction");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let world_state = conversation.world_state.items();
    let current_user = UserInput::text("current turn").into_message_and_skills().0;
    let active_skill = message(
        "user",
        "<skill_context>\n<instructions>exact active workflow</instructions>\n</skill_context>"
            .to_string(),
    );
    let summary = json!({
        "type": "compaction_summary",
        "id": "cmp_mid",
        "encrypted_content": "opaque",
    });
    let mut active_turn_context = ActiveTurnContext::default();
    active_turn_context.record_input(vec![active_skill.clone()]);

    conversation
        .replace_compacted(
            vec![current_user.clone(), summary.clone()],
            InitialContextInjection::BeforeLastUserMessage,
            &active_turn_context,
            None,
        )
        .unwrap();
    assert_eq!(conversation.items().last(), Some(&summary));
    let user_index = conversation
        .items()
        .iter()
        .position(|item| item == &current_user)
        .unwrap();
    let skill_index = conversation
        .items()
        .iter()
        .position(|item| item == &active_skill)
        .unwrap();
    assert!(skill_index < user_index);
    for item in world_state {
        let world_index = conversation
            .items()
            .iter()
            .position(|candidate| candidate == &item)
            .unwrap();
        assert!(world_index < skill_index);
    }

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_turn_repairs_calls_before_adding_the_notice() {
    let (root, cwd) = temporary_repository("interrupt");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .extend([json!({
            "id": "ctc_1",
            "type": "custom_tool_call",
            "call_id": "call_1",
            "name": "exec",
            "input": "text(true)",
        })])
        .unwrap();

    conversation.mark_interrupted().unwrap();
    let call_index = conversation
        .items()
        .iter()
        .position(|item| item["call_id"] == "call_1" && item["type"] == "custom_tool_call")
        .unwrap();
    assert_eq!(
        conversation.items()[call_index + 1]["type"],
        "custom_tool_call_output"
    );
    assert_eq!(conversation.items()[call_index + 1]["output"], "aborted");
    assert!(
        conversation.items().last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("<turn_aborted>")
    );

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn resume_recovers_an_unfinished_turn_and_closes_its_journal_state() {
    let (root, cwd) = temporary_repository("crash-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.start_turn("turn-crashed").unwrap();
    conversation
        .extend([json!({
            "id": "fc_crashed",
            "type": "function_call",
            "call_id": "call_crashed",
            "name": "example",
            "arguments": "{}",
        })])
        .unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(
        resumed
            .items()
            .iter()
            .any(|item| item["call_id"] == "call_crashed" && item["output"] == "aborted")
    );
    assert!(resumed.items().iter().any(|item| {
        item["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("previous bettercodex process ended"))
    }));
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn.is_none());

    std::fs::remove_dir_all(root).unwrap();
}
