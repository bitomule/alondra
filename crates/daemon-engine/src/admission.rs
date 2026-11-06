//! The gate every node start passes through: at most the working-node limit at once, one start
//! a minute (one every 3 while macOS warns of memory pressure), none while memory has no room for
//! one more node (`MacState::room`, which reserves free memory for every node admitted in the last
//! `IN_FLIGHT`), and none while the disk has no room for its build
//! on top of what the nodes at work may still take (`disk_peaks`). A paused node coming back, the user's Resume
//! included, passes the same checks as a start. Memory only ever holds starts and
//! resumes, never pauses a working node, and the CPU holds nothing: builds spend it, agents spend memory. The
//! limit is the user's number, or in automatic what this Mac's memory takes at the measured cost of a node. What does not
//! fit waits in a queue ordered by priority, then resumed work before new starts, then age.
//!
//! Launches the orchestrator or the user asked for wait in the durable `launch_queue` and this
//! gate starts them; the automatic launches, the scheduled runs and the revival of lost nodes keep
//! their own durable claims and only ask the gate, which wakes them when their turn comes. Nodes
//! paused for the machine (`pauses`) wait here too and the gate tells them to carry on.
use crate::locks::Poisonable as _;
use crate::domain_orchestrator_tools::{LaunchBody, Origin, launch_from, request_choices};
use crate::domain_runtime::DomainRuntime;
use crate::pauses::{Pauses, Paused, Reason};
use crate::machine_state::{MacState, NoRoom, WARNING_SPACING};
use crate::domain_store::{AutoLaunchReason, AutoLaunchState};
use alondra_core::domain::{AgentId, Command, CommandEnvelope, DomainState, NodeEvent, NodePhase, PROTOCOL_VERSION, Pause, PauseCleanup, PauseReason, TaskId, TaskPriority, TaskState};
use alondra_core::machine_incident::bytes_text;
use alondra_core::views::{Lang, LiveHold, LivePacing, LivePaused, LivePauseReason, LiveQueued};
use alondra_core::Millis;
use alondra_core::options::{NodeLimit, NodeLimits};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use crate::runtime_mutex::RuntimeMutex as Mutex;
use std::time::Duration;

pub(crate) const FILE: &str = "admission.json";
/// The gate's file before it was renamed; startup moves it to `FILE`.
pub(crate) const PREVIOUS_FILE: &str = "fleet-gate.json";
pub(crate) const SPACING: Millis = 60_000;
/// The limit of tests and of a sandbox with fast timing, in place of automatic.
pub const UNPACED: u32 = 12;
/// A node admitted this recently counts as working even before its mod reports a turn, and its
/// memory, which does not show yet, is reserved before one more starts.
const IN_FLIGHT: Millis = 3 * 60_000;
const TICK: Duration = Duration::from_secs(10);
/// How long a task stays stranded before it goes back to work: a state between two commands of the
/// same step never reaches it.
pub(crate) const STRANDED_AFTER: Millis = 5 * 60_000;
/// How often the log repeats why the queue waits while the reason stays the same.
const HOLD_LOG_EVERY: Millis = 10 * 60_000;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Key {
    Task(TaskId),
    Run(String),
    /// A node coming back to work, by its session: paused, or revived after its connection was lost.
    Resume(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    pub key: Key,
    pub priority: TaskPriority,
    /// Work a pause cut short goes back before new starts of its priority.
    pub resuming: bool,
    pub since: Millis,
}

/// Why the head of the queue does not start yet.
#[derive(Clone, Debug, PartialEq)]
pub enum Hold {
    Slots { working: usize, limit: u32 },
    Memory { why: NoRoom, free: Option<u32> },
    Spacing { until: Millis },
    /// No room on the disk for one more build: the Mac's own, or the volume named.
    Disk { free: u64, needed: u64, volume: Option<String> },
}

pub(crate) fn ordered(mut queue: Vec<Waiting>) -> Vec<Waiting> {
    queue.sort_by(|a, b| (a.priority, !a.resuming, a.since, &a.key).cmp(&(b.priority, !b.resuming, b.since, &b.key)));
    queue
}

/// Whether `head` may start now, with `in_flight` nodes admitted in the last `IN_FLIGHT`. Memory
/// holds every start and resume, and the CPU holds none; macOS's warning only spaces them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide(head: &Waiting, working: usize, limit: u32, mac: &MacState, in_flight: usize, last_admission: Option<Millis>, spacing: Millis, now: Millis) -> Result<(), Hold> {
    mac.room(in_flight, now).map_err(|why| Hold::Memory { why, free: mac.free_percent })?;
    if working >= limit as usize {
        return Err(Hold::Slots { working, limit });
    }
    let spacing = if mac.warned() { spacing.max(WARNING_SPACING) } else { spacing };
    if let Some(until) = last_admission.map(|at| at + spacing).filter(|until| *until > now) {
        return Err(Hold::Spacing { until });
    }
    Ok(())
}

/// The working-node limit the user chose: a number, or automatic, from this Mac's cores and what
/// its nodes measure. Free memory never lowers it: it only holds starts while it has no room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Fixed(u32),
    Auto { max: u32 },
}

impl Limit {
    pub(crate) fn count(self) -> u32 {
        match self {
            Limit::Fixed(count) | Limit::Auto { max: count } => count,
        }
    }
}

/// What the gate keeps across restarts. Fields an older daemon wrote, such as the limit it learned
/// or the step between resumes, are ignored.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Saved {
    last_admission: Option<Millis>,
    #[serde(default)]
    pauses: Pauses,
}

/// The pacing the gate keeps to; tests start without spacing and with the highest limit unless
/// they set their own. A number the user chose in options.toml still holds, so a sandbox can fill
/// its slots.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    pub spacing: Millis,
    /// The limit in place of automatic.
    pub limit: Option<Limit>,
}

impl Default for Tuning {
    fn default() -> Self {
        if cfg!(any(test, feature = "test-hooks")) {
            Self { spacing: 0, limit: Some(Limit::Fixed(UNPACED)) }
        } else {
            Self { spacing: SPACING, limit: None }
        }
    }
}

#[derive(Default)]
struct Inner {
    saved: Saved,
    /// Pauses an older daemon kept in the gate's file, handed to the node machine on first use.
    legacy: BTreeMap<String, Paused>,
    /// Starts the gate let through and when, counted as working while their node comes up.
    admitted: Vec<(Key, Millis)>,
    /// Starts whose caller keeps them durable and asks again when woken.
    asking: BTreeMap<Key, Waiting>,
    /// Resumes the gate let through whose node was seen at work since: its own phase holds its
    /// slot from then on, so the admission no longer counts.
    at_work: BTreeSet<Key>,
    tuning: Tuning,
    told: Told,
    /// Resumes the node machine refused, by session, with the revision of its task's lifecycle
    /// then: the pause stays out of the queue until that lifecycle moves, so a node that cannot
    /// come back is not retried every minute at the head of the queue.
    refused: BTreeMap<String, u64>,
    /// A sandbox with fast timing: its fake nodes cost nothing, so the Mac never holds a start.
    ignores_mac: bool,
    /// The tasks seen stranded, and since when: `STRANDED_AFTER` later they go back through the gate.
    stranded: BTreeMap<TaskId, Millis>,
}

