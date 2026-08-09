//! Windows pipe process spawning.

use std::collections::HashMap;
use std::io;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::process::ChildTerminator;
use super::process::ProcessHandle;
use super::process::ProcessSignal;
use super::process::SpawnedProcess;
use super::process::exit_code_from_status;

enum WindowsChildTerminator {
    Job(Arc<super::win::JobObject>),
    Process(u32),
}

struct PipeChildTerminator {
    windows: WindowsChildTerminator,
}

impl ChildTerminator for PipeChildTerminator {
    fn signal(&mut self, signal: ProcessSignal) -> io::Result<()> {
        match signal {
            ProcessSignal::Interrupt => self.kill(),
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        match &self.windows {
            WindowsChildTerminator::Job(job) => job.terminate(),
            WindowsChildTerminator::Process(pid) => kill_process(*pid),
        }
    }
}

fn kill_process(pid: u32) -> io::Result<()> {
    unsafe {
        let handle = winapi::um::processthreadsapi::OpenProcess(
            winapi::um::winnt::PROCESS_TERMINATE,
            0,
            pid,
        );
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let success = winapi::um::processthreadsapi::TerminateProcess(handle, 1);
        let error = io::Error::last_os_error();
        winapi::um::handleapi::CloseHandle(handle);
        if success == 0 { Err(error) } else { Ok(()) }
    }
}

async fn read_output_stream<R>(mut reader: R, output_tx: mpsc::Sender<Vec<u8>>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0u8; 8_192];
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

#[derive(Clone, Copy)]
enum PipeStdinMode {
    #[cfg(test)]
    Piped,
    Null,
}

/// Process-tree containment is best-effort because Tokio returns only after
/// the root process starts, so job assignment cannot be atomic.
async fn spawn_process_with_stdin_mode(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    stdin_mode: PipeStdinMode,
) -> Result<SpawnedProcess> {
    if program.is_empty() {
        anyhow::bail!("missing program for pipe spawn");
    }

    let mut command = Command::new(program);
    command.current_dir(cwd);
    command.env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    for arg in args {
        command.arg(arg);
    }
    match stdin_mode {
        #[cfg(test)]
        PipeStdinMode::Piped => {
            command.stdin(Stdio::piped());
        }
        PipeStdinMode::Null => {
            command.stdin(Stdio::null());
        }
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let job = super::win::JobObject::create().map(Arc::new);
    let mut child = command.spawn()?;
    let windows_terminator = {
        // Accept the small race: a descendant created between spawn and
        // assignment is not guaranteed to join the job and can escape termination.
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("missing child pid"))?;
        let assigned_job = job.and_then(|job| {
            let process_handle = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("missing child process handle"))?;
            job.assign_process(process_handle)?;
            Ok(job)
        });
        match assigned_job {
            Ok(job) => WindowsChildTerminator::Job(job),
            Err(error) => {
                log::warn!(
                    "Windows pipe process tree containment unavailable for pid {pid}: {error}"
                );
                WindowsChildTerminator::Process(pid)
            }
        }
    };

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(128);
    let writer_handle = if let Some(stdin) = stdin {
        tokio::spawn(async move {
            let mut writer = stdin;
            while let Some(bytes) = writer_rx.recv().await {
                let _ = writer.write_all(&bytes).await;
                let _ = writer.flush().await;
            }
        })
    } else {
        drop(writer_rx);
        tokio::spawn(async {})
    };

    let stdout_handle = stdout.map(|stdout| {
        let stdout_tx = stdout_tx.clone();
        tokio::spawn(async move {
            read_output_stream(BufReader::new(stdout), stdout_tx).await;
        })
    });
    let stderr_handle = stderr.map(|stderr| {
        let stderr_tx = stderr_tx.clone();
        tokio::spawn(async move {
            read_output_stream(BufReader::new(stderr), stderr_tx).await;
        })
    });
    let mut reader_abort_handles = Vec::new();
    if let Some(handle) = stdout_handle.as_ref() {
        reader_abort_handles.push(handle.abort_handle());
    }
    if let Some(handle) = stderr_handle.as_ref() {
        reader_abort_handles.push(handle.abort_handle());
    }
    let reader_handle = tokio::spawn(async move {
        if let Some(handle) = stdout_handle {
            let _ = handle.await;
        }
        if let Some(handle) = stderr_handle {
            let _ = handle.await;
        }
    });

    let (exit_tx, exit_rx) = oneshot::channel::<i32>();
    let exit_status = Arc::new(AtomicBool::new(false));
    let wait_exit_status = Arc::clone(&exit_status);
    let exit_code = Arc::new(StdMutex::new(None));
    let wait_exit_code = Arc::clone(&exit_code);
    let wait_job = match &windows_terminator {
        WindowsChildTerminator::Job(job) => Some(Arc::clone(job)),
        WindowsChildTerminator::Process(_) => None,
    };
    let wait_handle: JoinHandle<()> = tokio::spawn(async move {
        let code = match child.wait().await {
            Ok(status) => {
                if let Some(job) = wait_job
                    && let Err(error) = job.preserve_descendants()
                {
                    log::warn!(
                        "Windows pipe failed to preserve descendants after root exit: {error}"
                    );
                }
                exit_code_from_status(status)
            }
            Err(_) => -1,
        };
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = wait_exit_code.lock() {
            *guard = Some(code);
        }
        let _ = exit_tx.send(code);
    });

    let handle = ProcessHandle::new(
        writer_tx,
        Box::new(PipeChildTerminator {
            windows: windows_terminator,
        }),
        reader_handle,
        reader_abort_handles,
        writer_handle,
        wait_handle,
        exit_status,
        exit_code,
        /*pty_handles*/ None,
    );

    Ok(SpawnedProcess {
        session: handle,
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}

/// Spawn a process with writable stdin for process-runtime tests.
#[cfg(test)]
pub async fn spawn_process(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<SpawnedProcess> {
    spawn_process_with_stdin_mode(program, args, cwd, env, PipeStdinMode::Piped).await
}

/// Spawn a process using regular pipes and close stdin immediately.
pub async fn spawn_process_no_stdin(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<SpawnedProcess> {
    spawn_process_with_stdin_mode(program, args, cwd, env, PipeStdinMode::Null).await
}

#[cfg(test)]
#[path = "pipe_tests.rs"]
mod tests;
