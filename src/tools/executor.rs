//! Single-environment adaptation of Codex unified exec at
//! `1669c2403f793d0230065397dfc25f52b844244e`.
//!
//! BetterCodex removes Codex's sandbox, approval, remote-environment, hook,
//! and telemetry layers, while retaining its model-visible process/session,
//! PTY, yield, environment, and output-truncation behavior.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_shell_command::shell_detect::DetectedShell;
use codex_shell_command::shell_detect::ShellType;
use codex_shell_command::shell_detect::default_user_shell;
use codex_shell_command::shell_detect::get_shell_by_model_provided_path;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_tokens_from_byte_count;
use codex_utils_output_truncation::formatted_truncate_text;
use codex_utils_output_truncation::truncate_text;
use portable_pty::ChildKiller;
use portable_pty::CommandBuilder;
use portable_pty::PtySize;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DEFAULT_EXEC_YIELD: Duration = Duration::from_secs(10);
const MIN_EXEC_YIELD: Duration = Duration::from_millis(250);
const MAX_EXEC_YIELD: Duration = Duration::from_secs(30);
const DEFAULT_WRITE_YIELD: Duration = Duration::from_millis(250);
const DEFAULT_POLL_YIELD: Duration = Duration::from_secs(5);
const MAX_POLL_YIELD: Duration = Duration::from_secs(5 * 60);
const MAX_PROCESSES: usize = 64;
const RETAINED_HEAD_BYTES: usize = 512 * 1024;
const RETAINED_TAIL_BYTES: usize = 512 * 1024;
const UNIFIED_EXEC_ENV: [(&str, &str); 10] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("CODEX_CI", "1"),
];

pub(super) struct ProcessManager {
    cwd: PathBuf,
    active_sessions: AtomicUsize,
    sessions: Mutex<HashMap<i32, Arc<ProcessSession>>>,
    reserved_session_ids: Mutex<HashSet<i32>>,
}

