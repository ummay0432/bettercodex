//! Windows ConPTY process spawning.
//!
//! This module is only compiled from the Windows process runtime. Keeping the
//! unreachable Unix PTY and inherited-file-descriptor backend here made every
//! Windows build parse and discard a second implementation.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Result;
use portable_pty::CommandBuilder;
use portable_pty::PtySystem as _;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::process::ChildTerminator;
use super::process::ProcessHandle;
use super::process::ProcessSignal;
use super::process::PtyHandles;
use super::process::SpawnedProcess;
use super::process::TerminalSize;

struct PtyChildTerminator {
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
}

impl ChildTerminator for PtyChildTerminator {
    fn signal(&mut self, signal: ProcessSignal) -> std::io::Result<()> {
        Err(super::process::unsupported_signal(signal))
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.killer.kill()
    }
}

/// Spawn a process attached to the Windows pseudo-console.
pub async fn spawn_process(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    size: TerminalSize,
) -> Result<SpawnedProcess> {
    if program.is_empty() {
        anyhow::bail!("missing program for PTY spawn");
    }

    let pair = super::win::ConPtySystem::default().openpty(size.into())?;
    let mut command = CommandBuilder::new(program);
    command.cwd(cwd);
    command.env_clear();
    for arg in args {
        command.arg(arg);
    }
    for (key, value) in env {
        command.env(key, value);
    }

    let mut child = pair.slave.spawn_command(command)?;
    let killer = child.clone_killer();

    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (_stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(1);
    let mut reader = pair.master.try_clone_reader()?;
    let reader_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
        let mut buffer = [0u8; 8_192];
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

    let writer = Arc::new(tokio::sync::Mutex::new(pair.master.take_writer()?));
    let writer_handle: JoinHandle<()> = tokio::spawn({
        let writer = Arc::clone(&writer);
        async move {
            let mut windows_input = super::windows_input::WindowsTtyInputNormalizer::default();
            while let Some(bytes) = writer_rx.recv().await {
                let bytes = windows_input.normalize(&bytes);
                let mut writer = writer.lock().await;
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
        }
    });

    let (exit_tx, exit_rx) = oneshot::channel::<i32>();
    let exit_status = Arc::new(AtomicBool::new(false));
    let wait_exit_status = Arc::clone(&exit_status);
    let exit_code = Arc::new(StdMutex::new(None));
    let wait_exit_code = Arc::clone(&exit_code);
    let wait_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
        let code = child
            .wait()
            .map(|status| status.exit_code() as i32)
            .unwrap_or(-1);
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = wait_exit_code.lock() {
            *guard = Some(code);
        }
        let _ = exit_tx.send(code);
    });

    let handles = PtyHandles {
        _slave: pair.slave,
        master: pair.master,
    };
    let handle = ProcessHandle::new(
        writer_tx,
        Box::new(PtyChildTerminator { killer }),
        reader_handle,
        Vec::new(),
        writer_handle,
        wait_handle,
        exit_status,
        exit_code,
        Some(handles),
    );

    Ok(SpawnedProcess {
        session: handle,
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}
