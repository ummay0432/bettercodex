//! Low-level process spawning, lifecycle, and bounded output retention for unified exec.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_shell_command::shell_detect::DetectedShell;
use codex_shell_command::shell_detect::ShellType;
use portable_pty::ChildKiller;
use portable_pty::CommandBuilder;
use portable_pty::PtySize;
use std::collections::VecDeque;
use std::io::Read;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::time::timeout;

const POST_EXIT_CLOSE_WAIT_CAP: Duration = Duration::from_millis(50);
const PROCESS_READ_BUFFER_BYTES: usize = 64 * 1024;
const PROCESS_STDIN_QUEUE_CAPACITY: usize = 8;
pub(super) const RETAINED_HEAD_BYTES: usize = 512 * 1024;
pub(super) const RETAINED_TAIL_BYTES: usize = 512 * 1024;
const UNIFIED_EXEC_ENV: [(&str, &str); 9] = [
    ("NO_COLOR", "1"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("CODEX_CI", "1"),
];

#[derive(Clone, Copy)]
pub(super) enum ProcessMode {
    Piped,
    Pty,
}

#[derive(Clone, Copy)]
pub(super) enum ShellStartup {
    Login,
    NonLogin,
}

pub(super) struct ProcessSession {
    state: Mutex<ProcessState>,
    writer: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    killer: Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>,
    notify: Notify,
    interaction: Arc<AsyncMutex<()>>,
    mode: ProcessMode,
    process_group_id: Option<i32>,
}

impl ProcessSession {
    pub(super) fn spawn(
        shell: &DetectedShell,
        shell_startup: ShellStartup,
        command: &str,
        cwd: &Path,
        mode: ProcessMode,
    ) -> Result<Arc<Self>> {
        match mode {
            ProcessMode::Piped => spawn_piped(shell, shell_startup, command, cwd),
            ProcessMode::Pty => spawn_pty(shell, shell_startup, command, cwd),
        }
    }

    pub(super) fn new(
        writer: Option<mpsc::Sender<Vec<u8>>>,
        killer: Option<Box<dyn ChildKiller + Send + Sync>>,
        readers: usize,
        mode: ProcessMode,
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
            interaction: Arc::new(AsyncMutex::new(())),
            mode,
            process_group_id,
        })
    }

    pub(super) fn interaction(&self) -> Arc<AsyncMutex<()>> {
        Arc::clone(&self.interaction)
    }

    pub(super) fn has_terminal(&self) -> bool {
        matches!(self.mode, ProcessMode::Pty)
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

    pub(super) fn exited(&self, exit_code: i32, error: Option<String>) {
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

    pub(super) async fn wait(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        let mut post_exit_deadline = None;
        loop {
            let notified = self.notify.notified();
            let (exited, output_closed) = self
                .state
                .lock()
                .map(|state| (state.exit_code.is_some(), state.readers == 0))
                .unwrap_or((true, true));
            if exited && output_closed {
                return;
            }
            let now = Instant::now();
            let wait_deadline = if exited {
                *post_exit_deadline.get_or_insert_with(|| {
                    now + deadline
                        .saturating_duration_since(now)
                        .min(POST_EXIT_CLOSE_WAIT_CAP)
                })
            } else {
                deadline
            };
            if now >= wait_deadline {
                return;
            }
            let remaining = wait_deadline.saturating_duration_since(now);
            if timeout(remaining, notified).await.is_err() {
                return;
            }
        }
    }

    pub(super) async fn write(&self, bytes: Vec<u8>) -> Result<()> {
        let writer = self
            .writer
            .lock()
            .map_err(|_| anyhow!("process stdin lock was poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("stdin is closed"))?;
        writer
            .send(bytes)
            .await
            .map_err(|_| anyhow!("stdin is closed"))
    }

    pub(super) fn interrupt(&self) -> Result<()> {
        if self.signal_process_group(libc::SIGINT) {
            Ok(())
        } else {
            Err(anyhow!("failed to interrupt process"))
        }
    }

    pub(super) fn kill(&self) {
        if self
            .state
            .lock()
            .is_ok_and(|state| state.exit_code.is_some())
        {
            return;
        }
        let _ = self.signal_process_group(libc::SIGKILL);
        // PTY children also retain a direct killer. Current Codex invokes it even after the group
        // signal in case the cached process-group identifier became stale.
        if let Ok(mut killer) = self.killer.lock()
            && let Some(killer) = killer.as_mut()
        {
            let _ = killer.kill();
        }
    }

    pub(super) fn has_exited(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.exit_code.is_some())
    }

    fn signal_process_group(&self, signal: i32) -> bool {
        let Some(process_group_id) = self.process_group_id else {
            return false;
        };
        // Both launch paths create a new process group whose leader is the spawned shell, matching
        // Codex's whole-command signals.
        #[cfg(target_os = "macos")]
        if matches!(self.mode, ProcessMode::Piped) && signal == libc::SIGKILL {
            return signal_process_group_with_member_fallback(process_group_id, signal)
                .unwrap_or(false);
        }
        signal_process_group_id(process_group_id, signal).unwrap_or(false)
    }

    pub(super) fn snapshot(&self) -> Result<ProcessSnapshot> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("process output lock was poisoned"))?;
        // A running process may be polled between the bytes of one UTF-8 scalar. Keep that short
        // suffix for the next snapshot; once the process has exited this is the last response the
        // manager will expose, so flush any incomplete bytes lossily instead of dropping them.
        let boundary = if state.exit_code.is_some() || state.readers == 0 {
            SnapshotBoundary::Final
        } else {
            SnapshotBoundary::Intermediate
        };
        let mut output = state.output.take(boundary);
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

pub(super) struct ProcessSnapshot {
    pub(super) output: String,
    pub(super) exit_code: Option<i32>,
    pub(super) total_bytes: usize,
    pub(super) omitted_bytes: usize,
}

impl ProcessSnapshot {
    pub(super) fn has_exited(&self) -> bool {
        self.exit_code.is_some()
    }
}

#[derive(Default)]
struct PendingOutput {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted: usize,
}

#[derive(Clone, Copy)]
enum SnapshotBoundary {
    Intermediate,
    Final,
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
            self.omitted = self
                .omitted
                .saturating_add(self.tail.len())
                .saturating_add(bytes.len().saturating_sub(RETAINED_TAIL_BYTES));
            self.tail.clear();
            self.tail
                .extend(&bytes[bytes.len() - RETAINED_TAIL_BYTES..]);
            return;
        }
        let excess = (self.tail.len() + bytes.len()).saturating_sub(RETAINED_TAIL_BYTES);
        if excess > 0 {
            // A Vec shifts the entire retained tail on every process read after reaching the
            // limit. Advancing VecDeque's front keeps append work proportional to new output.
            drop(self.tail.drain(..excess));
            self.omitted = self.omitted.saturating_add(excess);
        }
        self.tail.extend(bytes);
    }

    fn take(&mut self, boundary: SnapshotBoundary) -> PendingOutputSnapshot {
        let omitted_bytes = self.omitted;
        let retained_bytes = self.head.len().saturating_add(self.tail.len());
        let mut bytes = std::mem::take(&mut self.head);
        if omitted_bytes > 0 {
            let marker = format!("\n... {omitted_bytes} bytes omitted ...\n");
            bytes.reserve(marker.len().saturating_add(self.tail.len()));
            bytes.extend_from_slice(marker.as_bytes());
        } else {
            bytes.reserve(self.tail.len());
        }
        bytes.extend(std::mem::take(&mut self.tail));
        self.omitted = 0;

        let pending = match boundary {
            SnapshotBoundary::Intermediate => {
                let length = incomplete_utf8_suffix_length(&bytes);
                bytes.split_off(bytes.len().saturating_sub(length))
            }
            SnapshotBoundary::Final => Vec::new(),
        };
        let total_bytes = retained_bytes
            .saturating_add(omitted_bytes)
            .saturating_sub(pending.len());
        self.head = pending;
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
        };
        PendingOutputSnapshot {
            text,
            total_bytes,
            omitted_bytes,
        }
    }
}

