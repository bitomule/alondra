//! Requests and tasks: what David asked for, and the pieces of work it became. A task moves only
//! through `transition`, one exhaustive `match` over (state, event): a pair nobody decided does
//! not compile, and a pair that is not allowed is refused with the reason.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{Attachment, Millis};

mod archive;
mod define;
mod derive;

pub use archive::Archive;
pub use define::{Candidate, CandidateTask, Dependency, dependencies};
pub use derive::{Before, CardTask, NODE_CLOSED, Pass, task_id};

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
}

impl Request {
    fn new(id: &str, created_at: Millis, synthetic: bool) -> Self {
        Request { id: id.to_string(), text: None, attachments: vec![], options: None, created_at, synthetic, archived_at: None }
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
    DependencyReady { task: String },
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
    /// A PR of a task this one depends on that is not merged yet: this task's PRs merge after it.
    pub unmerged_dependency: Option<u64>,
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

fn blocked(g: &Guards) -> Result<(), Refused> {
    match g.blocker {
        Some(b) => Err(refused(format!("{} #{} is still open", b.kind.noun(), b.question))),
        None => Ok(()),
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
        (WaitingDependency, E::DependencyReady { .. }) => Ok(if g.waiting_on.is_some() { WaitingDependency } else { Running }),
        (Running | NeedsYou | InReview | Closing | Done | Cancelled, E::DependencyReady { .. }) => Ok(state),

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

        (Running | NeedsYou | InReview | Closing, E::BlockerRaised { replaces, .. }) => {
            if *replaces { Ok(NeedsYou) } else { blocked(g).map(|()| NeedsYou) }
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

        (Running | InReview, E::AutoMerged { .. }) => mergeable(g).map(|()| after_merge(state, g)),
        (NeedsYou | Closing | Done | Cancelled, E::AutoMerged { .. }) => mergeable(g).map(|()| state),

        (Running | Closing, E::FinalReport { .. }) => nothing_pending(g).map(|()| Done),
        (InReview, E::FinalReport { .. }) => Ok(if g.reviews_left() { InReview } else { Done }),
        (NeedsYou, E::FinalReport { .. }) => blocked(g).and_then(|()| nothing_pending(g)).map(|()| Done),
        (Done, E::FinalReport { .. }) => Ok(Done),
        (Cancelled, E::FinalReport { .. }) => Err(is_cancelled()),

        (NeedsYou, E::Answered { .. }) => Ok(Running),
        (Running | InReview | Closing | Done | Cancelled, E::Answered { .. }) => Err(nothing_to_answer()),

        (InReview | Closing, E::ChangesRequested { .. }) => Ok(Running),
        (Running | NeedsYou, E::ChangesRequested { .. }) => Ok(state),
        (Done, E::ChangesRequested { .. }) => Err(is_done()),
        (Cancelled, E::ChangesRequested { .. }) => Err(is_cancelled()),

        (Running | InReview | Closing, E::NodeUpdated { .. }) => Ok(if g.reviews_left() { InReview } else { state }),
        (NeedsYou, E::NodeUpdated { .. }) => Ok(NeedsYou),
        (Done, E::NodeUpdated { .. }) => Err(is_done()),
        (Cancelled, E::NodeUpdated { .. }) => Err(is_cancelled()),

        (Running | InReview, E::Merged { .. }) => dependencies_merged(g).map(|()| after_merge(state, g)),
        (NeedsYou | Closing | Done | Cancelled, E::Merged { .. }) => dependencies_merged(g).map(|()| state),

        (InReview, E::ReviewClosed { .. }) => Ok(if g.reviews_left() {
            InReview
        } else if g.nothing_after_merge {
            Done
        } else {
            Running
        }),
        (Running | NeedsYou | Closing | Done | Cancelled, E::ReviewClosed { .. }) => Ok(state),

        (Done | Cancelled, E::Reopen) => Ok(Running),
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
    /// A launch was matched to the task's node.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub launched: bool,
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

    fn last_change(&self) -> Millis {
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
    pub fn request_state(&self, id: &str) -> RequestState {
        let mut tasks = self.tasks.values().filter(|t| t.request == id).peekable();
        if tasks.peek().is_none() {
            RequestState::Defining
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
                match &event {
                    TaskEvent::BlockerRaised { blocker, .. } => task.blocker = Some(*blocker),
                    TaskEvent::FinalReport { report } => task.final_report = Some(*report),
                    TaskEvent::ChangesRequested { .. } | TaskEvent::Reopen => task.final_report = None,
                    _ => {}
                }
                if to != TaskState::NeedsYou {
                    task.blocker = None;
                }
                if !to.finished() {
                    task.archived_at = None;
                }
                if to != from {
                    task.state = to;
                    task.state_since = at;
                }
                task.history.push(Transition { from, to, event, at });
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
            launched: t.launched,
            launched_as: None,
            start: t.start.clone(),
            recorded: true,
            history: vec![],
            anomalies: vec![],
            archived_at: None,
        };
        self.tasks.entry(t.id.clone()).or_insert(task);
    }

    /// Forgets tasks nobody can act on any more: their node is gone, nothing of theirs waits for
    /// David and nothing happened to them since `before`. A finished task stays until it is
    /// archived (`prune_archived`). Requests left without tasks, and launches that never found
    /// their node, go with them.
    pub(crate) fn prune(&mut self, before: Millis, keep: impl Fn(&Task) -> bool) {
        self.tasks.retain(|_, t| keep(t) || t.state.finished() || t.last_change() >= before);
        let finished: std::collections::HashSet<String> = self.requests.keys().filter(|id| self.finished_without_tasks(id)).cloned().collect();
        let tasks = &self.tasks;
        self.requests.retain(|id, r| r.created_at >= before || finished.contains(id) || tasks.values().any(|t| &t.request == id));
        self.launches.retain(|_, l| l.at >= before);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TaskState::*;

    const Q: Blocker = Blocker { kind: BlockerKind::Question, question: 7, since: 0 };

    fn every_event() -> Vec<TaskEvent> {
        let all = vec![
            TaskEvent::DependencyReady { task: "t-a".into() },
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
    const MATRIX: [Row; 13] = [
        ("dependency_ready", [Some(Running), Some(Running), Some(NeedsYou), Some(InReview), Some(Closing), Some(Done), Some(Cancelled)]),
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
        let replacing = TaskEvent::BlockerRaised { blocker: Q, replaces: true };
        let e = |name: &str| every_event().into_iter().find(|e| e.name() == name).unwrap();
        let cases: Vec<(TaskState, TaskEvent, &Guards, Result<TaskState, &str>)> = vec![
            (WaitingDependency, e("dependency_ready"), &waiting, Ok(WaitingDependency)),
            (Running, e("blocker_raised"), &blocker, Err("question #7 is still open")),
            (NeedsYou, e("blocker_raised"), &blocker, Err("question #7 is still open")),
            (NeedsYou, replacing, &blocker, Ok(NeedsYou)),
            (Running, e("pr_opened"), &auto, Ok(Running)),
            (Closing, e("pr_opened"), &auto, Ok(Closing)),
            (Running, e("auto_merged"), &failed, Err("the checks of PR #151 have not passed")),
            (Running, e("auto_merged"), &below, Err("merge PR #149 first")),
            (InReview, e("merged"), &below, Err("merge PR #149 first")),
            (Done, e("merged"), &below, Err("merge PR #149 first")),
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
            ("merging above an unmerged dependency", InReview, TaskEvent::Merged { item: Item::Pr(2) }, Guards { unmerged_dependency: Some(150), ..Guards::default() }, "merge PR #150 first"),
        ];
        for (what, state, event, g, reason) in cases {
            assert_eq!(transition(state, &event, &g), Err(Refused { reason: reason.into() }), "{what}");
        }
        for state in TaskState::ALL {
            let merged = transition(state, &TaskEvent::Merged { item: Item::Pr(1) }, &pr);
            assert_ne!((state, merged.clone()), (InReview, Ok(Done)), "in review to done without a merge left");
            if state == Done {
                assert_eq!(merged, Ok(Done), "a merge notice never reopens a done task");
            }
        }
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
            launched: false,
            launched_as: None,
            start: None,
            recorded: false,
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
