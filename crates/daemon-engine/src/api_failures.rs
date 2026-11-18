//! A node whose turn a Claude API error ended (Claude Code's `StopFailure` hook) never waits in
//! nobody's hands. A transient error pauses it in the gate, which sends it back to its task once
//! the retry is due (`alondra_domain_rules::after_api_failure`); the usage limit waits for its
//! reset; while the API looks down for every node they all wait, one of them probing it, with no
//! card. A card asks the user only when trying again cannot help, or when the API answers other
//! nodes and this one still fails after every retry. The hook itself cannot make Claude go on
//! (code.claude.com/docs/en/hooks), so going on is always a message to the node.
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use alondra_domain_rules::{AgentId, InteractionPayload, NodePhase, START_RETRY, StartProblem};
use anyhow::Result;

/// The hooks that prove the model answered: it called a tool or ended its turn.
pub(crate) fn proves_an_answer(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PostToolBatch" | "Stop" | "SubagentStop")
}

pub use alondra_engine_ports::api::{ApiWatch, outage_probe, resume_text};

/// A turn ended normally: the API answers this node again.
pub(crate) fn turn_ended(domain: &DomainRuntime, agent: &AgentId) -> Result<()> {
    let store = domain.store.lock_or_exit("the domain store");
    if store.api_failures()?.get(agent).is_some_and(|streak| streak.card.is_none()) {
        store.set_api_failures(agent, None)?;
    }
    Ok(())
}

/// Carries out the user's answer on the card of a node Alondra stopped sending back: "try again
/// now" sends it back at once; any other end of the card (the task cancelled, the card gone)
/// only forgets the streak.
pub fn settle_cards(domain: &DomainRuntime) -> Result<()> {
    let streaks = domain.store.lock_or_exit("the domain store").api_failures()?;
    let state = domain.state()?;
    for (agent, streak) in streaks {
        let Some(card) = streak.card.as_ref().and_then(|card| state.interactions.get(card)) else { continue };
        if state.open(card) {
            continue;
        }
        let retried = !card.withdrawn && matches!(&card.payload, InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == START_RETRY);
        let live = state.node_phase(&agent).is_some_and(NodePhase::live) && alondra_engine_ports::admission::open_task_of(&state, &agent.0).is_some();
        if retried && live {
            let text = match serde_json::from_str::<StartProblem>(&card.body) {
                Ok(StartProblem::ApiFailed { error, detail, .. }) => resume_text(&error, detail.as_deref()),
                _ => resume_text("unknown", None),
            };
            log!("the user asked to try {} again after its Claude API errors", agent.0);
            let _ = crate::sdk_handover::tell(domain, &agent.0, crate::node_push::ALONDRA, &text, true);
        }
        domain.store.lock_or_exit("the domain store").set_api_failures(&agent, None)?;
    }
    Ok(())
}
