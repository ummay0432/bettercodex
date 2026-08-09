use std::path::PathBuf;

use super::shell_detect::ShellType;
use super::shell_detect::detect_shell_type;

const POWERSHELL_FLAGS: &[&str] = &["-nologo", "-noprofile", "-command", "-c"];

/// Prefix used by current upstream Codex so redirected Windows PowerShell
/// output is UTF-8 instead of the active legacy console code page.
pub const UTF8_OUTPUT_PREFIX: &str =
    "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\n";

/// bettercodex already identifies the shell before constructing its argument
/// vector, so this focused upstream port accepts the script body directly.
pub fn prefix_powershell_script_with_utf8(script: &str) -> String {
    if script.trim_start().starts_with(UTF8_OUTPUT_PREFIX) {
        script.to_string()
    } else {
        format!("{UTF8_OUTPUT_PREFIX}{script}")
    }
}

/// Extract the PowerShell script body from an invocation such as:
///
/// - ["pwsh", "-NoProfile", "-Command", "Get-ChildItem -Recurse | Select-String foo"]
/// - ["pwsh", "-NoLogo", "-NoProfile", "-Command", "...script..."]
///
/// Returns (`shell`, `script`) when the first arg is a PowerShell executable and a
/// `-Command` (or `-c`) flag is present followed by a script string.
pub fn extract_powershell_command(command: &[String]) -> Option<(&str, &str)> {
    if command.len() < 3 {
        return None;
    }

    let shell = &command[0];
    if !matches!(
        detect_shell_type(PathBuf::from(shell)),
        Some(ShellType::PowerShell)
    ) {
        return None;
    }

    // Find the first occurrence of -Command (accept common short alias -c as well)
    let mut i = 1usize;
    while i + 1 < command.len() {
        let flag = &command[i];
        // Reject unknown flags
        if !POWERSHELL_FLAGS.contains(&flag.to_ascii_lowercase().as_str()) {
            return None;
        }
        if flag.eq_ignore_ascii_case("-Command") || flag.eq_ignore_ascii_case("-c") {
            let script = &command[i + 1];
            return Some((shell, script));
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_script_with_best_effort_utf8_output() {
        assert_eq!(
            prefix_powershell_script_with_utf8("Write-Output 'ü'"),
            format!("{UTF8_OUTPUT_PREFIX}Write-Output 'ü'")
        );
    }

    #[test]
    fn does_not_duplicate_existing_utf8_prefix() {
        let script = format!("  {UTF8_OUTPUT_PREFIX}Write-Output ok");
        assert_eq!(prefix_powershell_script_with_utf8(&script), script);
    }
}
