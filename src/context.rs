use crate::compaction::InitialContextInjection;
use crate::deepwork::DeepworkStage;
use crate::deepwork::HarnessProfile;
pub(crate) use crate::model::EFFECTIVE_CONTEXT_WINDOW;
use crate::model::ModelSelection;
use crate::rate_limits::RateLimitSnapshot;
use crate::rate_limits::fill_missing_rate_limit_fields;
use crate::repository;
use crate::rollout::AutoCompactWindow;
use crate::rollout::HistoryReplacement;
use crate::rollout::LoadedRollout;
use crate::rollout::Rollout;
use crate::rollout::SYNTHETIC_ABORT_OUTPUT;
use crate::rollout::SessionIdentity;
use crate::rollout::SessionTranscriptToolOutcome;
use crate::rollout::SessionTranscriptToolOutput;
use crate::rollout::ToolLifecycleJournal;
use crate::rollout::ToolRecovery;
use crate::rollout::TurnOutcome;
use crate::rollout::is_legacy_exec_notification;
use crate::service_tier::ServiceTier;
use crate::skills::SkillCatalog;
use crate::text::escape_xml;
use crate::text::escape_xml_text;
use crate::truncation::TruncationPolicy;
use crate::truncation::formatted_truncate_text;
use crate::truncation::formatted_truncate_text_with_policy;
use crate::usage::TokenUsage;
use anyhow::Context;
use anyhow::Result;
use serde::Serialize;
use serde::ser::SerializeMap;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

// Match Codex's default aggregate AGENTS.md source-byte budget. The rendered request item is
// bounded separately because JSON escaping can expand those source bytes substantially.
const MAX_REPOSITORY_INSTRUCTIONS_BYTES: usize = 32 * 1024;
const MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS: u64 = 10_000;
const MAX_CONTEXT_NOTICE_TEXT_TOKENS: usize = 9_900;
const RESIZED_IMAGE_BYTES_ESTIMATE: u64 = 7_373;
const ORIGINAL_IMAGE_MAX_PATCHES: u64 = 10_000;
const SYNTHETIC_OUTPUT_NAMESPACE: Uuid = Uuid::from_u128(0x90d38d3e_6a5b_4d52_bfe2_2f1e634bfac4);
const INTERRUPTED_GUIDANCE: &str = "The user interrupted the previous turn on purpose. Any command or tool that was running may have partially executed. Inspect the workspace before repeating an interrupted action.";
const CRASH_NOTICE: &str =
    "The previous bettercodex process ended before its active turn completed.";
const CRASH_GUIDANCE: &str = "The previous bettercodex process ended before its active turn completed. Any command or tool that was running may have partially executed. Inspect the workspace before continuing or repeating an action.";
const LEGACY_REPOSITORY_ONBOARDING_PREFIX: &str = "# Repository onboarding from AGENTS.md for ";
const LEGACY_REPOSITORY_CONTEXT_PREFIX: &str = "<repository_context>";
const LEGACY_SKILLS_PREFIX: &str = "<skills>";
const LEGACY_SKILL_CONTEXT_PREFIX: &str = "<skill>";
const SYSTEM_REMINDER_OPEN: &str = "<system-reminder>";
const SYSTEM_REMINDER_CLOSE: &str = "</system-reminder>";
const WORKSPACE_INSTRUCTIONS_INTRO: &str = "The following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.";
const AVAILABLE_SKILLS_PREFIX: &str = "<available_skills>";
const FILE_CONTEXT_PREFIX: &str = "<file_context>";
pub(crate) const USER_MESSAGE_KIND_FIELD: &str = "bettercodex_user_message_kind";
const OPERATOR_USER_MESSAGE_KIND: &str = "operator";
const CONTEXTUAL_USER_MESSAGE_KIND: &str = "context";
const REPOSITORY_USER_MESSAGE_KIND: &str = "repository";
const SKILL_USER_MESSAGE_KIND_PREFIX: &str = "skill:";
const DEEPWORK_RUNTIME_USER_MESSAGE_KIND: &str = "deepwork_runtime";

/// Serialize one history item exactly as bettercodex sends it to Responses.
///
/// Local provenance is persisted for reconstruction but never crosses the API
/// boundary. Legacy rollout IDs without a server-style prefix are likewise
/// retained on disk and omitted from requests.
pub(crate) struct ResponseItemForRequest<'a>(&'a Value);

impl<'a> ResponseItemForRequest<'a> {
    pub(crate) fn new(item: &'a Value) -> Self {
        Self(item)
    }
}

impl Serialize for ResponseItemForRequest<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Value::Object(item) = self.0 else {
            return self.0.serialize(serializer);
        };
        let omit_id = item.get("id").is_some_and(|id| match id {
            Value::String(id) => !response_item_id_is_prefixed(id),
            _ => true,
        });
        let omit_provenance = item.contains_key(USER_MESSAGE_KIND_FIELD);
        if !omit_id && !omit_provenance {
            return self.0.serialize(serializer);
        }

        let omitted = usize::from(omit_id).saturating_add(usize::from(omit_provenance));
        let mut serialized = serializer.serialize_map(Some(item.len().saturating_sub(omitted)))?;
        for (name, value) in item {
            if !(omit_id && name == "id") && name != USER_MESSAGE_KIND_FIELD {
                serialized.serialize_entry(name, value)?;
            }
        }
        serialized.end()
    }
}

pub(crate) fn response_item_id_is_prefixed(id: &str) -> bool {
    id.split_once('_')
        .is_some_and(|(prefix, suffix)| !prefix.is_empty() && !suffix.is_empty())
}

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ContextSnapshot {
    pub(crate) used_tokens: u64,
    pub(crate) context_window: u64,
    pub(crate) compact_at_tokens: u64,
    /// Whether `used_tokens` came from a backend usage signal rather than only local estimation.
    pub(crate) measured: bool,
    pub(crate) ask_user_question_enabled: bool,
    pub(crate) specialist_coordination_enabled: bool,
    pub(crate) sections: Vec<ContextSection>,
    pub(crate) total_usage: TokenUsage,
    pub(crate) rate_limits: Vec<RateLimitSnapshot>,
}

pub(crate) struct Conversation {
    history: Vec<Value>,
    history_lineage: Uuid,
    history_normalization: HistoryNormalization,
    context_metrics: ContextMetrics,
    usage: Option<TokenUsage>,
    total_usage: TokenUsage,
    rate_limits: BTreeMap<String, RateLimitSnapshot>,
    usage_history_estimate: Option<u64>,
    server_reasoning_included: bool,
    // A backend context rejection is a durable active-context floor, not fabricated usage.
    context_window_full: bool,
    harness_profile: HarnessProfile,
    ask_user_question_enabled: bool,
    specialist_coordination_enabled: bool,
    model_selection: ModelSelection,
    service_tier: ServiceTier,
    rollout: Rollout,
    world_state: WorldState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HistoryCursor {
    lineage: Uuid,
    len: usize,
}

#[derive(Default)]
pub(crate) struct RequestHistoryOmissions {
    items: Vec<(usize, Value)>,
}

impl RequestHistoryOmissions {
    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn restore_into(self, history: &mut Vec<Value>) {
        for (index, item) in self.items {
            history.insert(index, item);
        }
    }
}

/// A pending history append and the context accounting computed for those exact items.
pub(crate) struct ContextProjection {
    base: HistoryCursor,
    items: Vec<Value>,
    metrics: ContextMetrics,
    additional_tokens: u64,
    projected_tokens: u64,
}

impl ContextProjection {
    pub(crate) fn additional_tokens(&self) -> u64 {
        self.additional_tokens
    }

    pub(crate) fn projected_tokens(&self) -> u64 {
        self.projected_tokens
    }
}

#[derive(Debug, Default)]
pub(crate) struct ActiveTurnContext {
    // Keep one block per real user input, including empty blocks, so retained
    // user messages can be matched newest-to-newest after remote truncation.
    input_blocks: Vec<Vec<Value>>,
}

impl ActiveTurnContext {
    pub(crate) fn record_real_user_input(&mut self, context: Vec<Value>) {
        self.input_blocks.push(context);
    }

    fn preferred_world_state_insertion(&self, history: &[Value]) -> Option<usize> {
        if self.input_blocks.is_empty() {
            return None;
        }
        let user_indices = real_user_message_indices(history);
        let retained_inputs = user_indices.len().min(self.input_blocks.len());
        let latest_agent_message = history.iter().rposition(|item| {
            item.get("type").and_then(Value::as_str) == Some("agent_message")
                && is_instruction_boundary(item)
        });
        if retained_inputs > 0 {
            let latest_retained_input = user_indices[user_indices.len() - 1];
            return Some(latest_agent_message.map_or(latest_retained_input, |agent| {
                latest_retained_input.max(agent)
            }));
        }
        latest_agent_message
            .or_else(|| history.iter().rposition(is_initial_context_boundary))
            .or_else(|| history.iter().rposition(is_compaction_item))
    }

    fn insert_into(
        &self,
        history: &mut Vec<Value>,
        initial_context_injection: InitialContextInjection,
    ) {
        if !self.input_blocks.iter().any(|block| !block.is_empty()) {
            return;
        }
        if initial_context_injection == InitialContextInjection::AfterCompaction {
            history.extend(self.input_blocks.iter().flatten().cloned());
            return;
        }

        let user_indices = real_user_message_indices(history);
        let retained_inputs = user_indices.len().min(self.input_blocks.len());
        let first_retained_block = self.input_blocks.len() - retained_inputs;
        let fallback_insertion = if retained_inputs > 0 {
            user_indices[user_indices.len() - retained_inputs]
        } else {
            history
                .iter()
                .rposition(is_initial_context_boundary)
                .or_else(|| history.iter().rposition(is_compaction_item))
                .unwrap_or(history.len())
        };
        for (block, user_index) in self.input_blocks[first_retained_block..]
            .iter()
            .zip(user_indices[user_indices.len() - retained_inputs..].iter())
            .rev()
        {
            history.splice(*user_index..*user_index, block.iter().cloned());
        }
        // If remote retention dropped older current-turn user messages, keep
        // their still-active context before the oldest surviving input.
        for block in self.input_blocks[..first_retained_block].iter().rev() {
            history.splice(
                fallback_insertion..fallback_insertion,
                block.iter().cloned(),
            );
        }
    }
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
    instruction_source_paths: Vec<PathBuf>,
    skills_catalogue: Option<Value>,
    skills: SkillCatalog,
}

#[derive(Clone, Copy)]
enum WorldStateRefreshPlacement {
    BeforeTrailingInput(usize),
    Exact(usize),
    Append,
}

struct RepositoryContext {
    text: String,
    source_paths: Vec<PathBuf>,
}

struct InstructionCandidate {
    path: PathBuf,
    display_path: String,
}

impl std::ops::Deref for RepositoryContext {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}

/// Aggregate request accounting kept in lockstep with `Conversation::history`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ContextMetrics {
    estimated_tokens: u64,
    tokens: [u64; CONTEXT_KINDS.len()],
    items: [usize; CONTEXT_KINDS.len()],
    encrypted_reasoning_tokens: u64,
    encrypted_reasoning_before_last_instruction: u64,
}