/// What holds the queue, without the numbers that change on every sample (free memory, load,
/// queue length), so the log repeats a hold only when its cause changes or every `HOLD_LOG_EVERY`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    Slots,
    Memory,
    Disk,
}

impl Cause {
    pub(crate) fn of(hold: &Hold) -> Option<Self> {
        match hold {
            Hold::Spacing { .. } => None,
            Hold::Slots { .. } => Some(Self::Slots),
            Hold::Memory { .. } => Some(Self::Memory),
            Hold::Disk { .. } => Some(Self::Disk),
        }
    }
}

/// The last cause the log gave for a queue that does not move, and when.
#[derive(Default)]
pub(crate) struct Told(Option<(Cause, Millis)>);

impl Told {
    pub(crate) fn due(&mut self, cause: Option<Cause>, now: Millis) -> bool {
        let Some(cause) = cause else {
            self.0 = None;
            return false;
        };
        if self.0.is_some_and(|(said, at)| said == cause && now - at < HOLD_LOG_EVERY) {
            return false;
        }
        self.0 = Some((cause, now));
        true
    }
}

pub struct Admission {
    /// A holder that panics leaves it whole, so it is recovered: see `Admission::lock`.
    inner: Mutex<Inner>,
    file: Option<PathBuf>,
    save_failures: crate::save::Failures,
    /// Bumped whenever a waiting start may go: the callers that ask the gate wait on it.
    turn: tokio::sync::watch::Sender<u64>,
    changed: tokio::sync::Notify,
}

impl Default for Admission {
    fn default() -> Self {
        Self { inner: Mutex::default(), file: None, save_failures: crate::save::Failures::default(), turn: tokio::sync::watch::Sender::new(0), changed: tokio::sync::Notify::new() }
    }
}

/// What the gate sees at one moment.
pub struct Snapshot {
    pub queue: Vec<Waiting>,
    pub working: usize,
    /// The tasks behind `working`: the busy ones and those starting.
    pub working_tasks: BTreeSet<TaskId>,
    pub limit: u32,
    /// What `options.toml` sets and what this Mac takes, which `limit` comes from.
    pub setting: NodeLimit,
    pub limits: NodeLimits,
    pub mac: MacState,
    /// Nodes admitted in the last `IN_FLIGHT`, whose memory is reserved.
    pub in_flight: usize,
    pub hold: Option<Hold>,
    pub disk: Option<crate::disk::Room>,
    disk_detail: Option<String>,
}

