//! Who an open task waits for. Every open task is in exactly one of these hands: its node works on
//! it, the gate holds it until there is room (capacity, memory, disk, usage, or a retry Alondra
//! scheduled), another open task holds it, the user holds it (a card, a review, their pause), or
//! it is closing. A task in nobody's hands is a bug: `holder` names it, and `react` and
//! `after_failure` are the decisions that keep every task in someone's.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    Node,
    Gate(GateCause),
    Task(TaskId),
    User(UserCause),
    Closing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateCause {
    /// It waits for room: the node limit, memory, disk or usage.
    Room,
    /// Its last start failed and Alondra tries again at this time.
    Retry { at: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserCause {
    Card(InteractionId),
    Review,
    Paused,
}

/// An open task nobody will move: what it lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ownerless {
    pub task: TaskId,
    pub why: &'static str,
}

/// What the daemon knows of how a task gets going, beside the domain state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartFacts {
    pub planned: bool,
    pub start: Start,
    pub node: NodeFact,
    /// The user paused it.
    pub held: bool,
    /// The user closed its node; a relaunch starts in that node's folder.
    pub closed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Start {
    /// Not started yet: the gate starts it when there is room.
    #[default]
    Ready,
    /// A node is being started for it.
    Launching,
    /// Its last start failed; Alondra tries again at `at`.
    Retry { attempt: u8, at: i64 },
    /// Its start failed for good or needs the user: a start problem card says why.
    Blocked,
    /// A node was started for it.
    Launched,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NodeFact {
    #[default]
    Alive,
    /// Its connection is gone and Alondra is restarting the session.
    Reviving,
    /// Restarting failed and a card asks the user what to do.
    Asking,
    /// Its session is gone and nothing brings it back.
    Gone,
    /// It ended its turn with nothing running that will wake it, neither a result nor a question;
    /// `sent_back` while Alondra still sends it back to its task, before its cap tells the user.
    Stopped { sent_back: bool },
}

/// Why a task cannot start by itself, as the card that puts it in front of the user says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartProblem {
    LaunchFailed { error: String },
    Untrusted {
        folder: String,
        /// Why Alondra could not trust it when the user asked it to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    NodeClosed,
    DependencyCancelled { dependency: TaskId },
    /// Claude API errors keep ending its node's turns and Alondra no longer sends it back by itself.
    ApiFailed {
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Its turns the error ended in a row.
        attempts: u8,
    },
}

pub const START_PROBLEM_PREFIX: &str = "start-problem-";
pub const START_RETRY: &str = "retry";
pub const START_CANCEL: &str = "cancel";
pub const START_WITHOUT: &str = "start_without";

/// How many times Alondra starts a failed task again by itself, and the wait before each.
pub const START_RETRIES: [i64; 3] = [60_000, 5 * 60_000, 15 * 60_000];

/// What follows a failed start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfterFailure {
    Retry { attempt: u8, at: i64 },
    Ask(StartProblem),
}

/// A start that failed is tried again by itself a few times, then put in front of the user. A
/// folder Claude Code does not trust goes to the user at once: only they can trust it.
pub fn after_failure(attempts: u8, problem: StartProblem, now: i64) -> AfterFailure {
    match (&problem, START_RETRIES.get(usize::from(attempts))) {
        (StartProblem::LaunchFailed { .. }, Some(wait)) => AfterFailure::Retry { attempt: attempts + 1, at: now + wait },
        _ => AfterFailure::Ask(problem),
    }
}

/// What the daemon does next so no task is left in nobody's hands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reaction {
    Ask(TaskId, StartProblem),
    /// The user answered "try again" on its start problem: the start goes back to the gate.
    Retry(TaskId),
}

impl DomainState {
    /// The start problem card of the task the user has not answered yet.
    pub fn open_start_problem(&self, task: &TaskId) -> Option<&Interaction> {
        self.start_problems(task).find(|i| self.open(i))
    }

    /// The task's last start problem, whatever became of it.
    pub fn last_start_problem(&self, task: &TaskId) -> Option<&Interaction> {
        self.start_problems(task).max_by_key(|i| i.order)
    }

    /// A question or request of the task, from anyone, still waiting for the user.
    pub fn open_user_card(&self, task: &TaskId) -> Option<&Interaction> {
        self.interactions.values().find(|i| {
            i.recipient == Recipient::User && self.open(i) && matches!(&i.scope, Scope::Task { task_id } if task_id == task)
                && matches!(i.payload, InteractionPayload::Question { .. } | InteractionPayload::ActionRequest { .. })
        })
    }

