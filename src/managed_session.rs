//! Process-boundary lifecycle for durable interactive sessions.
//!
//! An outer invocation reserves the first free `cN` tmux name and then replaces itself with the
//! tmux client. The pane command owns the inner bettercodex lifecycle, so tmux keeps it alive across
//! client disconnects and destroys the session as soon as bettercodex exits. A private one-use file
//! transfers the invoking environment to the pane without placing secret values in tmux's argv.

use crate::operator_settings::TmuxMode;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::time::Duration;
use std::time::Instant;
use uuid::Uuid;

const SESSION_PREFIX: char = 'c';
const CAFFEINATE: &str = "/usr/bin/caffeinate";
const CAFFEINATE_MARKER: &str = "BCODEX_CAFFEINATED";
const ENVIRONMENT_MARKER: &str = "BCODEX_MANAGED_ENVIRONMENT";
const ENVIRONMENT_FILE_PREFIX: &str = ".bettercodex-environment-";
const MAX_ENVIRONMENT_BYTES: usize = 8 * 1024 * 1024;
const ENVIRONMENT_HANDOFF_TIMEOUT: Duration = Duration::from_secs(10);

/// Restore the invoking process environment before any threads or agent state are created.
pub(crate) fn restore_environment() -> Result<()> {
    let Some(path) = std::env::var_os(ENVIRONMENT_MARKER).map(PathBuf::from) else {
        return Ok(());
    };
    validate_environment_path(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(&path)
        .with_context(|| format!("failed to open managed environment {}", path.display()))?;
    let metadata = file.metadata()?;
    // SAFETY: `geteuid` has no arguments, memory access, or preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(anyhow!(
            "managed environment {} is not a private regular file owned by this user",
            path.display()
        ));
    }
    std::fs::remove_file(&path)
        .with_context(|| format!("failed to consume managed environment {}", path.display()))?;
    if metadata.len() > MAX_ENVIRONMENT_BYTES as u64 {
        return Err(anyhow!(
            "managed environment exceeds the {MAX_ENVIRONMENT_BYTES}-byte limit"
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_ENVIRONMENT_BYTES.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ENVIRONMENT_BYTES {
        return Err(anyhow!(
            "managed environment exceeds the {MAX_ENVIRONMENT_BYTES}-byte limit"
        ));
    }
    let environment = decode_environment(&bytes)?;
    let inherited_names = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| !is_tmux_runtime_variable(name))
        .collect::<Vec<_>>();

    // SAFETY: `main` calls this before constructing the Tokio runtime or starting any other
    // threads. No other code can read or write the process environment concurrently.
    unsafe {
        for name in inherited_names {
            std::env::remove_var(name);
        }
        for (name, value) in environment {
            std::env::set_var(name, value);
        }
    }
    Ok(())
}

struct EnvironmentSnapshot {
    path: PathBuf,
}

impl EnvironmentSnapshot {
    fn capture() -> Result<Self> {
        let environment = std::env::vars_os()
            .filter(|(name, _)| !is_snapshot_excluded(name))
            .collect::<Vec<_>>();
        let bytes = encode_environment(environment)?;
        let path = std::env::temp_dir().join(format!(
            "{ENVIRONMENT_FILE_PREFIX}{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let write_result = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true).mode(0o600);
            let mut file = options.open(&path).with_context(|| {
                format!("failed to create managed environment {}", path.display())
            })?;
            file.write_all(&bytes)?;
            file.flush()?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&path);
        }
        write_result?;
        Ok(Self { path })
    }

    fn tmux_variable(&self) -> OsString {
        let mut variable = OsString::from(ENVIRONMENT_MARKER);
        variable.push("=");
        variable.push(&self.path);
        variable
    }

    fn wait_until_consumed(&self) -> Result<()> {
        let started = Instant::now();
        loop {
            match std::fs::metadata(&self.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to inspect managed environment {}",
                            self.path.display()
                        )
                    });
                }
                Ok(_) if started.elapsed() >= ENVIRONMENT_HANDOFF_TIMEOUT => {
                    return Err(anyhow!(
                        "tmux pane did not consume its managed environment within {} seconds",
                        ENVIRONMENT_HANDOFF_TIMEOUT.as_secs()
                    ));
                }
                Ok(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

impl Drop for EnvironmentSnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn encode_environment(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Vec<u8>> {
    let mut environment = environment.into_iter().collect::<Vec<_>>();
    environment.sort_unstable();
    let mut bytes = Vec::new();
    for (name, value) in environment {
        let name = name.as_bytes();
        let value = value.as_bytes();
        let entry_bytes = name
            .len()
            .checked_add(value.len())
            .and_then(|length| length.checked_add(2))
            .ok_or_else(|| anyhow!("managed environment size overflowed"))?;
        let new_length = bytes
            .len()
            .checked_add(entry_bytes)
            .ok_or_else(|| anyhow!("managed environment size overflowed"))?;
        if new_length > MAX_ENVIRONMENT_BYTES {
            return Err(anyhow!(
                "managed environment exceeds the {MAX_ENVIRONMENT_BYTES}-byte limit"
            ));
        }
        bytes.reserve(entry_bytes);
        bytes.extend_from_slice(name);
        bytes.push(b'=');
        bytes.extend_from_slice(value);
        bytes.push(0);
    }
    Ok(bytes)
}

fn decode_environment(bytes: &[u8]) -> Result<Vec<(OsString, OsString)>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.ends_with(&[0]) {
        return Err(anyhow!("managed environment has an incomplete final entry"));
    }
    let mut seen = BTreeSet::new();
    bytes[..bytes.len() - 1]
        .split(|byte| *byte == 0)
        .map(|entry| {
            let separator = entry
                .iter()
                .position(|byte| *byte == b'=')
                .filter(|separator| *separator > 0)
                .ok_or_else(|| anyhow!("managed environment contains an invalid entry"))?;
            let name = OsString::from_vec(entry[..separator].to_vec());
            if is_snapshot_excluded(&name) {
                return Err(anyhow!(
                    "managed environment contains reserved variable {}",
                    name.to_string_lossy()
                ));
            }
            if !seen.insert(name.clone()) {
                return Err(anyhow!(
                    "managed environment contains duplicate variable {}",
                    name.to_string_lossy()
                ));
            }
            let value = OsString::from_vec(entry[separator + 1..].to_vec());
            Ok((name, value))
        })
        .collect()
}

fn is_snapshot_excluded(name: &OsStr) -> bool {
    is_tmux_runtime_variable(name) || name == ENVIRONMENT_MARKER
}

fn validate_environment_path(path: &Path) -> Result<()> {
    let valid_name = path.file_name().is_some_and(|name| {
        name.as_bytes()
            .starts_with(ENVIRONMENT_FILE_PREFIX.as_bytes())
    });
    if !path.is_absolute() || !valid_name {
        return Err(anyhow!(
            "managed environment path is not a bettercodex temporary file: {}",
            path.display()
        ));
    }
    Ok(())
}

fn is_tmux_runtime_variable(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("TMUX" | "TMUX_PANE" | "TERM" | "TERMCAP")
    )
}

pub(crate) fn enter(
    arguments: &[String],
    interactive_terminal: bool,
    tmux_mode: TmuxMode,
) -> Result<()> {
    if should_launch_in_tmux(
        interactive_terminal,
        std::env::var_os("TMUX").as_deref(),
        tmux_mode,
    ) {
        launch_in_tmux(arguments)?;
    }
    prevent_macos_idle_sleep(arguments)
}

fn should_launch_in_tmux(
    interactive_terminal: bool,
    tmux_environment: Option<&std::ffi::OsStr>,
    tmux_mode: TmuxMode,
) -> bool {
    interactive_terminal && tmux_environment.is_none() && tmux_mode.is_on()
}

fn launch_in_tmux(arguments: &[String]) -> Result<()> {
    ensure_attachable_terminal(std::env::var_os("TERM").as_deref())?;
    let executable = std::env::current_exe().context("failed to locate the bcodex executable")?;
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    let size = crossterm::terminal::size().ok();
    let environment = EnvironmentSnapshot::capture()?;
    let session_environment = [environment.tmux_variable()];
    let mut occupied = occupied_slots()?;

    loop {
        let slot = first_free_slot(&occupied)?;
        let name = format!("{SESSION_PREFIX}{slot}");
        let create = tmux_create_arguments(
            &name,
            &executable,
            &cwd,
            arguments,
            size,
            &session_environment,
        );
        let output = run_tmux(&create)?;
        if output.status.success() {
            return match tmux_session_id(&output) {
                Ok(session_id) => {
                    if let Err(error) = environment.wait_until_consumed() {
                        kill_session(&session_id);
                        return Err(error);
                    }
                    attach(&session_id)
                }
                Err(error) => {
                    kill_session(&format!("={name}"));
                    Err(error)
                }
            };
        }
        if let Ok(session_id) = tmux_session_id(&output) {
            kill_session(&session_id);
            return Err(tmux_failure(&format!("configure session {name}"), &output));
        }
        if session_exists(&name)? {
            occupied.insert(slot);
            continue;
        }
        return Err(tmux_failure(&format!("create session {name}"), &output));
    }
}

fn ensure_attachable_terminal(term: Option<&std::ffi::OsStr>) -> Result<()> {
    if term.is_none_or(|term| term.is_empty() || term == "dumb") {
        return Err(anyhow!(
            "tmux requires a capable terminal; TERM is missing or set to `dumb`"
        ));
    }
    Ok(())
}

fn attach(session_id: &str) -> Result<()> {
    let result = (|| {
        std::io::stdout().flush()?;
        std::io::stderr().flush()?;
        let mut command = Command::new("tmux");
        command.args(["attach-session", "-t", session_id]);
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        Err(error).context("failed to attach the new tmux session")
    })();
    kill_session(session_id);
    result
}

fn kill_session(target: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", target])
        .output();
}

fn tmux_session_id(output: &Output) -> Result<String> {
    let session_id = std::str::from_utf8(&output.stdout)
        .context("tmux returned a non-UTF-8 session identifier")?;
    let session_id = session_id.trim();
    if session_id
        .strip_prefix('$')
        .is_none_or(|number| number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(anyhow!("tmux did not return the created session ID"));
    }
    Ok(session_id.to_string())
}

fn occupied_slots() -> Result<BTreeSet<u64>> {
    let output = run_tmux(&[
        OsString::from("list-sessions"),
        OsString::from("-F"),
        OsString::from("#{session_name}"),
    ])?;
    if !output.status.success() {
        return Ok(BTreeSet::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(session_slot)
        .collect())
}

fn first_free_slot(occupied: &BTreeSet<u64>) -> Result<u64> {
    let mut slot = 1_u64;
    while occupied.contains(&slot) {
        slot = slot
            .checked_add(1)
            .ok_or_else(|| anyhow!("all bcodex tmux session names are occupied"))?;
    }
    Ok(slot)
}

fn session_slot(name: &str) -> Option<u64> {
    let suffix = name.strip_prefix(SESSION_PREFIX)?;
    if suffix.is_empty()
        || suffix.starts_with('0')
        || !suffix.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    suffix.parse().ok()
}

fn session_exists(name: &str) -> Result<bool> {
    let output = run_tmux(&[
        OsString::from("has-session"),
        OsString::from("-t"),
        OsString::from(format!("={name}")),
    ])?;
    Ok(output.status.success())
}

fn run_tmux(arguments: &[OsString]) -> Result<Output> {
    Command::new("tmux")
        .args(arguments)
        .stdin(std::process::Stdio::inherit())
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                anyhow!("interactive bettercodex sessions require `tmux`; install tmux and retry")
            } else {
                anyhow!(error)
            }
        })
}

