//! Live migration of an interactive bettercodex terminal into tmux.
//!
//! Interactive launches outside tmux use a small supervisor and a private pseudoterminal. The
//! agent uses that pseudoterminal for its entire lifetime. Initially the supervisor relays it to
//! the invoking terminal; on `/tmux`, ownership of the same pseudoterminal master passes to a relay
//! in a fresh `cN` tmux session. The agent, active turn, tools, in-memory state, and terminal event
//! source never move or restart.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::DirBuilder;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Write;
use std::mem;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use uuid::Uuid;

const SESSION_PREFIX: char = 'c';
const CAFFEINATE: &str = "/usr/bin/caffeinate";
const CAFFEINATE_MARKER: &str = "BCODEX_CAFFEINATED";
const WORKER_CONTROL_FD: &str = "BCODEX_WORKER_CONTROL_FD";
const RELAY_COMMAND: &str = "--internal-tmux-relay";
const RELAY_DIRECTORY_PREFIX: &str = ".bettercodex-tmux-relay-";
const RELAY_SOCKET_NAME: &str = "socket";
const RELAY_FD_TAG: u8 = 0x42;
const RELAY_READY_TAG: u8 = 0x43;
const HANDOFF_COMMITTED_TAG: u8 = 0x44;
const HANDOFF_REJECTED_TAG: u8 = 0x45;
const MAX_CONTROL_MESSAGE_BYTES: usize = 256;
const RELAY_START_TIMEOUT: Duration = Duration::from_secs(10);
const RELAY_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const RELAY_IO_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CLEAR_VISIBLE_TERMINAL: &[u8] = b"\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[H";

static HOT_TMUX_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The worker end of the supervisor channel used for a one-time live handoff.
pub(crate) struct WorkerHandoff {
    control: Option<UnixStream>,
}

impl WorkerHandoff {
    pub(crate) fn transfer(&mut self, prepared: &PreparedTmuxSession) -> Result<()> {
        validate_session_id(&prepared.session_id)?;
        let control = self
            .control
            .as_mut()
            .context("the interactive supervisor was already transferred")?;
        control
            .set_read_timeout(Some(RELAY_START_TIMEOUT))
            .context("failed to configure the interactive supervisor response")?;

        let request = (|| {
            send_fd(control, prepared.relay.as_raw_fd())
                .context("failed to pass the tmux relay to the interactive supervisor")?;
            control
                .write_all(format!("{}\n", prepared.session_id).as_bytes())
                .context("failed to request the tmux terminal transfer")?;

            let mut response = [0_u8; 1];
            (&*control)
                .read_exact(&mut response)
                .context("the interactive supervisor exited during the tmux transfer")?;
            match response[0] {
                HANDOFF_COMMITTED_TAG => Ok(()),
                HANDOFF_REJECTED_TAG => {
                    let detail = read_control_line(control)?;
                    if detail.is_empty() {
                        Err(anyhow!(
                            "the interactive supervisor rejected the tmux transfer"
                        ))
                    } else {
                        Err(anyhow!(detail))
                    }
                }
                _ => Err(anyhow!(
                    "the interactive supervisor returned an invalid tmux transfer response"
                )),
            }
        })();

        match request {
            Ok(()) => {
                // The supervisor has already transferred the one live terminal and is launching
                // the tmux client. Treat that acknowledgement as the transaction's commit point;
                // a best-effort timeout reset cannot safely roll it back.
                let _ = control.set_read_timeout(None);
                self.control.take();
                Ok(())
            }
            Err(error) => {
                control
                    .set_read_timeout(None)
                    .context("failed to restore the interactive supervisor channel")?;
                Err(error)
            }
        }
    }
}

/// A detached tmux relay waiting to receive the agent's stable pseudoterminal master.
pub(crate) struct PreparedTmuxSession {
    session_id: String,
    session_name: String,
    relay: UnixStream,
    committed: bool,
}

impl PreparedTmuxSession {
    pub(crate) fn commit(mut self) -> String {
        self.committed = true;
        HOT_TMUX_ACTIVE.store(true, Ordering::Release);
        self.session_name.clone()
    }
}

impl Drop for PreparedTmuxSession {
    fn drop(&mut self) {
        if !self.committed {
            kill_session(&self.session_id);
        }
    }
}

