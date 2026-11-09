//! Machine pressure is recorded as samples, incidents and admission state. No incident creates
//! a user card or a fallback push. The admission gate resumes waiting nodes after recovery.
//! Disk incidents retain hysteresis; memory only paces new starts and never stops running work.
use alondra_core::domain::SystemCard;
#[cfg(test)]
use alondra_core::machine_incident::MACHINE_FIXED_LABEL;
use crate::locks::Poisonable as _;
use alondra_core::machine_incident::{ MACHINE_FIXED_OPTION, MACHINE_PREFIX, MachineCard, MachineCause, MachineResource};
use alondra_core::views::Lang;
use alondra_core::domain::TaskId;
use alondra_core::{Machine, MemoryPressure, Millis, now_millis};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One machine incident is open at a time, so its card has one cause.
#[cfg(test)]
const MACHINE_CAUSE: &str = "machine";
use std::sync::Mutex;
use std::time::Duration;

use crate::domain_runtime::DomainRuntime;
use crate::machine_state::{MacState, NoRoom};
use crate::usage_pause::Alerts;

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
    #[serde(default)]
    incident_volume: Option<PathBuf>,
    declared: BTreeMap<String, Declared>,
    /// Mount points of the volumes nodes were launched on, other than the Mac's own disk.
    #[serde(default)]
    volumes: BTreeSet<PathBuf>,
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
    #[serde(skip)]
    boot_short: bool,
    /// A sandbox's fake nodes build nothing: they take nothing from the disk and reserve nothing.
    #[serde(skip)]
    peaks_ignored: bool,
    #[serde(skip)]
    disk: Option<crate::disk::Space>,
    #[serde(skip)]
    disk_volume: Option<PathBuf>,
    #[serde(skip)]
    boot_disk: Option<crate::disk::Space>,
    #[serde(skip)]
    swap: crate::swap_disk::SwapDisk,
    #[serde(skip)]
    swap_short: bool,
    /// What a node of each project takes at its peak on each other volume builds were seen to
    /// write to, by mount point; such a volume is watched like one nodes were launched on.
    #[serde(default)]
    build_volumes: BTreeMap<PathBuf, crate::disk_peaks::Estimates>,
    /// The other volumes builds may write to, measured until they are seen to.
    #[serde(skip)]
    candidate_peaks: BTreeMap<PathBuf, crate::disk_peaks::Estimates>,
    #[serde(skip)]
    volume_stretches: BTreeMap<PathBuf, crate::disk_peaks::Ledger>,
    /// The latest space of every other volume builds may write to.
    #[serde(skip)]
    volume_spaces: BTreeMap<PathBuf, crate::disk::Space>,
    /// Only a complete mount table can suspend a restriction without a space reading.
    #[serde(skip)]
    volume_mounts: Option<BTreeSet<PathBuf>>,
    #[serde(skip)]
    volume_readings_fresh: bool,
    /// Work not yet covered by a newer disk budget, including tasks that have since ended.
    #[serde(skip)]
    volume_working: BTreeMap<PathBuf, BTreeMap<crate::admission::Key, Option<String>>>,
    #[serde(skip)]
    volume_grants: BTreeMap<PathBuf, BTreeMap<crate::admission::Key, u64>>,
    /// The volumes builds write to that were short of room for the next build when last asked.
    #[serde(skip)]
    volumes_short: BTreeSet<PathBuf>,
    #[serde(skip)]
    candidates: Candidates,
}

/// Where the other volumes builds may write to come from: none until the daemon looks at the
/// Mac's mounted volumes, so tests and sandboxes never measure the Mac's real ones.
#[derive(Clone, Debug, Default)]
enum Candidates {
    #[default]
    None,
    Mounted,
    #[cfg(any(test, feature = "test-hooks"))]
    Given(Vec<(PathBuf, crate::disk::Space, u64)>),
    #[cfg(any(test, feature = "test-hooks"))]
    Probed { mounts: Option<Vec<PathBuf>>, spaces: Vec<(PathBuf, crate::disk::Space, u64)> },
}

struct VolumeMeasurements {
    mounts: BTreeSet<PathBuf>,
    spaces: Vec<(PathBuf, crate::disk::Space, u64)>,
}

impl Record {
    /// The first volume builds write to short of room for the next node, of `project`, on top of
    /// what the `working` tasks may still take there.
    fn volume_short(&mut self, working: &BTreeMap<TaskId, Option<String>>, project: Option<&str>, head: Option<&crate::admission::Key>) -> Option<crate::disk::Room> {
        if self.peaks_ignored {
            return None;
        }
        let mut first = None;
        for (mount, estimates) in &self.build_volumes {
            if self.volume_mounts.as_ref().is_some_and(|mounts| !mounts.contains(mount)) { continue; }
            let unobserved = self.volume_working.entry(mount.clone()).or_default();
            for (task, project) in working {
                unobserved.entry(crate::admission::Key::Task(task.clone())).or_insert_with(|| project.clone());
            }
            let stretches = self.volume_stretches.get(mount);
            let grants = self.volume_grants.get(mount);
            let granted = grants.map_or(0, |grants| grants.values().copied().fold(0u64, u64::saturating_add));
            let reserved = unobserved.iter().filter(|(key, _)| Some(*key) != head && grants.is_none_or(|grants| !grants.contains_key(*key))).map(|(key, project)| {
                let estimate = estimates.estimate_beside(project.as_deref());
                match key {
                    crate::admission::Key::Task(task) => stretches.map_or(estimate, |stretches| stretches.reserved(task, estimate)),
                    _ => estimate,
                }
            }).fold(granted, u64::saturating_add);
            let needed = reserved.saturating_add(estimates.estimate_beside(project)).saturating_add(crate::disk::low_lines().usable);
            let room = self.volume_spaces.get(mount).map(|space| space.room_for(needed, self.volumes_short.contains(mount)));
            match room {
                Some(Ok(())) => {
                    self.volumes_short.remove(mount);
                }
                room => {
                    let room = match room {
                        Some(Err(room)) => {
                            self.volumes_short.insert(mount.clone());
                            room
                        }
                        None => crate::disk::Room { free: 0, needed, volume: None },
                        Some(Ok(())) => unreachable!(),
                    };
                    let name = mount.file_name().map_or_else(|| mount.display().to_string(), |name| name.to_string_lossy().into_owned());
                    if first.is_none() {
                        first = Some(crate::disk::Room { volume: Some(name), ..room });
                    }
                }
            }
        }
        first
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
pub(crate) enum Action {
    Log { id: String, card: MachineCard },
    Withdraw(String),
    /// The incident is over: its declared waits are left to the admission gate.
    Release(Vec<String>),
}

/// What the gauges say is wrong. Once the disk was the cause it must climb past
/// `disk::recovered_lines()` to count as fine, so a disk hovering at the line does not flap. Memory
/// and the CPU load open none: they only pace new starts at the gate.
pub(crate) fn failing(machine: &Machine, previous: Option<MachineCause>) -> Option<MachineCause> {
    (valid_disk(disk_space(machine)) && disk_space(machine).low(previous == Some(MachineCause::Disk))).then_some(MachineCause::Disk)
}

fn disk_space(machine: &Machine) -> crate::disk::Space {
    crate::disk::Space { free: machine.disk_available, purgeable: machine.disk_purgeable, total: machine.disk_total }
}

fn valid_disk(space: crate::disk::Space) -> bool {
    space.total > 0 && space.free.checked_add(space.purgeable).is_some_and(|usable| usable <= space.total)
}

fn card_for(cause: MachineCause, machine: &Machine, disk: Option<&str>, waiting: &[Waiter]) -> MachineCard {
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
        disk: disk.filter(|_| cause == MachineCause::Disk).map(str::to_owned),
        still: false,
    }
}

/// The machine with the disk gauge of its fullest volume: the Mac's own, or one a node works on,
/// named when it is not the Mac's own.
/// The fullest is the one with the least usable space, or, when one is low only because macOS has
/// not purged yet, that one.
#[cfg(test)]
pub(crate) fn tightest(machine: Machine, volumes: &[(String, crate::disk::Space)]) -> (Machine, Option<String>) {
    tightest_on(machine, volumes)
}

fn tightest_on<T: Clone>(machine: Machine, volumes: &[(T, crate::disk::Space)]) -> (Machine, Option<T>) {
    let fullness = |space: &crate::disk::Space| (!space.low(false), space.usable());
    match volumes.iter().min_by_key(|(_, space)| fullness(space)) {
        Some((name, space)) if fullness(space) < fullness(&disk_space(&machine)) || machine.disk_total == 0 =>
            (Machine { disk_available: space.free, disk_purgeable: space.purgeable, disk_total: space.total, ..machine }, Some(name.clone())),
        _ => (machine, None),
    }
}

/// One sample of the incident: the gauges, the nodes waiting, and whether the user just said they
/// fixed it. Pure, so every turn of it is tested without a clock.
#[cfg(any(test, feature = "test-hooks"))]
pub(crate) fn step(previous: Option<Incident>, machine: &Machine, waiting: &[Waiter], fixed: bool, now: Millis) -> (Option<Incident>, Vec<Action>) {
    step_on(previous, machine, None, waiting, fixed, now)
}

/// `step` with the name of the volume the disk gauge measured, when it is not the Mac's own.
pub(crate) fn step_on(previous: Option<Incident>, machine: &Machine, disk: Option<&str>, waiting: &[Waiter], fixed: bool, now: Millis) -> (Option<Incident>, Vec<Action>) {
    step_with_disk_measurement(previous, machine, disk, waiting, fixed, now, valid_disk(disk_space(machine)))
}

fn step_with_disk_measurement(previous: Option<Incident>, machine: &Machine, disk: Option<&str>, waiting: &[Waiter], fixed: bool, now: Millis, measured_disk: bool) -> (Option<Incident>, Vec<Action>) {
    let mut previous = previous;
    if let Some(incident) = previous.as_mut().filter(|incident| incident.cause == MachineCause::Disk && !measured_disk) {
        incident.healthy = 0;
        return (previous, vec![]);
    }
    let measured = failing(machine, previous.as_ref().map(|incident| incident.cause));
    let Some(mut incident) = previous else {
        let Some(cause) = measured else { return (None, vec![]) };
        return open(cause, machine, disk, waiting, now);
    };
    let fine = measured.is_none();
    incident.healthy = if fine { incident.healthy + 1 } else { 0 };
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
        let card = MachineCard { still: fixed, ..card_for(cause, machine, disk, waiting) };
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
fn open(cause: MachineCause, machine: &Machine, disk: Option<&str>, waiting: &[Waiter], now: Millis) -> (Option<Incident>, Vec<Action>) {
    let card = card_for(cause, machine, disk, waiting);
    let id = format!("{MACHINE_PREFIX}{now}");
    let incident = Incident { since: now, cause, card: Some(id.clone()), cards: 1, nodes_shown: waiting.len(), healthy: 0 };
    (Some(incident), vec![Action::Log { id, card }])
}

/// The volume the machine's own disk gauge measures.
fn own_disk() -> &'static Path {
    let data = Path::new(crate::machine::BOOT_DATA);
    if data.exists() { data } else { Path::new("/") }
}

pub struct MachineWatch {
    #[cfg(any(test, feature = "test-hooks"))]
    pub sampled: tokio::sync::Notify,
    #[cfg(any(test, feature = "test-hooks"))]
    pub sample_threads: Mutex<Vec<std::thread::ThreadId>>,
    /// A holder that panics leaves it whole, so it is recovered: see `MachineWatch::record`.
    record: Mutex<Record>,
    file: Option<PathBuf>,
    save_failures: crate::save::Failures,
    changed: tokio::sync::Notify,
}

impl Default for MachineWatch {
    fn default() -> Self {
        Self { record: Mutex::default(), file: None, save_failures: crate::save::Failures::default(), changed: tokio::sync::Notify::new(),
            #[cfg(any(test, feature = "test-hooks"))] sampled: tokio::sync::Notify::new(),
            #[cfg(any(test, feature = "test-hooks"))] sample_threads: Mutex::new(vec![]),
        }
    }
}

impl MachineWatch {
    /// A memory incident an earlier daemon left open is dropped: `watch` then withdraws its card
    /// as a stray, and its declared waits stay with the admission gate.
    pub(crate) fn persisted_in(home: &Path) -> Self {
        let file = home.join(FILE);
        let mut record: Record = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        if record.incident.take_if(|incident| incident.cause == MachineCause::Memory).is_some() {
            log!("dropping the memory incident an earlier daemon left open: memory no longer asks the user anything");
        }
        Self { record: Mutex::new(record), file: Some(file), ..Self::default() }
    }