impl Admission {
    pub(crate) fn persisted_in(home: &Path) -> Self {
        let file = home.join(FILE);
        let read = |path: &Path| std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok());
        let mut saved = read(&file).or_else(|| read(&home.join(PREVIOUS_FILE))).unwrap_or_default();
        let legacy = std::mem::take(&mut saved.pauses.paused);
        Self { inner: Mutex::new(Inner { saved, legacy, ..Inner::default() }), file: Some(file), ..Self::default() }
    }

    /// A holder that panicked leaves a gate that still works: the pauses are read again from the
    /// node machine on every `lock_with`, an admitted start expires after `IN_FLIGHT`, a waiting
    /// start is asked for again by its caller, and the file is only ever replaced whole.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock_or_recover("the admission gate")
    }

    /// The gate with the pauses the node machine holds now: the only place they live.
    fn lock_with(&self, domain: &DomainRuntime) -> std::sync::MutexGuard<'_, Inner> {
        let mut inner = self.lock();
        if !inner.legacy.is_empty() {
            let legacy = std::mem::take(&mut inner.legacy);
            let mut wanted = domain.state().map(|state| domain_paused(&state)).unwrap_or_default();
            for (session, paused) in legacy {
                wanted.entry(session).or_insert(paused);
            }
            hand_pauses(domain, &wanted);
        }
        if let Ok(state) = domain.state() {
            inner.saved.pauses.paused = domain_paused(&state);
            let Inner { saved, refused, .. } = &mut *inner;
            refused.retain(|session, _| saved.pauses.paused.contains_key(session));
        }
        inner
    }

    /// Hands what changed in the pauses to the node machine, then saves the rest of the gate.
    fn save_with(&self, domain: &DomainRuntime, inner: &mut Inner) {
        let refused = hand_pauses(domain, &inner.saved.pauses.paused);
        inner.refused.extend(refused);
        self.save(inner);
    }

    fn save(&self, inner: &Inner) {
        let Some(file) = &self.file else { return };
        let written = serde_json::to_vec(&inner.saved).map_err(std::io::Error::from)
            .and_then(|bytes| crate::save::atomic(file, &bytes));
        self.save_failures.record(file, written);
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn poison(&self) {
        self.inner.poison();
    }

    pub fn tune(&self, tuning: Tuning) {
        self.lock().tuning = tuning;
    }

    pub fn ignore_mac(&self) {
        self.lock().ignores_mac = true;
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.turn.subscribe()
    }

    pub(crate) fn paused(&self, domain: &DomainRuntime, session: &str) -> Option<Paused> {
        self.lock_with(domain).saved.pauses.paused.get(session).cloned()
    }

    pub(crate) fn pauses(&self, domain: &DomainRuntime) -> Pauses {
        self.lock_with(domain).saved.pauses.clone()
    }

    /// A node stopped for a reason of its own (`wait_for_machine`, the usage limit): it waits here
    /// with that reason and comes back through the queue. False for a session with no open task,
    /// which the gate does not pace.
    pub fn hold_back(&self, domain: &DomainRuntime, session: &str, reason: Reason, now: Millis) -> bool {
        let Some(task) = domain.state().ok().and_then(|state| open_task_of(&state, session)) else { return false };
        let mut inner = self.lock_with(domain);
        if inner.saved.pauses.paused.get(session).is_some_and(|paused| paused.reason.holds()) {
            return true;
        }
        let turn_since = domain.live.mods.session(session).and_then(|reported| reported.working_since);
        inner.saved.pauses.paused.insert(session.to_owned(), Paused {
            task, reason, since: now, cut: None, turn_since, cleanup: crate::pauses::Cleanup::Done, released: false,
        });
        self.save_with(domain, &mut inner);
        drop(inner);
        self.poke();
        true
    }

    /// The user paused a working node: it stops at once and only the user brings it back.
    pub fn pause_by_user(&self, domain: &DomainRuntime, session: &str, now: Millis) -> Result<(), &'static str> {
        let task = domain.state().ok().and_then(|state| open_task_of(&state, session)).ok_or("This node has no open task")?;
        let mut inner = self.lock_with(domain);
        inner.saved.pauses.resumed.remove(session);
        crate::pauses::stop(domain, &mut inner.saved.pauses, session, task, Reason::ByUser, now);
        self.save_with(domain, &mut inner);
        let paused = inner.saved.pauses.paused.get(session).cloned();
        drop(inner);
        if let Some(paused) = paused.filter(|_| crate::pauses::through_sdk(domain, session)) {
            crate::pauses::hold_sdk_turn(domain, session, &paused);
        }
        Ok(())
    }

    /// The user resumed a paused node: whatever its pause waited for, it waits in the queue like a
    /// start, by its priority and the age of its pause, and stays paused until the gate admits it.
    pub fn resume_now(&self, domain: &DomainRuntime, session: &str, now: Millis) -> bool {
        let mut inner = self.lock_with(domain);
        if !inner.saved.pauses.paused.contains_key(session) {
            return false;
        }
        inner.saved.pauses.resumed.entry(session.to_owned()).or_insert(now);
        inner.refused.remove(session);
        self.save_with(domain, &mut inner);
        drop(inner);
        self.poke();
        true
    }

    pub(crate) fn paused_by_user(&self, domain: &DomainRuntime, session: &str) -> bool {
        self.lock_with(domain).saved.pauses.paused.get(session).is_some_and(|paused| paused.reason == Reason::ByUser)
    }

    /// The reason of a pause that holds the node until Alondra or the user ends it, if it has one.
    pub(crate) fn holding(&self, domain: &DomainRuntime, session: &str) -> Option<Reason> {
        self.lock_with(domain).saved.pauses.paused.get(session).map(|paused| paused.reason.clone()).filter(Reason::holds)
    }

    /// The user wrote to a node of the Agent SDK they paused: their message waits with its task's
    /// inputs, and the node comes back through the queue as their Resume brings it.
    pub(crate) fn resume_by_user_message(&self, domain: &DomainRuntime, session: &str, now: Millis) {
        if self.paused_by_user(domain, session) {
            self.resume_now(domain, session, now);
        }
    }

    /// The user wrote to a `claude --bg` node paused by them, or for memory by an older daemon: the
    /// message already went down its connection and is what it goes on with, so the pause ends
    /// without the resume notice.
    pub(crate) fn release_by_user_message(&self, domain: &DomainRuntime, session: &str) {
        let mut inner = self.lock_with(domain);
        if inner.saved.pauses.paused.get(session).is_some_and(|paused| paused.reason.holds()) {
            inner.saved.pauses.paused.remove(session);
            log!("{session} is no longer paused: the user wrote to it");
            self.save_with(domain, &mut inner);
        }
    }

    /// The user said they fixed what the machine lacked: declared waits no longer wait out their grace.
    pub(crate) fn release_declared(&self, domain: &DomainRuntime) {
        let mut inner = self.lock_with(domain);
        for paused in inner.saved.pauses.paused.values_mut().filter(|paused| matches!(paused.reason, Reason::Declared { .. })) {
            paused.released = true;
        }
        self.save_with(domain, &mut inner);
        drop(inner);
        self.poke();
    }

    /// Something the gate decides on changed: a launch was queued, or a node ended.
    pub fn poke(&self) {
        self.changed.notify_one();
    }

    /// Whether `waiting` may start now. If it may, the start is counted at once; if not, it is
    /// kept in the queue and the caller is woken through `subscribe` when its turn may have come.
    pub fn ask(&self, domain: &DomainRuntime, waiting: Waiting, now: Millis) -> Result<(), Hold> {
        let state = domain.state().map_err(|_| Hold::Spacing { until: now + SPACING })?;
        let queued = queued_launches(domain, &state, now);
        let mut inner = self.lock_with(domain);
        let key = waiting.key.clone();
        let ended = match &key {
            Key::Task(task) => state.tasks.get(task).is_none_or(|task| task.over() || task.archived),
            Key::Run(_) => !still_waits(&state, &key),
            Key::Resume(_) => false,
        };
        if !ended && inner.admitted.iter().any(|(admitted, at)| *admitted == key && now - at < IN_FLIGHT) {
            return Ok(());
        }
        let waiting = inner.asking.get(&key).map_or(waiting.clone(), |known| Waiting { since: known.since, ..waiting });
        inner.asking.insert(key.clone(), waiting);
        note_at_work(&mut inner, &state);
        let snapshot = snapshot_locked(domain, &state, &inner, queued, now);
        let head = snapshot.queue.first().filter(|head| head.key == key);
        let Some(head) = head else {
            return Err(snapshot.hold.unwrap_or(Hold::Slots { working: snapshot.working, limit: snapshot.limit }));
        };
        hold_of(&inner, head, &snapshot, now)?;
        log!("the admission gate lets {} start", key_text(&state, &key));
        inner.asking.remove(&key);
        if let Key::Resume(session) = &key {
            resume_locked(domain, &mut inner, session, now);
        }
        admit(&mut inner, key, now);
        self.save_with(domain, &mut inner);
        Ok(())
    }

    /// What the gate still owes a node it let back while it was unreachable, taken so that whoever
    /// reaches the node first says it once.
    pub(crate) fn take_owed(&self, session: &str) -> Option<String> {
        let mut inner = self.lock();
        let owed = inner.saved.pauses.owed.remove(session);
        if owed.is_some() {
            self.save(&inner);
        }
        owed
    }

    /// A start that no longer waits: its task ended, or its caller gave up on it.
    pub(crate) fn forget(&self, key: &Key) {
        self.lock().asking.remove(key);
    }

    /// A start the gate let through that did not start: no node comes up, so it holds no slot.
    pub(crate) fn not_started(&self, key: &Key) {
        self.lock().admitted.retain(|(admitted, _)| admitted != key);
    }

    pub fn snapshot(&self, domain: &DomainRuntime, now: Millis) -> Option<Snapshot> {
        let state = domain.state().ok()?;
        let queued = queued_launches(domain, &state, now);
        let inner = self.lock_with(domain);
        Some(snapshot_locked(domain, &state, &inner, queued, now))
    }

    /// What the views draw: the working count, the limit, the Mac and every queued task's place.
    pub fn live(&self, domain: &DomainRuntime, now: Millis) -> Option<LivePacing> {
        let snapshot = self.snapshot(domain, now)?;
        let state = domain.state().ok()?;
        let pauses = self.pauses(domain);
        let held = |session: &String| pauses.paused.get(session).is_some_and(|paused| paused.reason != Reason::Disk) && !pauses.resumed.contains_key(session);
        let is_paused = |waiting: &Waiting| matches!(&waiting.key, Key::Resume(session) if held(session));
        let starts: Vec<&Waiting> = snapshot.queue.iter().filter(|waiting| !is_paused(waiting)).collect();
        let total = starts.len();
        let hold = snapshot.hold.as_ref().map(live_hold);
        let queued = starts.iter().enumerate().filter_map(|(index, waiting)| {
            let task = task_of(&state, &waiting.key)?;
            Some(LiveQueued { task_id: task.0, position: index + 1, of: total, hold: hold.clone().unwrap_or(LiveHold::Next),
                pausable: matches!(waiting.key, Key::Task(_)) })
        }).collect();
        let mut order: Vec<String> = snapshot.queue.iter().filter(|waiting| is_paused(waiting))
            .filter_map(|waiting| match &waiting.key { Key::Resume(session) => Some(session.clone()), _ => None }).collect();
        let rest: Vec<String> = pauses.paused.keys().filter(|session| held(session) && !order.contains(session)).cloned().collect();
        order.extend(rest);
        let paused: Vec<LivePaused> = order.iter().filter_map(|session| {
            let entry = pauses.paused.get(session)?;
            let next = snapshot.queue.first().is_some_and(|head| head.key == Key::Resume(session.clone()))
                && matches!(snapshot.hold, None | Some(Hold::Spacing { .. }));
            Some(LivePaused {
                task_id: entry.task.0.clone(),
                session: Some(session.clone()),
                at: crate::usage_pause::clock_text(entry.since, now, false),
                reason: match &entry.reason {
                    Reason::Memory => LivePauseReason::Memory,
                    Reason::Declared { .. } | Reason::Disk => LivePauseReason::Machine,
                    Reason::Usage => LivePauseReason::Usage,
                    Reason::ByUser => LivePauseReason::User,
                    Reason::ApiError(retry) if retry.outage => LivePauseReason::ApiOutage,
                    Reason::ApiError(_) => LivePauseReason::ApiError,
                },
                retry: match &entry.reason {
                    Reason::ApiError(retry) => Some(alondra_core::views::LiveRetry {
                        at: crate::usage_pause::clock_text(retry.at, now, false), attempt: retry.attempt,
                    }),
                    _ => None,
                },
                fine_minutes: None,
                next,
                waits_for: match &entry.reason {
                    Reason::Declared { reason } => Some(reason.clone()),
                    _ => None,
                },
            })
        }).chain(pauses.held.iter().map(|(task, held)| LivePaused {
            task_id: task.0.clone(),
            session: None,
            at: crate::usage_pause::clock_text(held.since, now, false),
            reason: LivePauseReason::User,
            fine_minutes: None,
            next: false,
            waits_for: None,
            retry: None,
        })).collect();
        Some(LivePacing {
            working: snapshot.working,
            limit: snapshot.limit,
            fixed: match snapshot.setting {
                NodeLimit::Auto => None,
                NodeLimit::Fixed(count) => Some(count),
            },
            auto: snapshot.limits.auto,
            most: snapshot.limits.most,
            memory_free: snapshot.mac.free_percent,
            tight: snapshot.mac.room(snapshot.in_flight, now).is_err(),
            queued,
            paused,
        })
    }
}

