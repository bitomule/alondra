//! The runtime's operations beyond `Runtime`, as the server asks them through `RuntimeApi`.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::Millis;
use alondra_core::metrics::report::MetricsInputs;
use alondra_domain_model::{AgentId, Command, CommandEnvelope, CommandOutcome, DomainError, DomainState, TaskId};
use alondra_engine_kernel::sdk_backend::SdkOf as _;
use alondra_engine_ports::Runtime;
use alondra_engine_ports::journal::records::AllowedRule;
use alondra_engine_ports::node_close::{CloseOutcome, Sessions};
use alondra_foundation::blocking_work::BlockingWork;
use alondra_engine_ports::domain_api::{Lane, Pending, RuntimeApi};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

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

    fn execute_system(&self, runtime: &dyn Runtime, command: Command) -> Result<(), StoreError> {
        alondra_engine_kernel::domain_runtime::RuntimeRecords::execute_system_command(runtime, command)
    }

    fn stored_reads(&self, runtime: &dyn Runtime) -> Arc<alondra_engine_ports::stored_files::StoredReads> {
        stored_reads(runtime)
    }

    fn sdk_enabled(&self, runtime: &dyn Runtime) -> bool {
        runtime.sdk().is_some()
    }

    fn sessions(&self, runtime: &dyn Runtime) -> Arc<dyn Sessions> {
        Arc::new(crate::node_close::Machine(runtime.ports().worktrees()))
    }

    fn close_node<'a>(&'a self, runtime: &'a dyn Runtime, sessions: &'a dyn Sessions, agent: &'a AgentId, leave_if_not_listed: bool)
        -> Pending<'a, anyhow::Result<CloseOutcome>> {
        Box::pin(crate::node_close::close_node(runtime, sessions, agent, leave_if_not_listed))
    }

    fn cleanup<'a>(&'a self, runtime: &'a dyn Runtime, sessions: &'a dyn Sessions, claude_home: &'a Path, dry_run: bool, now: Millis)
        -> Pending<'a, Result<Value, (u16, String)>> {
        Box::pin(async move {
            crate::cleanup::run(runtime, sessions, claude_home, dry_run, now, crate::cleanup::STOP_WAIT).await
                .map_err(|(status, error)| (status.as_u16(), error))
        })
    }

    fn reconcile_allowed_rules(&self, runtime: &dyn Runtime) -> Vec<AllowedRule> {
        crate::allow_always::reconcile(runtime)
    }

    fn remove_allowed_rule(&self, runtime: &dyn Runtime, id: i64) -> anyhow::Result<Option<AllowedRule>> {
        crate::allow_always::remove_rule(runtime, id)
    }
}

/// The reads of stored files the runtime keeps, which the server serves attachments through.
pub fn stored_reads(runtime: &dyn Runtime) -> std::sync::Arc<alondra_engine_ports::stored_files::StoredReads> {
    DomainRuntime::of(runtime).stored_reads.clone()
}
