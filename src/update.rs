//! Published-release checks and the local update command.
//!
//! Installed builds compare their package version and embedded source revision
//! with the latest complete GitHub Release after the TUI renders. The explicit
//! command streams one verified zstd release asset directly into an atomic
//! replacement.

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
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const INSTALL_DIR_ENV: &str = "BCODEX_INSTALL_DIR";
const RELEASE_TAG_PREFIX: &str = "bcodex-v";

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
struct GitHubReleaseResponse {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GitHubReleaseAssetResponse>,
}

#[derive(Deserialize)]
struct GitHubReleaseAssetResponse {
    name: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReleaseAsset {
    name: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PublishedRelease {
    tag: String,
    version: String,
    revision: String,
    assets: Vec<ReleaseAsset>,
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

/// Checks once for a different published release after the TUI renders.
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
    check_for_release_update_with(
        OsStr::new("curl"),
        &repository,
        current_revision,
        env!("CARGO_PKG_VERSION"),
        UPDATE_CHECK_TIMEOUT,
    )
    .await
}

pub(crate) fn source_revision() -> Option<&'static str> {
    option_env!("BCODEX_SOURCE_REVISION").filter(|revision| is_source_revision(revision))
}

async fn check_for_release_update_with(
    curl_program: &OsStr,
    repository: &str,
    current_revision: &str,
    current_version: &str,
    timeout: Duration,
) -> Option<AvailableUpdate> {
    if !is_source_revision(current_revision) || validate_repository(repository).is_err() {
        return None;
    }
    let url = latest_release_api_url(repository);
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
    let release = parse_github_release(&output.stdout).ok()?;
    if release.revision.eq_ignore_ascii_case(current_revision)
        || package_version_precedes(&release.version, current_version)
        || !install::has_native_full_asset(&release)
    {
        return None;
    }
    Some(AvailableUpdate::new(current_revision, &release.revision))
}

fn latest_release_api_url(repository: &str) -> String {
    format!("{GITHUB_API_ROOT}/repos/{repository}/releases/latest")
}

fn parse_github_release(response: &[u8]) -> Result<PublishedRelease> {
    let response: GitHubReleaseResponse = serde_json::from_slice(response)
        .context("GitHub returned an invalid bettercodex release response")?;
    if response.draft || response.prerelease {
        bail!("GitHub returned an unpublished bettercodex release");
    }
    let mut release = parse_release_tag(&response.tag_name)?;
    release.assets = response
        .assets
        .into_iter()
        .map(|asset| ReleaseAsset {
            name: asset.name,
            size: asset.size,
            sha256: asset
                .digest
                .and_then(|digest| digest.strip_prefix("sha256:").map(str::to_string))
                .unwrap_or_default(),
        })
        .collect();
    Ok(release)
}

fn parse_release_tag(tag: &str) -> Result<PublishedRelease> {
    let release = tag
        .strip_prefix(RELEASE_TAG_PREFIX)
        .context("GitHub returned an invalid bettercodex release tag")?;
    let (version, revision) = release
        .rsplit_once('-')
        .context("GitHub returned an invalid bettercodex release tag")?;
    if !is_package_version(version)
        || !is_source_revision(revision)
        || revision.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        bail!("GitHub returned an invalid bettercodex release tag");
    }
    Ok(PublishedRelease {
        tag: tag.to_string(),
        version: version.to_string(),
        revision: revision.to_string(),
        assets: Vec::new(),
    })
}

fn is_package_version(version: &str) -> bool {
    package_version(version).is_some()
}

fn package_version(version: &str) -> Option<[u64; 3]> {
    let mut components = version.split('.');
    let parse_component = |component: &str| -> Option<u64> {
        if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        component.parse().ok()
    };
    let version = [
        parse_component(components.next()?)?,
        parse_component(components.next()?)?,
        parse_component(components.next()?)?,
    ];
    components.next().is_none().then_some(version)
}

fn package_version_precedes(candidate: &str, current: &str) -> bool {
    package_version(candidate)
        .zip(package_version(current))
        .is_some_and(|(candidate, current)| candidate < current)
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
        "this build has no embedded source revision; install bettercodex with INSTALL_COMMAND.txt before using `bcodex update`",
    )?;
    let repository = configured_repository()?;
    let release = resolve_published_release(OsStr::new("curl"), &repository)?;
    if release.revision.eq_ignore_ascii_case(current_revision) {
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "The latest published bettercodex release is already installed at {}.",
            short_revision(current_revision)
        )?;
        return Ok(());
    }
    if package_version_precedes(&release.version, env!("CARGO_PKG_VERSION")) {
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "This bettercodex {} build ({}) is newer than the latest published {} release ({}); no downgrade was installed.",
            env!("CARGO_PKG_VERSION"),
            short_revision(current_revision),
            release.version,
            short_revision(&release.revision)
        )?;
        return Ok(());
    }

    let executable =
        std::env::current_exe().context("could not locate the running bettercodex binary")?;
    let configured_dir = std::env::var_os(INSTALL_DIR_ENV);
    let install_dir = update_install_dir(&executable, configured_dir.as_deref())?;
    install::install_release(
        OsStr::new("curl"),
        &repository,
        &release,
        current_revision,
        &executable,
        &install_dir,
    )
}

fn resolve_published_release(curl_program: &OsStr, repository: &str) -> Result<PublishedRelease> {
    validate_repository(repository)?;
    let url = latest_release_api_url(repository);
    let output = ProcessCommand::new(curl_program)
        .args(CURL_ARGUMENTS)
        .args(["--header", "Accept: application/vnd.github+json", &url])
        .stdin(Stdio::null())
        .output()
        .context("could not query the latest bettercodex release")?;
    if !output.status.success() {
        bail!("GitHub could not resolve the latest bettercodex release");
    }
    parse_github_release(&output.stdout)
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

#[path = "update_install.rs"]
mod install;

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
