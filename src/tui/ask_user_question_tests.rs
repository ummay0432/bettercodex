use super::ask_user_question::AskUserQuestionCard;
use super::ask_user_question::AskUserQuestionCardAction;
use super::view::Action;
use super::view::View;
use crate::ask_user_question::AskUserQuestion;
use crate::ask_user_question::AskUserQuestionArgs;
use crate::ask_user_question::AskUserQuestionOption;
use crate::events::AgentEvent;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use serde_json::json;
use std::path::Path;
use std::time::Duration;

fn option(
    label: &str,
    description: &str,
    preview: Option<&str>,
    default_selected: bool,
) -> AskUserQuestionOption {
    AskUserQuestionOption {
        label: label.to_string(),
        description: description.to_string(),
        preview: preview.map(str::to_string),
        default_selected,
    }
}

fn args(multi_select: bool) -> AskUserQuestionArgs {
    AskUserQuestionArgs {
        questions: vec![AskUserQuestion {
            question: "Which deployment path should bettercodex use?".to_string(),
            header: "Deploy".to_string(),
            options: vec![
                option(
                    "Canary",
                    "Roll out gradually and watch health checks.",
                    Some("## Canary plan\n\nDeploy to **10%** first."),
                    multi_select,
                ),
                option(
                    "Immediate",
                    "Deploy to every instance at once.",
                    None,
                    false,
                ),
            ],
            multi_select,
        }],
    }
}

fn two_question_args() -> AskUserQuestionArgs {
    let mut arguments = args(false);
    arguments.questions.push(AskUserQuestion {
        question: "When should the deployment begin?".to_string(),
        header: "Timing".to_string(),
        options: vec![
            option("Tonight", "Start after peak traffic ends.", None, false),
            option("Tomorrow", "Wait for the next workday.", None, false),
        ],
        multi_select: false,
    });
    arguments
}

fn press(card: &mut AskUserQuestionCard, code: KeyCode) -> AskUserQuestionCardAction {
    card.handle_key(KeyEvent::new(code, KeyModifiers::NONE), 80)
}

