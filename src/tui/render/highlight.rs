//! Fixed, adaptive syntax highlighting for transcript code fences.
//!
//! Ported from Codex CLI revision `1669c2403f793d0230065397dfc25f52b844244e`.
//! bettercodex deliberately keeps the adaptive default themes and safety limits, but has no
//! configurable theme or theme-picker infrastructure.

use super::super::palette;
use ratatui::style::Color as RtColor;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::Color as SyntectColor;
use syntect::highlighting::FontStyle;
use syntect::highlighting::Highlighter;
use syntect::highlighting::Style as SyntectStyle;
use syntect::highlighting::Theme;
use syntect::parsing::Scope;
use syntect::parsing::SyntaxReference;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use two_face::theme::EmbeddedThemeName;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

const ANSI_ALPHA_INDEX: u8 = 0x00;
const ANSI_ALPHA_DEFAULT: u8 = 0x01;
const OPAQUE_ALPHA: u8 = 0xFF;
const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
const MAX_HIGHLIGHT_LINES: usize = 10_000;
const MAX_HIGHLIGHT_LINE_BYTES: usize = 4 * 1024;

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        two_face::theme::extra()
            .get(adaptive_theme_name(palette::default_background()))
            .clone()
    })
}

fn adaptive_theme_name(background: Option<(u8, u8, u8)>) -> EmbeddedThemeName {
    match background {
        Some(background) if palette::is_light(background) => EmbeddedThemeName::CatppuccinLatte,
        _ => EmbeddedThemeName::CatppuccinMocha,
    }
}

/// Query the adaptive syntax theme for the first foreground supplied by these scopes.
pub(crate) fn foreground_style_for_scopes(scope_names: &[&str]) -> Option<Style> {
    let highlighter = Highlighter::new(theme());
    scope_names.iter().find_map(|scope_name| {
        let scope = Scope::new(scope_name).ok()?;
        let foreground = highlighter.style_mod_for_stack(&[scope]).foreground?;
        convert_syntect_color(foreground).map(|color| Style::default().fg(color))
    })
}

fn find_syntax(language: &str) -> Option<&'static SyntaxReference> {
    let syntax_set = syntax_set();
    let normalized = language.to_ascii_lowercase();
    let language = match normalized.as_str() {
        "csharp" | "c-sharp" => "c#",
        "cu" | "cuh" | "cppm" | "cxxm" | "ixx" => "cpp",
        "golang" => "go",
        "python3" => "python",
        "shell" => "bash",
        _ => language,
    };

    syntax_set
        .find_syntax_by_token(language)
        .or_else(|| syntax_set.find_syntax_by_name(language))
        .or_else(|| {
            let language = language.to_ascii_lowercase();
            syntax_set
                .syntaxes()
                .iter()
                .find(|syntax| syntax.name.to_ascii_lowercase() == language)
        })
        .or_else(|| syntax_set.find_syntax_by_extension(language))
}

fn highlight_to_line_spans(code: &str, language: &str) -> Option<Vec<Vec<Span<'static>>>> {
    if code.is_empty()
        || code.len() > MAX_HIGHLIGHT_BYTES
        || code.lines().count() > MAX_HIGHLIGHT_LINES
        || code
            .lines()
            .any(|line| line.len() > MAX_HIGHLIGHT_LINE_BYTES)
    {
        return None;
    }

    let syntax = find_syntax(language)?;
    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(code) {
        let ranges = highlighter.highlight_line(line, syntax_set()).ok()?;
        let mut spans = Vec::new();
        for (style, text) in ranges {
            let text = text.trim_end_matches(['\n', '\r']);
            if !text.is_empty() {
                spans.push(Span::styled(text.to_string(), convert_style(style)));
            }
        }
        if spans.is_empty() {
            spans.push(Span::raw(String::new()));
        }
        lines.push(spans);
    }
    Some(lines)
}

/// Highlight a fenced block, falling back to equivalent plain lines for unknown or unsafe input.
pub(crate) fn highlight_code_to_lines(code: &str, language: &str) -> Vec<Line<'static>> {
    if let Some(lines) = highlight_to_line_spans(code, language) {
        return lines.into_iter().map(Line::from).collect();
    }

    let mut lines = code
        .lines()
        .map(|line| Line::from(line.to_string()))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

fn convert_style(style: SyntectStyle) -> Style {
    let mut converted = Style::default();
    if let Some(foreground) = convert_syntect_color(style.foreground) {
        converted = converted.fg(foreground);
    }
    if style.font_style.contains(FontStyle::BOLD) {
        converted.add_modifier |= Modifier::BOLD;
    }
    converted
}

fn convert_syntect_color(color: SyntectColor) -> Option<RtColor> {
    match color.a {
        ANSI_ALPHA_INDEX => Some(ansi_palette_color(color.r)),
        ANSI_ALPHA_DEFAULT => None,
        OPAQUE_ALPHA => Some(RtColor::Rgb(color.r, color.g, color.b)),
        _ => Some(RtColor::Rgb(color.r, color.g, color.b)),
    }
}

fn ansi_palette_color(index: u8) -> RtColor {
    match index {
        0 => RtColor::Black,
        1 => RtColor::Red,
        2 => RtColor::Green,
        3 => RtColor::Yellow,
        4 => RtColor::Blue,
        5 => RtColor::Magenta,
        6 => RtColor::Cyan,
        7 => RtColor::Gray,
        index => RtColor::Indexed(index),
    }
}
