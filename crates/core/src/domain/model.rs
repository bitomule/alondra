use super::{Checklist, ChecklistMark, Journals, LifecycleCache, NodeMcp, ScheduleState, TaskEnding};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub use alondra_foundation::{
    TaskId, AgentId, InteractionId, DocumentId, DocumentRevisionId, PrId, RequestId,
    AuthorizationId, CommandId, DataEpoch, ScheduleId, ScheduleRunId, Revision, MAX_REVISION,
};
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
    /// A short name the user reads in place of the whole text.
    #[serde(default)]
    pub title: Option<String>,
    /// What the orchestrator told the user when it closed a Request that needed no Task.
    #[serde(default)]
    pub answer: Option<String>,
    /// Closed because the user withdrew it.
    #[serde(default)]
    pub withdrawn: bool,
    /// The user chose that Alondra merges the PR of the final delivery once its checks pass,
    /// without waiting for their acceptance.
    #[serde(default)]
    pub auto_merge: bool,
    /// Sent with `alondra send` to keep the fleet running, not to get an answer: closing it
    /// tells the user nothing, and no card answers it.
    #[serde(default)]
    pub maintenance: bool,
    /// Sent as a message to the orchestrator, not written as a new request: the Tasks list shows
    /// it only once it has tasks.
    #[serde(default)]
    pub message: bool,
    /// The user cleared it from the completed work: a closed Request without Tasks no list shows again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cleared: bool,
}
impl Request {
    pub fn new(id: RequestId, body: String) -> Self {
        Request { id, body, closed: false, title: None, answer: None, withdrawn: false, auto_merge: false, maintenance: false, message: false, cleared: false }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub phase: super::NodePhase,
    /// When its phase last moved, by the clock of the event that moved it.
    #[serde(default)]
    pub since: crate::model::Millis,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    NotStarted,
    Running,
    NeedsYou,
    /// Delivered, but an open task blocks it: nothing for the user to do yet.
    AwaitingBlocker,
    /// Delivered and the user can act on it now.
    #[serde(alias = "in_review")]
    ForYourReview,
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
    /// Tasks this one starts after, as soon as they make their final delivery.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub delivered_dependencies: BTreeSet<TaskId>,
    /// Tasks that cannot reach the user's review, or have a card, while this one is open.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub blocks: BTreeSet<TaskId>,
    /// What this task's final delivery said about the tasks it blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// How many times its verdict sent the blocked tasks back to work.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review_rounds: u8,
    pub stack: Option<StackDependency>,
    pub documents: BTreeMap<DocumentId, String>,
    /// The user cleared it from the completed work: an ended Task no list shows again.
    pub archived: bool,
    /// The model its node was last launched with; absent for Tasks launched before it was recorded
    /// and for launches that left Claude's default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TaskModel>,
    /// How the task's end reaches its node: the cleanup request, the node's confirmation and what
    /// became of the node.
    #[serde(default, skip_serializing_if = "WrapUp::is_default")]
    pub wrap_up: WrapUp,
    /// Where its node stands in the queue of nodes waiting to work.
    #[serde(default, skip_serializing_if = "TaskPriority::is_normal")]
    pub priority: TaskPriority,
    /// How the task is ending while its state is Closing: read from its lifecycle, which holds
    /// Done or Cancelled back until every receipt of the ending is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ending: Option<TaskEnding>,
    /// Every git worktree the task made or took, by path: its node's own and those of the
    /// subagents it launched. A task ends only once each of them has ended.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub worktrees: BTreeMap<String, TaskWorktree>,
    /// The provider account its current execution runs on; absent for Tasks admitted before it
    /// was recorded, which run on the provider's own sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<crate::accounts::AccountId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskWorktree {
    pub origin: WorktreeOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The node said in its cleanup that what is left in it can go.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub discard: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<WorktreeEnd>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeOrigin {
    /// The worktree Alondra made for the task.
    Task,
    /// A worktree the task's node took over from an earlier node.
    Inherited,
    /// Made by a subagent its node launched with an isolated worktree.
    Subagent,
    /// Made by its node with EnterWorktree.
    Entered,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeEnd {
    /// It no longer exists: Alondra or Claude Code removed it.
    Removed,
    /// The user chose to keep the task's node, and with it what the node has on disk.
    KeptByUser,
    /// It is a folder of the user's that the node was launched in, which Alondra never deletes.
    UsersOwn,
}
impl Task {
    /// The worktrees that have not ended yet.
    pub fn open_worktrees(&self) -> impl Iterator<Item = (&String, &TaskWorktree)> {
        self.worktrees.iter().filter(|(_, w)| w.end.is_none())
    }
}
/// Higher priorities start before lower ones when nodes wait for a free slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    High,
    #[default]
    Normal,
    Low,
}
impl TaskPriority {
    pub fn is_normal(&self) -> bool {
        *self == TaskPriority::Normal
    }
}
fn is_zero(n: &u8) -> bool {
    *n == 0
}
/// The most rounds a blocking task sends the work back before its last word closes it.
pub const MAX_REVIEW_ROUNDS: u8 = 3;
/// What a task that blocks others concluded about the tasks it blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verdict {
    /// `open_issues` is set when the rounds ran out with failures still open: `summary` then
    /// holds them.
    Approved {
        summary: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        open_issues: bool,
        /// Serious failures found in this round: broken behaviour, lost data, or a security or
        /// session hole. Verdicts recorded before the count read as 0.
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        serious_findings: u32,
    },
    /// `serious_findings` is required from the reviewer; only verdicts recorded before the count
    /// lack it.
    ChangesRequested {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serious_findings: Option<u32>,
    },
}
impl Verdict {
    pub fn serious_findings(&self) -> u32 {
        match self {
            Self::Approved { serious_findings, .. } => *serious_findings,
            Self::ChangesRequested { serious_findings, .. } => serious_findings.unwrap_or(0),
        }
    }
}
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostMergeWork {
    pub reason: String,
    /// The revision of the node's last declaration: saying it again, even with the same reason,
    /// is the node answering that it still works on it.
    pub declared_at: Revision,
    /// A turn of the node began after this declaration: once the task is closing, that turn
    /// ending with nothing of the node's own running ends the work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub turn_since: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrapUp {
    /// The revision at which Alondra asked the node to clean up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_at: Option<Revision>,
    /// The revision of the node's last `cleanup_done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleaned_at: Option<Revision>,
    /// What the node says it still does after the merge; its session stays open meanwhile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_merge_work: Option<PostMergeWork>,
    /// The task was cancelled while it had a node, so Alondra closes that node.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub close_cancelled: bool,
    /// What became of the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeEnd>,
}
impl WrapUp {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeEnd {
    /// Alondra closed it with `claude rm` and its worktree is gone.
    Closed,
    /// It was already gone from `claude agents` when Alondra came to close it.
    Gone,
    /// Alondra could not close it and the user chose to leave it open.
    KeptByUser,
    /// It is not a session Alondra launched (the user's own, or the orchestrator): never closed.
    UsersOwn,
}
impl Task {
    /// Still under way or waiting on something: what every client counts as a Task in progress.
    pub fn active(&self) -> bool {
        !self.state.terminal() && !self.archived
    }

