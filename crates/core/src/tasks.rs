//! Requests and tasks: what David asked for, and the pieces of work it became. A task moves only
//! through `transition`, one exhaustive `match` over (state, event): a pair nobody decided does
//! not compile, and a pair that is not allowed is refused with the reason.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{Attachment, Millis};

mod archive;
mod define;
mod derive;
mod waits;

pub use archive::Archive;
pub use define::{Candidate, CandidateTask, Dependency, dependencies};
pub use derive::{Before, CardTask, LAUNCH_WINDOW, NODE_CLOSED, Pass, dependency_cancelled, marked_tasks, task_id};
pub use waits::{Named, merge_notice_text, pr_numbers_in};

/// What David asked for, in his words. Its state is derived from its tasks, never stored.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Request {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<crate::options::Resolved>,
    pub created_at: Millis,
    /// Made up for a session no request of David's stands behind.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub synthetic: bool,
    /// David archived it once it was finished without tasks. One with tasks is archived when
    /// all of them are (`Board::request_archived_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<Millis>,
    /// The short name the orchestrator gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The orchestrator closed it without tasks: it answered it, or David withdrew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<RequestClosure>,
}

/// How a request that needed no task ended.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RequestClosure {
    /// What the orchestrator told David, in one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// David withdrew it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub withdrawn: bool,
    pub at: Millis,
}

impl Request {
    fn new(id: &str, created_at: Millis, synthetic: bool) -> Self {
        Request { id: id.to_string(), text: None, attachments: vec![], options: None, created_at, synthetic, archived_at: None, title: None, closed: None }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    /// No task yet: the orchestrator is deciding what tasks come out of it.
    Defining,
    InProgress,
    /// Every task done or cancelled.
    Done,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    WaitingDependency,
    Running,
    NeedsYou,
    InReview,
    Closing,
    Done,
    Cancelled,
}

impl TaskState {
    pub const ALL: [TaskState; 7] = [
        TaskState::WaitingDependency,
        TaskState::Running,
        TaskState::NeedsYou,
        TaskState::InReview,
        TaskState::Closing,
        TaskState::Done,
        TaskState::Cancelled,
    ];

    pub fn name(self) -> &'static str {
        match self {
            TaskState::WaitingDependency => "waiting_dependency",
            TaskState::Running => "running",
            TaskState::NeedsYou => "needs_you",
            TaskState::InReview => "in_review",
            TaskState::Closing => "closing",
            TaskState::Done => "done",
            TaskState::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TaskState::WaitingDependency => "waiting for another task",
            TaskState::Running => "running",
            TaskState::NeedsYou => "needs you",
            TaskState::InReview => "in review",
            TaskState::Closing => "closing",
            TaskState::Done => "done",
            TaskState::Cancelled => "cancelled",
        }
    }

    pub fn finished(self) -> bool {
        matches!(self, TaskState::Done | TaskState::Cancelled)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum BlockerKind {
    Question,
    Permission,
    Action,
}

impl BlockerKind {
    fn noun(self) -> &'static str {
        match self {
            BlockerKind::Question => "question",
            BlockerKind::Permission => "permission request",
            BlockerKind::Action => "action",
        }
    }
}

/// The one thing that blocks a task on David: a card of its node.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Blocker {
    pub kind: BlockerKind,
    pub question: u64,
    pub since: Millis,
    /// Raised by the node's own `ask` or `request_action`, the only cards held to the task's PR
    /// under review. Claude Code's prompts and menus and the cards Alondra builds from the
    /// terminal are facts it never refuses; so are the blockers recorded before this marker
    /// existed, which replay exactly as they were.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asked: bool,
    /// The number of the task's PR under review the node said the card is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about_pr: Option<u64>,
}

/// What an event is about.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum Item {
    Pr(u64),
    Report(u64),
    Message(u64),
    /// David typed into the node's terminal.
    Terminal,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskEvent {
    /// A task this one depends on has its PR open, or is done: this one can build on top of it.
    /// For a task waiting for a dependency's merge: that PR is merged, or no longer open.
    DependencyReady { task: String },
    /// The node's turn ended with nothing of the task left to do before a dependency's PR is
    /// merged: the task waits for that merge.
    AwaitsMerge { pr: u64 },
    /// The node asked a question, hit a permission prompt or requested an action of David's.
    BlockerRaised {
        blocker: Blocker,
        #[serde(default)]
        replaces: bool,
    },
    PrOpened { pr: u64 },
    /// A document or artifact went to David for review.
    DocumentSent { report: u64 },
    /// A PR of an auto-merge task was merged without David.
    AutoMerged { pr: u64 },
    FinalReport { report: u64 },
    /// The blocker was answered, or withdrawn.
    Answered { question: u64 },
    /// David requested changes, asked or replied on what is under review.
    ChangesRequested { on: Item },
    /// The node pushed new commits or answered while a PR of it is open.
    NodeUpdated { on: Item },
    /// David merged a PR or approved a document.
    Merged { item: Item },
    /// A PR closed without a merge, a PR card set aside, or a document read without a verdict.
    ReviewClosed { item: Item },
    Reopen,
    Cancel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl TaskEvent {
    pub fn name(&self) -> &'static str {
        match self {
            TaskEvent::DependencyReady { .. } => "dependency_ready",
            TaskEvent::AwaitsMerge { .. } => "awaits_merge",
            TaskEvent::BlockerRaised { .. } => "blocker_raised",
            TaskEvent::PrOpened { .. } => "pr_opened",
            TaskEvent::DocumentSent { .. } => "document_sent",
            TaskEvent::AutoMerged { .. } => "auto_merged",
            TaskEvent::FinalReport { .. } => "final_report",
            TaskEvent::Answered { .. } => "answered",
            TaskEvent::ChangesRequested { .. } => "changes_requested",
            TaskEvent::NodeUpdated { .. } => "node_updated",
            TaskEvent::Merged { .. } => "merged",
            TaskEvent::ReviewClosed { .. } => "review_closed",
            TaskEvent::Reopen => "reopen",
            TaskEvent::Cancel { .. } => "cancel",
        }
    }
}