/// Incremental index of the call/output invariants enforced before sampling.
///
/// History is append-only between uncommon rewrites. Keeping the small amount of state needed to
/// validate each suffix avoids rebuilding several full-history hash tables before every model
/// request. Unexpected or ambiguous suffixes deliberately fall back to canonical normalization.
#[derive(Clone, Default)]
struct HistoryNormalization {
    calls: HashMap<String, TrackedCall>,
    missing_outputs: usize,
    requires_rebuild: bool,
}

#[derive(Clone, Copy)]
struct TrackedCall {
    output_kind: CallOutputKind,
    allows_notifications: bool,
    has_output: bool,
}

impl Conversation {
    pub(crate) fn create_with_selection(cwd: &Path, selection: ModelSelection) -> Result<Self> {
        selection.validate()?;
        let world_state = WorldState::load(cwd)?;
        let rollout = Rollout::create_with_selection(cwd, &selection)?;
        Self::from_world_state(world_state, rollout, selection)
    }

    fn from_world_state(
        world_state: WorldState,
        mut rollout: Rollout,
        model_selection: ModelSelection,
    ) -> Result<Self> {
        let history = world_state.items();
        rollout.replace_history(&history, HistoryReplacement::Initial)?;
        let context_metrics = ContextMetrics::from_history(&history, &world_state);
        let history_normalization = HistoryNormalization::from_history(&history);
        Ok(Self {
            history,
            history_lineage: uuid::Uuid::new_v4(),
            history_normalization,
            context_metrics,
            usage: None,
            total_usage: TokenUsage::default(),
            rate_limits: BTreeMap::new(),
            usage_history_estimate: None,
            server_reasoning_included: false,
            context_window_full: false,
            harness_profile: HarnessProfile::Main,
            ask_user_question_enabled: false,
            specialist_coordination_enabled: false,
            model_selection,
            service_tier: ServiceTier::default(),
            rollout,
            world_state,
        })
    }

    pub(crate) fn resume(cwd: &Path, loaded: LoadedRollout) -> Result<Self> {
        let LoadedRollout {
            rollout,
            history,
            usage,
            total_usage,
            usage_history_estimate,
            server_reasoning_included,
            context_window_full,
            model_selection,
            service_tier,
            unfinished_turn,
            unfinished_turn_has_activity,
            unfinished_turn_has_recovery_notice,
            unfinished_turn_recovered,
            tool_recoveries,
            crash_recovery_requires_inspection,
            ..
        } = loaded;
        let world_state = WorldState::load(cwd)?;
        let context_metrics = ContextMetrics::from_history(&history, &world_state);
        let history_normalization = HistoryNormalization::from_history(&history);
        let mut conversation = Self {
            history,
            history_lineage: uuid::Uuid::new_v4(),
            history_normalization,
            context_metrics,
            usage,
            total_usage,
            rate_limits: BTreeMap::new(),
            usage_history_estimate,
            server_reasoning_included,
            context_window_full,
            harness_profile: HarnessProfile::Main,
            ask_user_question_enabled: false,
            specialist_coordination_enabled: false,
            model_selection,
            service_tier,
            rollout,
            world_state,
        };
        match unfinished_turn {
            Some(turn_id) if unfinished_turn_recovered => {
                // The complete recovery checkpoint already contains normalized outputs,
                // transcript outcomes, the notice, and refreshed world state. Closing the turn is
                // the only recovery write still required after a second process death.
                conversation
                    .rollout
                    .finish_turn(&turn_id, TurnOutcome::Interrupted)?;
                conversation.refresh_world_state()?;
            }
            Some(turn_id) if unfinished_turn_has_activity => {
                conversation.recover_unfinished_turn(
                    &turn_id,
                    &tool_recoveries,
                    crash_recovery_requires_inspection,
                    unfinished_turn_has_recovery_notice,
                )?;
                conversation
                    .rollout
                    .finish_turn(&turn_id, TurnOutcome::Interrupted)?;
            }
            Some(turn_id) => {
                // Startup can normalize old history before any new input or tool work exists. A
                // crash there is housekeeping, not a model-visible interrupted turn.
                conversation.normalize()?;
                conversation.refresh_world_state_at_history_end()?;
                conversation
                    .rollout
                    .finish_turn(&turn_id, TurnOutcome::Interrupted)?;
            }
            None => {
                // Finished legacy and refactor-era turns can still contain malformed call/output
                // pairs. Repair them durably during resume so transcript replay and the next model
                // request observe the same settled history.
                conversation.normalize()?;
                conversation.refresh_world_state()?;
            }
        }
        // Run durable repair and world-state refresh against the saved representation first. Bound
        // legacy media only in the live history so a context-refresh checkpoint cannot replace the
        // rollout's original image payload with a request-specific resize. New images are already
        // prepared before insertion.
        crate::image_preparation::prepare_history_images(&mut conversation.history);
        conversation.history_normalization =
            HistoryNormalization::from_history(&conversation.history);
        conversation.context_metrics =
            ContextMetrics::from_history(&conversation.history, &conversation.world_state);
        Ok(conversation)
    }

    pub(crate) fn fork(&self, mut rollout: Rollout) -> Result<Self> {
        rollout.replace_history(&self.history, HistoryReplacement::Initial)?;
        if let (Some(usage), Some(history_estimate)) = (&self.usage, self.usage_history_estimate) {
            rollout.record_usage(usage, history_estimate, self.server_reasoning_included)?;
        }
        if self.context_window_full {
            rollout.record_context_window_exceeded()?;
        }
        if self.service_tier != ServiceTier::default() {
            rollout.record_service_tier(self.service_tier)?;
        }
        Ok(Self {
            history: self.history.clone(),
            history_lineage: uuid::Uuid::new_v4(),
            history_normalization: self.history_normalization.clone(),
            context_metrics: self.context_metrics.clone(),
            usage: self.usage.clone(),
            total_usage: self.total_usage.clone(),
            rate_limits: self.rate_limits.clone(),
            usage_history_estimate: self.usage_history_estimate,
            server_reasoning_included: self.server_reasoning_included,
            context_window_full: self.context_window_full,
            harness_profile: HarnessProfile::Main,
            ask_user_question_enabled: false,
            specialist_coordination_enabled: false,
            model_selection: self.model_selection.clone(),
            service_tier: self.service_tier,
            rollout,
            world_state: self.world_state.clone(),
        })
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.rollout.identity().session_id
    }

    pub(crate) fn identity(&self) -> &SessionIdentity {
        self.rollout.identity()
    }

    pub(crate) fn initial_auto_compact_window(&self) -> AutoCompactWindow {
        self.rollout.initial_auto_compact_window()
    }

    pub(crate) fn model_selection(&self) -> &ModelSelection {
        &self.model_selection
    }

    pub(crate) fn set_model_selection(&mut self, selection: ModelSelection) -> Result<()> {
        selection.validate()?;
        if self.model_selection == selection {
            return Ok(());
        }
        self.rollout.record_model_selection(&selection)?;
        self.model_selection = selection;
        Ok(())
    }

    pub(crate) fn service_tier(&self) -> ServiceTier {
        self.service_tier
    }

    pub(crate) fn instruction_source_paths(&self) -> &[PathBuf] {
        &self.world_state.instruction_source_paths
    }

    pub(crate) fn prior_usage_for_fork(&self) -> Option<TokenUsage> {
        let prior = self.usage.as_ref().map_or_else(
            || self.total_usage.clone(),
            |last| self.total_usage.saturating_sub(last),
        );
        (prior != TokenUsage::default()).then_some(prior)
    }

    pub(crate) fn set_service_tier(&mut self, service_tier: ServiceTier) -> Result<()> {
        if self.service_tier == service_tier {
            return Ok(());
        }
        self.rollout.record_service_tier(service_tier)?;
        self.service_tier = service_tier;
        Ok(())
    }

    pub(crate) fn set_harness_profile(&mut self, profile: HarnessProfile) -> Result<()> {
        if self.harness_profile == profile {
            return Ok(());
        }
        let world_state = self.world_state.clone().for_harness_profile(profile);
        self.replace_world_state(world_state)?;
        self.harness_profile = profile;
        Ok(())
    }

    pub(crate) fn set_ask_user_question_enabled(&mut self, enabled: bool) {
        self.ask_user_question_enabled = enabled;
    }

    pub(crate) fn set_specialist_coordination_enabled(&mut self, enabled: bool) {
        self.specialist_coordination_enabled = enabled;
    }

    pub(crate) fn start_turn(&mut self, turn_id: &str) -> Result<()> {
        self.rollout.start_turn(turn_id)
    }

    pub(crate) fn finish_turn(&mut self, turn_id: &str, outcome: TurnOutcome) -> Result<()> {
        self.rollout.finish_turn(turn_id, outcome)
    }

    pub(crate) fn snapshot_transcript(
        &mut self,
        items: Vec<crate::rollout::SessionTranscriptItem>,
    ) -> Result<()> {
        self.rollout.snapshot_transcript(items)
    }

    pub(crate) fn append_transcript(
        &mut self,
        items: Vec<crate::rollout::SessionTranscriptItem>,
    ) -> Result<()> {
        self.rollout.append_transcript(items)
    }

    pub(crate) fn record_tool_outcomes(
        &mut self,
        outcomes: Vec<SessionTranscriptToolOutcome>,
    ) -> Result<()> {
        self.rollout.record_tool_outcomes(outcomes)
    }

    pub(crate) fn tool_lifecycle_journal(&self) -> ToolLifecycleJournal {
        self.rollout.tool_lifecycle_journal()
    }

    pub(crate) fn extend(&mut self, items: impl IntoIterator<Item = Value>) -> Result<()> {
        let items = items.into_iter().collect::<Vec<_>>();
        if items.is_empty() {
            return Ok(());
        }
        self.rollout.append_history(&items)?;
        self.commit_history_append(items);
        Ok(())
    }

    pub(crate) fn extend_tool_results(
        &mut self,
        items: Vec<Value>,
        outcomes: Vec<SessionTranscriptToolOutcome>,
    ) -> Result<()> {
        if items.is_empty() {
            return self.rollout.record_tool_outcomes(outcomes);
        }
        self.rollout.append_tool_results(&items, outcomes)?;
        self.commit_history_append(items);
        Ok(())
    }

    fn commit_history_append(&mut self, items: Vec<Value>) {
        self.context_metrics.extend(&items, &self.world_state);
        self.history_normalization.record_append(&items);
        self.history.extend(items);
    }

