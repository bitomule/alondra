//! What the owners of the SDK hosts and the launching of tasks ask of the rest of the engine,
//! through the ports the daemon wires, each bound to the runtime they run in: `SdkEngine`, and what
//! they share with the gate (`AdmissionEngine`) and the orchestrator (`OrchestratorEngine`).
use alondra_core::accounts::{AccountId, AccountKey, Provider};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use alondra_core::{Millis, Node};
use alondra_domain_rules::{AgentId, DomainError, DomainState, NodeEvent, StartFacts, Task, TaskId};
use alondra_engine_ports::Runtime;
use alondra_engine_ports::admission::AdmissionEngine;
use alondra_engine_ports::node_close::{CloseOutcome, Sessions};
use alondra_engine_ports::orchestrator::{Exhausted, OrchestratorEngine};
use alondra_engine_ports::sdk::{ClosedNode, LastRequest, SdkEngine, SessionStart, StartKind};
use alondra_provider_contract::OrderedObservation;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

pub trait EngineOf {
    fn engine(&self) -> Engine<'_>;
}

impl<R: Runtime + ?Sized> EngineOf for R {
    fn engine(&self) -> Engine<'_> {
        Engine { runtime: self.as_runtime(), port: self.ports().port::<dyn SdkEngine>().expect("the daemon gives the runtime what the SDK hosts ask of the engine") }
    }
}

/// The engine as one runtime's SDK hosts see it.
pub struct Engine<'a> {
    runtime: &'a dyn Runtime,
    port: Arc<dyn SdkEngine>,
}

/// A launch of a task under way: no other launch of it starts until it is dropped.
pub struct Launching<'a> {
    engine: Engine<'a>,
    task: TaskId,
}

impl Drop for Launching<'_> {
    fn drop(&mut self) {
        self.engine.port.end_launch(self.engine.runtime, &self.task);
    }
}

impl<'a> Engine<'a> {
    fn admission(&self) -> Arc<dyn AdmissionEngine> {
        self.runtime.ports().port::<dyn AdmissionEngine>().expect("the daemon gives the runtime what the admission gate asks of the engine")
    }

    fn orchestrator(&self) -> Arc<dyn OrchestratorEngine> {
        self.runtime.ports().port::<dyn OrchestratorEngine>().expect("the engine gives the runtime what the orchestrator asks of it")
    }

    pub fn select_account(&self, home: &Path, intent: &TaskSelection, denied_tools: &[String], now: Millis) -> Result<SelectionDecision, DomainError> {
        self.port.select_account(self.runtime, home, intent, denied_tools, now)
    }

    pub fn choose_account(&self, home: &Path, now: Millis) -> Option<AccountId> {
        self.port.choose_account(self.runtime, home, now)
    }

    pub fn require_initial_check(&self) -> Result<(), DomainError> {
        self.port.require_initial_check(self.runtime)
    }

    pub fn waits_for_session(&self) -> bool {
        self.port.waits_for_session(self.runtime)
    }

    pub fn log_account_choice(&self, what: &str, account: &AccountKey, now: Millis) {
        self.orchestrator().log_account_choice(self.runtime, what, account, now);
    }

    pub fn usage_pause(&self) -> Option<Exhausted> {
        self.orchestrator().usage_pause(self.runtime)
    }

    pub fn usage_changes(&self) -> watch::Receiver<Option<Exhausted>> {
        self.orchestrator().usage_changes(self.runtime)
    }

    pub fn usage_paused(&self) -> bool {
        self.admission().usage_paused(self.runtime)
    }

    pub fn api_down(&self, now: Millis) -> bool {
        self.admission().api_watch(self.runtime).down(now)
    }

