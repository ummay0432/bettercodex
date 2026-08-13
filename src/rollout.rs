use crate::model::ModelSelection;
use crate::model::ReasoningEffort;
use crate::protocol::MessagePhase;
use crate::service_tier::ServiceTier;
use crate::time::unix_timestamp_millis;
use crate::usage::TokenUsage;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde::de::IgnoredAny;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::ops::Deref;
use std::ops::DerefMut;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

const ROLLOUT_VERSION: u32 = 1;
const STATE_DIRECTORY: &str = "bettercodex";
const SESSIONS_DIRECTORY: &str = "sessions";
const INSTALLATION_ID_FILE: &str = "installation_id";
const JOURNAL_BUFFER_BYTES: usize = 64 * 1024;
const MAX_SESSION_PREVIEW_CHARS: usize = 160;
const HISTORY_RECORD_PREFIX: &[u8] = br#"{"type":"history_"#;
const SESSION_LIST_MAX_WORKERS: usize = 4;
const SESSION_LIST_MIN_FILES_PER_WORKER: usize = 64;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct SessionIdentity {
    pub(crate) installation_id: String,
    pub(crate) session_id: String,
    pub(crate) thread_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct SessionMetadata {
    pub(crate) version: u32,
    pub(crate) identity: SessionIdentity,
    pub(crate) cwd: PathBuf,
    pub(crate) created_at_unix_ms: u64,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResumeSelector {
    LatestForCwd,
    Id(Uuid),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub(crate) id: Uuid,
    pub(crate) cwd: PathBuf,
    pub(crate) created_at_unix_ms: u64,
    pub(crate) updated_at_unix_ms: u64,
    pub(crate) preview: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SessionTranscriptItem {
    User {
        text: String,
        #[serde(default)]
        image_count: usize,
    },
    Assistant {
        text: String,
        phase: Option<MessagePhase>,
    },
    Tool {
        tool: SessionTranscriptTool,
    },
    Exploration {
        tools: Vec<SessionTranscriptTool>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct SessionTranscriptTool {
    pub(crate) call_id: String,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<SessionTranscriptToolOutput>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub(crate) enum SessionTranscriptToolOutput {
    Success(Value),
    Error(String),
}

pub(crate) struct LoadedRollout {
    pub(crate) rollout: Rollout,
    pub(crate) metadata: SessionMetadata,
    pub(crate) history: Vec<Value>,
    pub(crate) transcript: Vec<SessionTranscriptItem>,
    pub(crate) transcript_checkpoint: Option<usize>,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) total_usage: TokenUsage,
    pub(crate) usage_history_estimate: Option<u64>,
    pub(crate) server_reasoning_included: bool,
    pub(crate) compaction_count: u64,
    pub(crate) model_selection: ModelSelection,
    pub(crate) service_tier: ServiceTier,
    pub(crate) unfinished_turn: Option<String>,
    pub(crate) forked_from: Option<String>,
}

pub(crate) struct Rollout {
    file: LockedRolloutFile,
    path: PathBuf,
    metadata: SessionMetadata,
}

struct LockedRolloutFile(File);

impl Deref for LockedRolloutFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for LockedRolloutFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for LockedRolloutFile {
    fn drop(&mut self) {
        // Unlock explicitly: a concurrently forked command can briefly inherit
        // the close-on-exec descriptor and would otherwise extend ownership.
        let _ = File::unlock(&self.0);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RolloutRecordData<Items = Vec<Value>> {
    Session {
        metadata: SessionMetadata,
    },
    ForkedFrom {
        session_id: String,
        compaction_count: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prior_usage: Option<TokenUsage>,
    },
    TranscriptSnapshot {
        items: Vec<SessionTranscriptItem>,
        // Snapshots written before tool replay existed contained only messages.
        #[serde(default)]
        complete: bool,
    },
    TranscriptAppend {
        items: Vec<SessionTranscriptItem>,
    },
    HistoryAppend {
        items: Items,
    },
    HistoryReplace {
        reason: HistoryReplacement,
        items: Items,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_usage: Option<TokenUsage>,
    },
    Usage {
        usage: TokenUsage,
        history_estimate: u64,
        #[serde(default)]
        server_reasoning_included: bool,
    },
    ServiceTierChanged {
        service_tier: ServiceTier,
    },
    ModelChanged {
        selection: ModelSelection,
    },
    TurnStarted {
        turn_id: String,
    },
    TurnFinished {
        turn_id: String,
        outcome: TurnOutcome,
    },
    #[serde(other)]
    Unknown,
}

type RolloutRecord = RolloutRecordData;
// History items can contain multi-megabyte images and tool results. Keep the journal schema shared
// with replay while borrowing those payloads on the write path instead of deep-cloning them into a
// short-lived record.
type BorrowedRolloutRecord<'a> = RolloutRecordData<&'a [Value]>;

// Listing only needs message shape and preview text. Deserialize those selected fields directly
// from the journal stream while scanning all unselected payloads without materializing them.
#[derive(Debug, Deserialize)]
struct PreviewItem {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: Vec<PreviewContent>,
}

#[derive(Debug, Deserialize)]
struct PreviewContent {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: String,
}

// Deserialize previews directly from the journal reader. Fields absent from these lightweight
// records—including base64 images, reasoning, and tool payloads—are scanned but never allocated.
#[derive(Debug)]
enum PreviewRolloutRecord {
    Session { metadata: SessionMetadata },
    History { items: Vec<PreviewItem> },
    Other,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PreviewRecordKind {
    Session,
    HistoryAppend,
    HistoryReplace,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum PreviewRecordField {
    Type,
    Metadata,
    Items,
    #[serde(other)]
    Other,
}

struct PreviewRolloutRecordVisitor;

impl<'de> Visitor<'de> for PreviewRolloutRecordVisitor {
    type Value = PreviewRolloutRecord;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a saved session record")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut kind = None;
        let mut metadata = None;
        let mut items = None;
        while let Some(field) = map.next_key::<PreviewRecordField>()? {
            match field {
                PreviewRecordField::Type => {
                    if kind.is_some() {
                        return Err(serde::de::Error::duplicate_field("type"));
                    }
                    kind = Some(map.next_value::<PreviewRecordKind>()?);
                }
                PreviewRecordField::Metadata
                    if matches!(kind, Some(PreviewRecordKind::Session)) =>
                {
                    if metadata.is_some() {
                        return Err(serde::de::Error::duplicate_field("metadata"));
                    }
                    metadata = Some(map.next_value::<SessionMetadata>()?);
                }
                PreviewRecordField::Items
                    if matches!(
                        kind,
                        Some(PreviewRecordKind::HistoryAppend | PreviewRecordKind::HistoryReplace)
                    ) =>
                {
                    if items.is_some() {
                        return Err(serde::de::Error::duplicate_field("items"));
                    }
                    items = Some(map.next_value::<Vec<PreviewItem>>()?);
                }
                PreviewRecordField::Metadata
                | PreviewRecordField::Items
                | PreviewRecordField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        match kind.ok_or_else(|| serde::de::Error::missing_field("type"))? {
            PreviewRecordKind::Session => Ok(PreviewRolloutRecord::Session {
                metadata: metadata.ok_or_else(|| serde::de::Error::missing_field("metadata"))?,
            }),
            PreviewRecordKind::HistoryAppend | PreviewRecordKind::HistoryReplace => {
                Ok(PreviewRolloutRecord::History {
                    items: items.unwrap_or_default(),
                })
            }
            PreviewRecordKind::Other => Ok(PreviewRolloutRecord::Other),
        }
    }
}

impl<'de> Deserialize<'de> for PreviewRolloutRecord {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(PreviewRolloutRecordVisitor)
    }
}

enum JsonLineContent<T> {
    Blank,
    Record(serde_json::Result<T>),
}

struct FramedJsonLine<T> {
    content: JsonLineContent<T>,
    bytes_read: usize,
    terminated: bool,
}

struct JsonLineReader<'a, R> {
    reader: &'a mut R,
    bytes_read: usize,
    content_bytes: usize,
    content_is_only_carriage_return: bool,
    terminated: bool,
    finished: bool,
}

impl<'a, R: BufRead> JsonLineReader<'a, R> {
    fn new(reader: &'a mut R) -> Self {
        Self {
            reader,
            bytes_read: 0,
            content_bytes: 0,
            content_is_only_carriage_return: true,
            terminated: false,
            finished: false,
        }
    }

    fn finish(&mut self) -> std::io::Result<()> {
        let mut buffer = [0_u8; 8 * 1024];
        while self.read(&mut buffer)? != 0 {}
        Ok(())
    }

    fn is_blank(&self) -> bool {
        self.content_bytes == 0 || (self.content_bytes == 1 && self.content_is_only_carriage_return)
    }
}

impl<R: BufRead> Read for JsonLineReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.finished || output.is_empty() {
            return Ok(0);
        }
        let available = self.reader.fill_buf()?;
        if available.is_empty() {
            self.finished = true;
            return Ok(0);
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_available = newline.unwrap_or(available.len());
        let copied = content_available.min(output.len());
        output[..copied].copy_from_slice(&available[..copied]);
        self.content_bytes = self.content_bytes.saturating_add(copied);
        self.content_is_only_carriage_return &=
            available[..copied].iter().all(|byte| *byte == b'\r');

        let consumed_newline = newline.is_some() && copied == content_available;
        let consumed = copied.saturating_add(usize::from(consumed_newline));
        self.reader.consume(consumed);
        self.bytes_read = self.bytes_read.saturating_add(consumed);
        if consumed_newline {
            self.terminated = true;
            self.finished = true;
        }
        Ok(copied)
    }
}

fn read_json_line<T: DeserializeOwned>(
    reader: &mut impl BufRead,
) -> std::io::Result<Option<FramedJsonLine<T>>> {
    let mut line = JsonLineReader::new(reader);
    // serde_json's generic reader advances one byte at a time. Buffer the framed adapter so those
    // reads come from a fixed-size block instead of calling fill_buf for every byte in a large
    // ignored payload.
    let record = {
        let mut buffered = BufReader::with_capacity(JOURNAL_BUFFER_BYTES, &mut line);
        serde_json::from_reader(&mut buffered)
    };
    line.finish()?;
    if line.bytes_read == 0 {
        return Ok(None);
    }
    let content = if line.is_blank() {
        JsonLineContent::Blank
    } else {
        JsonLineContent::Record(record)
    };
    Ok(Some(FramedJsonLine {
        content,
        bytes_read: line.bytes_read,
        terminated: line.terminated,
    }))
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HistoryReplacement {
    Initial,
    Compaction,
    Normalization,
    ContextRefresh,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnOutcome {
    Completed,
    Interrupted,
    Failed,
}

impl Rollout {
    pub(crate) fn create_with_selection(cwd: &Path, selection: &ModelSelection) -> Result<Self> {
        Self::create_in_with_selection(&state_root()?, cwd, selection)
    }

    #[cfg(test)]
    pub(crate) fn create_in(root: &Path, cwd: &Path) -> Result<Self> {
        Self::create_in_with_selection(root, cwd, &ModelSelection::default())
    }

    pub(crate) fn create_in_with_selection(
        root: &Path,
        cwd: &Path,
        selection: &ModelSelection,
    ) -> Result<Self> {
        selection.validate()?;
        prepare_private_directory(root)?;
        let sessions = root.join(SESSIONS_DIRECTORY);
        prepare_private_directory(&sessions)?;

        let identity = SessionIdentity {
            installation_id: installation_id(root)?,
            session_id: uuid::Uuid::new_v4().to_string(),
            thread_id: uuid::Uuid::new_v4().to_string(),
        };
        let metadata = SessionMetadata {
            version: ROLLOUT_VERSION,
            identity,
            cwd: cwd.to_path_buf(),
            created_at_unix_ms: unix_timestamp_millis(),
            model: selection.model.clone(),
            reasoning_effort: selection.reasoning_effort.to_string(),
        };
        let path = sessions.join(format!("{}.jsonl", metadata.identity.session_id));
        let file = lock_rollout(open_private_append(&path, true)?, &path)?;
        let mut rollout = Self {
            file,
            path,
            metadata: metadata.clone(),
        };
        rollout.write_record(&RolloutRecord::Session { metadata })?;
        Ok(rollout)
    }

    pub(crate) fn resume(selector: ResumeSelector, cwd: &Path) -> Result<LoadedRollout> {
        Self::resume_in(&state_root()?, selector, cwd)
    }

    pub(crate) fn list_sessions() -> Result<Vec<SessionSummary>> {
        list_sessions_in(&state_root()?)
    }

    pub(crate) fn has_saved_sessions() -> Result<bool> {
        let sessions = state_root()?.join(SESSIONS_DIRECTORY);
        let entries = match std::fs::read_dir(&sessions) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", sessions.display()));
            }
        };
        for entry in entries {
            let entry =
                entry.with_context(|| format!("failed to inspect {}", sessions.display()))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl")
                || !entry
                    .file_type()
                    .with_context(|| format!("failed to inspect saved session {}", path.display()))?
                    .is_file()
            {
                continue;
            }
            let modified_at = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok();
            if read_session_summary(&path, modified_at)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn resume_in(
        root: &Path,
        selector: ResumeSelector,
        cwd: &Path,
    ) -> Result<LoadedRollout> {
        let sessions = root.join(SESSIONS_DIRECTORY);
        let path = match selector {
            ResumeSelector::Id(id) => sessions.join(format!("{id}.jsonl")),
            ResumeSelector::LatestForCwd => {
                latest_rollout_for_cwd(&sessions, cwd)?.ok_or_else(|| {
                    anyhow!("no saved bettercodex session exists for {}", cwd.display())
                })?
            }
        };
        load_rollout(path)
    }

    pub(crate) fn identity(&self) -> &SessionIdentity {
        &self.metadata.identity
    }

    pub(crate) fn append_history(&mut self, items: &[Value]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let record = BorrowedRolloutRecord::HistoryAppend { items };
        self.write_record(&record)
    }

    pub(crate) fn record_fork(
        &mut self,
        source_session_id: &str,
        compaction_count: u64,
        prior_usage: Option<TokenUsage>,
    ) -> Result<()> {
        self.write_record(&RolloutRecord::ForkedFrom {
            session_id: source_session_id.to_string(),
            compaction_count,
            prior_usage,
        })
    }

    pub(crate) fn snapshot_transcript(&mut self, items: Vec<SessionTranscriptItem>) -> Result<()> {
        self.write_record(&RolloutRecord::TranscriptSnapshot {
            items,
            complete: true,
        })
    }

    pub(crate) fn append_transcript(&mut self, items: Vec<SessionTranscriptItem>) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        self.write_record(&RolloutRecord::TranscriptAppend { items })
    }

    pub(crate) fn replace_history(
        &mut self,
        items: &[Value],
        reason: HistoryReplacement,
    ) -> Result<()> {
        let record = BorrowedRolloutRecord::HistoryReplace {
            reason,
            items,
            response_usage: None,
        };
        self.write_record(&record)
    }

    pub(crate) fn replace_compacted_history(
        &mut self,
        items: &[Value],
        response_usage: Option<&TokenUsage>,
    ) -> Result<()> {
        let record = BorrowedRolloutRecord::HistoryReplace {
            reason: HistoryReplacement::Compaction,
            items,
            response_usage: response_usage.cloned(),
        };
        self.write_record(&record)
    }

    pub(crate) fn record_usage(
        &mut self,
        usage: &TokenUsage,
        history_estimate: u64,
        server_reasoning_included: bool,
    ) -> Result<()> {
        self.write_record(&RolloutRecord::Usage {
            usage: usage.clone(),
            history_estimate,
            server_reasoning_included,
        })
    }

    pub(crate) fn record_service_tier(&mut self, service_tier: ServiceTier) -> Result<()> {
        self.write_record(&RolloutRecord::ServiceTierChanged { service_tier })
    }

    pub(crate) fn record_model_selection(&mut self, selection: &ModelSelection) -> Result<()> {
        selection.validate()?;
        self.write_record(&RolloutRecord::ModelChanged {
            selection: selection.clone(),
        })
    }

    pub(crate) fn start_turn(&mut self, turn_id: &str) -> Result<()> {
        self.write_record(&RolloutRecord::TurnStarted {
            turn_id: turn_id.to_string(),
        })
    }

    pub(crate) fn finish_turn(&mut self, turn_id: &str, outcome: TurnOutcome) -> Result<()> {
        self.write_record(&RolloutRecord::TurnFinished {
            turn_id: turn_id.to_string(),
            outcome,
        })
    }

    fn write_record(&mut self, record: &impl Serialize) -> Result<()> {
        let record_start = self.file.metadata()?.len();
        // Stream through a fixed-size buffer: buffering the complete JSON value would briefly
        // duplicate the active history. Restore the prior boundary if serialization or I/O stops
        // mid-record so a later append cannot turn a recoverable tail into interior corruption.
        let append_result = {
            let mut writer = BufWriter::with_capacity(JOURNAL_BUFFER_BYTES, &mut *self.file);
            serde_json::to_writer(&mut writer, record)
                .context("failed to encode session record")
                .and_then(|()| {
                    writer
                        .write_all(b"\n")
                        .context("failed to terminate session record")
                })
                .and_then(|()| writer.flush().context("failed to flush session record"))
        };
        if let Err(error) = append_result {
            self.file.set_len(record_start).with_context(|| {
                format!(
                    "failed to restore {} after an incomplete session record: {error:#}",
                    self.path.display()
                )
            })?;
            return Err(error).with_context(|| format!("failed to append {}", self.path.display()));
        }
        // `BufWriter::flush` above completes the JSONL record and reports write errors. Do not
        // force every record through `sync_data`: that blocks the agent on durable storage even
        // though process-crash recovery only needs complete records in the filesystem cache.
        // `load_rollout` repairs an interrupted final record before the journal is appended again.
        Ok(())
    }
}

fn load_rollout(path: PathBuf) -> Result<LoadedRollout> {
    let mut file = lock_rollout(open_private_append(&path, false)?, &path)?;
    let original_length = file.metadata()?.len();
    let mut reader = BufReader::new(&*file);
    let mut metadata = None;
    let mut history = Vec::new();
    let mut transcript = Vec::new();
    // History remains the compatibility source for journals written before complete transcript
    // records existed and for a turn interrupted before its final transcript checkpoint. A
    // successful checkpoint supersedes only the history-derived tail that precedes it.
    let mut transcript_tail = Vec::new();
    let mut transcript_tail_tools = HashMap::new();
    let mut transcript_tail_history_start = None;
    let mut has_transcript_checkpoint = false;
    let mut legacy_transcript_snapshot = false;
    let mut forked_session = false;
    let mut usage = None;
    let mut total_usage = TokenUsage::default();
    let mut usage_history_estimate = None;
    let mut server_reasoning_included = false;
    let mut compaction_count = 0_u64;
    let mut model_selection = None;
    let mut service_tier = ServiceTier::default();
    let mut unfinished_turn = None;
    let mut forked_from = None;
    let mut line_number = 0_usize;
    let mut valid_length = 0_u64;
    let mut valid_record_needs_newline = false;

    loop {
        let Some(line) = read_json_line::<RolloutRecord>(&mut reader)
            .with_context(|| format!("failed to read {}", path.display()))?
        else {
            break;
        };
        line_number = line_number.saturating_add(1);
        let bytes_read = u64::try_from(line.bytes_read)
            .context("saved session record exceeds the supported file size")?;
        let record = match line.content {
            JsonLineContent::Blank => {
                valid_length = valid_length.saturating_add(bytes_read);
                valid_record_needs_newline = !line.terminated;
                continue;
            }
            JsonLineContent::Record(Ok(record)) => record,
            JsonLineContent::Record(Err(error)) if error.is_io() => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
            JsonLineContent::Record(Err(_)) if !line.terminated => break,
            JsonLineContent::Record(Err(error)) => {
                return Err(error).with_context(|| {
                    format!("invalid session record at {}:{line_number}", path.display())
                });
            }
        };
        valid_length = valid_length.saturating_add(bytes_read);
        valid_record_needs_newline = !line.terminated;
        match record {
            RolloutRecord::Session {
                metadata: session_metadata,
            } => {
                let (_, initial_selection) = validate_session_metadata(&path, &session_metadata)
                    .with_context(|| {
                        format!("invalid session header at {}:{line_number}", path.display())
                    })?;
                model_selection = Some(initial_selection);
                if metadata.replace(session_metadata).is_some() {
                    return Err(anyhow!(
                        "{} contains multiple session headers",
                        path.display()
                    ));
                }
            }
            RolloutRecord::ForkedFrom {
                session_id,
                compaction_count: source_compaction_count,
                prior_usage,
            } => {
                forked_session = true;
                forked_from = Some(session_id);
                compaction_count = source_compaction_count;
                if let Some(prior_usage) = prior_usage {
                    total_usage.add_assign(&prior_usage);
                }
            }
            RolloutRecord::TranscriptSnapshot { items, complete } => {
                transcript = items;
                transcript_tail.clear();
                transcript_tail_tools.clear();
                transcript_tail_history_start = None;
                has_transcript_checkpoint = complete;
                legacy_transcript_snapshot = !complete;
            }
            RolloutRecord::TranscriptAppend { items } => {
                transcript.extend(items);
                transcript_tail.clear();
                transcript_tail_tools.clear();
                transcript_tail_history_start = None;
                has_transcript_checkpoint = true;
                legacy_transcript_snapshot = false;
            }
            RolloutRecord::HistoryAppend { items } => {
                transcript_tail_history_start.get_or_insert(history.len());
                history.extend(items);
            }
            RolloutRecord::HistoryReplace {
                reason,
                items,
                response_usage,
            } => {
                flush_transcript_history(
                    &history,
                    &mut transcript_tail_history_start,
                    &mut transcript_tail,
                    &mut transcript_tail_tools,
                );
                if legacy_transcript_snapshot
                    && forked_session
                    && reason == HistoryReplacement::Initial
                    && transcript_tail.is_empty()
                    && let Some(recovered) = recover_legacy_fork_transcript(&transcript, &items)
                {
                    transcript = recovered;
                    has_transcript_checkpoint = false;
                    legacy_transcript_snapshot = false;
                }
                if reason == HistoryReplacement::Compaction {
                    compaction_count = compaction_count.saturating_add(1);
                }
                history = items;
                if let Some(response_usage) = response_usage {
                    total_usage.add_assign(&response_usage);
                }
                if !matches!(
                    reason,
                    HistoryReplacement::Normalization | HistoryReplacement::ContextRefresh
                ) {
                    usage = None;
                    usage_history_estimate = None;
                    server_reasoning_included = false;
                }
            }
            RolloutRecord::Usage {
                usage: new_usage,
                history_estimate,
                server_reasoning_included: reasoning_included,
            } => {
                total_usage.add_assign(&new_usage);
                usage = Some(new_usage);
                usage_history_estimate = Some(history_estimate);
                server_reasoning_included = reasoning_included;
            }
            RolloutRecord::ServiceTierChanged {
                service_tier: updated_service_tier,
            } => service_tier = updated_service_tier,
            RolloutRecord::ModelChanged { mut selection } => {
                selection.normalize();
                selection.validate().with_context(|| {
                    format!(
                        "invalid model selection at {}:{line_number}",
                        path.display()
                    )
                })?;
                model_selection = Some(selection);
            }
            RolloutRecord::TurnStarted { turn_id } => {
                unfinished_turn = Some(turn_id);
            }
            RolloutRecord::TurnFinished { turn_id, .. } => {
                if unfinished_turn.as_deref() == Some(turn_id.as_str()) {
                    unfinished_turn = None;
                }
            }
            RolloutRecord::Unknown => {}
        }
    }

    drop(reader);

    flush_transcript_history(
        &history,
        &mut transcript_tail_history_start,
        &mut transcript_tail,
        &mut transcript_tail_tools,
    );
    repair_rollout_tail(
        &mut file,
        &path,
        original_length,
        valid_length,
        valid_record_needs_newline,
    )?;

    let metadata = metadata.ok_or_else(|| anyhow!("{} has no session header", path.display()))?;
    let model_selection =
        model_selection.ok_or_else(|| anyhow!("{} has no valid session model", path.display()))?;
    model_selection.validate()?;
    let transcript_checkpoint =
        (has_transcript_checkpoint && transcript_tail.is_empty()).then_some(transcript.len());
    transcript.append(&mut transcript_tail);

    let rollout = Rollout {
        file,
        path,
        metadata: metadata.clone(),
    };
    Ok(LoadedRollout {
        rollout,
        metadata,
        history,
        transcript,
        transcript_checkpoint,
        usage,
        total_usage,
        usage_history_estimate,
        server_reasoning_included,
        compaction_count,
        model_selection,
        service_tier,
        unfinished_turn,
        forked_from,
    })
}

fn model_selection_from_metadata(metadata: &SessionMetadata) -> Result<ModelSelection> {
    let effort = ReasoningEffort::from_str(&metadata.reasoning_effort)
        .map_err(|error| anyhow!("saved session has an invalid reasoning effort: {error}"))?;
    Ok(ModelSelection::from_identity(
        metadata.model.clone(),
        effort,
    ))
}

fn validate_session_metadata(
    path: &Path,
    metadata: &SessionMetadata,
) -> Result<(Uuid, ModelSelection)> {
    if metadata.version != ROLLOUT_VERSION {
        return Err(anyhow!(
            "saved session uses unsupported version {}",
            metadata.version
        ));
    }
    let session_id = Uuid::parse_str(&metadata.identity.session_id)
        .context("saved session has an invalid session ID")?;
    if path.file_stem().and_then(|stem| stem.to_str())
        != Some(metadata.identity.session_id.as_str())
    {
        return Err(anyhow!(
            "saved session ID {} does not match journal filename {}",
            metadata.identity.session_id,
            path.display()
        ));
    }
    let selection = model_selection_from_metadata(metadata)?;
    selection.validate()?;
    Ok((session_id, selection))
}

fn append_transcript_items(
    transcript: &mut Vec<SessionTranscriptItem>,
    pending_tools: &mut HashMap<String, usize>,
    items: &[Value],
) {
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => append_transcript_message(transcript, item),
            Some("function_call" | "custom_tool_call") => {
                let Some(tool) = transcript_tool_from_history(item) else {
                    continue;
                };
                let call_id = tool.call_id.clone();
                let index = transcript.len();
                transcript.push(SessionTranscriptItem::Tool { tool });
                pending_tools.insert(call_id, index);
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                if item.get("name").and_then(Value::as_str) == Some("exec") {
                    // Code Mode `notify(...)` records reuse the outer call ID. They are injected
                    // into model history, not rendered as separate transcript cells.
                    continue;
                }
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(index) = pending_tools.remove(call_id) else {
                    // Output-only records were not separate transcript cells while live, so they
                    // should not become orphan cells here.
                    continue;
                };
                let Some(SessionTranscriptItem::Tool { tool }) = transcript.get_mut(index) else {
                    continue;
                };
                let output = item.get("output").cloned().unwrap_or(Value::Null);
                tool.output = Some(transcript_tool_output_from_history(&tool.name, output));
            }
            _ => {}
        }
    }
}

fn flush_transcript_history(
    history: &[Value],
    start: &mut Option<usize>,
    transcript: &mut Vec<SessionTranscriptItem>,
    pending_tools: &mut HashMap<String, usize>,
) {
    let Some(start) = start.take() else {
        return;
    };
    append_transcript_items(
        transcript,
        pending_tools,
        &history[start.min(history.len())..],
    );
}

fn recover_legacy_fork_transcript(
    snapshot: &[SessionTranscriptItem],
    history: &[Value],
) -> Option<Vec<SessionTranscriptItem>> {
    if !snapshot.iter().all(|item| {
        matches!(
            item,
            SessionTranscriptItem::User { .. } | SessionTranscriptItem::Assistant { .. }
        )
    }) {
        return None;
    }
    let mut recovered = Vec::new();
    append_transcript_items(&mut recovered, &mut HashMap::new(), history);
    if !recovered
        .iter()
        .any(|item| matches!(item, SessionTranscriptItem::Tool { .. }))
    {
        return None;
    }
    let mut recovered_messages = recovered.iter().filter(|item| {
        matches!(
            item,
            SessionTranscriptItem::User { .. } | SessionTranscriptItem::Assistant { .. }
        )
    });
    let messages_match = snapshot
        .iter()
        .all(|item| recovered_messages.next() == Some(item))
        && recovered_messages.next().is_none();
    messages_match.then_some(recovered)
}

fn append_transcript_message(transcript: &mut Vec<SessionTranscriptItem>, item: &Value) {
    let Some(role) = item.get("role").and_then(Value::as_str) else {
        return;
    };
    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return;
    };
    let text_kind = match role {
        "user" => "input_text",
        "assistant" => "output_text",
        _ => return,
    };
    let text = content
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some(text_kind))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");

    match role {
        "user" => {
            let image_count = content
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("input_image"))
                .count();
            if (text.trim().is_empty() && image_count == 0)
                || crate::context::is_contextual_user_text(&text)
            {
                return;
            }
            transcript.push(SessionTranscriptItem::User { text, image_count });
        }
        "assistant" if !text.trim().is_empty() => {
            let phase = item
                .get("phase")
                .and_then(|phase| serde_json::from_value(phase.clone()).ok());
            transcript.push(SessionTranscriptItem::Assistant { text, phase });
        }
        "assistant" => {}
        _ => unreachable!("message roles were filtered above"),
    }
}

fn transcript_tool_from_history(item: &Value) -> Option<SessionTranscriptTool> {
    let call_id = item.get("call_id")?.as_str()?.to_string();
    let name = item.get("name")?.as_str()?;
    let namespace = match item.get("namespace") {
        None | Some(Value::Null) => None,
        Some(Value::String(namespace)) if namespace == "functions" => None,
        Some(Value::String(namespace)) => Some(namespace.as_str()),
        Some(_) => return None,
    };
    let input = match item.get("type").and_then(Value::as_str)? {
        "function_call" => item
            .get("arguments")
            .and_then(Value::as_str)
            .map(|arguments| {
                serde_json::from_str(arguments)
                    .unwrap_or_else(|_| Value::String(arguments.to_string()))
            }),
        "custom_tool_call" => item.get("input").cloned(),
        _ => return None,
    };
    let name = if let Some(namespace) = namespace {
        format!("{namespace}.{name}")
    } else {
        name.to_string()
    };
    Some(SessionTranscriptTool {
        call_id,
        name,
        input,
        output: None,
    })
}

fn transcript_tool_output_from_history(name: &str, output: Value) -> SessionTranscriptToolOutput {
    let text = history_output_text(&output);
    if text.starts_with("Script failed") || text.contains("\nScript error:\n") {
        return SessionTranscriptToolOutput::Error(text);
    }
    let projected = match name {
        "exec" | "wait" => Value::String(text),
        "exec_command" | "write_stdin" => project_process_output(output, &text),
        "apply_patch" | "update_plan" | "view_image" | "web.run" => Value::Null,
        // Older rollouts can contain results from the removed OpenAI Docs namespace. Keep those
        // document bodies out of resumed transcript snapshots.
        name if name.starts_with("openaiDeveloperDocs.") => Value::Null,
        "log_papercut" => serde_json::from_str(&text).unwrap_or(output),
        _ => output,
    };
    SessionTranscriptToolOutput::Success(projected)
}

fn history_output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("text").and_then(Value::as_str).filter(|_| {
                    matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("input_text" | "output_text")
                    )
                })
            })
            .collect(),
        Value::Object(object) => object
            .get("output")
            .map(history_output_text)
            .unwrap_or_else(|| output.to_string()),
        Value::Null | Value::Bool(_) | Value::Number(_) => output.to_string(),
    }
}

