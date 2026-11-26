//! Paused nodes and bringing them back through the gate. Memory never pauses a working node: it
//! only holds what starts or comes back, since cutting a build that uses a lot of memory only makes
//! it run again.
//! A node is paused by the user, by its own wait for the machine, for Claude usage or after an API
//! error. A pause by the user stops it whole. A node of the Agent SDK ends its host, with its turn
//! and every process under it, since a command left running would wake Claude, which takes that
//! turn by itself, with no input a pause could hold. A node of `claude --bg` is interrupted, then
//! what its shells left running stops, and a turn it starts by itself while held is cut again. No
//! other pause cuts what a node runs. Paused nodes wait in the
//! gate's queue ahead of new starts of their priority and come back as starts do, one a minute,
//! when there is a slot and room. A pause an older daemon made for memory returns at once and
//! comes back the same way.
use crate::engine::EngineOf as _;
use alondra_engine_ports::Runtime;
use alondra_domain_model::{ApiRetry, DomainState, TaskId, TaskState};
use alondra_core::Millis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const TERMINATE_AFTER: Millis = 10_000;
const KILL_AFTER: Millis = 20_000;

pub use alondra_engine_ports::admission::{Cleanup, Held, Paused, Pauses, Reason};

/// What a node is told when it comes back.
pub(crate) fn resume_text(paused: &Paused, now: Millis) -> String {
    let at = alondra_engine_ports::usage::clock_text(paused.since, now, false);
    let cut = paused.cut.as_ref().map(|cut| format!(", in the middle of `{cut}`")).unwrap_or_default();
    match &paused.reason {
        Reason::Memory => format!("Alondra stopped you at {at} because the Mac ran out of memory{cut}. The Mac has room again: carry on with your task and run again what was cut."),
        Reason::Declared { .. } => format!("You waited for the machine since {at}. The Mac is fine again: carry on with your task; if it still cannot go on, call wait_for_machine again with the reason."),
        Reason::Usage => "Your usage limit has reset. Carry on with what you were doing.".into(),
        Reason::Disk => format!("Alondra held your next turn at {at} while the disk was nearly full. There is room again: carry on with your task."),
        Reason::ApiError(retry) => alondra_engine_ports::api::resume_text(&retry.error, retry.detail.as_deref()),
        Reason::ByUser => format!("{} resumed you; you were paused at {at}{cut}. Carry on with your task and run again what was cut.", alondra_core::config::user_name()),
    }
}

/// The command a node's turn is running, as its mod reports it.
fn cut_of(domain: &dyn Runtime, session: &str) -> Option<String> {
    domain.engine().report(session)?.running
}

pub(crate) fn session_of(state: &DomainState, task: &TaskId) -> Option<String> {
    state.tasks.get(task)?.agent_id.as_ref().map(|agent| agent.0.clone())
}

/// Whether `session` is a node that runs through the Agent SDK: its processes are its owner's to
/// stop, never a push's or the process table's.
pub(crate) fn through_sdk(domain: &dyn Runtime, session: &str) -> bool {
    domain.read().index().node(&alondra_domain_model::AgentId(session.to_owned())).is_some_and(|node| node.through_sdk)
}

/// Stops a node's turn and records it as paused. A node of the Agent SDK is only recorded here:
/// `hold_sdk_turn` stops it once the node machine holds the pause.
pub(crate) fn stop(domain: &dyn Runtime, pauses: &mut Pauses, session: &str, task: TaskId, reason: Reason, now: Millis) {
    if through_sdk(domain, session) {
        log!("pausing {session} ({task}) for {reason:?}");
        pauses.paused.insert(session.to_owned(), Paused { task, reason, since: now, cut: None, turn_since: None, cleanup: Cleanup::Done, released: false });
        return;
    }
    let cut = cut_of(domain, session);
    let turn_since = domain.engine().report(session).and_then(|reported| reported.working_since);
    if !domain.engine().interrupt(session) {
        log!("could not interrupt {session} to pause it: it is not connected");
    }
    log!("pausing {session} ({task}) for {reason:?}{}", cut.as_ref().map(|cut| format!(", cutting {cut}")).unwrap_or_default());
    pauses.paused.insert(session.to_owned(), Paused { task, reason, since: now, cut, turn_since, cleanup: Cleanup::Pending, released: false });
}

