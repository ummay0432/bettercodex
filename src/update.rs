//! Published-release checks and prebuilt-binary updates.
//!
//! Distribution builds embed their GitHub release tag. After the TUI renders,
//! they compare its semantic version with the latest published full release.
//! The explicit update command fetches the installer from that release's
//! immutable source revision and lets it verify and atomically install the
//! matching prebuilt binary.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use reqwest::Client;
use serde::Deserialize;
use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Stdio;
use std::time::Duration;

const DEFAULT_REPOSITORY: &str = "ummay0432/bettercodex";
const GITHUB_API_ROOT: &str = "https://api.github.com";
const GITHUB_RAW_ROOT: &str = "https://raw.githubusercontent.com";
const MAX_RELEASE_METADATA_BYTES: usize = 1024 * 1024;
const MAX_INSTALLER_BYTES: usize = 1024 * 1024;
const MAX_RELEASE_ASSET_BYTES: u64 = 128 * 1024 * 1024;
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const INSTALL_DIR_ENV: &str = "BCODEX_INSTALL_DIR";
const INSTALL_RELEASE_TAG_ENV: &str = "BCODEX_INSTALL_RELEASE_TAG";
const RELEASE_TAG_PREFIX: &str = "bcodex-v";
const BUILD_RELEASE_TAG: Option<&str> = option_env!("BCODEX_RELEASE_TAG");

#[cfg(target_os = "linux")]
const RELEASE_ASSET_NAME: &str = "bcodex-x86_64-unknown-linux-gnu.gz";
#[cfg(target_os = "macos")]
const RELEASE_ASSET_NAME: &str = "bcodex-aarch64-apple-darwin.gz";

#[derive(Deserialize)]
struct GitHubReleaseResponse {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubReleaseAssetResponse>,
}

