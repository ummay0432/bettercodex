use super::*;
use std::path::Path;

fn canonical(source: &str, width: usize) -> Vec<HyperlinkLine> {
    let source = markdown::sanitize(source);
    markdown::render_markdown_agent_with_links_and_cwd(
        &source,
        Some(width),
        Some(Path::new("/workspace/project")),
    )
}

#[test]
fn incremental_render_matches_canonical_render_for_incomplete_streams() {
    let chunks = [
        "# Heading\n\nA paragraph with **bo",
        "ld** and https://example.com/a_(b).\n\n- item one\n",
        "- item two\n\n| Name | State |\n| --- | :---: |\n| renderer | rea",
        "dy |\n\n```rust\nfn main() {\n    println!(\"ok\");\n",
        "```\n",
    ];
    let mut source = String::new();
    let mut cache = MarkdownRenderCache::default();
    for chunk in chunks {
        source.push_str(chunk);
        assert_eq!(
            cache.render(
                &source,
                48,
                Path::new("/workspace/project"),
                /*streaming*/ true,
            ),
            canonical(&source, 48),
            "mismatch after appending {chunk:?}",
        );
    }
}

#[test]
fn completed_top_level_blocks_become_a_stable_prefix() {
    let cwd = Path::new("/workspace/project");
    let mut cache = MarkdownRenderCache::default();
    let first = "First paragraph.\n";
    let second = "\nSecond paragraph starts";

    let _ = cache.render(first, 40, cwd, /*streaming*/ true);
    let _ = cache.render(
        &format!("{first}{second}"),
        40,
        cwd,
        /*streaming*/ true,
    );

    assert!(cache.render.stable_source_len >= first.len());
    assert!(cache.render.stable_rendered_len > 0);
    assert!(cache.render.stable_source_len < first.len() + second.len());
}

#[test]
fn reference_definitions_keep_source_wide_rendering_correct() {
    let cwd = Path::new("/workspace/project");
    let mut source = "See [the guide][guide].\n\n".to_string();
    let mut cache = MarkdownRenderCache::default();
    let _ = cache.render(&source, 50, cwd, /*streaming*/ true);
    source.push_str("[guide]: https://example.com/guide\n");

    assert_eq!(
        cache.render(&source, 50, cwd, /*streaming*/ true),
        canonical(&source, 50),
    );
    assert!(cache.render.has_reference_link_definition);
}

#[test]
fn finalization_resanitizes_split_terminal_control_sequences() {
    let cwd = Path::new("/workspace/project");
    let mut cache = MarkdownRenderCache::default();
    let mut source = "safe\u{1b}".to_string();
    let _ = cache.render(&source, 40, cwd, /*streaming*/ true);
    source.push_str("[31mred\u{1b}[0m");

    assert_eq!(
        cache.render(&source, 40, cwd, /*streaming*/ false),
        canonical(&source, 40),
    );
}
