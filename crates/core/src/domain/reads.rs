//! Sectioned read contract (docs/domain-v2-read-endpoints.md). Rows reuse the
//! domain DTOs; these envelopes add paging, per-section revision tokens and
//! the truncation signal for oversized text.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SECTIONS: [&str; 11] = [
    "inbox",
    "tasks",
    "requests",
    "agents",
    "interactions",
    "pull_requests",
    "documents",
    "merge_authorizations",
    "stack_readiness",
    "superseded_acceptances",
    "schedules",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionInfo {
    pub revision: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub counts: InboxCounts,
    pub open_tasks: u64,
    pub connected_agents: u64,
    pub sections: BTreeMap<String, SectionInfo>,
    #[serde(default)]
    pub expansions: BTreeMap<String, Page<serde_json::Value>>,
    #[serde(default)]
    pub pending: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub section: String,
    pub section_revision: String,
    pub total: u64,
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub truncated: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item<T> {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub item: T,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxLookup {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub item: InboxItem,
    pub root_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentBundle {
    pub document: Document,
    pub revisions: Vec<DocumentRevision>,
    pub reviews: Vec<DocumentReview>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub task: Task,
    pub request: Request,
    pub agent: Option<Agent>,
    pub pull_request: Option<PullRequest>,
    pub documents: Vec<DocumentBundle>,
    pub merge_authorizations: Vec<MergeAuthorization>,
    pub stack_readiness: Option<StackReadiness>,
    pub superseded_acceptance: Option<SupersededAcceptance>,
    pub actions: Vec<AvailableAction>,
    pub interactions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestDetail {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub pull_request: PullRequest,
    pub merge_authorizations: Vec<MergeAuthorization>,
    pub stack_readiness: Vec<StackReadiness>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextChunk {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub id: String,
    pub offset: u64,
    pub total_bytes: u64,
    pub data: String,
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub changed: bool,
    #[serde(default)]
    pub sections: BTreeMap<String, String>,
}

/// One open card its author may replace (`card.replaces`) or withdraw.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenCard {
    pub id: InteractionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    pub kind: CardKind,
    pub need: String,
    /// The slot it holds: a new card in it replaces this one without `replaces`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<CardSlot>,
    pub blocking: bool,
    /// Held by a Claude Code hook (a plan or a permission): it ends with its hook.
    pub held: bool,
    pub revision: u64,
}

impl DomainState {
    pub fn open_card_rows(&self, reader: &Actor, task: Option<&TaskId>) -> Vec<OpenCard> {
        self.open_cards(reader, task).into_iter().map(|i| OpenCard {
            id: i.id.clone(),
            task_id: match &i.scope {
                Scope::Task { task_id } => Some(task_id.clone()),
                Scope::Orchestrator { .. } => None,
            },
            kind: i.card.kind,
            need: i.card.need.clone(),
            slot: CardSlot::of(&i.card, &i.payload),
            blocking: i.blocking,
            held: self.holds.contains_key(&i.id),
            revision: i.order,
        }).collect()
    }
}
