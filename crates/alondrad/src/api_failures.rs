//! A node whose turn a Claude API error ended (Claude Code's `StopFailure` hook) never waits in
//! nobody's hands. A transient error pauses it in the gate, which sends it back to its task once
//! the retry is due (`alondra_core::domain::after_api_failure`); the usage limit waits for its
//! reset; while the API looks down for every node they all wait, one of them probing it, with no
//! card. A card asks the user only when trying again cannot help, or when the API answers other
//! nodes and this one still fails after every retry. The hook itself cannot make Claude go on
//! (code.claude.com/docs/en/hooks), so going on is always a message to the node.
use crate::domain_runtime::DomainRuntime;
use crate::fleet_pause::{Paused, Reason};
use alondra_core::domain::{
    AfterApiFailure, AgentId, ApiErrorKind, ApiHealth, FRESH_TURN, OUTAGE_PROBE, InteractionId, InteractionPayload, NodeEvent, NodePhase,
    START_RETRY, StartProblem, after_api_failure, classify,
};
use alondra_core::Millis;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Failures of several nodes this close together, with no answer since, mean the API is down.
const OUTAGE_WINDOW: Millis = 10 * 60_000;

/// A node's turns an API error ended in a row, and the card raised once Alondra gave up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Streak {
    pub failures: u8,
    pub first_at: Millis,
    pub last_at: Millis,
    pub card: Option<InteractionId>,
}

/// When the model last answered each node, and when an API error last cut each one's turn.
#[derive(Default)]
pub(crate) struct ApiWatch {
    inner: Mutex<Watch>,
}

#[derive(Default)]
struct Watch {
    answered: BTreeMap<String, Millis>,
    failed: BTreeMap<String, Millis>,
    probed: Option<Millis>,
}

impl ApiWatch {
    fn lock(&self) -> std::sync::MutexGuard<'_, Watch> {
        self.inner.lock().expect("api watch poisoned")
    }

    pub(crate) fn answered(&self, session: &str, at: Millis) {
        let mut watch = self.lock();
        watch.answered.insert(session.to_owned(), at);
        watch.failed.remove(session);
    }

    /// A node waiting for the API was sent back to try it.
    pub(crate) fn probed(&self, at: Millis) {
        self.lock().probed = Some(at);
    }

    fn failed(&self, session: &str, at: Millis) {
        self.lock().failed.insert(session.to_owned(), at);
    }

    /// The model answered this node more than `span` after `at`: it got on with its task.
    fn worked_after(&self, session: &str, at: Millis, span: Millis) -> bool {
        self.lock().answered.get(session).is_some_and(|answered| *answered - at > span)
    }

    /// The model answered some node after `at`.
    pub(crate) fn answered_since(&self, at: Millis) -> bool {
        self.lock().answered.values().any(|answered| *answered > at)
    }

    /// What is known of the API for `session`, failing since `first_at`.
    fn health(&self, session: &str, first_at: Millis, now: Millis) -> ApiHealth {
        let watch = self.lock();
        if watch.answered.iter().any(|(other, at)| other != session && *at > first_at) {
            return ApiHealth::Proven;
        }
        let recent: Vec<Millis> = watch.failed.values().copied().filter(|at| now - at <= OUTAGE_WINDOW).collect();
        let earliest = recent.iter().min().copied().unwrap_or(now);
        match recent.len() >= 2 && !watch.answered.values().any(|at| *at > earliest) {
            true => ApiHealth::Down,
            false => ApiHealth::Unknown,
        }
    }

    pub(crate) fn down(&self, now: Millis) -> bool {
        self.health("", now, now) == ApiHealth::Down
    }
}

/// The hooks that prove the model answered: it called a tool or ended its turn.
pub(crate) fn proves_an_answer(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PostToolBatch" | "Stop" | "SubagentStop")
}

/// While the API looks down, the one node waiting for it that tries it next: the first whose
/// probe is due. The others wait for an answer.
pub(crate) fn outage_probe(paused: &BTreeMap<String, Paused>, watch: &ApiWatch, now: Millis) -> Option<String> {
    if watch.lock().probed.is_some_and(|at| now - at < OUTAGE_PROBE) {
        return None;
    }
    paused.iter()
        .filter_map(|(session, paused)| match &paused.reason {
            Reason::ApiError(retry) if retry.outage && now >= retry.at && !watch.answered_since(paused.since) => Some((retry.at, session)),
            _ => None,
        })
        .min()
        .map(|(_, session)| session.clone())
}

pub(crate) fn resume_text(error: &str, detail: Option<&str>) -> String {
    let what = detail.map(str::trim).filter(|detail| !detail.is_empty()).unwrap_or(error);
    format!("Your previous turn was cut by a Claude API error ({what}), not by anything you did. Carry on where you left off: check the state of your branch and of the work in progress, then continue your task.")
}