fn incomplete_utf8_suffix_length(bytes: &[u8]) -> usize {
    // A UTF-8 scalar is at most four bytes, so no valid incomplete suffix can exceed three. Check
    // each possible suffix independently: an earlier invalid byte must not hide a later valid
    // prefix that can be completed by the process's next write.
    (1..=bytes.len().min(3))
        .rev()
        .find(|length| {
            std::str::from_utf8(&bytes[bytes.len() - length..])
                .is_err_and(|error| error.valid_up_to() == 0 && error.error_len().is_none())
        })
        .unwrap_or(0)
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct PendingOutputSnapshot {
    text: String,
    total_bytes: usize,
    omitted_bytes: usize,
}

fn spawn_piped(
    shell: &DetectedShell,
    shell_startup: ShellStartup,
    command: &str,
    cwd: &Path,
) -> Result<Arc<ProcessSession>> {
    let (program, arguments) = shell_command(shell, shell_startup, command);
    let mut process = tokio::process::Command::new(program);
    process
        .args(arguments)
        .current_dir(cwd)
        .envs(UNIFIED_EXEC_ENV)
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process.process_group(0);
    let mut child = process
        .spawn()
        .with_context(|| format!("failed to start command in {}", cwd.display()))?;
    let stdout = child.stdout.take().context("failed to capture stdout")?;
    let stderr = child.stderr.take().context("failed to capture stderr")?;
    let process_group_id = child.id().and_then(|id| i32::try_from(id).ok());
    let session = ProcessSession::new(None, None, 2, ProcessMode::Piped, process_group_id);
    spawn_async_reader(Arc::clone(&session), stdout, "stdout");
    spawn_async_reader(Arc::clone(&session), stderr, "stderr");
    let waiter = Arc::clone(&session);
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => waiter.exited(exit_code_from_status(status), None),
            Err(error) => waiter.exited(-1, Some(format!("failed to wait for command: {error}"))),
        }
    });
    Ok(session)
}