#[derive(Deserialize)]
struct GitHubReleaseAssetResponse {
    name: String,
    state: String,
    size: u64,
    digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Release {
    tag: String,
    version: String,
    revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AvailableUpdate {
    current_revision: String,
    latest_revision: String,
}

impl AvailableUpdate {
    fn new(current_revision: &str, latest_revision: &str) -> Self {
        Self {
            current_revision: current_revision.to_string(),
            latest_revision: latest_revision.to_string(),
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

/// Checks once for a newer published bettercodex release after the TUI renders.
///
/// Development builds stay offline. Distribution builds can opt out by setting
/// `BCODEX_SKIP_UPDATE_CHECK`. Failures stay silent and are retried on the next
/// launch so startup and interactive work never depend on GitHub availability.
pub(crate) async fn check_for_update() -> Option<AvailableUpdate> {
    if cfg!(debug_assertions) || std::env::var_os("BCODEX_SKIP_UPDATE_CHECK").is_some() {
        return None;
    }
    let current = current_release()?;
    let repository = configured_repository().ok()?;
    check_for_release_update_with(&repository, &current, UPDATE_CHECK_TIMEOUT).await
}

pub(crate) fn release_tag() -> Option<&'static str> {
    let tag = BUILD_RELEASE_TAG?;
    let release = parse_release_tag(tag).ok()?;
    (release.version == env!("CARGO_PKG_VERSION")).then_some(tag)
}

/// Retained for release clients older than 0.1.3, which verify this exact
/// revision before accepting an update.
pub(crate) fn source_revision() -> Option<&'static str> {
    release_tag()?
        .rsplit_once('-')
        .map(|(_, revision)| revision)
}

fn current_release() -> Option<Release> {
    parse_release_tag(release_tag()?).ok()
}

async fn check_for_release_update_with(
    repository: &str,
    current: &Release,
    timeout: Duration,
) -> Option<AvailableUpdate> {
    if validate_repository(repository).is_err() {
        return None;
    }
    let url = latest_release_api_url(repository);
    check_for_release_update_at(&url, current, timeout).await
}

async fn check_for_release_update_at(
    url: &str,
    current: &Release,
    timeout: Duration,
) -> Option<AvailableUpdate> {
    let client = update_client(timeout).ok()?;
    let response = tokio::time::timeout(
        timeout,
        fetch_bounded(&client, url, MAX_RELEASE_METADATA_BYTES),
    )
    .await
    .ok()?
    .ok()?;
    let latest = parse_latest_release(&response).ok()?;
    is_newer(&latest.version, &current.version)
        .is_some_and(|newer| newer)
        .then(|| AvailableUpdate::new(&current.revision, &latest.revision))
}

fn latest_release_api_url(repository: &str) -> String {
    format!("{GITHUB_API_ROOT}/repos/{repository}/releases/latest")
}

fn installer_url(repository: &str, revision: &str) -> String {
    format!(
        "{GITHUB_RAW_ROOT}/{repository}/{revision}/{}",
        installer_path()
    )
}

fn installer_path() -> &'static str {
    "scripts/install.sh"
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

fn parse_latest_release(response: &[u8]) -> Result<Release> {
    let response: GitHubReleaseResponse = serde_json::from_slice(response)
        .context("GitHub returned invalid bettercodex release metadata")?;
    if response.draft || response.prerelease {
        bail!("GitHub returned a draft or prerelease as the latest full release");
    }
    let release = parse_release_tag(&response.tag_name)?;
    let mut matching_assets = response
        .assets
        .iter()
        .filter(|asset| asset.name == RELEASE_ASSET_NAME);
    let asset = matching_assets
        .next()
        .context("latest bettercodex release has no binary for this platform")?;
    if matching_assets.next().is_some()
        || asset.state != "uploaded"
        || asset.size == 0
        || asset.size > MAX_RELEASE_ASSET_BYTES
        || !is_sha256_digest(&asset.digest)
    {
        bail!("latest bettercodex release has an invalid binary asset");
    }
    Ok(release)
}

fn parse_release_tag(tag: &str) -> Result<Release> {
    let remainder = tag
        .strip_prefix(RELEASE_TAG_PREFIX)
        .context("bettercodex release tag has an invalid prefix")?;
    let (version, revision) = remainder
        .rsplit_once('-')
        .context("bettercodex release tag has no source revision")?;
    if parse_version(version).is_none() || !is_source_revision(revision) {
        bail!("bettercodex release tag is invalid");
    }
    Ok(Release {
        tag: tag.to_string(),
        version: version.to_string(),
        revision: revision.to_string(),
    })
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut components = version.split('.');
    let major = components.next()?.parse().ok()?;
    let minor = components.next()?.parse().ok()?;
    let patch = components.next()?.parse().ok()?;
    components.next().is_none().then_some((major, minor, patch))
}

fn is_newer(latest: &str, current: &str) -> Option<bool> {
    Some(parse_version(latest)? > parse_version(current)?)
}

fn is_source_revision(revision: &str) -> bool {
    revision.len() == 40
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn short_revision(revision: &str) -> &str {
    revision.get(..12).unwrap_or(revision)
}

fn is_sha256_digest(digest: &str) -> bool {
    digest
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
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
    let current = current_release().context(
        "this is not a published bettercodex build; install a published release before using `bcodex update`",
    )?;
    let repository = configured_repository()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start the bettercodex update runtime")?;
    let client = update_client(Duration::from_secs(30))?;
    let latest = runtime.block_on(resolve_latest_release(&client, &repository))?;
    if !is_newer(&latest.version, &current.version).unwrap_or(false) {
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "bettercodex {} is already the latest published release.",
            current.version
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
        &latest.revision,
        MAX_INSTALLER_BYTES,
    ))?;
    run_installer_script(&installer, &install_dir, &repository, &latest.tag)
}

async fn resolve_latest_release(client: &Client, repository: &str) -> Result<Release> {
    validate_repository(repository)?;
    let url = latest_release_api_url(repository);
    let response = fetch_bounded(client, &url, MAX_RELEASE_METADATA_BYTES).await?;
    parse_latest_release(&response)
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
    script.starts_with(b"#!/bin/sh\n")
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
    release_tag: &str,
) -> Result<()> {
    parse_release_tag(release_tag)?;
    if !valid_installer_prefix(script) {
        bail!("cannot run an invalid bettercodex installer");
    }
    let mut child = ProcessCommand::new("/bin/sh")
        .arg("-s")
        .env(INSTALL_DIR_ENV, install_dir)
        .env(INSTALL_RELEASE_TAG_ENV, release_tag)
        .env("BCODEX_REPOSITORY", repository)
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
