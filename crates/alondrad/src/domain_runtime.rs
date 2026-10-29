//! Serialization boundary for the SQL domain and its authenticated producers.
use crate::locks::{Poisonable as _, PoisonableRw as _};
use crate::domain_authority::{Authority, Capability, Principal};
use crate::runtime_mutex::RuntimeMutex;
use crate::domain_store::{DomainStore, OwnerTransaction, StagedOutcome, StoreError, TransportIdentity};
use alondra_core::domain::*;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::watch;

pub struct DomainRuntime {
    pub(crate) read_work: crate::blocking_work::BlockingWork,
    pub(crate) producer_work: crate::blocking_work::BlockingWork,
    pub(crate) external_work: crate::blocking_work::BlockingWork,
    pub(crate) command_work: crate::blocking_work::BlockingWork,
    pub(crate) hook_work: crate::blocking_work::BlockingWork,
    #[cfg(test)]
    pub(crate) allow_always_wait: Mutex<Option<crate::allow_always::ConfirmationWait>>,
    pub(crate) guard_work: crate::blocking_work::BlockingWork,
    home: PathBuf,
    /// A holder that panics can leave it half-changed, so the daemon exits: the guard stays held
    /// after the SQLite commit while the snapshot is published, the commit acknowledged, its seals
    /// kept and the watchers woken; a fresh start rebuilds all of them from the database.
    pub(crate) store: RuntimeMutex<DomainStore>,
    /// The last committed state and the read index built from it. Readers take them without the
    /// store, a transaction or a copy; only a commit replaces them, while it still holds the store.
    /// A holder that panics leaves it whole, so it is recovered: its only write swaps one `Arc`,
    /// built outside it.
    snapshot: RwLock<Arc<ReadSnapshot>>,
    approval_home: PathBuf,
    /// A holder that panics leaves it whole, so it is recovered: each change is one insert, retain
    /// or remove; what it shares with the store changes under the store's guard, which exits.
    authority: RuntimeMutex<Authority>,
    /// A holder that panics leaves it whole, so it is recovered: one insert, remove or lookup each,
    /// also in `Operation::drop`, which runs while unwinding and must not panic again.
    operations_live: Mutex<std::collections::BTreeSet<(String,String)>>,
    operations_changed: watch::Sender<u64>,
    changed: watch::Sender<Revision>,
    schedule_changed: watch::Sender<Revision>,
    pub(crate) stored_reads: Arc<crate::service_files::StoredReads>,
    pub(crate) local_actions: crate::views::actions::LocalActions,
    pub(crate) live: crate::client_routes::Live,
    /// User messages for sessions in the user's own terminal, handed at their next Stop hook.
    /// A holder that panics leaves it whole, so it is recovered: one remove or lookup each.
    pub(crate) held:
        Mutex<std::collections::BTreeMap<alondra_core::domain::AgentId, Vec<String>>>,
    rejections: rejections::Rejections,
    /// Store seals of commits whose state readers already see, for the SDK owners to claim,
    /// send and acknowledge.
    pub(crate) seals: crate::sdk_owner::Seals,
    /// Set at startup only in a sandbox daemon whose configuration names one: otherwise no task
    /// ever runs through the SDK.
    sdk: std::sync::OnceLock<(crate::sdk_owner::SdkBackend, std::sync::Weak<DomainRuntime>)>,
    /// A holder that panics leaves it whole, so it is recovered: `add_sdk_owner` takes, partitions
    /// and puts back the list with only field and atomic reads in between; the hosts stop outside
    /// it.
    pub(crate) sdk_owners: Mutex<Vec<crate::sdk_owner::Owner>>,
    /// The executions the user asked to take over again, while that runs.
    /// A holder that panics leaves it whole, so it is recovered: a retain and then one push of a
    /// thread already spawned.
    pub(crate) retakes: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// The executions whose takeover found the disk full: taken over again once the machine watch
    /// measures space, never given up for it.
    /// A holder that panics leaves it whole, so it is recovered: one insert or one take.
    pub(crate) waiting_for_space: Mutex<std::collections::BTreeSet<TaskId>>,
    /// Makes every write of the capability registry fail as on a full disk.
    #[cfg(test)]
    pub(crate) full_disk: std::sync::atomic::AtomicBool,
    /// Each `claude --bg` node's move to the SDK, once this daemon has an SDK backend.
    pub(crate) handovers: crate::sdk_handover::Handovers,
    /// Each move of an SDK task to another account when its own reaches its usage limit.
    pub(crate) moves: crate::account_move::Moves,
    pub(crate) selection_changes: crate::provider_transition::Transitions,
    /// What each SDK task's conversation measured, to compact it while it rests.
    pub(crate) contexts: crate::idle_compaction::Contexts,
    #[cfg(test)]
    before_owner_publish: Mutex<Option<Box<dyn FnMut() + Send>>>,
    #[cfg(test)]
    before_next_commit: Mutex<Option<Box<dyn FnOnce(&DomainRuntime) + Send>>>,
    #[cfg(test)]
    before_acknowledge: Mutex<Option<Box<dyn FnMut() + Send>>>,
    #[cfg(test)]
    owner_failures: Mutex<Option<std::sync::mpsc::Sender<String>>>,
    /// The nodes whose driver a test steps by hand, with no owner thread: they count as owned.
    #[cfg(test)]
    pub(crate) driven_by_hand: Mutex<std::collections::BTreeSet<AgentId>>,
    /// How many read indexes publication built: one per commit that changed the domain.
    #[cfg(test)]
    pub(crate) index_builds: std::sync::atomic::AtomicU64,
}
/// What one retention run released: the provider evidence, and the rows each policy changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Retained {
    pub(crate) evidence: alondra_store::EvidenceRetention,
    pub(crate) tables: Vec<(&'static str, usize)>,
    /// Ended tasks whose cards were compacted.
    pub(crate) compacted: usize,
    /// Ended tasks folded into their metrics.
    pub(crate) folded: usize,
}

pub(crate) mod batch_operations;

