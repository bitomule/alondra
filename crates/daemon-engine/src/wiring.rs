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
use alondra_engine_ports::sdk::{LastRequest, SdkEngine, SessionStart, StartKind, Work};
use alondra_engine_ports::node_close::{Answer, CloseOutcome, Sessions};
use alondra_engine_ports::{Registry, Runtime};
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_domain_model::{AgentId, DomainError, DomainState, NodeEvent, StartFacts, Task, TaskId};
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
    {
        alondra_foundation::durability::relax();
        alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules));
    }
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
    alondra_provider_claude::wire(runtime.ports());
    alondra_provider_codex::wire(runtime.ports());
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
    registry.provide::<dyn alondra_engine_ports::domain_api::RuntimeApi>(Arc::new(crate::runtime_api::Api));
    registry.provide::<dyn alondra_engine_ports::domain_api::LiveApi>(Arc::new(crate::server_api::Api));
    registry.provide::<dyn alondra_engine_ports::domain_api::AccountsApi>(Arc::new(crate::server_api::Api));
    registry.provide::<dyn alondra_engine_ports::domain_api::MessagesApi>(Arc::new(crate::server_api::Api));
    registry.provide::<dyn alondra_engine_ports::domain_api::ToolsApi>(Arc::new(crate::server_api::Api));
    registry.provide::<dyn alondra_engine_ports::domain_api::PullRequestsApi>(Arc::new(crate::server_api::Api));
    registry.provide::<dyn alondra_engine_ports::domain_api::McpApi>(Arc::new(crate::server_api::Api));
    registry.provide::<dyn alondra_provider_contract::SessionWorktrees>(Arc::new(alondra_provider_contract::EveryProvider(vec![
        Arc::new(alondra_provider_claude::SubagentNotes { claude_home: crate::watcher::claude_home }),
        Arc::new(alondra_provider_codex::NoSessionWorktrees),
    ])));
}

/// What the orchestrator asks of the engine.
pub struct Engine;

impl OrchestratorEngine for Engine {
    fn select_account(&self, runtime: &dyn Runtime, home: &Path, intent: &TaskSelection, now: Millis) -> Result<SelectionDecision, DomainError> {
        crate::accounts::select_for_launch(runtime, home, intent, now)
    }

    fn registered_accounts(&self, home: &Path, provider: Provider) -> Vec<AccountId> {
        crate::accounts::registered_provider(home, provider)
    }

    fn log_account_choice(&self, runtime: &dyn Runtime, what: &str, account: &AccountKey, now: Millis) {
        crate::accounts::log_choice(runtime, what, account, now);
    }

    fn account_wait<'a>(&self, runtime: &'a dyn Runtime) -> &'a AccountWait {
        &runtime.live().account_wait
    }

    fn follow_usage(&self, runtime: &dyn Runtime, now: Millis) {
        crate::usage_probe::follow(runtime, now);
    }

    fn reset_after_limit(&self, runtime: &dyn Runtime, account: &AccountId, now: Millis) -> Option<Millis> {
        runtime.live().measured.reset_after_limit(account, now)
    }

    fn usage_pause(&self, runtime: &dyn Runtime) -> Option<Exhausted> {
        runtime.live().usage_pause.current()
    }

    fn usage_changes(&self, runtime: &dyn Runtime) -> watch::Receiver<Option<Exhausted>> {
        runtime.live().usage_pause.subscribe()
    }

    fn settle_overdue(&self, runtime: &dyn Runtime, now: Millis) {
        crate::obligation_watch::pass(runtime, &mut runtime.live().obligations.lock_or_recover("the obligation watch"), now);
    }

    fn render_message(&self, state: &DomainState, message: &OutboxMessage) -> String {
        crate::domain_transport::render(state, message)
    }

    fn last_words(&self, runtime: &dyn Runtime, agent: &AgentId) -> Option<String> {
        runtime.live().mods.session(&agent.0).and_then(|reported| reported.answer).map(|answer| answer.text)
    }
}

