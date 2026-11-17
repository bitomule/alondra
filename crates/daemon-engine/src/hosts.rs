//! The Agent SDK hosts as the rest of the engine sees them: what it asks of `engine-sdk`, which owns
//! every host, through a port of the runtime that `engine-sdk` provides when the daemon is put
//! together. The engine's own tests open a runtime without it: no host runs there.
use crate::domain_runtime::DomainRuntime;
use crate::sdk_backend::SdkBackend;
use alondra_core::accounts::{AccountId, Provider};
use alondra_daemon_engine_model::{Failed, LaunchOptions};
use alondra_domain_rules::{AgentId, DomainError, DomainState, TaskId, TaskModel};
use std::any::Any;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

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
    fn owns(&self, domain: &DomainRuntime, agent: &AgentId) -> bool;
    /// The process group of every host an owner runs, by its node.
    fn node_groups_by_agent(&self, domain: &DomainRuntime) -> Vec<(AgentId, i32)>;
    /// Stops `agent`'s host and waits for it.
    fn stop(&self, domain: &DomainRuntime, agent: &AgentId) -> Result<(), Failed>;
    /// The owner of `task` gives its input back to the queue on its next step.
    fn requeue(&self, domain: &DomainRuntime, task: &TaskId);
    /// Stops what `agent`'s host runs for a pause, keeping its task.
    fn halt(&self, domain: &DomainRuntime, task: TaskId, agent: AgentId);
    /// Takes over `task` if no owner runs its host.
    fn take_if_unowned(&self, domain: &DomainRuntime, task: &TaskId);
    /// Takes over every working task no owner runs, once it has waited long enough.
    fn take_working_unowned(&self, domain: &DomainRuntime, state: &DomainState, now: alondra_core::Millis);
    /// Closes the hosts of nodes that ended.
    fn close_hosts_of_ended_nodes(&self, domain: &DomainRuntime, state: &DomainState);
    /// Takes over again the tasks that waited for room on the disk.
    fn retake_waiting_for_space(&self, domain: &DomainRuntime);
    /// The folder `task`'s first execution ran in, if it ran.
    fn original_directory(&self, domain: &DomainRuntime, task: &TaskId) -> Result<Option<String>, Failed>;
    /// Whether `task`'s session can be resumed with `account` in `directory`.
    fn require_recoverable_session(&self, domain: &DomainRuntime, backend: &SdkBackend, task: &TaskId, provider: Provider, account: &AccountId, directory: &str) -> Result<(), Failed>;
    /// Checks a reopening of `task` with `input` before it launches.
    #[allow(clippy::too_many_arguments)]
    fn prepare_reopening(&self, domain: &DomainRuntime, backend: &SdkBackend, task: &TaskId, provider: Provider, account: &AccountId, directory: &Path, input: String, recover_directory: bool) -> Result<Reopening, Failed>;
    /// Ends the host of `task` that no owner runs.
    fn end_unowned_host(&self, domain: Arc<DomainRuntime>, backend: &SdkBackend, task: TaskId) -> Result<(), Failed>;
    /// Starts `launch`'s node through the SDK and hands it to an owner.
    fn launch<'a>(&'a self, domain: &'a DomainRuntime, launch: Launch, reopening: Option<Reopening>) -> Launched<'a>;
    /// Whether `task` is moving to another provider.
    fn moving(&self, domain: &DomainRuntime, task: &TaskId) -> bool;
}

pub trait HostsOf {
    fn hosts(&self) -> Hosts<'_>;
}

impl HostsOf for DomainRuntime {
    fn hosts(&self) -> Hosts<'_> {
        let port = self.port::<dyn SdkHosts>();
        #[cfg(any(test, feature = "test-hooks"))]
        let port = port.or_else(|| Some(Arc::new(none::NoHosts)));
        Hosts { domain: self, port: port.expect("the daemon gives the runtime the owner of its SDK hosts") }
    }
}

/// The SDK hosts of one runtime.
pub struct Hosts<'a> {
    domain: &'a DomainRuntime,
    port: Arc<dyn SdkHosts>,
}