    /// Open, marked as started, and with no node to work on it: a state nothing can move, so no
    /// command may put a task in it.
    pub fn started_without_node(&self) -> bool {
        !self.state.terminal() && self.started && self.agent_id.is_none()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelChooser {
    Orchestrator,
    User,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskModel {
    pub model: String,
    pub chosen_by: ModelChooser,
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    #[default]
    Squash,
    Rebase,
}
impl MergeMethod {
    /// The order Alondra falls back in when a repository does not allow the preferred method.
    pub const ALL: [MergeMethod; 3] = [MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase];
    /// The method a PR merges by: the preferred one when its repository allows it, else the first
    /// it allows in [`MergeMethod::ALL`]'s order; none while GitHub has not said what it allows.
    pub fn for_pr(preferred: MergeMethod, allowed: &[MergeMethod]) -> Option<MergeMethod> {
        std::iter::once(preferred).chain(Self::ALL).find(|method| allowed.contains(method))
    }
    pub fn id(self) -> &'static str {
        match self {
            MergeMethod::Squash => "squash",
            MergeMethod::Merge => "merge",
            MergeMethod::Rebase => "rebase",
        }
    }
    pub fn parse(id: &str) -> Option<MergeMethod> {
        Self::ALL.into_iter().find(|method| method.id() == id)
    }
    pub fn is_default(&self) -> bool {
        *self == MergeMethod::default()
    }
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
impl PullRequest {
    /// The PR in the ids of cards about it: its own id while it is a plain one, its task's
    /// once the id carries its host and repository, which a card id cannot hold.
    pub fn slug(&self) -> &str {
        if self.id.0.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            &self.id.0
        } else {
            &self.task_id.0
        }
    }
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum InputValue {
    Text(String),
    Boolean(bool),
    Integer(i64),
}
/// The one answer field a card has: the user's own words, alone or with the option they pressed.
pub const ANSWER_TEXT: &str = "text";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    /// How the orchestrator settled a node's suggestion; answers written before it existed have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<SuggestionDecision>,
}
/// The only ways the orchestrator settles a node's suggestion. There is no answer outside them:
/// a new task is created by the same command that records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SuggestionDecision {
    NewTask {
        task_id: TaskId,
        request_id: RequestId,
        title: String,
        launch: super::LaunchPlan,
        #[serde(default)]
        dependencies: BTreeSet<TaskId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    CurrentTask { note: String },
    Discard { reason: String },
}
impl SuggestionDecision {
    /// The Propose card's option this decision stands for.
    pub fn option_id(&self) -> &'static str {
        match self {
            Self::NewTask { .. } => "new_task",
            Self::CurrentTask { .. } => "yours",
            Self::Discard { .. } => "discarded",
        }
    }
    pub fn note(&self) -> Option<&str> {
        match self {
            Self::NewTask { note, .. } => note.as_deref(),
            Self::CurrentTask { note } => Some(note),
            Self::Discard { reason } => Some(reason),
        }
    }
}
/// The tasks that wait for one where the user answered a decision, held back until the
/// orchestrator reviews the launch plan each was given before that decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanReview {
    /// Where the user decided.
    pub task_id: TaskId,
    /// The decision cards answered: one, or the decisions of a group answered together.
    pub decisions: Vec<InteractionId>,
    /// The revision that committed the answer.
    pub at_revision: Revision,
    /// Every dependent held, with how its plan review was settled once it is.
    pub dependents: BTreeMap<TaskId, Option<PlanReviewSettlement>>,
}
impl PlanReview {
    pub fn holds(&self, task: &TaskId) -> bool {
        matches!(self.dependents.get(task), Some(None))
    }
    pub fn open(&self) -> impl Iterator<Item = &TaskId> {
        self.dependents.iter().filter(|(_, settled)| settled.is_none()).map(|(task, _)| task)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanReviewSettlement {
    pub outcome: PlanReviewOutcome,
    pub at_revision: Revision,
}
/// The only ways a held dependent is released: the orchestrator reviewed its plan, or the user kept
/// the plans as they are. Time, the end of what it waits for and a reopening never release it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanReviewOutcome {
    Unchanged { reason: String },
    PlanChanged { launch: super::LaunchPlan, reason: String },
    Cancelled { reason: String },
    KeptByUser,
}
impl PlanReviewOutcome {
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Unchanged { reason } | Self::PlanChanged { reason, .. } | Self::Cancelled { reason } => Some(reason),
            Self::KeptByUser => None,
        }
    }
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
    #[serde(default)]
    pub instruction: Instruction,
}
impl Feedback {
    pub const ALONDRA: &'static str = "alondra";
}
/// What Alondra told the Agent to do with its delivery. The Task shows it until the Agent
/// presents again, which every instruction ends by asking for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Instruction {
    #[default]
    Changes,
    Question,
    Reply,
    Conflicts,
}
impl Instruction {
    /// The words the Agent receives: the user's text, then what to do once it is done.
    pub fn message(self, body: &str) -> String {
        match self {
            Self::Changes => format!("The user requested changes on your result: {body}\n\nWhen they are done, present the result again with present_result, for a PR with its new head."),
            Self::Question => format!("The user asks about your result: {body}\n\nPut the answer in the report, then present the result again with present_result, for a PR with its new head."),
            Self::Reply => format!("The user replied to your result: {body}\n\nWhen you have acted on it, present the result again with present_result, for a PR with its new head."),
            Self::Conflicts => body.to_owned(),
        }
    }
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
/// The plan of a final result the user approved for implementation: the Task that delivered it
/// and the delivery that carries it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedPlan {
    pub task_id: TaskId,
    pub presentation_id: InteractionId,
}
/// What the user does with a final result that proposes a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanChoice {
    /// Accept the result and start implementing its plan.
    Implement,
    /// Accept the result and leave its plan unbuilt.
    Archive,
}
/// What the card asks of the user: the kind comes from what the user contributes, not from the
/// object behind it (a PR or a plan is what the card carries).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Decide,
    Review,
    Authorize,
    Know,
    Provide,
    Test,
    Do,
    Problem,
    Propose,
}
impl CardKind {
    /// A notice or a suggestion never stops a node.
    pub fn never_blocks(self) -> bool {
        matches!(self, Self::Know | Self::Propose)
    }
    /// The asks a node opens one at a time.
    pub fn one_at_a_time(self) -> bool {
        matches!(self, Self::Decide | Self::Do | Self::Provide)
    }
    pub fn needs_why(self) -> bool {
        matches!(self, Self::Decide | Self::Do | Self::Provide | Self::Propose | Self::Problem)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Plan,
    Document,
    Diff,
    Command,
    Report,
    Steps,
    Options,
    Diagnosis,
    Attachments,
}
/// The long part of a card, shown only in its detail. Without `summary` Alondra computes one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardContent {
    pub kind: ContentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub body: String,
}
/// What a card holds back until it is answered.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CardTarget {
    PullRequest { pr_id: PrId },
    Document { document_id: DocumentId },
    Delivery { interaction_id: InteractionId },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardSpec {
    pub kind: CardKind,
    /// What the user must do, in the imperative: at most 90 characters.
    pub need: String,
    /// Why, and whom it holds back: at most 240 characters.
    #[serde(default)]
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<CardContent>,
    /// The option id the author recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<CardTarget>,
    /// The open card of the same author this one replaces; it is withdrawn in the same step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<InteractionId>,
    /// How the node will check the user did it (Do only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    /// The card is one question of several asked together: each is its own card, and the node
    /// gets every answer at once when the last one is answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<QuestionGroup>,
    /// What failed, for a Problem: a newer Problem with the same cause in the same place replaces
    /// the open one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// A command permission's «allow always»: the rules it adds, or why it is not offered. Only
    /// the daemon sets it, and only a held permission shows it or acts on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_always: Option<crate::allow_always::AllowAlways>,
    /// The work a final result proposes, as a plan with checklist lines: the user approves it,
    /// which starts its implementation as a new Task, or only archives the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Where Alondra keeps the whole report or plan of a final result, so it outlives the node's
    /// session. Only the daemon sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_at: Option<String>,
}
/// A place that holds one open card at a time: a new card in it replaces the open one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CardSlot {
    Plan,
    FinalResult,
    Problem { cause: String },
}
impl CardSlot {
    pub const CAUSE_MAX: usize = 120;

