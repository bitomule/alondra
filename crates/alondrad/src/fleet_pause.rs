//! Pausing working nodes when the Mac runs out of memory and bringing them back through the gate.
//! While the Mac is AtLimit one node is paused at a time, a minute apart: the lowest priority, and
//! within it the one whose turn started last, never a node waiting on the user and never the last
//! one working. Pausing interrupts the node's turn and then stops what its shells left running.
//! Paused nodes wait in the gate's queue ahead of new starts of their priority; they come back one
//! at a time once the Mac has been fine for 5 minutes, a step apart that doubles whenever a resume
//! saturates the Mac again. See docs/fleet-pacing.md §7.
use crate::domain_runtime::DomainRuntime;
use crate::machine_state::Health;
use alondra_core::domain::{DomainState, TaskId, TaskPriority, TaskState};
use alondra_core::Millis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) const PAUSE_EVERY: Millis = 60_000;
const TERMINATE_AFTER: Millis = 10_000;
const KILL_AFTER: Millis = 20_000;
pub(crate) const STEP: Millis = 3 * 60_000;
const STEP_MAX: Millis = 30 * 60_000;
/// Saturation this soon after a resume is blamed on it.
const BLAME_WINDOW: Millis = 10 * 60_000;
const CALM_RESET: Millis = 30 * 60_000;
/// A wait the Mac's gauges do not explain, such as a build queue, gets this long before it returns.
pub(crate) const DECLARED_GRACE: Millis = 5 * 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Reason {
    Memory,
    /// The node said it waits for the machine.
    Declared { reason: String },
    Usage,
    ByUser,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cleanup {
    #[default]
    Pending,
    Terminated,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Paused {
    pub task: TaskId,
    pub reason: Reason,
    pub since: Millis,
    /// The command its turn was running when it was stopped.
    #[serde(default)]
    pub cut: Option<String>,
    /// When its current turn started: a later turn means it went on by itself.
    #[serde(default)]
    pub turn_since: Option<Millis>,
    #[serde(default)]
    pub cleanup: Cleanup,
    /// The user released it ("I've fixed it"): it no longer waits out the grace of a declared wait.
    #[serde(default)]
    pub released: bool,
}

impl Paused {
    /// Whether the gate may bring it back now: a pause by the user waits for the user, and a
    /// declared wait for its grace.
    pub(crate) fn returns(&self, usage_clear: bool, now: Millis) -> bool {
        match &self.reason {
            Reason::ByUser => false,
            Reason::Usage => usage_clear,
            Reason::Declared { .. } => self.released || now - self.since >= DECLARED_GRACE,
            Reason::Memory => true,
        }
    }
}

/// The time between two resumes, doubled when a resume is followed by saturation and set back once
/// the Mac stays calm.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Step {
    pub step: Millis,
    pub last_resume: Option<Millis>,
    pub saturated_at: Option<Millis>,
}

impl Default for Step {
    fn default() -> Self {
        Self { step: STEP, last_resume: None, saturated_at: None }
    }
}

impl Step {
    /// The Mac went Tight or AtLimit: true when a recent resume is to blame, which doubles the step.
    pub(crate) fn saturated(&mut self, now: Millis) -> bool {
        self.saturated_at = Some(now);
        let blamed = self.last_resume.is_some_and(|at| now - at <= BLAME_WINDOW);
        if blamed {
            self.step = (self.step * 2).min(STEP_MAX);
        }
        blamed
    }

    pub(crate) fn calm(&mut self, now: Millis) {
        if self.step != STEP && self.saturated_at.is_none_or(|at| now - at >= CALM_RESET) {
            self.step = STEP;
        }
    }

    pub(crate) fn resumed(&mut self, now: Millis) {
        self.last_resume = Some(now);
    }

    pub(crate) fn next_at(&self) -> Option<Millis> {
        self.last_resume.map(|at| at + self.step)
    }
}

/// Everything about pauses the gate keeps across restarts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Pauses {
    /// By session: what the node machine holds, read when the gate is locked. An older daemon
    /// saved them in the gate's file; they are still read once, never written.
    #[serde(default, skip_serializing)]
    pub paused: BTreeMap<String, Paused>,
    pub step: Step,
    pub last_pause: Option<Millis>,
    /// Sessions admitted back while unreachable: told to carry on once they connect again.
    #[serde(default)]
    pub owed: BTreeMap<String, String>,
    /// Queued launches the user paused, with what the queue held for them.
    #[serde(default)]
    pub held: BTreeMap<TaskId, Held>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Held {
    pub body: serde_json::Value,
    pub origin: String,
    pub since: Millis,
}

/// A working node that could be paused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub session: String,
    pub task: TaskId,
    pub priority: TaskPriority,
    pub turn_since: Millis,
    pub waits_on_user: bool,
}

