//! Published-release checks and prebuilt-binary updates.
//!
//! One build policy classifies debug source builds, release-profile source
//! builds, and published binaries. Eligible builds compare their package
//! version with the latest published full release after the TUI renders. The
//! explicit update command resolves one immutable release, fetches its installer
//! from the encoded source revision, and passes the validated asset contract to
//! that installer for checksum verification and atomic installation.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use reqwest::Client;
use serde::Deserialize;
use std::cmp::Ordering;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::future::Future;
use std::io::Write;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Stdio;
use std::time::Duration;

const DEFAULT_REPOSITORY: &str = "ummay0432/bettercodex";
const GITHUB_API_ROOT: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2026-03-10";
const GITHUB_RAW_ROOT: &str = "https://raw.githubusercontent.com";
const MAX_RELEASE_METADATA_BYTES: usize = 1024 * 1024;
const MAX_INSTALLER_BYTES: usize = 1024 * 1024;
const MAX_RELEASE_ASSET_BYTES: u64 = 128 * 1024 * 1024;
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const HOME_ENV: &str = "HOME";
const INSTALL_ASSET_SHA256_ENV: &str = "BCODEX_INSTALL_ASSET_SHA256";
const INSTALL_ASSET_SIZE_ENV: &str = "BCODEX_INSTALL_ASSET_SIZE";
const INSTALL_DIR_ENV: &str = "BCODEX_INSTALL_DIR";
const INSTALL_RELEASE_TAG_ENV: &str = "BCODEX_INSTALL_RELEASE_TAG";
const REPOSITORY_ENV: &str = "BCODEX_REPOSITORY";
const RELEASE_TAG_PREFIX: &str = "bcodex-v";
const BUILD_RELEASE_TAG: Option<&str> = option_env!("BCODEX_RELEASE_TAG");
const INSTALLER_PREFIX: &[u8] = b"#!/bin/sh\n\n# Install the latest published bettercodex binary. The only persistent payload\n# is the executable itself; source, Rust, Cargo, and native build tools are not\n# downloaded or required.\n\nset -eu\n";

#[cfg(target_os = "linux")]
const RELEASE_ASSET_NAME: &str = "bcodex-x86_64-unknown-linux-gnu.gz";
#[cfg(target_os = "linux")]
const RUST_TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";
#[cfg(target_os = "macos")]
const RELEASE_ASSET_NAME: &str = "bcodex-aarch64-apple-darwin.gz";
#[cfg(target_os = "macos")]
const RUST_TARGET_TRIPLE: &str = "aarch64-apple-darwin";

