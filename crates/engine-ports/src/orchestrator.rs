//! What the orchestrator and the rest of the engine ask of each other, and the values they speak in.
use crate::Runtime;
use crate::outbox::OutboxMessage;
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use alondra_domain_rules::{AgentId, DomainError, DomainState};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::PoisonError;
use tokio::sync::watch;

/// What the orchestrator asks of the rest of the engine, which `daemon-engine` provides.
pub trait OrchestratorEngine: Send + Sync {
    /// The account and model a batch of the orchestrator runs with now.
    fn select_account(&self, runtime: &dyn Runtime, home: &Path, intent: &TaskSelection, now: Millis) -> Result<SelectionDecision, DomainError>;
    /// Every account of `provider` the user registered, on or off.
    fn registered_accounts(&self, home: &Path, provider: Provider) -> Vec<AccountId>;
    /// Logs why `account` was chosen for `what`.
    fn log_account_choice(&self, runtime: &dyn Runtime, what: &str, account: &AccountKey, now: Millis);
    /// Whether no work starts because no Claude account has a session, and why.
    fn account_wait<'a>(&self, runtime: &'a dyn Runtime) -> &'a AccountWait;
    /// Brings the usage pause in line with the accounts' last readings.
    fn follow_usage(&self, runtime: &dyn Runtime, now: Millis);
    /// When the limit `account` just reached resets, as its readings measured it.
    fn reset_after_limit(&self, runtime: &dyn Runtime, account: &AccountId, now: Millis) -> Option<Millis>;
    /// The standing usage pause, if new work waits for one.
    fn usage_pause(&self, runtime: &dyn Runtime) -> Option<Exhausted>;
    /// Every change of the usage pause from now on.
    fn usage_changes(&self, runtime: &dyn Runtime) -> watch::Receiver<Option<Exhausted>>;
    /// Steps the daemon's one clock over what every live task is owed.
    fn settle_overdue(&self, runtime: &dyn Runtime, now: Millis);
    /// The text a recipient reads for `message`.
    fn render_message(&self, state: &DomainState, message: &OutboxMessage) -> String;
    /// What `agent`'s node last said, as its mod reported it.
    fn last_words(&self, runtime: &dyn Runtime, agent: &AgentId) -> Option<String>;
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
        self.0.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// True when the wait starts now.
    pub fn start(&self) -> bool {
        let mut wait = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if wait.is_some() { return false; }
        *wait = Some(String::new());
        true
    }

    /// True when the wait starts now or its reason changed.
    pub fn wait_for(&self, reason: &str) -> bool {
        let mut wait = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if wait.as_deref() == Some(reason) { return false; }
        *wait = Some(reason.to_owned());
        true
    }

    pub fn reason(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// True when a standing wait ends now.
    pub fn end(&self) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take().is_some()
    }
}
