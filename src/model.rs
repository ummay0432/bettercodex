//! Model catalogue and active-selection state ported from current OpenAI Codex.
//!
//! The bundled picker rows track `codex-rs/models-manager/models.json` at
//! upstream commit 8cabf5a6 while retaining only reasoning efforts this
//! runtime implements. ChatGPT's `/models` response replaces this snapshot
//! when available, matching Codex's remote-catalogue behavior while retaining
//! a usable picker offline.

use crate::auth::SharedAuth;
use crate::http_client::bounded_error_body;
use crate::state_file;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de::Error as _;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::RwLock;
use std::time::Duration;

pub(crate) const DEFAULT_MODEL: &str = "gpt-5.6-sol";
pub(crate) const DEFAULT_RAW_CONTEXT_WINDOW: u64 = 272_000;
const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u64 = 95;

const MODEL_SETTINGS_FILE: &str = "model.json";
const MODEL_SETTINGS_VERSION: u32 = 1;
const MAX_MODEL_SETTINGS_BYTES: usize = 16 * 1024;
const MODELS_ENDPOINT_CLIENT_VERSION: &str = "0.147.0";
const MODELS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MODELS_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODELS_ERROR_BYTES: usize = 16_000;
const MAX_MODELS_ERROR_CHARS: usize = 4_000;
const MAX_CATALOGUE_MODELS: usize = 100;
const MAX_REASONING_LEVELS: usize = 16;
const MAX_MODEL_SLUG_BYTES: usize = 128;
const MAX_DESCRIPTION_BYTES: usize = 4_096;
const MAX_CUSTOM_EFFORT_BYTES: usize = 64;
const MAX_COMP_HASH_BYTES: usize = 256;
const MAX_CONTEXT_WINDOW: u64 = 10_000_000;

/// Reasoning efforts bettercodex sends to the Responses API.
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub(crate) enum ReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
    Custom(String),
}

impl ReasoningEffort {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Custom(effort) => effort,
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::None => "None".to_string(),
            Self::Minimal => "Minimal".to_string(),
            Self::Low => "Low".to_string(),
            Self::Medium => "Medium".to_string(),
            Self::High => "High".to_string(),
            Self::XHigh => "Extra high".to_string(),
            Self::Max => "Max".to_string(),
            Self::Custom(effort) => effort.clone(),
        }
    }

    pub(crate) fn is_advanced(&self) -> bool {
        matches!(self, Self::Max)
    }

    fn validate(&self) -> Result<()> {
        let value = self.as_str();
        if value.is_empty()
            || value.len() > MAX_CUSTOM_EFFORT_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(anyhow!(
                "reasoning effort must contain 1 to {MAX_CUSTOM_EFFORT_BYTES} non-control bytes"
            ));
        }
        Ok(())
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
            "none" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            // Codex's orchestration-only level uses Max on inference requests.
            // Normalize catalogue and saved-session values to the API effort.
            "ultra" => Ok(Self::Max),
            "" => Err("reasoning effort must not be empty".to_string()),
            effort if effort.len() <= MAX_CUSTOM_EFFORT_BYTES => {
                Ok(Self::Custom(effort.to_string()))
            }
            _ => Err(format!(
                "reasoning effort exceeds {MAX_CUSTOM_EFFORT_BYTES} bytes"
            )),
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
#[serde(deny_unknown_fields)]
pub(crate) struct ModelSelection {
    pub(crate) model: String,
    pub(crate) reasoning_effort: ReasoningEffort,
    #[serde(default = "default_context_window")]
    pub(crate) raw_context_window: u64,
    #[serde(default = "default_effective_context_window_percent")]
    pub(crate) effective_context_window_percent: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) configured_auto_compact_token_limit: Option<u64>,
    #[serde(default)]
    pub(crate) use_responses_lite: bool,
    #[serde(default = "default_true")]
    pub(crate) prefer_websocket: bool,
    #[serde(default)]
    pub(crate) supports_fast: bool,
    /// Opaque identifier for model configurations that can share uncompacted history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) comp_hash: Option<String>,
}

impl Default for ModelSelection {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            reasoning_effort: ReasoningEffort::Max,
            raw_context_window: DEFAULT_RAW_CONTEXT_WINDOW,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
            configured_auto_compact_token_limit: None,
            use_responses_lite: true,
            prefer_websocket: true,
            supports_fast: true,
            comp_hash: Some("3000".to_string()),
        }
    }
}

