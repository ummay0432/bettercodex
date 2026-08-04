//! Codex-compatible, cross-session prompt history for composer recall.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::RawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const HISTORY_FILENAME: &str = "history.jsonl";

#[derive(Debug, Deserialize, Serialize)]
struct HistoryEntry {
    session_id: String,
    ts: u64,
    text: String,
}

pub(crate) struct PromptHistory {
    file: File,
    session_id: String,
    entries: Vec<String>,
}

impl PromptHistory {
    pub(crate) fn open(session_id: &str) -> Result<Self> {
        let codex_home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
            .ok_or_else(|| anyhow!("cannot locate prompt history: HOME is not set"))?;
        Self::open_in(&codex_home.join(HISTORY_FILENAME), session_id)
    }

    fn open_in(path: &Path, session_id: &str) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create prompt history directory {}",
                    parent.display()
                )
            })?;
        }
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to open prompt history {}", path.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to protect prompt history {}", path.display()))?;

        let entries = {
            let _lock = FileLock::shared(&file)
                .with_context(|| format!("failed to lock prompt history {}", path.display()))?;
            let reader =
                BufReader::new(file.try_clone().with_context(|| {
                    format!("failed to read prompt history {}", path.display())
                })?);
            let mut entries = Vec::new();
            for line in reader.split(b'\n') {
                let line = line
                    .with_context(|| format!("failed to read prompt history {}", path.display()))?;
                if let Ok(entry) = serde_json::from_slice::<HistoryEntry>(&line)
                    && !entry.text.is_empty()
                {
                    entries.push(entry.text);
                }
            }
            entries
        };

        Ok(Self {
            file,
            session_id: session_id.to_string(),
            entries,
        })
    }

    pub(crate) fn entries(&self) -> &[String] {
        &self.entries
    }

    pub(crate) fn append(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let entry = HistoryEntry {
            session_id: self.session_id.clone(),
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| anyhow!("system clock is before the Unix epoch: {error}"))?
                .as_secs(),
            text: text.to_string(),
        };
        let mut encoded = serde_json::to_vec(&entry).context("failed to encode prompt history")?;
        encoded.push(b'\n');

        let _lock = FileLock::exclusive(&self.file).context("failed to lock prompt history")?;
        let mut file = &self.file;
        file.write_all(&encoded)
            .context("failed to append prompt history")?;
        file.flush().context("failed to flush prompt history")?;
        self.entries.push(text.to_string());
        Ok(())
    }
}

struct FileLock {
    descriptor: RawFd,
}

impl FileLock {
    fn shared(file: &File) -> Result<Self> {
        Self::lock(file, libc::LOCK_SH)
    }

    fn exclusive(file: &File) -> Result<Self> {
        Self::lock(file, libc::LOCK_EX)
    }

    fn lock(file: &File, operation: libc::c_int) -> Result<Self> {
        let descriptor = file.as_raw_fd();
        let result = unsafe { libc::flock(descriptor, operation) };
        if result == 0 {
            Ok(Self { descriptor })
        } else {
            Err(std::io::Error::last_os_error().into())
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.descriptor, libc::LOCK_UN) };
    }
}

#[cfg(test)]
#[path = "prompt_history_tests.rs"]
mod tests;