fn spawn_pty(
    shell: &DetectedShell,
    shell_startup: ShellStartup,
    command: &str,
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
    let (program, arguments) = shell_command(shell, shell_startup, command);
    let mut process = CommandBuilder::new(program);
    for argument in arguments {
        process.arg(argument);
    }
    process.cwd(cwd);
    for (key, value) in UNIFIED_EXEC_ENV {
        process.env(key, value);
    }
    process.env("TERM", "xterm-256color");
    let mut child = pair
        .slave
        .spawn_command(process)
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
    let writer = spawn_writer(writer);
    let session = ProcessSession::new(
        Some(writer),
        Some(killer),
        1,
        ProcessMode::Pty,
        process_group_id,
    );
    spawn_blocking_reader(Arc::clone(&session), reader, "PTY");
    let waiter = Arc::clone(&session);
    std::thread::spawn(move || match child.wait() {
        Ok(status) => waiter.exited(status.exit_code() as i32, None),
        Err(error) => waiter.exited(-1, Some(format!("failed to wait for PTY command: {error}"))),
    });
    Ok(session)
}

fn spawn_async_reader(
    session: Arc<ProcessSession>,
    mut reader: impl AsyncRead + Unpin + Send + 'static,
    label: &'static str,
) {
    tokio::spawn(async move {
        let mut buffer = vec![0_u8; PROCESS_READ_BUFFER_BYTES];
        let error = loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break None,
                Ok(read) => session.append(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Some(format!("failed to read {label}: {error}")),
            }
        };
        session.reader_finished(error);
    });
}

