use pretty_assertions::assert_eq;

use super::formatted_truncate_text;
use super::truncate_text;

#[test]
fn formatted_truncation_reports_size_and_preserves_both_ends() {
    let content =
        "this is an example of a long output that should be truncated\nalso some other line";

    assert_eq!(
        "Warning: truncated output (original token count: 21)\nTotal output lines: 2\n\nthis is an example o…11 tokens truncated…also some other line",
        formatted_truncate_text(content, 10),
    );
}

#[test]
fn formatted_truncation_returns_under_limit_text_unchanged() {
    assert_eq!(
        "example output",
        formatted_truncate_text("example output", 10)
    );
}

#[test]
fn truncation_preserves_utf8_boundaries() {
    let content = "😀😀😀😀😀😀😀😀😀😀\nsecond line with text\n";
    assert_eq!(
        "😀😀…11 tokens truncated…with text\n",
        truncate_text(content, 5)
    );
}
