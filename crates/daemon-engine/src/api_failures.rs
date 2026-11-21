//! A node whose turn a Claude API error ended (Claude Code's `StopFailure` hook) never waits in
//! nobody's hands. A transient error pauses it in the gate, which sends it back to its task once
//! the retry is due (`alondra_domain_rules::after_api_failure`); the usage limit waits for its
//! reset; while the API looks down for every node they all wait, one of them probing it, with no
//! card. A card asks the user only when trying again cannot help, or when the API answers other
//! nodes and this one still fails after every retry. The hook itself cannot make Claude go on
//! (code.claude.com/docs/en/hooks), so going on is always a message to the node.
use crate::locks::Poisonable as _;
use alondra_engine_ports::Runtime;
use alondra_domain_rules::AgentId;
use anyhow::Result;

/// The hooks that prove the model answered: it called a tool or ended its turn.
pub(crate) fn proves_an_answer(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PostToolBatch" | "Stop" | "SubagentStop")
}

pub use alondra_engine_ports::api::{ApiWatch, outage_probe, resume_text};

/// A turn ended normally: the API answers this node again.
pub(crate) fn turn_ended(domain: &dyn Runtime, agent: &AgentId) -> Result<()> {
    let store = domain.store().lock_or_exit("the domain store");
    if store.api_failures()?.get(agent).is_some_and(|streak| streak.card.is_none()) {
        store.set_api_failures(agent, None)?;
    }
    Ok(())
}
