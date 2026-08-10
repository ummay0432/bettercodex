//! Codex-compatible, cross-session prompt history for composer recall.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::borrow::Cow;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const HISTORY_FILENAME: &str = "history.jsonl";
const HISTORY_READ_BUFFER_SIZE: usize = 8 * 1024;
const MAX_BATCH_ROWS: usize = 128;
const MAX_BATCH_BYTES: usize = 64 * 1024;
const CURSOR_ANCHOR_BYTES: usize = 64;
const MAX_LOCK_RETRIES: usize = 10;
const LOCK_RETRY_SLEEP: Duration = Duration::from_millis(100);

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

/// Bounded reverse cursor over the persistent history snapshot present at TUI startup.
///
/// Appends after this cursor is created are already retained as local editor history, so the
/// cursor deliberately ignores them. A short byte anchor validates every continuation before it
/// seeks into the shared append-only file. Batches use independent read handles rather than
/// attempting to relock the writer descriptor.
pub(crate) struct PromptHistoryReader {
    path: PathBuf,
    log_id: Option<u64>,
    position: u64,
    anchor: Vec<u8>,
}

impl PromptHistory {
    pub(crate) fn open(session_id: &str) -> Result<Self> {
        let path = history_path()?;
        Self::open_in(&path, session_id)
    }

    pub(crate) fn open_with_reader(session_id: &str) -> Result<(Self, PromptHistoryReader)> {
        let path = history_path()?;
        Self::open_with_reader_in(&path, session_id)
    }

    fn open_with_reader_in(path: &Path, session_id: &str) -> Result<(Self, PromptHistoryReader)> {
        let history = Self::open_in(path, session_id)?;
        let reader = PromptHistoryReader::open(path)?;
        Ok((history, reader))
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

        let mut file =
            FileLock::exclusive(&mut self.file).context("failed to lock prompt history")?;
        file.file()
            .write_all(&encoded)
            .context("failed to append prompt history")?;
        file.file()
            .flush()
            .context("failed to flush prompt history")
    }
}

impl PromptHistoryReader {
    fn open(path: &Path) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .open(path)
            .with_context(|| format!("failed to open prompt history {}", path.display()))?;
        let mut file = FileLock::shared_with_retry(&mut file)
            .with_context(|| format!("failed to lock prompt history {}", path.display()))?;
        let metadata = file
            .file()
            .metadata()
            .with_context(|| format!("failed to inspect prompt history {}", path.display()))?;
        let position = metadata.len();
        let anchor = read_cursor_anchor(file.file(), position)
            .with_context(|| format!("failed to read prompt history {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            log_id: log_identity(&metadata),
            position,
            anchor,
        })
    }

    pub(crate) fn has_more(&self) -> bool {
        self.position > 0
    }

    /// Loads one newest-first batch and advances the cursor toward the beginning of the file.
    ///
    /// The row and byte caps mirror current Codex history search batches. One oversized newest row
    /// is allowed through by itself so every request makes progress.
    pub(crate) fn read_older(&mut self) -> Result<Vec<String>> {
        if self.position == 0 {
            return Ok(Vec::new());
        }

        let mut file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .with_context(|| format!("failed to open prompt history {}", self.path.display()))?;
        let mut file = FileLock::shared_with_retry(&mut file)
            .with_context(|| format!("failed to lock prompt history {}", self.path.display()))?;
        let metadata = file
            .file()
            .metadata()
            .with_context(|| format!("failed to inspect prompt history {}", self.path.display()))?;
        if self.log_id.is_some() && log_identity(&metadata) != self.log_id {
            return Err(anyhow!(
                "prompt history {} was replaced while it was being read",
                self.path.display()
            ));
        }
        if metadata.len() < self.position {
            return Err(anyhow!(
                "prompt history {} was truncated while it was being read",
                self.path.display()
            ));
        }
        let anchor = read_cursor_anchor(file.file(), self.position)
            .with_context(|| format!("failed to read prompt history {}", self.path.display()))?;
        if anchor != self.anchor {
            return Err(anyhow!(
                "prompt history {} changed before the active history cursor",
                self.path.display()
            ));
        }

        let batch = scan_older_batch(file.file(), self.position)
            .with_context(|| format!("failed to read prompt history {}", self.path.display()))?;
        self.position = batch.next_position;
        self.anchor = read_cursor_anchor(file.file(), self.position)
            .with_context(|| format!("failed to read prompt history {}", self.path.display()))?;
        Ok(batch.entries)
    }
}

struct ScannedHistoryBatch {
    entries: Vec<String>,
    next_position: u64,
}

struct RawHistoryRow {
    start: u64,
    end: u64,
    byte_len: usize,
    reversed_bytes: Option<Vec<u8>>,
}

#[derive(Default)]
struct HistoryBatchAccumulator {
    entries: Vec<String>,
    rows: usize,
    bytes: usize,
}

enum RetainRow {
    Continue,
    StopAt(u64),
}

