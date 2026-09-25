use serde::{Deserialize, Serialize};

use crate::model::{Answer, Message, Millis, NodeState, Question, Report};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct HookEvent {
    pub session_id: String,
    pub event: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub tool_summary: Option<String>,
    #[serde(default)]
    pub last_message: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub notification_type: Option<String>,
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// The task an Alondra tool call names, or the one an `Agent` call names after “Alondra task:”
    /// in its description or prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct DaemonObservation {
    pub session_id: String,
    pub short_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub tempo: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub needs: Option<String>,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub last_text: Option<String>,
    #[serde(default)]
    pub last_prompt: Option<String>,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub last_text_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<crate::model::BackgroundItem>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Hook(HookEvent),
    DaemonSeen(DaemonObservation),
    ProcessExited { session_id: String, pid: i32 },
    NodeRemoved { session_id: String },
    /// David marked the node as seen. Only daemons from before tasks record it; it changes nothing.
    Acknowledged { session_id: String },
    MarkedOrchestrator { session_id: String },
    QuestionAsked(Question),
    QuestionAnswered { id: u64, answer: Answer },
    QuestionDelivered { id: u64 },
    QuestionClosed { id: u64, status: crate::model::QuestionStatus, #[serde(default)] note: Option<String> },
    ReportPosted(Report),
    ReportRead { id: u64 },
    /// David reviewed the artifact a report sent: he approved it, or sent the node his comments.
    ArtifactReviewed { report: u64, verdict: ReviewVerdict },
    MessageQueued(Message),
    MessageHanded { id: u64 },
    Classified { session_id: String, turn: u64, state: NodeState, confidence: Option<f64> },
    WaitingMachine { session_id: String, reason: String },
    Pruned { before: Millis },
    /// What the Claude daemon says the node's terminal is waiting on (`waitingFor`), or that it
    /// no longer waits on anything.
    MenuSeen { session_id: String, waiting_for: Option<String> },
    /// A terminal attached to the node's session, or the last one left.
    AttachChanged { session_id: String, attached: bool },
    /// A node opened, or mentioned, a pull request, for the task it names.
    PrOpened {
        url: String,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
    },
    /// What GitHub says about a pull request now.
    PrUpdated(crate::pr::PullRequest),
    PrMerged { id: u64, method: crate::pr::MergeMethod },
    PrDismissed { id: u64 },
    /// Changes were requested on the PR; its card waits until the node pushes again.
    PrChangesRequested { id: u64 },
    /// David asked the node about its PR from the card; the card stays where it is.
    PrAsked { id: u64 },
    /// David or the orchestrator said which task of its node a PR card is for: the task it was
    /// on goes back to where its own facts leave it, and this one takes the PR as it is.
    PrReassigned { pr: u64, task: String },
    /// The user asked for new work with these options; launches that cite the request get them.
    RequestSent {
        request_id: String,
        options: crate::options::Resolved,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::model::Attachment>,
    },
    /// A node was launched with these options; `auto_merge` keeps its PRs out of the inbox. With
    /// `task` it was launched for that task, which it takes when it shows up.
    Launched {
        name: String,
        request_id: Option<String>,
        auto_merge: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
        /// The task's title, which otherwise is the node's name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// The short id `claude --bg` printed for the node's session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// alondrad tried to launch an orchestrator because none was running.
    OrchestratorLaunched {
        ok: bool,
        /// The short id `claude --bg` printed for its session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Alondra first heard from a session it launched: its hooks or its tools carried Alondra's
    /// mark, or its short id is one a launch printed. Alondra follows it from here on.
    Joined { session_id: String },
    /// From here on Alondra follows only the sessions it launched. Written once, by the first
    /// daemon that does: the sessions its records cannot tie to a launch leave the view.
    LaunchedOnly,
    /// alondrad restarted a node whose process ended while it still had work, or failed to.
    Resumed {
        session_id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A node said which of its tasks it works on now: until `minutes` go by or it says another,
    /// what its threads do with no task of their own counts for this one.
    Focused { session_id: String, task: String, minutes: u32 },
    /// A node's status line reported how full its context window is.
    ContextMeasured { session_id: String, percent: f64, window: u64 },
    /// A step of the `/compact` Alondra runs in an idle node before typing it a new task. The
    /// node's `PostCompact` hook ends it.
    Compaction { session_id: String, step: CompactionStep },
    /// From here on every task change is recorded as a task event. Written once, by the first
    /// daemon that records them.
    TasksRecorded,
    TaskCreated(crate::tasks::NewTask),
    /// A launched task's node showed up, or a subagent took the task.
    TaskAssigned {
        task: String,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
    },
    /// A task event applied to a recorded task, with the guards the view gives it then.
    TaskMoved { task: String, event: crate::tasks::TaskEvent },
    /// David archived finished tasks, and requests that finished without tasks.
    TasksArchived(crate::tasks::Archive),
    /// Tasks archived before `before` are forgotten: `archived_tasks_days` went by.
    ArchivedPruned { before: Millis },
    /// The orchestrator named a request.
    RequestTitled { request_id: String, title: String },
    /// David or the orchestrator named a task, replacing whatever it fell back to (most often a
    /// node's own name): it sticks, `follow_node` never overwrites it again.
    TaskTitled { task: String, title: String },
    /// The orchestrator closed a request that needed no task: it answered it, or David withdrew it.
    RequestClosed {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        withdrawn: bool,
    },
    Clock {
        #[serde(default)]
        machine_strained: bool,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStep {
    Requested,
    Typed,
    /// It could not be typed, or no `PostCompact` came in time: what waits for it goes in anyway.
    GaveUp,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Comments,
    Approved,
}

impl Event {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Event::Hook(h) => Some(&h.session_id),
            Event::DaemonSeen(d) => Some(&d.session_id),
            Event::ProcessExited { session_id, .. }
            | Event::NodeRemoved { session_id }
            | Event::Acknowledged { session_id }
            | Event::MarkedOrchestrator { session_id }
            | Event::Joined { session_id }
            | Event::Classified { session_id, .. }
            | Event::WaitingMachine { session_id, .. }
            | Event::AttachChanged { session_id, .. }
            | Event::MenuSeen { session_id, .. }
            | Event::Resumed { session_id, .. }
            | Event::ContextMeasured { session_id, .. }
            | Event::Compaction { session_id, .. }
            | Event::Focused { session_id, .. } => Some(session_id),
            Event::QuestionAsked(q) => Some(&q.session_id),
            Event::ReportPosted(r) => Some(&r.session_id),
            Event::MessageQueued(m) => Some(&m.session_id),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Event::Hook(_) => "hook",
            Event::DaemonSeen(_) => "daemon_seen",
            Event::ProcessExited { .. } => "process_exited",
            Event::NodeRemoved { .. } => "node_removed",
            Event::Acknowledged { .. } => "acknowledged",
            Event::MarkedOrchestrator { .. } => "marked_orchestrator",
            Event::QuestionAsked(_) => "question_asked",
            Event::QuestionAnswered { .. } => "question_answered",
            Event::QuestionDelivered { .. } => "question_delivered",
            Event::QuestionClosed { .. } => "question_closed",
            Event::ReportPosted(_) => "report_posted",
            Event::ReportRead { .. } => "report_read",
            Event::ArtifactReviewed { .. } => "artifact_reviewed",
            Event::MessageQueued(_) => "message_queued",
            Event::MessageHanded { .. } => "message_handed",
            Event::Classified { .. } => "classified",
            Event::WaitingMachine { .. } => "waiting_machine",
            Event::Pruned { .. } => "pruned",
            Event::OrchestratorLaunched { .. } => "orchestrator_launched",
            Event::Resumed { .. } => "resumed",
            Event::ContextMeasured { .. } => "context_measured",
            Event::Compaction { .. } => "compaction",
            Event::Focused { .. } => "focused",
            Event::AttachChanged { .. } => "attach_changed",
            Event::MenuSeen { .. } => "menu_seen",
            Event::PrOpened { .. } => "pr_opened",
            Event::PrUpdated(_) => "pr_updated",
            Event::PrMerged { .. } => "pr_merged",
            Event::PrDismissed { .. } => "pr_dismissed",
            Event::PrChangesRequested { .. } => "pr_changes_requested",
            Event::PrAsked { .. } => "pr_asked",
            Event::PrReassigned { .. } => "pr_reassigned",
            Event::TasksRecorded => "tasks_recorded",
            Event::TaskCreated(_) => "task_created",
            Event::TaskAssigned { .. } => "task_assigned",
            Event::TaskMoved { .. } => "task_moved",
            Event::TasksArchived(_) => "tasks_archived",
            Event::ArchivedPruned { .. } => "archived_pruned",
            Event::RequestTitled { .. } => "request_titled",
            Event::TaskTitled { .. } => "task_titled",
            Event::RequestClosed { .. } => "request_closed",
            Event::Clock { .. } => "clock",
            Event::RequestSent { .. } => "request_sent",
            Event::Launched { .. } => "launched",
            Event::Joined { .. } => "joined",
            Event::LaunchedOnly => "launched_only",
        }
    }
}
