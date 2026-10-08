//! The gate every node start passes through: at most the working-node limit at once, one start
//! a minute, none while the Mac is short of memory, and no new start while its CPU is overloaded.
//! What does not fit waits in a queue ordered by priority, then resumed work before new starts,
//! then age. See docs/fleet-pacing.md.
//!
//! Launches the orchestrator or the user asked for wait in the durable `launch_queue` and this
//! gate starts them; the automatic launches and the scheduled runs keep their own durable claims
//! and only ask the gate, which wakes them when their turn comes.
use crate::domain_orchestrator_tools::{LaunchBody, Origin, launch_from, request_choices};
use crate::domain_runtime::DomainRuntime;
use crate::machine_state::{Health, MacState};
use crate::node_reports::Activity;
use alondra_core::domain::{Actor, DomainState, ScheduleLaunchState, ScheduleRunState, TaskId, TaskPriority, TaskState};
use alondra_core::views::{LiveHold, LivePacing, LiveQueued};
use alondra_core::Millis;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FILE: &str = "fleet-gate.json";
const SPACING: Millis = 60_000;
/// A node admitted this recently counts as working even before its mod reports a turn.
const IN_FLIGHT: Millis = 3 * 60_000;
const LEARN_STEP: Millis = 15 * 60_000;
const TICK: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum Key {
    Task(TaskId),
    Run(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub key: Key,
    pub priority: TaskPriority,
    /// Work a pause cut short goes back before new starts of its priority.
    pub resuming: bool,
    pub since: Millis,
}

/// Why the head of the queue does not start yet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Hold {
    Slots { working: usize, limit: u32 },
    Memory { health: Health, free: Option<u32> },
    Cpu { load: f64, cpus: usize },
    Spacing { until: Millis },
}

pub(crate) fn ordered(mut queue: Vec<Waiting>) -> Vec<Waiting> {
    queue.sort_by(|a, b| (a.priority, !a.resuming, a.since, &a.key).cmp(&(b.priority, !b.resuming, b.since, &b.key)));
    queue
}

/// Whether `head` may start now. Memory holds everything; the CPU holds only new starts.
pub(crate) fn decide(head: &Waiting, working: usize, limit: u32, mac: &MacState, last_admission: Option<Millis>, spacing: Millis, now: Millis) -> Result<(), Hold> {
    if mac.health != Health::Fine {
        return Err(Hold::Memory { health: mac.health, free: mac.free_percent });
    }
    if mac.cpu_busy && !head.resuming {
        return Err(Hold::Cpu { load: mac.load, cpus: mac.cpus });
    }
    if working >= limit as usize {
        return Err(Hold::Slots { working, limit });
    }
    if let Some(until) = last_admission.map(|at| at + spacing).filter(|until| *until > now) {
        return Err(Hold::Spacing { until });
    }
    Ok(())
}

/// What the gate learned about how many nodes this Mac takes: lowered when the Mac reaches its
/// limit, raised by one every `LEARN_STEP` the Mac stays fine with someone waiting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Learned {
    pub cap: Option<u32>,
    since: Option<Millis>,
}

impl Learned {
    pub(crate) fn limit(&self, configured: u32) -> u32 {
        self.cap.map_or(configured, |cap| cap.min(configured))
    }

    pub(crate) fn at_limit(&mut self, working: usize) {
        self.cap = Some((working.saturating_sub(1)).max(1) as u32);
        self.since = None;
    }

