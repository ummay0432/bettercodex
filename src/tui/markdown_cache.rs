//! Source-backed assistant Markdown cache with Codex's stable-block streaming strategy.
//!
//! Completed top-level blocks remain rendered while only the final mutable block is reparsed.
//! Reference definitions deliberately fall back to a full parse because they can retroactively
//! change links elsewhere in the document. The raw transcript source is the sole ordinary-text
//! buffer; a filtered copy is allocated only after a control character appears.

use super::markdown;
use super::terminal_hyperlinks::HyperlinkLine;
use ratatui::text::Line;
use std::path::Path;

#[derive(Debug, Default)]
pub(super) struct MarkdownRenderCache {
    raw_source: String,
    sanitized_source: Option<String>,
    sanitizer: markdown::StreamingSanitizer,
    width: Option<usize>,
    render: IncrementalMarkdownRender,
    canonical: bool,
}

impl MarkdownRenderCache {
    pub(super) fn new(raw_source: String) -> Self {
        let mut sanitizer = markdown::StreamingSanitizer::default();
        let sanitized_source = markdown::requires_sanitization(&raw_source).then(|| {
            let mut sanitized = String::with_capacity(raw_source.len());
            sanitizer.push(&raw_source, &mut sanitized);
            sanitized
        });
        Self {
            raw_source,
            sanitized_source,
            sanitizer,
            ..Self::default()
        }
    }

    pub(super) fn source(&self) -> &str {
        &self.raw_source
    }

    pub(super) fn append(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(sanitized_source) = self.sanitized_source.as_mut() {
            self.sanitizer.push(delta, sanitized_source);
        } else if markdown::requires_sanitization(delta) {
            let mut sanitized_source =
                String::with_capacity(self.raw_source.len().saturating_add(delta.len()));
            sanitized_source.push_str(&self.raw_source);
            self.sanitizer.push(delta, &mut sanitized_source);
            self.sanitized_source = Some(sanitized_source);
        }
        self.raw_source.push_str(delta);
        self.canonical = false;
    }

    pub(super) fn replace(&mut self, raw_source: String) {
        if self.raw_source != raw_source {
            *self = Self::new(raw_source);
        }
    }

    pub(super) fn render_finalized(&mut self, width: usize, cwd: &Path) -> &[HyperlinkLine] {
        if self.width != Some(width) || !self.canonical {
            self.width = Some(width);
            self.render.clear();
            let source = self.sanitized_source.as_deref().unwrap_or(&self.raw_source);
            self.render.lines =
                markdown::render_markdown_agent_with_links_and_cwd(source, Some(width), Some(cwd));
            self.canonical = true;
        }
        &self.render.lines
    }

    pub(super) fn render_streaming(&mut self, width: usize, cwd: &Path) -> &[HyperlinkLine] {
        let source = self.sanitized_source.as_deref().unwrap_or(&self.raw_source);
        if self.width != Some(width) {
            self.width = Some(width);
            self.render.recompute(source, Some(width), cwd);
            self.canonical = false;
        } else if !self.canonical {
            self.render.append(source, Some(width), cwd);
        }

        &self.render.lines
    }
}

/// Incremental render state split at source and rendered-line boundaries.
#[derive(Debug, Default)]
struct IncrementalMarkdownRender {
    lines: Vec<HyperlinkLine>,
    stable_source_len: usize,
    stable_rendered_len: usize,
    parsed_source_len: usize,
    has_reference_link_definition: bool,
}

impl IncrementalMarkdownRender {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn recompute(&mut self, source: &str, width: Option<usize>, cwd: &Path) {
        let rendered =
            markdown::render_streaming_markdown_agent_with_links_and_cwd(source, width, Some(cwd));
        self.lines = rendered.lines;
        self.stable_source_len = 0;
        self.stable_rendered_len = 0;
        self.parsed_source_len = source.len();
        self.has_reference_link_definition = rendered.has_reference_link_definition;
    }

    fn append(&mut self, source: &str, width: Option<usize>, cwd: &Path) {
        if source.len() == self.parsed_source_len {
            return;
        }
        if source.len() < self.parsed_source_len || self.has_reference_link_definition {
            self.recompute(source, width, cwd);
            return;
        }

        let pending_source = &source[self.stable_source_len..];
        let pending = markdown::render_streaming_markdown_agent_with_links_and_cwd(
            pending_source,
            width,
            Some(cwd),
        );
        if pending.has_reference_link_definition {
            self.has_reference_link_definition = true;
            self.recompute(source, width, cwd);
            return;
        }

        let mut newly_stable_rendered_len = None;
        if let Some(boundary) = pending.last_top_level_block_start {
            let newly_stable_source = &pending_source[..boundary];
            let newly_stable = markdown::render_markdown_agent_with_links_and_cwd(
                newly_stable_source,
                width,
                Some(cwd),
            );
            self.stable_source_len += boundary;
            newly_stable_rendered_len = Some(newly_stable.len());
        }

        self.lines.truncate(self.stable_rendered_len);
        if !self.lines.is_empty()
            && (!pending.lines.is_empty() || !pending_source.trim().is_empty())
            && !pending.first_top_level_block_is_html
        {
            self.lines.push(HyperlinkLine::new(Line::default()));
        }
        let pending_render_start = self.lines.len();
        self.lines.extend(pending.lines);
        if let Some(newly_stable_rendered_len) = newly_stable_rendered_len {
            self.stable_rendered_len = pending_render_start + newly_stable_rendered_len;
        }
        self.parsed_source_len = source.len();
    }
}
