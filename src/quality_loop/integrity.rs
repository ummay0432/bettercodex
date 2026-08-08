use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const MAX_PACKAGE_FILES: usize = 10_000;
const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PACKAGE_PATH_BYTES: usize = 1_024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct PackageManifest {
    pub(crate) digest: String,
    entries: BTreeMap<String, ManifestEntry>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct ManifestEntry {
    kind: ManifestKind,
    mode: u32,
    digest_or_target: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ManifestKind {
    File,
    Directory,
    Symlink,
}

impl PackageManifest {
    pub(crate) fn make_private(root: &Path) -> Result<()> {
        let metadata = std::fs::symlink_metadata(root)
            .with_context(|| format!("failed to inspect evaluator workspace {}", root.display()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(anyhow!("evaluator workspace is not a safe directory"));
        }
        let mut count = 0;
        make_tree_private(root, root, &mut count)
    }

    pub(crate) fn capture(root: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(root)
            .with_context(|| format!("failed to inspect evaluator workspace {}", root.display()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(anyhow!("evaluator workspace is not a safe directory"));
        }
        let mut entries = BTreeMap::new();
        entries.insert(
            ".".to_string(),
            ManifestEntry {
                kind: ManifestKind::Directory,
                mode: metadata.permissions().mode() & 0o7777,
                digest_or_target: None,
            },
        );
        let mut total_bytes = 0_u64;
        collect(root, root, &mut entries, &mut total_bytes)?;
        if entries.len() == 1 {
            return Err(anyhow!("evaluator workspace is empty"));
        }
        let digest = hash(&serde_json::to_vec(&entries)?);
        Ok(Self { digest, entries })
    }

    pub(crate) fn verify(&self, root: &Path) -> Result<()> {
        let current = Self::capture(root)?;
        if current != *self {
            let changed = differing_paths(&self.entries, &current.entries);
            return Err(anyhow!(
                "frozen evaluator integrity failed{}",
                if changed.is_empty() {
                    String::new()
                } else {
                    format!(": {}", changed.join(", "))
                }
            ));
        }
        Ok(())
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        super::state::atomic_private_write(path, &bytes)
    }
}

fn make_tree_private(root: &Path, path: &Path, count: &mut usize) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    *count = count.saturating_add(1);
    if *count > MAX_PACKAGE_FILES {
        return Err(anyhow!(
            "evaluator package exceeds the {MAX_PACKAGE_FILES}-entry limit"
        ));
    }
    if metadata.file_type().is_symlink() {
        validate_symlink_target(root, path)?;
    } else if metadata.is_dir() {
        let mut entries = std::fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            make_tree_private(root, &entry.path(), count)?;
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    } else if metadata.is_file() {
        if metadata.nlink() != 1 {
            return Err(anyhow!(
                "hard-linked files are not permitted in the evaluator package: {}",
                path.display()
            ));
        }
        let mode = metadata.permissions().mode();
        let private_mode = 0o600 | (mode & 0o100);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(private_mode))?;
    } else {
        return Err(anyhow!(
            "unsupported object in evaluator package: {}",
            path.display()
        ));
    }
    Ok(())
}

fn collect(
    root: &Path,
    directory: &Path,
    output: &mut BTreeMap<String, ManifestEntry>,
    total_bytes: &mut u64,
) -> Result<()> {
    let mut entries = std::fs::read_dir(directory)
        .with_context(|| format!("failed to read evaluator package {}", directory.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if output.len() >= MAX_PACKAGE_FILES {
            return Err(anyhow!(
                "evaluator package exceeds the {MAX_PACKAGE_FILES}-entry limit"
            ));
        }
        let path = entry.path();
        let relative = normalized_relative(root, &path)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        let mode = metadata.permissions().mode() & 0o7777;
        let manifest_entry = if metadata.file_type().is_symlink() {
            validate_symlink_target(root, &path)?;
            let target = std::fs::read_link(&path)?.into_os_string().into_vec();
            *total_bytes = total_bytes.saturating_add(target.len() as u64);
            ManifestEntry {
                kind: ManifestKind::Symlink,
                mode,
                digest_or_target: Some(STANDARD.encode(target)),
            }
        } else if metadata.is_dir() {
            ManifestEntry {
                kind: ManifestKind::Directory,
                mode,
                digest_or_target: None,
            }
        } else if metadata.is_file() {
            if metadata.nlink() != 1 {
                return Err(anyhow!(
                    "hard-linked files are not permitted in the evaluator package: {}",
                    path.display()
                ));
            }
            if metadata.len() > MAX_PACKAGE_BYTES.saturating_sub(*total_bytes) {
                return Err(anyhow!(
                    "evaluator package exceeds the {MAX_PACKAGE_BYTES}-byte limit"
                ));
            }
            let bytes = std::fs::read(&path)?;
            *total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            ManifestEntry {
                kind: ManifestKind::File,
                mode,
                digest_or_target: Some(hash(&bytes)),
            }
        } else {
            return Err(anyhow!(
                "unsupported object in evaluator package: {}",
                path.display()
            ));
        };
        if *total_bytes > MAX_PACKAGE_BYTES {
            return Err(anyhow!(
                "evaluator package exceeds the {MAX_PACKAGE_BYTES}-byte limit"
            ));
        }
        let descend = manifest_entry.kind == ManifestKind::Directory;
        output.insert(relative, manifest_entry);
        if descend {
            collect(root, &path, output, total_bytes)?;
        }
    }
    Ok(())
}

fn validate_symlink_target(root: &Path, path: &Path) -> Result<()> {
    let target = std::fs::read_link(path)?;
    if target.is_absolute() {
        return Err(anyhow!(
            "evaluator package symlink escapes its workspace: {}",
            path.display()
        ));
    }
    let canonical_root = root.canonicalize()?;
    let canonical_target = path.canonicalize().with_context(|| {
        format!(
            "evaluator package symlink is unresolved: {}",
            path.display()
        )
    })?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(anyhow!(
            "evaluator package symlink escapes its workspace: {}",
            path.display()
        ));
    }
    Ok(())
}

fn normalized_relative(root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(root)?;
    let value = relative
        .to_str()
        .ok_or_else(|| anyhow!("evaluator package path is not UTF-8"))?
        .replace('\\', "/");
    if value.is_empty() || value.len() > MAX_PACKAGE_PATH_BYTES {
        return Err(anyhow!("evaluator package path exceeds the path limit"));
    }
    Ok(value)
}

fn differing_paths(
    expected: &BTreeMap<String, ManifestEntry>,
    actual: &BTreeMap<String, ManifestEntry>,
) -> Vec<String> {
    expected
        .keys()
        .chain(actual.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|path| expected.get(*path) != actual.get(*path))
        .take(12)
        .cloned()
        .collect()
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn package_manifest_bounds_files_detects_modes_and_rejects_links() {
        let root =
            std::env::temp_dir().join(format!("bettercodex-loop-package-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("check.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(root.join("check.sh"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(root.join("check.sh"), permissions).unwrap();
        let manifest = PackageManifest::capture(&root).unwrap();

        std::fs::set_permissions(
            root.join("check.sh"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(manifest.verify(&root).is_err());
        std::fs::set_permissions(
            root.join("check.sh"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let root_mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o7777;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(manifest.verify(&root).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(root_mode)).unwrap();

        std::fs::hard_link(root.join("check.sh"), root.join("hard-link")).unwrap();
        assert!(PackageManifest::capture(&root).is_err());
        assert!(PackageManifest::make_private(&root).is_err());
        std::fs::remove_file(root.join("hard-link")).unwrap();

        std::os::unix::fs::symlink("check.sh", root.join("symlink")).unwrap();
        PackageManifest::make_private(&root).unwrap();
        let linked_manifest = PackageManifest::capture(&root).unwrap();
        std::fs::remove_file(root.join("symlink")).unwrap();
        std::os::unix::fs::symlink("missing", root.join("symlink")).unwrap();
        assert!(linked_manifest.verify(&root).is_err());
        assert!(PackageManifest::capture(&root).is_err());
        std::fs::remove_file(root.join("symlink")).unwrap();

        let outside = root.with_extension("outside");
        std::fs::write(&outside, "outside\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("symlink")).unwrap();
        assert!(PackageManifest::capture(&root).is_err());
        assert!(PackageManifest::make_private(&root).is_err());
        std::fs::remove_file(root.join("symlink")).unwrap();
        std::fs::remove_file(outside).unwrap();

        let sparse = std::fs::File::create(root.join("oversized")).unwrap();
        sparse.set_len(MAX_PACKAGE_BYTES + 1).unwrap();
        assert!(PackageManifest::capture(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_privacy_rejects_a_symlinked_workspace_root() {
        let parent = std::env::temp_dir().join(format!(
            "bettercodex-loop-package-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        let workspace = parent.join("workspace");
        let target = parent.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, &workspace).unwrap();

        assert!(PackageManifest::make_private(&workspace).is_err());

        std::fs::remove_dir_all(parent).unwrap();
    }
}