    pub(crate) fn project_append(&self, items: Vec<Value>) -> ContextProjection {
        // A real user message advances the instruction boundary used by Codex's
        // X-Reasoning-Included fallback. Project the complete accounting state so prior
        // encrypted reasoning cannot appear only after input admission.
        let mut metrics = self.context_metrics.clone();
        let additional_tokens = metrics.extend(&items, &self.world_state);
        let projected_tokens = self
            .context_tokens_with_metrics(&metrics)
            .unwrap_or_else(|| self.estimated_context_tokens(&metrics));
        ContextProjection {
            base: self.history_cursor(),
            items,
            metrics,
            additional_tokens,
            projected_tokens,
        }
    }

    pub(crate) fn append_projected(&mut self, projection: ContextProjection) -> Result<()> {
        let ContextProjection {
            base,
            items,
            metrics,
            ..
        } = projection;
        if self.history_cursor() != base {
            anyhow::bail!("conversation changed after its context append was projected");
        }
        if items.is_empty() {
            return Ok(());
        }
        self.rollout.append_history(&items)?;
        self.context_metrics = metrics;
        self.history_normalization.record_append(&items);
        self.history.extend(items);
        Ok(())
    }

    pub(crate) fn replace_compacted(
        &mut self,
        mut history: Vec<Value>,
        initial_context_injection: InitialContextInjection,
        active_turn_context: &ActiveTurnContext,
        response_usage: Option<&TokenUsage>,
        rate_limits: &[RateLimitSnapshot],
        auto_compact_window: AutoCompactWindow,
    ) -> Result<()> {
        let preferred_insertion = match initial_context_injection {
            InitialContextInjection::AfterCompaction => None,
            InitialContextInjection::BeforeLastUserMessage => {
                active_turn_context.preferred_world_state_insertion(&history)
            }
        };
        self.world_state.insert_missing_into(
            &mut history,
            initial_context_injection,
            preferred_insertion,
        );
        active_turn_context.insert_into(&mut history, initial_context_injection);
        let context_metrics = ContextMetrics::from_history(&history, &self.world_state);
        // Match Codex's remote-v2 replacement fidelity exactly: retained images stay attached even
        // when their estimated cost leaves little immediate headroom. The opaque compaction item
        // already summarizes discarded history; silently removing retained inputs here loses
        // information that upstream keeps.
        self.rollout
            .replace_compacted_history(&history, response_usage, auto_compact_window)?;
        if let Some(response_usage) = response_usage {
            self.total_usage.add_assign(response_usage);
        }
        self.update_rate_limits(rate_limits.iter().cloned());
        self.history_normalization = HistoryNormalization::from_history(&history);
        self.context_metrics = context_metrics;
        self.history = history;
        self.history_lineage = uuid::Uuid::new_v4();
        self.usage = None;
        self.usage_history_estimate = None;
        self.server_reasoning_included = false;
        self.context_window_full = false;
        Ok(())
    }

    pub(crate) fn history_cursor(&self) -> HistoryCursor {
        HistoryCursor {
            lineage: self.history_lineage,
            len: self.history.len(),
        }
    }

    pub(crate) fn take_history_for_sampling(
        &mut self,
    ) -> (Vec<Value>, HistoryCursor, RequestHistoryOmissions) {
        let cursor = self.history_cursor();
        let mut history = std::mem::take(&mut self.history);
        let omissions = omit_deepwork_context_for_request(
            &mut history,
            self.ask_user_question_enabled || self.specialist_coordination_enabled,
        );
        (history, cursor, omissions)
    }

    pub(crate) fn history_for_model_request(&self) -> std::borrow::Cow<'_, [Value]> {
        let deepwork_active =
            self.ask_user_question_enabled || self.specialist_coordination_enabled;
        if !has_deepwork_context_to_omit(&self.history, deepwork_active) {
            return std::borrow::Cow::Borrowed(&self.history);
        }
        let mut history = self.history.clone();
        let _ = omit_deepwork_context_for_request(&mut history, deepwork_active);
        std::borrow::Cow::Owned(history)
    }

    pub(crate) fn restore_history_after_sampling(
        &mut self,
        mut history: Vec<Value>,
        cursor: HistoryCursor,
        omissions: RequestHistoryOmissions,
    ) -> Result<()> {
        if self.history_lineage != cursor.lineage
            || history.len().saturating_add(omissions.items.len()) != cursor.len
        {
            anyhow::bail!("conversation changed while its sampling history was in flight");
        }
        omissions.restore_into(&mut history);
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

    pub(crate) fn reload_world_state_for_active_turn(
        &mut self,
        cwd: &Path,
        active_turn_context: &ActiveTurnContext,
    ) -> Result<()> {
        let world_state = WorldState::load(cwd)?.for_harness_profile(self.harness_profile);
        let placement = active_turn_context
            .preferred_world_state_insertion(&self.history)
            .map(|insertion| world_state_placement_before_input(&self.history, insertion))
            .unwrap_or_else(|| world_state_refresh_placement(&self.history));
        self.replace_world_state_at(world_state, placement)
    }

    pub(crate) fn reload_skills(&mut self, cwd: &Path) -> Result<()> {
        if matches!(self.harness_profile, HarnessProfile::Specialist(_)) {
            return Ok(());
        }
        let skills = SkillCatalog::load(cwd);
        let mut world_state = self.world_state.clone();
        world_state.skills_catalogue = skills.catalogue_message(EFFECTIVE_CONTEXT_WINDOW);
        world_state.skills = skills;
        self.replace_world_state(world_state)
    }

    pub(crate) fn record_uninstalled_response(
        &mut self,
        usage: Option<TokenUsage>,
        rate_limits: Vec<RateLimitSnapshot>,
    ) -> Result<()> {
        if let Some(usage) = usage {
            self.rollout.record_total_usage(&usage)?;
            self.total_usage.add_assign(&usage);
        }
        self.update_rate_limits(rate_limits);
        Ok(())
    }

    pub(crate) fn mark_context_window_full(&mut self) -> Result<()> {
        self.rollout.record_context_window_exceeded()?;
        self.context_window_full = true;
        Ok(())
    }

    pub(crate) fn record_usage(
        &mut self,
        usage: Option<TokenUsage>,
        server_reasoning_included: bool,
        rate_limits: Vec<RateLimitSnapshot>,
    ) -> Result<()> {
        self.update_rate_limits(rate_limits);
        let Some(usage) = usage else {
            return Ok(());
        };
        let history_estimate = self.context_metrics.estimated_tokens;
        self.rollout
            .record_usage(&usage, history_estimate, server_reasoning_included)?;
        self.total_usage.add_assign(&usage);
        self.usage = Some(usage);
        self.usage_history_estimate = Some(history_estimate);
        self.server_reasoning_included = server_reasoning_included;
        self.context_window_full = false;
        Ok(())
    }

    fn update_rate_limits(&mut self, rate_limits: impl IntoIterator<Item = RateLimitSnapshot>) {
        for mut snapshot in rate_limits {
            let limit_id = snapshot.limit_id.clone();
            if let Some(previous) = self.rate_limits.get(&limit_id) {
                fill_missing_rate_limit_fields(&mut snapshot, previous);
            }
            self.rate_limits.insert(limit_id, snapshot);
        }
    }

    pub(crate) fn context_tokens(&self) -> Option<u64> {
        self.context_tokens_with_metrics(&self.context_metrics)
    }

    fn context_tokens_with_metrics(&self, metrics: &ContextMetrics) -> Option<u64> {
        let history_estimate = metrics.estimated_tokens;
        let measured = match (&self.usage, self.usage_history_estimate) {
            (Some(usage), Some(baseline)) => {
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
            _ => None,
        };
        if self.context_window_full {
            Some(
                measured
                    .unwrap_or_default()
                    .max(self.model_selection.effective_context_window()),
            )
        } else {
            measured
        }
    }

    pub(crate) fn context_snapshot(&self) -> ContextSnapshot {
        let [tools_tokens, system_prompt_tokens] = crate::api::estimated_harness_tokens_for(
            self.harness_profile,
            self.ask_user_question_enabled,
            self.specialist_coordination_enabled,
        );
        let mut tokens = self.context_metrics.tokens;
        let mut items = self.context_metrics.items;
        record_context_estimate(
            &mut tokens,
            &mut items,
            ContextKind::ToolCatalogue,
            tools_tokens,
        );
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
            context_window: self.model_selection.effective_context_window(),
            compact_at_tokens: self.model_selection.auto_compact_token_limit(),
            measured: measured_total.is_some(),
            ask_user_question_enabled: self.ask_user_question_enabled,
            specialist_coordination_enabled: self.specialist_coordination_enabled,
            sections,
            total_usage: self.total_usage.clone(),
            rate_limits: self.rate_limits.values().cloned().collect(),
        }
    }

    pub(crate) fn active_context_tokens(&self) -> u64 {
        self.context_tokens()
            .unwrap_or_else(|| self.estimated_context_tokens(&self.context_metrics))
    }

    fn estimated_context_tokens(&self, metrics: &ContextMetrics) -> u64 {
        let [_, system_prompt_tokens] = crate::api::estimated_harness_tokens_for(
            self.harness_profile,
            self.ask_user_question_enabled,
            self.specialist_coordination_enabled,
        );
        // Match Codex's fallback/recomputed active-context estimate: base instructions plus
        // history. Tool declarations remain visible in the UI breakdown, but reserving them again
        // here would trigger compaction earlier than upstream before the next server usage sample.
        metrics
            .estimated_tokens
            .saturating_add(system_prompt_tokens)
    }

    pub(crate) fn needs_compaction(&self) -> bool {
        self.active_context_tokens() >= self.model_selection.auto_compact_token_limit()
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
        self.normalize_with_recoveries(&HashMap::new())
    }

    fn normalize_with_recoveries(
        &mut self,
        recoveries: &HashMap<String, ToolRecovery>,
    ) -> Result<bool> {
        if self.history_normalization.is_normalized() {
            return Ok(false);
        }
        let mut normalized = self.history.clone();
        let outcomes = normalize_history_with_recoveries(&mut normalized, recoveries);
        self.rollout.replace_history_with_outcomes(
            &normalized,
            HistoryReplacement::Normalization,
            outcomes,
        )?;
        self.history_normalization = HistoryNormalization::from_history(&normalized);
        self.context_metrics = ContextMetrics::from_history(&normalized, &self.world_state);
        self.history = normalized;
        self.history_lineage = uuid::Uuid::new_v4();
        Ok(true)
    }

    fn recover_unfinished_turn(
        &mut self,
        turn_id: &str,
        recoveries: &HashMap<String, ToolRecovery>,
        requires_inspection: bool,
        has_recovery_notice: bool,
    ) -> Result<()> {
        let mut recovered = self.history.clone();
        let outcomes = normalize_history_with_recoveries(&mut recovered, recoveries);
        if !has_recovery_notice {
            recovered.push(context_notice(
                "turn_aborted",
                if requires_inspection {
                    CRASH_GUIDANCE
                } else {
                    CRASH_NOTICE
                },
            ));
        }
        let placement = world_state_refresh_placement(&recovered);
        if let Some(refreshed) =
            refreshed_world_state_history(&recovered, &self.world_state, placement)
        {
            recovered = refreshed;
        }

        // One complete replacement is the recovery commit point. If the process dies while this
        // line is being appended, tail repair restores the pre-recovery state; if it dies after the
        // line, replay sees the checkpoint and only needs to close the turn.
        self.rollout.replace_recovered_history(
            &recovered,
            outcomes,
            turn_id,
            requires_inspection,
        )?;
        self.history_normalization = HistoryNormalization::from_history(&recovered);
        self.context_metrics = ContextMetrics::from_history(&recovered, &self.world_state);
        self.history = recovered;
        self.history_lineage = uuid::Uuid::new_v4();
        Ok(())
    }

    fn append_context_notice(&mut self, tag: &str, guidance: &str) -> Result<()> {
        self.extend([context_notice(tag, guidance)])
    }

    fn refresh_world_state(&mut self) -> Result<()> {
        self.replace_world_state(self.world_state.clone())
    }

    fn refresh_world_state_at_history_end(&mut self) -> Result<()> {
        let insertion = self
            .history
            .iter()
            .rposition(|item| !is_world_state_refresh_item(item))
            .map_or(0, |index| index + 1);
        self.replace_world_state_at(
            self.world_state.clone(),
            WorldStateRefreshPlacement::Exact(insertion),
        )
    }

    fn replace_world_state(&mut self, world_state: WorldState) -> Result<()> {
        let placement = world_state_refresh_placement(&self.history);
        self.replace_world_state_at(world_state, placement)
    }

    fn replace_world_state_at(
        &mut self,
        world_state: WorldState,
        placement: WorldStateRefreshPlacement,
    ) -> Result<()> {
        let Some(refreshed) = refreshed_world_state_history(&self.history, &world_state, placement)
        else {
            self.world_state = world_state;
            return Ok(());
        };
        self.rollout
            .replace_history(&refreshed, HistoryReplacement::ContextRefresh)?;
        self.history_normalization = HistoryNormalization::from_history(&refreshed);
        self.context_metrics = ContextMetrics::from_history(&refreshed, &world_state);
        self.history = refreshed;
        self.history_lineage = uuid::Uuid::new_v4();
        self.world_state = world_state;
        Ok(())
    }
}