fn project_process_output(output: Value, text: &str) -> Value {
    if output.get("output").is_some() {
        return output;
    }
    let (header, body) = text
        .split_once("\nOutput:\n")
        .map_or(("", text), |(header, body)| (header, body));
    let mut projected = serde_json::Map::new();
    projected.insert("output".to_string(), Value::String(body.to_string()));
    for line in header.lines() {
        if let Some(value) = line.strip_prefix("Chunk ID: ") {
            projected.insert("chunk_id".to_string(), Value::String(value.to_string()));
        } else if let Some(value) = line
            .strip_prefix("Wall time: ")
            .or_else(|| line.strip_prefix("Wall time "))
            .and_then(|value| value.strip_suffix(" seconds"))
            .and_then(|value| value.parse::<f64>().ok())
        {
            if let Some(value) = serde_json::Number::from_f64(value) {
                projected.insert("wall_time_seconds".to_string(), Value::Number(value));
            }
        } else if let Some(value) = line
            .strip_prefix("Process exited with code ")
            .and_then(|value| value.parse::<i64>().ok())
        {
            projected.insert("exit_code".to_string(), Value::Number(value.into()));
        } else if let Some(value) = line.strip_prefix("Process running with session ID ") {
            let value = value.parse::<i64>().map_or_else(
                |_| Value::String(value.to_string()),
                |value| Value::Number(value.into()),
            );
            projected.insert("session_id".to_string(), value);
        } else if let Some(value) = line
            .strip_prefix("Original token count: ")
            .and_then(|value| value.parse::<u64>().ok())
        {
            projected.insert(
                "original_token_count".to_string(),
                Value::Number(value.into()),
            );
        }
    }
    Value::Object(projected)
}

