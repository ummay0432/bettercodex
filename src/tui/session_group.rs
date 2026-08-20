use super::SessionPromptHistory;
use super::TurnCompletion;
use super::TurnResult;
use super::TurnTaskState;
use super::prompt_history_for_agent;
use crate::agent::Agent;
use crate::agent::TurnControl;
use crate::agent::TurnHandle;
use crate::context::ContextSnapshot;
use crate::deepwork::DeepworkLiveSpecialist;
use crate::deepwork::DeepworkQuestionBatch;
use crate::deepwork::DeepworkStage;
use crate::deepwork::DeepworkState;
use crate::deepwork::DeepworkStatus;
use crate::deepwork::SpecialistEvent;
use crate::deepwork::SpecialistEventKind;
use crate::deepwork::SpecialistRole;
use crate::deepwork::bound_status;
use crate::deepwork::bounded_event_text;
use crate::events::AgentEvent;
use crate::events::SteerId;
use crate::input::UserInput;
use crate::input::UserPrompt;
use crate::model::ModelSelection;
use crate::prompt_history::PromptHistory;
use crate::prompt_history::PromptHistoryReader;
use crate::rollout::ResumeSelector;
use crate::service_tier::ServiceTier;
use crate::session_group::ChildLifecycle;
use crate::session_group::ChildSessionLink;
use crate::session_group::SessionGroupLinkage;
use crate::session_group::SessionGroupStore;
use crate::session_group::SessionId;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::view::View;

pub(super) type RoutedAgentEvent = (SessionId, AgentEvent);
pub(super) type RoutedTurnResult = (SessionId, std::result::Result<TurnResult, String>);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChildLaunch {
    pub(crate) role: SpecialistRole,
    pub(crate) stage_attempt: u32,
    pub(crate) accepted_handoff: Option<String>,
    pub(crate) initial_message: String,
}

pub(super) enum AgentSlotKind {
    Main,
    Child {
        role: SpecialistRole,
        stage_attempt: u32,
        fixed_model: ModelSelection,
    },
}

pub(super) struct AgentSlot {
    pub(super) id: SessionId,
    pub(super) session_id: String,
    pub(super) cwd: PathBuf,
    pub(super) kind: AgentSlotKind,
    pub(super) agent: Option<Agent>,
    pub(super) turn: TurnTaskState,
    pub(super) turn_handle: Option<TurnHandle>,
    pub(super) view: View,
    pub(super) context_snapshot: ContextSnapshot,
    pub(super) forked_from: Option<String>,
    pub(super) instruction_source_paths: Vec<PathBuf>,
    pub(super) model_selection: ModelSelection,
    pub(super) service_tier: ServiceTier,
    pub(super) prompt_history: Option<PromptHistory>,
    pub(super) prompt_history_reader: Option<PromptHistoryReader>,
    pub(super) prompt_history_task: Option<super::PromptHistoryTask>,
    pub(super) prompt_history_exclusions: HashSet<String>,
    pub(super) pending_operator_contexts: VecDeque<String>,
    pub(super) operator_context_steers: Vec<(SteerId, String)>,
    pub(super) turn_started_at: Option<Instant>,
    meaningful_events: VecDeque<SpecialistEvent>,
    waiters: VecDeque<oneshot::Sender<Result<SpecialistEvent>>>,
}

impl AgentSlot {
    pub(super) fn main(agent: Agent) -> Result<Self> {
        Self::from_agent(agent, AgentSlotKind::Main)
    }

    fn child(agent: Agent, link: &ChildSessionLink) -> Result<Self> {
        Self::from_agent(agent, Self::child_kind(link))
    }

    fn child_kind(link: &ChildSessionLink) -> AgentSlotKind {
        AgentSlotKind::Child {
            role: SpecialistRole::parse(&link.role)
                .unwrap_or_else(|_| unreachable!("validated specialist role")),
            stage_attempt: link.stage_attempt,
            fixed_model: link.model_selection.clone(),
        }
    }

    fn from_agent(agent: Agent, kind: AgentSlotKind) -> Result<Self> {
        let history = prompt_history_for_agent(&agent)?;
        Self::from_agent_with_prompt_history(agent, kind, Some(history))
    }

