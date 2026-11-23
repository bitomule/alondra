//! Machine pressure is recorded as samples, incidents and admission state. No incident creates
//! a user card or a fallback push. The admission gate resumes waiting nodes after recovery.
//! Disk incidents retain hysteresis; memory only paces new starts and never stops running work.
use crate::engine::EngineOf as _;
use crate::AdmissionOf as _;
use alondra_domain_rules::SystemCard;
#[cfg(test)]
use alondra_core::machine_incident::MACHINE_FIXED_LABEL;
use crate::locks::Poisonable as _;
use alondra_core::machine_incident::{ MACHINE_FIXED_OPTION, MACHINE_PREFIX, MachineCard, MachineCause, MachineResource};
use alondra_view_model::Lang;
use alondra_domain_rules::TaskId;
use alondra_core::{Machine, MemoryPressure, Millis, now_millis};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One machine incident is open at a time, so its card has one cause.
#[cfg(test)]
const MACHINE_CAUSE: &str = "machine";
use std::sync::{Arc, Mutex};

use alondra_engine_ports::disk::Disk;
use std::time::Duration;

use alondra_engine_ports::Runtime;
use crate::machine_state::{MacState, NoRoom};
use alondra_engine_ports::usage::Alerts;

pub(crate) const FILE: &str = "machine-incident.json";
/// Measurements in a row past the recovery line that end an incident: one alone may be a blip.
const HEALTHY_SAMPLES: u32 = 2;
const TICK: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declared {
    pub reason: String,
    #[serde(default)]
    pub resource: Option<MachineResource>,
    pub at: Millis,
    /// The node's turn when it declared: a later turn means it went on.
    pub turn: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Incident {
    pub since: Millis,
    pub cause: MachineCause,
    /// The open card, if the domain could write it.
    pub card: Option<String>,
    pub cards: u32,
    pub nodes_shown: usize,
    pub healthy: u32,
}

#[derive(Default, Serialize, Deserialize)]
struct Record {
    incident: Option<Incident>,
    declared: BTreeMap<String, Declared>,
    /// Measured again for a minute after every start, so never saved.
    #[serde(skip)]
    mac: MacState,
    #[serde(default)]
    sampled_at: Millis,
    #[serde(skip)]
    node_cost: crate::node_cost::NodeCost,
    #[serde(skip)]
    nodes_failed: bool,
    /// What a node of each project takes on the disk at its peak.
    #[serde(default)]
    peaks: crate::disk_peaks::Estimates,
    /// The nodes at work on the Mac's own disk and its latest reading, so a restart goes on from
    /// what each one took.
    #[serde(default)]
    stretches: crate::disk_peaks::Ledger,
    #[serde(skip)]
    disk_working: BTreeMap<crate::admission::Key, Option<String>>,
    #[serde(skip)]
    disk_grants: BTreeMap<crate::admission::Key, u64>,
    /// The disk was short of room for the next build when last asked.
    #[serde(skip)]
    short: bool,
    /// A sandbox's fake nodes build nothing: they take nothing from the disk and reserve nothing.
    #[serde(skip)]
    peaks_ignored: bool,
    /// The latest valid reading of the Mac's own disk.
    #[serde(skip)]
    disk: Option<alondra_engine_ports::disk::Space>,
    #[serde(skip)]
    swap: crate::swap_disk::SwapDisk,
    #[serde(skip)]
    swap_short: bool,
}

impl Record {
    /// What the `working` tasks, the work seen since the last reading and the admissions granted
    /// since then may still take from the disk, leaving out the gate's `head`.
    fn reserved(&mut self, working: &BTreeMap<TaskId, Option<String>>, head: Option<&crate::admission::Key>) -> u64 {
        if self.peaks_ignored {
            return 0;
        }
        for (task, project) in working {
            self.disk_working.entry(crate::admission::Key::Task(task.clone())).or_insert_with(|| project.clone());
        }
        let granted = self.disk_grants.values().copied().fold(0u64, u64::saturating_add);
        self.disk_working.iter().filter(|(key, _)| Some(*key) != head && !self.disk_grants.contains_key(*key)).map(|(key, project)| {
            let estimate = self.peaks.estimate(project.as_deref());
            match key {
                crate::admission::Key::Task(task) => self.stretches.reserved(task, estimate),
                _ => estimate,
            }
        }).fold(granted, u64::saturating_add)
    }
}

/// A node stopped for the machine, as the card names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiter {
    pub session: String,
    pub name: String,
    pub reason: String,
    pub resource: Option<MachineResource>,
    pub at: Millis,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Log { id: String, card: MachineCard },
    Withdraw(String),
    /// The incident is over: its declared waits are left to the admission gate.
    Release(Vec<String>),
}

/// What the gauges say is wrong. Once the disk was the cause it must climb past
/// `alondra_engine_ports::disk::recovered_lines()` to count as fine, so a disk hovering at the line does not flap. Memory
/// and the CPU load open none: they only pace new starts at the gate.
pub(crate) fn failing(machine: &Machine, previous: Option<MachineCause>) -> Option<MachineCause> {
    (valid_disk(disk_space(machine)) && disk_space(machine).low(previous == Some(MachineCause::Disk))).then_some(MachineCause::Disk)
}

fn disk_space(machine: &Machine) -> alondra_engine_ports::disk::Space {
    alondra_engine_ports::disk::Space { free: machine.disk_available, purgeable: machine.disk_purgeable, total: machine.disk_total }
}

fn valid_disk(space: alondra_engine_ports::disk::Space) -> bool {
    space.total > 0 && space.free.checked_add(space.purgeable).is_some_and(|usable| usable <= space.total)
}

fn card_for(cause: MachineCause, machine: &Machine, waiting: &[Waiter]) -> MachineCard {
    let space = disk_space(machine);
    let short_now = cause == MachineCause::Disk && space.short_now();
    MachineCard {
        cause,
        disk_free: space.shown_free(),
        purgeable: if short_now { space.purgeable } else { 0 },
        nodes: waiting.iter().map(|waiter| waiter.name.clone()).collect(),
        reason: (cause == MachineCause::Declared).then(|| waiting.first().map(|waiter| waiter.reason.clone())).flatten(),
        resource: (cause == MachineCause::Declared).then(|| waiting.first().and_then(|waiter| waiter.resource)).flatten(),
        minutes: None,
        still: false,
    }
}

/// One sample of the incident: the gauges, the nodes waiting, and whether the user just said they
/// fixed it. Pure, so every turn of it is tested without a clock.
pub(crate) fn step(previous: Option<Incident>, machine: &Machine, waiting: &[Waiter], fixed: bool, now: Millis) -> (Option<Incident>, Vec<Action>) {
    step_with_disk_measurement(previous, machine, waiting, fixed, now, valid_disk(disk_space(machine)), true)
}

/// A look again at a sample already taken (`fresh` false) moves no recovery: only a new
/// measurement counts toward the healthy samples that end an incident.
fn step_with_disk_measurement(previous: Option<Incident>, machine: &Machine, waiting: &[Waiter], fixed: bool, now: Millis, measured_disk: bool, fresh: bool) -> (Option<Incident>, Vec<Action>) {
    let mut previous = previous;
    if let Some(incident) = previous.as_mut().filter(|incident| incident.cause == MachineCause::Disk && !measured_disk) {
        incident.healthy = 0;
        return (previous, vec![]);
    }
    let measured = failing(machine, previous.as_ref().map(|incident| incident.cause));
    let Some(mut incident) = previous else {
        let Some(cause) = measured else { return (None, vec![]) };
        return open(cause, machine, waiting, now);
    };
    let fine = measured.is_none();
    if fresh {
        incident.healthy = if fine { incident.healthy + 1 } else { 0 };
    }
    let declared_only = incident.cause == MachineCause::Declared;
    if declared_only || (fine && (fixed || incident.healthy >= HEALTHY_SAMPLES)) {
        let mut actions: Vec<Action> = incident.card.into_iter().map(Action::Withdraw).collect();
        if !waiting.is_empty() {
            actions.push(Action::Release(waiting.iter().map(|waiter| waiter.session.clone()).collect()));
        }
        return (None, actions);
    }
    let cause = measured.unwrap_or(incident.cause);
    let first_stop = incident.nodes_shown == 0 && !waiting.is_empty();
    let mut actions = vec![];
    if first_stop || cause != incident.cause || fixed {
        let card = MachineCard { still: fixed, ..card_for(cause, machine, waiting) };
        actions.extend(incident.card.take().map(Action::Withdraw));
        incident.cards += 1;
        let id = format!("{MACHINE_PREFIX}{}-{}", incident.since, incident.cards);
        incident.card = Some(id.clone());
        incident.cause = cause;
        incident.nodes_shown = waiting.len();
        actions.push(Action::Log { id, card });
    }
    (Some(incident), actions)
}

/// A problem the Mac's gauges measure opens an incident with its card. A wait a node declared
/// with no measured cause, such as a build queue, opens none: it is the node's state.
fn open(cause: MachineCause, machine: &Machine, waiting: &[Waiter], now: Millis) -> (Option<Incident>, Vec<Action>) {
    let card = card_for(cause, machine, waiting);
    let id = format!("{MACHINE_PREFIX}{now}");
    let incident = Incident { since: now, cause, card: Some(id.clone()), cards: 1, nodes_shown: waiting.len(), healthy: 0 };
    (Some(incident), vec![Action::Log { id, card }])
}

