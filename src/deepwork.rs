//! Fixed `$deepwork` pipeline definitions, persisted state, and tool/runtime requests.

use crate::ask_user_question::AskUserQuestionArgs;
use crate::ask_user_question::AskUserQuestionResponse;
use crate::ask_user_question::MAX_FREE_TEXT_BYTES;
use crate::ask_user_question::MAX_OPTION_LABEL_CHARS;
use crate::ask_user_question::MAX_OPTIONS;
use crate::ask_user_question::MAX_QUESTION_CHARS;
use crate::ask_user_question::MAX_QUESTIONS;
use crate::model::ModelSelection;
use crate::model::ReasoningEffort;
use crate::session_group::ChildLifecycle;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest as _;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(crate) const TOOL_NAME: &str = "coordinate_specialist";
const STATE_VERSION: u32 = 1;
const WORKSPACE_DIRECTORY: &str = ".deepwork";
const MAX_ORIGINAL_TASK_BYTES: usize = 10 * 1024;
const MAX_CONTRACT_BYTES: usize = 10 * 1024;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_ACCEPTED_HANDOFF_BYTES: usize = 64 * 1024;
const MAX_RISKS_BYTES: usize = 16 * 1024;
const MAX_SKIP_REASON_BYTES: usize = 2 * 1024;
const MAX_ARTIFACTS: usize = 32;
const MAX_ARTIFACT_PATH_BYTES: usize = 4 * 1024;
const MAX_QUESTION_BATCHES: usize = 16;
const MAX_QUESTION_BATCH_JSON_BYTES: usize = 8 * 1024 - 2;
const MAX_QUESTION_HISTORY_JSON_BYTES: usize = 160 * 1024;
const MAX_STATUS_ANSWERS_BYTES: usize = MAX_QUESTION_BATCH_JSON_BYTES;
const MAX_STATUS_JSON_BYTES: usize = 32 * 1024;
const QUESTION_HISTORY_TRUNCATION_MARKER: &str = "… truncated in persisted history …";
pub(crate) const MAX_SPECIALIST_EVENT_TEXT_BYTES: usize = 24 * 1024;

const ACCEPTANCE_PROMPT: &str = include_str!("../subagents/acceptance.md");
const MANIFEST_PROMPT: &str = include_str!("../subagents/manifest.md");
const WORKER_PROMPT: &str = include_str!("../subagents/worker.md");
const REVIEWER_PROMPT: &str = include_str!("../subagents/reviewer.md");

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpecialistRole {
    #[serde(alias = "evals")]
    Acceptance,
    Manifest,
    Worker,
    Reviewer,
}

impl SpecialistRole {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Acceptance => "acceptance",
            Self::Manifest => "manifest",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Acceptance => "$acceptance",
            Self::Manifest => "$manifest",
            Self::Worker => "$worker",
            Self::Reviewer => "$reviewer",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Acceptance => {
                "Defines the task's evidence-backed completion contract before implementation"
            }
            Self::Manifest => {
                "Researches the required official documentation and writes the worker's routing manifest"
            }
            Self::Worker => {
                "Implements and verifies the accepted task against its completion contract, constraints, and documentation handoff"
            }
            Self::Reviewer => {
                "Surgically cleans, polishes, and refines the worker's implementation against the accepted success criteria"
            }
        }
    }

    pub(crate) const fn prompt(self) -> &'static str {
        match self {
            Self::Acceptance => ACCEPTANCE_PROMPT,
            Self::Manifest => MANIFEST_PROMPT,
            Self::Worker => WORKER_PROMPT,
            Self::Reviewer => REVIEWER_PROMPT,
        }
    }

    pub(crate) fn model_selection(self) -> ModelSelection {
        match self {
            Self::Acceptance | Self::Worker => {
                ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh)
            }
            Self::Manifest => ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh),
            Self::Reviewer => ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::Max),
        }
    }

    pub(crate) fn prompt_revision(self) -> &'static str {
        fn revision(prompt: &str) -> String {
            format!("sha256:{:x}", Sha256::digest(prompt.as_bytes()))
        }
        static ACCEPTANCE: OnceLock<String> = OnceLock::new();
        static MANIFEST: OnceLock<String> = OnceLock::new();
        static WORKER: OnceLock<String> = OnceLock::new();
        static REVIEWER: OnceLock<String> = OnceLock::new();
        match self {
            Self::Acceptance => ACCEPTANCE.get_or_init(|| revision(ACCEPTANCE_PROMPT)),
            Self::Manifest => MANIFEST.get_or_init(|| revision(MANIFEST_PROMPT)),
            Self::Worker => WORKER.get_or_init(|| revision(WORKER_PROMPT)),
            Self::Reviewer => REVIEWER.get_or_init(|| revision(REVIEWER_PROMPT)),
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value.trim().trim_start_matches('$') {
            "acceptance" | "evals" => Ok(Self::Acceptance),
            "manifest" => Ok(Self::Manifest),
            "worker" => Ok(Self::Worker),
            "reviewer" => Ok(Self::Reviewer),
            _ => Err(anyhow!("unknown deepwork specialist `{value}`")),
        }
    }

    const fn order(self) -> u8 {
        match self {
            Self::Acceptance => 0,
            Self::Manifest => 1,
            Self::Worker => 2,
            Self::Reviewer => 3,
        }
    }
}

impl fmt::Display for SpecialistRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpecialistEventKind {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpecialistEvent {
    pub(crate) session_id: String,
    pub(crate) role: SpecialistRole,
    pub(crate) stage_attempt: u32,
    pub(crate) kind: SpecialistEventKind,
    pub(crate) status: ChildLifecycle,
    pub(crate) message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) final_result: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum HarnessProfile {
    #[default]
    Main,
    Specialist(SpecialistRole),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeepworkStage {
    Interview,
    #[serde(alias = "evals")]
    Acceptance,
    Manifest,
    #[serde(alias = "readiness")]
    Worker,
    Reviewer,
    Completed,
}

impl DeepworkStage {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Interview => "interview",
            Self::Acceptance => "acceptance",
            Self::Manifest => "manifest",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Completed => "completed",
        }
    }

    pub(crate) const fn expected_specialist(self) -> Option<SpecialistRole> {
        match self {
            Self::Acceptance => Some(SpecialistRole::Acceptance),
            Self::Manifest => Some(SpecialistRole::Manifest),
            Self::Worker => Some(SpecialistRole::Worker),
            Self::Reviewer => Some(SpecialistRole::Reviewer),
            Self::Interview | Self::Completed => None,
        }
    }

    fn for_role(role: SpecialistRole) -> Self {
        match role {
            SpecialistRole::Acceptance => Self::Acceptance,
            SpecialistRole::Manifest => Self::Manifest,
            SpecialistRole::Worker => Self::Worker,
            SpecialistRole::Reviewer => Self::Reviewer,
        }
    }