fn render(card: &AskUserQuestionCard, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| {
            card.render(
                frame,
                frame.area(),
                Style::default(),
                Path::new("/tmp/bettercodex"),
            );
        })
        .expect("render AskUserQuestion card");
    let buffer = terminal.backend().buffer();
    (buffer.area.y..buffer.area.bottom())
        .map(|y| {
            (buffer.area.x..buffer.area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_ascii_text(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<(u16, u16)> {
    let width = u16::try_from(text.len()).ok()?;
    for y in buffer.area.y..buffer.area.bottom() {
        for x in buffer.area.x..=buffer.area.right().saturating_sub(width) {
            let rendered = (0..width)
                .map(|offset| buffer[(x.saturating_add(offset), y)].symbol())
                .collect::<String>();
            if rendered == text {
                return Some((x, y));
            }
        }
    }
    None
}

fn render_view(view: &mut View, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| view.render(frame))
        .expect("render view");
    let buffer = terminal.backend().buffer();
    (buffer.area.y..buffer.area.bottom())
        .map(|y| {
            (buffer.area.x..buffer.area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn tool_transcript_uses_compact_waiting_and_answered_states() {
    let arguments = args(false);
    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.handle_agent_event(AgentEvent::ToolStarted {
        call_id: "call-transcript".to_string(),
        name: "ask_user_question".to_string(),
        input: Some(serde_json::to_value(&arguments).expect("question arguments")),
    });
    let waiting = render_view(&mut view, 80, 24);
    assert!(waiting.contains("Waiting for your answer"), "{waiting}");
    assert!(!waiting.contains("Which deployment path"), "{waiting}");

    view.handle_agent_event(AgentEvent::ToolCompleted {
        call_id: "call-transcript".to_string(),
        output: Ok(json!({
            "answers": [{
                "question": arguments.questions[0].question,
                "selectedOptions": ["Canary"]
            }],
            "cancelled": false
        })),
        file_change: None,
        duration: Duration::from_millis(10),
    });
    let answered = render_view(&mut view, 80, 24);
    assert!(answered.contains("Answered 1 question"), "{answered}");
    let transcript = view.session_transcript();
    let retained = transcript
        .iter()
        .find_map(|item| match item {
            crate::rollout::SessionTranscriptItem::Tool { tool }
                if tool.call_id == "call-transcript" =>
            {
                tool.output.as_ref()
            }
            _ => None,
        })
        .expect("retained AskUserQuestion result");
    assert!(matches!(
        retained,
        crate::rollout::SessionTranscriptToolOutput::Success(output)
            if output["answers"][0]["selectedOptions"] == json!(["Canary"])
    ));
}

#[test]
fn questionnaire_card_leaves_one_blank_row_above_its_surface() {
    let card = AskUserQuestionCard::new("call-gap".to_string(), args(true));
    let width = 80;
    let height = card.preferred_height(width, Path::new("/tmp/bettercodex"));
    let surface_background = Color::Rgb(47, 47, 47);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| {
            card.render(
                frame,
                frame.area(),
                Style::default().bg(surface_background),
                Path::new("/tmp/bettercodex"),
            );
        })
        .expect("render AskUserQuestion card");
    let buffer = terminal.backend().buffer();

    for x in buffer.area.x..buffer.area.right() {
        assert_eq!(buffer[(x, buffer.area.y)].bg, Color::Reset);
        assert_eq!(
            buffer[(x, buffer.area.y.saturating_add(1))].bg,
            surface_background
        );
    }
}

#[test]
fn multi_select_defaults_are_visible_and_enter_toggles_without_submitting() {
    let mut card = AskUserQuestionCard::new("call-1".to_string(), args(true));
    let initial = render(&card, 80, 20);
    assert!(initial.contains("[✓] Canary"), "{initial}");
    assert!(initial.contains("Canary plan"), "{initial}");

    assert_eq!(
        press(&mut card, KeyCode::Enter),
        AskUserQuestionCardAction::None
    );
    let toggled = render(&card, 80, 20);
    assert!(toggled.contains("[ ] Canary"), "{toggled}");

    assert_eq!(
        press(&mut card, KeyCode::Enter),
        AskUserQuestionCardAction::None
    );
    assert_eq!(
        press(&mut card, KeyCode::End),
        AskUserQuestionCardAction::None
    );
    assert_eq!(
        press(&mut card, KeyCode::Enter),
        AskUserQuestionCardAction::None
    );
    let review = render(&card, 80, 20);
    assert!(review.contains("Review your answers"), "{review}");
    let AskUserQuestionCardAction::Submit { response, .. } = press(&mut card, KeyCode::Enter)
    else {
        panic!("expected explicit multi-select submission");
    };
    assert!(!response.cancelled);
    assert_eq!(response.answers[0].selected_options, ["Canary"]);
}

#[test]
fn single_select_uses_vertical_descriptions_and_auto_submits_one_question() {
    let mut card = AskUserQuestionCard::new("call-2".to_string(), args(false));
    let initial = render(&card, 80, 18);
    let lines = initial.lines().collect::<Vec<_>>();
    let canary = lines
        .iter()
        .position(|line| line.contains("› 1. Canary"))
        .expect("focused Canary row");
    assert_eq!(
        lines[canary + 1].trim(),
        "Roll out gradually and watch health checks."
    );
    assert!(initial.contains("3. Type something."), "{initial}");

    let AskUserQuestionCardAction::Submit { response, .. } = press(&mut card, KeyCode::Enter)
    else {
        panic!("expected a single-select answer");
    };
    assert_eq!(response.answers[0].selected_options, ["Canary"]);
    assert_eq!(response.answers[0].free_text, None);
}

#[test]
fn type_something_accepts_typing_without_enter_and_returns_structured_free_text() {
    let mut card = AskUserQuestionCard::new("call-3".to_string(), args(false));
    assert_eq!(
        press(&mut card, KeyCode::Char('3')),
        AskUserQuestionCardAction::None
    );
    assert_eq!(
        press(&mut card, KeyCode::Char('S')),
        AskUserQuestionCardAction::None
    );
    card.handle_paste("tage it overnight");
    let editing = render(&card, 64, 18);
    assert!(editing.contains("› 3. Stage it overnight"), "{editing}");

    let AskUserQuestionCardAction::Submit { response, .. } = press(&mut card, KeyCode::Enter)
    else {
        panic!("expected custom answer submission");
    };
    assert!(response.answers[0].selected_options.is_empty());
    assert_eq!(
        response.answers[0].free_text.as_deref(),
        Some("Stage it overnight")
    );
}

#[test]
fn questionnaire_card_uses_white_primary_text_and_distinct_focus_and_selection() {
    let mut card = AskUserQuestionCard::new("call-color".to_string(), args(true));
    assert_eq!(
        press(&mut card, KeyCode::Down),
        AskUserQuestionCardAction::None
    );
    let width = 80;
    let height = card.preferred_height(width, Path::new("/tmp/bettercodex"));
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| {
            card.render(
                frame,
                frame.area(),
                Style::default(),
                Path::new("/tmp/bettercodex"),
            );
        })
        .expect("render AskUserQuestion card");
    let buffer = terminal.backend().buffer();

    let question = "Which deployment path should bettercodex use?";
    let (question_x, question_y) =
        find_ascii_text(buffer, question).expect("rendered question prompt");
    for x in question_x..question_x.saturating_add(question.len() as u16) {
        let cell = &buffer[(x, question_y)];
        assert_eq!(cell.fg, Color::White);
        assert!(cell.modifier.contains(Modifier::BOLD));
    }

    let (selected_x, selected_y) =
        find_ascii_text(buffer, "Canary").expect("selected option label");
    for x in selected_x..selected_x.saturating_add("Canary".len() as u16) {
        let cell = &buffer[(x, selected_y)];
        assert_eq!(cell.fg, Color::White);
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert!(!cell.modifier.contains(Modifier::REVERSED));
    }

    let (focused_x, focused_y) =
        find_ascii_text(buffer, "Immediate").expect("focused option label");
    for x in focused_x..focused_x.saturating_add("Immediate".len() as u16) {
        let cell = &buffer[(x, focused_y)];
        assert_eq!(cell.fg, Color::White);
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert!(cell.modifier.contains(Modifier::REVERSED));
    }

    let description = "Roll out gradually and watch health checks.";
    let (description_x, description_y) =
        find_ascii_text(buffer, description).expect("secondary option description");
    for x in description_x..description_x.saturating_add(description.len() as u16) {
        let cell = &buffer[(x, description_y)];
        assert_eq!(cell.fg, Color::Indexed(250));
        assert!(!cell.modifier.contains(Modifier::REVERSED));
    }
}

#[test]
fn questionnaire_card_leaves_a_blank_row_before_the_follow_up_action() {
    let mut card = AskUserQuestionCard::new("call-action-gap".to_string(), args(true));
    let width = 80;
    let height = card.preferred_height(width, Path::new("/tmp/bettercodex"));
    let initial = render(&card, width, height);
    let lines = initial.lines().collect::<Vec<_>>();
    let other = lines
        .iter()
        .position(|line| line.contains("Type something."))
        .expect("free-text choice");
    let action = lines
        .iter()
        .position(|line| line.trim() == "Submit")
        .expect("submit action");
    assert_eq!(action, other + 2, "{initial}");
    assert!(lines[other + 1].trim().is_empty(), "{initial}");

    assert_eq!(
        press(&mut card, KeyCode::Char('3')),
        AskUserQuestionCardAction::None
    );
    assert_eq!(
        press(&mut card, KeyCode::Char('S')),
        AskUserQuestionCardAction::None
    );
    card.handle_paste("tage it overnight");
    let editing = render(&card, width, height);
    let lines = editing.lines().collect::<Vec<_>>();
    let editor = lines
        .iter()
        .position(|line| line.contains("Stage it overnight"))
        .expect("free-text editor");
    let action = lines
        .iter()
        .position(|line| line.trim() == "Submit")
        .expect("submit action while editing");
    assert_eq!(action, editor + 2, "{editing}");
    assert!(lines[editor + 1].trim().is_empty(), "{editing}");
}

#[test]
fn multiple_single_select_questions_advance_through_tabs_then_review() {
    let mut card = AskUserQuestionCard::new("call-tabs".to_string(), two_question_args());
    let first = render(&card, 80, 20);
    assert!(first.contains("□ Deploy"), "{first}");
    assert!(first.contains("□ Timing"), "{first}");
    assert!(first.contains("✓ Submit"), "{first}");

    assert_eq!(
        press(&mut card, KeyCode::Enter),
        AskUserQuestionCardAction::None
    );
    let second = render(&card, 80, 20);
    assert!(second.contains("✓ Deploy"), "{second}");
    assert!(
        second.contains("When should the deployment begin?"),
        "{second}"
    );

    assert_eq!(
        press(&mut card, KeyCode::Enter),
        AskUserQuestionCardAction::None
    );
    let review = render(&card, 80, 20);
    assert!(review.contains("Review your answers"), "{review}");
    assert!(review.contains("Canary"), "{review}");
    assert!(review.contains("Tonight"), "{review}");

    let AskUserQuestionCardAction::Submit { response, .. } = press(&mut card, KeyCode::Enter)
    else {
        panic!("expected reviewed answers to submit");
    };
    assert_eq!(response.answers.len(), 2);
}

#[test]
fn cancellation_is_explicit_and_narrow_terminals_keep_the_card_operable() {
    let card = AskUserQuestionCard::new("call-4".to_string(), args(true));
    let narrow = render(&card, 36, 18);
    assert!(narrow.contains("✓ Deploy"), "{narrow}");
    assert!(narrow.contains("✓ Submit"), "{narrow}");
    assert!(narrow.contains("Canary"), "{narrow}");
    assert!(narrow.contains("esc cancel"), "{narrow}");

    let mut view = View::new(Path::new("/tmp/bettercodex"));
    view.show_ask_user_question("call-4".to_string(), args(true));
    assert_eq!(
        view.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE,))),
        Action::ResolveAskUserQuestion {
            call_id: "call-4".to_string(),
            response: crate::ask_user_question::AskUserQuestionResponse::cancelled(),
        }
    );
}
