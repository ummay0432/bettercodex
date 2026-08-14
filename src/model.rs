//! Fixed GPT-5.6 model selection and persistence.
//!
//! bettercodex supports the three GPT-5.6 Codex variants. Their shared runtime
//! limits are constants; persisted selections contain only model identity and
//! reasoning effort.

use crate::state_file;
use crate::truncation::TruncationPolicy;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de::Error as _;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::RwLock;

pub(crate) const DEFAULT_MODEL: &str = "gpt-5.6-sol";
pub(crate) const RAW_CONTEXT_WINDOW: u64 = 272_000;
pub(crate) const EFFECTIVE_CONTEXT_WINDOW: u64 = RAW_CONTEXT_WINDOW * 95 / 100;
pub(crate) const AUTO_COMPACT_TOKEN_LIMIT: u64 = RAW_CONTEXT_WINDOW * 90 / 100;
pub(crate) const TOOL_TRUNCATION_POLICY: TruncationPolicy = TruncationPolicy::Tokens(10_000);

const MODEL_SETTINGS_FILE: &str = "model.json";
const MODEL_SETTINGS_VERSION: u32 = 1;
const MAX_MODEL_SETTINGS_BYTES: usize = 16 * 1024;

/// Reasoning efforts exposed by the GPT-5.6 Codex catalogue.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub(crate) enum ReasoningEffort {
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::XHigh => "Extra high",
            Self::Max => "Max",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Low => "Fast responses with lighter reasoning",
            Self::Medium => "Balances speed and reasoning depth for everyday tasks",
            Self::High => "Greater reasoning depth for complex problems",
            Self::XHigh => "Extra high reasoning depth for complex problems",
            Self::Max => "Maximum reasoning depth for the hardest problems",
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            _ => Err(format!("unsupported GPT-5.6 reasoning effort `{value}`")),
        }
    }
}

impl Serialize for ReasoningEffort {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ReasoningEffort {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ModelSelection {
    pub(crate) model: String,
    pub(crate) reasoning_effort: ReasoningEffort,
}

impl Default for ModelSelection {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            reasoning_effort: ReasoningEffort::XHigh,
        }
    }
}

impl ModelSelection {
    pub(crate) fn from_identity(
        model: impl Into<String>,
        reasoning_effort: ReasoningEffort,
    ) -> Self {
        let mut selection = Self {
            model: model.into(),
            reasoning_effort,
        };
        selection.normalize();
        selection
    }

    pub(crate) const fn effective_context_window(&self) -> u64 {
        EFFECTIVE_CONTEXT_WINDOW
    }

    pub(crate) const fn auto_compact_token_limit(&self) -> u64 {
        AUTO_COMPACT_TOKEN_LIMIT
    }

    pub(crate) const fn truncation_policy(&self) -> TruncationPolicy {
        TOOL_TRUNCATION_POLICY
    }

    pub(crate) fn normalize(&mut self) {
        if !available_models()
            .iter()
            .any(|preset| preset.model == self.model)
        {
            self.model = DEFAULT_MODEL.to_string();
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if available_models()
            .iter()
            .any(|preset| preset.model == self.model)
        {
            Ok(())
        } else {
            Err(anyhow!(
                "unsupported model `{}`; bettercodex supports only GPT-5.6 Sol, Terra, and Luna",
                self.model
            ))
        }
    }
}

/// Shared model identity retained by the Responses client across turns.
#[derive(Clone)]
pub(crate) struct SharedModelSelection(Arc<RwLock<ModelSelection>>);

impl SharedModelSelection {
    pub(crate) fn new(selection: ModelSelection) -> Self {
        Self(Arc::new(RwLock::new(selection)))
    }

    pub(crate) fn get(&self) -> ModelSelection {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn set(&self, selection: ModelSelection) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = selection;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelPreset {
    pub(crate) model: &'static str,
    pub(crate) description: &'static str,
    pub(crate) default_reasoning_effort: ReasoningEffort,
}

impl ModelPreset {
    pub(crate) fn selection(self, reasoning_effort: ReasoningEffort) -> ModelSelection {
        ModelSelection {
            model: self.model.to_string(),
            reasoning_effort,
        }
    }
}

const STANDARD_REASONING_EFFORTS: [ReasoningEffort; 4] = [
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
];
const ADVANCED_REASONING_EFFORTS: [ReasoningEffort; 1] = [ReasoningEffort::Max];

pub(crate) const fn standard_reasoning_efforts() -> &'static [ReasoningEffort] {
    &STANDARD_REASONING_EFFORTS
}

pub(crate) const fn advanced_reasoning_efforts() -> &'static [ReasoningEffort] {
    &ADVANCED_REASONING_EFFORTS
}

const MODELS: [ModelPreset; 3] = [
    ModelPreset {
        model: "gpt-5.6-sol",
        description: "Latest frontier agentic coding model.",
        default_reasoning_effort: ReasoningEffort::Low,
    },
    ModelPreset {
        model: "gpt-5.6-terra",
        description: "Balanced agentic coding model for everyday work.",
        default_reasoning_effort: ReasoningEffort::Medium,
    },
    ModelPreset {
        model: "gpt-5.6-luna",
        description: "Fast and affordable agentic coding model.",
        default_reasoning_effort: ReasoningEffort::Medium,
    },
];

pub(crate) const fn available_models() -> &'static [ModelPreset] {
    &MODELS
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ModelSettings {
    version: u32,
    selection: ModelSelection,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            version: MODEL_SETTINGS_VERSION,
            selection: ModelSelection::default(),
        }
    }
}

pub(crate) fn load_default_selection() -> Result<ModelSelection> {
    let Some(home) = crate::paths::bettercodex_home() else {
        return Ok(ModelSelection::default());
    };
    let mut settings = read_model_settings(&home.join(MODEL_SETTINGS_FILE))?;
    settings.selection.normalize();
    settings.selection.validate()?;
    Ok(settings.selection)
}

pub(crate) fn save_default_selection(selection: &ModelSelection) -> Result<()> {
    selection.validate()?;
    let home = crate::paths::bettercodex_home()
        .ok_or_else(|| anyhow!("cannot save the model because no bettercodex home is available"))?;
    let path = home.join(MODEL_SETTINGS_FILE);
    state_file::update_json(
        &path,
        MAX_MODEL_SETTINGS_BYTES,
        read_model_settings,
        |settings| {
            let changed = settings.selection != *selection;
            if changed {
                settings.selection = selection.clone();
            }
            Ok(state_file::StateChange::from_changed(changed))
        },
    )
}

fn read_model_settings(path: &std::path::Path) -> Result<ModelSettings> {
    let settings: ModelSettings =
        state_file::read_json(path, MAX_MODEL_SETTINGS_BYTES)?.unwrap_or_default();
    if settings.version != MODEL_SETTINGS_VERSION {
        return Err(anyhow!(
            "unsupported model settings version {}; expected {MODEL_SETTINGS_VERSION}",
            settings.version
        ));
    }
    Ok(settings)
}