fn latest_rollout_for_cwd(sessions: &Path, cwd: &Path) -> Result<Option<PathBuf>> {
    let entries = match std::fs::read_dir(sessions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to list saved bettercodex sessions"),
    };
    let mut latest = None::<(u128, u64, PathBuf)>;
    for entry in entries {
        let entry = entry.context("failed to inspect a saved bettercodex session")?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let modified_at = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok();
        let Some(summary) = read_session_summary(&path, modified_at)? else {
            continue;
        };
        if summary.cwd != cwd {
            continue;
        }
        let modified_at = modified_at
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or_else(|| u128::from(summary.created_at_unix_ms) * 1_000_000);
        let candidate = (modified_at, summary.created_at_unix_ms, path);
        if latest
            .as_ref()
            .is_none_or(|current| (candidate.0, candidate.1) > (current.0, current.1))
        {
            latest = Some(candidate);
        }
    }
    Ok(latest.map(|(_, _, path)| path))
}

fn list_sessions_in(root: &Path) -> Result<Vec<SessionSummary>> {
    let sessions_directory = root.join(SESSIONS_DIRECTORY);
    let entries = match std::fs::read_dir(&sessions_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("failed to list saved bettercodex sessions"),
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.context("failed to inspect a saved bettercodex session")?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let modified_at = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok();
        candidates.push((path, modified_at));
    }
    let worker_count = session_list_worker_count(candidates.len());
    let mut sessions = if worker_count == 1 {
        read_session_summaries(candidates.iter())?
    } else {
        std::thread::scope(|scope| -> Result<Vec<SessionSummary>> {
            let candidates = &candidates;
            let mut workers = Vec::with_capacity(worker_count);
            for worker_index in 0..worker_count {
                let worker = std::thread::Builder::new()
                    .name(format!("session-list-{worker_index}"))
                    .spawn_scoped(scope, move || {
                        read_session_summaries(
                            candidates.iter().skip(worker_index).step_by(worker_count),
                        )
                    })
                    .context("failed to start saved session discovery worker")?;
                workers.push(worker);
            }

            let mut sessions = Vec::with_capacity(candidates.len());
            for worker in workers {
                let mut found = worker
                    .join()
                    .map_err(|_| anyhow!("saved session discovery worker panicked"))??;
                sessions.append(&mut found);
            }
            Ok(sessions)
        })?
    };
    sessions.sort_unstable_by(|left, right| {
        right
            .updated_at_unix_ms
            .cmp(&left.updated_at_unix_ms)
            .then_with(|| right.created_at_unix_ms.cmp(&left.created_at_unix_ms))
            .then_with(|| right.id.cmp(&left.id))
    });
    Ok(sessions)
}