impl ProcessManager {
    pub(super) fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            active_sessions: AtomicUsize::new(0),
            sessions: Mutex::new(HashMap::new()),
            reserved_session_ids: Mutex::new(HashSet::new()),
        }
    }

    pub(super) async fn exec_command(
        &self,
        input: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        let arguments: ExecCommandArgs = serde_json::from_value(input)
            .map_err(|error| anyhow!("failed to parse function arguments: {error}"))?;

        let workdir = resolve_workdir(&self.cwd, arguments.workdir.as_deref());
        let shell = arguments
            .shell
            .as_ref()
            .map_or_else(default_user_shell, |shell| {
                get_shell_by_model_provided_path(&PathBuf::from(shell))
            });
        let login = arguments.login.unwrap_or(true);
        let reservation = self.reserve_session()?;
        let session_id_reservation = self.reserve_session_id()?;
        let session_id = session_id_reservation.session_id;
        let session = if arguments.tty.unwrap_or(false) {
            spawn_pty(&shell, login, &arguments.cmd, &workdir)?
        } else {
            spawn_piped(&shell, login, &arguments.cmd, &workdir)?
        };
        self.sessions
            .lock()
            .map_err(|_| anyhow!("process table lock was poisoned"))?
            .insert(session_id, Arc::clone(&session));
        session_id_reservation.commit();
        reservation.commit();

        let yield_time = bounded_duration(
            arguments.yield_time_ms,
            DEFAULT_EXEC_YIELD,
            MIN_EXEC_YIELD,
            MAX_EXEC_YIELD,
        );
        let started = Instant::now();
        tokio::select! {
            _ = cancellation.cancelled() => {
                session.kill();
                self.remove_session(session_id)?;
                return Err(anyhow!("exec_command was interrupted"));
            }
            _ = session.wait(yield_time) => {}
        }
        self.output(
            session_id,
            session,
            started.elapsed(),
            arguments.max_output_tokens,
        )
    }

    pub(super) async fn write_stdin(
        &self,
        input: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        let arguments: WriteStdinArgs = serde_json::from_value(input)
            .map_err(|error| anyhow!("failed to parse function arguments: {error}"))?;
        let session = self
            .sessions
            .lock()
            .map_err(|_| anyhow!("process table lock was poisoned"))?
            .get(&arguments.session_id)
            .cloned()
            .ok_or_else(|| anyhow!("Unknown process id {}", arguments.session_id))?;
        let _interaction = session.interaction.lock().await;

        let has_input = !arguments.chars.is_empty();
        if has_input {
            if !session.tty {
                if arguments.chars == "\u{3}" {
                    session.interrupt()?;
                } else {
                    return Err(anyhow!(
                        "stdin is closed for this session; rerun exec_command with tty=true to keep stdin open"
                    ));
                }
            } else {
                session.write(arguments.chars.into_bytes()).await?;
                // Codex gives the process a brief chance to react before the
                // response collection window starts.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let yield_time = if has_input {
            bounded_duration(
                arguments.yield_time_ms,
                DEFAULT_WRITE_YIELD,
                MIN_EXEC_YIELD,
                MAX_EXEC_YIELD,
            )
        } else {
            bounded_duration(
                arguments.yield_time_ms,
                DEFAULT_POLL_YIELD,
                DEFAULT_POLL_YIELD,
                MAX_POLL_YIELD,
            )
        };
        let started = Instant::now();
        tokio::select! {
            _ = cancellation.cancelled() => {
                session.kill();
                self.remove_session(arguments.session_id)?;
                return Err(anyhow!("write_stdin was interrupted"));
            }
            _ = session.wait(yield_time) => {}
        }
        self.output(
            arguments.session_id,
            Arc::clone(&session),
            started.elapsed(),
            arguments.max_output_tokens,
        )
    }

    fn output(
        &self,
        session_id: i32,
        session: Arc<ProcessSession>,
        wall_time: Duration,
        max_output_tokens: Option<usize>,
    ) -> Result<Value> {
        let snapshot = session.snapshot()?;
        if snapshot.finished {
            self.remove_session(session_id)?;
        }
        let original_token_count =
            usize::try_from(approx_tokens_from_byte_count(snapshot.total_bytes))
                .unwrap_or(usize::MAX);
        let output = truncate_output(
            &snapshot.output,
            max_output_tokens,
            original_token_count,
            snapshot.omitted_bytes,
        );
        let mut result = json!({
            "chunk_id": chunk_id(),
            "wall_time_seconds": wall_time.as_secs_f64(),
            "output": output,
        });
        if let Some(exit_code) = snapshot.exit_code {
            result["exit_code"] = json!(exit_code);
        }
        if !snapshot.finished {
            result["session_id"] = json!(session_id);
        }
        result["original_token_count"] = json!(original_token_count);
        Ok(result)
    }

    fn reserve_session_id(&self) -> Result<SessionIdReservation<'_>> {
        let mut reserved_session_ids = self
            .reserved_session_ids
            .lock()
            .map_err(|_| anyhow!("process ID table lock was poisoned"))?;
        loop {
            let bytes = *Uuid::new_v4().as_bytes();
            let random = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let candidate = 1_000 + (random % 99_000) as i32;
            if reserved_session_ids.insert(candidate) {
                return Ok(SessionIdReservation {
                    reserved_session_ids: &self.reserved_session_ids,
                    session_id: candidate,
                    committed: false,
                });
            }
        }
    }

    fn reserve_session(&self) -> Result<SessionReservation<'_>> {
        self.active_sessions
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_PROCESSES).then_some(active + 1)
            })
            .map_err(|_| anyhow!("exec_command process limit of {MAX_PROCESSES} was reached"))?;
        Ok(SessionReservation {
            active_sessions: &self.active_sessions,
            committed: false,
        })
    }

    fn remove_session(&self, session_id: i32) -> Result<()> {
        let removed = self
            .sessions
            .lock()
            .map_err(|_| anyhow!("process table lock was poisoned"))?
            .remove(&session_id)
            .is_some();
        self.reserved_session_ids
            .lock()
            .map_err(|_| anyhow!("process ID table lock was poisoned"))?
            .remove(&session_id);
        if removed {
            self.active_sessions.fetch_sub(1, Ordering::AcqRel);
        }
        Ok(())
    }
}

struct SessionIdReservation<'a> {
    reserved_session_ids: &'a Mutex<HashSet<i32>>,
    session_id: i32,
    committed: bool,
}

