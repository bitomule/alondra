//! What the daemon's input adapter asks of the engine, one part per piece that answers it: the
//! runtime's own operations (`daemon-engine`), the schedules and the Agent SDK (`engine-sdk`), the
//! admission gate (`engine-admission`) and the orchestrator (`engine-orchestrator`). Each piece
//! gives the registry its part in its `wire`, so the server never names a piece of the engine.
use alondra_foundation::blocking_work::BlockingWork;
use crate::journal::{StoreError, TransportIdentity};
use crate::orchestrator::{ActiveRun, Decision};
use crate::{Registry, Runtime};
use alondra_core::Millis;
use alondra_core::accounts::AccountKey;
use alondra_core::machine_incident::MachineResource;
use alondra_core::metrics::report::MetricsInputs;
use alondra_core::model_selection::{ModelSelection, TaskSelection};
use alondra_core::options::NodeLimit;
use alondra_domain_model::{Actor, CommandEnvelope, CommandOutcome, DomainError, DomainState, TaskId};
use alondra_view_model::LivePacing;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

/// The runtime's lanes of blocking work, each with its own workers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Read,
    Producer,
    External,
    Command,
}

/// What the runtime does beyond the `Runtime` every piece shares, which `daemon-engine` provides.
pub trait RuntimeApi: Send + Sync {
    fn work<'a>(&self, runtime: &'a dyn Runtime, lane: Lane) -> &'a BlockingWork;
    /// Turns on plan mode for the task of the node holding `bearer`.
    fn request_plan_mode(&self, runtime: &dyn Runtime, bearer: &str, task: &TaskId, operation: &str) -> Result<CommandOutcome, StoreError>;
    /// What the orchestrator's batches produced, for the orchestrator's bearer only.
    fn read_batch_outcomes(&self, runtime: &dyn Runtime, bearer: &str, run: Option<&str>, list_runs: bool, after: &str, limit: usize) -> Result<Value, DomainError>;
    fn metrics_inputs(&self, runtime: &dyn Runtime, now: Millis) -> anyhow::Result<MetricsInputs>;
    /// Commits `envelope` as the holder of `bearer` once `check` accepts the state it commits on;
    /// true when it was committed before.
    fn execute_bearer_answered(
        &self,
        runtime: &dyn Runtime,
        bearer: &str,
        envelope: &CommandEnvelope,
        check: Box<dyn FnOnce(&DomainState) -> Result<(), DomainError> + '_>,
    ) -> Result<(CommandOutcome, bool), StoreError>;
}

/// The schedules, a task's change of provider or account, and the move to the Agent SDK, which
/// `engine-sdk` provides. Answers are `{"status", "body"}` as the route returns them.
pub trait SdkApi: Send + Sync {
    fn schedule_read(&self, runtime: &dyn Runtime, path: &str) -> Value;
    #[allow(clippy::too_many_arguments)]
    fn schedule_mutation(
        &self,
        runtime: &dyn Runtime,
        home: &Path,
        method: &str,
        path: &str,
        body: Option<&Value>,
        identity: Option<TransportIdentity<'_>>,
        authorize: &dyn Fn() -> anyhow::Result<()>,
    ) -> Value;
    /// A mutation a paired device sealed as `sender`.
    #[allow(clippy::too_many_arguments)]
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
    ) -> Value;
    /// A mutation the orchestrator asked for: its schedules use Automatic accounts.
    #[allow(clippy::too_many_arguments)]
    fn orchestrator_schedule_mutation(
        &self,
        runtime: &dyn Runtime,
        home: &Path,
        method: &str,
        path: &str,
        body: Option<&Value>,
        identity: Option<TransportIdentity<'_>>,
        authorize: &dyn Fn() -> anyhow::Result<()>,
    ) -> Value;
    /// Moves `task`'s execution to `account` and `model`.
    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<Value, DomainError>;
    /// Moves `task` to `account`, keeping its model where the account offers it.
    fn move_task(
        &self,
        runtime: &dyn Runtime,
        actor: &Actor,
        task: &TaskId,
        account: AccountKey,
        expected_selection: &str,
        authorize: Box<dyn FnOnce() -> Result<(), DomainError> + '_>,
    ) -> Result<Value, DomainError>;
    /// What the move to the Agent SDK does with each task in progress, one row each; it changes
    /// nothing.
    fn handover_rows(&self, runtime: &dyn Runtime) -> anyhow::Result<Vec<Value>>;
}

/// The admission gate's controls and reads beyond `AdmissionGate`, which `engine-admission`
/// provides.
pub trait AdmissionApi: Send + Sync {
    /// The gate as the live view shows it: its queue, its pauses and why the head waits.
    fn pacing(&self, runtime: &dyn Runtime, now: Millis) -> Option<LivePacing>;
    /// The user paused a queued task: nothing starts it until the user resumes it.
    fn hold_queued(&self, runtime: &dyn Runtime, task: &TaskId, now: Millis) -> Result<(), &'static str>;
    fn release_queued(&self, runtime: &dyn Runtime, task: &TaskId) -> Result<(), &'static str>;
    /// The most nodes the Mac may run at once.
    fn most_working_nodes(&self, runtime: &dyn Runtime) -> u32;
    fn set_node_limit(&self, home: &Path, limit: NodeLimit) -> Result<(), &'static str>;
    /// `session`'s node said it waits for the machine during its turn `turn`.
    fn declare_machine_wait(&self, runtime: &dyn Runtime, session: &str, resource: Option<MachineResource>, reason: &str, turn: u64, now: Millis);
}

/// The orchestrator as the user sees and steers it, which `engine-orchestrator` provides.
pub trait OrchestratorApi: Send + Sync {
    /// Its phase and the actions available to the user.
    fn status(&self, runtime: Arc<dyn Runtime>, home: &Path) -> anyhow::Result<Value>;
    /// The user's decision on the execution `body` names, as `{"status", "body"}`.
    fn decide(&self, runtime: Arc<dyn Runtime>, home: &Path, decision: Decision, body: Option<&Value>) -> Value;
    fn active_run(&self, home: &Path) -> anyhow::Result<ActiveRun>;
}

/// Every part of what the server asks of the engine, as the registry holds them.
#[derive(Clone)]
pub struct DomainApi {
    pub runtime: Arc<dyn RuntimeApi>,
    pub sdk: Arc<dyn SdkApi>,
    pub admission: Arc<dyn AdmissionApi>,
    pub orchestrator: Arc<dyn OrchestratorApi>,
}

impl DomainApi {
    pub fn of(registry: &Registry) -> Self {
        Self {
            runtime: registry.port::<dyn RuntimeApi>().expect("daemon-engine gives the registry the runtime's operations"),
            sdk: registry.port::<dyn SdkApi>().expect("engine-sdk gives the registry the schedules and the Agent SDK"),
            admission: registry.port::<dyn AdmissionApi>().expect("engine-admission gives the registry the gate's controls"),
            orchestrator: registry.port::<dyn OrchestratorApi>().expect("engine-orchestrator gives the registry the orchestrator"),
        }
    }
}
