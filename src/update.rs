//! Public-main revision checks and the local source update command.
//!
//! Installed builds compare their embedded source revision with public `main`
//! after the TUI renders. The explicit command pins that exact commit, fetches
//! the installer from the same immutable revision, and lets the installer build,
//! verify, and atomically replace the running command.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use memchr::memmem;
use reqwest::Client;
use serde::Deserialize;
use std::ffi::OsStr;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Stdio;
use std::time::Duration;

const DEFAULT_REPOSITORY: &str = "ummay0432/bettercodex";
const GITHUB_API_ROOT: &str = "https://api.github.com";
const GITHUB_RAW_ROOT: &str = "https://raw.githubusercontent.com";
const MAX_INSTALLER_BYTES: usize = 1024 * 1024;
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const INSTALL_DIR_ENV: &str = "BCODEX_INSTALL_DIR";
const INSTALL_REVISION_ENV: &str = "BCODEX_INSTALL_REVISION";
// Cargo tracks option_env! values independently from source mtimes. Feeding it
// the installer's content hash prevents a relocated archive with colliding
// mtimes and sizes from reusing a stale package binary.
const BUILD_INPUT_HASH: Option<&str> = option_env!("BCODEX_BUILD_INPUT_HASH");
const SOURCE_REVISION_PREFIX: &[u8] = b"\0bettercodex.source-revision.v1=";
const SOURCE_REVISION_LENGTH: usize = 40;
const SOURCE_REVISION_OFFSET: usize = SOURCE_REVISION_PREFIX.len();
const SOURCE_REVISION_METADATA_LENGTH: usize = SOURCE_REVISION_OFFSET + SOURCE_REVISION_LENGTH + 2;

// Keep the revision in a uniquely framed, fixed-size data record. Source builds
// contain the non-hexadecimal placeholder and therefore remain development
// builds. The installer stamps its selected immutable commit into a staged copy
// after Cargo finishes, so advancing main does not invalidate an otherwise
// reusable Rust compilation solely to change 40 metadata bytes.
#[used]
static SOURCE_REVISION_METADATA: [u8; SOURCE_REVISION_METADATA_LENGTH] =
    *b"\0bettercodex.source-revision.v1=........................................;\0";

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
    check_for_source_update_with(&repository, current_revision, UPDATE_CHECK_TIMEOUT).await
}

pub(crate) fn source_revision() -> Option<&'static str> {
    let metadata = std::hint::black_box(&SOURCE_REVISION_METADATA);
    let revision = std::str::from_utf8(
        &metadata[SOURCE_REVISION_OFFSET..SOURCE_REVISION_OFFSET + SOURCE_REVISION_LENGTH],
    )
    .ok()?;
    is_source_revision(revision).then_some(revision)
}

pub(crate) fn stage_current_binary(
    destination: &Path,
    revision: &str,
    expected_build_input_hash: &str,
) -> Result<()> {
    if !is_source_revision(revision) {
        bail!("cannot stage a binary with an invalid source revision");
    }
    if !is_build_input_hash(expected_build_input_hash) {
        bail!("cannot stage a binary with an invalid build-input hash");
    }
    if build_input_hash() != Some(expected_build_input_hash) {
        bail!("binary does not match the selected source build inputs");
    }
    let executable = std::env::current_exe().context("could not locate the binary being staged")?;
    let mut image = fs::read(&executable)
        .with_context(|| format!("could not read binary {}", executable.display()))?;
    patch_source_revision(&mut image, revision)?;

    let mut staged = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("could not create staged binary {}", destination.display()))?;
    staged
        .write_all(&image)
        .with_context(|| format!("could not write staged binary {}", destination.display()))?;
    Ok(())
}

fn patch_source_revision(image: &mut [u8], revision: &str) -> Result<()> {
    if !is_source_revision(revision) {
        bail!("cannot patch an invalid source revision");
    }
    let marker = std::hint::black_box(SOURCE_REVISION_METADATA.as_slice());
    let mut matches = memmem::find_iter(image, marker);
    let marker_offset = matches
        .next()
        .context("binary has no bettercodex source-revision marker")?;
    if matches.next().is_some() {
        bail!("binary has multiple bettercodex source-revision markers");
    }
    let revision_offset = marker_offset + SOURCE_REVISION_OFFSET;
    image[revision_offset..revision_offset + SOURCE_REVISION_LENGTH]
        .copy_from_slice(revision.as_bytes());
    Ok(())
}

async fn check_for_source_update_with(
    repository: &str,
    current_revision: &str,
    timeout: Duration,
) -> Option<AvailableUpdate> {
    if !is_source_revision(current_revision) || validate_repository(repository).is_err() {
        return None;
    }
    let url = main_ref_api_url(repository);
    check_for_source_update_at(&url, current_revision, timeout).await
}

async fn check_for_source_update_at(
    url: &str,
    current_revision: &str,
    timeout: Duration,
) -> Option<AvailableUpdate> {
    if !is_source_revision(current_revision) {
        return None;
    }
    let client = update_client(timeout).ok()?;
    let response = tokio::time::timeout(timeout, fetch_bounded(&client, url, MAX_INSTALLER_BYTES))
        .await
        .ok()?
        .ok()?;
    let latest_revision = parse_github_main_revision(&response).ok()?;
    if latest_revision.eq_ignore_ascii_case(current_revision) {
        return None;
    }
    Some(AvailableUpdate::new(current_revision, &latest_revision))
}

