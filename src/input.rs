use crate::skills::SkillSelection;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use serde_json::json;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

const MAX_TOTAL_IMAGE_BYTES: usize = 50 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UserPrompt {
    text: String,
    skills: Vec<SkillSelection>,
}

impl UserPrompt {
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            skills: Vec::new(),
        }
    }

    pub(crate) fn with_skills(text: impl Into<String>, skills: Vec<SkillSelection>) -> Self {
        Self {
            text: text.into(),
            skills,
        }
    }

    pub(crate) fn joined(prompts: Vec<Self>) -> Self {
        let mut text = String::new();
        let mut skills = Vec::new();
        for prompt in prompts {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&prompt.text);
            skills.extend(prompt.skills);
        }
        Self { text, skills }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn skills(&self) -> &[SkillSelection] {
        &self.skills
    }

    pub(crate) fn into_parts(self) -> (String, Vec<SkillSelection>) {
        (self.text, self.skills)
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
        let (text, selected_skills) = prompt.into_parts();
        Self {
            text,
            images: Vec::new(),
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
            let bytes = std::fs::read(path)
                .with_context(|| format!("failed to read image {}", path.display()))?;
            total = total
                .checked_add(bytes.len())
                .filter(|total| *total <= MAX_TOTAL_IMAGE_BYTES)
                .ok_or_else(|| {
                    anyhow!(
                        "attached images exceed BetterCodex's {} MiB input limit",
                        MAX_TOTAL_IMAGE_BYTES / (1024 * 1024)
                    )
                })?;
            let mime = image_mime(path, &bytes)?;
            let image_url = format!("data:{mime};base64,{}", STANDARD.encode(bytes));
            images.push(json!({
                "type": "input_image",
                "image_url": image_url,
                "detail": detail.as_str(),
            }));
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
}
