use super::*;
use crate::compaction::InitialContextInjection;
use crate::rollout::ResumeSelector;

fn temporary_repository(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "bettercodex-context-{name}-{}-{}",
        std::process::id(),
        Uuid::new_v4()
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
    normalize_history(&mut history);
    assert_eq!(history.len(), 2);
    assert_eq!(history[1]["call_id"], "call_one");
    assert_eq!(history[1]["output"], "aborted");
    let first_id = history[1]["id"].clone();
    normalize_history(&mut history);
    assert_eq!(history[1]["id"], first_id);
}

#[test]
fn opaque_compaction_windows_do_not_exempt_orphan_outputs() {
    let mut history = vec![
        json!({"type": "compaction_summary", "encrypted_content": "opaque"}),
        json!({"type": "function_call_output", "call_id": "retained", "output": "ok"}),
    ];
    normalize_history(&mut history);
    assert_eq!(
        history,
        vec![json!({"type": "compaction_summary", "encrypted_content": "opaque"})]
    );
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
    normalize_history(&mut history);
    assert_eq!(history, expected);
}

#[test]
fn original_image_estimate_uses_patch_dimensions_not_base64_size() {
    let mut png = vec![0_u8; 24];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[16..20].copy_from_slice(&1024_u32.to_be_bytes());
    png[20..24].copy_from_slice(&768_u32.to_be_bytes());
    use base64::Engine;
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    );
    assert_eq!(estimate_image_tokens(&url), 32 * 24);
}