/// Establish the process boundary needed for a later live handoff.
///
/// This returns only in the agent worker (or when no supervisor is needed). The outer interactive
/// process owns the stable pseudoterminal master, relays it to the invoking terminal, and hosts the
/// tmux client after a handoff so it can restore a clean terminal when that client exits.
pub(crate) fn enter_agent_process(
    arguments: &[String],
    interactive_tui: bool,
) -> Result<Option<WorkerHandoff>> {
    if let Some(worker_fd) = worker_control_fd()? {
        prevent_macos_idle_sleep(arguments)?;
        return take_worker_handoff(worker_fd).map(Some);
    }

    if interactive_tui && !in_tmux_environment() {
        launch_worker(arguments)?;
        unreachable!("the interactive supervisor exits after the worker or tmux client");
    }

    prevent_macos_idle_sleep(arguments)?;
    Ok(None)
}

pub(crate) fn is_tmux_active() -> bool {
    HOT_TMUX_ACTIVE.load(Ordering::Acquire) || in_tmux_environment()
}

fn in_tmux_environment() -> bool {
    std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some()
}

fn worker_control_fd() -> Result<Option<RawFd>> {
    let Some(value) = std::env::var_os(WORKER_CONTROL_FD) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .ok_or_else(|| anyhow!("{WORKER_CONTROL_FD} is not valid UTF-8"))?;
    let fd = value
        .parse::<RawFd>()
        .with_context(|| format!("{WORKER_CONTROL_FD} is not a file descriptor"))?;
    if fd <= libc::STDERR_FILENO {
        return Err(anyhow!(
            "{WORKER_CONTROL_FD} is not a private file descriptor"
        ));
    }
    Ok(Some(fd))
}

fn take_worker_handoff(fd: RawFd) -> Result<WorkerHandoff> {
    set_close_on_exec(fd, true).context("failed to protect the supervisor control channel")?;
    // SAFETY: the supervisor created this descriptor as one owned end of a UnixStream and passed
    // its number only to this worker. This function is called exactly once in the final worker.
    let control = unsafe { UnixStream::from_raw_fd(fd) };
    // SAFETY: agent process setup runs before the Tokio runtime or any worker threads exist, so no
    // other thread can concurrently access the process environment.
    unsafe {
        std::env::remove_var(WORKER_CONTROL_FD);
    }
    Ok(WorkerHandoff {
        control: Some(control),
    })
}

fn launch_worker(arguments: &[String]) -> Result<()> {
    let size = terminal_size(libc::STDIN_FILENO)
        .context("failed to read the invoking terminal size before agent startup")?;
    let pty = open_pty((size.ws_col, size.ws_row))?;
    let (mut supervisor, worker) = UnixStream::pair()
        .context("failed to create the interactive supervisor control channel")?;
    set_close_on_exec(worker.as_raw_fd(), false)
        .context("failed to pass the supervisor channel to the agent worker")?;

    let executable = std::env::current_exe().context("failed to locate the bcodex executable")?;
    let input = File::from(
        pty.slave
            .try_clone()
            .context("failed to duplicate the agent terminal input")?,
    );
    let output = File::from(
        pty.slave
            .try_clone()
            .context("failed to duplicate the agent terminal output")?,
    );
    let error = File::from(
        pty.slave
            .try_clone()
            .context("failed to duplicate the agent terminal error stream")?,
    );
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .env(WORKER_CONTROL_FD, worker.as_raw_fd().to_string())
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(error));
    // SAFETY: pre_exec runs after fork and before exec. setsid and ioctl are async-signal-safe
    // syscalls, and fd 0 has already been assigned to the private pseudoterminal slave.
    unsafe {
        command.pre_exec(isolate_worker_terminal);
    }
    let mut child = command
        .spawn()
        .context("failed to start the interactive agent worker")?;
    // Command keeps its configured stdio descriptors so it can be spawned again. They are clones
    // of the PTY slave, so retaining the reusable command would keep that slave alive after the
    // worker exits and prevent the tmux relay from observing terminal closure.
    drop(command);
    drop(worker);
    drop(pty.slave);
    supervisor
        .set_read_timeout(Some(RELAY_START_TIMEOUT))
        .context("failed to configure the interactive worker protocol")?;

    let raw_terminal = match RawTerminal::enter(libc::STDIN_FILENO) {
        Ok(raw_terminal) => raw_terminal,
        Err(error) => {
            signal_worker_hangup(&mut child);
            let _ = child.wait();
            return Err(error).context("failed to configure the invoking terminal relay");
        }
    };
    let outcome = supervise_worker(&mut child, &mut supervisor, pty.master.as_raw_fd());
    drop(raw_terminal);

    match outcome? {
        SupervisorOutcome::WorkerExited(status) => exit_with_status(status),
        SupervisorOutcome::EnterTmux(session_id) => attach_tmux(&session_id),
    }
}

