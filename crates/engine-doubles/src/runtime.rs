//! A runtime that is not the kernel's: it hands every call to the runtime it wraps. A test runs a
//! piece of the engine on it to show the piece needs only the `Runtime` context, never the
//! runtime's own type.
use alondra_domain_rules::*;
use alondra_engine_ports::journal::OwnerJournal;
use alondra_engine_ports::journal::owner::StagedOutcome;
use alondra_engine_ports::journal::{Journal, ReadTransaction, RequestLaunch, StoreError, TransportIdentity};
use alondra_engine_ports::{Registry, Runtime};
use alondra_foundation::runtime_mutex::RuntimeMutex;
use alondra_store::{AuthorityScope, Committed, ContextPage, ContextRef};
use alondra_foundation::principal::{Capability, Principal};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

pub struct Forwarded(pub Arc<dyn Runtime>);

impl Runtime for Forwarded {
    fn ports(&self) -> &Registry {
        self.0.ports()
    }
    fn extensions(&self) -> &alondra_engine_ports::extensions::Extensions {
        self.0.extensions()
    }
    fn home(&self) -> &Path {
        self.0.home()
    }
    fn state(&self) -> Arc<DomainState> {
        self.0.state()
    }
    fn read(&self) -> Arc<ReadSnapshot> {
        self.0.read()
    }
    fn live_read(&self) -> ReadTransaction<'_> {
        self.0.live_read()
    }
    fn store(&self) -> &RuntimeMutex<Box<dyn Journal>> {
        self.0.store()
    }
    fn subscribe(&self) -> watch::Receiver<Revision> {
        self.0.subscribe()
    }
    fn subscribe_snapshots(&self) -> watch::Receiver<u64> {
        self.0.subscribe_snapshots()
    }
    fn subscribe_schedules(&self) -> watch::Receiver<Revision> {
        self.0.subscribe_schedules()
    }
    fn wake_subscribers(&self) {
        self.0.wake_subscribers();
    }
    fn execute_as_system(&self, envelope: CommandEnvelope) -> Result<CommandOutcome, StoreError> {
        self.0.execute_as_system(envelope)
    }
    fn execute_authorized(&self, actor: &Actor, envelope: &CommandEnvelope, transport: Option<TransportIdentity<'_>>, launch: Option<&RequestLaunch>, authorize: &mut dyn FnMut() -> Result<(), StoreError>) -> Result<CommandOutcome, StoreError> {
        self.0.execute_authorized(actor, envelope, transport, launch, authorize)
    }
    fn own_with(&self, scope: AuthorityScope, stage: &mut dyn FnMut(&mut dyn OwnerJournal) -> Result<(), StoreError>) -> Result<Vec<StagedOutcome>, StoreError> {
        self.0.own_with(scope, stage)
    }
    fn own_projected_with(&self, scope: AuthorityScope, stage: &mut dyn FnMut(&mut dyn OwnerJournal) -> Result<(), StoreError>, project: &mut dyn FnMut(&mut dyn Journal) -> Result<(), anyhow::Error>) -> Result<Vec<StagedOutcome>, StoreError> {
        self.0.own_projected_with(scope, stage, project)
    }
    fn take_seals(&self, task: &TaskId, node: &AgentId) -> Committed {
        self.0.take_seals(task, node)
    }
    fn wait_for_seals(&self, task: &TaskId, node: &AgentId, timeout: Duration, stopped: &dyn Fn() -> bool) -> Committed {
        self.0.wait_for_seals(task, node, timeout, stopped)
    }
    fn pause_for_seals(&self, timeout: Duration, stopped: &dyn Fn() -> bool) {
        self.0.pause_for_seals(timeout, stopped);
    }
    fn forget_sealed_observations(&self, task: &TaskId) {
        self.0.forget_sealed_observations(task);
    }
    fn held(&self) -> &Mutex<BTreeMap<AgentId, Vec<String>>> {
        self.0.held()
    }
    fn system_command(&self, envelope: CommandEnvelope) -> Result<CommandOutcome, DomainError> {
        self.0.system_command(envelope)
    }
    fn wake_seals(&self) {
        self.0.wake_seals();
    }
    fn launch_plan(&self, task: &TaskId) -> Option<LaunchPlan> {
        self.0.launch_plan(task)
    }
    fn bind_agent(&self, session_id: &str) -> Result<Capability, StoreError> {
        self.0.bind_agent(session_id)
    }
    fn revoke_agent(&self, session: &str) -> Result<(), StoreError> {
        self.0.revoke_agent(session)
    }
    fn admit_change(&self, bearer: &str, what: &str) -> Result<Principal, StoreError> {
        self.0.admit_change(bearer, what)
    }
    fn resolve_bearer(&self, bearer: &str) -> Result<Principal, StoreError> {
        self.0.resolve_bearer(bearer)
    }
    fn agent_capability(&self, session: &str) -> Option<[u8; 32]> {
        self.0.agent_capability(session)
    }
    fn revoke_capability(&self, digest: &[u8; 32]) -> Result<(), StoreError> {
        self.0.revoke_capability(digest)
    }
    fn transport_receipt(&self, identity: TransportIdentity<'_>, authorize: &mut dyn FnMut() -> Result<(), StoreError>) -> Result<Option<CommandOutcome>, StoreError> {
        self.0.transport_receipt(identity, authorize)
    }
    fn before_acknowledge(&self) {
        self.0.before_acknowledge();
    }
    fn read_current_task_context(&self, bearer: &str, task: &TaskId, after: Option<&ContextRef>, limit: usize) -> Result<ContextPage, DomainError> {
        self.0.read_current_task_context(bearer, task, after, limit)
    }
}
