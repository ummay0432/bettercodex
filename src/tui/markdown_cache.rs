//! Source-backed assistant Markdown cache with Codex's stable-block streaming strategy.
//!
//! Completed top-level blocks remain rendered while only the final mutable block is reparsed.
//! Reference definitions deliberately fall back to a full parse because they can retroactively
//! change links elsewhere in the document.

use super::markdown;
use super::terminal_hyperlinks::HyperlinkLine;
use ratatui::text::Line;
use std::path::Path;

#[derive(Debug, Default)]
pub(super) struct MarkdownRenderCache {
    raw_source: String,
    sanitized_source: String,
    width: Option<usize>,
    render: IncrementalMarkdownRender,
    canonical: bool,
}

impl MarkdownRenderCache {
    pub(super) fn render(
        &mut self,
        raw_source: &str,
        width: usize,
        cwd: &Path,
        streaming: bool,
    ) -> Vec<HyperlinkLine> {
        let source_was_replaced = !raw_source.starts_with(&self.raw_source);
        if source_was_replaced {
            self.raw_source.clear();
            self.sanitized_source.clear();
            self.render.clear();
            self.canonical = false;
        }

        if raw_source.len() > self.raw_source.len() {
            let appended = &raw_source[self.raw_source.len()..];
            self.sanitized_source
                .push_str(&markdown::sanitize(appended));
            self.raw_source.push_str(appended);
            self.canonical = false;
        }

        if !streaming {
            let fully_sanitized = markdown::sanitize(raw_source);
            if fully_sanitized != self.sanitized_source {
                self.sanitized_source = fully_sanitized;
                self.canonical = false;
            }
            self.raw_source.clear();
            self.raw_source.push_str(raw_source);

            if self.width != Some(width) || !self.canonical {
                self.width = Some(width);
                self.render.clear();
                self.render.lines = markdown::render_markdown_agent_with_links_and_cwd(
                    &self.sanitized_source,
                    Some(width),
                    Some(cwd),
                );
                self.canonical = true;
            }

            return self.render.lines.clone();
        }

        if self.width != Some(width) {
            self.width = Some(width);
            self.render
                .recompute(&self.sanitized_source, Some(width), cwd);
            self.canonical = false;
        } else if !self.canonical {
            self.render.append(&self.sanitized_source, Some(width), cwd);
        }

        self.render.lines.clone()
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

#[cfg(test)]
#[path = "markdown_cache_tests.rs"]
mod tests;