    fn from_agent_with_prompt_history(
        mut agent: Agent,
        kind: AgentSlotKind,
        history: Option<SessionPromptHistory>,
    ) -> Result<Self> {
        let cwd = agent.cwd().to_path_buf();
        let id = SessionId::parse(agent.session_id().to_string())?;
        let mut view = View::new(&cwd);
        let model_selection = agent.model_selection().clone();
        let service_tier = agent.service_tier();
        let context_snapshot = agent.context_snapshot();
        view.set_model_selection(model_selection.clone());
        view.set_service_tier(service_tier);
        view.sync_context_snapshot(&context_snapshot);
        view.replay_transcript(agent.take_resumed_transcript());
        view.set_skills(agent.skills().to_vec());
        for warning in agent.skill_warnings() {
            view.add_notice(format!("Skill warning: {warning}"));
        }
        let (
            prompt_history,
            prompt_history_reader,
            prompt_history_exclusions,
            composer_history,
            has_persistent_history,
        ) = match history {
            Some(SessionPromptHistory {
                writer,
                reader,
                exclusions: mut prompt_history_exclusions,
                composer_history,
            }) => {
                let has_persistent_history = reader.has_more();
                if !has_persistent_history {
                    prompt_history_exclusions.clear();
                }
                (
                    Some(writer),
                    Some(reader),
                    prompt_history_exclusions,
                    composer_history,
                    has_persistent_history,
                )
            }
            None => (None, None, HashSet::new(), Vec::new(), false),
        };
        view.seed_prompt_history(composer_history, has_persistent_history);
        Ok(Self {
            session_id: id.to_string(),
            id,
            cwd,
            kind,
            forked_from: agent.forked_from().map(str::to_string),
            instruction_source_paths: agent.instruction_source_paths().to_vec(),
            model_selection,
            service_tier,
            context_snapshot,
            agent: Some(agent),
            turn: TurnTaskState::Idle,
            turn_handle: None,
            view,
            prompt_history,
            prompt_history_reader,
            prompt_history_task: None,
            prompt_history_exclusions,
            pending_operator_contexts: VecDeque::new(),
            operator_context_steers: Vec::new(),
            turn_started_at: None,
            meaningful_events: VecDeque::new(),
            waiters: VecDeque::new(),
        })
    }

    pub(super) fn set_context_snapshot(&mut self, snapshot: ContextSnapshot) {
        self.view.sync_context_snapshot(&snapshot);
        self.context_snapshot = snapshot;
    }

    pub(super) fn is_child(&self) -> bool {
        matches!(self.kind, AgentSlotKind::Child { .. })
    }

    pub(super) fn child_identity(&self) -> Option<(SpecialistRole, u32, &ModelSelection)> {
        match &self.kind {
            AgentSlotKind::Main => None,
            AgentSlotKind::Child {
                role,
                stage_attempt,
                fixed_model,
            } => Some((*role, *stage_attempt, fixed_model)),
        }
    }

    pub(super) fn deliver_meaningful_event(&mut self, mut event: SpecialistEvent) {
        event.message = bounded_event_text(&event.message);
        event.final_result = event.final_result.as_deref().map(bounded_event_text);
        while let Some(waiter) = self.waiters.pop_front() {
            if waiter.send(Ok(event.clone())).is_ok() {
                return;
            }
        }
        self.meaningful_events.push_back(event);
    }

    fn wait(&mut self, response: oneshot::Sender<Result<SpecialistEvent>>) {
        if let Some(event) = self.meaningful_events.pop_front() {
            let _ = response.send(Ok(event));
        } else {
            self.waiters.push_back(response);
        }
    }

    fn fail_waiters(&mut self, error: &str) {
        for waiter in self.waiters.drain(..) {
            let _ = waiter.send(Err(anyhow!(error.to_string())));
        }
    }
}

impl Drop for AgentSlot {
    fn drop(&mut self) {
        if let Some(turn) = self.turn_handle.take() {
            turn.cancel();
        }
        if let TurnTaskState::Running(task) = std::mem::replace(&mut self.turn, TurnTaskState::Idle)
        {
            task.abort();
        }
        if let Some(task) = self.prompt_history_task.take() {
            task.abort();
        }
        self.fail_waiters("the specialist session is no longer live");
    }
}

pub(super) struct SessionGroup {
    pub(super) linkage: SessionGroupLinkage,
    store: SessionGroupStore,
    slots: BTreeMap<SessionId, AgentSlot>,
    pub(super) agent_events_tx: UnboundedSender<RoutedAgentEvent>,
    pub(super) turn_results_tx: UnboundedSender<RoutedTurnResult>,
}

impl SessionGroup {
    pub(super) fn new(
        mut main: AgentSlot,
    ) -> Result<(
        Self,
        UnboundedReceiver<RoutedAgentEvent>,
        UnboundedReceiver<RoutedTurnResult>,
    )> {
        let main_id = main.id.clone();
        let store = SessionGroupStore::for_main(&main_id)?;
        let linkage = match store.load(&main_id) {
            Ok(Some(linkage)) => linkage,
            Ok(None) => SessionGroupLinkage::new(main_id.clone()),
            Err(error) => {
                main.view.add_notice(format!(
                    "Session-group linkage could not be loaded; Main resumed without children: {error:#}"
                ));
                SessionGroupLinkage::new(main_id.clone())
            }
        };
        let (mut group, agent_events, turn_results) = Self::from_parts(main, store, linkage);
        group.restore_main_deepwork_access();
        group.restore_linked_children();
        Ok((group, agent_events, turn_results))
    }