fn tmux_create_arguments(
    name: &str,
    executable: &Path,
    cwd: &Path,
    arguments: &[String],
    size: Option<(u16, u16)>,
    environment: &[OsString],
) -> Vec<OsString> {
    let mut tmux = vec![
        "new-session".into(),
        "-d".into(),
        "-P".into(),
        "-F".into(),
        "#{session_id}".into(),
        "-s".into(),
        name.into(),
        "-n".into(),
        "bcodex".into(),
        "-c".into(),
        tmux_literal(cwd.as_os_str()),
    ];
    if let Some((columns, rows)) = size {
        tmux.extend([
            "-x".into(),
            columns.max(1).to_string().into(),
            "-y".into(),
            rows.max(1).to_string().into(),
        ]);
    }
    for variable in environment {
        tmux.extend(["-e".into(), tmux_literal(variable)]);
    }
    tmux.push("--".into());
    tmux.push(tmux_literal(executable.as_os_str()));
    tmux.extend(
        arguments
            .iter()
            .map(|argument| tmux_literal(argument.as_ref())),
    );
    tmux.extend([
        ";".into(),
        "set-option".into(),
        "-t".into(),
        name.into(),
        "destroy-unattached".into(),
        "off".into(),
        ";".into(),
        "set-option".into(),
        "-t".into(),
        name.into(),
        "detach-on-destroy".into(),
        "on".into(),
        ";".into(),
        "set-window-option".into(),
        "-t".into(),
        name.into(),
        "remain-on-exit".into(),
        "off".into(),
    ]);
    tmux
}

