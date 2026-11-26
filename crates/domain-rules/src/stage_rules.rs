use super::*;
use crate::{stage_input, ExecutionRole, HandoverCause, PlanApproval, RoleTool, StageHandover, TaskRun, MAX_STAGE_ATTEMPTS};
use alondra_core::model_selection::{PendingTaskSelection, TaskSelection};

/// The code of a call an agent's role does not allow.
pub const STAGE_REFUSED: &str = "role_forbids_tool";
pub use alondra_domain::HANDED_OVER_PLAN;

impl DomainState {
    pub(super) fn record_task_stages(&mut self, actor: &Actor, task_id: &TaskId, staged: bool, reason: &str, selections: &BTreeMap<ExecutionRole, TaskSelection>) -> Result<()> {
        system(actor)?;
        nonempty(reason)?;
        if reason.len() > 2000 {
            return fail("invalid_input", "The reason for the task's stages is one short paragraph");
        }
        let task = self.active(task_id)?;
        if task.started || !task.stages.runs.is_empty() {
            return fail("conflict", "A task's stages are chosen before its first node starts");
        }
        if staged != ExecutionRole::ALL.iter().all(|role| selections.contains_key(role)) || (!staged && !selections.is_empty()) {
            return fail("invalid_input", "A task with stages has the selection of every role, and one without stages has none");
        }
        if selections.iter().any(|(role, selection)| selection.profile != role.profile() || selection.effective.is_some()) {
            return fail("invalid_input", "Each role starts with the profile of its role and nothing running yet");
        }
        let stages = &mut self.tasks.get_mut(task_id).expect("checked above").stages;
        stages.staged = staged;
        stages.reason = Some(reason.trim().to_owned());
        stages.selections = selections.clone();
        Ok(())
    }

    /// The new execution of a staged task takes the role it hands over to, else the role it had,
    /// else the author's; a task without stages records its executions without a role.
    /// Its model is the one the admission names, else the one the change that starts it chose,
    /// else the one its selection runs.
    pub(super) fn record_run(&mut self, task_id: &TaskId, agent: &AgentId, generation: u64, admitted: Option<&TaskModel>) {
        let task = self.tasks.get_mut(task_id).expect("admitted");
        let role = task.stages.next_role();
        let chosen = task.pending_selection.as_ref().map(|pending| &pending.model.model)
            .or_else(|| task.selection.as_ref().and_then(|selection| selection.effective.as_ref()).map(|effective| &effective.model))
            .map(|key| TaskModel { model: key.model.clone(), chosen_by: ModelChooser::Orchestrator });
        let model = admitted.cloned().or(chosen).or_else(|| task.model.clone());
        task.stages.admitted(TaskRun { generation, agent: agent.clone(), role, provider: task.provider, account: task.account.clone(), model });
    }

    /// The user approved a plan of the task: Alondra records it, and a staged task's author hands
    /// the task over to a new implementer once it rests. The input the implementer starts from is
    /// queued now, so the author's turn ends on it and is never woken to go on by itself.
    pub(super) fn record_plan_approval(&mut self, task_id: &TaskId, card: &InteractionId, generation: u64, lifecycles: &mut Lifecycles) -> Result<()> {
        let revision = self.revision + 1;
        let Some(task) = self.tasks.get_mut(task_id) else { return Ok(()) };
        let role = task.stages.role();
        task.stages.approved(PlanApproval { card: card.clone(), revision, generation, role });
        if role != Some(ExecutionRole::Author) || task.stages.handover.is_some() {
            return Ok(());
        }
        task.stages.handover = Some(StageHandover {
            to: ExecutionRole::Implementer,
            cause: HandoverCause::PlanApproved { card: card.clone() },
            transition: None,
            attempts: 0,
            refused: false,
        });
        let input = self.next_input(task_id);
        self.decide(lifecycles, task_id, LifecycleRecord::Told { input, text: Some(stage_input(ExecutionRole::Implementer)), answers: None })
    }

