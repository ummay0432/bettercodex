//! Clipboard backend for `/copy` and `Ctrl+O`.

use base64::Engine;
use std::io::Write;
use std::process::Command;
use std::process::Stdio;

const OSC52_MAX_RAW_BYTES: usize = 100_000;
#[cfg(target_os = "macos")]
static STDERR_SUPPRESSION_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> =
    std::sync::OnceLock::new();

pub(super) fn copy_to_clipboard(text: &str) -> Result<Option<ClipboardLease>, String> {
    copy_to_clipboard_with(
        text,
        CopyEnvironment {
            ssh: std::env::var_os("SSH_TTY").is_some()
                || std::env::var_os("SSH_CONNECTION").is_some(),
            tmux: crate::managed_session::is_tmux_active(),
        },
        tmux_copy,
        osc52_copy,
        native_copy,
    )
}

pub(super) struct ClipboardLease {
    #[cfg(target_os = "linux")]
    _clipboard: arboard::Clipboard,
}

#[derive(Clone, Copy)]
struct CopyEnvironment {
    ssh: bool,
    tmux: bool,
}

fn copy_to_clipboard_with(
    text: &str,
    environment: CopyEnvironment,
    tmux_copy: impl Fn(&str) -> Result<(), String>,
    osc52_copy: impl Fn(&str) -> Result<(), String>,
    native_copy: impl Fn(&str) -> Result<Option<ClipboardLease>, String>,
) -> Result<Option<ClipboardLease>, String> {
    let terminal_copy = |text: &str| {
        if environment.tmux {
            match tmux_copy(text) {
                Ok(()) => return Ok(()),
                Err(tmux_error) => {
                    return osc52_copy(text).map_err(|osc_error| {
                        format!("tmux clipboard: {tmux_error}; OSC 52 fallback: {osc_error}")
                    });
                }
            }
        }
        osc52_copy(text)
    };

    if environment.ssh {
        return terminal_copy(text).map(|()| None).map_err(|error| {
            if environment.tmux {
                format!("terminal clipboard copy failed over SSH: {error}")
            } else {
                format!("OSC 52 clipboard copy failed over SSH: {error}")
            }
        });
    }

    match native_copy(text) {
        Ok(lease) => Ok(lease),
        Err(native_error) => terminal_copy(text)
            .map(|()| None)
            .map_err(|terminal_error| {
                format!("native clipboard: {native_error}; terminal fallback: {terminal_error}")
            }),
    }
}

#[cfg(target_os = "linux")]
fn native_copy(text: &str) -> Result<Option<ClipboardLease>, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;
    clipboard
        .set_text(text)
        .map_err(|error| format!("failed to set clipboard text: {error}"))?;
    Ok(Some(ClipboardLease {
        _clipboard: clipboard,
    }))
}

#[cfg(target_os = "macos")]
fn native_copy(text: &str) -> Result<Option<ClipboardLease>, String> {
    let _lock = STDERR_SUPPRESSION_MUTEX
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .map_err(|_| "clipboard stderr suppression lock was poisoned".to_string())?;
    let _stderr = SuppressStderr::new();
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;
    clipboard
        .set_text(text)
        .map_err(|error| format!("failed to set clipboard text: {error}"))?;
    Ok(None)
}

#[cfg(windows)]
fn native_copy(text: &str) -> Result<Option<ClipboardLease>, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;
    clipboard
        .set_text(text)
        .map_err(|error| format!("failed to set clipboard text: {error}"))?;
    Ok(None)
}

fn tmux_copy(text: &str) -> Result<(), String> {
    let set_clipboard = command_output("tmux", &["show-options", "-gv", "set-clipboard"])?;
    if set_clipboard.trim() == "off" {
        return Err("tmux clipboard forwarding is disabled".to_string());
    }
    let info = command_output("tmux", &["info"])?;
    if info.lines().any(|line| line.contains("Ms: [missing]")) {
        return Err("tmux is missing the clipboard capability".to_string());
    }
    pipe_to_command("tmux", &["load-buffer", "-w", "-"], text)
}