    fn from_parts(
        main: AgentSlot,
        store: SessionGroupStore,
        linkage: SessionGroupLinkage,
    ) -> (
        Self,
        UnboundedReceiver<RoutedAgentEvent>,
        UnboundedReceiver<RoutedTurnResult>,
    ) {
        let main_id = main.id.clone();
        let (agent_events_tx, agent_events) = unbounded_channel();
        let (turn_results_tx, turn_results) = unbounded_channel();
        (
            Self {
                linkage,
                store,
                slots: BTreeMap::from([(main_id, main)]),
                agent_events_tx,
                turn_results_tx,
            },
            agent_events,
            turn_results,
        )
    }

    fn restore_main_deepwork_access(&mut self) {
        if self.linkage.deepwork.is_none() {
            return;
        }
        let main = self.main_mut();
        let Some(agent) = main.agent.as_mut() else {
            return;
        };
        agent.restore_deepwork_access();
        let snapshot = agent.context_snapshot();
        main.set_context_snapshot(snapshot);
    }

    pub(super) fn main_id(&self) -> &SessionId {
        &self.linkage.main_session_id
    }

    pub(super) fn active_id(&self) -> &SessionId {
        &self.linkage.active_session_id
    }

    pub(super) fn is_main_active(&self) -> bool {
        self.active_id() == self.main_id()
    }

    pub(super) fn active(&self) -> &AgentSlot {
        self.slots
            .get(self.active_id())
            .unwrap_or_else(|| unreachable!("active session is always live"))
    }

    pub(super) fn active_mut(&mut self) -> &mut AgentSlot {
        let active_id = self.linkage.active_session_id.clone();
        self.slots
            .get_mut(&active_id)
            .unwrap_or_else(|| unreachable!("active session is always live"))
    }

    pub(super) fn activate(&mut self, session_id: &SessionId) -> Result<()> {
        let slot = self
            .slots
            .get(session_id)
            .context("selected session is not live")?;
        if slot.is_child()
            && !self
                .linkage
                .child(session_id)
                .is_some_and(|child| child.lifecycle.is_live())
        {
            return Err(anyhow!("selected specialist session is not live"));
        }
        if self.active_id() == session_id {
            return Ok(());
        }
        let mut proposed = self.linkage.clone();
        proposed.active_session_id = session_id.clone();
        self.store.save(&proposed)?;
        self.linkage = proposed;
        Ok(())
    }

    pub(super) fn destination_ids(&self) -> Vec<SessionId> {
        let active_id = self.active_id();
        let mut destinations = Vec::new();
        if active_id != self.main_id() {
            destinations.push(self.main_id().clone());
        }
        let mut children = self
            .linkage
            .children
            .iter()
            .enumerate()
            .filter(|(_, child)| {
                &child.session_id != active_id
                    && child.lifecycle.is_live()
                    && self.slots.contains_key(&child.session_id)
            })
            .collect::<Vec<_>>();
        children.sort_by_key(|(index, child)| {
            (specialist_order(&child.role), child.stage_attempt, *index)
        });
        destinations.extend(
            children
                .into_iter()
                .map(|(_, child)| child.session_id.clone()),
        );
        destinations
    }

    pub(super) fn main(&self) -> &AgentSlot {
        self.slots
            .get(self.main_id())
            .unwrap_or_else(|| unreachable!("session group always owns Main"))
    }

    pub(super) fn main_mut(&mut self) -> &mut AgentSlot {
        let main_id = self.linkage.main_session_id.clone();
        self.slots
            .get_mut(&main_id)
            .unwrap_or_else(|| unreachable!("session group always owns Main"))
    }

    pub(super) fn slot(&self, session_id: &SessionId) -> Option<&AgentSlot> {
        self.slots.get(session_id)
    }

    pub(super) fn slot_mut(&mut self, session_id: &SessionId) -> Option<&mut AgentSlot> {
        self.slots.get_mut(session_id)
    }

    pub(super) fn live_session_ids(&self) -> Vec<SessionId> {
        self.slots.keys().cloned().collect()
    }

    pub(super) fn deepwork_status(&self) -> Result<DeepworkStatus> {
        let mut status = self
            .linkage
            .deepwork
            .as_ref()
            .context("no `$deepwork` run is active for this Main session")?
            .status();
        status.live_specialist = self
            .linkage
            .children
            .iter()
            .find(|child| child.lifecycle.is_live())
            .map(|child| -> Result<DeepworkLiveSpecialist> {
                Ok(DeepworkLiveSpecialist {
                    session_id: child.session_id.to_string(),
                    role: SpecialistRole::parse(&child.role)?,
                    stage_attempt: child.stage_attempt,
                    lifecycle: child.lifecycle,
                })
            })
            .transpose()?;
        bound_status(&mut status);
        Ok(status)
    }