impl Hosts<'_> {
    pub fn owns(&self, agent: &AgentId) -> bool {
        self.port.owns(self.domain, agent)
    }

    pub fn node_groups_by_agent(&self) -> Vec<(AgentId, i32)> {
        self.port.node_groups_by_agent(self.domain)
    }

    pub fn stop(&self, agent: &AgentId) -> Result<(), Failed> {
        self.port.stop(self.domain, agent)
    }

    pub fn requeue(&self, task: &TaskId) {
        self.port.requeue(self.domain, task);
    }

    pub fn halt(&self, task: TaskId, agent: AgentId) {
        self.port.halt(self.domain, task, agent);
    }

    pub fn take_if_unowned(&self, task: &TaskId) {
        self.port.take_if_unowned(self.domain, task);
    }

    pub fn take_working_unowned(&self, state: &DomainState, now: alondra_core::Millis) {
        self.port.take_working_unowned(self.domain, state, now);
    }

    pub fn close_hosts_of_ended_nodes(&self, state: &DomainState) {
        self.port.close_hosts_of_ended_nodes(self.domain, state);
    }

    pub fn retake_waiting_for_space(&self) {
        self.port.retake_waiting_for_space(self.domain);
    }

    pub fn original_directory(&self, task: &TaskId) -> Result<Option<String>, Failed> {
        self.port.original_directory(self.domain, task)
    }

    pub fn require_recoverable_session(&self, backend: &SdkBackend, task: &TaskId, provider: Provider, account: &AccountId, directory: &str) -> Result<(), Failed> {
        self.port.require_recoverable_session(self.domain, backend, task, provider, account, directory)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_reopening(&self, backend: &SdkBackend, task: &TaskId, provider: Provider, account: &AccountId, directory: &Path, input: String, recover_directory: bool) -> Result<Reopening, Failed> {
        self.port.prepare_reopening(self.domain, backend, task, provider, account, directory, input, recover_directory)
    }

    pub fn end_unowned_host(&self, domain: Arc<DomainRuntime>, backend: &SdkBackend, task: TaskId) -> Result<(), Failed> {
        self.port.end_unowned_host(domain, backend, task)
    }

    pub async fn launch(&self, launch: Launch, reopening: Option<Reopening>) -> Result<serde_json::Value, DomainError> {
        self.port.launch(self.domain, launch, reopening).await
    }

    pub fn moving(&self, task: &TaskId) -> bool {
        self.port.moving(self.domain, task)
    }
}

/// The hosts of a runtime nobody gave an owner: none runs, and nothing starts.
#[cfg(any(test, feature = "test-hooks"))]
mod none {
    use super::*;
    use crate::sdk_backend::NO_HOST;

    pub(super) struct NoHosts;

    impl SdkHosts for NoHosts {
        fn owns(&self, _: &DomainRuntime, _: &AgentId) -> bool {
            false
        }
        fn node_groups_by_agent(&self, _: &DomainRuntime) -> Vec<(AgentId, i32)> {
            vec![]
        }
        fn stop(&self, _: &DomainRuntime, _: &AgentId) -> Result<(), Failed> {
            Ok(())
        }
        fn requeue(&self, _: &DomainRuntime, _: &TaskId) {}
        fn halt(&self, _: &DomainRuntime, _: TaskId, _: AgentId) {}
        fn take_if_unowned(&self, _: &DomainRuntime, _: &TaskId) {}
        fn take_working_unowned(&self, _: &DomainRuntime, _: &DomainState, _: alondra_core::Millis) {}
        fn close_hosts_of_ended_nodes(&self, _: &DomainRuntime, _: &DomainState) {}
        fn retake_waiting_for_space(&self, _: &DomainRuntime) {}
        fn original_directory(&self, _: &DomainRuntime, _: &TaskId) -> Result<Option<String>, Failed> {
            Ok(None)
        }
        fn require_recoverable_session(&self, _: &DomainRuntime, _: &SdkBackend, _: &TaskId, _: Provider, _: &AccountId, _: &str) -> Result<(), Failed> {
            Err(Failed::new(NO_HOST.into()))
        }
        fn prepare_reopening(&self, _: &DomainRuntime, _: &SdkBackend, _: &TaskId, _: Provider, _: &AccountId, _: &Path, _: String, _: bool) -> Result<Reopening, Failed> {
            Err(Failed::new(NO_HOST.into()))
        }
        fn end_unowned_host(&self, _: Arc<DomainRuntime>, _: &SdkBackend, _: TaskId) -> Result<(), Failed> {
            Ok(())
        }
        fn launch<'a>(&'a self, _: &'a DomainRuntime, _: Launch, _: Option<Reopening>) -> Launched<'a> {
            Box::pin(async { Err(DomainError::new("unavailable", NO_HOST)) })
        }
        fn moving(&self, _: &DomainRuntime, _: &TaskId) -> bool {
            false
        }
    }
}
