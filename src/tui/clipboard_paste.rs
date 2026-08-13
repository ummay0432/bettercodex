//! Pasted-path image ingestion for the interactive composer.

use crate::input::ImageDetail;
use crate::input::PromptImage;
use crate::input::image_mime;
use std::io::Read;
use std::path::PathBuf;

/// Interpret an explicit terminal paste as an image only when it names an existing supported file.
/// Ordinary paths and prose continue through the text-paste path unchanged.
pub(super) fn image_from_pasted_path(pasted: &str) -> Option<Result<PromptImage, String>> {
    let path = normalize_pasted_path(pasted)?;
    if !path.is_file() {
        return None;
    }
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) => {
            return Some(Err(format!(
                "could not read pasted image {}: {error}",
                path.display()
            )));
        }
    };
    let mut signature = [0_u8; 12];
    let signature_len = match file.read(&mut signature) {
        Ok(len) => len,
        Err(error) => {
            return Some(Err(format!(
                "could not read pasted image {}: {error}",
                path.display()
            )));
        }
    };
    if image_mime(&path, &signature[..signature_len]).is_err() {
        return None;
    }
    Some(PromptImage::from_path(&path, ImageDetail::default()).map_err(|error| error.to_string()))
}

fn normalize_pasted_path(pasted: &str) -> Option<PathBuf> {
    let pasted = pasted.trim();
    if pasted.is_empty() || pasted.contains(['\r', '\n']) {
        return None;
    }
    let unquoted = pasted
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            pasted
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(pasted);
    if let Ok(url) = url::Url::parse(unquoted)
        && url.scheme() == "file"
    {
        return url.to_file_path().ok();
    }
    Some(PathBuf::from(unquoted))
}
