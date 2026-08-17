//! Fixed GPT-5.6 `/model` picker.

use super::bottom_pane::scroll_state::ScrollState;
use super::bottom_pane::selection_popup_common::GenericDisplayRow;
use super::bottom_pane::selection_popup_common::MAX_POPUP_ROWS;
use super::bottom_pane::selection_popup_common::measure_rows_height;
use super::bottom_pane::selection_popup_common::measure_text_height;
use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use super::bottom_pane::selection_popup_common::render_rows;
use super::palette;
use crate::model::DEFAULT_MODEL;
use crate::model::ModelPreset;
use crate::model::ModelSelection;
use crate::model::advanced_reasoning_efforts;
use crate::model::available_models;
use crate::model::standard_reasoning_efforts;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

const FOOTER_HEIGHT: u16 = 1;

#[derive(Clone, Copy)]
enum PickerStage {
    Models {
        state: ScrollState,
    },
    Reasoning {
        preset: ModelPreset,
        state: ScrollState,
        model_state: ScrollState,
    },
    Advanced {
        preset: ModelPreset,
        state: ScrollState,
        reasoning_state: ScrollState,
        model_state: ScrollState,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ModelPickerAction {
    None,
    Close,
    Select(ModelSelection),
}

pub(super) struct ModelPicker {
    current: ModelSelection,
    stage: PickerStage,
}

impl ModelPicker {
    pub(super) fn new(current: ModelSelection) -> Self {
        Self {
            stage: PickerStage::Models {
                state: initial_model_state(&current),
            },
            current,
        }
    }

    pub(super) fn preferred_height(&self, width: u16) -> u16 {
        let header = self.header_lines();
        let rows = self.rows();
        let row_width = width.saturating_sub(2).max(1);
        measure_text_height(&header, width.saturating_sub(4))
            .saturating_add(measure_rows_height(
                &rows,
                self.state(),
                row_width.saturating_add(1),
            ))
            .saturating_add(menu_surface_padding_height())
            .saturating_add(1)
            .saturating_add(FOOTER_HEIGHT)
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ModelPickerAction {
        let len = self.row_count();
        self.state_mut().clamp_selection(len);
        let visible = MAX_POPUP_ROWS.min(len);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || control => {
                return self.go_back();
            }
            KeyCode::Up | KeyCode::Char('p') if key.code == KeyCode::Up || control => {
                self.state_mut().move_up_wrap(len);
            }
            KeyCode::Down | KeyCode::Char('n') if key.code == KeyCode::Down || control => {
                self.state_mut().move_down_wrap(len);
            }
            KeyCode::PageUp => self.state_mut().page_up_clamped(len, visible),
            KeyCode::PageDown => self.state_mut().page_down_clamped(len, visible),
            KeyCode::Home => self.state_mut().jump_top(len, visible),
            KeyCode::End => self.state_mut().jump_bottom(len, visible),
            KeyCode::Enter => return self.accept(),
            KeyCode::Char(number)
                if key.modifiers.is_empty()
                    && number
                        .to_digit(10)
                        .is_some_and(|number| number > 0 && number as usize <= len) =>
            {
                self.state_mut().selected_idx = number
                    .to_digit(10)
                    .and_then(|number| usize::try_from(number).ok())
                    .map(|number| number - 1);
                return self.accept();
            }
            _ => return ModelPickerAction::None,
        }
        self.state_mut().ensure_visible(len, visible);
        ModelPickerAction::None
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
            let header = self.header_lines();
            let header_height = measure_text_height(&header, inner.width).min(inner.height);
            let header_area = Rect::new(inner.x, inner.y, inner.width, header_height);
            frame.render_widget(
                Paragraph::new(header).wrap(Wrap { trim: false }),
                header_area,
            );

            let rows = self.rows();
            let row_width = content_area.width.saturating_sub(2).max(1);
            let requested_rows =
                measure_rows_height(&rows, self.state(), row_width.saturating_add(1));
            let list_y = header_area.bottom().saturating_add(1).min(inner.bottom());
            let list_area = Rect::new(
                inner.x.saturating_sub(2),
                list_y,
                row_width,
                requested_rows.min(inner.bottom().saturating_sub(list_y)),
            );
            render_rows(
                list_area,
                frame.buffer_mut(),
                &rows,
                self.state(),
                "no matches",
            );
        }

        if !footer_area.is_empty() {
            let hint_area = Rect::new(
                footer_area.x.saturating_add(2),
                footer_area.y,
                footer_area.width.saturating_sub(2),
                footer_area.height,
            );
            frame.render_widget(Paragraph::new(standard_popup_hint()).dim(), hint_area);
        }
    }

    fn state(&self) -> &ScrollState {
        match &self.stage {
            PickerStage::Models { state }
            | PickerStage::Reasoning { state, .. }
            | PickerStage::Advanced { state, .. } => state,
        }
    }

    fn state_mut(&mut self) -> &mut ScrollState {
        match &mut self.stage {
            PickerStage::Models { state }
            | PickerStage::Reasoning { state, .. }
            | PickerStage::Advanced { state, .. } => state,
        }
    }

    fn row_count(&self) -> usize {
        match &self.stage {
            PickerStage::Models { .. } => available_models().len(),
            PickerStage::Reasoning { .. } => {
                standard_reasoning_efforts().len()
                    + usize::from(!advanced_reasoning_efforts().is_empty())
            }
            PickerStage::Advanced { .. } => advanced_reasoning_efforts().len(),
        }
    }

    fn header_lines(&self) -> Vec<Line<'static>> {
        match &self.stage {
            PickerStage::Models { .. } => vec![
                Line::from("Select Model and Effort").bold(),
                Line::from("Choose a GPT-5.6 model and reasoning level.").dim(),
            ],
            PickerStage::Reasoning { preset, .. } => {
                vec![Line::from(format!("Select Reasoning Level for {}", preset.model)).bold()]
            }
            PickerStage::Advanced { .. } => vec![
                Line::from("Advanced Reasoning").bold(),
                Line::from("Warning: Consumes usage limits faster").fg(palette::warning_color()),
            ],
        }
    }

