use super::*;
use serde_json::json;

#[test]
fn bundled_catalogue_preserves_picker_models_and_supported_efforts() {
    let models = bundled_models();
    assert_eq!(
        models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        [
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.2",
        ]
    );
    assert!(models[0].is_default);
    assert!(models[..4].iter().all(|model| model.supports_fast));
    assert!(!models[4].supports_fast);
    assert!(
        models[..4]
            .iter()
            .all(|model| model.supports_image_detail_original)
    );
    assert!(!models[4].supports_image_detail_original);
    assert!(models[..3].iter().all(|model| model.use_responses_lite));
    assert!(models[3..].iter().all(|model| !model.use_responses_lite));
    assert!(
        models
            .iter()
            .all(|model| model.supports_parallel_tool_calls)
    );
    assert!(
        models[..3]
            .iter()
            .all(|model| model.tool_mode == ToolMode::CodeModeOnly)
    );
    assert!(
        models[3..]
            .iter()
            .all(|model| model.tool_mode == ToolMode::Direct)
    );
    assert!(
        models[..4]
            .iter()
            .all(|model| model.truncation_policy == TruncationPolicy::Tokens(10_000))
    );
    assert_eq!(models[4].truncation_policy, TruncationPolicy::Bytes(10_000));
    assert_eq!(
        models[0]
            .supported_reasoning_efforts
            .iter()
            .map(|preset| preset.effort.as_str())
            .collect::<Vec<_>>(),
        ["low", "medium", "high", "xhigh", "max"]
    );
}

#[test]
fn remote_catalogue_filters_sorts_and_derives_transport_capabilities() -> Result<()> {
    let body = serde_json::to_vec(&json!({
        "models": [
            {
                "slug": "hidden",
                "visibility": "hide",
                "priority": 0
            },
            {
                "slug": "later",
                "description": "Later model",
                "default_reasoning_level": "high",
                "supported_reasoning_levels": [
                    {"effort": "high", "description": "Think hard"}
                ],
                "context_window": 100000,
                "tool_mode": "future_mode",
                "visibility": "list",
                "priority": 20
            },
            {
                "slug": "first",
                "description": "First model",
                "default_reasoning_level": "medium",
                "supported_reasoning_levels": [
                    {"effort": "medium", "description": "Balanced"},
                    {"effort": "ultra", "description": "Automatic task delegation"},
                    {"effort": "max", "description": "Deep reasoning"}
                ],
                "context_window": 200000,
                "effective_context_window_percent": 80,
                "auto_compact_token_limit": 170000,
                "comp_hash": "compatible-v2",
                "use_responses_lite": true,
                "supports_parallel_tool_calls": true,
                "truncation_policy": {"mode": "tokens", "limit": 12345},
                "supports_image_detail_original": true,
                "tool_mode": "code_mode_only",
                "prefer_websockets": false,
                "service_tiers": [{"id": "priority"}],
                "visibility": "list",
                "priority": 10
            }
        ]
    }))?;

    let models = parse_models_response(body)?;
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model, "first");
    assert!(models[0].is_default);
    assert!(models[0].use_responses_lite);
    assert!(models[0].supports_parallel_tool_calls);
    assert!(models[0].supports_image_detail_original);
    assert!(!models[0].prefer_websocket);
    assert!(models[0].supports_fast);
    assert_eq!(models[0].tool_mode, ToolMode::CodeModeOnly);
    assert_eq!(
        models[0].truncation_policy,
        TruncationPolicy::Tokens(12_345)
    );
    assert_eq!(models[0].comp_hash.as_deref(), Some("compatible-v2"));
    assert_eq!(models[0].raw_context_window, 200_000);
    let selection = models[0].selection(ReasoningEffort::Medium);
    assert_eq!(selection.effective_context_window(), 160_000);
    assert_eq!(selection.auto_compact_token_limit(), 160_000);
    assert_eq!(
        models[0]
            .supported_reasoning_efforts
            .iter()
            .map(|preset| preset.effort.as_str())
            .collect::<Vec<_>>(),
        ["medium", "max"]
    );
    assert_eq!(
        models[0]
            .supported_reasoning_efforts
            .iter()
            .map(|preset| preset.description.as_str())
            .collect::<Vec<_>>(),
        ["Balanced", "Deep reasoning"]
    );
    assert_eq!(models[1].model, "later");
    assert!(!models[1].is_default);
    assert_eq!(models[1].tool_mode, ToolMode::Direct);
    assert_eq!(models[1].truncation_policy, TruncationPolicy::Bytes(10_000));
    Ok(())
}

