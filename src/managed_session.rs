//! Process-boundary lifecycle for durable interactive sessions.
//!
//! An outer invocation reserves the first free `cN` tmux name and then replaces itself with the
//! tmux client. The pane command owns the inner bettercodex lifecycle, so tmux keeps it alive across
//! client disconnects and destroys the session as soon as bettercodex exits.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::process::Command;
use std::process::Output;

const SESSION_PREFIX: char = 'c';
const CAFFEINATE: &str = "/usr/bin/caffeinate";
const CAFFEINATE_MARKER: &str = "BCODEX_CAFFEINATED";

pub(crate) fn enter(arguments: &[String], interactive_terminal: bool) -> Result<()> {
    if interactive_terminal && std::env::var_os("TMUX").is_none() {
        launch_in_tmux(arguments)?;
    }
    prevent_macos_idle_sleep(arguments)
}

fn launch_in_tmux(arguments: &[String]) -> Result<()> {
    ensure_attachable_terminal(std::env::var_os("TERM").as_deref())?;
    let executable = std::env::current_exe().context("failed to locate the bcodex executable")?;
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    let size = crossterm::terminal::size().ok();
    let environment = tmux_environment();
    let mut occupied = occupied_slots()?;

    loop {
        let slot = first_free_slot(&occupied)?;
        let name = format!("{SESSION_PREFIX}{slot}");
        let create = tmux_create_arguments(&name, &executable, &cwd, arguments, size, &environment);
        let output = run_tmux(&create)?;
        if output.status.success() {
            return match tmux_session_id(&output) {
                Ok(session_id) => attach(&session_id),
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

fn tmux_environment() -> Vec<OsString> {
    let mut environment = std::env::vars_os()
        .filter(|(name, _)| {
            !matches!(
                name.to_str(),
                Some("TMUX" | "TMUX_PANE" | "TERM" | "TERMCAP")
            )
        })
        .map(|(mut name, value)| {
            name.push("=");
            name.push(value);
            name
        })
        .collect::<Vec<_>>();
    environment.sort_unstable();
    environment
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
