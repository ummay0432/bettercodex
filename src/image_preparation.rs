//! Model-facing image preparation ported from Codex at
//! `902bd9e06b3ecb32cbf7f8e64cd23b956be3e7fe`,
//! `codex-rs/core/src/image_preparation.rs`.
//!
//! Images are prepared before entering live history. Reconstructed history is
//! prepared once in memory on resume so rollouts created by older bettercodex
//! versions receive the same bounds without rewriting the saved source records.

use crate::image::HIGH_DETAIL_LIMITS;
use crate::image::ImageProcessingError;
use crate::image::ORIGINAL_DETAIL_LIMITS;
use crate::image::load_data_url_for_prompt;
use crate::protocol::FunctionCallOutputContentItem;
use crate::protocol::ImageDetail;
use serde_json::Value;
use serde_json::json;

const IMAGE_PROCESSING_ERROR_PLACEHOLDER: &str =
    "image content omitted because it could not be processed";
const IMAGE_TOO_LARGE_PLACEHOLDER: &str =
    "image content omitted because it exceeded the supported size limit; use a smaller image";
const UNSUPPORTED_LOW_DETAIL_PLACEHOLDER: &str = "image content omitted because detail 'low' is not supported; use 'high', 'original', or 'auto'";
const REMOTE_IMAGE_URL_PLACEHOLDER: &str =
    "image content omitted because remote image URLs are not supported";

enum ImagePreparationError {
    RemoteUrlUnsupported,
    UnsupportedLowDetail,
    Processing(ImageProcessingError),
}

impl ImagePreparationError {
    fn placeholder(&self) -> &'static str {
        match self {
            Self::RemoteUrlUnsupported => REMOTE_IMAGE_URL_PLACEHOLDER,
            Self::UnsupportedLowDetail => UNSUPPORTED_LOW_DETAIL_PLACEHOLDER,
            Self::Processing(ImageProcessingError::ImageTooLarge { .. }) => {
                IMAGE_TOO_LARGE_PLACEHOLDER
            }
            Self::Processing(_) => IMAGE_PROCESSING_ERROR_PLACEHOLDER,
        }
    }
}

pub(crate) fn prepare_tool_output_images(items: &mut [FunctionCallOutputContentItem]) {
    for item in items {
        if let FunctionCallOutputContentItem::InputImage { image_url, detail } = item
            && let Err(error) = prepare_image(image_url, *detail)
        {
            *item = FunctionCallOutputContentItem::InputText {
                text: error.placeholder().to_string(),
            };
        }
    }
}

pub(crate) fn prepare_history_images(items: &mut [Value]) {
    for item in items {
        let Some(content) = image_content_items_mut(item) else {
            continue;
        };
        for content_item in content {
            if content_item.get("type").and_then(Value::as_str) != Some("input_image") {
                continue;
            }
            let detail = match history_image_detail(content_item.get("detail")) {
                Some(detail) => detail,
                None => continue,
            };
            let Some(Value::String(image_url)) = content_item.get_mut("image_url") else {
                continue;
            };
            if let Err(error) = prepare_image(image_url, detail) {
                *content_item = json!({
                    "type": "input_text",
                    "text": error.placeholder(),
                });
            }
        }
    }
}

pub(crate) fn image_content_items_mut(item: &mut Value) -> Option<&mut Vec<Value>> {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => item.get_mut("content")?.as_array_mut(),
        Some("function_call_output" | "custom_tool_call_output") => {
            item.get_mut("output")?.as_array_mut()
        }
        _ => None,
    }
}

fn history_image_detail(detail: Option<&Value>) -> Option<Option<ImageDetail>> {
    match detail {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(detail)) => match detail.as_str() {
            "auto" => Some(Some(ImageDetail::Auto)),
            "low" => Some(Some(ImageDetail::Low)),
            "high" => Some(Some(ImageDetail::High)),
            "original" => Some(Some(ImageDetail::Original)),
            _ => None,
        },
        Some(_) => None,
    }
}

fn prepare_image(
    image_url: &mut String,
    detail: Option<ImageDetail>,
) -> Result<(), ImagePreparationError> {
    if is_remote_image_url(image_url) {
        return Err(ImagePreparationError::RemoteUrlUnsupported);
    }
    if !is_data_url(image_url) {
        return Ok(());
    }

    let limits = match detail {
        None | Some(ImageDetail::Auto | ImageDetail::High) => HIGH_DETAIL_LIMITS,
        Some(ImageDetail::Original) => ORIGINAL_DETAIL_LIMITS,
        Some(ImageDetail::Low) => return Err(ImagePreparationError::UnsupportedLowDetail),
    };
    let image =
        load_data_url_for_prompt(image_url, limits).map_err(ImagePreparationError::Processing)?;
    *image_url = image.into_data_url();
    Ok(())
}

fn is_remote_image_url(image_url: &str) -> bool {
    image_url.split_once(':').is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

fn is_data_url(image_url: &str) -> bool {
    image_url
        .get(.."data:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
}
