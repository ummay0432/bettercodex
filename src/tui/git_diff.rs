//! Safe local Git diff collection for `/diff`, including untracked files.

use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;

pub(super) async fn get_git_diff(cwd: PathBuf) -> Result<String, String> {
    tokio::task::spawn_blocking(move || get_git_diff_blocking(&cwd))
        .await
        .map_err(|error| format!("Git diff task failed: {error}"))?
}

fn get_git_diff_blocking(cwd: &Path) -> Result<String, String> {
    let inside = run_git(cwd, &[], &["rev-parse", "--is-inside-work-tree"])?;
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
    )?;
    let mut diff = diff_output(tracked, "git diff")?;

    let untracked = run_git(
        cwd,
        &[],
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
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
        let output = run_git_os(cwd, &filter_overrides, &arguments)?;
        diff.push_str(&diff_output(output, "git diff --no-index")?);
        if diff.len() > MAX_DIFF_BYTES {
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
    )?;
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

fn run_git(cwd: &Path, overrides: &[(String, String)], args: &[&str]) -> Result<Output, String> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    run_git_os(cwd, overrides, &args)
}

fn run_git_os(
    cwd: &Path,
    overrides: &[(String, String)],
    args: &[OsString],
) -> Result<Output, String> {
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
        .output()
        .map_err(|error| format!("failed to run git: {error}"))
}

fn diff_output(output: Output, command: &str) -> Result<String, String> {
    if output.status.success() || output.status.code() == Some(1) {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(command_failure(command, &output))
    }
}

fn command_failure(command: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
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
    if !diff.ends_with('\n') {
        diff.push('\n');
    }
    diff.push_str("\n… diff truncated at 8 MiB …\n");
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
}
