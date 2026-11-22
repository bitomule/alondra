//! Serialization boundary for the SQL domain and its authenticated producers.
use crate::domain_store::IntoDomainError as _;
use crate::locks::{Poisonable as _, PoisonableRw as _};
use crate::domain_authority::{Authority, Capability, Principal};
use crate::runtime_mutex::RuntimeMutex;
use crate::domain_store::{schedule_command, Journal, JournalFiles, OwnerJournal, StagedOutcome, StoreError, TransportIdentity};
use alondra_domain_rules::*;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::watch;

pub struct DomainRuntime {
    pub read_work: crate::blocking_work::BlockingWork,
    pub producer_work: crate::blocking_work::BlockingWork,
    pub external_work: crate::blocking_work::BlockingWork,
    pub command_work: crate::blocking_work::BlockingWork,
    pub hook_work: crate::blocking_work::BlockingWork,
    pub guard_work: crate::blocking_work::BlockingWork,
    home: PathBuf,
    /// A holder that panics can leave it half-changed, so the daemon exits: the guard stays held
    /// after the SQLite commit while the snapshot is published, the commit acknowledged, its seals
    /// kept and the watchers woken; a fresh start rebuilds all of them from the database.
    pub store: RuntimeMutex<Box<dyn Journal>>,
    /// A read-only connection of the views' own: in WAL it reads the last commit while a commit
    /// holds `store`, so a screen never waits for the writer. A holder that panics leaves it
    /// whole, so it is recovered: the next read ends the transaction it left open.
    pub reads: Mutex<Box<dyn Journal>>,
    /// The last committed state and the read index built from it. Readers take them without the
    /// store, a transaction or a copy; only a commit replaces them, while it still holds the store.
    /// A holder that panics leaves it whole, so it is recovered: its only write swaps one `Arc`,
    /// built outside it.
    snapshot: RwLock<Arc<ReadSnapshot>>,
    snapshot_changed: watch::Sender<u64>,
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
    pub stored_reads: Arc<crate::service_files::StoredReads>,
    /// User messages for sessions in the user's own terminal, handed at their next Stop hook.
    /// A holder that panics leaves it whole, so it is recovered: one remove or lookup each.
    pub held:
        Mutex<std::collections::BTreeMap<alondra_domain_rules::AgentId, Vec<String>>>,
    rejections: rejections::Rejections,
    /// Store seals of commits whose state readers already see, for the SDK owners to claim,
    /// send and acknowledge.
    pub seals: seals::Seals,
    /// Makes every write of the capability registry fail as on a full disk.
    pub full_disk: std::sync::atomic::AtomicBool,
    /// What each subsystem keeps, without the runtime naming it.
    extensions: extensions::Extensions,
    /// The subsystems that hear of every commit, in the order the engine registered them.
    observers: Vec<Box<dyn CommitObserver>>,
    /// The ports the pieces of the engine call each other through, given when the engine is put together.
    ports: Arc<alondra_engine_ports::Registry>,
    /// How many turns the orchestrator has ended, as its own record says: the evidence Alondra hands
    /// it what it still has to re-plan by.
    pub orchestrator_rests: std::sync::atomic::AtomicU64,
    /// Counts what the orchestrator did, for the watchdog that tells a working orchestrator from a
    /// stuck one.
    orchestrator_progress: std::sync::atomic::AtomicU64,
    before_owner_publish: Mutex<Option<Box<dyn FnMut() + Send>>>,
    before_next_commit: Mutex<Option<Box<dyn FnOnce(&DomainRuntime) + Send>>>,
    before_acknowledge: Mutex<Option<Box<dyn FnMut() + Send>>>,
    /// How many read indexes publication built: one per commit that changed the domain.
    pub index_builds: std::sync::atomic::AtomicU64,
}
pub use alondra_daemon_engine_model::retention::Retained;

pub mod batch_operations;
pub mod extensions;
pub mod observers;
pub mod seals;

pub use extensions::{Extension, Slot};
pub use observers::{Carried, CommitObserver};

impl alondra_engine_ports::Runtime for DomainRuntime {
    fn ports(&self) -> &alondra_engine_ports::Registry {
        &self.ports
    }

    fn extensions(&self) -> &alondra_engine_ports::extensions::Extensions {
        &self.extensions
    }

    fn home(&self) -> &Path {
        &self.home
    }

    fn state(&self) -> Arc<DomainState> {
        Arc::clone(self.read().state())
    }

    fn system_command(&self, envelope: CommandEnvelope) -> Result<CommandOutcome, DomainError> {
        self.execute_as_system(envelope.clone()).map_err(|error| error.into_domain_error(&envelope))
    }

    fn wake_seals(&self) {
        self.seals.wake();
    }

    fn launch_plan(&self, task: &alondra_domain_rules::TaskId) -> Option<alondra_domain_rules::LaunchPlan> {
        DomainRuntime::launch_plan(self, task).ok().flatten()
    }
}

impl DomainRuntime {
    pub fn ports(&self) -> &alondra_engine_ports::Registry {
        &self.ports
    }

    /// The runtime a port was called on: a daemon runs no other kind. A bridge while the pieces of
    /// the engine are still written against this type; each asks the [`alondra_engine_ports::Runtime`]
    /// context instead once it no longer depends on the kernel.
    pub fn of(runtime: &dyn alondra_engine_ports::Runtime) -> &DomainRuntime {
        (runtime as &dyn std::any::Any).downcast_ref().expect("ports are called on the engine's domain runtime")
    }