impl SessionIdReservation<'_> {
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SessionIdReservation<'_> {
    fn drop(&mut self) {
        if !self.committed
            && let Ok(mut reserved_session_ids) = self.reserved_session_ids.lock()
        {
            reserved_session_ids.remove(&self.session_id);
        }
    }
}

struct SessionReservation<'a> {
    active_sessions: &'a AtomicUsize,
    committed: bool,
}

impl SessionReservation<'_> {
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SessionReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.active_sessions.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        let Ok(sessions) = self.sessions.get_mut() else {
            return;
        };
        for session in sessions.values() {
            session.kill();
        }
    }
}

#[derive(Deserialize)]
struct ExecCommandArgs {
    cmd: String,
    workdir: Option<String>,
    shell: Option<String>,
    login: Option<bool>,
    tty: Option<bool>,
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
}

#[derive(Deserialize)]
struct WriteStdinArgs {
    session_id: i32,
    #[serde(default)]
    chars: String,
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
}

struct ProcessSession {
    state: Mutex<ProcessState>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    notify: Notify,
    interaction: AsyncMutex<()>,
    tty: bool,
    process_group_id: Option<i32>,
}

impl ProcessSession {
    fn new(
        writer: Option<Box<dyn Write + Send>>,
        killer: Box<dyn ChildKiller + Send + Sync>,
        readers: usize,
        tty: bool,
        process_group_id: Option<i32>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ProcessState {
                output: PendingOutput::default(),
                exit_code: None,
                readers,
                errors: Vec::new(),
            }),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            notify: Notify::new(),
            interaction: AsyncMutex::new(()),
            tty,
            process_group_id,
        })
    }

    fn append(&self, bytes: &[u8]) {
        if let Ok(mut state) = self.state.lock() {
            state.output.append(bytes);
        }
        self.notify.notify_waiters();
    }

    fn reader_finished(&self, error: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.readers = state.readers.saturating_sub(1);
            if let Some(error) = error {
                state.errors.push(error);
            }
        }
        self.notify.notify_waiters();
    }

    fn exited(&self, exit_code: i32, error: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.exit_code = Some(exit_code);
            if let Some(error) = error {
                state.errors.push(error);
            }
        }
        if let Ok(mut writer) = self.writer.lock() {
            *writer = None;
        }
        self.notify.notify_waiters();
    }

    async fn wait(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        loop {
            let notified = self.notify.notified();
            let done = self.state.lock().is_ok_and(|state| state.finished());
            if done || Instant::now() >= deadline {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if timeout(remaining, notified).await.is_err() {
                return;
            }
        }
    }

    async fn write(&self, bytes: Vec<u8>) -> Result<()> {
        let writer = self
            .writer
            .lock()
            .map_err(|_| anyhow!("process stdin lock was poisoned"))?
            .take()
            .ok_or_else(|| anyhow!("stdin is closed"))?;
        let result = tokio::task::spawn_blocking(move || {
            let mut writer = writer;
            writer.write_all(&bytes)?;
            writer.flush()?;
            Ok::<_, std::io::Error>(writer)
        })
        .await
        .context("stdin writer task failed")?;
        match result {
            Ok(writer) => {
                *self
                    .writer
                    .lock()
                    .map_err(|_| anyhow!("process stdin lock was poisoned"))? = Some(writer);
                Ok(())
            }
            Err(error) => Err(error).context("failed to write process stdin"),
        }
    }

    fn interrupt(&self) -> Result<()> {
        if self.signal_process_group(libc::SIGINT) {
            Ok(())
        } else {
            Err(anyhow!("failed to interrupt process"))
        }
    }

    fn kill(&self) {
        if self
            .state
            .lock()
            .is_ok_and(|state| state.exit_code.is_some())
        {
            return;
        }
        if self.signal_process_group(libc::SIGKILL) {
            return;
        }
        if let Ok(mut killer) = self.killer.lock() {
            let _ = killer.kill();
        }
    }

    fn signal_process_group(&self, signal: i32) -> bool {
        let Some(process_group_id) = self.process_group_id else {
            return false;
        };
        // Both the piped and PTY launch paths create a new process group whose
        // leader is the spawned shell, matching Codex's whole-command signals.
        unsafe { libc::kill(-process_group_id, signal) == 0 }
    }

    fn snapshot(&self) -> Result<ProcessSnapshot> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("process output lock was poisoned"))?;
        let mut output = state.output.take();
        if !state.errors.is_empty() {
            if !output.text.is_empty() && !output.text.ends_with('\n') {
                output.text.push('\n');
            }
            let errors = state.errors.join("\n");
            output.total_bytes = output.total_bytes.saturating_add(errors.len());
            output.text.push_str(&errors);
            state.errors.clear();
        }
        Ok(ProcessSnapshot {
            output: output.text,
            exit_code: state.exit_code,
            finished: state.finished(),
            total_bytes: output.total_bytes,
            omitted_bytes: output.omitted_bytes,
        })
    }
}

