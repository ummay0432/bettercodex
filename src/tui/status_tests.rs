use super::status::StatusSnapshot;
use super::terminal_hyperlinks::HyperlinkLine;
use super::view::Action;
use super::view::View;
use super::width::line_width;
use crate::auth::ChatGptAccount;
use crate::context::ContextSnapshot;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use crate::model::ModelSelection;
use crate::rate_limits::CreditsSnapshot;
use crate::rate_limits::RateLimitSnapshot;
use crate::rate_limits::RateLimitWindow;
use crate::usage::TokenUsage;
use chrono::Utc;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use std::path::PathBuf;

fn visible(line: &HyperlinkLine) -> String {
    line.line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn snapshot() -> StatusSnapshot {
    let now = Utc::now().timestamp();
    StatusSnapshot {
        model: ModelSelection::default(),
        directory: PathBuf::from("/home/tester/work/company/very-long-project-name"),
        instruction_source_paths: vec![
            PathBuf::from("/home/tester/AGENTS.md"),
            PathBuf::from("/home/tester/work/company/very-long-project-name/AGENTS.md"),
        ],
        session_id: "0198a8f0-0000-7000-8000-000000000000".to_string(),
        forked_from: Some("0198a8e0-0000-7000-8000-000000000000".to_string()),
        account: ChatGptAccount {
            email: Some("user@example.com".to_string()),
            plan: Some("Pro".to_string()),
        },
        context: ContextSnapshot {
            used_tokens: 36_000,
            context_window: EFFECTIVE_CONTEXT_WINDOW,
            compact_at_tokens: 244_800,
            measured: true,
            sections: Vec::new(),
            total_usage: TokenUsage {
                input_tokens: 125_000,
                cached_input_tokens: 100_000,
                output_tokens: 4_500,
                total_tokens: 129_500,
                ..TokenUsage::default()
            },
            rate_limits: Vec::new(),
        },
        rate_limits: vec![RateLimitSnapshot {
            limit_id: "codex".to_string(),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 17.0,
                window_minutes: Some(300),
                resets_at: Some(now + 3_600),
            }),
            secondary: Some(RateLimitWindow {
                used_percent: 42.0,
                window_minutes: Some(7 * 24 * 60),
                resets_at: Some(now + 7 * 24 * 3_600),
            }),
            credits: Some(CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance: Some("37.4".to_string()),
            }),
            captured_at: now,
        }],
        refreshing_rate_limits: true,
    }
}

#[test]
fn status_card_matches_codex_information_architecture() {
    let lines = snapshot().display_lines(100);
    let rendered = lines.iter().map(visible).collect::<Vec<_>>().join("\n");

    assert_eq!(visible(&lines[0]), "/status");
    assert!(rendered.contains(&format!(">_ bettercodex (v{})", env!("CARGO_PKG_VERSION"))));
    assert!(rendered.contains("https://chatgpt.com/codex/settings/usage"));
    assert!(rendered.contains("Model:"));
    assert!(rendered.contains("Permissions:"));
    assert!(rendered.contains("Full Access"));
    assert!(rendered.contains("Agents.md:"));
    assert!(rendered.contains("Account:"));
    assert!(rendered.contains("user@example.com (Pro)"));
    assert!(rendered.contains("Collaboration mode:"));
    assert!(rendered.contains("Default"));
    assert!(rendered.contains("Session:"));
    assert!(rendered.contains("Forked from:"));
    assert!(!rendered.contains("Token usage:"));
    assert!(rendered.contains("Context window:"));
    assert!(rendered.contains("272K"));
    assert!(rendered.contains("5h limit:"));
    assert!(rendered.contains("Weekly limit:"));
    assert!(rendered.contains("Credits:"));
    assert!(rendered.contains("37 credits"));
    assert!(lines.iter().any(|line| {
        line.hyperlinks
            .iter()
            .any(|link| link.destination == "https://chatgpt.com/codex/settings/usage")
    }));
}

#[test]
fn subscriber_status_before_first_response_omits_local_usage_estimates() {
    let mut snapshot = snapshot();
    snapshot.context.measured = false;
    snapshot.context.total_usage = TokenUsage::default();
    let rendered = snapshot
        .display_lines(100)
        .iter()
        .map(visible)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!rendered.contains("Token usage:"));
    assert!(!rendered.contains("Context window:"));
    assert!(rendered.contains("Weekly limit:"));
}

#[test]
fn status_card_preserves_reset_details_and_width_on_narrow_terminals() {
    let width = 52;
    let lines = snapshot().display_lines(width);
    let rendered = lines.iter().map(visible).collect::<Vec<_>>().join("\n");

    assert!(rendered.contains("(resets"));
    assert!(rendered.contains("…"));
    assert!(
        lines
            .iter()
            .all(|line| line_width(&line.line) <= usize::from(width))
    );
}

#[test]
fn status_is_a_local_command_and_its_card_is_not_saved_as_conversation_history() {
    let mut view = View::new(PathBuf::from("/tmp/bettercodex").as_path());
    assert_eq!(
        view.handle_terminal_event(Event::Paste("/status".to_string())),
        Action::None
    );
    assert_eq!(
        view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))),
        Action::ShowStatus
    );

    view.add_status(snapshot());

    assert!(view.session_transcript().is_empty());
}