    /// Whether the approval of `card` hands the task over to a new execution: the session that
    /// asked ends its turn instead of implementing the plan itself.
    pub(super) fn hands_over(&self, task_id: &TaskId, card: &InteractionId) -> bool {
        self.tasks.get(task_id).and_then(|task| task.stages.handover.as_ref())
            .is_some_and(|handover| handover.cause == HandoverCause::PlanApproved { card: card.clone() })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn begin_stage_handover(&mut self, actor: &Actor, task_id: &TaskId, expected_selection: &str, transition: &str, pending: &PendingTaskSelection) -> Result<()> {
        system(actor)?;
        self.not_ending(task_id)?;
        nonempty(transition)?;
        nonempty(&pending.reason)?;
        let task = self.active(task_id)?;
        let Some(handover) = task.stages.handover.clone().filter(StageHandover::due) else {
            return fail("conflict", "The task owes no change of role");
        };
        if task.pending_selection.is_some() {
            return fail("selection_pending", "A selection change is already pending for this task");
        }
        if self.task_selection_stamp(task_id).as_deref() != Some(expected_selection) {
            return fail("stale_selection", "The task's selection or execution changed; read it again");
        }
        let selection = task.stages.selections.get(&handover.to).cloned()
            .ok_or_else(|| DomainError::new("invalid_input", "The task has no selection for the role it hands over to"))?;
        if pending.account.provider != pending.model.model.provider || !selection.allows(&pending.account, &pending.model.model) {
            return fail("invalid_input", "The account and model the next role starts on are not the ones its selection allows");
        }
        let task = self.tasks.get_mut(task_id).expect("checked above");
        task.selection = Some(selection);
        task.pending_selection = Some(pending.clone());
        task.move_refused = None;
        task.stages.handover.as_mut().expect("checked above").transition = Some(transition.to_owned());
        Ok(())
    }

    pub(super) fn stage_handover_failed(&mut self, actor: &Actor, task_id: &TaskId, transition: &str, refused: bool) -> Result<()> {
        system(actor)?;
        let task = self.tasks.get_mut(task_id).ok_or_else(|| DomainError::new("not_found", "Task does not exist"))?;
        let Some(handover) = task.stages.handover.as_mut().filter(|handover| handover.transition.as_deref() == Some(transition)) else {
            return Ok(());
        };
        handover.transition = None;
        handover.attempts = handover.attempts.saturating_add(1);
        handover.refused = refused || handover.attempts >= MAX_STAGE_ATTEMPTS;
        Ok(())
    }

    /// The user asked to try the refused change of role again.
    pub(super) fn retry_stage_handover(&mut self, task_id: &TaskId) {
        if let Some(handover) = self.tasks.get_mut(task_id).and_then(|task| task.stages.handover.as_mut()) {
            handover.attempts = 0;
            handover.refused = false;
        }
    }

    /// Why a plan of the task's current execution is denied without a card: its role does not
    /// plan, or its approved plan already passes to a new execution.
    pub(super) fn role_plan_denial(&self, task_id: &TaskId) -> Option<String> {
        let task = self.tasks.get(task_id)?;
        let role = task.stages.role()?;
        if !role.allows(RoleTool::Plan) {
            return Some(role.refusal_fix(RoleTool::Plan));
        }
        task.stages.handover.is_some().then(|| HANDED_OVER_PLAN.to_owned())
    }

    /// Whether `agent` waits on a question it asked in `task` that nobody answered yet: a change of
    /// role never ends an execution that waits for an answer.
    pub fn asks_open(&self, task_id: &TaskId, agent: &AgentId) -> bool {
        self.interactions.values().any(|i| matches!(&i.scope, Scope::Task { task_id: t } if t == task_id)
            && i.author == Author::Agent { agent_id: agent.clone() }
            && !i.withdrawn
            && matches!(&i.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. }))
    }

    /// An agent of a staged task uses only the tools of its role.
    pub(super) fn role_allows(&self, actor: &Actor, task_id: &TaskId, tool: RoleTool) -> Result<()> {
        let Actor::Agent { agent_id } = actor else { return Ok(()) };
        let Some(task) = self.tasks.get(task_id).filter(|task| task.agent_id.as_ref() == Some(agent_id)) else { return Ok(()) };
        match task.stages.role() {
            Some(role) if !role.allows(tool) => refuse("role_forbids_tool", tool.name(), &role.refusal_fix(tool)),
            _ => Ok(()),
        }
    }
}
