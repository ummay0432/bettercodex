//! Model-facing tool image preparation ported from Codex at
//! `1669c2403f793d0230065397dfc25f52b844244e`,
//! `codex-rs/core/src/image_preparation.rs`.
//!
//! Codex deliberately leaves `view_image` results as opaque data URLs inside
//! the exec runtime, then validates and prepares any image the JavaScript
//! program chooses to return before inserting the outer tool result into history.

use crate::image::ImageProcessingError;
use crate::image::PromptImageResizeLimits;
use crate::image::load_data_url_for_prompt;
use crate::protocol::FunctionCallOutputContentItem;
use crate::protocol::ImageDetail;

const IMAGE_PROCESSING_ERROR_PLACEHOLDER: &str =
    "image content omitted because it could not be processed";
const IMAGE_TOO_LARGE_PLACEHOLDER: &str =
    "image content omitted because it exceeded the supported size limit; use a smaller image";
const UNSUPPORTED_LOW_DETAIL_PLACEHOLDER: &str = "image content omitted because detail 'low' is not supported; use 'high', 'original', or 'auto'";
const REMOTE_IMAGE_URL_PLACEHOLDER: &str =
    "image content omitted because remote image URLs are not supported";

const HIGH_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 2048,
    max_patches: 2_500,
};
const ORIGINAL_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 6000,
    max_patches: 10_000,
};

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

pub(super) fn prepare_tool_output_images(items: &mut [FunctionCallOutputContentItem]) {
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