    pub fn note_orchestrator_progress(&self) {
        self.orchestrator_progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn orchestrator_progress(&self) -> u64 {
        self.orchestrator_progress.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn transport_receipt(
        &self,
        identity: TransportIdentity<'_>,
        authorize: impl FnOnce() -> std::result::Result<(), StoreError>,
    ) -> Result<Option<CommandOutcome>, StoreError> {
        let mut store = self.store.lock_or_exit("the domain store");
        let _approval = crate::approval_lease::ApprovalLease::shared(&self.approval_home)?;
        authorize()?;
        store.transport_receipt(identity)
    }

    /// A runtime that tells no subsystem of its commits, for the kernel's own tests.
    #[cfg(test)]
    pub fn open_unwired(home: &Path) -> Result<Self> {
        Self::open_unwired_at(home, &crate::domain_store::active_path(home))
    }

    /// The same at `database`, for the tests of the kernel and of the subsystem crates.
    pub fn open_unwired_at(home: &Path, database: &Path) -> Result<Self> {
        Self::open_observed(home, database, crate::test_journal(), None, vec![])
    }

    /// The runtime of the domain database at `database`, telling `observers` of every commit,
    /// those it makes while opening included.
    pub fn open_observed(
        home: &Path,
        database: &Path,
        files: &dyn JournalFiles,
        decisions: Option<Arc<dyn crate::domain_store::wording::SdkDecisions>>,
        observers: Vec<Box<dyn CommitObserver>>,
    ) -> Result<Self> {
        let mut store = files.open(database, decisions)?;
        let reads = files.open_readonly(database)?;
        store.recover_settings(home)?;
        let options = crate::options::options_at(home).unwrap_or_default();
        let home_of_user = crate::options::user_home();
        let projects_root = options.projects_root(&home_of_user);
        store.fill_task_projects(&projects_root)?;
        let ports = Arc::new(alondra_engine_ports::Registry::default());
        #[cfg(test)]
        alondra_engine_doubles::machine::wire(&ports, alondra_daemon_support::watcher::claude_home);
        store.protect(alondra_engine_ports::worktrees::WorktreeRules::new(
            alondra_engine_ports::worktrees::Protected { home: home_of_user, projects_root },
            ports.clone(),
        ));
        store.fill_task_models()?;
        let state = store.load()?;
        let (changed, _) = watch::channel(state.revision);
        let (schedule_changed, _) = watch::channel(state.revision);
        let authority = capabilities::load(home, &state)?;
        let runtime = Self {
            read_work: crate::blocking_work::BlockingWork::new(2),
            producer_work: crate::blocking_work::BlockingWork::new(2),
            external_work: crate::blocking_work::BlockingWork::new(2),
            command_work: crate::blocking_work::BlockingWork::new(2),
            hook_work: crate::blocking_work::BlockingWork::new(1),
            guard_work: crate::blocking_work::BlockingWork::new(2),
            home: home.to_path_buf(),
            store: RuntimeMutex::new(store),
            reads: Mutex::new(reads),
            snapshot: RwLock::new(Arc::new(ReadSnapshot::build(state))),
            snapshot_changed: watch::channel(0).0,
            approval_home: home.to_path_buf(),
            authority: RuntimeMutex::new(authority),
            operations_live: Default::default(), operations_changed: watch::channel(0).0,
            changed,
            schedule_changed,
            stored_reads: Default::default(),
            held: Default::default(),
            rejections: Default::default(),
            seals: Default::default(),
            full_disk: Default::default(),
            extensions: Default::default(),
            observers,
            ports,
            orchestrator_rests: Default::default(),
            orchestrator_progress: Default::default(),
            before_owner_publish: Default::default(),
            before_next_commit: Default::default(),
            before_acknowledge: Default::default(),
            index_builds: Default::default(),
        };
        runtime.review_waits()?;
        runtime.run_request_tasks(Command::AcknowledgeRequestTasks)?;
        Ok(runtime)
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
    /// Tells the user, on a card per request, the tasks the orchestrator gave their open
    /// requests that no card named yet. Run once a turn of the orchestrator ended: a turn that
    /// failed leaves them for the next one, which finds them still unnamed.
    pub fn announce_request_tasks(&self) -> Result<()> {
        self.run_request_tasks(Command::AnnounceRequestTasks)
    }
    fn run_request_tasks(&self, command: Command) -> Result<()> {
        let state = self.state()?;
        if state.unannounced_tasks().is_empty() {
            return Ok(());
        }
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("request-tasks-{}", crate::auth::random_hex(8)).into(),
            expected_revision: 0,
            command,
        };
        match self.execute_as_system(envelope) {
            Ok(_) => Ok(()),
            Err(StoreError::Domain(error)) => {
                log!("could not record the tasks of the user's requests: {}", error.message);
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
    pub fn home(&self) -> &Path {
        &self.home
    }
    pub fn state(&self) -> Result<Arc<DomainState>> {
        Ok(Arc::clone(self.read().state()))
    }
    /// A read of the committed store that never waits for the writer: what is read through it
    /// is one commit's state.
    pub fn live_read(&self) -> crate::domain_store::ReadTransaction<'_> {
        crate::domain_store::ReadTransaction::begin(self.reads.lock_or_recover("the domain reader"))
    }
    /// The last committed state with its read index: both of the same revision.
    pub fn read(&self) -> Arc<ReadSnapshot> {
        Arc::clone(&self.snapshot.read_or_recover("the domain snapshot"))
    }
    /// Releases the raw provider evidence and the rows of each retention policy no reader needs
    /// any more as of `now`, one bounded pass at a time under the writer lock so other writers run
    /// between passes, then gives the disk the space.
    pub fn retain(&self, now: i64) -> Result<Retained, StoreError> {
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
        let policies = self.store.lock_or_exit("the domain store").retention_policies();
        for policy in policies {
            let mut changed = 0;
            loop {
                let pass = self.store.lock_or_exit("the domain store").retain(policy, now, PASS)?;
                changed += pass;
                if pass < PASS {
                    break;
                }
            }
            retained.tables.push((policy, changed));
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
    pub fn own<T>(
        &self,
        scope: alondra_store::AuthorityScope,
        stage: impl FnOnce(&mut dyn OwnerJournal) -> Result<T, StoreError>,
    ) -> Result<(T, Vec<StagedOutcome>), StoreError> {
        self.own_projected(scope, stage, |_| Ok(()))
    }

    pub fn own_projected<T>(
        &self,
        scope: alondra_store::AuthorityScope,
        stage: impl FnOnce(&mut dyn OwnerJournal) -> Result<T, StoreError>,
        project: impl FnOnce(&mut dyn Journal) -> anyhow::Result<()>,
    ) -> Result<(T, Vec<StagedOutcome>), StoreError> {
        let mut store = self.store.lock_or_exit("the domain store");
        let _approval = crate::approval_lease::ApprovalLease::shared(&self.approval_home)?;
        let (value, commit) = store.own(scope, stage)?;
        if let Err(why) = project(&mut **store) {
            log!("The authority transaction committed; its recoverable settings projection needs repair before another settings read: {why:#}");
        }
        if let Some(hook) = self.before_owner_publish.lock_or_recover("a test's hook").as_mut() {
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
    pub fn take_acknowledgements(&self) -> Vec<alondra_store::Committed> {
        self.seals.take_all()
    }
    /// Runs `hook` once, as the next command reaches the writer lock.
    pub fn before_next_commit(&self, hook: impl FnOnce(&DomainRuntime) + Send + 'static) {
        *self.before_next_commit.lock_or_recover("a test's hook") = Some(Box::new(hook));
    }
    pub fn before_owner_publish(&self, hook: impl FnMut() + Send + 'static) {
        *self.before_owner_publish.lock_or_recover("a test's hook") = Some(Box::new(hook));
    }
    pub fn on_acknowledge(&self, hook: impl FnMut() + Send + 'static) {
        *self.before_acknowledge.lock_or_recover("a test's hook") = Some(Box::new(hook));
    }
    pub fn before_acknowledge(&self) {
        if let Some(hook) = self.before_acknowledge.lock_or_recover("a test's hook").as_mut() {
            hook();
        }
    }
    /// Builds the index of a new commit; a replayed command hands back the state already read.
    fn publish(&self, state: &Arc<DomainState>) {
        if Arc::ptr_eq(self.read().state(), state) {
            return;
        }
        self.index_builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let before = self.read();
        let snapshot = Arc::new(ReadSnapshot::build_after(Arc::clone(state), &before));
        for observer in &self.observers {
            observer.published(self, before.state(), state);
        }
        for (task, card) in snapshot.index().unlisted_waits() {
            if before.index().task(task).and_then(|read| read.unlisted.as_ref()) != Some(card) {
                log!("{task} waits on card {card} that the inbox does not list");
            }
        }
        {
            let mut current = self.snapshot.write_or_recover("the domain snapshot");
            *current = snapshot;
            self.snapshot_changed.send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        self.seals.wake();
    }
    pub fn publish_batch_report(&self, run: &str, cursor: u64, evidence: &[u8], report: &crate::domain_store::batch_effects::BatchReport) -> Result<(), StoreError> {
        let mut store = self.store.lock_or_exit("the domain store");
        store.record_batch_report(run, cursor, evidence, report)?;
        if matches!(report.kind.as_str(), "assistant" | "user" | "result" | "process_exited"
            | "alondra/threadReady" | "alondra/inputAccepted" | "turn/started" | "turn/completed"
            | "item/started" | "item/completed"
            | "model/rerouted") {
            self.note_orchestrator_progress();
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
    pub fn reload(&self) {
        let state = self.store.lock_or_exit("the domain store").load().unwrap();
        self.publish(&state);
    }
    pub fn schedule_request_tools(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<Vec<String>>> {
        self.store
            .lock_or_exit("the domain store")
            .schedule_request_tools(request_id)
    }
    pub fn was_schedule_launch_failed(&self, request_id: &RequestId) -> Result<bool> {
        self.store
            .lock_or_exit("the domain store")
            .was_schedule_launch_failed(request_id)
    }
    pub fn subscribe(&self) -> watch::Receiver<Revision> {
        self.changed.subscribe()
    }
    /// Changes when a snapshot is replaced, including a reload at the same domain revision.
    /// Release the receiver's borrow before reading the snapshot: publication holds the snapshot
    /// lock while announcing its generation, then releases both before waking SDK owners.
    pub fn subscribe_snapshots(&self) -> watch::Receiver<u64> {
        self.snapshot_changed.subscribe()
    }
    /// Wakes whoever waits on a change of the domain after something outside it ended, such as the
    /// thread that stops a host: the waiter looks again at what that end settled.
    pub fn wake_subscribers(&self) {
        let revision = *self.changed.borrow();
        self.changed.send_replace(revision);
    }
    pub fn subscribe_schedules(&self) -> watch::Receiver<Revision> {
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
    /// for that session stops working the moment the new one is issued. A full
    /// registry first retires the capabilities of agents with no host left; if
    /// it is still full the start waits and the user sees why on one card.
    pub fn bind_agent(&self, session_id: &str) -> Result<Capability, StoreError> {
        let bound = self.bind_agent_in_registry(session_id);
        match &bound {
            Err(StoreError::Domain(error)) if error.code == capability_capacity_card::CAPABILITY_CAPACITY_CODE => self.raise_capacity_card(),
            Ok(_) => self.settle_capacity_card(),
            Err(_) => {}
        }
        bound
    }

    fn bind_agent_in_registry(&self, session_id: &str) -> Result<Capability, StoreError> {
        let mut authority = self.authority.lock_or_recover("the domain authority");
        let state = self.state()?;
        if !state.agents.contains_key(&AgentId(session_id.to_owned())) {
            return Err(DomainError::new("not_found", "Agent does not exist").into());
        }
        authority.revoke_agent(session_id);
        if authority.full() {
            let live = capabilities::live_agents(&state);
            if authority.retain_agents(|agent| live.contains(agent)) {
                log!("domain: the capability registry was full; the capabilities of agents with no host left were retired");
            }
        }
        let capability = authority
            .issue(Principal::Agent {
                session_id: session_id.to_owned(),
                task_binding: None,
                provider_agent_id: None,
            })
            .map_err(|message| if message == crate::domain_authority::CAPACITY_REACHED {
                DomainError::new(capability_capacity_card::CAPABILITY_CAPACITY_CODE, "Alondra has no room for another node credential: every one belongs to a node that may still work")
            } else {
                DomainError::new("forbidden", message)
            })?;
        self.persist_issued(&mut authority, &capability)?;
        Ok(capability)
    }

    fn open_capacity_cards(&self) -> Vec<InteractionId> {
        let Ok(state) = self.state() else { return vec![] };
        state.interactions.values()
            .filter(|card| card.system_card == Some(SystemCard::CapabilityCapacity) && !card.withdrawn
                && matches!(card.payload, InteractionPayload::Question { answer: None, .. }))
            .map(|card| card.id.clone())
            .collect()
    }

    fn raise_capacity_card(&self) {
        if !self.open_capacity_cards().is_empty() {
            return;
        }
        let live = self.authority.lock_or_recover("the domain authority").entries().count();
        let card = capability_capacity_card::CapabilityCapacityCard { live: live as u32, limit: crate::domain_authority::CAPACITY as u32 };
        let (need, why) = card.text(alondra_core::text::Lang::En);
        let option = QuestionOption {
            id: capability_capacity_card::CHECK_AGAIN_OPTION.into(),
            label: capability_capacity_card::CHECK_AGAIN_LABEL.into(),
            description: None,
        };
        let id = format!("{}{}", capability_capacity_card::CAPABILITY_CAPACITY_PREFIX, alondra_core::now_millis());
        if let Err(error) = self.raise_worded_problem(&id, Some(SystemCard::CapabilityCapacity), capability_capacity_card::CAPABILITY_CAPACITY_CAUSE,
            need, why, card.to_body(), vec![option], Some(capability_capacity_card::CHECK_AGAIN_OPTION)) {
            log!("domain: could not tell the user the capability registry is full: {error:?}");
        }
    }

    /// A registry with room again withdraws the card that said it had none.
    fn settle_capacity_card(&self) {
        if self.authority.lock_or_recover("the domain authority").full() {
            return;
        }
        for id in self.open_capacity_cards() {
            if let Err(error) = self.withdraw_problem(&id) {
                log!("domain: could not withdraw the card of the full capability registry: {error:?}");
            }
        }
    }

    /// A capability that could not be written is never handed out, so it leaves the registry in
    /// memory too: memory and disk hold the same live capabilities. Revocations stay in memory
    /// even unwritten, so a revoked bearer is refused either way.
    fn persist_issued(&self, authority: &mut Authority, capability: &Capability) -> Result<()> {
        self.persist_authority(authority).inspect_err(|_| authority.forget(capability))
    }

    fn persist_authority(&self, authority: &Authority) -> Result<()> {
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

    /// The digest of the capability the session holds now, which only it can retire later.
    pub fn agent_capability(&self, session: &str) -> Option<[u8; 32]> {
        self.authority.lock_or_recover("the domain authority").agent_digest(session)
    }

    /// Revokes the capability named by `digest`: one its session was issued after it stays.
    pub fn revoke_capability(&self, digest: &[u8; 32]) -> Result<()> {
        let persisted = {
            let mut authority = self.authority.lock_or_recover("the domain authority");
            authority.revoke_digest(digest);
            self.persist_authority(&authority)
        };
        self.settle_capacity_card();
        persisted
    }

    pub fn revoke_agent(&self, session: &str) -> Result<()> {
        let persisted = {
            let mut authority = self.authority.lock_or_recover("the domain authority");
            authority.revoke_agent(session);
            self.persist_authority(&authority)
        };
        self.settle_capacity_card();
        persisted
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

    pub fn request_plan_mode(&self, bearer: &str, task: &TaskId, operation: &str) -> Result<CommandOutcome,StoreError> {
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
            let already_requested = state.in_plan_mode(task);
            let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION,data_epoch:epoch,
                command_id:format!("plan-mode-{operation}").into(),expected_revision:0,
                command:Command::SetPermissionMode { task_id:task.clone(),mode:PermissionModeRecord::Plan } };
            let outcome = owner.stage_command(&Actor::System,&envelope,crate::domain_store::Attached::None)?.clone();
            if !outcome.changed || (already_requested && outcome.revision > before_revision) { return Ok(outcome); }
            let continuation = CommandEnvelope { protocol_version:PROTOCOL_VERSION,data_epoch:envelope.data_epoch.clone(),
                command_id:format!("plan-mode-input-{operation}").into(),expected_revision:0,
                command:Command::TellNode { task_id:task.clone(),text:"Alondra has enabled plan mode. Prepare the task plan for the user's approval before implementation. Preserve the current session and completed work.".into(), from_user: false, answers: None } };
            owner.stage_command(&Actor::System,&continuation,crate::domain_store::Attached::None).cloned()
        })?;
        Ok(outcome)
    }

    /// Expose only the public protocol cursor to a daemon-authorized producer.
    /// Commands recheck the capability while holding the writer lock.
    pub fn read_current_task_context(&self, bearer: &str, task: &TaskId, after: Option<&alondra_store::ContextRef>, limit: usize) -> std::result::Result<alondra_store::ContextPage, DomainError> {
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

    pub fn read_batch_outcomes(&self, bearer: &str, run: Option<&str>, list_runs: bool, after: &str, limit: usize) -> std::result::Result<serde_json::Value, DomainError> {
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

    pub fn launch_plan(&self, task_id: &TaskId) -> Result<Option<alondra_daemon_engine_model::LaunchPlan>> {
        self.store.lock_or_exit("the domain store").launch_plan(task_id)
    }
    pub fn inherited_worktree(&self, agent: &str) -> Result<Option<String>> {
        self.store.lock_or_exit("the domain store").inherited_worktree(agent)
    }
    pub fn set_inherited_worktree(&self, agent: &str, dir: &str, from_agent: &str) -> Result<()> {
        self.store.lock_or_exit("the domain store").set_inherited_worktree(agent, dir, from_agent, alondra_core::now_millis())
    }
    /// Why `path` cannot be a task's worktree, if it cannot.
    pub fn registrable_worktree(&self, path: &Path) -> std::result::Result<(), String> {
        let rules = self.store.lock_or_exit("the domain store").protected().clone();
        rules.registrable(path)
    }
    /// Gives every card whose attachments still sit in a worktree its own copy of them. A file
    /// that is gone or no longer matches its digest stays as it was; the names come back.
    pub fn keep_attachments(&self) -> Result<(usize, Vec<String>)> {
        let mut store = self.store.lock_or_exit("the domain store");
        let roots = worktree_roots(&*store.load()?);
        let (mut kept, mut lost) = (0, vec![]);
        for (interaction, id, name, digest, reference) in store.attachments_kept_elsewhere()? {
            match crate::domain_content::keep_attachment(&self.home, &reference, &digest, &roots) {
                Ok(copy) => {
                    store.keep_attachment_at(&interaction, &id, &copy)?;
                    kept += 1;
                }
                Err(_) => lost.push(format!("{interaction}: {name}")),
            }
        }
        Ok((kept, lost))
    }
    pub fn task_worktree(&self, task: &TaskId) -> Result<Option<String>> {
        self.store.lock_or_exit("the domain store").task_worktree(task)
    }
    /// The worktree the task's node works in, kept for its next launch and recorded as the
    /// task's own, which it ends only once that worktree is gone.
    pub fn set_task_worktree(&self, task: &TaskId, dir: &str) -> Result<()> {
        self.store.lock_or_exit("the domain store").set_task_worktree(task, dir, alondra_core::now_millis())?;
        let command = Command::RecordTaskWorktree { task_id: task.clone(), path: dir.to_owned(), origin: WorktreeOrigin::Task, branch: None };
        self.execute_system_command(command).map_err(|error| anyhow::anyhow!("{error}"))
    }
    /// Carries out `command` as the system, against the state it reads now.
    pub fn execute_system_command(&self, command: Command) -> Result<(), StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("github-{}", crate::auth::random_hex(16)).into(),
            expected_revision: state.revision,
            command,
        };
        self.execute_as_system(envelope).map(drop)
    }
    pub fn forget_inherited_worktree(&self, agent: &str) -> Result<()> {
        self.store.lock_or_exit("the domain store").forget_inherited_worktree(agent)
    }
    pub fn record_model_decision(&self, decision: &crate::domain_store::model_metrics::Decision) -> Result<()> {
        self.store.lock_or_exit("the domain store").record_model_decision(decision)
    }

    pub fn record_spend(&self, spent: &crate::domain_store::metrics_fold::Spent<'_>) -> Result<()> {
        self.store.lock_or_exit("the domain store").record_spend(spent)
    }

    pub fn record_session_start(&self, started: &alondra_engine_ports::journal::metrics::SessionStarted<'_>) -> Result<()> {
        self.store.lock_or_exit("the domain store").record_session_start(started)
    }

    pub fn session_starts(&self, task: &str) -> Result<Vec<alondra_engine_ports::journal::metrics::RecordedStart>> {
        self.store.lock_or_exit("the domain store").session_starts(task)
    }

    pub fn record_release(&self, version: &str, at: alondra_core::Millis) -> Result<()> {
        self.store.lock_or_exit("the domain store").record_release(version, at)
    }

    pub fn prs_without_size(&self, now: alondra_core::Millis, limit: usize) -> Result<Vec<(String, String)>> {
        self.store.lock_or_exit("the domain store").prs_without_size(now, limit)
    }

    pub fn record_pr_lines(&self, task_id: &str, lines: i64) -> Result<()> {
        self.store.lock_or_exit("the domain store").record_pr_lines(task_id, lines)
    }

    /// What the Metrics screen sums, with the tasks that ended since the last fold folded first.
    pub fn metrics_inputs(&self, now: alondra_core::Millis) -> Result<alondra_core::metrics::report::MetricsInputs> {
        const PASS: usize = 256;
        while self.store.lock_or_exit("the domain store").fold_ended_tasks(PASS)? == PASS {}
        self.store.lock_or_exit("the domain store").metrics_inputs(now)
    }

    /// What the user chose when asking for the Request, if it was asked for with choices.
    pub fn request_launch(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<crate::domain_store::RequestLaunch>> {
        self.store
            .lock_or_exit("the domain store")
            .request_launch(request_id)
    }

    pub fn execute_with(
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
    pub fn execute_as_system(&self, envelope: CommandEnvelope) -> Result<CommandOutcome, StoreError> {
        let envelope = CommandEnvelope { expected_revision: 0, ..envelope };
        self.commit(&Actor::System, &envelope, None, None, true, || Ok(()))
    }

    /// A fixture's synchronous actor step takes its revision on the writer lane, as an internal
    /// producer does. Client tests use `execute_with` to exercise their optimistic revision.
    pub fn execute_fixture(&self, actor: &Actor, envelope: CommandEnvelope) -> Result<CommandOutcome, StoreError> {
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
        {
            let hook = self.before_next_commit.lock_or_recover("a test's hook").take();
            if let Some(hook) = hook {
                hook(self);
            }
        }
        if let Some(carry) = self.observers.iter().find_map(|observer| observer.carries_out(self, actor, &envelope.command)) {
            let _approval = crate::approval_lease::ApprovalLease::shared(&self.approval_home)?;
            authorize()?;
            return carry();
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
        let prepared = crate::domain_content::prepare(&self.home, envelope, &worktree_roots(&*store.load()?))?;
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
        if let Some(sealed) = commit.sealed.take().filter(|sealed| !seals::empty(sealed)) {
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
            for observer in &self.observers {
                observer.changed(self, &commit.state);
            }
            for observer in &self.observers {
                observer.committed(self, &prepared.envelope.command, &commit.state);
            }
        }
        Ok(commit.outcome)
    }

    pub fn execute_bearer(
        &self,
        bearer: &str,
        envelope: &CommandEnvelope,
    ) -> Result<CommandOutcome, StoreError> {
        self.execute_bearer_checked(bearer, envelope, |_| Ok(()))
    }

    /// Rechecks external preflight facts against the state protected by the command writer.
    pub fn execute_bearer_checked(
        &self,
        bearer: &str,
        envelope: &CommandEnvelope,
        check: impl FnOnce(&DomainState) -> Result<(), DomainError>,
    ) -> Result<CommandOutcome, StoreError> {
        self.execute_bearer_answered(bearer, envelope, check).map(|(outcome, _)| outcome)
    }

    /// The outcome, and whether it is the receipt of the same command already recorded.
    pub fn execute_bearer_answered(
        &self,
        bearer: &str,
        envelope: &CommandEnvelope,
        check: impl FnOnce(&DomainState) -> Result<(), DomainError>,
    ) -> Result<(CommandOutcome, bool), StoreError> {
        let result = self.execute_bearer_once(bearer, envelope, check);
        self.count_rejections(bearer, envelope, &result);
        result
    }

    fn execute_bearer_once(
        &self,
        bearer: &str,
        envelope: &CommandEnvelope,
        check: impl FnOnce(&DomainState) -> Result<(), DomainError>,
    ) -> Result<(CommandOutcome, bool), StoreError> {
        // Keep the capability lease until commit: concurrent revocation cannot pass this write.
        let mut authority = self.authority.lock_or_recover("the domain authority");
        let principal = authority
            .resolve(&envelope.data_epoch.0, bearer)
            .map_err(|message| DomainError::new("forbidden", message))?;
        let mut store = self.store.lock_or_exit("the domain store");
        let state = store.load()?;
        check(&state)?;
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
        let prepared = crate::domain_content::prepare(&self.home, envelope, &worktree_roots(&*store.load()?))?;
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
            self.note_orchestrator_progress();
        }
        self.publish(&commit.state);
        if let Some(sealed) = commit.sealed.take().filter(|sealed| !seals::empty(sealed)) {
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
            for observer in &self.observers {
                observer.changed(self, &commit.state);
            }
        }
        Ok((commit.outcome, commit.replayed))
    }
}

/// The command's name on the wire, such as `create_task`.
fn command_kind(command: &Command) -> String {
    serde_json::to_value(command).ok().and_then(|value| value["kind"].as_str().map(str::to_owned)).unwrap_or_default()
}

/// Exhaustive scope resolution for delegated producers; unscoped commands are never admitted.
/// Where a node's files may be read from besides Alondra's folder: the worktrees of the tasks.
fn worktree_roots(state: &DomainState) -> Vec<std::path::PathBuf> {
    state.tasks.values().flat_map(|task| task.worktrees.keys()).map(std::path::PathBuf::from).collect()
}

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
        | Command::CancelTask { task_id, .. }
        | Command::AdoptOpenPr { task_id, .. }
        | Command::ClosePrOfCancelledTask { task_id, .. }
        | Command::SettleOpenPr { task_id, .. }
        | Command::ReopenTask { task_id }
        | Command::ConfirmCleanup { task_id, .. }
        | Command::RecordTaskWorktree { task_id, .. }
        | Command::NotePrOpened { task_id, .. }
        | Command::NotePrChanges { task_id }
        | Command::RecordOpenedPr { task_id, .. }
        | Command::AskToRegisterPr { task_id }
        | Command::ReleaseTaskWorktree { task_id, .. }
        | Command::DeclarePostMergeWork { task_id, .. }
        | Command::RecordNodeEnd { task_id, .. }
        | Command::RecordTaskCleanup { task_id }
        | Command::AdmitExecution { task_id, .. }
        | Command::RecordRuntimeFacts { task_id, .. }
        | Command::RecordTurnBoundary { task_id, .. }
        | Command::RecordClaudeHold { task_id, .. }
        | Command::RecordHostEnded { task_id, .. }
        | Command::RecordHostReleased { task_id, .. }
        | Command::RecordHostRetaken { task_id, .. }
        | Command::RecordInterruptAccepted { task_id, .. }
        | Command::RecordDoubtfulHandoff { task_id, .. }
        | Command::ReconcileRecoveryCards { task_id }
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
        | Command::WaitForTasks { task_id, .. }
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
        | Command::DeferTaskMove { task_id, .. }
        | Command::AbortTaskSelection { task_id, .. }
        | Command::RequestTaskSelection { task_id, .. }
        | Command::MoveTask { task_id, .. }
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
        Command::SnoozeCard { interaction_id, .. }
        | Command::UnsnoozeCard { interaction_id }
        | Command::MarkRead { interaction_id }
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
        Command::ResurfaceDueCards { .. }
        | Command::CreateRequest { .. }
        | Command::CreateCardReply { .. }
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
        | Command::ReviewWaits
        | Command::AnnounceRequestTasks
        | Command::AcknowledgeRequestTasks
        | Command::RegisterAgent { .. }
        | Command::MoveNode { .. }
        | Command::RecordLastWords { .. }
        | Command::AdvanceOrchestratorGeneration { .. }
        | Command::BindOrchestratorSession { .. }
        | Command::HoldHook { .. }
        | Command::HoldQuestions { .. }
        | Command::ProbeClaudeAccess { .. }
        | Command::BeginAccountLogin { .. }
        | Command::CancelAccountLogin { .. }
        | Command::RecordAccountLogin { .. }
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
        | Command::SetMergeMethod { .. }
        | Command::SetBranchUpdate { .. } => None,
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
        let runtime = Arc::new(crate::domain_runtime::DomainRuntime::open_unwired(&home).unwrap());
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
    fn snapshot_publication_wakes_owners_after_invalidation_without_clock_advance() {
        use alondra_core::clock;
        use std::sync::atomic::{AtomicBool, Ordering};

        for reload in [false, true] {
            let time = clock::VirtualRuntime::new(0).unwrap();
            let _entered = time.enter();
            let directory = crate::test_support::Directory::new();
            std::fs::create_dir_all(&directory).unwrap();
            let runtime = Arc::new(DomainRuntime::open_unwired(&directory).unwrap());
            let register = |id: &str| {
                let snapshot = runtime.read();
                CommandEnvelope {
                    protocol_version: PROTOCOL_VERSION,
                    data_epoch: snapshot.state().data_epoch.clone(),
                    command_id: id.into(),
                    expected_revision: snapshot.state().revision,
                    command: Command::RegisterAgent { agent_id: id.into(), name: id.into(), at: 0 },
                }
            };
            runtime.execute(&Actor::System, &register("agent-a"), None, || Ok(())).unwrap();
            assert!(runtime.seals.take_all().is_empty());
            let before = runtime.read();
            let mut snapshots = runtime.subscribe_snapshots();
            let generation = *snapshots.borrow_and_update();
            let domain_changes = runtime.subscribe();

            // Observe the actual notification while it holds the same monitor as wait_for's
            // predicate. Reading the snapshot here also checks publication released its locks.
            let notifications = Arc::new(Mutex::new(Vec::new()));
            let observed = notifications.clone();
            let at_wake = snapshots.clone();
            let weak = Arc::downgrade(&runtime);
            runtime.seals.on_wake(move || {
                let domain = weak.upgrade().unwrap();
                let changed = at_wake.has_changed().unwrap();
                let current_generation = *at_wake.borrow();
                let snapshot = domain.read();
                observed.lock().unwrap().push((changed, current_generation, snapshot.state().revision));
            });

            let (ready, waiting) = mpsc::channel();
            let waiter_domain = runtime.clone();
            let waiter_changes = snapshots.clone();
            let cleanup = Arc::new(AtomicBool::new(false));
            let waiter_cleanup = cleanup.clone();
            let waiter = clock::spawn_thread(move || {
                let first = std::cell::Cell::new(true);
                let committed = waiter_domain.seals.wait_for(&"no-seals".into(), Duration::from_secs(60), || {
                    let changed = waiter_changes.has_changed().unwrap();
                    if first.replace(false) {
                        ready.send(()).unwrap();
                    }
                    changed || waiter_cleanup.load(Ordering::SeqCst)
                });
                assert!(seals::empty(&committed));
                clock::now_millis()
            });
            waiting.recv().unwrap();
            let command = register("agent-b");
            if reload {
                let connection = rusqlite::Connection::open(crate::domain_store::active_path(&directory)).unwrap();
                assert_eq!(connection.execute("UPDATE agents SET name='Renamed' WHERE id='agent-a'", []).unwrap(), 1);
                runtime.reload();
            } else {
                runtime.execute(&Actor::System, &command, None, || Ok(())).unwrap();
            }
            let notifications_at_publish = notifications.lock().unwrap().clone();
            // A removed or reordered wake still releases the thread, then fails on the captured
            // operations, rather than waiting for a real timeout or advancing the virtual clock.
            if !notifications_at_publish.iter().any(|(changed, _, _)| *changed) {
                cleanup.store(true, Ordering::SeqCst);
                runtime.seals.wake();
            }
            assert_eq!(waiter.join().unwrap(), 0);
            assert_eq!(clock::now_millis(), 0);
            let after = runtime.read();
            let expected_revision = before.state().revision + u64::from(!reload);
            assert_eq!(notifications_at_publish, vec![(true, generation + 1, expected_revision)]);
            assert_eq!(*snapshots.borrow_and_update(), generation + 1);
            assert_eq!(after.state().revision, expected_revision);
            assert!(after.index().describes(after.state()));
            assert!(runtime.seals.take_all().is_empty(), "the wake requires no committed seals");
            if reload {
                assert_eq!(before.state().agents[&AgentId::from("agent-a")].name, "agent-a");
                assert_eq!(after.state().agents[&AgentId::from("agent-a")].name, "Renamed");
                assert!(matches!(after.node_changes_since(&before.node_cursor()), alondra_domain_rules::NodeChanges::Reset { .. }));
                assert_eq!(after.index().live_node(&AgentId::from("agent-a")).unwrap().name, "Renamed");
                assert_eq!(before.index().live_node(&AgentId::from("agent-a")).unwrap().name, "agent-a");
                assert!(!domain_changes.has_changed().unwrap(), "reload keeps the existing revision channel's contract");
                runtime.reload();
            } else {
                assert!(!before.state().agents.contains_key(&AgentId::from("agent-b")));
                assert!(after.state().agents.contains_key(&AgentId::from("agent-b")));
                assert_eq!(after.node_changes_since(&before.node_cursor()), alondra_domain_rules::NodeChanges::Changed {
                    cursor: after.node_cursor(), nodes: [AgentId::from("agent-b")].into_iter().collect(),
                });
                assert!(domain_changes.has_changed().unwrap());
                runtime.execute(&Actor::System, &command, None, || Ok(())).unwrap();
            }
            assert!(!snapshots.has_changed().unwrap(), "reusing the published state sends no event");
            assert_eq!(notifications.lock().unwrap().len(), 1, "reusing the snapshot does not wake owners");
        }
    }

    #[test]
    fn reads_never_wait_for_the_store_and_see_each_commit() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-domain-snapshot-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let runtime = Arc::new(crate::domain_runtime::DomainRuntime::open_unwired(&home).unwrap());
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
        let runtime = crate::domain_runtime::DomainRuntime::open_unwired(&home).unwrap();
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
        let runtime = crate::domain_runtime::DomainRuntime::open_unwired(&home).unwrap();
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
                    Command::CreateTask { plan_items: None, continues: None,
                        task_id: "task-a".into(),
                        request_id: "request-a".into(),
                        title: "Task".into(),
                        agent_id: Some("agent-a".into()),
                        review_prs: false,
                        dependencies: Default::default(),
                        stack: None,
                        launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
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
                body: "Progress".into(), card: alondra_domain_rules::CardSpec::derived(alondra_domain_rules::CardKind::Know, "Read the update"),
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
pub mod problems;
mod rejections;

#[cfg(test)]
mod cost_tests;
#[cfg(test)]
mod mcp_flow_tests;
#[cfg(test)]
mod wait_review_tests;
#[cfg(test)]
mod poison_tests;
#[cfg(test)]
mod journal_tests;
