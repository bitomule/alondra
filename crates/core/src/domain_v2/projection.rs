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
    PullRequest {
        pr_id: PrId,
    },
    DocumentReview {
        task_id: TaskId,
        document_id: DocumentId,
        revision_id: DocumentRevisionId,
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
    pub children: Vec<InboxItem>,
    pub available_actions: Vec<AvailableAction>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableAction {
    pub kind: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
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
        let mut scratch = self.clone();
        let error = scratch
            .execute(
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
            disabled_reason: error.map(|e| e.code),
            expected_revision: self.revision,
            presentation_id: presentation,
            expected_head: head,
        }
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
        let active = task.is_none_or(|t| !t.state.terminal());
        let (kind, pending, actions) = match &i.payload {
            InteractionPayload::Information { read_by } => (
                "information",
                read_by.is_empty(),
                vec![AvailableAction {
                    kind: "mark_read".into(),
                    enabled: read_by.is_empty(),
                    disabled_reason: (!read_by.is_empty()).then(|| "already_read".into()),
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
                    if matches!(i.payload, InteractionPayload::Question { .. }) {
                        "question"
                    } else {
                        "action_request"
                    },
                    answer.is_none() && !i.withdrawn,
                    vec![AvailableAction {
                        kind: "answer_question".into(),
                        enabled,
                        disabled_reason: reason,
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
                    current && active && !acceptance.as_ref().is_some_and(|a| a.valid),
                    actions,
                )
            }
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
            children: vec![],
            available_actions: actions,
        }
    }
    /// Complete coherent projection. IDs, grouping and action guards are server owned.
    /// read_by is shared across the single owner domain, irrespective of device identity.
    pub fn projection(&self) -> DomainProjection {
        let mut roots: BTreeMap<String, InboxItem> = BTreeMap::new();
        let mut ungrouped = vec![];
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
            let item = self.item(i);
            let explicit = i.subject.clone().or_else(|| match &i.payload {
                InteractionPayload::Delivery { targets, .. } => targets
                    .iter()
                    .find_map(|target| match target {
                        DeliveryTarget::PullRequest { pr_id, .. } => {
                            Some(InteractionSubject::PullRequest {
                                pr_id: pr_id.clone(),
                            })
                        }
                        _ => None,
                    })
                    .or_else(|| {
                        targets.iter().find_map(|target| match target {
                            DeliveryTarget::DocumentRevision {
                                document_id,
                                revision_id,
                            } => Some(InteractionSubject::DocumentReview {
                                document_id: document_id.clone(),
                                revision_id: revision_id.clone(),
                            }),
                            _ => None,
                        })
                    }),
                _ => None,
            });
            if let (Some(subject), Some(t)) = (explicit, task) {
                let (key, kind, subject, title) = match subject {
                    InteractionSubject::PullRequest { pr_id } => {
                        let p = &self.pull_requests[&pr_id];
                        (
                            format!("pr:{pr_id}"),
                            "pull_request",
                            InboxSubject::PullRequest { pr_id },
                            format!("{} #{}", p.repository, p.number),
                        )
                    }
                    InteractionSubject::DocumentReview {
                        document_id,
                        revision_id,
                    } => {
                        let d = &self.documents[&document_id];
                        (
                            format!("document:{}:{}:{}", t.id, document_id, revision_id),
                            "document_review",
                            InboxSubject::DocumentReview {
                                task_id: t.id.clone(),
                                document_id,
                                revision_id,
                            },
                            d.title.clone(),
                        )
                    }
                };
                let root = roots.entry(key.clone()).or_insert_with(|| InboxItem {
                    id: key,
                    kind: kind.into(),
                    subject,
                    title,
                    body: None,
                    task_id: Some(t.id.clone()),
                    request_id: Some(t.request_id.clone()),
                    agent_id: t.agent_id.clone(),
                    revision: self.revision,
                    order: i.order,
                    pending: false,
                    children: vec![],
                    available_actions: vec![],
                });
                root.order = root.order.min(item.order);
                root.pending |= item.pending;
                root.children.push(item);
            } else {
                ungrouped.push(item)
            }
        }
        for root in roots.values_mut() {
            root.children
                .sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
        }
        let mut inbox: Vec<_> = roots.into_values().chain(ungrouped).collect();
        inbox.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
        fn pending(items: &[InboxItem]) -> u64 {
            items
                .iter()
                .map(|i| {
                    if i.children.is_empty() {
                        u64::from(i.pending)
                    } else {
                        pending(&i.children)
                    }
                })
                .sum()
        }
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
            pending: pending(&inbox),
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
