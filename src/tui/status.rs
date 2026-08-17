//! `/status` transcript card.
//!
//! This is a focused port of OpenAI Codex's `tui/src/status/{card,format,
//! helpers,rate_limits}.rs` at 279b93242cfef379e65da97e87e44b83c5934fd7.
//! Unsupported provider, sandbox-profile, and app-server fields are
//! intentionally absent; bettercodex's fixed equivalents are rendered directly.

use super::markdown;
use super::palette;
use super::render::line_utils::line_to_static;
use super::terminal_hyperlinks;
use super::terminal_hyperlinks::HyperlinkLine;
use super::width::display_width;
use super::width::line_width;
use crate::auth::ChatGptAccount;
use crate::context::ContextSnapshot;
use crate::model::ModelSelection;
use crate::model::RAW_CONTEXT_WINDOW;
use crate::rate_limits::CreditsSnapshot;
use crate::rate_limits::RateLimitSnapshot;
use crate::rate_limits::RateLimitWindow;
use chrono::DateTime;
use chrono::Duration as ChronoDuration;
use chrono::Local;
use chrono::Utc;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use unicode_segmentation::UnicodeSegmentation;

const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/codex/settings/usage";
const RATE_LIMIT_STALE_MINUTES: i64 = 15;
const RATE_LIMIT_BAR_SEGMENTS: usize = 20;
const BASELINE_CONTEXT_TOKENS: u64 = 12_000;

#[derive(Clone, Debug)]
pub(super) struct StatusSnapshot {
    pub(super) model: ModelSelection,
    pub(super) directory: PathBuf,
    pub(super) instruction_source_paths: Vec<PathBuf>,
    pub(super) session_id: String,
    pub(super) forked_from: Option<String>,
    pub(super) account: ChatGptAccount,
    pub(super) context: ContextSnapshot,
    pub(super) rate_limits: Vec<RateLimitSnapshot>,
    pub(super) refreshing_rate_limits: bool,
}

impl StatusSnapshot {
    pub(super) fn display_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut output = vec![terminal_hyperlinks::HyperlinkLine::new(Line::from(
            "/status".magenta(),
        ))];
        output.push(HyperlinkLine::default());
        output.extend(
            self.card_lines(width)
                .into_iter()
                .map(terminal_hyperlinks::annotate_web_urls_in_line),
        );
        output
    }

    fn card_lines(&self, width: u16) -> Vec<Line<'static>> {
        let available_inner_width = usize::from(width.saturating_sub(4));
        if available_inner_width == 0 {
            return Vec::new();
        }

        let rate_limit_rows = rate_limit_rows(&self.rate_limits, Local::now());
        let mut labels = vec!["Model", "Directory", "Permissions", "Agents.md"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut seen = labels.iter().cloned().collect::<BTreeSet<_>>();
        push_label(&mut labels, &mut seen, "Account");
        push_label(&mut labels, &mut seen, "Collaboration mode");
        push_label(&mut labels, &mut seen, "Session");
        if self.forked_from.is_some() {
            push_label(&mut labels, &mut seen, "Forked from");
        }
        if self.context.measured {
            push_label(&mut labels, &mut seen, "Context window");
        }
        match &rate_limit_rows {
            RateLimitRows::Available { rows, stale } => {
                for row in rows {
                    push_label(&mut labels, &mut seen, &row.label);
                }
                if *stale {
                    push_label(&mut labels, &mut seen, "Warning");
                }
            }
            RateLimitRows::Unavailable | RateLimitRows::Missing => {
                push_label(&mut labels, &mut seen, "Limits");
            }
        }
        let formatter = FieldFormatter::from_labels(labels.iter().map(String::as_str));
        let value_width = formatter.value_width(available_inner_width);

        let mut lines = vec![Line::from(vec![
            Span::from(format!("{}>_ ", FieldFormatter::INDENT)).dim(),
            Span::from("bettercodex").bold(),
            Span::from(" ").dim(),
            Span::from(format!("(v{})", env!("CARGO_PKG_VERSION"))).dim(),
        ])];
        lines.push(Line::default());

        let usage_note = [
            Line::from(vec![
                Span::styled("Visit ", palette::accent_text_style()),
                Span::styled(CHATGPT_USAGE_URL, palette::accent_link_style()),
                Span::styled(" for up-to-date", palette::accent_text_style()),
            ]),
            Line::styled(
                "information on rate limits and credits",
                palette::accent_text_style(),
            ),
        ];
        for source in &usage_note {
            lines.extend(
                super::wrapping::word_wrap_line(source, available_inner_width.max(1))
                    .iter()
                    .map(line_to_static),
            );
        }
        lines.push(Line::default());

        lines.push(formatter.line(
            "Model",
            vec![
                Span::from(self.model.model.clone()),
                Span::from(" (").dim(),
                Span::from(format!("reasoning {}", self.model.reasoning_effort)).dim(),
                Span::from(")").dim(),
            ],
        ));
        lines.push(formatter.line(
            "Directory",
            vec![Span::from(display_directory(
                &self.directory,
                Some(value_width),
            ))],
        ));
        lines.push(formatter.line("Permissions", vec![Span::from("Full Access")]));
        lines.push(formatter.line(
            "Agents.md",
            vec![Span::from(instruction_sources_summary(
                &self.directory,
                &self.instruction_source_paths,
            ))],
        ));
        lines.push(formatter.line("Account", vec![Span::from(account_display(&self.account))]));
        lines.push(formatter.line("Collaboration mode", vec![Span::from("Default")]));
        lines.push(formatter.line("Session", vec![Span::from(self.session_id.clone())]));
        if let Some(forked_from) = &self.forked_from {
            lines.push(formatter.line("Forked from", vec![Span::from(forked_from.clone())]));
        }

        lines.push(Line::default());
        if self.context.measured {
            lines.push(formatter.line("Context window", context_window_spans(&self.context)));
        }
        append_rate_limit_lines(
            &mut lines,
            &rate_limit_rows,
            self.refreshing_rate_limits,
            available_inner_width,
            &formatter,
        );

        let inner_width = lines
            .iter()
            .map(line_width)
            .max()
            .unwrap_or_default()
            .min(available_inner_width);
        let lines = lines
            .into_iter()
            .map(|line| truncate_line_to_width(line, inner_width))
            .collect();
        with_border(lines, inner_width)
    }
}

