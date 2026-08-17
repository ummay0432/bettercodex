use crate::model::ModelSelection;
use crate::model::ReasoningEffort;
use crate::protocol::MessagePhase;
use crate::protocol::ToolFileChange;
use crate::service_tier::ServiceTier;
use crate::time::unix_timestamp_millis;
use crate::usage::TokenUsage;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use aws_lc_rs::digest;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde::de::IgnoredAny;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::ops::Deref;
use std::ops::DerefMut;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

const ROLLOUT_VERSION: u32 = 1;
const STATE_DIRECTORY: &str = "bettercodex";
const SESSIONS_DIRECTORY: &str = "sessions";
const INSTALLATION_ID_FILE: &str = "installation_id";
const MAX_INSTALLATION_ID_BYTES: usize = 128;
const JOURNAL_BUFFER_BYTES: usize = 64 * 1024;
const MAX_SESSION_PREVIEW_CHARS: usize = 160;
const HISTORY_RECORD_PREFIX: &[u8] = br#"{"type":"history_"#;
const SESSION_LIST_MAX_WORKERS: usize = 4;
const SESSION_LIST_MIN_FILES_PER_WORKER: usize = 64;
pub(crate) const MAX_TOOL_PRE_STATE_HASH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOOL_RECOVERY_HASH_BYTES: u64 = MAX_TOOL_PRE_STATE_HASH_BYTES;
pub(crate) const SYNTHETIC_ABORT_OUTPUT: &str = "aborted";

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
    #[serde(with = "path_serde")]
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<crate::web_search::UrlCitation>,
    },
    WebSearch {
        search: crate::web_search::WebSearchCall,
    },
    Tool {
        tool: SessionTranscriptTool,
    },
    Exploration {
        tools: Vec<SessionTranscriptTool>,
    },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionTranscriptToolOrigin {
    #[default]
    Agent,
    Operator,
}