/// A node the disk paused has work under way: it goes back before anything new, whatever the priority.
fn disk_returns_first(queue: Vec<Waiting>, pauses: &Pauses) -> Vec<Waiting> {
    let (returning, rest): (Vec<Waiting>, Vec<Waiting>) = queue.into_iter().partition(|waiting| {
        matches!(&waiting.key, Key::Resume(session) if pauses.paused.get(session).is_some_and(|paused| paused.reason == Reason::Disk))
    });
    returning.into_iter().chain(rest).collect()
}

/// Whether `head` may start now, with everything the gate knows: the disk, the Mac, the limit and
/// the spacing of starts, which resumes keep too.
fn hold_of(inner: &Inner, head: &Waiting, snapshot: &Snapshot, now: Millis) -> Result<(), Hold> {
    if let Some(room) = &snapshot.disk {
        return Err(Hold::Disk { free: room.free, needed: room.needed, volume: room.volume.clone() });
    }
    let mac = if inner.ignores_mac { MacState::default() } else { snapshot.mac.clone() };
    decide(head, snapshot.working, snapshot.limit, &mac, snapshot.in_flight, inner.saved.last_admission, inner.tuning.spacing, now)
}

fn on_volume(volume: Option<&str>) -> String {
    volume.map(|volume| format!(" on {volume}")).unwrap_or_default()
}

fn reason_of(reason: &PauseReason) -> Reason {
    match reason {
        PauseReason::Memory => Reason::Memory,
        PauseReason::Declared { reason } => Reason::Declared { reason: reason.clone() },
        PauseReason::Usage => Reason::Usage,
        PauseReason::Disk => Reason::Disk,
        PauseReason::ByUser => Reason::ByUser,
        PauseReason::ApiError(retry) => Reason::ApiError(retry.clone()),
    }
}

fn pause_reason(reason: &Reason) -> PauseReason {
    match reason {
        Reason::Memory => PauseReason::Memory,
        Reason::Declared { reason } => PauseReason::Declared { reason: reason.clone() },
        Reason::Usage => PauseReason::Usage,
        Reason::Disk => PauseReason::Disk,
        Reason::ByUser => PauseReason::ByUser,
        Reason::ApiError(retry) => PauseReason::ApiError(retry.clone()),
    }
}

fn cleanup_of(cleanup: PauseCleanup) -> crate::pauses::Cleanup {
    match cleanup {
        PauseCleanup::Pending => crate::pauses::Cleanup::Pending,
        PauseCleanup::Terminated => crate::pauses::Cleanup::Terminated,
        PauseCleanup::Done => crate::pauses::Cleanup::Done,
    }
}

fn pause_cleanup(cleanup: crate::pauses::Cleanup) -> PauseCleanup {
    match cleanup {
        crate::pauses::Cleanup::Pending => PauseCleanup::Pending,
        crate::pauses::Cleanup::Terminated => PauseCleanup::Terminated,
        crate::pauses::Cleanup::Done => PauseCleanup::Done,
    }
}

/// The paused nodes as the node machine holds them, by session, with the task each one keeps.
fn domain_paused(state: &DomainState) -> BTreeMap<String, Paused> {
    state.paused_nodes().into_iter().filter_map(|(agent, pause)| {
        let task = state.node_task(&agent).filter(|task| !task.over())?.id.clone();
        Some((agent.0.clone(), Paused {
            task, reason: reason_of(&pause.reason), since: pause.since, cut: pause.cut.clone(), turn_since: pause.turn_since,
            cleanup: cleanup_of(pause.cleanup), released: pause.released,
        }))
    }).collect()
}

/// Turns the pauses the gate wants into events of the node machine: a new pause, its cleanup, its
/// release, a resume. The machine refuses what cannot be (pausing a node that waits on the user),
/// and the gate reads back what it accepted. A node of the Agent SDK has no processes of its own
/// to clean up, and its task's lifecycle keeps only that it is paused. Returns the resumes the
/// machine refused, with the revision of their task's lifecycle.
fn hand_pauses(domain: &DomainRuntime, wanted: &BTreeMap<String, Paused>) -> BTreeMap<String, u64> {
    let Ok(state) = domain.state() else { return BTreeMap::new() };
    let held = domain_paused(&state);
    let mut events = vec![];
    for (session, paused) in wanted {
        match held.get(session) {
            Some(_) if state.through_sdk(&AgentId(session.clone())) => {}
            None => events.push((session, NodeEvent::Paused { pause: Pause {
                reason: pause_reason(&paused.reason), since: paused.since, cut: paused.cut.clone(), turn_since: paused.turn_since,
                cleanup: pause_cleanup(paused.cleanup), released: paused.released,
            } })),
            Some(known) => {
                if known.cleanup != paused.cleanup {
                    events.push((session, NodeEvent::PauseCleanup { cleanup: pause_cleanup(paused.cleanup) }));
                }
                if paused.released && !known.released {
                    events.push((session, NodeEvent::PauseReleased));
                }
            }
        }
    }
    for session in held.keys().filter(|session| !wanted.contains_key(*session)) {
        events.push((session, NodeEvent::Resumed));
    }
    let ended = events.iter().any(|(_, event)| matches!(event, NodeEvent::Resumed | NodeEvent::PauseReleased));
    let mut resumed = vec![];
    let mut refused = BTreeMap::new();
    for (session, event) in events {
        let key = event.key();
        let resume = matches!(event, NodeEvent::Resumed);
        let resumes = resume && state.through_sdk(&AgentId(session.clone()));
        match crate::domain_agent_lifecycle::node_event(domain, &AgentId(session.clone()), event) {
            Ok(true) if resumes => resumed.extend(open_task_of(&state, session)),
            Ok(_) => {}
            Err(error) => {
                log!("could not record that {session} {key}: {error}");
                if let Some(revision) = resume.then(|| lifecycle_revision(&state, session)).flatten() {
                    refused.insert(session.clone(), revision);
                }
            }
        }
    }
    if ended {
        domain.seals.wake();
    }
    for task in resumed {
        crate::sdk_owner::take_if_unowned(domain, &task);
    }
    refused
}