fn session_list_worker_count(session_count: usize) -> usize {
    let useful_workers = (session_count / SESSION_LIST_MIN_FILES_PER_WORKER).max(1);
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(SESSION_LIST_MAX_WORKERS)
        .min(useful_workers)
}

fn read_session_summaries<'a>(
    candidates: impl Iterator<Item = &'a (PathBuf, Option<SystemTime>)>,
) -> Result<Vec<SessionSummary>> {
    let mut sessions = Vec::new();
    for (path, modified_at) in candidates {
        if let Some(summary) = read_session_summary(path, *modified_at)? {
            sessions.push(summary);
        }
    }
    Ok(sessions)
}

fn read_session_summary(
    path: &Path,
    modified_at: Option<SystemTime>,
) -> Result<Option<SessionSummary>> {
    let file = File::open(path)
        .with_context(|| format!("failed to inspect saved session {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let Some(header) = read_json_line::<PreviewRolloutRecord>(&mut reader)
        .with_context(|| format!("failed to inspect saved session {}", path.display()))?
    else {
        return Ok(None);
    };
    let metadata = match header.content {
        JsonLineContent::Record(Ok(PreviewRolloutRecord::Session { metadata })) => metadata,
        JsonLineContent::Blank
        | JsonLineContent::Record(Ok(
            PreviewRolloutRecord::History { .. } | PreviewRolloutRecord::Other,
        )) => {
            return Ok(None);
        }
        JsonLineContent::Record(Err(error)) if error.is_io() => {
            return Err(error)
                .with_context(|| format!("failed to inspect saved session {}", path.display()));
        }
        JsonLineContent::Record(Err(_)) => return Ok(None),
    };
    let Some(id) = compatible_session_id(path, &metadata) else {
        return Ok(None);
    };

    let mut preview = None;
    while let Some(record) = read_next_preview_history_record(&mut reader)
        .with_context(|| format!("failed to inspect saved session {}", path.display()))?
    {
        match record {
            Ok(PreviewRolloutRecord::History { items }) => {
                if let Some(found) = preview_from_items(&items) {
                    preview = Some(found);
                    break;
                }
            }
            Ok(PreviewRolloutRecord::Session { .. } | PreviewRolloutRecord::Other) => {}
            Err(error) if error.is_io() => {
                return Err(error).with_context(|| {
                    format!("failed to inspect saved session {}", path.display())
                });
            }
            Err(_) => break,
        }
    }
    let Some(preview) = preview else {
        return Ok(None);
    };

    let updated_at_unix_ms = modified_at
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| duration.as_millis().try_into().ok())
        .unwrap_or(metadata.created_at_unix_ms);
    Ok(Some(SessionSummary {
        id,
        cwd: metadata.cwd,
        created_at_unix_ms: metadata.created_at_unix_ms,
        updated_at_unix_ms,
        preview,
    }))
}

/// Finds and streams the next history record without parsing unrelated journal records.
///
/// Rollout serialization writes the externally tagged record type first. Inspecting that fixed
/// prefix lets session discovery discard model, usage, reasoning, and tool records directly from
/// the buffered input while matching records retain the line-framed, bounded-memory JSON parser.
fn read_next_preview_history_record(
    reader: &mut impl BufRead,
) -> std::io::Result<Option<serde_json::Result<PreviewRolloutRecord>>> {
    if !skip_to_record_prefix(reader, HISTORY_RECORD_PREFIX)? {
        return Ok(None);
    }

    let mut line = JsonLineReader::new(reader);
    let record = {
        let prefix = std::io::Cursor::new(HISTORY_RECORD_PREFIX).chain(&mut line);
        let mut buffered = BufReader::with_capacity(JOURNAL_BUFFER_BYTES, prefix);
        serde_json::from_reader(&mut buffered)
    };
    line.finish()?;
    Ok(Some(record))
}

fn skip_to_record_prefix(reader: &mut impl BufRead, prefix: &[u8]) -> std::io::Result<bool> {
    'records: loop {
        let mut matched = 0;
        while matched < prefix.len() {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(false);
            }
            let compared = available.len().min(prefix.len() - matched);
            let expected = &prefix[matched..matched + compared];
            if let Some(mismatch) = available[..compared]
                .iter()
                .zip(expected)
                .position(|(actual, expected)| actual != expected)
            {
                let mismatch_byte = available[mismatch];
                reader.consume(mismatch + 1);
                if mismatch_byte != b'\n' {
                    reader.skip_until(b'\n')?;
                }
                continue 'records;
            }
            reader.consume(compared);
            matched += compared;
        }
        return Ok(true);
    }
}