    pub(super) fn activate_deepwork(
        &mut self,
        repository_root: &Path,
        original_task: String,
    ) -> Result<DeepworkStatus> {
        let mut proposed = self.linkage.clone();
        let state = match proposed.deepwork.take() {
            Some(state) if state.stage != DeepworkStage::Completed => {
                state.ensure_workspace()?;
                state
            }
            _ => DeepworkState::activate(repository_root, original_task)?,
        };
        let status = state.status();
        proposed.deepwork = Some(state);
        self.store.save(&proposed)?;
        self.linkage = proposed;
        Ok(status)
    }

    pub(super) fn approve_deepwork_interview(
        &mut self,
        contract: String,
    ) -> Result<DeepworkStatus> {
        self.update_deepwork(|state| state.approve_interview(contract))
    }

    pub(super) fn approve_deepwork_readiness(
        &mut self,
        contract: String,
    ) -> Result<DeepworkStatus> {
        self.update_deepwork(|state| state.approve_readiness(contract))
    }

    pub(super) fn record_deepwork_question_batch(
        &mut self,
        batch: DeepworkQuestionBatch,
    ) -> Result<()> {
        self.update_deepwork(|state| state.record_question_batch(batch))
            .map(|_| ())
    }

    fn update_deepwork(
        &mut self,
        update: impl FnOnce(&mut DeepworkState) -> Result<()>,
    ) -> Result<DeepworkStatus> {
        let mut proposed = self.linkage.clone();
        let state = proposed
            .deepwork
            .as_mut()
            .context("no `$deepwork` run is active for this Main session")?;
        update(state)?;
        state.validate()?;
        let status = state.status();
        self.store.save(&proposed)?;
        self.linkage = proposed;
        Ok(status)
    }

    pub(super) fn start_deepwork_child(
        &mut self,
        cwd: &Path,
        role: SpecialistRole,
        handoff: String,
    ) -> Result<SessionId> {
        let launch = self.deepwork_launch(role, handoff)?;
        self.start_child(cwd, launch)
    }

    fn deepwork_launch(&self, role: SpecialistRole, handoff: String) -> Result<ChildLaunch> {
        let has_live_child =
            self.linkage.children.iter().any(|child| {
                child.lifecycle.is_live() && self.slots.contains_key(&child.session_id)
            });
        self.linkage
            .deepwork
            .as_ref()
            .context("no `$deepwork` run is active for this Main session")?
            .validate_start(role, has_live_child, &handoff)?;
        let stage_attempt = self
            .linkage
            .children
            .iter()
            .filter(|child| SpecialistRole::parse(&child.role).ok() == Some(role))
            .map(|child| child.stage_attempt)
            .max()
            .map_or(0, |attempt| attempt.saturating_add(1));
        Ok(ChildLaunch {
            role,
            stage_attempt,
            accepted_handoff: None,
            initial_message: handoff,
        })
    }

    pub(super) fn accept_and_retire_deepwork_child(
        &mut self,
        session_id: &SessionId,
        accepted_handoff: String,
        artifacts: Vec<String>,
        remaining_risks: String,
    ) -> Result<DeepworkStatus> {
        let slot = self
            .slots
            .get_mut(session_id)
            .context("specialist runtime is not live")?;
        if !slot.is_child() {
            return Err(anyhow!("Main cannot be retired"));
        }
        if slot.turn.is_active() {
            return Err(anyhow!(
                "specialist session {session_id} cannot be accepted while working"
            ));
        }
        let (role, stage_attempt, _) = slot
            .child_identity()
            .unwrap_or_else(|| unreachable!("child slot identity"));
        if let Some(agent) = slot.agent.as_mut() {
            super::persist_session_transcript(&slot.view, agent)?;
        }

        let mut proposed = self.linkage.clone();
        let state = proposed
            .deepwork
            .as_mut()
            .context("no `$deepwork` run is active for this Main session")?;
        state.accept_stage(
            role,
            session_id.to_string(),
            stage_attempt,
            accepted_handoff.clone(),
            artifacts,
            remaining_risks,
        )?;
        let link = proposed
            .child_mut(session_id)
            .context("specialist session is not linked to this group")?;
        link.lifecycle = ChildLifecycle::Retired;
        link.accepted_handoff = Some(accepted_handoff);
        if proposed.active_session_id == *session_id {
            proposed.active_session_id = proposed.main_session_id.clone();
        }
        let status = proposed
            .deepwork
            .as_ref()
            .unwrap_or_else(|| unreachable!("validated deepwork state"))
            .status();
        self.store.save(&proposed)?;
        self.linkage = proposed;
        self.slots.remove(session_id);
        Ok(status)
    }

    pub(super) fn deepwork_state_fields(&self) -> Result<(DeepworkStage, u64, String)> {
        let state = self
            .linkage
            .deepwork
            .as_ref()
            .context("no `$deepwork` run is active for this Main session")?;
        Ok((
            state.stage,
            state.run_index,
            state
                .workspace
                .strip_prefix(&state.repository_root)
                .unwrap_or(&state.workspace)
                .to_string_lossy()
                .to_string(),
        ))
    }