#[derive(Deserialize)]
struct GitHubReleaseResponse {
    tag_name: String,
    target_commitish: String,
    draft: bool,
    prerelease: bool,
    immutable: bool,
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
struct ReleaseAsset {
    size: u64,
    sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParsedReleaseTag<'a> {
    version: &'a str,
    revision: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Release {
    tag: String,
    version: String,
    revision: String,
    asset: ReleaseAsset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfiguredRepository {
    name: String,
    overridden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AvailableUpdate {
    current_version: String,
    latest_version: String,
    update_command: String,
}

impl AvailableUpdate {
    fn new(current_version: &str, latest_version: &str, update_command: String) -> Self {
        Self {
            current_version: current_version.to_string(),
            latest_version: latest_version.to_string(),
            update_command,
        }
    }

    pub(crate) fn current_version(&self) -> &str {
        &self.current_version
    }

    pub(crate) fn latest_version(&self) -> &str {
        &self.latest_version
    }

    pub(crate) fn update_command(&self) -> &str {
        &self.update_command
    }

    #[cfg(test)]
    pub(crate) fn test_fixture() -> Self {
        Self::new("1.2.3", "1.3.0", "bcodex update".to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuildKind {
    DebugSource,
    ReleaseSource,
    Published,
}

impl BuildKind {
    fn current() -> Self {
        classify_build(cfg!(debug_assertions), release_tag().is_some())
    }

    fn checks_for_updates(self) -> bool {
        !matches!(self, Self::DebugSource)
    }

    fn supports_explicit_update(self) -> bool {
        !matches!(self, Self::DebugSource)
    }

    fn update_command(
        self,
        executable: &Path,
        install_dir: &Path,
        repository: &str,
    ) -> Option<String> {
        self.supports_explicit_update()
            .then(|| shell_update_command(executable, install_dir, repository))
    }

    fn command_summary(self) -> &'static str {
        match self {
            Self::DebugSource => "Unavailable in debug source builds",
            Self::ReleaseSource => "Install the selected published release",
            Self::Published => "Atomically update this published binary",
        }
    }

    fn help_heading(self) -> &'static str {
        match self {
            Self::DebugSource => "Published-release updates are unavailable in this debug build",
            Self::ReleaseSource => "Install the selected published bettercodex release",
            Self::Published => "Atomically update this published bettercodex binary",
        }
    }

    fn help_text(self) -> &'static str {
        match self {
            Self::DebugSource => {
                "This debug source build cannot use the update command and performs no background update checks. Build with `cargo build --release --locked` or install a published release first."
            }
            Self::ReleaseSource => {
                "This release-profile source build installs the selected published release through BCODEX_INSTALL_DIR or $HOME/.local/bin. It never replaces the running checkout or Cargo output, even when the source package version is equal to or newer than the selected release."
            }
            Self::Published => {
                "This published build updates its own bcodex executable atomically when the default release channel is newer. BCODEX_INSTALL_DIR cannot redirect a published update; repository overrides deliberately select that repository's validated release channel."
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExplicitUpdateDecision {
    Install,
    AlreadyLatest,
    AlreadySelected,
    CurrentIsNewer,
}

#[derive(Clone, Copy)]
enum GitHubRequestKind {
    Api,
    Raw,
}

fn classify_build(debug_assertions: bool, has_valid_release_tag: bool) -> BuildKind {
    if debug_assertions {
        BuildKind::DebugSource
    } else if has_valid_release_tag {
        BuildKind::Published
    } else {
        BuildKind::ReleaseSource
    }
}

/// Returns a bounded, failure-silent update check only for eligible builds.
///
/// Returning `None` keeps the task itself absent from debug builds and from
/// opt-out launches. The TUI starts the returned future only after its first
/// frame has rendered.
pub(crate) fn background_update_check()
-> Option<impl Future<Output = Option<AvailableUpdate>> + Send + 'static> {
    let build_kind = BuildKind::current();
    if !build_kind.checks_for_updates() || std::env::var_os("BCODEX_SKIP_UPDATE_CHECK").is_some() {
        return None;
    }
    Some(check_for_update(build_kind))
}

async fn check_for_update(build_kind: BuildKind) -> Option<AvailableUpdate> {
    let executable = std::env::current_exe().ok()?;
    let configured_dir = configured_install_dir().ok()?;
    let install_dir =
        update_install_dir(build_kind, &executable, configured_dir.as_deref()).ok()?;
    let current_version = current_version().ok()?;
    let repository = configured_repository().ok()?;
    let update_command = build_kind.update_command(&executable, &install_dir, &repository.name)?;
    let latest_version =
        check_for_release_update_with(&repository.name, current_version, UPDATE_CHECK_TIMEOUT)
            .await?;
    Some(AvailableUpdate::new(
        current_version,
        &latest_version,
        update_command,
    ))
}

pub(crate) fn update_command_summary() -> &'static str {
    BuildKind::current().command_summary()
}

pub(crate) fn update_help_heading() -> &'static str {
    BuildKind::current().help_heading()
}

pub(crate) fn update_help_text() -> &'static str {
    BuildKind::current().help_text()
}

pub(crate) fn release_tag() -> Option<&'static str> {
    valid_embedded_release_tag(
        cfg!(debug_assertions),
        BUILD_RELEASE_TAG,
        env!("CARGO_PKG_VERSION"),
    )
}

fn valid_embedded_release_tag<'a>(
    debug_assertions: bool,
    tag: Option<&'a str>,
    package_version: &str,
) -> Option<&'a str> {
    if debug_assertions {
        return None;
    }
    let tag = tag?;
    let parsed = parse_release_tag(tag).ok()?;
    (parsed.version == package_version).then_some(tag)
}

/// Retained for release clients older than 0.1.3, which verify this exact
/// revision before accepting an update.
pub(crate) fn source_revision() -> Option<&'static str> {
    release_tag()?
        .rsplit_once('-')
        .map(|(_, revision)| revision)
}

