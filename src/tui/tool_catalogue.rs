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
        }
    }

    pub(super) fn preferred_height(&self) -> u16 {
        catalogue_content_height(crate::tools::display_tools())
            .saturating_add(HEADER_HEIGHT)
            .saturating_add(menu_surface_padding_height())
            .saturating_add(FOOTER_HEIGHT)
    }

    pub(super) fn handle_key(&self, code: KeyCode) -> CatalogueAction {
        match code {
            KeyCode::Esc => CatalogueAction::Close,
            _ => CatalogueAction::StayOpen,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect, surface_style: Style) {
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
        frame.render_widget(
            Paragraph::new(catalogue_lines(crate::tools::display_tools(), self.metrics)),
            body_area,
        );
        if !footer_area.is_empty() {
            let hint_area = Rect::new(
                footer_area.x.saturating_add(2),
                footer_area.y,
                footer_area.width.saturating_sub(2),
                footer_area.height,
            );
            frame.render_widget(Paragraph::new("Press esc to go back").dim(), hint_area);
        }
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
    let groups = [CatalogueRoute::Request, CatalogueRoute::InsideExec]
        .into_iter()
        .filter(|route| tools.iter().any(|tool| tool.route == *route))
        .count();
    let lines = tools
        .len()
        .saturating_add(groups)
        .saturating_add(groups.saturating_sub(1))
        .saturating_add(1 + usize::from(!tools.is_empty()));
    u16::try_from(lines).unwrap_or(u16::MAX)
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000 {
        let tenths = tokens.saturating_add(50) / 100;
        format!("{}.{:01}K", tenths / 10, tenths % 10)
    } else {
        tokens.to_string()
    }
}