    /// None when `task` is being launched already.
    pub fn begin_launch(self, task: &TaskId) -> Option<Launching<'a>> {
        self.port.begin_launch(self.runtime, task).then(|| Launching { engine: self, task: task.clone() })
    }

    pub async fn launch_planned(&self, home: &Path, body: serde_json::Value) -> Result<serde_json::Value, DomainError> {
        self.port.launch_planned(self.runtime, home, body).await
    }

    pub async fn relaunch(&self, home: &Path, task: &TaskId, by_itself: bool) -> Result<serde_json::Value, DomainError> {
        self.port.relaunch(self.runtime, home, task, by_itself).await
    }

    pub fn start_facts(&self, state: &DomainState) -> anyhow::Result<BTreeMap<TaskId, StartFacts>> {
        self.port.start_facts(self.runtime, state)
    }

    pub fn closed_task(&self, task: &TaskId) -> Option<ClosedNode> {
        self.port.closed_task(self.runtime, task)
    }

    pub fn remember_closed_task(&self, task: &TaskId, node: ClosedNode) {
        self.port.remember_closed_task(self.runtime, task, node);
    }

    pub fn forget_closed_task(&self, task: &TaskId) {
        self.port.forget_closed_task(self.runtime, task);
    }

    pub fn node(&self, session: &str) -> Option<Node> {
        self.port.node(self.runtime, session)
    }

    pub fn node_cwd(&self, session: &str) -> Option<String> {
        self.port.node_cwd(self.runtime, session)
    }

    pub fn node_removed(&self, session: &str) {
        self.port.node_removed(self.runtime, session);
    }

    pub fn close_interrupted_at(&self, session: &str) -> Option<Millis> {
        self.port.close_interrupted_at(self.runtime, session)
    }

    pub fn set_close_interrupted(&self, session: &str, at: Option<Millis>) {
        self.port.set_close_interrupted(self.runtime, session, at);
    }

    pub fn begin_watch(&self, session: &str) -> bool {
        self.port.begin_watch(self.runtime, session)
    }

    pub fn end_watch(&self, session: &str) {
        self.port.end_watch(self.runtime, session);
    }

    pub fn interrupt(&self, session: &str) -> bool {
        self.admission().interrupt(self.runtime, session)
    }

    pub async fn close_node(&self, sessions: &dyn Sessions, agent: &AgentId, leave_if_not_listed: bool) -> anyhow::Result<CloseOutcome> {
        self.port.close_node(self.runtime, sessions, agent, leave_if_not_listed).await
    }

    pub fn launched_by_alondra(&self, agent: &AgentId) -> anyhow::Result<bool> {
        self.port.launched_by_alondra(self.runtime, agent)
    }

    pub fn forget_ended_session(&self, session: &str) -> anyhow::Result<()> {
        self.port.forget_ended_session(self.runtime, session)
    }

    pub fn user_closed(&self, session: &str) -> anyhow::Result<()> {
        self.port.user_closed(self.runtime, session)
    }

    pub fn session_ended(&self, agent: &AgentId) -> anyhow::Result<()> {
        self.port.session_ended(self.runtime, agent)
    }

    pub fn node_event(&self, agent: &AgentId, event: NodeEvent) -> Result<bool, String> {
        self.admission().node_event(self.runtime, agent, event)
    }

    pub fn tell_node(&self, task: &TaskId, text: &str, command: &str) -> Result<(), String> {
        self.admission().tell_node(self.runtime, task, text, command)
    }

    pub fn tell_session(&self, session: &str, text: &str, as_user: bool) -> bool {
        self.admission().tell_session(self.runtime, session, text, as_user)
    }

    pub fn feed(&self, provider: Provider, agent: &AgentId, observation: &OrderedObservation) {
        self.port.feed(self.runtime, provider, agent, observation);
    }

    pub fn host_ended(&self, agent: &AgentId) {
        self.port.host_ended(self.runtime, agent);
    }

    pub fn restore_codex_scope(&self, task: &TaskId, agent: &AgentId, checkpoint: &[u8]) {
        self.port.restore_codex_scope(self.runtime, task, agent, checkpoint);
    }

    pub fn note_output(&self, task: &TaskId) {
        self.port.note_output(self.runtime, task);
    }

    pub fn context_launched(&self, task: &TaskId, kind: StartKind) {
        self.port.context_launched(self.runtime, task, kind);
    }

    pub fn context_handed(&self, task: &TaskId, at: Millis) {
        self.port.context_handed(self.runtime, task, at);
    }

    pub fn context_observed(&self, task: &TaskId, provider: Provider, observation: &OrderedObservation, at: Millis) -> Option<SessionStart> {
        self.port.context_observed(self.runtime, task, provider, observation, at)
    }

    pub fn forget_context(&self, task: &TaskId) {
        self.port.forget_context(self.runtime, task);
    }

    pub fn last_request(&self, task: &TaskId) -> Option<LastRequest> {
        self.port.last_request(self.runtime, task)
    }

    pub async fn worktree_ready(&self, path: &str) -> anyhow::Result<()> {
        self.port.worktree_ready(self.runtime, path).await
    }

    pub fn worktree_heir(&self, task: &Task, path: &str) -> anyhow::Result<Option<TaskId>> {
        self.port.worktree_heir(self.runtime, task, path)
    }

    pub fn may_handle_worktree(&self, path: &str) -> Result<(), String> {
        self.port.may_handle_worktree(self.runtime, path)
    }

    pub async fn worktree_leftovers(&self, task: &Task) -> Vec<(String, String)> {
        self.port.worktree_leftovers(self.runtime, task).await
    }

    pub async fn release_worktrees(&self, task: &Task) -> anyhow::Result<Vec<(String, String)>> {
        self.port.release_worktrees(self.runtime, task).await
    }

    pub async fn sweep_cancelled_worktrees(&self) -> anyhow::Result<()> {
        self.port.sweep_cancelled_worktrees(self.runtime).await
    }

    pub async fn sweep_worktree_shells(&self) -> anyhow::Result<usize> {
        self.port.sweep_worktree_shells(self.runtime).await
    }

    pub async fn release_cancelled_worktrees(&self, task: &Task) -> anyhow::Result<()> {
        self.port.release_cancelled_worktrees(self.runtime, task).await
    }

    pub async fn apply_cancelled_worktree_answers(&self) -> anyhow::Result<()> {
        self.port.apply_cancelled_worktree_answers(self.runtime).await
    }
}
