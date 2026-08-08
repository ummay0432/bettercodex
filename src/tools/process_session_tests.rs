use super::*;

#[test]
fn retained_output_keeps_head_and_tail() {
    let mut output = PendingOutput::default();
    let bytes = vec![b'x'; RETAINED_HEAD_BYTES + RETAINED_TAIL_BYTES + 17];
    output.append(&bytes);
    let snapshot = output.take(SnapshotBoundary::Final);
    let rendered = snapshot.text;
    assert_eq!(snapshot.total_bytes, bytes.len());
    assert_eq!(snapshot.omitted_bytes, 17);
    assert!(rendered.starts_with(&"x".repeat(RETAINED_HEAD_BYTES)));
    assert!(rendered.contains("17 bytes omitted"));
    assert!(rendered.ends_with(&"x".repeat(RETAINED_TAIL_BYTES)));
}

#[tokio::test]
async fn dropping_an_exited_session_terminates_descendants_holding_output_open() {
    let shell = DetectedShell {
        shell_type: ShellType::Sh,
        shell_path: PathBuf::from("/bin/sh"),
    };
    let cwd = std::env::current_dir().unwrap();
    let session = ProcessSession::spawn(
        &shell,
        ShellStartup::NonLogin,
        "sleep 30 & child=$!; printf 'child:%s\\n' \"$child\"",
        &cwd,
        ProcessMode::Piped,
        &HashMap::new(),
    )
    .await
    .unwrap();
    let mut output = String::new();
    let child_pid = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            session.wait(Duration::from_millis(50)).await;
            let snapshot = session.snapshot().unwrap();
            output.push_str(&snapshot.output);
            if let Some(exit_code) = snapshot.exit_code
                && let Some(child_pid) = output
                    .lines()
                    .find_map(|line| line.strip_prefix("child:"))
                    .and_then(|pid| pid.parse::<libc::pid_t>().ok())
            {
                assert_eq!(exit_code, 0);
                break child_pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for the root process to exit");
    assert!(process_exists(child_pid));

    drop(session);

    let descendant_exited = tokio::time::timeout(Duration::from_secs(2), async {
        while process_exists(child_pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    if !descendant_exited {
        let _ = unsafe { libc::kill(child_pid, libc::SIGKILL) };
    }
    assert!(
        descendant_exited,
        "descendant process {child_pid} survived session cleanup"
    );
}

fn process_exists(process_id: libc::pid_t) -> bool {
    if unsafe { libc::kill(process_id, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