/// What `transition` needs to know about the task besides its state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Guards {
    /// The task's PRs are merged without David once their checks pass.
    pub auto_merge: bool,
    /// A task this one depends on that has no PR open yet and is not done.
    pub waiting_on: Option<String>,
    /// A task this one depends on that was cancelled before it was ready: this one cannot start.
    pub cancelled_dependency: Option<String>,
    /// A PR of a task this one depends on that is not merged yet: this task's PRs merge after it.
    pub unmerged_dependency: Option<u64>,
    /// The task started, and waits for the merge of a dependency's PR: what its node does or is
    /// told about it moves it as if it were running.
    pub started: bool,
    /// The number of the dependency's PR the task waits to see merged, while it is still open.
    pub awaited_merge: Option<u64>,
    /// What already blocks the task on David.
    pub blocker: Option<Blocker>,
    /// The number of a PR of the task still waiting for David's merge.
    pub unmerged_pr: Option<u64>,
    /// A document of the task still waiting for David's review.
    pub unreviewed_document: Option<u64>,
    /// The node said it was finished while its work waited for David: nothing follows the merge.
    pub nothing_after_merge: bool,
    /// The PR being auto-merged, when its checks have not passed.
    pub checks_failed: Option<u64>,
}

impl Guards {
    fn reviews_left(&self) -> bool {
        self.unmerged_pr.is_some() || self.unreviewed_document.is_some()
    }
}