fn context_notice(tag: &str, guidance: &str) -> Value {
    let guidance = escape_xml_text(guidance);
    let mut guidance_budget = MAX_CONTEXT_NOTICE_TEXT_TOKENS;
    loop {
        let guidance = formatted_truncate_text(&guidance, guidance_budget);
        let item = message("user", format!("<{tag}>\n{guidance}\n</{tag}>"));
        let item_tokens = estimate_value_tokens(&item);
        if item_tokens <= MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS || guidance_budget == 0 {
            return item;
        }
        guidance_budget = reduced_budget(
            guidance_budget,
            MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS,
            item_tokens,
        );
    }
}

fn refreshed_world_state_history(
    history: &[Value],
    world_state: &WorldState,
    placement: WorldStateRefreshPlacement,
) -> Option<Vec<Value>> {
    let current = world_state.items();
    let saved = history
        .iter()
        .enumerate()
        .filter(|(_, item)| is_generated_world_state_message(item))
        .collect::<Vec<_>>();
    let correctly_placed = match placement {
        WorldStateRefreshPlacement::BeforeTrailingInput(insertion) => {
            saved.iter().all(|(index, _)| *index < insertion)
        }
        WorldStateRefreshPlacement::Exact(insertion) => saved
            .iter()
            .enumerate()
            .all(|(offset, (index, _))| insertion.checked_add(offset) == Some(*index)),
        WorldStateRefreshPlacement::Append => true,
    };
    let already_current = !history.iter().any(is_legacy_harness_prefix_item)
        && saved.len() == current.len()
        && saved
            .iter()
            .zip(&current)
            .all(|((_, existing), expected)| same_model_visible_message(existing, expected))
        && correctly_placed;
    if already_current {
        return None;
    }

    let mut refreshed = history
        .iter()
        .filter(|item| !is_world_state_refresh_item(item))
        .cloned()
        .collect::<Vec<_>>();
    let insertion = match placement {
        WorldStateRefreshPlacement::BeforeTrailingInput(insertion)
        | WorldStateRefreshPlacement::Exact(insertion) => history[..insertion]
            .iter()
            .filter(|item| !is_world_state_refresh_item(item))
            .count(),
        WorldStateRefreshPlacement::Append => refreshed.len(),
    };
    refreshed.splice(insertion..insertion, current);
    Some(refreshed)
}

impl WorldState {
    fn load(cwd: &Path) -> Result<Self> {
        let skills = SkillCatalog::load(cwd);
        let skills_catalogue = skills.catalogue_message(EFFECTIVE_CONTEXT_WINDOW);
        let repository_context = repository_context(cwd)?;
        let instruction_source_paths = repository_context
            .as_ref()
            .map(|context| context.source_paths.clone())
            .unwrap_or_default();
        let repository_context = repository_context.map(|context| {
            let mut item = message("user", context.text);
            mark_user_message_kind(&mut item, REPOSITORY_USER_MESSAGE_KIND);
            item
        });
        Ok(Self {
            environment: message("developer", environment_context(cwd)),
            repository_context,
            instruction_source_paths,
            skills_catalogue,
            skills,
        })
    }

    fn for_harness_profile(mut self, profile: HarnessProfile) -> Self {
        if matches!(profile, HarnessProfile::Specialist(_)) {
            self.skills_catalogue = None;
            self.skills = SkillCatalog::default();
        }
        self
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
        preferred_insertion: Option<usize>,
    ) {
        let missing = self.missing_from(history);
        if missing.is_empty() {
            return;
        }
        match initial_context_injection {
            InitialContextInjection::AfterCompaction => history.extend(missing),
            InitialContextInjection::BeforeLastUserMessage => {
                let insertion = preferred_insertion.unwrap_or_else(|| {
                    history
                        .iter()
                        .rposition(is_initial_context_boundary)
                        .or_else(|| history.iter().rposition(is_compaction_item))
                        .unwrap_or(history.len())
                });
                history.splice(insertion..insertion, missing);
            }
        }
    }

    fn missing_from(&self, history: &[Value]) -> Vec<Value> {
        self.items()
            .into_iter()
            .filter(|expected| {
                !history.iter().any(|existing| {
                    !is_explicit_operator_user_message(existing)
                        && same_model_visible_message(existing, expected)
                })
            })
            .collect()
    }
}

fn real_user_message_indices(history: &[Value]) -> Vec<usize> {
    history
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            (is_user_message(item) && !is_contextual_user_message(item)).then_some(index)
        })
        .collect()
}

impl ContextMetrics {
    fn from_history(history: &[Value], world_state: &WorldState) -> Self {
        let mut metrics = Self::default();
        metrics.extend(history, world_state);
        metrics
    }

    fn extend(&mut self, history: &[Value], world_state: &WorldState) -> u64 {
        let mut additional_tokens = 0_u64;
        for item in history {
            if is_reasoning_instruction_boundary(item) {
                self.encrypted_reasoning_before_last_instruction = self.encrypted_reasoning_tokens;
            }
            let explicit_operator = is_explicit_operator_user_message(item);
            let kind = if same_model_visible_message(item, &world_state.environment) {
                ContextKind::Environment
            } else if !explicit_operator
                && world_state
                    .repository_context
                    .as_ref()
                    .is_some_and(|context| same_model_visible_message(item, context))
            {
                ContextKind::RepositoryInstructions
            } else if !explicit_operator
                && world_state
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
            additional_tokens = additional_tokens.saturating_add(estimated);
        }
        additional_tokens
    }
}

fn same_model_visible_message(left: &Value, right: &Value) -> bool {
    ["type", "role", "content"]
        .into_iter()
        .all(|field| left.get(field) == right.get(field))
}

