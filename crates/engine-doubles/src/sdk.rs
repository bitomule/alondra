//! What the SDK hosts' owners and the launching of tasks ask of the rest of the engine, for tests
//! that do not run that engine: the account and model a launch takes, whether a worktree is ready
//! and the last request the daemon timed come from a script the test writes; what the hosts report
//! about a node is taken and dropped. Every other question fails and names itself: a test that asks it needs the whole
//! engine (`engine_sdk_engine_test`).
use alondra_core::Millis;
use alondra_core::accounts::{AccountId, Provider};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use alondra_domain_model::{AgentId, DomainError, DomainState, StartFacts, Task, TaskId};
use alondra_engine_ports::hosts::Launched;
use alondra_engine_ports::node_close::{Answer, CloseOutcome, Sessions};
use alondra_engine_ports::sdk::{LastRequest, SdkEngine, SessionStart, StartKind, Work};
use alondra_engine_ports::{Registry, Runtime};
use alondra_provider_contract::OrderedObservation;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// What the engine answers, and what it was asked.
pub struct Script {
    /// What a launch with a model selection gets; without it the selection is resolved from the
    /// domain's own accounts, models and order.
    pub selection: Option<Result<SelectionDecision, DomainError>>,
    /// The Claude account a launch without a model selection takes: none when no account has usage
    /// left.
    pub account: Option<AccountId>,
    /// Whether no account has a session, so a launch waits for one.
    pub waits_for_session: bool,
    /// Why accounts are still being checked after startup, so no node starts yet.
    pub checking_accounts: Option<DomainError>,
    /// The worktrees whose storage is not reachable, with why.
    pub unready: BTreeMap<String, String>,
    /// The last request of each task the daemon timed: none unless the test says so.
    pub last_requests: BTreeMap<TaskId, LastRequest>,

    pub calls: Vec<&'static str>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            selection: None,
            account: Some(AccountId::default_account()),
            waits_for_session: false,
            checking_accounts: None,
            unready: BTreeMap::new(),
            last_requests: BTreeMap::new(),
            calls: vec![],
        }
    }
}

#[derive(Default)]
pub struct Engine {
    script: Mutex<Script>,
}

impl Engine {
    /// Gives `registry` this double as the engine the SDK hosts ask, and keeps it there for the test.
    pub fn wire(registry: &Registry) -> Arc<Engine> {
        let engine = Arc::new(Engine::default());
        registry.provide::<dyn SdkEngine>(engine.clone());
        registry.provide::<Engine>(engine.clone());
        engine
    }

    /// The double `wire` gave `runtime`.
    pub fn of(runtime: &dyn Runtime) -> Arc<Engine> {
        runtime.ports().port::<Engine>().expect("the test wired the SDK engine's double")
    }

    pub fn script(&self) -> MutexGuard<'_, Script> {
        self.script.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn asked(&self, call: &'static str) -> MutexGuard<'_, Script> {
        let mut script = self.script();
        script.calls.push(call);
        script
    }

    fn note(&self, call: &'static str) {
        self.script().calls.push(call);
    }
}

/// Also written straight to stderr: the hosts' owners ask from threads of their own, whose panic
/// the test harness keeps until the test ends, and the test it fails waits on that owner.
fn whole_engine(call: &str) -> ! {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "{call} needs the whole engine: this test belongs in engine_sdk_engine_test");
    panic!("{call} needs the whole engine: this test belongs in engine_sdk_engine_test, with the feature whole-engine")
}

impl SdkEngine for Engine {
    fn select_account(&self, runtime: &dyn Runtime, _: &Path, intent: &TaskSelection, _: &[String], now: Millis) -> Result<SelectionDecision, DomainError> {
        if let Some(answer) = self.asked("select_account").selection.clone() {
            return answer;
        }
        let state = runtime.state();
        let eligible = alondra_core::usage::eligible_account_keys(&state.account_order, &state.accounts, &state.account_usage, now);
        if eligible.is_empty() {
            return Err(DomainError::new("account_unavailable", "No account is signed in with available quota"));
        }
        alondra_core::model_selection::resolve_selection(intent, &state.model_profiles, &state.model_catalogs, &eligible)
            .map_err(|reasons| DomainError::new("model_unavailable", reasons.join("; ")))
    }

    fn choose_account(&self, _: &dyn Runtime, _: &Path, _: Millis) -> Option<AccountId> {
        self.asked("choose_account").account.clone()
    }

    fn require_initial_check(&self, _: &dyn Runtime) -> Result<(), DomainError> {
        self.asked("require_initial_check").checking_accounts.clone().map_or(Ok(()), Err)
    }

    fn waits_for_session(&self, _: &dyn Runtime) -> bool {
        self.asked("waits_for_session").waits_for_session
    }

    fn begin_launch(&self, _: &dyn Runtime, _: &TaskId) -> bool {
        whole_engine("begin_launch")
    }

    fn end_launch(&self, _: &dyn Runtime, _: &TaskId) {
        whole_engine("end_launch")
    }

