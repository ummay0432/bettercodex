//! Codex-compatible, cross-session prompt history for composer recall.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::borrow::Cow;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const HISTORY_FILENAME: &str = "history.jsonl";

#[derive(Debug, Deserialize)]
struct HistoryEntry<'a> {
    #[serde(borrow, rename = "session_id")]
    _session_id: Cow<'a, str>,
    #[serde(rename = "ts")]
    _ts: u64,
    text: String,
}

#[derive(Serialize)]
struct NewHistoryEntry<'a> {
    session_id: &'a str,
    ts: u64,
    text: &'a str,
}

pub(crate) struct PromptHistory {
    file: File,
    session_id: String,
}

impl PromptHistory {
    pub(crate) fn open(session_id: &str) -> Result<Self> {
        let path = history_path()?;
        Self::open_in(&path, session_id)
    }

    pub(crate) fn open_with_entries(session_id: &str) -> Result<(Self, Vec<String>)> {
        let path = history_path()?;
        Self::open_with_entries_in(&path, session_id)
    }

    fn open_with_entries_in(path: &Path, session_id: &str) -> Result<(Self, Vec<String>)> {
        let history = Self::open_in(path, session_id)?;
        let entries = history.read_entries(path)?;
        Ok((history, entries))
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
        let mut options = OpenOptions::new();
        options.read(true).append(true).create(true);
        crate::platform_fs::configure_private_file(&mut options);
        let file = options
            .open(path)
            .with_context(|| format!("failed to open prompt history {}", path.display()))?;
        crate::platform_fs::protect_file(&file)
            .with_context(|| format!("failed to protect prompt history {}", path.display()))?;
        Ok(Self {
            file,
            session_id: session_id.to_string(),
        })
    }

    fn read_entries(&self, path: &Path) -> Result<Vec<String>> {
        let _lock = FileLock::shared(&self.file)
            .with_context(|| format!("failed to lock prompt history {}", path.display()))?;
        let reader = BufReader::new(
            self.file
                .try_clone()
                .with_context(|| format!("failed to read prompt history {}", path.display()))?,
        );
        let mut entries = Vec::new();
        for line in reader.split(b'\n') {
            let line =
                line.with_context(|| format!("failed to read prompt history {}", path.display()))?;
            if let Ok(entry) = serde_json::from_slice::<HistoryEntry<'_>>(&line)
                && !entry.text.is_empty()
            {
                entries.push(entry.text);
            }
        }
        Ok(entries)
    }

    pub(crate) fn append(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let entry = NewHistoryEntry {
            session_id: &self.session_id,
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| anyhow!("system clock is before the Unix epoch: {error}"))?
                .as_secs(),
            text,
        };
        let mut encoded = serde_json::to_vec(&entry).context("failed to encode prompt history")?;
        encoded.push(b'\n');

        let _lock = FileLock::exclusive(&self.file).context("failed to lock prompt history")?;
        let mut file = &self.file;
        file.write_all(&encoded)
            .context("failed to append prompt history")?;
        file.flush().context("failed to flush prompt history")
    }
}

fn history_path() -> Result<PathBuf> {
    let codex_home = crate::paths::codex_home()
        .ok_or_else(|| anyhow!("cannot locate prompt history: no user home is available"))?;
    Ok(codex_home.join(HISTORY_FILENAME))
}

struct FileLock<'a>(&'a File);

impl<'a> FileLock<'a> {
    fn shared(file: &'a File) -> Result<Self> {
        File::lock_shared(file)?;
        Ok(Self(file))
    }

    fn exclusive(file: &'a File) -> Result<Self> {
        File::lock(file)?;
        Ok(Self(file))
    }
}

impl Drop for FileLock<'_> {
    fn drop(&mut self) {
        let _ = File::unlock(self.0);
    }
}

#[cfg(test)]
#[path = "prompt_history_tests.rs"]
mod tests;