fn isolate_worker_terminal() -> io::Result<()> {
    // SAFETY: setsid has no memory-safety preconditions. The freshly forked worker is not a process
    // group leader because it inherits the supervisor's foreground process group.
    if unsafe { libc::setsid() } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: standard input is the open private pseudoterminal slave installed on the Command.
    if unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

enum SupervisorOutcome {
    WorkerExited(std::process::ExitStatus),
    EnterTmux(String),
}

fn supervise_worker(
    child: &mut Child,
    control: &mut UnixStream,
    master_fd: RawFd,
) -> Result<SupervisorOutcome> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut last_size = None;
    let mut control_open = true;
    sync_terminal_size(libc::STDIN_FILENO, master_fd, &mut last_size)?;

    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to inspect the interactive agent worker")?
        {
            drain_terminal_output(master_fd, &mut buffer)?;
            return Ok(SupervisorOutcome::WorkerExited(status));
        }

        sync_terminal_size(libc::STDIN_FILENO, master_fd, &mut last_size)?;
        let mut descriptors = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if control_open {
                    control.as_raw_fd()
                } else {
                    -1
                },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let timeout = poll_timeout(RELAY_IO_POLL_INTERVAL);
        // SAFETY: descriptors contains initialized pollfd values and remains live during poll.
        let ready =
            unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, timeout) };
        if ready == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("interactive terminal relay poll failed");
        }
        if descriptors[1].revents & libc::POLLIN != 0 {
            let Some(read) = read_fd(master_fd, &mut buffer)? else {
                return wait_for_worker(child, master_fd, &mut buffer);
            };
            write_all_fd(libc::STDOUT_FILENO, &buffer[..read])
                .context("failed to relay agent terminal output")?;
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            let Some(read) = read_fd(libc::STDIN_FILENO, &mut buffer)? else {
                signal_worker_hangup(child);
                return wait_for_worker(child, master_fd, &mut buffer);
            };
            write_all_fd(master_fd, &buffer[..read])
                .context("failed to relay invoking terminal input")?;
        }
        if control_open && descriptors[2].revents & libc::POLLIN != 0 {
            if let Some(relay_fd) = receive_optional_fd(control)? {
                let session_id = read_control_line(control)?;
                let handoff = validate_session_id(&session_id).and_then(|()| {
                    drain_terminal_output(master_fd, &mut buffer)?;
                    complete_relay_handoff(relay_fd, master_fd)
                });
                match handoff {
                    Ok(()) => {
                        control
                            .write_all(&[HANDOFF_COMMITTED_TAG])
                            .context("failed to confirm the tmux terminal transfer")?;
                        return Ok(SupervisorOutcome::EnterTmux(session_id));
                    }
                    Err(error) => reject_handoff(control, &error)?,
                }
            } else {
                control_open = false;
            }
        }

        let closed = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if descriptors[0].revents & closed != 0 && descriptors[0].revents & libc::POLLIN == 0 {
            signal_worker_hangup(child);
            return wait_for_worker(child, master_fd, &mut buffer);
        }
        if descriptors[1].revents & closed != 0 && descriptors[1].revents & libc::POLLIN == 0 {
            return wait_for_worker(child, master_fd, &mut buffer);
        }
        if control_open
            && descriptors[2].revents & closed != 0
            && descriptors[2].revents & libc::POLLIN == 0
        {
            control_open = false;
        }
    }
}

fn wait_for_worker(
    child: &mut Child,
    master_fd: RawFd,
    buffer: &mut [u8],
) -> Result<SupervisorOutcome> {
    let status = child
        .wait()
        .context("failed to wait for the interactive agent worker")?;
    drain_terminal_output(master_fd, buffer)?;
    Ok(SupervisorOutcome::WorkerExited(status))
}

fn signal_worker_hangup(child: &mut Child) {
    let Ok(process_group) = libc::pid_t::try_from(child.id()) else {
        let _ = child.kill();
        return;
    };
    // SAFETY: the worker creates a process group whose ID is its PID before exec. A negative target
    // sends SIGHUP to that isolated group without affecting the invoking shell.
    if unsafe { libc::kill(-process_group, libc::SIGHUP) } == -1 {
        let _ = child.kill();
    }
}

fn reject_handoff(control: &mut UnixStream, error: &anyhow::Error) -> Result<()> {
    let mut detail = format!("{error:#}").replace(['\r', '\n'], " ");
    if detail.len() > MAX_CONTROL_MESSAGE_BYTES {
        let mut end = MAX_CONTROL_MESSAGE_BYTES;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
    }
    control
        .write_all(&[HANDOFF_REJECTED_TAG])
        .and_then(|()| control.write_all(detail.as_bytes()))
        .and_then(|()| control.write_all(b"\n"))
        .context("failed to reject the tmux terminal transfer")
}