fn spawn_blocking_reader(
    session: Arc<ProcessSession>,
    mut reader: impl Read + Send + 'static,
    label: &'static str,
) {
    std::thread::spawn(move || {
        // A larger read amortizes syscalls, retention-lock acquisitions, and wakeups on noisy
        // commands. Pipe and PTY reads still return with available bytes, preserving interactive
        // output latency.
        let mut buffer = [0_u8; PROCESS_READ_BUFFER_BYTES];
        let error = loop {
            match reader.read(&mut buffer) {
                Ok(0) => break None,
                Ok(read) => session.append(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Some(format!("failed to read {label}: {error}")),
            }
        };
        session.reader_finished(error);
    });
}

fn spawn_writer(writer: Box<dyn Write + Send>) -> mpsc::Sender<Vec<u8>> {
    let (sender, mut receiver) = mpsc::channel::<Vec<u8>>(PROCESS_STDIN_QUEUE_CAPACITY);
    tokio::spawn(async move {
        let mut writer = writer;
        while let Some(bytes) = receiver.recv().await {
            let result = tokio::task::spawn_blocking(move || {
                writer.write_all(&bytes)?;
                writer.flush()?;
                Ok::<_, std::io::Error>(writer)
            })
            .await;
            match result {
                Ok(Ok(returned_writer)) => writer = returned_writer,
                Ok(Err(_)) | Err(_) => return,
            }
        }
    });
    sender
}

fn exit_code_from_status(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(-1)
}

fn signal_process_group_id(process_group_id: i32, signal: i32) -> std::io::Result<bool> {
    let result = unsafe { libc::killpg(process_group_id, signal) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(false);
    }
    Err(error)
}

#[cfg(target_os = "macos")]
fn signal_process_id(process_id: libc::pid_t, signal: i32) -> std::io::Result<bool> {
    if unsafe { libc::kill(process_id, signal) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(false);
    }
    Err(error)
}

#[cfg(target_os = "macos")]
fn signal_process_group_with_member_fallback(
    process_group_id: i32,
    signal: i32,
) -> std::io::Result<bool> {
    if process_group_id <= 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid process group ID",
        ));
    }
    match signal_process_group_id(process_group_id, signal) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
        result => return result,
    }

    let mut process_ids: Vec<libc::pid_t> = vec![0; 16];
    loop {
        let buffer_size = libc::c_int::try_from(std::mem::size_of_val(process_ids.as_slice()))
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "process group is too large",
                )
            })?;
        let count = unsafe {
            libc::proc_listpgrppids(
                process_group_id,
                process_ids.as_mut_ptr().cast(),
                buffer_size,
            )
        };
        if count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let count = count as usize;
        if count < process_ids.len() {
            process_ids.truncate(count);
            break;
        }
        let capacity = process_ids.len().checked_mul(2).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "process group is too large",
            )
        })?;
        process_ids.resize(capacity, 0);
    }
    // Signal descendants first so an exiting leader cannot orphan work before it is terminated.
    process_ids.sort_unstable_by_key(|process_id| *process_id == process_group_id);

    let mut signalled = false;
    let mut first_error = None;
    for process_id in process_ids {
        if process_id <= 0 {
            continue;
        }
        let current_group_id = unsafe { libc::getpgid(process_id) };
        if current_group_id == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) && first_error.is_none() {
                first_error = Some(error);
            }
            continue;
        }
        if current_group_id != process_group_id {
            continue;
        }
        match signal_process_id(process_id, signal) {
            Ok(delivered) => signalled |= delivered,
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }

    if signalled {
        Ok(true)
    } else {
        first_error.map_or(Ok(false), Err)
    }
}

pub(super) fn shell_command(
    shell: &DetectedShell,
    shell_startup: ShellStartup,
    command: &str,
) -> (PathBuf, Vec<String>) {
    let arguments = match shell.shell_type {
        ShellType::Zsh | ShellType::Bash | ShellType::Sh => vec![
            match shell_startup {
                ShellStartup::Login => "-lc",
                ShellStartup::NonLogin => "-c",
            }
            .to_string(),
            command.to_string(),
        ],
        ShellType::PowerShell => {
            let mut arguments = Vec::new();
            if matches!(shell_startup, ShellStartup::NonLogin) {
                arguments.push("-NoProfile".to_string());
            }
            arguments.push("-Command".to_string());
            arguments.push(command.to_string());
            arguments
        }
        ShellType::Cmd => vec!["/c".to_string(), command.to_string()],
    };
    (shell.shell_path.clone(), arguments)
}

#[cfg(test)]
#[path = "process_session_tests.rs"]
mod tests;
