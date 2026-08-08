//! Safe local Git diff collection for `/diff`, including untracked files.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;
use tokio::time::Instant;
use tokio::time::timeout_at;

const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;
const MAX_GIT_METADATA_BYTES: usize = 8 * 1024 * 1024;
const MAX_GIT_STDERR_BYTES: usize = 64 * 1024;
const GIT_DIFF_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) async fn get_git_diff(cwd: PathBuf) -> Result<String, String> {
    get_git_diff_with_program(cwd, OsString::from("git")).await
}

async fn get_git_diff_with_program(cwd: PathBuf, program: OsString) -> Result<String, String> {
    let runner = GitRunner::new(&cwd, &program);
    let inside = runner
        .run(
            &[],
            &["rev-parse", "--is-inside-work-tree"],
            MAX_GIT_METADATA_BYTES,
        )
        .await?;
    if inside.stdout_truncated {
        return Err("git rev-parse produced too much output".to_string());
    }
    if !inside.status.success() {
        return Ok("`/diff` — _not inside a Git repository_".to_string());
    }

    let filter_overrides = filter_overrides(&runner).await?;
    let tracked = runner
        .run(
            &filter_overrides,
            &[
                "diff",
                "--no-textconv",
                "--no-ext-diff",
                "--submodule=short",
                "--ignore-submodules=dirty",
                "--color=always",
            ],
            MAX_DIFF_BYTES,
        )
        .await?;
    let (mut diff, tracked_truncated) = diff_output(tracked, "git diff")?;
    if tracked_truncated {
        truncate_diff(&mut diff);
        return Ok(diff);
    }

    let untracked = runner
        .run(
            &[],
            &["ls-files", "--others", "--exclude-standard", "-z"],
            MAX_GIT_METADATA_BYTES,
        )
        .await?;
    if untracked.stdout_truncated {
        truncate_diff(&mut diff);
        return Ok(diff);
    }
    if !untracked.status.success() {
        return Err(command_failure("git ls-files", &untracked));
    }
    for path in untracked
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(path.to_vec())
        };
        #[cfg(not(unix))]
        let path = OsString::from(String::from_utf8_lossy(path).into_owned());
        let arguments = vec![
            OsString::from("diff"),
            OsString::from("--no-textconv"),
            OsString::from("--no-ext-diff"),
            OsString::from("--submodule=short"),
            OsString::from("--ignore-submodules=dirty"),
            OsString::from("--color=always"),
            OsString::from("--no-index"),
            OsString::from("--"),
            OsString::from("/dev/null"),
            path,
        ];
        let remaining = MAX_DIFF_BYTES.saturating_sub(diff.len());
        if remaining == 0 {
            truncate_diff(&mut diff);
            break;
        }
        let output = runner
            .run_os(&filter_overrides, &arguments, remaining)
            .await?;
        let (untracked_diff, truncated) = diff_output(output, "git diff --no-index")?;
        diff.push_str(&untracked_diff);
        if truncated || diff.len() > MAX_DIFF_BYTES {
            truncate_diff(&mut diff);
            break;
        }
    }

    if diff.is_empty() {
        Ok("_No changes._".to_string())
    } else {
        if diff.len() > MAX_DIFF_BYTES {
            truncate_diff(&mut diff);
        }
        Ok(diff)
    }
}

async fn filter_overrides(runner: &GitRunner<'_>) -> Result<Vec<(String, String)>, String> {
    let output = runner
        .run(
            &[],
            &[
                "config",
                "--null",
                "--name-only",
                "--get-regexp",
                "^filter\\..*\\.(clean|process)$",
            ],
            MAX_GIT_METADATA_BYTES,
        )
        .await?;
    if output.stdout_truncated {
        return Err("git config produced too much output".to_string());
    }
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(command_failure("git config", &output));
    }
    let mut drivers = String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter_map(|key| {
            key.strip_suffix(".clean")
                .or_else(|| key.strip_suffix(".process"))
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    drivers.sort();
    drivers.dedup();
    Ok(drivers
        .into_iter()
        .flat_map(|driver| {
            [
                (format!("{driver}.clean"), String::new()),
                (format!("{driver}.process"), String::new()),
                (format!("{driver}.required"), "false".to_string()),
            ]
        })
        .collect())
}

struct GitRunner<'a> {
    cwd: &'a Path,
    program: &'a OsStr,
    deadline: Instant,
}

