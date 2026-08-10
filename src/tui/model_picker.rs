//! Codex-compatible `/model` picker.
//!
//! The menu hierarchy, copy, row markers, keyboard behavior, and layout are
//! ported from OpenAI Codex's current `model_popups.rs` and generic selection
//! view. bettercodex keeps the focused state machine local because its TUI does
//! not carry Codex's general bottom-pane view stack.

use super::bottom_pane::scroll_state::ScrollState;
use super::bottom_pane::selection_popup_common::GenericDisplayRow;
use super::bottom_pane::selection_popup_common::MAX_POPUP_ROWS;
use super::bottom_pane::selection_popup_common::measure_rows_height;
use super::bottom_pane::selection_popup_common::measure_text_height;
use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use super::bottom_pane::selection_popup_common::render_rows;
use crate::model::ModelPreset;
use crate::model::ModelSelection;
use crate::model::ReasoningEffort;
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
enum ModelReturn {
    Quick(ScrollState),
    Models {
        state: ScrollState,
        quick_state: Option<ScrollState>,
    },
}

enum PickerStage {
    Quick {
        state: ScrollState,
    },
    Models {
        state: ScrollState,
        quick_state: Option<ScrollState>,
    },
    Reasoning {
        preset: ModelPreset,
        state: ScrollState,
        model_return: ModelReturn,
    },
    Advanced {
        preset: ModelPreset,
        state: ScrollState,
        reasoning_state: ScrollState,
        model_return: ModelReturn,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ModelPickerAction {
    None,
    Close,
    Select(ModelSelection),
}

pub(super) struct ModelPicker {
    models: Vec<ModelPreset>,
    current: ModelSelection,
    stage: PickerStage,
}

impl ModelPicker {
    pub(super) fn new(models: Vec<ModelPreset>, current: ModelSelection) -> Self {
        let auto_models = auto_models(&models);
        let stage = if auto_models.is_empty() {
            PickerStage::Models {
                state: initial_model_state(&non_auto_models(&models), &current),
                quick_state: None,
            }
        } else {
            let has_all_models = !non_auto_models(&models).is_empty();
            PickerStage::Quick {
                state: initial_quick_state(&auto_models, &current, has_all_models),
            }
        };
        Self {
            models,
            current,
            stage,
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
            PickerStage::Quick { state }
            | PickerStage::Models { state, .. }
            | PickerStage::Reasoning { state, .. }
            | PickerStage::Advanced { state, .. } => state,
        }
    }

    fn state_mut(&mut self) -> &mut ScrollState {
        match &mut self.stage {
            PickerStage::Quick { state }
            | PickerStage::Models { state, .. }
            | PickerStage::Reasoning { state, .. }
            | PickerStage::Advanced { state, .. } => state,
        }
    }

    fn row_count(&self) -> usize {
        match &self.stage {
            PickerStage::Quick { .. } => {
                let auto_count = auto_models(&self.models).len();
                auto_count + usize::from(!non_auto_models(&self.models).is_empty())
            }
            PickerStage::Models { .. } => non_auto_models(&self.models).len(),
            PickerStage::Reasoning { preset, .. } => {
                let normal = normal_efforts(preset).len();
                normal + usize::from(!advanced_efforts(preset).is_empty())
            }
            PickerStage::Advanced { preset, .. } => advanced_efforts(preset).len(),
        }
    }

    fn header_lines(&self) -> Vec<Line<'static>> {
        match &self.stage {
            PickerStage::Quick { .. } => vec![
                Line::from("Select Model").bold(),
                Line::from("Pick a quick auto mode or browse all models.").dim(),
            ],
            PickerStage::Models { .. } => vec![
                Line::from("Select Model and Effort").bold(),
                Line::from("Choose a model and reasoning level available to your account.").dim(),
            ],
            PickerStage::Reasoning { preset, .. } => {
                vec![Line::from(format!("Select Reasoning Level for {}", preset.model)).bold()]
            }
            PickerStage::Advanced { .. } => vec![
                Line::from("Advanced Reasoning").bold(),
                Line::from("Warning: Consumes usage limits faster").cyan(),
            ],
        }
    }

    fn rows(&self) -> Vec<GenericDisplayRow> {
        match &self.stage {
            PickerStage::Quick { state } => self.quick_rows(state),
            PickerStage::Models { state, .. } => {
                model_rows(&non_auto_models(&self.models), state, &self.current)
            }
            PickerStage::Reasoning { preset, state, .. } => {
                reasoning_rows(preset, state, &self.current)
            }
            PickerStage::Advanced { preset, state, .. } => {
                advanced_rows(preset, state, &self.current)
            }
        }
    }

    fn quick_rows(&self, state: &ScrollState) -> Vec<GenericDisplayRow> {
        let autos = auto_models(&self.models);
        let mut rows = model_rows(&autos, state, &self.current);
        let others = non_auto_models(&self.models);
        if !others.is_empty() {
            let index = rows.len();
            let selected = state.selected_idx == Some(index);
            let current_is_auto = autos
                .iter()
                .any(|preset| preset.model == self.current.model);
            rows.push(display_row(
                index,
                selected,
                format!(
                    "All models{}",
                    if current_is_auto { "" } else { " (current)" }
                ),
                Some(format!(
                    "Choose a specific model and reasoning level (current: {})",
                    self.current.model
                )),
            ));
        }
        rows
    }

    fn accept(&mut self) -> ModelPickerAction {
        let selected = self.state().selected_idx.unwrap_or_default();
        match &self.stage {
            PickerStage::Quick { state } => {
                let autos = auto_models(&self.models);
                if let Some(preset) = autos.get(selected).map(|preset| (*preset).clone()) {
                    if preset
                        .supported_reasoning_efforts
                        .iter()
                        .any(|option| option.effort.is_advanced())
                        || preset.default_reasoning_effort.is_advanced()
                    {
                        return self.open_reasoning(preset, ModelReturn::Quick(*state));
                    }
                    return ModelPickerAction::Select(
                        preset.selection(preset.default_reasoning_effort.clone()),
                    );
                }
                let models = non_auto_models(&self.models);
                if !models.is_empty() && selected == autos.len() {
                    self.stage = PickerStage::Models {
                        state: initial_model_state(&models, &self.current),
                        quick_state: Some(*state),
                    };
                }
                ModelPickerAction::None
            }
            PickerStage::Models { state, quick_state } => {
                let models = non_auto_models(&self.models);
                let Some(preset) = models.get(selected).map(|preset| (*preset).clone()) else {
                    return ModelPickerAction::None;
                };
                self.open_reasoning(
                    preset,
                    ModelReturn::Models {
                        state: *state,
                        quick_state: *quick_state,
                    },
                )
            }
            PickerStage::Reasoning {
                preset,
                state,
                model_return,
            } => {
                let normal = normal_efforts(preset);
                if let Some(effort) = normal.get(selected) {
                    return ModelPickerAction::Select(preset.selection((*effort).clone()));
                }
                if selected == normal.len() && !advanced_efforts(preset).is_empty() {
                    let mut advanced_state = ScrollState::new();
                    let advanced = advanced_efforts(preset);
                    let initial = if preset.model == self.current.model {
                        advanced
                            .iter()
                            .position(|effort| *effort == &self.current.reasoning_effort)
                    } else {
                        None
                    };
                    advanced_state.selected_idx = Some(initial.unwrap_or_default());
                    advanced_state.clamp_selection(advanced.len());
                    self.stage = PickerStage::Advanced {
                        preset: preset.clone(),
                        state: advanced_state,
                        reasoning_state: *state,
                        model_return: *model_return,
                    };
                }
                ModelPickerAction::None
            }
            PickerStage::Advanced { preset, .. } => advanced_efforts(preset)
                .get(selected)
                .map(|effort| ModelPickerAction::Select(preset.selection((*effort).clone())))
                .unwrap_or(ModelPickerAction::None),
        }
    }

    fn open_reasoning(
        &mut self,
        preset: ModelPreset,
        model_return: ModelReturn,
    ) -> ModelPickerAction {
        let mut efforts = preset
            .supported_reasoning_efforts
            .iter()
            .map(|option| option.effort.clone())
            .collect::<Vec<_>>();
        if efforts.is_empty() {
            efforts.push(preset.default_reasoning_effort.clone());
        }
        if efforts.len() == 1 && !efforts[0].is_advanced() {
            return ModelPickerAction::Select(preset.selection(efforts.remove(0)));
        }
        let mut state = ScrollState::new();
        state.selected_idx = Some(initial_reasoning_index(&preset, &self.current));
        state.clamp_selection(
            normal_efforts(&preset).len() + usize::from(!advanced_efforts(&preset).is_empty()),
        );
        self.stage = PickerStage::Reasoning {
            preset,
            state,
            model_return,
        };
        ModelPickerAction::None
    }

    fn go_back(&mut self) -> ModelPickerAction {
        match &self.stage {
            PickerStage::Quick { .. } => ModelPickerAction::Close,
            PickerStage::Models {
                quick_state: Some(quick_state),
                ..
            } => {
                self.stage = PickerStage::Quick {
                    state: *quick_state,
                };
                ModelPickerAction::None
            }
            PickerStage::Models {
                quick_state: None, ..
            } => ModelPickerAction::Close,
            PickerStage::Reasoning { model_return, .. } => {
                self.restore_model_stage(*model_return);
                ModelPickerAction::None
            }
            PickerStage::Advanced {
                preset,
                reasoning_state,
                model_return,
                ..
            } => {
                self.stage = PickerStage::Reasoning {
                    preset: preset.clone(),
                    state: *reasoning_state,
                    model_return: *model_return,
                };
                ModelPickerAction::None
            }
        }
    }

    fn restore_model_stage(&mut self, model_return: ModelReturn) {
        self.stage = match model_return {
            ModelReturn::Quick(state) => PickerStage::Quick { state },
            ModelReturn::Models { state, quick_state } => {
                PickerStage::Models { state, quick_state }
            }
        };
    }
}

fn auto_models(models: &[ModelPreset]) -> Vec<&ModelPreset> {
    let mut models = models
        .iter()
        .filter(|preset| preset.model.starts_with("codex-auto-"))
        .collect::<Vec<_>>();
    models.sort_by_key(|preset| match preset.model.as_str() {
        "codex-auto-fast" => 0,
        "codex-auto-balanced" => 1,
        "codex-auto-thorough" => 2,
        _ => 3,
    });
    models
}

fn non_auto_models(models: &[ModelPreset]) -> Vec<&ModelPreset> {
    models
        .iter()
        .filter(|preset| !preset.model.starts_with("codex-auto-"))
        .collect()
}

fn initial_quick_state(
    models: &[&ModelPreset],
    current: &ModelSelection,
    has_all_models: bool,
) -> ScrollState {
    let mut state = ScrollState::new();
    state.selected_idx = Some(
        models
            .iter()
            .position(|preset| preset.model == current.model)
            .unwrap_or_else(|| {
                if has_all_models {
                    models.len()
                } else {
                    models
                        .iter()
                        .position(|preset| preset.is_default)
                        .unwrap_or_default()
                }
            }),
    );
    state.clamp_selection(models.len() + usize::from(has_all_models));
    state
}

fn initial_model_state(models: &[&ModelPreset], current: &ModelSelection) -> ScrollState {
    let mut state = ScrollState::new();
    state.selected_idx = Some(
        models
            .iter()
            .position(|preset| preset.model == current.model)
            .or_else(|| models.iter().position(|preset| preset.is_default))
            .unwrap_or_default(),
    );
    state.clamp_selection(models.len());
    state
}

fn initial_reasoning_index(preset: &ModelPreset, current: &ModelSelection) -> usize {
    let normal = normal_efforts(preset);
    let selected = if preset.model == current.model {
        &current.reasoning_effort
    } else {
        &preset.default_reasoning_effort
    };
    normal
        .iter()
        .position(|effort| *effort == selected)
        .unwrap_or_else(|| {
            if selected.is_advanced() && !advanced_efforts(preset).is_empty() {
                normal.len()
            } else {
                normal
                    .iter()
                    .position(|effort| *effort == &preset.default_reasoning_effort)
                    .unwrap_or_default()
            }
        })
}

fn model_rows(
    models: &[&ModelPreset],
    state: &ScrollState,
    current: &ModelSelection,
) -> Vec<GenericDisplayRow> {
    models
        .iter()
        .enumerate()
        .map(|(index, preset)| {
            let marker = if preset.model == current.model {
                " (current)"
            } else if preset.is_default {
                " (default)"
            } else {
                ""
            };
            display_row(
                index,
                state.selected_idx == Some(index),
                format!("{}{marker}", preset.model),
                (!preset.description.is_empty()).then(|| preset.description.clone()),
            )
        })
        .collect()
}

fn reasoning_rows(
    preset: &ModelPreset,
    state: &ScrollState,
    current: &ModelSelection,
) -> Vec<GenericDisplayRow> {
    let normal = normal_efforts(preset);
    let mut rows = normal
        .iter()
        .enumerate()
        .map(|(index, effort)| {
            let default_marker = if effort == &&preset.default_reasoning_effort {
                " (default)"
            } else {
                ""
            };
            let current_marker =
                if preset.model == current.model && effort == &&current.reasoning_effort {
                    " (current)"
                } else {
                    ""
                };
            let mut description = effort_description(preset, effort);
            if state.selected_idx == Some(index) && reasoning_warning_effort(preset) == Some(effort)
            {
                let warning = format!(
                    "Warning: {} reasoning effort can quickly consume Plus plan rate limits.",
                    effort.label()
                );
                description = Some(description.map_or(warning.clone(), |description| {
                    format!("{description}\n{warning}")
                }));
            }
            display_row(
                index,
                state.selected_idx == Some(index),
                format!("{}{default_marker}{current_marker}", effort.label()),
                description,
            )
        })
        .collect::<Vec<_>>();
    let advanced = advanced_efforts(preset);
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
            if preset.model == current.model && current.reasoning_effort.is_advanced() {
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
    preset: &ModelPreset,
    state: &ScrollState,
    current: &ModelSelection,
) -> Vec<GenericDisplayRow> {
    advanced_efforts(preset)
        .into_iter()
        .enumerate()
        .map(|(index, effort)| {
            let current_marker =
                if preset.model == current.model && effort == &current.reasoning_effort {
                    " (current)"
                } else {
                    ""
                };
            let description =
                "For difficult problems when quality matters more than speed · higher usage";
            display_row(
                index,
                state.selected_idx == Some(index),
                format!("{}{current_marker}", effort.label()),
                Some(description.to_string()),
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

fn normal_efforts(preset: &ModelPreset) -> Vec<&ReasoningEffort> {
    preset
        .supported_reasoning_efforts
        .iter()
        .map(|option| &option.effort)
        .filter(|effort| !effort.is_advanced())
        .collect()
}

fn advanced_efforts(preset: &ModelPreset) -> Vec<&ReasoningEffort> {
    let mut efforts = preset
        .supported_reasoning_efforts
        .iter()
        .map(|option| &option.effort)
        .filter(|effort| effort.is_advanced())
        .collect::<Vec<_>>();
    if efforts.is_empty() && preset.default_reasoning_effort.is_advanced() {
        efforts.push(&preset.default_reasoning_effort);
    }
    efforts
}

fn effort_description(preset: &ModelPreset, effort: &ReasoningEffort) -> Option<String> {
    preset
        .supported_reasoning_efforts
        .iter()
        .find(|option| &option.effort == effort)
        .map(|option| option.description.clone())
        .filter(|description| !description.is_empty())
}

fn reasoning_warning_effort(preset: &ModelPreset) -> Option<&ReasoningEffort> {
    let warn_for_model = preset.model.starts_with("gpt-5.1-codex")
        || preset.model.starts_with("gpt-5.1-codex-max")
        || preset.model.starts_with("gpt-5.2");
    if !warn_for_model {
        return None;
    }
    preset
        .supported_reasoning_efforts
        .iter()
        .map(|option| &option.effort)
        .find(|effort| matches!(effort, ReasoningEffort::XHigh))
        .or_else(|| {
            preset
                .supported_reasoning_efforts
                .iter()
                .map(|option| &option.effort)
                .find(|effort| matches!(effort, ReasoningEffort::High))
        })
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
    use crate::model::bundled_models;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn model_picker_describes_the_supported_selection_surface() {
        let current = bundled_models()[4].selection(ReasoningEffort::XHigh);
        let picker = ModelPicker::new(bundled_models().to_vec(), current);
        assert_eq!(
            render_trimmed(&picker, 80),
            concat!(
                "  Select Model and Effort\n",
                "  Choose a model and reasoning level available to your account.\n",
                "\n",
                "  1. gpt-5.6-sol (default)  Latest frontier agentic coding model.\n",
                "  2. gpt-5.6-terra          Balanced agentic coding model for everyday work.\n",
                "  3. gpt-5.6-luna           Fast and affordable agentic coding model.\n",
                "  4. gpt-5.5                Frontier model for complex coding, research, and\n",
                "                            real-world work.\n",
                "› 5. gpt-5.2 (current)      Optimized for professional work and long-running\n",
                "                            agents.\n",
                "\n",
                "  Press enter to confirm or esc to go back",
            )
        );
    }

    #[test]
    fn reasoning_surface_shows_max_as_the_only_advanced_effort() {
        let mut preset = bundled_models()[0].clone();
        preset.model = "gpt-5.4".to_string();
        preset.default_reasoning_effort = ReasoningEffort::Medium;
        let current = preset.selection(ReasoningEffort::High);
        let mut picker = ModelPicker::new(vec![preset], current);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelPickerAction::None
        );
        assert_eq!(
            render_trimmed(&picker, 80),
            concat!(
                "  Select Reasoning Level for gpt-5.4\n",
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
    }

    #[test]
    fn advanced_reasoning_surface_shows_max() {
        let mut preset = bundled_models()[0].clone();
        preset.model = "gpt-5.4".to_string();
        preset.default_reasoning_effort = ReasoningEffort::Medium;
        let current = preset.selection(ReasoningEffort::Max);
        let mut picker = ModelPicker::new(vec![preset], current);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelPickerAction::None
        );

        let row_count = picker.row_count();
        picker.state_mut().jump_bottom(row_count, MAX_POPUP_ROWS);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelPickerAction::None
        );
        assert_eq!(
            render_trimmed(&picker, 80),
            concat!(
                "  Advanced Reasoning\n",
                "  Warning: Consumes usage limits faster\n",
                "\n",
                "› 1. Max (current)  For difficult problems when quality matters more than\n",
                "                    speed · higher usage\n",
                "\n",
                "  Press enter to confirm or esc to go back",
            )
        );

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ModelPickerAction::None
        );
        assert!(render_trimmed(&picker, 80).contains("Select Reasoning Level"));
    }

    #[test]
    fn extra_high_warning_surface_matches_codex() {
        let preset = bundled_models()[4].clone();
        let current = preset.selection(ReasoningEffort::XHigh);
        let mut picker = ModelPicker::new(vec![preset], current);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelPickerAction::None
        );
        assert_eq!(
            render_trimmed(&picker, 80),
            concat!(
                "  Select Reasoning Level for gpt-5.2\n",
                "\n",
                "  1. Low                   Balances speed with some reasoning; useful for\n",
                "                           straightforward queries and short explanations\n",
                "  2. Medium (default)      Provides a solid balance of reasoning depth and\n",
                "                           latency for general-purpose tasks\n",
                "  3. High                  Maximizes reasoning depth for complex or ambiguous\n",
                "                           problems\n",
                "› 4. Extra high (current)  Extra high reasoning for complex problems\n",
                "                           Warning: Extra high reasoning effort can quickly\n",
                "                           consume Plus plan rate limits.\n",
                "\n",
                "  Press enter to confirm or esc to go back",
            )
        );
    }

    #[test]
    fn quick_picker_with_only_auto_models_keeps_a_valid_initial_selection() {
        let mut auto = bundled_models()[0].clone();
        auto.model = "codex-auto-fast".to_string();
        auto.supported_reasoning_efforts.truncate(1);
        let expected = auto.selection(auto.default_reasoning_effort.clone());
        let current = ModelSelection::from_identity("missing", ReasoningEffort::Medium);
        let mut picker = ModelPicker::new(vec![auto], current);

        let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(action, ModelPickerAction::Select(expected));
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
