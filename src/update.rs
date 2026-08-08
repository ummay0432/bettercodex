//! Source-revision checks and the local update command.
//!
//! Installed builds compare their embedded source revision with public `main`
//! after the TUI renders. The explicit command fetches the current installer,
//! mirroring upstream Codex's standalone update path, and that installer builds
//! one immutable source commit while reusing compatible dependency artifacts.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde::Deserialize;
use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command as AsyncProcessCommand;

const DEFAULT_REPOSITORY: &str = "ummay0432/bettercodex";
const GITHUB_API_ROOT: &str = "https://api.github.com";
const GITHUB_RAW_ROOT: &str = "https://raw.githubusercontent.com";
const INSTALLER_PATH: &str = "scripts/install.sh";
const MAX_INSTALLER_BYTES: usize = 1024 * 1024;
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const INSTALL_DIR_ENV: &str = "BCODEX_INSTALL_DIR";
const INSTALL_REVISION_ENV: &str = "BCODEX_INSTALL_REVISION";

const CURL_ARGUMENTS: &[&str] = &[
    "--proto",
    "=https",
    "--tlsv1.2",
    "--fail",
    "--silent",
    "--show-error",
    "--location",
    "--connect-timeout",
    "10",
    "--max-time",
    "30",
    "--user-agent",
    "bettercodex",
];

#[derive(Deserialize)]
struct GitHubCommitResponse {
    sha: String,
}

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

/// Checks once for a different public `main` revision after the TUI renders.
///
/// Development builds stay offline. Installed builds can opt out by setting
/// `BCODEX_SKIP_UPDATE_CHECK`. Failures stay silent and are retried on the next
/// launch so startup and interactive work never depend on GitHub availability.
pub(crate) async fn check_for_update() -> Option<AvailableUpdate> {
    if cfg!(debug_assertions) || std::env::var_os("BCODEX_SKIP_UPDATE_CHECK").is_some() {
        return None;
    }
    let current_revision = source_revision()?;
    let repository = configured_repository().ok()?;
    check_for_source_update_with(
        OsStr::new("curl"),
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
    curl_program: &OsStr,
    repository: &str,
    current_revision: &str,
    timeout: Duration,
) -> Option<AvailableUpdate> {
    if !is_source_revision(current_revision) || validate_repository(repository).is_err() {
        return None;
    }
    let url = commit_api_url(repository);
    let mut command = AsyncProcessCommand::new(curl_program);
    command
        .args(CURL_ARGUMENTS)
        .args(["--header", "Accept: application/vnd.github+json", &url])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let latest_revision = parse_github_revision(&output.stdout).ok()?;
    if latest_revision.eq_ignore_ascii_case(current_revision) {
        return None;
    }
    Some(AvailableUpdate::new(current_revision, &latest_revision))
}

fn commit_api_url(repository: &str) -> String {
    format!("{GITHUB_API_ROOT}/repos/{repository}/commits/main")
}

fn installer_url(repository: &str, revision: &str) -> String {
    format!("{GITHUB_RAW_ROOT}/{repository}/{revision}/{INSTALLER_PATH}")
}

fn parse_github_revision(response: &[u8]) -> Result<String> {
    let response: GitHubCommitResponse = serde_json::from_slice(response)
        .context("GitHub returned an invalid BetterCodex revision response")?;
    if !is_source_revision(&response.sha) {
        bail!("GitHub returned an invalid BetterCodex source revision");
    }
    Ok(response.sha.to_ascii_lowercase())
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
    let latest_revision = resolve_source_revision(OsStr::new("curl"), &repository)?;
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
        OsStr::new("curl"),
        &repository,
        &latest_revision,
        MAX_INSTALLER_BYTES,
    )?;
    run_installer_script(
        &installer,
        OsStr::new("/bin/sh"),
        &install_dir,
        &repository,
        &latest_revision,
    )
}

fn resolve_source_revision(curl_program: &OsStr, repository: &str) -> Result<String> {
    validate_repository(repository)?;
    let url = commit_api_url(repository);
    let output = ProcessCommand::new(curl_program)
        .args(CURL_ARGUMENTS)
        .args(["--header", "Accept: application/vnd.github+json", &url])
        .stdin(Stdio::null())
        .output()
        .context("could not query the current BetterCodex main commit")?;
    if !output.status.success() {
        bail!("GitHub could not resolve BetterCodex main");
    }
    parse_github_revision(&output.stdout)
}

fn fetch_installer(
    curl_program: &OsStr,
    repository: &str,
    revision: &str,
    maximum_bytes: usize,
) -> Result<Vec<u8>> {
    validate_repository(repository)?;
    if !is_source_revision(revision) {
        bail!("cannot fetch the installer from an invalid source revision");
    }
    let url = installer_url(repository, revision);
    let output = ProcessCommand::new(curl_program)
        .args(CURL_ARGUMENTS)
        .arg(&url)
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
    revision: &str,
) -> Result<()> {
    if !is_source_revision(revision) {
        bail!("cannot run the installer for an invalid source revision");
    }
    let mut child = ProcessCommand::new(shell)
        .arg("-s")
        .env(INSTALL_DIR_ENV, install_dir)
        .env(INSTALL_REVISION_ENV, revision)
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

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