/// A transition the machine does not allow, and what stands in the way.
#[derive(Clone, Debug, PartialEq)]
pub struct Refused {
    pub reason: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

fn refused(reason: impl Into<String>) -> Refused {
    Refused { reason: reason.into() }
}

fn not_started(g: &Guards) -> Refused {
    match &g.waiting_on {
        Some(t) => refused(format!("the task has not started: it waits for {t}")),
        None => refused("the task has not started"),
    }
}

fn is_done() -> Refused {
    refused("the task is done: reopen it first")
}

fn is_cancelled() -> Refused {
    refused("the task was cancelled")
}

fn nothing_to_answer() -> Refused {
    refused("no question of the task is open")
}

/// How the reason ends when a task already has its one blocker.
pub const STILL_OPEN: &str = "is still open";

fn blocked(g: &Guards) -> Result<(), Refused> {
    match g.blocker {
        Some(b) => Err(refused(format!("{} #{} {STILL_OPEN}", b.kind.noun(), b.question))),
        None => Ok(()),
    }
}

/// While a PR of the task waits for review, that PR is the task's one pending point: the node's
/// own card has to be about it, and other work goes in a task of its own (t-47328: question #254,
/// about a cron, sat in PR #102's card and went with its merge unanswered).
fn about_the_review(b: &Blocker, g: &Guards) -> Result<(), Refused> {
    match g.unmerged_pr {
        Some(n) if b.asked && b.about_pr.is_none() => Err(refused(format!(
            "PR #{n} waits for review. If the answer changes what PR #{n} contains or how it ships, resend with about_pr: {n}. \
             Otherwise open a separate task with new_task and ask there"
        ))),
        _ => Ok(()),
    }
}

fn nothing_pending(g: &Guards) -> Result<(), Refused> {
    match (g.unmerged_pr, g.unreviewed_document) {
        (Some(n), _) => Err(refused(format!("PR #{n} is not merged"))),
        (None, Some(r)) => Err(refused(format!("document #{r} is not reviewed yet"))),
        (None, None) => Ok(()),
    }
}

fn dependencies_merged(g: &Guards) -> Result<(), Refused> {
    match g.unmerged_dependency {
        Some(n) => Err(refused(format!("merge PR #{n} first"))),
        None => Ok(()),
    }
}

fn mergeable(g: &Guards) -> Result<(), Refused> {
    dependencies_merged(g)?;
    match g.checks_failed {
        Some(n) => Err(refused(format!("the checks of PR #{n} have not passed"))),
        None => Ok(()),
    }
}

/// A merge is a fact of GitHub's: it moves the task whatever Alondra would have decided. What
/// would have kept Alondra from merging stays on the transition.
fn merge_note(event: &TaskEvent, g: &Guards) -> Option<String> {
    let kept_back = match event {
        TaskEvent::Merged { .. } => dependencies_merged(g),
        TaskEvent::AutoMerged { .. } => mergeable(g),
        _ => Ok(()),
    };
    kept_back.err().map(|r| r.reason)
}

/// A reopened task waits again while a dependency is not ready, and cannot come back while one
/// was cancelled.
fn reopened(g: &Guards) -> Result<TaskState, Refused> {
    match (&g.cancelled_dependency, &g.waiting_on) {
        (Some(d), _) => Err(refused(format!("{d} was cancelled: reopen it first"))),
        (None, Some(_)) => Ok(TaskState::WaitingDependency),
        (None, None) => Ok(TaskState::Running),
    }
}

/// Where a merge leaves a task that was in `stay`: there while something else waits for David,
/// done when the node already said it was finished, closing otherwise.
fn after_merge(stay: TaskState, g: &Guards) -> TaskState {
    if g.reviews_left() {
        stay
    } else if g.nothing_after_merge {
        TaskState::Done
    } else {
        TaskState::Closing
    }
}

pub fn transition(state: TaskState, event: &TaskEvent, g: &Guards) -> Result<TaskState, Refused> {
    use TaskEvent as E;
    use TaskState::*;
    match (state, event) {
        (WaitingDependency, E::DependencyReady { .. }) => Ok(if g.waiting_on.is_some() || g.awaited_merge.is_some() { WaitingDependency } else { Running }),
        (Running | NeedsYou | InReview | Closing | Done | Cancelled, E::DependencyReady { .. }) => Ok(state),

        (Running | WaitingDependency, E::AwaitsMerge { .. }) => Ok(WaitingDependency),
        (NeedsYou | InReview | Closing, E::AwaitsMerge { .. }) => Ok(state),
        (Done, E::AwaitsMerge { .. }) => Err(is_done()),
        (Cancelled, E::AwaitsMerge { .. }) => Err(is_cancelled()),

        (
            WaitingDependency,
            event @ (E::BlockerRaised { .. }
            | E::PrOpened { .. }
            | E::DocumentSent { .. }
            | E::AutoMerged { .. }
            | E::FinalReport { .. }
            | E::Answered { .. }
            | E::ChangesRequested { .. }
            | E::NodeUpdated { .. }
            | E::Merged { .. }
            | E::ReviewClosed { .. }),
        ) if g.started => transition(Running, event, g),
        (
            WaitingDependency,
            E::BlockerRaised { .. }
            | E::PrOpened { .. }
            | E::DocumentSent { .. }
            | E::AutoMerged { .. }
            | E::FinalReport { .. }
            | E::Answered { .. }
            | E::ChangesRequested { .. }
            | E::NodeUpdated { .. }
            | E::Merged { .. }
            | E::ReviewClosed { .. },
        ) => Err(not_started(g)),

        (Running | NeedsYou | InReview | Closing, E::BlockerRaised { blocker, replaces }) => {
            if !*replaces {
                blocked(g)?;
            }
            about_the_review(blocker, g).map(|()| NeedsYou)
        }
        (Done, E::BlockerRaised { .. }) => Err(is_done()),
        (Cancelled, E::BlockerRaised { .. }) => Err(is_cancelled()),

        (Running, E::PrOpened { .. }) => Ok(if g.auto_merge { Running } else { InReview }),
        (Closing, E::PrOpened { .. }) => Ok(if g.auto_merge { Closing } else { InReview }),
        (NeedsYou | InReview, E::PrOpened { .. }) => Ok(state),
        (Done, E::PrOpened { .. }) => Err(is_done()),
        (Cancelled, E::PrOpened { .. }) => Err(is_cancelled()),

        (Running | Closing, E::DocumentSent { .. }) => Ok(InReview),
        (NeedsYou | InReview, E::DocumentSent { .. }) => Ok(state),
        (Done, E::DocumentSent { .. }) => Err(is_done()),
        (Cancelled, E::DocumentSent { .. }) => Err(is_cancelled()),

        (Running | InReview, E::AutoMerged { .. }) => Ok(after_merge(state, g)),
        (NeedsYou | Closing | Done | Cancelled, E::AutoMerged { .. }) => Ok(state),

        (Running | Closing, E::FinalReport { .. }) => nothing_pending(g).map(|()| Done),
        (InReview, E::FinalReport { .. }) => Ok(if g.reviews_left() { InReview } else { Done }),
        (NeedsYou, E::FinalReport { .. }) => blocked(g).and_then(|()| nothing_pending(g)).map(|()| Done),
        (Done, E::FinalReport { .. }) => Ok(Done),
        (Cancelled, E::FinalReport { .. }) => Err(is_cancelled()),

        (NeedsYou, E::Answered { .. }) => Ok(if g.reviews_left() { InReview } else { Running }),
        (Running | InReview | Closing | Done | Cancelled, E::Answered { .. }) => Err(nothing_to_answer()),

        (InReview | Closing, E::ChangesRequested { .. }) => Ok(Running),
        (Running | NeedsYou, E::ChangesRequested { .. }) => Ok(state),
        (Done, E::ChangesRequested { .. }) => Err(is_done()),
        (Cancelled, E::ChangesRequested { .. }) => Err(is_cancelled()),

        (Running | InReview | Closing, E::NodeUpdated { .. }) => Ok(if g.reviews_left() { InReview } else { state }),
        (NeedsYou, E::NodeUpdated { .. }) => Ok(NeedsYou),
        (Done, E::NodeUpdated { .. }) => Err(is_done()),
        (Cancelled, E::NodeUpdated { .. }) => Err(is_cancelled()),

        (Running | InReview, E::Merged { .. }) => Ok(after_merge(state, g)),
        (NeedsYou | Closing | Done | Cancelled, E::Merged { .. }) => Ok(state),

        (InReview, E::ReviewClosed { .. }) => Ok(if g.reviews_left() {
            InReview
        } else if g.nothing_after_merge {
            Done
        } else {
            Running
        }),
        (Running | NeedsYou | Closing | Done | Cancelled, E::ReviewClosed { .. }) => Ok(state),

        (Done | Cancelled, E::Reopen) => reopened(g),
        (WaitingDependency | Running | NeedsYou | InReview | Closing, E::Reopen) => Err(refused("only a done or cancelled task can be reopened")),

        (WaitingDependency | Running | NeedsYou | InReview | Closing, E::Cancel { .. }) => Ok(Cancelled),
        (Done, E::Cancel { .. }) => Err(refused("the task is already done")),
        (Cancelled, E::Cancel { .. }) => Err(refused("the task is already cancelled")),
    }
}

/// The launch options a task carries: today's per-launch options. PRs are reviewed by David
/// unless `auto_merge`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct TaskOptions {
    #[serde(default)]
    pub auto_merge: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

/// How a task the orchestrator defined starts once nothing holds it back: in a node Alondra
/// launches for it, or on a live node it was given to.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum Start {
    NewNode {
        name: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Node { session_id: String },
}

/// Who works on a task: a node, and the subagent of it when one does.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Assignee {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Transition {
    pub from: TaskState,
    pub to: TaskState,
    pub event: TaskEvent,
    pub at: Millis,
    /// What would have kept Alondra from making this move itself, for a fact it had to take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A fact the machine refused: it happened, so it is kept, but it did not move the task.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Anomaly {
    pub state: TaskState,
    pub event: TaskEvent,
    pub reason: String,
    pub at: Millis,
}

