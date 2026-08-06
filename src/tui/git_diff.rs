//! Safe local Git diff collection for `/diff`, including untracked files.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;

const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;
const MAX_GIT_METADATA_BYTES: usize = 1024 * 1024;
const MAX_GIT_STDERR_BYTES: usize = 64 * 1024;

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

pub(super) async fn get_git_diff(cwd: PathBuf) -> Result<String, String> {
    tokio::task::spawn_blocking(move || get_git_diff_blocking(&cwd))
        .await
        .map_err(|error| format!("Git diff task failed: {error}"))?
}

fn get_git_diff_blocking(cwd: &Path) -> Result<String, String> {
    let inside = run_git(
        cwd,
        &[],
        &["rev-parse", "--is-inside-work-tree"],
        MAX_GIT_METADATA_BYTES,
    )?;
    require_complete_output("git rev-parse", &inside)?;
    if !inside.status.success() {
        return Ok("`/diff` — _not inside a Git repository_".to_string());
    }

    let filter_overrides = filter_overrides(cwd)?;
    let tracked = run_git(
        cwd,
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
    )?;
    let (mut diff, tracked_truncated) = diff_output(tracked, "git diff")?;
    if tracked_truncated {
        truncate_diff(&mut diff);
        return Ok(diff);
    }

    let untracked = run_git(
        cwd,
        &[],
        &["ls-files", "--others", "--exclude-standard", "-z"],
        MAX_DIFF_BYTES,
    )?;
    if !untracked.status.success() && !untracked.stdout_truncated {
        return Err(command_failure("git ls-files", &untracked));
    }
    let untracked_list_truncated = untracked.stdout_truncated;
    let complete_untracked_bytes = if untracked_list_truncated {
        untracked
            .stdout
            .iter()
            .rposition(|byte| *byte == 0)
            .map_or(0, |index| index.saturating_add(1))
    } else {
        untracked.stdout.len()
    };
    for path in untracked.stdout[..complete_untracked_bytes]
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
            return Ok(diff);
        }
        let output = run_git_os(cwd, &filter_overrides, &arguments, remaining)?;
        let (output, truncated) = diff_output(output, "git diff --no-index")?;
        diff.push_str(&output);
        if truncated || diff.len() > MAX_DIFF_BYTES {
            truncate_diff(&mut diff);
            return Ok(diff);
        }
    }

    if untracked_list_truncated {
        truncate_diff_with_notice(&mut diff, "… untracked file list truncated at 8 MiB …");
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

fn filter_overrides(cwd: &Path) -> Result<Vec<(String, String)>, String> {
    let output = run_git(
        cwd,
        &[],
        &[
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            "^filter\\..*\\.(clean|process)$",
        ],
        MAX_GIT_METADATA_BYTES,
    )?;
    require_complete_output("git config", &output)?;
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

fn run_git(
    cwd: &Path,
    overrides: &[(String, String)],
    args: &[&str],
    max_stdout_bytes: usize,
) -> Result<GitOutput, String> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    run_git_os(cwd, overrides, &args, max_stdout_bytes)
}

fn run_git_os(
    cwd: &Path,
    overrides: &[(String, String)],
    args: &[OsString],
    max_stdout_bytes: usize,
) -> Result<GitOutput, String> {
    let mut command = Command::new("git");
    command.current_dir(cwd).args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
    ]);
    for (key, value) in overrides {
        command.arg("-c").arg(format!("{key}={value}"));
    }
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to run git: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture git stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture git stderr".to_string())?;
    let stderr_task = match std::thread::Builder::new()
        .name("bettercodex-git-stderr".to_string())
        .spawn(move || read_bounded(stderr, MAX_GIT_STDERR_BYTES, false))
    {
        Ok(task) => task,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("failed to start git stderr reader: {error}"));
        }
    };

    let stdout_result = read_bounded(&mut stdout, max_stdout_bytes, true);
    if stdout_result
        .as_ref()
        .is_ok_and(|(_, truncated)| *truncated)
        || stdout_result.is_err()
    {
        let _ = child.kill();
    }
    drop(stdout);
    let status = child
        .wait()
        .map_err(|error| format!("failed to wait for git: {error}"))?;
    let (stdout, stdout_truncated) =
        stdout_result.map_err(|error| format!("failed to read git stdout: {error}"))?;
    let (stderr, stderr_truncated) = stderr_task
        .join()
        .map_err(|_| "git stderr reader panicked".to_string())?
        .map_err(|error| format!("failed to read git stderr: {error}"))?;
    Ok(GitOutput {
        status,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

fn read_bounded(
    mut reader: impl Read,
    limit: usize,
    stop_at_limit: bool,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let keep = read.min(limit.saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
        if keep < read {
            truncated = true;
            if stop_at_limit {
                break;
            }
        }
    }
    Ok((retained, truncated))
}

fn require_complete_output(command: &str, output: &GitOutput) -> Result<(), String> {
    if output.stdout_truncated {
        Err(format!(
            "{command} produced more than {MAX_GIT_METADATA_BYTES} bytes"
        ))
    } else {
        Ok(())
    }
}

fn diff_output(output: GitOutput, command: &str) -> Result<(String, bool), String> {
    if output.stdout_truncated || output.status.success() || output.status.code() == Some(1) {
        Ok((
            String::from_utf8_lossy(&output.stdout).into_owned(),
            output.stdout_truncated,
        ))
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
    truncate_diff_with_notice(diff, "… diff truncated at 8 MiB …");
}

fn truncate_diff_with_notice(diff: &mut String, notice: &str) {
    let mut boundary = MAX_DIFF_BYTES.min(diff.len());
    while !diff.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    diff.truncate(boundary);
    if !diff.ends_with('\n') {
        diff.push('\n');
    }
    diff.push('\n');
    diff.push_str(notice);
    diff.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temporary_directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "bettercodex-diff-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn diff_includes_tracked_and_untracked_files() {
        let cwd = temporary_directory();
        assert!(
            Command::new("git")
                .current_dir(&cwd)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(cwd.join("tracked.txt"), "before\n").unwrap();
        assert!(
            Command::new("git")
                .current_dir(&cwd)
                .args(["add", "tracked.txt"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(cwd.join("tracked.txt"), "after\n").unwrap();
        std::fs::write(cwd.join("untracked.txt"), "new\n").unwrap();

        let diff = get_git_diff(cwd.clone()).await.unwrap();
        assert!(diff.contains("tracked.txt"), "{diff}");
        assert!(diff.contains("untracked.txt"), "{diff}");
        assert!(diff.contains("\x1b["), "expected styled Git output: {diff}");

        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[tokio::test]
    async fn diff_reports_directories_outside_git() {
        let cwd = temporary_directory();
        assert_eq!(
            get_git_diff(cwd.clone()).await.unwrap(),
            "`/diff` — _not inside a Git repository_"
        );
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[tokio::test]
    async fn large_untracked_diff_is_bounded_during_collection() {
        let cwd = temporary_directory();
        assert!(
            Command::new("git")
                .current_dir(&cwd)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(
            cwd.join("large.txt"),
            vec![b'x'; MAX_DIFF_BYTES.saturating_add(1024)],
        )
        .unwrap();

        let diff = get_git_diff(cwd.clone()).await.unwrap();

        assert!(diff.contains("diff truncated at 8 MiB"), "{diff}");
        assert!(diff.len() <= MAX_DIFF_BYTES + 64, "{} bytes", diff.len());
        std::fs::remove_dir_all(cwd).unwrap();
    }
}
