//! What the daemon's input adapter asks of the engine, one part per piece that answers it: the
//! runtime's own operations, its live view, the accounts, the messages to nodes, the orchestrator's
//! tools, the pull requests and the MCP servers (`daemon-engine`), the schedules and the Agent SDK
//! (`engine-sdk`), the admission gate (`engine-admission`) and the orchestrator
//! (`engine-orchestrator`). Each piece gives the registry its part in its `wire`, so the server
//! never names a piece of the engine.
use alondra_foundation::blocking_work::BlockingWork;
use alondra_foundation::changes::{Cursor, Delta};
use crate::github::PrFacts;
use crate::journal::records::AllowedRule;
use crate::journal::{OutboxMessage, StoreError, TransportIdentity};
use crate::live_view::{SessionReading, Used};
use crate::mcp::{McpProbes, Opened, Refused as McpRefused, Target as McpTarget};
use crate::node_close::{CloseOutcome, Sessions};
use crate::orchestrator::{ActiveRun, Decision, Exhausted};
use crate::{Registry, Runtime};
use alondra_core::accounts::{AccountChange, AccountKey, Provider};
use alondra_core::activity::Activity;
use alondra_core::machine_incident::MachineResource;
use alondra_core::metrics::report::MetricsInputs;
use alondra_core::model_selection::{ModelProfile, ModelSelection, RoleModelPolicy, SelectionDecision, TaskSelection};
use alondra_core::options::{NodeLimit, ProjectEntry, Resolved};
use alondra_core::{Event, Millis, Node};
use alondra_domain_model::{
    Actor, AgentId, Command, CommandEnvelope, CommandOutcome, DeliveryTarget, DomainError, DomainState, InteractionId, LaunchPlan, McpNodeReading,
    MergeVerdict, PrId, PullRequest, RequestId, Task, TaskId,
};
use alondra_view_model::{Lang, LivePacing};
use serde::Deserialize;
use serde_json::Value;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

/// Work the engine does for the server that ends later.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What an orchestrator tool does once its request is read: its answer, or why it was refused.
pub type ToolWork = Pending<'static, Result<Value, DomainError>>;

/// A page of a task's context, as its node asks for it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContextQuery {
    pub task_id: TaskId,
    #[serde(default)]
    pub after: Option<alondra_store::ContextRef>,
    #[serde(default = "page_size")]
    pub limit: usize,
}

fn page_size() -> usize { 32 }

/// Why a change from Settings to the accounts was not made.
#[derive(Debug)]
pub enum AccountRefusal {
    /// The change's own rules refuse it.
    Domain(DomainError),
    /// The accounts file or its folders refuse it, as the user reads it.
    Conflict(String),
}

/// Lets a change the user asked for from Settings go ahead.
pub type Authorize<'a> = &'a (dyn Fn() -> anyhow::Result<()> + Sync);

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
    /// Carries out `command` as the system, against the state it reads now.
    fn execute_system(&self, runtime: &dyn Runtime, command: Command) -> Result<(), StoreError>;
    /// The reads of stored files the runtime keeps, which attachments are served through.
    fn stored_reads(&self, runtime: &dyn Runtime) -> Arc<crate::stored_files::StoredReads>;
    /// Whether the nodes run through the Agent SDK host on this Mac.
    fn sdk_enabled(&self, runtime: &dyn Runtime) -> bool;
    /// Claude Code and git on this Mac, as closing a session uses them.
    fn sessions(&self, runtime: &dyn Runtime) -> Arc<dyn Sessions>;
    /// Closes `agent`'s session the way `claude rm` does, refusing while it has something pending.
    fn close_node<'a>(&'a self, runtime: &'a dyn Runtime, sessions: &'a dyn Sessions, agent: &'a AgentId, leave_if_not_listed: bool)
        -> Pending<'a, anyhow::Result<CloseOutcome>>;
    /// Closes the sessions of finished tasks and the ones stopped without a task; refused with
    /// its HTTP status.
    fn cleanup<'a>(&'a self, runtime: &'a dyn Runtime, sessions: &'a dyn Sessions, claude_home: &'a Path, dry_run: bool, now: Millis)
        -> Pending<'a, Result<Value, (u16, String)>>;
    /// The rules «allow always», each checked against the file Claude Code keeps it in.
    fn reconcile_allowed_rules(&self, runtime: &dyn Runtime) -> Vec<AllowedRule>;
    /// Takes the rule `id` away from Claude Code's settings and from the list.
    fn remove_allowed_rule(&self, runtime: &dyn Runtime, id: i64) -> anyhow::Result<Option<AllowedRule>>;
}