/// The node to pause, given every node working: none while only one works.
pub(crate) fn choose(working: &[Candidate]) -> Option<&Candidate> {
    if working.len() < 2 {
        return None;
    }
    working.iter().filter(|node| !node.waits_on_user).max_by_key(|node| (node.priority, node.turn_since, node.session.clone()))
}

/// What a node is told when it comes back.
pub(crate) fn resume_text(paused: &Paused, now: Millis) -> String {
    let at = crate::usage_pause::clock_text(paused.since, now, false);
    let cut = paused.cut.as_ref().map(|cut| format!(", in the middle of `{cut}`")).unwrap_or_default();
    match &paused.reason {
        Reason::Memory => format!("Alondra stopped you at {at} because the Mac ran out of memory{cut}. The Mac has been fine for 5 minutes: carry on with your task and run again what was cut."),
        Reason::Declared { .. } => format!("You waited for the machine since {at}. The Mac is fine again: carry on with your task; if it still cannot go on, call wait_for_machine again with the reason."),
        Reason::Usage => "Your Claude usage limit has reset. Carry on with what you were doing.".into(),
        Reason::ByUser => format!("{} resumed you; you were paused at {at}{cut}. Carry on with your task and run again what was cut.", crate::domain_messages::user_name()),
    }
}

/// The command a node's turn is running, as its mod reports it.
fn cut_of(domain: &DomainRuntime, session: &str) -> Option<String> {
    let reported = domain.live.mods.session(session)?;
    let tool = reported.tool?;
    Some(crate::node_reports::short_label(tool.summary.as_deref().unwrap_or(&tool.name)))
}

pub(crate) fn waits_on_user(domain: &DomainRuntime, state: &DomainState, task: &TaskId, session: &str) -> bool {
    state.tasks.get(task).is_some_and(|task| task.state == TaskState::NeedsYou)
        || crate::domain_holds::waits_on_user(state, session)
        || domain.live.node(session).is_some_and(|node| node.waiting_permission)
}

pub(crate) fn session_of(state: &DomainState, task: &TaskId) -> Option<String> {
    state.tasks.get(task)?.agent_id.as_ref().map(|agent| agent.0.clone())
}

/// The nodes working now that are not paused already.
pub(crate) fn candidates(domain: &DomainRuntime, state: &DomainState, pauses: &Pauses) -> Vec<Candidate> {
    crate::fleet_gate::busy_tasks(state).into_iter().filter_map(|task| {
        let session = session_of(state, &task)?;
        if pauses.paused.contains_key(&session) {
            return None;
        }
        let reported = domain.live.mods.session(&session);
        let turn_since = reported.and_then(|reported| reported.working_since.or(reported.started_at)).unwrap_or(0);
        Some(Candidate {
            waits_on_user: waits_on_user(domain, state, &task, &session),
            priority: state.tasks.get(&task).map_or(TaskPriority::Normal, |task| task.priority),
            session,
            task,
            turn_since,
        })
    }).collect()
}

