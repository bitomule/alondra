//! How Alondra starts a Task by itself: the launch plan its creator gives it with `CreateTask`.
//! A Task never exists without one, so it always has a way to start.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPlan {
    pub name: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approach: Option<crate::options::Approach>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<TaskRole>,
    /// The project the task belongs to. Alondra sets it from where the task works when the task is
    /// created; whatever its creator sends is replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl LaunchPlan {
    pub fn new(name: impl Into<String>, cwd: impl Into<String>) -> Self {
        LaunchPlan { name: name.into(), cwd: cwd.into(), mission: None, model: None, effort: None, approach: None, reason: None, role: None, project: None }
    }

    /// The required field a plan leaves blank, if any.
    pub fn missing(&self) -> Option<&'static str> {
        if self.name.trim().is_empty() {
            Some("launch.name")
        } else if self.cwd.trim().is_empty() {
            Some("launch.cwd")
        } else {
            None
        }
    }
}

/// The part work split into design, implementation and review plays, so each is measured apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRole {
    Design,
    Implementation,
    Review,
}

impl TaskRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Implementation => "implementation",
            Self::Review => "review",
        }
    }
}
