//! Single-environment adaptation of Codex's model-visible unified exec contract at
//! `1669c2403f793d0230065397dfc25f52b844244e`. Low-level process transport is delegated to the
//! newer upstream runtime documented in `process_session`.
//!
//! bettercodex removes Codex's sandbox, approval, remote-environment, hook,
//! and telemetry layers, while retaining its model-visible process/session,
//! PTY, yield, environment, and output-truncation behavior.

use crate::shell_command::shell_detect::default_user_shell;
use crate::shell_command::shell_detect::get_shell_by_model_provided_path;
use crate::truncation::TruncationPolicy;
use crate::truncation::approx_tokens_from_byte_count;
use crate::truncation::formatted_truncate_text;
use crate::truncation::truncate_text;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use super::process_session::ProcessMode;
use super::process_session::ProcessSession;
use super::process_session::ShellStartup;
use super::process_session::shell_command;

const DEFAULT_EXEC_YIELD: Duration = Duration::from_secs(10);
const MIN_EXEC_YIELD: Duration = Duration::from_millis(250);
const MAX_EXEC_YIELD: Duration = Duration::from_secs(30);
const DEFAULT_WRITE_YIELD: Duration = Duration::from_millis(250);
const DEFAULT_POLL_YIELD: Duration = Duration::from_secs(5);
const MAX_POLL_YIELD: Duration = Duration::from_secs(5 * 60);
const MAX_PROCESSES: usize = 64;

#[derive(Clone)]
pub(crate) struct ProcessManager {
    cwd: Arc<PathBuf>,
    store: Arc<Mutex<ProcessStore>>,
    _cleanup: Arc<ProcessCleanup>,
}

struct ProcessCleanup {
    store: Arc<Mutex<ProcessStore>>,
}

#[derive(Default)]
struct ProcessStore {
    sessions: HashMap<i32, ProcessEntry>,
    reserved_session_ids: HashSet<i32>,
}

