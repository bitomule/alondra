//! What the engine gives the runtime it runs in: what the orchestrator asks of it
//! (`OrchestratorEngine`) and the subsystems of the engine that hear of its commits. `engine-sdk`
//! puts the whole engine together; the engine's own tests open a runtime with only its part.
use crate::domain_runtime::{CommitObserver, DomainRuntime};
use crate::domain_store::OutboxMessage;
use crate::live::LiveOf as _;
use crate::locks::Poisonable as _;
use crate::ports::{AccountWait, Exhausted, OrchestratorEngine};
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_domain_rules::{AgentId, DomainError, DomainState};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

/// The engine as its tests open it: the engine's own observers and the orchestrator's port, or
/// the whole daemon's when a crate above it set `TEST_OPEN`.
#[cfg(any(test, feature = "test-hooks"))]
pub fn open(home: &Path) -> Result<DomainRuntime> {
    open_at(home, &crate::domain_database_migration::active_path(home))
}

#[cfg(any(test, feature = "test-hooks"))]
pub fn open_at(home: &Path, database: &Path) -> Result<DomainRuntime> {
    match TEST_OPEN.get() {
        Some(open) => open(home, database),
        None => open_engine_at(home, database),
    }
}

/// The engine alone, with its own observers and the orchestrator's port.
#[cfg(any(test, feature = "test-hooks"))]
pub fn open_engine_at(home: &Path, database: &Path) -> Result<DomainRuntime> {
    let runtime = DomainRuntime::open_observed(home, database, Some(held_cards()), vec![
        Box::new(crate::gate::PauseResume),
        Box::new(crate::sdk_live::EndedWaits),
        Box::new(crate::allow_always::ChosenAlways),
    ])?;
    runtime.provide::<dyn OrchestratorEngine>(Arc::new(Engine));
    Ok(runtime)
}

/// How the tests of a crate above the engine open the runtime the engine's fixtures open: with
/// that crate's observers and ports, as `alondrad` does when it puts the daemon together.
#[cfg(any(test, feature = "test-hooks"))]
pub static TEST_OPEN: std::sync::OnceLock<fn(&Path, &Path) -> Result<DomainRuntime>> = std::sync::OnceLock::new();

/// The decisions of the cards a node's mod holds.
pub fn held_cards() -> Arc<crate::domain_holds::HeldCards> {
    Arc::new(crate::domain_holds::HeldCards)
}

/// What the orchestrator asks of the engine.
pub struct Engine;

impl OrchestratorEngine for Engine {
    fn select_account(&self, domain: &DomainRuntime, home: &Path, intent: &TaskSelection, now: Millis) -> Result<SelectionDecision, DomainError> {
        crate::accounts::select_for_launch(domain, home, intent, now)
    }

    fn registered_accounts(&self, home: &Path, provider: Provider) -> Vec<AccountId> {
        crate::accounts::registered_provider(home, provider)
    }

    fn log_account_choice(&self, domain: &DomainRuntime, what: &str, account: &AccountKey, now: Millis) {
        crate::accounts::log_choice(domain, what, account, now);
    }

    fn account_wait<'a>(&self, domain: &'a DomainRuntime) -> &'a AccountWait {
        &domain.live().account_wait
    }

    fn follow_usage(&self, domain: &DomainRuntime, now: Millis) {
        crate::usage_probe::follow(domain, now);
    }

    fn reset_after_limit(&self, domain: &DomainRuntime, account: &AccountId, now: Millis) -> Option<Millis> {
        domain.live().measured.reset_after_limit(account, now)
    }

    fn usage_pause(&self, domain: &DomainRuntime) -> Option<Exhausted> {
        domain.live().usage_pause.current()
    }

    fn usage_changes(&self, domain: &DomainRuntime) -> watch::Receiver<Option<Exhausted>> {
        domain.live().usage_pause.subscribe()
    }

    fn settle_overdue(&self, domain: &DomainRuntime, now: Millis) {
        crate::obligation_watch::pass(domain, &mut domain.live().obligations.lock_or_recover("the obligation watch"), now);
    }

    fn render_message(&self, state: &DomainState, message: &OutboxMessage) -> String {
        crate::domain_transport::render(state, message)
    }

    fn last_words(&self, domain: &DomainRuntime, agent: &AgentId) -> Option<String> {
        domain.live().mods.session(&agent.0).and_then(|reported| reported.answer).map(|answer| answer.text)
    }
}

#[cfg(any(test, feature = "test-hooks"))]
#[path = "wiring/owner_tests.rs"]
pub mod owner_tests;
#[cfg(test)]
#[path = "wiring/load_tests.rs"]
mod load_tests;
#[cfg(test)]
#[path = "wiring/metrics_tests.rs"]
mod metrics_tests;