fn account_display(account: &ChatGptAccount) -> String {
    markdown::sanitize_inline(&match (&account.email, &account.plan) {
        (Some(email), Some(plan)) => format!("{email} ({plan})"),
        (Some(email), None) => email.clone(),
        (None, Some(plan)) => plan.clone(),
        (None, None) => "ChatGPT".to_string(),
    })
}

fn context_window_spans(context: &ContextSnapshot) -> Vec<Span<'static>> {
    let effective_window = RAW_CONTEXT_WINDOW.saturating_sub(BASELINE_CONTEXT_TOKENS);
    let used = context.used_tokens.saturating_sub(BASELINE_CONTEXT_TOKENS);
    let remaining = effective_window.saturating_sub(used);
    let percent = if effective_window == 0 {
        0
    } else {
        ((remaining as f64 / effective_window as f64) * 100.0)
            .clamp(0.0, 100.0)
            .round() as u64
    };
    vec![
        Span::from(format!("{percent}% left")),
        Span::from(" (").dim(),
        Span::from(format_tokens_compact(context.used_tokens)).dim(),
        Span::from(" used / ").dim(),
        Span::from(format_tokens_compact(RAW_CONTEXT_WINDOW)).dim(),
        Span::from(")").dim(),
    ]
}

#[derive(Clone, Debug)]
struct RateLimitRow {
    label: String,
    used_percent: Option<f64>,
    resets_at: Option<String>,
    text: Option<String>,
}

enum RateLimitRows {
    Available {
        rows: Vec<RateLimitRow>,
        stale: bool,
    },
    Unavailable,
    Missing,
}