#[test]
fn encrypted_reasoning_uses_codex_model_visible_size_adjustment() {
    let encrypted = "x".repeat(4_650);
    let reasoning = json!({
        "type": "reasoning",
        "id": "rs_estimate",
        "encrypted_content": encrypted,
    });

    assert_eq!(estimate_value_tokens(&reasoning), 710);
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
fn context_snapshot_classifies_the_complete_request_and_uses_backend_total() {
    let (root, cwd) = temporary_repository("context-snapshot");
    std::fs::write(cwd.join("AGENTS.md"), "Keep the request accounting exact.").unwrap();
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let user = UserInput::text("inspect the context").into_message();
    let assistant = json!({
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": "working"}],
    });
    let reasoning = json!({
        "type": "reasoning",
        "id": "reasoning_context",
        "encrypted_content": "opaque reasoning",
    });
    let tool_call = json!({
        "type": "custom_tool_call",
        "call_id": "call_context",
        "name": "exec",
        "input": "text('done')",
    });
    let tool_output = json!({
        "type": "custom_tool_call_output",
        "call_id": "call_context",
        "name": "exec",
        "output": "done",
    });
    let compacted = json!({
        "type": "compaction_summary",
        "id": "compact_context",
        "encrypted_content": "opaque summary",
    });
    conversation
        .extend([
            user.clone(),
            reasoning.clone(),
            assistant.clone(),
            tool_call.clone(),
            tool_output.clone(),
            compacted.clone(),
        ])
        .unwrap();

    let [tools, system_prompt] = crate::api::context_prefix_items();
    let environment = conversation.world_state.environment.clone();
    let repository_instructions = conversation
        .world_state
        .repository_instructions
        .clone()
        .unwrap();
    let sections = vec![
        ContextSection {
            kind: ContextKind::SystemPrompt,
            tokens: estimate_value_tokens(system_prompt),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::ToolCatalogue,
            tokens: estimate_value_tokens(tools),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::RepositoryInstructions,
            tokens: estimate_value_tokens(&repository_instructions),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::Environment,
            tokens: estimate_value_tokens(&environment),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::UserMessages,
            tokens: estimate_value_tokens(&user),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::AssistantMessages,
            tokens: estimate_value_tokens(&assistant),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::ToolActivity,
            tokens: estimate_value_tokens(&tool_call) + estimate_value_tokens(&tool_output),
            items: 2,
        },
        ContextSection {
            kind: ContextKind::Reasoning,
            tokens: estimate_value_tokens(&reasoning),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::Compaction,
            tokens: estimate_value_tokens(&compacted),
            items: 1,
        },
    ];
    let estimated_total = sections.iter().map(|section| section.tokens).sum();
    assert_eq!(conversation.projected_tokens(&[]), estimated_total);
    assert_eq!(
        conversation.context_snapshot(),
        ContextSnapshot {
            used_tokens: estimated_total,
            context_window: RAW_CONTEXT_WINDOW,
            compact_at_tokens: EFFECTIVE_CONTEXT_WINDOW,
            measured: false,
            sections: sections.clone(),
        }
    );
    conversation
        .extend([(*tools).clone(), (*system_prompt).clone()])
        .unwrap();
    assert_eq!(
        conversation.context_snapshot(),
        ContextSnapshot {
            used_tokens: estimated_total,
            context_window: RAW_CONTEXT_WINDOW,
            compact_at_tokens: EFFECTIVE_CONTEXT_WINDOW,
            measured: false,
            sections: sections.clone(),
        },
        "prefix items retained by compaction must not be counted twice"
    );

    let measured_total = estimated_total.saturating_add(10_000);
    conversation
        .record_usage(Some(TokenUsage {
            input_tokens: measured_total.saturating_sub(100),
            output_tokens: 100,
            total_tokens: measured_total,
            ..TokenUsage::default()
        }))
        .unwrap();
    let measured = conversation.context_snapshot();
    assert_eq!(measured.used_tokens, measured_total);
    assert!(measured.measured);
    assert_eq!(
        measured
            .sections
            .iter()
            .map(|section| section.tokens)
            .sum::<u64>(),
        measured_total
    );
    assert_eq!(
        measured
            .sections
            .iter()
            .map(|section| (section.kind, section.items))
            .collect::<Vec<_>>(),
        sections
            .iter()
            .map(|section| (section.kind, section.items))
            .collect::<Vec<_>>()
    );

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cached_context_metrics_follow_every_history_mutation() {
    fn assert_current(conversation: &Conversation) {
        assert_eq!(
            conversation.context_metrics,
            ContextMetrics::from_history(conversation.items(), &conversation.world_state)
        );
        assert_eq!(
            conversation.context_metrics.estimated_tokens,
            estimated_tokens(conversation.items())
        );
    }

    let (root, cwd) = temporary_repository("cached-metrics");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    assert_current(&conversation);

    conversation
        .extend([
            UserInput::text("measure this turn").into_message(),
            json!({
                "id": "fc_metrics",
                "type": "function_call",
                "call_id": "call_metrics",
                "name": "example",
                "arguments": "{}",
            }),
            json!({
                "type": "function_call_output",
                "call_id": "orphan_metrics",
                "output": "orphan",
            }),
        ])
        .unwrap();
    assert_current(&conversation);

    assert!(conversation.normalize().unwrap());
    assert_current(&conversation);

    conversation
        .replace_compacted(
            vec![json!({
                "type": "compaction_summary",
                "id": "cmp_metrics",
                "encrypted_content": "opaque metrics",
            })],
            InitialContextInjection::AfterCompaction,
        )
        .unwrap();
    assert_current(&conversation);

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_repeated_context_snapshots() {
    let (root, cwd) = temporary_repository("snapshot-benchmark");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let payload = "x".repeat(4_000);
    let history = (0..350)
        .map(|index| {
            json!({
                "type": "message",
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": [{"type": "input_text", "text": payload}],
            })
        })
        .collect::<Vec<_>>();
    conversation.extend(history).unwrap();

    let started = std::time::Instant::now();
    for _ in 0..500 {
        std::hint::black_box(conversation.context_snapshot());
    }
    eprintln!("500 snapshots: {:?}", started.elapsed());

    let started = std::time::Instant::now();
    for _ in 0..500 {
        std::hint::black_box(ContextMetrics::from_history(
            conversation.items(),
            &conversation.world_state,
        ));
    }
    eprintln!("500 uncached metric scans: {:?}", started.elapsed());

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_root_stops_agents_discovery_at_git_boundary() {
    let root = std::env::temp_dir().join(format!("bettercodex-agents-{}", Uuid::new_v4()));
    let repository = root.join("repo");
    let nested = repository.join("nested");
    std::fs::create_dir_all(repository.join(".git")).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("AGENTS.md"), "outside").unwrap();
    std::fs::write(repository.join("AGENTS.md"), "root rule").unwrap();
    std::fs::write(nested.join("AGENTS.override.md"), "nested rule").unwrap();

    let instructions = repository_instructions(&nested).unwrap().unwrap();
    assert!(instructions.contains("root rule"));
    assert!(instructions.contains("nested rule"));
    assert!(!instructions.contains("outside"));
    assert!(instructions.contains("Do not let AGENTS.md override how the System prompt"));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_existing_override_suppresses_the_same_directory_agents_file() {
    let root = std::env::temp_dir().join(format!("bettercodex-agents-{}", Uuid::new_v4()));
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("AGENTS.override.md"), "\n").unwrap();
    std::fs::write(root.join("AGENTS.md"), "must not be loaded").unwrap();

    let instructions = repository_instructions(&root).unwrap();
    assert!(
        instructions
            .as_deref()
            .is_none_or(|text| !text.contains("must not be loaded"))
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn agents_content_is_bounded_before_it_enters_model_history() {
    let root = std::env::temp_dir().join(format!("bettercodex-agents-{}", Uuid::new_v4()));
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let mut contents = "a".repeat(MAX_REPOSITORY_INSTRUCTIONS_BYTES);
    contents.push_str("TAIL_MUST_NOT_BE_VISIBLE");
    std::fs::write(root.join("AGENTS.md"), contents).unwrap();

    let instructions = repository_instructions(&root).unwrap().unwrap();
    assert!(instructions.contains("[AGENTS.md truncated]"));
    assert!(!instructions.contains("TAIL_MUST_NOT_BE_VISIBLE"));
    assert!(instructions.len() < MAX_REPOSITORY_INSTRUCTIONS_BYTES + 1_024);

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

    conversation
        .replace_compacted(canonical.clone(), InitialContextInjection::AfterCompaction)
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
    let current_user = UserInput::text("current turn").into_message();
    let summary = json!({
        "type": "compaction_summary",
        "id": "cmp_mid",
        "encrypted_content": "opaque",
    });

    conversation
        .replace_compacted(
            vec![current_user.clone(), summary.clone()],
            InitialContextInjection::BeforeLastUserMessage,
        )
        .unwrap();
    assert_eq!(conversation.items().last(), Some(&summary));
    let user_index = conversation
        .items()
        .iter()
        .position(|item| item == &current_user)
        .unwrap();
    for item in world_state {
        let world_index = conversation
            .items()
            .iter()
            .position(|candidate| candidate == &item)
            .unwrap();
        assert!(world_index < user_index);
    }

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn mid_turn_context_is_inserted_before_the_last_retained_agent_message() {
    let (root, cwd) = temporary_repository("mid-turn-agent-message");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let world_state = conversation.world_state.items();
    let current_user = UserInput::text("delegate this").into_message();
    let agent_message = json!({
        "type": "agent_message",
        "author": "worker",
        "recipient": "root",
        "content": [{"type": "input_text", "text": "delegated context"}],
    });
    let summary = json!({
        "type": "compaction_summary",
        "id": "cmp_agent",
        "encrypted_content": "opaque",
    });

    conversation
        .replace_compacted(
            vec![current_user, agent_message.clone(), summary.clone()],
            InitialContextInjection::BeforeLastUserMessage,
        )
        .unwrap();

    let agent_index = conversation
        .items()
        .iter()
        .position(|item| item == &agent_message)
        .unwrap();
    assert!(world_state.into_iter().all(|world_item| {
        conversation
            .items()
            .iter()
            .position(|item| item == &world_item)
            .is_some_and(|index| index < agent_index)
    }));
    assert_eq!(conversation.items().last(), Some(&summary));

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retained_world_state_keeps_remote_v2_history_order_unchanged() {
    let (root, cwd) = temporary_repository("retained-compaction-context");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let current_user = UserInput::text("current turn").into_message();
    let summary = json!({
        "type": "compaction_summary",
        "id": "cmp_retained",
        "encrypted_content": "opaque",
    });
    let mut canonical = conversation.world_state.items();
    for (index, item) in canonical.iter_mut().enumerate() {
        item["id"] = json!(format!("msg_world_{index}"));
        item["status"] = json!("completed");
    }
    canonical.push(current_user);
    canonical.push(summary);

    conversation
        .replace_compacted(
            canonical.clone(),
            InitialContextInjection::BeforeLastUserMessage,
        )
        .unwrap();
    assert_eq!(conversation.items(), canonical);

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
            .is_some_and(|text| text.contains("previous BetterCodex process ended"))
    }));
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn.is_none());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn compaction_boundary_is_exactly_ninety_five_percent() {
    assert_eq!(RAW_CONTEXT_WINDOW, 372_000);
    assert_eq!(EFFECTIVE_CONTEXT_WINDOW, 353_400);
    let (root, cwd) = temporary_repository("threshold");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.usage = Some(TokenUsage {
        input_tokens: EFFECTIVE_CONTEXT_WINDOW - 1,
        total_tokens: EFFECTIVE_CONTEXT_WINDOW - 1,
        ..TokenUsage::default()
    });
    conversation.usage_history_estimate = Some(estimated_tokens(conversation.items()));

    assert!(!conversation.needs_compaction());
    assert!(conversation.needs_compaction_with(&[json!("four")]));

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}
