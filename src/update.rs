//! Lazy source-revision checks and the self-update command.
//!
//! The one-shot, failure-silent background check follows Pi's startup behavior
//! (`packages/coding-agent/src/utils/version-check.ts` and
//! `modes/interactive/interactive-mode.ts` at
//! `e47b8e37a6211ebd0b2942fa87059d64f81eec02`). Bettercodex adapts the lookup
//! to an authenticated private repository and reuses its checked-in,
//! source-building installer for the explicit update command. Installed builds
//! compare immutable source revisions so integrated updates do not depend on a
//! separate version bump or release tag.

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AvailableUpdate;

/// Checks once for a newer private source revision after the TUI has rendered.
///
/// Development builds stay offline by default. Installed builds can
/// opt out by setting `BCODEX_SKIP_UPDATE_CHECK`.
pub(crate) async fn check_for_update() -> Option<AvailableUpdate> {
    if cfg!(debug_assertions) || std::env::var_os("BCODEX_SKIP_UPDATE_CHECK").is_some() {
        return None;
    }
    let repository =
        std::env::var("BCODEX_REPOSITORY").unwrap_or_else(|_| DEFAULT_REPOSITORY.to_string());
    match source_revision() {
        Some(current_revision) => {
            check_for_source_update_with(
                OsStr::new("gh"),
                &repository,
                current_revision,
                UPDATE_CHECK_TIMEOUT,
            )
            .await
        }
        None => {
            // Release builds made outside the installer predate, or omit, the source-revision
            // contract. Preserve the tag check so those builds still receive versioned updates.
            check_for_release_update_with(
                OsStr::new("gh"),
                &repository,
                env!("CARGO_PKG_VERSION"),
                UPDATE_CHECK_TIMEOUT,
            )
            .await
        }
    }
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
    if !is_source_revision(current_revision) {
        return None;
    }
    let mut command = AsyncProcessCommand::new(gh_program);
    let endpoint = format!("repos/{repository}/commits/main");
    command
        .args(["api", &endpoint, "--jq", ".sha"])
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
    Some(AvailableUpdate)
}

async fn check_for_release_update_with(
    gh_program: &OsStr,
    repository: &str,
    current_version: &str,
    timeout: Duration,
) -> Option<AvailableUpdate> {
    let mut command = AsyncProcessCommand::new(gh_program);
    let endpoint = format!("repos/{repository}/tags?per_page=100");
    command
        .args(["api", &endpoint, "--paginate", "--jq", ".[].name"])
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
    let tags = std::str::from_utf8(&output.stdout).ok()?;
    let latest_version = tags
        .lines()
        .filter_map(|tag| {
            let version = tag.strip_prefix('v')?;
            Some((parse_stable_version(version)?, version))
        })
        .max_by_key(|(parsed, _)| *parsed)?
        .1;
    is_newer(latest_version, current_version)?.then_some(AvailableUpdate)
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

fn is_source_revision(revision: &str) -> bool {
    revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
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