fn context_kind(item: &Value) -> ContextKind {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => match item.get("role").and_then(Value::as_str) {
            Some("user") if is_user_shell_command_message(item) => ContextKind::ToolActivity,
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

pub(crate) fn mark_operator_user_message(item: &mut Value) {
    mark_user_message_kind(item, OPERATOR_USER_MESSAGE_KIND);
}

pub(crate) fn mark_contextual_user_message(item: &mut Value) {
    mark_user_message_kind(item, CONTEXTUAL_USER_MESSAGE_KIND);
}

pub(crate) fn mark_skill_context_message(item: &mut Value, name: &str) {
    mark_user_message_kind(item, &format!("{SKILL_USER_MESSAGE_KIND_PREFIX}{name}"));
}

fn mark_deepwork_runtime_message(item: &mut Value) {
    mark_user_message_kind(item, DEEPWORK_RUNTIME_USER_MESSAGE_KIND);
}

pub(crate) fn injected_skill_name(item: &Value) -> Option<&str> {
    is_user_message(item)
        .then(|| user_message_kind(item))
        .flatten()
        .and_then(|kind| kind.strip_prefix(SKILL_USER_MESSAGE_KIND_PREFIX))
}

fn mark_user_message_kind(item: &mut Value, kind: &str) {
    debug_assert!(is_user_message(item));
    if let Some(item) = item.as_object_mut() {
        item.insert(
            USER_MESSAGE_KIND_FIELD.to_string(),
            Value::String(kind.to_string()),
        );
    }
}

fn user_message_kind(item: &Value) -> Option<&str> {
    item.get(USER_MESSAGE_KIND_FIELD).and_then(Value::as_str)
}

fn is_explicit_operator_user_message(item: &Value) -> bool {
    is_user_message(item) && user_message_kind(item) == Some(OPERATOR_USER_MESSAGE_KIND)
}

pub(crate) fn is_contextual_user_message(item: &Value) -> bool {
    is_user_message(item)
        && is_contextual_user_text_with_kind(
            message_text(item).unwrap_or_default(),
            user_message_kind(item),
        )
}

pub(crate) fn is_contextual_user_text_with_kind(text: &str, kind: Option<&str>) -> bool {
    match kind {
        Some(OPERATOR_USER_MESSAGE_KIND) => false,
        Some(CONTEXTUAL_USER_MESSAGE_KIND)
        | Some(REPOSITORY_USER_MESSAGE_KIND)
        | Some(DEEPWORK_RUNTIME_USER_MESSAGE_KIND) => true,
        Some(kind) if kind.starts_with(SKILL_USER_MESSAGE_KIND_PREFIX) => true,
        _ => is_contextual_user_text(text),
    }
}

pub(crate) fn is_contextual_user_text(text: &str) -> bool {
    let text = text.trim_start();
    is_repository_context_text(text)
        || is_complete_context_wrapper(text, AVAILABLE_SKILLS_PREFIX, "</available_skills>")
        || is_complete_context_wrapper(text, "<environment_context>", "</environment_context>")
        || is_complete_context_wrapper(text, "<skill_context>", "</skill_context>")
        || is_complete_context_wrapper(text, "<deepwork_runtime>", "</deepwork_runtime>")
        || is_complete_context_wrapper(text, FILE_CONTEXT_PREFIX, "</file_context>")
        || is_complete_context_wrapper(text, LEGACY_SKILL_CONTEXT_PREFIX, "</skill>")
        || is_user_shell_command_text(text)
        || is_complete_context_wrapper(text, "<turn_aborted>", "</turn_aborted>")
        || is_complete_context_wrapper(text, "<response_interrupted>", "</response_interrupted>")
}

fn is_user_shell_command_text(text: &str) -> bool {
    is_complete_context_wrapper(text, "<user_shell_command>", "</user_shell_command>")
}

pub(crate) fn is_user_shell_command_message(item: &Value) -> bool {
    is_contextual_user_message(item)
        && message_text(item)
            .map(str::trim_start)
            .is_some_and(is_user_shell_command_text)
}

fn is_complete_context_wrapper(text: &str, opening: &str, closing: &str) -> bool {
    text.starts_with(opening) && text.trim_end().ends_with(closing)
}

fn is_repository_context_text(text: &str) -> bool {
    let current = is_complete_context_wrapper(text, SYSTEM_REMINDER_OPEN, SYSTEM_REMINDER_CLOSE)
        && text
            .strip_prefix(SYSTEM_REMINDER_OPEN)
            .and_then(|text| text.strip_prefix('\n'))
            .is_some_and(|text| text.starts_with(WORKSPACE_INSTRUCTIONS_INTRO));
    let legacy_xml = is_complete_context_wrapper(
        text,
        LEGACY_REPOSITORY_CONTEXT_PREFIX,
        "</repository_context>",
    );
    let legacy_onboarding = text.starts_with(LEGACY_REPOSITORY_ONBOARDING_PREFIX)
        && text.trim_end().ends_with("# End repository onboarding");
    current || legacy_xml || legacy_onboarding
}

fn is_initial_context_boundary(item: &Value) -> bool {
    is_instruction_boundary(item) || is_assistant_commentary_message(item)
}

fn is_reasoning_instruction_boundary(item: &Value) -> bool {
    // Match Codex's token-accounting boundary rather than its initial-context placement boundary:
    // every structured agent message closes a reasoning segment, including FINAL_ANSWER messages
    // that are intentionally skipped when deciding where fresh world state should be inserted.
    (is_user_message(item) && !is_contextual_user_message(item))
        || item.get("type").and_then(Value::as_str) == Some("agent_message")
}

fn is_instruction_boundary(item: &Value) -> bool {
    (is_user_message(item) && !is_contextual_user_message(item))
        || (item.get("type").and_then(Value::as_str) == Some("agent_message")
            && !item
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.starts_with("Message Type: FINAL_ANSWER\n")))
}

pub(crate) fn is_assistant_commentary_message(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("assistant")
        && item.get("phase").and_then(Value::as_str) == Some("commentary")
}

fn is_generated_world_state_message(item: &Value) -> bool {
    if is_explicit_operator_user_message(item) {
        return false;
    }
    let role = item.get("role").and_then(Value::as_str);
    if role == Some("user") && user_message_kind(item) == Some(REPOSITORY_USER_MESSAGE_KIND) {
        return true;
    }
    let Some(text) = message_text(item).map(str::trim_start) else {
        return false;
    };
    (role == Some("developer")
        && (text.starts_with("<environment_context>") || text.starts_with(LEGACY_SKILLS_PREFIX)))
        || (role == Some("user")
            && (is_repository_context_text(text)
                || (text.starts_with(AVAILABLE_SKILLS_PREFIX)
                    && text.trim_end().ends_with("</available_skills>"))))
}

// Before tools and instructions moved to top-level request fields, compacted rollouts could
// persist their in-band harness prefix. Current bettercodex has no client-authored developer
// messages, so every non-world-state developer message is part of that obsolete prefix.
fn is_legacy_harness_prefix_item(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("additional_tools")
        || (item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("role").and_then(Value::as_str) == Some("developer")
            && !is_generated_world_state_message(item))
}

fn is_world_state_refresh_item(item: &Value) -> bool {
    is_generated_world_state_message(item) || is_legacy_harness_prefix_item(item)
}

fn world_state_refresh_placement(history: &[Value]) -> WorldStateRefreshPlacement {
    let mut cursor = history.len();
    let mut trailing_world_state = false;
    while cursor > 0 && is_world_state_refresh_item(&history[cursor - 1]) {
        trailing_world_state |= is_generated_world_state_message(&history[cursor - 1]);
        cursor -= 1;
    }

    let user_index = if cursor > 0 && is_turn_recovery_notice(&history[cursor - 1]) {
        let recovery_index = cursor - 1;
        let mut context_start = recovery_index;
        while context_start > 0 && is_world_state_refresh_item(&history[context_start - 1]) {
            context_start -= 1;
        }
        if context_start < recovery_index {
            return WorldStateRefreshPlacement::Exact(context_start);
        }

        cursor = recovery_index;
        let mut earliest_user = None;
        let mut persisted_context_start = None;
        while cursor > 0 {
            let item = &history[cursor - 1];
            if is_terminal_assistant_message(item)
                || is_turn_abort_notice(item)
                || (is_compaction_item(item) && trailing_world_state)
            {
                break;
            }
            if is_world_state_refresh_item(item) {
                let mut start = cursor - 1;
                while start > 0 && is_world_state_refresh_item(&history[start - 1]) {
                    start -= 1;
                }
                persisted_context_start = Some(start);
                break;
            }
            if is_user_message(item) && !is_contextual_user_message(item) {
                earliest_user = Some(cursor - 1);
            }
            cursor -= 1;
        }
        let Some(user_index) = earliest_user else {
            return persisted_context_start.map_or(
                WorldStateRefreshPlacement::Append,
                WorldStateRefreshPlacement::Exact,
            );
        };
        user_index
    } else if cursor > 0 && is_compaction_item(&history[cursor - 1]) && !trailing_world_state {
        // Mid-turn compaction keeps its opaque item last. Refresh the active turn's world state
        // above the latest retained real user or non-final agent message, or immediately above the
        // compaction item when remote retention dropped every instruction boundary, so a
        // tool-driven AGENTS.md change cannot demote the opaque continuation token from the
        // model-trained terminal position.
        let compaction_index = cursor - 1;
        let Some(input_index) = history[..compaction_index]
            .iter()
            .rposition(is_instruction_boundary)
        else {
            let mut insertion = compaction_index;
            while insertion > 0
                && (is_world_state_refresh_item(&history[insertion - 1])
                    || is_turn_input_context_message(&history[insertion - 1]))
            {
                insertion -= 1;
            }
            return WorldStateRefreshPlacement::Exact(insertion);
        };
        input_index
    } else {
        let Some(user_index) = cursor.checked_sub(1) else {
            return WorldStateRefreshPlacement::Append;
        };
        let item = &history[user_index];
        if is_turn_input_context_message(item) {
            return world_state_placement_before_input(history, cursor);
        }
        if !is_user_message(item) || is_contextual_user_message(item) {
            return WorldStateRefreshPlacement::Append;
        }
        user_index
    };

    world_state_placement_before_input(history, user_index)
}

fn world_state_placement_before_input(
    history: &[Value],
    input_index: usize,
) -> WorldStateRefreshPlacement {
    let mut start = input_index;
    let mut scan = start;
    while scan > 0 {
        let preceding = &history[scan - 1];
        if is_world_state_refresh_item(preceding) {
            scan -= 1;
        } else if is_turn_input_context_message(preceding) {
            scan -= 1;
            start = scan;
        } else {
            break;
        }
    }
    WorldStateRefreshPlacement::BeforeTrailingInput(start)
}

fn is_turn_input_context_message(item: &Value) -> bool {
    if !is_contextual_user_message(item) {
        return false;
    }
    let Some(text) = message_text(item).map(str::trim_start) else {
        return false;
    };
    is_complete_context_wrapper(text, "<skill_context>", "</skill_context>")
        || is_complete_context_wrapper(text, "<deepwork_runtime>", "</deepwork_runtime>")
        || is_complete_context_wrapper(text, FILE_CONTEXT_PREFIX, "</file_context>")
        || is_complete_context_wrapper(text, LEGACY_SKILL_CONTEXT_PREFIX, "</skill>")
        || is_user_shell_command_text(text)
}

fn is_turn_recovery_notice(item: &Value) -> bool {
    is_turn_abort_notice(item) || is_response_interrupted_notice(item)
}

pub(crate) fn is_turn_abort_notice(item: &Value) -> bool {
    is_context_notice(item, "<turn_aborted>", "</turn_aborted>")
}

fn is_response_interrupted_notice(item: &Value) -> bool {
    is_context_notice(item, "<response_interrupted>", "</response_interrupted>")
}

fn is_context_notice(item: &Value, opening: &str, closing: &str) -> bool {
    is_contextual_user_message(item)
        && message_text(item)
            .map(str::trim_start)
            .is_some_and(|text| is_complete_context_wrapper(text, opening, closing))
}

fn is_terminal_assistant_message(item: &Value) -> bool {
    if item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("assistant")
    {
        return !is_assistant_commentary_message(item);
    }
    item.get("type").and_then(Value::as_str) == Some("agent_message")
        && item
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.starts_with("Message Type: FINAL_ANSWER\n"))
}

fn message_text(item: &Value) -> Option<&str> {
    item.get("content")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|content| content.get("text").and_then(Value::as_str))
}

fn is_deepwork_skill_context(item: &Value) -> bool {
    if injected_skill_name(item) == Some("deepwork") {
        return true;
    }
    if !is_contextual_user_message(item) {
        return false;
    }
    let Some(text) = message_text(item).map(str::trim) else {
        return false;
    };
    is_complete_context_wrapper(text, "<skill_context>", "</skill_context>")
        && text
            .lines()
            .any(|line| line.trim() == "<name>deepwork</name>")
}

fn is_deepwork_runtime_context(item: &Value) -> bool {
    if !is_contextual_user_message(item) {
        return false;
    }
    let Some(text) = message_text(item).map(str::trim) else {
        return false;
    };
    user_message_kind(item) == Some(DEEPWORK_RUNTIME_USER_MESSAGE_KIND)
        || is_complete_context_wrapper(text, "<deepwork_runtime>", "</deepwork_runtime>")
}

