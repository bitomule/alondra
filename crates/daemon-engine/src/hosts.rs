//! The Agent SDK hosts as the rest of the engine sees them: what it asks of `engine-sdk`, which owns
//! every host, through the port `engine-sdk` gives the registry when the daemon is put together.
//! A test registers a double of it.
use alondra_engine_ports::Runtime;
use alondra_core::accounts::{AccountId, Provider};
use alondra_daemon_engine_model::Failed;
use alondra_domain_rules::{AgentId, DomainError, DomainState, TaskId};
pub use alondra_engine_ports::hosts::{Launch, Launched, Reopening, SdkHosts};
use std::path::Path;
use std::sync::Arc;

pub trait HostsOf {
    fn hosts(&self) -> Hosts<'_>;
}

impl<R: alondra_engine_ports::Runtime + ?Sized> HostsOf for R {
    fn hosts(&self) -> Hosts<'_> {
        let port = self.ports().port::<dyn SdkHosts>();
        Hosts { domain: self.as_runtime(), port: port.expect("the daemon gives the runtime the owner of its SDK hosts") }
    }
}

/// The SDK hosts of one runtime.
pub struct Hosts<'a> {
    domain: &'a dyn Runtime,
    port: Arc<dyn SdkHosts>,
}

impl Hosts<'_> {
    pub fn owns(&self, agent: &AgentId) -> bool {
        self.port.owns(self.domain, agent)
    }

    pub fn node_footprints(&self) -> std::io::Result<std::collections::BTreeMap<AgentId, u64>> {
        self.port.node_footprints(self.domain)
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

    pub fn require_recoverable_session(&self, task: &TaskId, provider: Provider, account: &AccountId, directory: &str) -> Result<(), Failed> {
        self.port.require_recoverable_session(self.domain, task, provider, account, directory)
    }

    pub fn prepare_reopening(&self, task: &TaskId, provider: Provider, account: &AccountId, directory: &Path, input: String, recover_directory: bool) -> Result<Reopening, Failed> {
        self.port.prepare_reopening(self.domain, task, provider, account, directory, input, recover_directory)
    }

    pub fn end_unowned_host(&self, task: TaskId) -> Result<(), Failed> {
        self.port.end_unowned_host(self.domain, task)
    }

    pub async fn launch(&self, launch: Launch, reopening: Option<Reopening>) -> Result<serde_json::Value, DomainError> {
        self.port.launch(self.domain, launch, reopening).await
    }

    pub fn moving(&self, task: &TaskId) -> bool {
        self.port.moving(self.domain, task)
    }
}