    fn after(role: SpecialistRole) -> Self {
        match role {
            SpecialistRole::Acceptance => Self::Manifest,
            SpecialistRole::Manifest => Self::Worker,
            SpecialistRole::Worker => Self::Reviewer,
            SpecialistRole::Reviewer => Self::Completed,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AcceptedStage {
    pub(crate) role: SpecialistRole,
    pub(crate) session_id: String,
    pub(crate) stage_attempt: u32,
    pub(crate) accepted_handoff: String,
    pub(crate) artifacts: Vec<String>,
    pub(crate) remaining_risks: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SkippedStage {
    pub(crate) role: SpecialistRole,
    pub(crate) reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeepworkQuestionBatch {
    pub(crate) questions: Vec<String>,
    pub(crate) answers: Vec<DeepworkAnswer>,
    pub(crate) cancelled: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeepworkAnswer {
    pub(crate) question: String,
    pub(crate) selected_options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) free_text: Option<String>,
}

impl DeepworkQuestionBatch {
    pub(crate) fn from_response(
        arguments: &AskUserQuestionArgs,
        response: &AskUserQuestionResponse,
    ) -> Result<Self> {
        arguments.validate()?;
        response.validate_for(arguments)?;
        let batch = Self {
            questions: arguments
                .questions
                .iter()
                .map(|question| question.question.clone())
                .collect(),
            answers: response
                .answers
                .iter()
                .map(|answer| DeepworkAnswer {
                    question: answer.question.clone(),
                    selected_options: answer.selected_options.clone(),
                    free_text: answer.free_text.clone(),
                })
                .collect(),
            cancelled: response.cancelled,
            truncated: false,
        };
        batch.validate()?;
        Ok(batch)
    }

    fn validate(&self) -> Result<()> {
        if self.questions.is_empty() || self.questions.len() > MAX_QUESTIONS {
            return Err(anyhow!(
                "deepwork question batch must contain between 1 and {MAX_QUESTIONS} questions"
            ));
        }
        for question in &self.questions {
            validate_question_history_text(
                "deepwork persisted question",
                question,
                MAX_QUESTION_CHARS,
                None,
            )?;
        }
        if self.cancelled {
            if !self.answers.is_empty() {
                return Err(anyhow!(
                    "cancelled deepwork question batch must not contain answers"
                ));
            }
        } else if self.answers.len() != self.questions.len() {
            return Err(anyhow!(
                "deepwork question batch must contain one answer for every question"
            ));
        } else {
            for (index, answer) in self.answers.iter().enumerate() {
                if answer.question != self.questions[index] {
                    return Err(anyhow!(
                        "deepwork persisted answer question does not match its submitted question"
                    ));
                }
                if answer.selected_options.len() > MAX_OPTIONS {
                    return Err(anyhow!(
                        "deepwork persisted answer exceeds the {MAX_OPTIONS}-selected-option limit"
                    ));
                }
                for option in &answer.selected_options {
                    validate_question_history_text(
                        "deepwork persisted selected option",
                        option,
                        MAX_OPTION_LABEL_CHARS,
                        None,
                    )?;
                }
                if let Some(free_text) = &answer.free_text {
                    validate_question_history_text(
                        "deepwork persisted free text",
                        free_text,
                        usize::MAX,
                        Some(MAX_FREE_TEXT_BYTES),
                    )?;
                }
                if answer.selected_options.is_empty() && answer.free_text.is_none() {
                    return Err(anyhow!(
                        "deepwork persisted answer must contain a selected option or free text"
                    ));
                }
            }
        }
        let serialized = serialized_json_size(self);
        if serialized > MAX_QUESTION_BATCH_JSON_BYTES {
            return Err(anyhow!(
                "deepwork question batch exceeds the {MAX_QUESTION_BATCH_JSON_BYTES}-byte serialized limit"
            ));
        }
        Ok(())
    }

    fn migrate_legacy_bounds(&mut self) -> bool {
        let mut changed = false;
        if self.questions.len() > MAX_QUESTIONS {
            self.questions.truncate(MAX_QUESTIONS);
            changed = true;
        }
        for question in &mut self.questions {
            changed |= bound_question_history_text(question, MAX_QUESTION_CHARS, None);
        }
        if self.cancelled {
            if !self.answers.is_empty() {
                self.answers.clear();
                changed = true;
            }
        } else {
            if self.answers.len() > self.questions.len() {
                self.answers.truncate(self.questions.len());
                changed = true;
            }
            for (index, answer) in self.answers.iter_mut().enumerate() {
                if let Some(question) = self.questions.get(index)
                    && answer.question != *question
                {
                    answer.question = question.clone();
                    changed = true;
                }
                if answer.selected_options.len() > MAX_OPTIONS {
                    answer.selected_options.truncate(MAX_OPTIONS);
                    changed = true;
                }
                for option in &mut answer.selected_options {
                    changed |= bound_question_history_text(option, MAX_OPTION_LABEL_CHARS, None);
                }
                if let Some(free_text) = &mut answer.free_text {
                    changed |= bound_question_history_text(
                        free_text,
                        usize::MAX,
                        Some(MAX_FREE_TEXT_BYTES),
                    );
                    if free_text.trim().is_empty() {
                        answer.free_text = None;
                        changed = true;
                    }
                }
            }
        }
        self.truncated |= changed;
        changed
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeepworkState {
    version: u32,
    pub(crate) repository_root: PathBuf,
    pub(crate) run_index: u64,
    pub(crate) workspace: PathBuf,
    pub(crate) original_task: String,
    pub(crate) stage: DeepworkStage,
    pub(crate) interview_approved: bool,
    #[serde(default, rename = "readiness_approved", skip_serializing)]
    _legacy_readiness_approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) canonical_contract: Option<String>,
    #[serde(default)]
    pub(crate) question_batches: Vec<DeepworkQuestionBatch>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) question_history_truncated: bool,
    #[serde(default)]
    pub(crate) accepted_stages: BTreeMap<SpecialistRole, AcceptedStage>,
    #[serde(default)]
    pub(crate) skipped_stages: BTreeMap<SpecialistRole, SkippedStage>,
}

impl DeepworkState {
    pub(crate) fn activate(repository_root: &Path, original_task: String) -> Result<Self> {
        validate_text(
            "deepwork original task",
            &original_task,
            MAX_ORIGINAL_TASK_BYTES,
            false,
        )?;
        let repository_root = repository_root
            .canonicalize()
            .with_context(|| format!("failed to resolve {}", repository_root.display()))?;
        let container = repository_root.join(WORKSPACE_DIRECTORY);
        ensure_directory(&container, "deepwork container")?;
        let run_index = allocate_run_directory(&container)?;
        let workspace = container.join(run_index.to_string());
        let state = Self {
            version: STATE_VERSION,
            repository_root,
            run_index,
            workspace,
            original_task,
            stage: DeepworkStage::Interview,
            interview_approved: false,
            _legacy_readiness_approved: false,
            canonical_contract: None,
            question_batches: Vec::new(),
            question_history_truncated: false,
            accepted_stages: BTreeMap::new(),
            skipped_stages: BTreeMap::new(),
        };
        state.validate()?;
        Ok(state)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != STATE_VERSION {
            return Err(anyhow!(
                "unsupported deepwork state version {}; expected {STATE_VERSION}",
                self.version
            ));
        }
        validate_text(
            "deepwork original task",
            &self.original_task,
            MAX_ORIGINAL_TASK_BYTES,
            false,
        )?;
        if self.workspace
            != self
                .repository_root
                .join(WORKSPACE_DIRECTORY)
                .join(self.run_index.to_string())
        {
            return Err(anyhow!(
                "deepwork workspace does not match its repository root and run index"
            ));
        }
        if let Some(contract) = &self.canonical_contract {
            validate_contract(contract)?;
        }
        if self.interview_approved && self.canonical_contract.is_none() {
            return Err(anyhow!(
                "deepwork interview approval requires a canonical contract"
            ));
        }
        let accepted = |role| self.accepted_stages.contains_key(&role);
        let skipped = |role| self.skipped_stages.contains_key(&role);
        let manifest_resolved =
            accepted(SpecialistRole::Manifest) || skipped(SpecialistRole::Manifest);
        let no_decisions = self.accepted_stages.is_empty() && self.skipped_stages.is_empty();
        let stage_is_valid = match self.stage {
            DeepworkStage::Interview => {
                !self.interview_approved && self.canonical_contract.is_none() && no_decisions
            }
            DeepworkStage::Acceptance => self.interview_approved && no_decisions,
            DeepworkStage::Manifest => {
                self.interview_approved
                    && accepted(SpecialistRole::Acceptance)
                    && !manifest_resolved
                    && self.accepted_stages.len() == 1
                    && self.skipped_stages.is_empty()
            }
            DeepworkStage::Worker => {
                self.interview_approved
                    && accepted(SpecialistRole::Acceptance)
                    && manifest_resolved
                    && !accepted(SpecialistRole::Worker)
                    && !accepted(SpecialistRole::Reviewer)
                    && self.accepted_stages.len() + self.skipped_stages.len() == 2
            }
            DeepworkStage::Reviewer => {
                self.interview_approved
                    && accepted(SpecialistRole::Acceptance)
                    && manifest_resolved
                    && accepted(SpecialistRole::Worker)
                    && !accepted(SpecialistRole::Reviewer)
                    && self.accepted_stages.len() + self.skipped_stages.len() == 3
            }
            DeepworkStage::Completed => {
                self.interview_approved
                    && accepted(SpecialistRole::Acceptance)
                    && manifest_resolved
                    && accepted(SpecialistRole::Worker)
                    && accepted(SpecialistRole::Reviewer)
                    && self.accepted_stages.len() + self.skipped_stages.len() == 4
            }
        };
        if !stage_is_valid {
            return Err(anyhow!(
                "deepwork stage {:?} is inconsistent with its interview gate and resolved stages",
                self.stage
            ));
        }
        if self.question_batches.len() > MAX_QUESTION_BATCHES {
            return Err(anyhow!(
                "deepwork canonical state exceeds the {MAX_QUESTION_BATCHES}-question-batch retained-history limit"
            ));
        }
        for batch in &self.question_batches {
            batch.validate()?;
        }
        if self.question_batches.iter().any(|batch| batch.truncated)
            && !self.question_history_truncated
        {
            return Err(anyhow!(
                "deepwork truncated question batches require the canonical history truncation marker"
            ));
        }
        let question_history_bytes = serialized_pretty_json_size(&self.question_batches);
        if question_history_bytes > MAX_QUESTION_HISTORY_JSON_BYTES {
            return Err(anyhow!(
                "deepwork canonical question history exceeds the {MAX_QUESTION_HISTORY_JSON_BYTES}-byte serialized limit"
            ));
        }
        for (role, accepted) in &self.accepted_stages {
            if role != &accepted.role {
                return Err(anyhow!(
                    "deepwork accepted-stage key does not match its specialist role"
                ));
            }
            if self.skipped_stages.contains_key(role) {
                return Err(anyhow!(
                    "deepwork stage {} cannot be both accepted and skipped",
                    role.label()
                ));
            }
            validate_text(
                "accepted specialist handoff",
                &accepted.accepted_handoff,
                MAX_ACCEPTED_HANDOFF_BYTES,
                false,
            )?;
            validate_text(
                "accepted specialist remaining risks",
                &accepted.remaining_risks,
                MAX_RISKS_BYTES,
                true,
            )?;
            validate_artifact_strings(&accepted.artifacts)?;
        }
        for (role, skipped) in &self.skipped_stages {
            if role != &skipped.role {
                return Err(anyhow!(
                    "deepwork skipped-stage key does not match its specialist role"
                ));
            }
            if *role != SpecialistRole::Manifest {
                return Err(anyhow!("only the `$manifest` stage may be skipped"));
            }
            validate_text(
                "skipped manifest reason",
                &skipped.reason,
                MAX_SKIP_REASON_BYTES,
                false,
            )?;
        }
        Ok(())
    }

    pub(crate) fn ensure_workspace(&self) -> Result<()> {
        ensure_directory(
            &self.repository_root.join(WORKSPACE_DIRECTORY),
            "deepwork container",
        )?;
        ensure_directory(&self.workspace, "deepwork run workspace")
    }

    pub(crate) fn approve_interview(&mut self, contract: String) -> Result<()> {
        if self.stage != DeepworkStage::Interview {
            return Err(anyhow!(
                "the deepwork interview gate can only be approved during the interview stage"
            ));
        }
        validate_contract(&contract)?;
        self.canonical_contract = Some(contract);
        self.interview_approved = true;
        self.stage = DeepworkStage::Acceptance;
        Ok(())
    }

    pub(crate) fn validate_start(
        &self,
        role: SpecialistRole,
        has_live_child: bool,
        handoff: &str,
    ) -> Result<()> {
        if self.stage.expected_specialist() != Some(role) {
            return Err(anyhow!(
                "deepwork stage {:?} cannot start {}; expected {}",
                self.stage,
                role.label(),
                self.stage
                    .expected_specialist()
                    .map_or("no specialist", SpecialistRole::label)
            ));
        }
        if has_live_child {
            return Err(anyhow!(
                "deepwork is strictly sequential; retire the live specialist before starting another stage"
            ));
        }
        validate_text("specialist handoff", handoff, MAX_MESSAGE_BYTES, false)?;
        let contract = self
            .canonical_contract
            .as_deref()
            .context("deepwork cannot start a specialist before the interview contract exists")?;
        let criteria = success_criteria_block(contract)?;
        if !contains_complete_line_block(handoff, criteria) {
            return Err(anyhow!(
                "specialist handoff must preserve the canonical `SUCCESS CRITERIA` block verbatim"
            ));
        }
        Ok(())
    }

    pub(crate) fn accept_stage(
        &mut self,
        role: SpecialistRole,
        session_id: String,
        stage_attempt: u32,
        accepted_handoff: String,
        artifacts: Vec<String>,
        remaining_risks: String,
    ) -> Result<Vec<String>> {
        if self.stage.expected_specialist() != Some(role) {
            return Err(anyhow!(
                "{} cannot be accepted while deepwork is in stage {:?}",
                role.label(),
                self.stage
            ));
        }
        validate_text(
            "accepted specialist handoff",
            &accepted_handoff,
            MAX_ACCEPTED_HANDOFF_BYTES,
            false,
        )?;
        validate_text(
            "accepted specialist remaining risks",
            &remaining_risks,
            MAX_RISKS_BYTES,
            true,
        )?;
        let artifacts = self.validate_artifacts(role, artifacts)?;
        self.accepted_stages.insert(
            role,
            AcceptedStage {
                role,
                session_id,
                stage_attempt,
                accepted_handoff,
                artifacts: artifacts.clone(),
                remaining_risks,
            },
        );
        self.stage = DeepworkStage::after(role);
        Ok(artifacts)
    }

    pub(crate) fn skip_manifest(&mut self, reason: String) -> Result<()> {
        if self.stage != DeepworkStage::Manifest {
            return Err(anyhow!(
                "`$manifest` can only be skipped during the manifest stage"
            ));
        }
        validate_text(
            "skipped manifest reason",
            &reason,
            MAX_SKIP_REASON_BYTES,
            false,
        )?;
        self.skipped_stages.insert(
            SpecialistRole::Manifest,
            SkippedStage {
                role: SpecialistRole::Manifest,
                reason,
            },
        );
        self.stage = DeepworkStage::Worker;
        Ok(())
    }

    pub(crate) fn can_reopen_skipped_manifest(&self) -> bool {
        self.stage == DeepworkStage::Worker
            && self.skipped_stages.contains_key(&SpecialistRole::Manifest)
            && !self.accepted_stages.contains_key(&SpecialistRole::Worker)
    }

    pub(crate) fn reopen_skipped_manifest(&mut self) -> Result<()> {
        if !self.can_reopen_skipped_manifest() {
            return Err(anyhow!(
                "a skipped `$manifest` can only be reopened before `$worker` starts"
            ));
        }
        self.reopen(SpecialistRole::Manifest);
        Ok(())
    }

    pub(crate) fn reopen(&mut self, role: SpecialistRole) {
        self.stage = DeepworkStage::for_role(role);
        self.accepted_stages
            .retain(|accepted_role, _| accepted_role.order() < role.order());
        self.skipped_stages
            .retain(|skipped_role, _| skipped_role.order() < role.order());
    }

    pub(crate) fn migrate_question_history(&mut self) -> bool {
        let mut changed = false;
        for batch in &mut self.question_batches {
            changed |= batch.migrate_legacy_bounds();
        }
        if self.question_batches.len() > MAX_QUESTION_BATCHES {
            let discarded = self.question_batches.len() - MAX_QUESTION_BATCHES;
            self.question_batches.drain(..discarded);
            self.question_history_truncated = true;
            changed = true;
        }
        if changed || self.question_batches.iter().any(|batch| batch.truncated) {
            changed |= !self.question_history_truncated;
            self.question_history_truncated = true;
        }
        changed
    }

    pub(crate) fn record_question_batch(&mut self, batch: DeepworkQuestionBatch) -> Result<()> {
        batch.validate()?;
        self.question_batches.push(batch);
        if self.question_batches.len() > MAX_QUESTION_BATCHES {
            let discarded = self.question_batches.len() - MAX_QUESTION_BATCHES;
            self.question_batches.drain(..discarded);
            self.question_history_truncated = true;
        }
        self.validate()
    }

    pub(crate) fn status(&self) -> DeepworkStatus {
        let (question_batches, question_batches_omitted) = if self.canonical_contract.is_none() {
            bounded_question_batches(&self.question_batches)
        } else {
            (Vec::new(), self.question_batches.len())
        };
        let mut status = DeepworkStatus {
            run_index: self.run_index,
            workspace: display_path(&self.repository_root, &self.workspace),
            stage: self.stage,
            interview_approved: self.interview_approved,
            original_task: self
                .canonical_contract
                .is_none()
                .then(|| self.original_task.clone()),
            canonical_contract: self.canonical_contract.clone(),
            question_batches,
            question_batches_omitted,
            question_history_truncated: self.question_history_truncated,
            accepted_stages: self.accepted_stages.values().cloned().collect(),
            skipped_stages: self.skipped_stages.values().cloned().collect(),
            live_specialist: None,
        };
        bound_status(&mut status);
        status
    }

    fn validate_artifacts(
        &self,
        role: SpecialistRole,
        mut artifacts: Vec<String>,
    ) -> Result<Vec<String>> {
        if artifacts.len() > MAX_ARTIFACTS {
            return Err(anyhow!(
                "accepted specialist artifacts exceed the {MAX_ARTIFACTS}-path limit"
            ));
        }
        let required = match role {
            SpecialistRole::Acceptance => {
                let current = self.workspace.join("ACCEPTANCE.md");
                let legacy = self.workspace.join("EVALUATOR.md");
                Some(if current.exists() || !legacy.exists() {
                    current
                } else {
                    legacy
                })
            }
            SpecialistRole::Manifest => Some(self.workspace.join("MANIFEST.md")),
            SpecialistRole::Worker | SpecialistRole::Reviewer => None,
        };
        if let Some(required) = required {
            let required_display = display_path(&self.repository_root, &required);
            if !artifacts
                .iter()
                .any(|artifact| artifact == &required_display)
            {
                artifacts.insert(0, required_display);
            }
        }
        validate_artifact_strings(&artifacts)?;
        let canonical_workspace = self.workspace.canonicalize().with_context(|| {
            format!(
                "deepwork workspace {} is unavailable",
                self.workspace.display()
            )
        })?;
        let mut normalized = Vec::with_capacity(artifacts.len());
        for artifact in artifacts {
            let path = PathBuf::from(&artifact);
            let path = if path.is_absolute() {
                path
            } else {
                self.repository_root.join(path)
            };
            let metadata = std::fs::symlink_metadata(&path)
                .with_context(|| format!("accepted artifact {} does not exist", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(anyhow!(
                    "accepted artifact {} is not a regular non-symlink file",
                    path.display()
                ));
            }
            let canonical = path.canonicalize().with_context(|| {
                format!("failed to resolve accepted artifact {}", path.display())
            })?;
            if !canonical.starts_with(&canonical_workspace) {
                return Err(anyhow!(
                    "accepted pipeline artifact {} is outside the current deepwork workspace",
                    path.display()
                ));
            }
            let display = display_path(&self.repository_root, &canonical);
            if !normalized.contains(&display) {
                normalized.push(display);
            }
        }
        Ok(normalized)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeepworkStatus {
    pub(crate) run_index: u64,
    pub(crate) workspace: String,
    pub(crate) stage: DeepworkStage,
    pub(crate) interview_approved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) original_task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) canonical_contract: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) question_batches: Vec<DeepworkQuestionBatch>,
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) question_batches_omitted: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) question_history_truncated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) accepted_stages: Vec<AcceptedStage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) skipped_stages: Vec<SkippedStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) live_specialist: Option<DeepworkLiveSpecialist>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeepworkLiveSpecialist {
    pub(crate) session_id: String,
    pub(crate) role: SpecialistRole,
    pub(crate) stage_attempt: u32,
    pub(crate) lifecycle: ChildLifecycle,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum CoordinateSpecialistArgs {
    Status,
    ApproveInterview {
        contract: String,
    },
    SkipManifest {
        reason: String,
    },
    Start {
        specialist: SpecialistRole,
        handoff: String,
    },
    Send {
        session_id: String,
        message: String,
    },
    Wait {
        session_id: String,
    },
    Cancel {
        session_id: String,
    },
    Retire {
        session_id: String,
        accepted_handoff: String,
        #[serde(default)]
        artifacts: Vec<String>,
        #[serde(default)]
        remaining_risks: String,
    },
    Revive {
        session_id: String,
        message: String,
    },
    Replace {
        session_id: String,
        message: String,
    },
}

impl CoordinateSpecialistArgs {
    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            Self::Status | Self::Wait { .. } | Self::Cancel { .. } => {}
            Self::ApproveInterview { contract } => {
                validate_contract(contract)?;
            }
            Self::SkipManifest { reason } => {
                validate_text(
                    "skipped manifest reason",
                    reason,
                    MAX_SKIP_REASON_BYTES,
                    false,
                )?;
            }
            Self::Start { handoff, .. } => {
                validate_text("specialist handoff", handoff, MAX_MESSAGE_BYTES, false)?;
            }
            Self::Send { message, .. }
            | Self::Revive { message, .. }
            | Self::Replace { message, .. } => {
                validate_text("specialist message", message, MAX_MESSAGE_BYTES, false)?;
            }
            Self::Retire {
                accepted_handoff,
                artifacts,
                remaining_risks,
                ..
            } => {
                validate_text(
                    "accepted specialist handoff",
                    accepted_handoff,
                    MAX_ACCEPTED_HANDOFF_BYTES,
                    false,
                )?;
                validate_text(
                    "accepted specialist remaining risks",
                    remaining_risks,
                    MAX_RISKS_BYTES,
                    true,
                )?;
                validate_artifact_strings(artifacts)?;
            }
        }
        for session_id in self.session_ids() {
            uuid::Uuid::parse_str(session_id)
                .with_context(|| format!("invalid specialist session ID `{session_id}`"))?;
        }
        Ok(())
    }

