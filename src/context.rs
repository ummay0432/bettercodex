use crate::compaction::InitialContextInjection;
use crate::repository;
use crate::rollout::HistoryReplacement;
use crate::rollout::LoadedRollout;
use crate::rollout::Rollout;
use crate::rollout::TurnOutcome;
use crate::skills::SkillCatalog;
use crate::usage::TokenUsage;
use anyhow::Context;
use anyhow::Result;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::formatted_truncate_text;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use uuid::Uuid;

const MAX_REPOSITORY_INSTRUCTIONS_BYTES: usize = 64 * 1024;
const MAX_CONTEXT_NOTICE_TEXT_TOKENS: usize = 9_900;
const RESIZED_IMAGE_BYTES_ESTIMATE: u64 = 7_373;
const ORIGINAL_IMAGE_MAX_PATCHES: u64 = 10_000;
// Codex derives these independently from the resolved raw model context: 95% is the
// usable hard window, while automatic compaction starts at 90% of the raw window.
pub(crate) const RAW_CONTEXT_WINDOW: u64 = 372_000;
pub(crate) const EFFECTIVE_CONTEXT_WINDOW: u64 = RAW_CONTEXT_WINDOW * 95 / 100;
pub(crate) const AUTO_COMPACT_TOKEN_LIMIT: u64 = RAW_CONTEXT_WINDOW * 90 / 100;
static STABLE_HARNESS_TOKEN_ESTIMATES: LazyLock<[u64; 2]> = LazyLock::new(|| {
    let [tools_item] = crate::api::stable_input_prefix_items();
    [
        estimate_value_tokens(tools_item),
        crate::api::estimated_harness_instruction_tokens(),
    ]
});
const SYNTHETIC_OUTPUT_NAMESPACE: Uuid = Uuid::from_u128(0x90d38d3e_6a5b_4d52_bfe2_2f1e634bfac4);
const INTERRUPTED_GUIDANCE: &str = "The user interrupted the previous turn on purpose. Any command or tool that was running may have partially executed. Inspect the workspace before repeating an interrupted action.";
const CRASH_GUIDANCE: &str = "The previous bettercodex process ended before its active turn completed. Any command or tool that was running may have partially executed. Inspect the workspace before continuing or repeating an action.";
const LEGACY_REPOSITORY_ONBOARDING_PREFIX: &str = "# Repository onboarding from AGENTS.md for ";
const LEGACY_SKILLS_PREFIX: &str = "<skills>";
const LEGACY_SKILL_CONTEXT_PREFIX: &str = "<skill>";
const REPOSITORY_CONTEXT_PREFIX: &str = "<repository_context>";
const AVAILABLE_SKILLS_PREFIX: &str = "<available_skills>";
const CONTEXTUAL_USER_PREFIXES: [&str; 8] = [
    LEGACY_REPOSITORY_ONBOARDING_PREFIX,
    REPOSITORY_CONTEXT_PREFIX,
    AVAILABLE_SKILLS_PREFIX,
    "<environment_context>",
    "<skill_context>",
    LEGACY_SKILL_CONTEXT_PREFIX,
    "<turn_aborted>",
    "<response_interrupted>",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContextKind {
    SystemPrompt,
    ToolCatalogue,
    RepositoryInstructions,
    Skills,
    Environment,
    UserMessages,
    AssistantMessages,
    ToolActivity,
    Reasoning,
    Compaction,
    Other,
}

const CONTEXT_KINDS: [ContextKind; 11] = [
    ContextKind::SystemPrompt,
    ContextKind::ToolCatalogue,
    ContextKind::RepositoryInstructions,
    ContextKind::Skills,
    ContextKind::Environment,
    ContextKind::UserMessages,
    ContextKind::AssistantMessages,
    ContextKind::ToolActivity,
    ContextKind::Reasoning,
    ContextKind::Compaction,
    ContextKind::Other,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContextSection {
    pub(crate) kind: ContextKind,
    /// Estimated share of the active total, calibrated to backend usage when available.
    pub(crate) tokens: u64,
    pub(crate) items: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContextSnapshot {
    pub(crate) used_tokens: u64,
    pub(crate) context_window: u64,
    pub(crate) compact_at_tokens: u64,
    /// Whether `used_tokens` came from Responses usage rather than only local estimation.
    pub(crate) measured: bool,
    pub(crate) sections: Vec<ContextSection>,
}

pub(crate) struct Conversation {
    history: Vec<Value>,
    history_lineage: Uuid,
    context_metrics: ContextMetrics,
    usage: Option<TokenUsage>,
    usage_history_estimate: Option<u64>,
    server_reasoning_included: bool,
    rollout: Rollout,
    world_state: WorldState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HistoryCursor {
    lineage: Uuid,
    len: usize,
}

impl HistoryCursor {
    pub(crate) fn len(self) -> usize {
        self.len
    }

    pub(crate) fn includes_response_after(self, previous: Self, response_items: usize) -> bool {
        self.lineage == previous.lineage
            && previous
                .len
                .checked_add(response_items)
                .is_some_and(|minimum_len| self.len >= minimum_len)
    }
}

#[derive(Clone)]
struct WorldState {
    environment: Value,
    repository_context: Option<Value>,
    skills_catalogue: Option<Value>,
    skills: SkillCatalog,
}

/// Aggregate request accounting kept in lockstep with `Conversation::history`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ContextMetrics {
    estimated_tokens: u64,
    tokens: [u64; CONTEXT_KINDS.len()],
    items: [usize; CONTEXT_KINDS.len()],
    encrypted_reasoning_tokens: u64,
    encrypted_reasoning_before_last_instruction: u64,
    has_tools: bool,
}

impl Conversation {
    pub(crate) fn new(cwd: &Path, mut rollout: Rollout) -> Result<Self> {
        let world_state = WorldState::load(cwd)?;
        let history = world_state.items();
        rollout.replace_history(&history, HistoryReplacement::Initial)?;
        let context_metrics = ContextMetrics::from_history(&history, &world_state);
        Ok(Self {
            history,
            history_lineage: Uuid::new_v4(),
            context_metrics,
            usage: None,
            usage_history_estimate: None,
            server_reasoning_included: false,
            rollout,
            world_state,
        })
    }

    pub(crate) fn resume(cwd: &Path, loaded: LoadedRollout) -> Result<Self> {
        let LoadedRollout {
            rollout,
            history,
            usage,
            usage_history_estimate,
            server_reasoning_included,
            unfinished_turn,
            ..
        } = loaded;
        let world_state = WorldState::load(cwd)?;
        let context_metrics = ContextMetrics::from_history(&history, &world_state);
        let mut conversation = Self {
            history,
            history_lineage: Uuid::new_v4(),
            context_metrics,
            usage,
            usage_history_estimate,
            server_reasoning_included,
            rollout,
            world_state,
        };
        if let Some(turn_id) = unfinished_turn {
            conversation.normalize()?;
            conversation.append_context_notice("turn_aborted", CRASH_GUIDANCE)?;
            conversation
                .rollout
                .finish_turn(&turn_id, TurnOutcome::Interrupted)?;
        }
        conversation.refresh_world_state()?;
        Ok(conversation)
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.rollout.identity().session_id
    }

    pub(crate) fn start_turn(&mut self, turn_id: &str) -> Result<()> {
        self.rollout.start_turn(turn_id)
    }

    pub(crate) fn finish_turn(&mut self, turn_id: &str, outcome: TurnOutcome) -> Result<()> {
        self.rollout.finish_turn(turn_id, outcome)
    }

    pub(crate) fn extend(&mut self, items: impl IntoIterator<Item = Value>) -> Result<()> {
        let items = items.into_iter().collect::<Vec<_>>();
        if items.is_empty() {
            return Ok(());
        }
        self.rollout.append_history(&items)?;
        self.context_metrics.extend(&items, &self.world_state);
        self.history.extend(items);
        Ok(())
    }

    pub(crate) fn replace_compacted(
        &mut self,
        mut history: Vec<Value>,
        initial_context_injection: InitialContextInjection,
        response_usage: Option<TokenUsage>,
    ) -> Result<()> {
        self.world_state
            .insert_missing_into(&mut history, initial_context_injection);
        self.rollout
            .replace_compacted_history(&history, response_usage.as_ref())?;
        self.context_metrics = ContextMetrics::from_history(&history, &self.world_state);
        self.history = history;
        self.history_lineage = Uuid::new_v4();
        self.usage = None;
        self.usage_history_estimate = None;
        self.server_reasoning_included = false;
        Ok(())
    }

    pub(crate) fn items(&self) -> &[Value] {
        &self.history
    }

    pub(crate) fn history_cursor(&self) -> HistoryCursor {
        HistoryCursor {
            lineage: self.history_lineage,
            len: self.history.len(),
        }
    }

    pub(crate) fn take_history_for_sampling(&mut self) -> (Vec<Value>, HistoryCursor) {
        let cursor = self.history_cursor();
        (std::mem::take(&mut self.history), cursor)
    }

    pub(crate) fn restore_history_after_sampling(
        &mut self,
        mut history: Vec<Value>,
        cursor: HistoryCursor,
    ) -> Result<()> {
        if self.history_lineage != cursor.lineage || history.len() != cursor.len {
            anyhow::bail!("conversation changed while its sampling history was in flight");
        }
        history.append(&mut self.history);
        self.history = history;
        Ok(())
    }

    pub(crate) fn prompt_history(&self) -> Vec<String> {
        self.history
            .iter()
            .filter(|item| is_user_message(item) && !is_contextual_user_message(item))
            .filter_map(message_text)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub(crate) fn skill_catalog(&self) -> &SkillCatalog {
        &self.world_state.skills
    }

    pub(crate) fn reload_skills(&mut self, cwd: &Path) -> Result<()> {
        let skills = SkillCatalog::load(cwd);
        let mut world_state = self.world_state.clone();
        world_state.skills_catalogue = skills.catalogue_message(EFFECTIVE_CONTEXT_WINDOW);
        world_state.skills = skills;
        self.replace_world_state(world_state)
    }

    pub(crate) fn record_usage(
        &mut self,
        usage: Option<TokenUsage>,
        server_reasoning_included: bool,
    ) -> Result<()> {
        let Some(usage) = usage else {
            return Ok(());
        };
        let history_estimate = self.context_metrics.estimated_tokens;
        self.rollout
            .record_usage(&usage, history_estimate, server_reasoning_included)?;
        self.usage = Some(usage);
        self.usage_history_estimate = Some(history_estimate);
        self.server_reasoning_included = server_reasoning_included;
        Ok(())
    }

    pub(crate) fn context_tokens(&self) -> Option<u64> {
        self.context_tokens_with_metrics(&self.context_metrics)
    }

    fn context_tokens_with_metrics(&self, metrics: &ContextMetrics) -> Option<u64> {
        let usage = self.usage.as_ref()?;
        let baseline = self.usage_history_estimate?;
        let history_estimate = metrics.estimated_tokens;
        let active_context = if history_estimate >= baseline {
            usage
                .active_context_tokens()
                .saturating_add(history_estimate - baseline)
        } else {
            usage
                .active_context_tokens()
                .saturating_sub(baseline - history_estimate)
        };
        let omitted_reasoning = if self.server_reasoning_included {
            0
        } else {
            metrics.encrypted_reasoning_before_last_instruction
        };
        Some(active_context.saturating_add(omitted_reasoning))
    }

    pub(crate) fn context_snapshot(&self) -> ContextSnapshot {
        let [tools_tokens, system_prompt_tokens] = *STABLE_HARNESS_TOKEN_ESTIMATES;
        let mut tokens = self.context_metrics.tokens;
        let mut items = self.context_metrics.items;
        if !self.context_metrics.has_tools {
            record_context_estimate(
                &mut tokens,
                &mut items,
                ContextKind::ToolCatalogue,
                tools_tokens,
            );
        }
        record_context_estimate(
            &mut tokens,
            &mut items,
            ContextKind::SystemPrompt,
            system_prompt_tokens,
        );

        let mut sections = CONTEXT_KINDS
            .into_iter()
            .enumerate()
            .filter(|(index, _)| items[*index] > 0)
            .map(|(index, kind)| ContextSection {
                kind,
                tokens: tokens[index],
                items: items[index],
            })
            .collect::<Vec<_>>();
        let estimated_total = sections.iter().map(|section| section.tokens).sum();
        let measured_total = self.context_tokens_with_metrics(&self.context_metrics);
        let used_tokens = measured_total.unwrap_or(estimated_total);
        if measured_total.is_some() {
            scale_context_sections(&mut sections, used_tokens);
        }

        ContextSnapshot {
            used_tokens,
            context_window: EFFECTIVE_CONTEXT_WINDOW,
            compact_at_tokens: AUTO_COMPACT_TOKEN_LIMIT,
            measured: measured_total.is_some(),
            sections,
        }
    }

    pub(crate) fn projected_tokens(&self, additional: &[Value]) -> u64 {
        if additional.is_empty() {
            return self
                .context_tokens()
                .unwrap_or_else(|| self.estimated_context_tokens(&self.context_metrics));
        }

        // A real user message advances the instruction boundary used by Codex's
        // X-Reasoning-Included fallback. Project the complete accounting state so
        // prior encrypted reasoning cannot appear only after input admission.
        let mut projected_metrics = self.context_metrics.clone();
        projected_metrics.extend(additional, &self.world_state);
        self.context_tokens_with_metrics(&projected_metrics)
            .unwrap_or_else(|| self.estimated_context_tokens(&projected_metrics))
    }

    fn estimated_context_tokens(&self, metrics: &ContextMetrics) -> u64 {
        let [tools_tokens, system_prompt_tokens] = *STABLE_HARNESS_TOKEN_ESTIMATES;
        let mut estimate = metrics.estimated_tokens;
        if !metrics.has_tools {
            estimate = estimate.saturating_add(tools_tokens);
        }
        estimate.saturating_add(system_prompt_tokens)
    }

    pub(crate) fn needs_compaction(&self) -> bool {
        self.projected_tokens(&[]) >= AUTO_COMPACT_TOKEN_LIMIT
    }

    pub(crate) fn needs_compaction_with(&self, additional: &[Value]) -> bool {
        self.projected_tokens(additional) >= AUTO_COMPACT_TOKEN_LIMIT
    }

    pub(crate) fn mark_interrupted(&mut self) -> Result<()> {
        self.normalize()?;
        self.append_context_notice("turn_aborted", INTERRUPTED_GUIDANCE)
    }

    pub(crate) fn mark_stream_interrupted(&mut self, message: &str) -> Result<()> {
        self.normalize()?;
        self.append_context_notice(
            "response_interrupted",
            &format!(
                "The model response stream ended before response.completed: {message}. Completed response items were preserved. Continue from the preserved state without assuming an unfinished action succeeded."
            ),
        )
    }

    pub(crate) fn normalize(&mut self) -> Result<bool> {
        if history_is_normalized(&self.history) {
            return Ok(false);
        }
        let mut normalized = self.history.clone();
        normalize_history(&mut normalized);
        self.rollout
            .replace_history(&normalized, HistoryReplacement::Normalization)?;
        self.context_metrics = ContextMetrics::from_history(&normalized, &self.world_state);
        self.history = normalized;
        self.history_lineage = Uuid::new_v4();
        Ok(true)
    }

    fn append_context_notice(&mut self, tag: &str, guidance: &str) -> Result<()> {
        let guidance = formatted_truncate_text(
            guidance,
            TruncationPolicy::Tokens(MAX_CONTEXT_NOTICE_TEXT_TOKENS),
        );
        self.extend([message("user", format!("<{tag}>\n{guidance}\n</{tag}>"))])
    }

    fn refresh_world_state(&mut self) -> Result<()> {
        self.replace_world_state(self.world_state.clone())
    }

    fn replace_world_state(&mut self, world_state: WorldState) -> Result<()> {
        let current = world_state.items();
        let saved = self
            .history
            .iter()
            .filter(|item| is_generated_world_state_message(item))
            .collect::<Vec<_>>();
        let already_current = saved.len() == current.len()
            && current.iter().all(|expected| {
                saved
                    .iter()
                    .any(|existing| same_model_visible_message(existing, expected))
            });
        if already_current {
            self.world_state = world_state;
            return Ok(());
        }

        let mut refreshed = self
            .history
            .iter()
            .filter(|item| !is_generated_world_state_message(item))
            .cloned()
            .collect::<Vec<_>>();
        let insertion = if refreshed
            .last()
            .is_some_and(|item| is_user_message(item) && !is_contextual_user_message(item))
        {
            refreshed.len().saturating_sub(1)
        } else {
            refreshed.len()
        };
        refreshed.splice(insertion..insertion, current);
        self.rollout
            .replace_history(&refreshed, HistoryReplacement::ContextRefresh)?;
        self.context_metrics = ContextMetrics::from_history(&refreshed, &world_state);
        self.history = refreshed;
        self.history_lineage = Uuid::new_v4();
        self.world_state = world_state;
        Ok(())
    }
}

impl WorldState {
    fn load(cwd: &Path) -> Result<Self> {
        let skills = SkillCatalog::load(cwd);
        let skills_catalogue = skills.catalogue_message(EFFECTIVE_CONTEXT_WINDOW);
        Ok(Self {
            environment: message("developer", environment_context(cwd)),
            repository_context: repository_context(cwd)?.map(|context| message("user", context)),
            skills_catalogue,
            skills,
        })
    }

    fn items(&self) -> Vec<Value> {
        let mut items = vec![self.environment.clone()];
        if let Some(context) = &self.repository_context {
            items.push(context.clone());
        }
        if let Some(catalogue) = &self.skills_catalogue {
            items.push(catalogue.clone());
        }
        items
    }

    fn insert_missing_into(
        &self,
        history: &mut Vec<Value>,
        initial_context_injection: InitialContextInjection,
    ) {
        let missing = self.missing_from(history);
        if missing.is_empty() {
            return;
        }
        match initial_context_injection {
            InitialContextInjection::AfterCompaction => history.extend(missing),
            InitialContextInjection::BeforeLastUserMessage => {
                let insertion = history
                    .iter()
                    .rposition(is_initial_context_boundary)
                    .or_else(|| history.iter().rposition(is_compaction_item))
                    .unwrap_or(history.len());
                history.splice(insertion..insertion, missing);
            }
        }
    }

    fn missing_from(&self, history: &[Value]) -> Vec<Value> {
        self.items()
            .into_iter()
            .filter(|expected| {
                !history
                    .iter()
                    .any(|existing| same_model_visible_message(existing, expected))
            })
            .collect()
    }
}

pub(crate) fn initial_context_items(cwd: &Path) -> Result<Vec<Value>> {
    Ok(WorldState::load(cwd)?.items())
}

impl ContextMetrics {
    fn from_history(history: &[Value], world_state: &WorldState) -> Self {
        let mut metrics = Self::default();
        metrics.extend(history, world_state);
        metrics
    }

    fn extend(&mut self, history: &[Value], world_state: &WorldState) {
        let [tools_item] = crate::api::stable_input_prefix_items();
        for item in history {
            if is_initial_context_boundary(item) {
                self.encrypted_reasoning_before_last_instruction = self.encrypted_reasoning_tokens;
            }
            let kind = if same_additional_tools_item(item, tools_item) {
                self.has_tools = true;
                ContextKind::ToolCatalogue
            } else if same_model_visible_message(item, &world_state.environment) {
                ContextKind::Environment
            } else if world_state
                .repository_context
                .as_ref()
                .is_some_and(|context| same_model_visible_message(item, context))
            {
                ContextKind::RepositoryInstructions
            } else if world_state
                .skills_catalogue
                .as_ref()
                .is_some_and(|catalogue| same_model_visible_message(item, catalogue))
            {
                ContextKind::Skills
            } else {
                context_kind(item)
            };
            let estimated = record_context_item(&mut self.tokens, &mut self.items, kind, item);
            if item.get("type").and_then(Value::as_str) == Some("reasoning")
                && item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .is_some()
            {
                self.encrypted_reasoning_tokens =
                    self.encrypted_reasoning_tokens.saturating_add(estimated);
            }
            self.estimated_tokens = self.estimated_tokens.saturating_add(estimated);
        }
    }
}

fn same_model_visible_message(left: &Value, right: &Value) -> bool {
    ["type", "role", "content"]
        .into_iter()
        .all(|field| left.get(field) == right.get(field))
}

fn same_additional_tools_item(item: &Value, expected: &Value) -> bool {
    ["type", "role", "tools"]
        .into_iter()
        .all(|field| item.get(field) == expected.get(field))
}

fn context_kind(item: &Value) -> ContextKind {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => match item.get("role").and_then(Value::as_str) {
            Some("user") => ContextKind::UserMessages,
            Some("assistant") => ContextKind::AssistantMessages,
            _ => ContextKind::Other,
        },
        Some("reasoning") => ContextKind::Reasoning,
        Some("compaction" | "compaction_summary") => ContextKind::Compaction,
        Some(item_type) if item_type.ends_with("_call") || item_type.ends_with("_call_output") => {
            ContextKind::ToolActivity
        }
        _ => ContextKind::Other,
    }
}

fn record_context_item(
    tokens: &mut [u64; CONTEXT_KINDS.len()],
    items: &mut [usize; CONTEXT_KINDS.len()],
    kind: ContextKind,
    item: &Value,
) -> u64 {
    let estimated = estimate_value_tokens(item);
    record_context_estimate(tokens, items, kind, estimated);
    estimated
}

fn record_context_estimate(
    tokens: &mut [u64; CONTEXT_KINDS.len()],
    items: &mut [usize; CONTEXT_KINDS.len()],
    kind: ContextKind,
    estimated: u64,
) {
    let index = context_kind_index(kind);
    tokens[index] = tokens[index].saturating_add(estimated);
    items[index] = items[index].saturating_add(1);
}

fn context_kind_index(kind: ContextKind) -> usize {
    match kind {
        ContextKind::SystemPrompt => 0,
        ContextKind::ToolCatalogue => 1,
        ContextKind::RepositoryInstructions => 2,
        ContextKind::Skills => 3,
        ContextKind::Environment => 4,
        ContextKind::UserMessages => 5,
        ContextKind::AssistantMessages => 6,
        ContextKind::ToolActivity => 7,
        ContextKind::Reasoning => 8,
        ContextKind::Compaction => 9,
        ContextKind::Other => 10,
    }
}

fn scale_context_sections(sections: &mut [ContextSection], target: u64) {
    let estimated = sections.iter().map(|section| section.tokens).sum::<u64>();
    if estimated == 0 || estimated == target {
        return;
    }

    let denominator = u128::from(estimated);
    let mut allocated = 0_u64;
    let mut remainders = Vec::with_capacity(sections.len());
    for (index, section) in sections.iter_mut().enumerate() {
        let scaled = u128::from(section.tokens) * u128::from(target);
        section.tokens = u64::try_from(scaled / denominator).unwrap_or(u64::MAX);
        allocated = allocated.saturating_add(section.tokens);
        remainders.push((index, scaled % denominator));
    }
    remainders.sort_by(|(left_index, left), (right_index, right)| {
        right.cmp(left).then_with(|| left_index.cmp(right_index))
    });
    let remaining = usize::try_from(target.saturating_sub(allocated)).unwrap_or(usize::MAX);
    for (index, _) in remainders.into_iter().take(remaining) {
        sections[index].tokens = sections[index].tokens.saturating_add(1);
    }
}

fn is_user_message(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("user")
}

pub(crate) fn is_contextual_user_message(item: &Value) -> bool {
    is_user_message(item) && message_text(item).is_some_and(is_contextual_user_text)
}

pub(crate) fn is_contextual_user_text(text: &str) -> bool {
    let text = text.trim_start();
    CONTEXTUAL_USER_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn is_initial_context_boundary(item: &Value) -> bool {
    (is_user_message(item) && !is_contextual_user_message(item))
        || (item.get("type").and_then(Value::as_str) == Some("agent_message")
            && !item
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.starts_with("Message Type: FINAL_ANSWER\n")))
}

fn is_generated_world_state_message(item: &Value) -> bool {
    let role = item.get("role").and_then(Value::as_str);
    let Some(text) = message_text(item).map(str::trim_start) else {
        return false;
    };
    (role == Some("developer")
        && (text.starts_with("<environment_context>") || text.starts_with(LEGACY_SKILLS_PREFIX)))
        || (role == Some("user")
            && ((text.starts_with(REPOSITORY_CONTEXT_PREFIX)
                && text.trim_end().ends_with("</repository_context>"))
                || (text.starts_with(AVAILABLE_SKILLS_PREFIX)
                    && text.trim_end().ends_with("</available_skills>"))
                || (text.starts_with(LEGACY_REPOSITORY_ONBOARDING_PREFIX)
                    && text.trim_end().ends_with("# End repository onboarding"))))
}

fn message_text(item: &Value) -> Option<&str> {
    item.get("content")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|content| content.get("text").and_then(Value::as_str))
}

fn is_compaction_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("compaction" | "compaction_summary")
    )
}