struct ProcessState {
    output: PendingOutput,
    exit_code: Option<i32>,
    readers: usize,
    errors: Vec<String>,
}

impl ProcessState {
    fn finished(&self) -> bool {
        self.exit_code.is_some() && self.readers == 0
    }
}

struct ProcessSnapshot {
    output: String,
    exit_code: Option<i32>,
    finished: bool,
    total_bytes: usize,
    omitted_bytes: usize,
}

#[derive(Default)]
struct PendingOutput {
    head: Vec<u8>,
    tail: Vec<u8>,
    omitted: usize,
}

impl PendingOutput {
    fn append(&mut self, mut bytes: &[u8]) {
        let head_space = RETAINED_HEAD_BYTES.saturating_sub(self.head.len());
        let head_bytes = head_space.min(bytes.len());
        self.head.extend_from_slice(&bytes[..head_bytes]);
        bytes = &bytes[head_bytes..];
        if bytes.is_empty() {
            return;
        }

        if bytes.len() >= RETAINED_TAIL_BYTES {
            self.omitted += self.tail.len() + bytes.len() - RETAINED_TAIL_BYTES;
            self.tail.clear();
            self.tail
                .extend_from_slice(&bytes[bytes.len() - RETAINED_TAIL_BYTES..]);
            return;
        }
        let excess = (self.tail.len() + bytes.len()).saturating_sub(RETAINED_TAIL_BYTES);
        if excess > 0 {
            self.tail.drain(..excess);
            self.omitted += excess;
        }
        self.tail.extend_from_slice(bytes);
    }

    fn take(&mut self) -> PendingOutputSnapshot {
        let omitted_bytes = self.omitted;
        let total_bytes = self
            .head
            .len()
            .saturating_add(self.tail.len())
            .saturating_add(omitted_bytes);
        let mut bytes = std::mem::take(&mut self.head);
        if omitted_bytes > 0 {
            bytes
                .extend_from_slice(format!("\n... {omitted_bytes} bytes omitted ...\n").as_bytes());
        }
        bytes.extend_from_slice(&std::mem::take(&mut self.tail));
        self.omitted = 0;
        PendingOutputSnapshot {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            total_bytes,
            omitted_bytes,
        }
    }
}

struct PendingOutputSnapshot {
    text: String,
    total_bytes: usize,
    omitted_bytes: usize,
}