    fn session_ids(&self) -> impl Iterator<Item = &str> {
        let session_id = match self {
            Self::Send { session_id, .. }
            | Self::Wait { session_id }
            | Self::Cancel { session_id }
            | Self::Retire { session_id, .. }
            | Self::Revive { session_id, .. }
            | Self::Replace { session_id, .. } => Some(session_id.as_str()),
            Self::Status
            | Self::ApproveInterview { .. }
            | Self::SkipManifest { .. }
            | Self::Start { .. } => None,
        };
        session_id.into_iter()
    }

    pub(crate) const fn action_name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::ApproveInterview { .. } => "approve_interview",
            Self::SkipManifest { .. } => "skip_manifest",
            Self::Start { .. } => "start",
            Self::Send { .. } => "send",
            Self::Wait { .. } => "wait",
            Self::Cancel { .. } => "cancel",
            Self::Retire { .. } => "retire",
            Self::Revive { .. } => "revive",
            Self::Replace { .. } => "replace",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoordinateSpecialistResponse {
    pub(crate) action: String,
    pub(crate) stage: DeepworkStage,
    pub(crate) run_index: u64,
    pub(crate) workspace: String,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) event: Option<SpecialistEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) state: Option<DeepworkStatus>,
}

#[derive(Clone)]
pub(crate) struct DeepworkRequester {
    requests: UnboundedSender<DeepworkRequest>,
}

