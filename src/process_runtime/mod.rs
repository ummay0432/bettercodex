#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
pub(crate) use windows::ProcessHandle;
#[cfg(windows)]
pub(crate) use windows::ProcessSignal;
#[cfg(windows)]
pub(crate) use windows::SpawnedProcess;
#[cfg(windows)]
pub(crate) use windows::TerminalSize;

#[cfg(windows)]
pub(crate) async fn spawn_pipe_process_no_stdin(
    program: &str,
    arguments: &[String],
    cwd: &std::path::Path,
    environment: &std::collections::HashMap<String, String>,
) -> anyhow::Result<SpawnedProcess> {
    windows::spawn_pipe_process_no_stdin(
        program,
        arguments,
        cwd,
        environment,
        /*arg0*/ &None,
        &[],
    )
    .await
}

#[cfg(windows)]
pub(crate) async fn spawn_pty_process(
    program: &str,
    arguments: &[String],
    cwd: &std::path::Path,
    environment: &std::collections::HashMap<String, String>,
    size: TerminalSize,
) -> anyhow::Result<SpawnedProcess> {
    windows::spawn_pty_process(
        program,
        arguments,
        cwd,
        environment,
        /*arg0*/ &None,
        size,
        &[],
    )
    .await
}
