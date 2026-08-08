//! Source-revision checks and the self-cleaning update command.
//!
//! Installed builds compare their embedded source revision with private `main`
//! after the TUI renders. The explicit command fetches the current installer,
//! mirroring upstream Codex's standalone update path, and that installer builds
//! one immutable source commit with disposable build output and a reusable
//! dependency download cache.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command as AsyncProcessCommand;

const DEFAULT_REPOSITORY: &str = "ummay0432/bettercodex";
const GITHUB_HOST: &str = "github.com";
const INSTALLER_PATH: &str = "scripts/install.sh";
const MAX_INSTALLER_BYTES: usize = 1024 * 1024;
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const INSTALL_DIR_ENV: &str = "BCODEX_INSTALL_DIR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AvailableUpdate {
    current_revision: String,
    latest_revision: String,
}

impl AvailableUpdate {
    fn new(current_revision: &str, latest_revision: &str) -> Self {
        Self {
            current_revision: current_revision.to_ascii_lowercase(),
            latest_revision: latest_revision.to_ascii_lowercase(),
        }
    }

    pub(crate) fn current_short_revision(&self) -> &str {
        short_revision(&self.current_revision)
    }

    pub(crate) fn latest_short_revision(&self) -> &str {
        short_revision(&self.latest_revision)
    }

    #[cfg(test)]
    pub(crate) fn test_fixture() -> Self {
        Self::new(
            "1111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222",
        )
    }
}

/// Checks once for a different private `main` revision after the TUI renders.
///
/// Development builds stay offline. Installed builds can opt out by setting
/// `BCODEX_SKIP_UPDATE_CHECK`. Failures stay silent and are retried on the next
/// launch so startup and interactive work never depend on GitHub availability.
pub(crate) async fn check_for_update() -> Option<AvailableUpdate> {
    if cfg!(debug_assertions) || std::env::var_os("BCODEX_SKIP_UPDATE_CHECK").is_some() {
        return None;
    }
    let current_revision = source_revision()?;
    let _ = tokio::task::spawn_blocking(cleanup_legacy_updater_cache).await;
    let repository = configured_repository().ok()?;
    check_for_source_update_with(
        OsStr::new("gh"),
        &repository,
        current_revision,
        UPDATE_CHECK_TIMEOUT,
    )
    .await
}

pub(crate) fn source_revision() -> Option<&'static str> {
    option_env!("BCODEX_SOURCE_REVISION").filter(|revision| is_source_revision(revision))
}

async fn check_for_source_update_with(
    gh_program: &OsStr,
    repository: &str,
    current_revision: &str,
    timeout: Duration,
) -> Option<AvailableUpdate> {
    if !is_source_revision(current_revision) || validate_repository(repository).is_err() {
        return None;
    }
    let endpoint = format!("repos/{repository}/commits/main");
    let mut command = AsyncProcessCommand::new(gh_program);
    command
        .args(["api", "--hostname", GITHUB_HOST, &endpoint, "--jq", ".sha"])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let latest_revision = std::str::from_utf8(&output.stdout).ok()?.trim();
    if !is_source_revision(latest_revision)
        || latest_revision.eq_ignore_ascii_case(current_revision)
    {
        return None;
    }
    Some(AvailableUpdate::new(current_revision, latest_revision))
}

fn is_source_revision(revision: &str) -> bool {
    revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn short_revision(revision: &str) -> &str {
    revision.get(..12).unwrap_or(revision)
}

fn configured_repository() -> Result<String> {
    let repository =
        std::env::var("BCODEX_REPOSITORY").unwrap_or_else(|_| DEFAULT_REPOSITORY.to_string());
    validate_repository(&repository)?;
    Ok(repository)
}

fn validate_repository(repository: &str) -> Result<()> {
    let mut components = repository.split('/');
    let owner = components.next().unwrap_or_default();
    let name = components.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || components.next().is_some()
        || !owner.bytes().all(is_repository_name_byte)
        || !name.bytes().all(is_repository_name_byte)
    {
        bail!("BCODEX_REPOSITORY must be an owner/repository name");
    }
    Ok(())
}

fn is_repository_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

pub(crate) fn run_update() -> Result<()> {
    let current_revision = source_revision().context(
        "this build has no embedded source revision; install BetterCodex with INSTALL_COMMAND.txt before using `bcodex update`",
    )?;
    let repository = configured_repository()?;
    cleanup_legacy_updater_cache()?;
    let latest_revision = resolve_source_revision(OsStr::new("gh"), &repository)?;
    if latest_revision.eq_ignore_ascii_case(current_revision) {
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "BetterCodex is already current at {}.",
            short_revision(current_revision)
        )?;
        return Ok(());
    }

    let executable =
        std::env::current_exe().context("could not locate the running BetterCodex binary")?;
    let configured_dir = std::env::var_os(INSTALL_DIR_ENV);
    let install_dir = update_install_dir(&executable, configured_dir.as_deref())?;
    let installer = fetch_installer(
        OsStr::new("gh"),
        &repository,
        &latest_revision,
        MAX_INSTALLER_BYTES,
    )?;
    run_installer_script(&installer, OsStr::new("/bin/sh"), &install_dir, &repository)
}

