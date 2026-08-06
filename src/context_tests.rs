use super::*;
use crate::compaction::InitialContextInjection;
use crate::input::UserInput;
use crate::rollout::ResumeSelector;
use std::fs;

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
    assert!(history_is_normalized(&history));
    normalize_history(&mut history);
    assert_eq!(history, expected);
}

#[test]
fn normalization_removes_function_outputs_mismatched_with_custom_exec() {
    let mut history = vec![
        json!({
            "type": "custom_tool_call",
            "name": "exec",
            "call_id": "call_exec",
            "input": "text('done')",
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call_exec",
            "output": "first",
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call_exec",
            "output": "duplicate",
        }),
    ];

    assert!(!history_is_normalized(&history));
    normalize_history(&mut history);
    assert_eq!(history.len(), 2);
    assert_eq!(history[1]["type"], "custom_tool_call_output");
    assert_eq!(history[1]["output"], "aborted");
    assert!(history_is_normalized(&history));
}

#[test]
fn normalization_removes_custom_outputs_mismatched_with_function_calls() {
    let mut history = vec![
        json!({
            "type": "function_call",
            "name": "shell_command",
            "call_id": "call_function",
            "arguments": "{}",
        }),
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_function",
            "output": "wrong kind",
        }),
    ];

    assert!(!history_is_normalized(&history));
    normalize_history(&mut history);
    assert_eq!(history.len(), 2);
    assert_eq!(history[1]["type"], "function_call_output");
    assert_eq!(history[1]["output"], "aborted");
    assert!(history_is_normalized(&history));
}

#[test]
fn normalization_preserves_the_backend_usage_baseline_for_local_repairs() {
    let (root, cwd) = temporary_repository("normalization-usage");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .extend([
            UserInput::text("run the tool").into_message_and_skills().0,
            json!({
                "id": "fc_missing_output",
                "type": "function_call",
                "call_id": "call_missing_output",
                "name": "example",
                "arguments": "{}",
            }),
        ])
        .unwrap();
    let usage = TokenUsage {
        input_tokens: 900,
        output_tokens: 100,
        total_tokens: 1_000,
        ..TokenUsage::default()
    };
    conversation
        .record_usage(Some(usage.clone()), true)
        .unwrap();

    assert!(conversation.normalize().unwrap());
    let repaired_output = conversation
        .items()
        .iter()
        .find(|item| {
            item["call_id"] == "call_missing_output"
                && item["type"]
                    .as_str()
                    .is_some_and(|item_type| item_type.ends_with("_output"))
        })
        .unwrap();
    assert_eq!(
        conversation.context_tokens(),
        Some(1_000 + estimate_value_tokens(repaired_output))
    );
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.usage, Some(usage));
    assert!(loaded.server_reasoning_included);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn original_and_auto_image_estimates_use_patch_dimensions_not_base64_size() {
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
    for detail in ["original", "auto"] {
        let item = json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_image",
                "image_url": url,
                "detail": detail,
            }],
        });
        let serialized_tokens = serde_json::to_vec(&item).unwrap().len().div_ceil(4) as u64;
        let payload_tokens = url.split_once(',').unwrap().1.len().div_ceil(4) as u64;
        assert_eq!(
            estimate_value_tokens(&item),
            serialized_tokens - payload_tokens + 32 * 24,
            "{detail} must retain the source patch dimensions"
        );
    }
}