    fn start_problems(&self, task: &TaskId) -> impl Iterator<Item = &Interaction> + '_ {
        let task = task.clone();
        self.interactions.values().filter(move |i| {
            i.id.0.starts_with(START_PROBLEM_PREFIX) && matches!(&i.scope, Scope::Task { task_id } if *task_id == task)
        })
    }

    /// Who the open task waits for.
    pub fn holder(&self, t: &Task, facts: &StartFacts) -> Result<Holder, Ownerless> {
        let ownerless = |why| Err(Ownerless { task: t.id.clone(), why });
        if t.state.terminal() || t.state == TaskState::Closing {
            return Ok(Holder::Closing);
        }
        if let Some(card) = self.open_user_card(&t.id) {
            return Ok(Holder::User(UserCause::Card(card.id.clone())));
        }
        if t.final_presentation.is_some() {
            match self.attention(t) {
                Attention::Act(_) => return Ok(Holder::User(UserCause::Review)),
                Attention::Wait(WaitReason::BlockedBy { task }) => return Ok(Holder::Task(task)),
                Attention::Wait(WaitReason::UpstreamPr { .. }) => {
                    return t.stack.as_ref().map_or_else(|| ownerless("stacked on a PR of no task"), |stack| Ok(Holder::Task(stack.upstream_task.clone())));
                }
                Attention::Wait(WaitReason::Answer { .. }) => return ownerless("waits for an answer to a card that is not open"),
                Attention::Wait(WaitReason::Merging { .. } | WaitReason::Closing) => return Ok(Holder::Closing),
                Attention::Wait(WaitReason::NodeFixing { .. }) | Attention::Idle => {}
            }
            if t.state == TaskState::AwaitingBlocker {
                return self.open_blockers(&t.id).into_iter().next().map_or_else(|| ownerless("awaits a blocker that is over"), |b| Ok(Holder::Task(b)));
            }
            if t.state == TaskState::WaitingDependency {
                return Ok(Holder::Closing);
            }
        }
        if t.state == TaskState::WaitingDependency {
            return match self.pending_dependency(t) {
                Some(dependency) if self.tasks.get(&dependency).is_some_and(Task::cancelled) =>
                    ownerless("depends on a cancelled task and nobody was asked"),
                Some(dependency) => Ok(Holder::Task(dependency)),
                None => ownerless("waits on nothing"),
            };
        }
        if t.agent_id.is_some() {
            return match facts.node {
                NodeFact::Alive | NodeFact::Reviving => Ok(Holder::Node),
                NodeFact::Asking => ownerless("its node is lost and the card about it is not open"),
                NodeFact::Gone => ownerless("its node is gone and nothing brings it back"),
                NodeFact::Stopped { sent_back: true } => Ok(Holder::Node),
                NodeFact::Stopped { sent_back: false } => ownerless("its node stopped without a result or a question and nobody was told"),
            };
        }
        if t.started {
            return ownerless("started, with no node");
        }
        if !facts.planned {
            return ownerless("has no launch plan");
        }
        if facts.held {
            return Ok(Holder::User(UserCause::Paused));
        }
        match facts.start {
            Start::Ready | Start::Launching => Ok(Holder::Gate(GateCause::Room)),
            Start::Retry { at, .. } => Ok(Holder::Gate(GateCause::Retry { at })),
            Start::Blocked if self.answered_retry(t, false) => Ok(Holder::Gate(GateCause::Room)),
            Start::Launched if self.answered_retry(t, true) => Ok(Holder::Gate(GateCause::Room)),
            Start::Blocked => ownerless("its start failed and nobody was asked"),
            Start::Launched => ownerless("its node is gone and nobody was asked"),
        }
    }

    /// The first task it waits for that is not done: a dependency, a delivered dependency or the
    /// task it is stacked on.
    pub fn pending_dependency(&self, t: &Task) -> Option<TaskId> {
        let stacked = t.stack.as_ref().map(|stack| &stack.upstream_task);
        t.dependencies.iter().filter(|d| self.tasks.get(*d).is_some_and(|d| d.state != TaskState::Done))
            .chain(t.delivered_dependencies.iter().filter(|d| self.tasks.get(*d).is_some_and(|d| d.cancelled() || d.final_presentation.is_none())))
            .chain(stacked.filter(|_| self.stack_ready(t).is_err()))
            .next()
            .cloned()
    }

    /// The user chose "try again" on its last start problem; with `since`, only if nothing happened
    /// to the task after, such as a new node closed again. A blocked start needs no such bound:
    /// the daemon unblocks it when it acts on the answer.
    fn answered_retry(&self, t: &Task, since: bool) -> bool {
        self.last_start_problem(&t.id).is_some_and(|i| !i.withdrawn && matches!(&i.payload,
            InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == START_RETRY && (!since || answer.at_revision >= t.revision)))
    }

    /// What must happen now so that every open task stays in someone's hands.
    pub fn react(&self, facts: &BTreeMap<TaskId, StartFacts>) -> Vec<Reaction> {
        let none = StartFacts::default();
        let mut out = vec![];
        for t in self.tasks.values().filter(|t| !t.state.terminal() && !t.archived) {
            let f = facts.get(&t.id).unwrap_or(&none);
            if self.open_start_problem(&t.id).is_some() {
                continue;
            }
            if t.state == TaskState::WaitingDependency && t.final_presentation.is_none()
                && let Some(dependency) = self.pending_dependency(t).filter(|d| self.tasks.get(d).is_some_and(Task::cancelled))
            {
                out.push(Reaction::Ask(t.id.clone(), StartProblem::DependencyCancelled { dependency }));
                continue;
            }
            let unstarted = t.state == TaskState::NotStarted && t.agent_id.is_none() && !t.started;
            if !unstarted || f.held {
                continue;
            }
            let relaunch = self.answered_retry(t, true);
            if (f.start == Start::Blocked && self.answered_retry(t, false)) || (f.start == Start::Launched && relaunch) {
                out.push(Reaction::Retry(t.id.clone()));
            } else if !relaunch && f.closed && matches!(f.start, Start::Launched | Start::Ready) {
                out.push(Reaction::Ask(t.id.clone(), StartProblem::NodeClosed));
            }
        }
        out
    }
}