impl ModelSelection {
    pub(crate) fn from_identity(model: impl Into<String>, effort: ReasoningEffort) -> Self {
        let model = model.into();
        bundled_models()
            .iter()
            .find(|preset| preset.model == model)
            .map(|preset| preset.selection(effort.clone()))
            .unwrap_or(Self {
                model,
                reasoning_effort: effort,
                raw_context_window: DEFAULT_RAW_CONTEXT_WINDOW,
                effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
                configured_auto_compact_token_limit: None,
                use_responses_lite: false,
                prefer_websocket: true,
                supports_fast: false,
                comp_hash: None,
            })
    }

    pub(crate) fn effective_context_window(&self) -> u64 {
        self.raw_context_window
            .saturating_mul(self.effective_context_window_percent)
            / 100
    }

    pub(crate) fn auto_compact_token_limit(&self) -> u64 {
        let context_limit =
            (self.raw_context_window.saturating_mul(9) / 10).min(self.effective_context_window());
        self.configured_auto_compact_token_limit
            .map_or(context_limit, |limit| limit.min(context_limit))
    }

    pub(crate) fn validate(&self) -> Result<()> {
        validate_model_slug(&self.model)?;
        self.reasoning_effort.validate()?;
        if self.raw_context_window == 0 || self.raw_context_window > MAX_CONTEXT_WINDOW {
            return Err(anyhow!(
                "model context window must be between 1 and {MAX_CONTEXT_WINDOW} tokens"
            ));
        }
        if !(1..=100).contains(&self.effective_context_window_percent) {
            return Err(anyhow!(
                "effective context window percentage must be between 1 and 100"
            ));
        }
        if self.configured_auto_compact_token_limit == Some(0) {
            return Err(anyhow!(
                "configured automatic compaction limit must be greater than zero"
            ));
        }
        if let Some(comp_hash) = &self.comp_hash {
            validate_comp_hash(comp_hash)?;
        }
        Ok(())
    }
}

/// Shared model identity for clients that outlive an individual request, such
/// as the standalone web-search adapter.
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReasoningEffortPreset {
    pub(crate) effort: ReasoningEffort,
    pub(crate) description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelPreset {
    pub(crate) model: String,
    pub(crate) description: String,
    pub(crate) default_reasoning_effort: ReasoningEffort,
    pub(crate) supported_reasoning_efforts: Vec<ReasoningEffortPreset>,
    pub(crate) raw_context_window: u64,
    pub(crate) effective_context_window_percent: u64,
    pub(crate) configured_auto_compact_token_limit: Option<u64>,
    pub(crate) use_responses_lite: bool,
    pub(crate) prefer_websocket: bool,
    pub(crate) supports_fast: bool,
    pub(crate) comp_hash: Option<String>,
    pub(crate) is_default: bool,
}

impl ModelPreset {
    pub(crate) fn selection(&self, reasoning_effort: ReasoningEffort) -> ModelSelection {
        ModelSelection {
            model: self.model.clone(),
            reasoning_effort,
            raw_context_window: self.raw_context_window,
            effective_context_window_percent: self.effective_context_window_percent,
            configured_auto_compact_token_limit: self.configured_auto_compact_token_limit,
            use_responses_lite: self.use_responses_lite,
            prefer_websocket: self.prefer_websocket,
            supports_fast: self.supports_fast,
            comp_hash: self.comp_hash.clone(),
        }
    }

    pub(crate) fn selection_with_reasoning_fallback(
        &self,
        requested_effort: &ReasoningEffort,
    ) -> ModelSelection {
        self.selection(self.reasoning_effort_with_fallback(requested_effort))
    }

    fn reasoning_effort_with_fallback(
        &self,
        requested_effort: &ReasoningEffort,
    ) -> ReasoningEffort {
        self.supported_reasoning_efforts
            .iter()
            .find(|preset| &preset.effort == requested_effort)
            .or_else(|| {
                self.supported_reasoning_efforts
                    .iter()
                    .find(|preset| preset.effort == self.default_reasoning_effort)
            })
            .or_else(|| self.supported_reasoning_efforts.first())
            .map_or_else(
                || self.default_reasoning_effort.clone(),
                |preset| preset.effort.clone(),
            )
    }
}

pub(crate) fn bundled_models() -> &'static [ModelPreset] {
    static MODELS: LazyLock<Vec<ModelPreset>> = LazyLock::new(|| {
        vec![
            model_preset(
                "gpt-5.6-sol",
                "Latest frontier agentic coding model.",
                ReasoningEffort::Low,
                common_efforts(),
                BundledModelMetadata {
                    comp_hash: Some("3000"),
                    use_responses_lite: true,
                    supports_fast: true,
                    is_default: true,
                },
            ),
            model_preset(
                "gpt-5.6-terra",
                "Balanced agentic coding model for everyday work.",
                ReasoningEffort::Medium,
                common_efforts(),
                BundledModelMetadata {
                    comp_hash: Some("3000"),
                    use_responses_lite: true,
                    supports_fast: true,
                    is_default: false,
                },
            ),
            model_preset(
                "gpt-5.6-luna",
                "Fast and affordable agentic coding model.",
                ReasoningEffort::Medium,
                common_efforts(),
                BundledModelMetadata {
                    comp_hash: Some("3000"),
                    use_responses_lite: true,
                    supports_fast: true,
                    is_default: false,
                },
            ),
            model_preset(
                "gpt-5.5",
                "Frontier model for complex coding, research, and real-world work.",
                ReasoningEffort::Medium,
                vec![
                    effort(
                        ReasoningEffort::Low,
                        "Fast responses with lighter reasoning",
                    ),
                    effort(
                        ReasoningEffort::Medium,
                        "Balances speed and reasoning depth for everyday tasks",
                    ),
                    effort(
                        ReasoningEffort::High,
                        "Greater reasoning depth for complex problems",
                    ),
                    effort(
                        ReasoningEffort::XHigh,
                        "Extra high reasoning depth for complex problems",
                    ),
                ],
                BundledModelMetadata {
                    comp_hash: Some("2911"),
                    use_responses_lite: false,
                    supports_fast: true,
                    is_default: false,
                },
            ),
            model_preset(
                "gpt-5.2",
                "Optimized for professional work and long-running agents.",
                ReasoningEffort::Medium,
                vec![
                    effort(
                        ReasoningEffort::Low,
                        "Balances speed with some reasoning; useful for straightforward queries and short explanations",
                    ),
                    effort(
                        ReasoningEffort::Medium,
                        "Provides a solid balance of reasoning depth and latency for general-purpose tasks",
                    ),
                    effort(
                        ReasoningEffort::High,
                        "Maximizes reasoning depth for complex or ambiguous problems",
                    ),
                    effort(
                        ReasoningEffort::XHigh,
                        "Extra high reasoning for complex problems",
                    ),
                ],
                BundledModelMetadata {
                    comp_hash: None,
                    use_responses_lite: false,
                    supports_fast: false,
                    is_default: false,
                },
            ),
        ]
    });
    MODELS.as_slice()
}

