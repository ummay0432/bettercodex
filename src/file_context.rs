use crate::context::estimated_tokens;
use crate::context::mark_contextual_user_message;
use crate::context::message;
use crate::text::escape_cdata;
use crate::text::escape_xml_text;
use anyhow::Context;
use anyhow::anyhow;
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

pub(crate) const MAX_FILE_CONTEXT_TOKENS: u64 = 8_000;
pub(crate) const MAX_TURN_FILE_CONTEXT_TOKENS: u64 = 16_000;
const MAX_FILE_CONTEXT_FILES: usize = 16;
const MAX_FILE_CONTEXT_SOURCE_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InjectedFileContext {
    pub(crate) path: String,
    pub(crate) tokens: u64,
}

#[derive(Debug, Default)]
pub(crate) struct FileContextInjectionOutcome {
    pub(crate) items: Vec<serde_json::Value>,
    pub(crate) injected: Vec<InjectedFileContext>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn inject_selected_files(
    cwd: &Path,
    selected_files: &[PathBuf],
) -> FileContextInjectionOutcome {
    let mut outcome = FileContextInjectionOutcome::default();
    let mut seen = HashSet::new();
    let mut total_tokens = 0_u64;

    for selected in selected_files {
        if !seen.insert(selected.clone()) {
            continue;
        }
        if seen.len() > MAX_FILE_CONTEXT_FILES {
            outcome.warnings.push(format!(
                "Additional file injections were skipped after bettercodex's {MAX_FILE_CONTEXT_FILES}-file per-turn limit"
            ));
            break;
        }

        let display_path = selected.to_string_lossy().into_owned();
        let resolved = if selected.is_absolute() {
            selected.clone()
        } else {
            cwd.join(selected)
        };
        let contents = match read_utf8_file(&resolved) {
            Ok(contents) => contents,
            Err(FileContextReadError::TooLarge) => {
                outcome.warnings.push(format!(
                    "File injection skipped for `{display_path}`: it exceeds bettercodex's {MAX_FILE_CONTEXT_TOKENS}-token per-file limit"
                ));
                continue;
            }
            Err(FileContextReadError::NotUtf8) => {
                outcome.warnings.push(format!(
                    "File injection skipped for `{display_path}`: only UTF-8 text files are supported"
                ));
                continue;
            }
            Err(FileContextReadError::Other(error)) => {
                outcome.warnings.push(format!(
                    "File injection skipped for `{display_path}`: {error:#}"
                ));
                continue;
            }
        };

        let path = escape_xml_text(&display_path);
        let body = format!(
            "<file_context>\n<path>{path}</path>\n<contents><![CDATA[\n{}\n]]></contents>\n</file_context>",
            escape_cdata(&contents),
        );
        let mut item = message("user", body);
        mark_contextual_user_message(&mut item);
        let tokens = estimated_tokens(std::slice::from_ref(&item));
        if tokens > MAX_FILE_CONTEXT_TOKENS {
            outcome.warnings.push(format!(
                "File injection skipped for `{display_path}`: it is estimated at {tokens} tokens, exceeding bettercodex's {MAX_FILE_CONTEXT_TOKENS}-token per-file limit"
            ));
            continue;
        }
        if total_tokens.saturating_add(tokens) > MAX_TURN_FILE_CONTEXT_TOKENS {
            outcome.warnings.push(format!(
                "File injection skipped for `{display_path}`: it would exceed bettercodex's {MAX_TURN_FILE_CONTEXT_TOKENS}-token per-turn file limit"
            ));
            continue;
        }

        total_tokens = total_tokens.saturating_add(tokens);
        outcome.items.push(item);
        outcome.injected.push(InjectedFileContext {
            path: display_path,
            tokens,
        });
    }

    outcome
}

enum FileContextReadError {
    TooLarge,
    NotUtf8,
    Other(anyhow::Error),
}

fn read_utf8_file(path: &Path) -> std::result::Result<String, FileContextReadError> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = std::fs::OpenOptions::new();
    let file = options
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("failed to read {}", path.display()))
        .map_err(FileContextReadError::Other)?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))
        .map_err(FileContextReadError::Other)?;
    if !metadata.is_file() {
        return Err(FileContextReadError::Other(anyhow!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > u64::try_from(MAX_FILE_CONTEXT_SOURCE_BYTES).unwrap_or(u64::MAX) {
        return Err(FileContextReadError::TooLarge);
    }

    let read_limit = MAX_FILE_CONTEXT_SOURCE_BYTES.saturating_add(1);
    let capacity = usize::try_from(metadata.len())
        .unwrap_or(MAX_FILE_CONTEXT_SOURCE_BYTES)
        .min(MAX_FILE_CONTEXT_SOURCE_BYTES);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(u64::try_from(read_limit).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))
        .map_err(FileContextReadError::Other)?;
    if bytes.len() > MAX_FILE_CONTEXT_SOURCE_BYTES {
        return Err(FileContextReadError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| FileContextReadError::NotUtf8)
}