impl Anomaly {
    pub fn kind(&self) -> String {
        format!("{}+{}", self.state.name(), self.event.name())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub request: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub options: TaskOptions,
    #[serde(default)]
    pub assignee: Option<Assignee>,
    #[serde(default)]
    pub prs: Vec<u64>,
    #[serde(default)]
    pub documents: Vec<u64>,
    #[serde(default)]
    pub blocker: Option<Blocker>,
    #[serde(default)]
    pub notices: Vec<u64>,
    pub state: TaskState,
    pub state_since: Millis,
    pub created_at: Millis,
    /// The node's final report while its work still waited for David.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_report: Option<u64>,
    /// The PR card of a dependency whose merge the task, already started, waits for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting: Option<u64>,
    /// A launch was matched to the task's node.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub launched: bool,
    /// When its node was last launched: until that node shows up, no other node takes the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_at: Option<Millis>,
    /// The name of the node launched for it, when that is not its title: the node that shows up
    /// under it takes the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_as: Option<String>,
    /// How Alondra starts it, for a task the orchestrator defined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Start>,
    /// Made by a `TaskCreated` event: everything that moves it is in the log as `TaskMoved`, and
    /// nothing is read into it from other events.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recorded: bool,
    /// David or the orchestrator named it with `TaskTitled`: its title is no longer a fallback,
    /// and `follow_node` leaves it alone even if the node it runs on is renamed or bound to a
    /// request later.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub titled: bool,
    #[serde(default)]
    pub history: Vec<Transition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anomalies: Vec<Anomaly>,
    /// David archived it once it was finished: it leaves the active list, and the board after
    /// `archived_tasks_days`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<Millis>,
}

/// A task as the daemon records its creation: at a launch, or for a session on its first card.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NewTask {
    pub id: String,
    pub request: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub options: TaskOptions,
    /// The node working on it; none yet for a launch whose node has not shown up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Made by a launch: the node that shows up under its title takes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub launched: bool,
    /// The launched node's name, when the task has a title of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_as: Option<String>,
    /// Defined by the orchestrator: how Alondra starts it. It waits while a dependency does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Start>,
}

impl Task {
    pub fn is_assigned_to(&self, session_id: &str) -> bool {
        self.assignee.as_ref().is_some_and(|a| a.session_id == session_id)
    }

    /// Alondra has yet to start it: it may start, and no node took it nor was launched for it.
    pub fn needs_start(&self) -> bool {
        self.recorded && self.start.is_some() && self.state == TaskState::Running && self.assignee.is_none() && !self.launched
    }

    /// The name the node launched for it goes by.
    pub fn launch_name(&self) -> &str {
        self.launched_as.as_deref().unwrap_or(&self.title)
    }

    /// What a move leaves the task holding: the blocker it raised, the final report it sent, the
    /// dependency's PR it waits to see merged.
    fn hold(&mut self, event: &TaskEvent, to: TaskState) {
        match event {
            TaskEvent::BlockerRaised { blocker, .. } => {
                self.blocker = Some(*blocker);
                self.final_report = None;
            }
            TaskEvent::FinalReport { report } => self.final_report = Some(*report),
            TaskEvent::ChangesRequested { .. } | TaskEvent::Reopen | TaskEvent::Answered { .. } => self.final_report = None,
            TaskEvent::AwaitsMerge { pr } if to == TaskState::WaitingDependency => self.awaiting = Some(*pr),
            _ => {}
        }
        if to != TaskState::NeedsYou {
            self.blocker = None;
        }
        if to != TaskState::WaitingDependency {
            self.awaiting = None;
        }
    }

    /// What the task holds after its history, worked out again once part of it was taken out.
    pub(crate) fn rehold(&mut self) {
        (self.blocker, self.final_report, self.awaiting) = (None, None, None);
        for i in 0..self.history.len() {
            let (event, to) = (self.history[i].event.clone(), self.history[i].to);
            self.hold(&event, to);
        }
    }

