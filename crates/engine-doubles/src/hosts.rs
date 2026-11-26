//! The owner of the SDK hosts in a runtime that runs none: nothing starts there.
use alondra_core::accounts::{AccountId, Provider};
use alondra_domain_model::{AgentId, DomainError, DomainState, TaskId};
use alondra_engine_ports::Runtime;
use alondra_engine_ports::hosts::{Launch, Launched, Reopening, SdkHosts};
use alondra_engine_ports::launch::Failed;
use std::collections::BTreeMap;
use std::path::Path;

const NO_HOST: &str = "no Agent SDK host runs in this test";

pub struct NoHosts;

impl SdkHosts for NoHosts {
    fn owns(&self, _: &dyn Runtime, _: &AgentId) -> bool {
        false
    }
    fn node_footprints(&self, _: &dyn Runtime) -> std::io::Result<BTreeMap<AgentId, u64>> {
        Ok(BTreeMap::new())
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
