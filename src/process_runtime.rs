//! Focused Unix process runtime retained from OpenAI Codex's `codex-utils-pty` at
//! `646f7c0a91b8e327d263335da68ae8ef212895ce`.
//!
//! bettercodex supports Linux and macOS, so it uses Codex's direct Unix PTY path instead of
//! fetching the full Codex repository for its cross-platform process crate and `portable-pty`.

use anyhow::Result;
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::io::ErrorKind;
use std::os::fd::FromRawFd;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command as StdCommand;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessSignal {
    Interrupt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalSize {
    pub(crate) rows: u16,
    pub(crate) cols: u16,
}

pub(crate) struct SpawnedProcess {
    pub(crate) session: ProcessHandle,
    pub(crate) stdout_rx: mpsc::Receiver<Vec<u8>>,
    pub(crate) stderr_rx: mpsc::Receiver<Vec<u8>>,
    pub(crate) exit_rx: oneshot::Receiver<i32>,
}

trait ChildTerminator: Send + Sync {
    fn signal(&mut self, signal: ProcessSignal) -> io::Result<()>;

    fn kill(&mut self) -> io::Result<()>;
}

pub(crate) struct ProcessHandle {
    writer_tx: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    killer: Mutex<Option<Box<dyn ChildTerminator>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    reader_abort_handles: Mutex<Vec<AbortHandle>>,
    writer_handle: Mutex<Option<JoinHandle<()>>>,
    wait_handle: Mutex<Option<JoinHandle<()>>>,
    exit_status: Arc<AtomicBool>,
    // Keep the PTY master alive until the process handle is dropped. Closing it early sends the
    // child a hangup and can truncate output that is still in flight.
    _pty_master: Option<File>,
}

impl ProcessHandle {
    #[allow(clippy::too_many_arguments)]
    fn new(
        writer_tx: mpsc::Sender<Vec<u8>>,
        killer: Box<dyn ChildTerminator>,
        reader_handle: JoinHandle<()>,
        reader_abort_handles: Vec<AbortHandle>,
        writer_handle: JoinHandle<()>,
        wait_handle: JoinHandle<()>,
        exit_status: Arc<AtomicBool>,
        pty_master: Option<File>,
    ) -> Self {
        Self {
            writer_tx: Mutex::new(Some(writer_tx)),
            killer: Mutex::new(Some(killer)),
            reader_handle: Mutex::new(Some(reader_handle)),
            reader_abort_handles: Mutex::new(reader_abort_handles),
            writer_handle: Mutex::new(Some(writer_handle)),
            wait_handle: Mutex::new(Some(wait_handle)),
            exit_status,
            _pty_master: pty_master,
        }
    }

    pub(crate) fn writer_sender(&self) -> mpsc::Sender<Vec<u8>> {
        if let Ok(writer_tx) = self.writer_tx.lock()
            && let Some(writer_tx) = writer_tx.as_ref()
        {
            return writer_tx.clone();
        }

        let (writer_tx, writer_rx) = mpsc::channel(1);
        drop(writer_rx);
        writer_tx
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.exit_status.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Kill the owned process group while leaving readers alive to drain output through EOF.
    pub(crate) fn request_terminate(&self) {
        if let Ok(mut killer) = self.killer.lock()
            && let Some(mut killer) = killer.take()
        {
            let _ = killer.kill();
        }
    }

    pub(crate) fn signal(&self, signal: ProcessSignal) -> io::Result<()> {
        let Ok(mut killer) = self.killer.lock() else {
            return Ok(());
        };
        let Some(killer) = killer.as_mut() else {
            return Ok(());
        };
        killer.signal(signal)
    }

    pub(crate) fn terminate(&self) {
        self.request_terminate();

        if let Ok(mut handle) = self.reader_handle.lock()
            && let Some(handle) = handle.take()
        {
            handle.abort();
        }
        if let Ok(mut handles) = self.reader_abort_handles.lock() {
            for handle in handles.drain(..) {
                handle.abort();
            }
        }
        if let Ok(mut handle) = self.writer_handle.lock()
            && let Some(handle) = handle.take()
        {
            handle.abort();
        }
        if let Ok(mut handle) = self.wait_handle.lock()
            && let Some(handle) = handle.take()
        {
            // Dropping a Tokio join handle detaches the task. It must keep running to reap the
            // child, including when its blocking waiter was queued behind other work.
            drop(handle);
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        self.terminate();
    }
}

struct ProcessGroupTerminator {
    process_group_id: u32,
    macos_member_fallback: bool,
}

impl ChildTerminator for ProcessGroupTerminator {
    fn signal(&mut self, signal: ProcessSignal) -> io::Result<()> {
        match signal {
            ProcessSignal::Interrupt => interrupt_process_group(self.process_group_id),
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        if self.macos_member_fallback {
            return kill_process_group_with_member_fallback(self.process_group_id);
        }

        let _ = self.macos_member_fallback;
        kill_process_group(self.process_group_id)
    }
}

async fn read_output_stream<R>(mut reader: R, output_tx: mpsc::Sender<Vec<u8>>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0_u8; 8_192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let _ = output_tx.send(buffer[..read].to_vec()).await;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

pub(crate) async fn spawn_pipe_process_no_stdin(
    program: &str,
    arguments: &[String],
    cwd: &Path,
    environment: &HashMap<String, String>,
) -> Result<SpawnedProcess> {
    if program.is_empty() {
        anyhow::bail!("missing program for pipe spawn");
    }

    let mut command = Command::new(program);
    #[cfg(target_os = "linux")]
    let parent_pid = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            detach_from_tty()?;
            #[cfg(target_os = "linux")]
            set_parent_death_signal(parent_pid)?;
            close_inherited_fds();
            Ok(())
        });
    }
    command.current_dir(cwd);
    command.env_clear();
    command.envs(environment);
    command.args(arguments);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn()?;
    let process_group_id = child
        .id()
        .ok_or_else(|| io::Error::other("spawned process has no PID"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (writer_tx, writer_rx) = mpsc::channel::<Vec<u8>>(1);
    drop(writer_rx);
    let writer_handle = tokio::spawn(async {});
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(128);
    let stdout_handle = stdout.map(|stdout| {
        tokio::spawn(async move {
            read_output_stream(tokio::io::BufReader::new(stdout), stdout_tx).await;
        })
    });
    let stderr_handle = stderr.map(|stderr| {
        tokio::spawn(async move {
            read_output_stream(tokio::io::BufReader::new(stderr), stderr_tx).await;
        })
    });
    let reader_abort_handles = stdout_handle
        .as_ref()
        .into_iter()
        .chain(stderr_handle.as_ref())
        .map(JoinHandle::abort_handle)
        .collect();
    let reader_handle = tokio::spawn(async move {
        if let Some(handle) = stdout_handle {
            let _ = handle.await;
        }
        if let Some(handle) = stderr_handle {
            let _ = handle.await;
        }
    });

    let (exit_tx, exit_rx) = oneshot::channel();
    let exit_status = Arc::new(AtomicBool::new(false));
    let wait_exit_status = Arc::clone(&exit_status);
    let wait_handle = tokio::spawn(async move {
        let code = child.wait().await.map(exit_code_from_status).unwrap_or(-1);
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = exit_tx.send(code);
    });
    let session = ProcessHandle::new(
        writer_tx,
        Box::new(ProcessGroupTerminator {
            process_group_id,
            macos_member_fallback: true,
        }),
        reader_handle,
        reader_abort_handles,
        writer_handle,
        wait_handle,
        exit_status,
        None,
    );

    Ok(SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}

pub(crate) async fn spawn_pty_process(
    program: &str,
    arguments: &[String],
    cwd: &Path,
    environment: &HashMap<String, String>,
    size: TerminalSize,
) -> Result<SpawnedProcess> {
    if program.is_empty() {
        anyhow::bail!("missing program for PTY spawn");
    }

    let (master, slave) = open_unix_pty(size)?;
    let mut command = StdCommand::new(program);
    command.current_dir(cwd);
    command.env_clear();
    command.envs(environment);
    command.args(arguments);
    let stdin = slave.try_clone()?;
    let stdout = slave.try_clone()?;
    let stderr = slave.try_clone()?;
    unsafe {
        command
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .pre_exec(move || {
                for signal in [
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGALRM,
                ] {
                    libc::signal(signal, libc::SIG_DFL);
                }

                let empty_set: libc::sigset_t = std::mem::zeroed();
                libc::sigprocmask(libc::SIG_SETMASK, &empty_set, std::ptr::null_mut());
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                close_inherited_fds();
                Ok(())
            });
    }

    let mut child = command.spawn()?;
    drop(slave);
    let process_group_id = child.id();
    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(1);
    drop(stderr_tx);

    let mut reader = master.try_clone()?;
    let reader_handle = tokio::task::spawn_blocking(move || {
        use std::io::Read;

        let mut buffer = [0_u8; 8_192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let _ = stdout_tx.blocking_send(buffer[..read].to_vec());
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    let writer = Arc::new(tokio::sync::Mutex::new(master.try_clone()?));
    let writer_handle = tokio::spawn(async move {
        use std::io::Write;

        while let Some(bytes) = writer_rx.recv().await {
            let mut writer = writer.lock().await;
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    });
    let (exit_tx, exit_rx) = oneshot::channel();
    let exit_status = Arc::new(AtomicBool::new(false));
    let wait_exit_status = Arc::clone(&exit_status);
    let wait_handle = tokio::task::spawn_blocking(move || {
        let code = child.wait().map(exit_code_from_status).unwrap_or(-1);
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = exit_tx.send(code);
    });
    let session = ProcessHandle::new(
        writer_tx,
        Box::new(ProcessGroupTerminator {
            process_group_id,
            macos_member_fallback: false,
        }),
        reader_handle,
        Vec::new(),
        writer_handle,
        wait_handle,
        exit_status,
        Some(master),
    );

    Ok(SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}

fn exit_code_from_status(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    use std::os::unix::process::ExitStatusExt;
    status.signal().map_or(-1, |signal| 128 + signal)
}

fn open_unix_pty(size: TerminalSize) -> Result<(File, File)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut size = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(size),
        )
    };
    if result != 0 {
        anyhow::bail!("failed to open PTY: {}", io::Error::last_os_error());
    }
    set_cloexec(master)?;
    set_cloexec(slave)?;
    Ok(unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) })
}

fn set_cloexec(file_descriptor: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(file_descriptor, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(file_descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn close_inherited_fds() {
    let Ok(directory) = std::fs::read_dir("/dev/fd") else {
        return;
    };
    let mut file_descriptors = Vec::new();
    for entry in directory.flatten() {
        let file_descriptor = entry
            .file_name()
            .into_string()
            .ok()
            .and_then(|name| name.parse::<RawFd>().ok());
        let Some(file_descriptor) = file_descriptor else {
            continue;
        };
        if file_descriptor <= 2 {
            continue;
        }
        // Keep CLOEXEC descriptors open so std::process can still use its internal exec-error
        // pipe to report spawn failures.
        let flags = unsafe { libc::fcntl(file_descriptor, libc::F_GETFD) };
        if flags == -1 || flags & libc::FD_CLOEXEC != 0 {
            continue;
        }
        file_descriptors.push(file_descriptor);
    }
    for file_descriptor in file_descriptors {
        unsafe {
            libc::close(file_descriptor);
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

fn interrupt_process_group(process_group_id: u32) -> io::Result<()> {
    signal_process_group(process_group_id, libc::SIGINT).map(|_| ())
}

fn kill_process_group(process_group_id: u32) -> io::Result<()> {
    signal_process_group(process_group_id, libc::SIGKILL).map(|_| ())
}

#[cfg(target_os = "macos")]
fn kill_process_group_with_member_fallback(process_group_id: u32) -> io::Result<()> {
    match signal_process_group(process_group_id, libc::SIGKILL) {
        Err(error) if error.kind() == ErrorKind::PermissionDenied => {}
        result => return result.map(|_| ()),
    }

    let process_group_id = libc::pid_t::try_from(process_group_id)
        .ok()
        .filter(|process_group_id| *process_group_id > 0)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "invalid process group ID"))?;
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
    for process_id in process_ids {
        if process_id <= 0 {
            continue;
        }
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
        if unsafe { libc::kill(process_id, libc::SIGKILL) } == 0 {
            signalled = true;
            continue;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) && first_error.is_none() {
            first_error = Some(error);
        }
    }
    if signalled {
        Ok(())
    } else {
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment() -> HashMap<String, String> {
        std::env::vars().collect()
    }

    #[tokio::test]
    async fn pipe_keeps_stdout_and_stderr_split() {
        let cwd = std::env::current_dir().unwrap();
        let spawned = spawn_pipe_process_no_stdin(
            "/bin/sh",
            &["-c".to_string(), "printf out; printf err >&2".to_string()],
            &cwd,
            &environment(),
        )
        .await
        .unwrap();
        let SpawnedProcess {
            session: _session,
            mut stdout_rx,
            mut stderr_rx,
            exit_rx,
        } = spawned;
        let stdout = tokio::spawn(async move {
            let mut output = Vec::new();
            while let Some(bytes) = stdout_rx.recv().await {
                output.extend(bytes);
            }
            output
        });
        let stderr = tokio::spawn(async move {
            let mut output = Vec::new();
            while let Some(bytes) = stderr_rx.recv().await {
                output.extend(bytes);
            }
            output
        });
        assert_eq!(exit_rx.await.unwrap(), 0);
        assert_eq!(stdout.await.unwrap(), b"out");
        assert_eq!(stderr.await.unwrap(), b"err");
    }

    #[tokio::test]
    async fn pty_runs_with_a_controlling_terminal() {
        let cwd = std::env::current_dir().unwrap();
        let spawned = spawn_pty_process(
            "/bin/sh",
            &[
                "-c".to_string(),
                "test -t 0 && test -t 1 && printf pty-ready".to_string(),
            ],
            &cwd,
            &environment(),
            TerminalSize { rows: 24, cols: 80 },
        )
        .await
        .unwrap();
        let SpawnedProcess {
            session: _session,
            mut stdout_rx,
            stderr_rx: _stderr_rx,
            exit_rx,
        } = spawned;
        assert_eq!(exit_rx.await.unwrap(), 0);
        let mut output = Vec::new();
        while let Some(bytes) = stdout_rx.recv().await {
            output.extend(bytes);
        }
        assert!(String::from_utf8_lossy(&output).contains("pty-ready"));
    }
}
