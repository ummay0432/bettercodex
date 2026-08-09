use crate::skills::SkillMention;
use crate::skills::SkillSelection;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use serde_json::json;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

pub(crate) const MAX_TOTAL_IMAGE_BYTES: usize = 50 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
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

    pub(crate) fn text_without_image_placeholders(&self) -> String {
        if self.image_attachments.is_empty() {
            return self.text.clone();
        }
        let mut text = String::with_capacity(self.text.len());
        let mut cursor = 0;
        for attachment in &self.image_attachments {
            if attachment.range.start < cursor || attachment.range.end > self.text.len() {
                continue;
            }
            text.push_str(&self.text[cursor..attachment.range.start]);
            cursor = attachment.range.end;
        }
        text.push_str(&self.text[cursor..]);
        text.trim().to_string()
    }

    pub(crate) fn into_parts(self) -> (String, Vec<SkillSelection>, Vec<PromptImage>) {
        let text = self.text_without_image_placeholders();
        (
            text,
            self.skill_mentions
                .into_iter()
                .map(|mention| mention.selection().clone())
                .collect(),
            self.image_attachments
                .into_iter()
                .map(|attachment| attachment.image)
                .collect(),
        )
    }
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
    Low,
    High,
    #[default]
    Original,
    Auto,
}

impl ImageDetail {
    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
            Self::Original => "original",
            Self::Auto => "auto",
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
    mime: &'static str,
    detail: ImageDetail,
}

impl PromptImage {
    pub(crate) fn from_path(path: &Path, detail: ImageDetail) -> Result<Self> {
        let mut file =
            File::open(path).with_context(|| format!("failed to read image {}", path.display()))?;
        let declared_len = file
            .metadata()
            .with_context(|| format!("failed to inspect image {}", path.display()))?
            .len();
        if declared_len > MAX_TOTAL_IMAGE_BYTES as u64 {
            return Err(image_size_error());
        }
        let mut bytes = Vec::with_capacity(declared_len.try_into().unwrap_or(0));
        file.by_ref()
            .take(MAX_TOTAL_IMAGE_BYTES.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read image {}", path.display()))?;
        if bytes.len() > MAX_TOTAL_IMAGE_BYTES {
            return Err(image_size_error());
        }
        Self::from_bytes(path, bytes, detail)
    }

    pub(crate) fn from_bytes(path: &Path, bytes: Vec<u8>, detail: ImageDetail) -> Result<Self> {
        let mime = image_mime(path, &bytes)?;
        Ok(Self {
            bytes: Arc::from(bytes),
            mime,
            detail,
        })
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    fn into_value(self) -> Value {
        let image_url = format!(
            "data:{};base64,{}",
            self.mime,
            STANDARD.encode(self.bytes.as_ref())
        );
        json!({
            "type": "input_image",
            "image_url": image_url,
            "detail": self.detail.as_str(),
        })
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
            "low" => Ok(Self::Low),
            "high" => Ok(Self::High),
            "original" => Ok(Self::Original),
            "auto" => Ok(Self::Auto),
            _ => Err(anyhow!(
                "invalid image detail `{value}`; use low, high, original, or auto"
            )),
        }
    }
}

#[derive(Debug)]
pub(crate) struct UserInput {
    text: String,
    images: Vec<Value>,
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
            images: images.into_iter().map(PromptImage::into_value).collect(),
            selected_skills,
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
            images.push(image.into_value());
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

    pub(crate) fn into_message_and_skills(self) -> (Value, String, Vec<SkillSelection>) {
        let mut content = Vec::with_capacity(self.images.len() + 1);
        if !self.text.trim().is_empty() {
            content.push(json!({"type": "input_text", "text": &self.text}));
        }
        content.extend(self.images);
        (
            json!({
                "type": "message",
                "role": "user",
                "content": content,
            }),
            self.text,
            self.selected_skills,
        )
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

    #[test]
    fn image_detail_parses_every_supported_wire_value() {
        for value in ["low", "high", "original", "auto"] {
            assert_eq!(value.parse::<ImageDetail>().unwrap().to_string(), value);
        }
        assert!("full".parse::<ImageDetail>().is_err());
    }

    #[test]
    fn text_input_uses_the_responses_message_shape() {
        assert_eq!(
            UserInput::text("inspect").into_message_and_skills().0,
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
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nfixture").unwrap();

        let message = UserInput::from_paths("", std::slice::from_ref(&path), ImageDetail::High)
            .unwrap()
            .into_message_and_skills()
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
    fn unknown_image_bytes_are_rejected_before_the_request() {
        let path = std::env::temp_dir().join(format!(
            "bettercodex-image-{}-{}.bin",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"not an image").unwrap();
        let error = UserInput::from_paths("inspect", std::slice::from_ref(&path), ImageDetail::Low)
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
