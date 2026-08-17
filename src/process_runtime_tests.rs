use super::*;

struct TemporaryDirectory(std::path::PathBuf);

impl TemporaryDirectory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "bettercodex-process-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn assert_process_is_terminated(process_id: libc::pid_t) {
    for _ in 0..20 {
        if unsafe { libc::kill(process_id, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    unsafe {
        libc::kill(process_id, libc::SIGKILL);
    }
    panic!("descendant {process_id} survived process-tree cleanup");
}

#[tokio::test]
async fn drains_concurrent_stdout_and_stderr_without_merging_them() {
    let emitted_bytes = READ_CHUNK_BYTES * 12;
    let command = format!(
        "(yes o | tr -d '\\n' | head -c {emitted_bytes}) & \
         (yes e | tr -d '\\n' | head -c {emitted_bytes} >&2) & wait; exit 7"
    );
    let output = run_bash(
        &command,
        &std::env::current_dir().unwrap(),
        None,
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(output.stdout, "o".repeat(emitted_bytes));
    assert_eq!(output.stderr, "e".repeat(emitted_bytes));
    assert_eq!(output.exit_code, 7);
}

#[tokio::test]
async fn operator_commands_use_the_detected_user_login_shell() {
    let shell = crate::shell_command::shell_detect::default_user_shell();
    let output = run_user_shell(
        "printf '%s' \"$0\"",
        &std::env::current_dir().unwrap(),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert!(
        output
            .stdout
            .trim_end()
            .ends_with(shell.shell_path.to_string_lossy().as_ref()),
        "expected {}, got {:?}",
        shell.shell_path.display(),
        output.stdout
    );
}

#[tokio::test]
async fn model_reachable_commands_do_not_inherit_launch_credentials() {
    const HELPER_ENV: &str = "BETTERCODEX_RESTRICTED_ENV_TEST_HELPER";

    if std::env::var_os(HELPER_ENV).is_some() {
        let output = run_bash(
            "env",
            &std::env::current_dir().unwrap(),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(output.exit_code, 0);
        for name in NON_INHERITABLE_ENV_VARS {
            assert!(
                !output
                    .stdout
                    .lines()
                    .any(|line| line.starts_with(&format!("{name}="))),
                "restricted environment variable {name} reached Bash"
            );
        }
        return;
    }

    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_runtime::tests::model_reachable_commands_do_not_inherit_launch_credentials",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, "1")
        .envs(
            NON_INHERITABLE_ENV_VARS
                .into_iter()
                .map(|name| (name, "must-not-be-inherited")),
        )
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "nested environment-scrubbing test failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn pre_cancelled_command_does_not_start() {
    let root = TemporaryDirectory::new("pre-cancelled");
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let output = run_bash("printf started > marker", &root.0, None, cancellation, None)
        .await
        .unwrap();

    assert_eq!(output.stdout, "");
    assert_eq!(output.stderr, "");
    assert_eq!(output.exit_code, CANCELLATION_EXIT_CODE);
    assert!(!root.0.join("marker").exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn closes_inheritable_parent_file_descriptors() {
    use std::os::fd::AsRawFd;

    let root = TemporaryDirectory::new("inherited-descriptor");
    let inherited = std::fs::File::create(root.0.join("parent-only")).unwrap();
    let descriptor = inherited.as_raw_fd();
    // SAFETY: the test owns this open descriptor for its full duration.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    assert!(flags >= 0);
    // SAFETY: clearing CLOEXEC deliberately creates the inheritance condition under test.
    assert_eq!(
        unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
        0
    );

    let output = run_bash(
        &format!(
            "if [ -e /proc/self/fd/{descriptor} ]; then printf inherited; else printf closed; fi"
        ),
        &root.0,
        None,
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(output.stdout, "closed");
}

#[tokio::test]
async fn explicit_timeout_uses_conventional_exit_code_and_kills_descendants() {
    let root = TemporaryDirectory::new("timeout-cleanup");
    let output = run_bash(
        "sleep 30 & printf '%s' \"$!\" > descendant-pid; exec 1>&- 2>&-; wait",
        &root.0,
        Some(Duration::from_secs(1)),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(output.exit_code, TIMEOUT_EXIT_CODE);
    let descendant_pid = std::fs::read_to_string(root.0.join("descendant-pid"))
        .unwrap()
        .parse::<libc::pid_t>()
        .unwrap();
    assert_process_is_terminated(descendant_pid).await;
}

#[tokio::test]
async fn cancellation_runs_cleanup_then_kills_resistant_descendants() {
    let root = TemporaryDirectory::new("cancellation-cleanup");
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let ready = root.0.join("ready");
    let cancel_task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Bash command did not become ready for cancellation");
        cancel.cancel();
    });
    let command = r#"(trap '' TERM; sleep 30) &
printf '%s' "$!" > descendant-pid
trap 'printf cleaned > cleanup; exit 0' TERM
printf ready > ready
while :; do sleep 1; done"#;

    let output = tokio::time::timeout(
        Duration::from_secs(5),
        run_bash(command, &root.0, None, cancellation, None),
    )
    .await
    .expect("cancellation did not stop the Bash command promptly")
    .unwrap();
    cancel_task.await.unwrap();

    assert_eq!(output.exit_code, CANCELLATION_EXIT_CODE);
    assert_eq!(
        std::fs::read_to_string(root.0.join("cleanup")).unwrap(),
        "cleaned"
    );
    let descendant_pid = std::fs::read_to_string(root.0.join("descendant-pid"))
        .unwrap()
        .parse::<libc::pid_t>()
        .unwrap();
    assert_process_is_terminated(descendant_pid).await;
}

#[tokio::test]
async fn aborting_the_runner_terminates_its_process_tree() {
    let root = TemporaryDirectory::new("aborted-runner");
    let task_root = root.0.clone();
    let task = tokio::spawn(async move {
        run_bash(
            "sleep 30 & printf '%s' \"$!\" > descendant-pid; printf ready > ready; wait",
            &task_root,
            None,
            CancellationToken::new(),
            None,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.0.join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Bash command did not become ready before task abortion");

    task.abort();
    let error = match task.await {
        Err(error) => error,
        Ok(_) => panic!("aborted Bash task completed normally"),
    };
    assert!(error.is_cancelled());
    let descendant_pid = std::fs::read_to_string(root.0.join("descendant-pid"))
        .unwrap()
        .parse::<libc::pid_t>()
        .unwrap();
    assert_process_is_terminated(descendant_pid).await;
}

#[tokio::test]
async fn natural_shell_exit_terminates_background_descendants() {
    let root = TemporaryDirectory::new("descendant-cleanup");
    let output = run_bash(
        "(sleep 0.2; printf survived > survivor) & printf done",
        &root.0,
        None,
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(output.stdout, "done");

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!root.0.join("survivor").exists());
}

#[tokio::test]
async fn live_output_preserves_utf8_across_pipe_reads() {
    let mut live_stdout = String::new();
    let mut collect_live_output = |stream, text: String| {
        assert_eq!(stream, OutputStream::Stdout);
        live_stdout.push_str(&text);
        LiveOutputAction::Continue
    };
    let output = run_bash(
        r#"printf '\360\237'; sleep 0.02; printf '\230\200'"#,
        &std::env::current_dir().unwrap(),
        None,
        CancellationToken::new(),
        Some(&mut collect_live_output),
    )
    .await
    .unwrap();

    assert_eq!(live_stdout, "😀");
    assert_eq!(output.stdout, "😀");
}

#[tokio::test]
async fn stopping_live_output_does_not_truncate_captured_output() {
    let emitted_bytes = READ_CHUNK_BYTES * 3;
    let command = format!("yes x | tr -d '\\n' | head -c {emitted_bytes}");
    let mut callback_count = 0;
    let mut stop_live_output = |_, _: String| {
        callback_count += 1;
        LiveOutputAction::Stop
    };

    let output = run_bash(
        &command,
        &std::env::current_dir().unwrap(),
        None,
        CancellationToken::new(),
        Some(&mut stop_live_output),
    )
    .await
    .unwrap();

    assert_eq!(callback_count, 1);
    assert_eq!(output.stdout, "x".repeat(emitted_bytes));
}

#[tokio::test]
async fn retained_output_keeps_a_bounded_head_and_tail() {
    let emitted_bytes = 256 * 1024;
    let command = format!(
        "printf head-marker; yes x | tr -d '\\n' | head -c {emitted_bytes}; printf tail-marker"
    );
    let output = run_bash(
        &command,
        &std::env::current_dir().unwrap(),
        None,
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    assert!(output.stdout.starts_with("head-marker"));
    assert!(output.stdout.contains(" bytes omitted ...\n"));
    assert!(output.stdout.ends_with("tail-marker"));
    assert!(output.stdout.len() < emitted_bytes);
}