    pub(super) fn start_child(&mut self, cwd: &Path, launch: ChildLaunch) -> Result<SessionId> {
        let agent = Agent::new_specialist(cwd, launch.role)?;
        self.start_child_with_agent(agent, launch, AgentSlot::child)
    }

    fn start_child_with_agent(
        &mut self,
        mut agent: Agent,
        launch: ChildLaunch,
        build_slot: impl FnOnce(Agent, &ChildSessionLink) -> Result<AgentSlot>,
    ) -> Result<SessionId> {
        if agent.cwd() != self.main().cwd {
            return Err(anyhow!(
                "specialist Agent must use Main's working directory {}",
                self.main().cwd.display()
            ));
        }
        if launch.initial_message.trim().is_empty() {
            return Err(anyhow!("specialist message cannot be empty"));
        }
        let fixed_model = launch.role.model_selection();
        if agent.model_selection() != &fixed_model {
            return Err(anyhow!(
                "specialist Agent does not match the requested fixed model profile"
            ));
        }
        agent.set_specialist_role(launch.role)?;
        let session_id = SessionId::parse(agent.session_id().to_string())?;
        let link = ChildSessionLink {
            session_id: session_id.clone(),
            role: launch.role.as_str().to_string(),
            stage_attempt: launch.stage_attempt,
            model_selection: fixed_model,
            lifecycle: ChildLifecycle::Working,
            accepted_handoff: launch.accepted_handoff,
            prompt_revision: Some(launch.role.prompt_revision().to_string()),
            replaces: None,
            replaced_by: None,
        };
        link.validate()?;
        let slot = build_slot(agent, &link)?;
        let mut proposed = self.linkage.clone();
        proposed.children.push(link);
        self.store.save(&proposed)?;
        self.linkage = proposed;
        self.slots.insert(session_id.clone(), slot);
        self.begin_child_turn(&session_id, launch.initial_message)?;
        Ok(session_id)
    }

    pub(super) fn send_child(&mut self, session_id: &SessionId, message: String) -> Result<()> {
        if message.trim().is_empty() {
            return Err(anyhow!("specialist message cannot be empty"));
        }
        let lifecycle = self
            .linkage
            .child(session_id)
            .map(|child| child.lifecycle)
            .context("specialist session is not linked to this group")?;
        if !lifecycle.is_live() {
            return Err(anyhow!("specialist session {session_id} is not live"));
        }
        self.validate_child_is_ready(session_id)?;
        let mut proposed = self.linkage.clone();
        proposed
            .child_mut(session_id)
            .context("specialist session is not linked to this group")?
            .lifecycle = ChildLifecycle::Working;
        self.store.save(&proposed)?;
        self.linkage = proposed;
        self.begin_child_turn(session_id, message)
    }

    fn validate_child_is_ready(&self, session_id: &SessionId) -> Result<()> {
        let slot = self
            .slots
            .get(session_id)
            .context("specialist runtime is not live")?;
        if slot.turn.is_active() {
            return Err(anyhow!(
                "specialist session {session_id} is already working"
            ));
        }
        let fixed_model = slot
            .child_identity()
            .map(|(_, _, fixed_model)| fixed_model)
            .context("Main cannot receive specialist coordination messages")?;
        let agent = slot
            .agent
            .as_ref()
            .context("specialist Agent is unavailable")?;
        if agent.model_selection() != fixed_model {
            return Err(anyhow!(
                "specialist session {session_id} no longer has its fixed model profile"
            ));
        }
        Ok(())
    }

    fn begin_child_turn(&mut self, session_id: &SessionId, message: String) -> Result<()> {
        let slot = self
            .slots
            .get_mut(session_id)
            .context("specialist runtime is not live")?;
        let agent = slot
            .agent
            .take()
            .context("specialist Agent is unavailable")?;
        let prompt = UserPrompt::text(message);
        slot.view.start_turn(&prompt);
        slot.turn_started_at = Some(Instant::now());
        let (turn_handle, turn_control) = TurnControl::channel();
        slot.turn_handle = Some(turn_handle);
        slot.turn = TurnTaskState::Running(spawn_submission_supervisor(
            session_id.clone(),
            agent,
            prompt,
            turn_control,
            self.agent_events_tx.clone(),
            self.turn_results_tx.clone(),
        ));
        Ok(())
    }

    pub(super) fn wait_child(
        &mut self,
        session_id: &SessionId,
        response: oneshot::Sender<Result<SpecialistEvent>>,
    ) {
        match self.slots.get_mut(session_id) {
            Some(slot) if slot.is_child() => slot.wait(response),
            Some(_) => {
                let _ = response.send(Err(anyhow!("Main is not a specialist session")));
            }
            None => {
                let _ = response.send(Err(anyhow!("specialist session {session_id} is not live")));
            }
        }
    }