/// Stops a node's turn and records it as paused.
pub(crate) fn stop(domain: &DomainRuntime, pauses: &mut Pauses, session: &str, task: TaskId, reason: Reason, now: Millis) {
    let cut = cut_of(domain, session);
    let turn_since = domain.live.mods.session(session).and_then(|reported| reported.working_since);
    if domain.live.pushes.interrupt(session).is_err() {
        log!("could not interrupt {session} to pause it: it is not connected");
    }
    log!("pausing {session} ({task}) for {reason:?}{}", cut.as_ref().map(|cut| format!(", cutting {cut}")).unwrap_or_default());
    pauses.paused.insert(session.to_owned(), Paused { task, reason, since: now, cut, turn_since, cleanup: Cleanup::Pending, released: false });
}

/// One look at the pauses: forgets the nodes that went on or ended, stops again a node the user
/// paused that started a turn without them, stops what interrupted nodes left running, and pauses
/// one more node while the Mac is AtLimit.
pub(crate) fn look(domain: &DomainRuntime, state: &DomainState, pauses: &mut Pauses, health: Health, now: Millis) {
    pauses.paused.retain(|session, paused| {
        let open = state.tasks.get(&paused.task).is_some_and(|task| !task.archived && !task.state.terminal() && !matches!(task.state, TaskState::Closing))
            && session_of(state, &paused.task).as_deref() == Some(session.as_str())
            && !domain.live.is_closed(session);
        let new_turn = domain.live.mods.session(session).and_then(|reported| reported.working_since)
            .filter(|since| *since > paused.since && Some(*since) != paused.turn_since);
        if open && paused.reason == Reason::ByUser && let Some(since) = new_turn {
            log!("{session} started a turn while the user paused it: stopping it again");
            if domain.live.pushes.interrupt(session).is_err() {
                log!("could not interrupt {session} to keep it paused: it is not connected");
            }
            paused.turn_since = Some(since);
            return true;
        }
        let went_on = new_turn.is_some();
        if !open || went_on {
            log!("{session} is no longer paused: {}", if open { "it went on" } else { "its task ended" });
        }
        open && !went_on
    });
    pauses.owed.retain(|session, _| pauses.paused.contains_key(session) || state.agents.contains_key(&alondra_core::domain::AgentId(session.clone())));
    clean_up(domain, pauses, now);
    match health {
        Health::AtLimit => {
            if pauses.last_pause.is_some_and(|at| now - at < PAUSE_EVERY) {
                return;
            }
            let working = candidates(domain, state, pauses);
            if let Some(victim) = choose(&working).cloned() {
                stop(domain, pauses, &victim.session, victim.task, Reason::Memory, now);
                pauses.last_pause = Some(now);
            }
        }
        Health::Fine => pauses.step.calm(now),
        Health::Tight => {}
    }
}

/// What the interruption left running in an interrupted node's shells: terminated after
/// `TERMINATE_AFTER`, killed after `KILL_AFTER`.
fn clean_up(domain: &DomainRuntime, pauses: &mut Pauses, now: Millis) {
    let due: Vec<(String, Cleanup)> = pauses.paused.iter()
        .filter(|(_, paused)| matches!(paused.reason, Reason::Memory | Reason::ByUser))
        .filter(|(_, paused)| match paused.cleanup {
            Cleanup::Pending => now - paused.since >= TERMINATE_AFTER,
            Cleanup::Terminated => now - paused.since >= KILL_AFTER,
            Cleanup::Done => false,
        })
        .map(|(session, paused)| (session.clone(), paused.cleanup)).collect();
    if due.is_empty() {
        return;
    }
    let table = crate::process_watch::snapshot();
    for (session, stage) in due {
        let left = domain.live.mods.session(&session).and_then(|reported| reported.peer_pid)
            .map(|pid| crate::process_watch::shell_descendants(&table, pid)).unwrap_or_default();
        let signal = if stage == Cleanup::Pending { libc::SIGTERM } else { libc::SIGKILL };
        if !left.is_empty() {
            log!("{session} was paused but {} process(es) of its shells still run: sending signal {signal}", left.len());
            crate::process_watch::signal(&left, signal);
        }
        if let Some(paused) = pauses.paused.get_mut(&session) {
            paused.cleanup = if stage == Cleanup::Pending && !left.is_empty() { Cleanup::Terminated } else { Cleanup::Done };
        }
    }
}

#[cfg(test)]
mod tests;
