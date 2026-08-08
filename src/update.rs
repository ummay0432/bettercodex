//! Public-main revision checks and the local source update command.
//!
//! Installed builds compare their embedded source revision with public `main`
//! after the TUI renders. The explicit command pins that exact commit, fetches
//! the installer from the same immutable revision, and lets the installer build,
//! verify, and atomically replace the running command.

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
    "--compressed",
    "--connect-timeout",
    "10",
    "--max-time",
    "30",
    "--max-filesize",
    "1048576",
    "--retry",
    "2",
    "--retry-delay",
    "1",
    "--retry-connrefused",
    "--user-agent",
    "bettercodex",
    "--header",
    "X-GitHub-Api-Version: 2022-11-28",
];

#[derive(Deserialize)]
struct GitHubRefResponse {
    #[serde(rename = "ref")]
    reference: String,
    object: GitHubRefObjectResponse,
}

#[derive(Deserialize)]
struct GitHubRefObjectResponse {
    sha: String,
    #[serde(rename = "type")]
    kind: String,
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
    let url = main_ref_api_url(repository);
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
    let latest_revision = parse_github_main_revision(&output.stdout).ok()?;
    if latest_revision.eq_ignore_ascii_case(current_revision) {
        return None;
    }
    Some(AvailableUpdate::new(current_revision, &latest_revision))
}

fn main_ref_api_url(repository: &str) -> String {
    format!("{GITHUB_API_ROOT}/repos/{repository}/git/ref/heads/main")
}

fn installer_url(repository: &str, revision: &str) -> String {
    format!("{GITHUB_RAW_ROOT}/{repository}/{revision}/{INSTALLER_PATH}")
}

fn parse_github_main_revision(response: &[u8]) -> Result<String> {
    let response: GitHubRefResponse = serde_json::from_slice(response)
        .context("GitHub returned an invalid bettercodex main response")?;
    if response.reference != "refs/heads/main"
        || response.object.kind != "commit"
        || !is_source_revision(&response.object.sha)
    {
        bail!("GitHub returned an invalid bettercodex main revision");
    }
    Ok(response.object.sha.to_ascii_lowercase())
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
        "this build has no embedded source revision; run INSTALL_COMMAND.txt before using `bcodex update`",
    )?;
    let repository = configured_repository()?;
    let latest_revision = resolve_main_revision(OsStr::new("curl"), &repository)?;
    if latest_revision.eq_ignore_ascii_case(current_revision) {
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "bettercodex is already current with main at {}.",
            short_revision(current_revision)
        )?;
        return Ok(());
    }

    let executable =
        std::env::current_exe().context("could not locate the running bettercodex binary")?;
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

fn resolve_main_revision(curl_program: &OsStr, repository: &str) -> Result<String> {
    validate_repository(repository)?;
    let url = main_ref_api_url(repository);
    let output = ProcessCommand::new(curl_program)
        .args(CURL_ARGUMENTS)
        .args(["--header", "Accept: application/vnd.github+json", &url])
        .stdin(Stdio::null())
        .output()
        .context("could not query the current bettercodex main revision")?;
    if !output.status.success() {
        bail!("GitHub could not resolve bettercodex main");
    }
    parse_github_main_revision(&output.stdout)
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
        .context("could not fetch the current bettercodex installer")?;
    if !output.status.success() {
        bail!("GitHub could not fetch the current bettercodex installer");
    }
    if output.stdout.is_empty() || output.stdout.len() > maximum_bytes {
        bail!("GitHub returned an empty or oversized bettercodex installer");
    }
    if !output.stdout.starts_with(b"#!/bin/sh\n") {
        bail!("GitHub returned an invalid bettercodex installer");
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
        .context("could not locate the running bettercodex binary directory")
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
        .env_remove("BCODEX_INSTALL_RELEASE_TAG")
        .env_remove("BCODEX_INSTALL_VERSION")
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
