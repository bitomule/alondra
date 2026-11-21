//! What the owners of the SDK hosts and the launching of tasks (`engine-sdk`) ask of the rest of the
//! engine, which `daemon-engine` provides, and the values both speak in: the live state of nodes,
//! accounts, launches, what the hosts report and the task worktrees.
use crate::Runtime;
use crate::hosts::Launched;
use crate::node_close::{Answer, CloseOutcome, Sessions};
use alondra_core::accounts::{AccountId, Provider};
use alondra_core::model_selection::{SelectionDecision, TaskSelection};
use alondra_core::{Millis, Node};
use alondra_domain_rules::{AgentId, DomainError, DomainState, StartFacts, Task, TaskId};
use alondra_provider_contract::OrderedObservation;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

pub type Work<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a relaunch needs of the node that closed while it worked on a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosedNode {
    pub name: String,
    pub cwd: Option<String>,
    #[serde(default)]
    pub relaunch: Relaunch,
}

/// Who starts the task's next node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relaunch {
    /// The user closed it: a card asks them to relaunch the task or cancel it.
    #[default]
    AskUser,
    /// Alondra closed it for a reason of its own: the automatic launch starts the next node, with
    /// its retries and, past them, the card that says the task does not start.
    ByItself,
}

/// The conversation's size after its last request, and when that request's cache expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub tokens: u64,
    pub expires_at: Millis,
}

/// The last request of a task this daemon timed: how large the conversation was after it, when it
/// started, how long its cache lasts, and whether a compaction may still be asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRequest {
    pub tokens: u64,
    pub requested_at: Millis,
    pub cache_ms: Millis,
    pub compactable: bool,
}

impl LastRequest {
    pub fn reading(&self) -> Reading {
        Reading { tokens: self.tokens, expires_at: self.requested_at.saturating_add(self.cache_ms) }
    }
}

/// How a host of a task came to run its conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartKind {
    /// It opened a new conversation.
    New,
    /// It resumed the conversation the host before it left.
    Resumed,
    /// It opened a new conversation that carries on from the task's records.
    Handoff,
}

impl StartKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StartKind::New => "new",
            StartKind::Resumed => "resumed",
            StartKind::Handoff => "handoff",
        }
    }
}

/// The first answer a new host got: what its request wrote to and read from the prompt cache, and
/// how long after the task's previous request it went out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionStart {
    pub kind: StartKind,
    pub requested_at: Millis,
    pub since_previous: Option<Millis>,
    pub tokens: u64,
    pub written_1h: u64,
    pub written_5m: u64,
    pub read: u64,
}

/// What the owners of the SDK hosts and the launching of tasks ask of the rest of the engine.
pub trait SdkEngine: Send + Sync {
    /// The account and model a launch runs with now, none of whose tools are `denied_tools`.
    fn select_account(&self, runtime: &dyn Runtime, home: &Path, intent: &TaskSelection, denied_tools: &[String], now: Millis) -> Result<SelectionDecision, DomainError>;
    /// The Claude account a launch starts on as the user's distribution picks it; none holds every
    /// launch until one can work.
    fn choose_account(&self, runtime: &dyn Runtime, home: &Path, now: Millis) -> Option<AccountId>;
    /// Refused while the accounts are checked after startup.
    fn require_initial_check(&self, runtime: &dyn Runtime) -> Result<(), DomainError>;
    /// Whether every launch waits because no Claude account has a session.
    fn waits_for_session(&self, runtime: &dyn Runtime) -> bool;

