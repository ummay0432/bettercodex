//! Helpers for truncating tool and exec output.
//!
//! Ported from OpenAI Codex commit
//! `1669c2403f793d0230065397dfc25f52b844244e`.

mod string;

pub(crate) use self::string::approx_bytes_for_tokens;
pub(crate) use self::string::approx_token_count;
pub(crate) use self::string::approx_tokens_from_byte_count;
use self::string::truncate_middle_with_token_budget;
use crate::protocol::FunctionCallOutputContentItem;

pub(crate) fn formatted_truncate_text(content: &str, max_tokens: usize) -> String {
    if content.len() <= approx_bytes_for_tokens(max_tokens) {
        return content.to_string();
    }

    let original_token_count = approx_token_count(content);
    let total_lines = content.lines().count();
    let result = truncate_text(content, max_tokens);
    format!(
        "Warning: truncated output (original token count: {original_token_count})\nTotal output lines: {total_lines}\n\n{result}"
    )
}

pub(crate) fn truncate_text(content: &str, max_tokens: usize) -> String {
    truncate_middle_with_token_budget(content, max_tokens).0
}

pub(crate) fn formatted_truncate_text_content_items(
    items: &[FunctionCallOutputContentItem],
    max_tokens: usize,
) -> (Vec<FunctionCallOutputContentItem>, Option<usize>) {
    let text_segments = items
        .iter()
        .filter_map(|item| match item {
            FunctionCallOutputContentItem::InputText { text } => Some(text.as_str()),
            FunctionCallOutputContentItem::InputImage { .. }
            | FunctionCallOutputContentItem::InputAudio { .. }
            | FunctionCallOutputContentItem::EncryptedContent { .. } => None,
        })
        .collect::<Vec<_>>();

    if text_segments.is_empty() {
        return (items.to_vec(), None);
    }

    let mut combined = String::new();
    for text in &text_segments {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(text);
    }

    if combined.len() <= approx_bytes_for_tokens(max_tokens) {
        return (items.to_vec(), None);
    }

    let original_token_count = approx_token_count(&combined);
    let mut out = vec![FunctionCallOutputContentItem::InputText {
        text: formatted_truncate_text(&combined, max_tokens),
    }];
    out.extend(items.iter().filter_map(|item| match item {
        FunctionCallOutputContentItem::InputImage { image_url, detail } => {
            Some(FunctionCallOutputContentItem::InputImage {
                image_url: image_url.clone(),
                detail: *detail,
            })
        }
        FunctionCallOutputContentItem::InputAudio { audio_url } => {
            Some(FunctionCallOutputContentItem::InputAudio {
                audio_url: audio_url.clone(),
            })
        }
        FunctionCallOutputContentItem::EncryptedContent { encrypted_content } => {
            Some(FunctionCallOutputContentItem::EncryptedContent {
                encrypted_content: encrypted_content.clone(),
            })
        }
        FunctionCallOutputContentItem::InputText { .. } => None,
    }));

    (out, Some(original_token_count))
}

pub(crate) fn truncate_function_output_items(
    items: &[FunctionCallOutputContentItem],
    max_tokens: usize,
    estimate_audio_token_count: impl Fn(&str) -> usize,
) -> Vec<FunctionCallOutputContentItem> {
    let mut out: Vec<FunctionCallOutputContentItem> = Vec::with_capacity(items.len());
    let mut remaining_budget = max_tokens;
    let mut omitted_text_items = 0usize;
    let mut omitted_audio_items = 0usize;

    for item in items {
        match item {
            FunctionCallOutputContentItem::InputText { text } => {
                if remaining_budget == 0 {
                    omitted_text_items += 1;
                    continue;
                }

                let cost = approx_token_count(text);

                if cost <= remaining_budget {
                    out.push(FunctionCallOutputContentItem::InputText { text: text.clone() });
                    remaining_budget = remaining_budget.saturating_sub(cost);
                } else {
                    let snippet = truncate_text(text, remaining_budget);
                    if snippet.is_empty() {
                        omitted_text_items += 1;
                    } else {
                        out.push(FunctionCallOutputContentItem::InputText { text: snippet });
                    }
                    remaining_budget = 0;
                }
            }
            FunctionCallOutputContentItem::InputImage { image_url, detail } => {
                out.push(FunctionCallOutputContentItem::InputImage {
                    image_url: image_url.clone(),
                    detail: *detail,
                });
            }
            FunctionCallOutputContentItem::InputAudio { audio_url } => {
                let cost = estimate_audio_token_count(audio_url);
                if cost <= remaining_budget {
                    out.push(FunctionCallOutputContentItem::InputAudio {
                        audio_url: audio_url.clone(),
                    });
                    remaining_budget = remaining_budget.saturating_sub(cost);
                } else {
                    omitted_audio_items += 1;
                }
            }
            FunctionCallOutputContentItem::EncryptedContent { encrypted_content } => {
                out.push(FunctionCallOutputContentItem::EncryptedContent {
                    encrypted_content: encrypted_content.clone(),
                });
            }
        }
    }

    if omitted_text_items > 0 {
        out.push(FunctionCallOutputContentItem::InputText {
            text: format!("[omitted {omitted_text_items} text items ...]"),
        });
    }
    if omitted_audio_items > 0 {
        out.push(FunctionCallOutputContentItem::InputText {
            text: format!("[omitted {omitted_audio_items} audio items ...]"),
        });
    }

    out
}

#[cfg(test)]
#[path = "truncation_tests.rs"]
mod tests;