#[derive(Clone, Copy)]
struct BundledModelMetadata {
    comp_hash: Option<&'static str>,
    use_responses_lite: bool,
    supports_fast: bool,
    is_default: bool,
}

fn model_preset(
    model: &str,
    description: &str,
    default_reasoning_effort: ReasoningEffort,
    supported_reasoning_efforts: Vec<ReasoningEffortPreset>,
    metadata: BundledModelMetadata,
) -> ModelPreset {
    ModelPreset {
        model: model.to_string(),
        description: description.to_string(),
        default_reasoning_effort,
        supported_reasoning_efforts,
        raw_context_window: DEFAULT_RAW_CONTEXT_WINDOW,
        effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        configured_auto_compact_token_limit: None,
        use_responses_lite: metadata.use_responses_lite,
        prefer_websocket: true,
        supports_fast: metadata.supports_fast,
        comp_hash: metadata.comp_hash.map(str::to_string),
        is_default: metadata.is_default,
    }
}

fn common_efforts() -> Vec<ReasoningEffortPreset> {
    vec![
        effort(
            ReasoningEffort::Low,
            "Fast responses with lighter reasoning",
        ),
        effort(
            ReasoningEffort::Medium,
            "Balances speed and reasoning depth for everyday tasks",
        ),
        effort(
            ReasoningEffort::High,
            "Greater reasoning depth for complex problems",
        ),
        effort(
            ReasoningEffort::XHigh,
            "Extra high reasoning depth for complex problems",
        ),
        effort(
            ReasoningEffort::Max,
            "Maximum reasoning depth for the hardest problems",
        ),
    ]
}

fn effort(effort: ReasoningEffort, description: &str) -> ReasoningEffortPreset {
    ReasoningEffortPreset {
        effort,
        description: description.to_string(),
    }
}

#[derive(Clone)]
pub(crate) struct ModelCatalogClient {
    client: reqwest::Client,
    auth: SharedAuth,
    base_url: String,
}

