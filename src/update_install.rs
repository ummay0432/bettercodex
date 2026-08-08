use super::PublishedRelease;
use super::ReleaseAsset;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use sha2::Digest;
use sha2::Sha256;
use std::ffi::OsStr;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const GITHUB_RELEASE_ROOT: &str = "https://github.com";
const ASSET_DOWNLOAD_ATTEMPTS: u8 = 3;
const MAX_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ZSTD_WINDOW_LOG: u32 = 27;
const LOCK_FRESHNESS: Duration = Duration::from_secs(30);

const CURL_ASSET_ARGUMENTS: &[&str] = &[
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
    "300",
    "--max-filesize",
    "134217728",
    "--user-agent",
    "bettercodex",
];

pub(super) fn install_release(
    curl_program: &OsStr,
    repository: &str,
    release: &PublishedRelease,
    current_revision: &str,
    current_executable: &Path,
    install_dir: &Path,
) -> Result<()> {
    let target = native_target(std::env::consts::OS, std::env::consts::ARCH)?;
    fs::create_dir_all(install_dir).with_context(|| {
        format!(
            "could not create the BetterCodex install directory {}",
            install_dir.display()
        )
    })?;
    let destination = install_dir.join("bcodex");
    reject_unsafe_destination(&destination)?;
    let _lock = InstallLock::acquire(install_dir)?;
    cleanup_stale_stages(install_dir)?;

    let full_name = format!("bcodex-{target}.zst");
    let full_asset = unique_asset(release, &full_name)?.with_context(|| {
        format!(
            "published BetterCodex release {} has no {full_name}",
            release.tag
        )
    })?;
    let patch_name = format!("bcodex-{target}-from-{current_revision}.patch.zst");
    let patch_asset = unique_asset(release, &patch_name)?;

    let full_candidate = || {
        eprintln!(
            "==> Downloading full BetterCodex {} update",
            release.version
        );
        download_verified_candidate(
            curl_program,
            repository,
            release,
            full_asset,
            install_dir,
            None,
        )
    };
    let candidate = if let Some(patch_asset) = patch_asset {
        match fs::read(current_executable) {
            Ok(current_bytes) if current_bytes.len() as u64 <= MAX_BINARY_BYTES => {
                eprintln!(
                    "==> Downloading compact BetterCodex {} update",
                    release.version
                );
                match download_verified_candidate(
                    curl_program,
                    repository,
                    release,
                    patch_asset,
                    install_dir,
                    Some(&current_bytes),
                ) {
                    Ok(candidate) => candidate,
                    Err(error) => {
                        drop(current_bytes);
                        eprintln!(
                            "bettercodex updater: warning: compact update failed ({error:#}); retrying with the full executable"
                        );
                        full_candidate()?
                    }
                }
            }
            Ok(_) => {
                eprintln!(
                    "bettercodex updater: warning: current executable is too large for a compact update; downloading the full executable"
                );
                full_candidate()?
            }
            Err(error) => {
                eprintln!(
                    "bettercodex updater: warning: could not read {} for a compact update ({error}); downloading the full executable",
                    current_executable.display()
                );
                full_candidate()?
            }
        }
    } else {
        full_candidate()?
    };

    candidate.install(&destination)?;
    if let Err(error) = cleanup_source_updater_caches() {
        eprintln!(
            "bettercodex updater: warning: could not remove every retired source-updater cache: {error:#}"
        );
    }
    eprintln!(
        "==> Updated BetterCodex to {} ({})",
        release.version,
        short_revision(&release.revision)
    );
    eprintln!("Restart any running BetterCodex session to use the new executable.");
    Ok(())
}

fn download_verified_candidate(
    curl_program: &OsStr,
    repository: &str,
    release: &PublishedRelease,
    asset: &ReleaseAsset,
    install_dir: &Path,
    reference: Option<&[u8]>,
) -> Result<StagedBinary> {
    let candidate = download_candidate_with_retries(
        curl_program,
        repository,
        release,
        asset,
        install_dir,
        reference,
    )?;
    verify_candidate(&candidate.path, release)?;
    Ok(candidate)
}