    pub fn of(card: &CardSpec, payload: &InteractionPayload) -> Option<Self> {
        if matches!(payload, InteractionPayload::Delivery { is_final: true, .. }) {
            return Some(Self::FinalResult);
        }
        if card.content.as_ref().is_some_and(|c| c.kind == ContentKind::Plan) {
            return Some(Self::Plan);
        }
        match (&card.kind, &card.cause) {
            (CardKind::Problem, Some(cause)) => Some(Self::Problem { cause: cause.clone() }),
            _ => None,
        }
    }
}
/// Where a card stands among the questions asked together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionGroup {
    /// The same on every question of the group.
    pub id: String,
    /// From 1.
    pub index: u8,
    pub total: u8,
}
/// One question of several asked together; it becomes a Decide card of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupQuestion {
    /// The question, clear on its own: at most 90 characters.
    pub need: String,
    /// What the user needs to know to answer it: at most 240 characters.
    #[serde(default)]
    pub why: String,
    pub options: Vec<QuestionOption>,
    /// The id of the option the author recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
}
impl QuestionGroup {
    pub const MAX: usize = 4;
    /// The id of the card for question `index` (from 1) of the group `id`.
    pub fn card_id(id: &InteractionId, index: usize) -> InteractionId {
        InteractionId(format!("{id}-q{index}"))
    }
}
/// A label an author marked with "(Recommended)", as Claude Code's own questions do: the label
/// without the suffix, when it had one. A recommendation belongs in `recommended`, not the label.
pub fn without_recommended_suffix(label: &str) -> Option<String> {
    const SUFFIX: &str = "(recommended)";
    let trimmed = label.trim_end();
    let cut = trimmed.len().checked_sub(SUFFIX.len())?;
    if !trimmed.is_char_boundary(cut) || !trimmed[cut..].eq_ignore_ascii_case(SUFFIX) {
        return None;
    }
    let bare = trimmed[..cut].trim_end();
    (!bare.is_empty()).then(|| bare.to_owned())
}
impl CardSpec {
    pub const NEED_MAX: usize = 90;
    /// The longest headline of Alondra's own cards that name their task: it fits a notification's
    /// title too, so no screen cuts what the card asks.
    pub const TITLED_NEED_MAX: usize = 80;
    pub const WHY_MAX: usize = 240;
    pub const SUMMARY_MAX: usize = 60;

