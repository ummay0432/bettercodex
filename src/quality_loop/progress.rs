use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_NAME_WIDTH: usize = 32;
const MAX_PULSE_WIDTH: usize = 80;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoopProgress {
    pub(crate) name: String,
    pub(crate) phase: String,
    pub(crate) additions: Option<u64>,
    pub(crate) deletions: Option<u64>,
    pub(crate) pulse: String,
}

impl LoopProgress {
    pub(crate) fn new(
        name: &str,
        phase: impl Into<String>,
        additions: Option<u64>,
        deletions: Option<u64>,
        pulse: &str,
    ) -> Self {
        Self {
            name: normalize_name(name),
            phase: normalize_fragment(&phase.into(), 16, "eval"),
            additions,
            deletions,
            pulse: normalize_fragment(pulse, MAX_PULSE_WIDTH, "working"),
        }
    }

    pub(crate) fn stderr_line(&self) -> String {
        format!("{} │ {} │ {}", self.name, self.phase, self.pulse)
    }
}

pub(crate) fn normalize_name(value: &str) -> String {
    let safe = value
        .chars()
        .map(|character| {
            if character.is_control() || character == '│' {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let normalized = safe
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = normalize_fragment(&normalized, MAX_NAME_WIDTH, "Quality loop");
    if normalized.split_whitespace().count() > 2 {
        "Quality loop".to_string()
    } else {
        normalized
    }
}

pub(crate) fn normalize_fragment(value: &str, width: usize, fallback: &str) -> String {
    let flattened = value
        .chars()
        .map(|character| {
            if character.is_control() || character == '│' {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let flattened = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    let source = if flattened.is_empty() {
        fallback
    } else {
        &flattened
    };
    truncate_width(source, width)
}

pub(crate) fn truncate_width(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_string();
    }
    let ellipsis_width = UnicodeWidthStr::width("…");
    let mut output = String::new();
    for grapheme in value.graphemes(true) {
        if UnicodeWidthStr::width(output.as_str())
            .saturating_add(UnicodeWidthStr::width(grapheme))
            .saturating_add(ellipsis_width)
            > width
        {
            break;
        }
        output.push_str(grapheme);
    }
    if output.is_empty() && width < ellipsis_width {
        return String::new();
    }
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_model_derived_fragments_before_terminal_output() {
        let progress = LoopProgress::new(
            "\u{1b}[31mShopify │ speed extra",
            "2/3\nignored",
            Some(12),
            Some(4),
            "kept\r\n│ \u{1b}[32m18% faster",
        );
        assert_eq!(progress.name, "[31mShopify speed");
        assert_eq!(progress.phase, "2/3 ignored");
        assert_eq!(progress.pulse, "kept [32m18% faster");
        let line = progress.stderr_line();
        assert!(!line.contains('\u{1b}'));
        assert_eq!(line.matches('│').count(), 2);
        assert!(!line.contains("+12"));
    }

    #[test]
    fn truncation_uses_display_width_without_splitting_graphemes() {
        assert_eq!(truncate_width("Shopify speed", 8), "Shopify…");
        assert_eq!(truncate_width("e\u{301}clair", 3), "e\u{301}c…");
        assert!(UnicodeWidthStr::width(truncate_width("界面速度", 5).as_str()) <= 5);
        assert_eq!(truncate_width("abc", 0), "");
    }

    #[test]
    fn invalid_or_overlong_name_falls_back_or_is_bounded() {
        assert_eq!(normalize_name(" \n │ \t"), "Quality loop");
        let name = normalize_name("one two three four");
        assert_eq!(name, "one two");
        assert!(UnicodeWidthStr::width(name.as_str()) <= MAX_NAME_WIDTH);
    }
}