fn download_candidate_with_retries(
    curl_program: &OsStr,
    repository: &str,
    release: &PublishedRelease,
    asset: &ReleaseAsset,
    install_dir: &Path,
    reference: Option<&[u8]>,
) -> Result<StagedBinary> {
    for attempt in 1..=ASSET_DOWNLOAD_ATTEMPTS {
        match download_and_expand(
            curl_program,
            repository,
            release,
            asset,
            install_dir,
            reference,
        ) {
            Ok(candidate) => return Ok(candidate),
            Err(error) if attempt < ASSET_DOWNLOAD_ATTEMPTS => {
                eprintln!(
                    "bettercodex updater: warning: download attempt {attempt} for {} failed ({error:#}); retrying",
                    asset.name
                );
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the bounded BetterCodex asset retry loop always returns")
}

pub(super) fn native_target(os: &str, architecture: &str) -> Result<&'static str> {
    match (os, architecture) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux" | "macos", _) => bail!("unsupported architecture: {architecture}"),
        _ => bail!("only macOS and Linux are supported"),
    }
}

pub(super) fn has_native_full_asset(release: &PublishedRelease) -> bool {
    let Ok(target) = native_target(std::env::consts::OS, std::env::consts::ARCH) else {
        return false;
    };
    let name = format!("bcodex-{target}.zst");
    matches!(
        unique_asset(release, &name),
        Ok(Some(asset)) if asset.validate().is_ok()
    )
}

fn unique_asset<'a>(
    release: &'a PublishedRelease,
    expected_name: &str,
) -> Result<Option<&'a ReleaseAsset>> {
    let mut matches = release
        .assets
        .iter()
        .filter(|asset| asset.name == expected_name);
    let asset = matches.next();
    if matches.next().is_some() {
        bail!(
            "published BetterCodex release {} contains duplicate {expected_name} assets",
            release.tag
        );
    }
    Ok(asset)
}

fn reject_unsafe_destination(destination: &Path) -> Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "refusing to replace symlinked BetterCodex executable {}",
                destination.display()
            )
        }
        Ok(metadata) if !metadata.is_file() => {
            bail!("{} exists but is not a regular file", destination.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "could not inspect the BetterCodex executable {}",
                destination.display()
            )
        }),
    }
}

fn download_and_expand(
    curl_program: &OsStr,
    repository: &str,
    release: &PublishedRelease,
    asset: &ReleaseAsset,
    install_dir: &Path,
    reference: Option<&[u8]>,
) -> Result<StagedBinary> {
    asset.validate()?;
    let url = format!(
        "{GITHUB_RELEASE_ROOT}/{repository}/releases/download/{}/{}",
        release.tag, asset.name
    );
    let mut child = Command::new(curl_program)
        .args(CURL_ASSET_ARGUMENTS)
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "could not download BetterCodex release asset {}",
                asset.name
            )
        })?;
    let stdout = child
        .stdout
        .take()
        .context("could not read the BetterCodex release download")?;
    let stage = StagedBinary::new(install_dir);
    let transfer = expand_zstd(stdout, &stage.path, asset, reference);
    if transfer.is_err() {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .context("could not wait for the BetterCodex release download")?;
    transfer?;
    if !status.success() {
        bail!("BetterCodex release download exited with {status}");
    }
    Ok(stage)
}