fn spawn_piped(
    shell: &DetectedShell,
    login: bool,
    cmd: &str,
    cwd: &Path,
) -> Result<Arc<ProcessSession>> {
    let (program, arguments) = shell_command(shell, login, cmd);
    let mut command = std::process::Command::new(program);
    use std::os::unix::process::CommandExt;
    command
        .args(arguments)
        .current_dir(cwd)
        .envs(UNIFIED_EXEC_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start command in {}", cwd.display()))?;
    let stdout = child.stdout.take().context("failed to capture stdout")?;
    let stderr = child.stderr.take().context("failed to capture stderr")?;
    let process_group_id = i32::try_from(child.id()).ok();
    let killer = child.clone_killer();
    let session = ProcessSession::new(None, killer, 2, false, process_group_id);
    spawn_reader(Arc::clone(&session), stdout, "stdout");
    spawn_reader(Arc::clone(&session), stderr, "stderr");
    let waiter = Arc::clone(&session);
    std::thread::spawn(move || match child.wait() {
        Ok(status) => waiter.exited(
            status
                .code()
                .unwrap_or_else(|| if status.success() { 0 } else { 1 }),
            None,
        ),
        Err(error) => waiter.exited(1, Some(format!("failed to wait for command: {error}"))),
    });
    Ok(session)
}

fn spawn_pty(
    shell: &DetectedShell,
    login: bool,
    cmd: &str,
    cwd: &Path,
) -> Result<Arc<ProcessSession>> {
    let pair = portable_pty::native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("failed to open PTY")?;
    let (program, arguments) = shell_command(shell, login, cmd);
    let mut command = CommandBuilder::new(program);
    for argument in arguments {
        command.arg(argument);
    }
    command.cwd(cwd);
    for (key, value) in UNIFIED_EXEC_ENV {
        command.env(key, value);
    }
    let mut child = pair
        .slave
        .spawn_command(command)
        .with_context(|| format!("failed to start PTY command in {}", cwd.display()))?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .context("failed to capture PTY output")?;
    let writer = pair
        .master
        .take_writer()
        .context("failed to capture PTY input")?;
    let process_group_id = child.process_id().and_then(|id| i32::try_from(id).ok());
    let killer = child.clone_killer();
    let session = ProcessSession::new(Some(writer), killer, 1, true, process_group_id);
    spawn_reader(Arc::clone(&session), reader, "PTY");
    let waiter = Arc::clone(&session);
    std::thread::spawn(move || match child.wait() {
        Ok(status) => waiter.exited(status.exit_code() as i32, None),
        Err(error) => waiter.exited(1, Some(format!("failed to wait for PTY command: {error}"))),
    });
    Ok(session)
}

fn spawn_reader(
    session: Arc<ProcessSession>,
    mut reader: impl Read + Send + 'static,
    label: &'static str,
) {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        let error = loop {
            match reader.read(&mut buffer) {
                Ok(0) => break None,
                Ok(read) => session.append(&buffer[..read]),
                Err(error) => break Some(format!("failed to read {label}: {error}")),
            }
        };
        session.reader_finished(error);
    });
}

fn resolve_workdir(cwd: &Path, requested: Option<&str>) -> PathBuf {
    requested
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map_or_else(|| cwd.to_path_buf(), |path| cwd.join(path))
}

fn shell_command(shell: &DetectedShell, login: bool, cmd: &str) -> (PathBuf, Vec<String>) {
    let arguments = match shell.shell_type {
        ShellType::Zsh | ShellType::Bash | ShellType::Sh => vec![
            if login { "-lc" } else { "-c" }.to_string(),
            cmd.to_string(),
        ],
        ShellType::PowerShell => {
            let mut arguments = Vec::new();
            if !login {
                arguments.push("-NoProfile".to_string());
            }
            arguments.push("-Command".to_string());
            arguments.push(cmd.to_string());
            arguments
        }
        ShellType::Cmd => vec!["/c".to_string(), cmd.to_string()],
    };
    (shell.shell_path.clone(), arguments)
}

pub(crate) fn command_argv_for_display(input: &Value) -> Vec<String> {
    let shell = input
        .get("shell")
        .and_then(Value::as_str)
        .map_or_else(default_user_shell, |shell| {
            get_shell_by_model_provided_path(&PathBuf::from(shell))
        });
    let login = input.get("login").and_then(Value::as_bool).unwrap_or(true);
    let cmd = input.get("cmd").and_then(Value::as_str).unwrap_or_default();
    let (program, arguments) = shell_command(&shell, login, cmd);
    std::iter::once(program.to_string_lossy().into_owned())
        .chain(arguments)
        .collect()
}

fn bounded_duration(
    milliseconds: Option<u64>,
    default: Duration,
    minimum: Duration,
    maximum: Duration,
) -> Duration {
    milliseconds
        .map(Duration::from_millis)
        .unwrap_or(default)
        .clamp(minimum, maximum)
}

fn chunk_id() -> String {
    Uuid::new_v4().simple().to_string()[..6].to_string()
}

