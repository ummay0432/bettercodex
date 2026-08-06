use crate::MODEL;
use crate::usage::TokenUsage;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_protocol::models::MessagePhase;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Write;
use std::ops::Deref;
use std::ops::DerefMut;
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

const ROLLOUT_VERSION: u32 = 1;
const STATE_DIRECTORY: &str = "bettercodex";
const SESSIONS_DIRECTORY: &str = "sessions";
const INSTALLATION_ID_FILE: &str = "installation_id";
const JOURNAL_BUFFER_BYTES: usize = 64 * 1024;
const MAX_SESSION_PREVIEW_CHARS: usize = 160;

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
    pub(crate) preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionTranscriptItem {
    User {
        text: String,
        image_count: usize,
    },
    Assistant {
        text: String,
        phase: Option<MessagePhase>,
    },
}

pub(crate) struct LoadedRollout {
    pub(crate) rollout: Rollout,
    pub(crate) metadata: SessionMetadata,
    pub(crate) history: Vec<Value>,
    pub(crate) transcript: Vec<SessionTranscriptItem>,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) usage_history_estimate: Option<u64>,
    pub(crate) server_reasoning_included: bool,
    pub(crate) compaction_count: u64,
    pub(crate) unfinished_turn: Option<String>,
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
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RolloutRecordData<Items = Vec<Value>> {
    Session {
        metadata: SessionMetadata,
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
    TurnStarted {
        turn_id: String,
    },
    TurnFinished {
        turn_id: String,
        outcome: TurnOutcome,
    },
}

type RolloutRecord = RolloutRecordData;
// History items can contain multi-megabyte images and tool results. Keep the journal schema
// shared with replay while borrowing those payloads on the write path instead of deep-cloning
// them into a short-lived record.
type BorrowedRolloutRecord<'a> = RolloutRecordData<&'a [Value]>;

#[derive(Debug, Deserialize)]
struct PreviewHistoryRecord {
    #[serde(default)]
    items: Vec<PreviewItem>,
}

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
    pub(crate) fn create(cwd: &Path) -> Result<Self> {
        Self::create_in(&state_root()?, cwd)
    }

    pub(crate) fn create_in(root: &Path, cwd: &Path) -> Result<Self> {
        prepare_private_directory(root)?;
        let sessions = root.join(SESSIONS_DIRECTORY);
        prepare_private_directory(&sessions)?;

        let identity = SessionIdentity {
            installation_id: installation_id(root)?,
            session_id: Uuid::new_v4().to_string(),
            thread_id: Uuid::new_v4().to_string(),
        };
        let metadata = SessionMetadata {
            version: ROLLOUT_VERSION,
            identity,
            cwd: cwd.to_path_buf(),
            created_at_unix_ms: unix_timestamp_millis(),
            model: MODEL.to_string(),
            reasoning_effort: "max".to_string(),
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
            self.file.sync_data().with_context(|| {
                format!(
                    "failed to persist the restored session journal {}",
                    self.path.display()
                )
            })?;
            return Err(error).with_context(|| format!("failed to append {}", self.path.display()));
        }
        self.file
            .sync_data()
            .with_context(|| format!("failed to persist {}", self.path.display()))
    }
}

fn load_rollout(path: PathBuf) -> Result<LoadedRollout> {
    let mut file = lock_rollout(open_private_append(&path, false)?, &path)?;
    let original_length = file.metadata()?.len();
    let mut reader = BufReader::new(&*file);
    let mut metadata = None;
    let mut history = Vec::new();
    let mut transcript = Vec::new();
    let mut usage = None;
    let mut usage_history_estimate = None;
    let mut server_reasoning_included = false;
    let mut compaction_count = 0_u64;
    let mut unfinished_turn = None;
    let mut line_number = 0_usize;
    let mut valid_length = 0_u64;
    let mut valid_record_needs_newline = false;

    loop {
        let mut line = Vec::new();
        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if bytes_read == 0 {
            break;
        }
        line_number += 1;
        let terminated = line.last() == Some(&b'\n');
        if terminated {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            valid_length = valid_length.saturating_add(bytes_read as u64);
            valid_record_needs_newline = !terminated;
            continue;
        }
        let record = match serde_json::from_slice::<RolloutRecord>(&line) {
            Ok(record) => record,
            Err(_) if !terminated => break,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("invalid session record at {}:{line_number}", path.display())
                });
            }
        };
        valid_length = valid_length.saturating_add(bytes_read as u64);
        valid_record_needs_newline = !terminated;
        match record {
            RolloutRecord::Session {
                metadata: session_metadata,
            } => {
                if metadata.replace(session_metadata).is_some() {
                    return Err(anyhow!(
                        "{} contains multiple session headers",
                        path.display()
                    ));
                }
            }
            RolloutRecord::HistoryAppend { items } => {
                append_transcript_items(&mut transcript, &items);
                history.extend(items);
            }
            RolloutRecord::HistoryReplace {
                reason,
                items,
                response_usage: _,
            } => {
                if reason == HistoryReplacement::Compaction {
                    compaction_count = compaction_count.saturating_add(1);
                }
                history = items;
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
                usage = Some(new_usage);
                usage_history_estimate = Some(history_estimate);
                server_reasoning_included = reasoning_included;
            }
            RolloutRecord::TurnStarted { turn_id } => unfinished_turn = Some(turn_id),
            RolloutRecord::TurnFinished { turn_id, .. } => {
                if unfinished_turn.as_deref() == Some(turn_id.as_str()) {
                    unfinished_turn = None;
                }
            }
        }
    }

    drop(reader);
    repair_rollout_tail(
        &mut file,
        &path,
        original_length,
        valid_length,
        valid_record_needs_newline,
    )?;

    let metadata = metadata.ok_or_else(|| anyhow!("{} has no session header", path.display()))?;
    if metadata.version != ROLLOUT_VERSION {
        return Err(anyhow!(
            "{} uses unsupported session version {}",
            path.display(),
            metadata.version
        ));
    }
    if metadata.model != MODEL || metadata.reasoning_effort != "max" {
        return Err(anyhow!(
            "saved session uses {} at {}; bettercodex requires {MODEL} at max",
            metadata.model,
            metadata.reasoning_effort
        ));
    }

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
        usage,
        usage_history_estimate,
        server_reasoning_included,
        compaction_count,
        unfinished_turn,
    })
}

