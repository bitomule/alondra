//! Where the engine is put together: the domain runtime of `engine-kernel` opens with the decisions
//! of the cards a node's mod holds, with every subsystem that hears of its commits and with what
//! the orchestrator asks of the engine. The kernel itself knows none of them.
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

pub fn open(home: &Path) -> Result<DomainRuntime> {
    open_at(home, &crate::domain_database_migration::active_path(home))
}

pub fn open_at(home: &Path, database: &Path) -> Result<DomainRuntime> {
    let runtime = DomainRuntime::open_observed(home, database, Some(Arc::new(crate::domain_holds::HeldCards)), observers())?;
    runtime.provide::<dyn OrchestratorEngine>(Arc::new(Engine));
    #[cfg(any(test, feature = "test-hooks"))]
    if let Some(wire) = TEST_WIRING.get() {
        wire(&runtime);
    }
    Ok(runtime)
}

/// What the tests of a crate above the engine give every runtime the engine's fixtures open: that
/// crate's ports, as `alondrad` gives them when it puts the daemon together.
#[cfg(any(test, feature = "test-hooks"))]
pub static TEST_WIRING: std::sync::OnceLock<fn(&DomainRuntime)> = std::sync::OnceLock::new();

/// Every subsystem that hears of the runtime's commits, in the order they hear of each one.
fn observers() -> Vec<Box<dyn CommitObserver>> {
    vec![
        Box::new(crate::gate::PauseResume),
        Box::new(crate::sdk_live::EndedWaits),
        Box::new(crate::task_closer::Unattended),
        Box::new(crate::allow_always::ChosenAlways),
        Box::new(crate::sdk_owner::Retakes),
    ]
}

/// What the orchestrator asks of the engine.
struct Engine;

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
#[path = "wiring/system_command_tests.rs"]
mod system_command_tests;
#[cfg(test)]
#[path = "wiring/load_tests.rs"]
mod load_tests;
#[cfg(test)]
#[path = "wiring/metrics_tests.rs"]
mod metrics_tests;