pub struct MachineWatch {
    /// The space a test gives a launch's folder, in place of the free space of the Mac running it.
    launch_space: Mutex<BTreeMap<PathBuf, alondra_engine_ports::disk::Space>>,
    /// A launch sees the boot disk the watch measured: a test's world never reads the real Mac's.
    launches_on_measured_disk: std::sync::atomic::AtomicBool,
    /// Told after every sample, for a test that waits for one.
    pub sampled: tokio::sync::Notify,
    /// The threads that took each sample, kept only once a test asks for them.
    sample_threads: Mutex<Option<Vec<std::thread::ThreadId>>>,
    /// A holder that panics leaves it whole, so it is recovered: see `MachineWatch::record`.
    record: Mutex<Record>,
    file: Option<PathBuf>,
    save_failures: crate::save::Failures,
    changed: tokio::sync::Notify,
    disk: Arc<dyn Disk>,
}

/// A watch saved nowhere, over the doubled disks of a test.
#[cfg(test)]
impl Default for MachineWatch {
    fn default() -> Self {
        Self::reading(Arc::new(alondra_engine_doubles::disk::Disks::default()))
    }
}

impl MachineWatch {
    fn reading(disk: Arc<dyn Disk>) -> Self {
        Self { launch_space: Mutex::default(), launches_on_measured_disk: Default::default(), record: Mutex::default(), file: None,
            save_failures: crate::save::Failures::default(), changed: tokio::sync::Notify::new(), sampled: tokio::sync::Notify::new(),
            sample_threads: Mutex::new(None), disk,
        }
    }

    /// A memory incident an earlier daemon left open is dropped: `watch` then withdraws its card
    /// as a stray, and its declared waits stay with the admission gate.
    pub(crate) fn persisted_in(home: &Path, disk: Arc<dyn Disk>) -> Self {
        let file = home.join(FILE);
        let mut record: Record = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        if record.incident.take_if(|incident| incident.cause == MachineCause::Memory).is_some() {
            log!("dropping the memory incident an earlier daemon left open: memory no longer asks the user anything");
        }
        Self { record: Mutex::new(record), file: Some(file), ..Self::reading(disk) }
    }

    /// A holder that panicked leaves a valid record: every step replaces one whole field or one
    /// whole entry, and the next sample rewrites what it reads.
    fn record(&self) -> std::sync::MutexGuard<'_, Record> {
        self.record.lock_or_recover("the machine watch")
    }

    pub fn poison(&self) {
        crate::locks::poison(&self.record);
    }

    /// From now on keeps the thread that takes each sample.
    pub fn record_sample_threads(&self) {
        self.sample_threads.lock_or_recover("the sampling threads").get_or_insert_default();
    }

    pub fn sample_threads(&self) -> Vec<std::thread::ThreadId> {
        self.sample_threads.lock_or_recover("the sampling threads").clone().unwrap_or_default()
    }

    /// From now on a launch sees the boot disk this watch measured.
    pub fn launch_on_measured_disk(&self) {
        self.launches_on_measured_disk.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn save(&self, record: &Record) {
        let Some(file) = &self.file else { return };
        let written = serde_json::to_vec(record).map_err(std::io::Error::from)
            .and_then(|bytes| crate::save::atomic(file, &bytes));
        self.save_failures.record(file, written);
    }

    /// A node said it waits for the machine; the watcher looks at once.
    pub fn declare(&self, session: &str, resource: Option<MachineResource>, reason: &str, turn: u64, now: Millis) {
        let mut record = self.record();
        record.declared.insert(session.to_owned(), Declared { reason: reason.to_owned(), resource, at: now, turn });
        self.save(&record);
        drop(record);
        self.changed.notify_one();
    }

    pub(crate) fn incident(&self) -> Option<Incident> {
        self.record().incident.clone()
    }

    /// How the Mac is doing, as the gate reads it.
    pub(crate) fn mac(&self) -> MacState {
        self.record().mac.clone()
    }

    /// Returns the budget before this sample, or `None` when it was already taken.
    fn sample(&self, machine: &Machine, now: Millis) -> Option<Result<(), NoRoom>> {
        let mut record = self.record();
        if machine.at != 0 && machine.at == record.sampled_at {
            return None;
        }
        record.sampled_at = machine.at;
        record.swap.sample(machine.compressor, now);
        let disk = disk_space(machine);
        if valid_disk(disk) { record.disk = Some(disk); }
        let before = record.mac.room(0, now);
        if record.mac.step(machine, now) {
            record.mac.observed_nodes = record.mac.nodes.clone();
            record.mac.observed_nodes_at = record.mac.nodes_at.filter(|at| *at <= now);
        }
        Some(before)
    }

    /// Keeps what each node measured at `now`; the gate reserves the new estimate from then on.
    pub(crate) fn measure_node_memory(&self, now: Millis, footprints: BTreeMap<alondra_domain_rules::AgentId, u64>) {
        let mut record = self.record();
        if record.node_cost.due(now) {
            record.node_cost.record(now, footprints.values().copied());
        }
        record.mac.node_cost = Some(record.node_cost.estimate());
        record.mac.nodes = footprints;
        record.mac.nodes_at = Some(now);
        if std::mem::take(&mut record.nodes_failed) {
            log!("node memory measurements recovered");
        }
    }

    pub fn measure_nodes(&self, now: Millis, footprints: Vec<u64>) {
        let mut record = self.record();
        record.node_cost.record(now, footprints);
        record.mac.node_cost = Some(record.node_cost.estimate());
    }

    fn nodes_unavailable(&self, error: &std::io::Error) {
        let mut record = self.record();
        record.mac.nodes_at = None;
        record.mac.observed_nodes_at = None;
        if !std::mem::replace(&mut record.nodes_failed, true) {
            log!("node memory measurements unavailable; keeping admission reserves: {error}");
        }
    }

    fn home(&self) -> Option<&Path> {
        self.file.as_deref().and_then(Path::parent)
    }

    pub fn set_disk_low(&self, low: bool) {
        let disk = Machine { disk_total: 500_000_000_000, disk_available: if low { 1_000_000_000 } else { 400_000_000_000 }, ..Machine::default() };
        self.measure_disk(disk_space(&disk));
        self.set_incident(step(None, &disk, &[], false, 0).0);
    }

    pub(crate) fn disk_low(&self) -> bool {
        self.incident().is_some_and(|incident| incident.cause == MachineCause::Disk)
    }

    /// Takes the Mac's own disk's space.
    pub fn measure_disk(&self, space: alondra_engine_ports::disk::Space) {
        if valid_disk(space) {
            let mut record = self.record();
            record.disk = Some(space);
            record.disk_working.clear();
            record.disk_grants.clear();
        }
    }

    pub fn ignore_peaks(&self) {
        self.record().peaks_ignored = true;
    }

    /// Takes a sample of the usable space of the Mac's own disk with the tasks at work, by
    /// project; returns whether one stopped working or one whose start went unseen began, which
    /// frees what they reserved.
    pub fn measure_peaks(&self, usable: u64, working: &BTreeMap<TaskId, Option<String>>, now: Millis) -> bool {
        let mut record = self.record();
        if record.peaks_ignored {
            return false;
        }
        record.disk_working.clear();
        record.disk_grants.clear();
        let Record { stretches, peaks, swap, .. } = &mut *record;
        let freed = stretches.sample_excluding(peaks, usable, working, now, swap.allocated_change);
        if freed || !stretches.is_empty() {
            self.save(&record);
        }
        freed
    }

    pub fn seed_peaks(&self, project: &str, peaks: &[u64]) {
        self.record().peaks.seed(project, peaks);
    }

    /// What `task`'s stretch at work has taken from the disk so far.
    pub(crate) fn disk_used(&self, task: &TaskId) -> u64 {
        self.record().stretches.used(task)
    }

    /// Writes not covered by a newer byte budget stay reserved even after their tasks end.
    pub(crate) fn reserve_admission(&self, key: crate::admission::Key, project: Option<String>) {
        let mut record = self.record();
        if record.peaks_ignored { return; }
        let cost = record.peaks.estimate(project.as_deref());
        let grant = record.disk_grants.entry(key.clone()).or_default();
        *grant = grant.saturating_add(cost);
        record.disk_working.entry(key).or_insert(project);
    }

    pub fn disk_reserved(&self, working: &BTreeMap<TaskId, Option<String>>) -> u64 {
        self.record().reserved(working, None)
    }

    /// The Mac's disk short of room for the next node, of `project`, on top of what the `working`
    /// tasks may still take. Also while the disk's incident is open, when no node starts. Once
    /// short, the disk needs margin to have room again.
    pub fn disk_room(&self, working: &BTreeMap<TaskId, Option<String>>, project: Option<&str>, now: Millis) -> Option<alondra_engine_ports::disk::Room> {
        self.disk_room_for(working, project, None, now)
    }

    /// The gate's `head` needs only the disk's line when nothing else reserves the disk, no other
    /// node at work nor an admission since its last reading, so one node can always work over the
    /// line. Read and decided under one lock, so a sample cannot clear the work it counts midway.
    pub(crate) fn disk_room_for(&self, working: &BTreeMap<TaskId, Option<String>>, project: Option<&str>, head: Option<&crate::admission::Key>, now: Millis) -> Option<alondra_engine_ports::disk::Room> {
        let mut record = self.record();
        let reserved = record.reserved(working, head);
        let alone = head.is_some() && record.disk_grants.is_empty() && record.disk_working.keys().all(|key| Some(key) == head);
        let estimate = if record.peaks_ignored || alone { 0 } else { record.peaks.estimate(project) };
        let needed = reserved.saturating_add(estimate).saturating_add(alondra_engine_ports::disk::low_lines().usable);
        let Some(disk) = record.disk else {
            return Some(alondra_engine_ports::disk::Room { free: 0, needed });
        };
        let short = disk.room_for(needed, record.short).err();
        record.short = short.is_some();
        let swap_short = if record.peaks_ignored { None } else {
            record.swap.reserve(now).and_then(|swap_reserve| {
                // Swap needs immediately writable space, even when macOS reports purgeable room.
                let needed = needed.saturating_add(swap_reserve);
                let margin = if record.swap_short { alondra_engine_ports::disk::recovered_lines().usable - alondra_engine_ports::disk::low_lines().usable } else { 0 };
                (disk.free < needed.saturating_add(margin)).then_some(alondra_engine_ports::disk::Room { free: disk.free, needed })
            })
        };
        record.swap_short = swap_short.is_some();
        let carded = record.incident.as_ref().is_some_and(|incident| incident.cause == MachineCause::Disk);
        swap_short.or(short).or_else(|| carded.then(|| alondra_engine_ports::disk::Room { free: disk.shown_free(), needed }))
    }

    /// Swap and disk budget beside the queue hold, for the throttled log.
    pub(crate) fn disk_detail(&self, working: &BTreeMap<TaskId, Option<String>>, project: Option<&str>, now: Millis) -> Option<String> {
        self.disk_detail_for(working, project, None, now)
    }

    pub(crate) fn disk_detail_for(&self, working: &BTreeMap<TaskId, Option<String>>, project: Option<&str>, head: Option<&crate::admission::Key>, now: Millis) -> Option<String> {
        let mut record = self.record();
        let reserved = record.reserved(working, head);
        let space = record.disk?;
        let swap = record.swap.line(now).unwrap_or_else(|| "swap unavailable".into());
        let builds = reserved.saturating_add(if record.peaks_ignored { 0 } else { record.peaks.estimate(project) });
        let remaining = i128::from(space.free) - i128::from(builds);
        Some(format!("{swap}; container free {:.2} GB, builds reserved {:.2} GB, free less builds {:.2} GB",
            space.free as f64 / 1_000_000_000.0, builds as f64 / 1_000_000_000.0, remaining as f64 / 1_000_000_000.0))
    }

    /// Whether the disk fell under its floor.
    pub fn under_floor(&self) -> bool {
        self.record().disk.is_some_and(|space| space.under_floor())
    }

    fn short(&self) -> bool {
        let record = self.record();
        record.short || record.swap_short
    }

    /// Why no node may start in `cwd` now: the Mac's disk has its incident open, or the disk the
    /// task's folder is on is low right now. That folder's disk is read once, at the launch; only
    /// the Mac's own disk is watched.
    pub(crate) fn refuse_launch(&self, cwd: &Path) -> Option<String> {
        let space = self.space_where_it_works(cwd);
        let low_here = space.is_some_and(|space| space.low(false));
        if !self.disk_low() && !low_here {
            return None;
        }
        if low_here {
            self.changed.notify_one();
        }
        let free = space.filter(|_| low_here).map(|space| format!(" ({} free where it works)", alondra_core::machine_incident::bytes_text(if space.short_now() { space.free } else { space.usable() }, Lang::En))).unwrap_or_default();
        Some(format!("The disk is nearly full{free}: Alondra starts no node until there is space again, and {} has a card about it", alondra_core::config::user_name()))
    }

    pub fn give_launch_space(&self, path: PathBuf, space: alondra_engine_ports::disk::Space) {
        self.launch_space.lock_or_recover("the launch volume measurements").insert(path, space);
    }

    fn space_where_it_works(&self, cwd: &Path) -> Option<alondra_engine_ports::disk::Space> {
        if let Some(space) = self.launch_space.lock_or_recover("the launch volume measurements").get(cwd).copied() {
            return Some(space);
        }
        if self.launches_on_measured_disk.load(std::sync::atomic::Ordering::Relaxed)
            && let Some(measured) = self.record().disk {
            return Some(measured);
        }
        self.disk.space(cwd)
    }

    pub fn declared(&self) -> BTreeMap<String, Declared> {
        self.record().declared.clone()
    }

    fn forget(&self, sessions: &[String]) {
        let mut record = self.record();
        if sessions.iter().fold(false, |any, session| record.declared.remove(session).is_some() || any) {
            self.save(&record);
        }
    }

    fn set_incident(&self, incident: Option<Incident>) {
        let mut record = self.record();
        if record.incident != incident {
            record.incident = incident;
            self.save(&record);
        }
    }
}