fn compatible_session_id(path: &Path, metadata: &SessionMetadata) -> Option<Uuid> {
    validate_session_metadata(path, metadata)
        .ok()
        .map(|(session_id, _)| session_id)
}

fn preview_from_items(items: &[PreviewItem]) -> Option<String> {
    for item in items {
        if item.kind != "message" || item.role != "user" {
            continue;
        }
        let mut texts = item
            .content
            .iter()
            .filter(|content| content.kind == "input_text")
            .map(|content| content.text.as_str());
        let first = texts.next().unwrap_or_default();
        let text = if let Some(second) = texts.next() {
            let mut joined = String::with_capacity(first.len() + second.len() + 1);
            joined.push_str(first);
            joined.push('\n');
            joined.push_str(second);
            for text in texts {
                joined.push('\n');
                joined.push_str(text);
            }
            Cow::Owned(joined)
        } else {
            Cow::Borrowed(first)
        };
        if crate::context::is_contextual_user_text(&text) {
            continue;
        }
        if let Some(preview) = normalized_preview(&text) {
            return Some(preview);
        }
        if item
            .content
            .iter()
            .any(|content| content.kind == "input_image")
        {
            return Some("Image attachment".to_string());
        }
    }
    None
}

fn normalized_preview(text: &str) -> Option<String> {
    let mut preview = String::new();
    let mut chars = 0_usize;
    let mut truncated = false;
    for word in text.split_whitespace() {
        if chars > 0 {
            if chars == MAX_SESSION_PREVIEW_CHARS {
                truncated = true;
                break;
            }
            preview.push(' ');
            chars += 1;
        }
        let remaining = MAX_SESSION_PREVIEW_CHARS.saturating_sub(chars);
        let mut word_chars = word.chars();
        for character in word_chars.by_ref().take(remaining) {
            preview.push(character);
            chars += 1;
        }
        if word_chars.next().is_some() {
            truncated = true;
            break;
        }
    }
    if preview.is_empty() {
        return None;
    }
    if truncated {
        if chars == MAX_SESSION_PREVIEW_CHARS {
            preview.pop();
        }
        preview.push('…');
    }
    Some(preview)
}

