use super::*;
use crate::compaction::InitialContextInjection;
use crate::input::UserInput;
use crate::rollout::ResumeSelector;
use crate::rollout::SessionTranscriptItem;
use crate::rollout::SessionTranscriptToolOutput;

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

    conversation.extend([call, output.clone()]).unwrap();
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
fn original_image_estimate_reads_uncapped_gpt_5_6_dimensions_from_base64_stream() {
    use base64::Engine as _;

    let mut png = vec![0_u8; 4096];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[16..20].copy_from_slice(&6400_u32.to_be_bytes());
    png[20..24].copy_from_slice(&6400_u32.to_be_bytes());
    let image_url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    );

    assert_eq!(estimate_image_tokens(&image_url), 200 * 200);

    let estimate_for_detail = |detail: Option<&str>| {
        let mut item = json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_image",
                "image_url": &image_url,
            }],
        });
        if let Some(detail) = detail {
            item["content"][0]["detail"] = Value::String(detail.to_string());
        }
        estimate_value_tokens(&item)
    };
    let high = estimate_for_detail(Some("high"));
    for original_equivalent in [None, Some("auto"), Some("original")] {
        assert!(estimate_for_detail(original_equivalent) > high * 10);
    }
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
fn resume_prepares_legacy_images_without_rewriting_the_rollout() {
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
        .extend([json!({
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
        })])
        .unwrap();
    drop(conversation);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(
        first_image_url(&loaded.history),
        Some(original_url.as_str())
    );
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    let prepared_dimensions = resumed
        .items()
        .iter()
        .filter_map(|item| item["content"].as_array())
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
        [(1600, 1600), (2048, 2048), (2048, 2048)]
    );
    drop(resumed);

    let loaded = Rollout::resume_in(&rollout_root, ResumeSelector::Id(session_id), &cwd).unwrap();
    assert_eq!(
        first_image_url(&loaded.history),
        Some(original_url.as_str())
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
            Vec::new(),
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
                Vec::new(),
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
fn mid_turn_compaction_keeps_the_opaque_summary_last() {
    let (root, cwd) = temporary_repository("mid-turn-compaction");
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
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
    active_turn_context.record_input(vec![active_skill.clone()]);

    conversation
        .replace_compacted(
            vec![current_user.clone(), summary.clone()],
            InitialContextInjection::BeforeLastUserMessage,
            &active_turn_context,
            None,
            Vec::new(),
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
                        Some(SessionTranscriptToolOutput::Error(error)) if error == "aborted"
                    )
        )
    }));
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
    assert!(loaded.transcript.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call_crashed"
                    && matches!(
                        tool.output.as_ref(),
                        Some(SessionTranscriptToolOutput::Error(error)) if error == "aborted"
                    )
        )
    }));
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
        .extend([json!({
            "id": "ctc_crashed",
            "type": "custom_tool_call",
            "call_id": "call_legacy_crashed",
            "name": "exec",
            "input": "text(true)",
        })])
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
                        Some(SessionTranscriptToolOutput::Error(error)) if error == "aborted"
                    )
        )
    }));
    let resumed = Conversation::resume(&cwd, loaded).unwrap();
    assert!(resumed.items().iter().any(|item| {
        item["type"] == "custom_tool_call_output"
            && item["call_id"] == "call_legacy_crashed"
            && item["output"] == "aborted"
    }));
}
