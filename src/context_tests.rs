use super::*;
use crate::compaction::InitialContextInjection;
use crate::input::UserInput;
use crate::rollout::ResumeSelector;
use crate::rollout::SessionTranscriptItem;
use crate::rollout::SessionTranscriptToolOutput;
use crate::rollout::ToolContentDigest;
use crate::rollout::ToolMutationEvidence;
use crate::rollout::ToolPathResolutionEvidence;
use crate::rollout::ToolSymlinkEvidence;
use crate::rollout::ToolTargetPreState;
use crate::tools::ToolCall;
use crate::tools::ToolRuntime;
use tokio_util::sync::CancellationToken;

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "bettercodex-context-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        )))
    }
}

impl std::ops::Deref for TemporaryDirectory {
    type Target = std::path::Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temporary_repository(name: &str) -> (TemporaryDirectory, PathBuf) {
    let root = TemporaryDirectory::new(name);
    let cwd = root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    (root, cwd)
}

fn first_image_url(items: &[Value]) -> Option<&str> {
    items.iter().find_map(|item| {
        item.get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|content| content.get("type").and_then(Value::as_str) == Some("input_image"))
            .and_then(|image| image.get("image_url"))
            .and_then(Value::as_str)
    })
}

fn large_full_resolution_image_url() -> &'static str {
    static IMAGE_URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    IMAGE_URL.get_or_init(|| {
        use base64::Engine as _;

        let mut png = vec![0_u8; 64];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[16..20].copy_from_slice(&6400_u32.to_be_bytes());
        png[20..24].copy_from_slice(&6400_u32.to_be_bytes());
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        )
    })
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
fn normalization_pairs_outputs_only_after_the_latest_matching_calls() {
    let mut history = vec![
        json!({
            "type": "function_call",
            "call_id": "call_reused",
            "name": "bash",
            "arguments": "{}",
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call_reused",
            "output": "stale",
        }),
        json!({
            "type": "function_call",
            "call_id": "call_reused",
            "name": "bash",
            "arguments": "{}",
        }),
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_reused",
            "output": "wrong kind",
        }),
        json!({
            "type": "function_call",
            "call_id": "call_duplicate",
            "name": "bash",
            "arguments": "{}",
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call_duplicate",
            "output": "older duplicate",
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call_duplicate",
            "output": "latest duplicate",
        }),
    ];

    assert!(!history_is_normalized(&history));
    normalize_history(&mut history);

    let latest_reused_call = history
        .iter()
        .rposition(|item| item["type"] == "function_call" && item["call_id"] == "call_reused")
        .unwrap();
    let reused_outputs = history
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item["type"] == "function_call_output" && item["call_id"] == "call_reused"
        })
        .collect::<Vec<_>>();
    assert_eq!(reused_outputs.len(), 1);
    assert!(reused_outputs[0].0 > latest_reused_call);
    assert_eq!(reused_outputs[0].1["output"], SYNTHETIC_ABORT_OUTPUT);
    assert!(history.iter().all(|item| {
        !(item["type"] == "custom_tool_call_output" && item["call_id"] == "call_reused")
    }));

    let duplicate_outputs = history
        .iter()
        .filter(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "call_duplicate"
        })
        .collect::<Vec<_>>();
    assert_eq!(duplicate_outputs.len(), 1);
    assert_eq!(duplicate_outputs[0]["output"], "older duplicate");
    assert!(history_is_normalized(&history));

    let normalized = history.clone();
    normalize_history(&mut history);
    assert_eq!(history, normalized);
}

#[test]
fn normalization_preserves_legacy_exec_final_output_and_notifications() {
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
            "output": [{"type": "input_text", "text": "done"}],
        }),
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call_exec",
            "name": "exec",
            "output": "working",
        }),
    ];

    assert!(history_is_normalized(&history));
    normalize_history(&mut history);
    assert_eq!(history.len(), 3);
}

#[test]
fn normalization_keeps_pre_refactor_synthetic_exec_completions() {
    let mut history = vec![
        json!({
            "id": "ctc_legacy_synthetic",
            "type": "custom_tool_call",
            "call_id": "call_legacy_synthetic",
            "name": "exec",
            "input": "text('done')",
        }),
        json!({
            "id": "ctco_legacy_synthetic",
            "type": "custom_tool_call_output",
            "call_id": "call_legacy_synthetic",
            "name": "exec",
            "output": "aborted",
        }),
    ];

    assert!(history_is_normalized(&history));
    normalize_history(&mut history);
    assert_eq!(history.len(), 2);
}

#[test]
fn rate_limit_updates_retain_omitted_account_metadata() {
    let (root, cwd) = temporary_repository("rate-limit-metadata-merge");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .record_uninstalled_response(
            None,
            vec![RateLimitSnapshot {
                limit_id: "codex".to_string(),
                limit_name: Some("Codex".to_string()),
                primary: Some(crate::rate_limits::RateLimitWindow {
                    used_percent: 10.0,
                    window_minutes: Some(300),
                    resets_at: Some(1_700),
                }),
                secondary: None,
                credits: Some(crate::rate_limits::CreditsSnapshot {
                    has_credits: true,
                    unlimited: false,
                    balance: Some("10.00".to_string()),
                }),
                captured_at: 1_700,
            }],
        )
        .unwrap();
    conversation
        .record_uninstalled_response(
            None,
            vec![RateLimitSnapshot {
                limit_id: "codex".to_string(),
                limit_name: None,
                primary: Some(crate::rate_limits::RateLimitWindow {
                    used_percent: 40.0,
                    window_minutes: Some(300),
                    resets_at: Some(1_800),
                }),
                secondary: None,
                credits: None,
                captured_at: 1_800,
            }],
        )
        .unwrap();

    let snapshot = conversation.context_snapshot();
    let rate_limit = snapshot.rate_limits.first().unwrap();
    assert_eq!(rate_limit.limit_name.as_deref(), Some("Codex"));
    assert_eq!(rate_limit.primary.as_ref().unwrap().used_percent, 40.0);
    assert_eq!(
        rate_limit.credits.as_ref().unwrap().balance.as_deref(),
        Some("10.00")
    );
    assert_eq!(rate_limit.captured_at, 1_800);
}

#[test]
fn legacy_token_usage_defaults_missing_cache_write_tokens() {
    let usage: TokenUsage = serde_json::from_value(json!({
        "input_tokens": 10,
        "cached_input_tokens": 4,
        "output_tokens": 3,
        "reasoning_output_tokens": 2,
        "total_tokens": 13,
    }))
    .unwrap();

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 10,
            cached_input_tokens: 4,
            cache_write_input_tokens: 0,
            output_tokens: 3,
            reasoning_output_tokens: 2,
            total_tokens: 13,
        }
    );
}

#[test]
fn conversation_repairs_only_malformed_call_output_appends() {
    let (root, cwd) = temporary_repository("incremental-normalization");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let call = json!({
        "id": "fc_incremental",
        "type": "function_call",
        "call_id": "call_incremental",
        "name": "example",
        "arguments": "{}",
    });
    let output = json!({
        "type": "function_call_output",
        "call_id": "call_incremental",
        "output": "done",
    });

    conversation.extend([call.clone(), output.clone()]).unwrap();
    assert!(!conversation.normalize().unwrap());

    conversation.extend([output]).unwrap();
    assert!(conversation.normalize().unwrap());
    assert_eq!(
        conversation
            .items()
            .iter()
            .filter(|item| item["type"] == "function_call_output"
                && item["call_id"] == "call_incremental")
            .count(),
        1
    );

    conversation.extend([call]).unwrap();
    assert!(conversation.normalize().unwrap());
    let latest_call = conversation
        .items()
        .iter()
        .rposition(|item| item["type"] == "function_call" && item["call_id"] == "call_incremental")
        .unwrap();
    let outputs = conversation
        .items()
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item["type"] == "function_call_output" && item["call_id"] == "call_incremental"
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert!(outputs[0].0 > latest_call);
    assert_eq!(outputs[0].1["output"], SYNTHETIC_ABORT_OUTPUT);
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
fn image_dimension_reader_stops_after_the_available_header() {
    let mut png = vec![0_u8; 1024 * 1024];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[16..20].copy_from_slice(&2304_u32.to_be_bytes());
    png[20..24].copy_from_slice(&864_u32.to_be_bytes());
    let mut reader = std::io::Cursor::new(png);

    assert_eq!(read_image_dimensions(&mut reader), Some((2304, 864)));
    assert_eq!(reader.position(), 64);
}

#[test]
fn image_dimension_reader_grows_until_a_jpeg_frame_header() {
    let mut jpeg = vec![0_u8; 128];
    jpeg[..4].copy_from_slice(&[0xff, 0xd8, 0xff, 0xe0]);
    jpeg[4..6].copy_from_slice(&100_u16.to_be_bytes());
    jpeg[104..106].copy_from_slice(&[0xff, 0xc0]);
    jpeg[106..108].copy_from_slice(&7_u16.to_be_bytes());
    jpeg[109..111].copy_from_slice(&864_u16.to_be_bytes());
    jpeg[111..113].copy_from_slice(&2304_u16.to_be_bytes());
    let mut reader = std::io::Cursor::new(jpeg);

    assert_eq!(read_image_dimensions(&mut reader), Some((2304, 864)));
    assert_eq!(reader.position(), 128);
}

#[test]
fn jpeg_dimensions_accept_fill_bytes_before_the_frame_header() {
    let jpeg = [
        0xff, 0xd8, 0xff, 0xff, 0xc0, 0x00, 0x07, 0x08, 0x01, 0xe0, 0x02, 0x80,
    ];

    assert_eq!(jpeg_dimensions(&jpeg), Some((640, 480)));
}

#[test]
fn gpt_5_6_full_resolution_image_estimate_uses_uncapped_patch_count() {
    let image_url = large_full_resolution_image_url();
    let expected_image_bytes = u64::from(6400_u32.div_ceil(32))
        .saturating_mul(u64::from(6400_u32.div_ceil(32)))
        .saturating_mul(4);

    assert_eq!(
        estimate_full_resolution_image_bytes(image_url),
        Some(expected_image_bytes)
    );

    let estimate_for_detail = |detail: Option<&str>| {
        let mut item = json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_image",
                "image_url": image_url,
            }],
        });
        if let Some(detail) = detail {
            item["content"][0]["detail"] = Value::String(detail.to_string());
        }
        estimate_value_tokens(&item)
    };
    let resized = estimate_for_detail(Some("high"));
    let full_resolution = [
        estimate_for_detail(None),
        estimate_for_detail(Some("auto")),
        estimate_for_detail(Some("original")),
    ];
    let minimum = *full_resolution.iter().min().unwrap();
    let maximum = *full_resolution.iter().max().unwrap();
    assert!(maximum.abs_diff(minimum) <= 8);
    for estimate in full_resolution {
        assert!(estimate > resized.saturating_add(7_000));
    }
}

#[test]
fn opaque_compaction_estimate_counts_every_preserved_field() {
    let base = json!({
        "type": "compaction",
        "id": "cmp_base",
        "encrypted_content": "encrypted".repeat(1_024),
    });
    let mut extended = base.clone();
    extended["opaque_extension"] = Value::String("extension".repeat(2_048));

    assert!(estimate_value_tokens(&extended) > estimate_value_tokens(&base).saturating_add(4_000));
}

#[test]
fn projected_append_commits_precomputed_context_metrics_and_history() {
    let (root, cwd) = temporary_repository("projected-append");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let initial_len = conversation.items().len();
    let items = vec![message("user", "project once".to_string())];
    let expected_additional_tokens = estimated_tokens(&items);
    let projection = conversation.project_append(items.clone());
    assert_eq!(projection.additional_tokens(), expected_additional_tokens);
    let projected_tokens = projection.projected_tokens();
    conversation.append_projected(projection).unwrap();

    assert_eq!(&conversation.items()[initial_len..], items);
    assert_eq!(conversation.active_context_tokens(), projected_tokens);
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(&loaded.history[initial_len..], items);
}

