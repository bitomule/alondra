use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainProjection {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub inbox: Vec<InboxItem>,
    pub counts: InboxCounts,
    pub tasks: Vec<Task>,
    pub agents: Vec<Agent>,
    pub requests: Vec<Request>,
    pub interactions: Vec<Interaction>,
    pub pull_requests: Vec<PullRequest>,
    pub documents: Vec<Document>,
    pub document_revisions: Vec<DocumentRevision>,
    pub document_reviews: Vec<DocumentReview>,
    pub merge_authorizations: Vec<MergeAuthorization>,
    pub stack_readiness: Vec<StackReadiness>,
    /// Per active Task, its latest acceptance when that no longer applies
    /// and the user did not withdraw it: "your acceptance no longer applies".
    #[serde(default)]
    pub superseded_acceptances: Vec<SupersededAcceptance>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxCounts {
    pub roots: u64,
    pub pending: u64,
    pub unread_information: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboxSubject {
    Interaction {
        interaction_id: InteractionId,
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
impl DomainState {
    /// User task controls are computed by the same guards as command execution.
    pub fn task_actions(&self, task_id: &TaskId) -> Option<Vec<AvailableAction>> {
        let task = self.tasks.get(task_id)?;
        let archive = !task.archived;
        Some(vec![
            self.action(
                "cancel_task",
                Command::CancelTask {
                    task_id: task_id.clone(),
                },
                None,
                None,
            ),
            self.action(
                "reopen_task",
                Command::ReopenTask {
                    task_id: task_id.clone(),
                },
                None,
                None,
            ),
            self.action(
                if archive {
                    "archive_task"
                } else {
                    "unarchive_task"
                },
                Command::ArchiveTask {
                    task_id: task_id.clone(),
                    archived: archive,
                },
                None,
                None,
            ),
        ])
    }
    fn action(
        &self,
        kind: &str,
        command: Command,
        presentation: Option<InteractionId>,
        head: Option<String>,
    ) -> AvailableAction {
        let error = self
            .preview(
                &Actor::User {
                    user_id: "projection-user".into(),
                },
                &CommandEnvelope {
                    protocol_version: PROTOCOL_VERSION,
                    data_epoch: self.data_epoch.clone(),
                    command_id: "projection-validation".into(),
                    expected_revision: self.revision,
                    command,
                },
            )
            .err();
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
    /// Agent presenting again puts it back in front of the user.
    pub fn handed_back(&self, presentation: &Interaction) -> Option<Instruction> {
        let InteractionPayload::Delivery { acceptance, feedback, .. } = &presentation.payload else {
            return None;
        };
        feedback
            .last()
            .filter(|f| acceptance.as_ref().is_none_or(|a| f.at_revision > a.at_revision))
            .map(|f| f.instruction)
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
        let (number, reason) = merge_failure(&feedback.last()?.body)?;
        Some((number, reason.to_owned()))
    }
    pub fn task_handed_back(&self, task_id: &TaskId) -> Option<Instruction> {
        let task = self.tasks.get(task_id).filter(|t| !t.state.terminal())?;
        self.handed_back(self.interactions.get(task.final_presentation.as_ref()?)?)
    }
    pub(crate) fn item(&self, i: &Interaction) -> InboxItem {
        let task_id = match &i.scope {
            Scope::Task { task_id } => Some(task_id.clone()),
            _ => None,
        };
        let task = task_id.as_ref().and_then(|id| self.tasks.get(id));
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
        let active = task.is_none_or(|t| !t.state.terminal() || Self::winding_down(t));
        let (kind, pending, mut actions) = match &i.payload {
            InteractionPayload::Information { read_by } => (
                "information",
                read_by.is_empty() && !i.withdrawn,
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
                    answer.is_none() && !i.withdrawn && active,
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
            InteractionPayload::Delivery {
                targets,
                acceptance,
                ..
            } => {
                let current = task.is_some_and(|t| t.final_presentation.as_ref() == Some(&i.id));
                let mut actions = vec![
                    self.action(
                        "accept_delivery",
                        Command::AcceptDelivery {
                            presentation_id: i.id.clone(),
                            targets: targets.clone(),
                        },
                        Some(i.id.clone()),
                        None,
                    ),
                    self.action(
                        "submit_feedback",
                        Command::SubmitFeedback {
                            attachments: vec![],
                            presentation_id: i.id.clone(),
                            targets: targets.clone(),
                            body: "feedback".into(),
                            instruction: Instruction::Changes,
                        },
                        Some(i.id.clone()),
                        None,
                    ),
                ];
                for target in targets {
                    if let DeliveryTarget::PullRequest { pr_id, head } = target {
                        if let Some(pr) = self.pull_requests.get(pr_id) {
                            if let Some(method) = pr.allowed_methods.first() {
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
                                        method: *method,
                                    },
                                    Some(i.id.clone()),
                                    Some(head.clone()),
                                ));
                            }
                        }
                    }
                }
                (
                    "delivery",
                    current
                        && active
                        && !acceptance.as_ref().is_some_and(|a| a.valid)
                        && self.handed_back(i).is_none()
                        && task.is_some_and(|t| matches!(self.attention(t), Attention::Act(card) if card.interaction == i.id)),
                    actions,
                )
            }
        };
        if let Some(action) = self.clarification_action(i) {
            actions.push(action);
        }
        let blocked_by = match &i.payload {
            InteractionPayload::Delivery { .. } => self.delivery_blocked_by(i),
            _ => vec![],
        };
        InboxItem {
            id: format!("interaction:{}", i.id),
            kind: kind.into(),
            subject: InboxSubject::Interaction {
                interaction_id: i.id.clone(),
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
            blocks: self.blocked_deliveries(i),
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
    fn inbox_rank(item: &InboxItem, blocking: bool) -> u8 {
        match item.card.kind {
            CardKind::Problem => 0,
            CardKind::Authorize | CardKind::Do | CardKind::Decide | CardKind::Provide if blocking => 1,
            CardKind::Authorize | CardKind::Do | CardKind::Decide | CardKind::Review | CardKind::Test => 2,
            CardKind::Provide => 3,
            CardKind::Propose => 4,
            CardKind::Know => 5,
        }
    }
    fn order_inbox(&self, items: Vec<InboxItem>) -> Vec<InboxItem> {
        let blocking = |item: &InboxItem| {
            let InboxSubject::Interaction { interaction_id } = &item.subject;
            self.interactions.get(interaction_id).is_some_and(|i| i.blocking)
        };
        let mut sorted: Vec<(u8, InboxItem)> = items
            .into_iter()
            .map(|item| (Self::inbox_rank(&item, blocking(&item)), item))
            .collect();
        sorted.sort_by(|(ra, a), (rb, b)| (ra, a.order, &a.id).cmp(&(rb, b.order, &b.id)));
        let interaction_of = |item: &InboxItem| {
            let InboxSubject::Interaction { interaction_id } = &item.subject;
            Some(interaction_id.clone())
        };
        let position: BTreeMap<InteractionId, usize> = sorted
            .iter()
            .enumerate()
            .filter_map(|(index, (_, item))| interaction_of(item).map(|id| (id, index)))
            .collect();
        let blockers: Vec<Vec<usize>> = sorted
            .iter()
            .map(|(_, item)| {
                let mut at: Vec<usize> = item.blocked_by.iter().filter_map(|id| position.get(id).copied()).collect();
                at.sort_unstable();
                at
            })
            .collect();
        let mut placed = vec![false; sorted.len()];
        let mut sequence = Vec::with_capacity(sorted.len());
        fn place(at: usize, blockers: &[Vec<usize>], placed: &mut [bool], sequence: &mut Vec<usize>) {
            if placed[at] {
                return;
            }
            placed[at] = true;
            for &blocker in &blockers[at] {
                place(blocker, blockers, placed, sequence);
            }
            sequence.push(at);
            for (dependent, held_by) in blockers.iter().enumerate() {
                if !placed[dependent] && held_by.contains(&at) && held_by.iter().all(|b| placed[*b]) {
                    place(dependent, blockers, placed, sequence);
                }
            }
        }
        for at in 0..sorted.len() {
            place(at, &blockers, &mut placed, &mut sequence);
        }
        let mut slots: Vec<Option<InboxItem>> = sorted.into_iter().map(|(_, item)| Some(item)).collect();
        sequence.into_iter().filter_map(|at| slots[at].take()).collect()
    }
    /// Complete coherent projection. IDs, order and action guards are server owned.
    /// read_by is shared across the single owner domain, irrespective of device identity.
    pub fn projection(&self) -> DomainProjection {
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
            items.push(self.item(i));
        }
        let inbox = self.order_inbox(items);
        let unread_information = self
            .interactions
            .values()
            .filter(|i| {
                i.recipient == Recipient::User
                    && matches!(&i.payload, InteractionPayload::Information { read_by } if read_by.is_empty())
                    && !matches!(&i.scope, Scope::Task { task_id } if self.tasks.get(task_id).is_some_and(|t| t.archived))
            })
            .count() as u64;
        let counts = InboxCounts {
            roots: inbox.len() as u64,
            pending: inbox.iter().filter(|item| item.pending).count() as u64,
            unread_information,
        };
        DomainProjection {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: self.data_epoch.clone(),
            revision: self.revision,
            inbox,
            counts,
            tasks: self.tasks.values().cloned().collect(),
            agents: self.agents.values().cloned().collect(),
            requests: self.requests.values().cloned().collect(),
            interactions: self.interactions.values().cloned().collect(),
            pull_requests: self.pull_requests.values().cloned().collect(),
            documents: self.documents.values().cloned().collect(),
            document_revisions: self.document_revisions.values().cloned().collect(),
            document_reviews: self.document_reviews.clone(),
            merge_authorizations: self.merge_authorizations.values().cloned().collect(),
            stack_readiness: self.stack_readiness.values().cloned().collect(),
            superseded_acceptances: self.superseded_acceptances(),
        }
    }
    fn superseded_acceptances(&self) -> Vec<SupersededAcceptance> {
        self.tasks
            .values()
            .filter(|t| !t.state.terminal())
            .filter_map(|t| {
                self.interactions
                    .values()
                    .filter(|p| {
                        p.scope
                            == (Scope::Task {
                                task_id: t.id.clone(),
                            })
                    })
                    .filter_map(|p| match &p.payload {
                        InteractionPayload::Delivery {
                            acceptance: Some(a),
                            ..
                        } => Some((a.at_revision, p)),
                        _ => None,
                    })
                    .max_by_key(|(at, _)| *at)
                    .and_then(|(_, p)| self.superseded(p))
            })
            .collect()
    }
}
