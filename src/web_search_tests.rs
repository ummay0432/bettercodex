use super::*;
use serde_json::json;

fn message(role: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{
            "type": if role == "assistant" { "output_text" } else { "input_text" },
            "text": text,
        }],
    })
}

#[test]
fn command_schema_matches_the_codex_alpha_search_surface() {
    let properties = input_schema()["properties"].as_object().unwrap();
    assert_eq!(
        properties.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "click",
            "finance",
            "find",
            "image_query",
            "open",
            "response_length",
            "screenshot",
            "search_query",
            "sports",
            "time",
            "weather",
        ]
    );
    assert_eq!(
        properties["response_length"]["enum"],
        json!(["short", "medium", "long"])
    );
    assert_eq!(
        properties["sports"]["items"]["properties"]["league"]["enum"],
        json!([
            "nba", "wnba", "nfl", "nhl", "mlb", "epl", "ncaamb", "ncaawb", "ipl"
        ])
    );
    assert_eq!(DESCRIPTION.len(), 7_507);
}

#[test]
fn recent_input_keeps_two_operator_turns_and_drops_images_and_world_state() {
    let history = vec![
        message(
            "developer",
            "<environment_context>ignored</environment_context>",
        ),
        message(
            "user",
            "# Repository onboarding from AGENTS.md for /repo\nignored\n# End repository onboarding",
        ),
        message("user", "old user"),
        message("assistant", "old assistant"),
        json!({
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "previous user"},
                {"type": "input_image", "image_url": "data:image/png;base64,AA=="},
            ],
        }),
        json!({
            "id": "msg_previous",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "previous assistant"}],
        }),
        message("user", "current user"),
        message("assistant", "not sent after the current user"),
    ];

    assert_eq!(
        serde_json::to_value(recent_input(&history)).unwrap(),
        json!([
            message("user", "previous user"),
            message("assistant", "previous assistant"),
            message("user", "current user"),
        ])
    );
}

#[test]
fn command_parser_preserves_search_and_fetch_operations() {
    let commands = parse_commands(Some(json!({
        "search_query": [{"q": "Codex", "domains": ["openai.com"]}],
        "open": [{"ref_id": "turn0search0", "lineno": 12}],
        "click": [{"ref_id": "turn0fetch0", "id": 3}],
        "find": [{"ref_id": "turn0fetch0", "pattern": "Responses"}],
    })))
    .unwrap();

    assert_eq!(
        serde_json::to_value(commands).unwrap(),
        json!({
            "search_query": [{"q": "Codex", "domains": ["openai.com"]}],
            "open": [{"ref_id": "turn0search0", "lineno": 12}],
            "click": [{"ref_id": "turn0fetch0", "id": 3}],
            "find": [{"ref_id": "turn0fetch0", "pattern": "Responses"}],
        })
    );
    assert!(parse_commands(Some(json!([]))).is_err());
}

#[test]
fn display_actions_match_codex_web_search_activity() {
    assert_eq!(
        action_for_display(Some(&json!({
            "search_query": [{"q": "first"}, {"q": "second"}],
        }))),
        WebSearchAction::Search {
            query: None,
            queries: Some(vec!["first".to_string(), "second".to_string()]),
        }
    );
    assert_eq!(
        action_for_display(Some(&json!({
            "open": [{"ref_id": "https://openai.com/research"}],
        }))),
        WebSearchAction::OpenPage {
            url: Some("https://openai.com/research".to_string()),
        }
    );
    assert_eq!(
        action_for_display(Some(&json!({
            "find": [{"ref_id": "turn0fetch0", "pattern": "Responses"}],
        }))),
        WebSearchAction::FindInPage {
            url: None,
            pattern: Some("Responses".to_string()),
        }
    );
}