#[test]
fn assistant_commentary_does_not_advance_the_reasoning_instruction_boundary() {
    let (root, cwd) = temporary_repository("commentary-reasoning-boundary");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let earlier_reasoning = json!({
        "type": "reasoning",
        "encrypted_content": "earlier encrypted reasoning".repeat(512),
    });
    let active_turn_reasoning = json!({
        "type": "reasoning",
        "encrypted_content": "active turn encrypted reasoning".repeat(512),
    });
    let commentary = json!({
        "type": "message",
        "role": "assistant",
        "phase": "commentary",
        "content": [{"type": "output_text", "text": "still working"}],
    });
    conversation
        .extend([
            message("user", "first instruction".to_string()),
            earlier_reasoning.clone(),
            message("user", "current instruction".to_string()),
            active_turn_reasoning,
            commentary,
        ])
        .unwrap();
    let usage = TokenUsage {
        input_tokens: 1_000,
        total_tokens: 1_000,
        ..TokenUsage::default()
    };
    conversation
        .record_usage(Some(usage.clone()), false, Vec::new())
        .unwrap();

    assert_eq!(
        conversation.active_context_tokens(),
        usage
            .active_context_tokens()
            .saturating_add(estimated_tokens(std::slice::from_ref(&earlier_reasoning,)))
    );
}

#[test]
fn resume_repairs_legacy_prefix_before_an_interrupted_active_turn() {
    let (root, cwd) = temporary_repository("legacy-harness-prefix-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let active_skill = message(
        "user",
        "<skill_context>\n<instructions>keep this active workflow</instructions>\n</skill_context>"
            .to_string(),
    );
    let previous_user = message("user", "stop the previous turn".to_string());
    let previous_recovery_notice = message(
        "user",
        format!("<turn_aborted>\n{INTERRUPTED_GUIDANCE}\n</turn_aborted>"),
    );
    let current_user = message("user", "continue the interrupted turn".to_string());
    let interrupted_call = json!({
        "id": "fc_interrupted",
        "type": "function_call",
        "call_id": "call_interrupted",
        "name": "read",
        "arguments": r#"{"path":"unfinished.txt"}"#,
    });

    conversation.start_turn("turn-interrupted").unwrap();
    conversation
        .extend([
            previous_user.clone(),
            previous_recovery_notice.clone(),
            json!({
                "type": "additional_tools",
                "role": "developer",
                "tools": [{"type": "namespace", "name": "functions", "tools": []}],
            }),
            message(
                "developer",
                "obsolete in-band harness instructions".to_string(),
            ),
            active_skill.clone(),
            current_user.clone(),
            interrupted_call.clone(),
        ])
        .unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "current repository instructions").unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let recovered_output = loaded.tool_recoveries["call_interrupted"].output.clone();
    let interrupted_output = synthetic_output_with_body(
        &call_descriptor(&interrupted_call).unwrap(),
        recovered_output,
    );
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let recovery_notice = message(
        "user",
        format!("<turn_aborted>\n{CRASH_NOTICE}\n</turn_aborted>"),
    );
    let expected_history = [previous_user, previous_recovery_notice]
        .into_iter()
        .chain(resumed.world_state.items())
        .chain([
            active_skill,
            current_user,
            interrupted_call,
            interrupted_output,
            recovery_notice,
        ])
        .collect::<Vec<_>>();
    let assert_repaired = |history: &[Value]| assert_eq!(history, expected_history);
    assert_repaired(resumed.items());
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_repaired(&loaded.history);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_repaired(resumed.items());
}

#[test]
fn resume_places_world_state_before_all_inputs_of_an_interrupted_turn() {
    let (root, cwd) = temporary_repository("multi-input-interrupted-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let prior_user = message("user", "complete an earlier turn".to_string());
    let terminal_answer = json!({
        "type": "message",
        "id": "msg_prior_terminal",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "earlier turn completed"}],
    });
    let first_skill = message(
        "user",
        "<skill_context>\n<instructions>first active workflow</instructions>\n</skill_context>"
            .to_string(),
    );
    let first_user = message("user", "begin the active turn".to_string());
    let commentary = json!({
        "type": "message",
        "id": "msg_active_commentary",
        "role": "assistant",
        "phase": "commentary",
        "content": [{"type": "output_text", "text": "working"}],
    });
    let response_interrupted = message(
        "user",
        "<response_interrupted>retry the same active turn</response_interrupted>".to_string(),
    );
    let second_skill = message(
        "user",
        "<skill_context>\n<instructions>steered workflow</instructions>\n</skill_context>"
            .to_string(),
    );
    let second_user = message("user", "steer the active turn".to_string());
    let interrupted_call = json!({
        "id": "fc_multi_input_interrupted",
        "type": "function_call",
        "call_id": "call_multi_input_interrupted",
        "name": "read",
        "arguments": r#"{"path":"unfinished.txt"}"#,
    });

    conversation.start_turn("turn-prior").unwrap();
    conversation
        .extend([prior_user.clone(), terminal_answer.clone()])
        .unwrap();
    conversation
        .finish_turn("turn-prior", TurnOutcome::Completed)
        .unwrap();
    conversation.start_turn("turn-interrupted").unwrap();
    conversation
        .extend([
            first_skill.clone(),
            first_user.clone(),
            commentary.clone(),
            response_interrupted.clone(),
            second_skill.clone(),
            second_user.clone(),
            interrupted_call.clone(),
        ])
        .unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "current repository instructions").unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let recovered_output = loaded.tool_recoveries["call_multi_input_interrupted"]
        .output
        .clone();
    let interrupted_output = synthetic_output_with_body(
        &call_descriptor(&interrupted_call).unwrap(),
        recovered_output,
    );
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let recovery_notice = message(
        "user",
        format!("<turn_aborted>\n{CRASH_NOTICE}\n</turn_aborted>"),
    );
    let expected_history = [prior_user, terminal_answer]
        .into_iter()
        .chain(resumed.world_state.items())
        .chain([
            first_skill,
            first_user,
            commentary,
            response_interrupted,
            second_skill,
            second_user,
            interrupted_call,
            interrupted_output,
            recovery_notice,
        ])
        .collect::<Vec<_>>();
    assert_eq!(resumed.items(), expected_history);
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.history, expected_history);
}

#[test]
fn resume_keeps_world_state_before_compacted_active_context_without_a_retained_user() {
    let (root, cwd) = temporary_repository("compacted-active-context-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let active_skill = message(
        "user",
        "<skill_context>\n<instructions>restore this compacted workflow</instructions>\n</skill_context>"
            .to_string(),
    );
    let retained_commentary = json!({
        "type": "message",
        "id": "msg_compacted_commentary",
        "role": "assistant",
        "phase": "commentary",
        "content": [{"type": "output_text", "text": "retain active-turn commentary"}],
    });
    let summary = json!({
        "type": "compaction_summary",
        "id": "cmp_without_retained_user",
        "encrypted_content": "opaque",
    });
    let interrupted_call = json!({
        "id": "fc_compacted_interrupted",
        "type": "function_call",
        "call_id": "call_compacted_interrupted",
        "name": "read",
        "arguments": r#"{"path":"unfinished.txt"}"#,
    });
    let mut active_turn_context = ActiveTurnContext::default();
    active_turn_context.record_real_user_input(vec![active_skill.clone()]);

    conversation
        .start_turn("turn-compacted-interrupted")
        .unwrap();
    conversation
        .replace_compacted(
            vec![retained_commentary.clone(), summary.clone()],
            InitialContextInjection::BeforeLastUserMessage,
            &active_turn_context,
            None,
            &[],
        )
        .unwrap();
    conversation.extend([interrupted_call.clone()]).unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "current repository instructions").unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let recovered_output = loaded.tool_recoveries["call_compacted_interrupted"]
        .output
        .clone();
    let interrupted_output = synthetic_output_with_body(
        &call_descriptor(&interrupted_call).unwrap(),
        recovered_output,
    );
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let recovery_notice = message(
        "user",
        format!("<turn_aborted>\n{CRASH_NOTICE}\n</turn_aborted>"),
    );
    let expected_history = resumed
        .world_state
        .items()
        .into_iter()
        .chain([
            active_skill,
            retained_commentary,
            summary,
            interrupted_call,
            interrupted_output,
            recovery_notice,
        ])
        .collect::<Vec<_>>();
    assert_eq!(resumed.items(), expected_history);
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.history, expected_history);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_eq!(resumed.items(), expected_history);
}

#[test]
fn resume_keeps_refreshed_world_state_after_a_terminal_answer() {
    let (root, cwd) = temporary_repository("terminal-answer-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let current_user = message("user", "finish before the process exits".to_string());
    let terminal_answer = json!({
        "type": "message",
        "id": "msg_terminal",
        "role": "assistant",
        "phase": "final_answer",
        "content": [{"type": "output_text", "text": "completed"}],
    });

    conversation
        .start_turn("turn-completed-before-crash")
        .unwrap();
    conversation
        .extend([current_user.clone(), terminal_answer.clone()])
        .unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "current repository instructions").unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let recovery_notice = message(
        "user",
        format!("<turn_aborted>\n{CRASH_NOTICE}\n</turn_aborted>"),
    );
    let expected_history = [current_user, terminal_answer, recovery_notice]
        .into_iter()
        .chain(resumed.world_state.items())
        .collect::<Vec<_>>();
    assert_eq!(resumed.items(), expected_history);
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.history, expected_history);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_eq!(resumed.items(), expected_history);
}

#[test]
fn resume_treats_pre_input_normalization_as_housekeeping() {
    let (root, cwd) = temporary_repository("pre-input-normalization-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let previous_user = message("user", "complete before housekeeping".to_string());
    let completed_commentary = json!({
        "type": "message",
        "id": "msg_completed_commentary",
        "role": "assistant",
        "phase": "commentary",
        "content": [{"type": "output_text", "text": "completed without a final answer"}],
    });
    let orphan_output = json!({
        "type": "function_call_output",
        "call_id": "orphan_from_completed_turn",
        "output": "obsolete",
    });

    conversation.start_turn("turn-completed").unwrap();
    conversation
        .extend([
            previous_user.clone(),
            completed_commentary.clone(),
            orphan_output,
        ])
        .unwrap();
    conversation
        .finish_turn("turn-completed", TurnOutcome::Completed)
        .unwrap();
    conversation
        .start_turn("turn-crashed-before-input")
        .unwrap();
    assert!(conversation.normalize().unwrap());
    std::fs::write(cwd.join("AGENTS.md"), "current repository instructions").unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let expected_history = [previous_user, completed_commentary]
        .into_iter()
        .chain(resumed.world_state.items())
        .collect::<Vec<_>>();
    assert_eq!(resumed.items(), expected_history);
    assert!(
        resumed
            .items()
            .iter()
            .all(|item| !is_turn_abort_notice(item))
    );
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.history, expected_history);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_eq!(resumed.items(), expected_history);
}

#[test]
fn resume_keeps_refreshed_world_state_after_an_unfinished_context_only_compaction() {
    let (root, cwd) = temporary_repository("context-only-compaction-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let previous_user = message("user", "completed before compaction".to_string());
    let summary = json!({
        "type": "compaction_summary",
        "id": "cmp_before_crash",
        "encrypted_content": "opaque",
    });

    conversation
        .start_turn("turn-compacted-before-crash")
        .unwrap();
    conversation
        .replace_compacted(
            vec![previous_user.clone(), summary.clone()],
            InitialContextInjection::AfterCompaction,
            &ActiveTurnContext::default(),
            None,
            &[],
        )
        .unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "current repository instructions").unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let recovery_notice = message(
        "user",
        format!("<turn_aborted>\n{CRASH_NOTICE}\n</turn_aborted>"),
    );
    let expected_history = [previous_user, summary]
        .into_iter()
        .chain(resumed.world_state.items())
        .chain([recovery_notice])
        .collect::<Vec<_>>();
    assert_eq!(resumed.items(), expected_history);
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.history, expected_history);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_eq!(resumed.items(), expected_history);
}

#[test]
fn resume_repairs_world_state_inserted_inside_trailing_turn_context() {
    let (root, cwd) = temporary_repository("misplaced-world-state-resume");
    let rollout_root = root.join("state");
    let mut rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let world_state = WorldState::load(&cwd).unwrap().items();
    let active_skill = message(
        "user",
        "<skill_context>\n<instructions>preserve this workflow</instructions>\n</skill_context>"
            .to_string(),
    );
    let current_user = message("user", "retry after a failed turn".to_string());
    let malformed = std::iter::once(active_skill.clone())
        .chain(world_state.iter().cloned())
        .chain(std::iter::once(current_user.clone()))
        .collect::<Vec<_>>();
    rollout
        .replace_history(&malformed, HistoryReplacement::ContextRefresh)
        .unwrap();
    drop(rollout);

    let expected = world_state
        .into_iter()
        .chain([active_skill, current_user])
        .collect::<Vec<_>>();
    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_eq!(resumed.items(), expected);
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.history, expected);
}