fn osc52_copy(text: &str) -> Result<(), String> {
    let sequence = osc52_sequence(text, crate::managed_session::is_tmux_active())?;
    #[cfg(unix)]
    {
        match std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            Ok(mut tty) => write_and_flush(&mut tty, sequence.as_bytes()),
            Err(_) => write_and_flush(&mut std::io::stdout().lock(), sequence.as_bytes()),
        }
    }
    #[cfg(windows)]
    {
        write_and_flush(&mut std::io::stdout().lock(), sequence.as_bytes())
    }
}

fn osc52_sequence(text: &str, tmux: bool) -> Result<String, String> {
    if text.len() > OSC52_MAX_RAW_BYTES {
        return Err(format!(
            "OSC 52 payload is {} bytes; maximum is {OSC52_MAX_RAW_BYTES}",
            text.len()
        ));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    if tmux {
        Ok(format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\"))
    } else {
        Ok(format!("\x1b]52;c;{encoded}\x07"))
    }
}

fn pipe_to_command(program: &str, arguments: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start {program}: {error}"))?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("failed to open {program} stdin"));
    };
    if let Err(error) = stdin.write_all(text.as_bytes()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("failed to write to {program}: {error}"));
    }
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for {program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        Err(format!("{program} exited with {}", output.status))
    } else {
        Err(format!("{program} failed: {stderr}"))
    }
}

fn command_output(program: &str, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("failed to start {program}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{program} exited with {}", output.status));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{program} output was not UTF-8: {error}"))
}

fn write_and_flush(writer: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    writer
        .write_all(bytes)
        .map_err(|error| format!("failed to write OSC 52: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("failed to flush OSC 52: {error}"))
}

#[cfg(target_os = "macos")]
struct SuppressStderr {
    saved: Option<libc::c_int>,
}

#[cfg(target_os = "macos")]
impl SuppressStderr {
    fn new() -> Self {
        let saved = unsafe { libc::dup(libc::STDERR_FILENO) };
        if saved < 0 {
            return Self { saved: None };
        }
        let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
        if null < 0 || unsafe { libc::dup2(null, libc::STDERR_FILENO) } < 0 {
            unsafe {
                libc::close(saved);
                if null >= 0 {
                    libc::close(null);
                }
            }
            return Self { saved: None };
        }
        unsafe { libc::close(null) };
        Self { saved: Some(saved) }
    }
}

#[cfg(target_os = "macos")]
impl Drop for SuppressStderr {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            unsafe {
                libc::dup2(saved, libc::STDERR_FILENO);
                libc::close(saved);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn osc52_round_trips_raw_markdown_and_wraps_tmux() {
        let markdown = "# Result\n\n```rust\nfn main() {}\n```";
        let plain = osc52_sequence(markdown, false).unwrap();
        let encoded = plain
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            markdown.as_bytes()
        );
        assert!(
            osc52_sequence(markdown, true)
                .unwrap()
                .starts_with("\x1bPtmux;")
        );
    }

    #[test]
    fn ssh_skips_native_clipboard() {
        let native_calls = Cell::new(0);
        let result = copy_to_clipboard_with(
            "text",
            CopyEnvironment {
                ssh: true,
                tmux: false,
            },
            |_| Ok(()),
            |_| Ok(()),
            |_| {
                native_calls.set(native_calls.get() + 1);
                Err("unexpected".to_string())
            },
        );
        assert!(result.unwrap().is_none());
        assert_eq!(native_calls.get(), 0);
    }

    #[test]
    fn local_copy_falls_back_from_native_to_terminal() {
        let result = copy_to_clipboard_with(
            "text",
            CopyEnvironment {
                ssh: false,
                tmux: false,
            },
            |_| Ok(()),
            |_| Ok(()),
            |_| Err("native unavailable".to_string()),
        );
        assert!(result.unwrap().is_none());
    }
}