/// The revision of the lifecycle of the open task of the node `session`.
fn lifecycle_revision(state: &DomainState, session: &str) -> Option<u64> {
    state.lifecycle_of(&open_task_of(state, session)?).ok().map(|lifecycle| lifecycle.revision())
}

/// A paused node's turn came: it is told to carry on, or brought back first if it is not connected.
fn resume_locked(domain: &DomainRuntime, inner: &mut Inner, session: &str, now: Millis) {
    let Some(mut paused) = inner.saved.pauses.paused.remove(session) else { return };
    if let Reason::ApiError(retry) = &paused.reason && retry.outage {
        domain.live.api_watch.probed(now);
    }
    if inner.saved.pauses.resumed.remove(session).is_some() {
        paused.reason = Reason::ByUser;
    }
    log!("resuming {session} ({}), paused since {}", paused.task, paused.since);
    if paused.reason == Reason::Disk {
        return;
    }
    let text = crate::pauses::resume_text(&paused, now);
    if !tell_to_go_on(domain, session, &text) {
        inner.saved.pauses.owed.insert(session.to_owned(), text);
        domain.live.losses.report(session);
    }
}

/// Tells a node it goes on: as an input of its Agent SDK task, which its host takes once the pause
/// ends, or down the connection of a `claude --bg` node.
fn tell_to_go_on(domain: &DomainRuntime, session: &str, text: &str) -> bool {
    if crate::pauses::through_sdk(domain, session) {
        let Some(task) = domain.state().ok().and_then(|state| open_task_of(&state, session)) else { return true };
        if waiting_input(domain, &task) {
            return true;
        }
        let command = format!("resume-{session}-{}", crate::auth::random_hex(8));
        return crate::domain_messages::tell_sdk_node(domain, &task, crate::node_push::ALONDRA, text, &command, false).is_ok();
    }
    crate::sdk_handover::tell(domain, session, crate::node_push::ALONDRA, text, true).is_ok()
}

/// Whether the task of a node of the Agent SDK already has what it goes on with, as a pause by the
/// user leaves it.
fn waiting_input(domain: &DomainRuntime, task: &alondra_core::domain::TaskId) -> bool {
    let read = domain.read();
    read.state().lifecycle_of(task).ok()
        .is_some_and(|lifecycle| !lifecycle.pending_inputs().is_empty())
}

/// Tells the nodes admitted back while unreachable to carry on, once they are connected again.
fn deliver_owed(domain: &DomainRuntime, inner: &mut Inner) {
    inner.saved.pauses.owed.retain(|session, text| !tell_to_go_on(domain, session, text));
}

fn admit(inner: &mut Inner, key: Key, now: Millis) {
    inner.admitted.retain(|(_, at)| now - at < IN_FLIGHT);
    inner.at_work.remove(&key);
    let admitted: BTreeSet<&Key> = inner.admitted.iter().map(|(key, _)| key).collect();
    inner.at_work.retain(|key| admitted.contains(key));
    inner.admitted.push((key, now));
    inner.saved.last_admission = Some(now);
}

/// Remembers the resumes the gate let through whose node now holds a slot of its own.
fn note_at_work(inner: &mut Inner, state: &DomainState) {
    let busy = working_tasks(state);
    let seen: Vec<Key> = inner.admitted.iter()
        .filter(|(key, _)| matches!(key, Key::Resume(_)) && task_of(state, key).is_some_and(|task| busy.contains(&task)))
        .map(|(key, _)| key.clone()).collect();
    inner.at_work.extend(seen);
}

fn live_hold(hold: &Hold) -> LiveHold {
    match hold {
        Hold::Slots { working, limit } => LiveHold::Slots { working: *working, limit: *limit },
        Hold::Memory { free, .. } => LiveHold::Memory { free: *free },
        Hold::Spacing { .. } => LiveHold::Next,
        Hold::Disk { free, needed, volume } => LiveHold::Disk { free: *free, needed: *needed, volume: volume.clone() },
    }
}

fn key_text(state: &DomainState, key: &Key) -> String {
    match key {
        Key::Task(task) => task.0.clone(),
        Key::Run(run) => format!("scheduled run {run}"),
        Key::Resume(session) => format!("{} coming back", open_task_of(state, session).map_or_else(|| session.clone(), |task| task.0)),
    }
}

/// Why the queue does not move, worded for the log; none for the minute between two starts.
pub(crate) fn hold_line(queued: usize, head: &str, priority: TaskPriority, hold: &Hold, working: &[TaskId], on_user: &[TaskId], paused: usize) -> Option<String> {
    let list = |tasks: &[TaskId]| tasks.iter().map(|task| task.0.as_str()).collect::<Vec<_>>().join(", ");
    let why = match hold {
        Hold::Spacing { .. } => return None,
        Hold::Slots { working: count, limit } => {
            let starting = count.saturating_sub(working.len());
            let mut why = format!("{count} of {limit} working ({}{})", list(working),
                if starting > 0 { format!(", {starting} starting") } else { String::new() });
            if !on_user.is_empty() {
                why.push_str(&format!("; waiting on the user, not counted: {}", list(on_user)));
            }
            why
        }
        Hold::Memory { why, free } => format!("memory has no room for one more node: {why} ({} free)", free.map_or_else(|| "unknown".into(), |free| format!("{free}%"))),
        Hold::Disk { free, needed, volume } => format!("the disk has no room for one more build: {} free of {} needed{}", bytes_text(*free, Lang::En), bytes_text(*needed, Lang::En), on_volume(volume.as_deref())),
    };
    let paused = if paused > 0 { format!("; {paused} node(s) paused") } else { String::new() };
    Some(format!("the admission gate holds {queued} waiting (next {head}, {priority:?}): {why}{paused}"))
}

