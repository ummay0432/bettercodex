use std::collections::HashMap;
use std::path::Path;

use pretty_assertions::assert_eq;

use super::SpawnedProcess;
use super::TerminalSize;
use super::combine_output_receivers;
use super::spawn_pipe_process;
use super::spawn_pipe_process_no_stdin;
use super::spawn_pty_process;

#[path = "windows_tests.rs"]
mod windows_tests;

fn find_python() -> Option<String> {
    ["python3", "python"].into_iter().find_map(|candidate| {
        std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| candidate.to_string())
    })
}

fn shell_command(script: &str) -> (String, Vec<String>) {
    let command = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
    (command, vec!["/C".to_string(), script.to_string()])
}

fn echo_sleep_command(marker: &str) -> String {
    format!("echo {marker} & ping -n 2 127.0.0.1 > NUL")
}

fn split_stdout_stderr_command() -> String {
    "(echo split-out)&(>&2 echo split-err)".to_string()
}

async fn collect_split_output(mut output_rx: tokio::sync::mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    let mut collected = Vec::new();
    while let Some(chunk) = output_rx.recv().await {
        collected.extend_from_slice(&chunk);
    }
    collected
}

fn combine_spawned_output(
    spawned: SpawnedProcess,
) -> (
    super::ProcessHandle,
    tokio::sync::broadcast::Receiver<Vec<u8>>,
    tokio::sync::oneshot::Receiver<i32>,
) {
    let SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;
    (
        session,
        combine_output_receivers(stdout_rx, stderr_rx),
        exit_rx,
    )
}

async fn collect_output_until_exit(
    mut output_rx: tokio::sync::broadcast::Receiver<Vec<u8>>,
    exit_rx: tokio::sync::oneshot::Receiver<i32>,
    timeout_ms: u64,
) -> (Vec<u8>, i32) {
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
    tokio::pin!(exit_rx);

    loop {
        tokio::select! {
            result = output_rx.recv() => {
                if let Ok(chunk) = result {
                    collected.extend_from_slice(&chunk);
                }
            }
            result = &mut exit_rx => {
                let code = result.unwrap_or(-1);
                // ConPTY can publish exit before its reader forwards the final bytes.
                let quiet = tokio::time::Duration::from_millis(200);
                let max_deadline =
                    tokio::time::Instant::now() + tokio::time::Duration::from_secs(2);
                while tokio::time::Instant::now() < max_deadline {
                    match tokio::time::timeout(quiet, output_rx.recv()).await {
                        Ok(Ok(chunk)) => collected.extend_from_slice(&chunk),
                        Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                        Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) | Err(_) => break,
                    }
                }
                return (collected, code);
            }
            _ = tokio::time::sleep_until(deadline) => return (collected, -1),
        }
    }
}

