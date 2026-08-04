use super::*;

#[test]
fn extracts_a_split_model_heading_and_resets_between_sections() {
    let mut status = ReasoningStatus::default();
    status.push_delta("**Inspecting");
    assert_eq!(status.heading(), None);
    status.push_delta(" the request**\n\nI am reading the relevant code.");
    assert_eq!(status.heading(), Some("Inspecting the request"));

    status.reset();
    status.push_delta("**Running\nvalidation**");
    assert_eq!(status.heading(), Some("Running validation"));
}

#[test]
fn sanitizes_and_bounds_model_authored_headings() {
    let mut status = ReasoningStatus::default();
    let oversized = format!("**\u{1b}[31m{}**", "x".repeat(MAX_HEADING_GRAPHEMES + 20));
    status.push_delta(&oversized);

    let expected = format!("{}…", "x".repeat(MAX_HEADING_GRAPHEMES));
    assert_eq!(status.heading(), Some(expected.as_str()));
}
