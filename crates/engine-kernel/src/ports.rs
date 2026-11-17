//! The traits through which crates of the engine that may not depend on each other call each other,
//! and the values they speak in. The runtime holds one implementation of each: the crate that
//! implements it provides it when the engine is put together ([`DomainRuntime::provide`]), and the
//! crate that needs it asks for it ([`DomainRuntime::port`]).
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::OutboxMessage;
use crate::locks::Poisonable as _;
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_domain_rules::{AgentId, DomainError, DomainState, TaskId};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use tokio::sync::watch;

pub mod admission;

/// What the orchestrator asks of the rest of the engine, which `daemon-engine` provides.
pub trait OrchestratorEngine: Send + Sync {
    /// The account and model a batch of the orchestrator runs with now.
    fn select_account(&self, domain: &DomainRuntime, home: &Path, intent: &TaskSelection, now: Millis) -> Result<SelectionDecision, DomainError>;
    /// Every account of `provider` the user registered, on or off.
    fn registered_accounts(&self, home: &Path, provider: Provider) -> Vec<AccountId>;
    /// Logs why `account` was chosen for `what`.
    fn log_account_choice(&self, domain: &DomainRuntime, what: &str, account: &AccountKey, now: Millis);
    /// Whether no work starts because no Claude account has a session, and why.
    fn account_wait<'a>(&self, domain: &'a DomainRuntime) -> &'a AccountWait;
    /// Brings the usage pause in line with the accounts' last readings.
    fn follow_usage(&self, domain: &DomainRuntime, now: Millis);
    /// When the limit `account` just reached resets, as its readings measured it.
    fn reset_after_limit(&self, domain: &DomainRuntime, account: &AccountId, now: Millis) -> Option<Millis>;
    /// The standing usage pause, if new work waits for one.
    fn usage_pause(&self, domain: &DomainRuntime) -> Option<Exhausted>;
    /// Every change of the usage pause from now on.
    fn usage_changes(&self, domain: &DomainRuntime) -> watch::Receiver<Option<Exhausted>>;
    /// Steps the daemon's one clock over what every live task is owed.
    fn settle_overdue(&self, domain: &DomainRuntime, now: Millis);
    /// The text a recipient reads for `message`.
    fn render_message(&self, state: &DomainState, message: &OutboxMessage) -> String;
    /// What `agent`'s node last said, as its mod reported it.
    fn last_words(&self, domain: &DomainRuntime, agent: &AgentId) -> Option<String>;
}

/// What the clock over what every task is owed asks of the orchestrator, which `engine-orchestrator`
/// provides.
pub trait OrchestratorAnswers: Send + Sync {
    /// Settles what the orchestrator owes the `due` tasks past its deadline.
    fn settle_answers(&self, domain: &DomainRuntime, state: &DomainState, due: &BTreeSet<TaskId>);
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exhausted {
    pub until: Millis,
    pub since: Millis,
    /// The user paused until tomorrow; Claude did not run out.
    #[serde(default)]
    pub by_user: bool,
    /// No provider account is available: `until` is no time at all, only an account becoming available ends the wait.
    #[serde(default)]
    pub no_session: bool,
}

/// Why no work starts for want of an account, while it waits for one. A holder that panics leaves it
/// whole, so it is recovered: one assignment or take each.
#[derive(Default)]
pub struct AccountWait(std::sync::Mutex<Option<String>>);

impl AccountWait {
    pub fn is_waiting(&self) -> bool {
        self.0.lock_or_recover("the account wait").is_some()
    }

    /// True when the wait starts now.
    pub fn start(&self) -> bool {
        let mut wait = self.0.lock_or_recover("the account wait");
        if wait.is_some() { return false; }
        *wait = Some(String::new());
        true
    }

    /// True when the wait starts now or its reason changed.
    pub fn wait_for(&self, reason: &str) -> bool {
        let mut wait = self.0.lock_or_recover("the account wait");
        if wait.as_deref() == Some(reason) { return false; }
        *wait = Some(reason.to_owned());
        true
    }

    pub fn reason(&self) -> Option<String> {
        self.0.lock_or_recover("the account wait").clone()
    }

    /// True when a standing wait ends now.
    pub fn end(&self) -> bool {
        self.0.lock_or_recover("the account wait").take().is_some()
    }
}