    /// A holder that panicked leaves a valid record: every step replaces one whole field or one
    /// whole entry, and the next sample rewrites what it reads.
    fn record(&self) -> std::sync::MutexGuard<'_, Record> {
        self.record.lock_or_recover("the machine watch")
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn poison(&self) {
        crate::locks::poison(&self.record);
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
        if valid_disk(disk) { record.boot_disk = Some(disk); }
        let before = record.mac.room(0, now);
        if record.mac.step(machine, now) {
            record.mac.observed_nodes = record.mac.nodes.clone();
            record.mac.observed_nodes_at = record.mac.nodes_at.filter(|at| *at <= now);
        }
        Some(before)
    }

    /// Keeps what each node measured at `now`; the gate reserves the new estimate from then on.
    pub(crate) fn measure_node_memory(&self, now: Millis, footprints: BTreeMap<alondra_core::domain::AgentId, u64>) {
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

    #[cfg(any(test, feature = "test-hooks"))]
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

    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn set_disk_low(&self, low: bool) {
        let disk = Machine { disk_total: 500_000_000_000, disk_available: if low { 1_000_000_000 } else { 400_000_000_000 }, ..Machine::default() };
        self.measure_disk(disk_space(&disk));
        self.set_incident(step(None, &disk, &[], false, 0).0);
    }

    pub(crate) fn disk_low(&self) -> bool {
        self.incident().is_some_and(|incident| incident.cause == MachineCause::Disk)
    }

    /// Takes the Mac's own disk's space.
    pub fn measure_disk(&self, space: crate::disk::Space) {
        if valid_disk(space) {
            let mut record = self.record();
            record.boot_disk = Some(space);
            record.disk = Some(space);
            record.disk_volume = None;
            record.disk_working.clear();
            record.disk_grants.clear();
        }
    }

    #[cfg(test)]
    fn measure_fullest_disk(&self, space: crate::disk::Space) {
        self.measure_fullest_disk_on(space, None);
    }

    fn measure_fullest_disk_on(&self, space: crate::disk::Space, volume: Option<PathBuf>) {
        if valid_disk(space) {
            let mut record = self.record();
            record.disk = Some(space);
            record.disk_volume = volume;
        }
    }

    fn suspend_unmounted(&self) -> Option<Incident> {
        let mut record = self.record();
        let absent = |volume: &Option<PathBuf>, mounts: &Option<BTreeSet<PathBuf>>| {
            volume.as_ref().is_some_and(|volume| mounts.as_ref().is_some_and(|mounts| !mounts.contains(volume)))
        };
        if absent(&record.disk_volume, &record.volume_mounts) {
            record.disk = None;
            record.disk_volume = None;
            record.short = false;
        }
        if record.incident.as_ref().is_some_and(|incident| incident.cause == MachineCause::Disk)
            && absent(&record.incident_volume, &record.volume_mounts) {
            let incident = record.incident.take();
            record.incident_volume = None;
            self.save(&record);
            incident
        } else {
            None
        }
    }

    pub fn ignore_peaks(&self) {
        self.record().peaks_ignored = true;
    }

    /// Measures from now on the Mac's other mounted volumes builds may write to.
    pub fn discover_volumes(&self) {
        self.record().candidates = Candidates::Mounted;
    }

    /// Volumes alone on their disk: what they hold grows by what their free space falls.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn give_volumes(&self, volumes: Vec<(PathBuf, crate::disk::Space)>) {
        let volumes = volumes.into_iter().map(|(mount, space)| (mount, space, space.total.saturating_sub(space.usable()))).collect();
        self.record().candidates = Candidates::Given(volumes);
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn give_volumes_holding(&self, volumes: Vec<(PathBuf, crate::disk::Space, u64)>) {
        self.record().candidates = Candidates::Given(volumes);
    }

    /// Every other volume builds may write to, its space and what it holds.
    fn candidate_spaces(&self) -> Option<VolumeMeasurements> {
        let (candidates, watched) = {
            let record = self.record();
            (record.candidates.clone(), record.volumes.iter().chain(record.build_volumes.keys()).cloned().collect::<BTreeSet<_>>())
        };
        match candidates {
            Candidates::None => None,
            Candidates::Mounted => {
                let mounts = crate::disk::mounted_volumes().ok()?;
                let wanted: BTreeSet<_> = mounts.candidates.into_iter().chain(watched).collect();
                let spaces = wanted.into_iter().filter(|mount| mounts.all.contains(mount))
                    .filter(|mount| !crate::disk::same_volume(mount, own_disk()))
                    .filter_map(|mount| {
                        if crate::disk::mount_point(&mount).as_ref() != Some(&mount) { return None; }
                        let space = crate::disk::space(&mount)?;
                        let held = crate::disk::held(&mount).unwrap_or_else(|| space.total.saturating_sub(space.usable()));
                        if crate::disk::mount_point(&mount).as_ref() != Some(&mount) { return None; }
                        Some((mount, space, held))
                    }).collect();
                Some(VolumeMeasurements { mounts: mounts.all, spaces })
            }
            #[cfg(any(test, feature = "test-hooks"))]
            Candidates::Given(spaces) => Some(VolumeMeasurements { mounts: spaces.iter().map(|(mount, _, _)| mount.clone()).collect(), spaces }),
            #[cfg(any(test, feature = "test-hooks"))]
            Candidates::Probed { mounts, spaces } => Some(VolumeMeasurements { mounts: mounts?.into_iter().collect(), spaces }),
        }
    }

    /// Takes a sample of every other volume builds may write to with the tasks at work, by what the
    /// volume itself holds: an APFS volume shares its free space with the others of its container,
    /// such as the Mac's own disk, so what they write never counts as taken from it. One a
    /// stretch took `disk_peaks::LEARNED_BYTES` from becomes a volume builds write to: watched, its
    /// card raised when it runs low, and its room reserved like the Mac's own disk's. Returns
    /// whether a stretch ended or the confirmed set of mounts changed.
    pub(crate) fn measure_volumes(&self, working: &BTreeMap<TaskId, Option<String>>, now: Millis) -> bool {
        if self.record().peaks_ignored {
            return false;
        }
        let Some(VolumeMeasurements { mounts, spaces }) = self.candidate_spaces() else {
            self.record().volume_readings_fresh = false;
            return false;
        };
        let spaces: Vec<_> = spaces.into_iter().filter(|(mount, space, _)| mounts.contains(mount) && valid_disk(*space)).collect();
        let mut record = self.record();
        let changed = record.volume_mounts.as_ref() != Some(&mounts);
        record.volume_spaces.retain(|mount, _| mounts.contains(mount));
        record.volume_stretches.retain(|mount, _| mounts.contains(mount));
        record.volume_working.retain(|mount, _| mounts.contains(mount));
        record.volume_grants.retain(|mount, _| mounts.contains(mount));
        record.volume_readings_fresh = record.volumes.iter().chain(record.build_volumes.keys())
            .filter(|mount| mounts.contains(*mount)).all(|mount| spaces.iter().any(|(measured, _, _)| measured == mount));
        record.volume_mounts = Some(mounts);
        let mut ended = false;
        let mut learned = vec![];
        for (mount, space, held) in &spaces {
            record.volume_working.remove(mount);
            record.volume_grants.remove(mount);
            if *held > space.total { continue; }
            let Record { build_volumes, candidate_peaks, volume_stretches, .. } = &mut *record;
            let known = build_volumes.contains_key(mount);
            let estimates = match build_volumes.get_mut(mount) {
                Some(estimates) => estimates,
                None => candidate_peaks.entry(mount.clone()).or_default(),
            };
            let stretches = volume_stretches.entry(mount.clone()).or_default();
            ended |= stretches.sample(estimates, space.total.saturating_sub(*held), working, now);
            if !known && estimates.most().max(stretches.most()) >= crate::disk_peaks::LEARNED_BYTES {
                learned.push(mount.clone());
            }
        }
        record.volume_spaces.extend(spaces.into_iter().map(|(mount, space, _)| (mount, space)));
        for mount in &learned {
            log!("builds write to {}: Alondra watches it and reserves the room they take there", mount.display());
            let estimates = record.candidate_peaks.remove(mount).unwrap_or_default();
            record.build_volumes.insert(mount.clone(), estimates);
            record.volumes.insert(mount.clone());
        }
        if ended || !learned.is_empty() {
            self.save(&record);
        }
        ended || changed
    }

    /// Takes a sample of the usable space of the Mac's own disk with the tasks at work, by
    /// project; returns whether one stopped working or one whose start went unseen began, which
    /// frees what they reserved.
    pub(crate) fn measure_peaks(&self, usable: u64, working: &BTreeMap<TaskId, Option<String>>, now: Millis) -> bool {
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

    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn seed_peaks(&self, project: &str, peaks: &[u64]) {
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
        record.disk_working.entry(key.clone()).or_insert_with(|| project.clone());
        let mounts: Vec<_> = record.build_volumes.iter()
            .filter(|(mount, _)| record.volume_mounts.as_ref().is_none_or(|mounts| mounts.contains(*mount)))
            .map(|(mount, estimates)| (mount.clone(), estimates.estimate_beside(project.as_deref()))).collect();
        for (mount, cost) in mounts {
            let grant = record.volume_grants.entry(mount.clone()).or_default().entry(key.clone()).or_default();
            *grant = grant.saturating_add(cost);
            record.volume_working.entry(mount).or_default().entry(key.clone()).or_insert_with(|| project.clone());
        }
    }

    pub(crate) fn disk_reserved(&self, working: &BTreeMap<TaskId, Option<String>>) -> u64 {
        self.disk_reserved_for(working, None)
    }

    fn disk_reserved_for(&self, working: &BTreeMap<TaskId, Option<String>>, head: Option<&crate::admission::Key>) -> u64 {
        let record = self.record();
        if record.peaks_ignored {
            return 0;
        }
        let mut unobserved = record.disk_working.clone();
        for (task, project) in working {
            unobserved.entry(crate::admission::Key::Task(task.clone())).or_insert_with(|| project.clone());
        }
        let granted = record.disk_grants.values().copied().fold(0u64, u64::saturating_add);
        unobserved.iter().filter(|(key, _)| Some(*key) != head && !record.disk_grants.contains_key(*key)).map(|(key, project)| {
            let estimate = record.peaks.estimate(project.as_deref());
            match key {
                crate::admission::Key::Task(task) => record.stretches.reserved(task, estimate),
                _ => estimate,
            }
        }).fold(granted, u64::saturating_add)
    }

    /// The disk short of room for the next node, of `project`, on top of what the `working` tasks
    /// may still take: the Mac's own disk, or else a volume builds write to, named. Also while the
    /// disk's card is open, when no node starts. Once short, a disk needs margin to have room again.
    pub(crate) fn disk_room(&self, working: &BTreeMap<TaskId, Option<String>>, project: Option<&str>, now: Millis) -> Option<crate::disk::Room> {
        self.disk_room_for(working, project, None, now)
    }

    pub(crate) fn disk_room_for(&self, working: &BTreeMap<TaskId, Option<String>>, project: Option<&str>, head: Option<&crate::admission::Key>, now: Millis) -> Option<crate::disk::Room> {
        {
            let mut record = self.record();
            for (task, project) in working {
                record.disk_working.entry(crate::admission::Key::Task(task.clone())).or_insert_with(|| project.clone());
            }
        }
        let reserved = self.disk_reserved_for(working, head);
        let mut record = self.record();
        let estimate = if record.peaks_ignored { 0 } else { record.peaks.estimate(project) };
        let needed = reserved.saturating_add(estimate).saturating_add(crate::disk::low_lines().usable);
        let volume_short = record.volume_short(working, project, head);
        if record.boot_disk.is_none() {
            return Some(crate::disk::Room { free: 0, needed, volume: None });
        }
        let boot_short = record.boot_disk.and_then(|space| space.room_for(needed, record.boot_short).err());
        record.boot_short = boot_short.is_some();
        let short = record.disk.and_then(|space| space.room_for(needed, record.short).err());
        record.short = short.is_some();
        let swap_short = if record.peaks_ignored { None } else {
            record.swap.reserve(now).zip(record.boot_disk).and_then(|(swap_reserve, space)| {
                // Swap needs immediately writable space, even when macOS reports purgeable room.
                let needed = needed.saturating_add(swap_reserve);
                let margin = if record.swap_short { crate::disk::recovered_lines().usable - crate::disk::low_lines().usable } else { 0 };
                (space.free < needed.saturating_add(margin)).then_some(crate::disk::Room {
                    free: space.free, needed, volume: None,
                })
            })
        };
        record.swap_short = swap_short.is_some();
        let short = swap_short.or(boot_short).or(short);
        let short = short.or(volume_short);
        let carded = record.incident.as_ref().is_some_and(|incident| incident.cause == MachineCause::Disk);
        short.or_else(|| carded.then(|| crate::disk::Room {
            free: record.disk.map_or(0, |space| space.shown_free()),
            needed,
            volume: None,
        }))
    }

    /// Swap and boot-container budget beside the queue hold, for the throttled log.
    pub(crate) fn disk_detail(&self, working: &BTreeMap<TaskId, Option<String>>, project: Option<&str>, now: Millis) -> Option<String> {
        self.disk_detail_for(working, project, None, now)
    }

    pub(crate) fn disk_detail_for(&self, working: &BTreeMap<TaskId, Option<String>>, project: Option<&str>, head: Option<&crate::admission::Key>, now: Millis) -> Option<String> {
        let reserved = self.disk_reserved_for(working, head);
        let record = self.record();
        let space = record.boot_disk?;
        let swap = record.swap.line(now).unwrap_or_else(|| "swap unavailable".into());
        let builds = reserved.saturating_add(if record.peaks_ignored { 0 } else { record.peaks.estimate(project) });
        let remaining = i128::from(space.free) - i128::from(builds);
        Some(format!("{swap}; container free {:.2} GB, builds reserved {:.2} GB, free less builds {:.2} GB",
            space.free as f64 / 1_000_000_000.0, builds as f64 / 1_000_000_000.0, remaining as f64 / 1_000_000_000.0))
    }

    /// Whether the fullest disk fell under its floor.
    pub(crate) fn under_floor(&self) -> bool {
        self.record().disk.is_some_and(|space| space.under_floor())
    }

    fn short(&self) -> bool {
        let record = self.record();
        record.short || record.boot_short || record.swap_short
    }

    /// Why no node may start in `cwd` now, if the disk is nearly full there or on the Mac. The
    /// folder's volume is watched from now on. Any learned build volume without its own byte budget
    /// waits for the watcher even if admission was granted before the volume appeared.
    pub(crate) fn refuse_launch(&self, cwd: &Path) -> Option<String> {
        let mount = crate::disk::mount_point(cwd).filter(|mount| !crate::disk::same_volume(mount, own_disk()));
        self.refuse_launch_on(mount, self.space_where_it_works(cwd))
    }

    /// With test hooks a launch sees the space the watch measured, never the free space of the Mac running the tests.
    fn space_where_it_works(&self, cwd: &Path) -> Option<crate::disk::Space> {
        #[cfg(any(test, feature = "test-hooks"))]
        if let Some(measured) = self.record().boot_disk {
            return Some(measured);
        }
        crate::disk::space(cwd)
    }

    fn refuse_launch_on(&self, mount: Option<PathBuf>, space: Option<crate::disk::Space>) -> Option<String> {
        if let Some(mount) = mount {
            self.watch_mount(mount);
        }
        let unbudgeted = {
            let record = self.record();
            if record.peaks_ignored { None } else {
                record.build_volumes.keys().find(|mount| {
                    record.volume_mounts.as_ref().is_none_or(|mounts| mounts.contains(*mount))
                        && !record.volume_spaces.get(*mount).is_some_and(|space| valid_disk(*space))
                }).cloned()
            }
        };
        if let Some(mount) = unbudgeted {
            self.changed.notify_one();
            return Some(format!("Waiting for a disk space measurement on {}: Alondra starts no node until that volume has a valid reading", mount.display()));
        }
        let low_here = space.is_some_and(|space| space.low(false));
        if !self.disk_low() && !low_here {
            return None;
        }
        if low_here {
            self.changed.notify_one();
        }
        let free = space.filter(|_| low_here).map(|space| format!(" ({} free where it works)", alondra_core::machine_incident::bytes_text(if space.short_now() { space.free } else { space.usable() }, Lang::En))).unwrap_or_default();
        Some(format!("The disk is nearly full{free}: Alondra starts no node until there is space again, and {} has a card about it", crate::domain_messages::user_name()))
    }

    fn watch_mount(&self, mount: PathBuf) {
        let mut record = self.record();
        let watched = record.volumes.insert(mount.clone());
        let newly_present = record.volume_mounts.as_mut().is_some_and(|mounts| mounts.insert(mount));
        record.volume_readings_fresh = false;
        if watched {
            self.save(&record);
        }
        drop(record);
        if watched || newly_present {
            self.changed.notify_one();
        }
    }

    /// The free space of every watched volume still mounted, by name.
    fn volumes(&self) -> Vec<(PathBuf, crate::disk::Space)> {
        let (volumes, measured, mounted) = {
            let record = self.record();
            (record.volumes.clone(), record.volume_spaces.clone(), record.volume_mounts.clone())
        };
        volumes.iter().filter_map(|mount| {
            if mounted.as_ref().is_some_and(|mounted| !mounted.contains(mount)) { return None; }
            let space = measured.get(mount).copied()?;
            Some((mount.clone(), space))
        }).collect()
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

    #[cfg(any(test, feature = "test-hooks"))]
    fn set_incident(&self, incident: Option<Incident>) {
        self.set_incident_on(incident, None);
    }

    fn set_incident_on(&self, incident: Option<Incident>, volume: Option<PathBuf>) {
        let mut record = self.record();
        if record.incident != incident || record.incident_volume != volume {
            record.incident = incident;
            record.incident_volume = volume;
            self.save(&record);
        }
    }
}

/// The nodes still waiting: a node that took another turn since it declared, or that the user
/// closed, went on and is forgotten.
fn waiters(domain: &DomainRuntime) -> Vec<Waiter> {
    let watch = &domain.live.machine_watch;
    let state = domain.state().ok();
    let mut gone = vec![];
    let mut waiting = vec![];
    for (session, declared) in watch.declared() {
        let moved_on = domain.live.node(&session).is_some_and(|node| node.turn > declared.turn);
        if moved_on || domain.live.is_closed(&session) {
            gone.push(session);
            continue;
        }
        let name = state.as_ref().and_then(|state| {
            let agent = alondra_core::domain::AgentId(session.clone());
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
fn withdraw_strays(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    let current = domain.live.machine_watch.incident().filter(|incident| incident.cause != MachineCause::Declared).and_then(|incident| incident.card);
    for (id, card) in &state.interactions {
        let open = !card.withdrawn && matches!(card.payload, alondra_core::domain::InteractionPayload::Question { answer: None, .. });
        if open && card.author == alondra_core::domain::Author::System && card.system_card == Some(SystemCard::Machine) && current.as_deref() != Some(id.0.as_str()) {
            log!("withdrawing the stray machine card {}", id.0);
            let _ = domain.withdraw_problem(id);
        }
    }
}

/// The answer "I've fixed it" on the open card, taken once.
fn fixed(domain: &DomainRuntime, incident: Option<&Incident>) -> bool {
    use alondra_core::domain::{InteractionId, InteractionPayload};
    let Some(card) = incident.and_then(|incident| incident.card.as_ref()) else { return false };
    let Ok(state) = domain.state() else { return false };
    matches!(state.interactions.get(&InteractionId(card.clone())).map(|card| &card.payload),
        Some(InteractionPayload::Question { answer: Some(answer), .. }) if answer.option_id == MACHINE_FIXED_OPTION)
}

/// Carries out one step's actions.
async fn act(domain: &std::sync::Arc<DomainRuntime>, _alerts: &impl Alerts, actions: Vec<Action>) {
    for action in actions {
        match action {
            Action::Log { id, card } => {
                let (need, why) = card.text(Lang::En);
                log!("machine incident {id}: {need}. {why}; the admission gate waits for recovery");
            }
            Action::Withdraw(id) => {
                let withdrawing = domain.clone();
                let _ = tokio::task::spawn_blocking(move || withdrawing.withdraw_problem(&alondra_core::domain::InteractionId(id))).await;
            }
            Action::Release(sessions) => {
                log!("the machine recovered: {} node(s) that waited come back through the admission gate", sessions.len());
                let releasing = domain.clone();
                tokio::task::spawn_blocking(move || releasing.live.machine_watch.forget(&sessions)).await.expect("machine release worker failed");
            }
        }
    }
}

/// A new sample of the Mac: kept in the samples file, and when memory gains or loses room for one
/// more node, one line in the log and a look of the gate.
pub fn pace(domain: &DomainRuntime, machine: &Machine) {
    #[cfg(any(test, feature = "test-hooks"))]
    domain.live.machine_watch.sample_threads.lock().unwrap().push(std::thread::current().id());
    let watch = &domain.live.machine_watch;
    let now = if machine.at == 0 { now_millis() } else { machine.at };
    let Some(before) = watch.sample(machine, now) else { return };
    let mut seen = BTreeSet::new();
    let footprints = domain.node_groups_by_agent().into_iter().filter(|(_, group)| seen.insert(*group))
        .map(|(agent, group)| alondra_process_group::footprint(group).map(|bytes| (agent, bytes)))
        .collect::<std::io::Result<BTreeMap<_, _>>>();
    match footprints {
        Ok(footprints) => watch.measure_node_memory(now_millis(), footprints),
        Err(error) => watch.nodes_unavailable(&error),
    }
    let mac = watch.mac();
    let working = domain.state().map(|state| crate::admission::working_projects(&state)).unwrap_or_default();
    let usable = disk_space(machine).usable();
    let measured_disk = valid_disk(disk_space(machine));
    let freed = measured_disk && watch.measure_peaks(usable, &working, now);
    if watch.measure_volumes(&working, now) || freed {
        domain.live.gate.poke();
    }
    if let Some(home) = watch.home() {
        let reserving = domain.state().map(|state| crate::admission::reserving_tasks(&state, working.keys())).unwrap_or_default();
        let disk = measured_disk.then(|| (usable, watch.disk_reserved(&reserving)));
        crate::machine_state::record(&home.join(crate::machine_state::SAMPLES_DIR), now, &mac, working.len(), disk);
    }
    let working = working.len();
    let room = mac.room(0, now);
    if before.is_ok() == room.is_ok() { return; }
    let free = mac.free_percent.map_or_else(|| "unknown".into(), |free| format!("{free}%"));
    match room {
        Ok(()) => log!("memory has room for one more node again ({free} free, {working} node(s) working); {}; disk free {} bytes, purgeable {} bytes", mac.memory_detail(0), machine.disk_available, machine.disk_purgeable),
        Err(why) => log!("memory has no room for one more node: {why} ({free} free, {working} node(s) working); running nodes go on; {}; disk free {} bytes, purgeable {} bytes", mac.memory_detail(0), machine.disk_available, machine.disk_purgeable),
    }
    domain.live.gate.poke();
}

/// One look at a sample of the machine: the incident it opens, keeps or ends, and the actions that
/// follow. A sample that measures space on the disk takes over the executions that found it full.
pub(crate) fn look(domain: &DomainRuntime, machine: Machine) -> Vec<Action> {
    let watch = &domain.live.machine_watch;
    watch.record().volume_readings_fresh = false;
    pace(domain, &machine);
    let suspended = watch.suspend_unmounted();
    let measured_volumes = {
        let record = watch.record();
        record.peaks_ignored || (record.volumes.is_empty() && record.build_volumes.is_empty()) || record.volume_readings_fresh
    };
    let measured_boot_disk = valid_disk(disk_space(&machine)) && measured_volumes;
    let (machine, volume) = tightest_on(machine, &watch.volumes());
    let disk = volume.as_ref().map(|mount| mount.file_name().map_or_else(|| mount.display().to_string(), |name| name.to_string_lossy().into_owned()));
    if valid_disk(disk_space(&machine)) {
        watch.measure_fullest_disk_on(disk_space(&machine), volume.clone());
        if watch.short() {
            domain.live.gate.poke();
        }
        if watch.under_floor() {
            crate::admission::hold_for_disk(domain, now_millis());
        }
    }
    let previous = watch.incident();
    let fixed = fixed(domain, previous.as_ref());
    let waiting = waiters(domain);
    let now = now_millis();
    let incident_volume = if failing(&machine, previous.as_ref().map(|incident| incident.cause)) == Some(MachineCause::Disk)
        && (previous.is_none() || measured_boot_disk) {
        volume
    } else {
        watch.record().incident_volume.clone()
    };
    let (next, mut actions) = step_with_disk_measurement(previous, &machine, disk.as_deref(), &waiting, fixed, now, measured_boot_disk);
    let incident_volume = next.as_ref().filter(|incident| incident.cause == MachineCause::Disk).and(incident_volume);
    if let Some(suspended) = suspended {
        actions.extend(suspended.card.into_iter().map(Action::Withdraw));
        if !waiting.is_empty() {
            actions.push(Action::Release(waiting.iter().map(|waiter| waiter.session.clone()).collect()));
        }
    }
    let space = measured_boot_disk && has_space(&machine, next.as_ref());
    watch.set_incident_on(next, incident_volume);
    if fixed {
        domain.live.gate.release_declared(domain);
    }
    if space {
        crate::sdk_owner::retake_waiting_for_space(domain);
    }
    domain.live.gate.poke();
    #[cfg(any(test, feature = "test-hooks"))]
    domain.live.machine_watch.sampled.notify_one();
    actions
}

/// Whether the sample measured the disk with room, past the line it is low at, and no disk
/// incident stays open.
fn has_space(machine: &Machine, incident: Option<&Incident>) -> bool {
    valid_disk(disk_space(machine)) && failing(machine, None).is_none() && incident.is_none_or(|incident| incident.cause != MachineCause::Disk)
}

/// Looks at the machine on every sample and whenever a node declares a wait or the user answers.
pub async fn run(domain: std::sync::Arc<DomainRuntime>, gauges: crate::machine::Gauges, alerts: impl Alerts) {
    watch(domain, move || gauges.lock_or_recover("the machine gauges").as_ref().map(|(machine, _, _)| machine.clone()), alerts, TICK).await
}

async fn watch(domain: std::sync::Arc<DomainRuntime>, sample: impl Fn() -> Option<Machine>, alerts: impl Alerts, tick: Duration) {
    let starting = domain.clone();
    tokio::task::spawn_blocking(move || withdraw_strays(&starting)).await.expect("machine startup worker failed");
    let mut cards = domain.subscribe();
    loop {
        if let Some(machine) = sample() {
            let measuring = domain.clone();
            let actions = tokio::task::spawn_blocking(move || look(&measuring, machine)).await.expect("machine watch worker failed");
            act(&domain, &alerts, actions).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            _ = domain.live.machine_watch.changed.notified() => {}
            changed = cards.changed() => if changed.is_err() { return },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_push::testing::Node;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, InteractionPayload, PROTOCOL_VERSION};
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
        let watch = MachineWatch::persisted_in(&home);
        watch.seed_peaks("app", &[GB; 3]);
        for step in 0..=30 {
            let at = 10_000 + step * SAMPLE;
            let compressor = alondra_core::MemoryCompressor { swap_used: GB + step as u64 * 10_000_000,
                swap_total: Some(2 * GB), ..alondra_core::MemoryCompressor::default() };
            watch.sample(&Machine { at, compressor: Some(compressor), ..machine(7 * GB, 29) }, at);
        }
        assert!(watch.record().swap.reserve(10_000 + 5 * MIN).is_some());
        watch.save(&watch.record());
        let reopened = MachineWatch::persisted_in(&home);
        assert_eq!(reopened.record().swap.reserve(10_000 + 5 * MIN), None);
        assert_eq!(reopened.record().peaks.estimate(Some("app")), GB);
    }

    #[test]
    fn startup_admission_disk_budget_requires_valid_measurements() {
        let watch = MachineWatch::default();
        let room = || watch.disk_room(&BTreeMap::new(), None, 0);
        assert!(room().is_some(), "no disk measurement yet");
        for space in [
            crate::disk::Space { total: 0, free: 0, purgeable: 0 },
            crate::disk::Space { total: 0, free: 100 * GB, purgeable: 0 },
            crate::disk::Space { total: 50 * GB, free: 100 * GB, purgeable: 0 },
            crate::disk::Space { total: 50 * GB, free: GB, purgeable: u64::MAX },
        ] {
            watch.measure_disk(space);
            assert!(room().is_some(), "invalid disk reading: {space:?}");
        }
        watch.measure_disk(crate::disk::Space { total: 500 * GB, free: 100 * GB, purgeable: 0 });
        assert!(room().is_none(), "the first valid measurement opens the disk gate");
        watch.measure_disk(crate::disk::Space { total: 500 * GB, free: 0, purgeable: 0 });
        assert!(room().is_some(), "zero available is a measured shortage");
        watch.measure_disk(crate::disk::Space { total: 0, free: 100 * GB, purgeable: 0 });
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
        let measured = crate::disk::Space { total: 500 * GB, free: 7 * GB, purgeable: 40 * GB };
        watch.sample(&Machine { at: SAMPLE, disk_total: measured.total, disk_available: measured.free,
            disk_purgeable: measured.purgeable, ..Machine::default() }, SAMPLE);
        for at in [2 * SAMPLE, 3 * SAMPLE] {
            watch.sample(&Machine { at, ..Machine::default() }, at);
            assert_eq!(watch.record().boot_disk, Some(measured));
        }
        watch.sample(&Machine { at: 4 * SAMPLE, disk_total: GB, disk_available: 2 * GB, ..Machine::default() }, 4 * SAMPLE);
        assert_eq!(watch.record().boot_disk, Some(measured), "invalid readings cannot replace the boot budget");
    }

    #[test]
    fn startup_admission_a_healthy_external_disk_cannot_replace_the_system_disk_budget() {
        let (_home, domain) = world();
        let watch = &domain.live.machine_watch;
        let cache = PathBuf::from("/Volumes/BuildCache");
        watch.record().volumes.insert(cache.clone());
        watch.give_volumes(vec![(cache, space(30 * GB))]);
        let base = now_millis();
        let unknown = |at| Machine { at, disk_total: 0, disk_available: 0, ..machine(0, 60) };
        let _ = look(&domain, unknown(base));
        assert_eq!(watch.record().boot_disk, None);
        assert!(watch.disk_room(&BTreeMap::new(), None, base).is_some(), "a healthy external volume is not a system disk measurement");

        let _ = look(&domain, Machine { at: base + SAMPLE, ..machine(100 * GB, 60) });
        assert!(watch.disk_room(&BTreeMap::new(), None, base + SAMPLE).is_none());
        let _ = look(&domain, Machine { at: base + 2 * SAMPLE, ..machine(0, 60) });
        assert!(watch.disk_room(&BTreeMap::new(), None, base + 2 * SAMPLE).is_some());
        for at in [base + 3 * SAMPLE, base + 4 * SAMPLE] {
            let _ = look(&domain, unknown(at));
            assert_eq!(watch.incident().expect("unknown system disk cannot close its incident").healthy, 0);
            assert!(watch.disk_room(&BTreeMap::new(), None, at).is_some(), "a missing system probe cannot recover a low system disk");
        }
    }

    #[test]
    fn swap_uses_the_boot_container_even_when_another_volume_is_tightest() {
        let watch = MachineWatch::default();
        watch.seed_peaks("app", &[GB; 3]);
        for step in 0..=30 {
            let at = 10_000 + step * SAMPLE;
            let compressor = alondra_core::MemoryCompressor { swap_used: GB + step as u64 * 10_000_000,
                swap_total: Some(2 * GB), ..alondra_core::MemoryCompressor::default() };
            watch.sample(&Machine { at, compressor: Some(compressor), ..machine(20 * GB, 29) }, at);
        }
        watch.measure_fullest_disk(space(8 * GB));
        let now = 10_000 + 5 * MIN;
        assert_eq!(watch.disk_room(&BTreeMap::new(), Some("app"), now), None,
            "the 8 GB on another volume is not swap's free space");
        watch.record().boot_disk = Some(crate::disk::Space { free: 7 * GB, purgeable: 40 * GB, total: 500 * GB });
        assert!(watch.disk_room(&BTreeMap::new(), Some("app"), now).is_some(),
            "purgeable space does not pay swap's immediate writes");
    }

    /// The daemon restarts while a node builds: what each project took and what the node at work
    /// took so far are kept, so it reserves only what it may still take.
    #[test]
    fn what_each_project_takes_and_what_a_node_at_work_took_are_kept_across_a_restart() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let watch = MachineWatch::persisted_in(&home);
        watch.measure_peaks(100 * GB, &BTreeMap::new(), 0);
        for (run, task) in ["t1", "t2", "t3"].iter().enumerate() {
            let at = run as Millis * 10;
            watch.measure_peaks(100 * GB, &working(&[(task, "app")]), at + 1);
            watch.measure_peaks(88 * GB, &working(&[(task, "app")]), at + 2);
            assert!(watch.measure_peaks(100 * GB, &BTreeMap::new(), at + 3), "its stretch ended");
        }
        watch.measure_peaks(100 * GB, &working(&[("t4", "app")]), 50);
        watch.measure_peaks(91 * GB, &working(&[("t4", "app")]), 51);
        let reopened = MachineWatch::persisted_in(&home);
        reopened.measure_disk(crate::disk::Space { free: 15 * GB, purgeable: 0, total: 500 * GB });
        assert_eq!(reopened.disk_reserved(&working(&[("t4", "app")])), 3 * GB, "12 GB less the 9 it took before the restart");
        reopened.measure_peaks(89 * GB, &working(&[("t4", "app")]), 100);
        assert_eq!(reopened.disk_reserved(&working(&[("t4", "app")])), GB, "and the 2 it took since");
        assert_eq!(reopened.disk_room(&working(&[("t4", "app")]), Some("app"), 100),
            Some(crate::disk::Room { free: 15 * GB, needed: GB + 12 * GB + 5 * GB, volume: None }), "t4's last GB and the next one's 12 on top of the 5 GB line");
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
        let watch = MachineWatch::persisted_in(&home);
        let free = 26_800_000_000;
        watch.measure_disk(crate::disk::Space { free, purgeable: 0, total: 494 * GB });
        let at_work: Vec<(String, &str)> = (1..=7).map(|index| (format!("t{index}"), "alondra")).collect();
        let at_work: Vec<(&str, &str)> = at_work.iter().map(|(task, project)| (task.as_str(), *project)).collect();
        let at_work = working(&at_work);
        assert!(watch.measure_peaks(free, &at_work, 0), "the gate looks again once their stretches began");
        assert_eq!(watch.disk_reserved(&at_work), 7 * GB);
        assert_eq!(watch.disk_room(&at_work, Some("alondra"), 0), None, "7 GB for the nodes at work and 8 GB for the next start fit over the 5 GB line");
    }

    fn space(free: u64) -> crate::disk::Space {
        crate::disk::Space { free, purgeable: 0, total: 37 * GB }
    }

    #[test]
    fn learned_volume_probe_failure_keeps_its_restriction() {
        let watch = MachineWatch::default();
        let mount = PathBuf::from("/Volumes/BuildCache");
        watch.measure_disk(crate::disk::Space { free: 400 * GB, purgeable: 0, total: 500 * GB });
        watch.record().build_volumes.entry(mount.clone()).or_default();
        watch.give_volumes(vec![(mount.clone(), space(GB))]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        let before = watch.disk_room(&BTreeMap::new(), None, 0).expect("the learned volume is short");
        watch.record().candidates = Candidates::Probed { mounts: Some(vec![mount]), spaces: vec![] };
        watch.measure_volumes(&BTreeMap::new(), 1);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 1), Some(before), "a mounted volume without a reading has not recovered");
    }

    #[test]
    fn startup_admission_missing_disk_cannot_cancel_unobserved_writes() {
        let watch = MachineWatch::default();
        let disk = |free| crate::disk::Space { free, purgeable: 0, total: 500 * GB };
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

    fn learned_watch(mount: &Path) -> MachineWatch {
        let watch = MachineWatch::default();
        watch.measure_disk(crate::disk::Space { free: 400 * GB, purgeable: 0, total: 500 * GB });
        let mut record = watch.record();
        record.build_volumes.entry(mount.to_owned()).or_default();
        record.volumes.insert(mount.to_owned());
        drop(record);
        watch
    }

    #[test]
    fn learned_volume_invalid_readings_preserve_budget_hysteresis_and_reserves() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        let working = working(&[("t1", "app")]);
        watch.give_volumes(vec![(mount.clone(), space(GB))]);
        watch.measure_volumes(&working, 0);
        let before = watch.disk_room(&working, None, 0).unwrap();
        let reserved = watch.record().volume_stretches[&mount].reserved(&TaskId::from("t1"), GB);
        for (at, reading) in [
            None,
            Some(crate::disk::Space { free: 30 * GB, total: 0, purgeable: 0 }),
            Some(crate::disk::Space { free: 30 * GB, total: GB, purgeable: 0 }),
            Some(crate::disk::Space { free: u64::MAX, total: u64::MAX, purgeable: 1 }),
        ].into_iter().enumerate() {
            watch.record().candidates = Candidates::Probed {
                mounts: Some(vec![mount.clone()]),
                spaces: reading.into_iter().map(|space| (mount.clone(), space, 0)).collect(),
            };
            watch.measure_volumes(&BTreeMap::new(), at as Millis + 1);
            assert_eq!(watch.disk_room(&working, None, at as Millis + 1), Some(before.clone()));
            assert_eq!(watch.record().volume_stretches[&mount].reserved(&TaskId::from("t1"), GB), reserved, "failed probes cannot end a stretch or credit consumption");
        }
        watch.record().candidates = Candidates::Probed { mounts: None, spaces: vec![] };
        watch.measure_volumes(&BTreeMap::new(), 5);
        assert_eq!(watch.disk_room(&working, None, 5), Some(before));
        watch.give_volumes(vec![(mount.clone(), space(0))]);
        watch.measure_volumes(&working, 6);
        assert_eq!(watch.record().volume_spaces[&mount].free, 0, "zero available is a real shortage");
        assert!(watch.disk_room(&working, None, 6).is_some());
    }

    #[test]
    fn learned_volume_ended_tasks_cannot_cancel_unobserved_disk_reserves() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        let running = working(&[("t1", "app"), ("t2", "app"), ("t3", "app")]);
        watch.give_volumes(vec![(mount.clone(), space(8 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        let before = watch.disk_room(&running, None, 0).expect("the three pending writes need 9 GB");
        assert_eq!(before.needed, 9 * GB);
        watch.record().candidates = Candidates::Probed { mounts: Some(vec![mount.clone()]), spaces: vec![] };
        watch.measure_volumes(&BTreeMap::new(), 1);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 1), Some(before), "ended tasks may have consumed their reserves after the cached budget");
        watch.give_volumes(vec![(mount, space(8 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 2);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 2), None, "fresh bytes include completed writes and can release the reserves");
    }

    #[test]
    fn learned_volume_failed_probe_keeps_reservations_seen_between_budgets() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        watch.give_volumes(vec![(mount, space(8 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        watch.record().candidates = Candidates::Probed { mounts: None, spaces: vec![] };
        let reserving = working(&[("t1", "app"), ("t2", "app"), ("t3", "app")]);
        watch.measure_volumes(&reserving, 1);
        assert_eq!(watch.disk_room(&reserving, None, 1).unwrap().needed, 9 * GB);
        watch.measure_volumes(&BTreeMap::new(), 2);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 2).unwrap().needed, 9 * GB, "reservations against the old budget remain after their tasks end");
    }

    #[test]
    fn learned_volume_head_excludes_forecasts_but_not_earlier_grants() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        watch.give_volumes(vec![(mount, space(6 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        let head = crate::admission::Key::Task("t1".into());
        let working = BTreeMap::from([("t1".into(), None)]);
        assert_eq!(watch.disk_room_for(&working, None, Some(&head), 0), None);
        watch.reserve_admission(head.clone(), None);
        assert_eq!(watch.disk_room_for(&BTreeMap::new(), None, Some(&head), 0).unwrap().needed, 7 * GB, "a new grant cannot erase the earlier grant's possible writes");
        let watch = MachineWatch::default();
        watch.measure_disk(crate::disk::Space { total: 500 * GB, free: 16 * GB, purgeable: 0 });
        assert_eq!(watch.disk_room_for(&working, None, Some(&head), 0), None);
        watch.reserve_admission(head.clone(), None);
        assert_eq!(watch.disk_room_for(&BTreeMap::new(), None, Some(&head), 0).unwrap().needed, 21 * GB);
        assert_eq!(watch.disk_reserved(&BTreeMap::new()), 8 * GB);
    }

    #[test]
    fn learned_volume_new_grants_cannot_reuse_older_consumption_credit() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        let working = working(&[("t1", "app")]);
        watch.measure_peaks(100 * GB, &working, 0);
        watch.measure_peaks(92 * GB, &working, 1);
        watch.give_volumes_holding(vec![(mount.clone(), space(10 * GB), 27 * GB)]);
        watch.measure_volumes(&working, 0);
        watch.give_volumes_holding(vec![(mount.clone(), space(8 * GB), 29 * GB)]);
        watch.measure_volumes(&working, 1);
        assert_eq!(watch.disk_used(&"t1".into()), 8 * GB);
        let head = crate::admission::Key::Task("t1".into());
        for _ in 0..3 { watch.reserve_admission(head.clone(), None); }
        assert_eq!(watch.disk_reserved(&BTreeMap::new()), 24 * GB, "each grant reserves a full build despite previous ledger credit");
        let held = watch.disk_room_for(&BTreeMap::new(), None, Some(&head), 1).unwrap();
        assert_eq!(held.volume.as_deref(), Some("BuildCache"));
        assert_eq!(held.needed, 9 * GB);
    }

    #[test]
    fn learned_volume_reserves_survive_a_system_disk_hold() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        watch.give_volumes(vec![(mount.clone(), space(8 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        watch.measure_disk(crate::disk::Space { free: 36 * GB, purgeable: 0, total: 500 * GB });
        let running = working(&[("t1", "app"), ("t2", "app"), ("t3", "app")]);
        let held = watch.disk_room(&running, None, 0).expect("system disk blocks first");
        assert_eq!(held.volume, None);
        watch.record().candidates = Candidates::Probed { mounts: Some(vec![mount]), spaces: vec![] };
        watch.measure_volumes(&BTreeMap::new(), 1);
        watch.measure_disk(crate::disk::Space { free: 400 * GB, purgeable: 0, total: 500 * GB });
        let held = watch.disk_room(&BTreeMap::new(), None, 1).expect("fresh system bytes cannot release unobserved external writes");
        assert_eq!(held.volume.as_deref(), Some("BuildCache"));
        assert_eq!(held.needed, 9 * GB);
    }

    #[test]
    fn fresh_disk_budgets_do_not_reserve_held_turns_against_their_return() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        let held = working(&[("t1", "app"), ("t2", "app"), ("t3", "app")]);
        // The ledger still observes these turns, but admission excludes their future builds.
        watch.give_volumes(vec![(mount, space(8 * GB))]);
        watch.measure_volumes(&held, 0);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 0), None);
        watch.measure_disk(crate::disk::Space { free: 36 * GB, purgeable: 0, total: 500 * GB });
        watch.measure_peaks(36 * GB, &held, 1);
        assert_eq!(watch.disk_reserved(&BTreeMap::new()), 0);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 1), None, "a fresh budget counts only the next build when returning held turns are excluded");
    }

    #[test]
    fn learned_volume_confirmed_unmount_suspends_the_hold_and_remount_needs_a_budget() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        watch.give_volumes(vec![(mount.clone(), space(GB))]);
        watch.measure_volumes(&working(&[("t1", "app")]), 0);
        assert!(watch.disk_room(&BTreeMap::new(), None, 0).is_some());
        watch.give_volumes(vec![]);
        watch.measure_volumes(&BTreeMap::new(), 1);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 1), None);
        assert!(watch.record().build_volumes.contains_key(&mount));
        assert!(!watch.record().volume_stretches.contains_key(&mount));
        assert!(watch.volumes().is_empty(), "an unmounted directory cannot supply its parent's disk budget");
        watch.record().candidates = Candidates::Probed { mounts: Some(vec![mount.clone()]), spaces: vec![] };
        watch.measure_volumes(&BTreeMap::new(), 2);
        assert!(watch.disk_room(&BTreeMap::new(), None, 2).is_some(), "a remounted volume needs its own reading");
        watch.give_volumes(vec![(mount.clone(), space(7 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 3);
        assert!(watch.disk_room(&BTreeMap::new(), None, 3).is_some(), "a known shortage still needs its recovery margin");
        watch.give_volumes(vec![(mount, space(8 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 4);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 4), None);
    }

    #[test]
    fn learned_volume_positive_remount_supersedes_an_older_absence() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        watch.give_volumes(vec![(mount.clone(), space(GB))]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        assert!(watch.disk_room(&BTreeMap::new(), None, 0).is_some());
        watch.give_volumes(vec![]);
        watch.measure_volumes(&BTreeMap::new(), 1);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 1), None);
        watch.record().candidates = Candidates::Probed { mounts: None, spaces: vec![] };
        watch.measure_volumes(&BTreeMap::new(), 2);
        watch.record().volume_readings_fresh = true;
        watch.watch_mount(mount.clone());
        let room = watch.disk_room(&BTreeMap::new(), None, 2).expect("positive mount evidence supersedes an older absence even when enumeration fails");
        assert_eq!(room.volume.as_deref(), Some("BuildCache"));
        assert!(!watch.record().volume_readings_fresh);
        watch.give_volumes(vec![(mount.clone(), space(7 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 3);
        assert!(watch.disk_room(&BTreeMap::new(), None, 3).is_some());
        watch.give_volumes(vec![(mount.clone(), space(8 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 4);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 4), None);
        watch.record().volume_mounts = None;
        watch.watch_mount(mount);
        assert_eq!(watch.record().volume_mounts, None, "one positive mount does not establish a complete mount table");
    }

    #[test]
    fn learned_volume_launch_preflight_cannot_use_admission_from_before_remount() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        watch.give_volumes(vec![]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 0), None,
            "admission before the volume reappears has no external budget to reserve");
        assert_eq!(watch.refuse_launch_on(None, None), None,
            "a confirmed absence does not block a launch on the system disk");
        for reading in [None, Some(space(30 * GB)),
            Some(crate::disk::Space { total: 0, free: 30 * GB, purgeable: 0 }),
            Some(crate::disk::Space { total: GB, free: 30 * GB, purgeable: 0 }),
            Some(crate::disk::Space { total: u64::MAX, free: u64::MAX, purgeable: 1 })] {
            assert!(watch.refuse_launch_on(Some(mount.clone()), reading).is_some(),
                "preflight must wait for the watcher's own coherent budget, even with a recent admission");
            assert!(watch.refuse_launch_on(None, None).is_some(),
                "an unknown build-volume budget also blocks a launch whose cwd is on the system disk");
        }
        watch.give_volumes(vec![(mount.clone(), space(30 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 1);
        assert_eq!(watch.refuse_launch_on(Some(mount), Some(space(30 * GB))), None);
        let unlearned = MachineWatch::default();
        assert_eq!(unlearned.refuse_launch_on(Some(PathBuf::from("/Volumes/Other")), None), None,
            "watching a new volume does not invent build usage there");
    }

    #[test]
    fn learned_volume_unknown_budget_waits_but_does_not_invent_a_shortage_margin() {
        let mount = PathBuf::from("/Volumes/BuildCache");
        let watch = learned_watch(&mount);
        assert!(watch.disk_room(&BTreeMap::new(), None, 0).is_some(), "persisted learning without readings must wait");
        watch.record().candidates = Candidates::Probed { mounts: Some(vec![mount.clone()]), spaces: vec![] };
        watch.measure_volumes(&BTreeMap::new(), 1);
        assert!(watch.disk_room(&BTreeMap::new(), None, 1).is_some());
        watch.give_volumes(vec![(mount, space(6 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 2);
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 2), None, "the first healthy budget is enough when no shortage was measured");
    }

    #[test]
    fn learned_volume_reopened_without_readings_keeps_admission_closed() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let mount = home.join("BuildCache");
        let watch = MachineWatch::persisted_in(&home);
        {
            let mut record = watch.record();
            record.build_volumes.entry(mount.clone()).or_default();
            record.volumes.insert(mount.clone());
            record.volume_mounts = Some(BTreeSet::new());
            watch.save(&record);
        }
        drop(watch);
        let reopened = MachineWatch::persisted_in(&home);
        reopened.measure_disk(crate::disk::Space { free: 400 * GB, purgeable: 0, total: 500 * GB });
        assert!(reopened.record().build_volumes.contains_key(&mount));
        assert_eq!(reopened.record().volume_mounts, None, "a previous unmount is not a new mount-table reading");
        reopened.record().candidates = Candidates::Probed { mounts: Some(vec![mount.clone()]), spaces: vec![] };
        reopened.measure_volumes(&BTreeMap::new(), 0);
        assert_eq!(reopened.disk_room(&BTreeMap::new(), None, 0).unwrap().volume.as_deref(), Some("BuildCache"));
        reopened.give_volumes(vec![(mount, space(6 * GB))]);
        reopened.measure_volumes(&BTreeMap::new(), 1);
        assert_eq!(reopened.disk_room(&BTreeMap::new(), None, 1), None);
    }

    #[test]
    fn learned_volume_unmount_retires_its_incident_without_a_new_system_probe() {
        for (boot, other, held) in [(Some(100 * GB), false, false), (None, false, true), (Some(12 * GB), false, true), (Some(100 * GB), true, true)] {
            let (home, domain) = world();
            let mount = home.0.join("BuildCache");
            let other_mount = home.0.join("OtherCache");
            let watch = &domain.live.machine_watch;
            watch.record().build_volumes.entry(mount.clone()).or_default();
            watch.record().volumes.insert(mount.clone());
            watch.give_volumes(vec![(mount.clone(), space(GB))]);
            let reading = |at, free: Option<u64>| Machine { at, disk_total: if free.is_some() { 500 * GB } else { 0 }, disk_available: free.unwrap_or(0), ..machine(0, 60) };
            let _ = look(&domain, reading(SAMPLE, boot));
            assert_eq!(watch.record().incident_volume.as_ref(), Some(&mount));
            assert!(watch.disk_room(&BTreeMap::new(), None, SAMPLE).is_some());
            if boot == Some(12 * GB) {
                let _ = look(&domain, reading(2 * SAMPLE, Some(14 * GB)));
                assert!(watch.disk_room(&BTreeMap::new(), None, 2 * SAMPLE).is_some());
            }
            if other {
                watch.record().build_volumes.entry(other_mount.clone()).or_default();
                watch.record().volumes.insert(other_mount.clone());
            }
            watch.give_volumes(if other { vec![(other_mount, space(GB))] } else { vec![] });
            let actions = look(&domain, reading(3 * SAMPLE, None));
            assert!(actions.iter().any(|action| matches!(action, Action::Withdraw(_))), "confirmed absence retires only the absent volume's incident");
            assert_eq!(watch.disk_room(&BTreeMap::new(), None, 3 * SAMPLE).is_some(), held, "own budget and other volumes retain their independent restrictions");
            if !other { assert!(watch.incident().is_none()); }
            if boot == Some(12 * GB) {
                assert!(watch.record().boot_short, "unmount cannot clear the system disk's recovery margin");
            }
        }
    }

    #[test]
    fn learned_volume_incident_origin_survives_reopen_without_guessing_old_records() {
        let mut record = Record::default();
        record.incident = step(None, &machine(GB, 60), &[], false, 0).0;
        record.incident_volume = Some(PathBuf::from("/Volumes/BuildCache"));
        let saved = serde_json::to_value(&record).unwrap();
        for has_origin in [true, false] {
            let mut saved = saved.clone();
            if !has_origin { saved.as_object_mut().unwrap().remove("incident_volume"); }
            let watch = MachineWatch { record: Mutex::new(serde_json::from_value(saved).unwrap()), ..MachineWatch::default() };
            watch.give_volumes(vec![]);
            watch.measure_volumes(&BTreeMap::new(), 1);
            assert_eq!(watch.suspend_unmounted().is_some(), has_origin, "only a recorded origin can identify the absent incident");
            assert!(watch.disk_room(&BTreeMap::new(), None, 1).is_some(), "reopen still needs its first system budget");
        }
    }

    #[test]
    fn learned_volume_cached_space_cannot_complete_incident_recovery() {
        let (home, domain) = world();
        let mount = home.0.join("BuildCache");
        let watch = &domain.live.machine_watch;
        watch.record().build_volumes.entry(mount.clone()).or_default();
        watch.record().volumes.insert(mount.clone());
        watch.declare("waiting-node", None, "disk full", 0, 0);
        watch.give_volumes(vec![(mount.clone(), space(GB))]);
        let _ = look(&domain, Machine { at: SAMPLE, ..machine(100 * GB, 60) });
        assert_eq!(watch.incident().unwrap().healthy, 0);
        watch.give_volumes(vec![(mount.clone(), space(30 * GB))]);
        let _ = look(&domain, Machine { at: 2 * SAMPLE, ..machine(100 * GB, 60) });
        assert_eq!(watch.incident().unwrap().healthy, 1);
        for (index, mounts) in [Some(vec![mount.clone()]), None].into_iter().enumerate() {
            watch.record().candidates = Candidates::Probed { mounts, spaces: vec![] };
            let actions = look(&domain, Machine { at: (index as Millis + 3) * SAMPLE, ..machine(100 * GB, 60) });
            assert_eq!(watch.incident().expect("failed readings cannot recover the incident").healthy, 0);
            assert!(!actions.iter().any(|action| matches!(action, Action::Release(_))));
            assert!(watch.disk_room(&BTreeMap::new(), None, 0).is_some());
        }
        watch.give_volumes(vec![(mount, space(30 * GB))]);
        let _ = look(&domain, Machine { at: 5 * SAMPLE, ..machine(100 * GB, 60) });
        assert_eq!(watch.incident().unwrap().healthy, 1);
        let actions = look(&domain, Machine { at: 6 * SAMPLE, ..machine(100 * GB, 60) });
        assert!(watch.incident().is_none());
        assert!(actions.iter().any(|action| matches!(action, Action::Release(_))));
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, 0), None);
    }

    /// 30 October: /Volumes/BuildCache filled while the Mac's own disk had 19 GB. A volume builds
    /// take space from while nodes work becomes one Alondra watches and reserves room on; one that
    /// fills while no node works, such as a disk of photos, does not. Both stay as they were after
    /// a restart.
    #[test]
    fn a_volume_builds_take_space_from_is_learned_reserved_and_kept_and_one_filled_by_hand_is_not() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let watch = MachineWatch::persisted_in(&home);
        let (cache, photos) = (PathBuf::from("/Volumes/BuildCache"), PathBuf::from("/Volumes/Fotos"));
        let give = |cache_free: u64, photos_free: u64| watch.give_volumes(vec![(cache.clone(), space(cache_free)), (photos.clone(), space(photos_free))]);
        let t1 = working(&[("t1", "app")]);
        give(30 * GB, 30 * GB);
        watch.measure_volumes(&BTreeMap::new(), 0);
        give(30 * GB, 10 * GB);
        watch.measure_volumes(&BTreeMap::new(), 1);
        watch.measure_volumes(&t1, 2);
        give(29_500_000_000, 10 * GB);
        watch.measure_volumes(&t1, 3);
        assert!(watch.record().build_volumes.is_empty(), "half a GB does not make it one builds write to");
        give(28 * GB, 10 * GB);
        watch.measure_volumes(&t1, 4);
        assert_eq!(watch.record().build_volumes.keys().collect::<Vec<_>>(), [&cache], "2 GB taken while t1 worked; 20 GB taken from Fotos while none did");
        assert!(watch.record().volumes.contains(&cache), "watched from now on");

        watch.measure_disk(crate::disk::Space { free: 400 * GB, purgeable: 0, total: 500 * GB });
        give(7 * GB, 10 * GB);
        watch.measure_volumes(&t1, 5);
        assert_eq!(watch.disk_room(&t1, Some("web"), 0), None, "t1 already took its estimate there: 1 GB for the next one fits over the 5 GB line");
        give(5_500_000_000, 10 * GB);
        watch.measure_volumes(&t1, 6);
        assert_eq!(watch.disk_room(&t1, Some("web"), 0), Some(crate::disk::Room { free: 5_500_000_000, needed: 6 * GB, volume: Some("BuildCache".into()) }),
            "the Mac's own disk has room, BuildCache does not, and is named");
        watch.measure_volumes(&BTreeMap::new(), 7);

        let reopened = MachineWatch::persisted_in(&home);
        assert_eq!(reopened.record().build_volumes.keys().collect::<Vec<_>>(), [&cache]);
        assert_eq!(reopened.record().build_volumes[&cache].most(), 24_500_000_000, "t1's peak there, the most the volume lost while it worked");
    }

    /// /Volumes/BuildCache is an APFS volume of the Mac's own container: its free space is the
    /// container's, so a node writing to the Mac's own disk lowers it too. It is learned only by
    /// what it holds itself.
    #[test]
    fn a_volume_sharing_the_macs_free_space_is_learned_by_what_it_holds() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let watch = MachineWatch::persisted_in(&home);
        let cache = PathBuf::from("/Volumes/BuildCache");
        let t1 = working(&[("t1", "app")]);
        let give = |free: u64, held: u64| watch.give_volumes_holding(vec![(cache.clone(), crate::disk::Space { free, purgeable: 0, total: 40 * GB }, held)]);
        give(30 * GB, 10 * GB);
        watch.measure_volumes(&BTreeMap::new(), 0);
        watch.measure_volumes(&t1, 1);
        give(20 * GB, 10 * GB);
        watch.measure_volumes(&t1, 2);
        assert!(watch.record().build_volumes.is_empty(), "t1 took 10 GB from the Mac's own disk, none from BuildCache");
        give(17 * GB, 13 * GB);
        watch.measure_volumes(&t1, 3);
        assert_eq!(watch.record().build_volumes.keys().collect::<Vec<_>>(), [&cache], "t1 wrote 3 GB into BuildCache");
        watch.measure_volumes(&BTreeMap::new(), 4);
        assert_eq!(watch.record().build_volumes[&cache].most(), 3 * GB, "its peak there is what it wrote there");
    }

    /// A volume builds write to running low opens the disk's card naming it, while the Mac's own
    /// disk is fine, and a node waiting for the machine is not told it may go on.
    #[test]
    fn a_volume_builds_write_to_running_low_opens_the_card_and_keeps_a_wait_for_the_machine() {
        let (_home, domain) = world();
        let watch = &domain.live.machine_watch;
        let cache = PathBuf::from("/Volumes/BuildCache");
        let t1 = working(&[("t1", "app")]);
        watch.give_volumes(vec![(cache.clone(), space(30 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        watch.measure_volumes(&t1, 1);
        watch.give_volumes(vec![(cache.clone(), space(659_000_000))]);
        watch.measure_volumes(&t1, 2);
        watch.declare("agent-t1", None, "the build ran out of disk", 0, 3);

        let actions = look(&domain, machine(19 * GB, 50));
        let incident = watch.incident().expect("the volume builds write to is nearly full");
        assert_eq!(incident.cause, MachineCause::Disk);
        assert_eq!(raised(&actions)[0].text(Lang::Es).0, "Libera espacio en disco: quedan 659 MB en BuildCache");
        assert!(!actions.iter().any(|action| matches!(action, Action::Release(_))), "the node waiting for the machine is not let go: {actions:?}");
        assert!(watch.disk_low());
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
        watch.measure_disk(crate::disk::Space { free: 8 * GB, purgeable: 0, total: 500 * GB });
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
    fn a_volume_nodes_work_on_running_low_opens_the_card_and_names_it_while_the_macs_own_disk_is_fine() {
        let external = [("Externo".to_owned(), crate::disk::Space { free: 3 * GB, purgeable: 0, total: 900 * GB }),
            ("Fotos".to_owned(), crate::disk::Space { free: 400 * GB, purgeable: 0, total: 900 * GB })];
        let (measured, disk) = tightest(machine(80 * GB, 50), &external);
        assert_eq!((measured.disk_available, disk.as_deref()), (3 * GB, Some("Externo")));
        let (incident, actions) = step_on(None, &measured, disk.as_deref(), &[], false, 0);
        assert_eq!(incident.unwrap().cause, MachineCause::Disk);
        assert_eq!(raised(&actions)[0].text(Lang::Es).0, "Libera espacio en disco: solo quedan 3,0 GB en Externo");

        let (measured, disk) = tightest(machine(2 * GB, 50), &external);
        assert_eq!((measured.disk_available, disk), (2 * GB, None), "the Mac's own disk is the fuller one: not named");
        assert_eq!(tightest(machine(80 * GB, 50), &[]).1, None);
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
    fn the_fullest_volume_counts_purgeable_space_but_not_purgeable_space_macos_has_not_freed() {
        let space = |free: u64, purgeable: u64| crate::disk::Space { free, purgeable, total: 900 * GB };
        let snapshots = [("Externo".to_owned(), space(3 * GB, 40 * GB))];
        assert_eq!(tightest(machine(20 * GB, 50), &snapshots).1, None, "43 GB usable on Externo beats 20 GB on the Mac");
        let unpurged = [("Externo".to_owned(), space(GB, 40 * GB))];
        let (measured, disk) = tightest(machine(20 * GB, 50), &unpurged);
        assert_eq!((measured.disk_available, measured.disk_purgeable, disk.as_deref()), (GB, 40 * GB, Some("Externo")));
    }

    #[test]
    fn no_node_starts_while_the_disk_is_low_and_one_does_once_it_recovers() {
        let watch = MachineWatch::default();
        let here = crate::test_support::Directory::new();
        std::fs::create_dir(&here).unwrap();
        watch.measure_disk(crate::disk::Space { total: 500 * GB, free: 400 * GB, purgeable: 0 });
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
        let here = crate::test_support::Directory::new();
        std::fs::create_dir(&here).unwrap();
        watch.measure_disk(crate::disk::Space { total: 500 * GB, free: GB, purgeable: 0 });
        assert!(watch.refuse_launch(&here).is_some_and(|why| why.contains("nearly full")), "a measured full disk refuses the launch");
        watch.measure_disk(crate::disk::Space { total: 500 * GB, free: 400 * GB, purgeable: 0 });
        assert_eq!(watch.refuse_launch(&here), None, "a measured roomy disk starts it");
    }

    #[test]
    fn a_launch_on_another_volume_watches_it_from_then_on() {
        let watch = MachineWatch::default();
        let here = crate::test_support::Directory::new();
        std::fs::create_dir(&here).unwrap();
        watch.refuse_launch(&here);
        let mount = crate::disk::mount_point(&here).unwrap();
        let watched = watch.record.lock().unwrap().volumes.clone();
        if crate::disk::same_volume(&mount, own_disk()) {
            assert!(watched.is_empty(), "the Mac's own disk already has its gauge: {watched:?}");
        } else {
            assert_eq!(watched.into_iter().collect::<Vec<_>>(), [mount]);
            assert_eq!(watch.volumes().len(), 1);
        }
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

    fn world() -> (Home, Arc<DomainRuntime>) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        (Home(home), domain)
    }

    struct Told(Mutex<Vec<String>>);
    impl Alerts for Arc<Told> {
        async fn tell(&self, text: impl Fn(bool) -> String + Send + Sync, _passive: bool) {
            self.0.lock().unwrap().push(text(true));
        }
    }

    fn open_machine_cards(domain: &DomainRuntime) -> Vec<alondra_core::domain::Interaction> {
        let state = domain.state().unwrap();
        state.interactions.values().filter(|card| card.system_card == Some(SystemCard::Machine) && !card.withdrawn
            && matches!(card.payload, InteractionPayload::Question { answer: None, .. })).cloned().collect()
    }

    fn orchestrator_inbox(domain: &DomainRuntime) -> Vec<String> {
        let state = domain.state().unwrap();
        domain.store.lock().unwrap().pending_messages().unwrap().iter()
            .filter(|message| message.recipient_kind == "orchestrator")
            .map(|message| crate::domain_transport::render(&state, message)).collect()
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

    fn watching(domain: &Arc<DomainRuntime>, clock: &Clock, machine: impl Fn(Millis) -> Machine + Send + 'static, told: &Arc<Told>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(watch(domain.clone(), clock.source(machine), told.clone(), Duration::from_millis(1)))
    }

    #[tokio::test]
    async fn a_full_disk_records_pressure_and_recovers_without_cards_or_pushes() {
        let (_home, domain) = world();
        domain.live.machine_watch.declare("node", None, "disk full", 0, now_millis());
        let disk = Arc::new(Mutex::new(0u64));
        let reading = disk.clone();
        let told = Arc::new(Told(Mutex::new(vec![])));
        let clock = Clock::default();
        let watcher = watching(&domain, &clock, move |at| Machine { at, ..machine(*reading.lock().unwrap(), 50) }, &told);
        clock.samples(1).await;
        assert!(domain.live.machine_watch.incident().is_some());
        assert!(open_machine_cards(&domain).is_empty());
        clock.samples(5).await;
        assert!(open_machine_cards(&domain).is_empty());
        assert!(told.0.lock().unwrap().is_empty());
        assert!(orchestrator_inbox(&domain).is_empty());
        *disk.lock().unwrap() = 50 * GB;
        clock.samples(HEALTHY_SAMPLES as Millis).await;
        watcher.abort();
        assert!(domain.live.machine_watch.incident().is_none());
        assert!(domain.live.machine_watch.declared().is_empty());
        assert!(open_machine_cards(&domain).is_empty());
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_cpu_load_opens_no_incident_no_card_and_no_notice() {
        let (_home, domain) = world();
        let told = Arc::new(Told(Mutex::new(vec![])));
        let clock = Clock::default();
        let watcher = watching(&domain, &clock, |at| Machine { at, load: [90.0; 3], unreliable: true, ..machine(50 * GB, 50) }, &told);
        clock.samples(10).await;
        watcher.abort();
        assert!(domain.live.machine_watch.incident().is_none());
        assert!(open_machine_cards(&domain).is_empty());
        assert!(orchestrator_inbox(&domain).is_empty(), "{:?}", orchestrator_inbox(&domain));
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_card_named_like_a_machine_card_is_not_one() {
        let (_home, domain) = world();
        let options = || vec![alondra_core::domain::QuestionOption { id: "x".into(), label: "x".into(), description: None }];
        domain.raise_worded_problem("machine-setup", None, "setup", "Choose the setup".into(), "Two ways to set it up".into(), "{}".into(), options(), None).unwrap();
        domain.raise_worded_problem("machine-1", Some(SystemCard::Machine), MACHINE_CAUSE, "Free up memory".into(), "Nodes wait".into(), "{}".into(), options(), None).unwrap();
        withdraw_strays(&domain);
        let state = domain.state().unwrap();
        assert!(!state.interactions[&"machine-setup".into()].withdrawn, "a card is not a machine card by its id");
        assert!(state.interactions[&"machine-1".into()].withdrawn, "a machine card no incident shows is withdrawn");
    }

    #[tokio::test]
    async fn recording_a_machine_incident_does_not_write_the_domain_or_push() {
        let (_home, domain) = world();
        let before = domain.state().unwrap();
        let told = Arc::new(Told(Mutex::new(vec![])));
        let (_, actions) = step(None, &machine(0, 50), &[], false, 0);
        act(&domain, &told, actions).await;
        let after = domain.state().unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.interactions, before.interactions);
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_full_disk_neither_breaks_the_machine_watch_nor_fills_the_log() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let watch = MachineWatch::persisted_in(&home);
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
        let watch = &domain.live.machine_watch;
        watch.poison();
        watch.measure_peaks(50 * GB, &working(&[("t1", "app")]), 0);
        watch.poison();
        watch.measure_disk(crate::disk::Space { free: 0, purgeable: 0, total: 500 * GB });
        watch.poison();
        assert!(watch.disk_room(&working(&[("t1", "app")]), None, 0).is_some(), "an empty disk has no room for the next build");
    }

    #[tokio::test]
    async fn a_thread_that_panicked_holding_the_machine_watch_does_not_stop_it() {
        let (_home, domain) = world();
        domain.live.machine_watch.poison();
        let mut watcher = tokio::spawn(watch(domain.clone(), || Some(machine(400 * GB, 50)), Arc::new(Told(Mutex::new(vec![]))), Duration::from_millis(20)));
        tokio::select! {
            () = domain.live.machine_watch.sampled.notified() => {}
            ended = &mut watcher => panic!("the machine watch stopped: {ended:?}"),
        }
        watcher.abort();
        domain.live.machine_watch.declare("s", None, "RAM", 0, now_millis());
        assert_eq!(domain.live.machine_watch.declared().len(), 1);
        assert!(domain.live.machine_watch.mac().free_percent.is_some());
    }

    #[test]
    fn a_node_that_took_another_turn_no_longer_waits() {
        let (_home, domain) = world();
        domain.live.machine_watch.declare("s", None, "RAM", 0, now_millis());
        assert_eq!(waiters(&domain).len(), 1);
        domain.live.observe(&serde_json::json!({"session_id": "s", "hook_event_name": "UserPromptSubmit", "prompt": "go"}));
        assert!(waiters(&domain).is_empty());
        assert!(waiters(&domain).is_empty(), "and it is forgotten");
    }

    fn pressed(at: Millis, under: bool) -> Machine {
        let free = if under { 3 } else { 60 };
        Machine { at, memory_pressure: Some(if under { MemoryPressure::Critical } else { MemoryPressure::Normal }), ..machine(100 * GB, free) }
    }

    fn holds(domain: &DomainRuntime, now: Millis) -> bool {
        let head = crate::admission::Waiting { key: crate::admission::Key::Task("t".into()), priority: Default::default(), resuming: false, since: 0 };
        crate::admission::decide(&head, 0, 4, &domain.live.machine_watch.mac(), 0, None, 0, now).is_err()
    }

    #[tokio::test]
    async fn sustained_memory_pressure_puts_no_card_and_no_push_while_the_nodes_are_held_and_resumed() {
        let (_home, domain) = world();
        domain.live.machine_watch.declare("s1", None, "killed for lack of memory", 0, now_millis());
        let clock = Clock::default();
        let under = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let reading = under.clone();
        let told = Arc::new(Told(Mutex::new(vec![])));
        let watcher = watching(&domain, &clock, move |at| pressed(at, reading.load(std::sync::atomic::Ordering::SeqCst)), &told);

        clock.sample_until(2 * MIN).await;
        assert!(holds(&domain, 2 * MIN), "the gate holds starts while memory has no room");
        clock.sample_until(60 * MIN).await;
        assert!(holds(&domain, 60 * MIN));
        assert!(domain.live.machine_watch.incident().is_none());
        assert!(open_machine_cards(&domain).is_empty(), "{:?}", open_machine_cards(&domain));
        assert!(told.0.lock().unwrap().is_empty(), "no push: {:?}", told.0.lock().unwrap());
        assert!(orchestrator_inbox(&domain).is_empty(), "pacing asks nothing of the orchestrator: {:?}", orchestrator_inbox(&domain));

        under.store(false, std::sync::atomic::Ordering::SeqCst);
        clock.sample_until(62 * MIN).await;
        watcher.abort();
        assert!(!holds(&domain, 62 * MIN), "the gate lets nodes start and resume once memory is back");
        assert!(orchestrator_inbox(&domain).is_empty(), "{:?}", orchestrator_inbox(&domain));
        let state = domain.state().unwrap();
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
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let body = r#"{"cause":"memory","disk_free":0,"memory_free_percent":38,"memory_pressure":true,"single_node":true}"#;
        domain.raise_worded_problem("machine-1000", Some(SystemCard::Machine), MACHINE_CAUSE, "Free up memory: macOS reports memory pressure (38% free) with a single node working".into(), "something outside Alondra uses the memory".into(), body.into(), vec![
            alondra_core::domain::QuestionOption { id: MACHINE_FIXED_OPTION.into(), label: MACHINE_FIXED_LABEL.into(), description: None },
        ], None).unwrap();
        assert_eq!(open_machine_cards(&domain).len(), 1);
        assert!(domain.live.machine_watch.incident().is_none(), "the memory incident is dropped on load");

        let told = Arc::new(Told(Mutex::new(vec![])));
        let clock = Clock::default();
        let watcher = watching(&domain, &clock, |at| pressed(at, true), &told);
        clock.samples(5).await;
        watcher.abort();
        assert!(open_machine_cards(&domain).is_empty(), "no new card in its place");
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_sample_already_taken_writes_no_second_row() {
        let (home, domain) = world();
        let sample = Machine { at: 1_792_662_011_421, ..machine(400 * GB, 50) };
        pace(&domain, &sample);
        pace(&domain, &sample);
        let day = crate::node_reports::local_day(sample.at);
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
            assert!(look(&domain, sample).is_empty());
        }
        let mac = domain.live.machine_watch.mac();
        assert!(matches!(mac.room(0, base + MIN), Err(NoRoom::SwapGrowth { .. })));
        let detail = mac.memory_detail(1);
        for expected in ["system memory", "available", "nodes", "pending", "swap used", "allocated", "swap growth"] {
            assert!(detail.contains(expected), "{detail}");
        }
        assert!(domain.live.machine_watch.disk_detail(&BTreeMap::new(), None, base + MIN).unwrap().contains("container free"));
        assert!(open_machine_cards(&domain).is_empty());
        assert!(orchestrator_inbox(&domain).is_empty());
        assert!(domain.live.machine_watch.incident().is_none());
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