    pub(crate) fn learn(&mut self, configured: u32, health: Health, queued: bool, now: Millis) {
        let Some(cap) = self.cap else { return };
        if health != Health::Fine || !queued {
            self.since = None;
            return;
        }
        let since = *self.since.get_or_insert(now);
        if now - since >= LEARN_STEP {
            self.cap = (cap + 1 < configured).then_some(cap + 1);
            self.since = Some(now);
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Saved {
    learned: Learned,
    last_admission: Option<Millis>,
}

/// The pacing the gate keeps to; tests start without spacing and with the highest limit unless
/// they set their own.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tuning {
    pub spacing: Millis,
    pub limit: Option<u32>,
}

impl Default for Tuning {
    fn default() -> Self {
        if cfg!(test) {
            Self { spacing: 0, limit: Some(alondra_core::options::MAX_WORKING_NODES) }
        } else {
            Self { spacing: SPACING, limit: None }
        }
    }
}

#[derive(Default)]
struct Inner {
    saved: Saved,
    /// Starts the gate let through and when, counted as working while their node comes up.
    admitted: Vec<(Key, Millis)>,
    /// Starts whose caller keeps them durable and asks again when woken.
    asking: BTreeMap<Key, Waiting>,
    tuning: Tuning,
}

pub(crate) struct FleetGate {
    inner: Mutex<Inner>,
    file: Option<PathBuf>,
    /// Bumped whenever a waiting start may go: the callers that ask the gate wait on it.
    turn: tokio::sync::watch::Sender<u64>,
    changed: tokio::sync::Notify,
}

impl Default for FleetGate {
    fn default() -> Self {
        Self { inner: Mutex::default(), file: None, turn: tokio::sync::watch::Sender::new(0), changed: tokio::sync::Notify::new() }
    }
}

/// What the gate sees at one moment.
pub(crate) struct Snapshot {
    pub queue: Vec<Waiting>,
    pub working: usize,
    pub limit: u32,
    pub configured: u32,
    pub mac: MacState,
    pub hold: Option<Hold>,
}

impl FleetGate {
    pub(crate) fn persisted_in(home: &Path) -> Self {
        let file = home.join(FILE);
        let saved = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self { inner: Mutex::new(Inner { saved, ..Inner::default() }), file: Some(file), ..Self::default() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("fleet gate poisoned")
    }

    fn save(&self, inner: &Inner) {
        let Some(file) = &self.file else { return };
        let next = file.with_extension("next");
        let written = serde_json::to_vec(&inner.saved).map_err(std::io::Error::from)
            .and_then(|bytes| std::fs::write(&next, bytes))
            .and_then(|()| std::fs::rename(&next, file));
        if let Err(error) = written {
            log!("could not save {}: {error}", file.display());
        }
    }

    #[cfg(test)]
    pub(crate) fn tune(&self, tuning: Tuning) {
        self.lock().tuning = tuning;
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.turn.subscribe()
    }

    /// The Mac reached its limit with `working` nodes: the gate admits fewer from now on.
    pub(crate) fn at_limit(&self, working: usize) {
        let mut inner = self.lock();
        inner.saved.learned.at_limit(working);
        self.save(&inner);
    }

    /// Something the gate decides on changed: a launch was queued, or a node ended.
    pub(crate) fn poke(&self) {
        self.changed.notify_one();
    }

    /// Whether `waiting` may start now. If it may, the start is counted at once; if not, it is
    /// kept in the queue and the caller is woken through `subscribe` when its turn may have come.
    pub(crate) fn ask(&self, domain: &DomainRuntime, waiting: Waiting, now: Millis) -> Result<(), Hold> {
        let state = domain.state().map_err(|_| Hold::Spacing { until: now + SPACING })?;
        let queued = queued_launches(domain, &state);
        let mut inner = self.lock();
        let key = waiting.key.clone();
        let waiting = inner.asking.get(&key).map_or(waiting.clone(), |known| Waiting { since: known.since, ..waiting });
        inner.asking.insert(key.clone(), waiting);
        let snapshot = snapshot_locked(domain, &state, &inner, queued, now);
        let head = snapshot.queue.first().filter(|head| head.key == key);
        let Some(head) = head else {
            return Err(snapshot.hold.unwrap_or(Hold::Slots { working: snapshot.working, limit: snapshot.limit }));
        };
        decide(head, snapshot.working, snapshot.limit, &snapshot.mac, inner.saved.last_admission, inner.tuning.spacing, now)?;
        inner.asking.remove(&key);
        admit(&mut inner, key, now);
        self.save(&inner);
        Ok(())
    }

    /// A start that no longer waits: its task ended, or its caller gave up on it.
    pub(crate) fn forget(&self, key: &Key) {
        self.lock().asking.remove(key);
    }

    pub(crate) fn snapshot(&self, domain: &DomainRuntime, now: Millis) -> Option<Snapshot> {
        let state = domain.state().ok()?;
        let queued = queued_launches(domain, &state);
        let inner = self.lock();
        Some(snapshot_locked(domain, &state, &inner, queued, now))
    }

    /// What the views draw: the working count, the limit, the Mac and every queued task's place.
    pub(crate) fn live(&self, domain: &DomainRuntime, now: Millis) -> Option<LivePacing> {
        let snapshot = self.snapshot(domain, now)?;
        let state = domain.state().ok()?;
        let total = snapshot.queue.len();
        let hold = snapshot.hold.as_ref().map(live_hold);
        let queued = snapshot.queue.iter().enumerate().filter_map(|(index, waiting)| {
            let task = task_of(&state, &waiting.key)?;
            Some(LiveQueued { task_id: task.0, position: index + 1, of: total, hold: hold.clone().unwrap_or(LiveHold::Next) })
        }).collect();
        Some(LivePacing {
            working: snapshot.working,
            limit: snapshot.limit,
            memory_free: snapshot.mac.free_percent,
            tight: snapshot.mac.health != Health::Fine,
            queued,
        })
    }
}

fn admit(inner: &mut Inner, key: Key, now: Millis) {
    inner.admitted.retain(|(_, at)| now - at < IN_FLIGHT);
    inner.admitted.push((key, now));
    inner.saved.last_admission = Some(now);
}

fn live_hold(hold: &Hold) -> LiveHold {
    match hold {
        Hold::Slots { working, limit } => LiveHold::Slots { working: *working, limit: *limit },
        Hold::Memory { free, .. } => LiveHold::Memory { free: *free },
        Hold::Cpu { load, cpus } => LiveHold::Cpu { load: load.round() as u32, cpus: *cpus },
        Hold::Spacing { .. } => LiveHold::Next,
    }
}

fn task_of(state: &DomainState, key: &Key) -> Option<TaskId> {
    match key {
        Key::Task(task) => Some(task.clone()),
        Key::Run(run) => state.schedule.runs.get(&run.as_str().into()).and_then(|run| run.task_id.clone()),
    }
}

fn priority_of(state: &DomainState, task: Option<&TaskId>) -> TaskPriority {
    task.and_then(|task| state.tasks.get(task)).map_or(TaskPriority::Normal, |task| task.priority)
}

/// The priority a schedule asks its runs to start with: its `priority` option, Normal without one.
pub(crate) fn run_priority(state: &DomainState, run: &alondra_core::domain::ScheduleRun) -> TaskPriority {
    let chosen = state.schedule.schedules.get(&run.schedule_id).and_then(|schedule| match schedule.options.get("priority") {
        Some(alondra_core::options::Choice::Value(value)) => serde_json::from_value(serde_json::Value::String(value.clone())).ok(),
        _ => None,
    });
    chosen.unwrap_or_else(|| priority_of(state, run.task_id.as_ref()))
}

/// A start still worth waiting for: its task is open and has no live node, or its run still waits
/// to launch.
fn still_waits(state: &DomainState, key: &Key) -> bool {
    match key {
        Key::Task(task) => state.tasks.get(task).is_some_and(|task| !task.archived && !task.state.terminal()
            && !matches!(task.state, TaskState::Closing)
            && task.agent_id.as_ref().is_none_or(|agent| !state.agents.get(agent).is_some_and(|agent| agent.connected))),
        Key::Run(run) => state.schedule.runs.get(&run.as_str().into()).is_some_and(|run| run.state == ScheduleRunState::Sent
            && run.launch.as_ref().is_some_and(|launch| launch.state == ScheduleLaunchState::Pending)),
    }
}

/// Nodes at work on a task, from what their mods report.
pub(crate) fn busy_tasks(domain: &DomainRuntime, state: &DomainState, now: Millis) -> BTreeSet<TaskId> {
    state.tasks.values()
        .filter(|task| !task.archived && !task.state.terminal())
        .filter(|task| task.agent_id.as_ref().and_then(|agent| domain.live.mods.session(&agent.0))
            .is_some_and(|reported| matches!(reported.activity(now), Activity::Working | Activity::Stalled)))
        .map(|task| task.id.clone())
        .collect()
}

fn queued_launches(domain: &DomainRuntime, state: &DomainState) -> Vec<Waiting> {
    let rows = domain.store.lock().expect("domain store poisoned").launch_queue().unwrap_or_default();
    rows.into_iter().map(|row| Waiting {
        priority: priority_of(state, Some(&row.task_id)),
        key: Key::Task(row.task_id),
        resuming: false,
        since: row.enqueued_at,
    }).collect()
}

fn configured_limit(domain: &DomainRuntime, tuning: Tuning) -> u32 {
    if let Some(limit) = tuning.limit {
        return limit;
    }
    let file = crate::domain_schedule_runner::options_at(domain.home()).unwrap_or_default();
    let (cores, memory) = machine_size();
    file.max_working_nodes.resolve(cores, memory)
}

fn machine_size() -> (usize, u64) {
    static SIZE: std::sync::OnceLock<(usize, u64)> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        (std::thread::available_parallelism().map_or(1, |cores| cores.get()), system.total_memory())
    })
}

fn snapshot_locked(domain: &DomainRuntime, state: &DomainState, inner: &Inner, queued: Vec<Waiting>, now: Millis) -> Snapshot {
    let busy = busy_tasks(domain, state, now);
    let in_flight = inner.admitted.iter()
        .filter(|(_, at)| now - at < IN_FLIGHT)
        .filter(|(key, _)| task_of(state, key).is_none_or(|task| !busy.contains(&task)))
        .count();
    let working = busy.len() + in_flight;
    let mut all: BTreeMap<Key, Waiting> = inner.asking.iter().filter(|(key, _)| still_waits(state, key))
        .map(|(key, waiting)| (key.clone(), waiting.clone())).collect();
    for waiting in queued.into_iter().filter(|waiting| still_waits(state, &waiting.key)) {
        all.insert(waiting.key.clone(), waiting);
    }
    let queue = ordered(all.into_values().collect());
    let configured = configured_limit(domain, inner.tuning);
    let limit = inner.saved.learned.limit(configured);
    let mac = domain.live.machine_watch.mac();
    let hold = queue.first().and_then(|head| decide(head, working, limit, &mac, inner.saved.last_admission, inner.tuning.spacing, now).err());
    Snapshot { queue, working, limit, configured, mac, hold }
}

/// Queues a launch the orchestrator or the user asked for; the gate starts it when its turn comes.
pub(crate) fn enqueue(domain: &DomainRuntime, body: &LaunchBody, origin: &str, now: Millis) -> anyhow::Result<()> {
    domain.store.lock().expect("domain store poisoned").enqueue_launch(body.task_id(), &body.to_queue(), origin, now)?;
    domain.live.gate.poke();
    Ok(())
}

/// The place and the wait of a queued task, worded for the orchestrator.
pub(crate) fn queued_text(domain: &DomainRuntime, task: &TaskId, now: Millis) -> String {
    let Some(pacing) = domain.live.gate.live(domain, now) else { return "It waits in the queue for a free slot.".into() };
    let Some(place) = pacing.queued.iter().find(|queued| queued.task_id == task.0) else {
        return "It waits in the queue for a free slot.".into();
    };
    let why = match &place.hold {
        LiveHold::Slots { working, limit } => format!("{working} of {limit} nodes are working"),
        LiveHold::Memory { free: Some(free) } => format!("the Mac is short of memory ({free}% free)"),
        LiveHold::Memory { free: None } => "the Mac is short of memory".into(),
        LiveHold::Cpu { load, cpus } => format!("the Mac is very busy (load {load} on {cpus} cores)"),
        LiveHold::Next => "it starts within a minute".into(),
    };
    format!("It waits in the queue, {} of {}: {why}. Alondra starts it by itself when its turn comes; you do not need to launch it again.",
        place.position, place.of)
}

/// Starts the queued launches whose turn came, and wakes the callers that asked the gate.
pub(crate) async fn run(domain: Arc<DomainRuntime>, home: PathBuf, launcher: impl crate::orchestrate::Launcher) {
    let mut changes = domain.subscribe();
    loop {
        if let Err(error) = pass(&domain, &home, &launcher).await {
            log!("the fleet gate could not start a queued launch: {error:#}");
        }
        tokio::select! {
            () = tokio::time::sleep(TICK) => {}
            () = domain.live.gate.changed.notified() => {}
            changed = changes.changed() => if changed.is_err() { return },
        }
    }
}

/// One look at the queue: learns the limit, drops what no longer waits, and lets the head start.
pub(crate) async fn pass(domain: &DomainRuntime, home: &Path, launcher: &impl crate::orchestrate::Launcher) -> anyhow::Result<()> {
    let now = alondra_core::now_millis();
    let state = domain.state()?;
    let rows = domain.store.lock().expect("domain store poisoned").launch_queue()?;
    for row in rows.iter().filter(|row| !still_waits(&state, &Key::Task(row.task_id.clone()))) {
        domain.store.lock().expect("domain store poisoned").dequeue_launch(&row.task_id)?;
    }
    let gate = &domain.live.gate;
    let head = {
        let queued = queued_launches(domain, &state);
        let mut inner = gate.lock();
        inner.asking.retain(|key, _| still_waits(&state, key));
        let snapshot = snapshot_locked(domain, &state, &inner, queued, now);
        let configured = snapshot.configured;
        inner.saved.learned.learn(configured, snapshot.mac.health, !snapshot.queue.is_empty(), now);
        gate.save(&inner);
        match (snapshot.queue.first(), snapshot.hold) {
            (Some(head), None) if inner.asking.contains_key(&head.key) => {
                gate.turn.send_modify(|turn| *turn += 1);
                None
            }
            (Some(head), None) => {
                let key = head.key.clone();
                admit(&mut inner, key.clone(), now);
                gate.save(&inner);
                Some(key)
            }
            _ => None,
        }
    };
    let Some(Key::Task(task)) = head else { return Ok(()) };
    let Some(row) = domain.store.lock().expect("domain store poisoned").launch_queue()?.into_iter().find(|row| row.task_id == task) else {
        return Ok(());
    };
    domain.store.lock().expect("domain store poisoned").dequeue_launch(&task)?;
    let body = LaunchBody::from_queue(&row.body)?;
    let choices = |request: &alondra_core::domain::RequestId| request_choices(domain, request).ok().flatten();
    log!("the fleet gate starts {task}, queued since {}", row.enqueued_at);
    if let Err(error) = launch_from(domain, home, &Actor::System, body, choices, launcher, Origin::Admitted).await {
        let title = state.tasks.get(&task).map_or_else(|| task.0.clone(), |task| task.title.clone());
        let text = format!("Alondra could not start the queued task “{title}” ({task}): {} Start it with launch (task {task}) once the cause is fixed.",
            error.message);
        if let Err(error) = domain.tell_orchestrator(&format!("queued-launch-failed-{task}-{}", crate::auth::random_hex(4)), text) {
            log!("could not tell the orchestrator that the queued launch of {task} failed: {error:?}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