#[test]
fn resume_prepares_legacy_images_without_rewriting_the_rollout() {
    const REFRESHED_REPOSITORY_CONTEXT: &str =
        "repository context added after the legacy image was saved";

    use base64::Engine as _;
    use image::DynamicImage;
    use image::GenericImageView;
    use image::ImageBuffer;
    use image::ImageFormat;
    use image::Rgba;
    use std::io::Cursor;

    let (root, cwd) = temporary_repository("legacy-image-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();

    let source = ImageBuffer::from_pixel(2048, 2048, Rgba([10_u8, 20, 30, 255]));
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(source)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    let original_url = crate::image::data_url_from_bytes("image/png", &encoded.into_inner());
    conversation
        .extend([
            json!({
                "type": "message",
                "role": "user",
                "content": [
                    {
                        "type": "input_image",
                        "image_url": &original_url,
                        "detail": "high",
                    },
                    {
                        "type": "input_image",
                        "image_url": &original_url,
                        "detail": "auto",
                    },
                    {
                        "type": "input_image",
                        "image_url": &original_url,
                    },
                ],
            }),
            json!({
                "type": "custom_tool_call",
                "call_id": "call_legacy_image",
                "name": "exec",
                "input": "text('legacy image')",
            }),
            json!({
                "type": "custom_tool_call_output",
                "call_id": "call_legacy_image",
                "output": [{
                    "type": "input_image",
                    "image_url": &original_url,
                    "detail": "high",
                }],
            }),
        ])
        .unwrap();
    drop(conversation);
    std::fs::write(cwd.join("AGENTS.md"), REFRESHED_REPOSITORY_CONTEXT).unwrap();

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(
        first_image_url(&loaded.history),
        Some(original_url.as_str())
    );
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(
        resumed
            .items()
            .iter()
            .any(|item| item.to_string().contains(REFRESHED_REPOSITORY_CONTEXT))
    );
    let prepared_dimensions = resumed
        .items()
        .iter()
        .filter_map(|item| {
            item.get("content")
                .or_else(|| item.get("output"))
                .and_then(Value::as_array)
        })
        .flatten()
        .filter_map(|content| content["image_url"].as_str())
        .map(|image_url| {
            let (_, payload) = image_url.split_once(',').unwrap();
            let prepared = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .unwrap();
            image::load_from_memory(&prepared).unwrap().dimensions()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        prepared_dimensions,
        [(1600, 1600), (2048, 2048), (2048, 2048), (1600, 1600),]
    );
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(
        first_image_url(&loaded.history),
        Some(original_url.as_str())
    );
    assert!(
        loaded
            .history
            .iter()
            .any(|item| item.to_string().contains(REFRESHED_REPOSITORY_CONTEXT))
    );
}

#[test]
fn projected_append_rejects_a_changed_conversation() {
    let (root, cwd) = temporary_repository("stale-projected-append");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let projection = conversation.project_append(vec![message("user", "stale".to_string())]);
    conversation
        .extend([message("assistant", "newer".to_string())])
        .unwrap();
    let expected = conversation.items().to_vec();

    let error = conversation.append_projected(projection).unwrap_err();

    assert!(error.to_string().contains("conversation changed"));
    assert_eq!(conversation.items(), expected);
}

#[test]
fn project_root_stops_agents_discovery_at_git_boundary() {
    let root = TemporaryDirectory::new("agents-boundary");
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
    assert!(context.starts_with("<repository_context>\n<repository_instructions path=\""));
    assert!(context.ends_with("</repository_context>"));
    assert!(context.contains("<repository_instructions path=\""));
    assert!(context.contains("<![CDATA["));
    assert!(context.contains("root rule"));
    assert!(context.contains("nested rule"));
    assert!(!context.contains("outside"));
}

#[test]
fn an_existing_override_suppresses_the_same_directory_agents_file() {
    let root = TemporaryDirectory::new("agents-override");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("AGENTS.override.md"), "\n").unwrap();
    std::fs::write(root.join("AGENTS.md"), "must not be loaded").unwrap();

    let context = repository_context(&root).unwrap();
    assert!(
        context
            .as_deref()
            .is_none_or(|text| !text.contains("must not be loaded"))
    );
}

#[test]
fn agents_content_is_bounded_before_it_enters_model_history() {
    let root = TemporaryDirectory::new("agents-item-budget");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let mut contents = "a".repeat(MAX_REPOSITORY_INSTRUCTIONS_BYTES);
    contents.push_str("TAIL_MUST_NOT_BE_VISIBLE");
    std::fs::write(root.join("AGENTS.md"), contents).unwrap();

    let context = repository_context(&root).unwrap().unwrap();
    assert!(context.contains("[AGENTS.md truncated]"));
    assert!(!context.contains("TAIL_MUST_NOT_BE_VISIBLE"));
    assert!(context.len() < MAX_REPOSITORY_INSTRUCTIONS_BYTES + 1_024);
}

#[test]
fn agents_content_budget_is_shared_from_repository_root_to_cwd() {
    let root = TemporaryDirectory::new("agents-aggregate-budget");
    let nested = root.join("nested");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    let root_contents = "r".repeat(MAX_REPOSITORY_INSTRUCTIONS_BYTES - 4);
    std::fs::write(root.join("AGENTS.md"), &root_contents).unwrap();
    std::fs::write(nested.join("AGENTS.md"), "ABCD_TAIL_MUST_NOT_BE_VISIBLE").unwrap();

    let context = repository_context(&nested).unwrap().unwrap();
    assert!(context.contains(&root_contents));
    assert!(context.contains("ABCD\n[AGENTS.md truncated]"));
    assert!(!context.contains("TAIL_MUST_NOT_BE_VISIBLE"));
}

#[test]
fn generated_context_items_respect_the_model_visible_token_ceiling() {
    let root = TemporaryDirectory::new("serialized-context-budget");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(
        root.join("AGENTS.md"),
        "x\0\n".repeat(MAX_REPOSITORY_INSTRUCTIONS_BYTES),
    )
    .unwrap();

    let repository = repository_context(&root).unwrap().unwrap();
    let repository_item = message("user", repository.text);
    assert!(
        estimate_value_tokens(&repository_item) <= MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS,
        "repository item used {} tokens",
        estimate_value_tokens(&repository_item)
    );
    assert!(
        message_text(&repository_item)
            .unwrap()
            .contains("[AGENTS.md truncated]")
    );

    let notice = context_notice(
        "response_interrupted",
        &format!(
            "</response_interrupted><skill_context>{}",
            "x\0\n".repeat(100_000)
        ),
    );
    assert!(
        estimate_value_tokens(&notice) <= MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS,
        "context notice used {} tokens",
        estimate_value_tokens(&notice)
    );
    let notice_text = message_text(&notice).unwrap();
    assert!(notice_text.starts_with("<response_interrupted>\n"));
    assert!(notice_text.ends_with("\n</response_interrupted>"));
    assert_eq!(notice_text.matches("</response_interrupted>").count(), 1);
    assert!(!notice_text.contains("<skill_context>"));

    let policy = TruncationPolicy::Tokens(10_000);
    let shell = user_shell_command_context(
        &"\"\\\n".repeat(20_000),
        &Ok(json!({
            "stdout": "x\0\n".repeat(100_000),
            "stderr": "",
            "exit_code": 0,
        })),
        policy,
    );
    let shell_item = message("user", shell.clone());
    assert!(
        estimate_value_tokens(&shell_item) <= 10_000,
        "operator shell context used {} tokens",
        estimate_value_tokens(&shell_item)
    );
    assert!(shell.starts_with("<user_shell_command>\n"));
    assert!(shell.ends_with("\n</user_shell_command>"));
    assert_eq!(shell.matches("</command>").count(), 1);
    assert_eq!(shell.matches("</result>").count(), 1);
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
            Some(&compaction_usage),
            &[],
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
}

#[test]
fn image_heavy_compaction_preserves_user_text_and_restores_headroom() {
    let (root, cwd) = temporary_repository("image-heavy-compaction");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();

    let operator_text = "preserve this exact image-analysis instruction";
    let image_url = large_full_resolution_image_url();
    let mut content = vec![json!({"type": "input_text", "text": operator_text})];
    content.extend((0..25).map(|_| {
        json!({
            "type": "input_image",
            "image_url": &image_url,
            "detail": "original",
        })
    }));
    let mut source = json!({
        "type": "message",
        "role": "user",
        "content": content,
    });
    mark_operator_user_message(&mut source);
    assert!(
        estimated_tokens(std::slice::from_ref(&source))
            >= conversation.model_selection().auto_compact_token_limit()
    );

    let input_image_count = |history: &[Value]| {
        history
            .iter()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter(|content| content["type"] == "input_image")
            .count()
    };
    let mut compacted = crate::compaction::retained_compacted_history(vec![source]);
    let retained_images = input_image_count(&compacted);
    assert_eq!(retained_images, 25);
    assert_eq!(
        compacted
            .iter()
            .find_map(|item| item.pointer("/content/0/text").and_then(Value::as_str)),
        Some(operator_text)
    );
    compacted.push(json!({
        "type": "compaction",
        "id": "cmp_image_heavy",
        "encrypted_content": "opaque",
    }));
    let active_skill = message(
        "user",
        "<skill_context>\n<instructions>keep image workflow active</instructions>\n</skill_context>"
            .to_string(),
    );
    let mut active_turn_context = ActiveTurnContext::default();
    active_turn_context.record_real_user_input(vec![active_skill.clone()]);

    conversation
        .replace_compacted(
            compacted,
            InitialContextInjection::BeforeLastUserMessage,
            &active_turn_context,
            None,
            &[],
        )
        .unwrap();
    assert!(!conversation.needs_compaction());
    let installed_images = input_image_count(conversation.items());
    assert!(installed_images > 0 && installed_images < retained_images);
    assert_eq!(conversation.prompt_history(), [operator_text]);
    let assert_active_order = |history: &[Value]| {
        let skill = history
            .iter()
            .position(|item| same_model_visible_message(item, &active_skill))
            .unwrap();
        let user = history
            .iter()
            .position(|item| {
                item.pointer("/content/0/text").and_then(Value::as_str) == Some(operator_text)
            })
            .unwrap();
        let compaction = history.iter().position(is_compaction_item).unwrap();
        assert!(skill < user && user < compaction);
        assert_eq!(
            history
                .iter()
                .filter(|item| same_model_visible_message(item, &active_skill))
                .count(),
            1
        );
    };
    assert_active_order(conversation.items());
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.compaction_count, 1);
    assert_eq!(input_image_count(&loaded.history), installed_images);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(!resumed.needs_compaction());
    assert_eq!(resumed.prompt_history(), [operator_text]);
    assert_active_order(resumed.items());
}

#[test]
fn repeated_compaction_cold_resume_keeps_only_the_latest_opaque_history() {
    let (root, cwd) = temporary_repository("repeated-compaction-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let world_state = conversation.world_state.items();
    let first = json!({
        "type": "compaction",
        "id": "cmp_first",
        "encrypted_content": "first opaque history",
    });
    let latest = json!({
        "type": "compaction",
        "id": "cmp_latest",
        "encrypted_content": "latest opaque history",
    });

    for compacted in [&first, &latest] {
        conversation
            .replace_compacted(
                vec![(*compacted).clone()],
                InitialContextInjection::AfterCompaction,
                &ActiveTurnContext::default(),
                None,
                &[],
            )
            .unwrap();
    }
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.compaction_count, 2);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let opaque_items = resumed
        .items()
        .iter()
        .filter(|item| is_compaction_item(item))
        .collect::<Vec<_>>();
    assert_eq!(opaque_items, vec![&latest]);
    for expected in world_state {
        assert_eq!(
            resumed
                .items()
                .iter()
                .filter(|item| same_model_visible_message(item, &expected))
                .count(),
            1
        );
    }
}