    fn rows(&self) -> Vec<GenericDisplayRow> {
        match &self.stage {
            PickerStage::Models { state } => model_rows(state, &self.current),
            PickerStage::Reasoning { preset, state, .. } => {
                reasoning_rows(*preset, state, &self.current)
            }
            PickerStage::Advanced { preset, state, .. } => {
                advanced_rows(*preset, state, &self.current)
            }
        }
    }

    fn accept(&mut self) -> ModelPickerAction {
        let selected = self.state().selected_idx.unwrap_or_default();
        match self.stage {
            PickerStage::Models { state } => {
                let Some(preset) = available_models().get(selected).copied() else {
                    return ModelPickerAction::None;
                };
                let mut reasoning_state = ScrollState::new();
                reasoning_state.selected_idx = Some(initial_reasoning_index(preset, &self.current));
                reasoning_state.clamp_selection(
                    standard_reasoning_efforts().len()
                        + usize::from(!advanced_reasoning_efforts().is_empty()),
                );
                self.stage = PickerStage::Reasoning {
                    preset,
                    state: reasoning_state,
                    model_state: state,
                };
                ModelPickerAction::None
            }
            PickerStage::Reasoning {
                preset,
                state,
                model_state,
            } => {
                let normal = standard_reasoning_efforts();
                if let Some(effort) = normal.get(selected) {
                    return ModelPickerAction::Select(preset.selection(*effort));
                }
                let advanced = advanced_reasoning_efforts();
                if selected == normal.len() && !advanced.is_empty() {
                    let mut advanced_state = ScrollState::new();
                    let initial = if preset.model == self.current.model {
                        advanced
                            .iter()
                            .position(|effort| *effort == self.current.reasoning_effort)
                    } else {
                        None
                    };
                    advanced_state.selected_idx = Some(initial.unwrap_or_default());
                    advanced_state.clamp_selection(advanced.len());
                    self.stage = PickerStage::Advanced {
                        preset,
                        state: advanced_state,
                        reasoning_state: state,
                        model_state,
                    };
                }
                ModelPickerAction::None
            }
            PickerStage::Advanced { preset, .. } => advanced_reasoning_efforts()
                .get(selected)
                .map(|effort| ModelPickerAction::Select(preset.selection(*effort)))
                .unwrap_or(ModelPickerAction::None),
        }
    }