pub(crate) fn message(role: &str, text: String) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{"type": "input_text", "text": text}],
    })
}

fn environment_context(cwd: &Path) -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let date = command_output("date", &["+%F"]).unwrap_or_else(|| "unknown".to_string());
    let timezone = std::fs::read_to_string("/etc/timezone")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| command_output("date", &["+%Z"]))
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "<environment_context>\n  <cwd>{}</cwd>\n  <shell>{}</shell>\n  <current_date>{date}</current_date>\n  <timezone>{timezone}</timezone>\n</environment_context>",
        escape_xml(&cwd.display().to_string()),
        escape_xml(&shell),
    )
}

fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn repository_context(cwd: &Path) -> Result<Option<String>> {
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("failed to resolve working directory {}", cwd.display()))?;
    let mut candidates = Vec::new();
    if let Some(codex_home) = codex_home()
        && let Some(path) = first_instruction_file(&codex_home)?
    {
        candidates.push(path);
    }

    let project_root = repository::find_root(&cwd).unwrap_or_else(|| cwd.clone());
    let mut directories = Vec::new();
    let mut directory = cwd.as_path();
    loop {
        directories.push(directory.to_path_buf());
        if directory == project_root {
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent;
    }
    directories.reverse();
    for directory in directories {
        if let Some(path) = first_instruction_file(&directory)? {
            candidates.push(path);
        }
    }

    let mut seen = HashSet::new();
    let mut remaining = MAX_REPOSITORY_INSTRUCTIONS_BYTES;
    let mut sections = Vec::new();
    for path in candidates {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !seen.insert(canonical) || remaining == 0 {
            continue;
        }
        let (bytes, truncated) = read_instruction_file(&path, remaining)?;
        let mut content = String::from_utf8_lossy(&bytes).into_owned();
        if truncated {
            content.push_str("\n[AGENTS.md truncated]");
        }
        if !content.trim().is_empty() {
            sections.push(format!(
                "<repository_instructions path=\"{}\">\n<![CDATA[\n{}\n]]>\n</repository_instructions>",
                escape_xml(&path.display().to_string()),
                escape_cdata(content.trim()),
            ));
            remaining = remaining.saturating_sub(bytes.len());
        }
    }

    if sections.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "<repository_context>\n{}\n</repository_context>",
        sections.join("\n"),
    )))
}

