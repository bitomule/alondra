//! What the engine gives the runtime it runs in: what the orchestrator, the gate and the SDK hosts
//! ask of it (`OrchestratorEngine`, `AdmissionEngine`, `SdkEngine`) and the subsystems of the engine
//! that hear of its commits. `alondrad` puts the whole engine together; the engine's own tests open
//! a runtime with only its part.
use crate::domain_runtime::{CommitObserver, DomainRuntime};
use crate::domain_store::OutboxMessage;
use crate::live::LiveOf as _;
use crate::locks::Poisonable as _;
use alondra_engine_ports::admission::{AdmissionEngine, NodeReport};
use alondra_engine_ports::api::ApiWatch;
use alondra_engine_ports::hosts::Launched;
use alondra_engine_ports::orchestrator::{AccountWait, Exhausted, OrchestratorEngine};
use alondra_engine_ports::sdk::{ClosedNode, LastRequest, SdkEngine, SessionStart, StartKind, Work};
use alondra_engine_ports::node_close::{Answer, CloseOutcome, Sessions};
use alondra_engine_ports::{Registry, Runtime};
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_domain_rules::{AgentId, DomainError, DomainState, NodeEvent, StartFacts, Task, TaskId};
use crate::contexts::ContextsOf as _;
use std::collections::BTreeMap;
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use alondra_engine_ports::journal::JournalFiles;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

/// The engine as its fixtures open it: as the test said on its thread with `open_with`, else, in the
/// engine's own tests, the engine alone with the doubles of the ports it does not own.
pub fn open(home: &Path) -> Result<DomainRuntime> {
    open_at(home, &crate::domain_store::active_path(home))
}

/// `open` at `database`, on the journal the test set (`alondra_engine_kernel::TEST_JOURNAL`).
pub fn open_at(home: &Path, database: &Path) -> Result<DomainRuntime> {
    #[cfg(test)]
    alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    open_on(home, database, alondra_engine_kernel::test_journal())
}

/// `open_at` on `files`.
pub fn open_on(home: &Path, database: &Path, files: &dyn JournalFiles) -> Result<DomainRuntime> {
    let open = TEST_OPEN.with(std::cell::Cell::get);
    #[cfg(test)]
    let open = open.or(Some(engine_alone as Opener));
    let open = open.expect("a test that opens the engine's fixtures says how on its thread, with wiring::open_with");
    open(home, database, files)
}

/// How a test opens a runtime at a home, its database and the journal it is kept in.
pub type Opener = fn(&Path, &Path, &dyn JournalFiles) -> Result<DomainRuntime>;

thread_local! {
    static TEST_OPEN: std::cell::Cell<Option<Opener>> = const { std::cell::Cell::new(None) };
}

/// How the engine's fixtures open runtimes on this thread: a crate above the engine opens them with
/// its observers and ports, as `alondrad` does when it puts the daemon together.
pub fn open_with(open: Opener) {
    TEST_OPEN.with(|current| current.set(Some(open)));
}

/// The engine alone, with its own observers and the ports it gives the orchestrator and the gate.
pub fn open_engine_at(home: &Path, database: &Path, files: &dyn JournalFiles) -> Result<DomainRuntime> {
    let runtime = DomainRuntime::open_observed(home, database, files, Some(held_cards()), vec![
        Box::new(crate::gate::PauseResume),
        Box::new(crate::sdk_live::EndedWaits),
        Box::new(crate::allow_always::ChosenAlways),
    ])?;
    wire(runtime.ports());
    Ok(runtime)
}

#[cfg(test)]
fn engine_alone(home: &Path, database: &Path, files: &dyn JournalFiles) -> Result<DomainRuntime> {
    let runtime = open_engine_at(home, database, files)?;
    alondra_engine_doubles::machine::wire(runtime.ports(), alondra_daemon_support::watcher::claude_home);
    runtime.ports().provide::<dyn alondra_engine_ports::admission::AdmissionGate>(Arc::new(alondra_engine_doubles::gate::OpenGate::default()));
    runtime.ports().provide::<dyn crate::hosts::SdkHosts>(Arc::new(alondra_engine_doubles::hosts::NoHosts));
    alondra_agent_runtime::wire(runtime.ports(), Default::default());
    Ok(runtime)
}