pub(crate) enum DeepworkRequest {
    Activate {
        original_task: String,
        response: oneshot::Sender<Result<DeepworkStatus>>,
    },
    Coordinate {
        arguments: CoordinateSpecialistArgs,
        cancellation: CancellationToken,
        response: oneshot::Sender<Result<CoordinateSpecialistResponse>>,
    },
}

impl DeepworkRequester {
    pub(crate) async fn activate(&self, original_task: String) -> Result<DeepworkStatus> {
        let (response, receiver) = oneshot::channel();
        self.requests
            .send(DeepworkRequest::Activate {
                original_task,
                response,
            })
            .map_err(|_| anyhow!("the deepwork coordinator is unavailable"))?;
        receive_response(receiver).await
    }

    pub(crate) async fn coordinate(
        &self,
        arguments: CoordinateSpecialistArgs,
        cancellation: &CancellationToken,
    ) -> Result<CoordinateSpecialistResponse> {
        let (response, receiver) = oneshot::channel();
        self.requests
            .send(DeepworkRequest::Coordinate {
                arguments,
                cancellation: cancellation.clone(),
                response,
            })
            .map_err(|_| anyhow!("the deepwork coordinator is unavailable"))?;
        tokio::select! {
            _ = cancellation.cancelled() => Err(anyhow!("{TOOL_NAME} was interrupted")),
            response = receiver => response
                .map_err(|_| anyhow!("the deepwork coordinator stopped before replying"))?,
        }
    }
}

