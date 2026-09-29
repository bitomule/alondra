use super::ScheduleState;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

macro_rules! ids { ($($name:ident),*) => {$ (
    #[derive(Clone,Debug,Default,PartialEq,Eq,PartialOrd,Ord,Hash,Serialize,Deserialize)]
    #[serde(transparent)] pub struct $name(pub String);
    impl From<&str> for $name { fn from(v:&str)->Self {Self(v.into())} }
    impl From<String> for $name { fn from(v:String)->Self {Self(v)} }
    impl fmt::Display for $name {fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result {self.0.fmt(f)}}
)*}; }
ids!(
    TaskId,
    AgentId,
    InteractionId,
    DocumentId,
    DocumentRevisionId,
    PrId,
    RequestId,
    AuthorizationId,
    CommandId,
    DataEpoch,
    ScheduleId,
    ScheduleRunId
);
pub type Revision = u64;
pub const MAX_REVISION: Revision = 9_007_199_254_740_991;
pub const PROTOCOL_VERSION: u32 = 2;

/// Never deserialize an Actor from a client payload. Only authenticated adapters construct it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    User { user_id: String },
    Agent { agent_id: AgentId },
    Orchestrator { generation: u64 },
    System,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    Agent { agent_id: AgentId },
    Orchestrator { generation: u64 },
    System,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recipient {
    User,
    Agent { agent_id: AgentId },
    Orchestrator,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    Task { task_id: TaskId },
    Orchestrator { request_id: Option<RequestId> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    pub body: String,
    pub closed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub connected: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    NotStarted,
    Running,
    NeedsYou,
    InReview,
    WaitingDependency,
    Closing,
    Done,
    Cancelled,
}
impl TaskState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub request_id: RequestId,
    pub agent_id: Option<AgentId>,
    pub title: String,
    pub state: TaskState,
    pub revision: Revision,
    pub started: bool,
    pub review_prs: bool,
    pub final_presentation: Option<InteractionId>,
    pub pr_id: Option<PrId>,
    pub dependencies: BTreeSet<TaskId>,
    pub stack: Option<StackDependency>,
    pub documents: BTreeMap<DocumentId, String>,
    pub archived: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackDependency {
    pub upstream_task: TaskId,
    pub presentation_id: InteractionId,
    pub pr_id: PrId,
    pub head: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackReadiness {
    pub task_id: TaskId,
    pub presentation_id: InteractionId,
    pub pr_id: PrId,
    pub head: String,
    pub ready: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub id: DocumentId,
    pub title: String,
    pub current_revision: DocumentRevisionId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRevision {
    pub id: DocumentRevisionId,
    pub document_id: DocumentId,
    pub number: Revision,
    pub content_digest: String,
    pub content_reference: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentReview {
    pub task_id: TaskId,
    pub document_id: DocumentId,
    pub decisions: Vec<DocumentDecision>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentDecision {
    pub revision_id: DocumentRevisionId,
    pub presentation_id: Option<InteractionId>,
    pub accepted: bool,
    pub user_id: String,
    pub at_revision: Revision,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: PrId,
    pub task_id: TaskId,
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub head: String,
    pub base: String,
    pub state: PrState,
    pub merged_head: Option<String>,
    pub allowed_methods: Vec<MergeMethod>,
    pub review: Option<PullRequestReview>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestReview {
    pub decisions: Vec<PrDecision>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrDecision {
    pub presentation_id: InteractionId,
    pub head: String,
    pub accepted: bool,
    pub user_id: String,
    pub at_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryTarget {
    Text {
        text: String,
    },
    DocumentRevision {
        document_id: DocumentId,
        revision_id: DocumentRevisionId,
    },
    PullRequest {
        pr_id: PrId,
        head: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteractionSubject {
    PullRequest {
        pr_id: PrId,
    },
    DocumentReview {
        document_id: DocumentId,
        revision_id: DocumentRevisionId,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Text,
    Boolean,
    Integer,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSpec {
    pub id: String,
    pub kind: InputKind,
    pub required: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum InputValue {
    Text(String),
    Boolean(bool),
    Integer(i64),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub inputs: Vec<InputSpec>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub id: String,
    pub name: String,
    pub content_digest: String,
    pub content_reference: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    pub option_id: String,
    pub inputs: BTreeMap<String, InputValue>,
    pub responder: String,
    pub at_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acceptance {
    pub user_id: String,
    pub at_revision: Revision,
    pub valid: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    pub user_id: String,
    pub body: String,
    pub at_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteractionPayload {
    Information {
        read_by: BTreeSet<String>,
    },
    Question {
        options: Vec<QuestionOption>,
        answer: Option<Answer>,
    },
    ActionRequest {
        options: Vec<QuestionOption>,
        answer: Option<Answer>,
    },
    Delivery {
        targets: Vec<DeliveryTarget>,
        is_final: bool,
        acceptance: Option<Acceptance>,
        feedback: Vec<Feedback>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interaction {
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    pub id: InteractionId,
    pub scope: Scope,
    pub author: Author,
    pub recipient: Recipient,
    pub body: String,
    pub blocking: bool,
    pub subject: Option<InteractionSubject>,
    pub payload: InteractionPayload,
    pub revision: Revision,
    pub order: Revision,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeAttemptState {
    Unclaimed,
    Claimed,
    Uncertain,
    Confirmed,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeAuthorization {
    pub id: AuthorizationId,
    pub task_id: TaskId,
    pub pr_id: PrId,
    pub presentation_id: InteractionId,
    pub head: String,
    pub method: MergeMethod,
    pub user_id: String,
    pub valid: bool,
    pub attempt: MergeAttemptState,
    pub claimed_by: Option<AgentId>,
    pub at_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainState {
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub orchestrator_generation: u64,
    pub requests: BTreeMap<RequestId, Request>,
    pub agents: BTreeMap<AgentId, Agent>,
    pub tasks: BTreeMap<TaskId, Task>,
    pub documents: BTreeMap<DocumentId, Document>,
    pub document_revisions: BTreeMap<DocumentRevisionId, DocumentRevision>,
    pub document_reviews: Vec<DocumentReview>,
    pub pull_requests: BTreeMap<PrId, PullRequest>,
    pub interactions: BTreeMap<InteractionId, Interaction>,
    pub merge_authorizations: BTreeMap<AuthorizationId, MergeAuthorization>,
    pub stack_readiness: BTreeMap<TaskId, StackReadiness>,
    #[serde(default)]
    pub schedule: ScheduleState,
}
impl DomainState {
    pub fn new(epoch: impl Into<DataEpoch>) -> Self {
        Self {
            data_epoch: epoch.into(),
            revision: 0,
            orchestrator_generation: 0,
            requests: BTreeMap::new(),
            agents: BTreeMap::new(),
            tasks: BTreeMap::new(),
            documents: BTreeMap::new(),
            document_revisions: BTreeMap::new(),
            document_reviews: vec![],
            pull_requests: BTreeMap::new(),
            interactions: BTreeMap::new(),
            merge_authorizations: BTreeMap::new(),
            stack_readiness: BTreeMap::new(),
            schedule: ScheduleState::default(),
        }
    }
}
