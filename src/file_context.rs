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

pub(crate) const MAX_FILE_CONTEXT_ITEM_TOKENS: u64 = 10_000;
const APPROXIMATE_BYTES_PER_TOKEN: u64 = 4;

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
    max_total_tokens: u64,
) -> FileContextInjectionOutcome {
    let mut outcome = FileContextInjectionOutcome::default();
    let mut seen = HashSet::new();
    let mut total_tokens = 0_u64;

    for selected in selected_files {
        if !seen.insert(selected.clone()) {
            continue;
        }

        let remaining_tokens = max_total_tokens.saturating_sub(total_tokens);
        if remaining_tokens == 0 {
            outcome.warnings.push(format!(
                "Additional file injections were skipped because the selected files already fill bettercodex's {max_total_tokens}-token effective context window"
            ));
            break;
        }

        let display_path = selected.to_string_lossy().into_owned();
        let resolved = if selected.is_absolute() {
            selected.clone()
        } else {
            cwd.join(selected)
        };
        let max_source_bytes = remaining_tokens.saturating_mul(APPROXIMATE_BYTES_PER_TOKEN);
        let contents = match read_utf8_file(&resolved, max_source_bytes) {
            Ok(contents) => contents,
            Err(FileContextReadError::TooLarge) => {
                outcome.warnings.push(format!(
                    "File injection skipped for `{display_path}`: its complete contents cannot fit in the remaining {remaining_tokens}-token effective-context budget"
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

        let Some(items) = file_context_items(&display_path, &contents) else {
            outcome.warnings.push(format!(
                "File injection skipped for `{display_path}`: its path cannot fit in a bounded context item"
            ));
            continue;
        };
        let tokens = estimated_tokens(&items);
        if tokens > remaining_tokens {
            outcome.warnings.push(format!(
                "File injection skipped for `{display_path}`: its complete contents require an estimated {tokens} tokens, exceeding the remaining {remaining_tokens}-token effective-context budget"
            ));
            continue;
        }

        total_tokens = total_tokens.saturating_add(tokens);
        outcome.items.extend(items);
        outcome.injected.push(InjectedFileContext {
            path: display_path,
            tokens,
        });
    }

    outcome
}

fn file_context_items(display_path: &str, contents: &str) -> Option<Vec<serde_json::Value>> {
    let escaped_path = escape_xml_text(display_path);
    let item = file_context_item(&escaped_path, contents, None);
    if estimated_tokens(std::slice::from_ref(&item)) <= MAX_FILE_CONTEXT_ITEM_TOKENS {
        return Some(vec![item]);
    }

    let boundaries = contents
        .char_indices()
        .map(|(offset, character)| offset.saturating_add(character.len_utf8()))
        .collect::<Vec<_>>();
    if boundaries.is_empty() {
        return None;
    }
    let mut items = Vec::new();
    let mut start = 0_usize;
    let mut next_boundary = 0_usize;
    let mut part = 1_usize;

    while next_boundary < boundaries.len() {
        let mut low = next_boundary;
        let mut high = boundaries.len();
        let mut best = None;
        while low < high {
            let candidate = low + (high - low) / 2;
            let end = boundaries[candidate];
            let item = file_context_item(&escaped_path, &contents[start..end], Some(part));
            if estimated_tokens(std::slice::from_ref(&item)) <= MAX_FILE_CONTEXT_ITEM_TOKENS {
                best = Some(candidate);
                low = candidate.saturating_add(1);
            } else {
                high = candidate;
            }
        }

        let boundary = best?;
        let end = boundaries[boundary];
        items.push(file_context_item(
            &escaped_path,
            &contents[start..end],
            Some(part),
        ));
        start = end;
        next_boundary = boundary.saturating_add(1);
        part = part.saturating_add(1);
    }

    Some(items)
}

fn file_context_item(escaped_path: &str, contents: &str, part: Option<usize>) -> serde_json::Value {
    let part = part.map_or_else(String::new, |part| format!("\n<part>{part}</part>"));
    let body = format!(
        "<file_context>\n<path>{escaped_path}</path>{part}\n<contents><![CDATA[\n{}\n]]></contents>\n</file_context>",
        escape_cdata(contents),
    );
    let mut item = message("user", body);
    mark_contextual_user_message(&mut item);
    item
}

enum FileContextReadError {
    TooLarge,
    NotUtf8,
    Other(anyhow::Error),
}

fn read_utf8_file(
    path: &Path,
    max_source_bytes: u64,
) -> std::result::Result<String, FileContextReadError> {
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
    if metadata.len() > max_source_bytes {
        return Err(FileContextReadError::TooLarge);
    }

    let read_limit = max_source_bytes.saturating_add(1);
    let capacity = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))
        .map_err(FileContextReadError::Other)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_source_bytes {
        return Err(FileContextReadError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| FileContextReadError::NotUtf8)
}