fn has_deepwork_context_to_omit(history: &[Value], deepwork_active: bool) -> bool {
    if !deepwork_active {
        return history
            .iter()
            .any(|item| is_deepwork_skill_context(item) || is_deepwork_runtime_context(item));
    }
    history
        .iter()
        .filter(|item| is_deepwork_skill_context(item))
        .nth(1)
        .is_some()
        || history
            .iter()
            .filter(|item| is_deepwork_runtime_context(item))
            .nth(1)
            .is_some()
}

fn omit_deepwork_context_for_request(
    history: &mut Vec<Value>,
    deepwork_active: bool,
) -> RequestHistoryOmissions {
    let latest_skill = deepwork_active
        .then(|| history.iter().rposition(is_deepwork_skill_context))
        .flatten();
    let latest_runtime = deepwork_active
        .then(|| history.iter().rposition(is_deepwork_runtime_context))
        .flatten();
    let mut retained = Vec::with_capacity(history.len());
    let mut omitted = Vec::new();
    for (index, item) in history.drain(..).enumerate() {
        let omit = if deepwork_active {
            (is_deepwork_skill_context(&item) && Some(index) != latest_skill)
                || (is_deepwork_runtime_context(&item) && Some(index) != latest_runtime)
        } else {
            is_deepwork_skill_context(&item) || is_deepwork_runtime_context(&item)
        };
        if omit {
            omitted.push((index, item));
        } else {
            retained.push(item);
        }
    }
    *history = retained;
    RequestHistoryOmissions { items: omitted }
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

pub(crate) fn deepwork_runtime_context(
    run_index: u64,
    workspace: &str,
    stage: DeepworkStage,
) -> Value {
    let workspace = escape_xml_text(workspace);
    let stage = stage.as_str();
    let mut item = message(
        "user",
        format!(
            "<deepwork_runtime>\nrun_index: {run_index}\nworkspace: {workspace}\nstage: {stage}\nThe runtime reserved or recovered this numbered workspace for pipeline artifacts before repository preflight.\n</deepwork_runtime>"
        ),
    );
    mark_deepwork_runtime_message(&mut item);
    item
}

pub(crate) fn user_shell_command_context(
    command: &str,
    output: &std::result::Result<Value, String>,
    policy: TruncationPolicy,
) -> String {
    let command = escape_xml_text(command);
    let result = match output {
        Ok(output) => {
            let stdout = output.get("stdout").and_then(Value::as_str);
            let stderr = output.get("stderr").and_then(Value::as_str);
            let exit_code = output.get("exit_code").and_then(Value::as_i64);
            match (stdout, stderr, exit_code) {
                (Some(stdout), Some(stderr), Some(exit_code)) => {
                    format!("Exit code: {exit_code}\nStdout:\n{stdout}\nStderr:\n{stderr}")
                }
                _ => format!("Result:\n{output}"),
            }
        }
        Err(error) => format!("Execution error:\n{error}"),
    };
    let result = escape_xml_text(&result);
    let (maximum_cost, maximum_content_budget) = match policy {
        TruncationPolicy::Bytes(bytes) => {
            let maximum_bytes =
                usize::try_from(MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS.saturating_mul(4))
                    .unwrap_or(usize::MAX);
            let maximum_bytes = bytes.min(maximum_bytes);
            (
                u64::try_from(maximum_bytes).unwrap_or(u64::MAX),
                maximum_bytes,
            )
        }
        TruncationPolicy::Tokens(tokens) => (
            u64::try_from(tokens)
                .unwrap_or(u64::MAX)
                .min(MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS),
            tokens
                .min(usize::try_from(MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS).unwrap_or(usize::MAX)),
        ),
    };
    let mut content_budget = maximum_content_budget;
    loop {
        let command_budget = content_budget.div_ceil(10);
        let result_budget = content_budget.saturating_sub(command_budget);
        let command_policy = truncation_policy_with_limit(policy, command_budget);
        let result_policy = truncation_policy_with_limit(policy, result_budget);
        let command = formatted_truncate_text_with_policy(&command, command_policy);
        let result = formatted_truncate_text_with_policy(&result, result_policy);
        let context = format!(
            "<user_shell_command>\n<command>\n{command}\n</command>\n<result>\n{result}\n</result>\n</user_shell_command>"
        );
        let item = message("user", context.clone());
        let actual_cost = match policy {
            TruncationPolicy::Bytes(_) => estimate_value_model_visible_bytes(&item),
            TruncationPolicy::Tokens(_) => estimate_value_tokens(&item),
        };
        if actual_cost <= maximum_cost || content_budget == 0 {
            return context;
        }
        content_budget = reduced_budget(content_budget, maximum_cost, actual_cost);
    }
}

fn truncation_policy_with_limit(policy: TruncationPolicy, limit: usize) -> TruncationPolicy {
    match policy {
        TruncationPolicy::Bytes(_) => TruncationPolicy::Bytes(limit),
        TruncationPolicy::Tokens(_) => TruncationPolicy::Tokens(limit),
    }
}

fn environment_context(cwd: &Path) -> String {
    let shell = crate::shell_command::shell_detect::default_user_shell();
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "unknown".to_string());
    format!(
        "<environment_context>\n  <cwd>{}</cwd>\n  <shell>{}</shell>\n  <current_date>{date}</current_date>\n  <timezone>{timezone}</timezone>\n</environment_context>",
        escape_xml(&cwd.display().to_string()),
        escape_xml(shell.name()),
    )
}

fn repository_context(cwd: &Path) -> Result<Option<RepositoryContext>> {
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("failed to resolve working directory {}", cwd.display()))?;
    let mut candidates = Vec::new();
    if let Some(codex_home) = crate::paths::codex_home()
        && let Some(path) = first_instruction_file(&codex_home)?
    {
        candidates.push(InstructionCandidate {
            display_path: home_relative_display_path(&path),
            path,
        });
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
            candidates.push(InstructionCandidate {
                display_path: project_relative_display_path(&path, &project_root),
                path,
            });
        }
    }

    let mut seen = HashSet::new();
    candidates.retain(|candidate| {
        let canonical = candidate
            .path
            .canonicalize()
            .unwrap_or_else(|_| candidate.path.clone());
        seen.insert(canonical)
    });

    let mut source_budget = MAX_REPOSITORY_INSTRUCTIONS_BYTES;
    loop {
        let Some(context) = repository_context_with_budget(&candidates, source_budget)? else {
            return Ok(None);
        };
        let item_tokens = estimate_value_tokens(&message("user", context.text.clone()));
        if item_tokens <= MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS || source_budget == 0 {
            return Ok(Some(context));
        }
        source_budget = reduced_budget(
            source_budget,
            MAX_MODEL_VISIBLE_CONTEXT_ITEM_TOKENS,
            item_tokens,
        );
    }
}

fn repository_context_with_budget(
    candidates: &[InstructionCandidate],
    source_budget: usize,
) -> Result<Option<RepositoryContext>> {
    let mut remaining = source_budget;
    let mut sections = Vec::new();
    let mut source_paths = Vec::new();
    for candidate in candidates {
        if remaining == 0 {
            break;
        }
        let (bytes, truncated) = read_instruction_file(&candidate.path, remaining)?;
        let mut content = String::from_utf8_lossy(&bytes).into_owned();
        if truncated {
            content.push_str("\n[AGENTS.md truncated]");
        }
        if !content.trim().is_empty() {
            sections.push(format!(
                "Instructions from: {}\n\n{}",
                candidate.display_path, content,
            ));
            source_paths.push(candidate.path.clone());
            remaining = remaining.saturating_sub(bytes.len());
        }
    }

    if sections.is_empty() {
        return Ok(None);
    }
    let body = format!(
        "{WORKSPACE_INSTRUCTIONS_INTRO}\n\n{}",
        sections.join("\n\n"),
    );
    let body = if body.contains(SYSTEM_REMINDER_CLOSE) {
        body.replace(SYSTEM_REMINDER_CLOSE, "<\\/system-reminder>")
    } else {
        body
    };
    Ok(Some(RepositoryContext {
        text: format!("{SYSTEM_REMINDER_OPEN}\n{body}\n{SYSTEM_REMINDER_CLOSE}"),
        source_paths,
    }))
}