fn first_instruction_file(directory: &Path) -> Result<Option<PathBuf>> {
    for name in ["AGENTS.override.md", "AGENTS.md"] {
        let path = directory.join(name);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => return Ok(Some(path)),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect instructions at {}", path.display())
                });
            }
        }
    }
    Ok(None)
}

fn read_instruction_file(path: &Path, limit: usize) -> Result<(Vec<u8>, bool)> {
    let file = File::open(path)
        .with_context(|| format!("failed to read instructions from {}", path.display()))?;
    let length = file.metadata()?.len();
    let mut bytes = Vec::with_capacity(limit.min(length.try_into().unwrap_or(usize::MAX)));
    file.take(limit as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read instructions from {}", path.display()))?;
    let truncated = length > bytes.len() as u64;
    Ok((bytes, truncated))
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

pub(crate) fn estimated_tokens(items: &[Value]) -> u64 {
    items
        .iter()
        .map(estimate_value_tokens)
        .fold(0_u64, u64::saturating_add)
}

#[derive(Default)]
struct SerializedSize {
    bytes: u64,
}

impl Write for SerializedSize {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(buffer.len()).unwrap_or(u64::MAX));
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn estimate_value_tokens(value: &Value) -> u64 {
    let item_type = value.get("type").and_then(Value::as_str);
    if matches!(
        item_type,
        Some("reasoning" | "compaction" | "compaction_summary" | "context_compaction")
    ) && let Some(encrypted) = value.get("encrypted_content").and_then(Value::as_str)
    {
        return estimate_reasoning_bytes(encrypted.len()).div_ceil(4);
    }

    let mut serialized_size = SerializedSize::default();
    let mut bytes = serde_json::to_writer(&mut serialized_size, value)
        .map(|()| serialized_size.bytes)
        .unwrap_or_default();
    visit_model_content(
        value,
        &mut |content| match content.get("type").and_then(Value::as_str) {
            Some("input_image") => {
                let Some(image_url) = content.get("image_url").and_then(Value::as_str) else {
                    return;
                };
                let Some(payload) = base64_data_url_payload(image_url, "image/") else {
                    return;
                };
                let replacement = if matches!(
                    content.get("detail").and_then(Value::as_str),
                    Some("original" | "auto")
                ) {
                    estimate_image_tokens(image_url).saturating_mul(4)
                } else {
                    RESIZED_IMAGE_BYTES_ESTIMATE
                };
                bytes = bytes
                    .saturating_sub(payload.len() as u64)
                    .saturating_add(replacement);
            }
            Some("input_audio") => {
                let Some(audio_url) = content.get("audio_url").and_then(Value::as_str) else {
                    return;
                };
                let Some(payload) = base64_data_url_payload(audio_url, "audio/") else {
                    return;
                };
                let replacement =
                    u64::try_from(codex_utils_audio::estimate_audio_token_count(audio_url))
                        .unwrap_or(u64::MAX)
                        .saturating_mul(4);
                bytes = bytes
                    .saturating_sub(payload.len() as u64)
                    .saturating_add(replacement);
            }
            Some("encrypted_content")
                if matches!(item_type, Some("function_call_output" | "agent_message")) =>
            {
                let Some(encrypted) = content.get("encrypted_content").and_then(Value::as_str)
                else {
                    return;
                };
                bytes = bytes
                    .saturating_sub(encrypted.len() as u64)
                    .saturating_add((encrypted.len() as u64).saturating_mul(9).div_ceil(16));
            }
            _ => {}
        },
    );
    bytes.div_ceil(4)
}

fn estimate_reasoning_bytes(encoded_len: usize) -> u64 {
    (encoded_len as u64)
        .saturating_mul(3)
        .checked_div(4)
        .unwrap_or_default()
        .saturating_sub(650)
}

fn visit_model_content(value: &Value, visitor: &mut impl FnMut(&Value)) {
    match value {
        Value::Array(values) => {
            for value in values {
                visit_model_content(value, visitor);
            }
        }
        Value::Object(object) => {
            visitor(value);
            for value in object.values() {
                visit_model_content(value, visitor);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn base64_data_url_payload<'a>(url: &'a str, media_type_prefix: &str) -> Option<&'a str> {
    if !url
        .get(.."data:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
    {
        return None;
    }
    let (metadata, payload) = url.split_once(',')?;
    let mut metadata_parts = metadata.get("data:".len()..)?.split(';');
    let mime_type = metadata_parts.next().unwrap_or_default();
    let has_base64_marker = metadata_parts.any(|part| part.eq_ignore_ascii_case("base64"));
    mime_type
        .get(..media_type_prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(media_type_prefix))
        .then_some(())?;
    has_base64_marker.then_some(payload)
}

fn estimate_image_tokens(image_url: &str) -> u64 {
    let Some(encoded) = base64_data_url_payload(image_url, "image/") else {
        return RESIZED_IMAGE_BYTES_ESTIMATE.div_ceil(4);
    };
    use base64::Engine;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return RESIZED_IMAGE_BYTES_ESTIMATE.div_ceil(4);
    };
    image_dimensions(&bytes)
        .map(|(width, height)| {
            u64::from(width.div_ceil(32)).saturating_mul(u64::from(height.div_ceil(32)))
        })
        .map(|patches| patches.min(ORIGINAL_IMAGE_MAX_PATCHES))
        .unwrap_or_else(|| RESIZED_IMAGE_BYTES_ESTIMATE.div_ceil(4))
}

fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        return Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        ));
    }
    if (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) && bytes.len() >= 10 {
        return Some((
            u16::from_le_bytes(bytes[6..8].try_into().ok()?).into(),
            u16::from_le_bytes(bytes[8..10].try_into().ok()?).into(),
        ));
    }
    if bytes.len() >= 30 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return webp_dimensions(bytes);
    }
    jpeg_dimensions(bytes)
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match bytes.get(12..16)? {
        b"VP8X" => Some((
            1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]),
            1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]),
        )),
        b"VP8 " if bytes.get(23..26) == Some(&b"\x9d\x01\x2a"[..]) => Some((
            u32::from(u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3fff),
            u32::from(u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3fff),
        )),
        b"VP8L" if bytes.get(20) == Some(&0x2f) => Some((
            1 + u32::from(bytes[21]) + (u32::from(bytes[22] & 0x3f) << 8),
            1 + u32::from(bytes[22] >> 6)
                + (u32::from(bytes[23]) << 2)
                + (u32::from(bytes[24] & 0x0f) << 10),
        )),
        _ => None,
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut offset = 2_usize;
    while offset + 9 <= bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        offset += 2;
        if marker == 0xd8 || marker == 0xd9 || marker == 0x01 {
            continue;
        }
        let length = usize::from(u16::from_be_bytes(
            bytes.get(offset..offset + 2)?.try_into().ok()?,
        ));
        if length < 2 || offset + length > bytes.len() {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) && length >= 7 {
            let height = u16::from_be_bytes(bytes[offset + 3..offset + 5].try_into().ok()?);
            let width = u16::from_be_bytes(bytes[offset + 5..offset + 7].try_into().ok()?);
            return Some((width.into(), height.into()));
        }
        offset += length;
    }
    None
}

