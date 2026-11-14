use super::{SystemCard, Checklist, ChecklistMark, Journals, LifecycleCache, Landed, NodeMcp, ScheduleState, TaskEnding};
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
    /// Sent with `alondra send` to keep the nodes running, not to get an answer: closing it
    /// tells the user nothing, and no card answers it.
    #[serde(default)]
    pub maintenance: bool,
    /// Sent as a message to the orchestrator, not written as a new request: the Tasks list shows
    /// it only once it has tasks.
    #[serde(default)]
    pub message: bool,
    /// The card the user replied from, captured when the message was committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<CardReference>,
    /// The user cleared it from the completed work: a closed Request without Tasks no list shows again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cleared: bool,
    /// Who asked: only the user's own requests and their schedules' tell the user the answer.
    #[serde(default)]
    pub origin: RequestOrigin,
    /// Closed without a Task of its own because this open Task already covered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folded_into: Option<TaskId>,
}
impl Request {
    pub fn new(id: RequestId, body: String) -> Self {
        Request { id, body, closed: false, title: None, answer: None, withdrawn: false, auto_merge: false, maintenance: false, message: false, reply_to: None, cleared: false, origin: RequestOrigin::User, folded_into: None }
    }
    /// Closed with an answer: for a Request without Tasks, its card carries all there is to read.
    pub fn answered(&self) -> bool {
        self.closed && !self.withdrawn && self.answer.is_some()
    }
}
/// The origin of a message, resolved from the domain rather than supplied by a client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardReference {
    pub interaction_id: InteractionId,
    pub title: String,
    pub scope: Scope,
    pub request_id: Option<RequestId>,
    pub task_ids: Vec<TaskId>,
}
/// Rows written before the origin was recorded read as the user's, so no answer they asked for is lost.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestOrigin {
    #[default]
    User,
    Orchestrator,
    Node { agent_id: AgentId },
    Schedule,
}
impl RequestOrigin {
    pub fn the_user_asked(&self) -> bool {
        matches!(self, Self::User | Self::Schedule)
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
    /// The revision that created it; 0 for Tasks stored before it was recorded.
    #[serde(default)]
    pub created_revision: Revision,
    pub started: bool,
    pub review_prs: bool,
    pub final_presentation: Option<InteractionId>,
    /// The first PR the task registered; `later_prs` holds the ones after it.
    pub pr_id: Option<PrId>,
    /// The PRs this task registered after its first, in any repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub later_prs: Vec<PrId>,
    /// PRs awaiting a GitHub read after the node opened one or ran a close command.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub opened_prs: BTreeSet<OpenedPr>,
    pub dependencies: BTreeSet<TaskId>,
    /// Tasks the node declared it waits for, until they finish or are cancelled.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub waiting_for_tasks: BTreeSet<TaskId>,
    /// Satisfied targets stay closed even if a named task is reopened.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ended_task_waits: BTreeMap<TaskId, Landed>,
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
    /// The cases about its node stopping with nothing explaining its next step that went to the
    /// orchestrator since the task last moved on (a delivery, a card of its node to the user, the
    /// user's word). Counted by turns, never by time.
    #[serde(default, skip_serializing_if = "is_zero_stalls")]
    pub stalls: u32,
    /// How the task closed because its node kept stopping with nothing explaining its next step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_closing: Option<StallClosing>,
    /// What Alondra still does with the task's open pull request because someone closed the task
    /// while it was open: deliver it to the user, or close it on GitHub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_pr: Option<OpenPrPending>,
    /// Every git worktree the task made or took, by path: its node's own and those of the
    /// subagents it launched. A task ends only once each of them has ended.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub worktrees: BTreeMap<String, TaskWorktree>,
    /// The provider account its current execution runs on; absent for Tasks admitted before it
    /// was recorded, which run on the provider's own sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<crate::accounts::AccountId>,
    #[serde(default, skip_serializing_if = "crate::accounts::Provider::is_claude")]
    pub provider: crate::accounts::Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<crate::model_selection::TaskSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_selection: Option<crate::model_selection::PendingTaskSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_refused: Option<crate::model_selection::MoveRefused>,
    /// What the user allowed for the rest of the task from its permission cards, by the card each
    /// was allowed on. It is forgotten when the task ends.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub allowed: BTreeMap<InteractionId, TaskGrant>,
    /// The project it belongs to, set when it was created; absent when Alondra could not tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The task this one carries on, in the same request: its worktree, branch and plan are this
    /// one's, and the request's outcome is this one's instead of that task's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<TaskId>,
}
/// A command the user allowed for the rest of a task, and how many prompts it answered since.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskGrant {
    pub allows: crate::allow_for_task::AllowForTask,
    #[serde(default, skip_serializing_if = "is_zero_uses")]
    pub uses: u32,
}
/// How a task whose node kept stopping closed: with what the orchestrator wrote of what got done,
/// or by Alondra itself when the orchestrator left the case unsettled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StallClosing {
    Settled { result: String },
    Unsettled,
}
/// A task never ends with its pull request open and nobody to answer for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpenPrPending {
    /// The orchestrator or Alondra closed the task: Alondra reads the PR on GitHub and delivers it
    /// to the user, with what the orchestrator wrote of the work, if anything.
    Deliver {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
    },
    /// The user cancelled the task: Alondra closes the PR on GitHub with this comment.
    Close { comment: String },
}
/// What GitHub reports of a pull request when Alondra decides what to do with it: its JSON
/// fields `state`, `mergeable` with `mergeStateStatus`, and `statusCheckRollup`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPrStatus {
    pub state: PrState,
    pub head: String,
    pub base: String,
    pub mergeable: PrMergeable,
    pub checks: PrChecks,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrMergeable {
    Clean,
    Conflicting,
    /// GitHub has not worked it out yet.
    Computing,
    /// Blocked, behind or unstable: only GitHub's answer to a merge tells.
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrChecks {
    None,
    Pending {
        #[serde(default)]
        names: Vec<String>,
    },
    Passed,
    Failed { names: Vec<String> },
    ActionRequired { names: Vec<String> },
}
fn is_zero_stalls(stalls: &u32) -> bool {
    *stalls == 0
}
fn is_zero_uses(uses: &u32) -> bool {
    *uses == 0
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
    /// Another task that has not ended works in it now, and it ends with that task.
    HandedOver,
}
impl Task {
    /// The ids of every PR of the task, the first one first.
    pub fn pr_ids(&self) -> impl Iterator<Item = &PrId> {
        self.pr_id.iter().chain(&self.later_prs)
    }
    pub fn account_key(&self) -> crate::accounts::AccountKey {
        crate::accounts::AccountKey::new(self.provider, self.account.clone().unwrap_or_else(crate::accounts::AccountId::default_account))
    }

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
/// How a node brings its PR's base into its branch when the PR falls behind or conflicts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchUpdate {
    #[default]
    Rebase,
    Merge,
}
impl BranchUpdate {
    pub const ALL: [BranchUpdate; 2] = [BranchUpdate::Rebase, BranchUpdate::Merge];
    pub fn id(self) -> &'static str {
        match self {
            BranchUpdate::Rebase => "rebase",
            BranchUpdate::Merge => "merge",
        }
    }
    pub fn parse(id: &str) -> Option<BranchUpdate> {
        Self::ALL.into_iter().find(|update| update.id() == id)
    }
    pub fn is_default(&self) -> bool {
        *self == BranchUpdate::default()
    }
    /// The same in a few words, for a reason the user also reads on the task.
    pub fn summary(self, base: &str) -> String {
        match self {
            BranchUpdate::Rebase => format!("rebase its branch on {base} and resolve them"),
            BranchUpdate::Merge => format!("merge {base} into its branch and resolve them"),
        }
    }
    /// How a node that needs what landed on the default branch brings it into its branch.
    pub fn from_default_branch(self) -> &'static str {
        match self {
            BranchUpdate::Rebase => "rebase it on the default branch and push with `git push --force-with-lease` to your task's own branch only",
            BranchUpdate::Merge => "merge the default branch into it and push",
        }
    }
    /// What a node is told to do to bring `base` into its branch, in every message that asks it to.
    pub fn instruction(self, base: &str) -> String {
        match self {
            BranchUpdate::Rebase => format!(
                "rebase your branch on {base} (`git fetch origin {base}`, `git rebase origin/{base}`), resolve the conflicts, \
                 run the tests and push with `git push --force-with-lease` to your task's own branch only, never to {base} or \
                 anyone else's branch. If the rebase gets hard (many conflicts in intermediate commits), run `git rebase --abort`, \
                 merge origin/{base} into your branch instead this once, and say so in your delivery"
            ),
            BranchUpdate::Merge => format!(
                "merge {base} into your branch (`git fetch origin {base}`, `git merge origin/{base}`), resolve the conflicts, \
                 run the tests and push"
            ),
        }
    }
}
/// Where a PR a node was seen opening lives, until Alondra reads the rest of it from GitHub.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OpenedPr {
    pub host: String,
    pub repository: String,
    pub number: u64,
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
        /// Left out of a presentation, it is the commit the PR is at when the delivery commits.
        #[serde(default)]
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
    /// The values of a card's form, each of its field's type.
    Form(serde_json::Map<String, serde_json::Value>),
}
/// The one answer field a card has: the user's own words, alone or with the option they pressed.
pub const ANSWER_TEXT: &str = "text";
/// The values the user filled in a card's form, beside the option that accepts it.
pub const ANSWER_CONTENT: &str = "content";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
pub const MAX_ATTACHMENT_BYTES: u64 = 64 * 1024 * 1024;
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
impl Answer {
    /// The values the user filled in the card's form.
    pub fn form_values(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        match self.inputs.get(ANSWER_CONTENT) {
            Some(InputValue::Form(content)) => Some(content),
            _ => None,
        }
    }
}
/// What an answered card's answer says in words: the option chosen, the values filled in and
/// the files attached. `None` when the card has no answer.
pub fn answer_said(interaction: &Interaction) -> Option<String> {
    let (options, answer) = match &interaction.payload {
        InteractionPayload::Question { options, answer: Some(answer) }
        | InteractionPayload::ActionRequest { options, answer: Some(answer) } => (options, answer),
        _ => return None,
    };
    let option = options.iter().find(|option| option.id == answer.option_id).map(|option| option.label.as_str());
    let values: Vec<String> = answer.inputs.values().map(|value| match value {
        InputValue::Text(value) => value.clone(),
        InputValue::Boolean(value) => value.to_string(),
        InputValue::Integer(value) => value.to_string(),
        InputValue::Form(content) => serde_json::Value::Object(content.clone()).to_string(),
    }).filter(|value| !value.trim().is_empty()).collect();
    let mut text = match option {
        Some(option) if values.is_empty() => format!("{option}."),
        Some(option) => format!("{option}. {}", values.join("; ")),
        None => values.join("; "),
    };
    for attachment in &answer.attachments {
        text.push_str(&format!(" Open the attached file “{}” at {}.", attachment.name, attachment.content_reference));
    }
    Some(text.trim_start().to_owned())
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
    /// The merge Alondra could not make, when this is the hand-back that says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_failure: Option<MergeFailure>,
}
/// A pull request Alondra could not merge, and GitHub's reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeFailure {
    pub number: u64,
    pub reason: String,
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
/// What the user does with a final result that proposes a plan: approve it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanChoice {
    /// Accept the result and start implementing its plan.
    Implement,
    /// Only read from decisions stored before a proposed plan became a plan card: the domain
    /// refuses it, so accepting a report never decides a plan.
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
#[serde(deny_unknown_fields)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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
    /// A command permission's «allow in this task»: what it keeps allowing until the task ends, or
    /// why it is not offered. Only the daemon sets it, and only a held permission acts on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_for_task: Option<crate::allow_for_task::AllowForTask>,
    /// The work a final result proposes, as a plan with checklist lines: the user approves it,
    /// which starts its implementation as a new Task, or only archives the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Where Alondra keeps the whole report or plan of a final result, so it outlives the node's
    /// session. Only the daemon sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_at: Option<String>,
    /// Where the secret a Provide card asks for is written: the answer then only says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_destination: Option<crate::secret_destination::SecretDestination>,
    /// The form an MCP server asks the user to fill, when its schema is one a screen can draw.
    /// Only a held elicitation carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<crate::requested_form::RequestedForm>,
    /// Which of Alondra's own cards this is, which the card keeps as `Interaction::system_card`.
    /// Only Alondra sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemCard>,
    /// The worktree Markdown files the author listed for the card, each included at the end of its
    /// content or plan under `## <path>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// The language a node declares the card is written in. Only the declaration is compared with
    /// the user's language; the text is never read. Cards stored or sent without it still decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<alondra_foundation::Lang>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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
    pub const TITLE_MAX: usize = 160;
    pub const NEED_MAX: usize = 90;
    /// The longest headline of Alondra's own cards that name their task: it fits a notification's
    /// title too, so no screen cuts what the card asks.
    pub const TITLED_NEED_MAX: usize = 80;
    pub const WHY_MAX: usize = 240;
    pub const SUMMARY_MAX: usize = 60;

    /// An internally authored subject with a short default request for its kind.
    pub fn new(kind: CardKind, title: impl Into<String>) -> Self {
        CardSpec {
            kind,
            title: Some(title.into()),
            need: fallback_need(kind).into(),
            why: String::new(),
            content: None,
            recommended: None,
            blocks: vec![],
            replaces: None,
            verify: None,
            group: None,
            cause: None,
            allow_always: None,
            allow_for_task: None,
            plan: None,
            kept_at: None,
            secret_destination: None,
            files: Vec::new(),
            form: None,
            system: None,
            language: None,
        }
    }
    pub fn needing(mut self, need: impl Into<String>) -> Self {
        self.need = need.into();
        self
    }
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
    pub fn effective_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.need)
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
        let mut card = CardSpec::new(kind, need.clone()).needing(need).why(why);
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
    /// Unix milliseconds, supplied by the trusted adapter; screens never compare it to a clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resurfaced_at_revision: Option<Revision>,
    /// What the card is, when it is one of Alondra's own: only the command that creates it says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_card: Option<SystemCard>,
    /// Its body no longer holds all its author wrote: the compaction of its ended Task cut it.
    #[serde(default)]
    pub shortened: bool,
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
        if self.author == Author::System || self.card.title.is_some() || body.is_empty() {
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
    pub const FOR_TASK: &'static str = "task";
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
        self.permission_offering(always, false)
    }

    /// A permission's options with «allow in this task» after its yes when `for_task`, and «allow
    /// always» after that when `always`.
    pub fn permission_offering(self, always: bool, for_task: bool) -> Vec<QuestionOption> {
        let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
        match self {
            Hold::Permission => {
                let mut options = vec![option(Self::ALLOW, "Allow")];
                if for_task {
                    options.push(option(Self::FOR_TASK, "Allow in this task"));
                }
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
    /// The revision the orchestrator's current batch started from: a Task created after it is
    /// part of that batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_batch_base: Option<Revision>,
    pub requests: BTreeMap<RequestId, Request>,
    pub agents: BTreeMap<AgentId, Agent>,
    pub tasks: BTreeMap<TaskId, Task>,
    pub documents: BTreeMap<DocumentId, Document>,
    pub document_revisions: BTreeMap<DocumentRevisionId, DocumentRevision>,
    pub document_reviews: Vec<DocumentReview>,
    pub pull_requests: BTreeMap<PrId, PullRequest>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub pr_adoptions: BTreeSet<OpenedPr>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pr_task_history: BTreeMap<PrId, BTreeSet<TaskId>>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_logins: Vec<super::AccountLogin>,
    /// How much of its session window an account may use before it has no margin.
    #[serde(default = "crate::usage::default_reserve", skip_serializing_if = "crate::usage::is_default_reserve")]
    pub account_reserve: crate::usage::Share,
    /// How new work is spread over the accounts.
    #[serde(default, skip_serializing_if = "crate::accounts::is_default_policy")]
    pub account_policy: crate::accounts::Policy,
    /// A node whose account reaches its limit goes on with another account.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub account_moves: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_order: Vec<crate::accounts::AccountKey>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account_config_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_catalogs: Vec<crate::model_selection::AccountModelCatalog>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_profiles: crate::model_selection::ModelProfiles,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_profiles_version: String,
    /// What each account last said of its usage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_usage: Vec<crate::usage::AccountUsage>,
    /// How this Mac merges PRs when the user presses merge or accepts a result, if the PR's
    /// repository allows it (`MergeMethod::for_pr`).
    #[serde(default, skip_serializing_if = "MergeMethod::is_default")]
    pub merge_method: MergeMethod,
    /// How nodes bring their PR's base into their branch, as the user set it in Settings.
    #[serde(default, skip_serializing_if = "BranchUpdate::is_default")]
    pub branch_update: BranchUpdate,
}
impl DomainState {
    pub fn new(epoch: impl Into<DataEpoch>) -> Self {
        Self {
            data_epoch: epoch.into(),
            revision: 0,
            orchestrator_generation: 0,
            orchestrator_session: None,
            orchestrator_batch_base: None,
            requests: BTreeMap::new(),
            agents: BTreeMap::new(),
            tasks: BTreeMap::new(),
            documents: BTreeMap::new(),
            document_revisions: BTreeMap::new(),
            document_reviews: vec![],
            pull_requests: BTreeMap::new(),
            pr_adoptions: BTreeSet::new(),
            pr_task_history: BTreeMap::new(),
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
            account_logins: Vec::new(),
            account_reserve: crate::usage::default_reserve(),
            account_policy: crate::accounts::Policy::Margin,
            account_moves: false,
            account_order: vec![],
            account_config_version: String::new(),
            model_catalogs: vec![],
            model_profiles: BTreeMap::new(),
            model_profiles_version: String::new(),
            account_usage: Vec::new(),
            merge_method: MergeMethod::default(),
            branch_update: BranchUpdate::default(),
        }
    }
    /// The orchestrator has neither defined it into Tasks nor closed it.
    pub fn is_defining(&self, request: &Request) -> bool {
        !request.closed && !self.tasks.values().any(|task| task.request_id == request.id)
    }
    /// For a Request born of approving the plan a final result proposed: where the plan came
    /// from and the Tasks the orchestrator created to build it, which the card telling the user
    /// what was created lists.
    pub fn tasks_of_approved_plan(&self, request_id: &RequestId) -> Option<(&ApprovedPlan, Vec<&Task>)> {
        let approved = self.request_plans.get(request_id)?;
        Some((approved, self.tasks.values().filter(|task| &task.request_id == request_id).collect()))
    }
}

/// What a node is told when it tries to send the user a Know card.
pub const NODES_DO_NOT_NOTIFY: &str = "Nodes don't send notices. To ask/decide use ask; to need an action use request_action; for input use request_input; to have the user try something use request_test; report results in present_result.";
/// What the orchestrator is told when it answers a maintenance message with a card.
pub const ALONDRA_TELLS_THE_OUTCOME: &str = "Alondra tells the user how their request ended: answer a Request without Tasks with close_request (headline and answer), and close one whose Tasks are over with close_request (answer: what came of it). No card from you for it or its Tasks.";
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
