//! Focused prompt-image preparation retained from OpenAI Codex commit
//! 902bd9e06b3ecb32cbf7f8e64cd23b956be3e7fe.
//!
//! bettercodex snapshots user attachments before submission and receives tool
//! images as data URLs, so the upstream utility's path-reading API is omitted.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::ColorType;
use image::DynamicImage;
use image::GenericImageView;
use image::ImageDecoder;
use image::ImageEncoder;
use image::ImageFormat;
use image::ImageReader;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use lru::LruCache;
use sha1::Digest;
use sha1::Sha1;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use thiserror::Error;

const DATA_URL_PREFIX: &str = "data:";
const PROMPT_IMAGE_PATCH_SIZE: u32 = 32;
const MAX_PROMPT_IMAGE_INPUT_BYTES: usize = 1024 * 1024 * 1024;
const MAX_IMAGE_CACHE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) const HIGH_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 2048,
    max_patches: 2_500,
};
pub(crate) const ORIGINAL_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 6000,
    max_patches: 10_000,
};

#[derive(Debug, Error)]
pub(crate) enum ImageProcessingError {
    #[error("failed to decode image at <data-url-image>: {0}")]
    Decode(#[source] image::ImageError),
    #[error("failed to encode image as {format:?}: {source}")]
    Encode {
        format: ImageFormat,
        #[source]
        source: image::ImageError,
    },
    #[error("unsupported image `unknown`")]
    UnsupportedImageFormat,
    #[error("invalid image data URL: {reason}")]
    InvalidDataUrl { reason: String },
    #[error("image {representation} is too large ({size} bytes; max {max} bytes)")]
    ImageTooLarge {
        representation: &'static str,
        size: usize,
        max: usize,
    },
}

impl ImageProcessingError {
    fn decode_error(source: image::ImageError) -> Self {
        if matches!(source, image::ImageError::Decoding(_)) {
            Self::Decode(source)
        } else {
            Self::UnsupportedImageFormat
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EncodedImage {
    bytes: Arc<[u8]>,
    mime: String,
}

impl EncodedImage {
    pub(crate) fn into_data_url(self) -> String {
        data_url_from_bytes(&self.mime, &self.bytes)
    }
}

pub(crate) fn data_url_from_bytes(mime: &str, bytes: &[u8]) -> String {
    let encoded = BASE64_STANDARD.encode(bytes);
    format!("data:{mime};base64,{encoded}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PromptImageResizeLimits {
    pub(crate) max_dimension: u32,
    pub(crate) max_patches: usize,
}

struct ImageMetadata {
    icc_profile: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ImageCacheKey {
    digest: [u8; 20],
    limits: PromptImageResizeLimits,
}

// Upstream shares a Tokio-aware cache utility across several crates. bettercodex has one
// synchronous image cache, so a dedicated standard mutex keeps this path independent of runtime
// context while holding the lock only for short LRU operations.
struct ImageCache {
    inner: Mutex<LruCache<ImageCacheKey, EncodedImage>>,
}

impl ImageCache {
    fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(capacity)),
        }
    }

    fn get(&self, key: &ImageCacheKey) -> Option<EncodedImage> {
        self.lock().get(key).cloned()
    }

    fn insert_bounded(&self, key: ImageCacheKey, image: EncodedImage, byte_capacity: usize) {
        if image.bytes.len() > byte_capacity {
            return;
        }

        let mut cache = self.lock();
        cache.put(key, image);
        let mut cached_bytes = cache
            .iter()
            .map(|(_, image)| image.bytes.len())
            .sum::<usize>();
        while cached_bytes > byte_capacity {
            let Some((_, evicted)) = cache.pop_lru() else {
                break;
            };
            cached_bytes -= evicted.bytes.len();
        }
    }

    fn lock(&self) -> MutexGuard<'_, LruCache<ImageCacheKey, EncodedImage>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

static IMAGE_CACHE: LazyLock<ImageCache> =
    LazyLock::new(|| ImageCache::new(NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN)));

fn sha1_digest(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut output = [0; 20];
    output.copy_from_slice(&result);
    output
}

pub(crate) fn load_data_url_for_prompt(
    image_url: &str,
    limits: PromptImageResizeLimits,
) -> Result<EncodedImage, ImageProcessingError> {
    let rest = image_url
        .get(..DATA_URL_PREFIX.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(DATA_URL_PREFIX))
        .and_then(|_| image_url.get(DATA_URL_PREFIX.len()..))
        .ok_or_else(|| ImageProcessingError::InvalidDataUrl {
            reason: "missing data: prefix".to_string(),
        })?;
    let (metadata, encoded) =
        rest.split_once(',')
            .ok_or_else(|| ImageProcessingError::InvalidDataUrl {
                reason: "missing comma separator".to_string(),
            })?;
    if !metadata
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"))
    {
        return Err(ImageProcessingError::InvalidDataUrl {
            reason: "only base64 data URLs are supported".to_string(),
        });
    }
    if encoded.len() > MAX_PROMPT_IMAGE_INPUT_BYTES {
        return Err(ImageProcessingError::ImageTooLarge {
            representation: "base64 payload",
            size: encoded.len(),
            max: MAX_PROMPT_IMAGE_INPUT_BYTES,
        });
    }
    let file_bytes =
        BASE64_STANDARD
            .decode(encoded)
            .map_err(|source| ImageProcessingError::InvalidDataUrl {
                reason: format!("invalid base64 payload: {source}"),
            })?;
    if file_bytes.len() > MAX_PROMPT_IMAGE_INPUT_BYTES {
        return Err(ImageProcessingError::ImageTooLarge {
            representation: "decoded input",
            size: file_bytes.len(),
            max: MAX_PROMPT_IMAGE_INPUT_BYTES,
        });
    }

    load_for_prompt_bytes(file_bytes.into(), limits)
}

pub(crate) fn load_for_prompt_bytes(
    file_bytes: Arc<[u8]>,
    limits: PromptImageResizeLimits,
) -> Result<EncodedImage, ImageProcessingError> {
    let key = ImageCacheKey {
        digest: sha1_digest(&file_bytes),
        limits,
    };
    if let Some(image) = IMAGE_CACHE.get(&key) {
        return Ok(image);
    }

    let image = (move || {
        let guessed_format =
            image::guess_format(&file_bytes).map_err(ImageProcessingError::decode_error)?;
        let source_format = match guessed_format {
            ImageFormat::Png => Some(ImageFormat::Png),
            ImageFormat::Jpeg => Some(ImageFormat::Jpeg),
            ImageFormat::Gif => Some(ImageFormat::Gif),
            ImageFormat::WebP => Some(ImageFormat::WebP),
            _ => None,
        };
        let mut decoder = ImageReader::with_format(Cursor::new(&file_bytes), guessed_format)
            .into_decoder()
            .map_err(ImageProcessingError::decode_error)?;
        let metadata = ImageMetadata {
            icc_profile: decoder
                .icc_profile()
                .ok()
                .flatten()
                .filter(|profile| profile.get(16..20) == Some(b"RGB ")),
            exif: decoder.exif_metadata().ok().flatten(),
        };
        let dynamic =
            DynamicImage::from_decoder(decoder).map_err(ImageProcessingError::decode_error)?;
        let (width, height) = dynamic.dimensions();
        let (target_width, target_height) =
            prompt_image_output_dimensions_for_limits(width, height, limits);

        let encoded = if (target_width, target_height) == (width, height) {
            if let Some(format) = source_format.filter(|format| can_preserve_source_bytes(*format))
            {
                EncodedImage {
                    bytes: file_bytes,
                    mime: format_to_mime(format),
                }
            } else {
                let (bytes, format) = encode_image(&dynamic, ImageFormat::Png, metadata)?;
                EncodedImage {
                    bytes: bytes.into(),
                    mime: format_to_mime(format),
                }
            }
        } else {
            let resized = dynamic.resize_exact(target_width, target_height, FilterType::Triangle);
            let target_format = source_format
                .filter(|format| can_preserve_source_bytes(*format))
                .unwrap_or(ImageFormat::Png);
            let (bytes, format) = encode_image(&resized, target_format, metadata)?;
            EncodedImage {
                bytes: bytes.into(),
                mime: format_to_mime(format),
            }
        };
        Ok(encoded)
    })()?;

    IMAGE_CACHE.insert_bounded(key, image.clone(), MAX_IMAGE_CACHE_BYTES);
    Ok(image)
}

fn prompt_image_output_dimensions_for_limits(
    width: u32,
    height: u32,
    limits: PromptImageResizeLimits,
) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    if prompt_image_dimensions_fit(width, height, limits) {
        return (width, height);
    }

    let max_dimension_scale =
        (f64::from(limits.max_dimension) / f64::from(width.max(height))).min(1.0);
    let width = ((f64::from(width) * max_dimension_scale).round() as u32).max(1);
    let height = ((f64::from(height) * max_dimension_scale).round() as u32).max(1);
    if prompt_image_dimensions_fit(width, height, limits) {
        return (width, height);
    }

    let width_f64 = f64::from(width);
    let height_f64 = f64::from(height);
    let patch_size = f64::from(PROMPT_IMAGE_PATCH_SIZE);
    let mut scale =
        (patch_size * patch_size * limits.max_patches as f64 / width_f64 / height_f64).sqrt();
    let scaled_patches_wide = width_f64 * scale / patch_size;
    let scaled_patches_high = height_f64 * scale / patch_size;
    scale *= (scaled_patches_wide.floor() / scaled_patches_wide)
        .min(scaled_patches_high.floor() / scaled_patches_high);

    (
        ((width_f64 * scale).floor() as u32).max(1),
        ((height_f64 * scale).floor() as u32).max(1),
    )
}

fn prompt_image_dimensions_fit(width: u32, height: u32, limits: PromptImageResizeLimits) -> bool {
    let patches_wide = width.div_ceil(PROMPT_IMAGE_PATCH_SIZE);
    let patches_high = height.div_ceil(PROMPT_IMAGE_PATCH_SIZE);
    let patch_count = u64::from(patches_wide) * u64::from(patches_high);
    width <= limits.max_dimension
        && height <= limits.max_dimension
        && patch_count <= limits.max_patches as u64
}

fn can_preserve_source_bytes(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    )
}

fn encode_image(
    image: &DynamicImage,
    preferred_format: ImageFormat,
    metadata: ImageMetadata,
) -> Result<(Vec<u8>, ImageFormat), ImageProcessingError> {
    let target_format = match preferred_format {
        ImageFormat::Jpeg => ImageFormat::Jpeg,
        ImageFormat::WebP => ImageFormat::WebP,
        _ => ImageFormat::Png,
    };
    let mut buffer = Vec::new();
    let ImageMetadata { icc_profile, exif } = metadata;

    match target_format {
        ImageFormat::Png => {
            let rgba = image.to_rgba8();
            let mut encoder = PngEncoder::new(&mut buffer);
            apply_image_metadata(&mut encoder, icc_profile, exif, target_format)?;
            encoder
                .write_image(
                    rgba.as_raw(),
                    image.width(),
                    image.height(),
                    ColorType::Rgba8.into(),
                )
                .map_err(|source| ImageProcessingError::Encode {
                    format: target_format,
                    source,
                })?;
        }
        ImageFormat::Jpeg => {
            let mut encoder = JpegEncoder::new_with_quality(&mut buffer, 85);
            apply_image_metadata(&mut encoder, icc_profile, exif, target_format)?;
            encoder
                .encode_image(image)
                .map_err(|source| ImageProcessingError::Encode {
                    format: target_format,
                    source,
                })?;
        }
        ImageFormat::WebP => {
            let rgba = image.to_rgba8();
            let mut encoder = WebPEncoder::new_lossless(&mut buffer);
            apply_image_metadata(&mut encoder, icc_profile, exif, target_format)?;
            encoder
                .write_image(
                    rgba.as_raw(),
                    image.width(),
                    image.height(),
                    ColorType::Rgba8.into(),
                )
                .map_err(|source| ImageProcessingError::Encode {
                    format: target_format,
                    source,
                })?;
        }
        _ => unreachable!("unsupported image output format"),
    }
    Ok((buffer, target_format))
}

fn apply_image_metadata(
    encoder: &mut impl ImageEncoder,
    icc_profile: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
    format: ImageFormat,
) -> Result<(), ImageProcessingError> {
    if let Some(icc_profile) = icc_profile {
        encoder
            .set_icc_profile(icc_profile)
            .map_err(|source| ImageProcessingError::Encode {
                format,
                source: image::ImageError::Unsupported(source),
            })?;
    }
    if let Some(exif) = exif {
        encoder
            .set_exif_metadata(exif)
            .map_err(|source| ImageProcessingError::Encode {
                format,
                source: image::ImageError::Unsupported(source),
            })?;
    }
    Ok(())
}

fn format_to_mime(format: ImageFormat) -> String {
    match format {
        ImageFormat::Jpeg => "image/jpeg".to_string(),
        ImageFormat::WebP => "image/webp".to_string(),
        _ => "image/png".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageBuffer;
    use image::Rgba;

    const LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
        max_dimension: 2048,
        max_patches: 2_500,
    };

    fn image_bytes(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let image = ImageBuffer::from_pixel(width, height, Rgba([10_u8, 20, 30, 255]));
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, format)
            .expect("encode test image");
        encoded.into_inner()
    }