fn rate_limit_rows(snapshots: &[RateLimitSnapshot], now: DateTime<Local>) -> RateLimitRows {
    if snapshots.is_empty() {
        return RateLimitRows::Missing;
    }
    let mut rows = Vec::new();
    let mut stale = false;
    for snapshot in snapshots {
        let captured_at = DateTime::<Utc>::from_timestamp(snapshot.captured_at, 0)
            .map(|time| time.with_timezone(&Local));
        stale |= captured_at.is_none_or(|captured| {
            now.signed_duration_since(captured) > ChronoDuration::minutes(RATE_LIMIT_STALE_MINUTES)
        });
        let limit_name =
            markdown::sanitize_inline(snapshot.limit_name.as_deref().unwrap_or(&snapshot.limit_id))
                .replace('_', "-");
        let is_codex = limit_name.eq_ignore_ascii_case("codex");
        let window_count =
            usize::from(snapshot.primary.is_some()) + usize::from(snapshot.secondary.is_some());
        if !is_codex && window_count > 1 {
            rows.push(RateLimitRow {
                label: format!("{limit_name} limit"),
                used_percent: None,
                resets_at: None,
                text: Some(String::new()),
            });
        }
        if let Some(primary) = &snapshot.primary {
            rows.push(window_row(
                primary,
                false,
                (!is_codex && window_count == 1).then_some(limit_name.as_str()),
                captured_at,
            ));
        }
        if let Some(secondary) = &snapshot.secondary {
            rows.push(window_row(
                secondary,
                true,
                (!is_codex && window_count == 1).then_some(limit_name.as_str()),
                captured_at,
            ));
        }
        if let Some(credits) = &snapshot.credits
            && let Some(row) = credits_row(credits)
        {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        RateLimitRows::Unavailable
    } else {
        RateLimitRows::Available { rows, stale }
    }
}

fn window_row(
    window: &RateLimitWindow,
    secondary: bool,
    bucket: Option<&str>,
    captured_at: Option<DateTime<Local>>,
) -> RateLimitRow {
    let label = limit_label(window.window_minutes, secondary);
    let label = bucket.map_or_else(
        || format!("{label} limit"),
        |bucket| format!("{bucket} {label} limit"),
    );
    let resets_at = window
        .resets_at
        .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
        .map(|time| {
            let local = time.with_timezone(&Local);
            let captured_at = captured_at.unwrap_or_else(Local::now);
            if local.date_naive() == captured_at.date_naive() {
                local.format("%H:%M").to_string()
            } else {
                local.format("%H:%M on %-d %b").to_string()
            }
        });
    RateLimitRow {
        label,
        used_percent: Some(window.used_percent),
        resets_at,
        text: None,
    }
}

fn credits_row(credits: &CreditsSnapshot) -> Option<RateLimitRow> {
    let text = if credits.unlimited {
        "Unlimited".to_string()
    } else if !credits.has_credits {
        return None;
    } else {
        credits
            .balance
            .as_deref()
            .and_then(|balance| balance.trim().parse::<f64>().ok())
            .filter(|balance| balance.is_finite() && *balance > 0.0)
            .map(|balance| format!("{balance:.0} credits"))
            .unwrap_or_else(|| "Available".to_string())
    };
    Some(RateLimitRow {
        label: "Credits".to_string(),
        used_percent: None,
        resets_at: None,
        text: Some(text),
    })
}

fn append_rate_limit_lines(
    lines: &mut Vec<Line<'static>>,
    rate_limits: &RateLimitRows,
    refreshing_rate_limits: bool,
    available_inner_width: usize,
    formatter: &FieldFormatter,
) {
    let RateLimitRows::Available { rows, stale } = rate_limits else {
        let text = match rate_limits {
            RateLimitRows::Unavailable => "not available for this account",
            RateLimitRows::Missing if refreshing_rate_limits => {
                "refresh requested; run /status again shortly."
            }
            RateLimitRows::Missing => "data not available yet",
            RateLimitRows::Available { .. } => unreachable!(),
        };
        lines.push(formatter.line("Limits", vec![Span::from(text).dim()]));
        return;
    };
    for row in rows {
        if let Some(text) = &row.text {
            lines.push(formatter.line(&row.label, vec![Span::from(text.clone())]));
            continue;
        }
        let percent_remaining = (100.0 - row.used_percent.unwrap_or_default()).clamp(0.0, 100.0);
        let summary = format!("{percent_remaining:.0}% left");
        let full = vec![
            Span::from(progress_bar(percent_remaining)),
            Span::from(" "),
            Span::from(summary.clone()),
        ];
        let value = if line_width(&Line::from(full.clone()))
            <= formatter.value_width(available_inner_width)
        {
            full
        } else {
            vec![Span::from(summary)]
        };
        let spans = formatter.full_spans(&row.label, value);
        let base_line = Line::from(spans.clone());
        if let Some(resets_at) = &row.resets_at {
            let reset_text = format!("(resets {resets_at})");
            let mut inline = spans;
            inline.push(Span::from(" ").dim());
            inline.push(Span::from(reset_text.clone()).dim());
            let projected = line_width(&Line::from(inline.clone()));
            if projected <= available_inner_width {
                lines.push(Line::from(inline));
            } else {
                lines.push(base_line);
                let reset_width = formatter.value_width(available_inner_width).max(1);
                let options = textwrap::Options::new(reset_width).break_words(false);
                lines.extend(
                    textwrap::wrap(&reset_text, options)
                        .into_iter()
                        .map(|wrapped| {
                            formatter.continuation(vec![Span::from(wrapped.into_owned()).dim()])
                        }),
                );
            }
        } else {
            lines.push(base_line);
        }
    }
    if *stale {
        lines.push(formatter.line(
            "Warning",
            vec![Span::from(if refreshing_rate_limits {
                "limits may be stale - run /status again shortly."
            } else {
                "limits may be stale - start new turn to refresh."
            })
            .dim()],
        ));
    }
}

fn progress_bar(percent_remaining: f64) -> String {
    let filled = ((percent_remaining / 100.0).clamp(0.0, 1.0) * RATE_LIMIT_BAR_SEGMENTS as f64)
        .round() as usize;
    format!(
        "[{}{}]",
        "█".repeat(filled.min(RATE_LIMIT_BAR_SEGMENTS)),
        "░".repeat(RATE_LIMIT_BAR_SEGMENTS.saturating_sub(filled)),
    )
}

fn limit_label(window_minutes: Option<i64>, secondary: bool) -> String {
    let duration = window_minutes.and_then(|minutes| {
        [
            (5 * 60, "5h"),
            (24 * 60, "Daily"),
            (7 * 24 * 60, "Weekly"),
            (30 * 24 * 60, "Monthly"),
            (365 * 24 * 60, "Annual"),
        ]
        .into_iter()
        .find(|(expected, _)| {
            let minutes = minutes.max(0) as f64;
            let expected = *expected as f64;
            minutes >= expected * 0.95 && minutes <= expected * 1.05
        })
        .map(|(_, label)| label.to_string())
    });
    duration.unwrap_or_else(|| {
        if secondary {
            "Secondary usage".to_string()
        } else {
            "Usage".to_string()
        }
    })
}

fn instruction_sources_summary(cwd: &Path, paths: &[PathBuf]) -> String {
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        let display = if path.parent() == Some(cwd) {
            file_name
        } else if let Some(parent) = path.parent() {
            let mut current = cwd;
            let mut levels = 0;
            let mut reached = false;
            while let Some(ancestor) = current.parent() {
                if current == parent {
                    reached = true;
                    break;
                }
                current = ancestor;
                levels += 1;
            }
            if reached {
                format!(
                    "{}{}",
                    format!("..{}", std::path::MAIN_SEPARATOR).repeat(levels),
                    file_name
                )
            } else if let Ok(relative) = path.strip_prefix(cwd) {
                relative.display().to_string()
            } else {
                display_directory(path, None)
            }
        } else {
            display_directory(path, None)
        };
        sources.push(markdown::sanitize_inline(&display));
    }
    if sources.is_empty() {
        "<none>".to_string()
    } else {
        sources.join(", ")
    }
}