fn append_transcript_items(transcript: &mut Vec<SessionTranscriptItem>, items: &[Value]) {
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(role) = item.get("role").and_then(Value::as_str) else {
            continue;
        };
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        let text_kind = match role {
            "user" => "input_text",
            "assistant" => "output_text",
            _ => continue,
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
                    continue;
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
        let Some(metadata) = read_metadata(&path)? else {
            continue;
        };
        if metadata.cwd != cwd {
            continue;
        }
        let modified_at = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(u128::from(metadata.created_at_unix_ms) * 1_000_000);
        let candidate = (modified_at, metadata.created_at_unix_ms, path);
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
    let mut sessions = Vec::new();
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
        if let Some(summary) = read_session_summary(&path, modified_at)? {
            sessions.push(summary);
        }
    }
    sessions.sort_unstable_by(|left, right| {
        right
            .updated_at_unix_ms
            .cmp(&left.updated_at_unix_ms)
            .then_with(|| right.created_at_unix_ms.cmp(&left.created_at_unix_ms))
            .then_with(|| right.id.cmp(&left.id))
    });
    Ok(sessions)
}

fn read_session_summary(
    path: &Path,
    modified_at: Option<SystemTime>,
) -> Result<Option<SessionSummary>> {
    let file = File::open(path)
        .with_context(|| format!("failed to inspect saved session {}", path.display()))?;
    let mut lines = BufReader::new(file).split(b'\n');
    let Some(header) = lines.next() else {
        return Ok(None);
    };
    let header = header?;
    let Ok(RolloutRecord::Session { metadata }) = serde_json::from_slice(&header) else {
        return Ok(None);
    };
    if metadata.version != ROLLOUT_VERSION
        || metadata.model != MODEL
        || metadata.reasoning_effort != "max"
    {
        return Ok(None);
    }
    let Ok(id) = Uuid::parse_str(&metadata.identity.session_id) else {
        return Ok(None);
    };
    if path.file_stem().and_then(|stem| stem.to_str())
        != Some(metadata.identity.session_id.as_str())
    {
        return Ok(None);
    }

    let mut preview = None;
    for line in lines {
        let line =
            line.with_context(|| format!("failed to inspect saved session {}", path.display()))?;
        // bettercodex writes the externally tagged record type first. Avoid decoding initial
        // context, reasoning, and tool payloads while looking for the first real user message.
        if !line.starts_with(br#"{"type":"history_append""#) {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<PreviewHistoryRecord>(&line) else {
            continue;
        };
        if let Some(found) = preview_from_items(&record.items) {
            preview = Some(found);
            break;
        }
    }

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

fn preview_from_items(items: &[PreviewItem]) -> Option<String> {
    for item in items {
        if item.kind != "message" || item.role != "user" {
            continue;
        }
        let text = item
            .content
            .iter()
            .filter(|content| content.kind == "input_text")
            .map(|content| content.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
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

fn read_metadata(path: &Path) -> Result<Option<SessionMetadata>> {
    let file = File::open(path)
        .with_context(|| format!("failed to inspect saved session {}", path.display()))?;
    let mut lines = BufReader::new(file).split(b'\n');
    let Some(line) = lines.next() else {
        return Ok(None);
    };
    let line = line?;
    let Ok(RolloutRecord::Session { metadata }) = serde_json::from_slice(&line) else {
        return Ok(None);
    };
    Ok(Some(metadata))
}

fn state_root() -> Result<PathBuf> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| anyhow!("cannot locate bettercodex state: HOME is not set"))?;
    Ok(codex_home.join(STATE_DIRECTORY))
}

fn installation_id(root: &Path) -> Result<String> {
    let path = root.join(INSTALLATION_ID_FILE);
    if let Ok(value) = std::fs::read_to_string(&path)
        && let Ok(id) = Uuid::parse_str(value.trim())
    {
        return Ok(id.to_string());
    }

    let value = Uuid::new_v4().to_string();
    let temporary = root.join(format!(
        ".{INSTALLATION_ID_FILE}.tmp-{}-{}",
        std::process::id(),
        Uuid::new_v4(),
    ));
    let mut file = open_private_replace(&temporary)?;
    file.write_all(value.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    let linked = std::fs::hard_link(&temporary, &path);
    let _ = std::fs::remove_file(&temporary);
    match linked {
        Ok(()) => Ok(value),
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
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create state directory {}", path.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to protect state directory {}", path.display()))?;
    Ok(())
}

fn open_private_append(path: &Path, create_new: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).read(true).write(true);
    if create_new {
        options.create_new(true);
    }
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open session journal {}", path.display()))?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn lock_rollout(file: File, path: &Path) -> Result<LockedRolloutFile> {
    // The lock remains attached to Rollout's file descriptor for the complete
    // process lifetime, covering both replay/repair and every later append.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(LockedRolloutFile(file));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return Err(anyhow!(
            "saved session {} is already open in another bettercodex process",
            path.display()
        ));
    }
    Err(error).with_context(|| format!("failed to lock saved session {}", path.display()))
}

fn open_private_replace(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .with_context(|| format!("failed to open private file {}", path.display()))
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "rollout_tests.rs"]
mod tests;