    pub(super) fn revive_child(
        &mut self,
        cwd: &Path,
        session_id: &SessionId,
        message: String,
    ) -> Result<()> {
        let link = self
            .linkage
            .child(session_id)
            .cloned()
            .context("specialist session is not linked to this group")?;
        let mut agent = Agent::resume(cwd, ResumeSelector::Id(session_id.as_uuid()?))?;
        if agent.model_selection() != &link.model_selection {
            agent.set_model_selection(link.model_selection)?;
        }
        agent.set_specialist_role(SpecialistRole::parse(&link.role)?)?;
        self.revive_child_with_agent(session_id, message, agent, AgentSlot::child)
    }

    fn revive_child_with_agent(
        &mut self,
        session_id: &SessionId,
        message: String,
        mut agent: Agent,
        build_slot: impl FnOnce(Agent, &ChildSessionLink) -> Result<AgentSlot>,
    ) -> Result<()> {
        if message.trim().is_empty() {
            return Err(anyhow!("specialist message cannot be empty"));
        }
        if self.slots.contains_key(session_id) {
            return Err(anyhow!("specialist session {session_id} is already live"));
        }
        let link = self
            .linkage
            .child(session_id)
            .cloned()
            .context("specialist session is not linked to this group")?;
        if link.lifecycle != ChildLifecycle::Retired {
            return Err(anyhow!("only a retired specialist session can be revived"));
        }
        if agent.session_id() != session_id.to_string()
            || agent.model_selection() != &link.model_selection
            || agent.cwd() != self.main().cwd
        {
            return Err(anyhow!(
                "revived specialist Agent does not match its persisted session linkage"
            ));
        }
        agent.set_specialist_role(SpecialistRole::parse(&link.role)?)?;
        let slot = build_slot(agent, &link)?;
        let mut proposed = self.linkage.clone();
        if let Some(state) = proposed.deepwork.as_mut() {
            state.reopen(SpecialistRole::parse(&link.role)?);
        }
        proposed
            .child_mut(session_id)
            .unwrap_or_else(|| unreachable!("validated child linkage"))
            .lifecycle = ChildLifecycle::Revived;
        self.store.save(&proposed)?;
        self.linkage = proposed;
        self.slots.insert(session_id.clone(), slot);
        self.begin_child_turn(session_id, message)
    }

    pub(super) fn replace_child(
        &mut self,
        cwd: &Path,
        session_id: &SessionId,
        message: String,
    ) -> Result<SessionId> {
        let old = self
            .linkage
            .child(session_id)
            .cloned()
            .context("specialist session is not linked to this group")?;
        let agent = Agent::new_specialist(cwd, SpecialistRole::parse(&old.role)?)?;
        self.replace_child_with_agent(session_id, message, agent, AgentSlot::child)
    }

    fn replace_child_with_agent(
        &mut self,
        session_id: &SessionId,
        message: String,
        mut agent: Agent,
        build_slot: impl FnOnce(Agent, &ChildSessionLink) -> Result<AgentSlot>,
    ) -> Result<SessionId> {
        if message.trim().is_empty() {
            return Err(anyhow!("specialist message cannot be empty"));
        }
        if self
            .slots
            .get(session_id)
            .is_some_and(|slot| slot.turn.is_active())
        {
            return Err(anyhow!(
                "specialist session {session_id} cannot be replaced while working"
            ));
        }
        let old = self
            .linkage
            .child(session_id)
            .cloned()
            .context("specialist session is not linked to this group")?;
        if old.lifecycle == ChildLifecycle::Replaced || old.replaced_by.is_some() {
            return Err(anyhow!(
                "specialist session {session_id} has already been replaced"
            ));
        }
        if let Some(slot) = self.slots.get_mut(session_id)
            && let Some(agent) = slot.agent.as_mut()
        {
            super::persist_session_transcript(&slot.view, agent)?;
        }
        if agent.model_selection() != &old.model_selection || agent.cwd() != self.main().cwd {
            return Err(anyhow!(
                "replacement specialist Agent does not match the fixed model profile and working directory"
            ));
        }
        agent.set_specialist_role(SpecialistRole::parse(&old.role)?)?;
        let replacement_id = SessionId::parse(agent.session_id().to_string())?;
        let replacement = ChildSessionLink {
            session_id: replacement_id.clone(),
            role: old.role.clone(),
            stage_attempt: old
                .stage_attempt
                .checked_add(1)
                .context("specialist stage-attempt counter overflowed")?,
            model_selection: old.model_selection.clone(),
            lifecycle: ChildLifecycle::Working,
            accepted_handoff: old.accepted_handoff.clone(),
            prompt_revision: old.prompt_revision,
            replaces: Some(session_id.clone()),
            replaced_by: None,
        };
        let slot = build_slot(agent, &replacement)?;
        let mut proposed = self.linkage.clone();
        if let Some(state) = proposed.deepwork.as_mut() {
            state.reopen(SpecialistRole::parse(&old.role)?);
        }
        proposed
            .child_mut(session_id)
            .unwrap_or_else(|| unreachable!("validated child linkage"))
            .lifecycle = ChildLifecycle::Replaced;
        proposed
            .child_mut(session_id)
            .unwrap_or_else(|| unreachable!("validated child linkage"))
            .replaced_by = Some(replacement_id.clone());
        proposed.children.push(replacement);
        if proposed.active_session_id == *session_id {
            proposed.active_session_id = proposed.main_session_id.clone();
        }
        self.store.save(&proposed)?;
        self.linkage = proposed;
        self.slots.remove(session_id);
        self.slots.insert(replacement_id.clone(), slot);
        self.begin_child_turn(&replacement_id, message)?;
        Ok(replacement_id)
    }

