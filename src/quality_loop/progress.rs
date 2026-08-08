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