    /// When anything of it last changed: a move, a refused fact, its creation.
    pub fn last_change(&self) -> Millis {
        let history = self.history.last().map(|t| t.at);
        let anomalies = self.anomalies.last().map(|a| a.at);
        self.state_since.max(self.created_at).max(history.unwrap_or(0)).max(anomalies.unwrap_or(0))
    }
}

/// A launch with no session to go with yet: its node shows up under this name.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Launch {
    #[serde(default)]
    pub request_id: Option<String>,
    pub auto_merge: bool,
    pub at: Millis,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Board {
    #[serde(default)]
    pub requests: BTreeMap<String, Request>,
    #[serde(default)]
    pub tasks: BTreeMap<String, Task>,
    /// Facts the machine refused, counted by `state+event`.
    #[serde(default)]
    pub anomalies: BTreeMap<String, u64>,
    /// Launches whose node has not shown up yet, by node name.
    #[serde(default)]
    pub launches: BTreeMap<String, Launch>,
    /// Since when the daemon records every task change in the log. Sessions first seen before it
    /// keep having their task read from their other events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_since: Option<Millis>,
}

impl Board {
    /// Its tasks give it its state; a request closed with none is done.
    pub fn request_state(&self, id: &str) -> RequestState {
        let mut tasks = self.tasks.values().filter(|t| t.request == id).peekable();
        if tasks.peek().is_none() {
            if self.requests.get(id).is_some_and(|r| r.closed.is_some()) { RequestState::Done } else { RequestState::Defining }
        } else if tasks.all(|t| t.state.finished()) {
            RequestState::Done
        } else {
            RequestState::InProgress
        }
    }

    /// Moves the task by a fact that already happened. One the machine refuses cannot be undone,
    /// so it stays on the task as an anomaly instead of moving it.
    pub fn apply_fact(&mut self, id: &str, event: TaskEvent, g: &Guards, at: Millis) -> Result<TaskState, Refused> {
        let Some(task) = self.tasks.get_mut(id) else { return Err(refused(format!("there is no task {id}"))) };
        let from = task.state;
        match transition(from, &event, g) {
            Ok(to) => {
                task.hold(&event, to);
                if !to.finished() {
                    task.archived_at = None;
                }
                if to != from {
                    task.state = to;
                    task.state_since = at;
                }
                let note = merge_note(&event, g);
                task.history.push(Transition { from, to, event, at, note });
                Ok(to)
            }
            Err(r) => {
                let anomaly = Anomaly { state: from, event, reason: r.reason.clone(), at };
                *self.anomalies.entry(anomaly.kind()).or_default() += 1;
                task.anomalies.push(anomaly);
                Err(r)
            }
        }
    }

    pub(crate) fn create(&mut self, t: &NewTask, at: Millis) {
        self.requests.entry(t.request.clone()).or_insert_with(|| Request::new(&t.request, at, true));
        let task = Task {
            id: t.id.clone(),
            request: t.request.clone(),
            title: t.title.clone(),
            brief: t.brief.clone(),
            depends_on: t.depends_on.clone(),
            options: t.options.clone(),
            assignee: t.session_id.as_ref().map(|s| Assignee { session_id: s.clone(), agent_id: None }),
            prs: vec![],
            documents: vec![],
            blocker: None,
            notices: vec![],
            state: if t.depends_on.is_empty() { TaskState::Running } else { TaskState::WaitingDependency },
            state_since: at,
            created_at: at,
            final_report: None,
            awaiting: None,
            launched: t.launched,
            launched_at: t.launched.then_some(at),
            launched_as: t.launched_as.clone(),
            start: t.start.clone(),
            recorded: true,
            titled: false,
            history: vec![],
            anomalies: vec![],
            archived_at: None,
        };
        self.tasks.entry(t.id.clone()).or_insert(task);
    }

