//! Helpers for truncating tool output.
//!
//! Ported from OpenAI Codex commit
//! `92cbfb4d2431bdc53dc03507aea2dc5b8e932e40`.

mod string;

pub(crate) use self::string::approx_bytes_for_tokens;
pub(crate) use self::string::approx_token_count;
use self::string::truncate_middle_chars;
use self::string::truncate_middle_with_token_budget;
use serde::Deserialize;
use serde::Serialize;
use std::ops::Mul;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", content = "limit", rename_all = "snake_case")]
pub(crate) enum TruncationPolicy {
    Bytes(usize),
    Tokens(usize),
}

impl TruncationPolicy {
    pub(crate) fn byte_budget(self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Tokens(tokens) => approx_bytes_for_tokens(tokens),
        }
    }
}

impl Mul<f64> for TruncationPolicy {
    type Output = Self;

    fn mul(self, multiplier: f64) -> Self::Output {
        match self {
            Self::Bytes(bytes) => Self::Bytes((bytes as f64 * multiplier).ceil() as usize),
            Self::Tokens(tokens) => Self::Tokens((tokens as f64 * multiplier).ceil() as usize),
        }
    }
}

pub(crate) fn formatted_truncate_text(content: &str, max_tokens: usize) -> String {
    formatted_truncate_text_with_policy(content, TruncationPolicy::Tokens(max_tokens))
}

pub(crate) fn formatted_truncate_text_with_policy(
    content: &str,
    policy: TruncationPolicy,
) -> String {
    if content.len() <= policy.byte_budget() {
        return content.to_string();
    }

    let original_token_count = approx_token_count(content);
    let total_lines = content.lines().count();
    let result = truncate_text_with_policy(content, policy);
    format!(
        "Warning: truncated output (original token count: {original_token_count})\nTotal output lines: {total_lines}\n\n{result}"
    )
}

pub(crate) fn truncate_text(content: &str, max_tokens: usize) -> String {
    truncate_text_with_policy(content, TruncationPolicy::Tokens(max_tokens))
}

pub(crate) fn truncate_text_with_policy(content: &str, policy: TruncationPolicy) -> String {
    match policy {
        TruncationPolicy::Bytes(bytes) => truncate_middle_chars(content, bytes),
        TruncationPolicy::Tokens(tokens) => truncate_middle_with_token_budget(content, tokens).0,
    }
}

#[cfg(test)]
#[path = "truncation_tests.rs"]
mod tests;