fn current_version() -> Result<&'static str> {
    let version = env!("CARGO_PKG_VERSION");
    parse_version(version)
        .context("bettercodex package version is not plain major.minor.patch")
        .map(|_| version)
}

async fn check_for_release_update_with(
    repository: &str,
    current_version: &str,
    timeout: Duration,
) -> Option<String> {
    if validate_repository(repository).is_err() {
        return None;
    }
    let url = latest_release_api_url(repository);
    check_for_release_update_at(&url, current_version, timeout).await
}

async fn check_for_release_update_at(
    url: &str,
    current_version: &str,
    timeout: Duration,
) -> Option<String> {
    let client = update_client(timeout).ok()?;
    let response = tokio::time::timeout(
        timeout,
        fetch_bounded(
            &client,
            url,
            MAX_RELEASE_METADATA_BYTES,
            GitHubRequestKind::Api,
        ),
    )
    .await
    .ok()?
    .ok()?;
    let latest = parse_latest_release(&response).ok()?;
    compare_versions(&latest.version, current_version)
        .is_some_and(Ordering::is_gt)
        .then_some(latest.version)
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
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.url().scheme() != "https" {
                    attempt.error(std::io::Error::other(
                        "bettercodex updates refuse non-HTTPS redirects",
                    ))
                } else if attempt.previous().len() >= 5 {
                    attempt.error(std::io::Error::other(
                        "bettercodex update exceeded the redirect limit",
                    ))
                } else {
                    attempt.follow()
                }
            })),
    )
}

async fn fetch_bounded(
    client: &Client,
    url: &str,
    maximum_bytes: usize,
    kind: GitHubRequestKind,
) -> Result<Vec<u8>> {
    let mut request = client.get(url);
    if matches!(kind, GitHubRequestKind::Api) {
        request = request
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION);
    }
    let mut response = request
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
    if !response.immutable {
        bail!("latest bettercodex release is not immutable");
    }
    let parsed_tag = parse_release_tag(&response.tag_name)?;
    if response.target_commitish != parsed_tag.revision {
        bail!("latest bettercodex release target does not match its encoded source revision");
    }
    let version = parsed_tag.version.to_string();
    let revision = parsed_tag.revision.to_string();
    let mut matching_assets = response
        .assets
        .iter()
        .filter(|asset| asset.name == RELEASE_ASSET_NAME);
    let asset = matching_assets
        .next()
        .context("latest bettercodex release has no binary for this platform")?;
    let sha256 = parse_sha256_digest(&asset.digest)
        .context("latest bettercodex release has an invalid binary asset digest")?;
    if matching_assets.next().is_some()
        || asset.state != "uploaded"
        || asset.size == 0
        || asset.size > MAX_RELEASE_ASSET_BYTES
    {
        bail!("latest bettercodex release has an invalid binary asset");
    }
    Ok(Release {
        tag: response.tag_name,
        version,
        revision,
        asset: ReleaseAsset {
            size: asset.size,
            sha256: sha256.to_string(),
        },
    })
}