fn complete_relay_handoff(relay_fd: OwnedFd, master_fd: RawFd) -> Result<()> {
    let mut relay = UnixStream::from(relay_fd);
    relay
        .set_read_timeout(Some(RELAY_START_TIMEOUT))
        .context("failed to configure the tmux relay handshake")?;
    send_fd(&relay, master_fd).context("failed to pass the agent terminal to the tmux relay")?;
    let mut ready = [0_u8; 1];
    relay
        .read_exact(&mut ready)
        .context("tmux relay exited before accepting the agent terminal")?;
    if ready != [RELAY_READY_TAG] {
        return Err(anyhow!("tmux relay returned an invalid handshake"));
    }
    Ok(())
}

fn read_control_line(stream: &UnixStream) -> Result<String> {
    let mut bytes = Vec::new();
    let mut reader = stream;
    for _ in 0..=MAX_CONTROL_MESSAGE_BYTES {
        let mut byte = [0_u8; 1];
        reader
            .read_exact(&mut byte)
            .context("failed to read the interactive supervisor message")?;
        if byte[0] == b'\n' {
            return String::from_utf8(bytes)
                .context("interactive supervisor message was not valid UTF-8");
        }
        bytes.push(byte[0]);
    }
    Err(anyhow!(
        "interactive supervisor message was missing or too long"
    ))
}

fn drain_terminal_output(master_fd: RawFd, buffer: &mut [u8]) -> Result<()> {
    loop {
        let mut descriptor = libc::pollfd {
            fd: master_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor is initialized and remains live for the zero-timeout poll.
        let ready = unsafe { libc::poll(&raw mut descriptor, 1, 0) };
        if ready == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("failed to drain agent terminal output");
        }
        if ready == 0 || descriptor.revents & libc::POLLIN == 0 {
            return Ok(());
        }
        let Some(read) = read_fd(master_fd, buffer)? else {
            return Ok(());
        };
        write_all_fd(libc::STDOUT_FILENO, &buffer[..read])
            .context("failed to drain agent terminal output")?;
    }
}

fn attach_tmux(session_id: &str) -> ! {
    let mut command = Command::new("tmux");
    command.args(["attach-session", "-t", session_id]);

    // tmux saves the normal screen before entering its alternate screen and writes `[exited]`
    // after restoring that screen. Keep the supervisor around for the client lifetime so it can
    // erase both the worker's old inline UI before attach and tmux's exit marker afterward.
    let mut output = io::stdout().lock();
    let status = run_with_terminal_cleanup(&mut output, |_| command.status());
    drop(output);

    match status {
        Ok(status) => exit_with_status(status),
        Err(error) => {
            eprintln!("bcodex: failed to attach tmux session {session_id}: {error}");
            std::process::exit(1)
        }
    }
}

fn run_with_terminal_cleanup<W, T>(output: &mut W, run: impl FnOnce(&mut W) -> T) -> T
where
    W: Write,
{
    let _ = clear_invoking_terminal(&mut *output);
    let result = run(output);
    let _ = clear_invoking_terminal(output);
    result
}

fn clear_invoking_terminal(mut output: impl Write) -> io::Result<()> {
    output.write_all(CLEAR_VISIBLE_TERMINAL)?;
    output.flush()
}

fn exit_with_status(status: std::process::ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;

    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    let signal = status.signal().unwrap_or(1);
    std::process::exit(128_i32.saturating_add(signal));
}

/// Handle the private relay invocation before normal CLI parsing.
pub(crate) fn run_relay_command(arguments: &[String]) -> Option<Result<()>> {
    if arguments
        .first()
        .is_none_or(|argument| argument != RELAY_COMMAND)
    {
        return None;
    }
    Some((|| {
        let [_, encoded_path] = arguments else {
            return Err(anyhow!("invalid internal tmux relay invocation"));
        };
        let path = decode_path(encoded_path)?;
        validate_relay_path(&path)?;
        run_relay(&path)
    })())
}

