//! Task quality and metered usage, before hierarchical aggregation.
use super::{ModelKey, Rules};
use crate::metrics::fold::Outcome;
use crate::task_tags::TaskTags;
use serde::{Deserialize, Serialize};

pub const CHANGE_PENALTY: i64 = 30;
pub const CHANGE_CAP: i64 = 3;
pub const REOPEN_PENALTY: i64 = 30;
pub const INTERACTION_PENALTY: i64 = 5;
pub const INTERACTION_CAP: i64 = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quality {
    pub outcome: Outcome,
    pub change_rounds: i64,
    pub reviewer_rounds: i64,
    pub interventions: i64,
    pub reopened: bool,
    pub had_pr: bool,
    pub pr_merged: bool,
}

pub fn score(task: &Quality, rules: &Rules) -> Option<i64> {
    if task.outcome == Outcome::Cancelled {
        return None;
    }
    if task.had_pr && !task.pr_merged {
        return Some(0);
    }
    let rounds = (task.change_rounds - task.reviewer_rounds).max(0);
    let other = (task.interventions - rounds).max(0);
    Some(
        (100 - rules.change_penalty * rounds.min(rules.change_cap)
            - rules.reopen_penalty * i64::from(task.reopened)
            - rules.interaction_penalty * other.min(rules.interaction_cap))
        .max(0),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub model: ModelKey,
    pub cost_micros: Option<u64>,
    /// Input, output, cache read, cache write; None means unmeasured.
    pub tokens: Option<[u64; 4]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSample {
    /// The attempt that finished; abandoned attempts never become independent samples.
    pub model: Option<ModelKey>,
    pub tags: Option<TaskTags>,
    pub quality: Quality,
    pub steps: u64,
    pub subagent_steps: u64,
    /// All attempts, models and subagents, each usage contribution exactly once.
    pub usage: Vec<Usage>,
}

/// Log quantities are rounded per task, before addition; the caller supplies positive values.
pub fn milinepers(value: f64) -> i64 {
    (value.ln() * 1000.0).round() as i64
}
