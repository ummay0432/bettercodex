//! Stable multi-session identity and bounded persisted linkage.
//!
//! Agent rollouts remain ordinary per-session JSONL journals. This sidecar records only the
//! coordinator-owned relationship between a Main session and its independent child sessions so a
//! cold resume can reconstruct the group without changing the established saved-session path.

use crate::deepwork::DeepworkState;
use crate::deepwork::SpecialistRole;
use crate::model::ModelSelection;
use crate::model::ReasoningEffort;
use crate::state_file;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error as _;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

const LINKAGE_VERSION: u32 = 1;
const GROUPS_DIRECTORY: &str = "session-groups";
const MAX_LINKAGE_BYTES: usize = 512 * 1024;
const MAX_ROLE_BYTES: usize = 64;
const MAX_HANDOFF_BYTES: usize = 64 * 1024;
const MAX_PROMPT_REVISION_BYTES: usize = 128;
const LEGACY_EVALS_PROMPT_REVISION: &str =
    "sha256:ab5a6b2ceb1ec35dbfb023130199bdfd2bd3702bdccf51612cee071978ece069";
const LEGACY_MANIFEST_PROMPT_REVISION: &str =
    "sha256:524e1f9b93b35368126493012ac594c9c240ca285788ad96c9026112d0477f08";
const LEGACY_WORKER_PROMPT_REVISION: &str =
    "sha256:3e806487fb2c00113881e0ca3205a29b03ff601a06500ea7412aeea38ca6f8a5";
const LEGACY_REVIEWER_PROMPT_REVISION: &str =
    "sha256:bea1ee234f07edb89f033c22154c25a3f04de19a4a6a52759bfe702c14c73d0c";

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub(crate) struct SessionId(String);

impl SessionId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub(crate) fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let parsed = Uuid::parse_str(&value)
            .with_context(|| format!("invalid bettercodex session ID `{value}`"))?;
        if parsed.hyphenated().to_string() != value {
            return Err(anyhow!(
                "bettercodex session ID `{value}` is not in canonical UUID form"
            ));
        }
        Ok(Self(value))
    }

    pub(crate) fn as_uuid(&self) -> Result<Uuid> {
        Uuid::parse_str(&self.0).context("validated bettercodex session ID became invalid")
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildLifecycle {
    Active,
    Working,
    Cancelling,
    Paused,
    AwaitingReview,
    Retired,
    Revived,
    Replaced,
}

impl ChildLifecycle {
    pub(crate) const fn is_live(self) -> bool {
        matches!(
            self,
            Self::Active
                | Self::Working
                | Self::Cancelling
                | Self::Paused
                | Self::AwaitingReview
                | Self::Revived
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildSessionLink {
    pub(crate) session_id: SessionId,
    pub(crate) role: String,
    pub(crate) stage_attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) run_index: Option<u64>,
    pub(crate) model_selection: ModelSelection,
    pub(crate) lifecycle: ChildLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) accepted_handoff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prompt_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) replaces: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) replaced_by: Option<SessionId>,
}

