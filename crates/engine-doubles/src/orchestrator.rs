//! What the orchestrator asks of the rest of the engine, without that engine. An account is chosen
//! from the domain's own records of accounts, models and order; the usage pause is the one the
//! test sets, never a reading; and the clock over what every task is owed enforces nothing: what
//! the orchestrator owes has no deadline.
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use alondra_domain_rules::{AgentId, DomainError, DomainState};
use alondra_engine_ports::Runtime;
use alondra_engine_ports::extensions::extension;
use alondra_engine_ports::orchestrator::{AccountWait, Exhausted, OrchestratorEngine};
use alondra_engine_ports::outbox::OutboxMessage;
use std::path::Path;
use tokio::sync::watch;

pub struct Engine;

/// What the double keeps in each runtime: the pause and the wait the test sets.
pub struct Usage {
    pause: watch::Sender<Option<Exhausted>>,
    account_wait: AccountWait,
}

impl Default for Usage {
    fn default() -> Self {
        Self { pause: watch::channel(None).0, account_wait: AccountWait::default() }
    }
}

alondra_engine_ports::extension!(Usage);

/// The usage pause and the wait for an account of `runtime`, as the test sets them.
pub fn usage(runtime: &dyn Runtime) -> &Usage {
    extension::<Usage>(runtime)
}

impl Usage {
    /// No new work starts until `until`.
    pub fn exhaust(&self, until: Millis, since: Millis) {
        self.pause.send_replace(Some(Exhausted { until, since, by_user: false, no_session: false }));
    }

    /// The user paused new work until `until`.
    pub fn pause_by_user(&self, until: Millis, since: Millis) {
        self.pause.send_replace(Some(Exhausted { until, since, by_user: true, no_session: false }));
    }

    pub fn resume(&self) {
        self.pause.send_replace(None);
    }
}

impl OrchestratorEngine for Engine {
    fn select_account(&self, runtime: &dyn Runtime, _: &Path, intent: &TaskSelection, now: Millis) -> Result<SelectionDecision, DomainError> {
        let state = runtime.state();
        let eligible = alondra_core::usage::eligible_account_keys(&state.account_order, &state.accounts, &state.account_usage, now);
        if eligible.is_empty() {
            return Err(DomainError::new("account_unavailable", "No account is signed in with available quota"));
        }
        alondra_core::model_selection::resolve_selection(intent, &state.model_profiles, &state.model_catalogs, &eligible)
            .map_err(|reasons| DomainError::new("model_unavailable", reasons.join("; ")))
    }

    fn registered_accounts(&self, _: &Path, _: Provider) -> Vec<AccountId> {
        vec![AccountId::default_account()]
    }

    fn log_account_choice(&self, _: &dyn Runtime, _: &str, _: &AccountKey, _: Millis) {}

    fn account_wait<'a>(&self, runtime: &'a dyn Runtime) -> &'a AccountWait {
        &usage(runtime).account_wait
    }

    fn follow_usage(&self, _: &dyn Runtime, _: Millis) {}

    fn reset_after_limit(&self, _: &dyn Runtime, _: &AccountId, _: Millis) -> Option<Millis> {
        None
    }

    fn usage_pause(&self, runtime: &dyn Runtime) -> Option<Exhausted> {
        usage(runtime).pause.borrow().clone()
    }

    fn usage_changes(&self, runtime: &dyn Runtime) -> watch::Receiver<Option<Exhausted>> {
        usage(runtime).pause.subscribe()
    }

    /// No deadline of the engine is enforced in the double.
    fn settle_overdue(&self, _: &dyn Runtime, _: Millis) {}

    fn render_message(&self, state: &DomainState, message: &OutboxMessage) -> String {
        message.interaction_id.as_ref()
            .and_then(|id| state.interactions.get(&id.as_str().into()))
            .filter(|interaction| interaction.recipient == alondra_domain_rules::Recipient::Orchestrator)
            .map_or_else(|| message.message.clone(), |interaction| interaction.body.clone())
    }

    fn last_words(&self, _: &dyn Runtime, _: &AgentId) -> Option<String> {
        None
    }
}