    #[test]
    fn preserves_supported_image_within_limits() {
        let original = image_bytes(64, 32, ImageFormat::Png);
        let image = load_for_prompt_bytes(original.clone().into(), LIMITS).expect("process image");
        assert_eq!(image.mime, "image/png");
        assert_eq!(image.bytes.as_ref(), original);
    }

    #[test]
    fn enforces_dimension_and_patch_budgets() {
        let original = image_bytes(2048, 2048, ImageFormat::Png);
        let image = load_for_prompt_bytes(original.into(), LIMITS).expect("process image");
        let decoded = image::load_from_memory(&image.bytes).expect("decode output");
        assert_eq!(decoded.dimensions(), (1600, 1600));
    }

    #[test]
    fn bounds_cache_by_encoded_byte_size() {
        let cache = ImageCache::new(NonZeroUsize::new(4).expect("non-zero cache capacity"));
        let key = |digest_byte| ImageCacheKey {
            digest: [digest_byte; 20],
            limits: LIMITS,
        };
        let image = |size| EncodedImage {
            bytes: vec![0; size].into(),
            mime: "image/png".to_string(),
        };

        cache.insert_bounded(key(1), image(3), /*byte_capacity*/ 5);
        cache.insert_bounded(key(2), image(3), /*byte_capacity*/ 5);
        cache.insert_bounded(key(3), image(6), /*byte_capacity*/ 5);

        assert!(cache.get(&key(1)).is_none());
        assert!(cache.get(&key(2)).is_some());
        assert!(cache.get(&key(3)).is_none());
    }

    #[test]
    fn rejects_malformed_data_urls() {
        for image_url in [
            "image/png;base64,AAAA",
            "data:image/png;base64",
            "data:image/png,AAAA",
            "data:image/png;base64,not base64",
        ] {
            assert!(matches!(
                load_data_url_for_prompt(image_url, LIMITS),
                Err(ImageProcessingError::InvalidDataUrl { .. })
            ));
        }
    }
}
