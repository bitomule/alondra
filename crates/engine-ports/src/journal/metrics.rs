//! What the journal records to measure: the model decisions and what each session spent.
use alondra_core::Millis;
use alondra_core::metrics::steps::StepCounts;
use alondra_core::metrics::{Role, Usage};
use alondra_core::options::Approach;
use alondra_core::task_tags::TaskTags;
use alondra_domain_rules::ModelChooser;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionKind {
    Launch,
    Relaunch,
    Send,
}

impl DecisionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Relaunch => "relaunch",
            Self::Send => "send",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Decision {
    pub task_id: String,
    pub request_id: String,
    pub agent_id: Option<String>,
    pub at: Millis,
    pub kind: DecisionKind,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub approach: Option<Approach>,
    pub chosen_by: ModelChooser,
    pub reason: Option<String>,
    pub title: String,
    pub provider: Option<alondra_core::accounts::Provider>,
    pub tags: Option<TaskTags>,
}

/// What starting a host of a task wrote to and read from the prompt cache: its first request,
/// `since_previous` after the task's request before it, unknown when none was timed.
#[derive(Clone, Copy, Debug)]
pub struct SessionStarted<'a> {
    pub task_id: &'a str,
    pub session_id: &'a str,
    pub kind: &'a str,
    pub requested_at: Millis,
    pub since_previous: Option<Millis>,
    pub tokens: u64,
    pub written_1h: u64,
    pub written_5m: u64,
    pub read: u64,
}

/// A host start of a task as the journal keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedStart {
    pub session_id: String,
    pub kind: String,
    pub requested_at: Millis,
    pub since_previous: Option<Millis>,
    pub tokens: u64,
    pub written_1h: u64,
    pub written_5m: u64,
    pub read: u64,
}

/// What a session reported as one of its turns ended.
#[derive(Clone, Debug)]
pub struct Spent<'a> {
    pub session_id: &'a str,
    /// Fixed by the session's first report: a session without a task (the orchestrator, or a node
    /// no task was known for) is added up by day as it spends instead of folded.
    pub task_id: Option<&'a str>,
    pub role: Role,
    pub provider: &'a str,
    pub usage: &'a [Usage],
    pub steps: StepCounts,
    pub at: Millis,
}