fn task_of(state: &DomainState, key: &Key) -> Option<TaskId> {
    match key {
        Key::Task(task) => Some(task.clone()),
        Key::Resume(session) => open_task_of(state, session),
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

/// The open task a session's node works on.
pub(crate) fn open_task_of(state: &DomainState, session: &str) -> Option<TaskId> {
    let agent = alondra_core::domain::AgentId(session.to_owned());
    state.tasks.values().find(|task| task.agent_id.as_ref() == Some(&agent) && !task.archived && !task.state.terminal()
        && !matches!(task.state, TaskState::Closing)).map(|task| task.id.clone())
}

/// A start still worth waiting for: its task is open and has no live node, its run still waits
/// to launch, or its node still has an open task.
fn still_waits(state: &DomainState, key: &Key) -> bool {
    match key {
        Key::Resume(session) => open_task_of(state, session).is_some(),
        Key::Task(task) => state.tasks.get(task).is_some_and(|task| !task.archived && !task.state.terminal()
            && !matches!(task.state, TaskState::Closing)
            && task.agent_id.as_ref().is_none_or(|agent| !state.agents.get(agent).is_some_and(|agent| agent.phase.live() && !agent.phase.coming_back()))),
        Key::Run(run) => state.scheduled_launch_guard(&run.as_str().into()).is_ok(),
    }
}

/// The tasks whose node takes a slot, as the node machine says: the one count the gate, the
/// Tasks list and the header share. A paused node, one that waits on the user and one whose session
/// was lost and waits for the gate hold none, whatever their session still reports; one the gate let
/// back and Alondra is restarting holds one, and so does one the disk's floor held until the turn it
/// runs, and the work of its own it left running, end.
pub(crate) fn working_tasks(state: &DomainState) -> BTreeSet<TaskId> {
    state.slots().into_iter()
        .filter_map(|agent| state.node_task(&agent.id).filter(|task| !task.over()).map(|task| task.id.clone()))
        .chain(turns_held_for_disk(state))
        .collect()
}

fn turns_held_for_disk(state: &DomainState) -> impl Iterator<Item = TaskId> + '_ {
    state.paused_nodes().into_iter().filter(|(agent, pause)| pause.reason == PauseReason::Disk && state.through_sdk(agent))
        .filter_map(|(agent, _)| state.node_task(&agent).filter(|task| !task.over()).map(|task| task.id.clone()))
        .filter(|task| state.lifecycle_of(task).is_ok_and(|lifecycle| matches!(lifecycle.activity(), alondra_domain::ActivityView::Working | alondra_domain::ActivityView::OwnWork)))
}

/// `working_tasks` with the project each one works on.
pub(crate) fn working_projects(state: &DomainState) -> BTreeMap<TaskId, Option<String>> {
    projects_of(state, working_tasks(state).iter())
}

/// Those of `tasks` the disk is reserved for: a paused node reserves nothing, whatever its held turn
/// still runs, so the nodes the disk stopped never keep each other from coming back.
pub(crate) fn reserving_tasks<'a>(state: &DomainState, tasks: impl Iterator<Item = &'a TaskId>) -> BTreeMap<TaskId, Option<String>> {
    let held: BTreeSet<TaskId> = turns_held_for_disk(state).collect();
    projects_of(state, tasks.filter(|task| !held.contains(*task)))
}

fn projects_of<'a>(state: &DomainState, tasks: impl Iterator<Item = &'a TaskId>) -> BTreeMap<TaskId, Option<String>> {
    tasks.map(|task| (task.clone(), state.tasks.get(task).and_then(|task| task.project.clone()))).collect()
}

/// The disk fell under its floor: the next turn of every node at work but the one that took least
/// waits for room, with no card, and comes back through the queue. The turns they run go on, and a
/// node of `claude --bg`, whose next turn cannot wait, goes on too.
pub(crate) fn hold_for_disk(domain: &DomainRuntime, now: Millis) {
    let Ok(state) = domain.state() else { return };
    let gate = &domain.live.gate;
    let pauses = gate.pauses(domain);
    let at_work: Vec<crate::disk_peaks::AtWork> = working_tasks(&state).into_iter().filter_map(|task| {
        let session = crate::pauses::session_of(&state, &task)?;
        Some(crate::disk_peaks::AtWork {
            used: domain.live.machine_watch.disk_used(&task),
            can_wait: crate::pauses::through_sdk(domain, &session),
            held: pauses.paused.contains_key(&session),
            task,
        })
    }).collect();
    for task in crate::disk_peaks::to_hold(&at_work) {
        let Some(session) = crate::pauses::session_of(&state, &task) else { continue };
        log!("the disk is under its floor: the next turn of {session} ({task}) waits for room");
        gate.hold_back(domain, &session, Reason::Disk, now);
    }
}

/// The queued launches whose turn may come: one whose start failed waits out its retry or the
/// user's answer to its card where it is, keeping its place without holding back the rest.
fn queued_launches(domain: &DomainRuntime, state: &DomainState, now: Millis) -> Vec<Waiting> {
    let store = domain.live_read();
    let rows = store.launch_queue().unwrap_or_default();
    if rows.is_empty() {
        return vec![];
    }
    let retries = store.start_retries().unwrap_or_default();
    let blocked: BTreeSet<TaskId> = store.auto_launches().unwrap_or_default().into_iter()
        .filter(|row| row.state == AutoLaunchState::Blocked && row.reason != Some(AutoLaunchReason::UsageExhausted))
        .map(|row| row.task_id).collect();
    drop(store);
    rows.into_iter()
        .filter(|row| !blocked.contains(&row.task_id) && retries.get(&row.task_id).is_none_or(|(_, at)| *at <= now)
            && !state.plan_review_pending(&row.task_id))
        .map(|row| Waiting {
        priority: priority_of(state, Some(&row.task_id)),
        key: Key::Task(row.task_id),
        resuming: false,
        since: row.enqueued_at,
    }).collect()
}

/// What `options.toml` sets and what this Mac takes with its nodes measured at what `mac` says.
fn node_limits(domain: &DomainRuntime, mac: &MacState) -> (NodeLimit, NodeLimits) {
    let file = crate::domain_schedule_runner::options_at(domain.home()).unwrap_or_default();
    let (cores, memory) = machine_size();
    (file.max_working_nodes, NodeLimits::of(cores, memory, mac.node_cost.unwrap_or(crate::node_cost::DEFAULT)))
}

/// The highest number Settings offers and `set_limit` takes on this Mac.
pub fn most_working_nodes() -> u32 {
    let (cores, memory) = machine_size();
    NodeLimits::of(cores, memory, crate::node_cost::DEFAULT).most
}

fn limit_setting(setting: NodeLimit, limits: NodeLimits, tuning: Tuning) -> Limit {
    match (setting, tuning.limit) {
        (NodeLimit::Fixed(_), _) => Limit::Fixed(limits.applied(setting)),
        (NodeLimit::Auto, Some(limit)) => limit,
        (NodeLimit::Auto, None) => Limit::Auto { max: limits.auto },
    }
}