fn normalize_history(items: &mut Vec<Value>) {
    let calls = items
        .iter()
        .filter_map(call_descriptor)
        .map(|call| (call.call_id.clone(), call))
        .collect::<HashMap<_, _>>();
    let mut seen_outputs = HashSet::new();
    items.retain(|item| {
        let Some(output) = output_descriptor(item) else {
            return true;
        };
        let Some(call) = calls.get(output.call_id) else {
            return false;
        };
        if call.output_kind != output.kind {
            return false;
        }
        if call.allows_multiple_outputs() {
            return true;
        }
        seen_outputs.insert(output.call_id.to_string())
    });

    let present_outputs = items
        .iter()
        .filter_map(output_descriptor)
        .map(|output| output.call_id.to_string())
        .collect::<HashSet<_>>();
    let mut missing = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(call) = call_descriptor(item) else {
            continue;
        };
        if !present_outputs.contains(&call.call_id) {
            missing.push((index, synthetic_output(&call)));
        }
    }
    for (index, output) in missing.into_iter().rev() {
        items.insert(index + 1, output);
    }
}

fn history_is_normalized(items: &[Value]) -> bool {
    let calls = items
        .iter()
        .filter_map(call_descriptor)
        .map(|call| {
            let output_kind = call.output_kind;
            let allows_multiple_outputs = call.allows_multiple_outputs();
            (call.call_id, (output_kind, allows_multiple_outputs))
        })
        .collect::<HashMap<_, _>>();
    let mut present_outputs = HashSet::new();
    let mut seen_nonrepeat_outputs = HashSet::new();
    for item in items {
        let Some(output) = output_descriptor(item) else {
            continue;
        };
        let Some((output_kind, allows_multiple_outputs)) = calls.get(output.call_id) else {
            return false;
        };
        if *output_kind != output.kind {
            return false;
        }
        if *allows_multiple_outputs {
            present_outputs.insert(output.call_id);
        } else if !seen_nonrepeat_outputs.insert(output.call_id) {
            return false;
        } else {
            present_outputs.insert(output.call_id);
        }
    }
    calls
        .keys()
        .all(|call_id| present_outputs.contains(call_id.as_str()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallOutputKind {
    Function,
    Custom,
}

#[derive(Clone)]
struct CallDescriptor {
    item_id: Option<String>,
    call_id: String,
    name: Option<String>,
    output_kind: CallOutputKind,
}

impl CallDescriptor {
    fn allows_multiple_outputs(&self) -> bool {
        self.output_kind == CallOutputKind::Custom && self.name.as_deref() == Some("exec")
    }
}

fn call_descriptor(item: &Value) -> Option<CallDescriptor> {
    let item_type = item.get("type")?.as_str()?;
    if !matches!(
        item_type,
        "function_call" | "custom_tool_call" | "local_shell_call"
    ) {
        return None;
    }
    Some(CallDescriptor {
        item_id: item.get("id").and_then(Value::as_str).map(str::to_string),
        call_id: item.get("call_id")?.as_str()?.to_string(),
        name: item.get("name").and_then(Value::as_str).map(str::to_string),
        output_kind: if item_type == "custom_tool_call" {
            CallOutputKind::Custom
        } else {
            CallOutputKind::Function
        },
    })
}

struct OutputDescriptor<'a> {
    call_id: &'a str,
    kind: CallOutputKind,
}

fn output_descriptor(item: &Value) -> Option<OutputDescriptor<'_>> {
    let kind = match item.get("type")?.as_str()? {
        "function_call_output" => CallOutputKind::Function,
        "custom_tool_call_output" => CallOutputKind::Custom,
        _ => return None,
    };
    Some(OutputDescriptor {
        call_id: item.get("call_id")?.as_str()?,
        kind,
    })
}

fn synthetic_output(call: &CallDescriptor) -> Value {
    let id = call.item_id.as_deref().map(|item_id| {
        let prefix = if call.output_kind == CallOutputKind::Custom {
            "ctco"
        } else {
            "fco"
        };
        format!(
            "{prefix}_{}",
            Uuid::new_v5(
                &SYNTHETIC_OUTPUT_NAMESPACE,
                format!("{prefix}:{item_id}").as_bytes()
            )
        )
    });
    if call.output_kind == CallOutputKind::Custom {
        json!({
            "id": id,
            "type": "custom_tool_call_output",
            "call_id": call.call_id,
            "name": call.name,
            "output": "aborted",
        })
    } else {
        json!({
            "id": id,
            "type": "function_call_output",
            "call_id": call.call_id,
            "output": "aborted",
        })
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn escape_cdata(value: &str) -> String {
    value.replace("]]>", "]]]]><![CDATA[>")
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