#[test]
fn mid_turn_compaction_keeps_the_opaque_summary_last_across_refresh_and_resume() {
    const REFRESHED_REPOSITORY_CONTEXT: &str = "repository changed after mid-turn compaction";

    let (root, cwd) = temporary_repository("mid-turn-compaction");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let world_state = conversation.world_state.items();
    let current_user = UserInput::text("current turn")
        .into_message_and_skills()
        .unwrap()
        .0;
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
    active_turn_context.record_real_user_input(vec![active_skill.clone()]);

    conversation
        .replace_compacted(
            vec![current_user.clone(), summary.clone()],
            InitialContextInjection::BeforeLastUserMessage,
            &active_turn_context,
            None,
            &[],
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

    std::fs::write(cwd.join("AGENTS.md"), REFRESHED_REPOSITORY_CONTEXT).unwrap();
    conversation
        .reload_world_state_for_active_turn(&cwd, &ActiveTurnContext::default())
        .unwrap();
    let assert_refreshed = |history: &[Value]| {
        assert_eq!(history.last(), Some(&summary));
        let repository_index = history
            .iter()
            .position(|item| item.to_string().contains(REFRESHED_REPOSITORY_CONTEXT))
            .unwrap();
        let skill_index = history
            .iter()
            .position(|item| item == &active_skill)
            .unwrap();
        let user_index = history
            .iter()
            .position(|item| item == &current_user)
            .unwrap();
        assert!(repository_index < skill_index && skill_index < user_index);
    };
    assert_refreshed(conversation.items());
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.compaction_count, 1);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_refreshed(resumed.items());
}

#[test]
fn interrupted_turn_repairs_calls_before_adding_the_notice() {
    let (root, cwd) = temporary_repository("interrupt");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .extend([json!({
            "id": "fc_1",
            "type": "function_call",
            "call_id": "call_1",
            "namespace": "functions",
            "name": "bash",
            "arguments": "{\"command\":\"true\"}",
        })])
        .unwrap();

    conversation.mark_interrupted().unwrap();
    let call_index = conversation
        .items()
        .iter()
        .position(|item| item["call_id"] == "call_1" && item["type"] == "function_call")
        .unwrap();
    assert_eq!(
        conversation.items()[call_index + 1]["type"],
        "function_call_output"
    );
    assert_eq!(conversation.items()[call_index + 1]["output"], "aborted");
    assert!(
        conversation.items().last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("<turn_aborted>")
    );

    drop(conversation);
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
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_crashed"
                    && matches!(
                        tool.output.as_ref(),
                        Some(SessionTranscriptToolOutput::Error(error))
                            if error.contains("effects are not classified")
                    )
        )
    }));
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(resumed.items().iter().any(|item| {
        item["call_id"] == "call_crashed"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("effects are not classified"))
    }));
    assert!(resumed.items().iter().any(|item| {
        item["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains(CRASH_GUIDANCE))
    }));
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn.is_none());
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_crashed"
                    && matches!(
                        tool.output.as_ref(),
                        Some(SessionTranscriptToolOutput::Error(error))
                            if error.contains("effects are not classified")
                    )
        )
    }));
}

#[test]
fn unknown_active_turn_records_require_conservative_recovery_guidance() {
    use std::io::Write as _;

    let (root, cwd) = temporary_repository("unknown-active-turn-record");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let journal_path = rollout_root
        .join("sessions")
        .join(format!("{session_id}.jsonl"));
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.start_turn("turn-with-future-record").unwrap();
    drop(conversation);

    let mut journal = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_path)
        .unwrap();
    writeln!(
        journal,
        "{}",
        json!({"type": "future_effect_record", "payload": {"unknown": true}})
    )
    .unwrap();
    drop(journal);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn_has_activity);
    assert!(loaded.crash_recovery_requires_inspection);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(
        resumed
            .items()
            .iter()
            .any(|item| { message_text(item).is_some_and(|text| text.contains(CRASH_GUIDANCE)) })
    );
}

#[test]
fn unknown_record_after_recovery_checkpoint_reopens_recovery() {
    use std::io::Write as _;

    let (root, cwd) = temporary_repository("unknown-after-recovery-checkpoint");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let journal_path = rollout_root
        .join("sessions")
        .join(format!("{session_id}.jsonl"));
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let turn_id = "turn-with-late-future-record";
    conversation.start_turn(turn_id).unwrap();
    let mut checkpoint = conversation.items().to_vec();
    checkpoint.push(context_notice("turn_aborted", CRASH_NOTICE));
    conversation
        .rollout
        .replace_recovered_history(&checkpoint, Vec::new(), turn_id, false)
        .unwrap();
    drop(conversation);

    let mut journal = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_path)
        .unwrap();
    writeln!(journal, "{}", json!({"type": "future_effect_record"})).unwrap();
    drop(journal);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(!loaded.unfinished_turn_recovered);
    assert!(loaded.crash_recovery_requires_inspection);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(
        resumed
            .items()
            .iter()
            .any(|item| { message_text(item).is_some_and(|text| text.contains(CRASH_GUIDANCE)) })
    );
}

#[test]
fn resume_does_not_reuse_an_abort_notice_from_an_earlier_turn() {
    let (root, cwd) = temporary_repository("crash-after-prior-abort");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();

    conversation.start_turn("turn-interrupted").unwrap();
    conversation.mark_interrupted().unwrap();
    conversation
        .finish_turn("turn-interrupted", TurnOutcome::Interrupted)
        .unwrap();
    conversation.start_turn("turn-crashed").unwrap();
    conversation
        .record_usage(
            Some(TokenUsage {
                total_tokens: 1,
                ..TokenUsage::default()
            }),
            false,
            Vec::new(),
        )
        .unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn_has_activity);
    assert!(!loaded.unfinished_turn_has_recovery_notice);
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let notices = resumed
        .items()
        .iter()
        .filter(|item| is_turn_abort_notice(item))
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 2);
    assert!(
        notices
            .iter()
            .any(|item| { message_text(item).is_some_and(|text| text.contains(CRASH_NOTICE)) })
    );
}

#[test]
fn resume_uses_a_sparse_durable_tool_completion_without_registration() {
    let (root, cwd) = temporary_repository("sparse-tool-finish-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let journal_path = rollout_root
        .join("sessions")
        .join(format!("{session_id}.jsonl"));
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.start_turn("turn-sparse-finish").unwrap();
    drop(conversation);

    let mut journal = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_path)
        .unwrap();
    writeln!(
        journal,
        "{}",
        json!({
            "type": "history_append",
            "items": [{
                "type": "function_call",
                "call_id": "call_sparse_finish",
                "name": "bash",
                "arguments": "{\"command\":\"printf done\"}",
            }],
        })
    )
    .unwrap();
    writeln!(
        journal,
        "{}",
        json!({
            "type": "tool_finished",
            "call_id": "call_sparse_finish",
            "output": "{\"exit_code\":0,\"stderr\":\"\",\"stdout\":\"done\"}",
        })
    )
    .unwrap();
    drop(journal);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(
        loaded.tool_recoveries["call_sparse_finish"].output,
        Value::String(r#"{"exit_code":0,"stderr":"","stdout":"done"}"#.to_string())
    );
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_sparse_finish"
                    && matches!(
                        tool.output.as_ref(),
                        Some(SessionTranscriptToolOutput::Success(output))
                            if output["stdout"] == "done"
                    )
        )
    }));

    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_eq!(
        resumed
            .items()
            .iter()
            .filter(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "call_sparse_finish"
            })
            .count(),
        1
    );
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn.is_none());
    assert_eq!(
        loaded
            .history
            .iter()
            .filter(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "call_sparse_finish"
            })
            .count(),
        1
    );
}

#[test]
fn resume_does_not_trust_out_of_order_tool_lifecycle_records() {
    let (root, cwd) = temporary_repository("out-of-order-tool-lifecycle");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .start_turn("turn-out-of-order-lifecycle")
        .unwrap();
    conversation
        .extend([json!({
            "type": "function_call",
            "call_id": "call_out_of_order",
            "name": "bash",
            "arguments": "{\"command\":\"printf done\"}",
        })])
        .unwrap();
    let lifecycle = conversation.tool_lifecycle_journal();
    lifecycle
        .record_finished(
            "call_out_of_order",
            Value::String(r#"{"exit_code":0,"stderr":"","stdout":"done"}"#.to_string()),
            None,
            None,
            false,
        )
        .unwrap();
    lifecycle.record_started("call_out_of_order").unwrap();
    drop(lifecycle);
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let recovery = loaded.tool_recoveries["call_out_of_order"]
        .output
        .as_str()
        .unwrap();
    assert!(recovery.contains("conflict"), "{recovery}");
    assert!(loaded.crash_recovery_requires_inspection);
}

#[test]
fn repeated_unfinished_call_ids_never_borrow_lifecycle_evidence() {
    let (root, cwd) = temporary_repository("repeated-call-id-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.start_turn("turn-repeated-call-id").unwrap();
    conversation
        .extend([
            json!({
                "type": "function_call",
                "call_id": "call_repeated",
                "name": "write",
                "arguments": "{\"path\":\"first.txt\",\"content\":\"first\"}",
            }),
            json!({
                "type": "function_call",
                "call_id": "call_repeated",
                "name": "read",
                "arguments": "{\"path\":\"second.txt\"}",
            }),
        ])
        .unwrap();
    conversation
        .tool_lifecycle_journal()
        .record_started("call_repeated")
        .unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(
        loaded.tool_recoveries["call_repeated"]
            .output
            .as_str()
            .is_some_and(|output| output.contains("more than one saved tool call"))
    );
    let transcript_tools = loaded
        .transcript
        .iter()
        .filter_map(|item| match item {
            SessionTranscriptItem::Tool { tool } if tool.call_id == "call_repeated" => Some(tool),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(transcript_tools.len(), 2);
    assert!(matches!(
        transcript_tools[0].output.as_ref(),
        Some(SessionTranscriptToolOutput::Error(error)) if error == SYNTHETIC_ABORT_OUTPUT
    ));
    assert!(matches!(
        transcript_tools[1].output.as_ref(),
        Some(SessionTranscriptToolOutput::Error(error))
            if error.contains("more than one saved tool call")
    ));
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_eq!(
        resumed
            .items()
            .iter()
            .filter(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "call_repeated"
            })
            .count(),
        1
    );
    assert!(history_is_normalized(resumed.items()));
}

#[test]
fn sparse_lifecycle_cannot_borrow_a_prior_completed_call() {
    let (root, cwd) = temporary_repository("sparse-lifecycle-prior-call");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let call = json!({
        "type": "function_call",
        "call_id": "call_prior_only",
        "name": "bash",
        "arguments": "{\"command\":\"printf first\"}",
    });

    conversation.start_turn("turn-completed-call").unwrap();
    conversation.extend([call]).unwrap();
    conversation
        .extend_tool_results(
            vec![json!({
                "type": "function_call_output",
                "call_id": "call_prior_only",
                "output": r#"{"exit_code":0,"stderr":"","stdout":"first"}"#,
            })],
            Vec::new(),
        )
        .unwrap();
    conversation
        .finish_turn("turn-completed-call", TurnOutcome::Completed)
        .unwrap();

    conversation.start_turn("turn-orphan-lifecycle").unwrap();
    conversation
        .tool_lifecycle_journal()
        .record_started("call_prior_only")
        .unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn_has_activity);
    assert!(loaded.crash_recovery_requires_inspection);
    assert!(!loaded.tool_recoveries.contains_key("call_prior_only"));
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_prior_only"
                    && matches!(
                        tool.output.as_ref(),
                        Some(SessionTranscriptToolOutput::Success(output))
                            if output["stdout"] == "first"
                    )
        )
    }));

    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let outputs = resumed
        .items()
        .iter()
        .filter(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "call_prior_only"
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(
        outputs[0]["output"],
        r#"{"exit_code":0,"stderr":"","stdout":"first"}"#
    );
    assert!(
        resumed
            .items()
            .iter()
            .any(|item| { message_text(item).is_some_and(|text| text.contains(CRASH_GUIDANCE)) })
    );
}