fn parse_release_tag(tag: &str) -> Result<ParsedReleaseTag<'_>> {
    let remainder = tag
        .strip_prefix(RELEASE_TAG_PREFIX)
        .context("bettercodex release tag has an invalid prefix")?;
    let (version, revision) = remainder
        .rsplit_once('-')
        .context("bettercodex release tag has no source revision")?;
    if parse_version(version).is_none() || !is_source_revision(revision) {
        bail!("bettercodex release tag is invalid");
    }
    Ok(ParsedReleaseTag { version, revision })
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut components = version.split('.');
    let major = parse_version_component(components.next()?)?;
    let minor = parse_version_component(components.next()?)?;
    let patch = parse_version_component(components.next()?)?;
    components.next().is_none().then_some((major, minor, patch))
}

fn parse_version_component(component: &str) -> Option<u64> {
    let bytes = component.as_bytes();
    if bytes == b"0" {
        return Some(0);
    }
    if !matches!(bytes.first(), Some(b'1'..=b'9')) || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    component.parse().ok()
}

fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    Some(parse_version(left)?.cmp(&parse_version(right)?))
}

fn is_source_revision(revision: &str) -> bool {
    revision.len() == 40
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn parse_sha256_digest(digest: &str) -> Option<&str> {
    digest.strip_prefix("sha256:").filter(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn is_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn configured_repository() -> Result<ConfiguredRepository> {
    configured_repository_from(std::env::var_os(REPOSITORY_ENV))
}

fn configured_repository_from(value: Option<OsString>) -> Result<ConfiguredRepository> {
    let repository = match value {
        None => DEFAULT_REPOSITORY.to_string(),
        Some(value) if value.is_empty() => DEFAULT_REPOSITORY.to_string(),
        Some(value) => value.into_string().map_err(|_| {
            anyhow::anyhow!("{REPOSITORY_ENV} must be a UTF-8 owner/repository name")
        })?,
    };
    validate_repository(&repository)?;
    Ok(ConfiguredRepository {
        overridden: !repository.eq_ignore_ascii_case(DEFAULT_REPOSITORY),
        name: repository,
    })
}

fn validate_repository(repository: &str) -> Result<()> {
    let mut components = repository.split('/');
    let owner = components.next().unwrap_or_default();
    let name = components.next().unwrap_or_default();
    if !is_repository_component(owner)
        || !is_repository_component(name)
        || components.next().is_some()
    {
        bail!("{REPOSITORY_ENV} must be an owner/repository name");
    }
    Ok(())
}

fn is_repository_component(component: &str) -> bool {
    !component.is_empty()
        && !matches!(component, "." | "..")
        && component.bytes().all(is_repository_name_byte)
}

fn is_repository_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn configured_install_dir() -> Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(INSTALL_DIR_ENV) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    let directory = PathBuf::from(value);
    validate_install_dir(&directory)?;
    Ok(Some(directory))
}

fn validate_install_dir(directory: &Path) -> Result<()> {
    if !directory.is_absolute() {
        bail!("{INSTALL_DIR_ENV} must be an absolute path");
    }
    if directory
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        bail!("{INSTALL_DIR_ENV} must not contain parent-directory components");
    }
    Ok(())
}

fn default_install_dir() -> Result<PathBuf> {
    let home = std::env::var_os(HOME_ENV)
        .filter(|home| !home.is_empty())
        .context("HOME or BCODEX_INSTALL_DIR must be set")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        bail!("{HOME_ENV} must be an absolute path or {INSTALL_DIR_ENV} must be set");
    }
    if home
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        bail!("{HOME_ENV} must not contain parent-directory components");
    }
    Ok(home.join(".local/bin"))
}