/// Stops a node of the Agent SDK the node machine holds paused: its turn, its host and every
/// process under it. What it goes on with when the pause ends is told first, and its owner holds
/// it until then: the interrupted turn ends with its next step known, and no card asks the user
/// how it goes on.
pub(crate) fn hold_sdk_turn(domain: &dyn Runtime, session: &str, paused: &Paused) {
    let text = resume_text(paused, paused.since);
    let command = format!("pause-{session}-{}", paused.since);
    if !domain.engine().tell_node(&paused.task, &text, &command) {
        log!("pausing {session}: what it goes on with was not recorded");
    }
    interrupt_sdk_node(domain, session);
    domain.hosts().halt(paused.task.clone(), alondra_domain_model::AgentId(session.to_owned()));
}

pub fn interrupt_sdk_node(runtime: &dyn alondra_engine_ports::Runtime, session: &str) {
    let state = runtime.state();
    let envelope = alondra_domain_model::CommandEnvelope {
        protocol_version: alondra_domain_model::PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("pause-{session}-{}", crate::auth::random_hex(8)).into(),
        expected_revision: state.revision,
        command: alondra_domain_model::Command::InterruptTurn { agent_id: alondra_domain_model::AgentId(session.to_owned()) },
    };
    if let Err(error) = runtime.system_command(envelope) {
        log!("pausing {session}: its turn was not interrupted ({error:?}); what it is told waits all the same");
    }
}

/// The nodes of the Agent SDK a pause that stops them holds, whose host still runs what it ran when
/// the pause came: one a new pause reached, or one a daemon that restarted adopted.
pub(crate) fn to_halt(domain: &dyn Runtime, state: &DomainState, pauses: &Pauses) -> Vec<(String, Paused)> {
    pauses.paused.iter()
        .filter(|(session, paused)| paused.reason.holds() && through_sdk(domain, session) && !state.host_ended_since_pause(&paused.task)
            && domain.hosts().owns(&alondra_domain_model::AgentId((*session).clone())))
        .map(|(session, paused)| (session.clone(), paused.clone())).collect()
}

/// One look at the pauses: forgets the nodes that went on or ended, stops again a node paused by
/// the user, or for memory by an older daemon, that started a turn by itself, and stops what
/// interrupted nodes left running. It never pauses a working node.
pub(crate) fn look(domain: &dyn Runtime, state: &DomainState, pauses: &mut Pauses, now: Millis) {
    pauses.paused.retain(|session, paused| {
        let open = state.tasks.get(&paused.task).is_some_and(|task| !task.archived && !task.state.terminal() && !matches!(task.state, TaskState::Closing))
            && session_of(state, &paused.task).as_deref() == Some(session.as_str())
            && !domain.engine().is_closed(session);
        let new_turn = domain.engine().report(session).and_then(|reported| reported.working_since)
            .filter(|since| *since > paused.since && Some(*since) != paused.turn_since);
        if open && paused.reason.holds() && let Some(since) = new_turn {
            log!("{session} started a turn while paused for {:?}: stopping it again", paused.reason);
            if !domain.engine().interrupt(session) {
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
    pauses.owed.retain(|session, _| pauses.paused.contains_key(session) || state.agents.contains_key(&alondra_domain_model::AgentId(session.clone())));
    pauses.resumed.retain(|session, _| pauses.paused.contains_key(session));
    clean_up(domain, pauses, now);
}

/// What the interruption left running in an interrupted node's shells: terminated after
/// `TERMINATE_AFTER`, killed after `KILL_AFTER`.
fn clean_up(domain: &dyn Runtime, pauses: &mut Pauses, now: Millis) {
    let due: Vec<(String, Cleanup)> = pauses.paused.iter()
        .filter(|(_, paused)| matches!(paused.reason, Reason::Memory | Reason::ByUser))
        .filter(|(session, _)| !through_sdk(domain, session))
        .filter(|(_, paused)| match paused.cleanup {
            Cleanup::Pending => now - paused.since >= TERMINATE_AFTER,
            Cleanup::Terminated => now - paused.since >= KILL_AFTER,
            Cleanup::Done => false,
        })
        .map(|(session, paused)| (session.clone(), paused.cleanup)).collect();
    if due.is_empty() {
        return;
    }
    let processes = domain.ports().processes();
    let table = processes.table();
    for (session, stage) in due {
        let left = domain.engine().report(&session).and_then(|reported| reported.peer_pid)
            .map(|pid| crate::process_watch::work_descendants(&table, pid)).unwrap_or_default();
        let signal = if stage == Cleanup::Pending { libc::SIGTERM } else { libc::SIGKILL };
        if !left.is_empty() {
            log!("{session} was paused but {} process(es) of its calls still run: sending signal {signal}", left.len());
            processes.signal(&left, signal);
        }
        if let Some(paused) = pauses.paused.get_mut(&session) {
            paused.cleanup = if stage == Cleanup::Pending && !left.is_empty() { Cleanup::Terminated } else { Cleanup::Done };
        }
    }
}

#[cfg(test)]
mod tests;