#[test]
fn reloading_skill_policy_replaces_the_saved_catalogue_without_blocking_explicit_use() {
    let (root, cwd) = temporary_repository("reload-skills");
    let skill_directory = cwd.join(".bcodex/skills/context-reload");
    fs::create_dir_all(skill_directory.join("agents")).unwrap();
    let skill_path = skill_directory.join("SKILL.md");
    fs::write(
        &skill_path,
        "---\nname: context-reload\ndescription: Reload context policy\n---\n\nCONTEXT RELOAD BODY\n",
    )
    .unwrap();
    fs::write(
        skill_directory.join("agents/openai.yaml"),
        "policy:\n  allow_implicit_invocation: true\n",
    )
    .unwrap();
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.clone();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    assert!(
        conversation
            .items()
            .iter()
            .filter_map(message_text)
            .any(|text| text.starts_with("<available_skills>") && text.contains("context-reload"))
    );

    fs::write(
        skill_directory.join("agents/openai.yaml"),
        "policy:\n  allow_implicit_invocation: false\n",
    )
    .unwrap();
    conversation.reload_skills(&cwd).unwrap();

    assert!(
        !conversation
            .items()
            .iter()
            .filter_map(message_text)
            .any(|text| text.starts_with("<available_skills>") && text.contains("context-reload")),
        "the superseded implicitly invocable catalogue must not remain in history"
    );
    let skill = conversation
        .skill_catalog()
        .skills()
        .iter()
        .find(|skill| skill.name() == "context-reload")
        .unwrap();
    assert!(skill.is_enabled());
    assert!(!skill.allows_implicit_invocation());
    let injection = conversation.skill_catalog().explicit_injections(
        "use $context-reload",
        &[crate::skills::SkillSelection::new(
            "context-reload",
            skill_path.canonicalize().unwrap(),
        )],
    );
    assert_eq!(injection.items.len(), 1);
    assert!(
        message_text(&injection.items[0])
            .unwrap()
            .contains("CONTEXT RELOAD BODY")
    );

    let journal = fs::read_to_string(
        rollout_root
            .join("sessions")
            .join(format!("{session_id}.jsonl")),
    )
    .unwrap();
    assert!(journal.lines().any(|line| {
        let record: Value = serde_json::from_str(line).unwrap();
        record["type"] == "history_replace" && record["reason"] == "context_refresh"
    }));

    drop(conversation);
    fs::remove_dir_all(root).unwrap();
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
fn backend_usage_adds_past_reasoning_only_when_the_server_omits_it() {
    let (root, cwd) = temporary_repository("reasoning-usage");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let reasoning = json!({
        "type": "reasoning",
        "id": "rs_previous",
        "encrypted_content": "x".repeat(4_650),
    });
    conversation
        .extend([
            UserInput::text("first turn").into_message_and_skills().0,
            reasoning.clone(),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "first response"}],
            }),
            UserInput::text("second turn").into_message_and_skills().0,
        ])
        .unwrap();
    let usage = TokenUsage {
        input_tokens: 990,
        output_tokens: 10,
        total_tokens: 1_000,
        ..TokenUsage::default()
    };

    conversation
        .record_usage(Some(usage.clone()), true)
        .unwrap();
    assert_eq!(conversation.context_tokens(), Some(1_000));
    conversation
        .start_turn("turn-after-included-usage")
        .unwrap();
    assert_eq!(
        conversation.context_tokens(),
        Some(1_000),
        "the reasoning-included flag qualifies the retained usage baseline across turns"
    );
    conversation.record_usage(Some(usage), false).unwrap();
    assert_eq!(
        conversation.context_tokens(),
        Some(1_000 + estimate_value_tokens(&reasoning))
    );
    conversation
        .finish_turn("turn-after-included-usage", TurnOutcome::Completed)
        .unwrap();

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn projected_user_boundary_includes_reasoning_that_the_server_omitted() {
    let (root, cwd) = temporary_repository("projected-reasoning-boundary");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let reasoning = json!({
        "type": "reasoning",
        "id": "rs_before_steering",
        "encrypted_content": "x".repeat(110_000),
    });
    conversation
        .extend([
            UserInput::text("initial request")
                .into_message_and_skills()
                .0,
            reasoning.clone(),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "working"}],
            }),
        ])
        .unwrap();
    let measured_tokens = AUTO_COMPACT_TOKEN_LIMIT - 10_000;
    conversation
        .record_usage(
            Some(TokenUsage {
                input_tokens: measured_tokens,
                total_tokens: measured_tokens,
                ..TokenUsage::default()
            }),
            false,
        )
        .unwrap();
    let steering = UserInput::text("answer this while you work")
        .into_message_and_skills()
        .0;
    let expected = measured_tokens
        .saturating_add(estimate_value_tokens(&reasoning))
        .saturating_add(estimate_value_tokens(&steering));

    assert_eq!(conversation.context_tokens(), Some(measured_tokens));
    assert_eq!(
        conversation.projected_tokens(std::slice::from_ref(&steering)),
        expected
    );
    assert!(conversation.needs_compaction_with(std::slice::from_ref(&steering)));

    conversation.extend([steering]).unwrap();
    assert_eq!(conversation.context_tokens(), Some(expected));

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn contextual_user_messages_do_not_turn_current_reasoning_into_past_reasoning() {
    let (root, cwd) = temporary_repository("contextual-reasoning-boundary");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .extend([
            UserInput::text("real user turn")
                .into_message_and_skills()
                .0,
            json!({
                "type": "reasoning",
                "id": "rs_current",
                "encrypted_content": "x".repeat(4_650),
            }),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "current response"}],
            }),
        ])
        .unwrap();
    conversation
        .record_usage(
            Some(TokenUsage {
                input_tokens: 990,
                output_tokens: 10,
                total_tokens: 1_000,
                ..TokenUsage::default()
            }),
            false,
        )
        .unwrap();

    let contextual = message(
        "user",
        "<repository_context>\nupdated instructions\n</repository_context>".to_string(),
    );
    let contextual_tokens = estimate_value_tokens(&contextual);
    conversation.extend([contextual]).unwrap();

    assert_eq!(
        conversation.context_tokens(),
        Some(1_000 + contextual_tokens)
    );

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stream_interruption_notices_are_bounded_before_history_insertion() {
    let (root, cwd) = temporary_repository("bounded-stream-notice");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();

    conversation
        .mark_stream_interrupted(&"x".repeat(50_000))
        .unwrap();

    let notice = conversation.items().last().unwrap();
    let text = message_text(notice).unwrap();
    assert!(text.starts_with("<response_interrupted>\nWarning: truncated output"));
    assert!(text.ends_with("\n</response_interrupted>"));
    assert!(estimate_value_tokens(notice) <= 10_000);

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
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
    let user = UserInput::text("inspect the context")
        .into_message_and_skills()
        .0;
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

    let [tools] = crate::api::stable_input_prefix_items();
    let system_prompt = json!({
        "instructions": crate::api::harness_instructions(),
    });
    let environment = conversation.world_state.environment.clone();
    let repository_context = conversation.world_state.repository_context.clone().unwrap();
    let skills_catalogue = conversation.world_state.skills_catalogue.clone().unwrap();
    let sections = vec![
        ContextSection {
            kind: ContextKind::SystemPrompt,
            tokens: estimate_value_tokens(&system_prompt),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::ToolCatalogue,
            tokens: estimate_value_tokens(tools),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::RepositoryInstructions,
            tokens: estimate_value_tokens(&repository_context),
            items: 1,
        },
        ContextSection {
            kind: ContextKind::Skills,
            tokens: estimate_value_tokens(&skills_catalogue),
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
            context_window: EFFECTIVE_CONTEXT_WINDOW,
            compact_at_tokens: AUTO_COMPACT_TOKEN_LIMIT,
            measured: false,
            sections: sections.clone(),
        }
    );
    conversation.extend([(*tools).clone()]).unwrap();
    assert_eq!(
        conversation.context_snapshot(),
        ContextSnapshot {
            used_tokens: estimated_total,
            context_window: EFFECTIVE_CONTEXT_WINDOW,
            compact_at_tokens: AUTO_COMPACT_TOKEN_LIMIT,
            measured: false,
            sections: sections.clone(),
        },
        "prefix items retained by compaction must not be counted twice"
    );

    let measured_total = estimated_total.saturating_add(10_000);
    conversation
        .record_usage(
            Some(TokenUsage {
                input_tokens: measured_total.saturating_sub(100),
                output_tokens: 100,
                total_tokens: measured_total,
                ..TokenUsage::default()
            }),
            true,
        )
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
            UserInput::text("measure this turn")
                .into_message_and_skills()
                .0,
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
            None,
        )
        .unwrap();
    assert_current(&conversation);

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sampling_history_cursor_tracks_appends_and_rejects_rewrites() {
    let (root, cwd) = temporary_repository("sampling-history-cursor");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .extend([UserInput::text("first").into_message_and_skills().0])
        .unwrap();

    let (history, first) = conversation.take_history_for_sampling();
    assert!(conversation.items().is_empty());
    conversation
        .extend([message("assistant", "response".to_string())])
        .unwrap();
    conversation
        .restore_history_after_sampling(history, first)
        .unwrap();
    assert_eq!(conversation.prompt_history(), ["first"]);

    let (history, appended) = conversation.take_history_for_sampling();
    assert!(appended.includes_response_after(first, 1));
    conversation
        .restore_history_after_sampling(history, appended)
        .unwrap();

    conversation
        .replace_compacted(
            vec![json!({
                "type": "compaction_summary",
                "id": "cmp_cursor",
                "encrypted_content": "opaque",
            })],
            InitialContextInjection::AfterCompaction,
            None,
        )
        .unwrap();
    let (history, rewritten) = conversation.take_history_for_sampling();
    assert!(!rewritten.includes_response_after(appended, 0));
    conversation
        .restore_history_after_sampling(history, rewritten)
        .unwrap();

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

    let context = repository_context(&nested).unwrap().unwrap();
    assert!(context.starts_with("<repository_context>"));
    assert!(context.ends_with("</repository_context>"));
    assert!(context.contains("<repository_instructions path=\""));
    assert!(context.contains("<![CDATA["));
    assert!(context.contains("root rule"));
    assert!(context.contains("nested rule"));
    assert!(!context.contains("outside"));
    assert!(!context.contains("Do not let AGENTS.md override"));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn repository_context_cannot_close_its_cdata_field_from_agents_content() {
    assert_eq!(
        escape_cdata("before ]]> after"),
        "before ]]]]><![CDATA[> after"
    );
}

#[test]
fn an_existing_override_suppresses_the_same_directory_agents_file() {
    let root = std::env::temp_dir().join(format!("bettercodex-agents-{}", Uuid::new_v4()));
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
    let root = std::env::temp_dir().join(format!("bettercodex-agents-{}", Uuid::new_v4()));
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
    let summary = json!({
        "type": "compaction_summary",
        "id": "cmp_mid",
        "encrypted_content": "opaque",
    });

    conversation
        .replace_compacted(
            vec![current_user.clone(), summary.clone()],
            InitialContextInjection::BeforeLastUserMessage,
            None,
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
    let current_user = UserInput::text("delegate this").into_message_and_skills().0;
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
            None,
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
    let current_user = UserInput::text("current turn").into_message_and_skills().0;
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
            None,
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
            .is_some_and(|text| text.contains("previous bettercodex process ended"))
    }));
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn.is_none());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn resume_replaces_stale_world_state_without_losing_the_usage_baseline() {
    let (root, cwd) = temporary_repository("refresh-world-state");
    let rollout_root = root.join("state");
    std::fs::write(
        cwd.join("AGENTS.md"),
        "old saved instruction ".repeat(1_000),
    )
    .unwrap();
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .extend([UserInput::text("saved user turn")
            .into_message_and_skills()
            .0])
        .unwrap();
    let usage = TokenUsage {
        input_tokens: 19_000,
        output_tokens: 1_000,
        total_tokens: 20_000,
        ..TokenUsage::default()
    };
    conversation
        .record_usage(Some(usage.clone()), true)
        .unwrap();
    let context_before_refresh = conversation.context_tokens().unwrap();
    drop(conversation);

    std::fs::write(cwd.join("AGENTS.md"), "current instruction").unwrap();
    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let repository_context = resumed
        .items()
        .iter()
        .filter(|item| {
            item.get("role").and_then(Value::as_str) == Some("user")
                && message_text(item)
                    .is_some_and(|text| text.trim_start().starts_with(REPOSITORY_CONTEXT_PREFIX))
        })
        .collect::<Vec<_>>();
    assert_eq!(repository_context.len(), 1);
    let current_context = message_text(repository_context[0]).unwrap();
    assert!(current_context.contains("current instruction"));
    assert!(!current_context.contains("old saved instruction"));
    assert_eq!(
        resumed.items().last().and_then(message_text),
        Some("saved user turn"),
        "refreshing world state must preserve the current user request as the final input item"
    );
    assert!(resumed.context_tokens().unwrap() < context_before_refresh);
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.usage, Some(usage));
    assert!(loaded.server_reasoning_included);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn compaction_boundary_matches_codex_ninety_percent_auto_compact_limit() {
    assert_eq!(RAW_CONTEXT_WINDOW, 372_000);
    assert_eq!(EFFECTIVE_CONTEXT_WINDOW, 353_400);
    assert_eq!(AUTO_COMPACT_TOKEN_LIMIT, 334_800);
    let (root, cwd) = temporary_repository("threshold");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.usage = Some(TokenUsage {
        input_tokens: AUTO_COMPACT_TOKEN_LIMIT - 1,
        total_tokens: AUTO_COMPACT_TOKEN_LIMIT - 1,
        ..TokenUsage::default()
    });
    conversation.usage_history_estimate = Some(estimated_tokens(conversation.items()));

    assert!(!conversation.needs_compaction());
    assert!(conversation.needs_compaction_with(&[json!("four")]));
    conversation.usage = Some(TokenUsage {
        input_tokens: AUTO_COMPACT_TOKEN_LIMIT,
        total_tokens: AUTO_COMPACT_TOKEN_LIMIT,
        ..TokenUsage::default()
    });
    assert!(conversation.needs_compaction());

    drop(conversation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn resumed_prompt_history_contains_user_inputs_but_not_context_notices() {
    let (root, cwd) = temporary_repository("prompt-history");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .extend([UserInput::text("older prompt").into_message_and_skills().0])
        .unwrap();
    conversation.mark_interrupted().unwrap();
    conversation
        .extend([UserInput::text("newer prompt").into_message_and_skills().0])
        .unwrap();
    assert_eq!(
        conversation.prompt_history(),
        ["older prompt", "newer prompt"]
    );
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_eq!(resumed.prompt_history(), ["older prompt", "newer prompt"]);
    drop(resumed);
    std::fs::remove_dir_all(root).unwrap();
}
