use std::fs;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

struct TestHome {
    root: PathBuf,
    codex_home: PathBuf,
}

impl TestHome {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("bettercodex-login-{}", Uuid::new_v4()));
        let codex_home = root.join("codex-home");
        fs::create_dir_all(&codex_home).unwrap();
        Self { root, codex_home }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bcodex"));
        command
            .current_dir(&self.root)
            .env("CODEX_HOME", &self.codex_home)
            .env("HOME", self.root.join("home"))
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("BCODEX_MANAGED_ENVIRONMENT")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn login_status_reports_chatgpt_credentials() {
    let home = TestHome::new();
    fs::write(
        home.codex_home.join("auth.json"),
        r#"{"tokens":{"access_token":"test-access","refresh_token":"test-refresh"}}"#,
    )
    .unwrap();

    let output = home.command().args(["login", "status"]).output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"Logged in using ChatGPT\n");
}

#[test]
fn login_status_fails_when_credentials_are_absent() {
    let home = TestHome::new();

    let output = home.command().args(["login", "status"]).output().unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"error: Not logged in\n");
}

#[test]
fn logout_removes_codex_file_credentials() {
    let home = TestHome::new();
    let auth_path = home.codex_home.join("auth.json");
    fs::write(
        &auth_path,
        r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-test"}"#,
    )
    .unwrap();

    let output = home.command().arg("logout").output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"Successfully logged out\n");
    assert!(!auth_path.exists());
}

#[test]
fn login_help_documents_browser_device_and_status_flows() {
    let home = TestHome::new();

    let output = home.command().args(["login", "--help"]).output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("bcodex login [OPTIONS]"), "{stdout}");
    assert!(stdout.contains("bcodex login status"), "{stdout}");
    assert!(stdout.contains("--device-auth"), "{stdout}");
}