/// Create the first free `cN` session and accept its relay connection.
///
/// This performs process creation and bounded blocking I/O and should be called with
/// `tokio::task::spawn_blocking` from the TUI.
pub(crate) fn prepare_tmux_session(cwd: &Path, size: (u16, u16)) -> Result<PreparedTmuxSession> {
    ensure_attachable_terminal(std::env::var_os("TERM").as_deref())?;
    let executable = relay_executable()?;
    let endpoint = RelayEndpoint::new()?;
    let endpoint_argument = encode_path(&endpoint.path);
    let mut occupied = occupied_slots()?;

    loop {
        let slot = first_free_slot(&occupied)?;
        let name = format!("{SESSION_PREFIX}{slot}");
        let create = tmux_create_arguments(&name, &executable, cwd, &endpoint_argument, size);
        let output = run_tmux(&create)?;
        if output.status.success() {
            return match tmux_session_id(&output) {
                Ok(session_id) => match endpoint.accept_relay() {
                    Ok(relay) => Ok(PreparedTmuxSession {
                        session_id,
                        session_name: name,
                        relay,
                        committed: false,
                    }),
                    Err(error) => {
                        kill_session(&session_id);
                        Err(error)
                    }
                },
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

/// Resolve the exact process image that speaks this worker's relay protocol.
///
/// Linux appends ` (deleted)` to `current_exe()` after an install atomically replaces a running
/// bcodex binary. That pathname cannot be executed, but the process image remains available through
/// procfs for the worker's lifetime.
#[cfg(target_os = "linux")]
fn relay_executable() -> Result<PathBuf> {
    let executable = linux_process_executable(std::process::id());
    executable.metadata().with_context(|| {
        format!(
            "failed to access running bcodex image {}",
            executable.display()
        )
    })?;
    Ok(executable)
}

#[cfg(target_os = "linux")]
fn linux_process_executable(pid: u32) -> PathBuf {
    Path::new("/proc").join(pid.to_string()).join("exe")
}

#[cfg(not(target_os = "linux"))]
fn relay_executable() -> Result<PathBuf> {
    std::env::current_exe().context("failed to locate the bcodex executable")
}

fn ensure_attachable_terminal(term: Option<&OsStr>) -> Result<()> {
    if term.is_none_or(|term| term.is_empty() || term == "dumb") {
        return Err(anyhow!(
            "tmux requires a capable terminal; TERM is missing or set to `dumb`"
        ));
    }
    Ok(())
}

struct RelayEndpoint {
    directory: PathBuf,
    path: PathBuf,
    listener: UnixListener,
}

impl RelayEndpoint {
    fn new() -> Result<Self> {
        let directory = std::env::temp_dir().join(format!(
            "{RELAY_DIRECTORY_PREFIX}{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        builder.create(&directory).with_context(|| {
            format!(
                "failed to create tmux relay directory {}",
                directory.display()
            )
        })?;
        let path = directory.join(RELAY_SOCKET_NAME);
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = std::fs::remove_dir(&directory);
                return Err(error).with_context(|| {
                    format!("failed to create tmux relay socket {}", path.display())
                });
            }
        };
        let endpoint = Self {
            directory,
            path,
            listener,
        };
        endpoint
            .listener
            .set_nonblocking(true)
            .context("failed to configure the tmux relay listener")?;
        Ok(endpoint)
    }

    fn accept_relay(&self) -> Result<UnixStream> {
        let started = Instant::now();
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => return Ok(stream),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= RELAY_START_TIMEOUT {
                        return Err(anyhow!(
                            "tmux relay did not start within {} seconds",
                            RELAY_START_TIMEOUT.as_secs()
                        ));
                    }
                    std::thread::sleep(RELAY_ACCEPT_POLL_INTERVAL);
                }
                Err(error) => return Err(error).context("failed to accept the tmux relay"),
            }
        }
    }
}

impl Drop for RelayEndpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

fn validate_relay_path(path: &Path) -> Result<()> {
    let valid_parent = path.parent().and_then(Path::file_name).is_some_and(|name| {
        name.as_bytes()
            .starts_with(RELAY_DIRECTORY_PREFIX.as_bytes())
    });
    if !path.is_absolute()
        || path.file_name() != Some(OsStr::new(RELAY_SOCKET_NAME))
        || !valid_parent
    {
        return Err(anyhow!("invalid internal tmux relay path"));
    }
    Ok(())
}

