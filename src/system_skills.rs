//! Materialization of bettercodex's embedded system skills.

use anyhow::Context;
use anyhow::Result;
use include_dir::Dir;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;

const SYSTEM_SKILLS_DIR: Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/bundled-skills");
const SKILLS_DIRECTORY: &str = "skills";
const SYSTEM_DIRECTORY: &str = ".system";
const MARKER_FILE_NAME: &str = ".bettercodex-system-skills.marker";
const FINGERPRINT_SALT: &str = "v1";

pub(crate) fn root(home: &Path) -> PathBuf {
    home.join(SKILLS_DIRECTORY).join(SYSTEM_DIRECTORY)
}

/// Installs embedded system skills under the bettercodex home.
///
/// The fingerprint marker avoids rewriting an unchanged cache. A missing or
/// stale marker causes the disposable cache to be rematerialized from the
/// embedded source, matching current upstream Codex behavior.
pub(crate) fn install(home: &Path) -> Result<PathBuf> {
    let skills_root = home.join(SKILLS_DIRECTORY);
    std::fs::create_dir_all(&skills_root).with_context(|| {
        format!(
            "could not create the system skills parent {}",
            skills_root.display()
        )
    })?;

    let destination = root(home);
    let marker = destination.join(MARKER_FILE_NAME);
    let expected_fingerprint = embedded_fingerprint();
    if destination.is_dir()
        && read_marker(&marker).is_ok_and(|fingerprint| fingerprint == expected_fingerprint)
    {
        return Ok(destination);
    }

    if destination.exists() {
        std::fs::remove_dir_all(&destination).with_context(|| {
            format!(
                "could not remove the stale system skills cache {}",
                destination.display()
            )
        })?;
    }

    write_embedded_dir(&SYSTEM_SKILLS_DIR, &destination)?;
    std::fs::write(&marker, format!("{expected_fingerprint}\n"))
        .with_context(|| format!("could not write system skills marker {}", marker.display()))?;
    Ok(destination)
}

fn read_marker(path: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(path)
        .with_context(|| format!("could not read system skills marker {}", path.display()))?
        .trim()
        .to_string())
}

fn embedded_fingerprint() -> String {
    let mut items = Vec::new();
    collect_fingerprint_items(&SYSTEM_SKILLS_DIR, &mut items);
    items.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));

    let mut hasher = DefaultHasher::new();
    FINGERPRINT_SALT.hash(&mut hasher);
    for (path, contents_hash) in items {
        path.hash(&mut hasher);
        contents_hash.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

fn collect_fingerprint_items(directory: &Dir<'_>, items: &mut Vec<(String, Option<u64>)>) {
    for entry in directory.entries() {
        match entry {
            include_dir::DirEntry::Dir(child) => {
                items.push((child.path().to_string_lossy().to_string(), None));
                collect_fingerprint_items(child, items);
            }
            include_dir::DirEntry::File(file) => {
                let mut hasher = DefaultHasher::new();
                file.contents().hash(&mut hasher);
                items.push((
                    file.path().to_string_lossy().to_string(),
                    Some(hasher.finish()),
                ));
            }
        }
    }
}

fn write_embedded_dir(directory: &Dir<'_>, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination).with_context(|| {
        format!(
            "could not create system skills directory {}",
            destination.display()
        )
    })?;

    for entry in directory.entries() {
        match entry {
            include_dir::DirEntry::Dir(child) => {
                let child_destination = destination.join(child.path());
                std::fs::create_dir_all(&child_destination).with_context(|| {
                    format!(
                        "could not create system skills directory {}",
                        child_destination.display()
                    )
                })?;
                write_embedded_dir(child, destination)?;
            }
            include_dir::DirEntry::File(file) => {
                let path = destination.join(file.path());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!(
                            "could not create system skills directory {}",
                            parent.display()
                        )
                    })?;
                }
                std::fs::write(&path, file.contents()).with_context(|| {
                    format!("could not write embedded system skill {}", path.display())
                })?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SYSTEM_SKILLS_DIR;
    use super::collect_fingerprint_items;

    #[test]
    fn fingerprint_traverses_nested_entries() {
        let mut items = Vec::new();
        collect_fingerprint_items(&SYSTEM_SKILLS_DIR, &mut items);
        let mut paths = items.into_iter().map(|(path, _)| path).collect::<Vec<_>>();
        paths.sort_unstable();

        assert!(
            paths
                .binary_search(&"openai-docs/SKILL.md".to_string())
                .is_ok()
        );
        assert!(
            paths
                .binary_search(&"openai-docs/scripts/fetch-codex-manual.mjs".to_string())
                .is_ok()
        );
    }
}