fn expand_zstd(
    input: impl Read,
    output_path: &Path,
    asset: &ReleaseAsset,
    reference: Option<&[u8]>,
) -> Result<()> {
    let input = BufReader::new(HashedReader::new(input, asset.size));
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output_path)
        .with_context(|| format!("could not stage BetterCodex at {}", output_path.display()))?;
    let output = LimitedWriter::new(output, MAX_BINARY_BYTES);
    let (input, mut output, written) = decode_zstd(input, output, reference)
        .context("BetterCodex release asset could not be decompressed")?;
    output.flush()?;
    if written == 0 {
        bail!("BetterCodex release asset decompressed to an empty executable");
    }
    let input = input.into_inner();
    let (actual_size, actual_digest) = input.finish();
    if actual_size != asset.size {
        bail!(
            "BetterCodex release asset {} contained {actual_size} bytes, expected {}",
            asset.name,
            asset.size
        );
    }
    if actual_digest != asset.sha256 {
        bail!(
            "BetterCodex release asset {} has SHA-256 {actual_digest}, expected {}",
            asset.name,
            asset.sha256
        );
    }
    let mut permissions = output.get_ref().metadata()?.permissions();
    permissions.set_mode(0o755);
    output.get_ref().set_permissions(permissions)?;
    output.get_ref().sync_all()?;
    Ok(())
}

fn decode_zstd<R, W>(
    input: R,
    output: LimitedWriter<W>,
    reference: Option<&[u8]>,
) -> io::Result<(R, LimitedWriter<W>, u64)>
where
    R: BufRead,
    W: Write,
{
    if let Some(reference) = reference {
        let decoder = zstd::stream::read::Decoder::with_ref_prefix(input, reference)?;
        copy_decoded(decoder, output)
    } else {
        let decoder = zstd::stream::read::Decoder::with_buffer(input)?;
        copy_decoded(decoder, output)
    }
}

fn copy_decoded<'a, R, W>(
    mut decoder: zstd::stream::read::Decoder<'a, R>,
    mut output: LimitedWriter<W>,
) -> io::Result<(R, LimitedWriter<W>, u64)>
where
    R: BufRead,
    W: Write,
{
    decoder.window_log_max(MAX_ZSTD_WINDOW_LOG)?;
    io::copy(&mut decoder, &mut output)?;
    let written = output.written;
    Ok((decoder.finish(), output, written))
}

struct HashedReader<R> {
    inner: R,
    hasher: Sha256,
    expected_size: u64,
    bytes_read: u64,
}

impl<R> HashedReader<R> {
    fn new(inner: R, expected_size: u64) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            expected_size,
            bytes_read: 0,
        }
    }

    fn finish(self) -> (u64, String) {
        (self.bytes_read, format!("{:x}", self.hasher.finalize()))
    }
}

impl<R: Read> Read for HashedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let remaining = self.expected_size.saturating_sub(self.bytes_read);
        let maximum = usize::try_from(remaining.saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = self.inner.read(&mut buffer[..maximum])?;
        self.bytes_read = self
            .bytes_read
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("BetterCodex release asset size overflowed"))?;
        self.hasher.update(&buffer[..read]);
        if self.bytes_read > self.expected_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "BetterCodex release asset exceeded its published size",
            ));
        }
        Ok(read)
    }
}

struct LimitedWriter<W> {
    inner: W,
    limit: u64,
    written: u64,
}

impl<W> LimitedWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            limit,
            written: 0,
        }
    }

    fn get_ref(&self) -> &W {
        &self.inner
    }
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.written);
        if buffer.len() as u64 > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "BetterCodex executable exceeds the update size limit",
            ));
        }
        let written = self.inner.write(buffer)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl ReleaseAsset {
    pub(super) fn validate(&self) -> Result<()> {
        if self.size == 0 || self.size > MAX_BINARY_BYTES {
            bail!(
                "published BetterCodex release asset {} has invalid size {}",
                self.name,
                self.size
            );
        }
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            bail!(
                "published BetterCodex release asset {} has no canonical SHA-256 digest",
                self.name
            );
        }
        Ok(())
    }
}

