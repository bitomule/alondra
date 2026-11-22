//! The provider-neutral form of what an adapter told the domain. Native identities become their
//! order of first appearance (turns T1, T2…, work W1…, continuations C1…, decision requests D1…,
//! and the cursors they were asked at E1…); Alondra's own input ids stay as the transcript names
//! them.
use alondra_agent_application::ObservedFact;
use alondra_domain::{RuntimeFact, Trigger};
use alondra_provider_contract::{TurnOutcome, UsageReport, UsageWindowKind};
use std::collections::BTreeMap;

/// What the harness itself says between the adapter's facts.
pub const HOST_ENDED: &str = "HostEnded";
const AVAILABLE: &str = "Available";

#[derive(Default)]
pub struct Canon {
    names: BTreeMap<(char, String), String>,
    counts: BTreeMap<char, usize>,
}

impl Canon {
    fn name(&mut self, kind: char, native: &str) -> String {
        let key = (kind, native.to_owned());
        if let Some(name) = self.names.get(&key) {
            return name.clone();
        }
        let count = self.counts.entry(kind).or_default();
        *count += 1;
        let name = format!("{kind}{count}");
        self.names.insert(key, name.clone());
        name
    }

    pub fn fact(&mut self, fact: &ObservedFact) -> String {
        match fact {
            ObservedFact::Runtime(fact) => self.runtime(fact),
            ObservedFact::ContinuationRequested { work, .. } => format!("ContinuationRequested({})", self.name('W', work.as_str())),
            ObservedFact::TurnEnded { turn, outcome } => format!("TurnEnded({}, {})", self.name('T', turn.as_str()), outcome_text(outcome)),
            ObservedFact::InputDropped { input } => format!("InputDropped({})", input.as_str()),
            ObservedFact::Reconciled { idle, live } => {
                let live: Vec<String> = live.iter().map(|work| self.name('W', work.as_str())).collect();
                format!("Reconciled({}, live=[{}])", if *idle { "idle" } else { "busy" }, live.join(", "))
            }
            ObservedFact::ModelAnswered => "ModelAnswered".into(),
            ObservedFact::Asked { request, epoch, kind, .. } => {
                format!("Asked({}, {}, {kind:?})", self.name('D', request), self.name('E', &epoch.to_string()))
            }
            ObservedFact::Settled { epoch, answered, continued } => format!(
                "Settled({}, {}, {})",
                self.name('E', &epoch.to_string()),
                if *answered { "answered" } else { "unanswered" },
                if *continued { "continued" } else { "ended" }
            ),
            ObservedFact::Usage(report) => usage_text(report),
            ObservedFact::Worktree { subagent, .. } => format!("Worktree(subagent={subagent})"),
            ObservedFact::SubagentWorktree { .. } => "SubagentWorktree".into(),
            ObservedFact::PullRequestsChanged => "PullRequestsChanged".into(),
            ObservedFact::McpServers { servers } => format!("McpServers({})", servers.len()),
            ObservedFact::McpSignIn { server, .. } => format!("McpSignIn({server})"),
            ObservedFact::McpToolFailed => "McpToolFailed".into(),
        }
    }

    fn runtime(&mut self, fact: &RuntimeFact) -> String {
        match fact {
            RuntimeFact::ResourceAvailable => AVAILABLE.into(),
            RuntimeFact::TurnStarted { turn, trigger } => {
                let turn = self.name('T', turn.as_str());
                match trigger {
                    Trigger::Input(input) => format!("TurnStarted({turn}, {})", input.as_str()),
                    Trigger::OwnedContinuation(continuation) => format!("TurnResumed({turn}, {})", self.name('C', continuation.as_str())),
                    Trigger::Unprompted => format!("TurnUnprompted({turn})"),
                }
            }
            RuntimeFact::InputConsumed { input } => format!("InputConsumed({})", input.as_str()),
            RuntimeFact::InputHeld { input } => format!("InputHeld({})", input.as_str()),
            RuntimeFact::WorkStarted { turn, work } => format!("WorkStarted({}, {})", self.name('T', turn.as_str()), self.name('W', work.as_str())),
            RuntimeFact::WorkEnded { work, continuation } => format!("WorkEnded({}, {})", self.name('W', work.as_str()), self.name('C', continuation.as_str())),
            RuntimeFact::ContinuationConsumed { continuation } => format!("ContinuationConsumed({})", self.name('C', continuation.as_str())),
            other => format!("{other:?}"),
        }
    }
}

fn outcome_text(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed => "Completed".into(),
        TurnOutcome::Failed { code } => format!("Failed({code})"),
        TurnOutcome::ApiFailed(failure) => format!(
            "ApiFailed({}, status={}, resets={})",
            failure.error,
            failure.status.map_or("none".into(), |status| status.to_string()),
            failure.resets_at.map_or("none".into(), |at| at.to_string())
        ),
    }
}

fn usage_text(report: &UsageReport) -> String {
    let mut parts: Vec<String> = report
        .windows
        .iter()
        .map(|window| {
            let kind = match &window.kind {
                UsageWindowKind::Session => "session".to_owned(),
                UsageWindowKind::Weekly => "weekly".to_owned(),
                UsageWindowKind::Other(name) => name.clone(),
            };
            format!(
                "{kind} used={} {}resets={}",
                window.used,
                if window.exhausted { "exhausted " } else { "" },
                window.resets_at.map_or("none".into(), |at| at.to_string())
            )
        })
        .collect();
    if let Some(plan) = &report.plan {
        parts.push(format!("plan={plan}"));
    }
    format!("Usage({})", parts.join("; "))
}

/// Whether the provider is up is a fact about the provider, not about the conversation: one
/// provider says so as its thread opens, another as it starts the first input. Said before any
/// turn, it reads as the first fact; said later, it stays where it was said.
pub fn settle_availability(facts: &mut Vec<String>) {
    let Some(at) = facts.iter().position(|fact| fact == AVAILABLE) else { return };
    let first_turn = facts.iter().position(|fact| fact.starts_with("Turn") && !fact.starts_with("TurnEnded"));
    if first_turn.is_none_or(|turn| at < turn) {
        let available = facts.remove(at);
        facts.insert(0, available);
    }
}
