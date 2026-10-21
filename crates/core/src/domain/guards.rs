//! The rules of the commands the user is offered as buttons, judged on the state before the
//! command and without changing it. A screen asks them to know whether to show a button; `apply`
//! asks the same ones before it changes anything, so the two never disagree.
use super::*;

impl DomainState {
    /// Whether the command would be refused, and why, without copying or changing the state:
    /// the envelope's checks, then the command's own rules. Only the commands a screen offers as
    /// buttons have them; any other command is judged only when it is applied.
    pub fn guard(&self, actor: &Actor, envelope: &CommandEnvelope) -> Result<()> {
        self.guard_given(actor, envelope, self)
    }

    /// The same rules, reading what holds a delivery back from `holds`.
    pub(in crate::domain) fn guard_given(&self, actor: &Actor, envelope: &CommandEnvelope, holds: &dyn Holds) -> Result<()> {
        self.admit(actor, envelope)?;
        match &envelope.command {
            Command::SubmitFeedback { attachments, presentation_id, targets, body, instruction } => {
                self.feedback_guard(actor, presentation_id, targets, body, attachments, *instruction).map(drop)
            }
            Command::AcceptDelivery { presentation_id, targets } => {
                self.accept_delivery_guard(actor, presentation_id, targets, holds).map(drop)
            }
            Command::DecideProposedPlan { presentation_id, targets, choice } => {
                self.decide_plan_guard(actor, presentation_id, targets, *choice, holds).map(drop)
            }
            Command::AuthorizeMerge { authorization_id, presentation_id, pr_id, expected_head, method } => {
                self.merge_guard(actor, authorization_id, presentation_id, pr_id, expected_head, *method, holds).map(drop)
            }
            Command::CancelTask { task_id } => self.cancel_guard(actor, task_id),
            Command::ReopenTask { task_id } => self.reopen_guard(actor, task_id),
            Command::ArchiveTask { task_id, archived } => self.archive_guard(actor, task_id, *archived),
            Command::InterruptTurn { agent_id } => self.interrupt_guard(actor, agent_id).map(drop),
            Command::SetModel { task_id, model } => {
                self.control_guard(actor, task_id, ControlRecord::SetModel { model: model.clone() }).map(drop)
            }
            Command::SetPermissionMode { task_id, mode } => {
                self.control_guard(actor, task_id, ControlRecord::SetPermissionMode { mode: *mode }).map(drop)
            }
            Command::ReadMcpStatus { task_id } => self.control_guard(actor, task_id, ControlRecord::ReadMcpStatus).map(drop),
            Command::ReconnectMcp { task_id, server } => self.mcp_reconnect_guard(actor, task_id, server).map(drop),
            Command::StartMcpSignIn { task_id, server } => self.mcp_sign_in_guard(actor, task_id, server).map(drop),
            Command::CompleteMcpSignIn { task_id, server, state } => self.mcp_complete_guard(actor, task_id, server, state).map(drop),
            Command::WaitForMcp { task_id, server } => self.mcp_wait_guard(actor, task_id, server).map(drop),
            Command::CompactConversation { task_id } => self.compact_guard(actor, task_id).map(drop),
            Command::RecordDecisionSettled { task_id, generation, epoch } => {
                self.decision_end_guard(actor, task_id, *generation, *epoch, true).map(drop)
            }
            Command::RecordDecisionAborted { task_id, generation, epoch } => {
                self.decision_end_guard(actor, task_id, *generation, *epoch, false).map(drop)
            }
            _ => fail("invalid_input", "Only the commands offered as buttons are judged before they run"),
        }
    }

    /// The checks every command passes before its own rules: protocol, epoch, command id, the
    /// actor's fence, and whether the client saw the last change to what it decides on.
    pub(super) fn admit(&self, actor: &Actor, envelope: &CommandEnvelope) -> Result<()> {
        if envelope.protocol_version != PROTOCOL_VERSION {
            return fail("incompatible_protocol", "Protocol version 2 is required");
        };
        if envelope.data_epoch != self.data_epoch {
            return fail("stale_epoch", "Business data epoch changed");
        };
        id(&envelope.command_id.0)?;
        self.fence(actor)?;
        // §5: staleness is judged against the subject being decided. A client
        // may act on a snapshot that predates unrelated changes, never on one
        // that predates the last change to this subject or the future.
        let floor = self
            .subject_revision(&envelope.command)
            .unwrap_or(self.revision);
        if envelope.expected_revision < floor || envelope.expected_revision > self.revision {
            let mut e = DomainError::new("stale_revision", "Refresh before applying this decision");
            e.current_revision = Some(self.revision);
            return Err(e);
        };
        if self.revision >= MAX_REVISION {
            return fail("invalid_input", "Revision limit reached");
        };
        Ok(())
    }