impl HistoryBatchAccumulator {
    fn retain(&mut self, file: &mut File, mut row: RawHistoryRow) -> std::io::Result<RetainRow> {
        if self.rows > 0
            && (self.rows.saturating_add(1) > MAX_BATCH_ROWS
                || self.bytes.saturating_add(row.byte_len) > MAX_BATCH_BYTES)
        {
            return Ok(RetainRow::StopAt(row.end));
        }

        let bytes = if let Some(bytes) = row.reversed_bytes.as_mut() {
            bytes.reverse();
            std::mem::take(bytes)
        } else {
            let mut bytes = vec![0; row.byte_len];
            file.seek(SeekFrom::Start(row.start))?;
            file.read_exact(&mut bytes)?;
            bytes
        };
        if let Ok(entry) = serde_json::from_slice::<HistoryEntry<'_>>(&bytes)
            && !entry.text.is_empty()
        {
            self.entries.push(entry.text);
        }
        self.rows = self.rows.saturating_add(1);
        self.bytes = self.bytes.saturating_add(row.byte_len);

        if self.rows >= MAX_BATCH_ROWS || self.bytes >= MAX_BATCH_BYTES || row.start == 0 {
            Ok(RetainRow::StopAt(row.start))
        } else {
            Ok(RetainRow::Continue)
        }
    }
}

fn scan_older_batch(file: &mut File, end_position: u64) -> std::io::Result<ScannedHistoryBatch> {
    let mut batch = HistoryBatchAccumulator::default();
    let mut reversed_row = Some(Vec::new());
    let mut row_byte_len = 0_usize;
    let mut row_end = end_position;
    let mut read_end = end_position;
    let mut read_buffer = [0_u8; HISTORY_READ_BUFFER_SIZE];

    while read_end > 0 {
        let read_start = read_end.saturating_sub(HISTORY_READ_BUFFER_SIZE as u64);
        let read_len = usize::try_from(read_end - read_start).unwrap_or(HISTORY_READ_BUFFER_SIZE);
        file.seek(SeekFrom::Start(read_start))?;
        file.read_exact(&mut read_buffer[..read_len])?;

        for index in (0..read_len).rev() {
            let byte = read_buffer[index];
            if byte == b'\n' && row_byte_len > 0 {
                let row_start = read_start + index as u64 + 1;
                match batch.retain(
                    file,
                    RawHistoryRow {
                        start: row_start,
                        end: row_end,
                        byte_len: row_byte_len,
                        reversed_bytes: reversed_row.take(),
                    },
                )? {
                    RetainRow::StopAt(next_position) => {
                        return Ok(ScannedHistoryBatch {
                            entries: batch.entries,
                            next_position,
                        });
                    }
                    RetainRow::Continue => {
                        row_end = row_start;
                        reversed_row = Some(vec![b'\n']);
                        row_byte_len = 1;
                    }
                }
            } else {
                row_byte_len = row_byte_len.saturating_add(1);
                if let Some(bytes) = reversed_row.as_mut() {
                    if row_byte_len <= MAX_BATCH_BYTES {
                        bytes.push(byte);
                    } else {
                        reversed_row = None;
                    }
                }
            }
        }
        read_end = read_start;
    }

    if row_byte_len > 0 {
        match batch.retain(
            file,
            RawHistoryRow {
                start: 0,
                end: row_end,
                byte_len: row_byte_len,
                reversed_bytes: reversed_row,
            },
        )? {
            RetainRow::Continue => unreachable!("the oldest history row always ends a batch"),
            RetainRow::StopAt(next_position) => {
                return Ok(ScannedHistoryBatch {
                    entries: batch.entries,
                    next_position,
                });
            }
        }
    }

    Ok(ScannedHistoryBatch {
        entries: batch.entries,
        next_position: 0,
    })
}

fn read_cursor_anchor(file: &mut File, position: u64) -> std::io::Result<Vec<u8>> {
    let start = position.saturating_sub(CURSOR_ANCHOR_BYTES as u64);
    let length = usize::try_from(position - start).unwrap_or(CURSOR_ANCHOR_BYTES);
    let mut anchor = vec![0; length];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut anchor)?;
    Ok(anchor)
}

fn history_path() -> Result<PathBuf> {
    let codex_home = crate::paths::codex_home()
        .ok_or_else(|| anyhow!("cannot locate prompt history: no user home is available"))?;
    Ok(codex_home.join(HISTORY_FILENAME))
}

struct FileLock<'a>(&'a mut File);

impl<'a> FileLock<'a> {
    fn shared_with_retry(file: &'a mut File) -> std::io::Result<Self> {
        for _ in 0..MAX_LOCK_RETRIES {
            match File::try_lock_shared(&*file) {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(LOCK_RETRY_SLEEP);
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "prompt history lock remained busy",
        ))
    }

    fn exclusive(file: &'a mut File) -> Result<Self> {
        File::lock(&*file)?;
        Ok(Self(file))
    }

    fn file(&mut self) -> &mut File {
        self.0
    }
}

impl Drop for FileLock<'_> {
    fn drop(&mut self) {
        let _ = File::unlock(&*self.0);
    }
}

#[cfg(unix)]
fn log_identity(metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.ino())
}

#[cfg(windows)]
fn log_identity(metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::windows::fs::MetadataExt;
    Some(metadata.creation_time())
}

#[cfg(not(any(unix, windows)))]
fn log_identity(_metadata: &std::fs::Metadata) -> Option<u64> {
    None
}

#[cfg(test)]
#[path = "prompt_history_tests.rs"]
mod tests;
