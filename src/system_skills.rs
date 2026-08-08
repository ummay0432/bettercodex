//! Materialization of bettercodex's embedded system skills.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::fs::OpenOptions;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

const SKILLS_DIRECTORY: &str = "skills";
const SYSTEM_DIRECTORY: &str = ".system";
const MARKER_FILE_NAME: &str = ".bettercodex-system-skills.marker";
const LOCK_FILE_NAME: &str = ".bettercodex-system-skills.lock";
const STAGING_DIRECTORY_NAME: &str = ".bettercodex-system-skills.stage";
const BACKUP_DIRECTORY_NAME: &str = ".bettercodex-system-skills.backup";
const FINGERPRINT_SALT: &str = "v1";

struct EmbeddedFile {
    relative_path: &'static str,
    contents: &'static [u8],
}

const EMBEDDED_FILES: &[EmbeddedFile] = &[
    EmbeddedFile {
        relative_path: "loop/SKILL.md",
        contents: include_bytes!("../bundled-skills/loop/SKILL.md"),
    },
    EmbeddedFile {
        relative_path: "loop/agents/openai.yaml",
        contents: include_bytes!("../bundled-skills/loop/agents/openai.yaml"),
    },
    EmbeddedFile {
        relative_path: "loop/references/evals-manifest.md",
        contents: include_bytes!("../docs/evals/MANIFEST.md"),
    },
    EmbeddedFile {
        relative_path: "manifest/SKILL.md",
        contents: include_bytes!("../bundled-skills/manifest/SKILL.md"),
    },
    EmbeddedFile {
        relative_path: "manifest/agents/openai.yaml",
        contents: include_bytes!("../bundled-skills/manifest/agents/openai.yaml"),
    },
    EmbeddedFile {
        relative_path: "manifest/references/exemplar-shopify-graphql-manifest.md",
        contents: include_bytes!(
            "../bundled-skills/manifest/references/exemplar-shopify-graphql-manifest.md"
        ),
    },
    EmbeddedFile {
        relative_path: "openai-docs/LICENSE.txt",
        contents: include_bytes!("../bundled-skills/openai-docs/LICENSE.txt"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/SKILL.md",
        contents: include_bytes!("../bundled-skills/openai-docs/SKILL.md"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/agents/openai.yaml",
        contents: include_bytes!("../bundled-skills/openai-docs/agents/openai.yaml"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/assets/openai-small.svg",
        contents: include_bytes!("../bundled-skills/openai-docs/assets/openai-small.svg"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/assets/openai.png",
        contents: include_bytes!("../bundled-skills/openai-docs/assets/openai.png"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/references/codex-self-knowledge.md",
        contents: include_bytes!(
            "../bundled-skills/openai-docs/references/codex-self-knowledge.md"
        ),
    },
    EmbeddedFile {
        relative_path: "openai-docs/references/latest-model.md",
        contents: include_bytes!("../bundled-skills/openai-docs/references/latest-model.md"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/references/mcp-diagnostics.md",
        contents: include_bytes!("../bundled-skills/openai-docs/references/mcp-diagnostics.md"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/references/model-migration.md",
        contents: include_bytes!("../bundled-skills/openai-docs/references/model-migration.md"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/references/model-selection.md",
        contents: include_bytes!("../bundled-skills/openai-docs/references/model-selection.md"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/references/official-docs.md",
        contents: include_bytes!("../bundled-skills/openai-docs/references/official-docs.md"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/references/prompting-guide.md",
        contents: include_bytes!("../bundled-skills/openai-docs/references/prompting-guide.md"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/references/upgrade-guide.md",
        contents: include_bytes!("../bundled-skills/openai-docs/references/upgrade-guide.md"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/references/upgrading-to-gpt-5p6-sol.md",
        contents: include_bytes!(
            "../bundled-skills/openai-docs/references/upgrading-to-gpt-5p6-sol.md"
        ),
    },
    EmbeddedFile {
        relative_path: "openai-docs/scripts/fetch-codex-manual.mjs",
        contents: include_bytes!("../bundled-skills/openai-docs/scripts/fetch-codex-manual.mjs"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/scripts/resolve-latest-model-info",
        contents: include_bytes!("../bundled-skills/openai-docs/scripts/resolve-latest-model-info"),
    },
    EmbeddedFile {
        relative_path: "openai-docs/scripts/resolve-latest-model-info.cjs",
        contents: include_bytes!(
            "../bundled-skills/openai-docs/scripts/resolve-latest-model-info.cjs"
        ),
    },
    EmbeddedFile {
        relative_path: "papercut/SKILL.md",
        contents: include_bytes!("../bundled-skills/papercut/SKILL.md"),
    },
    EmbeddedFile {
        relative_path: "papercut/agents/openai.yaml",
        contents: include_bytes!("../bundled-skills/papercut/agents/openai.yaml"),
    },
    EmbeddedFile {
        relative_path: "review/SKILL.md",
        contents: include_bytes!("../bundled-skills/review/SKILL.md"),
    },
    EmbeddedFile {
        relative_path: "review/agents/openai.yaml",
        contents: include_bytes!("../bundled-skills/review/agents/openai.yaml"),
    },
];

pub(crate) fn root(home: &Path) -> PathBuf {
    home.join(SKILLS_DIRECTORY).join(SYSTEM_DIRECTORY)
}

/// Installs embedded system skills under the bettercodex home.
///
/// This follows Codex's on-disk progressive-disclosure design: the catalogue can
/// advertise a real `SKILL.md` path while the full body stays out of model context
/// until the model or operator selects it.
pub(crate) fn install(home: &Path) -> Result<PathBuf> {
    let skills_root = home.join(SKILLS_DIRECTORY);
    create_private_directory(&skills_root).with_context(|| {
        format!(
            "could not create the system skills parent {}",
            skills_root.display()
        )
    })?;

    let lock_path = skills_root.join(LOCK_FILE_NAME);
    let mut lock_options = OpenOptions::new();
    lock_options
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    let lock = lock_options.open(&lock_path).with_context(|| {
        format!(
            "could not open the system skills lock {}",
            lock_path.display()
        )
    })?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("could not lock system skills at {}", skills_root.display()));
    }

    let destination = root(home);
    let staging = skills_root.join(STAGING_DIRECTORY_NAME);
    let backup = skills_root.join(BACKUP_DIRECTORY_NAME);
    recover_interrupted_install(&skills_root, &destination, &staging, &backup)?;

    let expected_fingerprint = embedded_fingerprint();
    let destination_is_directory = match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => {
            return Err(anyhow!(
                "system skills path {} exists but is not a regular directory",
                destination.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "could not inspect the system skills path {}",
                    destination.display()
                )
            });
        }
    };
    if destination_is_directory && installation_matches(&destination, &expected_fingerprint)? {
        return Ok(destination);
    }

    let install_result = (|| -> Result<()> {
        remove_work_path(&staging)?;
        create_private_directory(&staging)?;
        for embedded in EMBEDDED_FILES {
            let path = staging.join(embedded.relative_path);
            let parent = path.parent().ok_or_else(|| {
                anyhow!(
                    "embedded system skill path has no parent: {}",
                    path.display()
                )
            })?;
            create_private_directory(parent)?;
            write_private_file(&path, embedded.contents)?;
        }
        let marker = staging.join(MARKER_FILE_NAME);
        write_private_file(&marker, format!("{expected_fingerprint}\n").as_bytes())?;
        File::open(&staging)?.sync_all()?;

        if destination_is_directory {
            std::fs::rename(&destination, &backup).with_context(|| {
                format!(
                    "could not stage the previous system skills directory {}",
                    destination.display()
                )
            })?;
        }
        if let Err(error) = std::fs::rename(&staging, &destination) {
            if destination_is_directory {
                let _ = std::fs::rename(&backup, &destination);
            }
            return Err(error).with_context(|| {
                format!(
                    "could not activate system skills at {}",
                    destination.display()
                )
            });
        }
        File::open(&skills_root)?.sync_all()?;
        remove_work_path(&backup)?;
        File::open(&skills_root)?.sync_all()?;
        Ok(())
    })();
    if install_result.is_err() {
        let _ = remove_work_path(&staging);
    }
    install_result.with_context(|| {
        format!(
            "could not materialize embedded system skills at {}",
            destination.display()
        )
    })?;

    Ok(destination)
}

fn recover_interrupted_install(
    skills_root: &Path,
    destination: &Path,
    staging: &Path,
    backup: &Path,
) -> Result<()> {
    let destination_metadata = std::fs::symlink_metadata(destination);
    match destination_metadata {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            remove_work_path(backup)?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(backup) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    std::fs::rename(backup, destination).with_context(|| {
                        format!(
                            "could not restore interrupted system skills install at {}",
                            destination.display()
                        )
                    })?;
                    File::open(skills_root)?.sync_all()?;
                }
                Ok(_) => {
                    return Err(anyhow!(
                        "system skills backup path {} is not a regular directory",
                        backup.display()
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "could not inspect system skills backup {}",
                            backup.display()
                        )
                    });
                }
            }
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "could not inspect the system skills path {}",
                    destination.display()
                )
            });
        }
    }
    remove_work_path(staging)
}

fn installation_matches(destination: &Path, expected_fingerprint: &str) -> Result<bool> {
    let marker = destination.join(MARKER_FILE_NAME);
    if !regular_file_matches(&marker, format!("{expected_fingerprint}\n").as_bytes())? {
        return Ok(false);
    }
    for embedded in EMBEDDED_FILES {
        if !regular_file_matches(&destination.join(embedded.relative_path), embedded.contents)? {
            return Ok(false);
        }
    }

    let mut expected_files = BTreeSet::from([PathBuf::from(MARKER_FILE_NAME)]);
    let mut expected_directories = BTreeSet::from([PathBuf::new()]);
    for embedded in EMBEDDED_FILES {
        let path = PathBuf::from(embedded.relative_path);
        expected_files.insert(path.clone());
        let mut parent = path.parent();
        while let Some(directory) = parent {
            expected_directories.insert(directory.to_path_buf());
            parent = directory.parent();
        }
    }
    exact_tree_matches(
        destination,
        Path::new(""),
        &expected_files,
        &expected_directories,
    )
}

fn regular_file_matches(path: &Path, expected: &[u8]) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("could not inspect {}", path.display()));
        }
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Ok(false);
    }
    std::fs::read(path)
        .map(|contents| contents == expected)
        .with_context(|| format!("could not read {}", path.display()))
}