fn update_install_dir(
    build_kind: BuildKind,
    executable: &Path,
    configured: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(configured) = configured {
        validate_install_dir(configured)?;
    }
    match build_kind {
        BuildKind::DebugSource => {
            bail!("debug bettercodex builds cannot select an update destination")
        }
        BuildKind::Published => {
            if executable.file_name() != Some(OsStr::new("bcodex")) {
                bail!(
                    "the published bettercodex executable must be named bcodex to update in place"
                );
            }
            let directory = executable
                .parent()
                .filter(|parent| parent.is_absolute())
                .context("could not locate the running bettercodex binary directory")?;
            if is_cargo_artifact_path(directory) {
                bail!(
                    "a published update cannot replace a binary inside a Cargo artifact tree; install the release into a normal command directory first"
                );
            }
            if configured.is_some_and(|configured| !same_location(configured, directory)) {
                bail!(
                    "{INSTALL_DIR_ENV} cannot redirect a published update; unset it or set it to the running executable's directory"
                );
            }
            Ok(directory.to_path_buf())
        }
        BuildKind::ReleaseSource => {
            let directory = configured
                .map(Path::to_path_buf)
                .map_or_else(default_install_dir, Ok)?;
            if is_cargo_artifact_path(&directory) {
                bail!(
                    "a source-build update cannot install inside a Cargo artifact tree; choose another {INSTALL_DIR_ENV}"
                );
            }
            if source_cargo_artifact_root(executable)
                .is_some_and(|root| path_is_within(&directory, &root))
            {
                bail!(
                    "a source-build update cannot install inside its Cargo artifact tree; choose another {INSTALL_DIR_ENV}"
                );
            }
            Ok(directory)
        }
    }
}

fn is_cargo_artifact_path(path: &Path) -> bool {
    let path = comparable_path(path);
    if path.ancestors().any(|ancestor| {
        ancestor.join(".rustc_info.json").is_file()
            || (ancestor.join(".fingerprint").is_dir() && ancestor.join("deps").is_dir())
    }) {
        return true;
    }
    path.ancestors()
        .filter(|ancestor| ancestor.file_name() == Some(OsStr::new("target")))
        .any(|target_root| {
            let Ok(relative) = path.strip_prefix(target_root) else {
                return false;
            };
            let mut components = relative
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(component) => Some(component),
                    _ => None,
                });
            let Some(first) = components.next() else {
                return true;
            };
            is_cargo_profile_name(first) || components.next().is_some_and(is_cargo_profile_name)
        })
}

fn is_cargo_profile_name(name: &OsStr) -> bool {
    matches!(name.to_str(), Some("debug" | "release"))
}

fn source_cargo_artifact_root(executable: &Path) -> Option<PathBuf> {
    let profile_directory = executable.parent()?;
    let looks_like_cargo_profile = profile_directory.file_name() == Some(OsStr::new("release"))
        || (profile_directory.join("deps").is_dir()
            && profile_directory.join(".fingerprint").is_dir());
    if !looks_like_cargo_profile {
        return None;
    }
    let parent = profile_directory.parent()?;
    let root = if parent.file_name() == Some(OsStr::new(RUST_TARGET_TRIPLE)) {
        parent.parent()?
    } else {
        parent
    };
    Some(comparable_path(root))
}

fn path_is_within(path: &Path, directory: &Path) -> bool {
    comparable_path(path).starts_with(comparable_path(directory))
}

fn same_location(left: &Path, right: &Path) -> bool {
    comparable_path(left) == comparable_path(right)
}

fn comparable_path(path: &Path) -> PathBuf {
    let normalized = lexical_absolute_path(path);
    let mut cursor = normalized.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(mut resolved) = std::fs::canonicalize(cursor) {
            for component in missing.iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
        let Some(file_name) = cursor.file_name() else {
            return normalized;
        };
        missing.push(file_name.to_os_string());
        let Some(parent) = cursor.parent() else {
            return normalized;
        };
        cursor = parent;
    }
}

fn lexical_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            std::path::Component::RootDir => normalized = PathBuf::from("/"),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(component) => normalized.push(component),
            std::path::Component::Prefix(_) => unreachable!("Unix paths have no prefix component"),
        }
    }
    normalized
}