struct ProcessEntry {
    session: Arc<ProcessSession>,
    command: String,
    cwd: PathBuf,
    started_at: Instant,
    last_used: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackgroundProcess {
    pub(crate) session_id: i32,
    pub(crate) command: String,
    pub(crate) cwd: PathBuf,
    pub(crate) running_for: Duration,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProcessStorage {
    Stored,
    Transient,
}

impl ProcessManager {
    pub(crate) fn new(cwd: PathBuf) -> Self {
        let store = Arc::new(Mutex::new(ProcessStore::default()));
        Self {
            cwd: Arc::new(cwd),
            _cleanup: Arc::new(ProcessCleanup {
                store: Arc::clone(&store),
            }),
            store,
        }
    }

    pub(crate) fn list_background_processes(&self) -> Vec<BackgroundProcess> {
        let now = Instant::now();
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut processes = store
            .sessions
            .iter()
            .filter(|(_, entry)| !entry.session.has_exited())
            .map(|(session_id, entry)| BackgroundProcess {
                session_id: *session_id,
                command: entry.command.clone(),
                cwd: entry.cwd.clone(),
                running_for: now.saturating_duration_since(entry.started_at),
            })
            .collect::<Vec<_>>();
        processes.sort_by_key(|process| process.session_id);
        processes
    }

    pub(crate) fn stop_all_background_processes(&self) -> usize {
        let entries = {
            let mut store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store.reserved_session_ids.clear();
            store
                .sessions
                .drain()
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>()
        };
        let count = entries
            .iter()
            .filter(|entry| !entry.session.has_exited())
            .count();
        for entry in entries {
            entry.session.kill();
        }
        count
    }

    pub(crate) async fn run_operator_command(&self, command: String) -> Result<Value> {
        self.exec_command(
            json!({
                "cmd": command,
            }),
            CancellationToken::new(),
        )
        .await
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
        let shell_startup = if arguments.login.unwrap_or(true) {
            ShellStartup::Login
        } else {
            ShellStartup::NonLogin
        };
        let session_id_reservation = self.reserve_session_id()?;
        let session_id = session_id_reservation.session_id;
        let mode = if arguments.tty.unwrap_or(false) {
            ProcessMode::Pty
        } else {
            ProcessMode::Piped
        };
        let session =
            ProcessSession::spawn(&shell, shell_startup, &arguments.cmd, &workdir, mode).await?;
        let started = Instant::now();
        let storage = if session.has_exited() {
            ProcessStorage::Transient
        } else {
            self.store_session(
                session_id,
                Arc::clone(&session),
                arguments.cmd.clone(),
                workdir,
                started,
            )?;
            session_id_reservation.commit();
            ProcessStorage::Stored
        };

        let yield_time = bounded_duration(
            arguments.yield_time_ms,
            DEFAULT_EXEC_YIELD,
            MIN_EXEC_YIELD,
            MAX_EXEC_YIELD,
        );
        tokio::select! {
            _ = cancellation.cancelled() => {
                session.kill();
                if storage == ProcessStorage::Stored {
                    self.remove_session(session_id)?;
                }
                return Err(anyhow!("exec_command was interrupted"));
            }
            _ = session.wait(yield_time) => {}
        }
        self.output(
            session_id,
            session,
            started.elapsed(),
            arguments.max_output_tokens,
            storage,
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
            .store
            .lock()
            .map_err(|_| anyhow!("process table lock was poisoned"))?
            .sessions
            .get(&arguments.session_id)
            .map(|entry| Arc::clone(&entry.session))
            .ok_or_else(|| anyhow!("Unknown process id {}", arguments.session_id))?;
        let _interaction = session.interaction().lock_owned().await;
        {
            let mut store = self
                .store
                .lock()
                .map_err(|_| anyhow!("process table lock was poisoned"))?;
            let entry = store
                .sessions
                .get_mut(&arguments.session_id)
                .filter(|entry| Arc::ptr_eq(&entry.session, &session))
                .ok_or_else(|| anyhow!("Unknown process id {}", arguments.session_id))?;
            entry.last_used = Instant::now();
        }

        let has_input = !arguments.chars.is_empty();
        if has_input {
            if !session.has_terminal() {
                if arguments.chars == "\u{3}" {
                    session.interrupt()?;
                } else {
                    return Err(anyhow!(
                        "stdin is closed for this session; rerun exec_command with tty=true to keep stdin open"
                    ));
                }
            } else {
                tokio::select! {
                    _ = cancellation.cancelled() => {
                        session.kill();
                        self.remove_session(arguments.session_id)?;
                        return Err(anyhow!("write_stdin was interrupted"));
                    }
                    result = session.write(arguments.chars.into_bytes()) => result?,
                }
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
            ProcessStorage::Stored,
        )
    }

    fn output(
        &self,
        session_id: i32,
        session: Arc<ProcessSession>,
        wall_time: Duration,
        max_output_tokens: Option<usize>,
        storage: ProcessStorage,
    ) -> Result<Value> {
        let snapshot = match storage {
            ProcessStorage::Stored => {
                let store = self
                    .store
                    .lock()
                    .map_err(|_| anyhow!("process table lock was poisoned"))?;
                store
                    .sessions
                    .get(&session_id)
                    .filter(|entry| Arc::ptr_eq(&entry.session, &session))
                    .ok_or_else(|| anyhow!("Unknown process id {session_id}"))?;
                session.snapshot()?
            }
            ProcessStorage::Transient => session.snapshot()?,
        };
        if storage == ProcessStorage::Stored && snapshot.has_exited() {
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
        if storage == ProcessStorage::Stored && !snapshot.has_exited() {
            result["session_id"] = json!(session_id);
        }
        result["original_token_count"] = json!(original_token_count);
        Ok(result)
    }

    fn reserve_session_id(&self) -> Result<SessionIdReservation<'_>> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow!("process ID table lock was poisoned"))?;
        loop {
            let bytes = *uuid::Uuid::new_v4().as_bytes();
            let random = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let candidate = 1_000 + (random % 99_000) as i32;
            if store.reserved_session_ids.insert(candidate) {
                return Ok(SessionIdReservation {
                    store: &self.store,
                    session_id: candidate,
                    committed: false,
                });
            }
        }
    }

    fn remove_session(&self, session_id: i32) -> Result<()> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow!("process table lock was poisoned"))?;
        let removed = store.sessions.remove(&session_id).is_some();
        if removed {
            store.reserved_session_ids.remove(&session_id);
        }
        Ok(())
    }

    fn store_session(
        &self,
        session_id: i32,
        session: Arc<ProcessSession>,
        command: String,
        cwd: PathBuf,
        started_at: Instant,
    ) -> Result<()> {
        let pruned_entry = {
            let mut store = self
                .store
                .lock()
                .map_err(|_| anyhow!("process table lock was poisoned"))?;
            let pruned = Self::prune_processes_if_needed(&mut store.sessions);
            store.sessions.insert(
                session_id,
                ProcessEntry {
                    session,
                    command,
                    cwd,
                    started_at,
                    last_used: started_at,
                },
            );
            if let Some((pruned_id, _)) = pruned.as_ref() {
                store.reserved_session_ids.remove(pruned_id);
            }
            pruned.map(|(_, entry)| entry)
        };
        if let Some(pruned_entry) = pruned_entry {
            pruned_entry.session.kill();
        }
        Ok(())
    }

    fn prune_processes_if_needed(
        sessions: &mut HashMap<i32, ProcessEntry>,
    ) -> Option<(i32, ProcessEntry)> {
        if sessions.len() < MAX_PROCESSES {
            return None;
        }

        let mut metadata = sessions
            .iter()
            .map(|(id, entry)| (*id, entry.last_used, entry.session.has_exited()))
            .collect::<Vec<_>>();
        let mut found_locked_exited_process = false;

        while let Some(session_id) = process_id_to_prune_from_meta(&metadata) {
            let candidate = sessions
                .get(&session_id)
                .map(|entry| Arc::clone(&entry.session));
            let candidate_has_exited = candidate
                .as_ref()
                .is_some_and(|session| session.has_exited());
            if found_locked_exited_process && !candidate_has_exited {
                return None;
            }

            if let Some(interaction) = candidate.as_ref().map(|session| session.interaction())
                && let Ok(_interaction) = interaction.try_lock_owned()
                && let Some(entry) = sessions.remove(&session_id)
            {
                return Some((session_id, entry));
            }
            found_locked_exited_process |=
                candidate_has_exited || candidate.is_some_and(|session| session.has_exited());
            metadata.retain(|(id, _, _)| *id != session_id);
        }

        None
    }
}

fn process_id_to_prune_from_meta(metadata: &[(i32, Instant, bool)]) -> Option<i32> {
    if metadata.is_empty() {
        return None;
    }

    let mut by_recency = metadata.to_vec();
    by_recency.sort_by_key(|(_, last_used, _)| Reverse(*last_used));
    let protected = by_recency
        .iter()
        .take(8)
        .map(|(session_id, _, _)| *session_id)
        .collect::<HashSet<_>>();

    let mut least_recently_used = metadata.to_vec();
    least_recently_used.sort_by_key(|(_, last_used, _)| *last_used);
    if let Some((session_id, _, _)) = least_recently_used
        .iter()
        .find(|(session_id, _, exited)| !protected.contains(session_id) && *exited)
    {
        return Some(*session_id);
    }

    least_recently_used
        .into_iter()
        .find(|(session_id, _, _)| !protected.contains(session_id))
        .map(|(session_id, _, _)| session_id)
}

struct SessionIdReservation<'a> {
    store: &'a Mutex<ProcessStore>,
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
            && let Ok(mut store) = self.store.lock()
        {
            store.reserved_session_ids.remove(&self.session_id);
        }
    }
}