impl ModelCatalogClient {
    pub(crate) fn new(client: reqwest::Client, auth: SharedAuth, base_url: String) -> Self {
        Self {
            client,
            auth,
            base_url,
        }
    }

    pub(crate) async fn list_models(&self) -> Result<Vec<ModelPreset>> {
        tokio::time::timeout(MODELS_REFRESH_TIMEOUT, self.list_models_with_auth_retry())
            .await
            .map_err(|_| anyhow!("model catalogue refresh timed out"))?
    }

    async fn list_models_with_auth_retry(&self) -> Result<Vec<ModelPreset>> {
        let mut force_refresh = false;
        for attempt in 0..2 {
            let auth = if force_refresh {
                self.auth.force_refreshed_snapshot(&self.client).await?
            } else {
                self.auth.refreshed_snapshot(&self.client).await?
            };
            let mut request = self
                .client
                .get(format!("{}/models", self.base_url.trim_end_matches('/')))
                .query(&[("client_version", MODELS_ENDPOINT_CLIENT_VERSION)])
                .header("authorization", auth.authorization);
            if let Some(account_id) = auth.account_id {
                request = request.header("chatgpt-account-id", account_id);
            }
            let response = request
                .send()
                .await
                .context("failed to refresh the model catalogue")?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                force_refresh = true;
                continue;
            }
            if !response.status().is_success() {
                let status = response.status();
                let body =
                    bounded_error_body(response, MAX_MODELS_ERROR_BYTES, MAX_MODELS_ERROR_CHARS)
                        .await;
                return Err(anyhow!(
                    "model catalogue request failed with {status}: {body}"
                ));
            }
            return parse_models_response(read_bounded_response(response).await?);
        }
        Err(anyhow!("model catalogue authentication failed"))
    }
}

