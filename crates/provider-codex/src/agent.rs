//! Codex, through its app-server host. The engine reaches this provider only through the port `AgentProvider`, which
//! `wire` adds to the registry.
use crate::CodexWorker as W;
use alondra_agent_application::{InputMetadata, ObservationNormalizer};
use alondra_engine_ports::Registry;
use alondra_engine_ports::agent_host::{EndedHost, HostStop};
use alondra_engine_ports::agent_workers::{AgentProvider, AgentProviders, AgentWorker, EndedJournal, Found, NativeLiveness, Provider, Retained, RetainedEvent, WorkerConfiguration};
use alondra_foundation::CommandId;
use alondra_foundation::clock::Bell;
use alondra_provider_contract::{AdapterError, Handoff, HandoffProof, OrderedObservation, PreparedCommand, ProviderBinding};
use alondra_store::{AdapterState, CommittedCommand, CommittedControl, CommittedObservation, ControlAttempt, DispatchAttempt, OwnerGrant};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Adds Codex to the providers the engine runs agents on.
pub fn wire(registry: &Registry) {
    AgentProviders::add(registry, Arc::new(Codex));
}

pub struct Codex;

impl AgentProvider for Codex {
    fn provider(&self) -> Provider { Provider::Codex }
    fn configured(&self, binding: ProviderBinding, configuration: WorkerConfiguration) -> Result<Box<dyn AgentWorker>, AdapterError> {
        Ok(Box::new(W::configured(binding, configuration)?))
    }
    fn conversation_session(&self, metadata: &[u8]) -> Option<String> { crate::records::conversation_session(metadata) }
    fn answered_by_event(&self, command: &[u8]) -> Option<CommandId> { crate::adapter::answered_by_event(command) }
    fn answered_control(&self, observation: &OrderedObservation) -> Option<CommandId> { crate::adapter::answered_control(observation) }
    fn native_exited(&self, observation: &OrderedObservation) -> bool { crate::adapter::native_exited(observation) }
    fn background_left(&self, checkpoint: &[u8]) -> Vec<String> { crate::normalizer::background_left(checkpoint) }
    fn sdk_message(&self, _: &OrderedObservation) -> Option<serde_json::Value> { None }
    fn native_message(&self, _: &OrderedObservation) -> Option<serde_json::Value> { None }
    fn signed_out(&self, _: &OrderedObservation) -> bool { false }
    fn session_failure(&self, observation: &OrderedObservation) -> Option<String> { crate::context::session_failure(observation).map(|failure| failure.to_string()) }
    fn account_refusal(&self, observation: &OrderedObservation) -> Option<&'static str> { crate::context::account_refusal(observation) }
    fn blocked_policy(&self, checkpoint: &[u8]) -> Option<String> { crate::normalizer::blocked_policy(checkpoint) }
}

impl AgentWorker for W {
    fn state_directory(&self) -> &std::path::Path { W::state_directory(self) }
    fn binding(&self) -> &ProviderBinding { W::binding(self) }
    fn owner_generation(&self) -> Result<u64, AdapterError> { W::owner_generation(self) }
    fn install_owner(&mut self, grant: OwnerGrant, state: AdapterState) -> Result<(), AdapterError> { W::install_owner(self, grant, state) }
    fn adopt_owner(&mut self, grant: OwnerGrant, state: AdapterState) -> Result<(), AdapterError> { W::adopt_owner(self, grant, state) }
    fn find_host(&mut self, grant: &OwnerGrant, state: &AdapterState) -> Result<Found, AdapterError> {
        Ok(match W::find_host(self, grant, state)? {
            crate::Found::Live => Found::Live,
            crate::Found::Ended(journal) => Found::Ended(Box::new(journal)),
        })
    }
    fn take_ended(&mut self) -> Option<EndedHost> { W::take_ended(self) }
    fn relaunch(&mut self, grant: OwnerGrant, state: AdapterState, base: u64, resume: bool, opens_on_control: bool, environment: BTreeMap<String, String>) -> Result<(), AdapterError> {
        W::relaunch(self, grant, state, base, resume, opens_on_control, environment)
    }
    fn reconcile(&mut self, command: &[u8]) -> Result<Handoff, AdapterError> { W::reconcile(self, command) }
    fn archived_handoffs(&self, grant: &OwnerGrant, state: &AdapterState, commands: &[&[u8]]) -> Result<Vec<HandoffProof>, AdapterError> { W::archived_handoffs(self, grant, state, commands) }
    fn ended_handoffs(&self, grant: &OwnerGrant, state: &AdapterState, commands: &[&[u8]]) -> Result<Vec<HandoffProof>, AdapterError> { W::ended_handoffs(self, grant, state, commands) }
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> { W::stage_input(self, metadata) }
    fn prepare(&mut self, attempt: &DispatchAttempt, metadata: InputMetadata<'_, '_>) -> Result<PreparedCommand, AdapterError> { W::prepare(self, attempt, metadata) }
    fn submit(&mut self, command: &CommittedCommand) -> Result<Handoff, AdapterError> { W::submit(self, command) }
    fn stopper(&self) -> Option<HostStop> { W::stopper(self) }
    fn retired_host(&mut self) -> Option<PathBuf> { W::retired_host(self) }
    fn host_group(&self) -> Option<i32> { W::host_group(self) }
    fn runs_another_release(&self) -> bool { W::runs_another_release(self) }
    fn host_exited(&mut self) -> bool { W::host_exited(self) }
    fn prepare_control(&mut self, attempt: &ControlAttempt) -> Result<PreparedCommand, AdapterError> { W::prepare_control(self, attempt) }
    fn submit_control(&mut self, command: &CommittedControl) -> Result<Handoff, AdapterError> { W::submit_control(self, command) }
    fn stop(&mut self) -> Result<(), AdapterError> { W::stop(self) }
    fn poll(&mut self) -> Result<Option<OrderedObservation>, AdapterError> { W::poll(self) }
    fn readable(&self, timeout: Duration) -> Result<bool, AdapterError> { W::readable(self, timeout) }
    fn readable_unless_rung(&self, timeout: Duration, bell: &Bell) -> Result<bool, AdapterError> { W::readable_unless_rung(self, timeout, bell) }
    fn normalizer(&self) -> &dyn ObservationNormalizer { W::normalizer(self) }
    fn acknowledge(&mut self, observation: &CommittedObservation) -> Result<(), AdapterError> { W::acknowledge(self, observation) }
    fn continue_session(&mut self, session: &str) -> Result<(), AdapterError> { W::continue_session(self, session) }
    fn discard_prepared(&mut self) {}
    fn supports_plan_supersession(&self) -> bool { W::supports_plan_supersession(self) }
    fn native_liveness(&mut self) -> Result<Option<NativeLiveness>, AdapterError> { W::native_liveness(self).map(Some) }
}

impl EndedJournal for crate::EndedJournal {
    fn events(&self) -> Vec<RetainedEvent> { crate::EndedJournal::events(self).to_vec() }
    fn retained(&self, command: &[u8]) -> Result<Retained, AdapterError> { crate::EndedJournal::retained(self, command) }
    fn last_cursor(&self) -> u64 { crate::EndedJournal::last_cursor(self) }
}
