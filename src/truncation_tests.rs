use crate::protocol::DEFAULT_IMAGE_DETAIL;
use crate::protocol::FunctionCallOutputContentItem;
use pretty_assertions::assert_eq;

use super::approx_token_count;
use super::formatted_truncate_text;
use super::formatted_truncate_text_content_items;
use super::truncate_function_output_items;
use super::truncate_text;

#[test]
fn truncate_tokens_less_than_placeholder_returns_placeholder() {
    let content = "example output";

    assert_eq!(
        "Warning: truncated output (original token count: 4)\nTotal output lines: 1\n\nex…3 tokens truncated…ut",
        formatted_truncate_text(content, 1),
    );
}

#[test]
fn truncate_tokens_under_limit_returns_original() {
    let content = "example output";

    assert_eq!(content, formatted_truncate_text(content, 10),);
}

#[test]
fn truncate_tokens_over_limit_returns_truncated() {
    let content = "this is an example of a long output that should be truncated";

    assert_eq!(
        "Warning: truncated output (original token count: 15)\nTotal output lines: 1\n\nthis is an…10 tokens truncated… truncated",
        formatted_truncate_text(content, 5),
    );
}

#[test]
fn truncate_tokens_reports_original_line_count_when_truncated() {
    let content =
        "this is an example of a long output that should be truncated\nalso some other line";

    assert_eq!(
        "Warning: truncated output (original token count: 21)\nTotal output lines: 2\n\nthis is an example o…11 tokens truncated…also some other line",
        formatted_truncate_text(content, 10),
    );
}

#[test]
fn truncate_tokens_handles_utf8_content() {
    let s = "😀😀😀😀😀😀😀😀😀😀\nsecond line with text\n";
    let out = truncate_text(s, 5);
    assert_eq!(out, "😀😀…11 tokens truncated…with text\n");
}

#[test]
fn truncates_across_multiple_under_limit_texts_and_reports_omitted() {
    let chunk = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega.\n";
    let chunk_tokens = approx_token_count(chunk);
    assert!(chunk_tokens > 0, "chunk must consume tokens");
    let limit = chunk_tokens * 3;
    let t1 = chunk.to_string();
    let t2 = chunk.to_string();
    let t3 = chunk.repeat(10);
    let t4 = chunk.to_string();
    let t5 = chunk.to_string();

    let items = vec![
        FunctionCallOutputContentItem::InputText { text: t1.clone() },
        FunctionCallOutputContentItem::InputText { text: t2.clone() },
        FunctionCallOutputContentItem::InputImage {
            image_url: "img:mid".to_string(),
            detail: Some(DEFAULT_IMAGE_DETAIL),
        },
        FunctionCallOutputContentItem::InputText { text: t3 },
        FunctionCallOutputContentItem::InputText { text: t4 },
        FunctionCallOutputContentItem::InputText { text: t5 },
    ];

    let output = truncate_function_output_items(&items, limit, |_| 0);

    assert_eq!(output.len(), 5);

    let first_text = match &output[0] {
        FunctionCallOutputContentItem::InputText { text } => text,
        other => panic!("unexpected first item: {other:?}"),
    };
    assert_eq!(first_text, &t1);

    let second_text = match &output[1] {
        FunctionCallOutputContentItem::InputText { text } => text,
        other => panic!("unexpected second item: {other:?}"),
    };
    assert_eq!(second_text, &t2);

    assert_eq!(
        output[2],
        FunctionCallOutputContentItem::InputImage {
            image_url: "img:mid".to_string(),
            detail: Some(DEFAULT_IMAGE_DETAIL),
        }
    );

    let fourth_text = match &output[3] {
        FunctionCallOutputContentItem::InputText { text } => text,
        other => panic!("unexpected fourth item: {other:?}"),
    };
    assert!(
        fourth_text.contains("tokens truncated"),
        "expected marker in truncated snippet: {fourth_text}"
    );

    let summary_text = match &output[4] {
        FunctionCallOutputContentItem::InputText { text } => text,
        other => panic!("unexpected summary item: {other:?}"),
    };
    assert!(summary_text.contains("omitted 2 text items"));
}

#[test]
fn formatted_truncate_text_content_items_returns_original_under_limit() {
    let items = vec![
        FunctionCallOutputContentItem::InputText {
            text: "alpha".to_string(),
        },
        FunctionCallOutputContentItem::InputText {
            text: String::new(),
        },
        FunctionCallOutputContentItem::InputText {
            text: "beta".to_string(),
        },
    ];

    let (output, original_token_count) = formatted_truncate_text_content_items(&items, 8);

    assert_eq!(output, items);
    assert_eq!(original_token_count, None);
}

