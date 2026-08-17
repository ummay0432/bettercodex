//! Focused non-PTY command runtime for the direct `bash` tool.
//!
//! Process-group setup, parent-death handling, and macOS's process-group
//! fallback retain the corresponding current Codex behavior. Descriptor cleanup
//! remains fork-safe on every supported platform. The stateful PTY, stdin,
//! polling, and background-session protocol is deliberately absent.

use anyhow::Context;
use anyhow::Result;
use std::collections::VecDeque;
use std::io;
use std::io::ErrorKind;
#[cfg(target_os = "macos")]
use std::os::fd::RawFd;
use std::path::Path;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const READ_CHUNK_BYTES: usize = 8 * 1024;
const RETAINED_HEAD_BYTES: usize = 64 * 1024;
const RETAINED_TAIL_BYTES: usize = 64 * 1024;
const IO_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const CANCELLATION_TERMINATION_GRACE_PERIOD: Duration = Duration::from_millis(50);
const TIMEOUT_EXIT_CODE: i32 = 124;
const CANCELLATION_EXIT_CODE: i32 = 130;

const COMMAND_ENV: [(&str, &str); 10] = [
    ("NO_COLOR", "1"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("CODEX_CI", "1"),
    ("TERM", "dumb"),
];
const NON_INHERITABLE_ENV_VARS: [&str; 4] = [
    "CODEX_EXEC_SERVER_NOISE_AUTH_TOKEN",
    "OPENAI_FEDERATION_RULE_ID",
    "OPENAI_IDENTITY_TOKEN_FILE",
    "OPENAI_WORKLOAD_IDENTITY_CONTEXT",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveOutputAction {
    Continue,
    Stop,
}

/// Fits one decoded stream chunk into a cumulative live-output byte budget.
///
/// Returns whether any bytes from this chunk were omitted. Reaching the budget exactly is not an
/// omission; a later non-empty chunk will report the overflow instead.
pub(crate) fn fit_live_output_budget(
    chunk: &mut String,
    forwarded_bytes: &mut usize,
    maximum_bytes: usize,
) -> bool {
    let remaining = maximum_bytes.saturating_sub(*forwarded_bytes);
    let omitted = chunk.len() > remaining;
    if omitted {
        let mut end = remaining;
        while !chunk.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        chunk.truncate(end);
    }
    *forwarded_bytes = (*forwarded_bytes).saturating_add(chunk.len());
    omitted
}

pub(crate) struct CommandOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_code: i32,
}

enum Completion {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

struct ProcessGuard {
    process_group_id: u32,
    armed: bool,
}

impl ProcessGuard {
    fn new(process_group_id: u32) -> Self {
        Self {
            process_group_id,
            armed: true,
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        signal_process_group_reliably(self.process_group_id, libc::SIGKILL)?;
        self.armed = false;
        Ok(())
    }

    fn terminate(&self) -> io::Result<()> {
        signal_process_group_reliably(self.process_group_id, libc::SIGTERM)
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

struct OutputRead {
    stream: OutputStream,
    result: io::Result<usize>,
}

struct OutputCapture {
    buffers: [[u8; READ_CHUNK_BYTES]; 2],
    open: [bool; 2],
    retained: [BoundedBytes; 2],
    decoders: [Utf8StreamDecoder; 2],
}

impl Default for OutputCapture {
    fn default() -> Self {
        Self {
            buffers: [[0; READ_CHUNK_BYTES]; 2],
            open: [true; 2],
            retained: [BoundedBytes::default(), BoundedBytes::default()],
            decoders: [Utf8StreamDecoder::default(), Utf8StreamDecoder::default()],
        }
    }
}

/// Runs one non-interactive Bash command and waits for its process tree to finish.
///
/// Output is retained with a fixed head/tail memory bound. When present,
/// `on_output` receives valid UTF-8 chunks as soon as either pipe produces them;
/// invalid bytes are replaced only once their invalidity is known. Returning
/// [`LiveOutputAction::Stop`] disables further live-output work without affecting
/// the retained command output.
pub(crate) async fn run_bash(
    command: &str,
    cwd: &Path,
    timeout: Option<Duration>,
    cancellation: CancellationToken,
    on_output: Option<&mut (dyn FnMut(OutputStream, String) -> LiveOutputAction + Send)>,
) -> Result<CommandOutput> {
    let shell = bash_path()?;
    run_shell(
        command,
        cwd,
        timeout,
        cancellation,
        on_output,
        &shell,
        ShellStartup::NonLogin,
    )
    .await
}

/// Runs an operator command with the detected user shell's login environment.
pub(crate) async fn run_user_shell(
    command: &str,
    cwd: &Path,
    cancellation: CancellationToken,
    on_output: Option<&mut (dyn FnMut(OutputStream, String) -> LiveOutputAction + Send)>,
) -> Result<CommandOutput> {
    let shell = crate::shell_command::shell_detect::default_user_shell();
    run_shell(
        command,
        cwd,
        None,
        cancellation,
        on_output,
        &shell.shell_path,
        ShellStartup::Login,
    )
    .await
}

async fn run_shell(
    command: &str,
    cwd: &Path,
    timeout: Option<Duration>,
    cancellation: CancellationToken,
    mut on_output: Option<&mut (dyn FnMut(OutputStream, String) -> LiveOutputAction + Send)>,
    shell: &Path,
    startup: ShellStartup,
) -> Result<CommandOutput> {
    if cancellation.is_cancelled() {
        return Ok(CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: CANCELLATION_EXIT_CODE,
        });
    }

    let (mut child, mut process) = spawn(shell, startup, command, cwd)
        .with_context(|| format!("failed to start Bash command in {}", cwd.display()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("spawned Bash process has no stdout pipe"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("spawned Bash process has no stderr pipe"))?;

    let mut captured = OutputCapture::default();

    let completion = {
        let wait = child.wait();
        tokio::pin!(wait);
        let expiration = async {
            match timeout {
                Some(timeout) => tokio::time::sleep(timeout).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(expiration);

        loop {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => break Completion::Cancelled,
                _ = &mut expiration => break Completion::TimedOut,
                status = &mut wait => break Completion::Exited(status.context("failed to wait for Bash command")?),
                output = captured.read_next(&mut stdout, &mut stderr) => {
                    captured.handle_read(output, &mut on_output)?;
                }
            }
        }
    };

    match completion {
        Completion::Exited(_) => {
            // There is no background-process protocol. Killing the group after the shell
            // exits also closes pipes inherited by an accidental `command &` descendant.
            process
                .kill()
                .context("failed to terminate Bash process tree")?;
        }
        Completion::TimedOut => {
            kill_and_reap(&mut child, &mut process).await?;
        }
        Completion::Cancelled => {
            // Match current Codex cancellation behavior: let TERM-aware commands perform
            // cleanup briefly, then kill any survivors in the original process group.
            process
                .terminate()
                .context("failed to interrupt Bash process tree")?;
            let graceful_exit = async {
                loop {
                    tokio::select! {
                        biased;
                        status = child.wait() => return status
                            .context("failed to reap interrupted Bash command")
                            .map(|_| ()),
                        output = captured.read_next(&mut stdout, &mut stderr) => {
                            captured.handle_read(output, &mut on_output)?;
                        }
                    }
                }
            };
            match tokio::time::timeout(CANCELLATION_TERMINATION_GRACE_PERIOD, graceful_exit).await {
                Ok(result) => {
                    result?;
                    process
                        .kill()
                        .context("failed to terminate interrupted Bash process tree")?;
                }
                Err(_) => kill_and_reap(&mut child, &mut process).await?,
            }
        }
    }

    let drain = async {
        while captured.is_open() {
            let output = captured.read_next(&mut stdout, &mut stderr).await;
            captured.handle_read(output, &mut on_output)?;
        }
        Ok::<(), anyhow::Error>(())
    };
    if let Ok(result) = tokio::time::timeout(IO_DRAIN_TIMEOUT, drain).await {
        result?;
    }

    let (stdout, stderr) = captured.finish(&mut on_output);
    let exit_code = match completion {
        Completion::Exited(status) => exit_code_from_status(status),
        Completion::TimedOut => TIMEOUT_EXIT_CODE,
        Completion::Cancelled => CANCELLATION_EXIT_CODE,
    };
    Ok(CommandOutput {
        stdout,
        stderr,
        exit_code,
    })
}

async fn kill_and_reap(
    child: &mut tokio::process::Child,
    process: &mut ProcessGuard,
) -> Result<()> {
    let process_group_result = process.kill();
    let child_result = child.kill().await;
    process_group_result.context("failed to terminate Bash process tree")?;
    child_result.context("failed to terminate and reap Bash command")?;
    Ok(())
}

impl OutputCapture {
    async fn read_next<Stdout, Stderr>(
        &mut self,
        stdout: &mut Stdout,
        stderr: &mut Stderr,
    ) -> OutputRead
    where
        Stdout: AsyncRead + Unpin,
        Stderr: AsyncRead + Unpin,
    {
        if !self.is_open() {
            return std::future::pending().await;
        }
        let [stdout_buffer, stderr_buffer] = &mut self.buffers;
        tokio::select! {
            result = stdout.read(stdout_buffer), if self.open[0] => OutputRead {
                stream: OutputStream::Stdout,
                result,
            },
            result = stderr.read(stderr_buffer), if self.open[1] => OutputRead {
                stream: OutputStream::Stderr,
                result,
            },
        }
    }

    fn handle_read(
        &mut self,
        output: OutputRead,
        on_output: &mut Option<&mut (dyn FnMut(OutputStream, String) -> LiveOutputAction + Send)>,
    ) -> Result<()> {
        let OutputRead { stream, result } = output;
        let index = stream_index(stream);
        let read = match result {
            Err(error) if error.kind() == ErrorKind::Interrupted => return Ok(()),
            result => result,
        }
        .with_context(|| {
            format!(
                "failed to read Bash {}",
                match stream {
                    OutputStream::Stdout => "stdout",
                    OutputStream::Stderr => "stderr",
                }
            )
        })?;
        if read == 0 {
            self.open[index] = false;
            return Ok(());
        }

        let bytes = &self.buffers[index][..read];
        self.retained[index].append(bytes);
        let Some(callback) = on_output.as_mut() else {
            return Ok(());
        };
        let Some(text) = self.decoders[index].append(bytes) else {
            return Ok(());
        };
        if callback(stream, text) == LiveOutputAction::Stop {
            *on_output = None;
            for decoder in &mut self.decoders {
                decoder.discard();
            }
        }
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.open.into_iter().any(|open| open)
    }

    fn finish(
        mut self,
        on_output: &mut Option<&mut (dyn FnMut(OutputStream, String) -> LiveOutputAction + Send)>,
    ) -> (String, String) {
        for stream in [OutputStream::Stdout, OutputStream::Stderr] {
            let Some(text) = self.decoders[stream_index(stream)].finish() else {
                continue;
            };
            if on_output
                .as_mut()
                .is_some_and(|callback| callback(stream, text) == LiveOutputAction::Stop)
            {
                *on_output = None;
            }
        }
        let [stdout, stderr] = self.retained;
        (captured_text(stdout), captured_text(stderr))
    }
}

fn stream_index(stream: OutputStream) -> usize {
    match stream {
        OutputStream::Stdout => 0,
        OutputStream::Stderr => 1,
    }
}

#[derive(Clone, Copy)]
enum ShellStartup {
    Login,
    NonLogin,
}

fn configured_command(shell: &Path, startup: ShellStartup, source: &str, cwd: &Path) -> Command {
    let mut command = Command::new(shell);
    command
        .arg(match startup {
            ShellStartup::Login => "-lc",
            ShellStartup::NonLogin => "-c",
        })
        .arg(source)
        .current_dir(cwd)
        .env_clear()
        .envs(std::env::vars_os().filter(|(name, _)| {
            !name.to_str().is_some_and(|name| {
                NON_INHERITABLE_ENV_VARS
                    .iter()
                    .any(|restricted| restricted.eq_ignore_ascii_case(name))
            })
        }))
        .envs(COMMAND_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

fn spawn(
    shell: &Path,
    startup: ShellStartup,
    source: &str,
    cwd: &Path,
) -> Result<(tokio::process::Child, ProcessGuard)> {
    let mut command = configured_command(shell, startup, source, cwd);
    #[cfg(target_os = "linux")]
    let parent_pid = unsafe { libc::getpid() };
    #[cfg(target_os = "linux")]
    let file_descriptor_limit = file_descriptor_limit();
    unsafe {
        command.pre_exec(move || {
            detach_from_tty()?;
            #[cfg(target_os = "linux")]
            {
                set_parent_death_signal(parent_pid)?;
                close_inherited_fds(file_descriptor_limit);
            }
            #[cfg(target_os = "macos")]
            close_inherited_fds();
            Ok(())
        });
    }
    let child = command.spawn()?;
    let process_group_id = child
        .id()
        .ok_or_else(|| io::Error::other("spawned Bash process has no PID"))?;
    Ok((child, ProcessGuard::new(process_group_id)))
}

fn bash_path() -> Result<std::path::PathBuf> {
    crate::shell_command::shell_detect::get_shell(
        crate::shell_command::shell_detect::ShellType::Bash,
        None,
    )
    .map(|shell| shell.shell_path)
    .ok_or_else(|| {
        anyhow::anyhow!("Bash was not found; install Bash and make it available on PATH")
    })
}

#[derive(Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    fn append(&mut self, bytes: &[u8]) -> Option<String> {
        if self.pending.is_empty() {
            return decode_utf8_prefix(bytes, &mut self.pending, false);
        }

        let mut combined = std::mem::take(&mut self.pending);
        combined.extend_from_slice(bytes);
        decode_utf8_prefix(&combined, &mut self.pending, false)
    }

    fn finish(&mut self) -> Option<String> {
        let pending = std::mem::take(&mut self.pending);
        decode_utf8_prefix(&pending, &mut self.pending, true)
    }

    fn discard(&mut self) {
        self.pending.clear();
    }
}

fn decode_utf8_prefix(bytes: &[u8], pending: &mut Vec<u8>, final_chunk: bool) -> Option<String> {
    debug_assert!(pending.is_empty());
    let mut output = String::new();
    let mut remaining = bytes;
    loop {
        match std::str::from_utf8(remaining) {
            Ok(text) => {
                output.push_str(text);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    // SAFETY: `Utf8Error::valid_up_to` guarantees that this prefix is UTF-8.
                    output.push_str(unsafe { std::str::from_utf8_unchecked(&remaining[..valid]) });
                    remaining = &remaining[valid..];
                    continue;
                }
                match error.error_len() {
                    Some(length) => {
                        output.push('\u{fffd}');
                        remaining = &remaining[length..];
                    }
                    None if final_chunk => {
                        output.push('\u{fffd}');
                        break;
                    }
                    None => {
                        pending.extend_from_slice(remaining);
                        break;
                    }
                }
            }
        }
    }
    (!output.is_empty()).then_some(output)
}

fn captured_text(retained: BoundedBytes) -> String {
    String::from_utf8_lossy(&retained.into_bytes()).into_owned()
}

#[derive(Default)]
struct BoundedBytes {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted: usize,
}

impl BoundedBytes {
    fn append(&mut self, mut bytes: &[u8]) {
        let head_bytes = RETAINED_HEAD_BYTES
            .saturating_sub(self.head.len())
            .min(bytes.len());
        self.head.extend_from_slice(&bytes[..head_bytes]);
        bytes = &bytes[head_bytes..];
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
        let excess = self
            .tail
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(RETAINED_TAIL_BYTES);
        if excess > 0 {
            drop(self.tail.drain(..excess));
            self.omitted = self.omitted.saturating_add(excess);
        }
        self.tail.extend(bytes);
    }

    fn into_bytes(mut self) -> Vec<u8> {
        if self.omitted > 0 {
            self.head.extend_from_slice(
                format!("\n... {} bytes omitted ...\n", self.omitted).as_bytes(),
            );
        }
        self.head.extend(self.tail);
        self.head
    }
}

fn exit_code_from_status(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(-1)
}

// macOS needs a fork-safe sweep because std filesystem operations are not safe
// after fork in a multithreaded process.
#[cfg(target_os = "macos")]
fn close_inherited_fds() {
    let mut descriptors = [libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    }; 1024];
    // SAFETY: proc_pidinfo writes descriptor records into the stack buffer.
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            descriptors.as_mut_ptr().cast(),
            std::mem::size_of_val(&descriptors) as libc::c_int,
        )
    };
    let close_inheritable = |fd| {
        if fd <= libc::STDERR_FILENO {
            return;
        }
        // std::process keeps a CLOEXEC pipe open until exec to report spawn errors.
        // SAFETY: fcntl and close only operate on a descriptor owned by this process.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::close(fd);
            }
        }
    };
    if bytes > 0 && (bytes as usize) < std::mem::size_of_val(&descriptors) {
        let count = bytes as usize / std::mem::size_of::<libc::proc_fdinfo>();
        for descriptor in descriptors.iter().take(count) {
            close_inheritable(descriptor.proc_fd);
        }
        return;
    }

    // SAFETY: proc_pidinfo accepts a null buffer when its size is zero.
    let descriptor_table_bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    if descriptor_table_bytes > 0 {
        let upper_bound =
            descriptor_table_bytes as usize / std::mem::size_of::<libc::proc_fdinfo>();
        for fd in libc::STDERR_FILENO + 1..upper_bound as RawFd {
            close_inheritable(fd);
        }
        return;
    }

    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes into the stack-owned resource-limit structure.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } == 0 {
        let upper_bound = limit.rlim_cur.min(RawFd::MAX as _) as RawFd;
        for fd in libc::STDERR_FILENO + 1..upper_bound {
            close_inheritable(fd);
        }
    }
}