/// The nodes still waiting: a node that took another turn since it declared, or that the user
/// closed, went on and is forgotten.
fn waiters(domain: &dyn Runtime) -> Vec<Waiter> {
    let watch = &domain.machine_watch();
    let state = Some(domain.state());
    let mut gone = vec![];
    let mut waiting = vec![];
    for (session, declared) in watch.declared() {
        let moved_on = domain.engine().turn(&session).is_some_and(|turn| turn > declared.turn);
        if moved_on || domain.engine().is_closed(&session) {
            gone.push(session);
            continue;
        }
        let name = state.as_ref().and_then(|state| {
            let agent = alondra_domain_rules::AgentId(session.clone());
            state.tasks.values().find(|task| task.agent_id.as_ref() == Some(&agent) && !task.archived).map(|task| task.title.clone())
                .or_else(|| state.agents.get(&agent).map(|agent| agent.name.clone()))
        }).unwrap_or_else(|| session.chars().take(8).collect());
        waiting.push(Waiter { session, name, reason: declared.reason, resource: declared.resource, at: declared.at });
    }
    watch.forget(&gone);
    waiting
}

/// Machine cards still open that are not the open incident's card, such as the ones an earlier
/// daemon raised for a wait no gauge measured: none of them asks the user anything real.
fn withdraw_strays(domain: &dyn Runtime) {
    let state = domain.state();
    let current = domain.machine_watch().incident().filter(|incident| incident.cause != MachineCause::Declared).and_then(|incident| incident.card);
    for (id, card) in &state.interactions {
        let open = !card.withdrawn && matches!(card.payload, alondra_domain_rules::InteractionPayload::Question { answer: None, .. });
        if open && card.author == alondra_domain_rules::Author::System && card.system_card == Some(SystemCard::Machine) && current.as_deref() != Some(id.0.as_str()) {
            log!("withdrawing the stray machine card {}", id.0);
            let _ = domain.withdraw_problem(id);
        }
    }
}

/// The answer "I've fixed it" on the open card, taken once.
fn fixed(domain: &dyn Runtime, incident: Option<&Incident>) -> bool {
    use alondra_domain_rules::{InteractionId, InteractionPayload};
    let Some(card) = incident.and_then(|incident| incident.card.as_ref()) else { return false };
    let state = domain.state();
    matches!(state.interactions.get(&InteractionId(card.clone())).map(|card| &card.payload),
        Some(InteractionPayload::Question { answer: Some(answer), .. }) if answer.option_id == MACHINE_FIXED_OPTION)
}

/// Carries out one step's actions.
async fn act(domain: &std::sync::Arc<dyn Runtime>, _alerts: &impl Alerts, actions: Vec<Action>) {
    for action in actions {
        match action {
            Action::Log { id, card } => {
                let (need, why) = card.text(Lang::En);
                log!("machine incident {id}: {need}. {why}; the admission gate waits for recovery");
            }
            Action::Withdraw(id) => {
                let withdrawing = domain.clone();
                let _ = tokio::task::spawn_blocking(move || withdrawing.withdraw_problem(&alondra_domain_rules::InteractionId(id))).await;
            }
            Action::Release(sessions) => {
                log!("the machine recovered: {} node(s) that waited come back through the admission gate", sessions.len());
                let releasing = domain.clone();
                tokio::task::spawn_blocking(move || releasing.machine_watch().forget(&sessions)).await.expect("machine release worker failed");
            }
        }
    }
}

/// A new sample of the Mac: kept in the samples file, and when memory gains or loses room for one
/// more node, one line in the log and a look of the gate. Returns whether the sample was new.
pub fn pace(domain: &dyn Runtime, machine: &Machine) -> bool {
    if let Some(threads) = domain.machine_watch().sample_threads.lock_or_recover("the sampling threads").as_mut() {
        threads.push(std::thread::current().id());
    }
    let watch = &domain.machine_watch();
    let now = if machine.at == 0 { now_millis() } else { machine.at };
    let Some(before) = watch.sample(machine, now) else { return false };
    match domain.hosts().node_footprints() {
        Ok(footprints) => watch.measure_node_memory(now_millis(), footprints),
        Err(error) => watch.nodes_unavailable(&error),
    }
    let mac = watch.mac();
    let working = crate::admission::working_projects(domain, &domain.state());
    let usable = disk_space(machine).usable();
    let measured_disk = valid_disk(disk_space(machine));
    if measured_disk && watch.measure_peaks(usable, &working, now) {
        domain.admission().poke();
    }
    if let Some(home) = watch.home() {
        let reserving = crate::admission::reserving_tasks(&domain.state(), working.keys());
        let disk = measured_disk.then(|| (usable, watch.disk_reserved(&reserving)));
        crate::machine_state::record(&home.join(crate::machine_state::SAMPLES_DIR), now, &mac, working.len(), disk);
    }
    let working = working.len();
    let room = mac.room(0, now);
    if before.is_ok() == room.is_ok() { return true; }
    let free = mac.free_percent.map_or_else(|| "unknown".into(), |free| format!("{free}%"));
    match room {
        Ok(()) => log!("memory has room for one more node again ({free} free, {working} node(s) working); {}; disk free {} bytes, purgeable {} bytes", mac.memory_detail(0), machine.disk_available, machine.disk_purgeable),
        Err(why) => log!("memory has no room for one more node: {why} ({free} free, {working} node(s) working); running nodes go on; {}; disk free {} bytes, purgeable {} bytes", mac.memory_detail(0), machine.disk_available, machine.disk_purgeable),
    }
    domain.admission().poke();
    true
}

