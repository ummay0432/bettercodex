//! Bounded, path-keyed operator overrides for discovered skills.

use crate::skills::SkillUpdate;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

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
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("skill settings path has no parent: {}", path.display()))?;
    let mut directory = std::fs::DirBuilder::new();
    directory.recursive(true).mode(0o700);
    directory.create(parent).with_context(|| {
        format!(
            "failed to create bettercodex home directory {}",
            parent.display()
        )
    })?;

    let lock_path = parent.join(format!(".{FILE_NAME}.lock"));
    let mut lock_options = OpenOptions::new();
    lock_options.create(true).read(true).write(true).mode(0o600);
    let lock = lock_options
        .open(&lock_path)
        .with_context(|| format!("failed to open skill settings lock {}", lock_path.display()))?;
    let lock_result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    if lock_result != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to lock skill settings {}", path.display()));
    }

    let mut document = read(path)?;
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
    write(path, &document)
}

pub(crate) fn read(path: &Path) -> Result<Document> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Document::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read skill settings {}", path.display()));
        }
    };
    if file.metadata()?.len() > MAX_BYTES as u64 {
        return Err(anyhow!("file exceeds the {MAX_BYTES}-byte limit"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BYTES.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(anyhow!("file exceeds the {MAX_BYTES}-byte limit"));
    }
    let document: Document = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
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

fn write(path: &Path, document: &Document) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(document)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_BYTES {
        return Err(anyhow!(
            "updated skill settings would exceed the {MAX_BYTES}-byte limit"
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("skill settings path has no parent: {}", path.display()))?;
    let temporary = parent.join(format!(
        ".{FILE_NAME}.tmp-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let write_result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true).mode(0o600);
        let mut file = options.open(&temporary).with_context(|| {
            format!(
                "failed to open temporary skill settings {}",
                temporary.display()
            )
        })?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path).with_context(|| {
            format!(
                "failed to replace skill settings {} with {}",
                path.display(),
                temporary.display()
            )
        })?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}