#[test]
fn prior_output_with_reused_call_id_cannot_hide_an_unfinished_lifecycle() {
    let (root, cwd) = temporary_repository("reused-call-id-prior-output");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let call = || {
        json!({
            "type": "function_call",
            "call_id": "call_reused",
            "name": "bash",
            "arguments": "{\"command\":\"printf first\"}",
        })
    };

    conversation.start_turn("turn-completed-call").unwrap();
    conversation.extend([call()]).unwrap();
    conversation
        .extend_tool_results(
            vec![json!({
                "type": "function_call_output",
                "call_id": "call_reused",
                "output": r#"{"exit_code":0,"stderr":"","stdout":"first"}"#,
            })],
            Vec::new(),
        )
        .unwrap();
    conversation
        .finish_turn("turn-completed-call", TurnOutcome::Completed)
        .unwrap();

    conversation.start_turn("turn-reused-call").unwrap();
    conversation.extend([call()]).unwrap();
    conversation
        .tool_lifecycle_journal()
        .record_started("call_reused")
        .unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let recovery = loaded.tool_recoveries["call_reused"]
        .output
        .as_str()
        .unwrap();
    assert!(
        recovery.contains("more than one saved tool call"),
        "{recovery}"
    );
    assert!(loaded.crash_recovery_requires_inspection);
    let transcript_tools = loaded
        .transcript
        .iter()
        .filter_map(|item| match item {
            SessionTranscriptItem::Tool { tool } if tool.call_id == "call_reused" => Some(tool),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(transcript_tools.len(), 2);
    assert!(matches!(
        transcript_tools[0].output.as_ref(),
        Some(SessionTranscriptToolOutput::Success(output)) if output["stdout"] == "first"
    ));
    assert!(matches!(
        transcript_tools[1].output.as_ref(),
        Some(SessionTranscriptToolOutput::Error(error))
            if error.contains("more than one saved tool call")
    ));

    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let latest_call = resumed
        .items()
        .iter()
        .rposition(|item| item["type"] == "function_call" && item["call_id"] == "call_reused")
        .unwrap();
    let outputs = resumed
        .items()
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item["type"] == "function_call_output" && item["call_id"] == "call_reused"
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert!(outputs[0].0 > latest_call);
    assert!(
        outputs[0]
            .1
            .get("output")
            .and_then(Value::as_str)
            .is_some_and(|output| output.contains("more than one saved tool call"))
    );
    assert!(history_is_normalized(resumed.items()));
}

#[test]
fn non_utf8_recovery_paths_are_rendered_losslessly() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let (root, cwd) = temporary_repository("non-utf8-recovery-path");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.start_turn("turn-non-utf8-path").unwrap();
    conversation
        .extend([json!({
            "type": "function_call",
            "call_id": "call_non_utf8_path",
            "name": "write",
            "arguments": "{}",
        })])
        .unwrap();
    let target = cwd.join(OsString::from_vec(b"target-\xff.txt".to_vec()));
    let lifecycle = conversation.tool_lifecycle_journal();
    lifecycle.record_started("call_non_utf8_path").unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_non_utf8_path",
            ToolMutationEvidence {
                target: target.clone(),
                target_parent: Some(
                    crate::private_fs::AnchoredPath::open(&target)
                        .unwrap()
                        .parent_identity(),
                ),
                path_resolution: None,
                pre_state: ToolTargetPreState::Absent,
                post_state: ToolContentDigest::from_bytes(b"intended\n"),
                staging: None,
                missing_parent: None,
            },
        )
        .unwrap();
    drop(lifecycle);
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let recovery = loaded.tool_recoveries["call_non_utf8_path"]
        .output
        .as_str()
        .unwrap();
    assert!(recovery.contains("unix-bytes:"));
    assert!(recovery.contains(r"\xff"));
    assert!(!recovery.contains('\u{fffd}'));

    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(resumed.items().iter().any(|item| {
        item["call_id"] == "call_non_utf8_path"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains(r"\xff"))
    }));
}

#[test]
fn resume_preserves_inspection_required_durable_completion() {
    let (root, cwd) = temporary_repository("inspection-required-completion");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.start_turn("turn-inspection-required").unwrap();
    conversation
        .extend([json!({
            "type": "function_call",
            "call_id": "call_inspection_required",
            "name": "write",
            "arguments": "{}",
        })])
        .unwrap();
    let warning = "Atomic replacement committed in a pinned directory; inspect the requested path before retrying";
    let lifecycle = conversation.tool_lifecycle_journal();
    lifecycle
        .record_finished(
            "call_inspection_required",
            Value::String(warning.to_string()),
            None,
            None,
            true,
        )
        .unwrap();
    drop(lifecycle);
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.crash_recovery_requires_inspection);
    assert_eq!(
        loaded.tool_recoveries["call_inspection_required"].output,
        Value::String(warning.to_string())
    );
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_inspection_required"
                    && tool
                        .output
                        .as_ref()
                        .and_then(SessionTranscriptToolOutput::recovered_file_state_message)
                        == Some(warning)
        )
    }));

    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(resumed.items().iter().any(|item| {
        item.pointer("/content/0/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains(CRASH_GUIDANCE))
    }));
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_inspection_required"
                    && tool
                        .output
                        .as_ref()
                        .and_then(SessionTranscriptToolOutput::recovered_file_state_message)
                        == Some(warning)
        )
    }));
}

#[test]
fn resume_requires_inspection_for_unanchored_legacy_mutation_evidence() {
    let (root, cwd) = temporary_repository("legacy-unanchored-mutation");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.start_turn("turn-legacy-mutation").unwrap();
    conversation
        .extend([json!({
            "type": "function_call",
            "call_id": "call_legacy_mutation",
            "name": "write",
            "arguments": "{}",
        })])
        .unwrap();
    let target = cwd.join("legacy-target.txt");
    std::fs::write(&target, "after\n").unwrap();
    let lifecycle = conversation.tool_lifecycle_journal();
    lifecycle.record_started("call_legacy_mutation").unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_legacy_mutation",
            ToolMutationEvidence {
                target,
                target_parent: None,
                path_resolution: None,
                pre_state: ToolTargetPreState::Digest(ToolContentDigest::from_bytes(b"before\n")),
                post_state: ToolContentDigest::from_bytes(b"after\n"),
                staging: None,
                missing_parent: None,
            },
        )
        .unwrap();
    drop(lifecycle);
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let recovery = loaded.tool_recoveries["call_legacy_mutation"]
        .output
        .as_str()
        .unwrap();
    assert!(recovery.contains("exact contents intended"));
    assert!(recovery.contains("does not include a stable parent identity"));
    assert!(loaded.crash_recovery_requires_inspection);

    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(resumed.items().iter().any(|item| {
        item.pointer("/content/0/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains(CRASH_GUIDANCE))
    }));
}

#[test]
fn resume_reconciles_effect_aware_tool_lifecycles() {
    let (root, cwd) = temporary_repository("effect-aware-crash-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .start_turn("turn-effect-aware-crashed")
        .unwrap();

    let calls = [
        ("call_read", "read"),
        ("call_bash_not_started", "bash"),
        ("call_bash_unknown", "bash"),
        ("call_bash_finished", "bash"),
        ("call_write_not_started", "write"),
        ("call_edit_not_prepared", "edit"),
        ("call_write_post", "write"),
        ("call_edit_pre", "edit"),
        ("call_write_changed", "write"),
        ("call_write_rebound_parent", "write"),
        ("call_write_rebound_symlink", "write"),
        ("call_same_target_old", "write"),
        ("call_same_target_new", "write"),
        ("call_write_parent_residue", "write"),
    ];
    conversation
        .extend(calls.iter().map(|(call_id, name)| {
            json!({
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": "{}",
            })
        }))
        .unwrap();
    let lifecycle = conversation.tool_lifecycle_journal();

    lifecycle.record_started("call_bash_unknown").unwrap();
    lifecycle.record_started("call_bash_finished").unwrap();
    lifecycle
        .record_finished(
            "call_bash_finished",
            Value::String(r#"{"exit_code":0,"stderr":"","stdout":"done"}"#.to_string()),
            None,
            None,
            false,
        )
        .unwrap();
    lifecycle.record_started("call_edit_not_prepared").unwrap();

    let post_path = cwd.join("post.txt");
    lifecycle.record_started("call_write_post").unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_write_post",
            ToolMutationEvidence {
                target: post_path.clone(),
                target_parent: Some(
                    crate::private_fs::AnchoredPath::open(&post_path)
                        .unwrap()
                        .parent_identity(),
                ),
                path_resolution: None,
                pre_state: ToolTargetPreState::Absent,
                post_state: ToolContentDigest::from_bytes(b"post\n"),
                staging: None,
                missing_parent: None,
            },
        )
        .unwrap();
    std::fs::write(&post_path, "post\n").unwrap();

    let pre_path = cwd.join("pre.txt");
    std::fs::write(&pre_path, "before\n").unwrap();
    lifecycle.record_started("call_edit_pre").unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_edit_pre",
            ToolMutationEvidence {
                target: pre_path.clone(),
                target_parent: Some(
                    crate::private_fs::AnchoredPath::open(&pre_path)
                        .unwrap()
                        .parent_identity(),
                ),
                path_resolution: None,
                pre_state: ToolTargetPreState::Digest(ToolContentDigest::from_bytes(b"before\n")),
                post_state: ToolContentDigest::from_bytes(b"after\n"),
                staging: None,
                missing_parent: None,
            },
        )
        .unwrap();

    let changed_path = cwd.join("changed.txt");
    std::fs::write(&changed_path, "before\n").unwrap();
    lifecycle.record_started("call_write_changed").unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_write_changed",
            ToolMutationEvidence {
                target: changed_path.clone(),
                target_parent: Some(
                    crate::private_fs::AnchoredPath::open(&changed_path)
                        .unwrap()
                        .parent_identity(),
                ),
                path_resolution: None,
                pre_state: ToolTargetPreState::Digest(ToolContentDigest::from_bytes(b"before\n")),
                post_state: ToolContentDigest::from_bytes(b"after\n"),
                staging: None,
                missing_parent: None,
            },
        )
        .unwrap();
    std::fs::write(&changed_path, "third state\n").unwrap();

    let original_parent = cwd.join("original-parent");
    let replacement_parent = cwd.join("replacement-parent");
    std::fs::create_dir_all(&original_parent).unwrap();
    std::fs::create_dir_all(&replacement_parent).unwrap();
    std::fs::write(original_parent.join("target.txt"), "before\n").unwrap();
    std::fs::write(replacement_parent.join("target.txt"), "after\n").unwrap();
    let routed_parent = cwd.join("routed-parent");
    std::os::unix::fs::symlink(&original_parent, &routed_parent).unwrap();
    let rebound_path = routed_parent.join("target.txt");
    lifecycle
        .record_started("call_write_rebound_parent")
        .unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_write_rebound_parent",
            ToolMutationEvidence {
                target: rebound_path.clone(),
                target_parent: Some(
                    crate::private_fs::AnchoredPath::open(&rebound_path)
                        .unwrap()
                        .parent_identity(),
                ),
                path_resolution: None,
                pre_state: ToolTargetPreState::Digest(ToolContentDigest::from_bytes(b"before\n")),
                post_state: ToolContentDigest::from_bytes(b"after\n"),
                staging: None,
                missing_parent: None,
            },
        )
        .unwrap();
    std::fs::remove_file(&routed_parent).unwrap();
    std::os::unix::fs::symlink(&replacement_parent, &routed_parent).unwrap();

    let original_symlink_target = cwd.join("original-symlink-target.txt");
    let replacement_symlink_target = cwd.join("replacement-symlink-target.txt");
    std::fs::write(&original_symlink_target, "after\n").unwrap();
    std::fs::write(&replacement_symlink_target, "after\n").unwrap();
    let requested_link = cwd.join("requested-link.txt");
    std::os::unix::fs::symlink("original-symlink-target.txt", &requested_link).unwrap();
    let link_snapshot =
        crate::private_fs::file_snapshot(&std::fs::symlink_metadata(&requested_link).unwrap());
    lifecycle
        .record_started("call_write_rebound_symlink")
        .unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_write_rebound_symlink",
            ToolMutationEvidence {
                target: original_symlink_target.clone(),
                target_parent: Some(
                    crate::private_fs::AnchoredPath::open(&original_symlink_target)
                        .unwrap()
                        .parent_identity(),
                ),
                path_resolution: Some(ToolPathResolutionEvidence {
                    requested: requested_link.clone(),
                    symlinks: vec![ToolSymlinkEvidence {
                        path: requested_link.clone(),
                        snapshot: link_snapshot,
                    }],
                }),
                pre_state: ToolTargetPreState::Digest(ToolContentDigest::from_bytes(b"before\n")),
                post_state: ToolContentDigest::from_bytes(b"after\n"),
                staging: None,
                missing_parent: None,
            },
        )
        .unwrap();
    std::fs::remove_file(&requested_link).unwrap();
    std::os::unix::fs::symlink("replacement-symlink-target.txt", &requested_link).unwrap();

    let same_target = cwd.join("same-target.txt");
    std::fs::write(&same_target, "after\n").unwrap();
    let same_target_parent = crate::private_fs::AnchoredPath::open(&same_target)
        .unwrap()
        .parent_identity();
    lifecycle.record_started("call_same_target_old").unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_same_target_old",
            ToolMutationEvidence {
                target: same_target.clone(),
                target_parent: Some(same_target_parent),
                path_resolution: None,
                pre_state: ToolTargetPreState::Digest(ToolContentDigest::from_bytes(b"before\n")),
                post_state: ToolContentDigest::from_bytes(b"middle\n"),
                staging: None,
                missing_parent: None,
            },
        )
        .unwrap();
    lifecycle.record_started("call_same_target_new").unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_same_target_new",
            ToolMutationEvidence {
                target: same_target,
                target_parent: Some(same_target_parent),
                path_resolution: None,
                pre_state: ToolTargetPreState::Digest(ToolContentDigest::from_bytes(b"middle\n")),
                post_state: ToolContentDigest::from_bytes(b"after\n"),
                staging: None,
                missing_parent: None,
            },
        )
        .unwrap();

    let missing_parent = cwd.join("created-parent");
    let absent_path = missing_parent.join("nested/absent.txt");
    lifecycle
        .record_started("call_write_parent_residue")
        .unwrap();
    lifecycle
        .record_mutation_prepared(
            "call_write_parent_residue",
            ToolMutationEvidence {
                target: absent_path.clone(),
                target_parent: None,
                path_resolution: None,
                pre_state: ToolTargetPreState::Absent,
                post_state: ToolContentDigest::from_bytes(b"intended\n"),
                staging: None,
                missing_parent: Some(missing_parent),
            },
        )
        .unwrap();
    std::fs::create_dir_all(absent_path.parent().unwrap()).unwrap();
    drop(lifecycle);
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(loaded.tool_recoveries.len(), calls.len());
    let transcript_output = |call_id: &str| {
        loaded.transcript.iter().find_map(|item| match item {
            SessionTranscriptItem::Tool { tool } if tool.call_id == call_id => tool.output.clone(),
            _ => None,
        })
    };
    assert!(matches!(
        transcript_output("call_bash_finished"),
        Some(SessionTranscriptToolOutput::Success(output)) if output["stdout"] == "done"
    ));
    assert!(
        transcript_output("call_write_post")
            .as_ref()
            .and_then(SessionTranscriptToolOutput::recovered_file_state_message)
            .is_some_and(|message| message.contains("exact contents intended"))
    );
    assert!(
        transcript_output("call_edit_pre")
            .as_ref()
            .and_then(SessionTranscriptToolOutput::recovered_file_state_message)
            .is_some_and(|message| message.contains("recorded pre-mutation contents"))
    );

    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let recovered_output = |call_id: &str| {
        resumed.items().iter().find_map(|item| {
            (item["type"] == "function_call_output" && item["call_id"] == call_id)
                .then(|| item["output"].clone())
        })
    };
    assert!(
        recovered_output("call_read")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("no intentional workspace mutation"))
    );
    assert!(
        recovered_output("call_bash_not_started")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("did not start"))
    );
    assert!(
        recovered_output("call_bash_unknown")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("effects are unknown"))
    );
    assert_eq!(
        recovered_output("call_bash_finished"),
        Some(Value::String(
            r#"{"exit_code":0,"stderr":"","stdout":"done"}"#.to_string()
        ))
    );
    assert!(
        recovered_output("call_write_not_started")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("mutation was not attempted"))
    );
    assert!(
        recovered_output("call_edit_not_prepared")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("stopped before its file mutation was prepared"))
    );
    assert!(
        recovered_output("call_write_post")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| {
                output.contains("exact contents intended") && !output.contains("Wrote")
            })
    );
    assert!(
        recovered_output("call_edit_pre")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("recorded pre-mutation contents"))
    );
    assert!(
        recovered_output("call_write_changed")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("outcome is unknown"))
    );
    assert!(
        recovered_output("call_write_rebound_parent")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("outcome is unknown"))
    );
    assert!(
        recovered_output("call_write_rebound_symlink")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("outcome is unknown"))
    );
    assert!(
        recovered_output("call_same_target_old")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("outcome is unknown"))
    );
    assert!(
        recovered_output("call_same_target_new")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("exact contents intended"))
    );
    assert!(
        recovered_output("call_write_parent_residue")
            .and_then(|output| output.as_str().map(str::to_string))
            .is_some_and(|output| output.contains("may remain from parent creation"))
    );
    assert!(resumed.items().iter().any(|item| {
        item.pointer("/content/0/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains(CRASH_GUIDANCE))
    }));
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn.is_none());
    assert!(loaded.tool_recoveries.is_empty());
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_write_post"
                    && tool
                        .output
                        .as_ref()
                        .and_then(SessionTranscriptToolOutput::recovered_file_state_message)
                        .is_some_and(|message| message.contains("exact contents intended"))
        )
    }));
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_write_changed"
                    && tool
                        .output
                        .as_ref()
                        .and_then(SessionTranscriptToolOutput::recovered_file_state_message)
                        .is_some_and(|message| message.contains("outcome is unknown"))
        )
    }));
}