fn project_relative_display_path(path: &Path, project_root: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn home_relative_display_path(path: &Path) -> String {
    let Some(home) = crate::paths::home_dir() else {
        return path.display().to_string();
    };
    let Ok(relative) = path.strip_prefix(home) else {
        return path.display().to_string();
    };
    Path::new("~").join(relative).display().to_string()
}

fn reduced_budget(current: usize, maximum_cost: u64, actual_cost: u64) -> usize {
    debug_assert!(actual_cost > maximum_cost);
    if current == 0 || actual_cost == 0 {
        return 0;
    }
    let scaled = (current as u128)
        .saturating_mul(u128::from(maximum_cost))
        .checked_div(u128::from(actual_cost))
        .unwrap_or_default();
    usize::try_from(scaled)
        .unwrap_or(usize::MAX)
        .min(current - 1)
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
    let read_limit = limit.saturating_add(1);
    let mut bytes = Vec::with_capacity(read_limit);
    file.take(u64::try_from(read_limit).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read instructions from {}", path.display()))?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok((bytes, truncated))
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
    estimate_value_model_visible_bytes(value).div_ceil(4)
}

fn estimate_value_model_visible_bytes(value: &Value) -> u64 {
    let item_type = value.get("type").and_then(Value::as_str);
    if matches!(
        item_type,
        Some("reasoning" | "compaction" | "compaction_summary" | "context_compaction")
    ) && let Some(encrypted) = value.get("encrypted_content").and_then(Value::as_str)
    {
        // Codex's history estimator treats opaque reasoning/compaction payloads as a dedicated
        // server-visible token blob and does not add their JSON wrapper bytes.
        return estimate_reasoning_bytes(encrypted.len());
    }
    let mut serialized_size = SerializedSize::default();
    let mut bytes =
        serde_json::to_writer(&mut serialized_size, &ResponseItemForRequest::new(value))
            .map(|()| serialized_size.bytes)
            .unwrap_or_default();
    visit_model_content(
        value,
        &mut |content| match content.get("type").and_then(Value::as_str) {
            Some("input_image") => {
                let Some(image_url) = content.get("image_url").and_then(Value::as_str) else {
                    return;
                };
                let Some(payload) = base64_image_data_url_payload(image_url) else {
                    return;
                };
                let replacement = estimated_image_payload_replacement_bytes(
                    image_url,
                    content.get("detail").and_then(Value::as_str),
                );
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
    bytes
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

fn base64_image_data_url_payload(url: &str) -> Option<&str> {
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
        .get(.."image/".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
        .then_some(())?;
    has_base64_marker.then_some(payload)
}

fn estimated_image_payload_replacement_bytes(image_url: &str, detail: Option<&str>) -> u64 {
    if detail == Some("original") {
        estimate_full_resolution_image_bytes(image_url).unwrap_or(RESIZED_IMAGE_BYTES_ESTIMATE)
    } else {
        RESIZED_IMAGE_BYTES_ESTIMATE
    }
}

fn estimate_full_resolution_image_bytes(image_url: &str) -> Option<u64> {
    let encoded = base64_image_data_url_payload(image_url)?;
    let decoder = base64::read::DecoderReader::new(
        encoded.as_bytes(),
        &base64::engine::general_purpose::STANDARD,
    );
    read_image_dimensions(decoder).map(|(width, height)| {
        u64::from(width.div_ceil(32))
            .saturating_mul(u64::from(height.div_ceil(32)))
            .min(ORIGINAL_IMAGE_MAX_PATCHES)
            .saturating_mul(4)
    })
}

fn read_image_dimensions(mut reader: impl Read) -> Option<(u32, u32)> {
    // PNG, GIF, and WebP dimensions live in their small fixed headers. A JPEG's SOF marker can
    // follow variable-length metadata, so grow geometrically until the existing parser finds it.
    // Valid common inputs stop after the first read instead of decoding and allocating the full
    // inline image merely to inspect its dimensions.
    const INITIAL_HEADER_BYTES: usize = 64;
    let mut bytes = Vec::with_capacity(INITIAL_HEADER_BYTES);
    let mut target = INITIAL_HEADER_BYTES;
    loop {
        let missing = target.saturating_sub(bytes.len());
        let read = reader
            .by_ref()
            .take(u64::try_from(missing).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .ok()?;
        if let Some(dimensions) = image_dimensions(&bytes) {
            return Some(dimensions);
        }
        if read < missing || !could_be_supported_image(&bytes) {
            return None;
        }
        target = target.checked_mul(2)?;
    }
}

fn could_be_supported_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..]))
        || bytes.starts_with(&[0xff, 0xd8])
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
    while offset < bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes.get(offset)?;
        offset += 1;
        if marker == 0x00 || marker == 0x01 || matches!(marker, 0xd0..=0xd9) {
            continue;
        }
        let length = usize::from(u16::from_be_bytes(
            bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
        ));
        let segment_end = offset.checked_add(length)?;
        if length < 2 || segment_end > bytes.len() {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) && length >= 7 {
            let height = u16::from_be_bytes(bytes[offset + 3..offset + 5].try_into().ok()?);
            let width = u16::from_be_bytes(bytes[offset + 5..offset + 7].try_into().ok()?);
            return Some((width.into(), height.into()));
        }
        offset = segment_end;
    }
    None
}

impl HistoryNormalization {
    fn from_history(items: &[Value]) -> Self {
        let mut normalization = Self::default();
        let mut latest_call_indices = HashMap::new();
        for (index, item) in items.iter().enumerate() {
            let Some(call) = call_tracking_descriptor(item) else {
                continue;
            };
            latest_call_indices.insert(call.call_id.to_string(), index);
            normalization.calls.insert(
                call.call_id.to_string(),
                TrackedCall {
                    output_kind: call.output_kind,
                    allows_notifications: call.allows_notifications,
                    has_output: false,
                },
            );
        }
        normalization.missing_outputs = normalization.calls.len();
        for (index, item) in items.iter().enumerate() {
            if let Some(output) = output_descriptor(item) {
                if latest_call_indices
                    .get(output.call_id)
                    .is_some_and(|call_index| index <= *call_index)
                {
                    normalization.requires_rebuild = true;
                    break;
                }
                normalization.record_output(output);
            }
        }
        normalization
    }

    fn record_append(&mut self, items: &[Value]) {
        if self.requires_rebuild {
            return;
        }
        for item in items {
            if let Some(call) = call_tracking_descriptor(item) {
                let tracked = TrackedCall {
                    output_kind: call.output_kind,
                    allows_notifications: call.allows_notifications,
                    has_output: false,
                };
                match self.calls.entry(call.call_id.to_string()) {
                    Entry::Vacant(entry) => {
                        entry.insert(tracked);
                        self.missing_outputs = self.missing_outputs.saturating_add(1);
                    }
                    Entry::Occupied(_) => {
                        // A prior output cannot complete a later call that reused the same ID.
                        // Rebuild against item order before the next request.
                        self.requires_rebuild = true;
                        return;
                    }
                }
            } else if let Some(output) = output_descriptor(item) {
                self.record_output(output);
                if self.requires_rebuild {
                    return;
                }
            }
        }
    }

    fn record_output(&mut self, output: OutputDescriptor<'_>) {
        let Some(call) = self.calls.get_mut(output.call_id) else {
            self.requires_rebuild = true;
            return;
        };
        if call.output_kind != output.kind
            || (output.is_notification && !call.allows_notifications)
            || (!output.is_notification && call.has_output)
        {
            self.requires_rebuild = true;
            return;
        }
        if !output.is_notification {
            self.missing_outputs = self.missing_outputs.saturating_sub(1);
            call.has_output = true;
        }
    }

    fn is_normalized(&self) -> bool {
        !self.requires_rebuild && self.missing_outputs == 0
    }
}

fn normalize_history_with_recoveries(
    items: &mut Vec<Value>,
    recoveries: &HashMap<String, ToolRecovery>,
) -> Vec<SessionTranscriptToolOutcome> {
    let canonical = canonical_call_outputs(items);
    let mut original_index = 0_usize;
    items.retain(|item| {
        let index = original_index;
        original_index = original_index.saturating_add(1);
        let Some(output) = output_descriptor(item) else {
            return true;
        };
        let Some(call) = canonical.calls.get(output.call_id) else {
            return false;
        };
        if index <= call.index || call.call.output_kind != output.kind {
            return false;
        }
        if output.is_notification {
            return call.call.allows_notifications();
        }
        if recoveries.contains_key(output.call_id) {
            // A remaining lifecycle with an apparently completed ID means an older output shadows
            // a later interrupted call. Remove that stale association and synthesize one
            // conservative output after the latest call below.
            return false;
        }
        canonical.output_indices.get(output.call_id) == Some(&index)
    });

    let present_outputs = items
        .iter()
        .filter_map(output_descriptor)
        .filter(|output| !output.is_notification)
        .map(|output| output.call_id.to_string())
        .collect::<HashSet<_>>();
    let last_notification_indices = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let output = output_descriptor(item)?;
            output
                .is_notification
                .then(|| (output.call_id.to_string(), index))
        })
        .collect::<HashMap<_, _>>();
    let mut missing = Vec::new();
    let mut outcomes = Vec::new();
    let mut synthesized_calls = HashSet::new();
    // Work newest-to-oldest so a repeated call ID gets one output at its latest occurrence. Such
    // IDs receive only conservative recovery evidence, never evidence borrowed from one copy.
    for (index, item) in items.iter().enumerate().rev() {
        let Some(call) = call_descriptor(item) else {
            continue;
        };
        if present_outputs.contains(&call.call_id)
            || !synthesized_calls.insert(call.call_id.clone())
        {
            continue;
        }
        let (output, transcript_output, file_change) = recoveries.get(&call.call_id).map_or_else(
            || {
                (
                    Value::String(SYNTHETIC_ABORT_OUTPUT.to_string()),
                    SessionTranscriptToolOutput::Error(SYNTHETIC_ABORT_OUTPUT.to_string()),
                    None,
                )
            },
            |recovery| {
                (
                    recovery.output.clone(),
                    recovery.transcript_output.clone(),
                    recovery.file_change.clone(),
                )
            },
        );
        let insertion = last_notification_indices
            .get(&call.call_id)
            .copied()
            .unwrap_or(index)
            .max(index);
        missing.push((insertion, synthetic_output_with_body(&call, output)));
        outcomes.push(SessionTranscriptToolOutcome {
            call_id: call.call_id,
            output: Some(transcript_output),
            error: None,
            file_change,
        });
    }
    missing.sort_unstable_by_key(|item| std::cmp::Reverse(item.0));
    for (index, output) in missing {
        items.insert(index + 1, output);
    }
    outcomes
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallOutputKind {
    Function,
    Custom,
}

struct CallTrackingDescriptor<'a> {
    call_id: &'a str,
    output_kind: CallOutputKind,
    allows_notifications: bool,
}

fn call_tracking_descriptor(item: &Value) -> Option<CallTrackingDescriptor<'_>> {
    let item_type = item.get("type")?.as_str()?;
    if !matches!(
        item_type,
        "function_call" | "custom_tool_call" | "local_shell_call"
    ) {
        return None;
    }
    let output_kind = if item_type == "custom_tool_call" {
        CallOutputKind::Custom
    } else {
        CallOutputKind::Function
    };
    Some(CallTrackingDescriptor {
        call_id: item.get("call_id")?.as_str()?,
        output_kind,
        allows_notifications: output_kind == CallOutputKind::Custom
            && item.get("name").and_then(Value::as_str) == Some("exec"),
    })
}

#[derive(Clone)]
struct CallDescriptor {
    item_id: Option<String>,
    call_id: String,
    name: Option<String>,
    output_kind: CallOutputKind,
}

struct IndexedCallDescriptor {
    index: usize,
    call: CallDescriptor,
}

struct CanonicalCallOutputs {
    calls: HashMap<String, IndexedCallDescriptor>,
    output_indices: HashMap<String, usize>,
}

impl CallDescriptor {
    fn allows_notifications(&self) -> bool {
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
    is_notification: bool,
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
        is_notification: kind == CallOutputKind::Custom && is_legacy_exec_notification(item),
    })
}

fn canonical_call_outputs(items: &[Value]) -> CanonicalCallOutputs {
    let calls = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let call = call_descriptor(item)?;
            Some((call.call_id.clone(), IndexedCallDescriptor { index, call }))
        })
        .collect::<HashMap<_, _>>();
    let mut output_indices = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        let Some(output) = output_descriptor(item).filter(|output| !output.is_notification) else {
            continue;
        };
        let Some(call) = calls.get(output.call_id) else {
            continue;
        };
        if index > call.index && output.kind == call.call.output_kind {
            // Preserve the first matching completion, as ordinary append processing does, while
            // discarding any later duplicate records deterministically.
            output_indices
                .entry(output.call_id.to_string())
                .or_insert(index);
        }
    }
    CanonicalCallOutputs {
        calls,
        output_indices,
    }
}

pub(crate) fn missing_call_output_ids(items: &[Value]) -> HashSet<String> {
    let canonical = canonical_call_outputs(items);
    canonical
        .calls
        .into_keys()
        .filter(|call_id| !canonical.output_indices.contains_key(call_id))
        .collect()
}