fn resolve_source_revision(gh_program: &OsStr, repository: &str) -> Result<String> {
    validate_repository(repository)?;
    let endpoint = format!("repos/{repository}/commits/main");
    let output = ProcessCommand::new(gh_program)
        .args(["api", "--hostname", GITHUB_HOST, &endpoint, "--jq", ".sha"])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .output()
        .context("could not query the current BetterCodex main commit")?;
    if !output.status.success() {
        bail!("GitHub could not resolve private BetterCodex main");
    }
    let revision = std::str::from_utf8(&output.stdout)
        .context("GitHub returned a non-UTF-8 BetterCodex revision")?
        .trim();
    if !is_source_revision(revision) {
        bail!("GitHub returned an invalid BetterCodex source revision");
    }
    Ok(revision.to_ascii_lowercase())
}

fn fetch_installer(
    gh_program: &OsStr,
    repository: &str,
    revision: &str,
    maximum_bytes: usize,
) -> Result<Vec<u8>> {
    validate_repository(repository)?;
    if !is_source_revision(revision) {
        bail!("cannot fetch the installer from an invalid source revision");
    }
    let endpoint = format!("repos/{repository}/contents/{INSTALLER_PATH}?ref={revision}");
    let output = ProcessCommand::new(gh_program)
        .args([
            "api",
            "--hostname",
            GITHUB_HOST,
            "-H",
            "Accept: application/vnd.github.raw+json",
            &endpoint,
        ])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .output()
        .context("could not fetch the current BetterCodex installer")?;
    if !output.status.success() {
        bail!("GitHub could not fetch the current BetterCodex installer");
    }
    if output.stdout.is_empty() || output.stdout.len() > maximum_bytes {
        bail!("GitHub returned an empty or oversized BetterCodex installer");
    }
    if !output.stdout.starts_with(b"#!/bin/sh\n") {
        bail!("GitHub returned an invalid BetterCodex installer");
    }
    Ok(output.stdout)
}

fn update_install_dir(executable: &Path, configured: Option<&OsStr>) -> Result<PathBuf> {
    if let Some(configured) = configured {
        let directory = PathBuf::from(configured);
        if !directory.is_absolute() {
            bail!("{INSTALL_DIR_ENV} must be an absolute path");
        }
        return Ok(directory);
    }
    executable
        .parent()
        .filter(|parent| parent.is_absolute())
        .map(Path::to_path_buf)
        .context("could not locate the running BetterCodex binary directory")
}

fn run_installer_script(
    script: &[u8],
    shell: &OsStr,
    install_dir: &Path,
    repository: &str,
) -> Result<()> {
    let mut child = ProcessCommand::new(shell)
        .arg("-s")
        .env(INSTALL_DIR_ENV, install_dir)
        .env("BCODEX_REPOSITORY", repository)
        .stdin(Stdio::piped())
        .spawn()
        .context("could not start the BetterCodex installer")?;
    let write_result = child
        .stdin
        .take()
        .context("could not open the BetterCodex installer input")?
        .write_all(script);
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error).context("could not send the BetterCodex installer to the shell");
    }
    let status = child
        .wait()
        .context("could not wait for the BetterCodex installer")?;
    if !status.success() {
        bail!("BetterCodex installer exited with {status}");
    }
    Ok(())
}

fn cleanup_legacy_updater_cache() -> Result<()> {
    let xdg_cache_home = std::env::var_os("XDG_CACHE_HOME");
    let home = std::env::var_os("HOME");
    let cache_root = legacy_cache_root_from(xdg_cache_home.as_deref(), home.as_deref())?;
    let Some(cache_root) = cache_root else {
        return Ok(());
    };
    cleanup_legacy_updater_cache_in(&cache_root)
}

fn legacy_cache_root_from(
    xdg_cache_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<Option<PathBuf>> {
    let (base, variable) = if let Some(cache) = xdg_cache_home.filter(|value| !value.is_empty()) {
        (PathBuf::from(cache), "XDG_CACHE_HOME")
    } else if let Some(home) = home.filter(|value| !value.is_empty()) {
        (PathBuf::from(home).join(".cache"), "HOME")
    } else {
        return Ok(None);
    };
    if !base.is_absolute() {
        bail!("{variable} must be an absolute path before retired cache cleanup");
    }
    Ok(Some(base.join("bettercodex")))
}

fn cleanup_legacy_updater_cache_in(cache_root: &Path) -> Result<()> {
    for name in ["build", "tmp"] {
        let path = cache_root.join(name);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not inspect retired cache {}", path.display())
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("could not remove retired cache {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
