use super::*;
use pretty_assertions::assert_eq;

fn parsed(text: &str) -> Option<LoopInvocation> {
    parse_invocation(text, false).unwrap()
}

#[test]
fn recognizes_both_entry_points_and_default_count() {
    for text in [
        "$loop improve it",
        "improve $loop it",
        "improve it $loop",
        "  /loop improve it",
        "quoted `$loop` still runs",
        "``\n$loop\n`` implement it",
    ] {
        assert_eq!(parsed(text).unwrap().iterations, 3, "{text}");
    }
}

#[test]
fn accepts_every_closed_count_form() {
    for text in [
        "$loop 5 times improve it",
        "$loop 5 iterations improve it",
        "$loop 5x improve it",
        "5x $loop improve it",
        "/loop 5x improve it",
        "$loop, (5 times): improve it",
    ] {
        assert_eq!(parsed(text).unwrap().iterations, 5, "{text}");
    }
}

#[test]
fn unrelated_numbers_and_separated_count_words_remain_task_text() {
    for text in [
        "$loop fix issue 123",
        "$loop 5 improve iterations",
        "$loop release 5x behavior",
        "/loop task 8x",
        "/loop 8 task",
    ] {
        assert_eq!(parsed(text).unwrap().iterations, 3, "{text}");
    }
}

#[test]
fn repeated_triggers_merge_agreeing_counts_and_reject_conflicts() {
    assert_eq!(parsed("$loop task $loop 7x").unwrap().iterations, 7);
    assert_eq!(parsed("7x $loop task $loop 7 times").unwrap().iterations, 7);
    assert!(parse_invocation("3x $loop task $loop 4x", false).is_err());
}

#[test]
fn rejects_invalid_local_counts_without_starting_a_default_loop() {
    for text in [
        "$loop 0x task",
        "$loop -2x task",
        "$loop +2 times task",
        "$loop 1.5x task",
        "$loop 1e3x task",
        "$loop 5xx task",
        "$loop + 2x task",
        "$loop - 2 times task",
        "$loop . 5 iterations task",
        "$loop 5 time task",
        "$loop 5 iteration task",
        "/loop x task",
        "/loop 999999999999999999999999999999999999x task",
    ] {
        assert!(parse_invocation(text, false).is_err(), "{text}");
    }
}

#[test]
fn slash_is_interactive_only_and_must_be_the_first_token() {
    assert!(
        parse_invocation_with_mode("/loop task", false, false)
            .unwrap()
            .is_none()
    );
    assert!(
        parse_invocation_with_mode("prefix /loop task", false, true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        parse_invocation_with_mode(" \t/loop 8x task", false, true)
            .unwrap()
            .unwrap()
            .iterations,
        8
    );
}

#[test]
fn consumed_metadata_points_into_the_unchanged_submission() {
    let text = "  5x $loop preserve 42 exactly $loop";
    let invocation = parsed(text).unwrap();
    assert_eq!(invocation.iterations, 5);
    assert_eq!(invocation.triggers.len(), 2);
    assert_eq!(invocation.counts.len(), 1);
    for trigger in invocation.triggers {
        assert_eq!(&text[trigger.start..trigger.end], "$loop");
    }
    let count = &invocation.counts[0];
    assert_eq!(&text[count.start..count.end], "5x");
    assert!(text.contains("42"));
}

#[test]
fn exact_boundaries_leave_near_matches_ordinary() {
    for text in ["$looper task", "/looping task", "ordinary task"] {
        assert_eq!(parse_invocation(text, false).unwrap(), None, "{text}");
    }
    assert!(parsed("x$loop task").is_some());
}

#[test]
fn requires_task_content_but_accepts_an_attachment() {
    for text in ["$loop", "$loop 3x", "/loop", "  /loop, 3 times!!!"] {
        assert!(parse_invocation(text, false).is_err(), "{text}");
        assert!(parse_invocation(text, true).unwrap().is_some(), "{text}");
    }
}
