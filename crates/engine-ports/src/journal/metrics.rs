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