    /// False when `task` is being launched already; true when its launch starts now.
    fn begin_launch(&self, runtime: &dyn Runtime, task: &TaskId) -> bool;
    fn end_launch(&self, runtime: &dyn Runtime, task: &TaskId);
    /// Launches what `body`, a launch of the task's plan in the form the gate's queue keeps, says, as
    /// the automatic launch does.
    fn launch_planned<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, body: serde_json::Value) -> Launched<'a>;
    /// Starts a new node for `task` in the folder of the node that closed; `by_itself` when the
    /// automatic launch asks, else as the orchestrator's relaunch.
    fn relaunch<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, task: &'a TaskId, by_itself: bool) -> Launched<'a>;
    /// What the engine knows of how every open task gets going.
    fn start_facts(&self, runtime: &dyn Runtime, state: &DomainState) -> anyhow::Result<BTreeMap<TaskId, StartFacts>>;
    fn closed_task(&self, runtime: &dyn Runtime, task: &TaskId) -> Option<ClosedNode>;
    fn remember_closed_task(&self, runtime: &dyn Runtime, task: &TaskId, node: ClosedNode);
    fn forget_closed_task(&self, runtime: &dyn Runtime, task: &TaskId);

    /// The live state of the `claude --bg` node `session`.
    fn node(&self, runtime: &dyn Runtime, session: &str) -> Option<Node>;
    /// The folder `session`'s node last reported working in.
    fn node_cwd(&self, runtime: &dyn Runtime, session: &str) -> Option<String>;
    /// `session`'s node is gone: it is closed and out of the live view.
    fn node_removed(&self, runtime: &dyn Runtime, session: &str);
    fn close_interrupted_at(&self, runtime: &dyn Runtime, session: &str) -> Option<Millis>;
    fn set_close_interrupted(&self, runtime: &dyn Runtime, session: &str, at: Option<Millis>);
    /// False when `session` is watched already; true when the watch starts now.
    fn begin_watch(&self, runtime: &dyn Runtime, session: &str) -> bool;
    fn end_watch(&self, runtime: &dyn Runtime, session: &str);
    /// Closes the session of `agent` the way `claude rm` does; one `claude agents` does not list
    /// closes only when `leave_if_not_listed` says so.
    fn close_node<'a>(&'a self, runtime: &'a dyn Runtime, sessions: &'a dyn Sessions, agent: &'a AgentId, leave_if_not_listed: bool) -> Answer<'a, anyhow::Result<CloseOutcome>>;
    /// Whether Alondra launched `agent`'s session: a recorded launch, or a scheduled run it started.
    fn launched_by_alondra(&self, runtime: &dyn Runtime, agent: &AgentId) -> anyhow::Result<bool>;
    /// Forgets what Alondra kept about `session` only while it lived.
    fn forget_ended_session(&self, runtime: &dyn Runtime, session: &str) -> anyhow::Result<()>;
    /// The user closed `session`'s node: its tasks are released.
    fn user_closed(&self, runtime: &dyn Runtime, session: &str) -> anyhow::Result<()>;
    /// `agent`'s session ended: its bearer is revoked and its session kept only while it has an open task.
    fn session_ended(&self, runtime: &dyn Runtime, agent: &AgentId) -> anyhow::Result<()>;

    /// What a host of `provider` observed, for the live view and the node's metrics.
    fn feed(&self, runtime: &dyn Runtime, provider: Provider, agent: &AgentId, observation: &OrderedObservation);
    /// The host of `agent` ended.
    fn host_ended(&self, runtime: &dyn Runtime, agent: &AgentId);
    /// An adopted Codex host goes on with the live scope its `checkpoint` holds, if it holds one.
    fn restore_codex_scope(&self, runtime: &dyn Runtime, task: &TaskId, agent: &AgentId, checkpoint: &[u8]);
    /// The node of `task` gave an output now.
    fn note_output(&self, runtime: &dyn Runtime, task: &TaskId);
    fn context_launched(&self, runtime: &dyn Runtime, task: &TaskId, kind: StartKind);
    fn context_handed(&self, runtime: &dyn Runtime, task: &TaskId, at: Millis);
    /// What a host of `provider` observed of its conversation's size; the first answer of a new
    /// host says what starting it wrote to the prompt cache.
    fn context_observed(&self, runtime: &dyn Runtime, task: &TaskId, provider: Provider, observation: &OrderedObservation, at: Millis) -> Option<SessionStart>;
    fn forget_context(&self, runtime: &dyn Runtime, task: &TaskId);
    fn last_request(&self, runtime: &dyn Runtime, task: &TaskId) -> Option<LastRequest>;

    /// Makes sure the worktree at `path` is ready for a node.
    fn worktree_ready<'a>(&'a self, runtime: &'a dyn Runtime, path: &'a str) -> Work<'a, anyhow::Result<()>>;
    /// The open task that inherits the worktree at `path` from `task`, recorded as its own.
    fn worktree_heir(&self, runtime: &dyn Runtime, task: &Task, path: &str) -> anyhow::Result<Option<TaskId>>;
    /// Whether Alondra may handle the worktree at `path`; why not otherwise.
    fn may_handle_worktree(&self, runtime: &dyn Runtime, path: &str) -> Result<(), String>;
    /// What each worktree of `task` still holds that no remote has, by path.
    fn worktree_leftovers<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Work<'a, Vec<(String, String)>>;
    /// Releases every worktree of `task`; what is left, by path.
    fn release_worktrees<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Work<'a, anyhow::Result<Vec<(String, String)>>>;
    fn sweep_cancelled_worktrees<'a>(&'a self, runtime: &'a dyn Runtime) -> Work<'a, anyhow::Result<()>>;
    /// Deletes what ended tasks left of their worktrees once git removed the contents; how many went.
    fn sweep_worktree_shells<'a>(&'a self, runtime: &'a dyn Runtime) -> Work<'a, anyhow::Result<usize>>;
    fn release_cancelled_worktrees<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Work<'a, anyhow::Result<()>>;
    fn apply_cancelled_worktree_answers<'a>(&'a self, runtime: &'a dyn Runtime) -> Work<'a, anyhow::Result<()>>;
}