    /// Who sends the feedback and the task it goes to.
    pub(super) fn feedback_guard(
        &self,
        actor: &Actor,
        presentation_id: &InteractionId,
        targets: &[DeliveryTarget],
        body: &str,
        attachments: &[AttachmentRef],
        instruction: Instruction,
    ) -> Result<(String, TaskId)> {
        let u = match (actor, instruction) {
            (Actor::System, Instruction::Conflicts) => Feedback::ALONDRA.to_owned(),
            _ => user(actor)?,
        };
        content(body, attachments)?;
        let (p, t, current) = self.presentation(presentation_id)?;
        if current != targets {
            return fail("acceptance_stale", "Feedback targets changed");
        };
        if matches!(actor, Actor::User { .. }) && self.in_cleanup(t) && self.merged_delivery(p) {
            return fail("task_closing", "This task's pull request is already merged and the task is closing, so it takes no more changes: ask for more work as a new request.");
        }
        Ok((u, t.id.clone()))
    }

    /// Who accepts. Every PR the delivery carries that is neither merged at its head nor
    /// already authorized is authorized with it, so each must pass the merge rules too.
    pub(super) fn accept_delivery_guard(
        &self,
        actor: &Actor,
        presentation_id: &InteractionId,
        targets: &[DeliveryTarget],
        holds: &dyn Holds,
    ) -> Result<String> {
        let u = user(actor)?;
        let (_, _, current) = self.presentation(presentation_id)?;
        if current != targets {
            return fail("acceptance_stale", "Delivery targets changed");
        };
        if self.interactions[presentation_id].card.plan.is_some() {
            return refuse("plan_choice_required", "choice",
                "this result proposes a plan: decide it with DecideProposedPlan, implement or archive, so accepting it never leaves the plan unbuilt by accident");
        }
        self.unblocked(presentation_id, holds)?;
        for (pr_id, head, method) in self.accepted_merges(presentation_id, targets)? {
            let authorization = self.acceptance_authorization(pr_id);
            self.authorize_merge_guard(&authorization, presentation_id, pr_id, head, method, holds)?;
        }
        self.acceptance_guard(presentation_id)?;
        Ok(u)
    }