async fn receive_response<T>(receiver: oneshot::Receiver<Result<T>>) -> Result<T> {
    receiver
        .await
        .map_err(|_| anyhow!("the deepwork coordinator stopped before replying"))?
}

pub(crate) fn channel() -> (DeepworkRequester, UnboundedReceiver<DeepworkRequest>) {
    let (requests, receiver) = unbounded_channel();
    (DeepworkRequester { requests }, receiver)
}

pub(crate) fn bounded_event_text(text: &str) -> String {
    if text.len() <= MAX_SPECIALIST_EVENT_TEXT_BYTES {
        return text.to_string();
    }
    let marker = "\n… specialist result truncated for Main …";
    let budget = MAX_SPECIALIST_EVENT_TEXT_BYTES.saturating_sub(marker.len());
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &text[..end])
}

fn validate_contract(contract: &str) -> Result<()> {
    validate_text(
        "deepwork canonical contract",
        contract,
        MAX_CONTRACT_BYTES,
        false,
    )?;
    let _ = success_criteria_block(contract)?;
    Ok(())
}

fn success_criteria_block(contract: &str) -> Result<&str> {
    const LABEL: &str = "SUCCESS CRITERIA";
    let label_start = contract
        .split_inclusive('\n')
        .scan(0_usize, |offset, line| {
            let start = *offset;
            *offset = offset.saturating_add(line.len());
            Some((start, line.strip_suffix('\n').unwrap_or(line)))
        })
        .find_map(|(start, line)| (line == LABEL).then_some(start))
        .ok_or_else(|| {
            anyhow!("deepwork contract must contain the literal `SUCCESS CRITERIA` label")
        })?;
    let suffix = &contract[label_start..];
    let bullets = suffix.strip_prefix("SUCCESS CRITERIA\n").ok_or_else(|| {
        anyhow!(
            "deepwork contract must place at least one plain bullet directly below `SUCCESS CRITERIA`"
        )
    })?;
    let mut block_end = LABEL.len() + 1;
    let mut saw_bullet = false;
    for line in bullets.split_inclusive('\n') {
        let line_without_newline = line.strip_suffix('\n').unwrap_or(line);
        if !line_without_newline.starts_with("- ") || line_without_newline.len() <= 2 {
            break;
        }
        saw_bullet = true;
        block_end = block_end.saturating_add(line.len());
    }
    if !saw_bullet {
        return Err(anyhow!(
            "deepwork contract must contain at least one plain bullet directly below `SUCCESS CRITERIA`"
        ));
    }
    Ok(suffix[..block_end].trim_end_matches('\n'))
}

fn contains_complete_line_block(text: &str, block: &str) -> bool {
    text.match_indices(block).any(|(start, matched)| {
        let ends_at = start.saturating_add(matched.len());
        (start == 0 || text.as_bytes().get(start.wrapping_sub(1)) == Some(&b'\n'))
            && (ends_at == text.len() || text.as_bytes().get(ends_at) == Some(&b'\n'))
    })
}

fn validate_artifact_strings(artifacts: &[String]) -> Result<()> {
    if artifacts.len() > MAX_ARTIFACTS {
        return Err(anyhow!(
            "accepted specialist artifacts exceed the {MAX_ARTIFACTS}-path limit"
        ));
    }
    for artifact in artifacts {
        validate_text(
            "accepted specialist artifact path",
            artifact,
            MAX_ARTIFACT_PATH_BYTES,
            false,
        )?;
    }
    Ok(())
}

fn validate_question_history_text(
    label: &str,
    value: &str,
    maximum_chars: usize,
    maximum_bytes: Option<usize>,
) -> Result<()> {
    if value.trim().is_empty() {
        return Err(anyhow!("{label} cannot be empty"));
    }
    if value.chars().count() > maximum_chars {
        return Err(anyhow!(
            "{label} exceeds the {maximum_chars}-character limit"
        ));
    }
    if maximum_bytes.is_some_and(|maximum| value.len() > maximum) {
        return Err(anyhow!(
            "{label} exceeds the {}-byte limit",
            maximum_bytes.unwrap_or_default()
        ));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(anyhow!("{label} contains unsupported control characters"));
    }
    Ok(())
}

fn bound_question_history_text(
    value: &mut String,
    maximum_chars: usize,
    maximum_bytes: Option<usize>,
) -> bool {
    let mut changed = false;
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        *value = value
            .chars()
            .map(|character| {
                if character.is_control() && !matches!(character, '\n' | '\t') {
                    '�'
                } else {
                    character
                }
            })
            .collect();
        changed = true;
    }
    if value.chars().count() > maximum_chars {
        let marker_chars = QUESTION_HISTORY_TRUNCATION_MARKER.chars().count();
        let content_chars = maximum_chars.saturating_sub(marker_chars);
        let marker = QUESTION_HISTORY_TRUNCATION_MARKER
            .chars()
            .take(maximum_chars.saturating_sub(content_chars))
            .collect::<String>();
        let mut bounded = value.chars().take(content_chars).collect::<String>();
        bounded.push_str(&marker);
        *value = bounded;
        changed = true;
    }
    if let Some(maximum_bytes) = maximum_bytes
        && value.len() > maximum_bytes
    {
        *value = truncate_status_text(value, maximum_bytes, QUESTION_HISTORY_TRUNCATION_MARKER);
        changed = true;
    }
    changed
}

fn validate_text(label: &str, value: &str, maximum_bytes: usize, allow_empty: bool) -> Result<()> {
    if !allow_empty && value.trim().is_empty() {
        return Err(anyhow!("{label} cannot be empty"));
    }
    if value.len() > maximum_bytes {
        return Err(anyhow!("{label} exceeds the {maximum_bytes}-byte limit"));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(anyhow!("{label} contains unsupported control characters"));
    }
    Ok(())
}

fn ensure_directory(path: &Path, label: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(anyhow!("{label} {} is not a directory", path.display()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(path)
            .with_context(|| format!("failed to create {label} {}", path.display())),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {label} {}", path.display()))
        }
    }
}

fn allocate_run_directory(container: &Path) -> Result<u64> {
    let mut next = 0_u64;
    for entry in std::fs::read_dir(container).with_context(|| {
        format!(
            "failed to inspect deepwork container {}",
            container.display()
        )
    })? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(index) = name.parse::<u64>() else {
            continue;
        };
        ensure_directory(&entry.path(), "deepwork run workspace")?;
        next = next.max(index.saturating_add(1));
    }
    loop {
        let workspace = container.join(next.to_string());
        match std::fs::create_dir(&workspace) {
            Ok(()) => return Ok(next),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure_directory(&workspace, "deepwork run workspace")?;
                next = next
                    .checked_add(1)
                    .context("deepwork run index overflowed")?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create deepwork run workspace {}",
                        workspace.display()
                    )
                });
            }
        }
    }
}

