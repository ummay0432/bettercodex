use super::*;
use crate::tui::markdown_render;
use crate::tui::terminal_hyperlinks::HyperlinkLine;
use crate::tui::width::display_width;
use pretty_assertions::assert_eq;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Text;
use std::path::Path;

const CWD: &str = "/workspace/project";

fn rendered(source: &str, width: Option<usize>) -> Vec<HyperlinkLine> {
    let source = sanitize(source);
    render_markdown_agent_with_links_and_cwd(&source, width, Some(Path::new(CWD)))
}

fn visible(source: &str, width: Option<usize>) -> Vec<String> {
    rendered(source, width)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

#[test]
fn headings_keep_codex_styles_and_blank_spacing() {
    let text = markdown_render::render_markdown_text(
        "# One\n## Two\n### Three\n#### Four\n##### Five\n###### Six\n",
    );
    assert_eq!(
        text,
        Text::from_iter([
            Line::from_iter(["# ".bold().underlined(), "One".bold().underlined()]),
            Line::default(),
            Line::from_iter(["## ".bold(), "Two".bold()]),
            Line::default(),
            Line::from_iter(["### ".bold().italic(), "Three".bold().italic()]),
            Line::default(),
            Line::from_iter(["#### ".italic(), "Four".italic()]),
            Line::default(),
            Line::from_iter(["##### ".italic(), "Five".italic()]),
            Line::default(),
            Line::from_iter(["###### ".italic(), "Six".italic()]),
        ])
    );
}

#[test]
fn paragraphs_breaks_nested_lists_and_blockquotes_keep_structure() {
    let source = "Before\nsoft  \nhard\n\n1. outer\n   - inner words that wrap here\n     > quoted continuation\n\nAfter";
    assert_eq!(
        visible(source, Some(24)),
        vec![
            "Before",
            "soft",
            "hard",
            "",
            "1. outer",
            "    - inner words that",
            "      wrap here",
            "      > quoted",
            "      > continuation",
            "",
            "After",
        ]
    );
}

#[test]
fn inline_styles_rules_and_raw_html_are_preserved() {
    let lines = rendered(
        "**strong *emphasis*** ~~gone~~ `code`\n\n---\n\nHello <span>world</span>!\n\n<div>\n  raw\n</div>",
        Some(80),
    );
    assert_eq!(
        lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>(),
        vec![
            "strong emphasis gone code",
            "",
            "———",
            "",
            "Hello <span>world</span>!",
            "<div>",
            "  raw",
            "</div>",
        ]
    );
    let first = &lines[0].spans;
    assert!(
        first
            .iter()
            .any(|span| span.content == "strong "
                && span.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(first.iter().any(
        |span| span.content == "emphasis" && span.style.add_modifier.contains(Modifier::ITALIC)
    ));
    assert!(
        first.iter().any(|span| span.content == "gone"
            && span.style.add_modifier.contains(Modifier::CROSSED_OUT))
    );
    assert!(
        first
            .iter()
            .any(|span| span.content == "code" && span.style.fg == Some(Color::Cyan))
    );
}

#[test]
fn task_lists_and_footnotes_survive_the_codex_renderer() {
    let source = "- [x] shipped\n- [ ] pending with note[^1]\n\n[^1]: Footnote **detail**.";
    let lines = rendered(source, Some(60));
    let text = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert!(text.iter().any(|line| line == "- [x] shipped"), "{text:?}");
    assert!(
        text.iter().any(|line| line == "- [ ] pending with note[1]"),
        "{text:?}"
    );
    assert!(
        text.iter().any(|line| line.contains("[1] Footnote detail")),
        "{text:?}"
    );
    assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
        span.content.contains("detail") && span.style.add_modifier.contains(Modifier::BOLD)
    }));
}

#[test]
fn reference_links_resolve_and_incomplete_markdown_stays_visible() {
    let complete = rendered(
        "Read [the guide][guide].\n\n[guide]: https://example.com/docs",
        Some(80),
    );
    assert!(
        complete
            .iter()
            .flat_map(|line| &line.hyperlinks)
            .all(|link| link.destination == "https://example.com/docs")
    );
    assert!(
        complete
            .iter()
            .flat_map(|line| &line.hyperlinks)
            .next()
            .is_some()
    );

    for source in [
        "**unfinished",
        "[unfinished](https://example.com",
        "```rust\nfn main(",
        "| A | B |\n| ---",
    ] {
        let text = visible(source, Some(40)).join("\n");
        assert!(
            !text.is_empty(),
            "incomplete source disappeared: {source:?}"
        );
    }
}

#[test]
fn fenced_code_highlights_aliases_falls_back_and_remains_unwrapped() {
    let highlighted = rendered("```python3\ndef answer(): return 42\n```", Some(12));
    assert_eq!(highlighted.len(), 1);
    assert_eq!(highlighted[0].to_string(), "def answer(): return 42");
    assert!(
        highlighted[0]
            .spans
            .iter()
            .any(|span| span.style.fg.is_some())
    );

    let fallback = rendered("```not-a-language\na very long code line\n```", Some(8));
    assert_eq!(
        fallback
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>(),
        vec!["a very long code line"]
    );
}

