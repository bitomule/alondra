//! The port through which the engine runs a task's agent on its provider: each provider's adapter
//! configures the worker that talks to its host and reads what its host writes. The engine asks
//! the registry for the provider a binding names and never sees which crate answers.
use crate::agent_host::{AgentHost, EndedHost, HostStop, VerifiedArtifact};
use alondra_agent_application::{InputMetadata, ObservationNormalizer};
pub use alondra_core::accounts::Provider;
use alondra_foundation::CommandId;
use alondra_foundation::clock::Bell;
use alondra_provider_contract::{AdapterError, Handoff, HandoffProof, OrderedObservation, PreparedCommand, ProviderBinding};
use alondra_store::{AdapterState, CommittedCommand, CommittedControl, CommittedObservation, ControlAttempt, DispatchAttempt, OwnerGrant};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

/// Configuration carries no dispatch or process-launch authority.
pub struct WorkerConfiguration {
    pub artifact: VerifiedArtifact,
    pub state_directory: PathBuf,
    pub host_environment: BTreeMap<String, String>,
    pub native_executable: PathBuf,
    pub native_directory: PathBuf,
    pub native_environment: BTreeMap<String, String>,
    pub profile_id: String,
    pub account_id: String,
    pub host_release: String,
    pub io_timeout: Duration,
    /// A Seatbelt profile the whole host runs under, the native process included.
    pub confinement: Option<PathBuf>,
    /// Starts, adopts and reclaims the host process; a scenario swaps it.
    pub hosts: Arc<dyn AgentHost>,
    /// What a task's native agent loads besides the user's own settings, skills, plugins and MCP.
    pub worker: Option<WorkerProfile>,
}

/// Alondra's plugin directories and launch settings for a task's native agent, which loads them
/// on top of every setting source of the user's default profile.
#[derive(Clone, Debug, serde::Serialize)]
pub struct WorkerProfile {
    pub plugins: Vec<PathBuf>,
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// The model, effort and denied tools the task was launched with; absent, the provider's own
    /// defaults apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_context: Option<String>,
}

/// The last durable receipt the ended host kept for a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retained {
    /// The input reached the native session (or the decision was applied).
    Offered,
    /// Refused before anything was offered.
    Refused,
    /// The host took the command and may have acted on it: its outcome is unknown.
    AdmittedOnly,
    /// The host never took the command: nothing of it reached the native session.
    Absent,
}

/// One unacknowledged event of the ended host, with its exact payload bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedEvent {
    pub cursor: u64,
    pub payload: Vec<u8>,
}

/// What the journal of a host that ended proves.
pub trait EndedJournal: Send {
    fn events(&self) -> Vec<RetainedEvent>;
    fn retained(&self, command: &[u8]) -> Result<Retained, AdapterError>;
    fn last_cursor(&self) -> u64;
}

/// What a worker finds where its host ran.
pub enum Found {
    /// A host still runs: adopt it.
    Live,
    /// No host runs any more: what its journal proves, for a relaunch.
    Ended(Box<dyn EndedJournal>),
}

/// Kernel-backed native availability, independent of the host's control socket. A replacement
/// opens its native process lazily, so absence before its first input is not a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeLiveness {
    NotStarted,
    Alive(alondra_process_group::Member),
    Lost,
}

/// The worker of one execution: it owns the connection to its host.
pub trait AgentWorker: Send {
    fn state_directory(&self) -> &Path;
    fn binding(&self) -> &ProviderBinding;
    fn owner_generation(&self) -> Result<u64, AdapterError>;
    fn install_owner(&mut self, grant: OwnerGrant, state: AdapterState) -> Result<(), AdapterError>;
    fn adopt_owner(&mut self, grant: OwnerGrant, state: AdapterState) -> Result<(), AdapterError>;
    fn find_host(&mut self, grant: &OwnerGrant, state: &AdapterState) -> Result<Found, AdapterError>;
    fn take_ended(&mut self) -> Option<EndedHost>;
    #[allow(clippy::too_many_arguments)]
    fn relaunch(&mut self, grant: OwnerGrant, state: AdapterState, base: u64, resume: bool, opens_on_control: bool, environment: BTreeMap<String, String>) -> Result<(), AdapterError>;
    fn reconcile(&mut self, command: &[u8]) -> Result<Handoff, AdapterError>;
    fn archived_handoffs(&self, grant: &OwnerGrant, state: &AdapterState, commands: &[&[u8]]) -> Result<Vec<HandoffProof>, AdapterError>;
    fn ended_handoffs(&self, grant: &OwnerGrant, state: &AdapterState, commands: &[&[u8]]) -> Result<Vec<HandoffProof>, AdapterError>;
    /// Resumes the native session `session`; a provider that cannot refuses it as unsupported.
    fn continue_session(&mut self, session: &str) -> Result<(), AdapterError>;
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError>;
    fn prepare(&mut self, attempt: &DispatchAttempt, metadata: InputMetadata<'_, '_>) -> Result<PreparedCommand, AdapterError>;
    fn submit(&mut self, command: &CommittedCommand) -> Result<Handoff, AdapterError>;
    /// Forgets a dispatch it prepared and will not submit.
    fn discard_prepared(&mut self);
    fn stopper(&self) -> Option<HostStop>;
    fn retired_host(&mut self) -> Option<PathBuf>;
    fn host_group(&self) -> Option<i32>;
    fn supports_plan_supersession(&self) -> bool;
    fn runs_another_release(&self) -> bool;
    fn host_exited(&mut self) -> bool;
    /// Whether the native process runs, for a provider whose native runs as a process of its own;
    /// `None` for one whose native lives inside its host.
    fn native_liveness(&mut self) -> Result<Option<NativeLiveness>, AdapterError>;
    fn prepare_control(&mut self, attempt: &ControlAttempt) -> Result<PreparedCommand, AdapterError>;
    fn submit_control(&mut self, command: &CommittedControl) -> Result<Handoff, AdapterError>;
    fn stop(&mut self) -> Result<(), AdapterError>;
    fn poll(&mut self) -> Result<Option<OrderedObservation>, AdapterError>;
    fn readable(&self, timeout: Duration) -> Result<bool, AdapterError>;
    fn readable_unless_rung(&self, timeout: Duration, bell: &Bell) -> Result<bool, AdapterError>;
    fn normalizer(&self) -> &dyn ObservationNormalizer;
    fn acknowledge(&mut self, observation: &CommittedObservation) -> Result<(), AdapterError>;
}