    fn launch_planned<'a>(&'a self, _: &'a dyn Runtime, _: &'a Path, _: serde_json::Value) -> Launched<'a> {
        whole_engine("launch_planned")
    }

    fn relaunch<'a>(&'a self, _: &'a dyn Runtime, _: &'a Path, _: &'a TaskId, _: bool) -> Launched<'a> {
        whole_engine("relaunch")
    }

    fn start_facts(&self, _: &dyn Runtime, _: &DomainState) -> anyhow::Result<BTreeMap<TaskId, StartFacts>> {
        whole_engine("start_facts")
    }

    fn node(&self, _: &dyn Runtime, _: &str) -> Option<alondra_core::Node> {
        whole_engine("node")
    }

    fn node_removed(&self, _: &dyn Runtime, _: &str) {
        whole_engine("node_removed")
    }

    fn close_interrupted_at(&self, _: &dyn Runtime, _: &str) -> Option<Millis> {
        whole_engine("close_interrupted_at")
    }

    fn set_close_interrupted(&self, _: &dyn Runtime, _: &str, _: Option<Millis>) {
        whole_engine("set_close_interrupted")
    }

    fn begin_watch(&self, _: &dyn Runtime, _: &str) -> bool {
        whole_engine("begin_watch")
    }

    fn end_watch(&self, _: &dyn Runtime, _: &str) {
        whole_engine("end_watch")
    }

    fn close_node<'a>(&'a self, _: &'a dyn Runtime, _: &'a dyn Sessions, _: &'a AgentId, _: bool) -> Answer<'a, anyhow::Result<CloseOutcome>> {
        whole_engine("close_node")
    }

    fn launched_by_alondra(&self, _: &dyn Runtime, _: &AgentId) -> anyhow::Result<bool> {
        whole_engine("launched_by_alondra")
    }

    fn forget_ended_session(&self, _: &dyn Runtime, _: &str) -> anyhow::Result<()> {
        whole_engine("forget_ended_session")
    }

    fn user_closed(&self, _: &dyn Runtime, _: &str) -> anyhow::Result<()> {
        whole_engine("user_closed")
    }

    fn session_ended(&self, _: &dyn Runtime, _: &AgentId) -> anyhow::Result<()> {
        whole_engine("session_ended")
    }

    fn feed(&self, _: &dyn Runtime, _: Provider, _: &AgentId, _: &OrderedObservation) {
        self.note("feed");
    }

    fn feed_antigravity(&self, _: &dyn Runtime, _: &AgentId, _: &serde_json::Value) {
        self.note("feed");
    }

    fn host_ended(&self, _: &dyn Runtime, _: &AgentId) {
        self.note("host_ended");
    }

    fn restore_codex_scope(&self, _: &dyn Runtime, _: &TaskId, _: &AgentId, _: &[u8]) {
        whole_engine("restore_codex_scope")
    }

    fn note_output(&self, _: &dyn Runtime, _: &TaskId) {
        self.note("note_output");
    }

    fn context_launched(&self, _: &dyn Runtime, _: &TaskId, _: StartKind) {
        self.note("context_launched");
    }

    fn context_handed(&self, _: &dyn Runtime, _: &TaskId, _: Millis) {
        self.note("context_handed");
    }

    fn context_observed(&self, _: &dyn Runtime, _: &TaskId, _: Provider, _: &OrderedObservation, _: Millis) -> Option<SessionStart> {
        self.note("context_observed");
        None
    }

    fn forget_context(&self, _: &dyn Runtime, _: &TaskId) {
        self.note("forget_context");
    }

    fn last_request(&self, _: &dyn Runtime, task: &TaskId) -> Option<LastRequest> {
        self.asked("last_request").last_requests.get(task).copied()
    }

    fn worktree_ready<'a>(&'a self, _: &'a dyn Runtime, path: &'a str) -> Work<'a, anyhow::Result<()>> {
        let unready = self.asked("worktree_ready").unready.get(path).cloned();
        Box::pin(async move { unready.map_or(Ok(()), |why| Err(anyhow::anyhow!(why))) })
    }

    fn worktree_heir(&self, _: &dyn Runtime, _: &Task, _: &str) -> anyhow::Result<Option<TaskId>> {
        whole_engine("worktree_heir")
    }

    fn may_handle_worktree(&self, _: &dyn Runtime, _: &str) -> Result<(), String> {
        whole_engine("may_handle_worktree")
    }

    fn worktree_leftovers<'a>(&'a self, _: &'a dyn Runtime, _: &'a Task) -> Work<'a, Vec<(String, String)>> {
        whole_engine("worktree_leftovers")
    }

    fn release_worktrees<'a>(&'a self, _: &'a dyn Runtime, _: &'a Task) -> Work<'a, anyhow::Result<Vec<(String, String)>>> {
        whole_engine("release_worktrees")
    }

    fn sweep_cancelled_worktrees<'a>(&'a self, _: &'a dyn Runtime) -> Work<'a, anyhow::Result<()>> {
        whole_engine("sweep_cancelled_worktrees")
    }

    fn sweep_worktree_shells<'a>(&'a self, _: &'a dyn Runtime) -> Work<'a, anyhow::Result<usize>> {
        whole_engine("sweep_worktree_shells")
    }

    fn release_cancelled_worktrees<'a>(&'a self, _: &'a dyn Runtime, _: &'a Task) -> Work<'a, anyhow::Result<()>> {
        whole_engine("release_cancelled_worktrees")
    }

    fn apply_cancelled_worktree_answers<'a>(&'a self, _: &'a dyn Runtime) -> Work<'a, anyhow::Result<()>> {
        whole_engine("apply_cancelled_worktree_answers")
    }
}