/// One look at a sample of the machine: the incident it opens, keeps or ends, and the actions that
/// follow. A sample that measures space on the disk takes over the executions that found it full.
pub fn look(domain: &dyn Runtime, machine: Machine) -> Vec<Action> {
    let watch = &domain.machine_watch();
    let fresh = pace(domain, &machine);
    let measured_disk = valid_disk(disk_space(&machine));
    if measured_disk {
        if watch.short() {
            domain.admission().poke();
        }
        if watch.under_floor() {
            crate::admission::hold_for_disk(domain, now_millis());
        }
    }
    let previous = watch.incident();
    let fixed = fixed(domain, previous.as_ref());
    let waiting = waiters(domain);
    let (next, actions) = step_with_disk_measurement(previous, &machine, &waiting, fixed, now_millis(), measured_disk, fresh);
    let space = measured_disk && has_space(&machine, next.as_ref());
    watch.set_incident(next);
    if fixed {
        domain.admission().release_declared(domain);
    }
    if space {
        domain.hosts().retake_waiting_for_space();
    }
    domain.admission().poke();
    domain.machine_watch().sampled.notify_one();
    actions
}

/// Whether the sample measured the disk with room, past the line it is low at, and no disk
/// incident stays open.
fn has_space(machine: &Machine, incident: Option<&Incident>) -> bool {
    valid_disk(disk_space(machine)) && failing(machine, None).is_none() && incident.is_none_or(|incident| incident.cause != MachineCause::Disk)
}

/// Looks at the machine on every sample and whenever a node declares a wait or the user answers.
pub async fn run(domain: std::sync::Arc<dyn Runtime>, gauges: alondra_daemon_engine_machine::machine::Gauges, alerts: impl Alerts) {
    watch(domain, move || gauges.lock_or_recover("the machine gauges").as_ref().map(|(machine, _, _)| machine.clone()), alerts, TICK).await
}