/// Writes `max_working_nodes` into `options.toml`, keeping the rest of the file; the gate
/// rereads it on its next decision.
pub fn set_node_limit(home: &Path, limit: NodeLimit) -> Result<(), &'static str> {
    let path = alondra_core::options::path_in(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err("Alondra could not read options.toml"),
    };
    let written = alondra_core::options::with_node_limit(&text, limit).map_err(|_| "options.toml has an error: fix it first")?;
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, written).and_then(|()| std::fs::rename(&temporary, &path))
        .map_err(|_| "Alondra could not write options.toml")
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
    let busy = working_tasks(state);
    let paused = |task: &TaskId| crate::pauses::session_of(state, task).is_some_and(|session| inner.saved.pauses.paused.contains_key(&session));
    let waiting_again: BTreeSet<&Key> = queued.iter()
        .filter(|waiting| !matches!(&waiting.key, Key::Task(task) if domain.live.is_launching(&task.0)))
        .map(|waiting| &waiting.key).collect();
    let in_flight: Vec<Option<TaskId>> = inner.admitted.iter()
        .filter(|(key, at)| now - at < IN_FLIGHT && !waiting_again.contains(key) && !inner.at_work.contains(key))
        .filter_map(|(key, _)| {
            let task = task_of(state, key);
            let counts = match (key, &task) {
                (Key::Resume(_), None) => false,
                (Key::Resume(session), Some(task)) => !busy.contains(task) && !paused(task)
                    && state.tasks.get(task).is_some_and(|task| !task.over())
                    && !state.node_waits_on_user(&AgentId(session.clone())),
                (_, None) => true,
                (_, Some(task)) => !busy.contains(task) && !paused(task)
                    && state.tasks.get(task).is_none_or(|task| !task.over() && task.agent_id.is_none()),
            };
            counts.then_some(task)
        })
        .collect();
    // A start counts until its task has a node, and a resume until its node is seen at work; from
    // then on the node's own phase says whether it takes a slot, as a node of the Agent SDK that
    // ends its first turn on a card within seconds.
    let working = busy.len() + in_flight.len();
    let admitted = inner.admitted.iter().filter(|(_, at)| now - at < IN_FLIGHT).count();
    let working_tasks: BTreeSet<TaskId> = busy.into_iter().chain(in_flight.into_iter().flatten()).collect();
    let mut all: BTreeMap<Key, Waiting> = inner.asking.iter().filter(|(key, _)| still_waits(state, key))
        .map(|(key, waiting)| {
            let priority = match key {
                Key::Task(task) => priority_of(state, Some(task)),
                _ => waiting.priority,
            };
            (key.clone(), Waiting { priority, ..waiting.clone() })
        }).collect();
    for waiting in queued.into_iter().filter(|waiting| still_waits(state, &waiting.key)) {
        all.insert(waiting.key.clone(), waiting);
    }
    let usage_clear = !domain.live.usage_pause.is_active();
    let probe = crate::api_failures::outage_probe(&inner.saved.pauses.paused, &domain.live.api_watch, now);
    let resumed = &inner.saved.pauses.resumed;
    let returning = inner.saved.pauses.paused.iter()
        .filter(|(_, paused)| !(paused.reason == Reason::Disk && working_tasks.contains(&paused.task)))
        .filter(|(session, _)| inner.refused.get(*session).is_none_or(|refused| lifecycle_revision(state, session) != Some(*refused)))
        .filter(|(session, paused)| resumed.contains_key(*session) || match &paused.reason {
        Reason::ApiError(retry) if retry.outage => domain.live.api_watch.answered_since(paused.since) || probe.as_ref() == Some(*session),
        _ => paused.returns(usage_clear && (!matches!(paused.reason, Reason::Usage)
            || crate::usage_pause::task_usage_ready(state, &paused.task, paused.since, now)), domain.live.api_watch.answered_since(paused.since), now),
    });
    for (session, paused) in returning {
        let key = Key::Resume(session.clone());
        if still_waits(state, &key) {
            all.insert(key.clone(), Waiting { key, priority: priority_of(state, Some(&paused.task)), resuming: true, since: paused.since });
        }
    }
    let queue = disk_returns_first(ordered(all.into_values().collect()), &inner.saved.pauses);
    let mac = domain.live.machine_watch.mac();
    let (setting, limits) = node_limits(domain, &mac);
    let limit = limit_setting(setting, limits, inner.tuning).count();
    let head = queue.first().and_then(|head| task_of(state, &head.key));
    let reserving = reserving_tasks(state, working_tasks.iter().filter(|task| Some(*task) != head.as_ref()));
    let project = head.and_then(|task| state.tasks.get(&task)).and_then(|task| task.project.as_deref());
    let disk = domain.live.machine_watch.disk_room(&reserving, project, now);
    let disk_detail = domain.live.machine_watch.disk_detail(&reserving, project, now);
    let mut snapshot = Snapshot { queue, working, working_tasks, limit, setting, limits, mac, in_flight: admitted, hold: None, disk, disk_detail };
    snapshot.hold = snapshot.queue.first().and_then(|head| hold_of(inner, head, &snapshot, now).err());
    snapshot
}

/// The user paused a queued task: it leaves the queue, and only the user brings it back. It waits
/// in the durable `launch_queue`, or at the gate for its automatic start with its launch plan.
pub fn hold_queued(domain: &DomainRuntime, task: &TaskId, now: Millis) -> Result<(), &'static str> {
    const NOT_QUEUED: &str = "This task is not queued";
    let mut store = domain.store.lock_or_exit("the domain store");
    let row = store.launch_queue().ok().and_then(|rows| rows.into_iter().find(|row| &row.task_id == task));
    let (body, origin) = match row {
        Some(row) => {
            store.dequeue_launch(task).map_err(|_| NOT_QUEUED)?;
            (row.body, row.origin)
        }
        None => {
            let plan = store.launch_plan(task).ok().flatten().ok_or(NOT_QUEUED)?;
            drop(store);
            let state = domain.state().map_err(|_| NOT_QUEUED)?;
            let key = Key::Task(task.clone());
            let waits = domain.live.gate.lock().asking.contains_key(&key)
                && state.tasks.get(task).is_some_and(|task| crate::auto_launch::candidate(&state, task));
            if !waits {
                return Err(NOT_QUEUED);
            }
            (LaunchBody::from_plan(task.clone(), &plan).to_queue(), AUTO_ORIGIN.to_owned())
        }
    };
    let gate = &domain.live.gate;
    let mut inner = gate.lock();
    inner.asking.remove(&Key::Task(task.clone()));
    inner.saved.pauses.held.insert(task.clone(), crate::pauses::Held { body, origin, since: now });
    gate.save(&inner);
    drop(inner);
    gate.poke();
    Ok(())
}

const AUTO_ORIGIN: &str = "auto";

/// A task the user paused in the queue: nothing starts it until the user resumes it.
pub(crate) fn held_by_user(domain: &DomainRuntime, task: &TaskId) -> bool {
    domain.live.gate.lock().saved.pauses.held.contains_key(task)
}

/// The user resumed a queued task they paused: it goes back to the queue with its age, and the
/// gate starts it when its turn comes.
pub fn release_queued(domain: &DomainRuntime, task: &TaskId) -> Result<(), &'static str> {
    let gate = &domain.live.gate;
    let held = gate.lock().saved.pauses.held.get(task).cloned().ok_or("This node or task is not paused")?;
    domain.store.lock_or_exit("the domain store").enqueue_launch(task, &held.body, &held.origin, held.since)
        .map_err(|_| "Alondra could not queue the task")?;
    let mut inner = gate.lock();
    inner.saved.pauses.held.remove(task);
    gate.save(&inner);
    drop(inner);
    gate.poke();
    Ok(())
}

/// Queues a launch the orchestrator or the user asked for; the gate starts it when its turn comes.
pub(crate) fn enqueue(domain: &DomainRuntime, body: &LaunchBody, origin: &str, now: Millis) -> anyhow::Result<()> {
    domain.store.lock_or_exit("the domain store").enqueue_launch(body.task_id(), &body.to_queue(), origin, now)?;
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
        LiveHold::Disk { free, needed, volume } => format!("the disk has no room for one more build ({} free of {} needed{})", bytes_text(*free, Lang::En), bytes_text(*needed, Lang::En), on_volume(volume.as_deref())),
    };
    format!("It waits in the queue, {} of {}: {why}. Alondra starts it by itself when its turn comes; you do not need to launch it again.",
        place.position, place.of)
}