async fn read_bounded_response(mut response: reqwest::Response) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read the model catalogue response")?
    {
        if body.len().saturating_add(chunk.len()) > MAX_MODELS_RESPONSE_BYTES {
            return Err(anyhow!(
                "model catalogue response exceeds {MAX_MODELS_RESPONSE_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Deserialize)]
struct RemoteModelsResponse {
    models: Vec<RemoteModel>,
}

#[derive(Deserialize)]
struct RemoteModel {
    slug: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    default_reasoning_level: Option<ReasoningEffort>,
    #[serde(default)]
    supported_reasoning_levels: Vec<RemoteReasoningEffort>,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default)]
    max_context_window: Option<i64>,
    #[serde(default)]
    auto_compact_token_limit: Option<i64>,
    #[serde(default)]
    comp_hash: Option<String>,
    #[serde(default = "default_remote_effective_context_window_percent")]
    effective_context_window_percent: i64,
    #[serde(default)]
    use_responses_lite: bool,
    #[serde(default = "default_true")]
    prefer_websockets: bool,
    #[serde(default)]
    service_tiers: Vec<RemoteServiceTier>,
    #[serde(default)]
    additional_speed_tiers: Vec<String>,
    #[serde(default)]
    visibility: ModelVisibility,
    #[serde(default)]
    priority: i64,
}

#[derive(Deserialize)]
struct RemoteReasoningEffort {
    effort: ReasoningEffort,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct RemoteServiceTier {
    id: String,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ModelVisibility {
    List,
    Hide,
    #[default]
    None,
}

fn parse_models_response(body: Vec<u8>) -> Result<Vec<ModelPreset>> {
    let mut response: RemoteModelsResponse =
        serde_json::from_slice(&body).context("failed to decode the model catalogue")?;
    if response.models.len() > MAX_CATALOGUE_MODELS {
        return Err(anyhow!(
            "model catalogue contains more than {MAX_CATALOGUE_MODELS} entries"
        ));
    }
    response.models.sort_by_key(|model| model.priority);
    let mut models = response
        .models
        .into_iter()
        .filter(|model| model.visibility == ModelVisibility::List)
        .map(remote_model_preset)
        .collect::<Result<Vec<_>>>()?;
    if models.is_empty() {
        return Err(anyhow!("model catalogue contains no picker-visible models"));
    }
    if let Some(default) = models.first_mut() {
        default.is_default = true;
    }
    Ok(models)
}

fn remote_model_preset(model: RemoteModel) -> Result<ModelPreset> {
    validate_model_slug(&model.slug)?;
    if let Some(comp_hash) = &model.comp_hash {
        validate_comp_hash(comp_hash)
            .with_context(|| format!("model `{}` has an invalid comp_hash", model.slug))?;
    }
    let raw_context_window = model.context_window.or(model.max_context_window).map_or(
        Ok(DEFAULT_RAW_CONTEXT_WINDOW),
        |window| {
            u64::try_from(window)
                .map_err(|_| anyhow!("model `{}` has an invalid context window", model.slug))
        },
    )?;
    if raw_context_window == 0 || raw_context_window > MAX_CONTEXT_WINDOW {
        return Err(anyhow!(
            "model `{}` has an invalid context window",
            model.slug
        ));
    }
    let effective_context_window_percent = u64::try_from(model.effective_context_window_percent)
        .map_err(|_| {
            anyhow!(
                "model `{}` has an invalid effective context window percentage",
                model.slug
            )
        })?;
    if !(1..=100).contains(&effective_context_window_percent) {
        return Err(anyhow!(
            "model `{}` has an invalid effective context window percentage",
            model.slug
        ));
    }
    let configured_auto_compact_token_limit = model
        .auto_compact_token_limit
        .map(|limit| {
            u64::try_from(limit).map_err(|_| {
                anyhow!(
                    "model `{}` has an invalid automatic compaction limit",
                    model.slug
                )
            })
        })
        .transpose()?;
    if configured_auto_compact_token_limit == Some(0) {
        return Err(anyhow!(
            "model `{}` has an invalid automatic compaction limit",
            model.slug
        ));
    }
    if model.supported_reasoning_levels.len() > MAX_REASONING_LEVELS {
        return Err(anyhow!(
            "model `{}` advertises too many reasoning levels",
            model.slug
        ));
    }
    let description = bounded_catalogue_text(model.description.unwrap_or_default())?;
    let default_reasoning_effort = model
        .default_reasoning_level
        .unwrap_or(ReasoningEffort::None);
    default_reasoning_effort.validate()?;
    let mut supported_reasoning_efforts = model
        .supported_reasoning_levels
        .into_iter()
        .map(|option| {
            option.effort.validate()?;
            Ok(ReasoningEffortPreset {
                effort: option.effort,
                description: bounded_catalogue_text(option.description)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    supported_reasoning_efforts.dedup_by(|left, right| left.effort == right.effort);
    if supported_reasoning_efforts.is_empty() {
        supported_reasoning_efforts.push(ReasoningEffortPreset {
            effort: default_reasoning_effort.clone(),
            description: String::new(),
        });
    }
    let supports_fast = model.service_tiers.iter().any(|tier| tier.id == "priority")
        || model
            .additional_speed_tiers
            .iter()
            .any(|tier| tier == "fast");
    Ok(ModelPreset {
        model: model.slug,
        description,
        default_reasoning_effort,
        supported_reasoning_efforts,
        raw_context_window,
        effective_context_window_percent,
        configured_auto_compact_token_limit,
        use_responses_lite: model.use_responses_lite,
        prefer_websocket: model.prefer_websockets,
        supports_fast,
        comp_hash: model.comp_hash,
        is_default: false,
    })
}

fn validate_model_slug(model: &str) -> Result<()> {
    if model.is_empty() || model.len() > MAX_MODEL_SLUG_BYTES || model.chars().any(char::is_control)
    {
        return Err(anyhow!(
            "model slug must contain 1 to {MAX_MODEL_SLUG_BYTES} non-control bytes"
        ));
    }
    Ok(())
}

fn validate_comp_hash(comp_hash: &str) -> Result<()> {
    if comp_hash.is_empty()
        || comp_hash.len() > MAX_COMP_HASH_BYTES
        || comp_hash.chars().any(char::is_control)
    {
        return Err(anyhow!(
            "comp_hash must contain 1 to {MAX_COMP_HASH_BYTES} non-control bytes"
        ));
    }
    Ok(())
}

fn bounded_catalogue_text(text: String) -> Result<String> {
    if text.len() > MAX_DESCRIPTION_BYTES || text.chars().any(char::is_control) {
        return Err(anyhow!(
            "model catalogue text exceeds its display safety limit"
        ));
    }
    Ok(text)
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
    let settings = read_model_settings(&home.join(MODEL_SETTINGS_FILE))?;
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
            settings.selection = selection.clone();
            Ok(())
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

const fn default_context_window() -> u64 {
    DEFAULT_RAW_CONTEXT_WINDOW
}

const fn default_effective_context_window_percent() -> u64 {
    DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT
}

const fn default_remote_effective_context_window_percent() -> i64 {
    DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT as i64
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
