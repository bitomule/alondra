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