fn display_directory(path: &Path, max_width: Option<usize>) -> String {
    let display = crate::paths::home_dir().and_then(|home| {
        path.strip_prefix(home).ok().map(|relative| {
            if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display())
            }
        })
    });
    let display = markdown::sanitize_inline(&display.unwrap_or_else(|| path.display().to_string()));
    max_width.map_or(display.clone(), |width| {
        center_truncate_path(&display, width)
    })
}

/// Port of Codex's path-aware status truncation: retain useful leading and
/// trailing segments rather than dropping the project name on narrow screens.
fn center_truncate_path(path: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if display_width(path) <= max_width {
        return path.to_string();
    }

    let separator = std::path::MAIN_SEPARATOR;
    let has_leading_separator = path.starts_with(separator);
    let has_trailing_separator = path.ends_with(separator);
    let mut raw_segments = path.split(separator).collect::<Vec<_>>();
    if has_leading_separator
        && raw_segments
            .first()
            .is_some_and(|segment| segment.is_empty())
    {
        raw_segments.remove(0);
    }
    if has_trailing_separator
        && raw_segments
            .last()
            .is_some_and(|segment| segment.is_empty())
    {
        raw_segments.pop();
    }
    if raw_segments.is_empty() {
        let root = separator.to_string();
        return if display_width(&root) <= max_width {
            root
        } else {
            "…".to_string()
        };
    }

    struct Segment<'a> {
        original: &'a str,
        text: String,
        truncatable: bool,
        is_suffix: bool,
    }

    let assemble = |segments: &[Segment<'_>]| {
        let mut output = String::new();
        if has_leading_separator {
            output.push(separator);
        }
        for segment in segments {
            if !output.is_empty() && !output.ends_with(separator) {
                output.push(separator);
            }
            output.push_str(&segment.text);
        }
        output
    };
    let front_truncate = |original: &str, allowed_width: usize| {
        if allowed_width == 0 {
            return String::new();
        }
        if display_width(original) <= allowed_width {
            return original.to_string();
        }
        if allowed_width == 1 {
            return "…".to_string();
        }
        let mut kept = Vec::new();
        let mut used_width = 1;
        for grapheme in original.graphemes(true).rev() {
            let width = display_width(grapheme);
            if used_width + width > allowed_width {
                break;
            }
            used_width += width;
            kept.push(grapheme);
        }
        kept.reverse();
        format!("…{}", kept.concat())
    };

    let segment_count = raw_segments.len();
    let mut combinations = Vec::new();
    for left in 1..=segment_count {
        let min_right = usize::from(left != segment_count);
        for right in min_right..=segment_count - left {
            combinations.push((left, right));
        }
    }
    let desired_suffix = segment_count.saturating_sub(1).min(2);
    combinations.sort_by(|(left_a, right_a), (left_b, right_b)| {
        let preferred_a = usize::from(*right_a >= desired_suffix);
        let preferred_b = usize::from(*right_b >= desired_suffix);
        preferred_b
            .cmp(&preferred_a)
            .then_with(|| left_b.cmp(left_a))
            .then_with(|| right_b.cmp(right_a))
            .then_with(|| (left_b + right_b).cmp(&(left_a + right_a)))
    });

    for (left_count, right_count) in combinations {
        let mut segments = raw_segments[..left_count]
            .iter()
            .map(|segment| Segment {
                original: segment,
                text: (*segment).to_string(),
                truncatable: true,
                is_suffix: false,
            })
            .collect::<Vec<_>>();
        let needs_ellipsis = left_count + right_count < segment_count;
        if needs_ellipsis {
            segments.push(Segment {
                original: "…",
                text: "…".to_string(),
                truncatable: false,
                is_suffix: false,
            });
        }
        if right_count > 0 {
            segments.extend(
                raw_segments[segment_count - right_count..]
                    .iter()
                    .map(|segment| Segment {
                        original: segment,
                        text: (*segment).to_string(),
                        truncatable: true,
                        is_suffix: true,
                    }),
            );
        }

        loop {
            let candidate = assemble(&segments);
            let width = display_width(&candidate);
            if width <= max_width {
                return candidate;
            }
            if !needs_ellipsis && segment_count > 2 {
                break;
            }

            let indices = segments
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, segment)| segment.truncatable)
                .map(|(index, segment)| (index, segment.is_suffix))
                .collect::<Vec<_>>();
            let mut changed = false;
            for prefer_suffix in [true, false] {
                for (index, is_suffix) in &indices {
                    if *is_suffix != prefer_suffix {
                        continue;
                    }
                    let original_width = display_width(segments[*index].original);
                    if original_width <= max_width && segment_count > 2 {
                        continue;
                    }
                    let segment_width = display_width(&segments[*index].text);
                    let other_width = width.saturating_sub(segment_width);
                    let allowed_width = max_width.saturating_sub(other_width).max(1);
                    let replacement = front_truncate(segments[*index].original, allowed_width);
                    if replacement != segments[*index].text {
                        segments[*index].text = replacement;
                        changed = true;
                        break;
                    }
                }
                if changed {
                    break;
                }
            }
            if !changed {
                break;
            }
        }
    }

    front_truncate(path, max_width)
}