/// tmux's argv parser uses an unescaped trailing semicolon as a command separator. Inserting one
/// backslash before it is tmux's literal representation; the parser removes that extra backslash
/// before passing the value to the pane command.
fn tmux_literal(value: &std::ffi::OsStr) -> OsString {
    let bytes = value.as_bytes();
    if !bytes.ends_with(b";") {
        return value.to_os_string();
    }
    let mut escaped = Vec::with_capacity(bytes.len() + 1);
    escaped.extend_from_slice(&bytes[..bytes.len() - 1]);
    escaped.extend_from_slice(b"\\;");
    OsString::from_vec(escaped)
}

fn tmux_failure(action: &str, output: &Output) -> anyhow::Error {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        anyhow!("tmux could not {action} ({})", output.status)
    } else {
        anyhow!("tmux could not {action}: {detail}")
    }
}

fn prevent_macos_idle_sleep(arguments: &[String]) -> Result<()> {
    if !cfg!(target_os = "macos")
        || std::env::var_os(CAFFEINATE_MARKER).is_some()
        || !Path::new(CAFFEINATE).is_file()
    {
        return Ok(());
    }
    let executable = std::env::current_exe().context("failed to locate the bcodex executable")?;
    let mut command = caffeinate_command(&executable, arguments);
    use std::os::unix::process::CommandExt;
    let error = command.exec();
    Err(error).context("failed to run bcodex under macOS caffeinate")
}

fn caffeinate_command(executable: &Path, arguments: &[String]) -> Command {
    let mut command = Command::new(CAFFEINATE);
    command
        .arg("-i")
        .arg("-s")
        .arg(executable)
        .args(arguments)
        .env(CAFFEINATE_MARKER, "1");
    command
}

#[cfg(test)]
#[path = "managed_session_tests.rs"]
mod tests;
