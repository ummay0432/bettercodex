//! Focused Codex wire and UI types used by bettercodex.
//!
//! Core types are ported from OpenAI Codex commit
//! `1669c2403f793d0230065397dfc25f52b844244e`,
//! `codex-rs/protocol/src/{models,parse_command}.rs`. File changes follow commit
//! `1c4f42863c1f84eb5175a1a0cfffe84641a63df3`, `codex-rs/tui/src/diff_model.rs`.
//! Keeping these stable data contracts local avoids compiling the unrelated
//! app-server, configuration, proxy, policy, and TypeScript export graph.

use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;

/// A text-file change rendered with the same add/update/delete model as Codex.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum FileChange {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
    },
}

/// The single file changed by one direct `write` or `edit` call.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ToolFileChange {
    pub(crate) path: PathBuf,
    pub(crate) change: FileChange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ImageDetail {
    Auto,
    Low,
    High,
    Original,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum FunctionCallOutputContentItem {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    EncryptedContent {
        encrypted_content: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ParsedCommand {
    Read {
        cmd: String,
        name: String,
        path: PathBuf,
    },
    ListFiles {
        cmd: String,
        path: Option<String>,
    },
    Search {
        cmd: String,
        query: Option<String>,
        path: Option<String>,
    },
    Unknown {
        cmd: String,
    },
}