/// What the engine observes of each node beside the domain: what its hooks deduced, what its mod
/// reported, the context each task used and the wait a node reported. Kept in memory while the
/// daemon runs; the screens read it next to the projection.
pub trait LiveApi: Send + Sync {
    fn node(&self, runtime: &dyn Runtime, session: &str) -> Option<Node>;
    fn nodes(&self, runtime: &dyn Runtime) -> Vec<Node>;
    fn threads(&self, runtime: &dyn Runtime, session: &str) -> Option<Vec<Activity>>;
    fn closed_sessions(&self, runtime: &dyn Runtime) -> Vec<String>;
    /// A hook event, as the live view applies it.
    fn apply(&self, runtime: &dyn Runtime, event: &Event);
    /// What `session`'s mod reported, as the screens read it.
    fn session(&self, runtime: &dyn Runtime, session: &str) -> Option<SessionReading>;
    /// Asks `session`'s node, through its mod, to cut its turn; false when no connection reaches it.
    fn interrupt(&self, runtime: &dyn Runtime, session: &str) -> bool;
    /// A sandbox daemon lets its operator reach any probe node.
    fn sandbox(&self, runtime: &dyn Runtime) -> bool;
    /// A client was seen at its desk now.
    fn touch(&self, runtime: &dyn Runtime, client: &str);
    fn touched_since(&self, runtime: &dyn Runtime, clients: &[&str], since: Millis) -> bool;
    /// The usage limit every node waits for, while one holds.
    fn usage(&self, runtime: &dyn Runtime) -> Option<Exhausted>;
    fn usage_changes(&self, runtime: &dyn Runtime) -> tokio::sync::watch::Receiver<Option<Exhausted>>;
    /// Wakes on every change the hooks make.
    fn hooks_changes(&self, runtime: &dyn Runtime) -> tokio::sync::watch::Receiver<()>;
    /// The sessions the hooks changed since `cursor`.
    fn hooks_cursor(&self, runtime: &dyn Runtime) -> Cursor;
    fn hooks_since(&self, runtime: &dyn Runtime, cursor: &Cursor) -> Delta<String>;
    /// Wakes on every report of a node's mod.
    fn reports_changes(&self, runtime: &dyn Runtime) -> tokio::sync::watch::Receiver<()>;
    /// The sessions whose mods reported since `cursor`.
    fn reports_cursor(&self, runtime: &dyn Runtime) -> Cursor;
    fn reports_since(&self, runtime: &dyn Runtime, cursor: &Cursor) -> Delta<String>;
    /// How far the contexts the tasks used changed, counted.
    fn contexts_version(&self, runtime: &dyn Runtime) -> u64;
    /// The tasks whose context changed since `cursor`.
    fn contexts_cursor(&self, runtime: &dyn Runtime) -> Cursor;
    fn contexts_since(&self, runtime: &dyn Runtime, cursor: &Cursor) -> Delta<TaskId>;
    fn context_used(&self, runtime: &dyn Runtime, task: &TaskId) -> Option<Used>;
    /// Where the Agent SDK node `instance` of `provider` reports what it waits for.
    fn wait_file(&self, runtime: &dyn Runtime, provider: Provider, instance: &str) -> Option<PathBuf>;
}