fn explicit_update_decision(
    build_kind: BuildKind,
    repository_overridden: bool,
    current_version: &str,
    current_tag: Option<&str>,
    latest: &Release,
) -> Result<ExplicitUpdateDecision> {
    if !build_kind.supports_explicit_update() {
        bail!(
            "debug bettercodex builds cannot be updated; use a release-profile build or install a published release"
        );
    }
    if current_tag == Some(latest.tag.as_str()) {
        return Ok(if repository_overridden {
            ExplicitUpdateDecision::AlreadySelected
        } else {
            ExplicitUpdateDecision::AlreadyLatest
        });
    }
    if build_kind == BuildKind::ReleaseSource || repository_overridden {
        return Ok(ExplicitUpdateDecision::Install);
    }
    match compare_versions(&latest.version, current_version)
        .context("could not compare bettercodex release versions")?
    {
        Ordering::Greater => Ok(ExplicitUpdateDecision::Install),
        Ordering::Less => Ok(ExplicitUpdateDecision::CurrentIsNewer),
        Ordering::Equal if current_tag.is_none() => Ok(ExplicitUpdateDecision::Install),
        Ordering::Equal => bail!(
            "latest published release reuses version {current_version} with a different source revision"
        ),
    }
}

pub(crate) fn run_update() -> Result<()> {
    let build_kind = BuildKind::current();
    if !build_kind.supports_explicit_update() {
        bail!(
            "debug bettercodex builds cannot be updated; use a release-profile build or install a published release"
        );
    }
    let current_version = current_version()?;
    let repository = configured_repository()?;
    let executable =
        std::env::current_exe().context("could not locate the running bettercodex binary")?;
    let configured_dir = configured_install_dir()?;
    let install_dir = update_install_dir(build_kind, &executable, configured_dir.as_deref())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start the bettercodex update runtime")?;
    let client = update_client(Duration::from_secs(30))?;
    let latest = runtime.block_on(resolve_latest_release(&client, &repository.name))?;
    match explicit_update_decision(
        build_kind,
        repository.overridden,
        current_version,
        release_tag(),
        &latest,
    )? {
        ExplicitUpdateDecision::Install => {}
        ExplicitUpdateDecision::AlreadyLatest => {
            writeln!(
                std::io::stdout().lock(),
                "bettercodex {current_version} is already the latest published release."
            )?;
            return Ok(());
        }
        ExplicitUpdateDecision::AlreadySelected => {
            writeln!(
                std::io::stdout().lock(),
                "bettercodex {current_version} is already the selected release from {}.",
                repository.name
            )?;
            return Ok(());
        }
        ExplicitUpdateDecision::CurrentIsNewer => {
            writeln!(
                std::io::stdout().lock(),
                "bettercodex {current_version} is newer than the latest published release {}.",
                latest.version
            )?;
            return Ok(());
        }
    }

    let installer = runtime.block_on(fetch_installer(
        &client,
        &repository.name,
        &latest.revision,
        MAX_INSTALLER_BYTES,
    ))?;
    run_installer_script(&installer, &install_dir, &repository.name, &latest)
}

async fn resolve_latest_release(client: &Client, repository: &str) -> Result<Release> {
    validate_repository(repository)?;
    let url = latest_release_api_url(repository);
    let response = fetch_bounded(
        client,
        &url,
        MAX_RELEASE_METADATA_BYTES,
        GitHubRequestKind::Api,
    )
    .await?;
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
    fetch_installer_at(client, &url, maximum_bytes).await
}

async fn fetch_installer_at(client: &Client, url: &str, maximum_bytes: usize) -> Result<Vec<u8>> {
    let installer = fetch_bounded(client, url, maximum_bytes, GitHubRequestKind::Raw).await?;
    if !valid_installer_prefix(&installer) {
        bail!("GitHub returned an invalid bettercodex installer");
    }
    Ok(installer)
}

fn valid_installer_prefix(script: &[u8]) -> bool {
    script.starts_with(INSTALLER_PREFIX)
}