    pub fn new(kind: CardKind, need: impl Into<String>) -> Self {
        CardSpec {
            kind,
            need: need.into(),
            why: String::new(),
            content: None,
            recommended: None,
            blocks: vec![],
            replaces: None,
            verify: None,
            group: None,
            cause: None,
            allow_always: None,
            plan: None,
            kept_at: None,
        }
    }
    pub fn plan(mut self, plan: impl Into<String>) -> Self {
        self.plan = Some(plan.into());
        self
    }
    pub fn cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }
    pub fn why(mut self, why: impl Into<String>) -> Self {
        self.why = why.into();
        self
    }
    pub fn content(mut self, kind: ContentKind, body: impl Into<String>) -> Self {
        self.content = Some(CardContent { kind, summary: None, body: body.into() });
        self
    }
    /// A card read from a body written before cards existed: its first line is what is needed,
    /// the next paragraph why, and a longer body goes to the content.
    pub fn derived(kind: CardKind, body: &str) -> Self {
        let mut lines = body.lines().map(str::trim).filter(|line| !line.is_empty());
        let first = lines.next().unwrap_or_default().trim_start_matches('#').trim();
        let need = if first.is_empty() { fallback_need(kind).to_owned() } else { clip(first, Self::NEED_MAX) };
        let rest: Vec<&str> = lines.collect();
        let why = match rest.first() {
            Some(paragraph) => clip(paragraph, Self::WHY_MAX),
            None if kind.needs_why() => need.clone(),
            None => String::new(),
        };
        let mut card = CardSpec::new(kind, need).why(why);
        if rest.len() > 1 || first.chars().count() > Self::NEED_MAX || rest.first().is_some_and(|p| p.chars().count() > Self::WHY_MAX) {
            card = card.content(ContentKind::Report, body.trim());
        }
        if kind == CardKind::Do {
            card.verify = Some("The node checks it once you answer.".into());
        }
        card
    }
    /// A question with two or more choices is a decision; one the user answers in words or with
    /// a file is something only they can provide.
    pub fn question(body: &str, options: &[QuestionOption]) -> Self {
        Self::derived(if options.len() >= 2 { CardKind::Decide } else { CardKind::Provide }, body)
    }
    /// A delivery: a final one is reviewed, a progress one only read.
    pub fn delivery(body: &str, targets: &[DeliveryTarget], is_final: bool) -> Self {
        let mut card = Self::derived(if is_final { CardKind::Review } else { CardKind::Know }, body);
        let kind = if targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { .. })) {
            ContentKind::Diff
        } else if targets.iter().any(|t| matches!(t, DeliveryTarget::DocumentRevision { .. })) {
            ContentKind::Document
        } else {
            ContentKind::Report
        };
        card.content = Some(CardContent { kind, summary: None, body: body.trim().to_owned() });
        card
    }
}
fn fallback_need(kind: CardKind) -> &'static str {
    match kind {
        CardKind::Decide => "Choose an option",
        CardKind::Review => "Review the result",
        CardKind::Authorize => "Allow this action",
        CardKind::Know => "Read this update",
        CardKind::Provide => "Provide what the node asks for",
        CardKind::Test => "Test the result",
        CardKind::Do => "Do what the node asks",
        CardKind::Problem => "Fix the problem",
        CardKind::Propose => "Consider this suggestion",
    }
}
/// `sentence` naming `title`, in at most `max` characters: when it is too long the title is
/// shortened, never the sentence, which says what the card asks.
pub fn fit_title(title: &str, max: usize, sentence: impl Fn(&str) -> String) -> String {
    let whole = sentence(title.trim());
    if whole.chars().count() <= max {
        return whole;
    }
    let room = max.saturating_sub(sentence("").chars().count()).max(1);
    sentence(&clip(title, room))
}
/// At most `max` characters, cut at a character boundary and marked with an ellipsis.
pub fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClarificationReason {
    Unclear,
    Contradicts,
    MissingContext,
}
/// The user asked the author to say it again: its next card replaces this one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clarification {
    pub reason: ClarificationReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub user_id: String,
    pub at_revision: Revision,
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
    /// A card its author withdrew or replaced: it no longer waits for anything.
    #[serde(default)]
    pub withdrawn: bool,
    pub card: CardSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarification: Option<Clarification>,
}
impl Interaction {
    /// The long part the reader sees: the card's content, or else the body its author wrote, so a
    /// notice sent with only `need` and `body` never loses its text. Alondra's own cards keep data
    /// in their body and only show their content.
    pub fn shown_content(&self) -> Option<std::borrow::Cow<'_, CardContent>> {
        use std::borrow::Cow;
        if let Some(content) = &self.card.content {
            return Some(Cow::Borrowed(content));
        }
        let body = self.body.trim();
        let (need, why) = (self.card.need.trim(), self.card.why.trim());
        let only_the_card = body
            .lines()
            .map(|line| line.trim().trim_start_matches('#').trim())
            .filter(|line| !line.is_empty())
            .all(|line| line == need || line == why);
        if self.author == Author::System || only_the_card {
            return None;
        }
        Some(Cow::Owned(CardContent { kind: ContentKind::Report, summary: None, body: body.to_owned() }))
    }
}
/// What a held question stands for: a tool permission prompt, a plan to approve, Claude's
/// AskUserQuestion, or an MCP server asking for input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    Permission,
    Plan,
    Question,
    Elicitation,
}
impl Hold {
    pub const ALLOW: &'static str = "allow";
    pub const DENY: &'static str = "deny";
    pub const ALWAYS: &'static str = "always";
    pub const APPROVE: &'static str = "approve";
    pub const REQUEST_CHANGES: &'static str = "request_changes";
    pub const ACCEPT: &'static str = "accept";
    pub const DECLINE: &'static str = "decline";
    pub const NOTE: &'static str = ANSWER_TEXT;