    /// Forgets what an older log left behind: a task read from a node's events once its node is
    /// gone, nothing of it waits for the user and nothing happened to it since `before`, and the
    /// request made up for it. A recorded task stays until it finishes and is archived, a finished
    /// one until it is archived (`prune_archived`), and a request the user sent until it is
    /// archived. Launches that never found their node go too.
    pub(crate) fn prune(&mut self, before: Millis, keep: impl Fn(&Task) -> bool) {
        self.tasks.retain(|_, t| keep(t) || t.recorded || t.state.finished() || t.last_change() >= before);
        let tasks = &self.tasks;
        self.requests.retain(|id, r| !r.synthetic || r.created_at >= before || tasks.values().any(|t| &t.request == id));
        self.launches.retain(|_, l| l.at >= before);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TaskState::*;

    const Q: Blocker = Blocker { kind: BlockerKind::Question, question: 7, since: 0, asked: false, about_pr: None };

    fn every_event() -> Vec<TaskEvent> {
        let all = vec![
            TaskEvent::DependencyReady { task: "t-a".into() },
            TaskEvent::AwaitsMerge { pr: 1 },
            TaskEvent::BlockerRaised { blocker: Q, replaces: false },
            TaskEvent::PrOpened { pr: 1 },
            TaskEvent::DocumentSent { report: 2 },
            TaskEvent::AutoMerged { pr: 1 },
            TaskEvent::FinalReport { report: 3 },
            TaskEvent::Answered { question: 7 },
            TaskEvent::ChangesRequested { on: Item::Pr(1) },
            TaskEvent::NodeUpdated { on: Item::Pr(1) },
            TaskEvent::Merged { item: Item::Pr(1) },
            TaskEvent::ReviewClosed { item: Item::Pr(1) },
            TaskEvent::Reopen,
            TaskEvent::Cancel { reason: None },
        ];
        for e in &all {
            match e {
                TaskEvent::DependencyReady { .. }
                | TaskEvent::AwaitsMerge { .. }
                | TaskEvent::BlockerRaised { .. }
                | TaskEvent::PrOpened { .. }
                | TaskEvent::DocumentSent { .. }
                | TaskEvent::AutoMerged { .. }
                | TaskEvent::FinalReport { .. }
                | TaskEvent::Answered { .. }
                | TaskEvent::ChangesRequested { .. }
                | TaskEvent::NodeUpdated { .. }
                | TaskEvent::Merged { .. }
                | TaskEvent::ReviewClosed { .. }
                | TaskEvent::Reopen
                | TaskEvent::Cancel { .. } => {}
            }
        }
        all
    }

    /// `None` is refused.
    type Row = (&'static str, [Option<TaskState>; 7]);

    /// Every (state, event) pair with nothing in the way: no blocker, nothing under review, no
    /// dependency left, checks passed, not auto-merge. Columns follow `TaskState::ALL`:
    /// waiting_dependency, running, needs_you, in_review, closing, done, cancelled.
    const MATRIX: [Row; 14] = [
        ("dependency_ready", [Some(Running), Some(Running), Some(NeedsYou), Some(InReview), Some(Closing), Some(Done), Some(Cancelled)]),
        ("awaits_merge", [Some(WaitingDependency), Some(WaitingDependency), Some(NeedsYou), Some(InReview), Some(Closing), None, None]),
        ("blocker_raised", [None, Some(NeedsYou), Some(NeedsYou), Some(NeedsYou), Some(NeedsYou), None, None]),
        ("pr_opened", [None, Some(InReview), Some(NeedsYou), Some(InReview), Some(InReview), None, None]),
        ("document_sent", [None, Some(InReview), Some(NeedsYou), Some(InReview), Some(InReview), None, None]),
        ("auto_merged", [None, Some(Closing), Some(NeedsYou), Some(Closing), Some(Closing), Some(Done), Some(Cancelled)]),
        ("final_report", [None, Some(Done), Some(Done), Some(Done), Some(Done), Some(Done), None]),
        ("answered", [None, None, Some(Running), None, None, None, None]),
        ("changes_requested", [None, Some(Running), Some(NeedsYou), Some(Running), Some(Running), None, None]),
        ("node_updated", [None, Some(Running), Some(NeedsYou), Some(InReview), Some(Closing), None, None]),
        ("merged", [None, Some(Closing), Some(NeedsYou), Some(Closing), Some(Closing), Some(Done), Some(Cancelled)]),
        ("review_closed", [None, Some(Running), Some(NeedsYou), Some(Running), Some(Closing), Some(Done), Some(Cancelled)]),
        ("reopen", [None, None, None, None, None, Some(Running), Some(Running)]),
        ("cancel", [Some(Cancelled), Some(Cancelled), Some(Cancelled), Some(Cancelled), Some(Cancelled), None, None]),
    ];

    #[test]
    fn every_state_and_event_pair_is_decided_as_the_matrix_says() {
        let events = every_event();
        assert_eq!(events.len(), MATRIX.len(), "one row per event");
        for event in &events {
            let (_, row) = MATRIX.iter().find(|(name, _)| *name == event.name()).unwrap_or_else(|| panic!("no row for {}", event.name()));
            for (state, expected) in TaskState::ALL.iter().zip(row) {
                let got = transition(*state, event, &Guards::default());
                assert_eq!(got.as_ref().ok(), expected.as_ref(), "{} + {}: {got:?}", state.name(), event.name());
                if let Err(r) = got {
                    assert!(!r.reason.is_empty(), "{} + {} refused without a reason", state.name(), event.name());
                }
            }
        }
    }

    /// The cells guards change, each with the guard that changes it.
    #[test]
    fn guards_decide_the_cells_they_name() {
        let pr = Guards { unmerged_pr: Some(150), ..Guards::default() };
        let document = Guards { unreviewed_document: Some(146), ..Guards::default() };
        let blocker = Guards { blocker: Some(Q), ..Guards::default() };
        let auto = Guards { auto_merge: true, ..Guards::default() };
        let finished = Guards { nothing_after_merge: true, ..Guards::default() };
        let waiting = Guards { waiting_on: Some("t-a".into()), ..Guards::default() };
        let failed = Guards { checks_failed: Some(151), ..Guards::default() };
        let below = Guards { unmerged_dependency: Some(149), ..Guards::default() };
        let cancelled = Guards { waiting_on: Some("t-a".into()), cancelled_dependency: Some("t-a".into()), ..Guards::default() };
        let awaiting = Guards { started: true, awaited_merge: Some(264), ..Guards::default() };
        let started = Guards { started: true, ..Guards::default() };
        let replacing = TaskEvent::BlockerRaised { blocker: Q, replaces: true };
        let e = |name: &str| every_event().into_iter().find(|e| e.name() == name).unwrap();
        let cases: Vec<(TaskState, TaskEvent, &Guards, Result<TaskState, &str>)> = vec![
            (WaitingDependency, e("dependency_ready"), &waiting, Ok(WaitingDependency)),
            (WaitingDependency, e("dependency_ready"), &awaiting, Ok(WaitingDependency)),
            (WaitingDependency, e("dependency_ready"), &started, Ok(Running)),
            (WaitingDependency, e("blocker_raised"), &awaiting, Ok(NeedsYou)),
            (WaitingDependency, e("pr_opened"), &awaiting, Ok(InReview)),
            (WaitingDependency, e("document_sent"), &awaiting, Ok(InReview)),
            (WaitingDependency, e("final_report"), &awaiting, Ok(Done)),
            (WaitingDependency, e("changes_requested"), &awaiting, Ok(Running)),
            (WaitingDependency, e("answered"), &awaiting, Err("no question of the task is open")),
            (WaitingDependency, e("reopen"), &awaiting, Err("only a done or cancelled task can be reopened")),
            (WaitingDependency, e("cancel"), &awaiting, Ok(Cancelled)),
            (Running, e("blocker_raised"), &blocker, Err("question #7 is still open")),
            (NeedsYou, e("blocker_raised"), &blocker, Err("question #7 is still open")),
            (NeedsYou, replacing, &blocker, Ok(NeedsYou)),
            (Running, e("pr_opened"), &auto, Ok(Running)),
            (Closing, e("pr_opened"), &auto, Ok(Closing)),
            (Running, e("auto_merged"), &failed, Ok(Closing)),
            (Running, e("auto_merged"), &below, Ok(Closing)),
            (InReview, e("merged"), &below, Ok(Closing)),
            (Done, e("merged"), &below, Ok(Done)),
            (Done, e("reopen"), &waiting, Ok(WaitingDependency)),
            (Cancelled, e("reopen"), &waiting, Ok(WaitingDependency)),
            (Cancelled, e("reopen"), &cancelled, Err("t-a was cancelled: reopen it first")),
            (Running, e("auto_merged"), &pr, Ok(Running)),
            (Running, e("auto_merged"), &finished, Ok(Done)),
            (Running, e("final_report"), &pr, Err("PR #150 is not merged")),
            (Running, e("final_report"), &document, Err("document #146 is not reviewed yet")),
            (Closing, e("final_report"), &pr, Err("PR #150 is not merged")),
            (InReview, e("final_report"), &pr, Ok(InReview)),
            (NeedsYou, e("final_report"), &blocker, Err("question #7 is still open")),
            (NeedsYou, e("final_report"), &pr, Err("PR #150 is not merged")),
            (NeedsYou, e("final_report"), &document, Err("document #146 is not reviewed yet")),
            (Running, e("node_updated"), &pr, Ok(InReview)),
            (Closing, e("node_updated"), &document, Ok(InReview)),
            (InReview, e("merged"), &pr, Ok(InReview)),
            (InReview, e("merged"), &finished, Ok(Done)),
            (Running, e("merged"), &finished, Ok(Done)),
            (InReview, e("review_closed"), &document, Ok(InReview)),
            (InReview, e("review_closed"), &finished, Ok(Done)),
        ];
        for (state, event, g, expected) in cases {
            let got = transition(state, &event, g).map_err(|r| r.reason);
            assert_eq!(got, expected.map_err(str::to_string), "{} + {} with {g:?}", state.name(), event.name());
        }
    }

    /// The transitions §2.4 of docs/tasks-plan.md calls impossible, refused with what blocks them.
    #[test]
    fn impossible_transitions_are_refused_with_the_reason() {
        let pr = Guards { unmerged_pr: Some(150), ..Guards::default() };
        let cases: Vec<(&str, TaskState, TaskEvent, Guards, &str)> = vec![
            ("finishing with an unmerged PR", Running, TaskEvent::FinalReport { report: 3 }, pr.clone(), "PR #150 is not merged"),
            ("a second blocker", NeedsYou, TaskEvent::BlockerRaised { blocker: Q, replaces: false }, Guards { blocker: Some(Q), ..Guards::default() }, "question #7 is still open"),
            ("done again from anything but a reopen", Done, TaskEvent::BlockerRaised { blocker: Q, replaces: false }, Guards::default(), "the task is done: reopen it first"),
            ("done again from a push", Done, TaskEvent::NodeUpdated { on: Item::Pr(1) }, Guards::default(), "the task is done: reopen it first"),
            ("done again from a PR", Done, TaskEvent::PrOpened { pr: 1 }, Guards::default(), "the task is done: reopen it first"),
            ("cancelling what is finished", Done, TaskEvent::Cancel { reason: None }, Guards::default(), "the task is already done"),
            ("reopening what is not done", Running, TaskEvent::Reopen, Guards::default(), "only a done or cancelled task can be reopened"),
            ("working before its dependency", WaitingDependency, TaskEvent::PrOpened { pr: 1 }, Guards { waiting_on: Some("t-a".into()), ..Guards::default() }, "the task has not started: it waits for t-a"),
        ];
        for (what, state, event, g, reason) in cases {
            assert_eq!(transition(state, &event, &g), Err(Refused { reason: reason.into() }), "{what}");
        }
        let mut board = Board::default();
        board.tasks.insert("t-s".into(), task(InReview));
        let below = Guards { unmerged_dependency: Some(150), ..Guards::default() };
        assert_eq!(board.apply_fact("t-s", TaskEvent::Merged { item: Item::Pr(2) }, &below, 5), Ok(Closing), "merging above an unmerged dependency: GitHub merged it, so the task moves");
        assert_eq!(board.tasks["t-s"].history[0].note.as_deref(), Some("merge PR #150 first"), "what Alondra would have refused stays on the move");
        for state in TaskState::ALL {
            let merged = transition(state, &TaskEvent::Merged { item: Item::Pr(1) }, &pr);
            assert_ne!((state, merged.clone()), (InReview, Ok(Done)), "in review to done without a merge left");
            if state == Done {
                assert_eq!(merged, Ok(Done), "a merge notice never reopens a done task");
            }
        }
    }

    /// t-47328 (25 Sep): PR #102 waited for review when its node asked #253 (how #102 ships) and
    /// #254 (a cron #102 breaks). Both joined the PR's card; the merge withdrew #254 unanswered.
    #[test]
    fn a_pr_under_review_is_the_tasks_one_pending_point() {
        let review = Guards { unmerged_pr: Some(102), ..Guards::default() };
        let raised = |kind, asked, about_pr| TaskEvent::BlockerRaised { blocker: Blocker { kind, asked, about_pr, ..Q }, replaces: false };
        let other_work = Err(refused(
            "PR #102 waits for review. If the answer changes what PR #102 contains or how it ships, resend with about_pr: 102. \
             Otherwise open a separate task with new_task and ask there",
        ));
        for state in [Running, InReview, Closing] {
            assert_eq!(transition(state, &raised(BlockerKind::Question, true, None), &review), other_work, "{state:?}: #254, about other work");
            assert_eq!(transition(state, &raised(BlockerKind::Action, true, None), &review), other_work, "{state:?}: an action about other work");
            assert_eq!(transition(state, &raised(BlockerKind::Question, true, Some(102)), &review), Ok(NeedsYou), "{state:?}: #253, about the PR");
            assert_eq!(transition(state, &raised(BlockerKind::Permission, false, None), &review), Ok(NeedsYou), "{state:?}: Claude Code's prompt is a fact");
            assert_eq!(transition(state, &raised(BlockerKind::Question, false, None), &review), Ok(NeedsYou), "{state:?}: a menu, a terminal card, or a blocker recorded before the marker");
            assert_eq!(transition(state, &raised(BlockerKind::Question, true, None), &Guards::default()), Ok(NeedsYou), "{state:?}: no PR under review");
        }
        let replacing = TaskEvent::BlockerRaised { blocker: Blocker { asked: true, ..Q }, replaces: true };
        assert_eq!(transition(NeedsYou, &replacing, &Guards { blocker: Some(Q), ..review.clone() }), other_work, "replacing a question does not skip the review");
        let answered = TaskEvent::Answered { question: 253 };
        assert_eq!(transition(NeedsYou, &answered, &review), Ok(InReview), "answered while the PR still waits: back in review, not running");
        assert_eq!(transition(NeedsYou, &answered, &Guards::default()), Ok(Running));
    }

    fn task(state: TaskState) -> Task {
        Task {
            id: "t-s".into(),
            request: "r-1".into(),
            title: "fix".into(),
            brief: None,
            depends_on: vec![],
            options: TaskOptions::default(),
            assignee: None,
            prs: vec![],
            documents: vec![],
            blocker: None,
            notices: vec![],
            state,
            state_since: 0,
            created_at: 0,
            final_report: None,
            awaiting: None,
            launched: false,
            launched_at: None,
            launched_as: None,
            start: None,
            recorded: false,
            titled: false,
            history: vec![],
            anomalies: vec![],
            archived_at: None,
        }
    }

    #[test]
    fn a_refused_fact_stays_on_the_task_and_is_counted() {
        let mut board = Board::default();
        board.tasks.insert("t-s".into(), task(Running));
        let raised = |q: u64| TaskEvent::BlockerRaised { blocker: Blocker { question: q, ..Q }, replaces: false };
        assert_eq!(board.apply_fact("t-s", raised(7), &Guards::default(), 10), Ok(NeedsYou));
        let g = Guards { blocker: board.tasks["t-s"].blocker, ..Guards::default() };
        assert!(board.apply_fact("t-s", raised(8), &g, 20).is_err());
        let t = &board.tasks["t-s"];
        assert_eq!((t.state, t.blocker.map(|b| b.question), t.state_since), (NeedsYou, Some(7), 10), "the first blocker stays");
        assert_eq!(t.anomalies.len(), 1);
        assert_eq!(board.anomalies.get("needs_you+blocker_raised"), Some(&1));
        assert_eq!(board.apply_fact("t-s", TaskEvent::Answered { question: 7 }, &Guards::default(), 30), Ok(Running));
        let t = &board.tasks["t-s"];
        assert_eq!(t.blocker, None);
        assert_eq!(t.history.iter().map(|h| (h.from, h.to)).collect::<Vec<_>>(), vec![(Running, NeedsYou), (NeedsYou, Running)]);
    }

    #[test]
    fn a_request_is_defining_until_it_has_tasks_and_done_when_all_are_finished() {
        let mut board = Board::default();
        assert_eq!(board.request_state("r-1"), RequestState::Defining);
        board.tasks.insert("t-a".into(), Task { id: "t-a".into(), ..task(Done) });
        board.tasks.insert("t-b".into(), Task { id: "t-b".into(), ..task(Running) });
        assert_eq!(board.request_state("r-1"), RequestState::InProgress);
        board.tasks.get_mut("t-b").unwrap().state = Cancelled;
        assert_eq!(board.request_state("r-1"), RequestState::Done);
    }
}