    pub(super) fn install_turn_result(
        &mut self,
        session_id: &SessionId,
        result: std::result::Result<TurnResult, String>,
    ) -> Result<()> {
        let slot = self
            .slots
            .get_mut(session_id)
            .context("turn completed for a session that is no longer live")?;
        match result {
            Ok(result) => slot.turn = TurnTaskState::Presenting(Box::new(result)),
            Err(error) if slot.is_child() => {
                slot.turn = TurnTaskState::Idle;
                slot.turn_handle = None;
                let (role, stage_attempt, _) = slot
                    .child_identity()
                    .unwrap_or_else(|| unreachable!("child slot identity"));
                let mut message = format!("specialist task stopped unexpectedly: {error}");
                if let Err(persistence_error) =
                    self.set_lifecycle(session_id, ChildLifecycle::Active)
                {
                    if let Some(link) = self.linkage.child_mut(session_id) {
                        link.lifecycle = ChildLifecycle::Active;
                    }
                    message.push_str(&format!(
                        "; lifecycle could not be persisted: {persistence_error:#}"
                    ));
                }
                self.slots
                    .get_mut(session_id)
                    .unwrap_or_else(|| unreachable!("live specialist slot"))
                    .deliver_meaningful_event(SpecialistEvent {
                        session_id: session_id.to_string(),
                        role,
                        stage_attempt,
                        kind: SpecialistEventKind::Failed,
                        status: ChildLifecycle::Active,
                        message,
                        final_result: None,
                    });
            }
            Err(error) => return Err(anyhow!("agent task stopped unexpectedly: {error}")),
        }
        Ok(())
    }

    pub(super) fn set_lifecycle(
        &mut self,
        session_id: &SessionId,
        lifecycle: ChildLifecycle,
    ) -> Result<()> {
        let mut proposed = self.linkage.clone();
        proposed
            .child_mut(session_id)
            .context("specialist session is not linked to this group")?
            .lifecycle = lifecycle;
        self.store.save(&proposed)?;
        self.linkage = proposed;
        Ok(())
    }

    fn restore_linked_children(&mut self) {
        self.restore_linked_children_with(
            |cwd, session_id| Agent::resume(cwd, ResumeSelector::Id(session_id.as_uuid()?)),
            AgentSlot::child,
        );
    }

    fn restore_linked_children_with(
        &mut self,
        mut resume_agent: impl FnMut(&Path, &SessionId) -> Result<Agent>,
        build_slot: impl Fn(Agent, &ChildSessionLink) -> Result<AgentSlot>,
    ) {
        let main_cwd = self.main().cwd.clone();
        let live_links = self
            .linkage
            .children
            .iter()
            .filter(|child| child.lifecycle.is_live())
            .cloned()
            .collect::<Vec<_>>();
        let mut changed = false;
        for link in live_links {
            let mut agent = match resume_agent(&main_cwd, &link.session_id) {
                Ok(agent) => agent,
                Err(error) => {
                    self.main_mut().view.add_notice(format!(
                        "Child session {} could not be restored: {error:#}",
                        link.session_id
                    ));
                    continue;
                }
            };
            if agent.cwd() != main_cwd {
                self.main_mut().view.add_notice(format!(
                    "Child session {} uses a different working directory and was not restored",
                    link.session_id
                ));
                continue;
            }
            if agent.model_selection() != &link.model_selection
                && let Err(error) = agent.set_model_selection(link.model_selection.clone())
            {
                self.main_mut().view.add_notice(format!(
                    "Child session {} could not restore its fixed model: {error:#}",
                    link.session_id
                ));
                continue;
            }
            let role = SpecialistRole::parse(&link.role)
                .unwrap_or_else(|_| unreachable!("validated specialist role"));
            if let Err(error) = agent.set_specialist_role(role) {
                self.main_mut().view.add_notice(format!(
                    "Child session {} could not restore its specialist prompt: {error:#}",
                    link.session_id
                ));
                continue;
            }
            match build_slot(agent, &link) {
                Ok(mut slot) => {
                    if matches!(
                        link.lifecycle,
                        ChildLifecycle::Working | ChildLifecycle::Revived
                    ) {
                        let role = SpecialistRole::parse(&link.role)
                            .unwrap_or_else(|_| unreachable!("validated specialist role"));
                        slot.deliver_meaningful_event(SpecialistEvent {
                            session_id: link.session_id.to_string(),
                            role,
                            stage_attempt: link.stage_attempt,
                            kind: SpecialistEventKind::Interrupted,
                            status: ChildLifecycle::AwaitingReview,
                            message: "specialist turn was interrupted before cold resume"
                                .to_string(),
                            final_result: None,
                        });
                        if let Some(persisted) = self.linkage.child_mut(&link.session_id) {
                            persisted.lifecycle = ChildLifecycle::AwaitingReview;
                            changed = true;
                        }
                    }
                    self.slots.insert(link.session_id, slot);
                }
                Err(error) => self.main_mut().view.add_notice(format!(
                    "Child session {} could not initialize its runtime: {error:#}",
                    link.session_id
                )),
            }
        }
        if !self.slots.contains_key(&self.linkage.active_session_id) {
            self.linkage.active_session_id = self.linkage.main_session_id.clone();
            changed = true;
        }
        if changed && let Err(error) = self.store.save(&self.linkage) {
            self.main_mut().view.add_notice(format!(
                "Recovered child lifecycle could not be saved: {error:#}"
            ));
        }
    }
}