/// The provider accounts as Settings reads and changes them. Answers are `(status, body)` as the
/// route returns them; `provider_accounts` says whether the client reads every provider.
#[allow(clippy::too_many_arguments)]
pub trait AccountsApi: Send + Sync {
    fn read(&self, runtime: &dyn Runtime, home: &Path, lang: Lang, provider_accounts: bool) -> (u16, Value);
    fn write(&self, runtime: &dyn Runtime, home: &Path, body: &Value, lang: Lang, provider_accounts: bool) -> (u16, Value);
    /// Checks every account's sign-in, unless a check is recent enough.
    fn check(&self, runtime: &dyn Runtime, home: &Path);
    /// Checks every account's sign-in now, as after the user signs one in.
    fn check_now(&self, runtime: &dyn Runtime, home: &Path);
    fn refresh_models(&self, runtime: &dyn Runtime, home: &Path, key: &AccountKey) -> Result<(), String>;
    fn select_for_active_model(&self, runtime: &dyn Runtime, task: &TaskId, intent: &TaskSelection) -> Result<SelectionDecision, DomainError>;
    fn connect(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, provider: Provider, authorize: Authorize<'_>, lang: Lang)
        -> Result<(), AccountRefusal>;
    fn change(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, provider: Provider, change: &AccountChange, authorize: Authorize<'_>, lang: Lang)
        -> Result<(), AccountRefusal>;
    fn set_credential(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, key: &AccountKey, secret: Option<alondra_core::antigravity_account::Secret>,
        authorize: Authorize<'_>, lang: Lang) -> Result<(), AccountRefusal>;
    fn set_model_profile(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, provider: Provider, profile: ModelProfile, policy: &RoleModelPolicy,
        expected_config_version: &str, authorize: Authorize<'_>, lang: Lang) -> Result<(), AccountRefusal>;
    fn set_order(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, order: &[AccountKey], expected_config_version: &str, authorize: Authorize<'_>, lang: Lang)
        -> Result<(), AccountRefusal>;
}

/// What the user and Alondra say to a node, and how the outbox words it.
#[allow(clippy::too_many_arguments)]
pub trait MessagesApi: Send + Sync {
    /// A message `user` sends to `agent`, its body as the client sent it; `None` when the body is
    /// not a message.
    fn deliver<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, user: &'a Actor, name: &'a str, agent: &'a AgentId, body: Value, as_user: bool)
        -> Option<Pending<'a, Result<Value, DomainError>>>;
    /// The same message relayed from a paired device.
    fn relayed<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, sender: &'a str, agent: AgentId, body: Option<&'a Value>)
        -> Pending<'a, Result<Value, DomainError>>;
    /// A message from a paired device for `task`, kept until its node can take it.
    fn relayed_to_task(&self, runtime: &dyn Runtime, home: &Path, sender: &str, task: &TaskId, body: Option<&Value>) -> Result<Value, DomainError>;
    /// Asks the node of `pr`'s task to resolve its conflicts.
    fn resolve_conflicts<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, user: &'a Actor, name: &'a str, pr: &'a PrId)
        -> Pending<'a, Result<Value, DomainError>>;
    /// Tells the Agent SDK node of `task`, as an input of its lifecycle.
    fn tell_sdk_node(&self, runtime: &dyn Runtime, task: &TaskId, author: &str, text: &str, command_id: &str, from_user: bool, answers: Option<InteractionId>)
        -> Result<(), DomainError>;
    /// The task and node a `claude --bg` session moved to the Agent SDK continues as.
    fn successor(&self, runtime: &dyn Runtime, agent: &AgentId) -> Option<(TaskId, AgentId)>;
    /// The text a node reads for an outbox message.
    fn render(&self, state: &DomainState, message: &OutboxMessage) -> String;
    /// Whether the user wrote the outbox message.
    fn from_user(&self, state: &DomainState, message: &OutboxMessage) -> bool;
}

/// The orchestrator's tools and reads, and the launch plans and choices a request carries.
pub trait ToolsApi: Send + Sync {
    /// The orchestrator holding `bearer`, which may do `what`.
    fn orchestrator(&self, runtime: &dyn Runtime, bearer: Option<&str>, what: Option<&str>) -> Result<Actor, DomainError>;
    /// Each tool reads its request first; its work runs once the operation is recorded.
    fn launch(&self, runtime: Arc<dyn Runtime>, home: PathBuf, request: &Value) -> Result<ToolWork, serde_json::Error>;
    fn set_launch_plan(&self, runtime: Arc<dyn Runtime>, home: PathBuf, request: &Value) -> Result<ToolWork, serde_json::Error>;
    fn update_launch_plan(&self, runtime: Arc<dyn Runtime>, home: PathBuf, request: &Value) -> Result<ToolWork, serde_json::Error>;
    fn send(&self, runtime: Arc<dyn Runtime>, home: PathBuf, actor: Actor, request: &Value) -> Result<ToolWork, serde_json::Error>;
    fn interrupt(&self, runtime: Arc<dyn Runtime>, request: &Value) -> Result<ToolWork, serde_json::Error>;
    fn get_launch_plan(&self, runtime: &dyn Runtime, task: &TaskId) -> Result<Value, DomainError>;
    /// Records the tool operation `kind` of the orchestrator holding `bearer`, then runs `work`
    /// once: a retry with the same identity gets the first answer.
    fn tracked(&self, runtime: Arc<dyn Runtime>, bearer: String, identity: Option<String>, kind: &'static str, request: Value, work: ToolWork)
        -> Pending<'static, Result<Value, DomainError>>;
    fn tasks_in_progress(&self, runtime: &dyn Runtime, machine: Value, usage: Value) -> Result<Value, DomainError>;
    fn node(&self, runtime: &dyn Runtime, key: &str) -> Result<Value, DomainError>;
    /// A page of the context of the task whose node holds `bearer`.
    fn task_context(&self, runtime: &dyn Runtime, bearer: &str, query: TaskContextQuery) -> Result<Value, DomainError>;
    /// Starts the stopped `task` again, as the orchestrator would.
    fn relaunch<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, task: &'a TaskId) -> Pending<'a, Result<Value, DomainError>>;
    /// A paid account the user named for a request is the user's own grant.
    fn authorize_user_request_selection(&self, runtime: &dyn Runtime, resolved: &mut Resolved, explicit_account: bool);
    fn validate_request_selection(&self, runtime: &dyn Runtime, resolved: &Resolved) -> Result<(), DomainError>;
    fn validate_launch_plan(&self, runtime: &dyn Runtime, home: &Path, request: &RequestId, plan: &LaunchPlan) -> Result<(), DomainError>;
    /// Puts the project a launch plan's request names into `command`.
    fn stamp_project(&self, runtime: &dyn Runtime, home: &Path, command: &mut Command);
}

/// The pull requests as GitHub reports them and the domain records them.
pub trait PullRequestsApi: Send + Sync {
    /// Reads from GitHub every PR of `task` a node was seen opening and makes it the task's.
    fn catch_up_task<'a>(&'a self, runtime: &'a dyn Runtime, github: &'a crate::github::GitHub, source: &'a dyn crate::github::PrObserver, task: &'a TaskId, is_final: bool)
        -> Pending<'a, anyhow::Result<()>>;
    /// Records what GitHub said of `pr`.
    fn observe(&self, runtime: &dyn Runtime, pr: &PrId, facts: &PrFacts) -> anyhow::Result<()>;
    fn record_verdict(&self, runtime: &dyn Runtime, pr: &PullRequest, verdict: MergeVerdict) -> anyhow::Result<()>;
    /// Why a result may not go out while one of its PRs is still a draft.
    fn draft_refusal<'a>(&'a self, state: &'a DomainState, targets: &'a [DeliveryTarget], merger: &'a dyn crate::github::PrMerger)
        -> Pending<'a, Option<DomainError>>;
    /// The task's worktrees with work not saved elsewhere, each with why.
    fn leftovers<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Pending<'a, Vec<(String, String)>>;
}

/// The MCP servers: the cards that wait for one, the checks of a project and the deletions.
pub trait McpApi: Send + Sync {
    fn connect<'a>(&'a self, runtime: &'a dyn Runtime, target: &'a McpTarget) -> Pending<'a, Result<Opened, DomainError>>;
    fn give_back<'a>(&'a self, runtime: Arc<dyn Runtime>, target: &'a McpTarget, address: String) -> Pending<'a, Result<(), DomainError>>;
    fn reconnect(&self, runtime: &dyn Runtime, target: &McpTarget) -> Result<usize, DomainError>;
    fn sign_out(&self, runtime: &dyn Runtime, target: &McpTarget) -> Result<String, DomainError>;
    /// Checks `project` in the background through the Agent SDK host; false when it is already
    /// being checked or there is no host. `done` runs once the result is in `probes`.
    fn check(&self, runtime: &dyn Runtime, probes: Arc<McpProbes>, project: String, directory: PathBuf, done: Box<dyn FnOnce() + Send>) -> bool;
    /// Deletes the server `subject` names; `Err` carries its name when it is known.
    fn remove(&self, runtime: &dyn Runtime, nodes: &[McpNodeReading], probes: &McpProbes, projects: &[ProjectEntry], subject: &str)
        -> Result<String, (Option<String>, McpRefused)>;
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
    pub live: Arc<dyn LiveApi>,
    pub accounts: Arc<dyn AccountsApi>,
    pub messages: Arc<dyn MessagesApi>,
    pub tools: Arc<dyn ToolsApi>,
    pub pull_requests: Arc<dyn PullRequestsApi>,
    pub mcp: Arc<dyn McpApi>,
    pub sdk: Arc<dyn SdkApi>,
    pub admission: Arc<dyn AdmissionApi>,
    pub orchestrator: Arc<dyn OrchestratorApi>,
}

impl DomainApi {
    pub fn of(registry: &Registry) -> Self {
        Self {
            runtime: registry.port::<dyn RuntimeApi>().expect("daemon-engine gives the registry the runtime's operations"),
            live: registry.port::<dyn LiveApi>().expect("daemon-engine gives the registry the live view"),
            accounts: registry.port::<dyn AccountsApi>().expect("daemon-engine gives the registry the accounts"),
            messages: registry.port::<dyn MessagesApi>().expect("daemon-engine gives the registry the messages to nodes"),
            tools: registry.port::<dyn ToolsApi>().expect("daemon-engine gives the registry the orchestrator's tools"),
            pull_requests: registry.port::<dyn PullRequestsApi>().expect("daemon-engine gives the registry the pull requests"),
            mcp: registry.port::<dyn McpApi>().expect("daemon-engine gives the registry the MCP servers"),
            sdk: registry.port::<dyn SdkApi>().expect("engine-sdk gives the registry the schedules and the Agent SDK"),
            admission: registry.port::<dyn AdmissionApi>().expect("engine-admission gives the registry the gate's controls"),
            orchestrator: registry.port::<dyn OrchestratorApi>().expect("engine-orchestrator gives the registry the orchestrator"),
        }
    }
}
