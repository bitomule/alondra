//! What the engine gives the runtime it runs in: what the orchestrator asks of it
//! (`OrchestratorEngine`) and the subsystems of the engine that hear of its commits. `engine-sdk`
//! puts the whole engine together; the engine's own tests open a runtime with only its part.
use crate::domain_runtime::{CommitObserver, DomainRuntime};
use crate::domain_store::OutboxMessage;
use crate::live::LiveOf as _;
use crate::locks::Poisonable as _;
use alondra_engine_ports::admission::{AdmissionEngine, NodeReport};
use alondra_engine_ports::api::ApiWatch;
use alondra_engine_ports::hosts::Launched;
use alondra_engine_ports::orchestrator::{AccountWait, Exhausted, OrchestratorEngine};
use alondra_engine_ports::{Registry, Runtime};
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_domain_rules::{AgentId, DomainError, DomainState, NodeEvent, TaskId};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

/// The engine as its tests open it: the engine's own observers and the orchestrator's port, or
/// the whole daemon's when a crate above it set `TEST_OPEN`.
#[cfg(any(test, feature = "test-hooks"))]
pub fn open(home: &Path) -> Result<DomainRuntime> {
    open_at(home, &crate::domain_store::active_path(home))
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
    alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    let runtime = DomainRuntime::open_observed(home, database, alondra_engine_kernel::test_journal(), Some(held_cards()), vec![
        Box::new(crate::gate::PauseResume),
        Box::new(crate::sdk_live::EndedWaits),
        Box::new(crate::allow_always::ChosenAlways),
    ])?;
    wire(runtime.ports());
    alondra_agent_runtime::wire(runtime.ports(), Default::default());
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

/// Gives the registry what the orchestrator asks of the engine.
pub fn wire(registry: &Registry) {
    registry.provide::<dyn OrchestratorEngine>(Arc::new(Engine));
    registry.provide::<dyn AdmissionEngine>(Arc::new(Engine));
}

/// What the orchestrator asks of the engine.
pub struct Engine;

impl OrchestratorEngine for Engine {
    fn select_account(&self, runtime: &dyn Runtime, home: &Path, intent: &TaskSelection, now: Millis) -> Result<SelectionDecision, DomainError> {
        let domain = DomainRuntime::of(runtime);
        crate::accounts::select_for_launch(domain, home, intent, now)
    }

    fn registered_accounts(&self, home: &Path, provider: Provider) -> Vec<AccountId> {
        crate::accounts::registered_provider(home, provider)
    }

    fn log_account_choice(&self, runtime: &dyn Runtime, what: &str, account: &AccountKey, now: Millis) {
        let domain = DomainRuntime::of(runtime);
        crate::accounts::log_choice(domain, what, account, now);
    }

    fn account_wait<'a>(&self, runtime: &'a dyn Runtime) -> &'a AccountWait {
        let domain = DomainRuntime::of(runtime);
        &domain.live().account_wait
    }

    fn follow_usage(&self, runtime: &dyn Runtime, now: Millis) {
        let domain = DomainRuntime::of(runtime);
        crate::usage_probe::follow(domain, now);
    }

    fn reset_after_limit(&self, runtime: &dyn Runtime, account: &AccountId, now: Millis) -> Option<Millis> {
        let domain = DomainRuntime::of(runtime);
        domain.live().measured.reset_after_limit(account, now)
    }

    fn usage_pause(&self, runtime: &dyn Runtime) -> Option<Exhausted> {
        let domain = DomainRuntime::of(runtime);
        domain.live().usage_pause.current()
    }

    fn usage_changes(&self, runtime: &dyn Runtime) -> watch::Receiver<Option<Exhausted>> {
        let domain = DomainRuntime::of(runtime);
        domain.live().usage_pause.subscribe()
    }

    fn settle_overdue(&self, runtime: &dyn Runtime, now: Millis) {
        let domain = DomainRuntime::of(runtime);
        crate::obligation_watch::pass(domain, &mut domain.live().obligations.lock_or_recover("the obligation watch"), now);
    }

    fn render_message(&self, state: &DomainState, message: &OutboxMessage) -> String {
        crate::domain_transport::render(state, message)
    }

    fn last_words(&self, runtime: &dyn Runtime, agent: &AgentId) -> Option<String> {
        let domain = DomainRuntime::of(runtime);
        domain.live().mods.session(&agent.0).and_then(|reported| reported.answer).map(|answer| answer.text)
    }
}

