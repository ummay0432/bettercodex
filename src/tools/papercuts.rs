use crate::repository;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::TryLockError;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

pub(super) const MAX_MESSAGE_CHARS: usize = 1_000;
const FILE_NAME: &str = "PAPERCUTS.md";
const FILE_HEADER: &str = "# Papercuts\n\n";
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);

#[derive(Deserialize)]
struct LogPapercutArgs {
    message: String,
}

pub(super) fn log(cwd: &Path, input: Value, cancellation: &CancellationToken) -> Result<Value> {
    ensure_not_cancelled(cancellation)?;
    let arguments: LogPapercutArgs = serde_json::from_value(input)
        .map_err(|error| anyhow!("invalid tools.log_papercut arguments: {error}"))?;
    let message = normalize_message(&arguments.message)?;
    let root = repository::find_root(cwd).ok_or_else(|| {
        anyhow!(
            "tools.log_papercut could not find a Git repository from {}; start bettercodex inside a Git worktree",
            cwd.display()
        )
    })?;
    let path = root.join(FILE_NAME);
    ensure_regular_file_or_missing(&path)?;

    let mut file = open_log(&path)?;
    lock_for_append(&file, &path, cancellation)?;
    ensure_not_cancelled(cancellation)?;

    let length = file
        .metadata()
        .with_context(|| format!("tools.log_papercut could not inspect {}", path.display()))?
        .len();
    let mut entry = String::with_capacity(FILE_HEADER.len() + message.len() + 4);
    if length == 0 {
        entry.push_str(FILE_HEADER);
    } else {
        file.seek(SeekFrom::End(-1))
            .with_context(|| format!("tools.log_papercut could not read {}", path.display()))?;
        let mut final_byte = [0_u8; 1];
        file.read_exact(&mut final_byte)
            .with_context(|| format!("tools.log_papercut could not read {}", path.display()))?;
        if final_byte[0] != b'\n' {
            entry.push('\n');
        }
    }
    entry.push_str("- ");
    entry.push_str(&message);
    entry.push('\n');

    ensure_not_cancelled(cancellation)?;
    file.write_all(entry.as_bytes())
        .with_context(|| format!("tools.log_papercut could not append to {}", path.display()))?;
    Ok(json!({"path": FILE_NAME}))
}

fn open_log(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        // bettercodex targets Unix. O_NOFOLLOW closes the check/open race for
        // symlinks, while O_NONBLOCK prevents a raced-in FIFO from hanging a
        // blocking worker before the descriptor can be rejected below.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("tools.log_papercut could not open {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("tools.log_papercut could not inspect {}", path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!(
            "tools.log_papercut requires {} to be a regular file",
            path.display()
        ));
    }
    Ok(file)
}

fn lock_for_append(file: &File, path: &Path, cancellation: &CancellationToken) -> Result<()> {
    let started_at = Instant::now();
    loop {
        ensure_not_cancelled(cancellation)?;
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if started_at.elapsed() < LOCK_WAIT_TIMEOUT => {
                std::thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(TryLockError::WouldBlock) => {
                return Err(anyhow!(
                    "tools.log_papercut could not lock {} within {} seconds; wait for the process writing the log to finish, then retry",
                    path.display(),
                    LOCK_WAIT_TIMEOUT.as_secs(),
                ));
            }
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| {
                    format!("tools.log_papercut could not lock {}", path.display())
                });
            }
        }
    }
}

fn normalize_message(message: &str) -> Result<String> {
    let message = message.trim();
    if message.is_empty() {
        return Err(anyhow!("tools.log_papercut requires a non-empty message"));
    }
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(anyhow!(
            "tools.log_papercut message exceeds {MAX_MESSAGE_CHARS} characters; shorten it to one or two sentences"
        ));
    }
    if message
        .chars()
        .any(|character| character.is_control() && !character.is_whitespace())
    {
        return Err(anyhow!(
            "tools.log_papercut message contains unsupported control characters; remove them"
        ));
    }

    let mut normalized = String::with_capacity(message.len());
    for word in message.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    Ok(normalized)
}

fn ensure_regular_file_or_missing(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(anyhow!(
            "tools.log_papercut requires {} to be a regular file",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("tools.log_papercut could not inspect {}", path.display())),
    }
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(anyhow!("tools.log_papercut was interrupted"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "papercuts_tests.rs"]
mod tests;
