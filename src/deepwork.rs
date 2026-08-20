//! Fixed `$deepwork` pipeline definitions, persisted state, and tool/runtime requests.

use crate::ask_user_question::AskUserQuestionArgs;
use crate::ask_user_question::AskUserQuestionResponse;
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
const MAX_ARTIFACTS: usize = 32;
const MAX_ARTIFACT_PATH_BYTES: usize = 4 * 1024;
const MAX_QUESTION_BATCHES: usize = 128;
const MAX_STATUS_ANSWERS_BYTES: usize = 8 * 1024;
const MAX_STATUS_JSON_BYTES: usize = 32 * 1024;
pub(crate) const MAX_SPECIALIST_EVENT_TEXT_BYTES: usize = 24 * 1024;

const EVALS_PROMPT: &str = include_str!("../subagents/evals.md");
const MANIFEST_PROMPT: &str = include_str!("../subagents/manifest.md");
const WORKER_PROMPT: &str = include_str!("../subagents/worker.md");
const REVIEWER_PROMPT: &str = include_str!("../subagents/reviewer.md");

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpecialistRole {
    Evals,
    Manifest,
    Worker,
    Reviewer,
}

impl SpecialistRole {
    const PIPELINE: [Self; 4] = [Self::Evals, Self::Manifest, Self::Worker, Self::Reviewer];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Evals => "evals",
            Self::Manifest => "manifest",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Evals => "$evals",
            Self::Manifest => "$manifest",
            Self::Worker => "$worker",
            Self::Reviewer => "$reviewer",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Evals => {
                "Builds the task-specific eval suite and acceptance gates before implementation"
            }
            Self::Manifest => {
                "Researches the required official documentation and writes the worker's routing manifest"
            }
            Self::Worker => {
                "Implements and validates the accepted task against the approved evaluator, constraints, and documentation handoff"
            }
            Self::Reviewer => {
                "Surgically cleans, polishes, and refines the worker's implementation against the accepted success criteria"
            }
        }
    }

    pub(crate) const fn prompt(self) -> &'static str {
        match self {
            Self::Evals => EVALS_PROMPT,
            Self::Manifest => MANIFEST_PROMPT,
            Self::Worker => WORKER_PROMPT,
            Self::Reviewer => REVIEWER_PROMPT,
        }
    }

    pub(crate) fn model_selection(self) -> ModelSelection {
        match self {
            Self::Evals | Self::Worker => {
                ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh)
            }
            Self::Manifest => ModelSelection::from_identity("gpt-5.6-luna", ReasoningEffort::Max),
            Self::Reviewer => ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::Max),
        }
    }

    pub(crate) fn prompt_revision(self) -> &'static str {
        fn revision(prompt: &str) -> String {
            format!("sha256:{:x}", Sha256::digest(prompt.as_bytes()))
        }
        static EVALS: OnceLock<String> = OnceLock::new();
        static MANIFEST: OnceLock<String> = OnceLock::new();
        static WORKER: OnceLock<String> = OnceLock::new();
        static REVIEWER: OnceLock<String> = OnceLock::new();
        match self {
            Self::Evals => EVALS.get_or_init(|| revision(EVALS_PROMPT)),
            Self::Manifest => MANIFEST.get_or_init(|| revision(MANIFEST_PROMPT)),
            Self::Worker => WORKER.get_or_init(|| revision(WORKER_PROMPT)),
            Self::Reviewer => REVIEWER.get_or_init(|| revision(REVIEWER_PROMPT)),
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value.trim().trim_start_matches('$') {
            "evals" => Ok(Self::Evals),
            "manifest" => Ok(Self::Manifest),
            "worker" => Ok(Self::Worker),
            "reviewer" => Ok(Self::Reviewer),
            _ => Err(anyhow!("unknown deepwork specialist `{value}`")),
        }
    }

    const fn order(self) -> u8 {
        match self {
            Self::Evals => 0,
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
    Evals,
    Manifest,
    Readiness,
    Worker,
    Reviewer,
    Completed,
}

impl DeepworkStage {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Interview => "interview",
            Self::Evals => "evals",
            Self::Manifest => "manifest",
            Self::Readiness => "readiness",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Completed => "completed",
        }
    }

    pub(crate) const fn expected_specialist(self) -> Option<SpecialistRole> {
        match self {
            Self::Evals => Some(SpecialistRole::Evals),
            Self::Manifest => Some(SpecialistRole::Manifest),
            Self::Worker => Some(SpecialistRole::Worker),
            Self::Reviewer => Some(SpecialistRole::Reviewer),
            Self::Interview | Self::Readiness | Self::Completed => None,
        }
    }

    fn for_role(role: SpecialistRole) -> Self {
        match role {
            SpecialistRole::Evals => Self::Evals,
            SpecialistRole::Manifest => Self::Manifest,
            SpecialistRole::Worker => Self::Worker,
            SpecialistRole::Reviewer => Self::Reviewer,
        }
    }

    fn after(role: SpecialistRole) -> Self {
        match role {
            SpecialistRole::Evals => Self::Manifest,
            SpecialistRole::Manifest => Self::Readiness,
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
pub(crate) struct DeepworkQuestionBatch {
    pub(crate) questions: Vec<String>,
    pub(crate) answers: Vec<DeepworkAnswer>,
    pub(crate) cancelled: bool,
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
    ) -> Self {
        Self {
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
        }
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
    pub(crate) readiness_approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) canonical_contract: Option<String>,
    #[serde(default)]
    pub(crate) question_batches: Vec<DeepworkQuestionBatch>,
    #[serde(default)]
    pub(crate) accepted_stages: BTreeMap<SpecialistRole, AcceptedStage>,
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
            readiness_approved: false,
            canonical_contract: None,
            question_batches: Vec::new(),
            accepted_stages: BTreeMap::new(),
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
        if self.readiness_approved
            && (!self.interview_approved
                || !self.accepted_stages.contains_key(&SpecialistRole::Evals)
                || !self.accepted_stages.contains_key(&SpecialistRole::Manifest))
        {
            return Err(anyhow!(
                "deepwork readiness approval requires accepted evals and manifest stages"
            ));
        }
        let accepted = |role| self.accepted_stages.contains_key(&role);
        let stage_is_valid = match self.stage {
            DeepworkStage::Interview => {
                !self.interview_approved
                    && !self.readiness_approved
                    && self.accepted_stages.is_empty()
            }
            DeepworkStage::Evals => {
                self.interview_approved
                    && !self.readiness_approved
                    && self.accepted_stages.is_empty()
            }
            DeepworkStage::Manifest => {
                self.interview_approved
                    && !self.readiness_approved
                    && accepted(SpecialistRole::Evals)
                    && self.accepted_stages.len() == 1
            }
            DeepworkStage::Readiness => {
                self.interview_approved
                    && !self.readiness_approved
                    && accepted(SpecialistRole::Evals)
                    && accepted(SpecialistRole::Manifest)
                    && self.accepted_stages.len() == 2
            }
            DeepworkStage::Worker => {
                self.interview_approved
                    && self.readiness_approved
                    && accepted(SpecialistRole::Evals)
                    && accepted(SpecialistRole::Manifest)
                    && self.accepted_stages.len() == 2
            }
            DeepworkStage::Reviewer => {
                self.interview_approved
                    && self.readiness_approved
                    && accepted(SpecialistRole::Evals)
                    && accepted(SpecialistRole::Manifest)
                    && accepted(SpecialistRole::Worker)
                    && self.accepted_stages.len() == 3
            }
            DeepworkStage::Completed => {
                self.interview_approved
                    && self.readiness_approved
                    && SpecialistRole::PIPELINE.iter().all(|role| accepted(*role))
                    && self.accepted_stages.len() == 4
            }
        };
        if !stage_is_valid {
            return Err(anyhow!(
                "deepwork stage {:?} is inconsistent with its accepted gates and stages",
                self.stage
            ));
        }
        if self.question_batches.len() > MAX_QUESTION_BATCHES {
            return Err(anyhow!(
                "deepwork canonical state exceeds the {MAX_QUESTION_BATCHES}-question-batch limit"
            ));
        }
        for (role, accepted) in &self.accepted_stages {
            if role != &accepted.role {
                return Err(anyhow!(
                    "deepwork accepted-stage key does not match its specialist role"
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
        self.readiness_approved = false;
        self.stage = DeepworkStage::Evals;
        Ok(())
    }

    pub(crate) fn approve_readiness(&mut self, contract: String) -> Result<()> {
        if self.stage != DeepworkStage::Readiness {
            return Err(anyhow!(
                "the deepwork readiness gate can only be approved after evals and manifest are accepted"
            ));
        }
        if !self.accepted_stages.contains_key(&SpecialistRole::Evals)
            || !self.accepted_stages.contains_key(&SpecialistRole::Manifest)
        {
            return Err(anyhow!(
                "the deepwork readiness gate requires accepted evals and manifest stages"
            ));
        }
        validate_contract(&contract)?;
        self.canonical_contract = Some(contract);
        self.readiness_approved = true;
        self.stage = DeepworkStage::Worker;
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
                    .map_or("a user approval gate", SpecialistRole::label)
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

    pub(crate) fn reopen(&mut self, role: SpecialistRole) {
        self.stage = DeepworkStage::for_role(role);
        self.accepted_stages
            .retain(|accepted_role, _| accepted_role.order() < role.order());
        if role.order() <= SpecialistRole::Manifest.order() {
            self.readiness_approved = false;
        }
    }

    pub(crate) fn record_question_batch(&mut self, batch: DeepworkQuestionBatch) -> Result<()> {
        if self.question_batches.len() >= MAX_QUESTION_BATCHES {
            return Err(anyhow!(
                "deepwork canonical state reached the {MAX_QUESTION_BATCHES}-question-batch limit"
            ));
        }
        self.question_batches.push(batch);
        self.validate()
    }

    pub(crate) fn status(&self) -> DeepworkStatus {
        let question_batches = if self.canonical_contract.is_none() {
            bounded_question_batches(&self.question_batches)
        } else {
            Vec::new()
        };
        let mut status = DeepworkStatus {
            run_index: self.run_index,
            workspace: display_path(&self.repository_root, &self.workspace),
            stage: self.stage,
            interview_approved: self.interview_approved,
            readiness_approved: self.readiness_approved,
            original_task: self
                .canonical_contract
                .is_none()
                .then(|| self.original_task.clone()),
            canonical_contract: self.canonical_contract.clone(),
            question_batches,
            accepted_stages: self.accepted_stages.values().cloned().collect(),
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
            SpecialistRole::Evals => Some(self.workspace.join("EVALUATOR.md")),
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
    pub(crate) readiness_approved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) original_task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) canonical_contract: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) question_batches: Vec<DeepworkQuestionBatch>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) accepted_stages: Vec<AcceptedStage>,
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
    ApproveReadiness {
        contract: String,
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
            Self::Status | Self::Wait { .. } => {}
            Self::ApproveInterview { contract } | Self::ApproveReadiness { contract } => {
                validate_contract(contract)?;
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
            | Self::Retire { session_id, .. }
            | Self::Revive { session_id, .. }
            | Self::Replace { session_id, .. } => Some(session_id.as_str()),
            Self::Status
            | Self::ApproveInterview { .. }
            | Self::ApproveReadiness { .. }
            | Self::Start { .. } => None,
        };
        session_id.into_iter()
    }

    pub(crate) const fn action_name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::ApproveInterview { .. } => "approve_interview",
            Self::ApproveReadiness { .. } => "approve_readiness",
            Self::Start { .. } => "start",
            Self::Send { .. } => "send",
            Self::Wait { .. } => "wait",
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
        if entry.file_type()?.is_dir() {
            next = next.max(index.saturating_add(1));
        }
    }
    loop {
        let workspace = container.join(next.to_string());
        match std::fs::create_dir(&workspace) {
            Ok(()) => return Ok(next),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
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

pub(crate) fn bound_status(status: &mut DeepworkStatus) {
    const MARKER: &str = "… truncated in status …";
    while serde_json::to_vec(status).map_or(usize::MAX, |encoded| encoded.len())
        > MAX_STATUS_JSON_BYTES
    {
        if !status.question_batches.is_empty() {
            status.question_batches.remove(0);
            continue;
        }

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
            *text = truncate_status_text(text, (length / 2).max(MARKER.len()), MARKER);
            continue;
        }

        if let Some(accepted) = status
            .accepted_stages
            .iter_mut()
            .max_by_key(|accepted| accepted.artifacts.len())
            && !accepted.artifacts.is_empty()
        {
            accepted.artifacts.pop();
            continue;
        }

        if let Some(original_task) = status.original_task.as_mut()
            && original_task.len() > MARKER.len()
        {
            *original_task = truncate_status_text(
                original_task,
                (original_task.len() / 2).max(MARKER.len()),
                MARKER,
            );
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
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &text[..end])
}

fn bounded_question_batches(batches: &[DeepworkQuestionBatch]) -> Vec<DeepworkQuestionBatch> {
    let mut selected = Vec::new();
    let mut bytes = 0_usize;
    for batch in batches.iter().rev() {
        let batch_bytes = serde_json::to_vec(batch).map_or(usize::MAX, |encoded| encoded.len());
        if bytes.saturating_add(batch_bytes) > MAX_STATUS_ANSWERS_BYTES {
            break;
        }
        bytes = bytes.saturating_add(batch_bytes);
        selected.push(batch.clone());
    }
    selected.reverse();
    selected
}
