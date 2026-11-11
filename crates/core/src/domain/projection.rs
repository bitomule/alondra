use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxCounts {
    pub roots: u64,
    pub pending: u64,
    pub unread_information: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboxSubject {
    /// A card, or for a delivery of several PRs the card of one of them.
    Interaction {
        interaction_id: InteractionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr_id: Option<PrId>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: String,
    pub kind: String,
    pub subject: InboxSubject,
    pub title: String,
    pub body: Option<String>,
    pub task_id: Option<TaskId>,
    pub request_id: Option<RequestId>,
    pub agent_id: Option<AgentId>,
    pub revision: Revision,
    pub order: Revision,
    pub pending: bool,
    pub available_actions: Vec<AvailableAction>,
    pub card: CardSpec,
    /// The deliveries this card holds back until it is answered.
    #[serde(default)]
    pub blocks: Vec<InteractionId>,
    /// The open cards, and for a stacked PR the upstream delivery, that hold this delivery back.
    #[serde(default)]
    pub blocked_by: Vec<InteractionId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableAction {
    pub kind: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    /// Why it cannot be used, for the person the card is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
    pub expected_revision: Revision,
    pub presentation_id: Option<InteractionId>,
    pub expected_head: Option<String>,
}
pub(super) struct InboxOrderFacts<'a> {
    pub(super) resurfaced: Option<Revision>,
    pub(super) id: &'a str,
    pub(super) interaction_id: &'a InteractionId,
    pub(super) order: Revision,
    pub(super) kind: CardKind,
    pub(super) blocking: bool,
    pub(super) blocked_by: &'a [InteractionId],
}

impl DomainState {
    /// The project a request's work belongs to: that of its first task that has one.
    pub fn request_project(&self, request: &RequestId) -> Option<&str> {
        self.tasks.values().filter(|task| task.request_id == *request).find_map(|task| task.project.as_deref())
    }
    /// Whether the user may make `change` to the accounts from Settings, by the guard of its
    /// command.
    pub fn account_login_action(&self, account: crate::accounts::AccountKey, cancel: Option<&str>) -> AvailableAction {
        let command = match cancel {
            Some(attempt) => Command::CancelAccountLogin { account, attempt: attempt.into() },
            None => Command::BeginAccountLogin { account, attempt: "requested".into(), started_at: 0 },
        };
        self.action("account_login", command, None, None, self)
    }
    pub fn account_action(&self, provider: crate::accounts::Provider, change: crate::accounts::AccountChange) -> AvailableAction {
        self.action("change_accounts", Command::ChangeAccounts { provider, change }, None, None, self)
    }
    pub fn task_selection_action(&self, task_id: &TaskId, selection: crate::model_selection::TaskSelection, pending: crate::model_selection::PendingTaskSelection) -> AvailableAction {
        self.action("task_selection", Command::RequestTaskSelection { task_id: task_id.clone(), expected_selection: self.task_selection_stamp(task_id).unwrap_or_default(), selection, pending }, None, None, self)
    }
    /// The button that moves the task to `account`, judged by the rule the move runs.
    pub fn move_task_action(&self, task_id: &TaskId, account: crate::accounts::AccountKey) -> AvailableAction {
        self.action("move_task", Command::MoveTask { task_id: task_id.clone(), expected_selection: self.task_selection_stamp(task_id).unwrap_or_default(), account }, None, None, self)
    }

    pub fn model_profile_action(&self, provider: crate::accounts::Provider, profile: crate::model_selection::ModelProfile,
        policy: crate::model_selection::RoleModelPolicy, expected_config_version: String) -> AvailableAction
    {
        self.action("set_model_profile", Command::SetModelProfile { provider, profile, policy, expected_config_version }, None, None, self)
    }
    /// User task controls are computed by the same guards as command execution.
    pub fn task_actions(&self, task_id: &TaskId) -> Option<Vec<AvailableAction>> {
        self.task_actions_given(task_id, self)
    }
    pub(super) fn task_actions_given(&self, task_id: &TaskId, holds: &dyn Holds) -> Option<Vec<AvailableAction>> {
        self.tasks.get(task_id)?;
        Some(vec![
            self.action(
                "cancel_task",
                Command::CancelTask {
                    task_id: task_id.clone(),
                },
                None,
                None,
                holds,
            ),
            self.action(
                "reopen_task",
                Command::ReopenTask {
                    task_id: task_id.clone(),
                },
                None,
                None,
                holds,
            ),
        ])
    }
    /// The button that clears the completed work off the lists, judged by the rule clearing runs.
    pub fn clear_completed_action(&self) -> AvailableAction {
        self.action("clear_completed", Command::ClearCompleted { seen: self.revision }, None, None, self)
    }
    /// The Revoke button of each thing the user allowed in the task, by the card it was allowed on.
    pub fn grant_actions(&self, task_id: &TaskId) -> Vec<(InteractionId, AvailableAction)> {
        self.grant_actions_given(task_id, self)
    }
    pub(super) fn grant_actions_given(&self, task_id: &TaskId, holds: &dyn Holds) -> Vec<(InteractionId, AvailableAction)> {
        let Some(task) = self.tasks.get(task_id) else { return vec![] };
        task.allowed.keys().map(|card| {
            let revoke = Command::RevokeTaskGrant { task_id: task_id.clone(), card: card.clone() };
            (card.clone(), self.action("revoke_task_grant", revoke, None, None, holds))
        }).collect()
    }
    /// The button that makes `method` this Mac's way of merging PRs, judged by the rule setting it runs.
    pub fn set_merge_method_action(&self, method: MergeMethod) -> AvailableAction {
        self.action("set_merge_method", Command::SetMergeMethod { method }, None, None, self)
    }
    /// The button that makes `update` the way nodes bring their PR's base into their branch.
    pub fn set_branch_update_action(&self, update: BranchUpdate) -> AvailableAction {
        self.action("set_branch_update", Command::SetBranchUpdate { update }, None, None, self)
    }
    pub(super) fn action(
        &self,
        kind: &str,
        command: Command,
        presentation: Option<InteractionId>,
        head: Option<String>,
        holds: &dyn Holds,
    ) -> AvailableAction {
        let actor = Actor::User { user_id: "projection-user".into() };
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: self.data_epoch.clone(),
            command_id: "projection-validation".into(),
            expected_revision: self.revision,
            command,
        };
        let error = self.guard_given(&actor, &envelope, holds).err();
        #[cfg(test)]
        assert_guard_matches_execution(self, &actor, &envelope, error.as_ref());
        AvailableAction {
            kind: kind.into(),
            enabled: error.is_none(),
            refusal: error.as_ref().map(DomainError::why),
            disabled_reason: error.map(|e| e.code),
            expected_revision: self.revision,
            presentation_id: presentation,
            expected_head: head,
        }
    }
    /// The instruction a delivery went back to its Agent with, since its acceptance. Only the
    /// Agent presenting again puts it back in front of the user, except after a reply: that one
    /// holds it only while its node attends to it (`open_reply`).
    pub fn handed_back(&self, presentation: &Interaction) -> Option<Instruction> {
        let InteractionPayload::Delivery { acceptance, feedback, .. } = &presentation.payload else {
            return None;
        };
        let reply = feedback.last().filter(|reply| self.open_reply(presentation) && self.node_attends(presentation) && self.reply_in_hand(presentation, reply));
        reply
            .or_else(|| last_hand_back(feedback))
            .filter(|f| acceptance.as_ref().is_none_or(|a| f.at_revision > a.at_revision))
            .map(|f| f.instruction)
    }
    /// A reply is the last thing that changed the delivery: its node has not ended a turn, been
    /// paused or lost its session since, and nothing else moved the delivery.
    pub fn open_reply(&self, presentation: &Interaction) -> bool {
        let InteractionPayload::Delivery { feedback, .. } = &presentation.payload else {
            return false;
        };
        feedback.last().is_some_and(|f| f.instruction == Instruction::Reply && f.at_revision == presentation.revision)
    }
    /// The delivery's node can act on what the user sends it now: it is not paused, lost or gone.
    pub fn node_attends(&self, presentation: &Interaction) -> bool {
        let Scope::Task { task_id } = &presentation.scope else {
            return false;
        };
        self.tasks.get(task_id).and_then(|t| t.agent_id.as_ref()).and_then(|id| self.agents.get(id))
            .is_some_and(|a| !matches!(a.phase, NodePhase::Lost | NodePhase::Closing | NodePhase::Closed) && self.node_pause(&a.id).is_none())
    }
    /// A node of the Agent SDK holds the reply until it took it and its session rests with nothing
    /// to wait for. That alone gives the delivery back, whatever fact
    /// of its turns reached Alondra, so a delivery is never out of the user's queue while its node
    /// does nothing for them.
    fn reply_in_hand(&self, presentation: &Interaction, reply: &Feedback) -> bool {
        let Scope::Task { task_id } = &presentation.scope else { return false };
        let sdk = self.tasks.get(task_id).and_then(|t| t.agent_id.as_ref())
            .is_some_and(|agent| self.node_phase(agent) == Some(&NodePhase::ThroughSdk) || self.through_sdk(agent));
        if !sdk || !self.told_since(task_id, reply.at_revision) {
            return true;
        }
        self.lifecycle_of(task_id).is_ok_and(|lifecycle| lifecycle.working_on_reply().is_some() || lifecycle.awaits().is_some())
    }
    /// The same for an active Task, through its current final delivery.
    /// The PR number and reason when the current final delivery went back because Alondra
    /// could not merge it.
    pub fn task_merge_failure(&self, task_id: &TaskId) -> Option<(u64, String)> {
        self.task_handed_back(task_id).filter(|i| *i == Instruction::Conflicts)?;
        let task = self.tasks.get(task_id)?;
        let InteractionPayload::Delivery { feedback, .. } = &self.interactions.get(task.final_presentation.as_ref()?)?.payload else {
            return None;
        };
        let failure = feedback.last()?.merge_failure.as_ref()?;
        Some((failure.number, failure.reason.clone()))
    }
    pub fn task_handed_back(&self, task_id: &TaskId) -> Option<Instruction> {
        let task = self.tasks.get(task_id).filter(|t| !t.over())?;
        self.handed_back(self.interactions.get(task.final_presentation.as_ref()?)?)
    }
    /// A delivery of several PRs is one card per PR still open, each offering only that PR's
    /// merge, so the user reviews and merges each apart. Any other card stands for itself.
    pub fn pr_cards(&self, item: &InboxItem) -> Vec<InboxItem> {
        let whole = || vec![item.clone()];
        let InboxSubject::Interaction { interaction_id, pr_id: None } = &item.subject else { return whole() };
        let Some(InteractionPayload::Delivery { targets, .. }) = self.interactions.get(interaction_id).map(|i| &i.payload) else {
            return whole();
        };
        let prs: Vec<(&PrId, &str)> = targets.iter().filter_map(|target| match target {
            DeliveryTarget::PullRequest { pr_id, head } => Some((pr_id, head.as_str())),
            _ => None,
        }).collect();
        let open: Vec<(&PrId, &str)> = prs.iter().copied()
            .filter(|(id, _)| self.pull_requests.get(*id).is_some_and(|pr| pr.state == PrState::Open))
            .collect();
        if prs.len() < 2 || open.is_empty() {
            return whole();
        }
        open.into_iter().map(|(pr_id, head)| {
            let mut card = item.clone();
            card.subject = InboxSubject::Interaction { interaction_id: interaction_id.clone(), pr_id: Some(pr_id.clone()) };
            card.available_actions.retain(|action| action.kind != "accept_delivery"
                && (action.kind != "authorize_merge" || action.expected_head.as_deref() == Some(head)));
            card
        }).collect()
    }
    /// The card decided from the state alone: what the index must agree with.
    #[cfg(test)]
    pub(crate) fn item(&self, i: &Interaction) -> InboxItem {
        let pending = self.inbox_pending(i, |t| self.attention(t));
        self.item_given(i, pending, self.inbox_blockers(i), self.blocked_deliveries(i), self)
    }
    fn inbox_task(&self, i: &Interaction) -> Option<&Task> {
        match &i.scope {
            Scope::Task { task_id } => self.tasks.get(task_id),
            _ => None,
        }
    }
    fn inbox_active(task: Option<&Task>) -> bool {
        task.is_none_or(|t| !t.over() || Self::winding_down(t))
    }
    /// Whether the card waits for the user now, which is what lists it in Waiting for You; a
    /// delivery only while its task's attention is that card.
    pub(super) fn inbox_pending(&self, i: &Interaction, attention: impl FnOnce(&Task) -> Attention) -> bool {
        i.snoozed_until.is_none() && self.card_waits_for_user(i, attention)
    }
    pub(super) fn card_waits_for_user(&self, i: &Interaction, attention: impl FnOnce(&Task) -> Attention) -> bool {
        let task = self.inbox_task(i);
        let active = Self::inbox_active(task);
        match &i.payload {
            InteractionPayload::Information { read_by } => read_by.is_empty() && !i.withdrawn,
            InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. } => {
                answer.is_none() && !i.withdrawn && active && !self.grouped_away(i) && !self.mcp_grouped_away(i) && !self.out_of_sight(i)
            }
            InteractionPayload::Delivery { acceptance, .. } => {
                task.is_some_and(|t| t.final_presentation.as_ref() == Some(&i.id))
                    && active
                    && !acceptance.as_ref().is_some_and(|a| a.valid)
                    && self.handed_back(i).is_none()
                    && task.is_some_and(|t| matches!(attention(t), Attention::Act(card) if card.interaction == i.id))
            }
        }
    }
    /// The card with its buttons, given whether it waits for the user and what it holds back or
    /// is held back by.
    pub(super) fn item_given(
        &self,
        i: &Interaction,
        pending: bool,
        blocked_by: Vec<InteractionId>,
        blocks: Vec<InteractionId>,
        holds: &dyn Holds,
    ) -> InboxItem {
        let task_id = match &i.scope {
            Scope::Task { task_id } => Some(task_id.clone()),
            _ => None,
        };
        let task = self.inbox_task(i);
        let request_id = task
            .map(|t| t.request_id.clone())
            .or_else(|| match &i.scope {
                Scope::Orchestrator { request_id } => request_id.clone(),
                _ => None,
            });
        let agent_id = match &i.author {
            Author::Agent { agent_id } => Some(agent_id.clone()),
            _ => None,
        };
        let active = Self::inbox_active(task);
        let (kind, mut actions) = match &i.payload {
            InteractionPayload::Information { read_by } => (
                "information",
                vec![AvailableAction {
                    kind: "mark_read".into(),
                    enabled: read_by.is_empty(),
                    disabled_reason: (!read_by.is_empty()).then(|| "already_read".into()),
                    refusal: None,
                    expected_revision: self.revision,
                    presentation_id: None,
                    expected_head: None,
                }],
            ),
            InteractionPayload::Question { answer, .. }
            | InteractionPayload::ActionRequest { answer, .. } => {
                let enabled =
                    answer.is_none() && !i.withdrawn && active && i.recipient == Recipient::User;
                let reason = if i.withdrawn {
                    Some("withdrawn".into())
                } else if i.recipient != Recipient::User {
                    Some("forbidden".into())
                } else if !active {
                    Some("task_not_active".into())
                } else if answer.is_some() {
                    Some("invalid_action_for_kind".into())
                } else {
                    None
                };
                (
                    match self.holds.get(&i.id) {
                        Some(Hold::Permission) => "permission",
                        Some(Hold::Plan) => "plan",
                        Some(Hold::Question | Hold::Elicitation) => "question",
                        None if matches!(i.payload, InteractionPayload::Question { .. }) => "question",
                        None => "action_request",
                    },
                    vec![AvailableAction {
                        kind: "answer_question".into(),
                        enabled,
                        disabled_reason: reason,
                        refusal: None,
                        expected_revision: self.revision,
                        presentation_id: None,
                        expected_head: None,
                    }],
                )
            }
            InteractionPayload::Delivery { targets, .. } => {
                let accepting = if i.card.plan.is_some() {
                    vec![self.action(
                        "approve_plan",
                        Command::DecideProposedPlan { presentation_id: i.id.clone(), targets: targets.clone(), choice: PlanChoice::Implement },
                        Some(i.id.clone()),
                        None,
                        holds,
                    )]
                } else {
                    vec![self.action(
                        "accept_delivery",
                        Command::AcceptDelivery {
                            presentation_id: i.id.clone(),
                            targets: targets.clone(),
                        },
                        Some(i.id.clone()),
                        None,
                        holds,
                    )]
                };
                let mut actions = accepting;
                actions.extend([
                    self.action(
                        "submit_feedback",
                        Command::SubmitFeedback {
                            attachments: vec![],
                            presentation_id: i.id.clone(),
                            targets: targets.clone(),
                            body: "feedback".into(),
                            instruction: Instruction::Changes,
                            merge_failure: None,
                        },
                        Some(i.id.clone()),
                        None,
                        holds,
                    ),
                ]);
                for target in targets {
                    if let DeliveryTarget::PullRequest { pr_id, head } = target {
                        if let Some(pr) = self.pull_requests.get(pr_id) {
                            if let Some(method) = MergeMethod::for_pr(self.merge_method, &pr.allowed_methods) {
                                let mut token = "projection-authorization".to_owned();
                                while self
                                    .merge_authorizations
                                    .contains_key(&AuthorizationId(token.clone()))
                                {
                                    token.push('x');
                                }
                                actions.push(self.action(
                                    "authorize_merge",
                                    Command::AuthorizeMerge {
                                        authorization_id: token.into(),
                                        presentation_id: i.id.clone(),
                                        pr_id: pr_id.clone(),
                                        expected_head: head.clone(),
                                        method,
                                    },
                                    Some(i.id.clone()),
                                    Some(head.clone()),
                                    holds,
                                ));
                            }
                        }
                    }
                }
                ("delivery", actions)
            }
        };
        if let Some(action) = self.clarification_action(i) {
            actions.push(action);
        }
        InboxItem {
            id: format!("interaction:{}", i.id),
            kind: kind.into(),
            subject: InboxSubject::Interaction {
                interaction_id: i.id.clone(),
                pr_id: None,
            },
            title: task
                .map(|t| t.title.clone())
                .unwrap_or_else(|| "Orchestrator".into()),
            body: Some(i.body.clone()),
            task_id,
            request_id,
            agent_id,
            revision: i.revision,
            order: i.order,
            pending,
            available_actions: actions,
            card: i.card.clone(),
            blocks,
            blocked_by,
        }
    }
    /// "Ask for clarification" is offered on an open card an agent or the orchestrator wrote,
    /// unless Claude Code is waiting on it; the same guards as the command.
    fn clarification_action(&self, i: &Interaction) -> Option<AvailableAction> {
        if i.recipient != Recipient::User
            || self.holds.contains_key(&i.id)
            || matches!(i.author, Author::System)
            || matches!(i.payload, InteractionPayload::Delivery { .. }) && !self.open(i)
        {
            return None;
        }
        let open = self.open(i);
        Some(AvailableAction {
            kind: "request_clarification".into(),
            enabled: open,
            disabled_reason: (!open).then(|| "invalid_action_for_kind".into()),
            refusal: None,
            expected_revision: self.revision,
            presentation_id: None,
            expected_head: None,
        })
    }
    /// What holds a delivery back: the open cards that block it, then the delivery of the PR
    /// it is stacked on while that one still waits.
    pub fn delivery_blocked_by(&self, delivery: &Interaction) -> Vec<InteractionId> {
        let mut by = self.blocked_by(&delivery.id);
        let upstream = match &delivery.scope {
            Scope::Task { task_id } => self.tasks.get(task_id).and_then(|t| t.stack.as_ref()),
            _ => None,
        };
        if let Some(stack) = upstream
            && let Some(upstream_delivery) = self.interactions.get(&stack.presentation_id)
            && self.open(upstream_delivery)
            && upstream_delivery.id != delivery.id
            && self.pull_requests.get(&stack.pr_id).is_none_or(|pr| pr.state == PrState::Open)
            && !by.contains(&upstream_delivery.id)
        {
            by.push(upstream_delivery.id.clone());
        }
        by
    }
    /// The "Waiting for You" order: a problem, then what holds a node up right now, then what
    /// to review or test, what to provide, proposals and notices; the oldest first in each
    /// group, and a card that holds another back sits right above it.
    fn inbox_rank(kind: CardKind, blocking: bool) -> u8 {
        match kind {
            CardKind::Problem => 0,
            CardKind::Authorize | CardKind::Do | CardKind::Decide | CardKind::Provide if blocking => 1,
            CardKind::Authorize | CardKind::Do | CardKind::Decide | CardKind::Review | CardKind::Test => 2,
            CardKind::Provide => 3,
            CardKind::Propose => 4,
            CardKind::Know => 5,
        }
    }
    pub(super) fn inbox_sequence(facts: &[InboxOrderFacts<'_>]) -> Vec<usize> {
        let mut sorted: Vec<(u8, usize)> = facts
            .iter()
            .enumerate()
            .map(|(index, item)| (Self::inbox_rank(item.kind, item.blocking), index))
            .collect();
        sorted.sort_by(|(ra, a), (rb, b)| {
            let a = &facts[*a];
            let b = &facts[*b];
            (a.resurfaced.is_none(), std::cmp::Reverse(a.resurfaced), ra, a.order, a.id)
                .cmp(&(b.resurfaced.is_none(), std::cmp::Reverse(b.resurfaced), rb, b.order, b.id))
        });
        let position: BTreeMap<InteractionId, usize> = sorted
            .iter()
            .enumerate()
            .map(|(index, (_, item))| (facts[*item].interaction_id.clone(), index))
            .collect();
        let blockers: Vec<Vec<usize>> = sorted
            .iter()
            .map(|(_, item)| {
                let mut at: Vec<usize> = facts[*item].blocked_by.iter().filter_map(|id| position.get(id).copied()).collect();
                at.sort_unstable();
                at
            })
            .collect();
        let mut dependents = vec![vec![]; blockers.len()];
        for (dependent, held_by) in blockers.iter().enumerate() {
            for (offset, &blocker) in held_by.iter().enumerate() {
                if offset == 0 || held_by[offset - 1] != blocker {
                    dependents[blocker].push(dependent);
                }
            }
        }
        let mut placed = vec![false; sorted.len()];
        let mut sequence = Vec::with_capacity(sorted.len());
        fn place(at: usize, blockers: &[Vec<usize>], dependents: &[Vec<usize>], placed: &mut [bool], sequence: &mut Vec<usize>) {
            if placed[at] {
                return;
            }
            placed[at] = true;
            for &blocker in &blockers[at] {
                place(blocker, blockers, dependents, placed, sequence);
            }
            sequence.push(at);
            for &dependent in &dependents[at] {
                if !placed[dependent] && blockers[dependent].iter().all(|b| placed[*b]) {
                    place(dependent, blockers, dependents, placed, sequence);
                }
            }
        }
        for at in 0..sorted.len() {
            place(at, &blockers, &dependents, &mut placed, &mut sequence);
        }
        sequence.into_iter().map(|at| sorted[at].1).collect()
    }
    #[cfg(test)]
    fn order_inbox(&self, items: Vec<InboxItem>) -> Vec<InboxItem> {
        let sequence = {
            let facts: Vec<_> = items.iter().map(|item| {
                let InboxSubject::Interaction { interaction_id, .. } = &item.subject;
                InboxOrderFacts {
                    id: &item.id,
                    interaction_id,
                    order: item.order,
                    resurfaced: self.interactions.get(interaction_id).and_then(|i| i.resurfaced_at_revision),
                    kind: item.card.kind,
                    blocking: self.interactions.get(interaction_id).is_some_and(|i| i.blocking),
                    blocked_by: &item.blocked_by,
                }
            }).collect();
            Self::inbox_sequence(&facts)
        };
        let mut slots: Vec<Option<InboxItem>> = items.into_iter().map(Some).collect();
        sequence.into_iter().filter_map(|at| slots[at].take()).collect()
    }
    fn inbox_blockers(&self, i: &Interaction) -> Vec<InteractionId> {
        match &i.payload {
            InteractionPayload::Delivery { .. } => self.delivery_blocked_by(i),
            _ => vec![],
        }
    }
    pub(super) fn select_inbox<'a, T>(&'a self, mut project: impl FnMut(&'a Interaction) -> T) -> Vec<T> {
        let mut items = vec![];
        for i in self.interactions.values() {
            if i.recipient != Recipient::User {
                continue;
            }
            let task = match &i.scope {
                Scope::Task { task_id } => self.tasks.get(task_id),
                _ => None,
            };
            if task.is_some_and(|t| t.archived) {
                continue;
            }
            if matches!(i.payload, InteractionPayload::Delivery { .. })
                && task.is_none_or(|t| t.final_presentation.as_ref() != Some(&i.id))
            {
                continue;
            }
            items.push(project(i));
        }
        items
    }
    pub fn inbox_orders(&self) -> Vec<(String, Revision)> {
        let entries = self.select_inbox(|i| (format!("interaction:{}", i.id), i, self.inbox_blockers(i)));
        let sequence = {
            let facts: Vec<_> = entries.iter().map(|(id, i, blocked_by)| InboxOrderFacts {
                id,
                interaction_id: &i.id,
                order: i.order,
                resurfaced: i.resurfaced_at_revision,
                kind: i.card.kind,
                blocking: self.interactions.get(&i.id).is_some_and(|i| i.blocking),
                blocked_by,
            }).collect();
            Self::inbox_sequence(&facts)
        };
        let mut slots: Vec<_> = entries.into_iter().map(|(id, i, _)| Some((id, i.order))).collect();
        sequence.into_iter().filter_map(|at| slots[at].take()).collect()
    }

}

#[cfg(test)]
mod inbox_order_tests {
    use super::*;

    fn item(id: &str, order: Revision, kind: CardKind, blockers: &[&str]) -> InboxItem {
        InboxItem {
            id: format!("interaction:{id}"), kind: "information".into(),
            subject: InboxSubject::Interaction { interaction_id: id.into(), pr_id: None },
            title: id.into(), body: Some("Body is irrelevant to ordering".into()),
            task_id: None, request_id: None, agent_id: None,
            revision: order, order, pending: false, available_actions: vec![],
            card: CardSpec::new(kind, "Ordering fixture"), blocks: vec![],
            blocked_by: blockers.iter().map(|id| (*id).into()).collect(),
        }
    }
    fn ordered(items: Vec<InboxItem>) -> Vec<String> {
        DomainState::new("ordering-fixture").order_inbox(items).into_iter().map(|item| item.id).collect()
    }

    #[test]
    fn inbox_rank_then_revision_then_formatted_id_is_preserved() {
        let items = vec![
            item("z", 2, CardKind::Know, &[]), item("a0", 2, CardKind::Know, &[]),
            item("a-1", 2, CardKind::Know, &[]), item("a", 2, CardKind::Know, &[]),
            item("proposal", 1, CardKind::Propose, &[]), item("provide", 1, CardKind::Provide, &[]),
            item("test", 1, CardKind::Test, &[]), item("problem", 8, CardKind::Problem, &[]),
        ];
        assert_eq!(ordered(items), ["interaction:problem", "interaction:test", "interaction:provide", "interaction:proposal",
            "interaction:a", "interaction:a-1", "interaction:a0", "interaction:z"]);
    }

    #[test]
    fn inbox_chain_multiple_and_missing_blockers_place_every_item_once() {
        let items = vec![
            item("leaf", 1, CardKind::Know, &["middle", "root", "not-selected"]),
            item("middle", 2, CardKind::Know, &["root"]), item("root", 3, CardKind::Know, &[]),
            item("other", 4, CardKind::Know, &["not-selected"]),
        ];
        assert_eq!(ordered(items), ["interaction:root", "interaction:middle", "interaction:leaf", "interaction:other"]);
    }

    #[test]
    fn inbox_synthetic_cycle_preserves_the_existing_each_once_permutation() {
        // Command guards reject cycles; the ordering kernel still terminates on this input.
        assert_eq!(ordered(vec![item("a", 1, CardKind::Know, &["b"]), item("b", 2, CardKind::Know, &["a"])]),
            ["interaction:b", "interaction:a"]);
    }
    #[test]
    fn inbox_duplicate_blockers_self_edges_and_cycles_keep_mark_before_emission() {
        assert_eq!(ordered(vec![
            item("a", 1, CardKind::Know, &["b", "b"]),
            item("b", 2, CardKind::Know, &["a", "b"]),
            item("c", 3, CardKind::Know, &["b", "b", "missing"]),
        ]), ["interaction:b", "interaction:c", "interaction:a"]);
    }

    #[test]
    fn inbox_diamond_checks_readiness_during_recursive_ascending_scan() {
        assert_eq!(ordered(vec![
            item("leaf", 1, CardKind::Know, &["a", "b"]),
            item("c", 2, CardKind::Know, &["a"]),
            item("a", 3, CardKind::Know, &[]), item("b", 4, CardKind::Know, &[]),
        ]), ["interaction:a", "interaction:c", "interaction:b", "interaction:leaf"]);
    }

    #[test]
    fn inbox_ready_siblings_use_sorted_position_not_arrival_order() {
        assert_eq!(ordered(vec![
            item("z", 2, CardKind::Know, &["root"]),
            item("a0", 2, CardKind::Know, &["root"]),
            item("root", 9, CardKind::Know, &[]),
            item("a", 2, CardKind::Know, &["root", "root"]),
        ]), ["interaction:root", "interaction:a0", "interaction:z", "interaction:a"]);
    }

    #[test]
    fn inbox_disconnected_cycle_components_keep_the_root_loop_order() {
        assert_eq!(ordered(vec![
            item("a", 1, CardKind::Know, &["b"]), item("b", 2, CardKind::Know, &["a"]),
            item("c", 3, CardKind::Know, &["d"]), item("d", 4, CardKind::Know, &["c"]),
        ]), ["interaction:b", "interaction:a", "interaction:d", "interaction:c"]);
    }

    // Frozen pre-index procedure; the expected permutation does not call the production kernel.
    fn legacy_sequence(blockers: &[Vec<usize>]) -> Vec<usize> {
        fn place(at: usize, blockers: &[Vec<usize>], placed: &mut [bool], sequence: &mut Vec<usize>) {
            if placed[at] { return; }
            placed[at] = true;
            for &blocker in &blockers[at] { place(blocker, blockers, placed, sequence); }
            sequence.push(at);
            for (dependent, held_by) in blockers.iter().enumerate() {
                if !placed[dependent] && held_by.contains(&at) && held_by.iter().all(|b| placed[*b]) {
                    place(dependent, blockers, placed, sequence);
                }
            }
        }
        let mut placed = vec![false; blockers.len()];
        let mut sequence = vec![];
        for at in 0..blockers.len() { place(at, blockers, &mut placed, &mut sequence); }
        sequence
    }

    #[test]
    fn inbox_all_three_node_edge_graphs_match_the_frozen_scan_permutation() {
        let ids = ["a", "b", "c"];
        for graph in 0..512 {
            let blockers: Vec<Vec<usize>> = (0..3).map(|dependent| {
                (0..3).filter(|blocker| graph & (1 << (dependent * 3 + blocker)) != 0).collect()
            }).collect();
            let expected: Vec<_> = legacy_sequence(&blockers).into_iter().map(|i| format!("interaction:{}", ids[i])).collect();
            let items = (0..3).map(|i| {
                let held_by: Vec<_> = blockers[i].iter().map(|b| ids[*b]).collect();
                item(ids[i], i as u64 + 1, CardKind::Know, &held_by)
            }).collect();
            let actual = ordered(items);
            assert_eq!(actual.len(), 3, "graph {graph} must emit every entry once");
            assert_eq!(actual, expected, "edge graph {graph}");
        }
    }

}

fn last_hand_back(feedback: &[Feedback]) -> Option<&Feedback> {
    feedback.iter().rev().find(|f| f.instruction != Instruction::Reply)
}

/// Every button any test projects on a valid state is also judged by running its command on a
/// copy: the guard must refuse exactly what the command refuses, with the same reason. A state
/// built by hand gets the journal its columns lead to first; one that breaks the domain's
/// invariants, or that no journal leads to, refuses every command, so it says nothing.
#[cfg(test)]
pub(super) fn assert_guard_matches_execution(
    state: &DomainState,
    actor: &Actor,
    envelope: &CommandEnvelope,
    guarded: Option<&DomainError>,
) {
    if state.validate().is_err() || state.validate_nodes().is_err() {
        return;
    }
    let mut journaled = state.clone();
    if !journaled.journal_every_task() {
        return;
    }
    let executed = journaled.preview(actor, envelope).err();
    assert_eq!(
        guarded,
        executed.as_ref(),
        "the guard and the command disagree on {:?}",
        envelope.command
    );
}