impl DomainRuntime {
    pub(crate) fn orchestrator_progress(&self) {
        self.live.orchestrator_progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn transport_receipt(
        &self,
        identity: TransportIdentity<'_>,
        authorize: impl FnOnce() -> std::result::Result<(), StoreError>,
    ) -> Result<Option<CommandOutcome>, StoreError> {
        let mut store = self.store.lock_or_exit("the domain store");
        let _approval = crate::approval_lease::ApprovalLease::shared(&self.approval_home)?;
        authorize()?;
        store.transport_receipt(identity)
    }

    pub fn open(home: &Path) -> Result<Self> {
        Self::open_at(home, &crate::domain_database_migration::active_path(home))
    }

    pub(crate) fn open_at(home: &Path, database: &Path) -> Result<Self> {
        let mut store = DomainStore::open(database)?;
        crate::domain_store::provider_settings::recover_store(&mut store, home)?;
        let options = crate::domain_schedule_runner::options_at(home).unwrap_or_default();
        store.fill_task_projects(&options.projects_root(&crate::options::user_home()))?;
        store.fill_task_models()?;
        let state = store.load()?;
        let (changed, _) = watch::channel(state.revision);
        let (schedule_changed, _) = watch::channel(state.revision);
        let authority = capabilities::load(home, state.data_epoch.0.clone())?;
        let runtime = Self {
            read_work: crate::blocking_work::BlockingWork::new(2),
            producer_work: crate::blocking_work::BlockingWork::new(2),
            external_work: crate::blocking_work::BlockingWork::new(2),
            command_work: crate::blocking_work::BlockingWork::new(2),
            hook_work: crate::blocking_work::BlockingWork::new(1),
            #[cfg(test)]
            allow_always_wait: Default::default(),
            guard_work: crate::blocking_work::BlockingWork::new(2),
            home: home.to_path_buf(),
            store: RuntimeMutex::new(store),
            snapshot: RwLock::new(Arc::new(ReadSnapshot::build(state))),
            approval_home: home.to_path_buf(),
            authority: RuntimeMutex::new(authority),
            operations_live: Default::default(), operations_changed: watch::channel(0).0,
            changed,
            schedule_changed,
            stored_reads: Default::default(),
            local_actions: Default::default(),
            live: crate::client_routes::Live::persisted_in(home),
            held: Default::default(),
            rejections: Default::default(),
            seals: Default::default(),
            sdk: Default::default(),
            sdk_owners: Default::default(),
            retakes: Default::default(),
            waiting_for_space: Default::default(),
            #[cfg(test)]
            full_disk: Default::default(),
            handovers: Default::default(),
            moves: Default::default(),
            selection_changes: Default::default(),
            contexts: Default::default(),
            #[cfg(test)]
            before_owner_publish: Default::default(),
            #[cfg(test)]
            before_next_commit: Default::default(),
            #[cfg(test)]
            before_acknowledge: Default::default(),
            #[cfg(test)]
            owner_failures: Default::default(),
            #[cfg(test)]
            driven_by_hand: Default::default(),
            #[cfg(test)]
            index_builds: Default::default(),
        };
        runtime.adopt_sdk_nodes()?;
        runtime.review_waits()?;
        Ok(runtime)
    }
    /// Moves every node of the Agent SDK an older daemon left in a phase of its own to its task's
    /// lifecycle, before the gate or an SDK owner reads the domain.
    fn adopt_sdk_nodes(&self) -> Result<()> {
        let state = self.state()?;
        if state.unadopted_sdk_nodes().is_empty() {
            return Ok(());
        }
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("adopt-sdk-nodes-{}", crate::auth::random_hex(8)).into(),
            expected_revision: 0,
            command: Command::AdoptSdkNodes,
        };
        match self.execute_as_system(envelope) {
            Ok(_) => Ok(()),
            Err(StoreError::Domain(error)) => {
                log!("could not move the nodes of the Agent SDK to their tasks' lifecycles: {}", error.message);
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
    /// Ends every wait the store keeps whose waits are over, such as one whose dependency a newer
    /// release counts as landed: the task goes on, saved, and its node is told or it starts.
    fn review_waits(&self) -> Result<()> {
        let state = self.state()?;
        let ended = state.ended_waits();
        if ended.is_empty() {
            return Ok(());
        }
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("review-waits-{}", crate::auth::random_hex(8)).into(),
            expected_revision: 0,
            command: Command::ReviewWaits,
        };
        let names = ended.iter().map(|task| task.0.as_str()).collect::<Vec<_>>().join(", ");
        match self.execute_as_system(envelope) {
            Ok(_) => {
                log!("what {names} waited for is done: each goes on");
                Ok(())
            }
            Err(StoreError::Domain(error)) => {
                log!("could not end the waits of {names}: {}", error.message);
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
    /// Only a sandbox daemon whose configuration names an SDK backend calls this, once.
    pub(crate) fn enable_sdk(self: &Arc<Self>, backend: crate::sdk_owner::SdkBackend) {
        let _ = self.sdk.set((backend, Arc::downgrade(self)));
    }
    /// The SDK backend and the runtime its owners run on, when this daemon has one.
    /// Stops every SDK owner and waits for each host's exit; nothing of an SDK host outlives the
    /// daemon that owns it.
    pub(crate) fn stop_sdk_owners(&self) {
        self.join_retakes();
        let owners = std::mem::take(&mut *self.sdk_owners.lock_or_recover("the SDK owners"));
        log!("stopping {} SDK host(s)", owners.len());
        for mut owner in owners {
            match owner.stop() {
                Ok(()) => log!("the SDK host of {} and its process group exited", owner.agent()),
                Err(error) => log!("the SDK host of {} did not stop with the daemon: {error}", owner.agent()),
            }
        }
    }
    /// Waits until every execution being taken over again is taken, or has its card.
    pub(crate) fn join_retakes(&self) {
        let retakes = std::mem::take(&mut *self.retakes.lock_or_recover("the retakes"));
        for retake in retakes {
            let _ = retake.join();
        }
    }
    /// Whether this daemon owns the SDK host of `agent`: an owner of it runs and has not ended.
    pub(crate) fn owns_sdk_host(&self, agent: &AgentId) -> bool {
        #[cfg(test)]
        if self.driven_by_hand.lock().expect("driven by hand poisoned").contains(agent) {
            return true;
        }
        self.sdk_owners.lock_or_recover("the SDK owners").iter().any(|owner| owner.agent() == agent && !owner.ended())
    }
    /// The process group of every node host this daemon owns now, which is what a node costs.
    pub(crate) fn node_groups(&self) -> Vec<i32> {
        self.sdk_owners.lock_or_recover("the SDK owners").iter().filter_map(crate::sdk_owner::Owner::host_group).collect()
    }
    /// Keeps `owner` as the owner of its node's host, in place of any of that node that ended on
    /// its own. Those are joined once the list is released: their end may read it.
    pub(crate) fn add_sdk_owner(&self, owner: crate::sdk_owner::Owner) {
        let ended: Vec<crate::sdk_owner::Owner> = {
            let mut owners = self.sdk_owners.lock_or_recover("the SDK owners");
            let (ended, kept) = std::mem::take(&mut *owners).into_iter().partition(|held| held.agent() == owner.agent() && held.ended());
            *owners = kept;
            owners.push(owner);
            ended
        };
        drop(ended);
    }
    /// Stops the SDK host of `agent` and waits until it and its whole process group exited. `Ok`
    /// with nothing to stop when this daemon owns no host for it.
    pub(crate) fn stop_sdk_host(&self, agent: &AgentId) -> Result<(), crate::sdk_owner::Failed> {
        self.take_sdk_owner(agent).map_or(Ok(()), |mut owner| owner.stop())
    }
    /// Stops the host of a node whose task is over, once the turn its Claude runs, if any, has
    /// ended as an interrupted turn.
    pub(crate) fn close_sdk_host(&self, agent: &AgentId) -> Result<(), crate::sdk_owner::Failed> {
        self.take_sdk_owner(agent).map_or(Ok(()), |mut owner| owner.close())
    }
    fn take_sdk_owner(&self, agent: &AgentId) -> Option<crate::sdk_owner::Owner> {
        let mut owners = self.sdk_owners.lock_or_recover("the SDK owners");
        owners.iter().position(|owner| owner.agent() == agent && !owner.ended())
            .or_else(|| owners.iter().position(|owner| owner.agent() == agent))
            .map(|at| owners.remove(at))
    }
    pub(crate) fn sdk(&self) -> Option<(&crate::sdk_owner::SdkBackend, Arc<DomainRuntime>)> {
        let (backend, runtime) = self.sdk.get()?;
        Some((backend, runtime.upgrade()?))
    }
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }
    pub fn state(&self) -> Result<Arc<DomainState>> {
        Ok(Arc::clone(self.read().state()))
    }
    /// The last committed state with its read index: both of the same revision.
    pub fn read(&self) -> Arc<ReadSnapshot> {
        Arc::clone(&self.snapshot.read_or_recover("the domain snapshot"))
    }
    /// Releases the raw provider evidence and the rows of each retention policy no reader needs
    /// any more as of `now`, one bounded pass at a time under the writer lock so other writers run
    /// between passes, then gives the disk the space.
    pub(crate) fn retain(&self, now: i64) -> Result<Retained, StoreError> {
        const PASS: usize = 256;
        let mut retained = Retained::default();
        loop {
            let pass = self.store.lock_or_exit("the domain store").retain_evidence(PASS)?;
            retained.evidence.released += pass.released;
            retained.evidence.removed += pass.removed;
            retained.evidence.commands_removed += pass.commands_removed;
            retained.evidence.texts_released += pass.texts_released;
            if [pass.released, pass.removed, pass.commands_removed, pass.texts_released].iter().all(|changed| *changed < PASS) {
                break;
            }
        }
        loop {
            let folded = self.store.lock_or_exit("the domain store").fold_ended_tasks(PASS)?;
            retained.folded += folded;
            if folded < PASS {
                break;
            }
        }
        for policy in &crate::domain_store::retention::POLICIES {
            let mut changed = 0;
            loop {
                let pass = self.store.lock_or_exit("the domain store").retain(policy, now, PASS)?;
                changed += pass;
                if pass < PASS {
                    break;
                }
            }
            retained.tables.push((policy.name, changed));
        }
        retained.compacted = self.compact_ended_tasks()?;
        self.store.lock_or_exit("the domain store").reclaim_space(4096)?;
        Ok(retained)
    }
    /// Compacts the cards of done and cancelled tasks, a batch per command, until none is left. A
    /// refusal, such as a task reopened meanwhile, ends this pass: the next one tries again.
    fn compact_ended_tasks(&self) -> Result<usize, StoreError> {
        const BATCH: usize = 32;
        let mut compacted = 0;
        loop {
            let state = self.state().map_err(StoreError::Storage)?;
            let task_ids = state.compactable_tasks(BATCH);
            if task_ids.is_empty() {
                self.store.lock_or_exit("the domain store").ended_tasks_compacted().map_err(anyhow::Error::from)?;
                return Ok(compacted);
            }
            let count = task_ids.len();
            let envelope = CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
                command_id: format!("compact-ended-tasks-{}", crate::auth::random_hex(8)).into(),
                expected_revision: 0,
                command: Command::CompactEndedTasks { task_ids },
            };
            match self.execute_as_system(envelope) {
                Ok(outcome) if outcome.changed => compacted += count,
                Ok(_) => return Ok(compacted),
                Err(StoreError::Domain(error)) => {
                    log!("could not compact the cards of ended tasks: {}", error.message);
                    return Ok(compacted);
                }
                Err(error) => return Err(error),
            }
        }
    }
    /// Provider evidence, its normalization and the domain's decision commit in one transaction.
    /// Still under the writer lock, readers get the committed state and the change is notified;
    /// only then are the store's seals queued for acknowledgement.
    pub(crate) fn own<T>(
        &self,
        scope: alondra_store::AuthorityScope,
        stage: impl FnOnce(&mut OwnerTransaction<'_>) -> Result<T, StoreError>,
    ) -> Result<(T, Vec<StagedOutcome>), StoreError> {
        self.own_projected(scope, stage, |_| Ok(()))
    }

    pub(crate) fn own_projected<T>(
        &self,
        scope: alondra_store::AuthorityScope,
        stage: impl FnOnce(&mut OwnerTransaction<'_>) -> Result<T, StoreError>,
        project: impl FnOnce(&mut DomainStore) -> anyhow::Result<()>,
    ) -> Result<(T, Vec<StagedOutcome>), StoreError> {
        let mut store = self.store.lock_or_exit("the domain store");
        let _approval = crate::approval_lease::ApprovalLease::shared(&self.approval_home)?;
        let (value, commit) = store.own(scope, stage)?;
        if let Err(why) = project(&mut store) {
            log!("The authority transaction committed; its recoverable settings projection needs repair before another settings read: {why:#}");
        }
        #[cfg(test)]
        if let Some(hook) = self.before_owner_publish.lock().unwrap().as_mut() {
            hook();
        }
        self.publish(&commit.draft.state);
        let (draft, sealed) = commit.acknowledge(&self.read())?;
        if draft.domain_changed {
            self.changed.send_replace(draft.state.revision);
            if draft.schedule_changed {
                self.schedule_changed.send_replace(draft.state.revision);
            }
        }
        self.seals.put(sealed);
        drop(store);
        Ok((value, draft.outcomes))
    }
    #[cfg(test)]
    pub(crate) fn take_acknowledgements(&self) -> Vec<alondra_store::Committed> {
        self.seals.take_all()
    }
    /// Runs `hook` once, as the next command reaches the writer lock.
    #[cfg(test)]
    pub(crate) fn before_next_commit(&self, hook: impl FnOnce(&DomainRuntime) + Send + 'static) {
        *self.before_next_commit.lock().unwrap() = Some(Box::new(hook));
    }
    #[cfg(test)]
    pub(crate) fn before_owner_publish(&self, hook: impl FnMut() + Send + 'static) {
        *self.before_owner_publish.lock().unwrap() = Some(Box::new(hook));
    }
    #[cfg(test)]
    pub(crate) fn on_acknowledge(&self, hook: impl FnMut() + Send + 'static) {
        *self.before_acknowledge.lock().unwrap() = Some(Box::new(hook));
    }
    /// Every failure an SDK owner reports from now on.
    #[cfg(test)]
    pub(crate) fn owner_failures(&self) -> std::sync::mpsc::Receiver<String> {
        let (sender, failures) = std::sync::mpsc::channel();
        *self.owner_failures.lock().unwrap() = Some(sender);
        failures
    }
    #[cfg(test)]
    pub(crate) fn owner_failed(&self, why: String) {
        if let Some(sender) = self.owner_failures.lock().unwrap().as_ref() {
            let _ = sender.send(why);
        }
    }
    #[cfg(test)]
    pub(crate) fn before_acknowledge(&self) {
        if let Some(hook) = self.before_acknowledge.lock().unwrap().as_mut() {
            hook();
        }
    }
    /// Builds the index of a new commit; a replayed command hands back the state already read.
    fn publish(&self, state: &Arc<DomainState>) {
        if Arc::ptr_eq(self.read().state(), state) {
            return;
        }
        #[cfg(test)]
        self.index_builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let snapshot = Arc::new(ReadSnapshot::build(Arc::clone(state)));
        *self.snapshot.write_or_recover("the domain snapshot") = snapshot;
    }
    pub(crate) fn publish_batch_report(&self, run: &str, cursor: u64, evidence: &[u8], report: &crate::domain_store::batch_effects::BatchReport) -> Result<(), StoreError> {
        let mut store = self.store.lock_or_exit("the domain store");
        store.record_batch_report(run, cursor, evidence, report)?;
        if matches!(report.kind.as_str(), "assistant" | "user" | "result" | "process_exited"
            | "alondra/threadReady" | "alondra/inputAccepted" | "turn/started" | "turn/completed"
            | "item/started" | "item/completed"
            | "model/rerouted") {
            self.orchestrator_progress();
        }
        let committed = store.load()?;
        self.publish(&committed);
        if !Arc::ptr_eq(self.read().state(), &committed) {
            return Err(StoreError::Storage(anyhow::anyhow!("The committed batch report has not been published")));
        }
        Ok(())
    }

    /// After a test writes domain rows straight to SQL, or through another runtime on the
    /// same file: the daemon itself only changes the domain by committing commands.
    #[cfg(test)]
    pub(crate) fn reload(&self) {
        let state = self.store.lock().unwrap().load().unwrap();
        self.publish(&state);
    }
    pub(crate) fn schedule_request_tools(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<Vec<String>>> {
        self.store
            .lock_or_exit("the domain store")
            .schedule_request_tools(request_id)
    }
    pub(crate) fn was_schedule_launch_failed(&self, request_id: &RequestId) -> Result<bool> {
        self.store
            .lock_or_exit("the domain store")
            .was_schedule_launch_failed(request_id)
    }
    pub fn subscribe(&self) -> watch::Receiver<Revision> {
        self.changed.subscribe()
    }
    pub(crate) fn subscribe_schedules(&self) -> watch::Receiver<Revision> {
        self.schedule_changed.subscribe()
    }

    /// Issued capabilities outlive this process: the Claude session keeps its
    /// bearer across daemon restarts, so the digest is persisted before the
    /// token is handed out.
    pub fn issue(&self, principal: Principal) -> Result<Capability, StoreError> {
        let mut authority = self.authority.lock_or_recover("the domain authority");
        let capability = authority
            .issue(principal)
            .map_err(|message| DomainError::new("forbidden", message))?;
        self.persist_issued(&mut authority, &capability)?;
        Ok(capability)
    }

    /// Bind a Claude session that the domain already knows as an Agent, such as
    /// one imported at cutover or resumed after a restart. Any earlier bearer
    /// for that session stops working the moment the new one is issued.
    pub fn bind_agent(&self, session_id: &str) -> Result<Capability, StoreError> {
        let mut authority = self.authority.lock_or_recover("the domain authority");
        let state = self.state()?;
        if !state.agents.contains_key(&AgentId(session_id.to_owned())) {
            return Err(DomainError::new("not_found", "Agent does not exist").into());
        }
        authority.revoke_agent(session_id);
        let capability = authority
            .issue(Principal::Agent {
                session_id: session_id.to_owned(),
                task_binding: None,
                provider_agent_id: None,
            })
            .map_err(|message| DomainError::new("forbidden", message))?;
        self.persist_issued(&mut authority, &capability)?;
        Ok(capability)
    }

    /// A capability that could not be written is never handed out, so it leaves the registry in
    /// memory too: memory and disk hold the same live capabilities. Revocations stay in memory
    /// even unwritten, so a revoked bearer is refused either way.
    fn persist_issued(&self, authority: &mut Authority, capability: &Capability) -> Result<()> {
        self.persist_authority(authority).inspect_err(|_| authority.forget(capability))
    }

    fn persist_authority(&self, authority: &Authority) -> Result<()> {
        #[cfg(test)]
        if self.full_disk.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow::Error::from(std::io::Error::from_raw_os_error(libc::ENOSPC)).context("writing the domain capability registry"));
        }
        capabilities::persist(&self.home, authority)
    }

    /// Bind one orchestrator execution to the current generation.
    pub fn bind_orchestrator(&self, execution_id: &str) -> Result<Capability, StoreError> {
        let mut authority = self.authority.lock_or_recover("the domain authority");
        let state = self.state()?;
        self.store.lock_or_exit("the domain store").bind_batch_effects(execution_id, &state.data_epoch)?;
        authority.revoke_execution(execution_id);
        let capability = authority
            .issue(Principal::Orchestrator {
                generation: state.orchestrator_generation,
                execution_id: execution_id.to_owned(),
            })
            .map_err(|message| DomainError::new("forbidden", message))?;
        self.persist_issued(&mut authority, &capability)?;
        Ok(capability)
    }

    /// Record the Claude session the orchestrator runs in, unless the domain already has it.
    pub fn bind_orchestrator_session(&self, session: &str) -> Result<(), StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        if state.orchestrator_session.as_ref().is_some_and(|bound| bound.0 == session) {
            return Ok(());
        }
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("orchestrator-session-{}", crate::auth::random_hex(16)).into(),
            expected_revision: 0,
            command: Command::BindOrchestratorSession { session_id: session.into() },
        })
        .map(drop)
    }

    pub fn revoke_agent(&self, session: &str) -> Result<()> {
        let mut authority = self.authority.lock_or_recover("the domain authority");
        authority.revoke_agent(session);
        self.persist_authority(&authority)
    }
    /// Revokes an orchestrator execution and returns what it was let change.
    pub fn end_execution(&self, execution: &str) -> Vec<String> {
        let mut authority = self.authority.lock_or_recover("the domain authority");
        let changes = authority.end_execution(execution);
        if let Err(error) = self.persist_authority(&authority) {
            log!("domain: cannot persist the revoked capability of {execution}: {error:#}");
        }
        changes
    }

    /// The principal of a bearer let change `what`; an orchestrator execution has it recorded
    /// before it acts, so its end never misses a change already under way.
    pub fn admit_change(&self, bearer: &str, what: &str) -> Result<Principal, StoreError> {
        let mut authority = self.authority.lock_or_recover("the domain authority");
        let state = self.state()?;
        let principal = authority
            .resolve(&state.data_epoch.0, bearer)
            .map_err(|message| DomainError::new("forbidden", message))?;
        authority.record_change(&principal, what);
        Ok(principal)
    }

    pub(crate) fn request_plan_mode(&self, bearer: &str, task: &TaskId, operation: &str) -> Result<CommandOutcome,StoreError> {
        if operation.is_empty() || operation.len() > 96 || !operation.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(DomainError::new("invalid_input","A bounded native operation identity is required").into());
        }
        let authority = self.authority.lock_or_recover("the domain authority");
        let epoch = self.read().state().data_epoch.clone();
        let principal = authority.resolve(&epoch.0,bearer).map_err(|_| DomainError::new("forbidden","A current task capability is required"))?;
        let Principal::Agent { session_id,task_binding,provider_agent_id } = principal else { return Err(DomainError::new("forbidden","Only the task's own node requests plan mode").into()) };
        if provider_agent_id.is_some() || task_binding.as_deref().is_some_and(|bound| bound != task.0) {
            return Err(DomainError::new("not_assigned","The node is bound to another execution").into());
        }
        let scope = alondra_store::AuthorityScope::new(epoch.clone())?;
        let (outcome,_) = self.own(scope,|owner| {
            let state = owner.state();
            if state.tasks.get(task).and_then(|task| task.agent_id.as_ref()).map(|node| node.0.as_str()) != Some(session_id.as_str()) {
                return Err(DomainError::new("not_assigned","Only the current assigned node requests plan mode").into());
            }
            let before_revision = state.revision;
            let already_requested = state.lifecycle_journal.get(task).and_then(|journal| journal.iter().rev().find_map(|record| match record {
                LifecycleRecord::ControlRequested { operation: ControlRecord::SetPermissionMode { mode }, .. } => Some(*mode == PermissionModeRecord::Plan),
                LifecycleRecord::Admitted { .. } => Some(false),
                _ => None,
            })).unwrap_or(false);
            let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION,data_epoch:epoch,
                command_id:format!("plan-mode-{operation}").into(),expected_revision:0,
                command:Command::SetPermissionMode { task_id:task.clone(),mode:PermissionModeRecord::Plan } };
            let outcome = owner.stage_command(&Actor::System,&envelope,crate::domain_store::Attached::None)?.clone();
            if !outcome.changed || (already_requested && outcome.revision > before_revision) { return Ok(outcome); }
            let continuation = CommandEnvelope { protocol_version:PROTOCOL_VERSION,data_epoch:envelope.data_epoch.clone(),
                command_id:format!("plan-mode-input-{operation}").into(),expected_revision:0,
                command:Command::TellNode { task_id:task.clone(),text:"Alondra has enabled plan mode. Prepare the task plan for the user's approval before implementation. Preserve the current session and completed work.".into(), from_user: false } };
            owner.stage_command(&Actor::System,&continuation,crate::domain_store::Attached::None).cloned()
        })?;
        Ok(outcome)
    }

    /// Expose only the public protocol cursor to a daemon-authorized producer.
    /// Commands recheck the capability while holding the writer lock.
    pub(crate) fn read_current_task_context(&self, bearer: &str, task: &TaskId, after: Option<&alondra_store::ContextRef>, limit: usize) -> std::result::Result<alondra_store::ContextPage, DomainError> {
        let authority = self.authority.lock_or_recover("the domain authority");
        let store = self.store.lock_or_exit("the domain store");
        let read = self.read();
        let principal = authority.resolve(&read.state().data_epoch.0, bearer)
            .map_err(|_| DomainError::new("forbidden", "A current node capability is required"))?;
        let Principal::Agent { session_id, task_binding, .. } = principal else {
            return Err(DomainError::new("forbidden", "Only a task's own node reads its portable conversation"));
        };
        if task_binding.as_deref().is_some_and(|binding| binding != task.0)
            || read.index().task_of_node(&AgentId::from(session_id)) != Some(task)
        { return Err(DomainError::new("not_assigned", "This conversation belongs to another task")); }
        store.public_task_context(task, after, limit)
            .map_err(|_| DomainError::new("storage_unavailable", "The retained task conversation could not be read"))
    }

    pub(crate) fn read_batch_outcomes(&self, bearer: &str, run: Option<&str>, list_runs: bool, after: &str, limit: usize) -> std::result::Result<serde_json::Value, DomainError> {
        if run.is_some_and(|run|run.is_empty() || run.len()>128) || after.len()>if list_runs{128}else{2048} || !(1..=32).contains(&limit) || (list_runs && run.is_some()) {
            return Err(DomainError::new("invalid_input","Invalid outcome page"));
        }
        let authority=self.authority.lock_or_recover("the domain authority");
        let store=self.store.lock_or_exit("the domain store");
        let read=self.read();
        let principal=authority.resolve(&read.state().data_epoch.0,bearer).map_err(|_|DomainError::new("forbidden","The current orchestrator is required"))?;
        let Principal::Orchestrator{generation,execution_id}=principal else{return Err(DomainError::new("forbidden","The current orchestrator is required"))};
        if generation!=read.state().orchestrator_generation{return Err(DomainError::new("stale_generation","The orchestrator generation changed"));}
        let epoch=&read.state().data_epoch;
        let result=if list_runs{store.batch_run_manifest(epoch,after,limit)}else{store.batch_outcomes(run.unwrap_or(&execution_id),epoch,after,limit)};
        result.map_err(|error|DomainError::new("outcome_unavailable",format!("{error:#}")))
    }

    pub fn resolve_bearer(&self, bearer: &str) -> Result<Principal, StoreError> {
        let authority = self.authority.lock_or_recover("the domain authority");
        let state = self.state()?;
        let principal = authority.resolve(&state.data_epoch.0,bearer).map_err(|message| DomainError::new("forbidden",message))?;
        if let Principal::Orchestrator { generation, .. } = &principal
            && *generation != state.orchestrator_generation {
            return Err(DomainError::new("stale_generation","The orchestrator generation changed").into());
        }
        Ok(principal)
    }

    /// `authorize` rechecks mutable transport approval while holding the writer lock.
    /// The actor is constructed by trusted adapters, never decoded from client JSON.
    pub fn execute(
        &self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        authorize: impl FnOnce() -> std::result::Result<(), StoreError>,
    ) -> Result<CommandOutcome, StoreError> {
        self.execute_with(actor, envelope, transport, None, authorize)
    }

    /// Whether Alondra launched this session: a recorded launch, or a scheduled run it started.
    /// A node mod connected: a session that says it is mid-turn tells the node machine so, as a
    /// daemon that restarted meanwhile learns it.
    pub(crate) fn connect_node(&self, hello: crate::node_reports::Hello, at: alondra_core::Millis) -> u64 {
        let (agent, working) = (AgentId(hello.session_id.clone()), hello.working);
        let number = self.live.mods.connect(hello, at);
        if working && let Err(error) = crate::domain_agent_lifecycle::node_event_at(self, &agent, alondra_core::domain::NodeEvent::TurnStarted, at) {
            log!("could not record that {} is mid-turn: {error}", agent.0);
        }
        number
    }

    /// A node mod's report: what it says is kept for the views, and a turn it started or a session
    /// it ended goes to the node machine. A turn's end is the Stop hook's to say: only it knows
    /// whether Alondra held the turn back.
    pub(crate) fn report(&self, envelope: crate::node_reports::Envelope, at: alondra_core::Millis) {
        let happened = match envelope.report {
            crate::node_reports::Report::TurnStart => Some(alondra_core::domain::NodeEvent::TurnStarted),
            crate::node_reports::Report::SessionEnd => Some(alondra_core::domain::NodeEvent::SessionLost),
            _ => None,
        };
        let agent = AgentId(envelope.session_id.clone());
        self.live.mods.apply(envelope, at);
        if let Some(happened) = happened
            && let Err(error) = crate::domain_agent_lifecycle::node_event_at(self, &agent, happened, at)
        {
            log!("could not record what {} reported: {error}", agent.0);
        }
    }

    pub(crate) fn launched_by_alondra(&self, agent: &AgentId) -> Result<bool> {
        if self.store.lock_or_exit("the domain store").launched_by_alondra(&agent.0)? || self.live.mods.started_with_a_launch_key(&agent.0) {
            return Ok(true);
        }
        Ok(self.state()?.schedule.runs.values().any(|run| run.launch.as_ref().is_some_and(|launch| launch.agent_id.as_ref() == Some(agent))))
    }
    pub(crate) fn launch_plan(&self, task_id: &TaskId) -> Result<Option<crate::launch_plans::LaunchPlan>> {
        self.store.lock_or_exit("the domain store").launch_plan(task_id)
    }
    pub(crate) fn inherited_worktree(&self, agent: &str) -> Result<Option<String>> {
        self.store.lock_or_exit("the domain store").inherited_worktree(agent)
    }
    pub(crate) fn set_inherited_worktree(&self, agent: &str, dir: &str, from_agent: &str) -> Result<()> {
        self.store.lock_or_exit("the domain store").set_inherited_worktree(agent, dir, from_agent, alondra_core::now_millis())
    }
    pub(crate) fn task_worktree(&self, task: &TaskId) -> Result<Option<String>> {
        self.store.lock_or_exit("the domain store").task_worktree(task)
    }
    /// The worktree the task's node works in, kept for its next launch and recorded as the
    /// task's own, which it ends only once that worktree is gone.
    pub(crate) fn set_task_worktree(&self, task: &TaskId, dir: &str) -> Result<()> {
        self.store.lock_or_exit("the domain store").set_task_worktree(task, dir, alondra_core::now_millis())?;
        let command = Command::RecordTaskWorktree { task_id: task.clone(), path: dir.to_owned(), origin: WorktreeOrigin::Task, branch: None };
        crate::domain_github::execute_system(self, command).map_err(|error| anyhow::anyhow!("{error}"))
    }
    /// What Alondra kept about a session only while it lives: the worktree it inherited and why
    /// its replacement stopped short.
    pub(crate) fn forget_ended_session(&self, agent: &str) -> Result<()> {
        self.live.take_aborted_replace(agent);
        self.store.lock_or_exit("the domain store").forget_inherited_worktree(agent)
    }
    pub(crate) fn record_model_decision(&self, decision: &crate::domain_store::model_metrics::Decision) -> Result<()> {
        self.store.lock_or_exit("the domain store").record_model_decision(decision)
    }

    pub(crate) fn record_spend(&self, spent: &crate::domain_store::metrics_fold::Spent<'_>) -> Result<()> {
        self.store.lock_or_exit("the domain store").record_spend(spent)
    }

    pub(crate) fn record_release(&self, version: &str, at: alondra_core::Millis) -> Result<()> {
        self.store.lock_or_exit("the domain store").record_release(version, at)
    }

    pub(crate) fn prs_without_size(&self, now: alondra_core::Millis, limit: usize) -> Result<Vec<(String, String)>> {
        self.store.lock_or_exit("the domain store").prs_without_size(now, limit)
    }

    pub(crate) fn record_pr_lines(&self, task_id: &str, lines: i64) -> Result<()> {
        self.store.lock_or_exit("the domain store").record_pr_lines(task_id, lines)
    }

    /// What the Metrics screen sums, with the tasks that ended since the last fold folded first.
    pub(crate) fn metrics_inputs(&self, now: alondra_core::Millis) -> Result<alondra_core::metrics::report::MetricsInputs> {
        const PASS: usize = 256;
        while self.store.lock_or_exit("the domain store").fold_ended_tasks(PASS)? == PASS {}
        self.store.lock_or_exit("the domain store").metrics_inputs(now)
    }

    /// What the user chose when asking for the Request, if it was asked for with choices.
    pub(crate) fn request_launch(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<crate::domain_store::RequestLaunch>> {
        self.store
            .lock_or_exit("the domain store")
            .request_launch(request_id)
    }

    pub(crate) fn execute_with(
        &self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        launch: Option<&crate::domain_store::RequestLaunch>,
        authorize: impl FnOnce() -> std::result::Result<(), StoreError>,
    ) -> Result<CommandOutcome, StoreError> {
        self.commit(actor, envelope, transport, launch, false, authorize)
    }

    /// A command the daemon issues itself, judged on the state it commits against: the revision
    /// is filled under the writer lock, so a command committed meanwhile never makes it stale.
    pub(crate) fn execute_as_system(&self, envelope: CommandEnvelope) -> Result<CommandOutcome, StoreError> {
        let envelope = CommandEnvelope { expected_revision: 0, ..envelope };
        self.commit(&Actor::System, &envelope, None, None, true, || Ok(()))
    }

    /// A fixture's synchronous actor step takes its revision on the writer lane, as an internal
    /// producer does. Client tests use `execute_with` to exercise their optimistic revision.
    #[cfg(test)]
    pub(crate) fn execute_fixture(&self, actor: &Actor, envelope: CommandEnvelope) -> Result<CommandOutcome, StoreError> {
        let envelope = CommandEnvelope { expected_revision: 0, ..envelope };
        self.commit(actor, &envelope, None, None, true, || Ok(()))
    }

    fn commit(
        &self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        launch: Option<&crate::domain_store::RequestLaunch>,
        on_current: bool,
        authorize: impl FnOnce() -> std::result::Result<(), StoreError>,
    ) -> Result<CommandOutcome, StoreError> {
        #[cfg(test)]
        {
            let hook = self.before_next_commit.lock().unwrap().take();
            if let Some(hook) = hook {
                hook(self);
            }
        }
        let mut store = self.store.lock_or_exit("the domain store");
        // Lock order is domain store → approval lease. Revocation takes only the
        // exclusive approval lease and never waits for the domain store.
        let _approval = if matches!(actor, Actor::User { .. }) || transport.is_some() {
            Some(crate::approval_lease::ApprovalLease::shared(
                &self.approval_home,
            )?)
        } else {
            None
        };
        authorize()?;
        let prepared = crate::domain_content::prepare(&self.home, envelope)?;
        let commit = match launch {
            Some(launch) => store.execute_launching(actor, &prepared.envelope, transport, launch),
            None if on_current => store.execute_filling_revision(actor, &prepared.envelope),
            None => store.execute_authenticated(actor, &prepared.envelope, transport),
        };
        if matches!(&commit, Err(StoreError::Domain(_))) {
            prepared.discard_uncommitted(&self.home);
        }
        let mut commit = commit?;
        self.publish(&commit.state);
        if let Some(sealed) = commit.sealed.take().filter(|sealed| !crate::sdk_owner::seals::empty(sealed)) {
            self.seals.put(sealed);
        }
        if !commit.replayed && commit.outcome.changed {
            self.changed.send_replace(commit.state.revision);
            if schedule_command(&prepared.envelope.command) {
                self.schedule_changed.send_replace(commit.state.revision);
            }
        }
        drop(store);
        if !commit.replayed && commit.outcome.changed {
            crate::task_closer::settle_unattended(self, &commit.state);
            self.allowed_always(&prepared.envelope.command, &commit.state);
            self.retake_lost(&prepared.envelope.command, &commit.state);
        }
        Ok(commit.outcome)
    }

    /// «Try again», chosen on the card of a task whose SDK execution could not be taken over and
    /// committed as its answer: the daemon takes the execution over again.
    fn retake_lost(&self, command: &Command, state: &DomainState) {
        let Command::AnswerQuestion { interaction_id, option_id, .. } = command else { return };
        if option_id != alondra_core::domain::RETAKE {
            return;
        }
        let Some(Scope::Task { task_id }) = state.interactions.get(interaction_id).map(|card| &card.scope) else { return };
        crate::sdk_owner::retake_manually(self, task_id.clone());
    }

    /// «Allow always», chosen on the card of a decision Claude asked for through the SDK and
    /// committed as its decision: the card's rules are written for the task's repo.
    fn allowed_always(&self, command: &Command, state: &DomainState) {
        let Command::AnswerQuestion { interaction_id, option_id, .. } = command else { return };
        if option_id != Hold::ALWAYS {
            return;
        }
        let Some(card) = state.interactions.get(interaction_id) else { return };
        let Scope::Task { task_id } = &card.scope else { return };
        let decided = state.lifecycle_journal.get(task_id).is_some_and(|journal| {
            journal.iter().any(|record| matches!(record, LifecycleRecord::Decided { card, .. } if card == interaction_id))
        });
        let Some(rules) = card.card.allow_always.as_ref().and_then(alondra_core::allow_always::AllowAlways::rules).filter(|_| decided) else { return };
        let written = self.task_worktree(task_id).and_then(|worktree| {
            crate::allow_always::chosen_through_sdk(self, worktree.as_deref(), rules, Some(&task_id.0), &interaction_id.0)
        });
        if let Err(error) = written {
            log!("allow always: the rules of {interaction_id} were not written: {error:#}");
        }
    }

    pub fn execute_bearer(
        &self,
        bearer: &str,
        envelope: &CommandEnvelope,
    ) -> Result<CommandOutcome, StoreError> {
        let result = self.execute_bearer_once(bearer, envelope);
        self.count_rejections(bearer, envelope, &result);
        result
    }

    fn execute_bearer_once(
        &self,
        bearer: &str,
        envelope: &CommandEnvelope,
    ) -> Result<CommandOutcome, StoreError> {
        // Keep the capability lease until commit: concurrent revocation cannot pass this write.
        let mut authority = self.authority.lock_or_recover("the domain authority");
        let principal = authority
            .resolve(&envelope.data_epoch.0, bearer)
            .map_err(|message| DomainError::new("forbidden", message))?;
        let mut store = self.store.lock_or_exit("the domain store");
        let state = store.load()?;
        let actor = match principal.clone() {
            Principal::Agent {
                session_id,
                task_binding,
                ..
            } => {
                if let Some(binding) = task_binding {
                    let task = command_task(&state, &envelope.command);
                    if task.as_ref().map(|id| id.0.as_str()) != Some(binding.as_str()) {
                        return Err(DomainError::new(
                            "not_assigned",
                            "provider child is bound to a different task",
                        )
                        .into());
                    }
                }
                Actor::Agent {
                    agent_id: session_id.into(),
                }
            }
            Principal::Orchestrator { generation, .. } => Actor::Orchestrator { generation },
        };
        let prepared = crate::domain_content::prepare(&self.home, envelope)?;
        let commit = match &principal {
            Principal::Orchestrator { execution_id, .. } => store.execute_from_batch(&actor, &prepared.envelope, execution_id),
            _ => store.execute_filling_revision(&actor, &prepared.envelope),
        };
        if matches!(&commit, Err(StoreError::Domain(_))) {
            prepared.discard_uncommitted(&self.home);
        }
        let mut commit = commit?;
        if !commit.replayed && commit.outcome.changed && matches!(principal, Principal::Orchestrator { .. }) {
            authority.record_change(&principal, &command_kind(&prepared.envelope.command));
            self.orchestrator_progress();
        }
        self.publish(&commit.state);
        if let Some(sealed) = commit.sealed.take().filter(|sealed| !crate::sdk_owner::seals::empty(sealed)) {
            self.seals.put(sealed);
        }
        if !commit.replayed && commit.outcome.changed {
            self.changed.send_replace(commit.state.revision);
            if schedule_command(&prepared.envelope.command) {
                self.schedule_changed.send_replace(commit.state.revision);
            }
        }
        drop(store);
        if !commit.replayed && commit.outcome.changed {
            crate::task_closer::settle_unattended(self, &commit.state);
        }
        Ok(commit.outcome)
    }
}