impl Drop for ProcessCleanup {
    fn drop(&mut self) {
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for entry in store.sessions.drain().map(|(_, entry)| entry) {
            entry.session.kill();
        }
        store.reserved_session_ids.clear();
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

fn resolve_workdir(cwd: &Path, requested: Option<&str>) -> PathBuf {
    requested
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map_or_else(|| cwd.to_path_buf(), |path| cwd.join(path))
}

pub(crate) fn command_argv_for_display(input: &Value) -> Vec<String> {
    let shell = input
        .get("shell")
        .and_then(Value::as_str)
        .map_or_else(default_user_shell, |shell| {
            get_shell_by_model_provided_path(&PathBuf::from(shell))
        });
    let shell_startup = if input.get("login").and_then(Value::as_bool).unwrap_or(true) {
        ShellStartup::Login
    } else {
        ShellStartup::NonLogin
    };
    let cmd = input.get("cmd").and_then(Value::as_str).unwrap_or_default();
    let (program, arguments) = shell_command(&shell, shell_startup, cmd);
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
    uuid::Uuid::new_v4().simple().to_string()[..6].to_string()
}

fn truncate_output(
    output: &str,
    max_tokens: Option<usize>,
    original_token_count: usize,
    omitted_bytes: usize,
) -> String {
    let max_tokens = max_tokens
        .unwrap_or(super::MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS)
        .min(super::MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS);
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
    async fn non_tty_interrupt_reports_conventional_signal_exit_code() {
        let cwd = std::env::current_dir().unwrap();
        let manager = ProcessManager::new(cwd);
        let started = manager
            .exec_command(
                json!({
                    "cmd": "printf ready; exec sleep 30",
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
                    "chars": "\u{3}",
                    "yield_time_ms": 1000,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(completed["exit_code"], 130);
        assert!(completed.get("session_id").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tty_input_backpressure_does_not_block_write_stdin() {
        let cwd = std::env::current_dir().unwrap();
        let manager = ProcessManager::new(cwd);
        let started = manager
            .exec_command(
                json!({
                    "cmd": "stty -echo; printf ready; sleep 30",
                    "login": false,
                    "shell": "/bin/sh",
                    "tty": true,
                    "yield_time_ms": 250,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let session_id = started["session_id"].as_i64().unwrap() as i32;
        let input = "x".repeat(256 * 1024);

        let polled = tokio::time::timeout(
            Duration::from_secs(3),
            manager.write_stdin(
                json!({
                    "session_id": session_id,
                    "chars": input,
                    "yield_time_ms": 250,
                }),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("write_stdin blocked on the child's full PTY input buffer")
        .unwrap();

        assert_eq!(polled["session_id"], session_id);
        assert_eq!(manager.stop_all_background_processes(), 1);
        tokio::time::sleep(Duration::from_millis(50)).await;
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