async fn watch(domain: std::sync::Arc<dyn Runtime>, sample: impl Fn() -> Option<Machine>, alerts: impl Alerts, tick: Duration) {
    let starting = domain.clone();
    tokio::task::spawn_blocking(move || withdraw_strays(&*starting)).await.expect("machine startup worker failed");
    let mut cards = domain.subscribe();
    loop {
        if let Some(machine) = sample() {
            let measuring = domain.clone();
            let actions = tokio::task::spawn_blocking(move || look(&*measuring, machine)).await.expect("machine watch worker failed");
            act(&domain, &alerts, actions).await;
        }
        tokio::select! {
            _ = alondra_core::clock::sleep(tick) => {}
            _ = domain.machine_watch().changed.notified() => {}
            changed = cards.changed() => if changed.is_err() { return },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_domain_rules::InteractionPayload;
    use std::sync::Arc;

    const GB: u64 = 1_000_000_000;
    const MIN: Millis = 60_000;
    const HOUR: Millis = 60 * MIN;
    const SAMPLE: Millis = 10_000;

    fn machine(disk_free: u64, memory_free: u32) -> Machine {
        Machine { at: 0, memory_total: 16 * GB, memory_available: 8 * GB, memory_free_percent: Some(memory_free), memory_pressure: None, compressor: None, load: [1.0; 3], cpus: 8,
            cpu_usage: 10.0, disk_total: 500 * GB, disk_available: disk_free, disk_purgeable: 0, unreliable: false }
    }

    fn purging(free: u64, purgeable: u64) -> Machine {
        Machine { disk_purgeable: purgeable, ..machine(free, 50) }
    }

    fn waiter(session: &str, name: &str, at: Millis) -> Waiter {
        Waiter { session: session.into(), name: name.into(), reason: "disk full".into(), resource: None, at }
    }

    fn raised(actions: &[Action]) -> Vec<&MachineCard> {
        actions.iter().filter_map(|action| match action { Action::Log { card, .. } => Some(card), _ => None }).collect()
    }

    fn raised_ids(actions: &[Action]) -> Vec<String> {
        actions.iter().filter_map(|action| match action { Action::Log { id, .. } => Some(id.clone()), _ => None }).collect()
    }

    fn working(tasks: &[(&str, &str)]) -> BTreeMap<TaskId, Option<String>> {
        tasks.iter().map(|(task, project)| (TaskId((*task).into()), Some((*project).into()))).collect()
    }

    #[test]
    fn swap_history_is_not_restored_but_build_estimates_are() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let watch = MachineWatch::persisted_in(&home, Arc::new(alondra_engine_doubles::disk::Disks::default()));
        watch.seed_peaks("app", &[GB; 3]);
        for step in 0..=30 {
            let at = 10_000 + step * SAMPLE;
            let compressor = alondra_core::MemoryCompressor { swap_used: GB + step as u64 * 10_000_000,
                swap_total: Some(2 * GB), ..alondra_core::MemoryCompressor::default() };
            watch.sample(&Machine { at, compressor: Some(compressor), ..machine(7 * GB, 29) }, at);
        }
        assert!(watch.record().swap.reserve(10_000 + 5 * MIN).is_some());
        watch.save(&watch.record());
        let reopened = MachineWatch::persisted_in(&home, Arc::new(alondra_engine_doubles::disk::Disks::default()));
        assert_eq!(reopened.record().swap.reserve(10_000 + 5 * MIN), None);
        assert_eq!(reopened.record().peaks.estimate(Some("app")), GB);
    }

    #[test]
    fn startup_admission_disk_budget_requires_valid_measurements() {
        let watch = MachineWatch::default();
        let room = || watch.disk_room(&BTreeMap::new(), None, 0);
        assert!(room().is_some(), "no disk measurement yet");
        for space in [
            alondra_engine_ports::disk::Space { total: 0, free: 0, purgeable: 0 },
            alondra_engine_ports::disk::Space { total: 0, free: 100 * GB, purgeable: 0 },
            alondra_engine_ports::disk::Space { total: 50 * GB, free: 100 * GB, purgeable: 0 },
            alondra_engine_ports::disk::Space { total: 50 * GB, free: GB, purgeable: u64::MAX },
        ] {
            watch.measure_disk(space);
            assert!(room().is_some(), "invalid disk reading: {space:?}");
        }
        watch.measure_disk(alondra_engine_ports::disk::Space { total: 500 * GB, free: 100 * GB, purgeable: 0 });
        assert!(room().is_none(), "the first valid measurement opens the disk gate");
        watch.measure_disk(alondra_engine_ports::disk::Space { total: 500 * GB, free: 0, purgeable: 0 });
        assert!(room().is_some(), "zero available is a measured shortage");
        watch.measure_disk(alondra_engine_ports::disk::Space { total: 0, free: 100 * GB, purgeable: 0 });
        assert!(room().is_some(), "an invalid reading does not replace the shortage");
    }

    #[test]
    fn startup_admission_invalid_disk_samples_cannot_release_an_incident() {
        let waiting = [waiter("s1", "Build", 0)];
        let (mut incident, _) = step(None, &machine(0, 50), &waiting, false, 0);
        for invalid in [Machine::default(), Machine { disk_total: GB, disk_available: 2 * GB, ..Machine::default() }] {
            let (next, _) = step(incident, &machine(100 * GB, 50), &waiting, false, SAMPLE);
            let (next, actions) = step(next, &invalid, &waiting, true, 2 * SAMPLE);
            assert!(actions.is_empty(), "an invalid probe cannot release waiting nodes");
            assert_eq!(next.as_ref().unwrap().healthy, 0);
            incident = next;
        }
        let (next, _) = step(incident, &machine(100 * GB, 50), &waiting, false, 3 * SAMPLE);
        assert!(next.is_some());
        let (next, actions) = step(next, &machine(100 * GB, 50), &waiting, false, 4 * SAMPLE);
        assert!(next.is_none());
        assert!(actions.iter().any(|action| matches!(action, Action::Release(_))));
    }

    #[test]
    fn startup_admission_missing_boot_disk_readings_keep_the_last_swap_budget() {
        let watch = MachineWatch::default();
        let measured = alondra_engine_ports::disk::Space { total: 500 * GB, free: 7 * GB, purgeable: 40 * GB };
        watch.sample(&Machine { at: SAMPLE, disk_total: measured.total, disk_available: measured.free,
            disk_purgeable: measured.purgeable, ..Machine::default() }, SAMPLE);
        for at in [2 * SAMPLE, 3 * SAMPLE] {
            watch.sample(&Machine { at, ..Machine::default() }, at);
            assert_eq!(watch.record().disk, Some(measured));
        }
        watch.sample(&Machine { at: 4 * SAMPLE, disk_total: GB, disk_available: 2 * GB, ..Machine::default() }, 4 * SAMPLE);
        assert_eq!(watch.record().disk, Some(measured), "invalid readings cannot replace the boot budget");
    }

    /// The daemon restarts while a node builds: what each project took and what the node at work
    /// took so far are kept, so it reserves only what it may still take.
    #[test]
    fn what_each_project_takes_and_what_a_node_at_work_took_are_kept_across_a_restart() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let watch = MachineWatch::persisted_in(&home, Arc::new(alondra_engine_doubles::disk::Disks::default()));
        watch.measure_peaks(100 * GB, &BTreeMap::new(), 0);
        for (run, task) in ["t1", "t2", "t3"].iter().enumerate() {
            let at = run as Millis * 10;
            watch.measure_peaks(100 * GB, &working(&[(task, "app")]), at + 1);
            watch.measure_peaks(88 * GB, &working(&[(task, "app")]), at + 2);
            assert!(watch.measure_peaks(100 * GB, &BTreeMap::new(), at + 3), "its stretch ended");
        }
        watch.measure_peaks(100 * GB, &working(&[("t4", "app")]), 50);
        watch.measure_peaks(91 * GB, &working(&[("t4", "app")]), 51);
        let reopened = MachineWatch::persisted_in(&home, Arc::new(alondra_engine_doubles::disk::Disks::default()));
        reopened.measure_disk(alondra_engine_ports::disk::Space { free: 15 * GB, purgeable: 0, total: 500 * GB });
        assert_eq!(reopened.disk_reserved(&working(&[("t4", "app")])), 3 * GB, "12 GB less the 9 it took before the restart");
        reopened.measure_peaks(89 * GB, &working(&[("t4", "app")]), 100);
        assert_eq!(reopened.disk_reserved(&working(&[("t4", "app")])), GB, "and the 2 it took since");
        assert_eq!(reopened.disk_room(&working(&[("t4", "app")]), Some("app"), 100),
            Some(alondra_engine_ports::disk::Room { free: 15 * GB, needed: GB + 12 * GB + 5 * GB }), "t4's last GB and the next one's 12 on top of the 5 GB line");
        reopened.measure_peaks(89 * GB, &BTreeMap::new(), 102);
        assert_eq!(reopened.record().peaks.estimate(Some("app")), 12 * GB);
        assert_eq!(reopened.record().peaks.most(), 12 * GB, "t4's 11 GB is kept beside the three 12 GB");
        let earlier: Record = serde_json::from_str(r#"{"incident": null, "declared": {}, "builds": [5000000000]}"#).unwrap();
        assert_eq!(earlier.peaks, crate::disk_peaks::Estimates::default(), "a file of an earlier daemon still reads");
        assert!(earlier.stretches.is_empty());
    }

    /// 2 November: the first daemon to reserve peaks started on a HOME that had measured none, with 7
    /// nodes at work and 26.8 GB free. Each reserved the 8 GB default, so the next start needed
    /// 69 GB. Their builds had already taken their space, which the free space shows.
    #[test]
    fn nodes_at_work_when_peaks_were_first_measured_leave_room_for_the_next_start() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(FILE), r#"{"incident": null, "declared": {}}"#).unwrap();
        let watch = MachineWatch::persisted_in(&home, Arc::new(alondra_engine_doubles::disk::Disks::default()));
        let free = 26_800_000_000;
        watch.measure_disk(alondra_engine_ports::disk::Space { free, purgeable: 0, total: 494 * GB });
        let at_work: Vec<(String, &str)> = (1..=7).map(|index| (format!("t{index}"), "alondra")).collect();
        let at_work: Vec<(&str, &str)> = at_work.iter().map(|(task, project)| (task.as_str(), *project)).collect();
        let at_work = working(&at_work);
        assert!(watch.measure_peaks(free, &at_work, 0), "the gate looks again once their stretches began");
        assert_eq!(watch.disk_reserved(&at_work), 7 * GB);
        assert_eq!(watch.disk_room(&at_work, Some("alondra"), 0), None, "7 GB for the nodes at work and 8 GB for the next start fit over the 5 GB line");
    }

    fn space(free: u64) -> alondra_engine_ports::disk::Space {
        alondra_engine_ports::disk::Space { free, purgeable: 0, total: 37 * GB }
    }

    #[test]
    fn startup_admission_missing_disk_cannot_cancel_unobserved_writes() {
        let watch = MachineWatch::default();
        let disk = |free| alondra_engine_ports::disk::Space { free, purgeable: 0, total: 500 * GB };
        watch.measure_disk(disk(36 * GB));
        watch.measure_peaks(36 * GB, &BTreeMap::new(), 0);
        let running = working(&[("t1", "app"), ("t2", "app"), ("t3", "app")]);
        let before = watch.disk_room(&running, None, 0).expect("three pending builds need 37 GB");
        assert_eq!(before.needed, 37 * GB);
        watch.sample(&Machine { at: SAMPLE, disk_total: 0, ..Machine::default() }, SAMPLE);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, SAMPLE), Some(before), "ended tasks cannot erase writes absent from the old budget");
        watch.measure_disk(disk(12 * GB));
        watch.measure_peaks(12 * GB, &BTreeMap::new(), 2 * SAMPLE);
        assert!(watch.disk_room(&BTreeMap::new(), None, 2 * SAMPLE).is_some(), "the next valid budget includes the 24 GB consumed while probes failed");
        watch.measure_disk(disk(36 * GB));
        watch.measure_peaks(36 * GB, &BTreeMap::new(), 3 * SAMPLE);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 3 * SAMPLE), None);
    }

    #[test]
    fn the_head_alone_needs_only_the_line_of_the_disk() {
        let watch = MachineWatch::default();
        watch.seed_peaks("app", &[8 * GB; 3]);
        watch.measure_disk(alondra_engine_ports::disk::Space { total: 500 * GB, free: 10 * GB, purgeable: 0 });
        let head = crate::admission::Key::Task("t1".into());
        assert_eq!(watch.disk_room_for(&BTreeMap::new(), Some("app"), Some(&head), 0), None, "nothing else at work: 10 GB are over the 5 GB line");
        assert!(watch.disk_room_for(&working(&[("t2", "app")]), Some("app"), Some(&head), 0).is_some(), "t2 may still take 8 GB");
    }

    #[test]
    fn the_head_excludes_forecasts_but_not_earlier_grants() {
        let head = crate::admission::Key::Task("t1".into());
        let working = BTreeMap::from([("t1".into(), None)]);
        let watch = MachineWatch::default();
        watch.measure_disk(alondra_engine_ports::disk::Space { total: 500 * GB, free: 16 * GB, purgeable: 0 });
        assert_eq!(watch.disk_room_for(&working, None, Some(&head), 0), None);
        watch.reserve_admission(head.clone(), None);
        assert_eq!(watch.disk_room_for(&BTreeMap::new(), None, Some(&head), 0).unwrap().needed, 21 * GB);
        assert_eq!(watch.disk_reserved(&BTreeMap::new()), 8 * GB);
    }

    #[test]
    fn fresh_disk_budgets_do_not_reserve_held_turns_against_their_return() {
        let watch = MachineWatch::default();
        let held = working(&[("t1", "app"), ("t2", "app"), ("t3", "app")]);
        watch.measure_disk(alondra_engine_ports::disk::Space { free: 36 * GB, purgeable: 0, total: 500 * GB });
        watch.measure_peaks(36 * GB, &held, 1);
        assert_eq!(watch.disk_reserved(&BTreeMap::new()), 0);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 1), None, "a fresh budget counts only the next build when returning held turns are excluded");
    }

    /// A sandbox's fake nodes build nothing: the gate reserves nothing for them, whatever the
    /// Mac's own disk does meanwhile.
    #[test]
    fn a_sandbox_with_fake_nodes_reserves_nothing_for_them() {
        let watch = MachineWatch::default();
        watch.ignore_peaks();
        assert!(watch.disk_room(&BTreeMap::new(), None, 0).is_some(), "fake builds still need a system disk measurement");
        watch.measure_peaks(50 * GB, &working(&[("t1", "app")]), 0);
        watch.measure_peaks(40 * GB, &working(&[("t1", "app")]), 1);
        watch.measure_disk(alondra_engine_ports::disk::Space { free: 8 * GB, purgeable: 0, total: 500 * GB });
        assert_eq!(watch.disk_used(&TaskId::from("t1")), 0);
        assert_eq!(watch.disk_room(&working(&[("t1", "app")]), Some("app"), 0), None, "8 GB free are over the 5 GB line");
    }

    #[test]
    fn a_full_disk_with_a_stopped_node_raises_one_card() {
        let phase0 = [waiter("s1", "Fase 0: quitar versión de identificadores", 0)];
        let (incident, actions) = step(None, &machine(0, 50), &phase0, false, 1_000);
        let incident = incident.unwrap();
        assert_eq!((incident.cause, incident.card.as_deref()), (MachineCause::Disk, Some("machine-1000")));
        let cards = raised(&actions);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].text(Lang::Es).0, "Libera espacio en disco: quedan 0 B");
        assert_eq!(cards[0].nodes, ["Fase 0: quitar versión de identificadores"]);

        let both = [phase0[0].clone(), waiter("s2", "Lumo", 2_000)];
        let (still, actions) = step(Some(incident), &machine(0, 50), &both, false, 11_000);
        assert!(actions.is_empty(), "a second node stopping is the same incident, not another card: {actions:?}");
        assert_eq!(still.unwrap().card.as_deref(), Some("machine-1000"));
    }

    #[test]
    fn a_disk_running_low_warns_before_any_node_stops_and_the_first_stop_renews_the_card() {
        let (incident, actions) = step(None, &machine(4 * GB, 50), &[], false, 0);
        assert_eq!(raised(&actions)[0].text(Lang::En).0, "Free up disk space: only 4.0 GB left");
        let (incident, actions) = step(incident, &machine(4 * GB, 50), &[], false, 10_000);
        assert!(actions.is_empty(), "no card again on every sample");
        let (incident, actions) = step(incident, &machine(0, 50), &[waiter("s1", "Lumo", 15_000)], false, 20_000);
        assert_eq!(actions[0], Action::Withdraw("machine-0".into()));
        assert_eq!(raised(&actions)[0].nodes, ["Lumo"]);
        assert_eq!(incident.unwrap().card.as_deref(), Some("machine-0-2"));
    }

    #[test]
    fn time_machine_snapshots_macos_can_purge_open_no_card_and_stop_no_node() {
        let (incident, actions) = step(None, &purging(4_800_000_000, 16_600_000_000), &[], false, 0);
        assert_eq!((incident, actions), (None, vec![]), "15 October: 4.8 GB free and 16.6 GB of snapshots");
        let (incident, _) = step(None, &machine(4_800_000_000, 50), &[], false, 0);
        assert!(incident.is_some(), "the same free space with nothing purgeable is low");
    }

    #[test]
    fn a_disk_macos_has_not_purged_in_time_opens_the_card_with_what_it_could_free() {
        let (incident, actions) = step(None, &purging(GB, 14 * GB), &[], false, 0);
        assert_eq!(incident.unwrap().cause, MachineCause::Disk, "7 October: 1 GB free, 14 GB purgeable not freed");
        let card = raised(&actions)[0];
        assert_eq!((card.disk_free, card.purgeable), (GB, 14 * GB));
        assert_eq!(card.text(Lang::En).0, "Free up disk space: only 1.0 GB free right now");
        let (_, actions) = step(None, &purging(GB, 3 * GB), &[], false, 0);
        assert_eq!((raised(&actions)[0].disk_free, raised(&actions)[0].purgeable), (4 * GB, 0), "little usable space: the usual card");
    }

    #[test]
    fn no_node_starts_while_the_disk_is_low_and_one_does_once_it_recovers() {
        let watch = MachineWatch::default();
        watch.launch_on_measured_disk();
        let here = crate::test_support::Directory::new();
        std::fs::create_dir(&here).unwrap();
        watch.measure_disk(alondra_engine_ports::disk::Space { total: 500 * GB, free: 400 * GB, purgeable: 0 });
        assert_eq!(watch.refuse_launch(&here), None, "a healthy disk starts nodes");
        let (incident, _) = step(None, &machine(GB, 50), &[], false, 0);
        watch.set_incident(incident);
        let refused = watch.refuse_launch(&here).expect("refused while the disk is low");
        assert!(refused.starts_with("The disk is nearly full: Alondra starts no node until there is space again, and "), "{refused}");
        assert!(refused.ends_with(" has a card about it"), "{refused}");
        watch.set_incident(None);
        assert_eq!(watch.refuse_launch(&here), None);
    }

    #[test]
    fn a_launch_sees_the_space_the_watch_measured_not_the_free_space_of_the_machine_running_the_test() {
        let watch = MachineWatch::default();
        watch.launch_on_measured_disk();
        let here = crate::test_support::Directory::new();
        std::fs::create_dir(&here).unwrap();
        watch.measure_disk(alondra_engine_ports::disk::Space { total: 500 * GB, free: GB, purgeable: 0 });
        assert!(watch.refuse_launch(&here).is_some_and(|why| why.contains("nearly full")), "a measured full disk refuses the launch");
        watch.measure_disk(alondra_engine_ports::disk::Space { total: 500 * GB, free: 400 * GB, purgeable: 0 });
        assert_eq!(watch.refuse_launch(&here), None, "a measured roomy disk starts it");
    }

    /// The daemon's own launches read the disk of the folder they work in, whatever the watch
    /// measured last: only a test's world takes the measurement in its place.
    #[test]
    fn a_launch_reads_its_folders_disk_unless_a_test_says_otherwise() {
        let watch = MachineWatch::default();
        let here = crate::test_support::Directory::new();
        std::fs::create_dir(&here).unwrap();
        let measured = alondra_engine_ports::disk::Space { total: 500 * GB, free: GB, purgeable: 0 };
        watch.measure_disk(measured);
        assert_ne!(watch.space_where_it_works(&here), Some(measured), "the daemon reads the folder's disk");
        watch.launch_on_measured_disk();
        assert_eq!(watch.space_where_it_works(&here), Some(measured), "a test's world reads the measurement");
    }

    /// Hours of a full disk with nodes waiting and nothing new measured: the one card stays, with
    /// no reminder and no other card.
    #[test]
    fn hours_of_a_full_disk_with_nothing_new_remind_nothing_and_raise_no_other_card() {
        let stopped = [waiter("s1", "Lumo", 0)];
        let (mut incident, _) = step(None, &machine(0, 50), &stopped, false, 0);
        for minute in 1..=12 * 60 {
            let (next, actions) = step(incident, &machine(0, 50), &stopped, false, minute * MIN);
            assert!(actions.is_empty(), "nothing at minute {minute}: {actions:?}");
            incident = next;
        }
        assert_eq!(incident.unwrap().card.as_deref(), Some("machine-0"));
    }

    /// The disk counts as fine once two measurements in a row are past the recovery line, at
    /// whatever time they come: everyone waiting is released then, never minutes later.
    #[test]
    fn the_disk_past_the_recovery_line_twice_in_a_row_releases_everyone_at_once() {
        let stopped = [waiter("s1", "Lumo", 0), waiter("s2", "Quill", 0)];
        let (incident, _) = step(None, &machine(0, 50), &stopped, false, 0);
        let (incident, actions) = step(incident, &machine(6 * GB, 50), &stopped, false, 10 * HOUR);
        assert!(actions.is_empty() && incident.is_some(), "6 GB is above the warning yet below the recovery line, however long");
        let (incident, actions) = step(incident, &machine(8 * GB, 50), &stopped, false, 10 * HOUR + SAMPLE);
        assert!(actions.is_empty() && incident.is_some(), "one measurement alone may be a blip");
        let (incident, actions) = step(incident, &machine(8 * GB, 50), &stopped, false, 10 * HOUR + 2 * SAMPLE);
        assert!(incident.is_none());
        assert_eq!(actions[0], Action::Withdraw("machine-0".into()));
        assert!(actions.contains(&Action::Release(vec!["s1".into(), "s2".into()])));
    }

    /// The watch looks again on every change of the domain, between two samples of the gauges: a
    /// second look at the same healthy sample is not a second measurement, so it neither ends the
    /// incident early nor starts its recovery over.
    #[test]
    fn a_second_look_at_the_same_sample_moves_no_recovery() {
        let (_home, domain) = world();
        let watch = &domain.machine_watch();
        let base = now_millis();
        let low = |at| Machine { at, ..machine(GB, 50) };
        let healthy = |at| Machine { at, ..machine(100 * GB, 50) };
        let _ = look(&*domain, low(base));
        assert!(watch.incident().is_some());
        let _ = look(&*domain, healthy(base + SAMPLE));
        assert_eq!(watch.incident().map(|incident| incident.healthy), Some(1));
        let _ = look(&*domain, healthy(base + SAMPLE));
        assert_eq!(watch.incident().map(|incident| incident.healthy), Some(1), "one healthy measurement, looked at twice");
        let _ = look(&*domain, healthy(base + 2 * SAMPLE));
        assert!(watch.incident().is_none(), "the second healthy measurement ends it");
    }

    /// 8 Oct 2026: the disk climbed past the recovery line for two samples and fell again, and a
    /// second card joined the first. The card now follows the measurements only: withdrawn when the
    /// disk is back past the line, a new one when it falls again, never two at once.
    #[test]
    fn a_disk_that_recovers_and_falls_again_never_shows_two_cards() {
        let (mut incident, actions) = step(None, &machine(4_800_000_000, 50), &[], false, 0);
        let mut open: Vec<String> = raised_ids(&actions);
        let samples = [(8 * GB, MIN), (8 * GB, MIN + SAMPLE), (8 * GB, MIN + 2 * SAMPLE), (3_800_000_000, 3 * MIN)];
        for (free, at) in samples {
            let (next, actions) = step(incident, &machine(free, 50), &[], false, at);
            for action in &actions {
                if let Action::Withdraw(id) = action {
                    open.retain(|card| card != id);
                }
            }
            open.extend(raised_ids(&actions));
            assert!(open.len() <= 1, "at {at}: {open:?}");
            incident = next;
        }
        assert_eq!(open, ["machine-180000"], "the disk fell again: one card, the new one");
    }

    #[test]
    fn a_wait_no_gauge_measures_is_the_nodes_state_never_a_card() {
        let queued = [Waiter { reason: "build queue full".into(), ..waiter("s1", "Quill", 0) }];
        for minute in 0..60 {
            let (incident, actions) = step(None, &machine(100 * GB, 50), &queued, false, minute * MIN);
            assert!(incident.is_none() && actions.is_empty(), "a wait no gauge measures is the node's state, never a card: {actions:?}");
        }
    }

    /// The morning of 8 October: a node waiting in the Bazel queue declared wait_for_machine, the
    /// user answered "I've fixed it", the node was woken, declared again, and a new card came out,
    /// three times in a row. A wait the Mac's gauges do not measure now puts no card at all, and an
    /// incident left from before closes with its card withdrawn.
    #[test]
    fn a_node_declaring_again_after_i_have_fixed_it_puts_no_card_in_front_of_the_user() {
        let queued = [Waiter { reason: "waiting in the Bazel queue behind make ablation".into(), ..waiter("s1", "Ritmo 2", 0) }];
        let mut incident = None;
        let mut cards = 0;
        for (round, fixed) in [(0, false), (1, true), (2, false), (3, true), (4, false), (5, true)] {
            let (next, actions) = step(incident, &machine(100 * GB, 50), &queued, fixed, round * MIN + 1_000);
            cards += raised(&actions).len();
            incident = next;
        }
        assert_eq!((cards, incident), (0, None));

        let left = Incident { since: 0, cause: MachineCause::Declared, card: Some("machine-1791441349131".into()), cards: 3, nodes_shown: 1, healthy: 0 };
        let (incident, actions) = step(Some(left), &machine(100 * GB, 50), &queued, false, MIN);
        assert!(incident.is_none());
        assert_eq!(actions, [Action::Withdraw("machine-1791441349131".into()), Action::Release(vec!["s1".into()])]);
    }

    #[test]
    fn i_have_fixed_it_releases_at_once_when_the_mac_is_fine_and_says_it_still_is_when_it_is_not() {
        let stopped = [waiter("s1", "Lumo", 0)];
        let (incident, _) = step(None, &machine(0, 50), &stopped, false, 0);
        let (incident, actions) = step(incident, &machine(GB / 2, 50), &stopped, true, MIN);
        assert_eq!(actions[0], Action::Withdraw("machine-0".into()), "the answered card goes");
        let cards = raised(&actions);
        assert_eq!(cards[0].text(Lang::Es).0, "Sigue: quedan 500 MB libres", "still full: a new card that says so");
        assert_eq!(cards[0].text(Lang::En).0, "Still: 500 MB free left");
        let (incident, actions) = step(incident, &machine(50 * GB, 50), &stopped, true, 2 * MIN);
        assert!(incident.is_none(), "no second sample needed once the user says so");
        assert!(actions.contains(&Action::Release(vec!["s1".into()])));
    }

    #[test]
    fn a_disk_that_fills_again_after_it_was_freed_gets_a_new_card() {
        let stopped = [waiter("s1", "Lumo", 0)];
        let (incident, first) = step(None, &machine(0, 50), &stopped, false, 0);
        let (incident, _) = step(incident, &machine(50 * GB, 50), &stopped, true, MIN);
        assert!(incident.is_none(), "freed and answered: the incident closes");
        let (incident, again) = step(None, &machine(GB, 50), &stopped, false, 60 * MIN);
        assert_eq!(raised(&first).len() + raised(&again).len(), 2);
        assert_eq!(incident.unwrap().card.as_deref(), Some("machine-3600000"), "its own card, though the last one was answered");
    }

    /// 9 Oct 2026: «Libera memoria: macOS avisa de presión de memoria (38% libre)», with
    /// «Responder al orquestador», asked the user for what Alondra already does by holding new
    /// starts. Memory, however long and however hard, opens no incident.
    #[test]
    fn memory_under_pressure_for_hours_with_nodes_waiting_opens_no_incident_and_no_card() {
        let stopped = [Waiter { reason: "killed for lack of memory".into(), ..waiter("s1", "Lumo", 0) }];
        let mut mac = MacState::default();
        let mut incident = None;
        for sample in 0..(3 * 60 * 6) {
            let now = sample * 10_000;
            let pressed = Machine { at: now, memory_pressure: Some(MemoryPressure::Critical), ..machine(100 * GB, 3) };
            mac.step(&pressed, now);
            let (next, actions) = step(incident, &pressed, &stopped, false, now);
            assert!(next.is_none() && actions.is_empty(), "no incident, card or push at {now}: {actions:?}");
            incident = next;
        }
        assert_eq!(mac.room(0, 3 * 60 * MIN), Err(NoRoom::Pressure(MemoryPressure::Critical)), "the gauges still see the pressure, and the gate holds new starts by them");
    }

    #[test]
    fn a_second_node_stopped_by_the_same_full_disk_and_hours_without_news_raise_no_other_card() {
        let (incident, _) = step(None, &machine(0, 50), &[waiter("s1", "Lumo", 0)], false, 0);
        let both = [waiter("s1", "Lumo", 0), waiter("s2", "Quill", 1_000)];
        let (incident, actions) = step(incident, &machine(0, 50), &both, false, 10_000);
        assert!(actions.is_empty(), "{actions:?}");
        let (incident, actions) = step(incident, &machine(0, 50), &both, false, 10 * HOUR);
        assert!(actions.is_empty(), "hours later with nothing new: {actions:?}");
        assert_eq!(incident.unwrap().card.as_deref(), Some("machine-0"));
    }

    struct Home(crate::test_support::Directory);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn world() -> (Home, Arc<dyn Runtime>) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::wiring::open(&home).unwrap());
        (Home(home), domain)
    }

    #[derive(Clone, Default)]
    struct Told(Arc<Mutex<Vec<String>>>);
    impl Alerts for Told {
        async fn tell(&self, text: impl Fn(bool) -> String + Send + Sync, _passive: bool) {
            self.0.lock().unwrap().push(text(true));
        }
    }

    fn open_machine_cards(domain: &dyn Runtime) -> Vec<alondra_domain_rules::Interaction> {
        let state = domain.state();
        state.interactions.values().filter(|card| card.system_card == Some(SystemCard::Machine) && !card.withdrawn
            && matches!(card.payload, InteractionPayload::Question { answer: None, .. })).cloned().collect()
    }

    fn orchestrator_inbox(domain: &dyn Runtime) -> Vec<String> {
        domain.store().lock().unwrap().pending_messages().unwrap().iter()
            .filter(|message| message.recipient_kind == "orchestrator")
            .map(|message| message.message.clone()).collect()
    }

    #[derive(Default)]
    struct Ticks { at: Millis, handed: Millis, done: Millis }

    /// The time the watcher samples at, moved by the test; `done` is the last sample the watcher
    /// finished with, known when it asks for the next one.
    #[derive(Clone, Default)]
    struct Clock { ticks: Arc<Mutex<Ticks>>, asked: Arc<tokio::sync::Notify> }

    impl Clock {
        /// The watcher's sample source: `machine(at)` once the test has moved the clock, nothing before.
        fn source(&self, machine: impl Fn(Millis) -> Machine + Send + 'static) -> impl Fn() -> Option<Machine> + Send + 'static {
            let clock = self.clone();
            move || {
                let at = {
                    let mut ticks = clock.ticks.lock().unwrap();
                    let at = ticks.at;
                    ticks.done = std::mem::replace(&mut ticks.handed, at);
                    at
                };
                clock.asked.notify_one();
                (at != 0).then(|| machine(at))
            }
        }

        /// Samples every `SAMPLE` up to `to`, each one only once the watcher is done with the one before.
        async fn sample_until(&self, to: Millis) {
            loop {
                let at = {
                    let mut ticks = self.ticks.lock().unwrap();
                    if ticks.at >= to {
                        return;
                    }
                    ticks.at += SAMPLE;
                    ticks.at
                };
                while self.ticks.lock().unwrap().done != at {
                    self.asked.notified().await;
                }
            }
        }

        async fn samples(&self, count: Millis) {
            let to = self.ticks.lock().unwrap().at + count * SAMPLE;
            self.sample_until(to).await;
        }
    }

    fn watching(domain: &Arc<dyn Runtime>, clock: &Clock, machine: impl Fn(Millis) -> Machine + Send + 'static, told: &Told) -> tokio::task::JoinHandle<()> {
        tokio::spawn(watch(domain.clone(), clock.source(machine), told.clone(), Duration::from_millis(1)))
    }

    #[tokio::test]
    async fn a_full_disk_records_pressure_and_recovers_without_cards_or_pushes() {
        let (_home, domain) = world();
        domain.machine_watch().declare("node", None, "disk full", 0, now_millis());
        let disk = Arc::new(Mutex::new(0u64));
        let reading = disk.clone();
        let told = Told::default();
        let clock = Clock::default();
        let watcher = watching(&domain, &clock, move |at| Machine { at, ..machine(*reading.lock().unwrap(), 50) }, &told);
        clock.samples(1).await;
        assert!(domain.machine_watch().incident().is_some());
        assert!(open_machine_cards(&*domain).is_empty());
        clock.samples(5).await;
        assert!(open_machine_cards(&*domain).is_empty());
        assert!(told.0.lock().unwrap().is_empty());
        assert!(orchestrator_inbox(&*domain).is_empty());
        *disk.lock().unwrap() = 50 * GB;
        clock.samples(HEALTHY_SAMPLES as Millis).await;
        watcher.abort();
        assert!(domain.machine_watch().incident().is_none());
        assert!(domain.machine_watch().declared().is_empty());
        assert!(open_machine_cards(&*domain).is_empty());
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_cpu_load_opens_no_incident_no_card_and_no_notice() {
        let (_home, domain) = world();
        let told = Told::default();
        let clock = Clock::default();
        let watcher = watching(&domain, &clock, |at| Machine { at, load: [90.0; 3], unreliable: true, ..machine(50 * GB, 50) }, &told);
        clock.samples(10).await;
        watcher.abort();
        assert!(domain.machine_watch().incident().is_none());
        assert!(open_machine_cards(&*domain).is_empty());
        assert!(orchestrator_inbox(&*domain).is_empty(), "{:?}", orchestrator_inbox(&*domain));
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_card_named_like_a_machine_card_is_not_one() {
        let (_home, domain) = world();
        let options = || vec![alondra_domain_rules::QuestionOption { id: "x".into(), label: "x".into(), description: None }];
        domain.raise_worded_problem("machine-setup", None, "setup", "Choose the setup".into(), "Two ways to set it up".into(), "{}".into(), options(), None).unwrap();
        domain.raise_worded_problem("machine-1", Some(SystemCard::Machine), MACHINE_CAUSE, "Free up memory".into(), "Nodes wait".into(), "{}".into(), options(), None).unwrap();
        withdraw_strays(&*domain);
        let state = domain.state();
        assert!(!state.interactions[&"machine-setup".into()].withdrawn, "a card is not a machine card by its id");
        assert!(state.interactions[&"machine-1".into()].withdrawn, "a machine card no incident shows is withdrawn");
    }

    #[tokio::test]
    async fn recording_a_machine_incident_does_not_write_the_domain_or_push() {
        let (_home, domain) = world();
        let before = domain.state();
        let told = Told::default();
        let (_, actions) = step(None, &machine(0, 50), &[], false, 0);
        act(&domain, &told, actions).await;
        let after = domain.state();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.interactions, before.interactions);
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_full_disk_neither_breaks_the_machine_watch_nor_fills_the_log() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let watch = MachineWatch::persisted_in(&home, Arc::new(alondra_engine_doubles::disk::Disks::default()));
        let disk = crate::save::full_disk::fill();
        for session in ["first", "second", "third"] {
            watch.declare(session, None, "disk full", 0, 1);
        }
        let incident = Incident { since: 1, cause: MachineCause::Disk, card: None, cards: 1, nodes_shown: 3, healthy: 0 };
        watch.set_incident(Some(incident.clone()));
        assert_eq!(watch.incident(), Some(incident));
        assert_eq!(watch.declared().len(), 3);
        watch.measure_nodes(1, vec![1 << 30]);
        let _ = watch.mac();
        let logged = |what: &str| disk.lines().iter().filter(|line| line.contains(what)).count();
        assert_eq!(logged("could not save"), 1, "{:?}", disk.lines());
        assert!(!home.join(FILE).exists());

        disk.free();
        watch.declare("fourth", None, "disk full", 0, 2);
        let saved: Record = serde_json::from_slice(&std::fs::read(home.join(FILE)).unwrap()).unwrap();
        assert_eq!(saved.declared.len(), 4);
        assert_eq!(logged(&format!("saved {} again", home.join(FILE).display())), 1, "{:?}", disk.lines());
        assert_eq!(logged("could not save"), 1);
    }

    #[test]
    fn peaks_and_disk_room_are_kept_after_a_panic_holding_the_machine_watch() {
        let (_home, domain) = world();
        let watch = &domain.machine_watch();
        watch.poison();
        watch.measure_peaks(50 * GB, &working(&[("t1", "app")]), 0);
        watch.poison();
        watch.measure_disk(alondra_engine_ports::disk::Space { free: 0, purgeable: 0, total: 500 * GB });
        watch.poison();
        assert!(watch.disk_room(&working(&[("t1", "app")]), None, 0).is_some(), "an empty disk has no room for the next build");
    }

    #[tokio::test]
    async fn a_thread_that_panicked_holding_the_machine_watch_does_not_stop_it() {
        let (_home, domain) = world();
        domain.machine_watch().poison();
        let mut watcher = tokio::spawn(watch(domain.clone(), || Some(machine(400 * GB, 50)), Told::default(), Duration::from_millis(20)));
        tokio::select! {
            () = domain.machine_watch().sampled.notified() => {}
            ended = &mut watcher => panic!("the machine watch stopped: {ended:?}"),
        }
        watcher.abort();
        domain.machine_watch().declare("s", None, "RAM", 0, now_millis());
        assert_eq!(domain.machine_watch().declared().len(), 1);
        assert!(domain.machine_watch().mac().free_percent.is_some());
    }

    #[test]
    fn a_node_that_took_another_turn_no_longer_waits() {
        let (_home, domain) = world();
        domain.machine_watch().declare("s", None, "RAM", 0, now_millis());
        assert_eq!(waiters(&*domain).len(), 1);
        crate::wiring::engine(&*domain).script().turns.insert("s".into(), 1);
        assert!(waiters(&*domain).is_empty());
        assert!(waiters(&*domain).is_empty(), "and it is forgotten");
    }

    fn pressed(at: Millis, under: bool) -> Machine {
        let free = if under { 3 } else { 60 };
        Machine { at, memory_pressure: Some(if under { MemoryPressure::Critical } else { MemoryPressure::Normal }), ..machine(100 * GB, free) }
    }

    fn holds(domain: &dyn Runtime, now: Millis) -> bool {
        let head = crate::admission::Waiting { key: crate::admission::Key::Task("t".into()), priority: Default::default(), resuming: false, since: 0 };
        crate::admission::decide(&head, 0, 4, &domain.machine_watch().mac(), 0, None, 0, now).is_err()
    }

    #[tokio::test]
    async fn sustained_memory_pressure_puts_no_card_and_no_push_while_the_nodes_are_held_and_resumed() {
        let (_home, domain) = world();
        domain.machine_watch().declare("s1", None, "killed for lack of memory", 0, now_millis());
        let clock = Clock::default();
        let under = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let reading = under.clone();
        let told = Told::default();
        let watcher = watching(&domain, &clock, move |at| pressed(at, reading.load(std::sync::atomic::Ordering::SeqCst)), &told);

        clock.sample_until(2 * MIN).await;
        assert!(holds(&*domain, 2 * MIN), "the gate holds starts while memory has no room");
        clock.sample_until(60 * MIN).await;
        assert!(holds(&*domain, 60 * MIN));
        assert!(domain.machine_watch().incident().is_none());
        assert!(open_machine_cards(&*domain).is_empty(), "{:?}", open_machine_cards(&*domain));
        assert!(told.0.lock().unwrap().is_empty(), "no push: {:?}", told.0.lock().unwrap());
        assert!(orchestrator_inbox(&*domain).is_empty(), "pacing asks nothing of the orchestrator: {:?}", orchestrator_inbox(&*domain));

        under.store(false, std::sync::atomic::Ordering::SeqCst);
        clock.sample_until(62 * MIN).await;
        watcher.abort();
        assert!(!holds(&*domain, 62 * MIN), "the gate lets nodes start and resume once memory is back");
        assert!(orchestrator_inbox(&*domain).is_empty(), "{:?}", orchestrator_inbox(&*domain));
        let state = domain.state();
        let cards = state.interactions.values().filter(|card| card.system_card == Some(SystemCard::Machine) && matches!(card.payload, InteractionPayload::Question { .. })).count();
        assert_eq!(cards, 0, "never a machine card, open or withdrawn");
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_new_daemon_retires_the_memory_card_an_earlier_one_left_open() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let left = Incident { since: 1_000, cause: MachineCause::Memory, card: Some("machine-1000".into()), cards: 1, nodes_shown: 0, healthy: 0 };
        std::fs::write(home.join(FILE), serde_json::json!({"incident": left, "declared": {}}).to_string()).unwrap();
        let domain = Arc::new(crate::wiring::open(&home).unwrap());
        let body = r#"{"cause":"memory","disk_free":0,"memory_free_percent":38,"memory_pressure":true,"single_node":true}"#;
        domain.raise_worded_problem("machine-1000", Some(SystemCard::Machine), MACHINE_CAUSE, "Free up memory: macOS reports memory pressure (38% free) with a single node working".into(), "something outside Alondra uses the memory".into(), body.into(), vec![
            alondra_domain_rules::QuestionOption { id: MACHINE_FIXED_OPTION.into(), label: MACHINE_FIXED_LABEL.into(), description: None },
        ], None).unwrap();
        assert_eq!(open_machine_cards(&*domain).len(), 1);
        assert!(domain.machine_watch().incident().is_none(), "the memory incident is dropped on load");

        let told = Told::default();
        let clock = Clock::default();
        let watcher = watching(&(domain.clone() as Arc<dyn Runtime>), &clock, |at| pressed(at, true), &told);
        clock.samples(5).await;
        watcher.abort();
        assert!(open_machine_cards(&*domain).is_empty(), "no new card in its place");
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_sample_already_taken_writes_no_second_row() {
        let (home, domain) = world();
        let sample = Machine { at: 1_792_662_011_421, ..machine(400 * GB, 50) };
        pace(&*domain, &sample);
        pace(&*domain, &sample);
        let day = alondra_daemon_engine_model::local_day(sample.at);
        let csv = std::fs::read_to_string(home.0.join(crate::machine_state::SAMPLES_DIR).join(format!("{day}.csv"))).unwrap();
        let rows: Vec<_> = csv.lines().skip(1).collect();
        assert_eq!(rows.len(), 1, "{csv}");
        assert!(rows[0].starts_with("1792662011421,"), "{csv}");
    }

    #[test]
    fn swap_growth_and_real_memory_shortage_hold_admission_without_a_card() {
        let (_home, domain) = world();
        let base = now_millis();
        for elapsed in (0..=MIN).step_by(SAMPLE as usize) {
            let growth = elapsed as u64 * (1025 << 20) / MIN as u64;
            let sample = Machine { at: base + elapsed, memory_total: 18 << 30,
                memory_available: 10 << 30, memory_free_percent: Some(60),
                compressor: Some(alondra_core::MemoryCompressor { page_size: 16384,
                    swap_used: (1 << 30) + growth, swap_total: Some((2 << 30) + growth),
                    swapouts: growth / 16384, ..Default::default() }),
                ..machine(100 * GB, 60) };
            assert!(look(&*domain, sample).is_empty());
        }
        let mac = domain.machine_watch().mac();
        assert!(matches!(mac.room(0, base + MIN), Err(NoRoom::SwapGrowth { .. })));
        let detail = mac.memory_detail(1);
        for expected in ["system memory", "available", "nodes", "pending", "swap used", "allocated", "swap growth"] {
            assert!(detail.contains(expected), "{detail}");
        }
        assert!(domain.machine_watch().disk_detail(&BTreeMap::new(), None, base + MIN).unwrap().contains("container free"));
        assert!(open_machine_cards(&*domain).is_empty());
        assert!(orchestrator_inbox(&*domain).is_empty());
        assert!(domain.machine_watch().incident().is_none());
    }

    #[test]
    fn failed_node_measurements_keep_the_last_footprint_but_cannot_cancel_reserves() {
        let watch = MachineWatch::default();
        let footprints = BTreeMap::from([("node".into(), 1 << 30)]);
        watch.measure_node_memory(0, footprints.clone());
        let _ = watch.sample(&Machine { at: SAMPLE, ..machine(100 * GB, 60) }, SAMPLE);
        assert_eq!(watch.mac().observed_nodes_at, Some(0));
        let logged = crate::save::full_disk::fill();
        logged.free();
        for _ in 0..2 { watch.nodes_unavailable(&std::io::ErrorKind::PermissionDenied.into()); }
        let mac = watch.mac();
        assert_eq!(mac.nodes, footprints);
        assert_eq!(mac.nodes_at, None);
        assert_eq!(mac.observed_nodes_at, None);
        watch.measure_node_memory(SAMPLE, footprints);
        assert_eq!(watch.mac().nodes_at, Some(SAMPLE));
        let lines = logged.lines();
        assert_eq!(lines.iter().filter(|line| line.contains("node memory measurements unavailable")).count(), 1);
        assert_eq!(lines.iter().filter(|line| line.contains("node memory measurements recovered")).count(), 1);
    }

    #[test]
    fn a_footprint_newer_than_the_system_sample_cannot_cancel_a_reserve() {
        let watch = MachineWatch::default();
        watch.measure_node_memory(2 * SAMPLE, BTreeMap::from([("node".into(), 1 << 30)]));
        let _ = watch.sample(&Machine { at: SAMPLE, ..machine(100 * GB, 60) }, SAMPLE);
        assert_eq!(watch.mac().observed_nodes_at, None);
        let _ = watch.sample(&Machine { at: 3 * SAMPLE, ..machine(100 * GB, 60) }, 3 * SAMPLE);
        assert_eq!(watch.mac().observed_nodes_at, Some(2 * SAMPLE));
    }
}
