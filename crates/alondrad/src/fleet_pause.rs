//! Paused nodes and bringing them back through the gate. Memory never pauses a working node: it
//! only holds new starts, since cutting a build that uses a lot of memory only makes it run again.
//! A node is paused by the user, by its own wait for the machine, for Claude usage or after an API
//! error. A pause by the user interrupts the node's turn and then stops what its shells left
//! running; a turn the node starts by itself while held is cut again. Paused nodes wait in the
//! gate's queue ahead of new starts of their priority and come back as starts do, one a minute,
//! when there is a slot and room. A pause an older daemon made for memory returns at once and
//! comes back the same way. See docs/fleet-pacing.md §7.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{ApiRetry, DomainState, TaskId, TaskState};
use alondra_core::Millis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const TERMINATE_AFTER: Millis = 10_000;
const KILL_AFTER: Millis = 20_000;
/// A wait the Mac's gauges do not explain, such as a build queue, gets this long before it returns.
pub(crate) const DECLARED_GRACE: Millis = 5 * 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Reason {
    /// Only an older daemon paused for memory: such a node returns at once and comes back as soon
    /// as the gate admits it. Kept so the pauses it left can be read.
    Memory,
    /// The node said it waits for the machine.
    Declared { reason: String },
    Usage,
    ByUser,
    ApiError(ApiRetry),
}

impl Reason {
    /// A pause only Alondra or the user ends: a turn the node starts by itself is cut again and its
    /// tools are refused, since that turn is the node reacting to what the pause killed.
    pub(crate) fn holds(&self) -> bool {
        matches!(self, Reason::ByUser | Reason::Memory)
    }
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
    /// Whether the gate may bring it back now: a pause by the user waits for the user, a declared
    /// wait for its grace, and an API error for its retry or for the model to answer another node.
    pub(crate) fn returns(&self, usage_clear: bool, api_answered: bool, now: Millis) -> bool {
        match &self.reason {
            Reason::ByUser => false,
            Reason::Usage => usage_clear,
            Reason::ApiError(retry) => api_answered || now >= retry.at,
            Reason::Declared { .. } => self.released || now - self.since >= DECLARED_GRACE,
            Reason::Memory => true,
        }
    }
}

/// Everything about pauses the gate keeps across restarts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Pauses {
    /// By session: what the node machine holds, read when the gate is locked. An older daemon
    /// saved them in the gate's file; they are still read once, never written.
    #[serde(default, skip_serializing)]
    pub paused: BTreeMap<String, Paused>,
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

/// What a node is told when it comes back.
pub(crate) fn resume_text(paused: &Paused, now: Millis) -> String {
    let at = crate::usage_pause::clock_text(paused.since, now, false);
    let cut = paused.cut.as_ref().map(|cut| format!(", in the middle of `{cut}`")).unwrap_or_default();
    match &paused.reason {
        Reason::Memory => format!("Alondra stopped you at {at} because the Mac ran out of memory{cut}. The Mac has room again: carry on with your task and run again what was cut."),
        Reason::Declared { .. } => format!("You waited for the machine since {at}. The Mac is fine again: carry on with your task; if it still cannot go on, call wait_for_machine again with the reason."),
        Reason::Usage => "Your Claude usage limit has reset. Carry on with what you were doing.".into(),
        Reason::ApiError(retry) => crate::api_failures::resume_text(&retry.error, retry.detail.as_deref()),
        Reason::ByUser => format!("{} resumed you; you were paused at {at}{cut}. Carry on with your task and run again what was cut.", crate::domain_messages::user_name()),
    }
}

/// The command a node's turn is running, as its mod reports it.
fn cut_of(domain: &DomainRuntime, session: &str) -> Option<String> {
    let reported = domain.live.mods.session(session)?;
    let tool = reported.tool?;
    Some(crate::node_reports::short_label(tool.summary.as_deref().unwrap_or(&tool.name)))
}

pub(crate) fn session_of(state: &DomainState, task: &TaskId) -> Option<String> {
    state.tasks.get(task)?.agent_id.as_ref().map(|agent| agent.0.clone())
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

/// One look at the pauses: forgets the nodes that went on or ended, stops again a node paused by
/// the user, or for memory by an older daemon, that started a turn by itself, and stops what
/// interrupted nodes left running. It never pauses a working node.
pub(crate) fn look(domain: &DomainRuntime, state: &DomainState, pauses: &mut Pauses, now: Millis) {
    pauses.paused.retain(|session, paused| {
        let open = state.tasks.get(&paused.task).is_some_and(|task| !task.archived && !task.state.terminal() && !matches!(task.state, TaskState::Closing))
            && session_of(state, &paused.task).as_deref() == Some(session.as_str())
            && !domain.live.is_closed(session);
        let new_turn = domain.live.mods.session(session).and_then(|reported| reported.working_since)
            .filter(|since| *since > paused.since && Some(*since) != paused.turn_since);
        if open && paused.reason.holds() && let Some(since) = new_turn {
            log!("{session} started a turn while paused for {:?}: stopping it again", paused.reason);
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
