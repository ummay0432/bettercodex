//! Bounded, path-keyed operator overrides for discovered skills.

use crate::skills::SkillUpdate;
use crate::state_file;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

pub(crate) const FILE_NAME: &str = "skills.json";
const VERSION: u32 = 1;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    version: u32,
    #[serde(default)]
    pub(crate) skills: BTreeMap<PathBuf, SkillSettings>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: VERSION,
            skills: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SkillSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) allow_implicit_invocation: Option<bool>,
}

pub(crate) fn save(path: &Path, skill_path: &Path, update: SkillUpdate) -> Result<()> {
    let skill_path = skill_path
        .canonicalize()
        .with_context(|| format!("failed to resolve skill {}", skill_path.display()))?;
    state_file::update_json(path, MAX_BYTES, read, |document| {
        if !document.skills.contains_key(&skill_path) && document.skills.len() >= MAX_ENTRIES {
            return Err(anyhow!(
                "skill settings {} reached its {MAX_ENTRIES}-entry limit",
                path.display()
            ));
        }
        let settings = document.skills.entry(skill_path).or_default();
        match update {
            SkillUpdate::Enabled(enabled) => settings.enabled = Some(enabled),
            SkillUpdate::AllowImplicitInvocation(allow) => {
                settings.allow_implicit_invocation = Some(allow);
            }
        }
        Ok(())
    })
}

pub(crate) fn read(path: &Path) -> Result<Document> {
    let document: Document = state_file::read_json(path, MAX_BYTES)?.unwrap_or_default();
    if document.version != VERSION {
        return Err(anyhow!(
            "unsupported version {}; expected {VERSION}",
            document.version
        ));
    }
    if document.skills.len() > MAX_ENTRIES {
        return Err(anyhow!("file exceeds the {MAX_ENTRIES}-entry limit"));
    }
    Ok(document)
}
