//! Focused truecolor detection.

use std::io::IsTerminal;
use std::sync::OnceLock;

pub(crate) fn stdout_supports_truecolor() -> bool {
    static SUPPORTS_TRUECOLOR: OnceLock<bool> = OnceLock::new();
    *SUPPORTS_TRUECOLOR.get_or_init(detect_stdout_truecolor)
}

fn detect_stdout_truecolor() -> bool {
    if let Some(forced) = force_color_level() {
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

fn force_color_level() -> Option<usize> {
    let force_color = std::env::var("FORCE_COLOR").ok();
    let clicolor_force = std::env::var("CLICOLOR_FORCE").ok();
    force_color_level_from_values(force_color.as_deref(), clicolor_force.as_deref())
}

fn force_color_level_from_values(
    force_color: Option<&str>,
    clicolor_force: Option<&str>,
) -> Option<usize> {
    if let Some(force) = force_color {
        Some(match force {
            "true" | "" => 1,
            "false" => 0,
            value => value.parse().unwrap_or(1).min(3),
        })
    } else {
        clicolor_force
            .is_some_and(|value| value != "0")
            .then_some(1)
    }
}

fn no_color() -> bool {
    !matches!(std::env::var("NO_COLOR").as_deref(), Ok("0") | Err(_))
}

#[cfg(test)]
mod tests {
    use super::force_color_level_from_values;

    #[test]
    fn explicit_force_color_disable_remains_authoritative() {
        assert_eq!(force_color_level_from_values(Some("0"), Some("1")), Some(0));
        assert_eq!(force_color_level_from_values(Some("false"), None), Some(0));
        assert_eq!(force_color_level_from_values(None, Some("0")), None);
        assert_eq!(force_color_level_from_values(None, None), None);
    }

    #[test]
    fn force_color_levels_are_bounded_for_truecolor_detection() {
        assert_eq!(force_color_level_from_values(Some(""), None), Some(1));
        assert_eq!(force_color_level_from_values(Some("2"), None), Some(2));
        assert_eq!(force_color_level_from_values(Some("3"), None), Some(3));
        assert_eq!(force_color_level_from_values(Some("9"), None), Some(3));
        assert_eq!(force_color_level_from_values(None, Some("1")), Some(1));
    }
}
