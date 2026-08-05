use crate::tools::CatalogueRoute;
use crate::tools::CatalogueTool;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

const RULE: Color = Color::Indexed(8);
const PREFERRED_WIDTH: u16 = 32;
const PREFERRED_HEIGHT: u16 = 14;

pub(super) const VIEWPORT_HEIGHT: u16 = PREFERRED_HEIGHT;

pub(super) struct ToolCatalogueView;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CatalogueAction {
    StayOpen,
    Close,
}

impl ToolCatalogueView {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) fn handle_key(&self, code: KeyCode) -> CatalogueAction {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => CatalogueAction::Close,
            _ => CatalogueAction::StayOpen,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            return;
        }
        let panel = Rect::new(
            area.x,
            area.y,
            PREFERRED_WIDTH.min(area.width),
            PREFERRED_HEIGHT.min(area.height),
        );
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(" Tools ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(RULE));
        let inner = block.inner(panel);
        frame.render_widget(block, panel);
        if inner.is_empty() {
            return;
        }

        frame.render_widget(
            Paragraph::new(catalogue_lines(crate::tools::display_tools())),
            inner,
        );
    }
}

fn catalogue_lines(tools: &[CatalogueTool]) -> Vec<Line<'static>> {
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
                Span::from(tool.name.clone()),
            ]));
        }
    }
    lines
}

#[cfg(test)]
#[path = "tool_catalogue_tests.rs"]
mod tests;