fn truncate_output(
    output: &str,
    max_tokens: Option<usize>,
    original_token_count: usize,
    omitted_bytes: usize,
) -> String {
    let Some(max_tokens) = max_tokens else {
        return output.to_string();
    };
    let policy = TruncationPolicy::Tokens(max_tokens);
    if omitted_bytes == 0 {
        return formatted_truncate_text(output, policy);
    }
    if output.len() <= policy.byte_budget() {
        return output.to_string();
    }

    let marker = format!("... {omitted_bytes} bytes omitted ...");
    let omission_notice = if output.contains(&marker) {
        String::new()
    } else {
        format!("{marker}\n")
    };
    format!(
        "Warning: truncated output (original token count: {original_token_count})\n{omission_notice}\n{}",
        truncate_text(output, policy),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_output_keeps_head_and_tail() {
        let mut output = PendingOutput::default();
        let bytes = vec![b'x'; RETAINED_HEAD_BYTES + RETAINED_TAIL_BYTES + 17];
        output.append(&bytes);
        let snapshot = output.take();
        let rendered = snapshot.text;
        assert_eq!(snapshot.total_bytes, bytes.len());
        assert_eq!(snapshot.omitted_bytes, 17);
        assert!(rendered.starts_with(&"x".repeat(RETAINED_HEAD_BYTES)));
        assert!(rendered.contains("17 bytes omitted"));
        assert!(rendered.ends_with(&"x".repeat(RETAINED_TAIL_BYTES)));
    }

    #[test]
    fn process_reservations_are_bounded() {
        let manager = ProcessManager::new(std::env::current_dir().unwrap());
        let reservations = (0..MAX_PROCESSES)
            .map(|_| manager.reserve_session().unwrap())
            .collect::<Vec<_>>();
        assert!(manager.reserve_session().is_err());
        drop(reservations);
        assert!(manager.reserve_session().is_ok());
    }

    #[test]
    fn chunk_ids_match_codex_shape() {
        let id = chunk_id();
        assert_eq!(id.len(), 6);
        assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn display_argv_uses_the_same_shell_resolution_as_execution() {
        assert_eq!(
            command_argv_for_display(&json!({
                "cmd": "printf ready",
                "shell": "/bin/bash",
                "login": false,
            })),
            ["/bin/bash", "-c", "printf ready"],
        );
    }

    #[tokio::test]
    async fn command_can_continue_through_write_stdin() {
        let cwd = std::env::current_dir().unwrap();
        let manager = ProcessManager::new(cwd);
        let started = manager
            .exec_command(
                json!({
                    "cmd": "read line; printf 'got:%s' \"$line\"",
                    "tty": true,
                    "yield_time_ms": 250,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let session_id = started["session_id"].as_i64().unwrap() as i32;
        let completed = manager
            .write_stdin(
                json!({
                    "session_id": session_id,
                    "chars": "hello\n",
                    "yield_time_ms": 1000,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(completed["exit_code"], 0);
        assert!(
            completed["output"]
                .as_str()
                .is_some_and(|output| output.contains("got:hello"))
        );
        assert!(completed["original_token_count"].as_u64().is_some());
        assert!(completed.get("session_id").is_none());
    }

    #[tokio::test]
    async fn non_tty_commands_match_codex_closed_stdin_behavior() {
        let cwd = std::env::current_dir().unwrap();
        let manager = ProcessManager::new(cwd);
        let started = manager
            .exec_command(
                json!({
                    "cmd": "sleep 2",
                    "yield_time_ms": 250,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let session_id = started["session_id"].as_i64().unwrap() as i32;
        let error = manager
            .write_stdin(
                json!({
                    "session_id": session_id,
                    "chars": "input\n",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "stdin is closed for this session; rerun exec_command with tty=true to keep stdin open",
        );
    }

    #[tokio::test]
    async fn tty_commands_receive_a_terminal() {
        let cwd = std::env::current_dir().unwrap();
        let manager = ProcessManager::new(cwd);
        let output = manager
            .exec_command(
                json!({
                    "cmd": "test -t 1 && printf tty",
                    "tty": true,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output["exit_code"], 0);
        assert!(output["output"].as_str().unwrap().contains("tty"));
    }

    #[test]
    fn explicit_output_budget_matches_codex_but_default_keeps_collected_output() {
        let text = "x".repeat(80);
        assert_eq!(truncate_output(&text, None, 20, 0), text);
        let truncated = truncate_output(&text, Some(5), 20, 0);
        assert!(truncated.starts_with("Warning: truncated output"));
    }

    #[test]
    fn empty_poll_uses_codex_five_second_floor() {
        assert_eq!(
            bounded_duration(
                Some(0),
                DEFAULT_POLL_YIELD,
                DEFAULT_POLL_YIELD,
                MAX_POLL_YIELD,
            ),
            DEFAULT_POLL_YIELD,
        );
    }
}