#[cfg(target_os = "linux")]
fn file_descriptor_limit() -> libc::c_int {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes into the stack-owned resource-limit structure.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return 0;
    }
    limit.rlim_max.min(libc::c_int::MAX as _) as libc::c_int
}

#[cfg(target_os = "linux")]
fn close_inherited_fds(file_descriptor_limit: libc::c_int) {
    // Mark every non-stdio descriptor CLOEXEC in one syscall. Walking `/dev/fd`
    // here would allocate and use filesystem APIs after fork, which is not safe
    // in a multithreaded process. CLOEXEC preserves std's exec-error pipe until
    // exec while preventing intentionally inheritable parent descriptors from
    // reaching Bash.
    // SAFETY: close_range only changes descriptor flags in the forked child.
    let closed = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            (libc::STDERR_FILENO + 1) as libc::c_uint,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    } == 0;
    if closed {
        return;
    }

    // CLOSE_RANGE_CLOEXEC arrived after close_range itself and either operation
    // can also be blocked by the invoking environment. Fall back to fork-safe
    // fcntl calls rather than silently exposing inheritable parent descriptors.
    // The limit was captured before fork because getrlimit is not guaranteed to
    // be async-signal-safe by POSIX.
    for fd in libc::STDERR_FILENO + 1..file_descriptor_limit {
        // SAFETY: fcntl only changes descriptor flags in the forked child.
        // Closed descriptor slots return EBADF and are skipped.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

fn detach_from_tty() -> io::Result<()> {
    if unsafe { libc::setsid() } != -1 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EPERM) {
        return Err(error);
    }
    if unsafe { libc::setpgid(0, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn set_parent_death_signal(parent_pid: libc::pid_t) -> io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::getppid() } != parent_pid {
        unsafe {
            libc::raise(libc::SIGTERM);
        }
    }
    Ok(())
}

fn signal_process_group(process_group_id: u32, signal: libc::c_int) -> io::Result<bool> {
    if unsafe { libc::killpg(process_group_id as libc::pid_t, signal) } != -1 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error)
    }
}

