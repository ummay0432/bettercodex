use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use crate::tools::CatalogueMetrics;
use crate::tools::CatalogueRoute;
use crate::tools::CatalogueTool;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

const HEADER_HEIGHT: u16 = 2;
const FOOTER_HEIGHT: u16 = 1;

pub(super) struct ToolCatalogueView {
    metrics: CatalogueMetrics,
    scroll: usize,
    viewport_rows: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CatalogueAction {
    StayOpen,
    Close,
}

impl ToolCatalogueView {
    pub(super) fn new() -> Self {
        Self {
            metrics: crate::tools::catalogue_metrics(),
            scroll: 0,
            viewport_rows: 0,
        }
    }

    pub(super) fn preferred_height(&self) -> u16 {
        catalogue_content_height(crate::tools::display_tools())
            .saturating_add(HEADER_HEIGHT)
            .saturating_add(menu_surface_padding_height())
            .saturating_add(FOOTER_HEIGHT)
    }

    pub(super) fn handle_key(&mut self, code: KeyCode) -> CatalogueAction {
        let page = self.viewport_rows.max(1);
        let max_scroll = self.max_scroll();
        match code {
            KeyCode::Esc => return CatalogueAction::Close,
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1).min(max_scroll),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(page).min(max_scroll);
            }
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = max_scroll,
            _ => {}
        }
        CatalogueAction::StayOpen
    }

    pub(super) fn render(&mut self, frame: &mut Frame<'_>, area: Rect, surface_style: Style) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        let footer_height = FOOTER_HEIGHT.min(area.height);
        let content_area = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.saturating_sub(footer_height),
        );
        let footer_area = Rect::new(area.x, content_area.bottom(), area.width, footer_height);
        let inner = render_menu_surface(content_area, frame.buffer_mut(), surface_style);
        if inner.is_empty() {
            return;
        }

        let header_height = HEADER_HEIGHT.min(inner.height);
        let header_area = Rect::new(inner.x, inner.y, inner.width, header_height);
        frame.render_widget(
            Paragraph::new(vec![Line::from("Tools").bold(), Line::default()]),
            header_area,
        );
        let body_area = Rect::new(
            inner.x,
            header_area.bottom(),
            inner.width,
            inner.height.saturating_sub(header_height),
        );
        let lines = catalogue_lines(crate::tools::display_tools(), self.metrics);
        self.viewport_rows = usize::from(body_area.height);
        self.scroll = self
            .scroll
            .min(lines.len().saturating_sub(self.viewport_rows));
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(self.scroll)
                    .take(self.viewport_rows)
                    .collect::<Vec<_>>(),
            ),
            body_area,
        );
        if !footer_area.is_empty() {
            let hint_area = Rect::new(
                footer_area.x.saturating_add(2),
                footer_area.y,
                footer_area.width.saturating_sub(2),
                footer_area.height,
            );
            frame.render_widget(
                Paragraph::new("↑/↓ scroll · home/end jump · esc go back").dim(),
                hint_area,
            );
        }
    }

    fn max_scroll(&self) -> usize {
        catalogue_content_line_count(crate::tools::display_tools())
            .saturating_sub(self.viewport_rows)
    }
}

fn catalogue_lines<'a>(tools: &'a [CatalogueTool], metrics: CatalogueMetrics) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    for (heading, route) in [
        ("Request tools", CatalogueRoute::Request),
        ("Inside exec", CatalogueRoute::InsideExec),
    ] {
        let group = tools
            .iter()
            .filter(|tool| tool.route == route)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(heading).cyan().bold());
        for (index, tool) in group.iter().enumerate() {
            let branch = if index + 1 == group.len() {
                "  └─ "
            } else {
                "  ├─ "
            };
            lines.push(Line::from(vec![
                Span::from(branch).dim(),
                Span::from("● ").green(),
                Span::from(tool.name.as_str()),
            ]));
        }
    }
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    lines.push(
        Line::from(format!(
            "{} tools · ~{} prompt tokens",
            tools.len(),
            format_tokens(metrics.estimated_tokens),
        ))
        .dim(),
    );
    lines
}

fn catalogue_content_height(tools: &[CatalogueTool]) -> u16 {
    u16::try_from(catalogue_content_line_count(tools)).unwrap_or(u16::MAX)
}

fn catalogue_content_line_count(tools: &[CatalogueTool]) -> usize {
    let groups = [CatalogueRoute::Request, CatalogueRoute::InsideExec]
        .into_iter()
        .filter(|route| tools.iter().any(|tool| tool.route == *route))
        .count();
    tools
        .len()
        .saturating_add(groups)
        .saturating_add(groups.saturating_sub(1))
        .saturating_add(1 + usize::from(!tools.is_empty()))
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000 {
        let tenths = tokens.saturating_add(50) / 100;
        format!("{}.{:01}K", tenths / 10, tenths % 10)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn narrow_catalogue_can_scroll_to_every_tool_and_metrics() {
        let mut catalogue = ToolCatalogueView::new();
        let first = render(&mut catalogue, 56, 8);
        let last_tool = crate::tools::display_tools()
            .last()
            .expect("tool catalogue")
            .name
            .clone();
        assert!(!first.contains(&last_tool), "{first}");

        assert_eq!(
            catalogue.handle_key(KeyCode::End),
            CatalogueAction::StayOpen
        );
        let last = render(&mut catalogue, 56, 8);
        assert!(last.contains(&last_tool), "{last}");
        assert!(last.contains("prompt tokens"), "{last}");
        assert!(last.contains("↑/↓ scroll"), "{last}");

        catalogue.handle_key(KeyCode::Home);
        assert_eq!(render(&mut catalogue, 56, 8), first);
    }

    fn render(catalogue: &mut ToolCatalogueView, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| catalogue.render(frame, frame.area(), Style::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