/// The decisions of the cards a node's mod holds.
pub fn held_cards() -> Arc<crate::domain_holds::HeldCards> {
    Arc::new(crate::domain_holds::HeldCards)
}

/// Gives the registry what the orchestrator asks of the engine, and the worktrees each provider
/// noted a task's sessions made.
pub fn wire(registry: &Registry) {
    registry.provide::<dyn OrchestratorEngine>(Arc::new(Engine));
    registry.provide::<dyn AdmissionEngine>(Arc::new(Engine));
    registry.provide::<dyn SdkEngine>(Arc::new(Engine));
    registry.provide::<dyn alondra_provider_contract::SessionWorktrees>(Arc::new(alondra_provider_contract::EveryProvider(vec![
        Arc::new(alondra_provider_claude::SubagentNotes { claude_home: crate::watcher::claude_home }),
        Arc::new(alondra_provider_codex::NoSessionWorktrees),
    ])));
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

    fn tell_node(&self, runtime: &dyn Runtime, task: &TaskId, text: &str, command: &str) -> Result<(), String> {
        crate::domain_messages::tell_sdk_node(DomainRuntime::of(runtime), task, crate::node_push::ALONDRA, text, command, false).map_err(|error| error.to_string())
    }

    fn tell_session(&self, runtime: &dyn Runtime, session: &str, text: &str, as_user: bool) -> bool {
        crate::sdk_handover::tell(DomainRuntime::of(runtime), session, crate::node_push::ALONDRA, text, as_user).is_ok()
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

/// What the owners of the SDK hosts and the launching of tasks ask of the engine.
impl SdkEngine for Engine {
    fn select_account(&self, runtime: &dyn Runtime, home: &Path, intent: &TaskSelection, denied_tools: &[String], now: Millis) -> Result<SelectionDecision, DomainError> {
        crate::accounts::select_for_launch_with_tools(DomainRuntime::of(runtime), home, intent, denied_tools, now)
    }

    fn choose_account(&self, runtime: &dyn Runtime, home: &Path, now: Millis) -> Option<AccountId> {
        crate::accounts::choose_for_launch(DomainRuntime::of(runtime), home, now)
    }

    fn require_initial_check(&self, runtime: &dyn Runtime) -> Result<(), DomainError> {
        crate::accounts::require_initial_check(DomainRuntime::of(runtime))
    }

    fn waits_for_session(&self, runtime: &dyn Runtime) -> bool {
        DomainRuntime::of(runtime).live().usage_pause.waits_for_session()
    }

    fn begin_launch(&self, runtime: &dyn Runtime, task: &TaskId) -> bool {
        DomainRuntime::of(runtime).live().begin_launch(&task.0)
    }

    fn end_launch(&self, runtime: &dyn Runtime, task: &TaskId) {
        DomainRuntime::of(runtime).live().end_launch(&task.0);
    }

    fn launch_planned<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, body: serde_json::Value) -> Launched<'a> {
        Box::pin(async move {
            let domain = DomainRuntime::of(runtime);
            let body = crate::domain_orchestrator_tools::LaunchBody::from_queue(&body)
                .map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
            let choices = |request: &alondra_domain_rules::RequestId| crate::domain_orchestrator_tools::request_choices(domain, request).ok().flatten();
            crate::domain_orchestrator_tools::launch_from(domain, home, body, choices, crate::domain_orchestrator_tools::Origin::Auto).await
        })
    }

    fn relaunch<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, task: &'a TaskId, by_itself: bool) -> Launched<'a> {
        use crate::domain_orchestrator_tools::Origin;
        let origin = if by_itself { Origin::Auto } else { Origin::Orchestrator };
        Box::pin(crate::domain_orchestrator_tools::relaunch_from(DomainRuntime::of(runtime), home, task, origin))
    }

    fn start_facts(&self, runtime: &dyn Runtime, state: &DomainState) -> Result<BTreeMap<TaskId, StartFacts>> {
        crate::start_failures::facts(DomainRuntime::of(runtime), state)
    }

    fn closed_task(&self, runtime: &dyn Runtime, task: &TaskId) -> Option<ClosedNode> {
        DomainRuntime::of(runtime).live().closed_task(&task.0)
    }

    fn remember_closed_task(&self, runtime: &dyn Runtime, task: &TaskId, node: ClosedNode) {
        DomainRuntime::of(runtime).live().remember_closed_task(&task.0, node);
    }

    fn forget_closed_task(&self, runtime: &dyn Runtime, task: &TaskId) {
        DomainRuntime::of(runtime).live().forget_closed_task(&task.0);
    }

    fn node(&self, runtime: &dyn Runtime, session: &str) -> Option<alondra_core::Node> {
        DomainRuntime::of(runtime).live().node(session)
    }

    fn node_cwd(&self, runtime: &dyn Runtime, session: &str) -> Option<String> {
        DomainRuntime::of(runtime).live().mods.session(session).and_then(|reported| reported.cwd)
    }

    fn node_removed(&self, runtime: &dyn Runtime, session: &str) {
        let live = DomainRuntime::of(runtime).live();
        live.mark_closed(session);
        live.apply(&alondra_core::Event::NodeRemoved { session_id: session.to_owned() });
    }

    fn close_interrupted_at(&self, runtime: &dyn Runtime, session: &str) -> Option<Millis> {
        DomainRuntime::of(runtime).live().close_interrupted_at(session)
    }

    fn set_close_interrupted(&self, runtime: &dyn Runtime, session: &str, at: Option<Millis>) {
        DomainRuntime::of(runtime).live().set_close_interrupted(session, at);
    }

    fn begin_watch(&self, runtime: &dyn Runtime, session: &str) -> bool {
        DomainRuntime::of(runtime).live().begin_watch(session)
    }

    fn end_watch(&self, runtime: &dyn Runtime, session: &str) {
        DomainRuntime::of(runtime).live().end_watch(session);
    }

    fn close_node<'a>(&'a self, runtime: &'a dyn Runtime, sessions: &'a dyn Sessions, agent: &'a AgentId, leave_if_not_listed: bool) -> Answer<'a, Result<CloseOutcome>> {
        Box::pin(crate::node_close::close_node(DomainRuntime::of(runtime), sessions, agent, leave_if_not_listed))
    }

    fn launched_by_alondra(&self, runtime: &dyn Runtime, agent: &AgentId) -> Result<bool> {
        DomainRuntime::of(runtime).launched_by_alondra(agent)
    }

    fn forget_ended_session(&self, runtime: &dyn Runtime, session: &str) -> Result<()> {
        DomainRuntime::of(runtime).forget_ended_session(session)
    }

    fn user_closed(&self, runtime: &dyn Runtime, session: &str) -> Result<()> {
        crate::node_exit::user_closed(DomainRuntime::of(runtime), session)
    }

    fn session_ended(&self, runtime: &dyn Runtime, agent: &AgentId) -> Result<()> {
        crate::domain_transport::session_ended(DomainRuntime::of(runtime), agent)
    }

    fn feed(&self, runtime: &dyn Runtime, provider: Provider, agent: &AgentId, observation: &alondra_provider_contract::OrderedObservation) {
        let domain = DomainRuntime::of(runtime);
        match provider {
            Provider::Claude => crate::sdk_live::feed(domain, &domain.live().mods, agent, observation),
            Provider::Codex => crate::codex_live::feed(domain, &domain.live().mods, agent, observation),
        }
    }

    fn host_ended(&self, runtime: &dyn Runtime, agent: &AgentId) {
        DomainRuntime::of(runtime).live().mods.sdk_ended(&agent.0, alondra_core::now_millis());
    }

    fn restore_codex_scope(&self, runtime: &dyn Runtime, task: &TaskId, agent: &AgentId, checkpoint: &[u8]) {
        let Some(scope) = alondra_provider_codex::live_scope(checkpoint) else { return };
        let domain = DomainRuntime::of(runtime);
        domain.contexts().forget(task);
        domain.live().mods.restore_codex_scope(&agent.0, scope, alondra_core::now_millis());
    }

    fn note_output(&self, runtime: &dyn Runtime, task: &TaskId) {
        DomainRuntime::of(runtime).live().outputs.lock_or_recover("the node outputs").insert(task.clone(), alondra_core::now_millis());
    }

    fn context_launched(&self, runtime: &dyn Runtime, task: &TaskId, kind: StartKind) {
        DomainRuntime::of(runtime).contexts().launched(task, kind);
    }

    fn context_handed(&self, runtime: &dyn Runtime, task: &TaskId, at: Millis) {
        DomainRuntime::of(runtime).contexts().handed(task, at);
    }

    fn context_observed(&self, runtime: &dyn Runtime, task: &TaskId, provider: Provider, observation: &alondra_provider_contract::OrderedObservation, at: Millis) -> Option<SessionStart> {
        let event = match provider {
            Provider::Claude => alondra_provider_claude::context_event(observation)?,
            // Codex live scope binds current root thread, turn and native model before telemetry.
            Provider::Codex => return None,
        };
        let start = DomainRuntime::of(runtime).contexts().observed(task, event, at)?;
        Some(SessionStart { kind: start.kind, requested_at: start.requested_at, since_previous: start.since_previous, tokens: start.tokens,
            written_1h: start.cached.written_1h, written_5m: start.cached.written_5m, read: start.cached.read })
    }

    fn forget_context(&self, runtime: &dyn Runtime, task: &TaskId) {
        DomainRuntime::of(runtime).contexts().forget(task);
    }

    fn last_request(&self, runtime: &dyn Runtime, task: &TaskId) -> Option<LastRequest> {
        DomainRuntime::of(runtime).contexts().last_request(task)
    }

    fn worktree_ready<'a>(&'a self, runtime: &'a dyn Runtime, path: &'a str) -> Work<'a, Result<()>> {
        Box::pin(crate::task_worktrees::ready(DomainRuntime::of(runtime), path))
    }

    fn worktree_heir(&self, runtime: &dyn Runtime, task: &Task, path: &str) -> Result<Option<TaskId>> {
        crate::task_worktrees::heir(DomainRuntime::of(runtime), task, path)
    }

    fn may_handle_worktree(&self, runtime: &dyn Runtime, path: &str) -> std::result::Result<(), String> {
        crate::task_worktrees::may_handle(DomainRuntime::of(runtime), path)
    }

    fn worktree_leftovers<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Work<'a, Vec<(String, String)>> {
        Box::pin(crate::task_worktrees::leftovers(DomainRuntime::of(runtime), task))
    }

    fn release_worktrees<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Work<'a, Result<Vec<(String, String)>>> {
        Box::pin(crate::task_worktrees::release_all(DomainRuntime::of(runtime), task))
    }

    fn sweep_cancelled_worktrees<'a>(&'a self, runtime: &'a dyn Runtime) -> Work<'a, Result<()>> {
        Box::pin(crate::cancelled_worktrees::sweep(DomainRuntime::of(runtime)))
    }

    fn sweep_worktree_shells<'a>(&'a self, runtime: &'a dyn Runtime) -> Work<'a, Result<usize>> {
        Box::pin(crate::task_worktrees::sweep_shells(DomainRuntime::of(runtime)))
    }

    fn release_cancelled_worktrees<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Work<'a, Result<()>> {
        Box::pin(crate::cancelled_worktrees::release_cancelled(DomainRuntime::of(runtime), task))
    }

    fn apply_cancelled_worktree_answers<'a>(&'a self, runtime: &'a dyn Runtime) -> Work<'a, Result<()>> {
        Box::pin(crate::cancelled_worktrees::apply_answers(DomainRuntime::of(runtime)))
    }
}

#[cfg(test)]
#[path = "wiring/owner_fixtures.rs"]
pub mod owner_fixtures;
#[cfg(test)]
#[path = "wiring/owner_tests.rs"]
mod owner_tests;
#[cfg(test)]
#[path = "wiring/load_tests.rs"]
mod load_tests;
#[cfg(test)]
#[path = "wiring/metrics_tests.rs"]
mod metrics_tests;
#[cfg(test)]
#[path = "wiring/opening_tests.rs"]
mod opening_tests;