pub(crate) fn completed_function_call_ids(items: &[Value]) -> HashSet<String> {
    let canonical = canonical_call_outputs(items);
    canonical
        .output_indices
        .into_keys()
        .filter(|call_id| {
            canonical
                .calls
                .get(call_id)
                .is_some_and(|call| call.call.output_kind == CallOutputKind::Function)
        })
        .collect()
}

pub(crate) fn canonical_synthetic_abort_call_ids(items: &[Value]) -> HashSet<String> {
    canonical_call_outputs(items)
        .output_indices
        .into_iter()
        .filter_map(|(call_id, index)| {
            (items[index].get("output").and_then(Value::as_str) == Some(SYNTHETIC_ABORT_OUTPUT))
                .then_some(call_id)
        })
        .collect()
}

fn synthetic_output_with_body(call: &CallDescriptor, output: Value) -> Value {
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
            "output": output,
        })
    } else {
        json!({
            "id": id,
            "type": "function_call_output",
            "call_id": call.call_id,
            "output": output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn deepwork_skill_context(marked: bool) -> Value {
        let mut item = message(
            "user",
            "<skill_context>\n<name>deepwork</name>\n<instructions>test</instructions>\n</skill_context>"
                .to_string(),
        );
        if marked {
            mark_skill_context_message(&mut item, "deepwork");
        }
        item
    }

    #[test]
    fn inactive_requests_omit_deepwork_context_without_rewriting_history() {
        let mut quoted = message(
            "user",
            "Document `<deepwork_runtime>` and `$deepwork` literally.".to_string(),
        );
        mark_operator_user_message(&mut quoted);
        let history = vec![
            quoted,
            deepwork_skill_context(false),
            deepwork_runtime_context(3, ".deepwork/3", DeepworkStage::Worker),
            message("assistant", "historical result".to_string()),
        ];
        let original = history.clone();
        let mut request = history;

        let omissions = omit_deepwork_context_for_request(&mut request, false);

        assert_eq!(omissions.items.len(), 2);
        assert_eq!(request.len(), 2);
        assert!(request.iter().all(|item| {
            !is_deepwork_skill_context(item) && !is_deepwork_runtime_context(item)
        }));

        omissions.restore_into(&mut request);
        assert_eq!(request, original);
    }

    #[test]
    fn active_requests_keep_only_the_latest_skill_and_runtime_context() {
        let old_skill = deepwork_skill_context(false);
        let current_skill = deepwork_skill_context(true);
        let old_runtime = deepwork_runtime_context(3, ".deepwork/3", DeepworkStage::Interview);
        let current_runtime = deepwork_runtime_context(3, ".deepwork/3", DeepworkStage::Reviewer);
        let mut history = vec![
            old_skill,
            old_runtime,
            message("assistant", "continued".to_string()),
            current_skill.clone(),
            current_runtime.clone(),
        ];

        let omissions = omit_deepwork_context_for_request(&mut history, true);

        assert_eq!(omissions.items.len(), 2);
        assert!(history.contains(&current_skill));
        assert!(history.contains(&current_runtime));
        assert_eq!(
            history
                .iter()
                .filter(|item| is_deepwork_skill_context(item))
                .count(),
            1
        );
        assert_eq!(
            history
                .iter()
                .filter(|item| is_deepwork_runtime_context(item))
                .count(),
            1
        );
    }

    #[test]
    fn world_state_uses_the_latest_retained_user_or_agent_boundary() {
        let first_user = message("user", "start this".to_string());
        let latest_user = message("user", "steer this".to_string());
        let agent = json!({
            "type": "agent_message",
            "author": "/root",
            "recipient": "/root/worker",
            "content": [{"type": "input_text", "text": "Message Type: TASK\nPayload:\nwork"}],
        });
        let summary = json!({
            "type": "compaction",
            "encrypted_content": "opaque",
        });
        let mut active = ActiveTurnContext::default();
        active.record_real_user_input(Vec::new());
        active.record_real_user_input(Vec::new());

        assert_eq!(
            active.preferred_world_state_insertion(&[
                first_user.clone(),
                latest_user.clone(),
                summary.clone(),
            ]),
            Some(1)
        );
        assert_eq!(
            active.preferred_world_state_insertion(&[first_user, latest_user, agent, summary]),
            Some(2)
        );
    }

    fn synthetic_png_data_url(width: u32, height: u32) -> String {
        let mut bytes = vec![0_u8; 24];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        format!("data:image/png;base64,{encoded}")
    }

    #[test]
    fn opaque_reasoning_estimates_exclude_json_wrapper_bytes_like_upstream() {
        let encrypted = "x".repeat(4_000);
        let expected_bytes = estimate_reasoning_bytes(encrypted.len());
        for item_type in [
            "reasoning",
            "compaction",
            "compaction_summary",
            "context_compaction",
        ] {
            let item = json!({
                "type": item_type,
                "id": "opaque_1",
                "encrypted_content": encrypted,
                "internal_chat_message_metadata_passthrough": {"trace": "ignored"},
            });
            assert_eq!(estimate_value_model_visible_bytes(&item), expected_bytes);
            assert_eq!(estimate_value_tokens(&item), expected_bytes.div_ceil(4));
        }
    }

    #[test]
    fn final_agent_messages_bound_omitted_reasoning_accounting_like_upstream() {
        let world_state = WorldState {
            environment: message("developer", "environment".to_string()),
            repository_context: None,
            instruction_source_paths: Vec::new(),
            skills_catalogue: None,
            skills: SkillCatalog::default(),
        };
        let before_boundary = json!({
            "type": "reasoning",
            "encrypted_content": "x".repeat(4_000),
        });
        let final_agent = json!({
            "type": "agent_message",
            "author": "/root/worker",
            "recipient": "/root",
            "content": [{
                "type": "input_text",
                "text": "Message Type: FINAL_ANSWER\nPayload:\ndone",
            }],
        });
        let after_boundary = json!({
            "type": "reasoning",
            "encrypted_content": "y".repeat(5_000),
        });
        let metrics = ContextMetrics::from_history(
            &[
                before_boundary.clone(),
                final_agent.clone(),
                after_boundary.clone(),
            ],
            &world_state,
        );

        assert!(!is_instruction_boundary(&final_agent));
        assert!(is_reasoning_instruction_boundary(&final_agent));
        assert_eq!(
            metrics.encrypted_reasoning_before_last_instruction,
            estimate_value_tokens(&before_boundary)
        );
        assert_eq!(
            metrics.encrypted_reasoning_tokens,
            estimate_value_tokens(&before_boundary)
                .saturating_add(estimate_value_tokens(&after_boundary))
        );
    }

    #[test]
    fn image_estimates_match_upstream_detail_policy_and_original_patch_cap() {
        let oversized = synthetic_png_data_url(6_400, 6_400);

        assert_eq!(
            estimated_image_payload_replacement_bytes(&oversized, None),
            RESIZED_IMAGE_BYTES_ESTIMATE
        );
        assert_eq!(
            estimated_image_payload_replacement_bytes(&oversized, Some("auto")),
            RESIZED_IMAGE_BYTES_ESTIMATE
        );
        assert_eq!(
            estimated_image_payload_replacement_bytes(&oversized, Some("high")),
            RESIZED_IMAGE_BYTES_ESTIMATE
        );
        assert_eq!(
            estimated_image_payload_replacement_bytes(&oversized, Some("original")),
            ORIGINAL_IMAGE_MAX_PATCHES.saturating_mul(4)
        );
        assert_eq!(
            estimated_image_payload_replacement_bytes(
                &synthetic_png_data_url(32, 32),
                Some("original")
            ),
            4
        );
    }

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn create(name: &str) -> Result<Self> {
            let directory =
                std::env::temp_dir().join(format!("bettercodex-{name}-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&directory)?;
            Ok(Self(directory))
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn post_compaction_world_state_refresh_keeps_a_newer_agent_boundary() {
        let history = vec![
            message("user", "delegate this".to_string()),
            json!({
                "type": "agent_message",
                "author": "/root",
                "recipient": "/root/worker",
                "content": [{"type": "input_text", "text": "Message Type: TASK\nPayload:\nwork"}],
            }),
            json!({
                "type": "compaction",
                "encrypted_content": "opaque",
            }),
        ];

        assert!(matches!(
            world_state_refresh_placement(&history),
            WorldStateRefreshPlacement::BeforeTrailingInput(1)
        ));
    }

    #[test]
    fn fallback_active_context_estimate_excludes_tool_declarations_like_upstream() -> Result<()> {
        let root = TemporaryDirectory::create("fallback-context-estimate")?;
        let cwd = root.0.join("repo");
        std::fs::create_dir_all(&cwd)?;
        let selection = ModelSelection::default();
        let rollout = Rollout::create_in_with_selection(&root.0, &cwd, &selection)?;
        let world_state = WorldState::load(&cwd)?;
        let conversation = Conversation::from_world_state(world_state, rollout, selection)?;
        let [tool_tokens, instruction_tokens] =
            crate::api::estimated_harness_tokens_for(HarnessProfile::Main, false, false);

        assert!(tool_tokens > 0);
        assert_eq!(
            conversation.active_context_tokens(),
            conversation
                .context_metrics
                .estimated_tokens
                .saturating_add(instruction_tokens)
        );
        Ok(())
    }

    #[test]
    fn compaction_install_preserves_every_retained_image() -> Result<()> {
        let root = TemporaryDirectory::create("compaction-images")?;
        let cwd = root.0.join("repo");
        std::fs::create_dir_all(&cwd)?;
        let selection = ModelSelection::default();
        let rollout = Rollout::create_in_with_selection(&root.0, &cwd, &selection)?;
        let auto_compact_window = rollout.initial_auto_compact_window().advance();
        let world_state = WorldState::load(&cwd)?;
        let mut conversation = Conversation::from_world_state(world_state, rollout, selection)?;
        let mut content = vec![json!({
            "type": "input_text",
            "text": "inspect every retained image",
        })];
        content.extend((0..150).map(|_| {
            json!({
                "type": "input_image",
                "image_url": "data:image/png;base64,AAAA",
                "detail": "low",
            })
        }));
        let user = json!({
            "type": "message",
            "role": "user",
            "content": content,
        });
        let summary = json!({
            "type": "compaction",
            "encrypted_content": "opaque",
        });

        conversation.replace_compacted(
            vec![user, summary],
            InitialContextInjection::BeforeLastUserMessage,
            &ActiveTurnContext::default(),
            None,
            &[],
            auto_compact_window,
        )?;

        let retained_images = conversation
            .history
            .iter()
            .flat_map(|item| {
                item.get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter(|content| content.get("type").and_then(Value::as_str) == Some("input_image"))
            .count();
        assert_eq!(retained_images, 150);
        Ok(())
    }
}
