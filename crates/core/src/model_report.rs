//! How each model the orchestrator chose did, as `alondra models` shows it. Each launch of a
//! Task is one attempt: its outcome is what happened to the Task until the next launch for it.
use crate::Millis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelReport {
    pub since: Option<Millis>,
    pub until: Millis,
    pub by_model: Vec<Group>,
    pub by_approach: Vec<Group>,
    /// Work split into design, implementation and review, measured apart.
    #[serde(default)]
    pub by_role: Vec<Group>,
    /// Sonnet attempts that were reopened, relaunched, cancelled, had changes requested on any
    /// delivery, or had serious findings.
    pub sonnet_trouble: Vec<Attempt>,
    /// Every attempt in the period, oldest first, to compare tasks one by one.
    #[serde(default)]
    pub attempts: Vec<Attempt>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Group {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approach: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub tasks: u32,
    /// Reached done with no reopen, relaunch or cancel.
    pub done_clean: u32,
    pub reopened: u32,
    pub relaunched: u32,
    /// The model each relaunch moved to, by how many times.
    pub relaunched_to: BTreeMap<String, u32>,
    pub cancelled: u32,
    /// How many times the Task went to needs-you, across the group's attempts.
    pub questions: u32,
    pub changes_requested: u32,
    /// Rounds of changes asked for, by the user or a reviewer, across the group's attempts.
    #[serde(default)]
    pub change_rounds: u32,
    /// Of those, the rounds a reviewer task sent back.
    #[serde(default)]
    pub reviewer_rounds: u32,
    /// Serious failures reviewers found: broken behaviour, lost data, a security or session hole.
    #[serde(default)]
    pub serious_findings: u32,
    pub median_ms_to_done: Option<Millis>,
    pub mean_cost_usd: Option<f64>,
    /// How many attempts a cost was recorded for, which the mean is over.
    pub costed: u32,
}

impl Group {
    pub fn mean_change_rounds(&self) -> f64 {
        if self.tasks == 0 { 0.0 } else { self.change_rounds as f64 / self.tasks as f64 }
    }

    pub fn percent(&self, count: u32) -> u32 {
        if self.tasks == 0 { 0 } else { ((count as f64) * 100.0 / self.tasks as f64).round() as u32 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Reopened,
    Relaunched,
    Cancelled,
    ChangesRequested,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub task_id: String,
    pub title: String,
    pub at: Millis,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub reason: Option<String>,
    pub outcomes: Vec<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relaunched_to: Option<String>,
    /// Times the user or a reviewer asked for changes during this attempt.
    #[serde(default)]
    pub change_rounds: u32,
    /// Of those, the times a reviewer task sent it back.
    #[serde(default)]
    pub reviewer_rounds: u32,
    /// Serious failures reviewers found during this attempt.
    #[serde(default)]
    pub serious_findings: u32,
}
