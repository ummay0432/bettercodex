//! Stable multi-session identity and bounded persisted linkage.
//!
//! Agent rollouts remain ordinary per-session JSONL journals. This sidecar records only the
//! coordinator-owned relationship between a Main session and its independent child sessions so a
//! cold resume can reconstruct the group without changing the established saved-session path.

use crate::deepwork::DeepworkState;
use crate::deepwork::SpecialistRole;
use crate::model::ModelSelection;
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
                if replacement.replaces.as_ref() != Some(&child.session_id) {
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
                    if SpecialistRole::parse(&child.role)? != expected =>
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
        let Some(linkage) =
            state_file::read_json::<SessionGroupLinkage>(&self.path, MAX_LINKAGE_BYTES)?
        else {
            return Ok(None);
        };
        linkage.validate()?;
        if &linkage.main_session_id != main_session_id {
            return Err(anyhow!(
                "session-group linkage {} belongs to Main session {}, not {main_session_id}",
                self.path.display(),
                linkage.main_session_id
            ));
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

fn validate_text(label: &str, value: &str, maximum_bytes: usize, allow_empty: bool) -> Result<()> {
    if !allow_empty && value.trim().is_empty() {
        return Err(anyhow!("{label} cannot be empty"));
    }
    if value.len() > maximum_bytes {
        return Err(anyhow!("{label} exceeds the {maximum_bytes}-byte limit"));
    }
    Ok(())
}
