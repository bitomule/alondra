//! The Agent SDK hosts as the rest of the engine sees them: what it asks of `engine-sdk`, which owns
//! every host.
use crate::Runtime;
use crate::launch::{Failed, LaunchOptions};
use alondra_core::accounts::{AccountId, Provider};
use alondra_domain_rules::{AgentId, DomainError, DomainState, TaskId, TaskModel};
use std::any::Any;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

/// The host an SDK execution runs on, as the domain records it: never `claude-bg`.
pub const HOST: &str = "alondra-sdk";

/// Whether `agent` holds the current execution of a task that runs through the SDK.
pub fn runs_through_sdk(state: &DomainState, agent: &AgentId) -> bool {
    state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(agent)).any(|task| {
        state
            .lifecycle_of(&task.id).ok()
            .is_some_and(|lifecycle| lifecycle.execution().is_some_and(|execution| execution.host().as_str() == HOST))
    })
}

/// What a new SDK node starts with.
pub struct Launch {
    pub task: TaskId,
    pub name: String,
    pub directory: String,
    pub prompt: String,
    /// Alondra's node plugin: its MCP server is how the task reaches Alondra.
    pub plugin: PathBuf,
    pub options: LaunchOptions,
    /// The model the launch chose, recorded on the task when its execution is admitted.
    pub model: Option<TaskModel>,
    /// The provider account its Claude runs on, recorded on the task with its execution.
    pub account: AccountId,
}

/// A reopening of a task's session the hosts' owner checked, which only it reads when it launches.
pub struct Reopening(pub Box<dyn Any + Send>);

pub type Launched<'a> = Pin<Box<dyn Future<Output = Result<serde_json::Value, DomainError>> + Send + 'a>>;

/// What the rest of the engine asks of the owner of the SDK hosts. Each call names the runtime
/// whose hosts it asks about: their owners live there.
pub trait SdkHosts: Send + Sync {
    /// Whether an owner of this daemon runs `agent`'s host.
    fn owns(&self, runtime: &dyn Runtime, agent: &AgentId) -> bool;
    /// The bytes the processes of every host an owner runs hold, by its node.
    fn node_footprints(&self, runtime: &dyn Runtime) -> std::io::Result<BTreeMap<AgentId, u64>>;
    /// Stops `agent`'s host and waits for it.
    fn stop(&self, runtime: &dyn Runtime, agent: &AgentId) -> Result<(), Failed>;
    /// The owner of `task` gives its input back to the queue on its next step.
    fn requeue(&self, runtime: &dyn Runtime, task: &TaskId);
    /// Stops what `agent`'s host runs for a pause, keeping its task.
    fn halt(&self, runtime: &dyn Runtime, task: TaskId, agent: AgentId);
    /// Takes over `task` if no owner runs its host.
    fn take_if_unowned(&self, runtime: &dyn Runtime, task: &TaskId);
    /// Takes over every working task no owner runs, once it has waited long enough.
    fn take_working_unowned(&self, runtime: &dyn Runtime, state: &DomainState, now: alondra_core::Millis);
    /// Closes the hosts of nodes that ended.
    fn close_hosts_of_ended_nodes(&self, runtime: &dyn Runtime, state: &DomainState);
    /// Takes over again the tasks that waited for room on the disk.
    fn retake_waiting_for_space(&self, runtime: &dyn Runtime);
    /// The folder `task`'s first execution ran in, if it ran.
    fn original_directory(&self, runtime: &dyn Runtime, task: &TaskId) -> Result<Option<String>, Failed>;
    /// Whether `task`'s session can be resumed with `account` in `directory`.
    fn require_recoverable_session(&self, runtime: &dyn Runtime, task: &TaskId, provider: Provider, account: &AccountId, directory: &str) -> Result<(), Failed>;
    /// Checks a reopening of `task` with `input` before it launches.
    #[allow(clippy::too_many_arguments)]
    fn prepare_reopening(&self, runtime: &dyn Runtime, task: &TaskId, provider: Provider, account: &AccountId, directory: &Path, input: String, recover_directory: bool) -> Result<Reopening, Failed>;
    /// Ends the host of `task` that no owner runs.
    fn end_unowned_host(&self, runtime: &dyn Runtime, task: TaskId) -> Result<(), Failed>;
    /// Starts `launch`'s node through the SDK and hands it to an owner.
    fn launch<'a>(&'a self, runtime: &'a dyn Runtime, launch: Launch, reopening: Option<Reopening>) -> Launched<'a>;
    /// Whether `task` is moving to another provider.
    fn moving(&self, runtime: &dyn Runtime, task: &TaskId) -> bool;
}