impl ChildSessionLink {
    pub(crate) fn validate(&self) -> Result<()> {
        validate_text("specialist role", &self.role, MAX_ROLE_BYTES, false)?;
        let role = SpecialistRole::parse(&self.role)?;
        self.model_selection.validate()?;
        if self.model_selection != role.model_selection() {
            return Err(anyhow!(
                "specialist {} does not use its fixed model profile",
                role.label()
            ));
        }
        if self.prompt_revision.as_deref() != Some(role.prompt_revision()) {
            return Err(anyhow!(
                "specialist {} does not use the approved embedded prompt revision",
                role.label()
            ));
        }
        if let Some(handoff) = &self.accepted_handoff {
            validate_text("accepted stage handoff", handoff, MAX_HANDOFF_BYTES, true)?;
        }
        if let Some(revision) = &self.prompt_revision {
            validate_text(
                "embedded prompt revision",
                revision,
                MAX_PROMPT_REVISION_BYTES,
                false,
            )?;
        }
        if self.replaces.as_ref() == Some(&self.session_id)
            || self.replaced_by.as_ref() == Some(&self.session_id)
        {
            return Err(anyhow!("a child session cannot replace itself"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionGroupLinkage {
    version: u32,
    pub(crate) group_id: SessionId,
    pub(crate) main_session_id: SessionId,
    pub(crate) active_session_id: SessionId,
    pub(crate) children: Vec<ChildSessionLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) deepwork: Option<DeepworkState>,
}

impl SessionGroupLinkage {
    pub(crate) fn new(main_session_id: SessionId) -> Self {
        Self {
            version: LINKAGE_VERSION,
            group_id: SessionId::new(),
            active_session_id: main_session_id.clone(),
            main_session_id,
            children: Vec::new(),
            deepwork: None,
        }
    }

    pub(crate) fn child(&self, session_id: &SessionId) -> Option<&ChildSessionLink> {
        self.children
            .iter()
            .find(|child| &child.session_id == session_id)
    }

    pub(crate) fn child_mut(&mut self, session_id: &SessionId) -> Option<&mut ChildSessionLink> {
        self.children
            .iter_mut()
            .find(|child| &child.session_id == session_id)
    }

    fn migrate_legacy(&mut self) -> Result<bool> {
        let mut changed = self
            .deepwork
            .as_mut()
            .is_some_and(DeepworkState::migrate_question_history);
        let legacy_generation = self
            .children
            .iter()
            .any(|child| child.role.trim().trim_start_matches('$') == "evals");
        let current_run_index = self.deepwork.as_ref().map(|state| state.run_index);
        let mut current_sessions = std::collections::HashSet::new();
        if let Some(deepwork) = &self.deepwork {
            for accepted in deepwork.accepted_stages.values() {
                current_sessions.insert(SessionId::parse(accepted.session_id.clone())?);
            }
            current_sessions.extend(
                self.children
                    .iter()
                    .filter(|child| child.lifecycle.is_live())
                    .map(|child| child.session_id.clone()),
            );
            loop {
                let previous_len = current_sessions.len();
                for child in &self.children {
                    if current_sessions.contains(&child.session_id)
                        || child
                            .replaces
                            .as_ref()
                            .is_some_and(|session_id| current_sessions.contains(session_id))
                        || child
                            .replaced_by
                            .as_ref()
                            .is_some_and(|session_id| current_sessions.contains(session_id))
                    {
                        current_sessions.insert(child.session_id.clone());
                        current_sessions.extend(child.replaces.iter().cloned());
                        current_sessions.extend(child.replaced_by.iter().cloned());
                    }
                }
                if current_sessions.len() == previous_len {
                    break;
                }
            }
        }

        for child in &mut self.children {
            let role = SpecialistRole::parse(&child.role)?;
            if child.role != role.as_str() {
                child.role = role.as_str().to_string();
                changed = true;
            }
            if legacy_generation {
                let current_model_selection = role.model_selection();
                if child.model_selection == legacy_model_selection(role)
                    && child.model_selection != current_model_selection
                {
                    child.model_selection = current_model_selection;
                    changed = true;
                }
                if child.prompt_revision.as_deref() == Some(legacy_prompt_revision(role))
                    && child.prompt_revision.as_deref() != Some(role.prompt_revision())
                {
                    child.prompt_revision = Some(role.prompt_revision().to_string());
                    changed = true;
                }
            }
            if child.run_index.is_none()
                && current_sessions.contains(&child.session_id)
                && let Some(run_index) = current_run_index
            {
                child.run_index = Some(run_index);
                changed = true;
            }
        }
        Ok(changed)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != LINKAGE_VERSION {
            return Err(anyhow!(
                "unsupported session-group linkage version {}; expected {LINKAGE_VERSION}",
                self.version
            ));
        }
        if self.active_session_id != self.main_session_id {
            let active = self.child(&self.active_session_id).ok_or_else(|| {
                anyhow!(
                    "active session {} is not part of session group {}",
                    self.active_session_id,
                    self.group_id
                )
            })?;
            if !active.lifecycle.is_live() {
                return Err(anyhow!(
                    "active session {} is not live in session group {}",
                    self.active_session_id,
                    self.group_id
                ));
            }
        }
        if let Some(deepwork) = &self.deepwork {
            deepwork.validate()?;
        } else if self.children.iter().any(|child| child.lifecycle.is_live()) {
            return Err(anyhow!(
                "deepwork approval or completion stage cannot retain a live specialist"
            ));
        }
        let mut seen = std::collections::HashSet::with_capacity(self.children.len());
        for child in &self.children {
            child.validate()?;
            if child.session_id == self.main_session_id {
                return Err(anyhow!("Main cannot also be recorded as a child session"));
            }
            if !seen.insert(child.session_id.clone()) {
                return Err(anyhow!(
                    "child session {} appears more than once",
                    child.session_id
                ));
            }
        }
        for child in &self.children {
            match (&child.lifecycle, &child.replaced_by) {
                (ChildLifecycle::Replaced, None) => {
                    return Err(anyhow!(
                        "replaced session {} does not identify its replacement",
                        child.session_id
                    ));
                }
                (ChildLifecycle::Replaced, Some(_)) | (_, None) => {}
                (_, Some(_)) => {
                    return Err(anyhow!(
                        "session {} identifies a replacement without being replaced",
                        child.session_id
                    ));
                }
            }
            if let Some(replaced_by) = &child.replaced_by {
                let replacement = self.child(replaced_by).ok_or_else(|| {
                    anyhow!(
                        "replacement session {replaced_by} is not part of session group {}",
                        self.group_id
                    )
                })?;
                if replacement.replaces.as_ref() != Some(&child.session_id)
                    || replacement.run_index != child.run_index
                    || SpecialistRole::parse(&replacement.role)?
                        != SpecialistRole::parse(&child.role)?
                    || child.stage_attempt.checked_add(1) != Some(replacement.stage_attempt)
                {
                    return Err(anyhow!(
                        "replacement link between {} and {replaced_by} is not reciprocal",
                        child.session_id
                    ));
                }
            }
            if let Some(replaces) = &child.replaces {
                let replaced = self.child(replaces).ok_or_else(|| {
                    anyhow!(
                        "replaced session {replaces} is not part of session group {}",
                        self.group_id
                    )
                })?;
                if replaced.replaced_by.as_ref() != Some(&child.session_id) {
                    return Err(anyhow!(
                        "replacement link between {replaces} and {} is not reciprocal",
                        child.session_id
                    ));
                }
            }
        }
        if let Some(deepwork) = &self.deepwork {
            for accepted in deepwork.accepted_stages.values() {
                let session_id = SessionId::parse(accepted.session_id.clone())?;
                let child = self.child(&session_id).ok_or_else(|| {
                    anyhow!(
                        "accepted {} stage references missing child session {session_id}",
                        accepted.role.label()
                    )
                })?;
                if SpecialistRole::parse(&child.role)? != accepted.role
                    || child.run_index != Some(deepwork.run_index)
                    || child.stage_attempt != accepted.stage_attempt
                    || child.accepted_handoff.as_deref() != Some(accepted.accepted_handoff.as_str())
                    || child.lifecycle != ChildLifecycle::Retired
                {
                    return Err(anyhow!(
                        "accepted {} stage does not match its retired child linkage",
                        accepted.role.label()
                    ));
                }
            }
            let live = self
                .children
                .iter()
                .filter(|child| child.lifecycle.is_live())
                .collect::<Vec<_>>();
            if live.len() > 1 {
                return Err(anyhow!(
                    "deepwork session group contains more than one live specialist"
                ));
            }
            match (deepwork.stage.expected_specialist(), live.first()) {
                (Some(expected), Some(child))
                    if child.run_index != Some(deepwork.run_index)
                        || SpecialistRole::parse(&child.role)? != expected =>
                {
                    return Err(anyhow!(
                        "live specialist does not match deepwork stage {:?}",
                        deepwork.stage
                    ));
                }
                (None, Some(_)) => {
                    return Err(anyhow!(
                        "deepwork approval or completion stage cannot retain a live specialist"
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SessionGroupStore {
    path: PathBuf,
}

impl SessionGroupStore {
    pub(crate) fn for_main(main_session_id: &SessionId) -> Result<Self> {
        Ok(Self::in_root(
            &crate::rollout::state_root()?,
            main_session_id,
        ))
    }

    pub(crate) fn in_root(root: &Path, main_session_id: &SessionId) -> Self {
        Self {
            path: root
                .join(GROUPS_DIRECTORY)
                .join(format!("{main_session_id}.json")),
        }
    }

    pub(crate) fn load(&self, main_session_id: &SessionId) -> Result<Option<SessionGroupLinkage>> {
        let Some(mut linkage) =
            state_file::read_json::<SessionGroupLinkage>(&self.path, MAX_LINKAGE_BYTES)?
        else {
            return Ok(None);
        };
        let migrated = linkage.migrate_legacy()?;
        linkage.validate()?;
        if &linkage.main_session_id != main_session_id {
            return Err(anyhow!(
                "session-group linkage {} belongs to Main session {}, not {main_session_id}",
                self.path.display(),
                linkage.main_session_id
            ));
        }
        if migrated {
            self.save(&linkage)?;
        }
        Ok(Some(linkage))
    }

    pub(crate) fn save(&self, linkage: &SessionGroupLinkage) -> Result<()> {
        linkage.validate()?;
        let replacement = linkage.clone();
        let initial = replacement.clone();
        let expected_main = replacement.main_session_id.clone();
        state_file::update_json(
            &self.path,
            MAX_LINKAGE_BYTES,
            |path| {
                let Some(existing) =
                    state_file::read_json::<SessionGroupLinkage>(path, MAX_LINKAGE_BYTES)?
                else {
                    return Ok(initial);
                };
                let mut existing = existing;
                existing.migrate_legacy()?;
                existing.validate()?;
                if existing.main_session_id != expected_main {
                    return Err(anyhow!(
                        "session-group linkage {} belongs to Main session {}, not {}",
                        path.display(),
                        existing.main_session_id,
                        expected_main
                    ));
                }
                Ok(existing)
            },
            |document| {
                *document = replacement;
                Ok(state_file::StateChange::Changed)
            },
        )
        .with_context(|| {
            format!(
                "failed to persist session-group linkage {}",
                self.path.display()
            )
        })
    }
}

fn legacy_model_selection(role: SpecialistRole) -> ModelSelection {
    match role {
        SpecialistRole::Acceptance | SpecialistRole::Worker => {
            ModelSelection::from_identity("gpt-5.6-sol", ReasoningEffort::XHigh)
        }
        SpecialistRole::Manifest => {
            ModelSelection::from_identity("gpt-5.6-luna", ReasoningEffort::Max)
        }
        SpecialistRole::Reviewer => role.model_selection(),
    }
}

const fn legacy_prompt_revision(role: SpecialistRole) -> &'static str {
    match role {
        SpecialistRole::Acceptance => LEGACY_EVALS_PROMPT_REVISION,
        SpecialistRole::Manifest => LEGACY_MANIFEST_PROMPT_REVISION,
        SpecialistRole::Worker => LEGACY_WORKER_PROMPT_REVISION,
        SpecialistRole::Reviewer => LEGACY_REVIEWER_PROMPT_REVISION,
    }
}

fn validate_text(label: &str, value: &str, maximum_bytes: usize, allow_empty: bool) -> Result<()> {
    if !allow_empty && value.trim().is_empty() {
        return Err(anyhow!("{label} cannot be empty"));
    }
    if value.len() > maximum_bytes {
        return Err(anyhow!("{label} exceeds the {maximum_bytes}-byte limit"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "bettercodex-session-group-{}",
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

    fn child_link(
        role: SpecialistRole,
        lifecycle: ChildLifecycle,
        run_index: u64,
    ) -> ChildSessionLink {
        ChildSessionLink {
            session_id: SessionId::new(),
            role: role.as_str().to_string(),
            stage_attempt: 0,
            run_index: Some(run_index),
            model_selection: role.model_selection(),
            lifecycle,
            accepted_handoff: None,
            prompt_revision: Some(role.prompt_revision().to_string()),
            replaces: None,
            replaced_by: None,
        }
    }

    #[test]
    fn ordinary_linkage_rejects_live_children_and_replacement_links_are_reciprocal() {
        let main = SessionId::new();
        let mut ordinary = SessionGroupLinkage::new(main.clone());
        ordinary.children.push(child_link(
            SpecialistRole::Worker,
            ChildLifecycle::Working,
            0,
        ));
        assert!(ordinary.validate().is_err());

        let mut replacement = SessionGroupLinkage::new(main);
        let mut old = child_link(SpecialistRole::Worker, ChildLifecycle::Replaced, 0);
        let mut new = child_link(SpecialistRole::Worker, ChildLifecycle::Retired, 0);
        new.stage_attempt = 1;
        old.replaced_by = Some(new.session_id.clone());
        new.replaces = Some(old.session_id.clone());
        replacement.children = vec![old, new];
        assert!(replacement.validate().is_ok());

        let valid_replacement = replacement.clone();
        replacement.children[1].replaces = None;
        assert!(replacement.validate().is_err());

        let mut cross_run = valid_replacement.clone();
        cross_run.children[1].run_index = Some(1);
        assert!(cross_run.validate().is_err());

        let mut skipped_attempt = valid_replacement.clone();
        skipped_attempt.children[1].stage_attempt = 2;
        assert!(skipped_attempt.validate().is_err());

        let mut changed_role = valid_replacement;
        changed_role.children[1].role = SpecialistRole::Reviewer.as_str().to_string();
        changed_role.children[1].model_selection = SpecialistRole::Reviewer.model_selection();
        changed_role.children[1].prompt_revision =
            Some(SpecialistRole::Reviewer.prompt_revision().to_string());
        assert!(changed_role.validate().is_err());
    }

    #[test]
    fn maximum_question_history_persists_within_the_sidecar_limit() {
        use crate::ask_user_question::MAX_FREE_TEXT_BYTES;
        use crate::ask_user_question::MAX_OPTION_LABEL_CHARS;
        use crate::ask_user_question::MAX_OPTIONS;
        use crate::ask_user_question::MAX_QUESTION_CHARS;
        use crate::ask_user_question::MAX_QUESTIONS;
        use crate::deepwork::DeepworkAnswer;
        use crate::deepwork::DeepworkQuestionBatch;

        let root = TestRoot::new();
        let repository = root.0.join("repository");
        std::fs::create_dir(&repository)
            .unwrap_or_else(|error| panic!("repository should be created: {error}"));
        let main = SessionId::new();
        let store = SessionGroupStore::in_root(&root.0, &main);
        let mut linkage = SessionGroupLinkage::new(main.clone());
        let mut state = DeepworkState::activate(&repository, "x".repeat(10 * 1024))
            .unwrap_or_else(|error| panic!("deepwork should activate: {error}"));
        let question = "😀".repeat(MAX_QUESTION_CHARS);
        let selected_options = ['😀', '😁', '😂', '😃', '😄', '😅']
            .into_iter()
            .take(MAX_OPTIONS)
            .map(|suffix| format!("{}{}", "😀".repeat(MAX_OPTION_LABEL_CHARS - 1), suffix))
            .collect::<Vec<_>>();
        for _ in 0..64 {
            state
                .record_question_batch(DeepworkQuestionBatch {
                    questions: vec![question.clone(); MAX_QUESTIONS],
                    answers: (0..MAX_QUESTIONS)
                        .map(|_| DeepworkAnswer {
                            question: question.clone(),
                            selected_options: selected_options.clone(),
                            free_text: Some("\"".repeat(MAX_FREE_TEXT_BYTES)),
                        })
                        .collect(),
                    cancelled: false,
                    truncated: false,
                })
                .unwrap_or_else(|error| panic!("maximum batch should persist: {error}"));
        }
        assert!(state.question_history_truncated);
        linkage.deepwork = Some(state);

        store
            .save(&linkage)
            .unwrap_or_else(|error| panic!("full question history should save: {error}"));
        let bytes = std::fs::metadata(&store.path)
            .unwrap_or_else(|error| panic!("saved sidecar should exist: {error}"))
            .len();
        assert!(bytes <= MAX_LINKAGE_BYTES as u64);
        let loaded = store
            .load(&main)
            .unwrap_or_else(|error| panic!("full question history should load: {error}"))
            .unwrap_or_else(|| panic!("saved linkage should exist"));
        assert_eq!(loaded, linkage);
    }

    #[test]
    fn legacy_evals_linkage_migrates_known_profiles_and_rejects_malformed_links() {
        let root = TestRoot::new();
        let main_session_id = SessionId::parse(uuid::Uuid::from_u128(1).hyphenated().to_string())
            .unwrap_or_else(|error| panic!("main session ID should parse: {error}"));
        let acceptance_session = uuid::Uuid::from_u128(2).hyphenated().to_string();
        let manifest_session = uuid::Uuid::from_u128(3).hyphenated().to_string();
        let replaced_worker_session = uuid::Uuid::from_u128(5).hyphenated().to_string();
        let worker_session = uuid::Uuid::from_u128(6).hyphenated().to_string();
        let repository = root.0.join("repository");
        let workspace = repository.join(".deepwork/7");
        let store = SessionGroupStore::in_root(&root.0, &main_session_id);
        let document = serde_json::json!({
            "version": LINKAGE_VERSION,
            "group_id": uuid::Uuid::from_u128(4).hyphenated().to_string(),
            "main_session_id": main_session_id.to_string(),
            "active_session_id": main_session_id.to_string(),
            "children": [
                {
                    "session_id": acceptance_session,
                    "role": "evals",
                    "stage_attempt": 1,
                    "model_selection": {
                        "model": "gpt-5.6-sol",
                        "reasoning_effort": "xhigh"
                    },
                    "lifecycle": "retired",
                    "accepted_handoff": "accepted completion contract",
                    "prompt_revision": LEGACY_EVALS_PROMPT_REVISION
                },
                {
                    "session_id": manifest_session,
                    "role": "manifest",
                    "stage_attempt": 1,
                    "model_selection": {
                        "model": "gpt-5.6-luna",
                        "reasoning_effort": "max"
                    },
                    "lifecycle": "retired",
                    "accepted_handoff": "accepted manifest",
                    "prompt_revision": LEGACY_MANIFEST_PROMPT_REVISION
                },
                {
                    "session_id": replaced_worker_session,
                    "role": "worker",
                    "stage_attempt": 0,
                    "model_selection": {
                        "model": "gpt-5.6-sol",
                        "reasoning_effort": "xhigh"
                    },
                    "lifecycle": "replaced",
                    "prompt_revision": LEGACY_WORKER_PROMPT_REVISION,
                    "replaced_by": worker_session
                },
                {
                    "session_id": worker_session,
                    "role": "worker",
                    "stage_attempt": 1,
                    "model_selection": {
                        "model": "gpt-5.6-sol",
                        "reasoning_effort": "xhigh"
                    },
                    "lifecycle": "working",
                    "prompt_revision": LEGACY_WORKER_PROMPT_REVISION,
                    "replaces": replaced_worker_session
                }
            ],
            "deepwork": {
                "version": 1,
                "repository_root": repository,
                "run_index": 7,
                "workspace": workspace,
                "original_task": "preserve behavior",
                "stage": "readiness",
                "interview_approved": true,
                "readiness_approved": false,
                "canonical_contract": "SUCCESS CRITERIA\n- preserve behavior",
                "accepted_stages": {
                    "evals": {
                        "role": "evals",
                        "session_id": acceptance_session,
                        "stage_attempt": 1,
                        "accepted_handoff": "accepted completion contract",
                        "artifacts": [],
                        "remaining_risks": ""
                    },
                    "manifest": {
                        "role": "manifest",
                        "session_id": manifest_session,
                        "stage_attempt": 1,
                        "accepted_handoff": "accepted manifest",
                        "artifacts": [],
                        "remaining_risks": ""
                    }
                }
            }
        });
        let parent = store
            .path
            .parent()
            .unwrap_or_else(|| panic!("store path should have a parent"));
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|error| panic!("store parent should be created: {error}"));
        std::fs::write(
            &store.path,
            serde_json::to_vec_pretty(&document)
                .unwrap_or_else(|error| panic!("legacy linkage should serialize: {error}")),
        )
        .unwrap_or_else(|error| panic!("legacy linkage should be written: {error}"));

        let linkage = store
            .load(&main_session_id)
            .unwrap_or_else(|error| panic!("legacy linkage should load: {error}"))
            .unwrap_or_else(|| panic!("legacy linkage should exist"));

        assert_eq!(linkage.children[0].role, "acceptance");
        for child in &linkage.children {
            let role = SpecialistRole::parse(&child.role)
                .unwrap_or_else(|error| panic!("migrated role should parse: {error}"));
            assert_eq!(child.model_selection, role.model_selection());
            assert_eq!(
                child.prompt_revision.as_deref(),
                Some(role.prompt_revision())
            );
            assert_eq!(child.run_index, Some(7));
        }
        assert!(linkage.validate().is_ok());

        let persisted = std::fs::read(&store.path)
            .unwrap_or_else(|error| panic!("migrated linkage should be readable: {error}"));
        let persisted: serde_json::Value = serde_json::from_slice(&persisted)
            .unwrap_or_else(|error| panic!("migrated linkage should be JSON: {error}"));
        assert_eq!(persisted["children"][0]["role"], "acceptance");
        assert_eq!(persisted["children"][0]["run_index"], 7);
        assert!(
            persisted["deepwork"]["accepted_stages"]
                .get("acceptance")
                .is_some()
        );
        assert!(
            persisted["deepwork"]["accepted_stages"]
                .get("evals")
                .is_none()
        );

        let mut multiple_live = linkage.clone();
        multiple_live.children.push(child_link(
            SpecialistRole::Worker,
            ChildLifecycle::Working,
            7,
        ));
        assert!(multiple_live.validate().is_err());

        let mut cross_run = linkage;
        cross_run.children[0].run_index = Some(6);
        assert!(cross_run.validate().is_err());

        let malformed_main = SessionId::parse(uuid::Uuid::from_u128(7).hyphenated().to_string())
            .unwrap_or_else(|error| panic!("malformed fixture Main ID should parse: {error}"));
        let malformed_store = SessionGroupStore::in_root(&root.0, &malformed_main);
        let mut malformed = document.clone();
        malformed["main_session_id"] = serde_json::json!(malformed_main.to_string());
        malformed["active_session_id"] = serde_json::json!(malformed_main.to_string());
        malformed["children"][1]["prompt_revision"] =
            serde_json::json!("sha256:unrecognized-legacy-manifest");
        let malformed_bytes = serde_json::to_vec_pretty(&malformed)
            .unwrap_or_else(|error| panic!("malformed fixture should serialize: {error}"));
        std::fs::write(&malformed_store.path, &malformed_bytes)
            .unwrap_or_else(|error| panic!("malformed fixture should be written: {error}"));

        assert!(malformed_store.load(&malformed_main).is_err());
        assert_eq!(
            std::fs::read(&malformed_store.path).unwrap_or_else(|error| panic!(
                "malformed fixture should remain readable: {error}"
            )),
            malformed_bytes,
            "a rejected legacy sidecar must not be normalized or reserialized"
        );

        let malformed_model_main = SessionId::parse(
            uuid::Uuid::from_u128(8).hyphenated().to_string(),
        )
        .unwrap_or_else(|error| panic!("malformed model fixture Main ID should parse: {error}"));
        let malformed_model_store = SessionGroupStore::in_root(&root.0, &malformed_model_main);
        let mut malformed_model = document;
        malformed_model["main_session_id"] = serde_json::json!(malformed_model_main.to_string());
        malformed_model["active_session_id"] = serde_json::json!(malformed_model_main.to_string());
        malformed_model["children"][1]["model_selection"] = serde_json::json!({
            "model": "gpt-5.6-sol",
            "reasoning_effort": "high"
        });
        let malformed_model_bytes = serde_json::to_vec_pretty(&malformed_model)
            .unwrap_or_else(|error| panic!("malformed model fixture should serialize: {error}"));
        std::fs::write(&malformed_model_store.path, &malformed_model_bytes)
            .unwrap_or_else(|error| panic!("malformed model fixture should be written: {error}"));

        assert!(malformed_model_store.load(&malformed_model_main).is_err());
        assert_eq!(
            std::fs::read(&malformed_model_store.path).unwrap_or_else(|error| panic!(
                "malformed model fixture should remain readable: {error}"
            )),
            malformed_model_bytes,
            "a rejected legacy model must not be normalized or reserialized"
        );
    }
}