impl SessionTranscriptToolOrigin {
    fn is_agent(origin: &Self) -> bool {
        *origin == Self::Agent
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct SessionTranscriptTool {
    pub(crate) call_id: String,
    #[serde(default, skip_serializing_if = "SessionTranscriptToolOrigin::is_agent")]
    pub(crate) origin: SessionTranscriptToolOrigin,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<SessionTranscriptToolOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) file_change: Option<ToolFileChange>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub(crate) enum SessionTranscriptToolOutput {
    Success(Value),
    Error(String),
}

impl SessionTranscriptToolOutput {
    // Keep the established transcript envelope so older readers can deserialize the session. The
    // current TUI recognizes this internal payload and renders it as neutral recovered state.
    pub(crate) fn recovered_file_state(message: String) -> Self {
        Self::Success(serde_json::json!({
            "type": "recovered_file_state",
            "message": message,
        }))
    }

    pub(crate) fn recovered_file_state_message(&self) -> Option<&str> {
        let Self::Success(value) = self else {
            return None;
        };
        if value.get("type").and_then(Value::as_str) != Some("recovered_file_state") {
            return None;
        }
        value.get("message").and_then(Value::as_str)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct SessionTranscriptToolOutcome {
    pub(crate) call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<SessionTranscriptToolOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) file_change: Option<ToolFileChange>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolEffectClass {
    ReadOnly,
    AtomicMutation,
    Opaque,
}

impl ToolEffectClass {
    fn for_tool(name: &str) -> Option<Self> {
        match name {
            "read" => Some(Self::ReadOnly),
            "write" | "edit" => Some(Self::AtomicMutation),
            "bash" => Some(Self::Opaque),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct ToolLifecycleRegistration {
    call_id: String,
    name: String,
    effect: ToolEffectClass,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ToolContentDigest {
    bytes: u64,
    sha256: String,
}

impl ToolContentDigest {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        let sha256 = digest::digest(&digest::SHA256, bytes);
        Self {
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            sha256: encode_lower_hex(sha256.as_ref()),
        }
    }

    pub(crate) fn from_bytes_with_checkpoint(
        bytes: &[u8],
        mut checkpoint: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        let mut hasher = digest::Context::new(&digest::SHA256);
        for chunk in bytes.chunks(64 * 1024) {
            checkpoint()?;
            hasher.update(chunk);
        }
        checkpoint()?;
        Ok(Self {
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            sha256: encode_lower_hex(hasher.finish().as_ref()),
        })
    }

    pub(crate) fn from_reader(reader: &mut impl Read) -> std::io::Result<Self> {
        let mut hasher = digest::Context::new(&digest::SHA256);
        let mut buffer = [0_u8; 64 * 1024];
        let mut bytes = 0_u64;
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            bytes = bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            hasher.update(&buffer[..read]);
        }
        Ok(Self {
            bytes,
            sha256: encode_lower_hex(hasher.finish().as_ref()),
        })
    }

    pub(crate) fn byte_len(&self) -> u64 {
        self.bytes
    }
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "digest", rename_all = "snake_case")]
pub(crate) enum ToolTargetPreState {
    Absent,
    Digest(ToolContentDigest),
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ToolStagingEvidence {
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) directory: Option<crate::private_fs::FileObjectIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<crate::private_fs::FileSnapshot>,
}

impl ToolStagingEvidence {
    fn refines(&self, previous: &Self) -> bool {
        self.name == previous.name
            && option_refines(&previous.directory, &self.directory)
            && match (&previous.content, &self.content) {
                (None, _) => true,
                (Some(previous), Some(current)) => {
                    previous.object_identity() == current.object_identity()
                }
                (Some(_), None) => false,
            }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ToolSymlinkEvidence {
    #[serde(with = "path_serde")]
    pub(crate) path: PathBuf,
    pub(crate) snapshot: crate::private_fs::FileSnapshot,
}

impl ToolSymlinkEvidence {
    pub(crate) fn is_current(&self) -> bool {
        std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_symlink()
                && crate::private_fs::file_snapshot(&metadata) == self.snapshot
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ToolPathResolutionEvidence {
    #[serde(with = "path_serde")]
    pub(crate) requested: PathBuf,
    pub(crate) symlinks: Vec<ToolSymlinkEvidence>,
}

impl ToolPathResolutionEvidence {
    fn is_current(&self) -> bool {
        !self.symlinks.is_empty() && self.symlinks.iter().all(ToolSymlinkEvidence::is_current)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ToolMutationEvidence {
    #[serde(with = "path_serde")]
    pub(crate) target: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target_parent: Option<crate::private_fs::FileObjectIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) path_resolution: Option<ToolPathResolutionEvidence>,
    pub(crate) pre_state: ToolTargetPreState,
    pub(crate) post_state: ToolContentDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) staging: Option<ToolStagingEvidence>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "path_serde::option"
    )]
    pub(crate) missing_parent: Option<PathBuf>,
}

impl ToolMutationEvidence {
    fn refines(&self, previous: &Self) -> bool {
        self.target == previous.target
            && self.path_resolution == previous.path_resolution
            && self.pre_state == previous.pre_state
            && self.post_state == previous.post_state
            && self.missing_parent == previous.missing_parent
            && option_refines(&previous.target_parent, &self.target_parent)
            && match (&previous.staging, &self.staging) {
                (None, _) => true,
                (Some(previous), Some(current)) => current.refines(previous),
                (Some(_), None) => false,
            }
    }
}

fn option_refines<T: PartialEq>(previous: &Option<T>, current: &Option<T>) -> bool {
    match (previous, current) {
        (None, _) => true,
        (Some(previous), Some(current)) => previous == current,
        (Some(_), None) => false,
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

// Serde's ordinary PathBuf encoding rejects non-UTF-8 paths. Unix working directories and tool
// paths can contain arbitrary non-NUL bytes, so retain the readable string form when possible and
// fall back to a lossless byte representation only when necessary.
mod path_serde {
    use serde::Deserialize;
    use serde::Deserializer;
    use serde::Serialize;
    use serde::Serializer;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::ffi::OsStringExt as _;
    use std::path::Path;
    use std::path::PathBuf;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum PathRepresentation {
        Utf8(String),
        UnixBytes { unix_bytes: Vec<u8> },
    }

    #[derive(Serialize)]
    struct UnixPathRepresentation<'a> {
        unix_bytes: &'a [u8],
    }

    fn from_representation(representation: PathRepresentation) -> PathBuf {
        match representation {
            PathRepresentation::Utf8(path) => PathBuf::from(path),
            PathRepresentation::UnixBytes { unix_bytes } => {
                PathBuf::from(OsString::from_vec(unix_bytes))
            }
        }
    }

    pub(super) fn serialize<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(path) = path.to_str() {
            serializer.serialize_str(path)
        } else {
            UnixPathRepresentation {
                unix_bytes: path.as_os_str().as_bytes(),
            }
            .serialize(serializer)
        }
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        PathRepresentation::deserialize(deserializer).map(from_representation)
    }

    pub(super) mod option {
        use super::PathRepresentation;
        use super::from_representation;
        use serde::Deserialize;
        use serde::Deserializer;
        use serde::Serializer;
        use std::path::PathBuf;

        pub(in crate::rollout) fn serialize<S>(
            path: &Option<PathBuf>,
            serializer: S,
        ) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            match path {
                Some(path) => super::serialize(path, serializer),
                None => serializer.serialize_none(),
            }
        }

        pub(in crate::rollout) fn deserialize<'de, D>(
            deserializer: D,
        ) -> Result<Option<PathBuf>, D::Error>
        where
            D: Deserializer<'de>,
        {
            Option::<PathRepresentation>::deserialize(deserializer)
                .map(|path| path.map(from_representation))
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ToolRecovery {
    pub(crate) output: Value,
    pub(crate) transcript_output: SessionTranscriptToolOutput,
    pub(crate) file_change: Option<ToolFileChange>,
    requires_inspection: bool,
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
    pub(crate) unfinished_turn_has_activity: bool,
    pub(crate) unfinished_turn_has_recovery_notice: bool,
    pub(crate) unfinished_turn_recovered: bool,
    pub(crate) tool_recoveries: HashMap<String, ToolRecovery>,
    pub(crate) crash_recovery_requires_inspection: bool,
    pub(crate) forked_from: Option<String>,
}

pub(crate) struct Rollout {
    file: Arc<SharedRolloutFile>,
    metadata: SessionMetadata,
}

#[derive(Clone)]
pub(crate) struct ToolLifecycleJournal {
    file: Arc<SharedRolloutFile>,
}

struct SharedRolloutFile {
    file: Mutex<LockedRolloutFile>,
    path: PathBuf,
    #[cfg(test)]
    fail_next_append: std::sync::atomic::AtomicBool,
}

struct LockedRolloutFile(File);

// Marks an append failure only after the prior JSONL boundary has been restored, making an exact
// retry safe. Failures to restore the boundary deliberately retain their ordinary error type.
#[derive(Debug)]
struct RolledBackAppendError;

impl fmt::Display for RolledBackAppendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("session append was rolled back to its prior record boundary")
    }
}

impl std::error::Error for RolledBackAppendError {}

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
    ToolOutcomes {
        outcomes: Vec<SessionTranscriptToolOutcome>,
    },
    HistoryAppend {
        items: Items,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcomes: Option<Vec<SessionTranscriptToolOutcome>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolLifecycleRegistration>>,
    },
    HistoryReplace {
        reason: HistoryReplacement,
        items: Items,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_usage: Option<TokenUsage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcomes: Option<Vec<SessionTranscriptToolOutcome>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery: Option<TurnRecoveryCheckpoint>,
    },
    // Short-lived refactor builds wrote registrations as a standalone record before they became
    // part of HistoryAppend's atomic persistence boundary.
    ToolCallsRegistered {
        calls: Vec<ToolLifecycleRegistration>,
    },
    ToolStarted {
        call_id: String,
    },
    ToolMutationPrepared {
        call_id: String,
        evidence: ToolMutationEvidence,
    },
    ToolFinished {
        call_id: String,
        output: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_change: Option<ToolFileChange>,
        #[serde(default, skip_serializing_if = "is_false")]
        requires_inspection: bool,
    },
    Usage {
        usage: TokenUsage,
        history_estimate: u64,
        #[serde(default)]
        server_reasoning_included: bool,
    },
    UsageTotal {
        usage: TokenUsage,
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
    #[serde(rename = "bettercodex_user_message_kind", default)]
    user_message_kind: Option<String>,
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct TurnRecoveryCheckpoint {
    turn_id: String,
    requires_inspection: bool,
}

#[derive(Default)]
struct LoadedToolLifecycle {
    registration: Option<ToolLifecycleRegistration>,
    inconsistent: bool,
    started: bool,
    prepared: Option<ToolMutationEvidence>,
    finished: Option<LoadedToolFinish>,
}

struct LoadedToolFinish {
    output: Value,
    error: Option<String>,
    file_change: Option<ToolFileChange>,
    requires_inspection: bool,
}

fn register_tool_lifecycles(
    lifecycles: &mut HashMap<String, LoadedToolLifecycle>,
    registrations: impl IntoIterator<Item = ToolLifecycleRegistration>,
) {
    for registration in registrations {
        let lifecycle = lifecycles.entry(registration.call_id.clone()).or_default();
        match &lifecycle.registration {
            None => {
                if lifecycle.started || lifecycle.prepared.is_some() || lifecycle.finished.is_some()
                {
                    lifecycle.inconsistent = true;
                }
                lifecycle.registration = Some(registration);
            }
            Some(existing) if existing == &registration => {}
            Some(_) => lifecycle.inconsistent = true,
        }
    }
}

impl ToolLifecycleJournal {
    fn write_record(&self, record: &RolloutRecord) -> Result<()> {
        write_bounded_rollout_record(&self.file, record)
    }

    async fn write_record_async(&self, record: RolloutRecord) -> Result<()> {
        let journal = self.clone();
        tokio::task::spawn_blocking(move || journal.write_record(&record))
            .await
            .map_err(|_| anyhow!("session journal writer is unavailable after a panic"))?
    }

    pub(crate) fn record_started(&self, call_id: &str) -> Result<()> {
        self.write_record(&RolloutRecord::ToolStarted {
            call_id: call_id.to_string(),
        })
    }

    pub(crate) async fn record_started_async(&self, call_id: &str) -> Result<()> {
        self.write_record_async(RolloutRecord::ToolStarted {
            call_id: call_id.to_string(),
        })
        .await
    }

    pub(crate) fn record_mutation_prepared(
        &self,
        call_id: &str,
        evidence: ToolMutationEvidence,
    ) -> Result<()> {
        self.write_record(&RolloutRecord::ToolMutationPrepared {
            call_id: call_id.to_string(),
            evidence,
        })
    }

    #[cfg(test)]
    pub(crate) fn record_finished(
        &self,
        call_id: &str,
        output: Value,
        error: Option<String>,
        file_change: Option<ToolFileChange>,
        requires_inspection: bool,
    ) -> Result<()> {
        self.write_record(&RolloutRecord::ToolFinished {
            call_id: call_id.to_string(),
            output,
            error,
            file_change,
            requires_inspection,
        })
    }

    pub(crate) async fn record_finished_async(
        &self,
        call_id: &str,
        output: Value,
        error: Option<String>,
        file_change: Option<ToolFileChange>,
        requires_inspection: bool,
    ) -> Result<()> {
        self.write_record_async(RolloutRecord::ToolFinished {
            call_id: call_id.to_string(),
            output,
            error,
            file_change,
            requires_inspection,
        })
        .await
    }
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
        let rollout = Self {
            file: Arc::new(SharedRolloutFile {
                file: Mutex::new(file),
                path: path.clone(),
                #[cfg(test)]
                fail_next_append: std::sync::atomic::AtomicBool::new(false),
            }),
            metadata: metadata.clone(),
        };
        if let Err(error) = rollout.write_record(&RolloutRecord::Session { metadata }) {
            drop(rollout);
            return match std::fs::remove_file(&path) {
                Ok(()) => Err(error),
                Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => {
                    Err(error)
                }
                Err(cleanup_error) => Err(error).with_context(|| {
                    format!(
                        "failed to remove incomplete session journal {}: {cleanup_error}",
                        path.display()
                    )
                }),
            };
        }
        Ok(rollout)
    }

    pub(crate) fn resume(selector: ResumeSelector, cwd: &Path) -> Result<LoadedRollout> {
        Self::resume_in(&state_root()?, selector, cwd)
    }

    pub(crate) fn list_sessions() -> Result<Vec<SessionSummary>> {
        list_sessions_in(&state_root()?)
    }

    pub(crate) fn has_saved_sessions() -> Result<bool> {
        has_saved_sessions_in(&state_root()?)
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
        let tool_calls = tool_lifecycle_registrations(items);
        let record = BorrowedRolloutRecord::HistoryAppend {
            items,
            outcomes: None,
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        };
        self.write_record(&record)
    }

    pub(crate) fn append_tool_results(
        &mut self,
        items: &[Value],
        outcomes: Vec<SessionTranscriptToolOutcome>,
    ) -> Result<()> {
        if items.is_empty() {
            return self.record_tool_outcomes(outcomes);
        }
        let outcomes = (!outcomes.is_empty()).then_some(outcomes);
        let record = BorrowedRolloutRecord::HistoryAppend {
            items,
            outcomes,
            tool_calls: None,
        };
        let first_error = match self.write_record(&record) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if !first_error.is::<RolledBackAppendError>() {
            return Err(first_error);
        }
        // Tool effects may already be durable. Retry the exact history/outcome projection once so
        // a transient journal error cannot turn a completed mutation into a synthetic abort.
        self.write_record(&record).with_context(|| {
            format!(
                "failed to persist tool results after retrying a rolled-back session append: {first_error:#}"
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn fail_next_append_for_test(&self) {
        self.file
            .fail_next_append
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

    pub(crate) fn record_tool_outcomes(
        &mut self,
        outcomes: Vec<SessionTranscriptToolOutcome>,
    ) -> Result<()> {
        if outcomes.is_empty() {
            return Ok(());
        }
        self.write_record(&RolloutRecord::ToolOutcomes { outcomes })
    }

    pub(crate) fn tool_lifecycle_journal(&self) -> ToolLifecycleJournal {
        ToolLifecycleJournal {
            file: Arc::clone(&self.file),
        }
    }

    pub(crate) fn replace_history(
        &mut self,
        items: &[Value],
        reason: HistoryReplacement,
    ) -> Result<()> {
        self.replace_history_with_outcomes(items, reason, Vec::new())
    }

    pub(crate) fn replace_history_with_outcomes(
        &mut self,
        items: &[Value],
        reason: HistoryReplacement,
        outcomes: Vec<SessionTranscriptToolOutcome>,
    ) -> Result<()> {
        let record = BorrowedRolloutRecord::HistoryReplace {
            reason,
            items,
            response_usage: None,
            outcomes: (!outcomes.is_empty()).then_some(outcomes),
            recovery: None,
        };
        self.write_record(&record)
    }

    pub(crate) fn replace_recovered_history(
        &mut self,
        items: &[Value],
        outcomes: Vec<SessionTranscriptToolOutcome>,
        turn_id: &str,
        requires_inspection: bool,
    ) -> Result<()> {
        let record = BorrowedRolloutRecord::HistoryReplace {
            reason: HistoryReplacement::Normalization,
            items,
            response_usage: None,
            outcomes: (!outcomes.is_empty()).then_some(outcomes),
            recovery: Some(TurnRecoveryCheckpoint {
                turn_id: turn_id.to_string(),
                requires_inspection,
            }),
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
            outcomes: None,
            recovery: None,
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

    pub(crate) fn record_total_usage(&mut self, usage: &TokenUsage) -> Result<()> {
        self.write_record(&RolloutRecord::UsageTotal {
            usage: usage.clone(),
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

    fn write_record(&self, record: &impl Serialize) -> Result<()> {
        write_rollout_record(&self.file, record)
    }
}

fn write_rollout_record(shared: &SharedRolloutFile, record: &impl Serialize) -> Result<()> {
    append_rollout_record(shared, |file| {
        // Stream through a fixed-size buffer: buffering the complete JSON value would briefly
        // duplicate the active history.
        let mut writer = BufWriter::with_capacity(JOURNAL_BUFFER_BYTES, &mut **file);
        serde_json::to_writer(&mut writer, record)
            .context("failed to encode session record")
            .and_then(|()| {
                #[cfg(test)]
                crate::process_termination_test_support::stop_at(
                    "journal_record_encoded_before_newline",
                );
                writer
                    .write_all(b"\n")
                    .context("failed to terminate session record")
            })
            .and_then(|()| writer.flush().context("failed to flush session record"))
    })
}

fn write_bounded_rollout_record(shared: &SharedRolloutFile, record: &impl Serialize) -> Result<()> {
    // Lifecycle records are hard-bounded by tool output, file-change preview, and filesystem path
    // limits. Encoding one complete line avoids allocating the 64 KiB streaming buffer for every
    // small phase record and keeps serialization outside the journal lock when several tools
    // complete concurrently.
    let mut encoded = serde_json::to_vec(record).context("failed to encode session record")?;
    #[cfg(test)]
    crate::process_termination_test_support::stop_at("journal_record_encoded_before_newline");
    encoded.push(b'\n');
    append_rollout_record(shared, |file| {
        file.write_all(&encoded)
            .context("failed to write session record")?;
        file.flush().context("failed to flush session record")
    })
}

fn append_rollout_record(
    shared: &SharedRolloutFile,
    append: impl FnOnce(&mut LockedRolloutFile) -> Result<()>,
) -> Result<()> {
    let path = &shared.path;
    let lock = shared.file.lock();
    let was_poisoned = lock.is_err();
    let mut file = lock.unwrap_or_else(std::sync::PoisonError::into_inner);
    if was_poisoned {
        // Every append panic is caught and rolled back below before the guard is released. Clear a
        // stale poison bit defensively so an unrelated prior panic cannot disable this session.
        shared.file.clear_poison();
    }
    let record_start = file.metadata()?.len();
    #[cfg(test)]
    let inject_failure = shared
        .fail_next_append
        .swap(false, std::sync::atomic::Ordering::SeqCst);
    let append_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(test)]
        if inject_failure {
            file.write_all(b"{\"type\":")?;
            file.flush()?;
            return Err(anyhow!("injected session append failure"));
        }
        append(&mut file)
    }));
    let append_result = match append_result {
        Ok(result) => result,
        Err(panic) => {
            let restore = file.set_len(record_start).with_context(|| {
                format!(
                    "failed to restore {} after a panicking session append",
                    path.display()
                )
            });
            drop(file);
            if let Err(error) = restore {
                panic!("{error:#}");
            }
            std::panic::resume_unwind(panic);
        }
    };
    if let Err(error) = append_result {
        // Restore the prior boundary so a later append cannot turn a recoverable tail into interior
        // corruption, regardless of whether the record used streaming or bounded encoding.
        file.set_len(record_start).with_context(|| {
            format!(
                "failed to restore {} after an incomplete session record: {error:#}",
                path.display()
            )
        })?;
        return Err(error)
            .context(RolledBackAppendError)
            .with_context(|| format!("failed to append {}", path.display()));
    }
    // Completing the JSONL record and flushing reports write errors. Do not force every record
    // through `sync_data`: process-crash recovery only needs complete records in the filesystem
    // cache, and `load_rollout` repairs an interrupted final record before appending again.
    Ok(())
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
    let mut normalized_aborted_calls = HashSet::new();
    let mut tool_lifecycles = HashMap::<String, LoadedToolLifecycle>::new();
    let mut unfinished_turn_call_ids = HashSet::new();
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
    let mut unfinished_turn_has_activity = false;
    let mut unfinished_turn_has_recovery_notice = false;
    let mut unfinished_turn_recovered = false;
    let mut recovery_checkpoint_requires_inspection = false;
    let mut legacy_recovery_requires_inspection = false;
    let mut unknown_record_requires_inspection = false;
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
                if metadata.is_none() {
                    return Err(anyhow!(
                        "session header must be the first record at {}:{line_number}",
                        path.display()
                    ));
                }
                valid_length = valid_length.saturating_add(bytes_read);
                valid_record_needs_newline = !line.terminated;
                continue;
            }
            JsonLineContent::Record(Ok(record)) => record,
            JsonLineContent::Record(Err(error)) if error.is_io() => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
            JsonLineContent::Record(Err(error)) if !line.terminated && error.is_eof() => break,
            JsonLineContent::Record(Err(error)) => {
                return Err(error).with_context(|| {
                    format!("invalid session record at {}:{line_number}", path.display())
                });
            }
        };
        if metadata.is_none() && !matches!(&record, RolloutRecord::Session { .. }) {
            return Err(anyhow!(
                "session header must be the first record at {}:{line_number}",
                path.display()
            ));
        }
        valid_length = valid_length.saturating_add(bytes_read);
        valid_record_needs_newline = !line.terminated;
        if unfinished_turn_recovered
            && !matches!(
                &record,
                RolloutRecord::TurnFinished { .. } | RolloutRecord::Unknown
            )
        {
            return Err(anyhow!(
                "session record follows a completed turn recovery checkpoint at {}:{line_number}",
                path.display()
            ));
        }
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
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                transcript = items;
                transcript_tail.clear();
                transcript_tail_tools.clear();
                transcript_tail_history_start = None;
                has_transcript_checkpoint = complete;
                legacy_transcript_snapshot = !complete;
            }
            RolloutRecord::TranscriptAppend { items } => {
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                transcript.extend(items);
                transcript_tail.clear();
                transcript_tail_tools.clear();
                transcript_tail_history_start = None;
                has_transcript_checkpoint = true;
                legacy_transcript_snapshot = false;
            }
            RolloutRecord::ToolOutcomes { outcomes } => {
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                // Ordinary function output remains the model-history source of truth. This
                // bounded metadata also patches explicit transcript tools such as operator shell
                // cells, whose completion has no corresponding Responses history item.
                flush_transcript_history(
                    &history,
                    &mut transcript_tail_history_start,
                    &mut transcript_tail,
                    &mut transcript_tail_tools,
                );
                apply_transcript_tool_outcomes(&mut transcript, &outcomes);
                apply_transcript_tool_outcomes(&mut transcript_tail, &outcomes);
            }
            RolloutRecord::HistoryAppend {
                items,
                outcomes,
                tool_calls,
            } => {
                if unfinished_turn.is_some()
                    && items
                        .last()
                        .is_some_and(crate::context::is_turn_abort_notice)
                {
                    // Recovery and interruption notices are appended as their own history record.
                    // A notice earlier in a mixed legacy append belongs to prior context and must
                    // not suppress recovery for the active turn.
                    unfinished_turn_has_recovery_notice = true;
                }
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                if unfinished_turn.is_some() {
                    unfinished_turn_call_ids.extend(
                        items
                            .iter()
                            .filter_map(saved_tool_call_id)
                            .map(str::to_string),
                    );
                }
                register_tool_lifecycles(&mut tool_lifecycles, tool_calls.into_iter().flatten());
                remove_completed_tool_lifecycles_from_append(&mut tool_lifecycles, &items);
                transcript_tail_history_start.get_or_insert(history.len());
                history.extend(items);
                if let Some(outcomes) = outcomes {
                    flush_transcript_history(
                        &history,
                        &mut transcript_tail_history_start,
                        &mut transcript_tail,
                        &mut transcript_tail_tools,
                    );
                    apply_transcript_tool_outcomes(&mut transcript, &outcomes);
                    apply_transcript_tool_outcomes(&mut transcript_tail, &outcomes);
                }
            }
            RolloutRecord::HistoryReplace {
                reason,
                items,
                response_usage,
                outcomes,
                recovery,
            } => {
                let had_recovery_notice = unfinished_turn_has_recovery_notice;
                if unfinished_turn.is_some()
                    && had_recovery_notice
                    && !items.iter().any(crate::context::is_turn_abort_notice)
                {
                    unfinished_turn_has_recovery_notice = false;
                }
                unfinished_turn_has_activity |= unfinished_turn.is_some()
                    && !matches!(
                        reason,
                        HistoryReplacement::Normalization | HistoryReplacement::ContextRefresh
                    );
                let has_recovery_checkpoint = recovery.is_some();
                if let Some(recovery) = recovery.as_ref() {
                    if unfinished_turn_recovered
                        || reason != HistoryReplacement::Normalization
                        || unfinished_turn.as_deref() != Some(recovery.turn_id.as_str())
                        || !recovery_checkpoint_has_current_notice(
                            &history,
                            &items,
                            had_recovery_notice,
                        )
                        || !recovery_checkpoint_outcomes_are_complete(&history, outcomes.as_deref())
                    {
                        return Err(anyhow!(
                            "invalid turn recovery checkpoint at {}:{line_number}",
                            path.display()
                        ));
                    }
                    unfinished_turn_recovered = true;
                    unfinished_turn_has_recovery_notice = true;
                    recovery_checkpoint_requires_inspection = recovery.requires_inspection;
                }
                let normalization_aborts = if reason == HistoryReplacement::Normalization {
                    let aborted = normalized_abort_call_ids(&history, &items);
                    normalized_aborted_calls.extend(aborted.iter().cloned());
                    if !has_recovery_checkpoint
                        && unfinished_turn.is_some()
                        && outcomes
                            .as_deref()
                            .is_some_and(normalization_outcomes_contain_recovery)
                    {
                        // Refactor-era sessions could persist recovery outputs before their notice
                        // and turn closure. Conservatively retain inspection guidance when adopting
                        // that multi-record state into the transactional checkpoint.
                        legacy_recovery_requires_inspection = true;
                    }
                    aborted
                } else {
                    HashSet::new()
                };
                remove_completed_tool_lifecycles_from_replacement(&mut tool_lifecycles, &items);
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
                // Normalization records written before transcript outcomes existed still define
                // the latest malformed call's presentation. Repair that exact boundary now so a
                // stale wrong-kind output cannot survive merely because it was already displayed.
                apply_latest_normalization_aborts(
                    &mut transcript,
                    &mut transcript_tail,
                    &normalization_aborts,
                );
                history = items;
                if let Some(outcomes) = outcomes {
                    apply_transcript_tool_outcomes(&mut transcript, &outcomes);
                    apply_transcript_tool_outcomes(&mut transcript_tail, &outcomes);
                }
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
            RolloutRecord::ToolCallsRegistered { calls } => {
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                register_tool_lifecycles(&mut tool_lifecycles, calls);
            }
            RolloutRecord::ToolStarted { call_id } => {
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                let lifecycle = tool_lifecycles.entry(call_id).or_default();
                if lifecycle.prepared.is_some() || lifecycle.finished.is_some() {
                    lifecycle.inconsistent = true;
                }
                lifecycle.started = true;
            }
            RolloutRecord::ToolMutationPrepared { call_id, evidence } => {
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                let lifecycle = tool_lifecycles.entry(call_id).or_default();
                if lifecycle.finished.is_some() {
                    lifecycle.inconsistent = true;
                }
                lifecycle.started = true;
                match &lifecycle.prepared {
                    None => lifecycle.prepared = Some(evidence),
                    Some(existing) if existing == &evidence => {}
                    Some(existing) if evidence.refines(existing) => {
                        lifecycle.prepared = Some(evidence);
                    }
                    Some(_) => lifecycle.inconsistent = true,
                }
            }
            RolloutRecord::ToolFinished {
                call_id,
                output,
                error,
                file_change,
                requires_inspection,
            } => {
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                let lifecycle = tool_lifecycles.entry(call_id).or_default();
                let finished = LoadedToolFinish {
                    output,
                    error,
                    file_change,
                    requires_inspection,
                };
                match &lifecycle.finished {
                    None => lifecycle.finished = Some(finished),
                    Some(existing)
                        if existing.output == finished.output
                            && existing.error == finished.error
                            && existing.file_change == finished.file_change
                            && existing.requires_inspection == finished.requires_inspection => {}
                    Some(_) => lifecycle.inconsistent = true,
                }
            }
            RolloutRecord::Usage {
                usage: new_usage,
                history_estimate,
                server_reasoning_included: reasoning_included,
            } => {
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                total_usage.add_assign(&new_usage);
                usage = Some(new_usage);
                usage_history_estimate = Some(history_estimate);
                server_reasoning_included = reasoning_included;
            }
            RolloutRecord::UsageTotal {
                usage: response_usage,
            } => {
                unfinished_turn_has_activity |= unfinished_turn.is_some();
                total_usage.add_assign(&response_usage);
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
                if let Some(active_turn) = &unfinished_turn {
                    return Err(anyhow!(
                        "turn {turn_id} started before active turn {active_turn} finished at {}:{line_number}",
                        path.display()
                    ));
                }
                tool_lifecycles.clear();
                unfinished_turn_call_ids.clear();
                unfinished_turn = Some(turn_id);
                unfinished_turn_has_activity = false;
                unfinished_turn_has_recovery_notice = false;
                unfinished_turn_recovered = false;
                recovery_checkpoint_requires_inspection = false;
                legacy_recovery_requires_inspection = false;
                unknown_record_requires_inspection = false;
            }
            RolloutRecord::TurnFinished { turn_id, .. } => {
                if unfinished_turn.as_deref() != Some(turn_id.as_str()) {
                    return Err(anyhow!(
                        "turn {turn_id} finished without a matching active turn at {}:{line_number}",
                        path.display()
                    ));
                }
                unfinished_turn = None;
                unfinished_turn_has_activity = false;
                unfinished_turn_has_recovery_notice = false;
                unfinished_turn_recovered = false;
                recovery_checkpoint_requires_inspection = false;
                legacy_recovery_requires_inspection = false;
                unknown_record_requires_inspection = false;
                tool_lifecycles.clear();
                unfinished_turn_call_ids.clear();
            }
            RolloutRecord::Unknown => {
                // A future record inside an active turn may describe observable work. Ignore its
                // payload for forward compatibility, but never classify a crash after unknown work
                // as harmless or encourage a retry without inspection.
                if unfinished_turn.is_some() {
                    unfinished_turn_has_activity = true;
                    unknown_record_requires_inspection = true;
                    if unfinished_turn_recovered {
                        // The checkpoint only covers records through itself. A future record after
                        // it reopens recovery rather than being silently hidden by turn closure.
                        unfinished_turn_recovered = false;
                        unfinished_turn_has_recovery_notice = false;
                    }
                }
            }
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
    // Repair presentation for malformed completed turns as well as active crash recovery. The
    // active-turn checks below still scope lifecycle evidence and inspection guidance to work that
    // was durably associated with the unfinished turn.
    let missing_calls = crate::context::missing_call_output_ids(&history);
    if unfinished_turn_recovered && !missing_calls.is_empty() {
        return Err(anyhow!(
            "turn recovery checkpoint in {} left missing tool outputs",
            path.display()
        ));
    }
    // Lifecycle evidence belongs only to calls durably appended after this active turn started.
    // A sparse record must never borrow an older completed call merely because its ID matches.
    let active_missing_calls = missing_calls
        .intersection(&unfinished_turn_call_ids)
        .cloned()
        .collect::<HashSet<_>>();
    let tool_recoveries = if unfinished_turn.is_some() && !unfinished_turn_recovered {
        recover_interrupted_tools(&history, &active_missing_calls, &tool_lifecycles)
    } else {
        HashMap::new()
    };
    let unrecovered_calls = if unfinished_turn.is_some() {
        missing_calls
            .iter()
            .filter(|call_id| !tool_recoveries.contains_key(*call_id))
            .cloned()
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };
    let unmatched_tool_lifecycle_requires_inspection = unfinished_turn.is_some()
        && tool_lifecycles
            .keys()
            .any(|call_id| !active_missing_calls.contains(call_id));
    let crash_recovery_requires_inspection = recovery_checkpoint_requires_inspection
        || legacy_recovery_requires_inspection
        || unknown_record_requires_inspection
        || unmatched_tool_lifecycle_requires_inspection
        || !unrecovered_calls.is_empty()
        || tool_recoveries
            .values()
            .any(|recovery| recovery.requires_inspection);
    normalized_aborted_calls.extend(unrecovered_calls);
    let transcript_checkpoint =
        (has_transcript_checkpoint && transcript_tail.is_empty()).then_some(transcript.len());
    transcript.append(&mut transcript_tail);
    apply_current_missing_aborts(&mut transcript, &missing_calls);
    apply_tool_recoveries(&mut transcript, &tool_recoveries);
    apply_normalized_aborts(&mut transcript, &normalized_aborted_calls);

    let rollout = Rollout {
        file: Arc::new(SharedRolloutFile {
            file: Mutex::new(file),
            path,
            #[cfg(test)]
            fail_next_append: std::sync::atomic::AtomicBool::new(false),
        }),
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
        unfinished_turn_has_activity,
        unfinished_turn_has_recovery_notice,
        unfinished_turn_recovered,
        tool_recoveries,
        crash_recovery_requires_inspection,
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
    tool_indices: &mut HashMap<String, usize>,
    items: &[Value],
) {
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => append_transcript_message(transcript, item),
            Some("web_search_call") => {
                if let Some(search) = crate::web_search::WebSearchCall::from_response_item(item) {
                    transcript.push(SessionTranscriptItem::WebSearch { search });
                }
            }
            Some("function_call" | "custom_tool_call") => {
                let Some(tool) = transcript_tool_from_history(item) else {
                    continue;
                };
                let call_id = tool.call_id.clone();
                let index = transcript.len();
                transcript.push(SessionTranscriptItem::Tool { tool });
                tool_indices.insert(call_id, index);
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                if is_legacy_exec_notification(item) {
                    // Legacy Code Mode `notify(...)` records reuse the outer call ID. They are
                    // model-history notifications, not the final transcript output for the call.
                    continue;
                }
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(index) = tool_indices.get(call_id).copied() else {
                    // Output-only records were not separate transcript cells while live, so they
                    // should not become orphan cells here.
                    continue;
                };
                let Some(SessionTranscriptItem::Tool { tool }) = transcript.get_mut(index) else {
                    continue;
                };
                if tool.output.is_none() {
                    let output = item.get("output").cloned().unwrap_or(Value::Null);
                    tool.output = Some(transcript_tool_output_from_history(&tool.name, output));
                }
            }
            _ => {}
        }
    }
}

fn apply_transcript_tool_outcomes(
    transcript: &mut [SessionTranscriptItem],
    outcomes: &[SessionTranscriptToolOutcome],
) {
    for outcome in outcomes {
        let Some(tool) = transcript.iter_mut().rev().find_map(|item| match item {
            SessionTranscriptItem::Tool { tool } if tool.call_id == outcome.call_id => Some(tool),
            SessionTranscriptItem::Exploration { tools } => tools
                .iter_mut()
                .rev()
                .find(|tool| tool.call_id == outcome.call_id),
            _ => None,
        }) else {
            continue;
        };
        if let Some(output) = &outcome.output {
            tool.output = Some(output.clone());
        } else if let Some(error) = &outcome.error {
            tool.output = Some(SessionTranscriptToolOutput::Error(error.clone()));
        }
        if let Some(file_change) = &outcome.file_change {
            tool.file_change = Some(file_change.clone());
        }
    }
}

fn recovery_checkpoint_has_current_notice(
    previous: &[Value],
    replacement: &[Value],
    had_recovery_notice: bool,
) -> bool {
    let previous_notices = previous
        .iter()
        .filter(|item| crate::context::is_turn_abort_notice(item))
        .count();
    let replacement_notices = replacement
        .iter()
        .filter(|item| crate::context::is_turn_abort_notice(item))
        .count();
    if had_recovery_notice {
        replacement_notices > 0 && replacement_notices >= previous_notices
    } else {
        replacement_notices > previous_notices
    }
}

fn recovery_checkpoint_outcomes_are_complete(
    previous: &[Value],
    outcomes: Option<&[SessionTranscriptToolOutcome]>,
) -> bool {
    let mut missing_calls = crate::context::missing_call_output_ids(previous);
    let outcomes = outcomes.unwrap_or_default();
    outcomes.len() == missing_calls.len()
        && outcomes.iter().all(|outcome| {
            outcome.output.is_some() && missing_calls.remove(outcome.call_id.as_str())
        })
        && missing_calls.is_empty()
}

fn normalization_outcomes_contain_recovery(outcomes: &[SessionTranscriptToolOutcome]) -> bool {
    outcomes.iter().any(|outcome| {
        outcome.output.as_ref().is_some_and(|output| match output {
            SessionTranscriptToolOutput::Error(message) => message.starts_with("Recovery:"),
            SessionTranscriptToolOutput::Success(_) => {
                output.recovered_file_state_message().is_some()
            }
        })
    })
}

pub(crate) fn is_legacy_exec_notification(item: &Value) -> bool {
    // Legacy Code Mode `notify(...)` records carry the outer tool name and no item ID. They may
    // repeat and do not prove that the custom tool produced its final output; older synthetic final
    // outputs can carry the same name but always serialized `id`.
    item.get("type").and_then(Value::as_str) == Some("custom_tool_call_output")
        && item.get("name").and_then(Value::as_str) == Some("exec")
        && item.get("id").is_none()
}

fn normalized_abort_call_ids(previous: &[Value], replacement: &[Value]) -> HashSet<String> {
    let missing = crate::context::missing_call_output_ids(previous);
    crate::context::canonical_synthetic_abort_call_ids(replacement)
        .intersection(&missing)
        .cloned()
        .collect()
}

fn remove_completed_tool_lifecycles_from_append(
    lifecycles: &mut HashMap<String, LoadedToolLifecycle>,
    items: &[Value],
) {
    let latest_calls = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let is_function = match item.get("type").and_then(Value::as_str)? {
                "function_call" | "local_shell_call" => true,
                "custom_tool_call" => false,
                _ => return None,
            };
            Some((
                item.get("call_id")?.as_str()?.to_string(),
                (index, is_function),
            ))
        })
        .collect::<HashMap<_, _>>();
    for (index, item) in items.iter().enumerate() {
        if item.get("type").and_then(Value::as_str) != Some("function_call_output") {
            continue;
        }
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            continue;
        };
        let completes_current_call = latest_calls
            .get(call_id)
            .is_none_or(|(call_index, is_function)| *is_function && index > *call_index);
        if completes_current_call {
            lifecycles.remove(call_id);
        }
    }
}

fn remove_completed_tool_lifecycles_from_replacement(
    lifecycles: &mut HashMap<String, LoadedToolLifecycle>,
    items: &[Value],
) {
    for call_id in crate::context::completed_function_call_ids(items) {
        lifecycles.remove(&call_id);
    }
}

fn saved_tool_call_id(item: &Value) -> Option<&str> {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call" | "custom_tool_call" | "local_shell_call") => {
            item.get("call_id").and_then(Value::as_str)
        }
        _ => None,
    }
}

struct RecoveryCall {
    call_id: String,
    name: Option<String>,
    effect: Option<ToolEffectClass>,
    ambiguous: bool,
}

impl RecoveryCall {
    fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("unknown tool")
    }
}

fn recover_interrupted_tools(
    history: &[Value],
    missing_calls: &HashSet<String>,
    lifecycles: &HashMap<String, LoadedToolLifecycle>,
) -> HashMap<String, ToolRecovery> {
    let mut calls = Vec::<RecoveryCall>::new();
    let mut call_indices = HashMap::<String, usize>::new();
    // Preserve newest-first order so a bounded hashing budget is spent on the latest mutation.
    // A repeated call ID is never allowed to borrow lifecycle evidence from an arbitrary copy.
    for item in history.iter().rev() {
        let Some(call) = recovery_call(item) else {
            continue;
        };
        if !missing_calls.contains(&call.call_id) {
            continue;
        }
        if let Some(index) = call_indices.get(&call.call_id).copied() {
            calls[index].ambiguous = true;
        } else {
            call_indices.insert(call.call_id.clone(), calls.len());
            calls.push(call);
        }
    }

    let mut remaining_hash_bytes = MAX_TOOL_RECOVERY_HASH_BYTES;
    calls
        .into_iter()
        .map(|call| {
            let lifecycle = lifecycles.get(&call.call_id);
            let recovery = recover_tool_lifecycle(&call, lifecycle, &mut remaining_hash_bytes);
            (call.call_id, recovery)
        })
        .collect()
}

fn recovery_call(item: &Value) -> Option<RecoveryCall> {
    let item_type = item.get("type")?.as_str()?;
    let call_id = item.get("call_id")?.as_str()?.to_string();
    let (name, effect) = match item_type {
        "function_call" => {
            let name = normalized_function_name(item);
            let effect = name.as_deref().and_then(ToolEffectClass::for_tool);
            (name, effect)
        }
        "custom_tool_call" => (
            item.get("name").and_then(Value::as_str).map(str::to_string),
            Some(ToolEffectClass::Opaque),
        ),
        "local_shell_call" => (
            item.get("name").and_then(Value::as_str).map_or_else(
                || Some("local_shell".to_string()),
                |name| Some(name.to_string()),
            ),
            Some(ToolEffectClass::Opaque),
        ),
        _ => return None,
    };
    Some(RecoveryCall {
        call_id,
        name,
        effect,
        ambiguous: false,
    })
}

fn tool_lifecycle_registrations(items: &[Value]) -> Vec<ToolLifecycleRegistration> {
    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .filter_map(|item| {
            let call_id = item.get("call_id")?.as_str()?.to_string();
            let name = normalized_function_name(item)?;
            let effect = ToolEffectClass::for_tool(&name)?;
            Some(ToolLifecycleRegistration {
                call_id,
                name,
                effect,
            })
        })
        .collect()
}

fn normalized_function_name(item: &Value) -> Option<String> {
    let name = item.get("name")?.as_str()?;
    match item.get("namespace") {
        None | Some(Value::Null) => Some(name.to_string()),
        Some(Value::String(namespace)) if namespace.is_empty() || namespace == "functions" => {
            Some(name.to_string())
        }
        Some(Value::String(namespace)) => Some(format!("{namespace}.{name}")),
        Some(_) => None,
    }
}

fn recover_tool_lifecycle(
    call: &RecoveryCall,
    lifecycle: Option<&LoadedToolLifecycle>,
    remaining_hash_bytes: &mut u64,
) -> ToolRecovery {
    if call.ambiguous {
        return uncertain_recovery_error(format!(
            "Recovery: more than one saved tool call used ID `{}`. Lifecycle evidence cannot be associated with one call safely; effects are unknown. Inspect relevant state before retrying.",
            call.call_id
        ));
    }

    let registration_is_exact = lifecycle.is_some_and(|lifecycle| {
        lifecycle.registration.as_ref().is_some_and(|registration| {
            registration.call_id == call.call_id
                && call.name.as_deref() == Some(registration.name.as_str())
                && call.effect == Some(registration.effect)
        })
    });
    let registration_is_inconsistent = lifecycle.is_some_and(|lifecycle| {
        lifecycle.inconsistent
            || lifecycle
                .registration
                .as_ref()
                .is_some_and(|_| !registration_is_exact)
    });
    if registration_is_inconsistent {
        return uncertain_recovery_error(format!(
            "Recovery: lifecycle records for the prior {} call conflict with its saved history. Effects are unknown; inspect relevant state before retrying it.",
            call.name()
        ));
    }

    if let Some(finished) = lifecycle.and_then(|lifecycle| lifecycle.finished.as_ref()) {
        let transcript_output = if let Some(error) = &finished.error {
            SessionTranscriptToolOutput::Error(error.clone())
        } else if finished.requires_inspection
            && call.effect == Some(ToolEffectClass::AtomicMutation)
        {
            SessionTranscriptToolOutput::recovered_file_state(history_output_text(&finished.output))
        } else {
            transcript_tool_output_from_history(call.name(), finished.output.clone())
        };
        return ToolRecovery {
            output: finished.output.clone(),
            transcript_output,
            file_change: finished.file_change.clone(),
            requires_inspection: finished.requires_inspection,
        };
    }

    match call.effect {
        Some(ToolEffectClass::ReadOnly) => recovery_error(format!(
            "Recovery: the prior {} call did not produce a durable result. It has no intentional workspace mutation; repeat the read if the observation is still needed.",
            call.name()
        )),
        Some(ToolEffectClass::Opaque) => {
            let Some(lifecycle) = lifecycle else {
                return uncertain_recovery_error(format!(
                    "Recovery: the prior {} call has no lifecycle records or durable result. It may have produced local or external effects; inspect relevant state before retrying it.",
                    call.name()
                ));
            };
            if lifecycle.started {
                uncertain_recovery_error(format!(
                    "Recovery: the prior {} call started but has no durable completion record. Local or external effects are unknown; inspect relevant state before retrying it.",
                    call.name()
                ))
            } else if registration_is_exact {
                recovery_error(format!(
                    "Recovery: the prior {} call was registered but did not start. Its command was not executed.",
                    call.name()
                ))
            } else {
                uncertain_recovery_error(format!(
                    "Recovery: the prior {} call has sparse lifecycle records that do not prove whether it started. Effects are unknown; inspect relevant state before retrying it.",
                    call.name()
                ))
            }
        }
        Some(ToolEffectClass::AtomicMutation) => {
            let Some(lifecycle) = lifecycle else {
                return uncertain_recovery_error(format!(
                    "Recovery: the prior {} call has no lifecycle records or durable result. Its file mutation may have been attempted; inspect the target before retrying it.",
                    call.name()
                ));
            };
            if let Some(evidence) = &lifecycle.prepared {
                return recover_prepared_mutation(call.name(), evidence, remaining_hash_bytes);
            }
            if lifecycle.started {
                return file_recovery(
                    format!(
                        "Recovery: the prior {} call stopped before its file mutation was prepared. Its intended file mutation was not attempted.",
                        call.name()
                    ),
                    false,
                );
            }
            if registration_is_exact {
                return file_recovery(
                    format!(
                        "Recovery: the prior {} call was registered but did not start. Its intended file mutation was not attempted.",
                        call.name()
                    ),
                    false,
                );
            }
            uncertain_recovery_error(format!(
                "Recovery: the prior {} call has sparse lifecycle records that do not prove whether its file mutation started. The outcome is unknown; inspect the target before retrying it.",
                call.name()
            ))
        }
        None => uncertain_recovery_error(format!(
            "Recovery: the prior {} call has no durable result and its effects are not classified. Inspect relevant state before retrying it.",
            call.name()
        )),
    }
}

fn recovery_error(message: String) -> ToolRecovery {
    ToolRecovery {
        output: Value::String(message.clone()),
        transcript_output: SessionTranscriptToolOutput::Error(message),
        file_change: None,
        requires_inspection: false,
    }
}

fn uncertain_recovery_error(message: String) -> ToolRecovery {
    ToolRecovery {
        output: Value::String(message.clone()),
        transcript_output: SessionTranscriptToolOutput::Error(message),
        file_change: None,
        requires_inspection: true,
    }
}

fn file_recovery(message: String, requires_inspection: bool) -> ToolRecovery {
    ToolRecovery {
        output: Value::String(message.clone()),
        transcript_output: SessionTranscriptToolOutput::recovered_file_state(message),
        file_change: None,
        requires_inspection,
    }
}

fn recovery_path_label(path: &Path) -> String {
    if let Some(path) = path.to_str() {
        let mut escaped = String::with_capacity(path.len());
        for character in path.chars() {
            match character {
                '\\' => escaped.push_str("\\\\"),
                '`' => escaped.push_str("\\`"),
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                character if character.is_control() => escaped.extend(character.escape_default()),
                character => escaped.push(character),
            }
        }
        return escaped;
    }

    use std::os::unix::ffi::OsStrExt as _;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = path.as_os_str().as_bytes();
    let mut escaped = String::with_capacity(bytes.len().saturating_mul(4).saturating_add(11));
    escaped.push_str("unix-bytes:");
    for &byte in bytes {
        match byte {
            b'\\' => escaped.push_str("\\\\"),
            b'`' => escaped.push_str("\\`"),
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            b'\t' => escaped.push_str("\\t"),
            b' '..=b'~' => escaped.push(char::from(byte)),
            _ => {
                escaped.push_str("\\x");
                escaped.push(char::from(HEX[usize::from(byte >> 4)]));
                escaped.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
    }
    escaped
}

fn recover_prepared_mutation(
    name: &str,
    evidence: &ToolMutationEvidence,
    remaining_hash_bytes: &mut u64,
) -> ToolRecovery {
    let staging_residue = cleanup_interrupted_staging(evidence);
    let current = inspect_recovery_target(evidence, remaining_hash_bytes);
    let lacks_stable_path_identity = evidence.target_parent.is_none() && evidence.staging.is_none();
    let target = recovery_path_label(
        evidence
            .path_resolution
            .as_ref()
            .map_or(&evidence.target, |resolution| &resolution.requested),
    );
    if matches!(&current, RecoveryTargetState::Digest(digest) if digest == &evidence.post_state) {
        let mut recovery = if lacks_stable_path_identity {
            format!(
                "Recovery: the saved target `{target}` currently has the exact contents intended by the interrupted {name} call. This older lifecycle evidence does not include a stable parent identity or requested symlink route, so the effect at the originally requested path is unknown; inspect relevant paths before retrying."
            )
        } else {
            format!(
                "Recovery: `{target}` currently has the exact contents intended by the interrupted {name} call. Do not repeat the call unless a new change is needed."
            )
        };
        append_staging_recovery_warning(&mut recovery, staging_residue.as_deref());
        return file_recovery(
            recovery,
            lacks_stable_path_identity || staging_residue.is_some(),
        );
    }

    let matches_pre_state = match (&current, &evidence.pre_state) {
        (RecoveryTargetState::Absent, ToolTargetPreState::Absent) => true,
        (RecoveryTargetState::Digest(current), ToolTargetPreState::Digest(previous)) => {
            current == previous
        }
        _ => false,
    };
    if matches_pre_state {
        let mut message = match (&evidence.pre_state, lacks_stable_path_identity) {
            (ToolTargetPreState::Absent, false) => format!(
                "Recovery: `{target}` is absent now; the contents intended by the interrupted {name} call are not present."
            ),
            (ToolTargetPreState::Digest(_), false) => format!(
                "Recovery: `{target}` currently has the exact recorded pre-mutation contents for the interrupted {name} call; its intended contents are not present now."
            ),
            (ToolTargetPreState::Absent, true) => format!(
                "Recovery: the saved target `{target}` is absent now. This older lifecycle evidence does not include a stable parent identity or requested symlink route, so the effect at the originally requested path is unknown; inspect relevant paths before retrying."
            ),
            (ToolTargetPreState::Digest(_), true) => format!(
                "Recovery: the saved target `{target}` currently has the exact recorded pre-mutation contents for the interrupted {name} call. This older lifecycle evidence does not include a stable parent identity or requested symlink route, so the effect at the originally requested path is unknown; inspect relevant paths before retrying."
            ),
            (ToolTargetPreState::Unknown, _) => {
                unreachable!("a matching pre-state cannot be unknown")
            }
        };
        append_parent_creation_recovery_warning(&mut message, evidence);
        append_staging_recovery_warning(&mut message, staging_residue.as_deref());
        return file_recovery(
            message,
            lacks_stable_path_identity || staging_residue.is_some(),
        );
    }

    let mut message = match current {
        RecoveryTargetState::Digest(_) | RecoveryTargetState::Mismatch => format!(
            "Recovery: `{target}` does not match the intended post-state of the interrupted {name} call, and its prior state cannot be confirmed. The outcome is unknown; inspect the file before retrying."
        ),
        RecoveryTargetState::Absent => format!(
            "Recovery: `{target}` is absent and does not match the recorded state of the interrupted {name} call. The outcome is unknown; inspect relevant state before retrying."
        ),
        RecoveryTargetState::Unknown => format!(
            "Recovery: the current state of `{target}` could not be reconciled with the interrupted {name} call. The outcome is unknown; inspect the file before retrying."
        ),
    };
    append_parent_creation_recovery_warning(&mut message, evidence);
    append_staging_recovery_warning(&mut message, staging_residue.as_deref());
    file_recovery(message, true)
}

fn append_parent_creation_recovery_warning(message: &mut String, evidence: &ToolMutationEvidence) {
    if let Some(parent) = evidence
        .missing_parent
        .as_ref()
        .filter(|parent| std::fs::symlink_metadata(parent).is_ok_and(|metadata| metadata.is_dir()))
    {
        message.push_str(&format!(
            " The directory `{}` may remain from parent creation.",
            recovery_path_label(parent)
        ));
    }
}

fn append_staging_recovery_warning(message: &mut String, residue: Option<&Path>) {
    if let Some(residue) = residue {
        message.push_str(&format!(
            " The private staging directory `{}` may remain.",
            recovery_path_label(residue)
        ));
    }
}

fn cleanup_interrupted_staging(evidence: &ToolMutationEvidence) -> Option<PathBuf> {
    let staging = evidence.staging.as_ref()?;
    let parent = evidence.target.parent()?;
    let residue = parent.join(&staging.name);
    let Some(expected_directory) = staging.directory else {
        return match std::fs::symlink_metadata(&residue) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Ok(_) | Err(_) => Some(residue),
        };
    };
    let Some(expected_parent) = evidence.target_parent else {
        return Some(residue);
    };
    let target = match crate::private_fs::AnchoredPath::open(&evidence.target) {
        Ok(target) => target,
        Err(_) => return Some(residue),
    };
    if target.parent_identity() != expected_parent
        || !target.parent_path_is_current().unwrap_or(false)
    {
        return Some(residue);
    }
    let staging_name = OsStr::new(&staging.name);
    let directory = match target.open_child_directory(staging_name) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(residue),
    };
    if directory.identity().ok() != Some(expected_directory) {
        return Some(residue);
    }
    match (
        staging.content,
        directory.entry_metadata(OsStr::new("content")),
    ) {
        (Some(expected), Ok(Some(metadata)))
            if metadata.is_file() && metadata.object_identity() == expected.object_identity() =>
        {
            // A crash during population or a successful create-via-hard-link commit can change
            // size, timestamps, or link count while leaving the private staging inode ours. As in
            // live cleanup, only an entry substitution makes unlinking this owned name unsafe.
            if directory.remove_file(OsStr::new("content")).is_err() {
                return Some(residue);
            }
        }
        (Some(_), Ok(None)) | (None, Ok(None)) => {}
        (Some(_), Ok(Some(_))) | (None, Ok(Some(_))) | (_, Err(_)) => return Some(residue),
    }
    match target.child_metadata(staging_name) {
        Ok(Some(metadata))
            if metadata.is_directory() && metadata.object_identity() == expected_directory => {}
        Ok(None) => return None,
        Ok(Some(_)) | Err(_) => return Some(residue),
    }
    if target.remove_directory(staging_name).is_err() {
        return Some(residue);
    }
    None
}

enum RecoveryTargetState {
    Absent,
    Digest(ToolContentDigest),
    Mismatch,
    Unknown,
}

fn inspect_recovery_target(
    evidence: &ToolMutationEvidence,
    remaining_hash_bytes: &mut u64,
) -> RecoveryTargetState {
    let path_resolution_is_current = || {
        evidence
            .path_resolution
            .as_ref()
            .is_none_or(ToolPathResolutionEvidence::is_current)
    };
    if !path_resolution_is_current() {
        return RecoveryTargetState::Unknown;
    }
    let target = match crate::private_fs::AnchoredPath::open(&evidence.target) {
        Ok(target) => target,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && evidence.target_parent.is_none() =>
        {
            return if path_resolution_is_current() {
                RecoveryTargetState::Absent
            } else {
                RecoveryTargetState::Unknown
            };
        }
        Err(_) => return RecoveryTargetState::Unknown,
    };
    if evidence
        .target_parent
        .is_some_and(|identity| identity != target.parent_identity())
        || !target.parent_path_is_current().unwrap_or(false)
    {
        return RecoveryTargetState::Unknown;
    }
    let metadata = match target.entry_metadata() {
        Ok(Some(metadata)) if metadata.is_file() => metadata,
        Ok(Some(_)) | Err(_) => return RecoveryTargetState::Unknown,
        Ok(None) => {
            return if target.parent_path_is_current().unwrap_or(false)
                && path_resolution_is_current()
            {
                RecoveryTargetState::Absent
            } else {
                RecoveryTargetState::Unknown
            };
        }
    };
    let snapshot = metadata.snapshot();
    let bytes = snapshot.byte_len();
    let matches_candidate_size = evidence.post_state.bytes == bytes
        || matches!(
            &evidence.pre_state,
            ToolTargetPreState::Digest(digest) if digest.bytes == bytes
        );
    if !matches_candidate_size {
        return RecoveryTargetState::Mismatch;
    }
    if bytes > *remaining_hash_bytes {
        return RecoveryTargetState::Unknown;
    }

    let file = match target.open_for_read() {
        Ok(file) => file,
        Err(_) => return RecoveryTargetState::Unknown,
    };
    match file.metadata() {
        Ok(metadata)
            if metadata.is_file() && crate::private_fs::file_snapshot(&metadata) == snapshot => {}
        Ok(_) | Err(_) => return RecoveryTargetState::Unknown,
    }
    // Charge only attempts that reached a stable regular-file handle. Failed opens and path
    // substitutions should not consume the bounded budget needed to classify later calls.
    *remaining_hash_bytes = remaining_hash_bytes.saturating_sub(bytes);

    let mut bounded = file.take(bytes);
    let digest = match ToolContentDigest::from_reader(&mut bounded) {
        Ok(digest) if digest.bytes == bytes => digest,
        Ok(_) | Err(_) => return RecoveryTargetState::Unknown,
    };
    let file = bounded.into_inner();
    let final_opened_metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return RecoveryTargetState::Unknown,
    };
    let final_path_metadata = match target.entry_metadata() {
        Ok(Some(metadata)) if metadata.is_file() => metadata,
        Ok(Some(_)) | Ok(None) | Err(_) => return RecoveryTargetState::Unknown,
    };
    if crate::private_fs::file_snapshot(&final_opened_metadata) != snapshot
        || final_path_metadata.snapshot() != snapshot
        || !target.parent_path_is_current().unwrap_or(false)
        || !path_resolution_is_current()
    {
        return RecoveryTargetState::Unknown;
    }
    RecoveryTargetState::Digest(digest)
}

fn apply_tool_recoveries(
    transcript: &mut [SessionTranscriptItem],
    recoveries: &HashMap<String, ToolRecovery>,
) {
    if recoveries.is_empty() {
        return;
    }
    let outcomes = recoveries
        .iter()
        .map(|(call_id, recovery)| SessionTranscriptToolOutcome {
            call_id: call_id.clone(),
            output: Some(recovery.transcript_output.clone()),
            error: None,
            file_change: recovery.file_change.clone(),
        })
        .collect::<Vec<_>>();
    apply_transcript_tool_outcomes(transcript, &outcomes);
}

fn apply_current_missing_aborts(
    transcript: &mut [SessionTranscriptItem],
    missing_calls: &HashSet<String>,
) {
    if missing_calls.is_empty() {
        return;
    }
    let mut latest_calls = missing_calls.clone();
    for item in transcript.iter_mut().rev() {
        match item {
            SessionTranscriptItem::Tool { tool } => {
                mark_current_missing_tool_aborted(tool, missing_calls, &mut latest_calls);
            }
            SessionTranscriptItem::Exploration { tools } => {
                for tool in tools.iter_mut().rev() {
                    mark_current_missing_tool_aborted(tool, missing_calls, &mut latest_calls);
                }
            }
            _ => {}
        }
    }
}

fn mark_current_missing_tool_aborted(
    tool: &mut SessionTranscriptTool,
    missing_calls: &HashSet<String>,
    latest_calls: &mut HashSet<String>,
) {
    if tool.origin != SessionTranscriptToolOrigin::Agent || !missing_calls.contains(&tool.call_id) {
        return;
    }
    if latest_calls.remove(&tool.call_id) || tool.output.is_none() {
        tool.output = Some(SessionTranscriptToolOutput::Error(
            SYNTHETIC_ABORT_OUTPUT.to_string(),
        ));
    }
}

fn apply_latest_normalization_aborts(
    transcript: &mut [SessionTranscriptItem],
    transcript_tail: &mut [SessionTranscriptItem],
    aborted_calls: &HashSet<String>,
) {
    let mut remaining = aborted_calls.clone();
    overwrite_latest_aborted_tools(transcript_tail, &mut remaining);
    overwrite_latest_aborted_tools(transcript, &mut remaining);
}

fn overwrite_latest_aborted_tools(
    transcript: &mut [SessionTranscriptItem],
    remaining: &mut HashSet<String>,
) {
    for item in transcript.iter_mut().rev() {
        match item {
            SessionTranscriptItem::Tool { tool } => overwrite_latest_tool_abort(tool, remaining),
            SessionTranscriptItem::Exploration { tools } => {
                for tool in tools.iter_mut().rev() {
                    overwrite_latest_tool_abort(tool, remaining);
                }
            }
            _ => {}
        }
        if remaining.is_empty() {
            break;
        }
    }
}

fn overwrite_latest_tool_abort(tool: &mut SessionTranscriptTool, remaining: &mut HashSet<String>) {
    if tool.origin == SessionTranscriptToolOrigin::Agent && remaining.remove(&tool.call_id) {
        tool.output = Some(SessionTranscriptToolOutput::Error(
            SYNTHETIC_ABORT_OUTPUT.to_string(),
        ));
    }
}

fn apply_normalized_aborts(
    transcript: &mut [SessionTranscriptItem],
    aborted_calls: &HashSet<String>,
) {
    if aborted_calls.is_empty() {
        return;
    }
    for item in transcript {
        match item {
            SessionTranscriptItem::Tool { tool } => mark_tool_aborted(tool, aborted_calls),
            SessionTranscriptItem::Exploration { tools } => {
                for tool in tools {
                    mark_tool_aborted(tool, aborted_calls);
                }
            }
            _ => {}
        }
    }
}

fn mark_tool_aborted(tool: &mut SessionTranscriptTool, aborted_calls: &HashSet<String>) {
    if tool.origin == SessionTranscriptToolOrigin::Agent
        && tool.output.is_none()
        && aborted_calls.contains(&tool.call_id)
    {
        tool.output = Some(SessionTranscriptToolOutput::Error(
            SYNTHETIC_ABORT_OUTPUT.to_string(),
        ));
    }
}

fn flush_transcript_history(
    history: &[Value],
    start: &mut Option<usize>,
    transcript: &mut Vec<SessionTranscriptItem>,
    tool_indices: &mut HashMap<String, usize>,
) {
    let Some(start) = start.take() else {
        return;
    };
    append_transcript_items(
        transcript,
        tool_indices,
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
    if !recovered.iter().any(|item| {
        matches!(
            item,
            SessionTranscriptItem::Tool { .. } | SessionTranscriptItem::WebSearch { .. }
        )
    }) {
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
                || crate::context::is_contextual_user_message(item)
            {
                return;
            }
            transcript.push(SessionTranscriptItem::User { text, image_count });
        }
        "assistant" if !text.trim().is_empty() => {
            let Some(message) =
                crate::assistant_message::AssistantMessage::from_response_item(item)
            else {
                return;
            };
            transcript.push(SessionTranscriptItem::Assistant {
                text: message.text,
                phase: message.phase,
                citations: message.citations,
            });
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
        Some(Value::String(namespace)) if namespace.is_empty() || namespace == "functions" => None,
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
        origin: SessionTranscriptToolOrigin::Agent,
        name,
        input,
        output: None,
        file_change: None,
    })
}

fn transcript_tool_output_from_history(name: &str, output: Value) -> SessionTranscriptToolOutput {
    let text = history_output_text(&output);
    if text.starts_with("Script failed") || text.contains("\nScript error:\n") {
        return SessionTranscriptToolOutput::Error(text);
    }
    let projected = match name {
        "bash" => match serde_json::from_str(&text) {
            Ok(output) => output,
            Err(_) => return SessionTranscriptToolOutput::Error(text),
        },
        "read" | "write" | "edit" => Value::Null,
        "exec" | "wait" => Value::String(text),
        "exec_command" | "write_stdin" => project_process_output(output, &text),
        "apply_patch" | "update_plan" | "view_image" | "web.run" => Value::Null,
        // Older rollouts can contain results from the removed OpenAI Docs namespace. Keep those
        // document bodies out of resumed transcript snapshots.
        name if name.starts_with("openaiDeveloperDocs.") => Value::Null,
        "log_papercut" => serde_json::from_str(&text).unwrap_or(output),
        _ => Value::Null,
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

fn has_saved_sessions_in(root: &Path) -> Result<bool> {
    let sessions = root.join(SESSIONS_DIRECTORY);
    let entries = match std::fs::read_dir(&sessions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", sessions.display()));
        }
    };
    for entry in entries {
        let Some(entry) = saved_session_entry(entry, &sessions) else {
            continue;
        };
        let Some((path, modified_at)) = saved_session_candidate(entry) else {
            continue;
        };
        if discover_session_summary(&path, modified_at).is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn latest_rollout_for_cwd(sessions: &Path, cwd: &Path) -> Result<Option<PathBuf>> {
    let entries = match std::fs::read_dir(sessions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to list saved bettercodex sessions"),
    };
    let mut latest = None::<(u128, u64, PathBuf)>;
    for entry in entries {
        let Some(entry) = saved_session_entry(entry, sessions) else {
            continue;
        };
        let Some((path, modified_at)) = saved_session_candidate(entry) else {
            continue;
        };
        let Some(summary) = discover_session_summary(&path, modified_at) else {
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
        let Some(entry) = saved_session_entry(entry, &sessions_directory) else {
            continue;
        };
        if let Some(candidate) = saved_session_candidate(entry) {
            candidates.push(candidate);
        }
    }
    let worker_count = session_list_worker_count(candidates.len());
    let mut sessions = if worker_count == 1 {
        read_session_summaries(candidates.iter())
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
                    .map_err(|_| anyhow!("saved session discovery worker panicked"))?;
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

fn saved_session_entry(
    entry: std::io::Result<std::fs::DirEntry>,
    directory: &Path,
) -> Option<std::fs::DirEntry> {
    match entry {
        Ok(entry) => Some(entry),
        Err(error) => {
            tracing::warn!(
                directory = %directory.display(),
                %error,
                "skipping an unreadable saved session directory entry"
            );
            None
        }
    }
}

fn saved_session_candidate(entry: std::fs::DirEntry) -> Option<(PathBuf, Option<SystemTime>)> {
    let path = entry.path();
    if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
        return None;
    }
    let file_type = match entry.file_type() {
        Ok(file_type) => file_type,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "skipping an unreadable saved session entry"
            );
            return None;
        }
    };
    if !file_type.is_file() {
        return None;
    }
    let modified_at = entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok();
    Some((path, modified_at))
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
) -> Vec<SessionSummary> {
    candidates
        .filter_map(|(path, modified_at)| discover_session_summary(path, *modified_at))
        .collect()
}

fn discover_session_summary(
    path: &Path,
    modified_at: Option<SystemTime>,
) -> Option<SessionSummary> {
    match read_session_summary(path, modified_at) {
        Ok(summary) => summary,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                ?error,
                "skipping an unreadable saved session"
            );
            None
        }
    }
}

fn read_session_summary(
    path: &Path,
    modified_at: Option<SystemTime>,
) -> Result<Option<SessionSummary>> {
    let mut options = OpenOptions::new();
    options.read(true);
    // O_NONBLOCK prevents a raced FIFO replacement from hanging discovery; it has no effect on
    // ordinary local files. O_NOFOLLOW keeps a raced symlink replacement out of the state reader.
    crate::private_fs::configure_private_file_nofollow(&mut options, true);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ELOOP) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect saved session {}", path.display()));
        }
    };
    if !file
        .metadata()
        .with_context(|| format!("failed to inspect saved session {}", path.display()))?
        .is_file()
    {
        return Ok(None);
    }
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
        if crate::context::is_contextual_user_text_with_kind(
            &text,
            item.user_message_kind.as_deref(),
        ) {
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
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    crate::private_fs::configure_private_file_nofollow(&mut options, true);
    let mut file = options
        .open(&path)
        .with_context(|| format!("failed to open installation ID {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect installation ID {}", path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!(
            "installation ID {} is not a regular file",
            path.display()
        ));
    }
    crate::private_fs::protect_file(&file)
        .with_context(|| format!("failed to protect installation ID {}", path.display()))?;
    // Serialize both first creation and repair so concurrent startups converge on one identity.
    File::lock(&file)
        .with_context(|| format!("failed to lock installation ID {}", path.display()))?;

    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_INSTALLATION_ID_BYTES.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() <= MAX_INSTALLATION_ID_BYTES
        && let Ok(value) = std::str::from_utf8(&bytes)
        && let Ok(id) = Uuid::parse_str(value.trim())
    {
        return Ok(id.to_string());
    }

    let value = Uuid::new_v4().to_string();
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(value.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    crate::private_fs::sync_directory(root)?;
    Ok(value)
}

fn prepare_private_directory(path: &Path) -> Result<()> {
    crate::private_fs::create_private_directory_all(path)
        .with_context(|| format!("failed to create state directory {}", path.display()))?;
    Ok(())
}

fn open_private_append(path: &Path, create_new: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).read(true).write(true);
    if create_new {
        options.create_new(true);
    }
    crate::private_fs::configure_private_file_nofollow(&mut options, false);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open session journal {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || crate::private_fs::is_link(&metadata) {
        return Err(anyhow!(
            "session journal {} is not a regular file",
            path.display()
        ));
    }
    crate::private_fs::protect_file(&file)?;
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

#[cfg(test)]
#[path = "rollout_tests.rs"]
mod tests;