fn signal_process_group_reliably(process_group_id: u32, signal: libc::c_int) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        signal_process_group_with_member_fallback(process_group_id, signal).map(|_| ())
    }
    #[cfg(not(target_os = "macos"))]
    {
        signal_process_group(process_group_id, signal).map(|_| ())
    }
}

#[cfg(target_os = "macos")]
fn signal_process_group_with_member_fallback(
    process_group_id: u32,
    signal: libc::c_int,
) -> io::Result<bool> {
    let process_group_id = libc::pid_t::try_from(process_group_id)
        .ok()
        .filter(|process_group_id| *process_group_id > 0)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "invalid process group ID"))?;
    match signal_process_group(process_group_id as u32, signal) {
        Err(error) if error.kind() == ErrorKind::PermissionDenied => {}
        result => return result,
    }

    let mut process_ids: Vec<libc::pid_t> = vec![0; 16];
    loop {
        let buffer_size = libc::c_int::try_from(std::mem::size_of_val(process_ids.as_slice()))
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "process group is too large"))?;
        let count = unsafe {
            libc::proc_listpgrppids(
                process_group_id,
                process_ids.as_mut_ptr().cast(),
                buffer_size,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        let count = count as usize;
        if count < process_ids.len() {
            process_ids.truncate(count);
            break;
        }
        let capacity = process_ids
            .len()
            .checked_mul(2)
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "process group is too large"))?;
        process_ids.resize(capacity, 0);
    }
    process_ids.sort_unstable_by_key(|process_id| *process_id == process_group_id);

    let mut signalled = false;
    let mut first_error = None;
    for process_id in process_ids.into_iter().filter(|process_id| *process_id > 0) {
        let current_group_id = unsafe { libc::getpgid(process_id) };
        if current_group_id == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) && first_error.is_none() {
                first_error = Some(error);
            }
            continue;
        }
        if current_group_id != process_group_id {
            continue;
        }
        if unsafe { libc::kill(process_id, signal) } == 0 {
            signalled = true;
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) && first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    if signalled {
        Ok(true)
    } else {
        first_error.map_or(Ok(false), Err)
    }
}

#[cfg(test)]
#[path = "process_runtime_tests.rs"]
mod tests;