fn repair_rollout_tail(
    file: &mut File,
    path: &Path,
    original_length: u64,
    valid_length: u64,
    needs_newline: bool,
) -> Result<()> {
    if original_length == valid_length && !needs_newline {
        return Ok(());
    }
    if original_length != valid_length {
        file.set_len(valid_length)
            .with_context(|| format!("failed to truncate saved session {}", path.display()))?;
    }
    if needs_newline {
        file.write_all(b"\n")?;
    }
    file.sync_data()
        .with_context(|| format!("failed to persist repaired session {}", path.display()))
}

fn state_root() -> Result<PathBuf> {
    let codex_home = crate::paths::codex_home()
        .ok_or_else(|| anyhow!("cannot locate bettercodex state: no user home is available"))?;
    Ok(codex_home.join(STATE_DIRECTORY))
}

fn installation_id(root: &Path) -> Result<String> {
    let path = root.join(INSTALLATION_ID_FILE);
    if let Ok(value) = std::fs::read_to_string(&path)
        && let Ok(id) = Uuid::parse_str(value.trim())
    {
        return Ok(id.to_string());
    }

    let value = uuid::Uuid::new_v4().to_string();
    let temporary = root.join(format!(
        ".{INSTALLATION_ID_FILE}.tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4(),
    ));
    let mut file = open_private_replace(&temporary)?;
    file.write_all(value.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    let linked = std::fs::hard_link(&temporary, &path);
    let _ = std::fs::remove_file(&temporary);
    match linked {
        Ok(()) => {
            crate::platform_fs::sync_directory(root)?;
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read_to_string(&path)?;
            Uuid::parse_str(existing.trim())
                .map(|id| id.to_string())
                .context("the bettercodex installation ID is invalid")
        }
        Err(error) => Err(error.into()),
    }
}

fn prepare_private_directory(path: &Path) -> Result<()> {
    crate::platform_fs::create_private_directory_all(path)
        .with_context(|| format!("failed to create state directory {}", path.display()))?;
    Ok(())
}

fn open_private_append(path: &Path, create_new: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).read(true).write(true);
    if create_new {
        options.create_new(true);
    }
    crate::platform_fs::configure_private_file_nofollow(&mut options, false);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open session journal {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || crate::platform_fs::is_link(&metadata) {
        return Err(anyhow!(
            "session journal {} is not a regular file",
            path.display()
        ));
    }
    crate::platform_fs::protect_file(&file)?;
    Ok(file)
}

fn lock_rollout(file: File, path: &Path) -> Result<LockedRolloutFile> {
    // The lock remains attached to Rollout's file descriptor for the complete
    // process lifetime, covering both replay/repair and every later append.
    match File::try_lock(&file) {
        Ok(()) => Ok(LockedRolloutFile(file)),
        Err(std::fs::TryLockError::WouldBlock) => Err(anyhow!(
            "saved session {} is already open in another bettercodex process",
            path.display()
        )),
        Err(std::fs::TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("failed to lock saved session {}", path.display()))
        }
    }
}

fn open_private_replace(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    crate::platform_fs::configure_private_file_nofollow(&mut options, false);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open private file {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || crate::platform_fs::is_link(&metadata) {
        return Err(anyhow!(
            "private path {} is not a regular file",
            path.display()
        ));
    }
    crate::platform_fs::protect_file(&file)?;
    Ok(file)
}

#[cfg(test)]
#[path = "rollout_tests.rs"]
mod tests;
