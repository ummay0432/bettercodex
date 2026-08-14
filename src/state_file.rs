//! Bounded, locked, atomic JSON persistence for bettercodex-owned state.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StateChange {
    Unchanged,
    Changed,
}

impl StateChange {
    pub(crate) const fn from_changed(changed: bool) -> Self {
        if changed {
            Self::Changed
        } else {
            Self::Unchanged
        }
    }
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path, max_bytes: usize) -> Result<Option<T>> {
    let mut options = OpenOptions::new();
    options.read(true);
    // O_NONBLOCK prevents a substituted FIFO from hanging startup on Unix;
    // no-follow handling keeps reads on the bettercodex-owned state file.
    crate::private_fs::configure_private_file_nofollow(&mut options, true);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect state file {}", path.display()))?;
    if !metadata.is_file() || crate::private_fs::is_link(&metadata) {
        return Err(anyhow!(
            "state file {} is not a regular file",
            path.display()
        ));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(anyhow!(
            "{} exceeds the {max_bytes}-byte limit",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() > max_bytes {
        return Err(anyhow!(
            "{} exceeds the {max_bytes}-byte limit",
            path.display()
        ));
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid JSON in {}", path.display()))
        .map(Some)
}

pub(crate) fn update_json<T: Serialize>(
    path: &Path,
    max_bytes: usize,
    load: impl FnOnce(&Path) -> Result<T>,
    update: impl FnOnce(&mut T) -> Result<StateChange>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("state path has no parent: {}", path.display()))?;
    crate::private_fs::create_private_directory_all(parent)
        .with_context(|| format!("failed to create bettercodex home {}", parent.display()))?;

    let lock_path = companion_path(path, ".lock")?;
    let mut lock_options = OpenOptions::new();
    lock_options.create(true).read(true).write(true);
    crate::private_fs::configure_private_file_nofollow(&mut lock_options, false);
    let lock = lock_options
        .open(&lock_path)
        .with_context(|| format!("failed to open state lock {}", lock_path.display()))?;
    let lock_metadata = lock
        .metadata()
        .with_context(|| format!("failed to inspect state lock {}", lock_path.display()))?;
    if !lock_metadata.is_file() || crate::private_fs::is_link(&lock_metadata) {
        return Err(anyhow!(
            "state lock {} is not a regular file",
            lock_path.display()
        ));
    }
    File::lock(&lock).with_context(|| format!("failed to lock {}", path.display()))?;

    let mut document = load(path)?;
    if update(&mut document)? == StateChange::Unchanged {
        return Ok(());
    }
    write_json(path, &document, max_bytes)
}

fn write_json<T: Serialize>(path: &Path, document: &T, max_bytes: usize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(document)?;
    bytes.push(b'\n');
    if bytes.len() > max_bytes {
        return Err(anyhow!(
            "updated {} would exceed the {max_bytes}-byte limit",
            path.display()
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("state path has no parent: {}", path.display()))?;
    let temporary = temporary_path(path)?;
    let write_result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        crate::private_fs::configure_private_file(&mut options);
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to open temporary state {}", temporary.display()))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        crate::private_fs::replace_file(&temporary, path).with_context(|| {
            format!(
                "failed to replace state file {} with {}",
                path.display(),
                temporary.display()
            )
        })?;
        crate::private_fs::sync_directory(parent)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

fn companion_path(path: &Path, suffix: &str) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("state path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("state path has no file name: {}", path.display()))?;
    let mut companion = OsString::from(".");
    companion.push(file_name);
    companion.push(suffix);
    Ok(parent.join(companion))
}

fn temporary_path(path: &Path) -> Result<PathBuf> {
    companion_path(
        path,
        &format!(".tmp-{}-{}", std::process::id(), uuid::Uuid::new_v4()),
    )
}

#[cfg(test)]
#[path = "state_file_tests.rs"]
mod tests;
