//! The Agent SDK hosts as the rest of the engine sees them: what it asks of `engine-sdk`, which owns
//! every host, through the port `engine-sdk` gives the registry when the daemon is put together.
//! The engine's own tests open a runtime without it: no host runs there.
use crate::domain_runtime::DomainRuntime;
use alondra_core::accounts::{AccountId, Provider};
use alondra_daemon_engine_model::Failed;
use alondra_domain_rules::{AgentId, DomainError, DomainState, TaskId};
pub use alondra_engine_ports::hosts::{Launch, Launched, Reopening, SdkHosts};
use std::path::Path;
use std::sync::Arc;

pub trait HostsOf {
    fn hosts(&self) -> Hosts<'_>;
}

impl HostsOf for DomainRuntime {
    fn hosts(&self) -> Hosts<'_> {
        let port = self.ports().port::<dyn SdkHosts>();
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

/// The hosts of a runtime nobody gave an owner: none runs, and nothing starts.
#[cfg(any(test, feature = "test-hooks"))]
mod none {
    use super::*;
    use crate::sdk_backend::NO_HOST;
    use alondra_engine_ports::Runtime;

    pub(super) struct NoHosts;

    impl SdkHosts for NoHosts {
        fn owns(&self, _: &dyn Runtime, _: &AgentId) -> bool {
            false
        }
        fn node_groups_by_agent(&self, _: &dyn Runtime) -> Vec<(AgentId, i32)> {
            vec![]
        }
        fn stop(&self, _: &dyn Runtime, _: &AgentId) -> Result<(), Failed> {
            Ok(())
        }
        fn requeue(&self, _: &dyn Runtime, _: &TaskId) {}
        fn halt(&self, _: &dyn Runtime, _: TaskId, _: AgentId) {}
        fn take_if_unowned(&self, _: &dyn Runtime, _: &TaskId) {}
        fn take_working_unowned(&self, _: &dyn Runtime, _: &DomainState, _: alondra_core::Millis) {}
        fn close_hosts_of_ended_nodes(&self, _: &dyn Runtime, _: &DomainState) {}
        fn retake_waiting_for_space(&self, _: &dyn Runtime) {}
        fn original_directory(&self, _: &dyn Runtime, _: &TaskId) -> Result<Option<String>, Failed> {
            Ok(None)
        }
        fn require_recoverable_session(&self, _: &dyn Runtime, _: &TaskId, _: Provider, _: &AccountId, _: &str) -> Result<(), Failed> {
            Err(Failed::new(NO_HOST.into()))
        }
        fn prepare_reopening(&self, _: &dyn Runtime, _: &TaskId, _: Provider, _: &AccountId, _: &Path, _: String, _: bool) -> Result<Reopening, Failed> {
            Err(Failed::new(NO_HOST.into()))
        }
        fn end_unowned_host(&self, _: &dyn Runtime, _: TaskId) -> Result<(), Failed> {
            Ok(())
        }
        fn launch<'a>(&'a self, _: &'a dyn Runtime, _: Launch, _: Option<Reopening>) -> Launched<'a> {
            Box::pin(async { Err(DomainError::new("unavailable", NO_HOST)) })
        }
        fn moving(&self, _: &dyn Runtime, _: &TaskId) -> bool {
            false
        }
    }
}