/// What the admission gate asks of the engine.
impl AdmissionEngine for Engine {
    fn report(&self, runtime: &dyn Runtime, session: &str) -> Option<NodeReport> {
        let reported = runtime.live().mods.session(session)?;
        let running = reported.tool.map(|tool| crate::node_reports::short_label(tool.summary.as_deref().unwrap_or(&tool.name)));
        Some(NodeReport { working_since: reported.working_since, peer_pid: reported.peer_pid, running })
    }

    fn turn(&self, runtime: &dyn Runtime, session: &str) -> Option<u64> {
        runtime.live().node(session).map(|node| node.turn)
    }

    fn interrupt(&self, runtime: &dyn Runtime, session: &str) -> bool {
        runtime.live().pushes.interrupt(session).is_ok()
    }

    fn is_closed(&self, runtime: &dyn Runtime, session: &str) -> bool {
        runtime.live().is_closed(session)
    }

    fn is_launching(&self, runtime: &dyn Runtime, task: &TaskId) -> bool {
        runtime.live().is_launching(&task.0)
    }

    fn usage_paused(&self, runtime: &dyn Runtime) -> bool {
        runtime.live().usage_pause.is_active()
    }

    fn api_watch<'a>(&self, runtime: &'a dyn Runtime) -> &'a ApiWatch {
        &runtime.live().api_watch
    }

    fn usage_ready(&self, state: &DomainState, task: &TaskId, since: Millis, now: Millis) -> bool {
        crate::usage_pause::task_usage_ready(state, task, since, now)
    }

    fn tell_node(&self, runtime: &dyn Runtime, task: &TaskId, text: &str, command: &str) -> Result<(), String> {
        crate::domain_messages::tell_sdk_node(runtime, task, crate::node_push::ALONDRA, text, command, false).map_err(|error| error.to_string())
    }

    fn tell_session(&self, runtime: &dyn Runtime, session: &str, text: &str, as_user: bool) -> bool {
        crate::sdk_handover::tell(runtime, session, crate::node_push::ALONDRA, text, as_user).is_ok()
    }

    fn node_event(&self, runtime: &dyn Runtime, agent: &AgentId, event: NodeEvent) -> Result<bool, String> {
        crate::domain_agent_lifecycle::node_event(runtime, agent, event).map_err(|error| error.to_string())
    }

    fn launch<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, queued: serde_json::Value) -> Launched<'a> {
        Box::pin(async move {
            let body = crate::domain_orchestrator_tools::LaunchBody::from_queue(&queued)
                .map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
            let choices = |request: &alondra_domain_model::RequestId| crate::domain_orchestrator_tools::request_choices(runtime, request).ok().flatten();
            crate::domain_orchestrator_tools::launch_from(runtime, home, body, choices, crate::domain_orchestrator_tools::Origin::Admitted).await
        })
    }

    fn start_ended(&self, runtime: &dyn Runtime, task: &TaskId) -> Result<bool, String> {
        crate::start_failures::ended(runtime, task).map_err(|error| error.to_string())
    }

    fn start_failed(&self, runtime: &dyn Runtime, task: &TaskId, error: &DomainError, now: Millis) -> Result<(), String> {
        match crate::start_failures::problem_of(&*runtime.ports().accounts(), &error.code, &error.message, None) {
            Some(problem) => crate::start_failures::failed(runtime, task, problem, now).map_err(|error| error.to_string()),
            None => Ok(()),
        }
    }
}

/// What the owners of the SDK hosts and the launching of tasks ask of the engine.
impl SdkEngine for Engine {
    fn select_account(&self, runtime: &dyn Runtime, home: &Path, intent: &TaskSelection, denied_tools: &[String], now: Millis) -> Result<SelectionDecision, DomainError> {
        crate::accounts::select_for_launch_with_tools(runtime, home, intent, denied_tools, now)
    }

    fn choose_account(&self, runtime: &dyn Runtime, home: &Path, now: Millis) -> Option<AccountId> {
        crate::accounts::choose_for_launch(runtime, home, now)
    }

    fn require_initial_check(&self, runtime: &dyn Runtime) -> Result<(), DomainError> {
        crate::accounts::require_initial_check(runtime)
    }

