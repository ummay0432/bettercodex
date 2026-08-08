//! Lazy release checks and the self-update command.
//!
//! The one-shot, failure-silent background check follows Pi's startup behavior
//! (`packages/coding-agent/src/utils/version-check.ts` and
//! `modes/interactive/interactive-mode.ts` at
//! `e47b8e37a6211ebd0b2942fa87059d64f81eec02`). Bettercodex adapts the lookup
//! to its authenticated private GitHub Releases and reuses its checked-in,
//! checksum-verifying installer for the explicit update command.

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
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const INSTALLER_SCRIPT: &[u8] = include_bytes!("../scripts/install.sh");

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AvailableUpdate {
    pub(crate) current_version: String,
    pub(crate) latest_version: String,
}

/// Checks once for a newer private release after the TUI has rendered.
///
/// Development builds stay offline by default. Installed release builds can
/// opt out by setting `BCODEX_SKIP_UPDATE_CHECK`.
pub(crate) async fn check_for_update() -> Option<AvailableUpdate> {
    if cfg!(debug_assertions) || std::env::var_os("BCODEX_SKIP_UPDATE_CHECK").is_some() {
        return None;
    }
    let repository =
        std::env::var("BCODEX_REPOSITORY").unwrap_or_else(|_| DEFAULT_REPOSITORY.to_string());
    check_for_update_with(
        OsStr::new("gh"),
        &repository,
        env!("CARGO_PKG_VERSION"),
        UPDATE_CHECK_TIMEOUT,
    )
    .await
}

async fn check_for_update_with(
    gh_program: &OsStr,
    repository: &str,
    current_version: &str,
    timeout: Duration,
) -> Option<AvailableUpdate> {
    let mut command = AsyncProcessCommand::new(gh_program);
    command
        .args([
            "release", "view", "--repo", repository, "--json", "tagName", "--jq", ".tagName",
        ])
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
    let latest_tag = std::str::from_utf8(&output.stdout).ok()?.trim();
    let latest_version = latest_tag.strip_prefix('v')?;
    is_newer(latest_version, current_version)?.then(|| AvailableUpdate {
        current_version: current_version.to_string(),
        latest_version: latest_version.to_string(),
    })
}

fn is_newer(latest: &str, current: &str) -> Option<bool> {
    Some(parse_stable_version(latest)? > parse_stable_version(current)?)
}

fn parse_stable_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut components = version.trim().split('.');
    let major = components.next()?.parse().ok()?;
    let minor = components.next()?.parse().ok()?;
    let patch = components.next()?.parse().ok()?;
    if components.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

pub(crate) fn run_update() -> Result<()> {
    let executable =
        std::env::current_exe().context("could not locate the running bettercodex binary")?;
    let configured_install_dir = std::env::var_os("BCODEX_INSTALL_DIR");
    let install_dir = update_install_dir(&executable, configured_install_dir.as_deref())?;
    run_installer_script(
        INSTALLER_SCRIPT,
        OsStr::new("/bin/sh"),
        install_dir.as_os_str(),
    )
}

fn update_install_dir(executable: &Path, configured: Option<&OsStr>) -> Result<PathBuf> {
    if let Some(configured) = configured {
        return Ok(PathBuf::from(configured));
    }
    executable
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .context("could not locate the running bettercodex binary directory")
}

fn run_installer_script(script: &[u8], shell: &OsStr, install_dir: &OsStr) -> Result<()> {
    let mut child = ProcessCommand::new(shell)
        .arg("-s")
        .env("BCODEX_RELEASE", "latest")
        .env("BCODEX_INSTALL_DIR", install_dir)
        .stdin(Stdio::piped())
        .spawn()
        .context("could not start the bettercodex installer")?;
    let write_result = child
        .stdin
        .take()
        .context("could not open the bettercodex installer input")?
        .write_all(script);
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error).context("could not send the bettercodex installer to the shell");
    }
    let status = child
        .wait()
        .context("could not wait for the bettercodex installer")?;
    if !status.success() {
        bail!("bettercodex installer exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