fn verify_candidate(candidate: &Path, release: &PublishedRelease) -> Result<()> {
    let version = command_line(candidate, "--version")?;
    if version != format!("bcodex {}", release.version) {
        bail!(
            "downloaded BetterCodex executable reported {version:?}, expected bcodex {}",
            release.version
        );
    }
    let revision = command_line(candidate, "--internal-source-revision")?;
    if revision != release.revision {
        bail!(
            "downloaded BetterCodex executable reported source revision {revision:?}, expected {}",
            release.revision
        );
    }

    let smoke = SmokeRoot::new()?;
    let output = Command::new(candidate)
        .arg("--internal-install-smoke")
        .current_dir(smoke.path.join("workspace"))
        .env("HOME", smoke.path.join("home"))
        .env("CODEX_HOME", smoke.path.join("codex-home"))
        .env("BCODEX_HOME", smoke.path.join("bcodex-home"))
        .env("BCODEX_SKIP_UPDATE_CHECK", "1")
        .stdin(Stdio::null())
        .output()
        .context("downloaded BetterCodex executable could not run its install smoke test")?;
    if !output.status.success() {
        bail!(
            "downloaded BetterCodex executable failed its install smoke test with {}",
            output.status
        );
    }
    let smoke_output = one_line(&output.stdout)
        .context("downloaded BetterCodex executable returned invalid install smoke output")?;
    let expected = format!("bcodex {} install smoke passed", release.version);
    if smoke_output != expected {
        bail!("downloaded BetterCodex executable returned {smoke_output:?}, expected {expected:?}");
    }
    Ok(())
}

fn command_line(binary: &Path, argument: &str) -> Result<String> {
    let output = Command::new(binary)
        .arg(argument)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("downloaded BetterCodex executable could not run {argument}"))?;
    if !output.status.success() {
        bail!(
            "downloaded BetterCodex executable failed {argument} with {}",
            output.status
        );
    }
    one_line(&output.stdout).with_context(|| {
        format!("downloaded BetterCodex executable returned invalid {argument} output")
    })
}

fn one_line(output: &[u8]) -> Option<String> {
    let output = std::str::from_utf8(output).ok()?;
    let output = output.strip_suffix('\n').unwrap_or(output);
    let output = output.strip_suffix('\r').unwrap_or(output);
    (!output.is_empty() && !output.bytes().any(|byte| matches!(byte, b'\n' | b'\r')))
        .then(|| output.to_string())
}

struct StagedBinary {
    path: PathBuf,
    armed: bool,
}

impl StagedBinary {
    fn new(install_dir: &Path) -> Self {
        Self {
            path: install_dir.join(format!(".bcodex-stage.{}", std::process::id())),
            armed: true,
        }
    }

    fn install(mut self, destination: &Path) -> Result<()> {
        fs::rename(&self.path, destination).with_context(|| {
            format!(
                "could not atomically replace BetterCodex at {}",
                destination.display()
            )
        })?;
        self.armed = false;
        if let Some(parent) = destination.parent() {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .with_context(|| {
                    format!(
                        "could not sync the BetterCodex install directory {}",
                        parent.display()
                    )
                })?;
        }
        Ok(())
    }
}

