//! Focused truecolor detection matching `supports-color` 3.0.2.

use std::io::IsTerminal;
use std::sync::OnceLock;

pub(crate) fn stdout_supports_truecolor() -> bool {
    static SUPPORTS_TRUECOLOR: OnceLock<bool> = OnceLock::new();
    *SUPPORTS_TRUECOLOR.get_or_init(detect_stdout_truecolor)
}

fn detect_stdout_truecolor() -> bool {
    let forced = force_color_level();
    if forced > 0 {
        return forced >= 3;
    }
    if no_color()
        || std::env::var("TERM").as_deref() == Ok("dumb")
        || !(std::io::stdout().is_terminal()
            || std::env::var("IGNORE_IS_TERMINAL").is_ok_and(|value| value != "0"))
    {
        return false;
    }

    std::env::var("COLORTERM").is_ok_and(|value| matches!(value.as_str(), "truecolor" | "24bit"))
        || std::env::var("TERM")
            .is_ok_and(|value| value.ends_with("direct") || value.ends_with("truecolor"))
        || std::env::var("TERM_PROGRAM").as_deref() == Ok("iTerm.app")
}

fn force_color_level() -> usize {
    if let Ok(force) = std::env::var("FORCE_COLOR") {
        match force.as_str() {
            "true" | "" => 1,
            "false" => 0,
            value => value.parse().unwrap_or(1).min(3),
        }
    } else if std::env::var("CLICOLOR_FORCE").is_ok_and(|value| value != "0") {
        1
    } else {
        0
    }
}

fn no_color() -> bool {
    !matches!(std::env::var("NO_COLOR").as_deref(), Ok("0") | Err(_))
}
