//! What an ended task comes to: how it ended, the rework it took, how often it needed the user
//! and where its time went. Read from what the store keeps of a task after its cards are
//! compacted: its history of states, the feedback on its deliveries and its launches.
use crate::Millis;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionKind {
    Launch,
    Relaunch,
    /// A message that hands the task to a node, which continues the attempt when it is the
    /// same node.
    Send,
}

/// A model chosen for the task: at a launch, a relaunch or a handover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub kind: DecisionKind,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub approach: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskFacts {
    /// Each state the task entered, in order, with when.
    pub history: Vec<(Millis, String)>,
    /// Each round of changes asked on its deliveries: whether a reviewer asked it.
    pub change_rounds: Vec<bool>,
    /// The user's own feedback on its deliveries: changes, questions and replies.
    pub user_feedback: i64,
    /// Oldest first.
    pub decisions: Vec<Decision>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Cancelled,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskMetrics {
    pub outcome: Outcome,
    /// When it first ran.
    pub started_at: Option<Millis>,
    pub finished_at: Millis,
    /// Of the attempt that ended it.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub approach: Option<String>,
    /// Done the first time: never reopened or relaunched, and no round of changes asked.
    pub clean: bool,
    pub change_rounds: i64,
    pub reviewer_rounds: i64,
    /// Times it waited for the user to answer, and feedback the user gave on its deliveries.
    pub interventions: i64,
    pub reopened: bool,
    pub relaunched: bool,
    pub work_ms: i64,
    /// Waiting for an answer or a review from the user.
    pub waiting_user_ms: i64,
    /// Waiting to start.
    pub queued_ms: i64,
    /// Waiting for another task.
    pub blocked_ms: i64,
}

fn terminal(state: &str) -> bool {
    matches!(state, "done" | "cancelled")
}

/// What the task came to, or nothing while it has not ended.
pub fn fold_task(facts: &TaskFacts) -> Option<TaskMetrics> {
    let (finished_at, last) = facts.history.last()?;
    let outcome = match last.as_str() {
        "done" => Outcome::Done,
        "cancelled" => Outcome::Cancelled,
        _ => return None,
    };
    let (mut work_ms, mut waiting_user_ms, mut queued_ms, mut blocked_ms) = (0, 0, 0, 0);
    for pair in facts.history.windows(2) {
        let ((from, state), (to, _)) = (&pair[0], &pair[1]);
        let spent = (to - from).max(0);
        match state.as_str() {
            "running" => work_ms += spent,
            "needs_you" | "for_your_review" | "in_review" => waiting_user_ms += spent,
            "not_started" => queued_ms += spent,
            "awaiting_blocker" | "waiting_dependency" => blocked_ms += spent,
            _ => {}
        }
    }
    let reopened = facts.history.windows(2).any(|pair| terminal(&pair[0].1) && !terminal(&pair[1].1));
    let (attempt, relaunched) = last_attempt(&facts.decisions);
    let change_rounds = facts.change_rounds.len() as i64;
    let questions = facts.history.iter().filter(|(_, state)| state == "needs_you").count() as i64;
    Some(TaskMetrics {
        outcome,
        started_at: facts.history.iter().find(|(_, state)| state == "running").map(|(at, _)| *at),
        finished_at: *finished_at,
        clean: outcome == Outcome::Done && !reopened && !relaunched && change_rounds == 0,
        change_rounds,
        reviewer_rounds: facts.change_rounds.iter().filter(|by_reviewer| **by_reviewer).count() as i64,
        interventions: questions + facts.user_feedback,
        reopened,
        relaunched,
        work_ms,
        waiting_user_ms,
        queued_ms,
        blocked_ms,
        model: attempt.as_ref().and_then(|d| d.model.clone()),
        effort: attempt.as_ref().and_then(|d| d.effort.clone()),
        approach: attempt.and_then(|d| d.approach),
    })
}

/// The attempt that ended the task, with what it left unsaid taken from earlier choices for its
/// node, and whether the task was ever launched again. A message to the node already working on
/// it continues its attempt; one to another node starts a new attempt without relaunching.
fn last_attempt(decisions: &[Decision]) -> (Option<Decision>, bool) {
    let mut current: Option<Decision> = None;
    let mut relaunched = false;
    for (index, d) in decisions.iter().enumerate() {
        if current.as_ref().is_some_and(|c| d.kind == DecisionKind::Send && c.agent_id == d.agent_id) {
            continue;
        }
        relaunched |= current.is_some() && d.kind != DecisionKind::Send;
        let earlier = |field: fn(&Decision) -> &Option<String>| decisions[..index].iter().rev()
            .filter(|e| e.agent_id.is_some() && e.agent_id == d.agent_id)
            .find_map(|e| field(e).clone());
        current = Some(Decision {
            model: d.model.clone().or_else(|| earlier(|e| &e.model)),
            effort: d.effort.clone().or_else(|| earlier(|e| &e.effort)),
            approach: d.approach.clone().or_else(|| earlier(|e| &e.approach)),
            ..d.clone()
        });
    }
    (current, relaunched)
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;