/// Starts the queued launches whose turn came, and wakes the callers that asked the gate.
pub async fn run(domain: Arc<DomainRuntime>, home: PathBuf) {
    let mut changes = domain.subscribe();
    loop {
        if let Err(error) = pass(&domain, &home).await {
            log!("the admission gate could not start a queued launch: {error:#}");
        }
        tokio::select! {
            () = tokio::time::sleep(TICK) => {}
            () = domain.live.gate.changed.notified() => {}
            changed = changes.changed() => if changed.is_err() { return },
        }
    }
}

/// One look at the queue: drops what no longer waits, and lets the head start.
pub async fn pass(domain: &DomainRuntime, home: &Path) -> anyhow::Result<()> {
    pass_at(domain, home, alondra_core::now_millis()).await
}

/// `pass` at `now`: looks at the pauses, then lets the head of the queue go, a paused node by
/// telling it to carry on. It never pauses a working node.
pub(crate) async fn pass_at(domain: &DomainRuntime, home: &Path, now: Millis) -> anyhow::Result<()> {
    let state = domain.state()?;
    let rows = domain.store.lock_or_exit("the domain store").launch_queue()?;
    for row in rows.iter().filter(|row| !still_waits(&state, &Key::Task(row.task_id.clone()))) {
        domain.store.lock_or_exit("the domain store").dequeue_launch(&row.task_id)?;
    }
    let gate = &domain.live.gate;
    let to_halt;
    let head = {
        let queued = queued_launches(domain, &state, now);
        let mut inner = gate.lock_with(domain);
        inner.asking.retain(|key, _| still_waits(&state, key));
        crate::pauses::look(domain, &state, &mut inner.saved.pauses, now);
        deliver_owed(domain, &mut inner);
        note_at_work(&mut inner, &state);
        let snapshot = snapshot_locked(domain, &state, &inner, queued, now);
        gate.save_with(domain, &mut inner);
        let held = snapshot.queue.first().zip(snapshot.hold.as_ref());
        if inner.told.due(held.and_then(|(_, hold)| Cause::of(hold)), now) && let Some((head, hold)) = held {
            let working: Vec<TaskId> = working_tasks(&state).into_iter().collect();
            let on_user: Vec<TaskId> = state.agents.values().filter(|agent| state.node_waits_on_user(&agent.id))
                .filter_map(|agent| state.node_task(&agent.id).map(|task| task.id.clone())).collect();
            if let Some(line) = hold_line(snapshot.queue.len(), &key_text(&state, &head.key), head.priority, hold, &working, &on_user, inner.saved.pauses.paused.len()) {
                let detail = snapshot.disk_detail.as_deref().filter(|_| matches!(hold, Hold::Disk { .. }))
                    .map(|detail| format!("; {detail}")).unwrap_or_default();
                log!("{line}{detail}");
            }
        }
        let head = match (snapshot.queue.first(), snapshot.hold) {
            (Some(head), None) if inner.asking.contains_key(&head.key) => {
                gate.turn.send_modify(|turn| *turn += 1);
                None
            }
            (Some(Waiting { key: Key::Resume(session), .. }), None) => {
                let session = session.clone();
                resume_locked(domain, &mut inner, &session, now);
                admit(&mut inner, Key::Resume(session), now);
                gate.save_with(domain, &mut inner);
                None
            }
            (Some(head), None) => {
                let key = head.key.clone();
                admit(&mut inner, key.clone(), now);
                gate.save_with(domain, &mut inner);
                Some(key)
            }
            _ => None,
        };
        to_halt = crate::pauses::to_halt(domain, &state, &inner.saved.pauses);
        head
    };
    for (session, paused) in to_halt {
        crate::pauses::hold_sdk_turn(domain, &session, &paused);
    }
    send_back_stranded(domain, &state, now);
    let Some(Key::Task(task)) = head else { return Ok(()) };
    let Some(row) = domain.store.lock_or_exit("the domain store").launch_queue()?.into_iter().find(|row| row.task_id == task) else {
        return Ok(());
    };
    let body = LaunchBody::from_queue(&row.body)?;
    let choices = |request: &alondra_core::domain::RequestId| request_choices(domain, request).ok().flatten();
    log!("the admission gate starts {task}, queued since {}", row.enqueued_at);
    match launch_from(domain, home, body, choices, Origin::Admitted).await {
        Ok(_) => {
            let mut store = domain.store.lock_or_exit("the domain store");
            store.dequeue_launch(&task)?;
            store.set_start_retry(&task, None)?;
        }
        Err(_) if crate::start_failures::ended(domain, &task)? => {}
        Err(error) => {
            log!("the admission gate could not start {task}: {}", error.message);
            refused(domain, &task, &error, now)?;
        }
    }
    Ok(())
}

/// A task only waits for capacity or the user: one stranded past `STRANDED_AFTER` goes back through
/// the gate like a node that resumes, with no card, and waits there while the Mac has no room.
fn send_back_stranded(domain: &DomainRuntime, state: &DomainState, now: Millis) {
    let gate = &domain.live.gate;
    let stranded = state.stranded_tasks();
    let due: Vec<(TaskId, Millis)> = {
        let mut inner = gate.lock_with(domain);
        let over: Vec<TaskId> = inner.stranded.keys().filter(|task| !stranded.contains(*task)).cloned().collect();
        for task in over {
            inner.stranded.remove(&task);
            if let Some(session) = crate::pauses::session_of(state, &task) {
                inner.asking.remove(&Key::Resume(session));
            }
        }
        stranded.into_iter().filter_map(|task| {
            let since = *inner.stranded.entry(task.clone()).or_insert(now);
            (now - since >= STRANDED_AFTER).then_some((task, since))
        }).collect()
    };
    for (task, since) in due {
        let Some(session) = crate::pauses::session_of(state, &task) else { continue };
        let waiting = Waiting { key: Key::Resume(session), priority: priority_of(state, Some(&task)), resuming: true, since };
        if gate.ask(domain, waiting, now).is_err() {
            continue;
        }
        gate.lock().stranded.remove(&task);
        log!("{task} has work its node owes and nothing to start a turn: it goes back to work");
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("stranded-{task}-{}", crate::auth::random_hex(8)).into(),
            expected_revision: 0,
            command: Command::RecordStranded { task_id: task.clone() },
        };
        match domain.execute_as_system(envelope) {
            Ok(_) => crate::sdk_owner::take_if_unowned(domain, &task),
            Err(error) => log!("could not send {task} back to work: {error:?}"),
        }
    }
}

/// A launch the gate let through and that did not start stays in the queue: a refusal that clears
/// by itself (usage, disk) is tried again on a later turn, any other is a failed start, tried again
/// by itself and then put to the user. `launch_from` records the failures it knows.
fn refused(domain: &DomainRuntime, task: &TaskId, error: &alondra_core::domain::DomainError, now: Millis) -> anyhow::Result<()> {
    domain.live.gate.not_started(&Key::Task(task.clone()));
    if domain.live.is_launching(&task.0) || matches!(error.code.as_str(), "launch_failed" | "workspace_untrusted" | "usage_exhausted" | "disk_low") {
        return Ok(());
    }
    let state = domain.state()?;
    if !still_waits(&state, &Key::Task(task.clone())) {
        return Ok(());
    }
    crate::start_failures::failed(domain, task, alondra_core::domain::StartProblem::LaunchFailed { error: error.message.clone() }, now)
}

#[cfg(test)]
mod tests;
