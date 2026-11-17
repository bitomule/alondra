//! What the orchestrator keeps for as long as the runtime lives, and what it asks of the rest of
//! the engine through the port of engine-ports.
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_domain_rules::{AgentId, DomainError, DomainState};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use alondra_engine_kernel::domain_runtime::DomainRuntime;
use alondra_engine_kernel::domain_store::OutboxMessage;
use alondra_engine_ports::orchestrator::{AccountWait, Exhausted, OrchestratorEngine};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Default)]
pub struct Orchestrator {
    /// Recovered after panic: each holder only reads or replaces one complete host lease.
    pub host: Mutex<Option<(String, alondra_engine_ports::agent_host::HostStop)>>,
    /// Retain a logical stop while storage is unavailable, until its finalization commits.
    /// Recovered after panic: one complete stop is assigned, cloned or cleared.
    pub stop: Mutex<Option<(crate::domain_orchestrator::Run, alondra_domain_rules::orchestrator_batch::BatchProblem)>>,
    /// Recovered after panic: each holder inserts or removes one completed-run marker.
    pub runs: Mutex<BTreeSet<String>>,
}

alondra_engine_kernel::extension!(Orchestrator);

pub trait OrchestratorOf {
    fn orchestrator(&self) -> &Orchestrator;
    fn engine(&self) -> Engine<'_>;
}

impl OrchestratorOf for DomainRuntime {
    fn orchestrator(&self) -> &Orchestrator {
        self.extension()
    }

    fn engine(&self) -> Engine<'_> {
        let port = self.ports().port::<dyn OrchestratorEngine>().expect("the engine gives the runtime what the orchestrator asks of it");
        Engine { domain: self, port }
    }
}

/// The rest of the engine, as the orchestrator of one runtime sees it.
pub struct Engine<'a> {
    domain: &'a DomainRuntime,
    port: Arc<dyn OrchestratorEngine>,
}

impl<'a> Engine<'a> {
    pub fn select_account(&self, home: &Path, intent: &TaskSelection, now: Millis) -> Result<SelectionDecision, DomainError> {
        self.port.select_account(self.domain, home, intent, now)
    }

    pub fn registered_accounts(&self, home: &Path, provider: Provider) -> Vec<AccountId> {
        self.port.registered_accounts(home, provider)
    }

    pub fn log_account_choice(&self, what: &str, account: &AccountKey, now: Millis) {
        self.port.log_account_choice(self.domain, what, account, now);
    }

    pub fn account_wait(&self) -> &'a AccountWait {
        self.port.account_wait(self.domain)
    }

    pub fn follow_usage(&self, now: Millis) {
        self.port.follow_usage(self.domain, now);
    }

    pub fn reset_after_limit(&self, account: &AccountId, now: Millis) -> Option<Millis> {
        self.port.reset_after_limit(self.domain, account, now)
    }

    pub fn usage_pause(&self) -> Option<Exhausted> {
        self.port.usage_pause(self.domain)
    }

    pub fn usage_changes(&self) -> watch::Receiver<Option<Exhausted>> {
        self.port.usage_changes(self.domain)
    }

    pub fn settle_overdue(&self, now: Millis) {
        self.port.settle_overdue(self.domain, now);
    }

    pub fn render_message(&self, state: &DomainState, message: &OutboxMessage) -> String {
        self.port.render_message(state, message)
    }

    pub fn last_words(&self, agent: &AgentId) -> Option<String> {
        self.port.last_words(self.domain, agent)
    }
}