impl Drop for StagedBinary {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct InstallLock {
    path: PathBuf,
}

impl InstallLock {
    fn acquire(install_dir: &Path) -> Result<Self> {
        let path = install_dir.join(".bcodex-install.lock");
        loop {
            match fs::create_dir(&path) {
                Ok(()) => {
                    if let Err(error) =
                        fs::write(path.join("pid"), format!("{}\n", std::process::id()))
                    {
                        let _ = fs::remove_dir_all(&path);
                        return Err(error)
                            .context("could not record the BetterCodex update lock owner");
                    }
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if lock_is_active(&path)? {
                        bail!("another BetterCodex install or update is already running");
                    }
                    let stale =
                        install_dir.join(format!(".bcodex-stale-lock.{}", std::process::id()));
                    match fs::rename(&path, &stale) {
                        Ok(()) => {
                            if let Err(error) = cleanup_recorded_source_install(&stale) {
                                let _ = fs::rename(&stale, &path);
                                return Err(error).context(
                                    "could not clean the temporary tree from an interrupted BetterCodex source install",
                                );
                            }
                            fs::remove_dir_all(&stale)
                                .context("could not remove a stale BetterCodex update lock")?;
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(error)
                                .context("could not recover a stale BetterCodex update lock");
                        }
                    }
                }
                Err(error) => {
                    return Err(error).context("could not acquire the BetterCodex update lock");
                }
            }
        }
    }
}

fn cleanup_recorded_source_install(stale_lock: &Path) -> Result<()> {
    let Some(recorded_temp) = read_first_line(&stale_lock.join("tmp")) else {
        return Ok(());
    };
    let parent = read_first_line(&stale_lock.join("tmp-parent"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let recorded_temp = PathBuf::from(recorded_temp);
    if !parent.is_absolute()
        || recorded_temp.parent() != Some(parent.as_path())
        || !recorded_temp
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
                name.strip_prefix("bettercodex-install.")
                    .is_some_and(|suffix| !suffix.is_empty())
            })
    {
        return Ok(());
    }
    match fs::symlink_metadata(&recorded_temp) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(&recorded_temp)?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&recorded_temp)?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn read_first_line(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn lock_is_active(path: &Path) -> Result<bool> {
    let pid = fs::read_to_string(path.join("pid"))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    if let Some(pid) = pid {
        return Ok(process_is_alive(pid));
    }
    let modified = fs::metadata(path)?.modified().unwrap_or(SystemTime::now());
    Ok(modified.elapsed().unwrap_or_default() < LOCK_FRESHNESS)
}

fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // Signal 0 performs existence/permission checking without sending a signal.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn cleanup_stale_stages(install_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(install_dir)? {
        let entry = entry?;
        if !entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(b".bcodex-stage.")
        {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_file() || metadata.file_type().is_symlink() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

struct SmokeRoot {
    path: PathBuf,
}

impl SmokeRoot {
    fn new() -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "bettercodex-update-smoke.{}.{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).with_context(|| {
            format!("could not create update smoke directory {}", path.display())
        })?;
        let smoke = Self { path };
        for child in ["home", "codex-home", "bcodex-home", "workspace"] {
            fs::create_dir(smoke.path.join(child)).with_context(|| {
                format!(
                    "could not create update smoke directory {}",
                    smoke.path.display()
                )
            })?;
        }
        Ok(smoke)
    }
}

impl Drop for SmokeRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cleanup_source_updater_caches() -> Result<()> {
    let cache_base = if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(cache)
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".cache")
    } else {
        return Ok(());
    };
    cleanup_source_updater_caches_at(&cache_base)
}

fn cleanup_source_updater_caches_at(cache_base: &Path) -> Result<()> {
    if !cache_base.is_absolute() {
        bail!("refusing to clean a relative BetterCodex cache path");
    }
    if path_is_symlink(cache_base)? {
        eprintln!(
            "bettercodex updater: warning: not removing source-updater caches through symlink {}",
            cache_base.display()
        );
        return Ok(());
    }
    let cache_root = cache_base.join("bettercodex");
    if path_is_symlink(&cache_root)? {
        eprintln!(
            "bettercodex updater: warning: not removing symlinked source-updater cache root {}",
            cache_root.display()
        );
        return Ok(());
    }
    for name in ["build", "cargo", "tmp"] {
        let path = cache_root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                eprintln!(
                    "bettercodex updater: warning: not removing source-updater cache symlink {}",
                    path.display()
                );
            }
            Ok(metadata) if metadata.is_dir() => {
                fs::remove_dir_all(&path).with_context(|| {
                    format!("could not remove source-updater cache {}", path.display())
                })?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not inspect source-updater cache {}", path.display())
                });
            }
        }
    }
    Ok(())
}

