//! What the engine asks of the admission gate, which `engine-admission` provides, and the values
//! both speak in: what waits at the gate, why it waits, and the pauses the gate keeps.
use crate::domain_runtime::DomainRuntime;
use alondra_core::Millis;
use alondra_domain_rules::{ApiRetry, DomainState, ScheduleRun, TaskId, TaskPriority, TaskState};
use alondra_daemon_engine_machine::machine_state::NoRoom;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::sync::watch;

/// The gate's file in Alondra's home, and the name an older daemon gave it.
pub const FILE: &str = "admission.json";
pub const PREVIOUS_FILE: &str = "fleet-gate.json";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Key {
    Task(TaskId),
    Run(String),
    /// A node coming back to work, by its session: paused, or revived after its connection was lost.
    Resume(String),
    /// The turn an input or an answer starts in a node that rests. It only needs a slot: its process
    /// exists or costs little to resume, and the user's answers do not go out one a minute.
    Turn(TaskId),
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    /// Only an older daemon paused for memory: such a node returns at once and comes back as soon
    /// as the gate admits it. Kept so the pauses it left can be read.
    Memory,
    /// The node said it waits for the machine.
    Declared { reason: String },
    Usage,
    ByUser,
    ApiError(ApiRetry),
    /// The disk fell under its floor while it built: its next turn waits for room.
    Disk,
}

