use crate::skills::SkillMention;
use crate::skills::SkillSelection;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value;
use serde_json::json;
use std::borrow::Cow;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

pub(crate) const MAX_TOTAL_IMAGE_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UserPrompt {
    text: String,
    skill_mentions: Vec<SkillMention>,
    image_attachments: Vec<PromptImageAttachment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptImageAttachment {
    image: PromptImage,
    range: Range<usize>,
}

impl PromptImageAttachment {
    pub(crate) fn new(image: PromptImage, range: Range<usize>) -> Self {
        Self { image, range }
    }

    pub(crate) fn image(&self) -> &PromptImage {
        &self.image
    }

    pub(crate) fn range(&self) -> &Range<usize> {
        &self.range
    }

    pub(crate) fn range_mut(&mut self) -> &mut Range<usize> {
        &mut self.range
    }

    fn shifted(mut self, offset: usize) -> Self {
        self.range = self.range.start.saturating_add(offset)..self.range.end.saturating_add(offset);
        self
    }
}

impl UserPrompt {
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            skill_mentions: Vec::new(),
            image_attachments: Vec::new(),
        }
    }

    pub(crate) fn with_attachments(
        text: impl Into<String>,
        mut skill_mentions: Vec<SkillMention>,
        mut image_attachments: Vec<PromptImageAttachment>,
    ) -> Self {
        skill_mentions.sort_by_key(|mention| mention.range().start);
        image_attachments.sort_by_key(|attachment| attachment.range.start);
        Self {
            text: text.into(),
            skill_mentions,
            image_attachments,
        }
    }

    pub(crate) fn joined(prompts: Vec<Self>) -> Self {
        let mut text = String::new();
        let mut skill_mentions = Vec::new();
        let mut image_attachments = Vec::new();
        for prompt in prompts {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            let offset = text.len();
            text.push_str(&prompt.text);
            skill_mentions.extend(
                prompt
                    .skill_mentions
                    .into_iter()
                    .map(|mention| mention.shifted(offset)),
            );
            image_attachments.extend(
                prompt
                    .image_attachments
                    .into_iter()
                    .map(|attachment| attachment.shifted(offset)),
            );
        }
        Self {
            text,
            skill_mentions,
            image_attachments,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn skill_mentions(&self) -> &[SkillMention] {
        &self.skill_mentions
    }

    pub(crate) fn image_attachments(&self) -> &[PromptImageAttachment] {
        &self.image_attachments
    }

    pub(crate) fn image_count(&self) -> usize {
        self.image_attachments.len()
    }

    pub(crate) fn text_without_image_placeholders(&self) -> Cow<'_, str> {
        if self.image_attachments.is_empty() {
            return Cow::Borrowed(&self.text);
        }
        Cow::Owned(text_without_image_placeholders(
            &self.text,
            &self.image_attachments,
        ))
    }

    pub(crate) fn into_parts(self) -> (String, Vec<SkillSelection>, Vec<PromptImage>) {
        let Self {
            text,
            skill_mentions,
            image_attachments,
        } = self;
        let text = if image_attachments.is_empty() {
            // Ordinary text prompts are overwhelmingly common. Move their existing allocation
            // directly into model input rather than cloning it immediately before submission.
            text
        } else {
            text_without_image_placeholders(&text, &image_attachments)
        };
        (
            text,
            skill_mentions
                .into_iter()
                .map(|mention| mention.selection().clone())
                .collect(),
            image_attachments
                .into_iter()
                .map(|attachment| attachment.image)
                .collect(),
        )
    }
}

fn text_without_image_placeholders(
    source: &str,
    image_attachments: &[PromptImageAttachment],
) -> String {
    let mut text = String::with_capacity(source.len());
    let mut cursor = 0;
    for attachment in image_attachments {
        if attachment.range.start < cursor || attachment.range.end > source.len() {
            continue;
        }
        text.push_str(&source[cursor..attachment.range.start]);
        cursor = attachment.range.end;
    }
    text.push_str(&source[cursor..]);
    // Preserve the existing trim semantics without allocating a second full prompt buffer.
    let trimmed_start = text.len().saturating_sub(text.trim_start().len());
    let trimmed_len = text.trim().len();
    text.truncate(trimmed_start + trimmed_len);
    text.drain(..trimmed_start);
    text
}

impl From<&str> for UserPrompt {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

impl From<String> for UserPrompt {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ImageDetail {
    #[default]
    High,
    Original,
}

impl ImageDetail {
    fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Original => "original",
        }
    }
}

impl fmt::Display for ImageDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptImage {
    bytes: Arc<[u8]>,
    source: PathBuf,
    detail: ImageDetail,
}