impl<'a> GitRunner<'a> {
    fn new(cwd: &'a Path, program: &'a OsStr) -> Self {
        Self {
            cwd,
            program,
            deadline: Instant::now() + GIT_DIFF_TIMEOUT,
        }
    }

    async fn run(
        &self,
        overrides: &[(String, String)],
        args: &[&str],
        stdout_limit: usize,
    ) -> Result<GitOutput, String> {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        self.run_os(overrides, &args, stdout_limit).await
    }

    async fn run_os(
        &self,
        overrides: &[(String, String)],
        args: &[OsString],
        stdout_limit: usize,
    ) -> Result<GitOutput, String> {
        if Instant::now() >= self.deadline {
            return Err("Git diff exceeded its 30-second time limit".to_string());
        }

        let mut command = TokioCommand::new(self.program);
        command
            .current_dir(self.cwd)
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in overrides {
            command.arg("-c").arg(format!("{key}={value}"));
        }
        let mut child = command
            .args(args)
            .spawn()
            .map_err(|error| format!("failed to run git: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to capture git stdout".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "failed to capture git stderr".to_string())?;
        let output = timeout_at(self.deadline, async {
            tokio::join!(
                child.wait(),
                capture_stream(stdout, stdout_limit),
                capture_stream(stderr, MAX_GIT_STDERR_BYTES),
            )
        })
        .await;
        let (status, stdout, stderr) = match output {
            Ok(output) => output,
            Err(_) => {
                let _ = child.kill().await;
                return Err("Git diff exceeded its 30-second time limit".to_string());
            }
        };
        let status = status.map_err(|error| format!("failed to wait for git: {error}"))?;
        let stdout = stdout.map_err(|error| format!("failed to read git stdout: {error}"))?;
        let stderr = stderr.map_err(|error| format!("failed to read git stderr: {error}"))?;
        Ok(GitOutput {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
        })
    }
}

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn capture_stream(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<CapturedStream> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    // This buffer lives across an await, so heap allocation keeps the nested
    // Git-diff future small enough to spawn without a large stack frame.
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let retained = read.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..retained]);
        if retained < read {
            truncated = true;
            break;
        }
    }
    Ok(CapturedStream { bytes, truncated })
}

fn diff_output(output: GitOutput, command: &str) -> Result<(String, bool), String> {
    if output.stdout_truncated {
        return Ok((String::from_utf8_lossy(&output.stdout).into_owned(), true));
    }
    if output.status.success() || output.status.code() == Some(1) {
        Ok((String::from_utf8_lossy(&output.stdout).into_owned(), false))
    } else {
        Err(command_failure(command, &output))
    }
}

fn command_failure(command: &str, output: &GitOutput) -> String {
    let mut stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.stderr_truncated {
        stderr.push_str(" … stderr truncated …");
    }
    if stderr.is_empty() {
        format!("{command} exited with {}", output.status)
    } else {
        format!("{command} failed: {stderr}")
    }
}

fn truncate_diff(diff: &mut String) {
    let mut boundary = MAX_DIFF_BYTES.min(diff.len());
    while !diff.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    diff.truncate(boundary);
    if !diff.is_empty() && !diff.ends_with('\n') {
        diff.push('\n');
    }
    if !diff.is_empty() {
        diff.push('\n');
    }
    diff.push_str("… diff truncated at 8 MiB …\n");
}