#[tokio::test]
async fn tracked_write_recovers_after_mutation_before_history_projection() {
    let (root, cwd) = temporary_repository("tracked-write-crash-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .start_turn("turn-tracked-write-crashed")
        .unwrap();
    let call_item = json!({
        "type": "function_call",
        "call_id": "call_tracked_write",
        "name": "write",
        "arguments": r#"{"path":"tracked.txt","content":"written\n"}"#,
    });
    let call = ToolCall::from_response_item(&call_item).unwrap();
    conversation.extend([call_item]).unwrap();
    let result = call
        .execute(
            &ToolRuntime::new(cwd.clone()),
            TruncationPolicy::Tokens(10_000),
            None,
            CancellationToken::new(),
            Some(conversation.tool_lifecycle_journal()),
        )
        .await;
    let (ordinary_output, _) = call.into_output_item(result);
    assert!(
        ordinary_output["output"]
            .as_str()
            .is_some_and(|output| output.starts_with("Wrote 8 bytes"))
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
        "written\n"
    );
    drop(conversation);

    // Simulate the deterministic crash boundary after the atomic replacement but before the
    // completion record becomes durable. The preceding prepared record must remain intact.
    let journal_path = rollout_root
        .join("sessions")
        .join(format!("{session_id}.jsonl"));
    let mut journal_bytes = std::fs::read(&journal_path).unwrap();
    assert!(journal_bytes.ends_with(b"\n"));
    journal_bytes.pop();
    let final_record_start = journal_bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    assert!(
        std::str::from_utf8(&journal_bytes[final_record_start..])
            .unwrap()
            .contains(r#""type":"tool_finished""#)
    );
    journal_bytes.truncate(final_record_start);
    std::fs::write(&journal_path, journal_bytes).unwrap();

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(resumed.items().iter().any(|item| {
        item["call_id"] == "call_tracked_write"
            && item["output"].as_str().is_some_and(|output| {
                output.contains("exact contents intended") && !output.contains("Wrote")
            })
    }));
    assert!(resumed.items().iter().any(|item| {
        item.pointer("/content/0/text")
            .and_then(Value::as_str)
            .is_some_and(|text| {
                text.contains(CRASH_NOTICE) && !text.contains("Inspect the workspace")
            })
    }));
}

#[tokio::test]
async fn tool_result_projection_retries_a_rolled_back_journal_append() {
    let (root, cwd) = temporary_repository("tool-result-projection-retry");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let turn_id = "turn-tool-result-projection-retry";
    conversation.start_turn(turn_id).unwrap();
    let call_item = json!({
        "type": "function_call",
        "call_id": "call_tool_result_projection_retry",
        "name": "write",
        "arguments": r#"{"path":"retried.txt","content":"written\n"}"#,
    });
    let call = ToolCall::from_response_item(&call_item).unwrap();
    conversation.extend([call_item]).unwrap();
    let result = call
        .execute(
            &ToolRuntime::new(cwd.clone()),
            TruncationPolicy::Tokens(10_000),
            None,
            CancellationToken::new(),
            Some(conversation.tool_lifecycle_journal()),
        )
        .await;
    let (output, completion) = call.into_output_item(result);
    let outcome = SessionTranscriptToolOutcome {
        call_id: completion.call_id,
        output: completion
            .inspection
            .map(SessionTranscriptToolOutput::recovered_file_state),
        error: completion.error,
        file_change: completion.file_change,
    };

    conversation.rollout.fail_next_append_for_test();
    conversation
        .extend_tool_results(vec![output], vec![outcome])
        .unwrap();
    conversation
        .finish_turn(turn_id, TurnOutcome::Completed)
        .unwrap();
    drop(conversation);

    assert_eq!(
        std::fs::read_to_string(cwd.join("retried.txt")).unwrap(),
        "written\n"
    );
    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn.is_none());
    assert!(loaded.tool_recoveries.is_empty());
    assert_eq!(
        loaded
            .history
            .iter()
            .filter(|item| {
                item["type"] == "function_call_output"
                    && item["call_id"] == "call_tool_result_projection_retry"
                    && item["output"]
                        .as_str()
                        .is_some_and(|output| output.starts_with("Wrote 8 bytes"))
            })
            .count(),
        1
    );
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_tool_result_projection_retry"
                    && tool.file_change.is_some()
                    && matches!(
                        tool.output.as_ref(),
                        Some(SessionTranscriptToolOutput::Success(Value::Null))
                    )
        )
    }));
}

#[tokio::test]
async fn tracked_failed_write_recovers_its_durable_error_and_parent_residue() {
    let (root, cwd) = temporary_repository("tracked-failed-write-crash-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation
        .start_turn("turn-tracked-failed-write-crashed")
        .unwrap();
    let created_parent = cwd.join("created-parent");
    let oversized_name = "x".repeat(256);
    let call_item = json!({
        "type": "function_call",
        "call_id": "call_tracked_failed_write",
        "name": "write",
        "arguments": json!({
            "path": format!("created-parent/{oversized_name}"),
            "content": "never written",
        })
        .to_string(),
    });
    let call = ToolCall::from_response_item(&call_item).unwrap();
    conversation.extend([call_item]).unwrap();
    let result = call
        .execute(
            &ToolRuntime::new(cwd.clone()),
            TruncationPolicy::Tokens(10_000),
            None,
            CancellationToken::new(),
            Some(conversation.tool_lifecycle_journal()),
        )
        .await;
    let (ordinary_output, _) = call.into_output_item(result);
    assert!(created_parent.is_dir());
    assert!(
        ordinary_output["output"]
            .as_str()
            .is_some_and(|output| output.contains("may remain from parent creation"))
    );
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_tracked_failed_write"
                    && matches!(
                        tool.output.as_ref(),
                        Some(SessionTranscriptToolOutput::Error(error))
                            if error.contains("may remain from parent creation")
                    )
        )
    }));
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(resumed.items().iter().any(|item| {
        item["call_id"] == "call_tracked_failed_write"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("may remain from parent creation"))
    }));
}

const PROCESS_TERMINATION_SCENARIO_ENV: &str = "BCODEX_TEST_PROCESS_TERMINATION_SCENARIO";
const PROCESS_TERMINATION_ROOT_ENV: &str = "BCODEX_TEST_PROCESS_TERMINATION_ROOT";

fn process_termination_call(call_id: &str, name: &str, arguments: Value) -> Value {
    json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string(),
    })
}

fn arm_process_termination_fault(root: &Path) {
    std::fs::write(root.join("fault-arm"), b"armed").unwrap();
}

async fn execute_process_termination_call(
    conversation: &mut Conversation,
    cwd: &Path,
    item: Value,
) {
    let call = ToolCall::from_response_item(&item).unwrap();
    conversation.extend([item]).unwrap();
    let result = call
        .execute(
            &ToolRuntime::new(cwd.to_path_buf()),
            TruncationPolicy::Tokens(10_000),
            None,
            CancellationToken::new(),
            Some(conversation.tool_lifecycle_journal()),
        )
        .await;
    let _ = call.into_output_item(result);
}