impl Reason {
    /// A pause only Alondra or the user ends: a turn the node starts by itself is cut again and its
    /// tools are refused, since that turn is the node reacting to what the pause killed.
    pub fn holds(&self) -> bool {
        matches!(self, Reason::ByUser | Reason::Memory)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cleanup {
    #[default]
    Pending,
    Terminated,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paused {
    pub task: TaskId,
    pub reason: Reason,
    pub since: Millis,
    /// The command its turn was running when it was stopped.
    #[serde(default)]
    pub cut: Option<String>,
    /// When its current turn started: a later turn means it went on by itself.
    #[serde(default)]
    pub turn_since: Option<Millis>,
    #[serde(default)]
    pub cleanup: Cleanup,
    /// The user released it ("I've fixed it").
    #[serde(default)]
    pub released: bool,
}

impl Paused {
    /// Whether the gate may bring it back now: a pause by the user waits for the user, and an API
    /// error for its retry or for the model to answer another node. A declared wait for the
    /// machine returns once the gate's measures of memory, disk and CPU say there is room, however
    /// long or short it waited.
    pub fn returns(&self, usage_clear: bool, api_answered: bool, now: Millis) -> bool {
        match &self.reason {
            Reason::ByUser => false,
            Reason::Usage => usage_clear,
            Reason::ApiError(retry) => api_answered || now >= retry.at,
            Reason::Declared { .. } | Reason::Memory | Reason::Disk => true,
        }
    }
}

/// Everything about pauses the gate keeps across restarts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pauses {
    /// By session: what the node machine holds, read when the gate is locked. An older daemon
    /// saved them in the gate's file; they are still read once, never written.
    #[serde(default, skip_serializing)]
    pub paused: BTreeMap<String, Paused>,
    /// Sessions admitted back while unreachable: told to carry on once they connect again.
    #[serde(default)]
    pub owed: BTreeMap<String, String>,
    /// Queued launches the user paused, with what the queue held for them.
    #[serde(default)]
    pub held: BTreeMap<TaskId, Held>,
    /// Paused nodes the user resumed, by session, and when: they wait in the gate's queue like a
    /// start, whatever their pause waited for, and stay paused until the gate admits them.
    #[serde(default)]
    pub resumed: BTreeMap<String, Millis>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub body: serde_json::Value,
    pub origin: String,
    pub since: Millis,
}

/// The open task a session's node works on.
pub fn open_task_of(state: &DomainState, session: &str) -> Option<TaskId> {
    let agent = alondra_domain_rules::AgentId(session.to_owned());
    state.tasks.values().find(|task| task.agent_id.as_ref() == Some(&agent) && !task.archived && !task.state.terminal()
        && !matches!(task.state, TaskState::Closing)).map(|task| task.id.clone())
}

/// The priority a schedule asks its runs to start with: its `priority` option, else the priority of
/// the run's task, Normal without one.
pub fn run_priority(state: &DomainState, run: &ScheduleRun) -> TaskPriority {
    let chosen = state.schedule.schedules.get(&run.schedule_id).and_then(|schedule| match schedule.options.get("priority") {
        Some(alondra_core::options::Choice::Value(value)) => serde_json::from_value(serde_json::Value::String(value.clone())).ok(),
        _ => None,
    });
    chosen.unwrap_or_else(|| priority_of(state, run.task_id.as_ref()))
}

/// The priority of `task`, Normal without one.
pub fn priority_of(state: &DomainState, task: Option<&TaskId>) -> TaskPriority {
    task.and_then(|task| state.tasks.get(task)).map_or(TaskPriority::Normal, |task| task.priority)
}

/// What the rest of the engine asks of the admission gate. Each call names the runtime whose gate
/// it asks: the gate keeps its state there.
pub trait AdmissionGate: Send + Sync {
    /// Whether `waiting` may start now. If it may, the start is counted at once; if not, it waits in
    /// the queue and `changes` wakes its caller when its turn may have come.
    fn ask(&self, domain: &DomainRuntime, waiting: Waiting, now: Millis) -> Result<(), Hold>;
    /// A start that no longer waits: its task ended, or its caller gave up on it.
    fn forget(&self, domain: &DomainRuntime, key: &Key);
    /// A start the gate let through that did not start: it holds no slot.
    fn not_started(&self, domain: &DomainRuntime, key: &Key);
    /// Something the gate decides on changed.
    fn poke(&self, domain: &DomainRuntime);
    /// Bumped whenever a waiting start may go.
    fn changes(&self, domain: &DomainRuntime) -> watch::Receiver<u64>;
    /// Whether the node of `task` may go on with what waits for a slot, counted at once when it may.
    fn may_take_turn(&self, domain: &DomainRuntime, task: &TaskId, now: Millis) -> bool;
    /// Whether what `task` goes on with still waits for a slot.
    fn waits_for_slot(&self, domain: &DomainRuntime, state: &DomainState, task: &TaskId) -> bool;
    /// The owner of `task` ended: nothing of it holds a slot.
    fn owner_ended(&self, domain: &DomainRuntime, task: &TaskId);
    /// A node stopped for a reason of its own waits with it and comes back through the queue. False
    /// for a session with no open task.
    fn hold_back(&self, domain: &DomainRuntime, session: &str, reason: Reason, now: Millis) -> bool;
    /// The user paused a working node: it stops at once and only the user brings it back.
    fn pause_by_user(&self, domain: &DomainRuntime, session: &str, now: Millis) -> Result<(), &'static str>;
    /// The user resumed a paused node: it waits in the queue like a start. False if it is not paused.
    fn resume_now(&self, domain: &DomainRuntime, session: &str, now: Millis) -> bool;
    /// The user wrote to a `claude --bg` node a holding pause stopped: the pause ends.
    fn release_by_user_message(&self, domain: &DomainRuntime, session: &str);
    fn paused(&self, domain: &DomainRuntime, session: &str) -> Option<Paused>;
    fn paused_by_user(&self, domain: &DomainRuntime, session: &str) -> bool;
    /// The reason of a pause that holds the node until Alondra or the user ends it, if it has one.
    fn holding(&self, domain: &DomainRuntime, session: &str) -> Option<Reason>;
    fn pauses(&self, domain: &DomainRuntime) -> Pauses;
    /// A queued task the user paused: nothing starts it until the user resumes it.
    fn held_by_user(&self, domain: &DomainRuntime, task: &TaskId) -> bool;
    /// The place and the wait of a queued task, worded for the orchestrator.
    fn queued_text(&self, domain: &DomainRuntime, task: &TaskId, now: Millis) -> String;
    /// Why no node may start in `cwd` now, if the disk is nearly full there or on the Mac.
    fn refuse_launch(&self, domain: &DomainRuntime, cwd: &Path) -> Option<String>;
    /// Whether the Mac's disk is under its floor.
    fn disk_low(&self, domain: &DomainRuntime) -> bool;
    /// Takes `machine` as a sample of the Mac and `disk` as its own disk's space: a test's fixture
    /// makes the Mac healthy, short or full with it.
    #[cfg(any(test, feature = "test-hooks"))]
    fn measure_machine(&self, _domain: &DomainRuntime, _machine: &alondra_core::Machine, _disk: alondra_daemon_support::disk::Space) {}
}
