//! Bounded-output adapter around Codex's unified pipe and PTY process runtime.
//!
//! Spawning, signalling, stdin backpressure, and child reaping come from `codex-utils-pty` at
//! `3aae5d885bac39c1262491aa3fd100dfd8b3919f`; this module retains BetterCodex's compact polling
//! state and model-visible output chunks.

use crate::shell_command::shell_detect::DetectedShell;
use crate::shell_command::shell_detect::ShellType;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_utils_pty::ProcessHandle;
use codex_utils_pty::ProcessSignal;
use codex_utils_pty::SpawnedProcess;
use codex_utils_pty::TerminalSize;
use codex_utils_pty::spawn_pipe_process_no_stdin;
use codex_utils_pty::spawn_pty_process;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::time::timeout;

const POST_EXIT_CLOSE_WAIT_CAP: Duration = Duration::from_millis(50);
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
    process: ProcessHandle,
    notify: Notify,
    interaction: Arc<AsyncMutex<()>>,
    mode: ProcessMode,
}

impl ProcessSession {
    pub(super) async fn spawn(
        shell: &DetectedShell,
        shell_startup: ShellStartup,
        command: &str,
        cwd: &Path,
        mode: ProcessMode,
        environment_overrides: &HashMap<String, String>,
    ) -> Result<Arc<Self>> {
        let (program, arguments) = shell_command(shell, shell_startup, command);
        let program = program
            .to_str()
            .ok_or_else(|| anyhow!("shell path is not valid UTF-8: {}", program.display()))?;
        let mut environment = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect::<HashMap<_, _>>();
        for (key, value) in UNIFIED_EXEC_ENV {
            environment.insert(key.to_string(), value.to_string());
        }
        environment.insert(
            "TERM".to_string(),
            match mode {
                ProcessMode::Piped => "dumb",
                ProcessMode::Pty => "xterm-256color",
            }
            .to_string(),
        );
        environment.extend(environment_overrides.clone());
        let arg0 = None;
        let spawned = match mode {
            ProcessMode::Piped => {
                spawn_pipe_process_no_stdin(program, &arguments, cwd, &environment, &arg0, &[])
                    .await
            }
            ProcessMode::Pty => {
                spawn_pty_process(
                    program,
                    &arguments,
                    cwd,
                    &environment,
                    &arg0,
                    TerminalSize { rows: 24, cols: 80 },
                    &[],
                )
                .await
            }
        }
        .with_context(|| format!("failed to start command in {}", cwd.display()))?;
        Ok(Self::from_spawned(spawned, mode))
    }

    fn from_spawned(spawned: SpawnedProcess, mode: ProcessMode) -> Arc<Self> {
        let SpawnedProcess {
            session: process,
            stdout_rx,
            stderr_rx,
            exit_rx,
        } = spawned;
        let session = Arc::new(Self {
            state: Mutex::new(ProcessState {
                output: PendingOutput::default(),
                exit_code: None,
                readers: 2,
                errors: Vec::new(),
            }),
            process,
            notify: Notify::new(),
            interaction: Arc::new(AsyncMutex::new(())),
            mode,
        });
        spawn_output_receiver(Arc::downgrade(&session), stdout_rx);
        spawn_output_receiver(Arc::downgrade(&session), stderr_rx);
        spawn_exit_receiver(Arc::downgrade(&session), exit_rx);
        session
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

    fn reader_finished(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.readers = state.readers.saturating_sub(1);
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
        self.process
            .writer_sender()
            .send(bytes)
            .await
            .map_err(|_| anyhow!("stdin is closed"))
    }

    pub(super) fn interrupt(&self) -> Result<()> {
        self.process
            .signal(ProcessSignal::Interrupt)
            .map_err(|_| anyhow!("failed to interrupt process"))
    }

    pub(super) fn kill(&self) {
        self.process.terminate();
    }

    pub(super) fn has_exited(&self) -> bool {
        self.process.has_exited()
            || self
                .state
                .lock()
                .is_ok_and(|state| state.exit_code.is_some())
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

fn spawn_output_receiver(session: Weak<ProcessSession>, mut receiver: mpsc::Receiver<Vec<u8>>) {
    tokio::spawn(async move {
        while let Some(bytes) = receiver.recv().await {
            let Some(session) = session.upgrade() else {
                return;
            };
            session.append(&bytes);
        }
        if let Some(session) = session.upgrade() {
            session.reader_finished();
        }
    });
}

fn spawn_exit_receiver(
    session: Weak<ProcessSession>,
    receiver: tokio::sync::oneshot::Receiver<i32>,
) {
    tokio::spawn(async move {
        let result = receiver.await;
        let Some(session) = session.upgrade() else {
            return;
        };
        match result {
            Ok(exit_code) => session.exited(exit_code, None),
            Err(error) => session.exited(
                -1,
                Some(format!(
                    "process waiter closed before reporting exit: {error}"
                )),
            ),
        }
    });
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