/// A turn ended normally: the API answers this node again.
pub(crate) fn turn_ended(domain: &DomainRuntime, agent: &AgentId) -> Result<()> {
    let store = domain.store.lock().expect("domain store poisoned");
    if store.api_failures()?.get(agent).is_some_and(|streak| streak.card.is_none()) {
        store.set_api_failures(agent, None)?;
    }
    Ok(())
}

/// A Claude API error ended `agent`'s turn: Alondra sends it back later, waits for the usage
/// limit or the API, or asks the user.
pub(crate) fn turn_failed(domain: &DomainRuntime, agent: &AgentId, payload: &Value, now: Millis) -> Result<()> {
    let error = crate::hook_text::str_field(payload, "error_type").unwrap_or_else(|| "unknown".into());
    let detail = crate::hook_text::str_field(payload, "error_details");
    let state = domain.state()?;
    let Some(phase) = state.node_phase(agent).cloned() else { return Ok(()) };
    let in_turn = matches!(phase, NodePhase::Working | NodePhase::Starting { .. });
    let task = crate::fleet_gate::open_task_of(&state, &agent.0);
    if !in_turn || task.is_none() {
        crate::domain_agent_lifecycle::node_event_at(domain, agent, NodeEvent::TurnFailed, now)?;
        return Ok(());
    }
    if classify(&error) == ApiErrorKind::Transient {
        domain.live.api_watch.failed(&agent.0, now);
    }
    let before = {
        let store = domain.store.lock().expect("domain store poisoned");
        store.api_failures()?.remove(agent)
            .filter(|streak| streak.card.is_none() && !domain.live.api_watch.worked_after(&agent.0, streak.last_at, FRESH_TURN))
            .unwrap_or(Streak { first_at: now, ..Streak::default() })
    };
    let attempt = before.failures.saturating_add(1);
    let health = domain.live.api_watch.health(&agent.0, before.first_at, now);
    let after = after_api_failure(&error, detail.as_deref(), attempt, health, now);
    log!("a Claude API error ({error}) ended the turn of {}: {after:?}", agent.0);
    match after {
        AfterApiFailure::Retry(retry) => {
            let failures = if retry.outage { before.failures } else { attempt };
            domain.store.lock().expect("domain store poisoned").set_api_failures(agent, Some(&Streak { failures, last_at: now, ..before }))?;
            hold(domain, agent, Reason::ApiError(retry), now)?;
        }
        AfterApiFailure::WaitUsage => hold(domain, agent, Reason::Usage, now)?,
        AfterApiFailure::Ask => {
            let task = task.expect("checked above");
            crate::start_failures::raise(domain, &task, StartProblem::ApiFailed { error, detail, attempts: attempt })?;
            let card = domain.state()?.open_start_problem(&task).map(|card| card.id.clone());
            domain.store.lock().expect("domain store poisoned").set_api_failures(agent, Some(&Streak { failures: 0, first_at: now, last_at: now, card }))?;
            crate::domain_agent_lifecycle::node_event_at(domain, agent, NodeEvent::TurnFailed, now)?;
        }
    }
    Ok(())
}

/// Pauses the node in the gate; a node the machine cannot pause (one waiting on the user) just
/// ends its turn.
fn hold(domain: &DomainRuntime, agent: &AgentId, reason: Reason, now: Millis) -> Result<()> {
    domain.live.gate.hold_back(domain, &agent.0, reason, now);
    if !matches!(domain.state()?.node_phase(agent), Some(NodePhase::Paused(_))) {
        crate::domain_agent_lifecycle::node_event_at(domain, agent, NodeEvent::TurnFailed, now)?;
    }
    Ok(())
}

/// Carries out the user's answer on the card of a node Alondra stopped sending back: "try again
/// now" sends it back at once; any other end of the card (the task cancelled, the card gone)
/// only forgets the streak.
pub(crate) fn settle_cards(domain: &DomainRuntime) -> Result<()> {
    let streaks = domain.store.lock().expect("domain store poisoned").api_failures()?;
    let state = domain.state()?;
    for (agent, streak) in streaks {
        let Some(card) = streak.card.as_ref().and_then(|card| state.interactions.get(card)) else { continue };
        if state.open(card) {
            continue;
        }
        let retried = !card.withdrawn && matches!(&card.payload, InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == START_RETRY);
        let live = state.node_phase(&agent).is_some_and(NodePhase::live) && crate::fleet_gate::open_task_of(&state, &agent.0).is_some();
        if retried && live {
            let text = match serde_json::from_str::<StartProblem>(&card.body) {
                Ok(StartProblem::ApiFailed { error, detail, .. }) => resume_text(&error, detail.as_deref()),
                _ => resume_text("unknown", None),
            };
            log!("the user asked to try {} again after its Claude API errors", agent.0);
            if crate::sdk_handover::tell(domain, &agent.0, crate::node_push::ALONDRA, &text, true).is_err() {
                domain.live.losses.report(&agent.0);
            }
        }
        domain.store.lock().expect("domain store poisoned").set_api_failures(&agent, None)?;
    }
    Ok(())
}