fn encode_path(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = path.as_os_str().as_bytes();
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_path(encoded: &str) -> Result<PathBuf> {
    if encoded.is_empty() || !encoded.len().is_multiple_of(2) {
        return Err(anyhow!("invalid internal tmux relay path encoding"));
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let high = decode_hex_digit(pair[0])?;
        let low = decode_hex_digit(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn decode_hex_digit(digit: u8) -> Result<u8> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        _ => Err(anyhow!("invalid internal tmux relay path encoding")),
    }
}

struct PtyPair {
    master: OwnedFd,
    slave: OwnedFd,
}

fn open_pty((columns, rows): (u16, u16)) -> Result<PtyPair> {
    let mut size = libc::winsize {
        ws_row: rows.max(1),
        ws_col: columns.max(1),
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: both output pointers are valid, the optional name and termios pointers are null, and
    // size is fully initialized. Successful descriptors become OwnedFd below.
    if unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    } == -1
    {
        return Err(io::Error::last_os_error())
            .context("failed to create the managed agent terminal");
    }
    // SAFETY: openpty returned two new, uniquely owned descriptors.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    // SAFETY: openpty returned two new, uniquely owned descriptors.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    set_close_on_exec(master.as_raw_fd(), true)?;
    set_close_on_exec(slave.as_raw_fd(), true)?;
    Ok(PtyPair { master, slave })
}

fn set_close_on_exec(fd: RawFd, enabled: bool) -> io::Result<()> {
    // SAFETY: F_GETFD only inspects the valid descriptor supplied by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let flags = if enabled {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: F_SETFD updates only descriptor flags for the valid descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

const SCM_FD_BYTES: libc::c_uint = mem::size_of::<RawFd>() as libc::c_uint;
const SCM_BUFFER_BYTES: usize = unsafe { libc::CMSG_SPACE(SCM_FD_BYTES) as usize };

#[repr(C)]
union ControlMessage {
    bytes: [u8; SCM_BUFFER_BYTES],
    alignment: libc::cmsghdr,
}

fn send_fd(stream: &UnixStream, fd: RawFd) -> io::Result<()> {
    configure_socket_send(stream)?;
    let mut payload = [RELAY_FD_TAG];
    let mut vector = libc::iovec {
        iov_base: payload.as_mut_ptr().cast::<libc::c_void>(),
        iov_len: payload.len(),
    };
    let mut control = ControlMessage {
        bytes: [0_u8; SCM_BUFFER_BYTES],
    };
    // SAFETY: all message pointers refer to live stack values for sendmsg. The control buffer is
    // cmsghdr-aligned and sized with CMSG_SPACE for one RawFd.
    let sent = unsafe {
        let mut message: libc::msghdr = mem::zeroed();
        message.msg_iov = &raw mut vector;
        message.msg_iovlen = 1;
        message.msg_control = (&raw mut control).cast::<libc::c_void>();
        message.msg_controllen = mem::size_of::<ControlMessage>() as _;
        let header = libc::CMSG_FIRSTHDR(&raw mut message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(SCM_FD_BYTES) as _;
        std::ptr::copy_nonoverlapping(
            (&raw const fd).cast::<u8>(),
            libc::CMSG_DATA(header),
            mem::size_of::<RawFd>(),
        );
        libc::sendmsg(stream.as_raw_fd(), &raw const message, send_message_flags())
    };
    if sent == -1 {
        return Err(io::Error::last_os_error());
    }
    if sent != 1 {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "terminal descriptor message was not sent",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn configure_socket_send(stream: &UnixStream) -> io::Result<()> {
    let enabled: libc::c_int = 1;
    // SAFETY: enabled points to a live c_int and its exact size is supplied to setsockopt.
    if unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&raw const enabled).cast::<libc::c_void>(),
            mem::size_of_val(&enabled) as libc::socklen_t,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn configure_socket_send(_stream: &UnixStream) -> io::Result<()> {
    Ok(())
}

#[cfg(any(target_os = "android", target_os = "linux"))]
const fn send_message_flags() -> libc::c_int {
    libc::MSG_NOSIGNAL
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
const fn send_message_flags() -> libc::c_int {
    0
}

fn receive_fd(stream: &UnixStream) -> Result<OwnedFd> {
    receive_optional_fd(stream)?.context("terminal descriptor message ended before a descriptor")
}

fn receive_optional_fd(stream: &UnixStream) -> Result<Option<OwnedFd>> {
    let mut payload = [0_u8; 1];
    let mut vector = libc::iovec {
        iov_base: payload.as_mut_ptr().cast::<libc::c_void>(),
        iov_len: payload.len(),
    };
    let mut control = ControlMessage {
        bytes: [0_u8; SCM_BUFFER_BYTES],
    };
    // SAFETY: all message pointers refer to live writable stack values for recvmsg. The control
    // buffer is correctly aligned and large enough for exactly one passed descriptor.
    let (received, flags, header) = unsafe {
        let mut message: libc::msghdr = mem::zeroed();
        message.msg_iov = &raw mut vector;
        message.msg_iovlen = 1;
        message.msg_control = (&raw mut control).cast::<libc::c_void>();
        message.msg_controllen = mem::size_of::<ControlMessage>() as _;
        let received = libc::recvmsg(
            stream.as_raw_fd(),
            &raw mut message,
            receive_message_flags(),
        );
        let header = libc::CMSG_FIRSTHDR(&raw mut message);
        (received, message.msg_flags, header)
    };
    if received == -1 {
        return Err(io::Error::last_os_error()).context("failed to receive a terminal descriptor");
    }
    if received == 0 {
        return Ok(None);
    }
    if received != 1 || payload != [RELAY_FD_TAG] || flags & libc::MSG_CTRUNC != 0 {
        return Err(anyhow!("received an invalid terminal descriptor message"));
    }
    if header.is_null() {
        return Err(anyhow!("terminal descriptor message had no descriptor"));
    }
    // SAFETY: recvmsg returned a non-null header in the aligned control buffer. Its metadata and
    // exact one-fd length are validated before copying the payload.
    let fd = unsafe {
        if (*header).cmsg_level != libc::SOL_SOCKET
            || (*header).cmsg_type != libc::SCM_RIGHTS
            || !cmsg_contains_one_fd(header)
        {
            return Err(anyhow!("received an invalid terminal descriptor message"));
        }
        let mut fd = -1;
        std::ptr::copy_nonoverlapping(
            libc::CMSG_DATA(header),
            (&raw mut fd).cast::<u8>(),
            mem::size_of::<RawFd>(),
        );
        fd
    };
    if fd < 0 {
        return Err(anyhow!("received an invalid terminal descriptor"));
    }
    // SAFETY: SCM_RIGHTS installed a new descriptor owned by this process.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    set_close_on_exec(fd.as_raw_fd(), true)?;
    Ok(Some(fd))
}

#[cfg(target_os = "macos")]
unsafe fn cmsg_contains_one_fd(header: *mut libc::cmsghdr) -> bool {
    // SAFETY: callers validate that header points into the recvmsg control buffer.
    unsafe { (*header).cmsg_len == libc::CMSG_LEN(SCM_FD_BYTES) }
}

#[cfg(not(target_os = "macos"))]
unsafe fn cmsg_contains_one_fd(header: *mut libc::cmsghdr) -> bool {
    // SAFETY: callers validate that header points into the recvmsg control buffer.
    unsafe { (*header).cmsg_len == libc::CMSG_LEN(SCM_FD_BYTES) as usize }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
const fn receive_message_flags() -> libc::c_int {
    libc::MSG_CMSG_CLOEXEC
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
const fn receive_message_flags() -> libc::c_int {
    0
}

fn run_relay(path: &Path) -> Result<()> {
    let mut control = UnixStream::connect(path)
        .with_context(|| format!("failed to connect to tmux relay socket {}", path.display()))?;
    let master = receive_fd(&control)?;
    let _raw_terminal = RawTerminal::enter(libc::STDIN_FILENO)
        .context("failed to configure the tmux relay terminal")?;
    let mut last_size = None;
    sync_terminal_size(libc::STDIN_FILENO, master.as_raw_fd(), &mut last_size)?;
    control
        .write_all(&[RELAY_READY_TAG])
        .context("failed to complete the tmux relay handshake")?;
    drop(control);
    relay_terminal_io(master.as_raw_fd(), &mut last_size)
}

struct RawTerminal {
    fd: RawFd,
    original: libc::termios,
}

impl RawTerminal {
    fn enter(fd: RawFd) -> io::Result<Self> {
        // SAFETY: termios is a plain C value and tcgetattr initializes it on success.
        let mut original = unsafe { mem::zeroed::<libc::termios>() };
        // SAFETY: fd is an open terminal descriptor and original is writable.
        if unsafe { libc::tcgetattr(fd, &raw mut original) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = original;
        // SAFETY: raw is a fully initialized termios value.
        unsafe { libc::cfmakeraw(&raw mut raw) };
        // SAFETY: fd remains open and raw points to a valid termios value.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const raw) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, original })
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        // SAFETY: fd remains owned by the process and original was captured from it.
        let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &raw const self.original) };
    }
}

fn relay_terminal_io(master_fd: RawFd, last_size: &mut Option<libc::winsize>) -> Result<()> {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        sync_terminal_size(libc::STDIN_FILENO, master_fd, last_size)?;
        let mut descriptors = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let timeout = poll_timeout(RELAY_IO_POLL_INTERVAL);
        // SAFETY: descriptors points to initialized pollfd values for the duration of poll.
        let ready =
            unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, timeout) };
        if ready == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("tmux relay terminal poll failed");
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            let Some(read) = read_fd(libc::STDIN_FILENO, &mut buffer)? else {
                return Ok(());
            };
            write_all_fd(master_fd, &buffer[..read])?;
        }
        if descriptors[1].revents & libc::POLLIN != 0 {
            let Some(read) = read_fd(master_fd, &mut buffer)? else {
                return Ok(());
            };
            write_all_fd(libc::STDOUT_FILENO, &buffer[..read])?;
        }
        let closed = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if descriptors.iter().any(|descriptor| {
            descriptor.revents & closed != 0 && descriptor.revents & libc::POLLIN == 0
        }) {
            return Ok(());
        }
    }
}

fn poll_timeout(duration: Duration) -> libc::c_int {
    duration.as_millis().min(libc::c_int::MAX as u128) as libc::c_int
}

fn read_fd(fd: RawFd, buffer: &mut [u8]) -> io::Result<Option<usize>> {
    loop {
        // SAFETY: buffer is writable for its full length and fd is open while the relay runs.
        let read =
            unsafe { libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len()) };
        if read > 0 {
            return Ok(Some(read as usize));
        }
        if read == 0 {
            return Ok(None);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.raw_os_error() == Some(libc::EIO) {
            return Ok(None);
        }
        return Err(error);
    }
}

fn write_all_fd(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        // SAFETY: bytes is readable for its full length and fd is open while the relay runs.
        let written =
            unsafe { libc::write(fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len()) };
        if written > 0 {
            bytes = &bytes[written as usize..];
            continue;
        }
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "terminal relay write returned zero",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

fn terminal_size(fd: RawFd) -> io::Result<libc::winsize> {
    // SAFETY: winsize is a plain C value and ioctl initializes it on success.
    let mut size = unsafe { mem::zeroed::<libc::winsize>() };
    // SAFETY: fd is expected to be an open terminal and size is writable.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &raw mut size) } == -1 {
        return Err(io::Error::last_os_error());
    }
    size.ws_col = size.ws_col.max(1);
    size.ws_row = size.ws_row.max(1);
    Ok(size)
}

fn sync_terminal_size(
    terminal_fd: RawFd,
    master_fd: RawFd,
    last_size: &mut Option<libc::winsize>,
) -> io::Result<()> {
    let size = terminal_size(terminal_fd)?;
    if last_size
        .as_ref()
        .is_some_and(|previous| winsize_eq(previous, &size))
    {
        return Ok(());
    }
    // SAFETY: master_fd is an open pseudoterminal master and size is initialized. TIOCSWINSZ also
    // notifies the slave's foreground process group with SIGWINCH on supported Unix terminals.
    if unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ as _, &raw const size) } == -1 {
        return Err(io::Error::last_os_error());
    }
    *last_size = Some(size);
    Ok(())
}

fn winsize_eq(left: &libc::winsize, right: &libc::winsize) -> bool {
    left.ws_row == right.ws_row
        && left.ws_col == right.ws_col
        && left.ws_xpixel == right.ws_xpixel
        && left.ws_ypixel == right.ws_ypixel
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
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                anyhow!("`/tmux` requires tmux; install tmux and retry")
            } else {
                anyhow!(error)
            }
        })
}

