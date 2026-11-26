//! What a launch carries and how it fails, as the ports speak of them.
use alondra_core::accounts::Provider;

/// What the task's Claude is launched with besides its prompt: the model and effort the launch
/// resolved and the tools its request denies. Kept beside the execution's host, so a host that
/// resumes the session launches Claude the same way.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LaunchOptions {
    #[serde(default)]
    pub provider: Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<alondra_core::model_selection::TaskSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
}

impl LaunchOptions {
    /// The effort levels the Agent SDK takes; any other a launch resolved is left to Claude.
    pub fn effort(&self) -> Option<String> {
        self.effort.clone().filter(|effort| matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max"))
    }
}

#[derive(Debug)]
pub struct Failed {
    pub why: String,
    /// A write it needed found the disk full: once there is space it can go on as it was.
    pub no_space: bool,
    /// The domain's refusal, when the domain refused it.
    pub refusal: Option<alondra_domain_model::DomainError>,
}

impl Failed {
    pub fn new(why: String) -> Self {
        Self { why, no_space: false, refusal: None }
    }
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

/// A task the automatic launch may start: not started, with no node, its request open, no plan review
/// pending and no schedule run behind it.
pub fn candidate(state: &alondra_domain_model::DomainState, task: &alondra_domain_model::Task) -> bool {
    task.state == alondra_domain_model::TaskState::NotStarted
        && !task.started
        && task.agent_id.is_none()
        && !task.archived
        && !state.plan_review_pending(&task.id)
        && state.requests.get(&task.request_id).is_some_and(|request| !request.closed)
        && !state.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(&task.id))
}