    fn go_back(&mut self) -> ModelPickerAction {
        match self.stage {
            PickerStage::Models { .. } => ModelPickerAction::Close,
            PickerStage::Reasoning { model_state, .. } => {
                self.stage = PickerStage::Models { state: model_state };
                ModelPickerAction::None
            }
            PickerStage::Advanced {
                preset,
                reasoning_state,
                model_state,
                ..
            } => {
                self.stage = PickerStage::Reasoning {
                    preset,
                    state: reasoning_state,
                    model_state,
                };
                ModelPickerAction::None
            }
        }
    }
}

fn initial_model_state(current: &ModelSelection) -> ScrollState {
    let models = available_models();
    let mut state = ScrollState::new();
    state.selected_idx = Some(
        models
            .iter()
            .position(|preset| preset.model == current.model)
            .or_else(|| {
                models
                    .iter()
                    .position(|preset| preset.model == DEFAULT_MODEL)
            })
            .unwrap_or_default(),
    );
    state.clamp_selection(models.len());
    state
}

fn initial_reasoning_index(preset: ModelPreset, current: &ModelSelection) -> usize {
    let normal = standard_reasoning_efforts();
    let selected = if preset.model == current.model {
        current.reasoning_effort
    } else {
        preset.default_reasoning_effort
    };
    normal
        .iter()
        .position(|effort| *effort == selected)
        .unwrap_or_else(|| {
            if advanced_reasoning_efforts().contains(&selected) {
                normal.len()
            } else {
                normal
                    .iter()
                    .position(|effort| *effort == preset.default_reasoning_effort)
                    .unwrap_or_default()
            }
        })
}

fn model_rows(state: &ScrollState, current: &ModelSelection) -> Vec<GenericDisplayRow> {
    available_models()
        .iter()
        .enumerate()
        .map(|(index, preset)| {
            let marker = if preset.model == current.model {
                " (current)"
            } else if preset.model == DEFAULT_MODEL {
                " (default)"
            } else {
                ""
            };
            display_row(
                index,
                state.selected_idx == Some(index),
                format!("{}{marker}", preset.model),
                Some(preset.description.to_string()),
            )
        })
        .collect()
}

fn reasoning_rows(
    preset: ModelPreset,
    state: &ScrollState,
    current: &ModelSelection,
) -> Vec<GenericDisplayRow> {
    let normal = standard_reasoning_efforts();
    let mut rows = normal
        .iter()
        .enumerate()
        .map(|(index, effort)| {
            let default_marker = if *effort == preset.default_reasoning_effort {
                " (default)"
            } else {
                ""
            };
            let current_marker =
                if preset.model == current.model && *effort == current.reasoning_effort {
                    " (current)"
                } else {
                    ""
                };
            display_row(
                index,
                state.selected_idx == Some(index),
                format!("{}{default_marker}{current_marker}", effort.label()),
                Some(effort.description().to_string()),
            )
        })
        .collect::<Vec<_>>();
    let advanced = advanced_reasoning_efforts();
    if !advanced.is_empty() {
        let index = rows.len();
        let names = advanced
            .iter()
            .map(|effort| effort.label())
            .collect::<Vec<_>>()
            .join(" and ");
        let verb = if advanced.len() == 1 {
            "consumes"
        } else {
            "consume"
        };
        let current_marker =
            if preset.model == current.model && advanced.contains(&current.reasoning_effort) {
                " (current)"
            } else {
                ""
            };
        rows.push(display_row(
            index,
            state.selected_idx == Some(index),
            format!("More reasoning…{current_marker}"),
            Some(format!("{names} {verb} usage limits faster")),
        ));
    }
    rows
}