fn path_is_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("could not inspect cache path {}", path.display()))
        }
    }
}

fn short_revision(revision: &str) -> &str {
    revision.get(..12).unwrap_or(revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::unix::fs::symlink;

    struct TemporaryDirectory {
        path: PathBuf,
    }

    impl TemporaryDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "bettercodex-update-install-test-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn asset(name: &str, compressed: &[u8]) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_string(),
            size: compressed.len() as u64,
            sha256: format!("{:x}", Sha256::digest(compressed)),
        }
    }

    #[test]
    fn maps_only_supported_native_targets() {
        assert_eq!(
            native_target("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            native_target("linux", "aarch64").unwrap(),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            native_target("macos", "x86_64").unwrap(),
            "x86_64-apple-darwin"
        );
        assert_eq!(
            native_target("macos", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert!(native_target("windows", "x86_64").is_err());
        assert!(native_target("linux", "riscv64").is_err());
    }

    #[test]
    fn validates_release_asset_size_and_digest() {
        let valid = ReleaseAsset {
            name: "bcodex-x86_64-unknown-linux-gnu.zst".to_string(),
            size: 42,
            sha256: "a".repeat(64),
        };
        assert!(valid.validate().is_ok());
        assert!(
            ReleaseAsset {
                size: 0,
                ..valid.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            ReleaseAsset {
                sha256: "A".repeat(64),
                ..valid
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn expands_full_and_previous_binary_zstd_assets() {
        let root = TemporaryDirectory::new();
        let previous = b"#!/bin/sh\nold BetterCodex executable bytes\n";
        let current = b"#!/bin/sh\nnew BetterCodex executable bytes with a little more data\n";

        let full = zstd::stream::encode_all(Cursor::new(current), 3).unwrap();
        let full_output = root.path.join("full");
        expand_zstd(
            Cursor::new(&full),
            &full_output,
            &asset("bcodex-test.zst", &full),
            None,
        )
        .unwrap();
        assert_eq!(fs::read(full_output).unwrap(), current);

        let mut encoder =
            zstd::stream::write::Encoder::with_ref_prefix(Vec::new(), 3, previous).unwrap();
        encoder.write_all(current).unwrap();
        let patch = encoder.finish().unwrap();
        let patch_output = root.path.join("patch");
        expand_zstd(
            Cursor::new(&patch),
            &patch_output,
            &asset("bcodex-test.patch.zst", &patch),
            Some(previous),
        )
        .unwrap();
        assert_eq!(fs::read(patch_output).unwrap(), current);
    }

    #[test]
    fn rejects_wrong_asset_digests() {
        let root = TemporaryDirectory::new();
        let current = b"current executable";
        let full = zstd::stream::encode_all(Cursor::new(current), 3).unwrap();
        let mut wrong_digest = asset("bcodex-test.zst", &full);
        wrong_digest.sha256 = "0".repeat(64);
        assert!(
            expand_zstd(
                Cursor::new(&full),
                &root.path.join("wrong-digest"),
                &wrong_digest,
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("SHA-256")
        );
    }

    #[test]
    fn retries_streams_verifies_and_atomically_installs_a_release_asset() {
        let root = TemporaryDirectory::new();
        let revision = "2222222222222222222222222222222222222222";
        let script = format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
               --version) printf '%s\\n' 'bcodex 0.1.3' ;;\n\
               --internal-source-revision) printf '%s\\n' '{revision}' ;;\n\
               --internal-install-smoke) printf '%s\\n' 'bcodex 0.1.3 install smoke passed' ;;\n\
               *) exit 2 ;;\n\
             esac\n"
        );
        let compressed = zstd::stream::encode_all(Cursor::new(script.as_bytes()), 3).unwrap();
        let compressed_path = root.path.join("release.zst");
        fs::write(&compressed_path, &compressed).unwrap();
        let requested_url = root.path.join("requested-url");
        let attempts_path = root.path.join("attempts");
        let curl = root.path.join("curl");
        fs::write(
            &curl,
            format!(
                "#!/bin/sh\n\
                 url_count=0\n\
                 for argument do\n\
                   case \"$argument\" in https://*) url=\"$argument\"; url_count=$((url_count + 1)) ;; esac\n\
                 done\n\
                 [ \"$url_count\" -eq 1 ] || exit 9\n\
                 printf '%s\\n' \"$url\" >'{}'\n\
                 attempts=0\n\
                 [ ! -f '{}' ] || attempts=\"$(/bin/cat '{}')\"\n\
                 attempts=$((attempts + 1))\n\
                 printf '%s\\n' \"$attempts\" >'{}'\n\
                 if [ \"$attempts\" -eq 1 ]; then printf '%s' truncated; exit 7; fi\n\
                 exec /bin/cat '{}'\n",
                requested_url.display(),
                attempts_path.display(),
                attempts_path.display(),
                attempts_path.display(),
                compressed_path.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&curl).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&curl, permissions).unwrap();

        let release = PublishedRelease {
            tag: format!("bcodex-v0.1.3-{revision}"),
            version: "0.1.3".to_string(),
            revision: revision.to_string(),
            assets: vec![asset("bcodex-x86_64-unknown-linux-gnu.zst", &compressed)],
        };
        let stage = download_verified_candidate(
            curl.as_os_str(),
            "owner/project",
            &release,
            &release.assets[0],
            &root.path,
            None,
        )
        .unwrap();
        let destination = root.path.join("bcodex");
        stage.install(&destination).unwrap();

        assert_eq!(fs::read_to_string(&destination).unwrap(), script);
        assert_eq!(fs::read_to_string(attempts_path).unwrap(), "2\n");
        assert_eq!(
            fs::read_to_string(requested_url).unwrap().trim(),
            format!(
                "https://github.com/owner/project/releases/download/{}/{}",
                release.tag, release.assets[0].name
            )
        );
        assert_eq!(
            fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn rejects_symlink_destinations() {
        let root = TemporaryDirectory::new();
        let destination = root.path.join("bcodex");
        let outside = root.path.join("outside");
        fs::write(&outside, b"operator data").unwrap();
        symlink(&outside, &destination).unwrap();

        assert!(reject_unsafe_destination(&destination).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"operator data");
    }

    #[test]
    fn stale_shell_lock_cleanup_is_narrowly_scoped() {
        let root = TemporaryDirectory::new();
        let stale_lock = root.path.join("stale-lock");
        let orphan = root.path.join("bettercodex-install.orphan");
        let unrelated = root.path.join("unrelated");
        fs::create_dir_all(&stale_lock).unwrap();
        fs::create_dir_all(&orphan).unwrap();
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(stale_lock.join("tmp"), format!("{}\n", orphan.display())).unwrap();
        fs::write(
            stale_lock.join("tmp-parent"),
            format!("{}\n", root.path.display()),
        )
        .unwrap();

        cleanup_recorded_source_install(&stale_lock).unwrap();

        assert!(!orphan.exists());
        assert!(unrelated.is_dir());
    }

    #[test]
    fn cache_cleanup_never_follows_a_linked_root() {
        assert!(cleanup_source_updater_caches_at(Path::new("relative-cache")).is_err());

        let root = TemporaryDirectory::new();
        let outside = root.path.join("outside");
        let linked_cache = root.path.join("linked-cache");
        fs::create_dir_all(outside.join("bettercodex/build")).unwrap();
        fs::write(outside.join("bettercodex/build/operator-data"), b"keep").unwrap();
        symlink(&outside, &linked_cache).unwrap();

        cleanup_source_updater_caches_at(&linked_cache).unwrap();

        assert_eq!(
            fs::read(outside.join("bettercodex/build/operator-data")).unwrap(),
            b"keep"
        );
    }
}
