use super::*;
use crate::agent::TurnControl;
use crate::input::UserInput;

#[test]
fn pending_steers_commit_by_admission_id_without_disturbing_follow_ups() {
    let (handle, _control) = TurnControl::channel();
    let first = handle.steer(UserInput::text("first steer")).unwrap();
    let second = handle.steer(UserInput::text("second steer")).unwrap();
    let mut pending = PendingInput::default();
    pending.add_steer(first, "first steer".to_string());
    pending.add_steer(second, "second steer".to_string());
    pending.queue_follow_up("later".to_string());

    assert_eq!(
        pending.commit_steer(second).as_deref(),
        Some("second steer")
    );
    assert_eq!(pending.steer_count(), 1);
    assert_eq!(pending.follow_up_count(), 1);
    assert_eq!(pending.commit_steer(first).as_deref(), Some("first steer"));
}

#[test]
fn restore_order_keeps_steers_before_fifo_follow_ups() {
    let (handle, _control) = TurnControl::channel();
    let id = handle.steer(UserInput::text("steer")).unwrap();
    let mut pending = PendingInput::default();
    pending.add_steer(id, "steer".to_string());
    pending.queue_follow_up("first follow-up".to_string());
    pending.queue_follow_up("second follow-up".to_string());

    assert_eq!(
        pending.take_all(),
        vec![
            "steer".to_string(),
            "first follow-up".to_string(),
            "second follow-up".to_string(),
        ]
    );
}

#[test]
fn rendered_preview_is_bounded_and_separates_steers_from_follow_ups() {
    let (handle, _control) = TurnControl::channel();
    let mut pending = PendingInput::default();
    for index in 0..5 {
        let prompt = format!("steer {index} {}", "x".repeat(400));
        let id = handle.steer(UserInput::text(&prompt)).unwrap();
        pending.add_steer(id, prompt);
    }
    pending.queue_follow_up("run this later".to_string());

    let rendered = pending
        .lines()
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Steering after the current model step"));
    assert!(rendered.contains("… 2 more"));
    assert!(rendered.contains("Queued follow-up inputs"));
    assert!(rendered.contains("run this later"));
    assert!(rendered.contains("Alt+Up / Shift+Left"));
    assert!(rendered.len() < 1_200, "{rendered}");
}
