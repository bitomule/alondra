//! The schedules, a task's change of provider or account and the move to the Agent SDK, as the
//! server asks them through `SdkApi`.
use alondra_core::accounts::AccountKey;
use alondra_core::model_selection::{ModelSelection, TaskSelection};
use alondra_domain_model::{Actor, DomainError, TaskId};
use alondra_engine_ports::Runtime;
use alondra_engine_ports::domain_api::SdkApi;
use alondra_engine_ports::journal::TransportIdentity;
use serde_json::Value;
use std::path::Path;

pub struct Api;

impl SdkApi for Api {
    fn schedule_read(&self, runtime: &dyn Runtime, path: &str) -> Value {
        crate::domain_schedule_api::read(runtime, path)
    }

    fn schedule_mutation(
        &self,
        runtime: &dyn Runtime,
        home: &Path,
        method: &str,
        path: &str,
        body: Option<&Value>,
        identity: Option<TransportIdentity<'_>>,
        authorize: &dyn Fn() -> anyhow::Result<()>,
    ) -> Value {
        crate::domain_schedule_api::mutation(runtime, home, method, path, body, identity, authorize)
    }

    fn schedule_encrypted_mutation(
        &self,
        runtime: &dyn Runtime,
        home: &Path,
        sender: &str,
        method: &str,
        path: &str,
        body: Option<&Value>,
        identity: TransportIdentity<'_>,
        authorize: &dyn Fn() -> anyhow::Result<()>,
    ) -> Value {
        crate::domain_schedule_api::encrypted_mutation(runtime, home, sender, method, path, body, identity, authorize)
    }

    fn orchestrator_schedule_mutation(
        &self,
        runtime: &dyn Runtime,
        home: &Path,
        method: &str,
        path: &str,
        body: Option<&Value>,
        identity: Option<TransportIdentity<'_>>,
        authorize: &dyn Fn() -> anyhow::Result<()>,
    ) -> Value {
        crate::domain_schedule_api::orchestrator_mutation(runtime, home, method, path, body, identity, authorize)
    }

    fn change_provider(
        &self,
        runtime: &dyn Runtime,
        actor: &Actor,
        task: &TaskId,
        account: AccountKey,
        model: ModelSelection,
        intent: TaskSelection,
        reason: &str,
        expected_selection: &str,
        authorize: Box<dyn FnOnce() -> Result<(), DomainError> + '_>,
    ) -> Result<Value, DomainError> {
        crate::provider_transition::request(runtime, actor, task, account, model, intent, reason, expected_selection, authorize)
    }

    fn move_task(
        &self,
        runtime: &dyn Runtime,
        actor: &Actor,
        task: &TaskId,
        account: AccountKey,
        expected_selection: &str,
        authorize: Box<dyn FnOnce() -> Result<(), DomainError> + '_>,
    ) -> Result<Value, DomainError> {
        crate::provider_transition::request_move(runtime, actor, task, account, expected_selection, authorize)
    }

    fn handover_rows(&self, runtime: &dyn Runtime) -> anyhow::Result<Vec<Value>> {
        crate::bg_handover::dry_run(runtime)?.iter().map(|row| Ok(serde_json::to_value(row)?)).collect()
    }
}