async fn wait_for_output_contains(
    output_rx: &mut tokio::sync::broadcast::Receiver<Vec<u8>>,
    needle: &str,
    timeout_ms: u64,
) -> anyhow::Result<Vec<u8>> {
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);

    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, output_rx.recv()).await {
            Ok(Ok(chunk)) => {
                collected.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&collected).contains(needle) {
                    return Ok(collected);
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                anyhow::bail!(
                    "PTY output closed while waiting for {needle:?}: {:?}",
                    String::from_utf8_lossy(&collected)
                );
            }
            Err(_) => break,
        }
    }

    anyhow::bail!(
        "timed out waiting for {needle:?} in PTY output: {:?}",
        String::from_utf8_lossy(&collected)
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pty_python_repl_emits_output_and_exits() -> anyhow::Result<()> {
    let Some(python) = find_python() else {
        eprintln!("python not found; skipping pty_python_repl_emits_output_and_exits");
        return Ok(());
    };

    let ready_marker = "__codex_pty_ready__";
    let args = vec![
        "-i".to_string(),
        "-q".to_string(),
        "-c".to_string(),
        format!("print('{ready_marker}')"),
    ];
    let environment: HashMap<String, String> = std::env::vars().collect();
    let spawned = spawn_pty_process(
        &python,
        &args,
        Path::new("."),
        &environment,
        TerminalSize::default(),
    )
    .await?;
    let (session, mut output_rx, exit_rx) = combine_spawned_output(spawned);
    let writer = session.writer_sender();
    let mut output = wait_for_output_contains(&mut output_rx, ready_marker, 10_000).await?;
    writer.send(b"print('hello from pty')\r\n".to_vec()).await?;
    writer.send(b"exit()\r\n".to_vec()).await?;

    let (remaining_output, code) = collect_output_until_exit(output_rx, exit_rx, 10_000).await;
    output.extend_from_slice(&remaining_output);
    assert!(
        String::from_utf8_lossy(&output).contains("hello from pty"),
        "expected Python output in PTY: {:?}",
        String::from_utf8_lossy(&output)
    );
    assert_eq!(code, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipe_process_round_trips_stdin() -> anyhow::Result<()> {
    let (program, args) = shell_command("set /p line= & echo(!line!");
    let environment: HashMap<String, String> = std::env::vars().collect();
    let spawned = spawn_pipe_process(&program, &args, Path::new("."), &environment).await?;
    let (session, output_rx, exit_rx) = combine_spawned_output(spawned);
    let writer = session.writer_sender();
    writer.send(b"roundtrip\r\n".to_vec()).await?;
    drop(writer);
    session.close_stdin();

    let (output, code) = collect_output_until_exit(output_rx, exit_rx, 5_000).await;
    assert!(String::from_utf8_lossy(&output).contains("roundtrip"));
    assert_eq!(code, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipe_and_pty_share_interface() -> anyhow::Result<()> {
    let environment: HashMap<String, String> = std::env::vars().collect();
    let (pipe_program, pipe_args) = shell_command(&echo_sleep_command("pipe_ok"));
    let (pty_program, pty_args) = shell_command(&echo_sleep_command("pty_ok"));

    let pipe = spawn_pipe_process(&pipe_program, &pipe_args, Path::new("."), &environment).await?;
    let pty = spawn_pty_process(
        &pty_program,
        &pty_args,
        Path::new("."),
        &environment,
        TerminalSize::default(),
    )
    .await?;
    let (_pipe_session, pipe_output_rx, pipe_exit_rx) = combine_spawned_output(pipe);
    let (_pty_session, pty_output_rx, pty_exit_rx) = combine_spawned_output(pty);

    let (pipe_output, pipe_code) =
        collect_output_until_exit(pipe_output_rx, pipe_exit_rx, 10_000).await;
    let (pty_output, pty_code) =
        collect_output_until_exit(pty_output_rx, pty_exit_rx, 10_000).await;
    assert_eq!(pipe_code, 0);
    assert_eq!(pty_code, 0);
    assert!(String::from_utf8_lossy(&pipe_output).contains("pipe_ok"));
    assert!(String::from_utf8_lossy(&pty_output).contains("pty_ok"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipe_drains_stderr_without_stdout_activity() -> anyhow::Result<()> {
    let Some(python) = find_python() else {
        eprintln!("python not found; skipping pipe_drains_stderr_without_stdout_activity");
        return Ok(());
    };

    let script = "import sys\nchunk = 'E' * 65536\nfor _ in range(64):\n    sys.stderr.write(chunk)\n    sys.stderr.flush()\n";
    let args = vec!["-c".to_string(), script.to_string()];
    let environment: HashMap<String, String> = std::env::vars().collect();
    let spawned = spawn_pipe_process(&python, &args, Path::new("."), &environment).await?;
    let (_session, output_rx, exit_rx) = combine_spawned_output(spawned);
    let (output, code) = collect_output_until_exit(output_rx, exit_rx, 10_000).await;
    assert_eq!(code, 0);
    assert!(!output.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipe_process_exposes_split_stdout_and_stderr() -> anyhow::Result<()> {
    let environment: HashMap<String, String> = std::env::vars().collect();
    let (program, args) = shell_command(&split_stdout_stderr_command());
    let spawned =
        spawn_pipe_process_no_stdin(&program, &args, Path::new("."), &environment).await?;
    let SpawnedProcess {
        session: _session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;

    let stdout_task = tokio::spawn(collect_split_output(stdout_rx));
    let stderr_task = tokio::spawn(collect_split_output(stderr_rx));
    let code = tokio::time::timeout(tokio::time::Duration::from_secs(10), exit_rx)
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for process exit"))??;
    let stdout = stdout_task.await?;
    let stderr = stderr_task.await?;
    assert_eq!(code, 0);
    assert!(String::from_utf8_lossy(&stdout).contains("split-out"));
    assert!(String::from_utf8_lossy(&stderr).contains("split-err"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipe_terminate_reaps_child() -> anyhow::Result<()> {
    let environment: HashMap<String, String> = std::env::vars().collect();
    let (program, args) = shell_command("ping -n 60 127.0.0.1 > NUL");
    let spawned = spawn_pipe_process(&program, &args, Path::new("."), &environment).await?;
    let (session, _output_rx, exit_rx) = combine_spawned_output(spawned);
    session.terminate();

    let exit_code = tokio::time::timeout(tokio::time::Duration::from_secs(5), exit_rx)
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for terminated child to be reaped"))?
        .map_err(|_| anyhow::anyhow!("child waiter was aborted before reaping"))?;
    assert_eq!(session.exit_code(), Some(exit_code));
    assert!(session.has_exited());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipe_drop_reaps_child() -> anyhow::Result<()> {
    let environment: HashMap<String, String> = std::env::vars().collect();
    let (program, args) = shell_command("ping -n 60 127.0.0.1 > NUL");
    let spawned = spawn_pipe_process(&program, &args, Path::new("."), &environment).await?;
    let (session, _output_rx, exit_rx) = combine_spawned_output(spawned);
    drop(session);

    tokio::time::timeout(tokio::time::Duration::from_secs(5), exit_rx)
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for dropped child to be reaped"))?
        .map_err(|_| anyhow::anyhow!("child waiter was aborted before reaping"))?;
    Ok(())
}