    fn waits_for_session(&self, runtime: &dyn Runtime) -> bool {
        runtime.live().usage_pause.waits_for_session()
    }

    fn begin_launch(&self, runtime: &dyn Runtime, task: &TaskId) -> bool {
        runtime.live().begin_launch(&task.0)
    }

    fn end_launch(&self, runtime: &dyn Runtime, task: &TaskId) {
        runtime.live().end_launch(&task.0);
    }

    fn launch_planned<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, body: serde_json::Value) -> Launched<'a> {
        Box::pin(async move {
            let body = crate::domain_orchestrator_tools::LaunchBody::from_queue(&body)
                .map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
            let choices = |request: &alondra_domain_model::RequestId| crate::domain_orchestrator_tools::request_choices(runtime, request).ok().flatten();
            crate::domain_orchestrator_tools::launch_from(runtime, home, body, choices, crate::domain_orchestrator_tools::Origin::Auto).await
        })
    }

    fn relaunch<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, task: &'a TaskId, by_itself: bool) -> Launched<'a> {
        use crate::domain_orchestrator_tools::Origin;
        let origin = if by_itself { Origin::Auto } else { Origin::Orchestrator };
        Box::pin(crate::domain_orchestrator_tools::relaunch_from(runtime, home, task, origin))
    }

    fn start_facts(&self, runtime: &dyn Runtime, state: &DomainState) -> Result<BTreeMap<TaskId, StartFacts>> {
        crate::start_failures::facts(runtime, state)
    }

    fn node(&self, runtime: &dyn Runtime, session: &str) -> Option<alondra_core::Node> {
        runtime.live().node(session)
    }

    fn node_removed(&self, runtime: &dyn Runtime, session: &str) {
        let live = runtime.live();
        live.mark_closed(session);
        live.apply(&alondra_core::Event::NodeRemoved { session_id: session.to_owned() });
    }

    fn close_interrupted_at(&self, runtime: &dyn Runtime, session: &str) -> Option<Millis> {
        runtime.live().close_interrupted_at(session)
    }

    fn set_close_interrupted(&self, runtime: &dyn Runtime, session: &str, at: Option<Millis>) {
        runtime.live().set_close_interrupted(session, at);
    }

    fn begin_watch(&self, runtime: &dyn Runtime, session: &str) -> bool {
        runtime.live().begin_watch(session)
    }

    fn end_watch(&self, runtime: &dyn Runtime, session: &str) {
        runtime.live().end_watch(session);
    }

    fn close_node<'a>(&'a self, runtime: &'a dyn Runtime, sessions: &'a dyn Sessions, agent: &'a AgentId, leave_if_not_listed: bool) -> Answer<'a, Result<CloseOutcome>> {
        Box::pin(crate::node_close::close_node(runtime, sessions, agent, leave_if_not_listed))
    }

    fn launched_by_alondra(&self, runtime: &dyn Runtime, agent: &AgentId) -> Result<bool> {
        runtime.launched_by_alondra(agent)
    }

    fn forget_ended_session(&self, runtime: &dyn Runtime, session: &str) -> Result<()> {
        runtime.forget_ended_session(session)
    }

    fn user_closed(&self, runtime: &dyn Runtime, session: &str) -> Result<()> {
        crate::node_exit::user_closed(runtime, session)
    }

    fn session_ended(&self, runtime: &dyn Runtime, agent: &AgentId) -> Result<()> {
        crate::domain_transport::session_ended(runtime, agent)
    }

    fn feed(&self, runtime: &dyn Runtime, provider: Provider, agent: &AgentId, observation: &alondra_provider_contract::OrderedObservation) {
        match provider {
            Provider::Claude => crate::sdk_live::feed(runtime, &runtime.live().mods, agent, observation),
            Provider::Codex => crate::codex_live::feed(runtime, &runtime.live().mods, agent, observation),
            // engine-sdk reads the Antigravity host's frames and passes agy's messages to `feed_antigravity`.
            Provider::Antigravity => {}
        }
    }

    fn feed_antigravity(&self, runtime: &dyn Runtime, agent: &AgentId, message: &serde_json::Value) {
        crate::antigravity_live::feed(&runtime.live().mods, agent, message);
    }

    fn host_ended(&self, runtime: &dyn Runtime, agent: &AgentId) {
        runtime.live().mods.sdk_ended(&agent.0, alondra_core::now_millis());
    }

    fn restore_codex_scope(&self, runtime: &dyn Runtime, task: &TaskId, agent: &AgentId, checkpoint: &[u8]) {
        let Some(scope) = alondra_provider_codex::live_scope(checkpoint) else { return };
        runtime.contexts().forget(task);
        runtime.live().mods.restore_codex_scope(&agent.0, scope, alondra_core::now_millis());
    }

    fn note_output(&self, runtime: &dyn Runtime, task: &TaskId) {
        runtime.live().outputs.lock_or_recover("the node outputs").insert(task.clone(), alondra_core::now_millis());
    }

    fn context_launched(&self, runtime: &dyn Runtime, task: &TaskId, kind: StartKind) {
        runtime.contexts().launched(task, kind);
    }

    fn context_handed(&self, runtime: &dyn Runtime, task: &TaskId, at: Millis) {
        runtime.contexts().handed(task, at);
    }

    fn context_observed(&self, runtime: &dyn Runtime, task: &TaskId, provider: Provider, observation: &alondra_provider_contract::OrderedObservation, at: Millis) -> Option<SessionStart> {
        let event = match provider {
            Provider::Claude => alondra_provider_claude::context_event(observation)?,
            // Codex live scope binds current root thread, turn and native model before telemetry.
            Provider::Codex | Provider::Antigravity => return None,
        };
        let start = runtime.contexts().observed(task, event, at)?;
        Some(SessionStart { kind: start.kind, requested_at: start.requested_at, since_previous: start.since_previous, tokens: start.tokens,
            written_1h: start.cached.written_1h, written_5m: start.cached.written_5m, read: start.cached.read })
    }

    fn forget_context(&self, runtime: &dyn Runtime, task: &TaskId) {
        runtime.contexts().forget(task);
    }

    fn last_request(&self, runtime: &dyn Runtime, task: &TaskId) -> Option<LastRequest> {
        runtime.contexts().last_request(task)
    }

    fn worktree_ready<'a>(&'a self, runtime: &'a dyn Runtime, path: &'a str) -> Work<'a, Result<()>> {
        Box::pin(crate::task_worktrees::ready(runtime, path))
    }

    fn worktree_heir(&self, runtime: &dyn Runtime, task: &Task, path: &str) -> Result<Option<TaskId>> {
        crate::task_worktrees::heir(runtime, task, path)
    }

    fn may_handle_worktree(&self, runtime: &dyn Runtime, path: &str) -> std::result::Result<(), String> {
        crate::task_worktrees::may_handle(runtime, path)
    }

    fn worktree_leftovers<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Work<'a, Vec<(String, String)>> {
        Box::pin(crate::task_worktrees::leftovers(runtime, task))
    }

    fn release_worktrees<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Work<'a, Result<Vec<(String, String)>>> {
        Box::pin(crate::task_worktrees::release_all(runtime, task))
    }

    fn sweep_cancelled_worktrees<'a>(&'a self, runtime: &'a dyn Runtime) -> Work<'a, Result<()>> {
        Box::pin(crate::cancelled_worktrees::sweep(runtime))
    }

    fn sweep_worktree_shells<'a>(&'a self, runtime: &'a dyn Runtime) -> Work<'a, Result<usize>> {
        Box::pin(crate::task_worktrees::sweep_shells(runtime))
    }

    fn release_cancelled_worktrees<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Work<'a, Result<()>> {
        Box::pin(crate::cancelled_worktrees::release_cancelled(runtime, task))
    }

    fn apply_cancelled_worktree_answers<'a>(&'a self, runtime: &'a dyn Runtime) -> Work<'a, Result<()>> {
        Box::pin(crate::cancelled_worktrees::apply_answers(runtime))
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