fn display_path(repository_root: &Path, path: &Path) -> String {
    path.strip_prefix(repository_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

#[derive(Default)]
struct JsonSizeWriter {
    bytes: usize,
}

impl std::io::Write for JsonSizeWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized_json_size<T>(value: &T) -> usize
where
    T: Serialize + ?Sized,
{
    let mut writer = JsonSizeWriter::default();
    serde_json::to_writer(&mut writer, value).map_or(usize::MAX, |()| writer.bytes)
}

fn serialized_pretty_json_size<T>(value: &T) -> usize
where
    T: Serialize + ?Sized,
{
    let mut writer = JsonSizeWriter::default();
    serde_json::to_writer_pretty(&mut writer, value).map_or(usize::MAX, |()| writer.bytes)
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

pub(crate) fn bound_status(status: &mut DeepworkStatus) {
    const MARKER: &str = "… truncated in status …";
    // DeepworkStatus omits this camelCase field after its last batch is removed.
    const QUESTION_BATCHES_FIELD_OVERHEAD: usize = b",\"questionBatches\":[]".len();

    let mut json_bytes = serialized_json_size(status);
    if json_bytes <= MAX_STATUS_JSON_BYTES {
        return;
    }

    let question_batch_count = status.question_batches.len();
    let mut discarded_batches = 0;
    for batch in &status.question_batches {
        if json_bytes <= MAX_STATUS_JSON_BYTES {
            break;
        }
        discarded_batches += 1;
        let structural_bytes = if discarded_batches == question_batch_count {
            QUESTION_BATCHES_FIELD_OVERHEAD
        } else {
            1
        };
        json_bytes =
            json_bytes.saturating_sub(serialized_json_size(batch).saturating_add(structural_bytes));
    }
    drop(status.question_batches.drain(..discarded_batches));
    status.question_batches_omitted = status
        .question_batches_omitted
        .saturating_add(discarded_batches);
    json_bytes = serialized_json_size(status);

    while json_bytes > MAX_STATUS_JSON_BYTES {
        let mut longest = None;
        for (index, accepted) in status.accepted_stages.iter().enumerate() {
            for (handoff, length) in [
                (true, accepted.accepted_handoff.len()),
                (false, accepted.remaining_risks.len()),
            ] {
                if length > MARKER.len()
                    && longest.is_none_or(|(_, _, longest_length)| length > longest_length)
                {
                    longest = Some((index, handoff, length));
                }
            }
        }
        if let Some((index, handoff, length)) = longest {
            let text = if handoff {
                &mut status.accepted_stages[index].accepted_handoff
            } else {
                &mut status.accepted_stages[index].remaining_risks
            };
            let old_json_bytes = serialized_json_size(text.as_str());
            let truncated = truncate_status_text(text, (length / 2).max(MARKER.len()), MARKER);
            let new_json_bytes = serialized_json_size(truncated.as_str());
            *text = truncated;
            json_bytes = json_bytes.saturating_sub(old_json_bytes.saturating_sub(new_json_bytes));
            continue;
        }

        if let Some(accepted) = status
            .accepted_stages
            .iter_mut()
            .max_by_key(|accepted| accepted.artifacts.len())
            && let Some(artifact) = accepted.artifacts.pop()
        {
            let structural_bytes = usize::from(!accepted.artifacts.is_empty());
            json_bytes = json_bytes.saturating_sub(
                serialized_json_size(artifact.as_str()).saturating_add(structural_bytes),
            );
            continue;
        }

        if let Some(original_task) = status.original_task.as_mut()
            && original_task.len() > MARKER.len()
        {
            let old_json_bytes = serialized_json_size(original_task.as_str());
            let truncated = truncate_status_text(
                original_task,
                (original_task.len() / 2).max(MARKER.len()),
                MARKER,
            );
            let new_json_bytes = serialized_json_size(truncated.as_str());
            *original_task = truncated;
            json_bytes = json_bytes.saturating_sub(old_json_bytes.saturating_sub(new_json_bytes));
            continue;
        }
        break;
    }
}

fn truncate_status_text(text: &str, maximum_bytes: usize, marker: &str) -> String {
    if text.len() <= maximum_bytes {
        return text.to_string();
    }
    let budget = maximum_bytes.saturating_sub(marker.len());
    let end = text.floor_char_boundary(budget.min(text.len()));
    let mut truncated = String::with_capacity(end.saturating_add(marker.len()));
    truncated.push_str(&text[..end]);
    truncated.push_str(marker);
    truncated
}

fn bounded_question_batches(
    batches: &[DeepworkQuestionBatch],
) -> (Vec<DeepworkQuestionBatch>, usize) {
    let mut selected = Vec::new();
    for batch in batches.iter().rev() {
        selected.insert(0, batch.clone());
        if serialized_json_size(&selected) > MAX_STATUS_ANSWERS_BYTES {
            selected.remove(0);
            break;
        }
    }
    let omitted = batches.len().saturating_sub(selected.len());
    (selected, omitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask_user_question::AskUserQuestion;
    use crate::ask_user_question::AskUserQuestionAnswer;
    use crate::ask_user_question::AskUserQuestionOption;
    use crate::ask_user_question::MAX_OPTION_DESCRIPTION_CHARS;
    use crate::ask_user_question::MAX_RESPONSE_JSON_BYTES;

    fn maximum_question_card(seed: usize) -> (AskUserQuestionArgs, AskUserQuestionResponse) {
        let suffix =
            char::from_u32(0x1F600 + u32::try_from(seed % 16).unwrap_or_default()).unwrap_or('😀');
        let question = format!("{}{}", "😀".repeat(MAX_QUESTION_CHARS - 1), suffix);
        let option_suffixes = ['😀', '😁', '😂', '😃', '😄', '😅'];
        let options = option_suffixes
            .into_iter()
            .take(MAX_OPTIONS)
            .map(|suffix| AskUserQuestionOption {
                label: format!("{}{}", "😀".repeat(MAX_OPTION_LABEL_CHARS - 1), suffix),
                description: "d".repeat(MAX_OPTION_DESCRIPTION_CHARS),
                preview: None,
                default_selected: false,
            })
            .collect::<Vec<_>>();
        let arguments = AskUserQuestionArgs {
            questions: (0..MAX_QUESTIONS)
                .map(|index| AskUserQuestion {
                    question: question.clone(),
                    header: format!("Q{}", index + 1),
                    options: options.clone(),
                    multi_select: true,
                })
                .collect(),
        };
        let response = AskUserQuestionResponse::answered(
            arguments
                .questions
                .iter()
                .map(|question| AskUserQuestionAnswer {
                    question: question.question.clone(),
                    selected_options: question
                        .options
                        .iter()
                        .map(|option| option.label.clone())
                        .collect(),
                    free_text: Some("\"".repeat(MAX_FREE_TEXT_BYTES)),
                })
                .collect(),
        );
        (arguments, response)
    }

    #[test]
    fn cancel_coordination_action_accepts_a_stable_session_id() {
        let session_id = uuid::Uuid::from_u128(1).hyphenated().to_string();
        let arguments: CoordinateSpecialistArgs = serde_json::from_value(serde_json::json!({
            "action": "cancel",
            "session_id": session_id,
        }))
        .unwrap_or_else(|error| panic!("cancel action should deserialize: {error}"));

        assert_eq!(arguments.action_name(), "cancel");
        assert!(arguments.validate().is_ok());
        assert!(matches!(arguments, CoordinateSpecialistArgs::Cancel { .. }));
    }

    #[test]
    fn manifest_acceptance_advances_directly_to_worker() {
        assert_eq!(
            DeepworkStage::after(SpecialistRole::Manifest),
            DeepworkStage::Worker
        );
    }

    #[test]
    fn manifest_skip_advances_without_a_session_and_can_reopen_before_worker() {
        let mut state = DeepworkState {
            version: STATE_VERSION,
            repository_root: PathBuf::from("/tmp/bettercodex-manifest-skip"),
            run_index: 4,
            workspace: PathBuf::from("/tmp/bettercodex-manifest-skip/.deepwork/4"),
            original_task: "use the existing Shopify importer".to_string(),
            stage: DeepworkStage::Manifest,
            interview_approved: true,
            _legacy_readiness_approved: false,
            canonical_contract: Some(
                "SUCCESS CRITERIA\n- import every supplied product".to_string(),
            ),
            question_batches: Vec::new(),
            question_history_truncated: false,
            accepted_stages: BTreeMap::from([(
                SpecialistRole::Acceptance,
                AcceptedStage {
                    role: SpecialistRole::Acceptance,
                    session_id: uuid::Uuid::from_u128(1).hyphenated().to_string(),
                    stage_attempt: 0,
                    accepted_handoff: "completion contract accepted".to_string(),
                    artifacts: Vec::new(),
                    remaining_risks: String::new(),
                },
            )]),
            skipped_stages: BTreeMap::new(),
        };

        state
            .skip_manifest("existing importer already supplies the required routing".to_string())
            .unwrap_or_else(|error| panic!("manifest skip should succeed: {error}"));

        assert_eq!(state.stage, DeepworkStage::Worker);
        assert!(
            !state
                .accepted_stages
                .contains_key(&SpecialistRole::Manifest)
        );
        assert_eq!(
            state
                .skipped_stages
                .get(&SpecialistRole::Manifest)
                .map(|stage| stage.reason.as_str()),
            Some("existing importer already supplies the required routing")
        );
        assert!(state.validate().is_ok());

        state
            .reopen_skipped_manifest()
            .unwrap_or_else(|error| panic!("skipped manifest should reopen: {error}"));
        assert_eq!(state.stage, DeepworkStage::Manifest);
        assert!(state.skipped_stages.is_empty());
        assert!(state.validate().is_ok());
    }

    #[test]
    fn skip_manifest_coordination_action_requires_a_reason() {
        let arguments: CoordinateSpecialistArgs = serde_json::from_value(serde_json::json!({
            "action": "skip_manifest",
            "reason": "the repository already contains the complete routing surface",
        }))
        .unwrap_or_else(|error| panic!("skip action should deserialize: {error}"));

        assert_eq!(arguments.action_name(), "skip_manifest");
        assert!(arguments.validate().is_ok());
        assert!(matches!(
            arguments,
            CoordinateSpecialistArgs::SkipManifest { .. }
        ));
    }

    #[test]
    fn readiness_approval_action_is_not_available() {
        let arguments = serde_json::from_value::<CoordinateSpecialistArgs>(serde_json::json!({
            "action": "approve_readiness",
            "contract": "SUCCESS CRITERIA\n- preserve behavior",
        }));

        assert!(arguments.is_err());
    }

    #[test]
    fn legacy_readiness_state_resumes_at_worker_without_reserializing_the_gate() {
        let state: DeepworkState = serde_json::from_value(serde_json::json!({
            "version": 1,
            "repository_root": "/tmp/bettercodex-legacy-deepwork",
            "run_index": 7,
            "workspace": "/tmp/bettercodex-legacy-deepwork/.deepwork/7",
            "original_task": "preserve behavior",
            "stage": "readiness",
            "interview_approved": true,
            "readiness_approved": false,
            "canonical_contract": "SUCCESS CRITERIA\n- preserve behavior",
            "accepted_stages": {
                "evals": {
                    "role": "evals",
                    "session_id": "00000000-0000-0000-0000-000000000001",
                    "stage_attempt": 1,
                    "accepted_handoff": "accepted evaluator",
                    "artifacts": [],
                    "remaining_risks": ""
                },
                "manifest": {
                    "role": "manifest",
                    "session_id": "00000000-0000-0000-0000-000000000002",
                    "stage_attempt": 1,
                    "accepted_handoff": "accepted manifest",
                    "artifacts": [],
                    "remaining_risks": ""
                }
            }
        }))
        .unwrap_or_else(|error| panic!("legacy readiness state should deserialize: {error}"));

        assert_eq!(state.stage, DeepworkStage::Worker);
        assert!(
            state
                .accepted_stages
                .contains_key(&SpecialistRole::Acceptance)
        );
        assert!(state.validate().is_ok());

        let persisted = serde_json::to_value(&state)
            .unwrap_or_else(|error| panic!("migrated deepwork state should serialize: {error}"));
        assert_eq!(
            persisted.get("stage").and_then(serde_json::Value::as_str),
            Some("worker")
        );
        assert!(persisted.get("readiness_approved").is_none());
        let accepted = persisted
            .get("accepted_stages")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("accepted stages should serialize as an object"));
        assert!(accepted.contains_key("acceptance"));
        assert!(!accepted.contains_key("evals"));

        let status = serde_json::to_value(state.status())
            .unwrap_or_else(|error| panic!("deepwork status should serialize: {error}"));
        assert!(status.get("readinessApproved").is_none());
    }

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "bettercodex-deepwork-{label}-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&root)
                .unwrap_or_else(|error| panic!("temporary root should be created: {error}"));
            Self(root)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn interview_state_rejects_a_contract_without_approval() {
        let root = TestRoot::new("interview-contract");
        let mut state = DeepworkState::activate(&root.0, "preserve behavior".to_string())
            .unwrap_or_else(|error| panic!("deepwork should activate: {error}"));
        state.canonical_contract =
            Some("SUCCESS CRITERIA\n- preserve ordinary behavior".to_string());

        assert!(state.validate().is_err());
    }

    #[test]
    fn run_allocation_is_monotonic_and_ignores_non_numeric_entries() {
        let root = TestRoot::new("run-allocation");
        let container = root.0.join(WORKSPACE_DIRECTORY);
        std::fs::create_dir(&container)
            .unwrap_or_else(|error| panic!("container should be created: {error}"));
        std::fs::create_dir(container.join("0"))
            .unwrap_or_else(|error| panic!("run zero should be created: {error}"));
        std::fs::create_dir(container.join("3"))
            .unwrap_or_else(|error| panic!("run three should be created: {error}"));
        std::fs::write(container.join("notes"), b"ignored")
            .unwrap_or_else(|error| panic!("non-numeric entry should be created: {error}"));

        assert_eq!(
            allocate_run_directory(&container)
                .unwrap_or_else(|error| panic!("next run should allocate: {error}")),
            4
        );
        assert!(container.join("4").is_dir());
    }

    #[test]
    fn run_allocation_rejects_a_numeric_regular_file() {
        let root = TestRoot::new("numeric-file");
        let container = root.0.join(WORKSPACE_DIRECTORY);
        std::fs::create_dir(&container)
            .unwrap_or_else(|error| panic!("container should be created: {error}"));
        std::fs::write(container.join("2"), b"unsafe")
            .unwrap_or_else(|error| panic!("numeric file should be created: {error}"));

        assert!(allocate_run_directory(&container).is_err());
        assert!(!container.join("3").exists());
    }

    #[cfg(unix)]
    #[test]
    fn run_allocation_rejects_a_numeric_symlink() {
        let root = TestRoot::new("numeric-symlink");
        let container = root.0.join(WORKSPACE_DIRECTORY);
        let target = root.0.join("target");
        std::fs::create_dir(&container)
            .unwrap_or_else(|error| panic!("container should be created: {error}"));
        std::fs::create_dir(&target)
            .unwrap_or_else(|error| panic!("target should be created: {error}"));
        std::os::unix::fs::symlink(&target, container.join("2"))
            .unwrap_or_else(|error| panic!("numeric symlink should be created: {error}"));

        assert!(allocate_run_directory(&container).is_err());
        assert!(!container.join("3").exists());
    }

    #[test]
    fn accepted_artifacts_are_deduplicated_and_reject_unsafe_paths() {
        let root = TestRoot::new("artifacts");
        let mut state = DeepworkState::activate(&root.0, "preserve behavior".to_string())
            .unwrap_or_else(|error| panic!("deepwork should activate: {error}"));
        state
            .approve_interview("SUCCESS CRITERIA\n- preserve behavior".to_string())
            .unwrap_or_else(|error| panic!("interview should approve: {error}"));
        let required = state.workspace.join("ACCEPTANCE.md");
        std::fs::write(&required, b"accepted")
            .unwrap_or_else(|error| panic!("required artifact should be written: {error}"));
        let required_display = display_path(&state.repository_root, &required);

        let normalized = state
            .validate_artifacts(
                SpecialistRole::Acceptance,
                vec![required_display.clone(), required_display],
            )
            .unwrap_or_else(|error| panic!("safe artifacts should validate: {error}"));
        assert_eq!(normalized.len(), 1);

        let outside = root.0.join("outside.md");
        std::fs::write(&outside, b"outside")
            .unwrap_or_else(|error| panic!("outside file should be written: {error}"));
        assert!(
            state
                .validate_artifacts(
                    SpecialistRole::Acceptance,
                    vec![display_path(&state.repository_root, &outside)],
                )
                .is_err()
        );

        #[cfg(unix)]
        {
            let symlink = state.workspace.join("linked.md");
            std::os::unix::fs::symlink(&required, &symlink)
                .unwrap_or_else(|error| panic!("artifact symlink should be created: {error}"));
            assert!(
                state
                    .validate_artifacts(
                        SpecialistRole::Acceptance,
                        vec![display_path(&state.repository_root, &symlink)],
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn accepted_pipeline_preserves_criteria_and_rejects_out_of_order_start() {
        let root = TestRoot::new("pipeline");
        let mut state = DeepworkState::activate(&root.0, "preserve behavior".to_string())
            .unwrap_or_else(|error| panic!("deepwork should activate: {error}"));
        let contract = "Plan\n\nSUCCESS CRITERIA\n- preserve ordinary behavior\n- persist every accepted stage\n\nConstraints";
        let criteria = success_criteria_block(contract)
            .unwrap_or_else(|error| panic!("criteria should parse: {error}"));
        state
            .approve_interview(contract.to_string())
            .unwrap_or_else(|error| panic!("interview should approve: {error}"));
        let handoff = format!("Implement this contract.\n\n{criteria}\n\nReturn evidence.");

        assert!(
            state
                .validate_start(SpecialistRole::Worker, false, &handoff)
                .is_err()
        );
        state
            .validate_start(SpecialistRole::Acceptance, false, &handoff)
            .unwrap_or_else(|error| panic!("acceptance should start: {error}"));
        std::fs::write(state.workspace.join("ACCEPTANCE.md"), b"accepted")
            .unwrap_or_else(|error| panic!("acceptance artifact should be written: {error}"));
        state
            .accept_stage(
                SpecialistRole::Acceptance,
                uuid::Uuid::from_u128(1).hyphenated().to_string(),
                0,
                "accepted completion contract".to_string(),
                Vec::new(),
                String::new(),
            )
            .unwrap_or_else(|error| panic!("acceptance should retire: {error}"));

        state
            .validate_start(SpecialistRole::Manifest, false, &handoff)
            .unwrap_or_else(|error| panic!("manifest should start: {error}"));
        std::fs::write(state.workspace.join("MANIFEST.md"), b"routing")
            .unwrap_or_else(|error| panic!("manifest artifact should be written: {error}"));
        state
            .accept_stage(
                SpecialistRole::Manifest,
                uuid::Uuid::from_u128(2).hyphenated().to_string(),
                0,
                "accepted routing manifest".to_string(),
                Vec::new(),
                String::new(),
            )
            .unwrap_or_else(|error| panic!("manifest should retire: {error}"));
        state
            .accept_stage(
                SpecialistRole::Worker,
                uuid::Uuid::from_u128(3).hyphenated().to_string(),
                0,
                "implementation validated".to_string(),
                Vec::new(),
                String::new(),
            )
            .unwrap_or_else(|error| panic!("worker should retire: {error}"));
        state
            .accept_stage(
                SpecialistRole::Reviewer,
                uuid::Uuid::from_u128(4).hyphenated().to_string(),
                0,
                "review complete".to_string(),
                Vec::new(),
                String::new(),
            )
            .unwrap_or_else(|error| panic!("reviewer should retire: {error}"));

        assert_eq!(state.stage, DeepworkStage::Completed);
        assert!(state.validate().is_ok());
        assert!(handoff.contains(criteria));
    }

    #[test]
    fn maximum_question_batches_fit_response_history_and_newest_status_budgets() {
        let root = TestRoot::new("question-budgets");
        let mut state = DeepworkState::activate(&root.0, "preserve decisions".to_string())
            .unwrap_or_else(|error| panic!("deepwork should activate: {error}"));

        for seed in 0..=MAX_QUESTION_BATCHES {
            let (arguments, response) = maximum_question_card(seed);
            response
                .validate_for(&arguments)
                .unwrap_or_else(|error| panic!("maximum response should validate: {error}"));
            assert!(serialized_json_size(&response) <= MAX_RESPONSE_JSON_BYTES);
            let batch = DeepworkQuestionBatch::from_response(&arguments, &response)
                .unwrap_or_else(|error| panic!("maximum batch should validate: {error}"));
            assert!(serialized_json_size(&batch) <= MAX_QUESTION_BATCH_JSON_BYTES);
            state
                .record_question_batch(batch)
                .unwrap_or_else(|error| panic!("maximum batch should persist: {error}"));
        }

        assert_eq!(state.question_batches.len(), MAX_QUESTION_BATCHES);
        assert!(state.question_history_truncated);
        assert!(
            serialized_pretty_json_size(&state.question_batches) <= MAX_QUESTION_HISTORY_JSON_BYTES
        );
        let status = state.status();
        assert_eq!(
            status.question_batches.last(),
            state.question_batches.last(),
            "the newest decision must remain visible"
        );
        assert!(status.question_batches_omitted > 0);
        assert!(status.question_history_truncated);
        assert!(serialized_json_size(&status.question_batches) <= MAX_STATUS_ANSWERS_BYTES);
    }

    #[test]
    fn legacy_question_history_migrates_to_the_bounded_newest_window() {
        let root = TestRoot::new("question-migration");
        let mut state = DeepworkState::activate(&root.0, "preserve decisions".to_string())
            .unwrap_or_else(|error| panic!("deepwork should activate: {error}"));
        state.question_batches = (0..MAX_QUESTION_BATCHES + 3)
            .map(|index| {
                let question = format!("legacy {index} {}", "😀".repeat(200));
                DeepworkQuestionBatch {
                    questions: vec![question.clone()],
                    answers: vec![DeepworkAnswer {
                        question,
                        selected_options: vec!["legacy option".repeat(20)],
                        free_text: Some("x".repeat(MAX_FREE_TEXT_BYTES + 200)),
                    }],
                    cancelled: false,
                    truncated: false,
                }
            })
            .collect();

        assert!(state.migrate_question_history());
        assert_eq!(state.question_batches.len(), MAX_QUESTION_BATCHES);
        assert!(state.question_history_truncated);
        assert!(state.question_batches.iter().all(|batch| batch.truncated));
        state
            .validate()
            .unwrap_or_else(|error| panic!("migrated history should validate: {error}"));
    }

    #[test]
    fn status_bound_keeps_worst_case_output_valid_utf8_and_json() {
        let accepted_stages = [
            SpecialistRole::Acceptance,
            SpecialistRole::Manifest,
            SpecialistRole::Worker,
            SpecialistRole::Reviewer,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, role)| AcceptedStage {
            role,
            session_id: uuid::Uuid::from_u128(index as u128 + 1)
                .hyphenated()
                .to_string(),
            stage_attempt: u32::try_from(index).unwrap_or(u32::MAX),
            accepted_handoff: "界".repeat(20_000),
            artifacts: (0..MAX_ARTIFACTS)
                .map(|artifact| format!(".deepwork/0/{index}-{artifact}-{}", "界".repeat(500)))
                .collect(),
            remaining_risks: "界".repeat(5_000),
        })
        .collect();
        let question_batches = (0..MAX_QUESTION_BATCHES)
            .map(|_| DeepworkQuestionBatch {
                questions: vec!["界".repeat(500)],
                answers: vec![DeepworkAnswer {
                    question: "界".repeat(500),
                    selected_options: vec!["界".repeat(500)],
                    free_text: Some("界".repeat(500)),
                }],
                cancelled: false,
                truncated: false,
            })
            .collect();
        let mut status = DeepworkStatus {
            run_index: 0,
            workspace: ".deepwork/0".to_string(),
            stage: DeepworkStage::Completed,
            interview_approved: true,
            original_task: None,
            canonical_contract: Some(format!("SUCCESS CRITERIA\n- {}", "界".repeat(3_000))),
            question_batches,
            question_batches_omitted: 0,
            question_history_truncated: false,
            accepted_stages,
            skipped_stages: Vec::new(),
            live_specialist: None,
        };

        bound_status(&mut status);
        let encoded = serde_json::to_vec(&status)
            .unwrap_or_else(|error| panic!("bounded status should serialize: {error}"));

        assert!(encoded.len() <= MAX_STATUS_JSON_BYTES);
        assert!(std::str::from_utf8(&encoded).is_ok());
        assert!(serde_json::from_slice::<serde_json::Value>(&encoded).is_ok());
    }
}