fn run_process_termination_child(scenario: &str, root: &Path) -> ! {
    let cwd = root.join("repo");
    let rollout_root = root.join("state");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    std::fs::write(
        root.join("session-id"),
        rollout.identity().session_id.as_bytes(),
    )
    .unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    let turn_id = format!("turn-process-termination-{scenario}");
    conversation.start_turn(&turn_id).unwrap();
    if scenario == "turn_started" {
        arm_process_termination_fault(root);
        crate::process_termination_test_support::stop_at("turn_started");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        match scenario {
            "call_persisted" => {
                conversation
                    .extend([process_termination_call(
                        "call_persisted",
                        "write",
                        json!({"path": "persisted.txt", "content": "after\n"}),
                    )])
                    .unwrap();
                arm_process_termination_fault(root);
                crate::process_termination_test_support::stop_at("call_persisted");
            }
            "write_started" => {
                std::fs::write(cwd.join("started.txt"), "before\n").unwrap();
                let call = process_termination_call(
                    "call_write_started",
                    "write",
                    json!({"path": "started.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "large_write_prepared" => {
                std::fs::write(cwd.join("large.txt"), vec![b'b'; 3 * 1024 * 1024]).unwrap();
                let call = process_termination_call(
                    "call_large_write_prepared",
                    "write",
                    json!({"path": "large.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "edit_replaced" => {
                std::fs::write(cwd.join("edit.txt"), "before\n").unwrap();
                let call = process_termination_call(
                    "call_edit_replaced",
                    "edit",
                    json!({
                        "path": "edit.txt",
                        "edits": [{"oldText": "before", "newText": "after"}],
                    }),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "write_finished" => {
                let call = process_termination_call(
                    "call_write_finished",
                    "write",
                    json!({"path": "finished.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "parent_created" => {
                let call = process_termination_call(
                    "call_parent_created",
                    "write",
                    json!({"path": "created/deep/target.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "create_committed" => {
                let call = process_termination_call(
                    "call_create_committed",
                    "write",
                    json!({"path": "created-committed.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "staging_written" => {
                std::fs::write(cwd.join("staged.txt"), "before\n").unwrap();
                let call = process_termination_call(
                    "call_staging_written",
                    "write",
                    json!({"path": "staged.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "staging_replaced" => {
                std::fs::write(cwd.join("staging-replaced.txt"), "before\n").unwrap();
                let call = process_termination_call(
                    "call_staging_replaced",
                    "write",
                    json!({"path": "staging-replaced.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "staging_substituted" => {
                std::fs::write(cwd.join("staging-substituted.txt"), "before\n").unwrap();
                let call = process_termination_call(
                    "call_staging_substituted",
                    "write",
                    json!({"path": "staging-substituted.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "symlink_rebound" => {
                std::fs::write(cwd.join("symlink-first.txt"), "before\n").unwrap();
                std::fs::write(cwd.join("symlink-second.txt"), "second\n").unwrap();
                std::os::unix::fs::symlink("symlink-first.txt", cwd.join("symlink-route.txt"))
                    .unwrap();
                let call = process_termination_call(
                    "call_symlink_rebound",
                    "write",
                    json!({"path": "symlink-route.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "bash_running" => {
                let call = process_termination_call(
                    "call_bash_running",
                    "bash",
                    json!({
                        "command": "printf '%s' \"$$\" > bash-pid; printf ready > bash-ready; exec sleep 30",
                    }),
                );
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "bash_result" => {
                let call = process_termination_call(
                    "call_bash_result",
                    "bash",
                    json!({"command": "printf effect > bash-effect; printf done"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "parallel_read_bash" => {
                std::fs::write(cwd.join("parallel.txt"), "parallel read\n").unwrap();
                let read_item = process_termination_call(
                    "call_parallel_read",
                    "read",
                    json!({"path": "parallel.txt"}),
                );
                let bash_item = process_termination_call(
                    "call_parallel_bash",
                    "bash",
                    json!({
                        "command": "printf '%s' \"$$\" > bash-pid; printf ready > bash-ready; exec sleep 30",
                    }),
                );
                let read_call = ToolCall::from_response_item(&read_item).unwrap();
                let bash_call = ToolCall::from_response_item(&bash_item).unwrap();
                conversation.extend([read_item, bash_item]).unwrap();
                let tools = ToolRuntime::new(cwd.clone());
                let lifecycle = conversation.tool_lifecycle_journal();
                let read = read_call.execute(
                    &tools,
                    TruncationPolicy::Tokens(10_000),
                    None,
                    CancellationToken::new(),
                    Some(lifecycle.clone()),
                );
                let bash = bash_call.execute(
                    &tools,
                    TruncationPolicy::Tokens(10_000),
                    None,
                    CancellationToken::new(),
                    Some(lifecycle),
                );
                let _ = tokio::join!(read, bash);
            }
            "external_change" => {
                std::fs::write(cwd.join("external.txt"), "before\n").unwrap();
                let call = process_termination_call(
                    "call_external_change",
                    "write",
                    json!({"path": "external.txt", "content": "after\n"}),
                );
                arm_process_termination_fault(root);
                execute_process_termination_call(&mut conversation, &cwd, call).await;
            }
            "history_projected" => {
                let item = process_termination_call(
                    "call_history_projected",
                    "bash",
                    json!({"command": "printf projected"}),
                );
                let call = ToolCall::from_response_item(&item).unwrap();
                conversation.extend([item]).unwrap();
                let result = call
                    .execute(
                        &ToolRuntime::new(cwd.clone()),
                        TruncationPolicy::Tokens(10_000),
                        None,
                        CancellationToken::new(),
                        Some(conversation.tool_lifecycle_journal()),
                    )
                    .await;
                let (output, _) = call.into_output_item(result);
                conversation
                    .extend_tool_results(vec![output], Vec::new())
                    .unwrap();
                arm_process_termination_fault(root);
                crate::process_termination_test_support::stop_at("history_projected");
            }
            "turn_finished" => {
                conversation
                    .extend([
                        message("user", "finish durably".to_string()),
                        message("assistant", "finished".to_string()),
                    ])
                    .unwrap();
                conversation
                    .finish_turn(&turn_id, TurnOutcome::Completed)
                    .unwrap();
                arm_process_termination_fault(root);
                crate::process_termination_test_support::stop_at("turn_finished");
            }
            "recovery_checkpoint" => {
                conversation
                    .extend([process_termination_call(
                        "call_recovery_checkpoint",
                        "bash",
                        json!({"command": "printf unknown-effect"}),
                    )])
                    .unwrap();
                conversation
                    .tool_lifecycle_journal()
                    .record_started("call_recovery_checkpoint")
                    .unwrap();
                drop(conversation);
                let loaded = Rollout::resume_in(
                    &rollout_root,
                    ResumeSelector::Id(
                        std::fs::read_to_string(root.join("session-id"))
                            .unwrap()
                            .parse::<Uuid>()
                            .unwrap(),
                    ),
                    &cwd,
                )
                .unwrap();
                arm_process_termination_fault(root);
                let _ = Conversation::resume(&cwd, loaded).unwrap();
            }
            "journal_tail" => {
                arm_process_termination_fault(root);
                conversation
                    .extend([json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": format!("interrupted-tail-{}", "x".repeat(256 * 1024)),
                        }],
                    })])
                    .unwrap();
            }
            other => panic!("unknown process-termination scenario {other}"),
        }
    });
    panic!("process-termination scenario {scenario} passed its required stop point");
}

fn wait_for_process_termination_marker(
    child: &mut std::process::Child,
    marker: &Path,
    scenario: &str,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if marker.exists() {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("process-termination child {scenario} exited early with {status}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "process-termination child {scenario} did not reach {}",
            marker.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn process_is_gone(process_id: libc::pid_t) -> bool {
    for _ in 0..100 {
        if unsafe { libc::kill(process_id, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

fn staging_files(cwd: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(cwd)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".bettercodex-") && name.ends_with(".tmp"))
        })
        .collect()
}

fn process_termination_recovery_text<'a>(loaded: &'a LoadedRollout, call_id: &str) -> &'a str {
    loaded.tool_recoveries[call_id].output.as_str().unwrap()
}

fn verify_process_termination_scenario(scenario: &str, root: &Path) {
    let cwd = root.join("repo");
    let rollout_root = root.join("state");
    let session_id = std::fs::read_to_string(root.join("session-id"))
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    if matches!(
        scenario,
        "create_committed" | "staging_written" | "staging_replaced" | "staging_substituted"
    ) {
        use std::os::unix::fs::PermissionsExt as _;

        let staging = staging_files(&cwd);
        assert_eq!(staging.len(), 1);
        let metadata = std::fs::metadata(&staging[0]).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        assert!(staging[0].join("content").is_file());
    }
    if scenario == "journal_tail" {
        let journal = rollout_root
            .join("sessions")
            .join(format!("{session_id}.jsonl"));
        assert!(!std::fs::read(&journal).unwrap().ends_with(b"\n"));
    }

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    match scenario {
        "turn_started" => {
            assert!(loaded.unfinished_turn.is_some());
            assert!(!loaded.unfinished_turn_has_activity);
            assert!(!loaded.unfinished_turn_recovered);
            assert!(loaded.tool_recoveries.is_empty());
        }
        "history_projected" => {
            assert!(loaded.unfinished_turn.is_some());
            assert!(loaded.unfinished_turn_has_activity);
            assert!(loaded.tool_recoveries.is_empty());
            assert_eq!(
                loaded
                    .history
                    .iter()
                    .filter(|item| {
                        item["type"] == "function_call_output"
                            && item["call_id"] == "call_history_projected"
                    })
                    .count(),
                1
            );
        }
        "turn_finished" => {
            assert!(loaded.unfinished_turn.is_none());
            assert!(loaded.tool_recoveries.is_empty());
            assert!(
                loaded
                    .history
                    .iter()
                    .all(|item| !is_turn_abort_notice(item))
            );
        }
        "recovery_checkpoint" => {
            assert!(loaded.unfinished_turn.is_some());
            assert!(loaded.unfinished_turn_recovered);
            assert!(loaded.crash_recovery_requires_inspection);
            assert!(loaded.tool_recoveries.is_empty());
            assert_eq!(
                loaded
                    .history
                    .iter()
                    .filter(|item| is_turn_abort_notice(item))
                    .count(),
                1
            );
            assert!(loaded.history.iter().any(|item| {
                item["call_id"] == "call_recovery_checkpoint"
                    && item["output"]
                        .as_str()
                        .is_some_and(|output| output.contains("effects are unknown"))
            }));
        }
        "call_persisted" => {
            assert!(
                process_termination_recovery_text(&loaded, "call_persisted")
                    .contains("registered but did not start")
            );
            assert!(!cwd.join("persisted.txt").exists());
        }
        "write_started" => {
            assert!(
                process_termination_recovery_text(&loaded, "call_write_started")
                    .contains("stopped before its file mutation was prepared")
            );
            assert_eq!(
                std::fs::read_to_string(cwd.join("started.txt")).unwrap(),
                "before\n"
            );
        }
        "large_write_prepared" => {
            assert!(
                process_termination_recovery_text(&loaded, "call_large_write_prepared")
                    .contains("exact recorded pre-mutation contents")
            );
            assert_eq!(
                std::fs::metadata(cwd.join("large.txt")).unwrap().len(),
                3 * 1024 * 1024
            );
        }
        "edit_replaced" => {
            let recovery = process_termination_recovery_text(&loaded, "call_edit_replaced");
            assert!(recovery.contains("exact contents intended"), "{recovery}");
            assert_eq!(
                std::fs::read_to_string(cwd.join("edit.txt")).unwrap(),
                "after\n"
            );
        }
        "write_finished" => {
            assert!(
                process_termination_recovery_text(&loaded, "call_write_finished")
                    .starts_with("Wrote 6 bytes")
            );
            assert!(loaded.transcript.iter().any(|item| {
                matches!(
                    item,
                    SessionTranscriptItem::Tool { tool }
                        if tool.call_id == "call_write_finished" && tool.file_change.is_some()
                )
            }));
            assert_eq!(
                std::fs::read_to_string(cwd.join("finished.txt")).unwrap(),
                "after\n"
            );
        }
        "parent_created" => {
            let recovery = process_termination_recovery_text(&loaded, "call_parent_created");
            assert!(recovery.contains("is absent now"));
            assert!(recovery.contains("may remain from parent creation"));
            assert!(cwd.join("created/deep").is_dir());
            assert!(!cwd.join("created/deep/target.txt").exists());
        }
        "create_committed" => {
            let recovery = process_termination_recovery_text(&loaded, "call_create_committed");
            assert!(recovery.contains("exact contents intended"), "{recovery}");
            assert!(!recovery.contains("private staging directory"));
            assert!(!loaded.crash_recovery_requires_inspection);
            assert_eq!(
                std::fs::read_to_string(cwd.join("created-committed.txt")).unwrap(),
                "after\n"
            );
            assert!(staging_files(&cwd).is_empty());
        }
        "staging_written" => {
            assert!(
                process_termination_recovery_text(&loaded, "call_staging_written")
                    .contains("exact recorded pre-mutation contents")
            );
            assert_eq!(
                std::fs::read_to_string(cwd.join("staged.txt")).unwrap(),
                "before\n"
            );
            let staging = staging_files(&cwd);
            assert!(
                staging.is_empty(),
                "{}; staging residue: {staging:?}",
                process_termination_recovery_text(&loaded, "call_staging_written")
            );
        }
        "staging_replaced" => {
            let recovery = process_termination_recovery_text(&loaded, "call_staging_replaced");
            assert!(recovery.contains("exact recorded pre-mutation contents"));
            assert!(!recovery.contains("private staging directory"));
            assert!(!loaded.crash_recovery_requires_inspection);
            assert_eq!(
                std::fs::read_to_string(cwd.join("staging-replaced.txt")).unwrap(),
                "before\n"
            );
            assert!(staging_files(&cwd).is_empty());
        }
        "staging_substituted" => {
            let recovery = process_termination_recovery_text(&loaded, "call_staging_substituted");
            assert!(recovery.contains("exact recorded pre-mutation contents"));
            assert!(recovery.contains("private staging directory"));
            assert!(recovery.contains("may remain"));
            assert!(loaded.crash_recovery_requires_inspection);
            assert_eq!(
                std::fs::read_to_string(cwd.join("staging-substituted.txt")).unwrap(),
                "before\n"
            );
            let staging = staging_files(&cwd);
            assert_eq!(staging.len(), 1);
            assert_eq!(
                std::fs::read_to_string(staging[0].join("content")).unwrap(),
                "external staging\n"
            );
        }
        "symlink_rebound" => {
            let recovery = process_termination_recovery_text(&loaded, "call_symlink_rebound");
            assert!(recovery.contains("could not be reconciled"), "{recovery}");
            assert!(recovery.contains("symlink-route.txt"), "{recovery}");
            assert!(loaded.crash_recovery_requires_inspection);
            assert_eq!(
                std::fs::read_to_string(cwd.join("symlink-first.txt")).unwrap(),
                "after\n"
            );
            assert_eq!(
                std::fs::read_to_string(cwd.join("symlink-second.txt")).unwrap(),
                "second\n"
            );
        }
        "bash_running" => {
            assert!(
                process_termination_recovery_text(&loaded, "call_bash_running")
                    .contains("effects are unknown")
            );
        }
        "bash_result" => {
            assert!(
                process_termination_recovery_text(&loaded, "call_bash_result")
                    .contains("effects are unknown")
            );
            assert_eq!(
                std::fs::read_to_string(cwd.join("bash-effect")).unwrap(),
                "effect"
            );
        }
        "parallel_read_bash" => {
            assert!(
                process_termination_recovery_text(&loaded, "call_parallel_read")
                    .contains("no intentional workspace mutation")
            );
            assert!(
                process_termination_recovery_text(&loaded, "call_parallel_bash")
                    .contains("effects are unknown")
            );
        }
        "external_change" => {
            assert!(
                process_termination_recovery_text(&loaded, "call_external_change")
                    .contains("outcome is unknown")
            );
            assert_eq!(
                std::fs::read_to_string(cwd.join("external.txt")).unwrap(),
                "external\n"
            );
        }
        "journal_tail" => {
            assert!(
                loaded
                    .history
                    .iter()
                    .all(|item| !item.to_string().contains("interrupted-tail-"))
            );
        }
        other => panic!("unknown process-termination scenario {other}"),
    }

    let mut resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(history_is_normalized(resumed.items()));
    match scenario {
        "turn_started" | "turn_finished" => {
            assert!(
                resumed
                    .items()
                    .iter()
                    .all(|item| !is_turn_abort_notice(item))
            );
        }
        "history_projected" => {
            assert_eq!(
                resumed
                    .items()
                    .iter()
                    .filter(|item| is_turn_abort_notice(item))
                    .count(),
                1
            );
            assert_eq!(
                resumed
                    .items()
                    .iter()
                    .filter(|item| {
                        item["type"] == "function_call_output"
                            && item["call_id"] == "call_history_projected"
                    })
                    .count(),
                1
            );
        }
        "recovery_checkpoint" => {
            assert_eq!(
                resumed
                    .items()
                    .iter()
                    .filter(|item| is_turn_abort_notice(item))
                    .count(),
                1
            );
            assert!(resumed.items().iter().any(|item| {
                item.pointer("/content/0/text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(CRASH_GUIDANCE))
            }));
        }
        _ => {}
    }
    if scenario == "edit_replaced" {
        resumed.start_turn("turn-after-recovery").unwrap();
        resumed
            .extend([
                message("user", "continue after recovery".to_string()),
                message("assistant", "continued".to_string()),
            ])
            .unwrap();
        resumed
            .finish_turn("turn-after-recovery", TurnOutcome::Completed)
            .unwrap();
        resumed
            .replace_compacted(
                vec![json!({
                    "type": "compaction",
                    "id": "cmp-after-process-termination",
                    "encrypted_content": "opaque",
                })],
                InitialContextInjection::AfterCompaction,
                &ActiveTurnContext::default(),
                None,
                &[],
            )
            .unwrap();
    }
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.unfinished_turn.is_none());
    assert!(loaded.tool_recoveries.is_empty());
    assert!(history_is_normalized(&loaded.history));
    if scenario == "write_finished" {
        assert!(loaded.transcript.iter().any(|item| {
            matches!(
                item,
                SessionTranscriptItem::Tool { tool }
                    if tool.call_id == "call_write_finished" && tool.file_change.is_some()
            )
        }));
    }
    match scenario {
        "turn_started" | "turn_finished" => {
            assert!(
                loaded
                    .history
                    .iter()
                    .all(|item| !is_turn_abort_notice(item))
            );
        }
        "history_projected" | "recovery_checkpoint" => {
            assert_eq!(
                loaded
                    .history
                    .iter()
                    .filter(|item| is_turn_abort_notice(item))
                    .count(),
                1
            );
        }
        _ => {}
    }
    if scenario == "edit_replaced" {
        assert_eq!(loaded.compaction_count, 1);
    }
}

#[test]
fn real_process_termination_fault_matrix() {
    if let Ok(scenario) = std::env::var(PROCESS_TERMINATION_SCENARIO_ENV) {
        let root = PathBuf::from(std::env::var_os(PROCESS_TERMINATION_ROOT_ENV).unwrap());
        run_process_termination_child(&scenario, &root);
    }

    let test_name = std::thread::current().name().unwrap().to_string();
    let scenarios = [
        ("turn_started", Some("turn_started")),
        ("call_persisted", Some("call_persisted")),
        ("write_started", Some("write_started")),
        ("large_write_prepared", Some("write_prepared")),
        ("edit_replaced", Some("edit_replaced")),
        ("write_finished", Some("tool_finished")),
        ("parent_created", Some("write_parent_created")),
        ("create_committed", Some("atomic_replacement_committed")),
        ("staging_written", Some("atomic_temporary_written")),
        ("staging_replaced", Some("atomic_temporary_written")),
        ("staging_substituted", Some("atomic_temporary_written")),
        ("symlink_rebound", Some("write_replaced")),
        ("bash_running", None),
        ("bash_result", Some("tool_result_before_finish")),
        ("parallel_read_bash", None),
        ("external_change", Some("write_prepared")),
        ("history_projected", Some("history_projected")),
        ("turn_finished", Some("turn_finished")),
        ("recovery_checkpoint", Some("turn_recovery_checkpoint")),
        (
            "journal_tail",
            Some("journal_record_encoded_before_newline"),
        ),
    ];

    for (scenario, stop_at) in scenarios {
        let root = TemporaryDirectory::new(&format!("process-termination-{scenario}"));
        let marker = root.join("fault-marker");
        let arm_file = root.join("fault-arm");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg(&test_name)
            .arg("--nocapture")
            .env(PROCESS_TERMINATION_SCENARIO_ENV, scenario)
            .env(PROCESS_TERMINATION_ROOT_ENV, &root.0)
            .env(
                crate::process_termination_test_support::ARM_FILE_ENV,
                &arm_file,
            )
            .env(
                crate::process_termination_test_support::MARKER_FILE_ENV,
                &marker,
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());
        if let Some(stop_at) = stop_at {
            command.env(
                crate::process_termination_test_support::STOP_AT_ENV,
                stop_at,
            );
        } else {
            command.env_remove(crate::process_termination_test_support::STOP_AT_ENV);
        }
        let mut child = command.spawn().unwrap();
        let ready = if matches!(scenario, "bash_running" | "parallel_read_bash") {
            root.join("repo/bash-ready")
        } else {
            marker.clone()
        };
        wait_for_process_termination_marker(&mut child, &ready, scenario);
        let bash_pid = matches!(scenario, "bash_running" | "parallel_read_bash").then(|| {
            std::fs::read_to_string(root.join("repo/bash-pid"))
                .unwrap()
                .parse::<libc::pid_t>()
                .unwrap()
        });
        if scenario == "external_change" {
            std::fs::write(root.join("repo/external.txt"), "external\n").unwrap();
        }
        if scenario == "staging_replaced" {
            let staging = staging_files(&root.join("repo"));
            assert_eq!(staging.len(), 1);
            std::fs::write(staging[0].join("content"), "external staging\n").unwrap();
        }
        if scenario == "staging_substituted" {
            let staging = staging_files(&root.join("repo"));
            assert_eq!(staging.len(), 1);
            let content = staging[0].join("content");
            std::fs::remove_file(&content).unwrap();
            std::fs::write(content, "external staging\n").unwrap();
        }
        if scenario == "symlink_rebound" {
            let link = root.join("repo/symlink-route.txt");
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink("symlink-second.txt", link).unwrap();
        }

        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) },
            0
        );
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        if let Some(bash_pid) = bash_pid {
            let gone = process_is_gone(bash_pid);
            #[cfg(target_os = "linux")]
            assert!(gone, "Bash process {bash_pid} survived parent SIGKILL");
            #[cfg(target_os = "macos")]
            if !gone {
                unsafe {
                    libc::killpg(bash_pid, libc::SIGKILL);
                }
            }
        }
        verify_process_termination_scenario(scenario, &root);
    }
}

#[test]
fn resume_repairs_an_unfinished_legacy_custom_tool_call() {
    let (root, cwd) = temporary_repository("legacy-custom-crash-resume");
    let rollout_root = root.join("state");
    let rollout = Rollout::create_in(&rollout_root, &cwd).unwrap();
    let session_id = rollout.identity().session_id.parse::<Uuid>().unwrap();
    let mut conversation = Conversation::new(&cwd, rollout).unwrap();
    conversation.start_turn("turn-legacy-crashed").unwrap();
    conversation
        .extend([
            json!({
                "id": "ctc_crashed",
                "type": "custom_tool_call",
                "call_id": "call_legacy_crashed",
                "name": "exec",
                "input": "notify('working'); text(true)",
            }),
            json!({
                "type": "custom_tool_call_output",
                "call_id": "call_legacy_crashed",
                "name": "exec",
                "output": "working",
            }),
        ])
        .unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_legacy_crashed"
                    && matches!(
                        tool.output.as_ref(),
                        Some(SessionTranscriptToolOutput::Error(error))
                            if error.contains("no lifecycle records")
                    )
        )
    }));
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert_eq!(
        resumed
            .items()
            .iter()
            .filter(|item| {
                item["type"] == "custom_tool_call_output"
                    && item["call_id"] == "call_legacy_crashed"
            })
            .count(),
        2
    );
    let notification_index = resumed
        .items()
        .iter()
        .position(is_legacy_exec_notification)
        .unwrap();
    let recovery_index = resumed
        .items()
        .iter()
        .position(|item| {
            item["type"] == "custom_tool_call_output"
                && item["call_id"] == "call_legacy_crashed"
                && item.get("name").is_none()
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("no lifecycle records"))
        })
        .unwrap();
    assert!(notification_index < recovery_index);
    assert!(resumed.items().iter().any(|item| {
        item.pointer("/content/0/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains(CRASH_GUIDANCE))
    }));
}