fn exact_tree_matches(
    root: &Path,
    relative: &Path,
    expected_files: &BTreeSet<PathBuf>,
    expected_directories: &BTreeSet<PathBuf>,
) -> Result<bool> {
    let directory = root.join(relative);
    let metadata = std::fs::symlink_metadata(&directory)
        .with_context(|| format!("could not inspect {}", directory.display()))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Ok(false);
    }
    for entry in std::fs::read_dir(&directory)
        .with_context(|| format!("could not read {}", directory.display()))?
    {
        let entry = entry.with_context(|| format!("could not read {}", directory.display()))?;
        let child_relative = relative.join(entry.file_name());
        let child_metadata = std::fs::symlink_metadata(entry.path())
            .with_context(|| format!("could not inspect {}", entry.path().display()))?;
        if child_metadata.is_dir() && !child_metadata.file_type().is_symlink() {
            if !expected_directories.contains(&child_relative)
                || !exact_tree_matches(root, &child_relative, expected_files, expected_directories)?
            {
                return Ok(false);
            }
        } else if child_metadata.is_file() && !child_metadata.file_type().is_symlink() {
            if !expected_files.contains(&child_relative) {
                return Ok(false);
            }
        } else {
            return Ok(false);
        }
    }
    Ok(true)
}

fn remove_work_path(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path)
                .with_context(|| format!("could not remove {}", path.display()))
        }
        Ok(_) => std::fs::remove_file(path)
            .with_context(|| format!("could not remove {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not inspect {}", path.display())),
    }
}

fn embedded_fingerprint() -> String {
    let mut hasher = DefaultHasher::new();
    FINGERPRINT_SALT.hash(&mut hasher);
    for embedded in EMBEDDED_FILES {
        embedded.relative_path.hash(&mut hasher);
        embedded.contents.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .with_context(|| format!("could not create directory {}", path.display()))
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true).mode(0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("could not write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("could not sync {}", path.display()))
}

#[cfg(test)]
#[path = "system_skills_tests.rs"]
mod tests;
