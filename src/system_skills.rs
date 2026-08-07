//! Materialization of bettercodex's embedded system skills.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::fs::OpenOptions;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;

const SKILLS_DIRECTORY: &str = "skills";
const SYSTEM_DIRECTORY: &str = ".system";
const MARKER_FILE_NAME: &str = ".bettercodex-system-skills.marker";
const LOCK_FILE_NAME: &str = ".bettercodex-system-skills.lock";
const FINGERPRINT_SALT: &str = "v1";

struct EmbeddedFile {
    relative_path: &'static str,
    contents: &'static [u8],
}

const EMBEDDED_FILES: &[EmbeddedFile] = &[
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
        relative_path: "papercut/SKILL.md",
        contents: include_bytes!("../bundled-skills/papercut/SKILL.md"),
    },
    EmbeddedFile {
        relative_path: "papercut/agents/openai.yaml",
        contents: include_bytes!("../bundled-skills/papercut/agents/openai.yaml"),
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
    let expected_fingerprint = embedded_fingerprint();
    let marker = destination.join(MARKER_FILE_NAME);
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
    if destination_is_directory
        && std::fs::read_to_string(&marker)
            .is_ok_and(|contents| contents.trim() == expected_fingerprint)
    {
        return Ok(destination);
    }

    if destination_is_directory {
        std::fs::remove_dir_all(&destination).with_context(|| {
            format!(
                "could not replace the system skills directory {}",
                destination.display()
            )
        })?;
    }

    let install_result = (|| -> Result<()> {
        create_private_directory(&destination)?;
        for embedded in EMBEDDED_FILES {
            let path = destination.join(embedded.relative_path);
            let parent = path.parent().ok_or_else(|| {
                anyhow!(
                    "embedded system skill path has no parent: {}",
                    path.display()
                )
            })?;
            create_private_directory(parent)?;
            write_private_file(&path, embedded.contents)?;
        }
        write_private_file(&marker, format!("{expected_fingerprint}\n").as_bytes())?;
        File::open(&destination)?.sync_all()?;
        File::open(&skills_root)?.sync_all()?;
        Ok(())
    })();
    if install_result.is_err() {
        let _ = std::fs::remove_dir_all(&destination);
    }
    install_result.with_context(|| {
        format!(
            "could not materialize embedded system skills at {}",
            destination.display()
        )
    })?;

    Ok(destination)
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
