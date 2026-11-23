//! What the orchestrator keeps for as long as the runtime lives, and what it asks of the rest of
//! the engine through the port of engine-ports.
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_domain_rules::{AgentId, DomainError, DomainState};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use alondra_engine_kernel::domain_store::OutboxMessage;
use alondra_engine_ports::orchestrator::{AccountWait, Exhausted, OrchestratorEngine};
use std::collections::BTreeSet;
use std::path::Path;
use crate::locks::Poisonable as _;
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
    /// Held around each write of `orchestrator.json`, and around the whole read, change and write
    /// of the writers that change one field (the host's start, the user's cancel): one writer at
    /// a time. Recovered after panic: it guards no value, only the file.
    pub record: Mutex<()>,
    pub injected: Injected,
}

type Hook = Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>;
type Fault = Mutex<Option<Arc<dyn Fn(&'static str) -> std::io::Result<()> + Send + Sync>>>;

/// What a test injects into the orchestrator of one runtime, at run time: a failure at a named
/// write, and a look at moments it cannot wait for otherwise. A daemon injects nothing.
#[derive(Default)]
pub struct Injected {
    write: Fault,
    spawned: Hook,
    draining: Hook,
}

impl Injected {
    /// Each write at a named point (`runtime_write`, `runtime_rename`, `owner_create`, `owner_write`,
    /// `owner_sync`, `owner_rename`, `owner_directory_sync`, `ack`) asks `fault` first, and fails
    /// with its error.
    pub fn fail_writes(&self, fault: impl Fn(&'static str) -> std::io::Result<()> + Send + Sync + 'static) {
        *self.write.lock_or_recover("the injected writes") = Some(Arc::new(fault));
    }

    /// `hook` hears the capability of each batch once its host started.
    pub fn on_spawn(&self, hook: impl Fn(&str) + Send + Sync + 'static) {
        *self.spawned.lock_or_recover("the injected spawns") = Some(Arc::new(hook));
    }

    /// `hook` hears each run whose admitted operations the batch is about to wait for.
    pub fn on_drain(&self, hook: impl Fn(&str) + Send + Sync + 'static) {
        *self.draining.lock_or_recover("the injected drains") = Some(Arc::new(hook));
    }

    pub(crate) fn write(&self, point: &'static str) -> std::io::Result<()> {
        let fault = self.write.lock_or_recover("the injected writes").clone();
        fault.map_or(Ok(()), |fault| fault(point))
    }

    pub(crate) fn spawned(&self, capability: &str) {
        let hook = self.spawned.lock_or_recover("the injected spawns").clone();
        if let Some(hook) = hook {
            hook(capability);
        }
    }

    pub(crate) fn draining(&self, run: &str) {
        let hook = self.draining.lock_or_recover("the injected drains").clone();
        if let Some(hook) = hook {
            hook(run);
        }
    }
}

alondra_engine_kernel::extension!(Orchestrator);

pub trait OrchestratorOf {
    fn orchestrator(&self) -> &Orchestrator;
    fn engine(&self) -> Engine<'_>;
}

impl<R: alondra_engine_ports::Runtime + ?Sized> OrchestratorOf for R {
    fn orchestrator(&self) -> &Orchestrator {
        alondra_engine_ports::extension(self.as_runtime())
    }

    fn engine(&self) -> Engine<'_> {
        let port = self.ports().port::<dyn OrchestratorEngine>().expect("the engine gives the runtime what the orchestrator asks of it");
        Engine { domain: self.as_runtime(), port }
    }
}

/// The rest of the engine, as the orchestrator of one runtime sees it.
pub struct Engine<'a> {
    domain: &'a dyn alondra_engine_ports::Runtime,
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