fn advanced_rows(
    preset: ModelPreset,
    state: &ScrollState,
    current: &ModelSelection,
) -> Vec<GenericDisplayRow> {
    advanced_reasoning_efforts()
        .iter()
        .enumerate()
        .map(|(index, effort)| {
            let current_marker =
                if preset.model == current.model && *effort == current.reasoning_effort {
                    " (current)"
                } else {
                    ""
                };
            display_row(
                index,
                state.selected_idx == Some(index),
                format!("{}{current_marker}", effort.label()),
                Some(
                    "For difficult problems when quality matters more than speed · higher usage"
                        .to_string(),
                ),
            )
        })
        .collect()
}

fn display_row(
    index: usize,
    selected: bool,
    name: String,
    description: Option<String>,
) -> GenericDisplayRow {
    GenericDisplayRow {
        name: format!("{} {}. {name}", if selected { '›' } else { ' ' }, index + 1),
        description,
        ..Default::default()
    }
}

fn standard_popup_hint() -> Line<'static> {
    Line::from(vec![
        "Press ".into(),
        "enter".bold(),
        " to confirm or ".into(),
        "esc".bold(),
        " to go back".into(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReasoningEffort;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn model_picker_exposes_only_the_gpt_5_6_family() {
        let current = available_models()[2].selection(ReasoningEffort::XHigh);
        let picker = ModelPicker::new(current);
        assert_eq!(
            render_trimmed(&picker, 80),
            concat!(
                "  Select Model and Effort\n",
                "  Choose a GPT-5.6 model and reasoning level.\n",
                "\n",
                "  1. gpt-5.6-sol (default)   Latest frontier agentic coding model.\n",
                "  2. gpt-5.6-terra           Balanced agentic coding model for everyday work.\n",
                "› 3. gpt-5.6-luna (current)  Fast and affordable agentic coding model.\n",
                "\n",
                "  Press enter to confirm or esc to go back",
            )
        );
    }

    #[test]
    fn reasoning_surface_keeps_max_behind_advanced_reasoning() {
        let preset = available_models()[1];
        let current = preset.selection(ReasoningEffort::High);
        let mut picker = ModelPicker::new(current);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelPickerAction::None
        );
        assert_eq!(
            render_trimmed(&picker, 80),
            concat!(
                "  Select Reasoning Level for gpt-5.6-terra\n",
                "\n",
                "  1. Low               Fast responses with lighter reasoning\n",
                "  2. Medium (default)  Balances speed and reasoning depth for everyday tasks\n",
                "› 3. High (current)    Greater reasoning depth for complex problems\n",
                "  4. Extra high        Extra high reasoning depth for complex problems\n",
                "  5. More reasoning…   Max consumes usage limits faster\n",
                "\n",
                "  Press enter to confirm or esc to go back",
            )
        );

        let row_count = picker.row_count();
        picker.state_mut().jump_bottom(row_count, MAX_POPUP_ROWS);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelPickerAction::None
        );
        assert!(render_trimmed(&picker, 80).contains("Max"));
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ModelPickerAction::None
        );
        assert!(render_trimmed(&picker, 80).contains("Select Reasoning Level"));
    }

    fn render_trimmed(picker: &ModelPicker, width: u16) -> String {
        let height = picker.preferred_height(width);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| picker.render(frame, frame.area(), Style::default()))
            .expect("render picker");
        let buffer = terminal.backend().buffer();
        let lines = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>();
        let first = lines.iter().position(|line| !line.is_empty()).unwrap_or(0);
        let last = lines
            .iter()
            .rposition(|line| !line.is_empty())
            .map_or(first, |last| last + 1);
        lines[first..last].join("\n")
    }
}