/// The command's name on the wire, such as `create_task`.
fn command_kind(command: &Command) -> String {
    serde_json::to_value(command).ok().and_then(|value| value["kind"].as_str().map(str::to_owned)).unwrap_or_default()
}

pub(crate) fn schedule_command(command: &Command) -> bool {
    matches!(
        command,
        Command::CreateSchedule { .. }
            | Command::UpdateSchedule { .. }
            | Command::DeleteSchedule { .. }
            | Command::ClaimScheduleRun { .. }
    )
}

/// Exhaustive scope resolution for delegated producers; unscoped commands are never admitted.
fn command_task(state: &DomainState, command: &Command) -> Option<TaskId> {
    match command {
        Command::CreateTask { task_id, .. }
        | Command::AssignTask { task_id, .. }
        | Command::RetitleTask { task_id, .. }
        | Command::SetDependencies { task_id, .. }
        | Command::SetRelations { task_id, .. }
        | Command::SetPriority { task_id, .. }
        | Command::RecordVerdict { task_id, .. }
        | Command::StartTask { task_id }
        | Command::CreateDocument { task_id, .. }
        | Command::ReviseDocument { task_id, .. }
        | Command::LinkTaskDocument { task_id, .. }
        | Command::DecideDocumentReview { task_id, .. }
        | Command::RegisterPr { task_id, .. }
        | Command::PresentResult { task_id, .. }
        | Command::PublishStackReadiness { task_id, .. }
        | Command::CompleteTask { task_id, .. }
        | Command::CancelTask { task_id }
        | Command::ReopenTask { task_id }
        | Command::ConfirmCleanup { task_id, .. }
        | Command::RecordTaskWorktree { task_id, .. }
        | Command::ReleaseTaskWorktree { task_id, .. }
        | Command::DeclarePostMergeWork { task_id, .. }
        | Command::RecordNodeEnd { task_id, .. }
        | Command::RecordTaskCleanup { task_id }
        | Command::AdmitExecution { task_id, .. }
        | Command::RecordRuntimeFacts { task_id, .. }
        | Command::RecordTurnBoundary { task_id, .. }
        | Command::RecordHostEnded { task_id, .. }
        | Command::RecordInterruptAccepted { task_id, .. }
        | Command::RecordDoubtfulHandoff { task_id, .. }
        | Command::RecordHostLost { task_id, .. }
        | Command::ResendUntakenInput { task_id, .. }
        | Command::RecordDroppedInput { task_id, .. }
        | Command::SetModel { task_id, .. }
        | Command::SetPermissionMode { task_id, .. }
        | Command::ReadMcpStatus { task_id }
        | Command::ReconnectMcp { task_id, .. }
        | Command::StartMcpSignIn { task_id, .. }
        | Command::ClearMcpSignIn { task_id, .. }
        | Command::RecordMcpServers { task_id, .. }
        | Command::RecordMcpSignIn { task_id, .. }
        | Command::CompleteMcpSignIn { task_id, .. }
        | Command::WaitForMcp { task_id, .. }
        | Command::CompactConversation { task_id }
        | Command::TellNode { task_id, .. }
        | Command::RecordDecisionRequest { task_id, .. }
        | Command::RefuseDecisionRequest { task_id, .. }
        | Command::RecordDecisionSettled { task_id, .. }
        | Command::RecordDecisionAborted { task_id, .. }
        | Command::ArchiveTask { task_id, .. }
        | Command::RevokeTaskGrant { task_id, .. }
        | Command::ReportFailure { task_id }
        | Command::ReleaseTask { task_id, .. }
        | Command::RaiseStartProblem { task_id, .. }
        | Command::RecordTaskSelection { task_id, .. }
        | Command::RequestTaskSelection { task_id, .. }
        | Command::SettlePlanReview { task_id, .. }
        | Command::SettleStall { task_id, .. } => Some(task_id.clone()),
        Command::PublishInformation { scope, .. }
        | Command::AskQuestion { scope, .. }
        | Command::AskQuestions { scope, .. }
        | Command::RequestAction { scope, .. }
        | Command::RaiseProblem { scope, .. } => match scope {
            Scope::Task { task_id } => Some(task_id.clone()),
            Scope::Orchestrator { .. } => None,
        },
        Command::MarkRead { interaction_id }
        | Command::Withdraw { interaction_id }
        | Command::RequestClarification { interaction_id, .. }
        | Command::AnswerQuestion { interaction_id, .. }
        | Command::SettleSuggestion { interaction_id, .. }
        | Command::ReopenAnswer { interaction_id }
        | Command::LinkInteractionSubject { interaction_id, .. } => {
            interaction_task(state, interaction_id)
        }
        Command::SubmitFeedback {
            presentation_id, ..
        }
        | Command::AcceptDelivery {
            presentation_id, ..
        }
        | Command::DecideProposedPlan {
            presentation_id, ..
        }
        | Command::AuthorizeMerge {
            presentation_id, ..
        } => interaction_task(state, presentation_id),
        Command::ClaimMergeAuthorization {
            authorization_id, ..
        }
        | Command::ObserveMergeAttempt {
            authorization_id, ..
        } => state
            .merge_authorizations
            .get(authorization_id)
            .map(|authorization| authorization.task_id.clone()),
        Command::ObservePrState { pr_id, .. } | Command::ObservePrMergeMethods { pr_id, .. } => {
            state.pull_requests.get(pr_id).map(|pr| pr.task_id.clone())
        }
        Command::CreateRequest { .. }
        | Command::CloseRequest { .. }
        | Command::TitleRequest { .. }
        | Command::CreateSchedule { .. }
        | Command::UpdateSchedule { .. }
        | Command::DeleteSchedule { .. }
        | Command::ClaimScheduleRun { .. }
        | Command::RecordScheduleRunSent { .. }
        | Command::ClaimScheduleLaunch { .. }
        | Command::RecordScheduleLaunchHanded { .. }
        | Command::RecordScheduleRunFailed { .. }
        | Command::InterruptScheduleRunsOnRestart { .. }
        | Command::AdoptSdkNodes
        | Command::ReviewWaits
        | Command::RegisterAgent { .. }
        | Command::MoveNode { .. }
        | Command::RecordLastWords { .. }
        | Command::ReconcileNodes { .. }
        | Command::AdvanceOrchestratorGeneration { .. }
        | Command::BindOrchestratorSession { .. }
        | Command::HoldHook { .. }
        | Command::HoldQuestions { .. }
        | Command::ProbeClaudeAccess { .. }
        | Command::RecordAccounts { .. }
        | Command::RecordAccountUsage { .. }
        | Command::RecordDistribution { .. }
        | Command::ChangeAccounts { .. }
        | Command::InterruptTurn { .. }
        | Command::ForceStopTurn { .. }
        | Command::CompactEndedTasks { .. }
        | Command::ClearCompleted { .. }
        | Command::SetAccountOrder { .. }
        | Command::ReplaceAccountConfiguration { .. }
        | Command::RecordAccountConfiguration { .. }
        | Command::RecordModelCatalog { .. }
        | Command::RecordModelProfiles { .. }
        | Command::SetModelProfile { .. }
        | Command::SetMergeMethod { .. } => None,
    }
}
fn interaction_task(state: &DomainState, id: &InteractionId) -> Option<TaskId> {
    match &state.interactions.get(id)?.scope {
        Scope::Task { task_id } => Some(task_id.clone()),
        Scope::Orchestrator { .. } => None,
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;
    use crate::auth::Auth;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use p256::ecdsa::SigningKey;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    #[test]
    fn external_revocation_waits_for_authorized_domain_commit() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-domain-lease-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[9; 32]).unwrap();
        let device = auth
            .pair(
                &auth.new_pairing_code(),
                "Test Phone",
                &STANDARD.encode(key.verifying_key().to_sec1_bytes()),
            )
            .unwrap();
        let runtime = Arc::new(DomainRuntime::open(&home).unwrap());
        let state = runtime.state().unwrap();
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: "command-lease".into(),
            expected_revision: state.revision,
            command: Command::CreateRequest {
                request_id: "request-lease".into(),
                body: "Work".into(),
                auto_merge: false, maintenance: false, message: false,
            },
        };
        let (checked_tx, checked_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let writing = runtime.clone();
        let writer_home = home.clone();
        let device_id = device.id.clone();
        let writer = std::thread::spawn(move || {
            writing.execute(
                &Actor::User {
                    user_id: format!("device:{device_id}"),
                },
                &envelope,
                None,
                || {
                    assert!(
                        Auth::open(&writer_home)
                            .unwrap()
                            .list()
                            .iter()
                            .any(|entry| entry.id == device_id)
                    );
                    checked_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    Ok(())
                },
            )
        });
        checked_rx.recv().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoker_home = home.clone();
        let revoke_id = device.id.clone();
        let revoker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let changed = Auth::open(&revoker_home)
                .unwrap()
                .revoke(&revoke_id)
                .unwrap();
            revoked_tx.send(changed).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(
            revoked_rx.recv_timeout(Duration::from_millis(30)).is_err(),
            "revoke returned while writer still held approval lease"
        );
        resume_tx.send(()).unwrap();
        assert!(writer.join().unwrap().unwrap().changed);
        assert!(revoked_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        revoker.join().unwrap();
        assert!(
            runtime
                .state()
                .unwrap()
                .requests
                .contains_key(&"request-lease".into())
        );
        assert!(
            !Auth::open(&home)
                .unwrap()
                .list()
                .iter()
                .any(|entry| entry.id == device.id)
        );
        drop(runtime);
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn reads_never_wait_for_the_store_and_see_each_commit() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-domain-snapshot-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let runtime = Arc::new(DomainRuntime::open(&home).unwrap());
        let before = runtime.state().unwrap();
        assert!(Arc::ptr_eq(&before, &runtime.state().unwrap()), "a read copies nothing");
        let store = runtime.store.lock().unwrap();
        let (read, done) = mpsc::channel();
        let reader = runtime.clone();
        std::thread::spawn(move || read.send(reader.state().unwrap().revision).unwrap());
        assert_eq!(done.recv_timeout(Duration::from_secs(2)), Ok(before.revision), "a read while a writer holds the store");
        drop(store);
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: before.data_epoch.clone(),
            command_id: "agent".into(),
            expected_revision: before.revision,
            command: Command::RegisterAgent { agent_id: "agent-a".into(), name: "A".into(), at: 0 },
        };
        runtime.execute(&Actor::System, &envelope, None, || Ok(())).unwrap();
        let after = runtime.state().unwrap();
        assert_eq!(after.revision, before.revision + 1);
        assert!(after.agents.contains_key(&AgentId::from("agent-a")));
        assert!(before.agents.is_empty(), "a reader's snapshot never changes under it");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn every_commit_publishes_its_state_with_the_index_of_that_revision() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-domain-read-index-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let runtime = DomainRuntime::open(&home).unwrap();
        let opened = runtime.read();
        assert!(opened.index().describes(opened.state()), "the index of the state the daemon opened");
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: opened.state().data_epoch.clone(),
            command_id: "agent".into(),
            expected_revision: opened.state().revision,
            command: Command::RegisterAgent { agent_id: "agent-a".into(), name: "A".into(), at: 0 },
        };
        runtime.execute(&Actor::System, &envelope, None, || Ok(())).unwrap();
        let committed = runtime.read();
        assert_eq!(committed.state().revision, opened.state().revision + 1);
        assert!(committed.index().describes(committed.state()), "the index of the committed revision");
        assert!(committed.index().node(&AgentId::from("agent-a")).is_some(), "the index reads the new node");
        assert!(Arc::ptr_eq(committed.state(), &runtime.state().unwrap()), "state() is the state of the snapshot");
        runtime.execute(&Actor::System, &envelope, None, || Ok(())).unwrap();
        assert!(Arc::ptr_eq(&committed, &runtime.read()), "a replayed command builds no index");
        let error = ReadSnapshot::pair(Arc::clone(committed.state()), Arc::clone(opened.index())).unwrap_err();
        assert_eq!(error.code, "stale_revision", "an index of another revision is refused");
        drop(runtime);
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod producer_revision_tests {
    use super::*;

    #[test]
    fn a_bearer_producer_may_omit_the_revision_and_retries_replay_its_receipt() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-domain-producer-revision-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let runtime = DomainRuntime::open(&home).unwrap();
        let envelope = |runtime: &DomainRuntime, id: &str, expected: Option<Revision>, command| {
            let state = runtime.state().unwrap();
            CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
                command_id: id.into(),
                expected_revision: expected.unwrap_or(state.revision),
                command,
            }
        };
        runtime
            .execute(
                &Actor::System,
                &envelope(
                    &runtime,
                    "agent",
                    None,
                    Command::RegisterAgent {
                        agent_id: "agent-a".into(),
                        name: "A".into(), at: 0
                    },
                ),
                None,
                || Ok(()),
            )
            .unwrap();
        let user = Actor::User {
            user_id: "u".into(),
        };
        runtime
            .execute(
                &user,
                &envelope(
                    &runtime,
                    "request",
                    None,
                    Command::CreateRequest {
                        request_id: "request-a".into(),
                        body: "Work".into(),
                        auto_merge: false, maintenance: false, message: false,
                    },
                ),
                None,
                || Ok(()),
            )
            .unwrap();
        runtime
            .execute(
                &Actor::Orchestrator { generation: 0 },
                &envelope(
                    &runtime,
                    "task",
                    None,
                    Command::CreateTask { plan_items: None,
                        task_id: "task-a".into(),
                        request_id: "request-a".into(),
                        title: "Task".into(),
                        agent_id: Some("agent-a".into()),
                        review_prs: false,
                        dependencies: Default::default(),
                        stack: None,
                        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
                    },
                ),
                None,
                || Ok(()),
            )
            .unwrap();
        let token = runtime
            .issue(Principal::Agent {
                session_id: "agent-a".into(),
                task_binding: None,
                provider_agent_id: None,
            })
            .unwrap();
        let bearer = token.expose_for_child_environment();
        let publish = envelope(
            &runtime,
            "producer-info",
            Some(0),
            Command::PublishInformation {
                attachments: vec![],
                interaction_id: "info-a".into(),
                scope: Scope::Task {
                    task_id: "task-a".into(),
                },
                recipient: Recipient::Orchestrator,
                body: "Progress".into(), card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
            },
        );
        let revision = runtime.state().unwrap().revision;
        let first = runtime.execute_bearer(bearer, &publish).unwrap();
        assert!(first.changed);
        assert_eq!(first.revision, revision + 1);
        assert_eq!(
            runtime.execute_bearer(bearer, &publish).unwrap(),
            first,
            "a retry with the bytes the producer sent replays its receipt"
        );
        assert_eq!(runtime.state().unwrap().revision, revision + 1);
        let stale = runtime.execute(
            &user,
            &CommandEnvelope {
                expected_revision: 0,
                ..envelope(
                    &runtime,
                    "user-zero",
                    None,
                    Command::CreateRequest {
                        request_id: "request-b".into(),
                        body: "More".into(),
                        auto_merge: false, maintenance: false, message: false,
                    },
                )
            },
            None,
            || Ok(()),
        );
        assert!(
            matches!(stale, Err(StoreError::Domain(error)) if error.code == "stale_revision"),
            "a user command is never filled"
        );
        drop(runtime);
        std::fs::remove_dir_all(home).unwrap();
    }
}

mod capabilities;
mod inbox;
mod problems;
mod rejections;

#[cfg(test)]
mod cost_tests;
#[cfg(test)]
mod mcp_flow_tests;
#[cfg(test)]
mod system_command_tests;
#[cfg(test)]
mod wait_review_tests;
#[cfg(test)]
mod poison_tests;
