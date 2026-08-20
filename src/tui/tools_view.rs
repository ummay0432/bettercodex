use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use super::context_window::format_tokens;
use super::palette;
use super::render::line_utils::line_to_static;
use super::width::display_width;
use super::wrapping::RtOptions;
use super::wrapping::word_wrap_line;
use crate::context::estimated_tokens;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

const MUTED: Color = Color::Indexed(245);
const FOOTER_HEIGHT: u16 = 1;
const PANEL_CHROME_HEIGHT: u16 = menu_surface_padding_height() + FOOTER_HEIGHT;

pub(super) struct ToolsView {
    tools: Vec<ToolSummary>,
    placement: ToolsPlacement,
}

#[derive(Clone, Copy)]
enum ToolsPlacement {
    Standalone,
    UnderContext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolsAction {
    StayOpen,
    Close,
}

struct ToolSummary {
    name: &'static str,
    description: &'static str,
    tokens: u64,
}

impl ToolsView {
    pub(super) fn standalone(
        ask_user_question_enabled: bool,
        specialist_coordination_enabled: bool,
    ) -> Self {
        Self::new(
            ToolsPlacement::Standalone,
            ask_user_question_enabled,
            specialist_coordination_enabled,
        )
    }

    pub(super) fn under_context(
        ask_user_question_enabled: bool,
        specialist_coordination_enabled: bool,
    ) -> Self {
        Self::new(
            ToolsPlacement::UnderContext,
            ask_user_question_enabled,
            specialist_coordination_enabled,
        )
    }

    fn new(
        placement: ToolsPlacement,
        ask_user_question_enabled: bool,
        specialist_coordination_enabled: bool,
    ) -> Self {
        Self {
            tools: tool_summaries(ask_user_question_enabled, specialist_coordination_enabled),
            placement,
        }
    }

    pub(super) fn preferred_height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(4).max(1);
        let content_height =
            u16::try_from(self.content_lines(content_width).len()).unwrap_or(u16::MAX);
        PANEL_CHROME_HEIGHT.saturating_add(content_height)
    }

    pub(super) fn handle_key(&self, code: KeyCode) -> ToolsAction {
        match code {
            KeyCode::Esc => ToolsAction::Close,
            _ => ToolsAction::StayOpen,
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
        if !inner.is_empty() {
            frame.render_widget(Paragraph::new(self.content_lines(inner.width)), inner);
        }
        if !footer_area.is_empty() {
            let hint_area = Rect::new(
                footer_area.x.saturating_add(2),
                footer_area.y,
                footer_area.width.saturating_sub(2),
                footer_area.height,
            );
            let hint = match self.placement {
                ToolsPlacement::Standalone => "Press esc to close",
                ToolsPlacement::UnderContext => "Press esc to go back to context",
            };
            frame.render_widget(
                Paragraph::new(hint).style(Style::default().fg(MUTED)),
                hint_area,
            );
        }
    }

    fn content_lines(&self, width: u16) -> Vec<Line<'static>> {
        let width = usize::from(width.max(1));
        let total_tokens = self
            .tools
            .iter()
            .map(|tool| tool.tokens)
            .fold(0_u64, u64::saturating_add);
        let direct_functions = self
            .tools
            .iter()
            .filter(|tool| tool.name != "web_search")
            .count();
        let mut lines = vec![Line::from("Tools").bold()];
        lines.extend(wrap_owned(
            Line::from(vec![
                Span::styled(
                    format!("{direct_functions} direct functions + hosted web search"),
                    palette::accent_style(),
                ),
                Span::from(format!(
                    "  ·  ~{} estimated context tokens per request",
                    format_tokens(total_tokens)
                )),
            ]),
            RtOptions::new(width),
        ));
        lines.extend(wrap_owned(
            Line::from(
                "Fixed function schemas and hosted search consume context on every request.",
            )
            .dim(),
            RtOptions::new(width),
        ));
        lines.extend(wrap_owned(
            Line::from("bettercodex runs functions locally; the Responses API runs web search.")
                .dim(),
            RtOptions::new(width),
        ));
        lines.push(Line::default());

        for tool in &self.tools {
            lines.extend(tool_header_lines(tool, width));
            lines.extend(wrap_owned(
                Line::from(vec![Span::from("  "), Span::from(tool.description).dim()]),
                RtOptions::new(width).subsequent_indent(Line::from("  ")),
            ));
        }
        lines
    }
}

fn tool_summaries(
    ask_user_question_enabled: bool,
    specialist_coordination_enabled: bool,
) -> Vec<ToolSummary> {
    crate::tools::responses_api_specifications_for(
        ask_user_question_enabled,
        specialist_coordination_enabled,
    )
    .iter()
    .map(|specification| {
        let hosted_web_search = specification
            .get("type")
            .and_then(serde_json::Value::as_str)
            == Some("web_search");
        let name = if hosted_web_search {
            "web_search"
        } else {
            specification
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
        };
        let description = if hosted_web_search {
            "Search and browse the live web using text and image results."
        } else {
            specification
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(brief_description)
                .unwrap_or("No description available.")
        };
        ToolSummary {
            name,
            description,
            tokens: estimated_tokens(std::slice::from_ref(specification)),
        }
    })
    .collect()
}

fn brief_description(description: &'static str) -> &'static str {
    let first_line = description
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(description)
        .trim();
    first_line
        .find(". ")
        .map_or(first_line, |end| &first_line[..=end])
}

fn tool_header_lines(tool: &ToolSummary, width: usize) -> Vec<Line<'static>> {
    let token_label = format!("~{} tokens", format_tokens(tool.tokens));
    let left_width = 2_usize.saturating_add(display_width(tool.name));
    let token_width = display_width(&token_label);
    let gap = width
        .saturating_sub(left_width.saturating_add(token_width))
        .max(2);
    wrap_owned(
        Line::from(vec![
            Span::styled("• ", palette::accent_text_style()),
            Span::styled(tool.name, palette::accent_style()),
            Span::from(" ".repeat(gap)),
            Span::from(token_label).style(Style::default().fg(MUTED)),
        ]),
        RtOptions::new(width).subsequent_indent(Line::from("  ")),
    )
}

fn wrap_owned(line: Line<'static>, options: RtOptions<'static>) -> Vec<Line<'static>> {
    word_wrap_line(&line, options)
        .iter()
        .map(line_to_static)
        .collect()
}