/// One provider: how its workers are configured and how its host's frames and records read.
pub trait AgentProvider: Send + Sync {
    fn provider(&self) -> Provider;
    fn configured(&self, binding: ProviderBinding, configuration: WorkerConfiguration) -> Result<Box<dyn AgentWorker>, AdapterError>;
    /// The native session a conversation's metadata records.
    fn conversation_session(&self, metadata: &[u8]) -> Option<String>;
    /// The command id of a recorded control the host answers with an event of its own.
    fn answered_by_event(&self, command: &[u8]) -> Option<CommandId>;
    /// The control a host event answers, when it is the result of one.
    fn answered_control(&self, observation: &OrderedObservation) -> Option<CommandId>;
    /// Whether the host observed the end of its native process.
    fn native_exited(&self, observation: &OrderedObservation) -> bool;
    /// The background work a checkpoint says the native session left running.
    fn background_left(&self, checkpoint: &[u8]) -> Vec<String>;
    /// The Agent SDK message a host event carries, for a provider whose host runs the Agent SDK.
    fn sdk_message(&self, observation: &OrderedObservation) -> Option<serde_json::Value>;
    /// The native message the live view reads in place of the host's frame, for a provider whose
    /// frames only its adapter reads.
    fn native_message(&self, observation: &OrderedObservation) -> Option<serde_json::Value>;
    /// Whether the native refused its input because it is signed out.
    fn signed_out(&self, observation: &OrderedObservation) -> bool;
    /// Why the native session ended without being asked to, as its host said it.
    fn session_failure(&self, observation: &OrderedObservation) -> Option<String>;
    /// A refusal of the account before the provider took the input.
    fn account_refusal(&self, observation: &OrderedObservation) -> Option<&'static str>;
    /// The policy a checkpoint says blocked the session.
    fn blocked_policy(&self, checkpoint: &[u8]) -> Option<String>;
}

/// Every provider the root wired, by the provider it is.
#[derive(Default)]
pub struct AgentProviders(RwLock<BTreeMap<Provider, Arc<dyn AgentProvider>>>);

impl AgentProviders {
    /// Adds `provider` to the registry's providers, in place of any of the same provider.
    pub fn add(registry: &crate::Registry, provider: Arc<dyn AgentProvider>) {
        let providers = registry.port::<AgentProviders>().unwrap_or_else(|| {
            let providers = Arc::new(AgentProviders::default());
            registry.provide::<AgentProviders>(providers.clone());
            providers
        });
        providers.0.write().unwrap_or_else(PoisonError::into_inner).insert(provider.provider(), provider);
    }

    /// The providers no adapter was wired for.
    pub fn missing(&self) -> Vec<Provider> {
        let wired = self.0.read().unwrap_or_else(PoisonError::into_inner);
        Provider::ALL.into_iter().filter(|provider| !wired.contains_key(provider)).collect()
    }
}

/// The registry's adapter of `provider`, or an error naming it when none was wired.
pub fn of(registry: &crate::Registry, provider: Provider) -> std::io::Result<Arc<dyn AgentProvider>> {
    registry.port::<AgentProviders>()
        .and_then(|providers| providers.0.read().unwrap_or_else(PoisonError::into_inner).get(&provider).cloned())
        .ok_or_else(|| std::io::Error::other(format!("no provider for {} is wired", provider.display_name())))
}

/// The registry's adapter of the provider `binding` names.
pub fn of_binding(registry: &crate::Registry, binding: &ProviderBinding) -> std::io::Result<Arc<dyn AgentProvider>> {
    let provider = Provider::from_driver(binding.driver()).ok_or_else(|| std::io::Error::other(format!("no provider drives {}", binding.driver())))?;
    of(registry, provider)
}

#[cfg(test)]
#[path = "agent_workers_tests.rs"]
mod tests;