    /// The PRs accepting a delivery authorizes, each with its head and the method it merges by.
    pub(super) fn accepted_merges<'a>(
        &self,
        presentation_id: &InteractionId,
        targets: &'a [DeliveryTarget],
    ) -> Result<Vec<(&'a PrId, &'a str, MergeMethod)>> {
        let mut merges = vec![];
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let pr = &self.pull_requests[pr_id];
            let already_merged = pr.state == PrState::Merged && pr.merged_head.as_ref() == Some(head);
            let locally_authorized = self.merge_authorizations.values().any(|a| {
                a.valid && a.presentation_id == *presentation_id && a.pr_id == *pr_id && a.head == *head
            });
            if already_merged || locally_authorized {
                continue;
            }
            let Some(method) = pr.allowed_methods.first().copied() else {
                return fail("invalid_input", "Merge methods required");
            };
            merges.push((pr_id, head.as_str(), method));
        }
        Ok(merges)
    }

    pub(super) fn acceptance_authorization(&self, pr_id: &PrId) -> AuthorizationId {
        AuthorizationId(format!("accept-{}-{}", self.revision + 1, pr_id.0))
    }

    /// Accepting records a decision on every document revision the delivery carries, which
    /// needs that document's review.
    pub(super) fn acceptance_guard(&self, presentation: &InteractionId) -> Result<()> {
        let (_, t, targets) = self.presentation(presentation)?;
        for target in targets {
            if let DeliveryTarget::DocumentRevision { document_id, .. } = target
                && !self.document_reviews.iter().any(|r| r.task_id == t.id && &r.document_id == document_id)
            {
                return fail("not_found", "Document review missing");
            }
        }
        Ok(())
    }

    /// Who decides, and the plan the delivery proposes.
    pub(super) fn decide_plan_guard(
        &self,
        actor: &Actor,
        presentation_id: &InteractionId,
        targets: &[DeliveryTarget],
        choice: PlanChoice,
        holds: &dyn Holds,
    ) -> Result<(String, String)> {
        let u = user(actor)?;
        let (_, _, current) = self.presentation(presentation_id)?;
        if current != targets {
            return fail("acceptance_stale", "Delivery targets changed");
        };
        let Some(plan) = self.interactions[presentation_id].card.plan.clone() else {
            return refuse("invalid_input", "choice", "this result proposes no plan: accept it with AcceptDelivery");
        };
        if matches!(&self.interactions[presentation_id].payload, InteractionPayload::Delivery { acceptance: Some(a), .. } if a.valid) {
            return refuse("invalid_input", "presentation_id", "this result is already accepted");
        }
        self.unblocked(presentation_id, holds)?;
        self.acceptance_guard(presentation_id)?;
        if choice == PlanChoice::Implement && self.requests.contains_key(&implementation_request(presentation_id)) {
            return fail("invalid_input", "Request already exists");
        }
        Ok((u, plan))
    }

    /// Who authorizes: the user, or Alondra on its own for an untouched delivery of an
    /// auto-merge Request.
    pub(super) fn merge_guard(
        &self,
        actor: &Actor,
        authorization_id: &AuthorizationId,
        presentation_id: &InteractionId,
        pr_id: &PrId,
        expected_head: &str,
        method: MergeMethod,
        holds: &dyn Holds,
    ) -> Result<Option<String>> {
        id(&authorization_id.0)?;
        let u = if matches!(actor, Actor::System) {
            let (p, t, _) = self.presentation(presentation_id)?;
            let untouched = matches!(&p.payload, InteractionPayload::Delivery { acceptance: None, feedback, .. } if feedback.is_empty());
            if !self.requests.get(&t.request_id).is_some_and(|r| r.auto_merge) || !untouched {
                return fail("forbidden", "Alondra merges on its own only an untouched delivery of an auto-merge Request");
            }
            self.unblocked(presentation_id, holds)?;
            None
        } else {
            Some(user(actor)?)
        };
        self.authorize_merge_guard(authorization_id, presentation_id, pr_id, expected_head, method, holds)?;
        Ok(u)
    }

    /// The go-ahead to merge one presented PR head. An attempt at an older head cannot be in
    /// flight while the PR is open at a newer one, so only an attempt at the current head holds
    /// a new authorization back.
    pub(super) fn authorize_merge_guard(
        &self,
        authorization_id: &AuthorizationId,
        presentation_id: &InteractionId,
        pr_id: &PrId,
        expected_head: &str,
        method: MergeMethod,
        holds: &dyn Holds,
    ) -> Result<TaskId> {
        let (_, t, targets) = self.presentation(presentation_id)?;
        self.unblocked(presentation_id, holds)?;
        let number = self.pull_requests[pr_id].number;
        if !targets.contains(&DeliveryTarget::PullRequest {
            pr_id: pr_id.clone(),
            head: expected_head.to_owned(),
        }) {
            return fail("stale_head", "PR/head not in current delivery").map_err(|e| e.because(Refusal::StaleHead { number }));
        };
        let pr = &self.pull_requests[pr_id];
        if pr.state != PrState::Open {
            return fail("invalid_action_for_kind", "Only an open PR can be authorized").map_err(|e| e.because(Refusal::PrClosed { number }));
        };
        if !pr.allowed_methods.contains(&method) {
            return fail("invalid_input", "Merge method not allowed")
                .map_err(|e| e.because(Refusal::MethodNotAllowed { method, allowed: pr.allowed_methods.clone() }));
        };
        if self.merge_authorizations.contains_key(authorization_id) {
            return fail("invalid_input", "Authorization ID already exists");
        };
        if self.merge_authorizations.values().any(|a| {
            a.pr_id == *pr_id
                && a.head == pr.head
                && matches!(a.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain)
        }) {
            return fail("delivery_uncertain", "Reconcile existing merge attempt before authorizing again")
                .map_err(|e| e.because(Refusal::MergeInProgress { number }));
        };
        Ok(t.id.clone())
    }

    /// The user, or Alondra, stops the turn or the work the node's SDK task runs now.
    pub(super) fn interrupt_guard(&self, actor: &Actor, agent_id: &AgentId) -> Result<(TaskId, LifecycleRecord)> {
        if !matches!(actor, Actor::User { .. } | Actor::System) {
            return fail("forbidden", "Only the user or Alondra interrupts a node");
        }
        if !self.agents.contains_key(agent_id) {
            return fail("not_found", "Agent missing");
        }
        let (task, lifecycle) = self.sdk_task_of(agent_id)?;
        let turn = match lifecycle.activity() {
            alondra_domain::ActivityView::Working => lifecycle.turn().map(|turn| turn.as_str().to_owned()),
            alondra_domain::ActivityView::OwnWork => None,
            _ => {
                return Err(DomainError::new("conflict", format!("Nothing is running on node {agent_id} to interrupt"))
                    .because(Refusal::NothingToInterrupt));
            }
        };
        if lifecycle.resource() != Some(alondra_domain::ResourceEvidence::Available) {
            let node = self.tasks.get(&task).map_or_else(|| agent_id.0.clone(), |task| task.title.clone());
            return Err(DomainError::new("conflict", format!("Node {agent_id} is not connected, so it cannot be interrupted now"))
                .because(Refusal::NodeUnreachable { node }));
        }
        let record = LifecycleRecord::ControlRequested {
            control: format!("control-{task}-{}", lifecycle.revision() + 1),
            operation: ControlRecord::Interrupt { turn },
        };
        let record = self.judged(&task, lifecycle, record)?;
        Ok((task, record))
    }

    /// Alondra runs an operation on the task's SDK execution.
    pub(super) fn control_guard(&self, actor: &Actor, task_id: &TaskId, operation: ControlRecord) -> Result<LifecycleRecord> {
        system(actor)?;
        let lifecycle = self.sdk_lifecycle(task_id)?;
        let control = format!("control-{task_id}-{}", lifecycle.revision() + 1);
        self.judged(task_id, lifecycle, LifecycleRecord::ControlRequested { control, operation })
    }

    /// Alondra has the task's resting SDK execution compact its conversation.
    pub(super) fn compact_guard(&self, actor: &Actor, task_id: &TaskId) -> Result<LifecycleRecord> {
        system(actor)?;
        let lifecycle = self.sdk_lifecycle(task_id)?;
        self.judged(task_id, lifecycle, LifecycleRecord::Compacted { input: self.next_input(task_id) })
    }

    /// Alondra cancels on its own only a task nothing has started; anyone else must manage it.
    pub(super) fn cancel_guard(&self, actor: &Actor, task_id: &TaskId) -> Result<()> {
        let unstarted = self.tasks.get(task_id).is_some_and(|task| {
            matches!(task.state, TaskState::NotStarted | TaskState::WaitingDependency) && !task.started && task.agent_id.is_none()
        });
        if !(matches!(actor, Actor::System) && unstarted) {
            self.manager(actor)?;
        }
        if self.active(task_id)?.ending() {
            return fail("task_not_active", "Task is already ending");
        }
        Ok(())
    }

    pub(super) fn reopen_guard(&self, actor: &Actor, task_id: &TaskId) -> Result<()> {
        self.manager(actor)?;
        let task = self.task(task_id)?;
        if !task.state.terminal() {
            return fail("task_not_active", "Only terminal Tasks can be reopened");
        };
        if !self
            .requests
            .get(&task.request_id)
            .is_some_and(|r| !r.closed)
        {
            return fail("not_found", "Open Request required to reopen Task");
        }
        if !task.blocks.is_empty() && !self.reviewing_open(task) {
            return refuse(
                "invalid_input",
                "task_id",
                "The task this one reviewed is already over, so there is nothing left to review: create a new task if you want another review",
            );
        }
        Ok(())
    }

    pub(super) fn archive_guard(&self, actor: &Actor, task_id: &TaskId, archived: bool) -> Result<()> {
        user(actor)?;
        let task = self.task(task_id)?;
        if archived && !task.state.terminal() {
            return fail("task_not_active", "Only terminal Tasks can be archived");
        };
        Ok(())
    }
}

/// The Request that implementing a proposed plan creates.
pub(super) fn implementation_request(presentation_id: &InteractionId) -> RequestId {
    RequestId(format!("implement-{}", presentation_id.0))
}