fn format_tokens_compact(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    let value = value as f64;
    let (scaled, suffix) = if value >= 1_000_000_000_000.0 {
        (value / 1_000_000_000_000.0, "T")
    } else if value >= 1_000_000_000.0 {
        (value / 1_000_000_000.0, "B")
    } else if value >= 1_000_000.0 {
        (value / 1_000_000.0, "M")
    } else {
        (value / 1_000.0, "K")
    };
    let decimals = if scaled < 10.0 {
        2
    } else if scaled < 100.0 {
        1
    } else {
        0
    };
    let mut formatted = format!("{scaled:.decimals$}");
    while formatted.ends_with('0') && formatted.contains('.') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    format!("{formatted}{suffix}")
}

struct FieldFormatter {
    label_width: usize,
    value_offset: usize,
    value_indent: String,
}

impl FieldFormatter {
    const INDENT: &'static str = " ";

    fn from_labels(labels: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let label_width = labels
            .into_iter()
            .map(|label| display_width(label.as_ref()))
            .max()
            .unwrap_or_default();
        let value_offset = display_width(Self::INDENT) + label_width + 4;
        Self {
            label_width,
            value_offset,
            value_indent: " ".repeat(value_offset),
        }
    }

    fn line(&self, label: &str, value: Vec<Span<'static>>) -> Line<'static> {
        Line::from(self.full_spans(label, value))
    }

    fn continuation(&self, mut spans: Vec<Span<'static>>) -> Line<'static> {
        let mut output = Vec::with_capacity(spans.len() + 1);
        output.push(Span::from(self.value_indent.clone()).dim());
        output.append(&mut spans);
        Line::from(output)
    }

    fn full_spans(&self, label: &str, mut value: Vec<Span<'static>>) -> Vec<Span<'static>> {
        let mut spans = vec![self.label_span(label)];
        spans.append(&mut value);
        spans
    }

    fn value_width(&self, available: usize) -> usize {
        available.saturating_sub(self.value_offset)
    }

    fn label_span(&self, label: &str) -> Span<'static> {
        let padding = 3 + self.label_width.saturating_sub(display_width(label));
        Span::from(format!("{}{label}:{}", Self::INDENT, " ".repeat(padding))).dim()
    }
}

fn push_label(labels: &mut Vec<String>, seen: &mut BTreeSet<String>, label: &str) {
    if seen.insert(label.to_string()) {
        labels.push(label.to_string());
    }
}

fn truncate_line_to_width(line: Line<'static>, max_width: usize) -> Line<'static> {
    let mut used = 0;
    let mut output = Vec::new();
    for span in line.spans {
        if used >= max_width {
            break;
        }
        let style = span.style;
        let text = span.content.into_owned();
        let mut retained = String::new();
        for grapheme in text.graphemes(true) {
            let width = display_width(grapheme);
            if used + width > max_width {
                break;
            }
            retained.push_str(grapheme);
            used += width;
        }
        if !retained.is_empty() {
            output.push(Span::styled(retained, style));
        }
    }
    Line::from(output)
}

fn with_border(lines: Vec<Line<'static>>, inner_width: usize) -> Vec<Line<'static>> {
    let border_style = Style::default().fg(Color::Indexed(8));
    let mut output = Vec::with_capacity(lines.len() + 2);
    output.push(Line::from(Span::styled(
        format!("╭{}╮", "─".repeat(inner_width + 2)),
        border_style,
    )));
    for mut line in lines {
        let used = line_width(&line);
        let mut spans = vec![Span::styled("│ ", border_style)];
        spans.append(&mut line.spans);
        spans.push(Span::styled(
            " ".repeat(inner_width.saturating_sub(used)),
            border_style,
        ));
        spans.push(Span::styled(" │", border_style));
        output.push(Line::from(spans));
    }
    output.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(inner_width + 2)),
        border_style,
    )));
    output
}
