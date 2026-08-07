//! Persisted operator choices that affect the bettercodex process boundary.

use crate::paths;
use crate::state_file;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;

const FILE_NAME: &str = "settings.json";
const VERSION: u32 = 1;
const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TmuxMode {
    #[default]
    On,
    Off,
}

impl TmuxMode {
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::On => Self::Off,
            Self::Off => Self::On,
        }
    }

    pub(crate) const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    #[serde(default = "tmux_default")]
    tmux: bool,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: VERSION,
            tmux: tmux_default(),
        }
    }
}

pub(crate) fn load_tmux_mode() -> Result<TmuxMode> {
    let Some(home) = paths::bettercodex_home() else {
        return Ok(TmuxMode::default());
    };
    read(&home.join(FILE_NAME))
}

pub(crate) fn save_tmux_mode(mode: TmuxMode) -> Result<()> {
    let home = paths::bettercodex_home().ok_or_else(|| {
        anyhow!("cannot save tmux setting because neither BCODEX_HOME nor HOME is set")
    })?;
    save(&home.join(FILE_NAME), mode)
}

fn read(path: &Path) -> Result<TmuxMode> {
    let document = read_document(path)?;
    Ok(if document.tmux {
        TmuxMode::On
    } else {
        TmuxMode::Off
    })
}

fn save(path: &Path, mode: TmuxMode) -> Result<()> {
    state_file::update_json(path, MAX_BYTES, read_document, |document| {
        document.tmux = mode.is_on();
        Ok(())
    })
}

fn read_document(path: &Path) -> Result<Document> {
    let document: Document = state_file::read_json(path, MAX_BYTES)?.unwrap_or_default();
    if document.version != VERSION {
        return Err(anyhow!(
            "unsupported settings version {}; expected {VERSION}",
            document.version
        ));
    }
    Ok(document)
}

const fn tmux_default() -> bool {
    true
}

#[cfg(test)]
#[path = "operator_settings_tests.rs"]
mod tests;
