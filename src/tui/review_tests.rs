use super::*;

#[test]
fn explicit_review_queues_behind_an_active_turn() {
    for prompt in [
        UserPrompt::text("/review the update logic"),
        UserPrompt::text("use $review on the update logic"),
    ] {
        assert_eq!(
            active_submission_route(&prompt),
            ActiveSubmissionRoute::QueueNextTurn
        );
    }
    assert_eq!(
        active_submission_route(&UserPrompt::text("ordinary follow-up")),
        ActiveSubmissionRoute::SteerOrdinary
    );
}