fn main_ref_api_url(repository: &str) -> String {
    format!("{GITHUB_API_ROOT}/repos/{repository}/git/ref/heads/main")
}

fn installer_url(repository: &str, revision: &str) -> String {
    format!(
        "{GITHUB_RAW_ROOT}/{repository}/{revision}/{}",
        installer_path()
    )
}

fn installer_path() -> &'static str {
    #[cfg(windows)]
    {
        "scripts/install.ps1"
    }
    #[cfg(unix)]
    {
        "scripts/install.sh"
    }
}

fn update_client(timeout: Duration) -> Result<Client> {
    crate::http_client::build_client(
        Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .user_agent("bettercodex")
            .redirect(reqwest::redirect::Policy::limited(5)),
    )
}

async fn fetch_bounded(client: &Client, url: &str, maximum_bytes: usize) -> Result<Vec<u8>> {
    let mut response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .with_context(|| format!("could not download {url}"))?
        .error_for_status()
        .with_context(|| format!("GitHub rejected {url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        bail!("GitHub response exceeded the {maximum_bytes}-byte limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("could not read {url}"))?
    {
        if chunk.len() > maximum_bytes.saturating_sub(bytes.len()) {
            bail!("GitHub response exceeded the {maximum_bytes}-byte limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        bail!("GitHub returned an empty response");
    }
    Ok(bytes)
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

fn build_input_hash() -> Option<&'static str> {
    BUILD_INPUT_HASH.filter(|hash| is_build_input_hash(hash))
}

fn is_build_input_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start the bettercodex update runtime")?;
    let client = update_client(Duration::from_secs(30))?;
    let latest_revision = runtime.block_on(resolve_main_revision(&client, &repository))?;
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
    let installer = runtime.block_on(fetch_installer(
        &client,
        &repository,
        &latest_revision,
        MAX_INSTALLER_BYTES,
    ))?;
    run_installer_script(&installer, &install_dir, &repository, &latest_revision)
}

async fn resolve_main_revision(client: &Client, repository: &str) -> Result<String> {
    validate_repository(repository)?;
    let url = main_ref_api_url(repository);
    let response = fetch_bounded(client, &url, MAX_INSTALLER_BYTES).await?;
    parse_github_main_revision(&response)
}

async fn fetch_installer(
    client: &Client,
    repository: &str,
    revision: &str,
    maximum_bytes: usize,
) -> Result<Vec<u8>> {
    validate_repository(repository)?;
    if !is_source_revision(revision) {
        bail!("cannot fetch the installer from an invalid source revision");
    }
    let url = installer_url(repository, revision);
    let installer = fetch_bounded(client, &url, maximum_bytes).await?;
    if !valid_installer_prefix(&installer) {
        bail!("GitHub returned an invalid bettercodex installer");
    }
    Ok(installer)
}

fn valid_installer_prefix(script: &[u8]) -> bool {
    #[cfg(unix)]
    {
        script.starts_with(b"#!/bin/sh\n")
    }
    #[cfg(windows)]
    {
        script.starts_with(b"#Requires -Version 5.1\n")
            || script.starts_with(b"#Requires -Version 5.1\r\n")
    }
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
    install_dir: &Path,
    repository: &str,
    revision: &str,
) -> Result<()> {
    if !is_source_revision(revision) {
        bail!("cannot run the installer for an invalid source revision");
    }
    if !valid_installer_prefix(script) {
        bail!("cannot run an invalid bettercodex installer");
    }
    #[cfg(unix)]
    let mut child = ProcessCommand::new("/bin/sh")
        .arg("-s")
        .env(INSTALL_DIR_ENV, install_dir)
        .env(INSTALL_REVISION_ENV, revision)
        .env("BCODEX_REPOSITORY", repository)
        .env_remove("BCODEX_INSTALL_RELEASE_TAG")
        .env_remove("BCODEX_INSTALL_VERSION")
        .stdin(Stdio::piped())
        .spawn()
        .context("could not start the bettercodex installer")?;
    #[cfg(windows)]
    let (mut child, script_path) = {
        let script_path = std::env::temp_dir().join(format!(
            "bettercodex-update-{}-{}.ps1",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        crate::platform_fs::configure_private_file(&mut options);
        let mut file = options
            .open(&script_path)
            .with_context(|| format!("could not create update script {}", script_path.display()))?;
        file.write_all(script)?;
        file.sync_all()?;
        drop(file);
        let child = ProcessCommand::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&script_path)
            .env(INSTALL_DIR_ENV, install_dir)
            .env(INSTALL_REVISION_ENV, revision)
            .env("BCODEX_REPOSITORY", repository)
            .env("BCODEX_UPDATE_PARENT_PID", std::process::id().to_string())
            .env_remove("BCODEX_INSTALL_RELEASE_TAG")
            .env_remove("BCODEX_INSTALL_VERSION")
            .stdin(Stdio::null())
            .spawn()
            .context("could not start the bettercodex PowerShell installer")?;
        (child, script_path)
    };
    #[cfg(unix)]
    {
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
    }
    let status = child
        .wait()
        .context("could not wait for the bettercodex installer")?;
    #[cfg(windows)]
    fs::remove_file(&script_path)
        .with_context(|| format!("could not remove update script {}", script_path.display()))?;
    if !status.success() {
        bail!("bettercodex installer exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
