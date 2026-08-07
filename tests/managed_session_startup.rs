use portable_pty::CommandBuilder;
use portable_pty::PtySize;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    bettercodex_home: PathBuf,
    tmux_log: PathBuf,
    fake_bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "bettercodex-managed-startup-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let bettercodex_home = root.join("bcodex-home");
        let fake_bin = root.join("bin");
        fs::create_dir_all(&bettercodex_home).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        let tmux = fake_bin.join("tmux");
        fs::write(
            &tmux,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TMUX_LOG\"\nexit 17\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&tmux).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&tmux, permissions).unwrap();
        Self {
            tmux_log: root.join("tmux.log"),
            root,
            bettercodex_home,
            fake_bin,
        }
    }

    fn set_tmux(&self, enabled: bool) {
        fs::write(
            self.bettercodex_home.join("settings.json"),
            format!("{{\"version\":1,\"tmux\":{enabled}}}\n"),
        )
        .unwrap();
    }

    fn command(&self) -> CommandBuilder {
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_bcodex"));
        command.cwd(&self.root);
        command.env("BCODEX_HOME", &self.bettercodex_home);
        command.env("CODEX_HOME", self.root.join("codex-home"));
        command.env("HOME", self.root.join("home"));
        command.env("TERM", "xterm-256color");
        command.env("TMUX_LOG", &self.tmux_log);
        command.env("STARTUP_SECRET", "startup-secret-must-not-enter-argv");
        command.env("PATH", prefixed_path(&self.fake_bin));
        command.env_remove("TMUX");
        command.env_remove("TMUX_PANE");
        command
    }

    fn environment_snapshot(&self) -> (PathBuf, PathBuf) {
        let restored_codex_home = self.root.join("restored-codex-home");
        let path = self.root.join(format!(
            ".bettercodex-environment-integration-{}",
            Uuid::new_v4()
        ));
        let environment = format!(
            "BCODEX_HOME={}\0CODEX_HOME={}\0HOME={}\0PATH={}\0STARTUP_SECRET=restored-secret\0",
            self.bettercodex_home.display(),
            restored_codex_home.display(),
            self.root.join("restored-home").display(),
            prefixed_path(&self.fake_bin).to_string_lossy(),
        );
        fs::write(&path, environment.as_bytes()).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&path, permissions).unwrap();
        (path, restored_codex_home)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn disabled_startup_bypasses_tmux_before_agent_initialization() {
    let fixture = Fixture::new();
    fixture.set_tmux(false);

    let output = run_in_pty(fixture.command());

    assert!(
        output.contains("failed to read ChatGPT credentials"),
        "{output:?}"
    );
    assert!(
        !fixture.tmux_log.exists(),
        "tmux was invoked while disabled: {}",
        fs::read_to_string(&fixture.tmux_log).unwrap_or_default()
    );
}

#[test]
fn enabled_startup_still_enters_the_managed_tmux_path() {
    let fixture = Fixture::new();
    fixture.set_tmux(true);

    let output = run_in_pty(fixture.command());

    let invocations = fs::read_to_string(&fixture.tmux_log).unwrap();
    assert!(invocations.contains("list-sessions -F"), "{invocations}");
    assert!(invocations.contains("new-session -d"), "{invocations}");
    assert!(
        invocations.contains("BCODEX_MANAGED_ENVIRONMENT="),
        "{invocations}"
    );
    assert!(!invocations.contains("startup-secret"), "{invocations}");
    assert!(!invocations.contains("BCODEX_HOME="), "{invocations}");
    assert!(
        output.contains("tmux could not create session c1"),
        "{output:?}"
    );
}

#[test]
fn managed_pane_restores_and_consumes_the_invoking_environment() {
    let fixture = Fixture::new();
    fixture.set_tmux(false);
    let (snapshot, restored_codex_home) = fixture.environment_snapshot();
    let mut command = fixture.command();
    command.env("BCODEX_MANAGED_ENVIRONMENT", &snapshot);
    command.env("CODEX_HOME", fixture.root.join("stale-codex-home"));

    let output = run_in_pty(command);

    assert!(
        output.contains(&restored_codex_home.display().to_string()),
        "{output:?}"
    );
    assert!(!snapshot.exists());
    assert!(!fixture.tmux_log.exists());
}

fn run_in_pty(command: CommandBuilder) -> String {
    let pair = portable_pty::native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    drop(pair.master);

    let (output_tx, output_rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if output_tx.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("failed to read startup PTY: {error}"),
            }
        }
    });

    let mut killer = child.clone_killer();
    let (status_tx, status_rx) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let _ = status_tx.send(child.wait());
    });
    let mut output = Vec::new();

    let status = match status_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(status) => status,
        Err(error) => {
            let _ = killer.kill();
            let _ = status_rx.recv_timeout(Duration::from_secs(2));
            waiter.join().unwrap();
            for chunk in output_rx {
                output.extend_from_slice(&chunk);
            }
            reader_thread.join().unwrap();
            panic!(
                "startup process did not exit ({error}); output: {:?}",
                String::from_utf8_lossy(&output)
            );
        }
    };
    waiter.join().unwrap();
    for chunk in output_rx {
        output.extend_from_slice(&chunk);
    }
    reader_thread.join().unwrap();
    status.unwrap();
    String::from_utf8_lossy(&output).into_owned()
}

fn prefixed_path(directory: &Path) -> std::ffi::OsString {
    let mut paths = vec![directory.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(paths).unwrap()
}