    /// The fixed yes/no options of a permission, plan or elicitation; a question brings its own.
    /// Each is a button: what the user writes on the card travels with the one they press.
    pub fn options(self) -> Vec<QuestionOption> {
        self.options_offering(false)
    }

    /// `always` adds a permission's «allow always» between its yes and its no.
    pub fn options_offering(self, always: bool) -> Vec<QuestionOption> {
        let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
        match self {
            Hold::Permission => {
                let mut options = vec![option(Self::ALLOW, "Allow")];
                if always {
                    options.push(option(Self::ALWAYS, "Allow always"));
                }
                options.push(option(Self::DENY, "Deny"));
                options
            }
            Hold::Plan => vec![option(Self::APPROVE, "Approve"), option(Self::REQUEST_CHANGES, "Request changes")],
            Hold::Elicitation => vec![option(Self::ACCEPT, "Accept"), option(Self::DECLINE, "Decline")],
            Hold::Question => vec![],
        }
    }

    pub fn approves(self, option_id: &str) -> bool {
        option_id == match self {
            Hold::Permission => Self::ALLOW,
            Hold::Plan => Self::APPROVE,
            Hold::Elicitation => Self::ACCEPT,
            Hold::Question => return false,
        }
    }
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
    /// The Claude session the orchestrator runs in now; Alondra rebinds it on every new session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_session: Option<AgentId>,
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
    /// Questions a Claude Code hook is waiting on: the hook, not the outbox, hands the answer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub holds: BTreeMap<InteractionId, Hold>,
    /// Each Task's scope: the points of the last plan the user approved for it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checklists: BTreeMap<TaskId, Checklist>,
    /// The points each final delivery marked done, with their evidence.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checklist_marks: BTreeMap<InteractionId, Vec<ChecklistMark>>,
    /// What each node said last when a turn of it ended: what the user reads if that turn ended
    /// with nothing for them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub last_words: BTreeMap<AgentId, String>,
    /// The Requests created by approving the plan a final result proposed, with where it came from.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub request_plans: BTreeMap<RequestId, ApprovedPlan>,
    /// The user's decisions that held dependent tasks for the orchestrator to review their plans,
    /// by the answered card that completed them; settled ones are kept.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plan_reviews: BTreeMap<InteractionId, PlanReview>,
    /// The decisions that moved each Task's lifecycle, in order, for the Tasks created since the
    /// journal exists. Replaying them is the only way to its lifecycle; the Task's state, node and
    /// start are read from that replay.
    #[serde(default, skip_serializing_if = "Journals::is_empty")]
    pub lifecycle_journal: Journals,
    #[serde(skip)]
    pub(super) lifecycles: LifecycleCache,
    /// The MCP servers each node's current SDK execution reported, and the sign-ins it opened.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<AgentId, NodeMcp>,
    /// The provider accounts nodes may run on, in the user's order, as the daemon last saw them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<crate::accounts::AccountStatus>,
    /// How much of its session window an account may use before it has no margin.
    #[serde(default = "crate::usage::default_reserve", skip_serializing_if = "crate::usage::is_default_reserve")]
    pub account_reserve: crate::usage::Share,
    /// What each account last said of its usage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_usage: Vec<crate::usage::AccountUsage>,
    /// How this Mac merges PRs when the user presses merge or accepts a result, if the PR's
    /// repository allows it (`MergeMethod::for_pr`).
    #[serde(default, skip_serializing_if = "MergeMethod::is_default")]
    pub merge_method: MergeMethod,
}
impl DomainState {
    pub fn new(epoch: impl Into<DataEpoch>) -> Self {
        Self {
            data_epoch: epoch.into(),
            revision: 0,
            orchestrator_generation: 0,
            orchestrator_session: None,
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
            holds: BTreeMap::new(),
            checklists: BTreeMap::new(),
            checklist_marks: BTreeMap::new(),
            last_words: BTreeMap::new(),
            request_plans: BTreeMap::new(),
            plan_reviews: BTreeMap::new(),
            lifecycle_journal: Journals::new(),
            lifecycles: LifecycleCache::default(),
            mcp: BTreeMap::new(),
            accounts: Vec::new(),
            account_reserve: crate::usage::default_reserve(),
            account_usage: Vec::new(),
            merge_method: MergeMethod::default(),
        }
    }
    /// The orchestrator has neither defined it into Tasks nor closed it.
    pub fn is_defining(&self, request: &Request) -> bool {
        !request.closed && !self.tasks.values().any(|task| task.request_id == request.id)
    }
}

