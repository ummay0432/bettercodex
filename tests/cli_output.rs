use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::process::Stdio;

#[test]
fn closed_stdout_pipe_is_normal_cli_termination() {
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);

    let output = Command::new(env!("CARGO_BIN_EXE_bcodex"))
        .arg("--tool-context-json")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("BCODEX_MANAGED_ENVIRONMENT")
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "closed stdout produced status {:?} and stderr {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, b"");
}
