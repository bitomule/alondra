use super::ClaudeConfiguration;
use alondra_agent_application::{InputMetadata, Normalization, ObservationMetadata, ObservationNormalizer};
use alondra_agent_runtime::owned_host::HostStop;
use alondra_core::accounts::Provider;
use alondra_foundation::CommandId;
use alondra_provider_claude::ClaudeWorker;
use alondra_provider_codex::CodexWorker;
use alondra_provider_contract::{AdapterError, Handoff, OrderedObservation, PreparedCommand, ProviderBinding};
use alondra_store::{AdapterState, CommittedCommand, CommittedControl, CommittedObservation, ControlAttempt, DispatchAttempt, OwnerGrant};
use std::collections::BTreeMap;
use std::time::Duration;

pub enum Worker { Claude(ClaudeWorker), Codex(CodexWorker) }
pub(super) enum Found { Live, Ended(EndedJournal) }
pub(super) enum EndedJournal { Claude(alondra_provider_claude::EndedJournal), Codex(alondra_provider_codex::EndedJournal) }
pub(super) struct RetainedEvent { pub cursor: u64, pub payload: Vec<u8> }
impl EndedJournal {
    pub fn events(&self) -> Vec<RetainedEvent> {
        match self {
            Self::Claude(journal) => journal.events().iter().map(|event| RetainedEvent { cursor: event.cursor, payload: event.payload.clone() }).collect(),
            Self::Codex(journal) => journal.events().iter().map(|event| RetainedEvent { cursor: event.cursor, payload: event.payload.clone() }).collect(),
        }
    }
    pub fn retained(&self, bytes: &[u8]) -> Result<alondra_provider_claude::Retained, AdapterError> {
        Ok(match self {
            Self::Claude(journal) => return journal.retained(bytes),
            Self::Codex(journal) => match journal.retained(bytes)? {
                alondra_provider_codex::Retained::Offered => alondra_provider_claude::Retained::Offered,
                alondra_provider_codex::Retained::Refused => alondra_provider_claude::Retained::Refused,
                alondra_provider_codex::Retained::AdmittedOnly => alondra_provider_claude::Retained::AdmittedOnly,
                alondra_provider_codex::Retained::Absent => alondra_provider_claude::Retained::Absent,
            },
        })
    }
    pub fn last_cursor(&self) -> u64 { match self { Self::Claude(journal) => journal.last_cursor(), Self::Codex(journal) => journal.last_cursor() } }
}
macro_rules! dispatch {
    ($self:expr, $method:ident $(, $arg:expr)*) => { match $self { Worker::Claude(worker) => worker.$method($($arg),*), Worker::Codex(worker) => worker.$method($($arg),*) } };
}
impl Worker {
    pub fn configured(binding: ProviderBinding, configuration: ClaudeConfiguration) -> Result<Self, AdapterError> {
        match Provider::from_driver(binding.driver()) {
            Some(Provider::Claude) => ClaudeWorker::configured(binding, configuration).map(Self::Claude),
            Some(Provider::Codex) => {
                let configuration = alondra_provider_codex::CodexConfiguration {
                    artifact: configuration.artifact, state_directory: configuration.state_directory,
                    host_environment: configuration.host_environment, native_executable: configuration.native_executable,
                    native_directory: configuration.native_directory, native_environment: configuration.native_environment,
                    profile_id: configuration.profile_id, account_id: configuration.account_id,
                    host_release: configuration.host_release, io_timeout: configuration.io_timeout, confinement: configuration.confinement,
                    worker: configuration.worker.map(|profile| alondra_provider_codex::WorkerProfile { plugins: profile.plugins, settings: profile.settings, model: profile.model, effort: profile.effort, disallowed_tools: profile.disallowed_tools, handoff_context: profile.handoff_context }),
                };
                CodexWorker::configured(binding, configuration).map(Self::Codex)
            }
            None => Err(AdapterError::Unsupported),
        }
    }
    pub fn state_directory(&self)->&std::path::Path{dispatch!(self,state_directory)}
    pub fn provider(&self) -> Provider { match self { Self::Claude(_) => Provider::Claude, Self::Codex(_) => Provider::Codex } }
    pub fn binding(&self) -> &ProviderBinding { dispatch!(self, binding) }
    pub fn owner_generation(&self) -> Result<u64, AdapterError> { dispatch!(self, owner_generation) }
    pub fn install_owner(&mut self, grant: OwnerGrant, state: AdapterState) -> Result<(), AdapterError> { dispatch!(self, install_owner, grant, state) }
    pub fn adopt_owner(&mut self, grant: OwnerGrant, state: AdapterState) -> Result<(), AdapterError> { dispatch!(self, adopt_owner, grant, state) }
    pub fn find_host(&mut self, grant: &OwnerGrant, state: &AdapterState) -> Result<Found, AdapterError> {
        Ok(match self {
            Self::Claude(worker) => match worker.find_host(grant, state)? { alondra_provider_claude::Found::Live => Found::Live, alondra_provider_claude::Found::Ended(journal) => Found::Ended(EndedJournal::Claude(journal)) },
            Self::Codex(worker) => match worker.find_host(grant, state)? { alondra_provider_codex::Found::Live => Found::Live, alondra_provider_codex::Found::Ended(journal) => Found::Ended(EndedJournal::Codex(journal)) },
        })
    }
    pub fn relaunch(&mut self, grant: OwnerGrant, state: AdapterState, base: u64, resume: bool, environment: BTreeMap<String, String>) -> Result<(), AdapterError> { dispatch!(self, relaunch, grant, state, base, resume, environment) }
    pub fn reconcile(&mut self, bytes: &[u8]) -> Result<Handoff, AdapterError> { dispatch!(self, reconcile, bytes) }
    pub fn continue_session(&mut self, session: &str) -> Result<(), AdapterError> { dispatch!(self, continue_session, session) }
    pub fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> { dispatch!(self, stage_input, metadata) }
    pub fn prepare(&mut self, attempt: &DispatchAttempt, metadata: InputMetadata<'_, '_>) -> Result<PreparedCommand, AdapterError> { dispatch!(self, prepare, attempt, metadata) }
    pub fn submit(&mut self, command: &CommittedCommand) -> Result<Handoff, AdapterError> { dispatch!(self, submit, command) }
    pub fn stopper(&self) -> Option<HostStop> { dispatch!(self, stopper) }
    pub fn host_group(&self) -> Option<i32> { dispatch!(self, host_group) }
    pub fn supports_plan_supersession(&self) -> bool {
        match self { Self::Claude(_) => true, Self::Codex(worker) => worker.supports_plan_supersession() }
    }
    pub fn runs_another_release(&self) -> bool { dispatch!(self, runs_another_release) }
    pub fn host_exited(&mut self) -> bool { dispatch!(self, host_exited) }
    pub fn native_liveness(&mut self) -> Result<Option<alondra_provider_codex::NativeLiveness>, AdapterError> {
        match self { Self::Codex(worker) => worker.native_liveness().map(Some), Self::Claude(_) => Ok(None) }
    }
    pub fn prepare_control(&mut self, attempt: &ControlAttempt) -> Result<PreparedCommand, AdapterError> { dispatch!(self, prepare_control, attempt) }
    pub fn submit_control(&mut self, command: &CommittedControl) -> Result<Handoff, AdapterError> { dispatch!(self, submit_control, command) }
    pub fn stop(&mut self) -> Result<(), AdapterError> { dispatch!(self, stop) }
    pub fn poll(&mut self) -> Result<Option<OrderedObservation>, AdapterError> { dispatch!(self, poll) }
    pub fn readable(&self, timeout: Duration) -> Result<bool, AdapterError> { dispatch!(self, readable, timeout) }
    pub fn normalizer(&self) -> &Self { self }
    pub fn acknowledge(&mut self, observation: &CommittedObservation) -> Result<(), AdapterError> { dispatch!(self, acknowledge, observation) }
    pub fn background_left(&self, bytes: &[u8]) -> Vec<String> { match self { Self::Claude(_) => alondra_provider_claude::background_left(bytes), Self::Codex(_) => alondra_provider_codex::background_left(bytes) } }
    pub fn answered_control(&self, batch: &OrderedObservation) -> Option<CommandId> { match self { Self::Claude(_) => alondra_provider_claude::answered_control(batch), Self::Codex(_) => alondra_provider_codex::answered_control(batch) } }
    pub fn native_exited(&self, batch: &OrderedObservation) -> bool { match self { Self::Claude(_) => alondra_provider_claude::native_exited(batch), Self::Codex(_) => alondra_provider_codex::native_exited(batch) } }
    pub fn context_event(&self, batch: &OrderedObservation) -> Option<alondra_provider_claude::ContextEvent> {
        match self {
            Self::Claude(_) => alondra_provider_claude::context_event(batch),
            // Codex live scope binds current root thread, turn and native model before telemetry.
            Self::Codex(_) => None,
        }
    }
}
impl ObservationNormalizer for Worker {
    fn normalize(&self, batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Normalization, AdapterError> {
        match self { Self::Claude(worker) => worker.normalizer().normalize(batch, metadata), Self::Codex(worker) => worker.normalizer().normalize(batch, metadata) }
    }
    fn skip(&self, batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Normalization, AdapterError> {
        match self { Self::Claude(worker) => worker.normalizer().skip(batch, metadata), Self::Codex(worker) => worker.normalizer().skip(batch, metadata) }
    }
}