impl PromptImage {
    pub(crate) fn from_path(path: &Path, detail: ImageDetail) -> Result<Self> {
        let bytes = read_image(path, MAX_TOTAL_IMAGE_BYTES)?;
        Self::from_bytes(path, bytes, detail)
    }

    pub(crate) fn from_bytes(path: &Path, bytes: Vec<u8>, detail: ImageDetail) -> Result<Self> {
        image_mime(path, &bytes)?;
        Ok(Self {
            bytes: Arc::from(bytes),
            source: path.to_path_buf(),
            detail,
        })
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    fn into_value(self) -> Result<Value> {
        let limits = match self.detail {
            ImageDetail::High => crate::image::HIGH_DETAIL_LIMITS,
            ImageDetail::Original => crate::image::ORIGINAL_DETAIL_LIMITS,
        };
        let image_url = crate::image::load_for_prompt_bytes(self.bytes, limits)
            .with_context(|| format!("failed to prepare image {}", self.source.display()))?
            .into_data_url();
        Ok(json!({
            "type": "input_image",
            "image_url": image_url,
            "detail": self.detail.as_str(),
        }))
    }
}

pub(crate) fn image_size_error() -> anyhow::Error {
    anyhow!(
        "attached images exceed bettercodex's {} MiB input limit",
        MAX_TOTAL_IMAGE_BYTES / (1024 * 1024)
    )
}

impl FromStr for ImageDetail {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "high" => Ok(Self::High),
            "original" => Ok(Self::Original),
            _ => Err(anyhow!(
                "invalid image detail `{value}`; use high or original"
            )),
        }
    }
}

#[derive(Debug)]
pub(crate) struct UserInput {
    text: String,
    images: Vec<PromptImage>,
    selected_skills: Vec<SkillSelection>,
}

impl UserInput {
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
            selected_skills: Vec::new(),
        }
    }

    pub(crate) fn prompt(prompt: UserPrompt) -> Self {
        let (text, selected_skills, images) = prompt.into_parts();
        Self {
            text,
            images,
            selected_skills,
        }
    }

    /// Prepare model input while the original prompt remains queued until a steer is committed.
    pub(crate) fn prompt_ref(prompt: &UserPrompt) -> Self {
        Self {
            text: prompt.text_without_image_placeholders().into_owned(),
            images: prompt
                .image_attachments()
                .iter()
                .map(|attachment| attachment.image().clone())
                .collect(),
            selected_skills: prompt
                .skill_mentions()
                .iter()
                .map(|mention| mention.selection().clone())
                .collect(),
        }
    }

    pub(crate) fn from_paths(
        text: impl Into<String>,
        paths: &[PathBuf],
        detail: ImageDetail,
    ) -> Result<Self> {
        let mut total = 0_usize;
        let mut images = Vec::with_capacity(paths.len());
        for path in paths {
            let bytes = read_image(path, MAX_TOTAL_IMAGE_BYTES.saturating_sub(total))?;
            total = total.saturating_add(bytes.len());
            let image = PromptImage::from_bytes(path, bytes, detail)?;
            images.push(image);
        }
        let input = Self {
            text: text.into(),
            images,
            selected_skills: Vec::new(),
        };
        if input.is_empty() {
            return Err(anyhow!("prompt and image list are both empty"));
        }
        Ok(input)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty()
    }

    pub(crate) fn has_images(&self) -> bool {
        !self.images.is_empty()
    }

    pub(crate) fn into_message_and_skills(self) -> Result<(Value, String, Vec<SkillSelection>)> {
        let Self {
            text,
            images,
            selected_skills,
        } = self;
        let mut content = Vec::with_capacity(images.len() + 1);
        if !text.trim().is_empty() {
            content.push(json!({"type": "input_text", "text": &text}));
        }
        for image in images {
            content.push(image.into_value()?);
        }
        Ok((
            json!({
                "type": "message",
                "role": "user",
                "content": content,
            }),
            text,
            selected_skills,
        ))
    }
}

