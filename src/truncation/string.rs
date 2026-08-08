//! Utilities for truncating large chunks of output while preserving a prefix
//! and suffix on UTF-8 boundaries.

const APPROX_BYTES_PER_TOKEN: usize = 4;

/// Truncate the middle of a UTF-8 string to at most `max_tokens` approximate
/// tokens, preserving the beginning and the end. Returns the possibly
/// truncated string and `Some(original_token_count)` if truncation occurred;
/// otherwise returns the original string and `None`.
pub(super) fn truncate_middle_with_token_budget(
    s: &str,
    max_tokens: usize,
) -> (String, Option<u64>) {
    if s.is_empty() {
        return (String::new(), None);
    }

    if max_tokens > 0 && s.len() <= approx_bytes_for_tokens(max_tokens) {
        return (s.to_string(), None);
    }

    let truncated = truncate_with_byte_estimate(s, approx_bytes_for_tokens(max_tokens));
    let total_tokens = u64::try_from(approx_token_count(s)).unwrap_or(u64::MAX);

    if truncated == s {
        (truncated, None)
    } else {
        (truncated, Some(total_tokens))
    }
}

fn truncate_with_byte_estimate(s: &str, max_bytes: usize) -> String {
    if s.is_empty() {
        return String::new();
    }

    if max_bytes == 0 {
        return format_truncation_marker(approx_tokens_from_byte_count(s.len()));
    }

    if s.len() <= max_bytes {
        return s.to_string();
    }

    let total_bytes = s.len();
    let (left_budget, right_budget) = split_budget(max_bytes);
    let (left, right) = split_string(s, left_budget, right_budget);
    let marker = format_truncation_marker(approx_tokens_from_byte_count(
        total_bytes.saturating_sub(max_bytes),
    ));

    assemble_truncated_output(left, right, &marker)
}

pub(crate) fn approx_token_count(text: &str) -> usize {
    let len = text.len();
    len.saturating_add(APPROX_BYTES_PER_TOKEN.saturating_sub(1)) / APPROX_BYTES_PER_TOKEN
}

pub(crate) fn approx_bytes_for_tokens(tokens: usize) -> usize {
    tokens.saturating_mul(APPROX_BYTES_PER_TOKEN)
}

pub(crate) fn approx_tokens_from_byte_count(bytes: usize) -> u64 {
    let bytes_u64 = bytes as u64;
    bytes_u64.saturating_add((APPROX_BYTES_PER_TOKEN as u64).saturating_sub(1))
        / (APPROX_BYTES_PER_TOKEN as u64)
}

fn split_string(s: &str, beginning_bytes: usize, end_bytes: usize) -> (&str, &str) {
    let len = s.len();
    let prefix_end = s.floor_char_boundary(beginning_bytes.min(len));
    let suffix_start = s
        .ceil_char_boundary(len.saturating_sub(end_bytes))
        .max(prefix_end);
    (&s[..prefix_end], &s[suffix_start..])
}

fn split_budget(budget: usize) -> (usize, usize) {
    let left = budget / 2;
    (left, budget - left)
}

fn format_truncation_marker(removed_count: u64) -> String {
    format!("…{removed_count} tokens truncated…")
}

fn assemble_truncated_output(prefix: &str, suffix: &str, marker: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + marker.len() + suffix.len() + 1);
    out.push_str(prefix);
    out.push_str(marker);
    out.push_str(suffix);
    out
}
