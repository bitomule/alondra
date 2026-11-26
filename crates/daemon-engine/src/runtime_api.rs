//! The runtime's operations beyond `Runtime`, as the server asks them through `RuntimeApi`.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::Millis;
use alondra_core::metrics::report::MetricsInputs;
use alondra_domain_model::{CommandEnvelope, CommandOutcome, DomainError, DomainState, TaskId};
use alondra_engine_ports::Runtime;
use alondra_foundation::blocking_work::BlockingWork;
use alondra_engine_ports::domain_api::{Lane, RuntimeApi};
use serde_json::Value;

pub struct Api;

impl RuntimeApi for Api {
    fn work<'a>(&self, runtime: &'a dyn Runtime, lane: Lane) -> &'a BlockingWork {
        let runtime = DomainRuntime::of(runtime);
        match lane {
            Lane::Read => &runtime.read_work,
            Lane::Producer => &runtime.producer_work,
            Lane::External => &runtime.external_work,
            Lane::Command => &runtime.command_work,
        }
    }

    fn request_plan_mode(&self, runtime: &dyn Runtime, bearer: &str, task: &TaskId, operation: &str) -> Result<CommandOutcome, StoreError> {
        DomainRuntime::of(runtime).request_plan_mode(bearer, task, operation)
    }

    fn read_batch_outcomes(&self, runtime: &dyn Runtime, bearer: &str, run: Option<&str>, list_runs: bool, after: &str, limit: usize) -> Result<Value, DomainError> {
        DomainRuntime::of(runtime).read_batch_outcomes(bearer, run, list_runs, after, limit)
    }

    fn metrics_inputs(&self, runtime: &dyn Runtime, now: Millis) -> anyhow::Result<MetricsInputs> {
        DomainRuntime::of(runtime).metrics_inputs(now)
    }

    fn execute_bearer_answered(
        &self,
        runtime: &dyn Runtime,
        bearer: &str,
        envelope: &CommandEnvelope,
        check: Box<dyn FnOnce(&DomainState) -> Result<(), DomainError> + '_>,
    ) -> Result<(CommandOutcome, bool), StoreError> {
        DomainRuntime::of(runtime).execute_bearer_answered(bearer, envelope, check)
    }
}

/// The reads of stored files the runtime keeps, which the server serves attachments through.
pub fn stored_reads(runtime: &dyn Runtime) -> std::sync::Arc<alondra_engine_ports::stored_files::StoredReads> {
    DomainRuntime::of(runtime).stored_reads.clone()
}