#[test]
fn tables_render_styled_aligned_grids_with_rich_cells() {
    let lines = rendered(
        "| Left | Center | Right |\n| :--- | :---: | ---: |\n| **bold** | [docs](https://example.com) | `42` |",
        Some(60),
    );
    let text = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert!(text[0].starts_with(" Left"), "{text:?}");
    assert!(text[0].contains("Center"), "{text:?}");
    assert!(text[0].ends_with("Right"), "{text:?}");
    assert!(text[1].contains('━'));
    assert!(text[2].contains("bold"));
    assert!(lines[0].style.add_modifier.contains(Modifier::BOLD));
    assert!(
        lines[2]
            .spans
            .iter()
            .any(|span| span.content == "bold" && span.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.hyperlinks)
            .any(|link| link.destination == "https://example.com")
    );
}

#[test]
fn narrow_tables_fall_back_to_rich_key_value_records() {
    let lines = rendered(
        "| Key | Content | Extra | More |\n| --- | --- | --- | --- |\n| item | [link](https://example.com) | **bold** | `code` |",
        Some(16),
    );
    let text = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert!(text.iter().any(|line| line.contains("Key")));
    assert!(text.iter().any(|line| line.contains("item")));
    assert!(text.iter().any(|line| line.contains("bold")));
    assert!(!text.iter().any(|line| line.contains('━')));
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.hyperlinks)
            .next()
            .is_some()
    );
    assert!(
        lines
            .iter()
            .all(|line| display_width(&line.to_string()) <= 16)
    );
}

#[test]
fn table_widths_and_wrapping_use_terminal_cell_geometry() {
    let lines = rendered(
        "| Key | Notes |\n| --- | --- |\n| \u{ff76}\u{ff9e}\u{ff8a}\u{ff9f}tail | First \u{6f22}\u{5b57} row with an escaped \\| pipe. |\n| short | Final \u{ff21} row. |",
        Some(23),
    );
    assert!(lines.iter().any(|line| line.to_string().contains("ｶﾞﾊﾟ")));
    let text = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains('漢') && text.contains('字'), "{text}");
    assert!(
        lines
            .iter()
            .all(|line| display_width(&line.to_string()) <= 23)
    );
}

#[test]
fn malformed_table_spillover_is_kept_as_plain_content() {
    let lines = visible(
        "| A | B |\n| --- | --- |\n| one | two |\n| detached |\n<div>outside</div>",
        Some(40),
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("one") && line.contains("two"))
    );
    assert!(lines.iter().any(|line| line.contains("detached")));
    assert!(lines.iter().any(|line| line.contains("<div>outside</div>")));
}

#[test]
fn markdown_fences_only_unwrap_complete_tables() {
    let table = visible(
        "```markdown\n| A | B |\n| --- | --- |\n| 1 | 2 |\n```",
        Some(40),
    );
    assert!(table.iter().any(|line| line.contains('━')));

    let prose = visible("```md\n**still code**\n```", Some(40));
    assert_eq!(prose, vec!["**still code**"]);
    let incomplete = visible("```md\n| A | B |\n| --- | --- |", Some(40));
    assert!(incomplete.iter().any(|line| line.contains("| A | B |")));
}

#[test]
fn explicit_and_bare_web_links_have_safe_visible_fallbacks() {
    let lines = rendered(
        "[docs](https://example.com/docs), bare (https://example.org/a_(b)). [unsafe](javascript:alert(1))",
        Some(48),
    );
    let text = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("docs (https://example.com/docs)"));
    assert!(text.contains("https://example.org/a_(b)"));
    assert!(text.contains("unsafe"), "{text}");
    assert!(text.contains("javascript:alert"), "{text}");
    let destinations = lines
        .iter()
        .flat_map(|line| &line.hyperlinks)
        .map(|link| link.destination.as_str())
        .collect::<Vec<_>>();
    assert!(destinations.contains(&"https://example.com/docs"));
    assert!(destinations.contains(&"https://example.org/a_(b)"));
    assert!(
        !destinations
            .iter()
            .any(|destination| destination.starts_with("javascript:"))
    );
}

#[test]
fn local_file_links_render_from_destinations_not_labels() {
    let source = concat!(
        "[wrong](/workspace/project/src/My%20File.rs#L12C3) ",
        "[outside](/opt/shared/lib.rs:4:2) ",
        "[relative](../other/mod.rs:7) ",
        "[dot](./src/main.rs) ",
        "[file](file:///workspace/project/README.md#L2-L4)"
    );
    let text = visible(source, Some(120)).join("\n");
    assert!(text.contains("src/My File.rs:12:3"), "{text}");
    assert!(text.contains("/opt/shared/lib.rs:4:2"), "{text}");
    assert!(text.contains("../other/mod.rs:7"), "{text}");
    assert!(text.contains("./src/main.rs"), "{text}");
    assert!(text.contains("README.md:2-4"), "{text}");
    assert!(!text.contains("wrong"));
}

#[test]
fn wrapping_preserves_styles_indentation_and_hyperlink_ranges() {
    let lines = rendered(
        "- **Read the detailed documentation** at https://example.com/a/very/long/path?q=one",
        Some(32),
    );
    assert!(lines.len() >= 3);
    assert!(lines[0].to_string().starts_with("- "));
    assert!(
        lines
            .iter()
            .skip(1)
            .all(|line| line.to_string().starts_with("  "))
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.content.contains("Read") || span.content.contains("documentation"))
            .all(|span| span.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.hyperlinks)
            .all(|link| link.destination == "https://example.com/a/very/long/path?q=one")
    );
}

#[test]
fn sanitizer_removes_terminal_controls_without_removing_unicode() {
    assert_eq!(
        sanitize("ok \u{ff21}\u{1b}[31mred\u{1b}[0m\u{1b}]0;secret\u{7}done\u{0}"),
        "ok \u{ff21}reddone"
    );
}
