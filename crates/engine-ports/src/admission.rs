//! What the engine asks of the admission gate, which `engine-admission` provides, and the values
//! both speak in: what waits at the gate, why it waits, and the pauses the gate keeps.
use crate::Runtime;
use crate::machine::NoRoom;
use alondra_core::Millis;
use alondra_domain_rules::{AgentId, ApiRetry, DomainError, DomainState, NodeEvent, ScheduleRun, TaskId, TaskPriority, TaskState};
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
    /// No room on the Mac's disk for one more build.
    Disk { free: u64, needed: u64 },
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
    /// Queued launches the user paused, with what the queue held for them, as an older daemon
    /// saved them: still read once and handed to the domain, never written.
    #[serde(default, skip_serializing)]
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

/// Whether `session`'s node wrote the card `id`.
pub fn asked_by(state: &DomainState, id: &alondra_domain_rules::InteractionId, session_id: &str) -> bool {
    state.interactions.get(id)
        .is_some_and(|interaction| matches!(&interaction.author, alondra_domain_rules::Author::Agent { agent_id } if agent_id.0 == session_id))
}

/// The session has a held card still open: it waits for the user, however silent it is. Read
/// from the domain, so it holds across a daemon restart that forgot which call the session is in.
pub fn waits_on_user(state: &DomainState, session_id: &str) -> bool {
    state.holds.keys().any(|id| asked_by(state, id, session_id) && state.interactions.get(id).is_some_and(|interaction| {
        !interaction.withdrawn && matches!(interaction.payload, alondra_domain_rules::InteractionPayload::Question { answer: None, .. })
    }))
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
    fn ask(&self, runtime: &dyn Runtime, waiting: Waiting, now: Millis) -> Result<(), Hold>;
    /// A start that no longer waits: its task ended, or its caller gave up on it.
    fn forget(&self, runtime: &dyn Runtime, key: &Key);
    /// A start the gate let through that did not start: it holds no slot.
    fn not_started(&self, runtime: &dyn Runtime, key: &Key);
    /// Something the gate decides on changed.
    fn poke(&self, runtime: &dyn Runtime);
    /// Bumped whenever a waiting start may go.
    fn changes(&self, runtime: &dyn Runtime) -> watch::Receiver<u64>;
    /// Whether the node of `task` may go on with what waits for a slot, counted at once when it may.
    fn may_take_turn(&self, runtime: &dyn Runtime, task: &TaskId, now: Millis) -> bool;
    /// Whether what `task` goes on with still waits for a slot.
    fn waits_for_slot(&self, runtime: &dyn Runtime, state: &DomainState, task: &TaskId) -> bool;
    /// The owner of `task` ended: nothing of it holds a slot.
    fn owner_ended(&self, runtime: &dyn Runtime, task: &TaskId);
    /// A node stopped for a reason of its own waits with it and comes back through the queue. False
    /// for a session with no open task.
    fn hold_back(&self, runtime: &dyn Runtime, session: &str, reason: Reason, now: Millis) -> bool;
    /// The user paused a working node: it stops at once and only the user brings it back.
    fn pause_by_user(&self, runtime: &dyn Runtime, session: &str, now: Millis) -> Result<(), &'static str>;
    /// The user resumed a paused node: it waits in the queue like a start. False if it is not paused.
    fn resume_now(&self, runtime: &dyn Runtime, session: &str, now: Millis) -> bool;
    /// The user wrote to a `claude --bg` node a holding pause stopped: the pause ends.
    fn release_by_user_message(&self, runtime: &dyn Runtime, session: &str);
    fn paused(&self, runtime: &dyn Runtime, session: &str) -> Option<Paused>;
    fn paused_by_user(&self, runtime: &dyn Runtime, session: &str) -> bool;
    /// The reason of a pause that holds the node until Alondra or the user ends it, if it has one.
    fn holding(&self, runtime: &dyn Runtime, session: &str) -> Option<Reason>;
    fn pauses(&self, runtime: &dyn Runtime) -> Pauses;
    /// A queued task the user paused: nothing starts it until the user resumes it.
    fn held_by_user(&self, runtime: &dyn Runtime, task: &TaskId) -> bool;
    /// The place and the wait of a queued task, worded for the orchestrator.
    fn queued_text(&self, runtime: &dyn Runtime, task: &TaskId, now: Millis) -> String;
    /// Why no node may start in `cwd` now, if the disk is nearly full there or on the Mac.
    fn refuse_launch(&self, runtime: &dyn Runtime, cwd: &Path) -> Option<String>;
    /// Whether the Mac's disk is under its floor.
    fn disk_low(&self, runtime: &dyn Runtime) -> bool;
}

/// What a node's mod last reported, as the gate reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeReport {
    pub working_since: Option<Millis>,
    pub peer_pid: Option<u32>,
    /// The command its turn is running, in a short label.
    pub running: Option<String>,
}

/// What the admission gate asks of the rest of the engine, which `daemon-engine` provides.
pub trait AdmissionEngine: Send + Sync {
    fn report(&self, runtime: &dyn Runtime, session: &str) -> Option<NodeReport>;
    /// The turn the live view counts for `session`'s node.
    fn turn(&self, runtime: &dyn Runtime, session: &str) -> Option<u64>;
    /// Stops the turn of a `claude --bg` node down its connection. False when it is not connected.
    fn interrupt(&self, runtime: &dyn Runtime, session: &str) -> bool;
    fn is_closed(&self, runtime: &dyn Runtime, session: &str) -> bool;
    fn is_launching(&self, runtime: &dyn Runtime, task: &TaskId) -> bool;
    /// Whether new work waits for the usage pause.
    fn usage_paused(&self, runtime: &dyn Runtime) -> bool;
    fn api_watch<'a>(&self, runtime: &'a dyn Runtime) -> &'a crate::api::ApiWatch;
    /// Whether the account `task` runs on may take work again after the limit that paused it at `since`.
    fn usage_ready(&self, state: &DomainState, task: &TaskId, since: Millis, now: Millis) -> bool;
    /// Gives `text` from Alondra to the node of the Agent SDK of `task`, as an input with `command`;
    /// why not, when it could not.
    fn tell_node(&self, runtime: &dyn Runtime, task: &TaskId, text: &str, command: &str) -> Result<(), String>;
    /// Sends `text` from Alondra to `session`'s node down its connection, typed as the user's when
    /// `as_user` says so.
    fn tell_session(&self, runtime: &dyn Runtime, session: &str, text: &str, as_user: bool) -> bool;
    /// Records `event` on `agent`'s node. True when it changed the node.
    fn node_event(&self, runtime: &dyn Runtime, agent: &AgentId, event: NodeEvent) -> Result<bool, String>;
    /// Starts the launch the gate's queue kept, `queued` as it was stored.
    fn launch<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, queued: serde_json::Value) -> crate::hosts::Launched<'a>;
    /// Whether `task` ended, so a start of it that failed is no failure.
    fn start_ended(&self, runtime: &dyn Runtime, task: &TaskId) -> Result<bool, String>;
    /// A start of `task` failed with `error`: it is tried again by itself and then put to the user.
    fn start_failed(&self, runtime: &dyn Runtime, task: &TaskId, error: &DomainError, now: Millis) -> Result<(), String>;
}