#[test]
fn selection_round_trips_custom_remote_metadata() -> Result<()> {
    let selection = ModelSelection {
        model: "remote-model".to_string(),
        reasoning_effort: ReasoningEffort::Custom("future".to_string()),
        raw_context_window: 456_000,
        effective_context_window_percent: 80,
        configured_auto_compact_token_limit: Some(350_000),
        use_responses_lite: true,
        supports_parallel_tool_calls: true,
        truncation_policy: Some(TruncationPolicy::Bytes(12_345)),
        supports_image_detail_original: Some(true),
        tool_mode: Some(ToolMode::CodeMode),
        tool_mode_selector_version: TOOL_MODE_SELECTOR_VERSION,
        prefer_websocket: false,
        supports_fast: true,
        comp_hash: Some("remote-v3".to_string()),
    };
    let encoded = serde_json::to_vec(&selection)?;
    let decoded: ModelSelection = serde_json::from_slice(&encoded)?;
    assert_eq!(decoded, selection);
    assert_eq!(decoded.effective_context_window(), 364_800);
    assert_eq!(decoded.auto_compact_token_limit(), 350_000);
    Ok(())
}

#[test]
fn legacy_collapsed_sol_selector_migrates_to_code_mode_only() -> Result<()> {
    let mut selection: ModelSelection = serde_json::from_value(json!({
        "model": "gpt-5.6-sol",
        "reasoning_effort": "max",
        "raw_context_window": 272000,
        "effective_context_window_percent": 95,
        "use_responses_lite": true,
        "supports_parallel_tool_calls": false,
        "tool_mode": "code_mode",
        "prefer_websocket": true,
        "supports_fast": true
    }))?;

    assert_eq!(selection.tool_mode(), ToolMode::CodeModeOnly);
    assert_eq!(
        selection.truncation_policy(),
        TruncationPolicy::Tokens(10_000)
    );
    selection.migrate_legacy_tool_mode_selector();
    assert_eq!(selection.tool_mode, Some(ToolMode::CodeModeOnly));
    assert_eq!(
        selection.truncation_policy,
        Some(TruncationPolicy::Tokens(10_000))
    );
    assert_eq!(selection.supports_image_detail_original, Some(true));
    assert!(selection.supports_parallel_tool_calls);
    assert_eq!(
        selection.tool_mode_selector_version,
        TOOL_MODE_SELECTOR_VERSION
    );

    let mut legacy_gpt_5_2: ModelSelection = serde_json::from_value(json!({
        "model": "gpt-5.2",
        "reasoning_effort": "medium"
    }))?;
    assert_eq!(
        legacy_gpt_5_2.truncation_policy(),
        TruncationPolicy::Bytes(10_000)
    );
    assert!(!legacy_gpt_5_2.supports_image_detail_original());
    assert!(!legacy_gpt_5_2.supports_parallel_tool_calls);
    legacy_gpt_5_2.migrate_legacy_tool_mode_selector();
    assert!(legacy_gpt_5_2.supports_parallel_tool_calls);
    assert_eq!(legacy_gpt_5_2.tool_mode(), ToolMode::Direct);
    Ok(())
}

#[test]
fn catalogue_rejects_display_control_characters() {
    assert!(bounded_catalogue_text("unsafe\nrow".to_string()).is_err());
    assert!(
        ModelSelection::from_identity("unsafe\nmodel", ReasoningEffort::Medium,)
            .validate()
            .is_err()
    );
    assert!(
        ModelSelection::from_identity(
            "safe-model",
            ReasoningEffort::Custom("unsafe\neffort".to_string()),
        )
        .validate()
        .is_err()
    );
}

#[test]
fn remote_catalogue_accepts_null_context_and_clamps_compaction_to_ninety_percent() -> Result<()> {
    let body = serde_json::to_vec(&json!({
        "models": [{
            "slug": "future",
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [{"effort": "medium"}],
            "context_window": null,
            "max_context_window": 100000,
            "auto_compact_token_limit": 95000,
            "visibility": "list",
            "priority": 1
        }]
    }))?;

    let models = parse_models_response(body)?;
    let selection = models[0].selection(ReasoningEffort::Medium);
    assert_eq!(selection.raw_context_window, 100_000);
    assert_eq!(selection.effective_context_window(), 95_000);
    assert_eq!(selection.auto_compact_token_limit(), 90_000);
    Ok(())
}