#[test]
fn formatted_truncate_text_content_items_preserves_empty_leading_text_behavior() {
    let items = vec![
        FunctionCallOutputContentItem::InputText {
            text: String::new(),
        },
        FunctionCallOutputContentItem::InputText {
            text: "abc".to_string(),
        },
    ];

    let (output, original_token_count) = formatted_truncate_text_content_items(&items, 0);

    assert_eq!(
        output,
        vec![FunctionCallOutputContentItem::InputText {
            text: "Warning: truncated output (original token count: 1)\nTotal output lines: 1\n\n…1 tokens truncated…".to_string(),
        }]
    );
    assert_eq!(original_token_count, Some(1));
}

#[test]
fn formatted_truncate_text_content_items_merges_text_and_appends_media() {
    let items = vec![
        FunctionCallOutputContentItem::InputText {
            text: "abcd".to_string(),
        },
        FunctionCallOutputContentItem::InputImage {
            image_url: "img:one".to_string(),
            detail: Some(DEFAULT_IMAGE_DETAIL),
        },
        FunctionCallOutputContentItem::InputText {
            text: "efgh".to_string(),
        },
        FunctionCallOutputContentItem::InputAudio {
            audio_url: "audio:one".to_string(),
        },
        FunctionCallOutputContentItem::InputText {
            text: "ijkl".to_string(),
        },
        FunctionCallOutputContentItem::InputImage {
            image_url: "img:two".to_string(),
            detail: Some(DEFAULT_IMAGE_DETAIL),
        },
    ];

    let (output, original_token_count) = formatted_truncate_text_content_items(&items, 2);

    assert_eq!(
        output,
        vec![
            FunctionCallOutputContentItem::InputText {
                text: "Warning: truncated output (original token count: 4)\nTotal output lines: 3\n\nabcd…2 tokens truncated…ijkl".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "img:one".to_string(),
                detail: Some(DEFAULT_IMAGE_DETAIL),
            },
            FunctionCallOutputContentItem::InputAudio {
                audio_url: "audio:one".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "img:two".to_string(),
                detail: Some(DEFAULT_IMAGE_DETAIL),
            },
        ]
    );
    assert_eq!(original_token_count, Some(4));
}

#[test]
fn formatted_truncate_text_content_items_preserves_encrypted_content() {
    let items = vec![
        FunctionCallOutputContentItem::InputText {
            text: "abcdefgh".to_string(),
        },
        FunctionCallOutputContentItem::EncryptedContent {
            encrypted_content: "enc_opaque".to_string(),
        },
    ];

    let (output, original_token_count) = formatted_truncate_text_content_items(&items, 1);

    assert_eq!(
        output,
        vec![
            FunctionCallOutputContentItem::InputText {
                text: "Warning: truncated output (original token count: 2)\nTotal output lines: 1\n\nab…1 tokens truncated…gh".to_string(),
            },
            FunctionCallOutputContentItem::EncryptedContent {
                encrypted_content: "enc_opaque".to_string(),
            },
        ]
    );
    assert_eq!(original_token_count, Some(2));
}

#[test]
fn truncate_function_output_items_omits_audio_over_budget() {
    let items = vec![
        FunctionCallOutputContentItem::InputText {
            text: "abcdefgh".to_string(),
        },
        FunctionCallOutputContentItem::EncryptedContent {
            encrypted_content: "enc_opaque".to_string(),
        },
        FunctionCallOutputContentItem::InputAudio {
            audio_url: "audio:one".to_string(),
        },
    ];

    let output = truncate_function_output_items(&items, 1, |_| 1);

    assert_eq!(
        output,
        vec![
            FunctionCallOutputContentItem::InputText {
                text: "ab…1 tokens truncated…gh".to_string(),
            },
            FunctionCallOutputContentItem::EncryptedContent {
                encrypted_content: "enc_opaque".to_string(),
            },
            FunctionCallOutputContentItem::InputText {
                text: "[omitted 1 audio items ...]".to_string(),
            },
        ]
    );
}

#[test]
fn truncate_function_output_items_charges_audio_against_token_budget() {
    let audio = FunctionCallOutputContentItem::InputAudio {
        audio_url: "audio:one".to_string(),
    };
    let items = vec![
        audio.clone(),
        FunctionCallOutputContentItem::InputText {
            text: "abcdefgh".to_string(),
        },
    ];

    let output = truncate_function_output_items(&items, 2, |_| 1);

    assert_eq!(
        output,
        vec![
            audio,
            FunctionCallOutputContentItem::InputText {
                text: truncate_text("abcdefgh", 1),
            },
        ]
    );
}

#[test]
fn formatted_truncate_text_content_items_merges_all_text_for_token_budget() {
    let items = vec![
        FunctionCallOutputContentItem::InputText {
            text: "abcdefgh".to_string(),
        },
        FunctionCallOutputContentItem::InputText {
            text: "ijklmnop".to_string(),
        },
    ];

    let (output, original_token_count) = formatted_truncate_text_content_items(&items, 2);

    assert_eq!(
        output,
        vec![FunctionCallOutputContentItem::InputText {
            text: "Warning: truncated output (original token count: 5)\nTotal output lines: 2\n\nabcd…3 tokens truncated…mnop".to_string(),
        }]
    );
    assert_eq!(original_token_count, Some(5));
}