fn tmux_create_arguments(
    name: &str,
    executable: &Path,
    cwd: &Path,
    endpoint: &str,
    (columns, rows): (u16, u16),
) -> Vec<OsString> {
    vec![
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
        "-x".into(),
        columns.max(1).to_string().into(),
        "-y".into(),
        rows.max(1).to_string().into(),
        "--".into(),
        tmux_literal(executable.as_os_str()),
        RELAY_COMMAND.into(),
        endpoint.into(),
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
    ]
}

/// tmux's argv parser uses an unescaped trailing semicolon as a command separator. Inserting one
/// backslash before it is tmux's literal representation; the parser removes that extra backslash
/// before passing the value to the pane command.
fn tmux_literal(value: &OsStr) -> OsString {
    let bytes = value.as_bytes();
    if !bytes.ends_with(b";") {
        return value.to_os_string();
    }
    let mut escaped = Vec::with_capacity(bytes.len() + 1);
    escaped.extend_from_slice(&bytes[..bytes.len() - 1]);
    escaped.extend_from_slice(b"\\;");
    OsString::from_vec(escaped)
}

fn tmux_session_id(output: &Output) -> Result<String> {
    let session_id = std::str::from_utf8(&output.stdout)
        .context("tmux returned a non-UTF-8 session identifier")?;
    let session_id = session_id.trim();
    validate_session_id(session_id)?;
    Ok(session_id.to_string())
}

fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id
        .strip_prefix('$')
        .is_none_or(|number| number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(anyhow!("tmux did not return a valid session ID"));
    }
    Ok(())
}

fn kill_session(target: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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