fn specialist_order(role: &str) -> u8 {
    match role.trim().trim_start_matches('$') {
        "evals" => 0,
        "manifest" => 1,
        "worker" => 2,
        "reviewer" => 3,
        _ => 4,
    }
}

fn spawn_submission_supervisor(
    session_id: SessionId,
    mut agent: Agent,
    prompt: UserPrompt,
    turn_control: TurnControl,
    agent_events: UnboundedSender<RoutedAgentEvent>,
    turn_results: UnboundedSender<RoutedTurnResult>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (events_tx, mut events_rx) = unbounded_channel();
        let worker = tokio::spawn(async move {
            let result = agent
                .submit_with_control(UserInput::prompt(prompt), events_tx, turn_control)
                .await;
            (agent, TurnCompletion::Submission(result))
        });
        supervise_worker(
            session_id,
            worker,
            &mut events_rx,
            agent_events,
            turn_results,
        )
        .await;
    })
}

pub(super) fn spawn_compaction_supervisor(
    session_id: SessionId,
    mut agent: Agent,
    turn_control: TurnControl,
    agent_events: UnboundedSender<RoutedAgentEvent>,
    turn_results: UnboundedSender<RoutedTurnResult>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (events_tx, mut events_rx) = unbounded_channel();
        let worker = tokio::spawn(async move {
            let result = agent.compact_with_control(events_tx, turn_control).await;
            (agent, TurnCompletion::Compaction(result))
        });
        supervise_worker(
            session_id,
            worker,
            &mut events_rx,
            agent_events,
            turn_results,
        )
        .await;
    })
}

pub(super) fn spawn_main_submission_supervisor(
    session_id: SessionId,
    mut agent: Agent,
    prompt: UserPrompt,
    turn_control: TurnControl,
    agent_events: UnboundedSender<RoutedAgentEvent>,
    turn_results: UnboundedSender<RoutedTurnResult>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (events_tx, mut events_rx) = unbounded_channel();
        let worker = tokio::spawn(async move {
            let result = agent
                .submit_with_control(UserInput::prompt(prompt), events_tx, turn_control)
                .await;
            (agent, TurnCompletion::Submission(result))
        });
        supervise_worker(
            session_id,
            worker,
            &mut events_rx,
            agent_events,
            turn_results,
        )
        .await;
    })
}

async fn supervise_worker(
    session_id: SessionId,
    worker: JoinHandle<TurnResult>,
    events: &mut UnboundedReceiver<AgentEvent>,
    ingress: UnboundedSender<RoutedAgentEvent>,
    completions: UnboundedSender<RoutedTurnResult>,
) {
    let mut worker = AbortOnDrop::new(worker);
    let result = loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(event) => {
                    if ingress.send((session_id.clone(), event)).is_err() {
                        return;
                    }
                }
                None => {
                    break worker.join().await;
                }
            },
            result = worker.join() => break result,
        }
    };
    while let Some(event) = events.recv().await {
        if ingress.send((session_id.clone(), event)).is_err() {
            return;
        }
    }
    let result = result.map_err(|error| error.to_string());
    let _ = completions.send((session_id, result));
}

struct AbortOnDrop<T> {
    task: Option<JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    fn new(task: JoinHandle<T>) -> Self {
        Self { task: Some(task) }
    }

    async fn join(&mut self) -> std::result::Result<T, tokio::task::JoinError> {
        let task = self
            .task
            .as_mut()
            .unwrap_or_else(|| unreachable!("worker task is joined once"));
        let result = task.await;
        self.task = None;
        result
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