/// What a node is told when it tries to send the user a Know card.
pub const NODES_DO_NOT_NOTIFY: &str = "Nodes don't send notices. To ask/decide use ask; to need an action use request_action; for input use request_input; to have the user try something use request_test; report results in present_result.";
/// What the orchestrator is told when it answers a maintenance message with a card.
pub const MAINTENANCE_GETS_NO_CARD: &str = "this Request is a maintenance message sent with alondra send: act on it and close it with close_request, without a card for the user. It is answered only when sent with --reply.";

/// The one name that always means the orchestrator, whatever session it runs in now.
pub const ORCHESTRATOR: &str = "orchestrator";

/// Who a typed name addresses: the orchestrator, a node, or nobody.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The orchestrator, with the session it runs in, if one was ever launched.
    Orchestrator(Option<AgentId>),
    Agent(AgentId),
    Unknown,
}

impl DomainState {
    /// Resolves "orchestrator", the orchestrator's session id or its last segment to the
    /// orchestrator; an exact agent id to that agent; anything else, translations included,
    /// to nobody.
    pub fn resolve_target(&self, name: &str) -> Target {
        if name == ORCHESTRATOR {
            return Target::Orchestrator(self.orchestrator_session.clone());
        }
        let names_session = |id: &AgentId| id.0 == name || (!name.is_empty() && id.0.contains('-') && id.0.rsplit('-').next() == Some(name));
        if let Some(session) = self.orchestrator_session.as_ref().filter(|session| names_session(session)) {
            return Target::Orchestrator(Some(session.clone()));
        }
        match self.agents.get(&AgentId::from(name)) {
            Some(agent) => Target::Agent(agent.id.clone()),
            None => Target::Unknown,
        }
    }
}