fn run_installer_script(
    script: &[u8],
    install_dir: &Path,
    repository: &str,
    release: &Release,
) -> Result<()> {
    let parsed_tag = parse_release_tag(&release.tag)?;
    if parsed_tag.version != release.version || parsed_tag.revision != release.revision {
        bail!("cannot run the installer with inconsistent release metadata");
    }
    validate_repository(repository)?;
    validate_install_dir(install_dir)?;
    if release.asset.size == 0
        || release.asset.size > MAX_RELEASE_ASSET_BYTES
        || !is_sha256(&release.asset.sha256)
    {
        bail!("cannot run the installer with invalid release asset metadata");
    }
    if !valid_installer_prefix(script) {
        bail!("cannot run an invalid bettercodex installer");
    }
    let mut command = ProcessCommand::new("/bin/sh");
    command
        .arg("-s")
        .env_remove(INSTALL_DIR_ENV)
        .env_remove(INSTALL_RELEASE_TAG_ENV)
        .env_remove(INSTALL_ASSET_SHA256_ENV)
        .env_remove(INSTALL_ASSET_SIZE_ENV)
        .env_remove(REPOSITORY_ENV)
        .env(INSTALL_DIR_ENV, install_dir)
        .env(INSTALL_RELEASE_TAG_ENV, &release.tag)
        .env(INSTALL_ASSET_SHA256_ENV, &release.asset.sha256)
        .env(INSTALL_ASSET_SIZE_ENV, release.asset.size.to_string())
        .env(REPOSITORY_ENV, repository)
        .stdin(Stdio::piped());
    let mut child = command
        .spawn()
        .context("could not start the bettercodex installer")?;
    let write_result = child
        .stdin
        .take()
        .context("could not open the bettercodex installer input")?
        .write_all(script);
    match write_result {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
            let status = child
                .wait()
                .context("could not wait for the bettercodex installer")?;
            if !status.success() {
                bail!("bettercodex installer exited with {status}");
            }
            return Err(error).context("could not send the bettercodex installer to the shell");
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("could not send the bettercodex installer to the shell");
        }
    }
    let status = child
        .wait()
        .context("could not wait for the bettercodex installer")?;
    if !status.success() {
        bail!("bettercodex installer exited with {status}");
    }
    Ok(())
}

fn shell_update_command(executable: &Path, install_dir: &Path, repository: &str) -> String {
    let executable_bytes = executable.as_os_str().as_bytes();
    let install_dir_bytes = install_dir.as_os_str().as_bytes();
    if let (Some(executable), Some(install_dir)) = (
        printable_shell_word(executable_bytes),
        printable_shell_word(install_dir_bytes),
    ) {
        return format!(
            "{REPOSITORY_ENV}={repository} {INSTALL_DIR_ENV}={install_dir} {executable} update"
        );
    }

    let executable = octal_shell_bytes(executable_bytes);
    let install_dir = octal_shell_bytes(install_dir_bytes);
    format!(
        "(_bcodex_exe=$(printf '%b_' '{executable}'); _bcodex_install_dir=$(printf '%b_' '{install_dir}'); {REPOSITORY_ENV}={repository} {INSTALL_DIR_ENV}=\"${{_bcodex_install_dir%_}}\" \"${{_bcodex_exe%_}}\" update)"
    )
}

fn printable_shell_word(bytes: &[u8]) -> Option<String> {
    if !bytes.iter().all(|byte| matches!(byte, b' '..=b'~')) {
        return None;
    }
    shlex::try_quote(std::str::from_utf8(bytes).ok()?)
        .ok()
        .map(std::borrow::Cow::into_owned)
}

fn octal_shell_bytes(bytes: &[u8]) -> String {
    let mut escaped = String::with_capacity(bytes.len().saturating_mul(5));
    for &byte in bytes {
        escaped.push('\\');
        escaped.push('0');
        escaped.push(char::from(b'0' + ((byte >> 6) & 7)));
        escaped.push(char::from(b'0' + ((byte >> 3) & 7)));
        escaped.push(char::from(b'0' + (byte & 7)));
    }
    escaped
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