fn read_image(path: &Path, remaining: usize) -> Result<Vec<u8>> {
    let file =
        File::open(path).with_context(|| format!("failed to read image {}", path.display()))?;
    let reported_length = file
        .metadata()
        .with_context(|| format!("failed to inspect image {}", path.display()))?
        .len();
    if reported_length > u64::try_from(remaining).unwrap_or(u64::MAX) {
        return Err(image_size_error());
    }

    // The metadata check avoids allocating for an already-oversized file. The bounded read also
    // covers files that grow between metadata collection and the final byte read.
    let capacity = usize::try_from(reported_length)
        .unwrap_or(remaining)
        .min(remaining);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(u64::try_from(remaining.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read image {}", path.display()))?;
    if bytes.len() > remaining {
        return Err(image_size_error());
    }
    Ok(bytes)
}

pub(crate) fn image_mime(path: &Path, bytes: &[u8]) -> Result<&'static str> {
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    };
    mime.ok_or_else(|| {
        anyhow!(
            "{} is not a supported PNG, JPEG, WEBP, or GIF image",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use image::DynamicImage;
    use image::GenericImageView;
    use image::ImageBuffer;
    use image::ImageFormat;
    use image::Rgba;
    use std::io::Cursor;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = ImageBuffer::from_pixel(width, height, Rgba([10_u8, 20, 30, 255]));
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode test image");
        encoded.into_inner()
    }

    fn image_dimensions(message: &Value) -> (u32, u32) {
        let image_url = message["content"][0]["image_url"]
            .as_str()
            .expect("image data URL");
        let (_, payload) = image_url.split_once(',').expect("image data URL payload");
        let bytes = STANDARD.decode(payload).expect("decode image data URL");
        image::load_from_memory(&bytes)
            .expect("decode prepared image")
            .dimensions()
    }

    #[test]
    fn image_detail_defaults_to_high_and_parses_user_selectable_values() {
        assert_eq!(ImageDetail::default(), ImageDetail::High);
        for value in ["high", "original"] {
            assert_eq!(value.parse::<ImageDetail>().unwrap().to_string(), value);
        }
        for value in ["low", "auto", "full"] {
            assert!(value.parse::<ImageDetail>().is_err());
        }
    }

    #[test]
    fn text_input_uses_the_responses_message_shape() {
        assert_eq!(
            UserInput::text("inspect")
                .into_message_and_skills()
                .unwrap()
                .0,
            json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "inspect"}],
            })
        );
    }

    #[test]
    fn image_only_input_embeds_a_typed_data_url_and_detail() {
        let path = std::env::temp_dir().join(format!(
            "bettercodex-image-{}-{}.png",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, png_bytes(1, 1)).unwrap();

        let message = UserInput::from_paths("", std::slice::from_ref(&path), ImageDetail::High)
            .unwrap()
            .into_message_and_skills()
            .unwrap()
            .0;
        assert_eq!(message["role"], "user");
        assert_eq!(message["content"][0]["type"], "input_image");
        assert_eq!(message["content"][0]["detail"], "high");
        assert!(
            message["content"][0]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn user_images_apply_upstream_detail_budgets_before_serialization() {
        let message_for = |source: &[u8], detail| {
            let image = PromptImage::from_bytes(Path::new("fixture.png"), source.to_vec(), detail)
                .expect("load image");
            UserInput {
                text: String::new(),
                images: vec![image],
                selected_skills: Vec::new(),
            }
            .into_message_and_skills()
            .expect("prepare image")
            .0
        };

        let square = png_bytes(2048, 2048);
        let high = message_for(&square, ImageDetail::default());
        assert_eq!(high["content"][0]["detail"], "high");
        assert_eq!(image_dimensions(&high), (1600, 1600));

        let original = message_for(&square, ImageDetail::Original);
        assert_eq!(original["content"][0]["detail"], "original");
        assert_eq!(image_dimensions(&original), (2048, 2048));

        let wide = png_bytes(6401, 100);
        let bounded_original = message_for(&wide, ImageDetail::Original);
        assert_eq!(image_dimensions(&bounded_original), (6000, 94));
    }

    #[test]
    fn unknown_image_bytes_are_rejected_before_the_request() {
        let path = std::env::temp_dir().join(format!(
            "bettercodex-image-{}-{}.bin",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"not an image").unwrap();
        let error =
            UserInput::from_paths("inspect", std::slice::from_ref(&path), ImageDetail::High)
                .unwrap_err();
        assert!(error.to_string().contains("not a supported"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn oversized_image_is_rejected_before_it_is_loaded() {
        let path = std::env::temp_dir().join(format!(
            "bettercodex-oversized-image-{}-{}.png",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let file = File::create(&path).unwrap();
        file.set_len(u64::try_from(MAX_TOTAL_IMAGE_BYTES).unwrap() + 1)
            .unwrap();

        let error =
            UserInput::from_paths("inspect", std::slice::from_ref(&path), ImageDetail::High)
                .unwrap_err();

        assert!(error.to_string().contains("50 MiB input limit"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn aggregate_image_limit_is_checked_before_loading_the_next_file() {
        let directory = std::env::temp_dir().join(format!(
            "bettercodex-image-total-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let first = directory.join("first.png");
        let second = directory.join("second.png");
        std::fs::write(&first, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let file = File::create(&second).unwrap();
        file.set_len(u64::try_from(MAX_TOTAL_IMAGE_BYTES).unwrap())
            .unwrap();

        let error =
            UserInput::from_paths("inspect", &[first, second], ImageDetail::High).unwrap_err();

        assert!(error.to_string().contains("50 MiB input limit"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