/// What the admission gate asks of the engine.
impl AdmissionEngine for Engine {
    fn report(&self, runtime: &dyn Runtime, session: &str) -> Option<NodeReport> {
        let reported = DomainRuntime::of(runtime).live().mods.session(session)?;
        let running = reported.tool.map(|tool| crate::node_reports::short_label(tool.summary.as_deref().unwrap_or(&tool.name)));
        Some(NodeReport { working_since: reported.working_since, peer_pid: reported.peer_pid, running })
    }

    fn turn(&self, runtime: &dyn Runtime, session: &str) -> Option<u64> {
        DomainRuntime::of(runtime).live().node(session).map(|node| node.turn)
    }

    fn interrupt(&self, runtime: &dyn Runtime, session: &str) -> bool {
        DomainRuntime::of(runtime).live().pushes.interrupt(session).is_ok()
    }

    fn is_closed(&self, runtime: &dyn Runtime, session: &str) -> bool {
        DomainRuntime::of(runtime).live().is_closed(session)
    }

    fn is_launching(&self, runtime: &dyn Runtime, task: &TaskId) -> bool {
        DomainRuntime::of(runtime).live().is_launching(&task.0)
    }

    fn usage_paused(&self, runtime: &dyn Runtime) -> bool {
        DomainRuntime::of(runtime).live().usage_pause.is_active()
    }

    fn api_watch<'a>(&self, runtime: &'a dyn Runtime) -> &'a ApiWatch {
        &DomainRuntime::of(runtime).live().api_watch
    }

    fn usage_ready(&self, state: &DomainState, task: &TaskId, since: Millis, now: Millis) -> bool {
        crate::usage_pause::task_usage_ready(state, task, since, now)
    }

    fn tell_node(&self, runtime: &dyn Runtime, task: &TaskId, text: &str, command: &str) -> bool {
        crate::domain_messages::tell_sdk_node(DomainRuntime::of(runtime), task, crate::node_push::ALONDRA, text, command, false).is_ok()
    }

    fn tell_session(&self, runtime: &dyn Runtime, session: &str, text: &str) -> bool {
        crate::sdk_handover::tell(DomainRuntime::of(runtime), session, crate::node_push::ALONDRA, text, true).is_ok()
    }

    fn node_event(&self, runtime: &dyn Runtime, agent: &AgentId, event: NodeEvent) -> Result<bool, String> {
        crate::domain_agent_lifecycle::node_event(DomainRuntime::of(runtime), agent, event).map_err(|error| error.to_string())
    }

    fn launch<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, queued: serde_json::Value) -> Launched<'a> {
        Box::pin(async move {
            let domain = DomainRuntime::of(runtime);
            let body = crate::domain_orchestrator_tools::LaunchBody::from_queue(&queued)
                .map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
            let choices = |request: &alondra_domain_rules::RequestId| crate::domain_orchestrator_tools::request_choices(domain, request).ok().flatten();
            crate::domain_orchestrator_tools::launch_from(domain, home, body, choices, crate::domain_orchestrator_tools::Origin::Admitted).await
        })
    }

    fn start_ended(&self, runtime: &dyn Runtime, task: &TaskId) -> Result<bool, String> {
        crate::start_failures::ended(DomainRuntime::of(runtime), task).map_err(|error| error.to_string())
    }

    fn start_failed(&self, runtime: &dyn Runtime, task: &TaskId, error: &DomainError, now: Millis) -> Result<(), String> {
        match crate::start_failures::problem_of(&*runtime.ports().accounts(), &error.code, &error.message, None) {
            Some(problem) => crate::start_failures::failed(DomainRuntime::of(runtime), task, problem, now).map_err(|error| error.to_string()),
            None => Ok(()),
        }
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
