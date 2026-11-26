//! What the admission gate asks of the engine, answered from a script the test writes: what each
//! node's mod reports, which nodes are closed or launching, whether usage holds new work. It keeps
//! what the gate told it, and moves nodes and tells them in the real domain, as the engine does, so
//! a test reads the gate's decisions in the domain it opened.
use alondra_core::Millis;
use alondra_domain_model::{AgentId, Command, CommandEnvelope, DomainError, DomainState, NodeEvent, PROTOCOL_VERSION, TaskId};
use alondra_engine_ports::admission::{AdmissionEngine, NodeReport};
use alondra_engine_ports::api::ApiWatch;
use alondra_engine_ports::hosts::Launched;
use alondra_engine_ports::{Registry, Runtime};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// What the engine answers, and what the gate told it.
#[derive(Default)]
pub struct Script {
    pub reports: BTreeMap<String, NodeReport>,
    /// The turn the live view counts for each session.
    pub turns: BTreeMap<String, u64>,
    pub closed: BTreeSet<String>,
    pub launching: BTreeSet<TaskId>,
    pub usage_paused: bool,
    /// From when the account of each task may take work again after a usage limit; a task not
    /// named here may at once.
    pub usage_ready_at: BTreeMap<TaskId, Millis>,
    /// The sessions whose connection is gone, so an interrupt does not reach them.
    pub unreachable: BTreeSet<String>,
    /// What a launch answers; a start with no answer here starts.
    pub launch_error: Option<DomainError>,
    /// The tasks whose start ended with their task, so a failed start of them is no failure.
    pub ended: BTreeSet<TaskId>,

    pub interrupted: Vec<String>,
    pub told_nodes: Vec<Told>,
    pub told_sessions: Vec<(String, String)>,
    pub events: Vec<(AgentId, NodeEvent)>,
    pub launches: Vec<serde_json::Value>,
    pub failed_starts: Vec<(TaskId, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Told {
    pub task: TaskId,
    pub text: String,
    pub command: String,
}

/// Every command a double commits has its own id, also across the runtimes a test reopens over one
/// home: an id used before would replay the earlier command.
static COMMANDS: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub struct Engine {
    script: Mutex<Script>,
}

impl Engine {
    /// Gives `registry` this double as the engine the gate asks, and keeps it there for the test.
    pub fn wire(registry: &Registry) -> Arc<Engine> {
        let engine = Arc::new(Engine::default());
        registry.provide::<dyn AdmissionEngine>(engine.clone());
        registry.provide::<Engine>(engine.clone());
        engine
    }

    /// The double `wire` gave `runtime`.
    pub fn of(runtime: &dyn Runtime) -> Arc<Engine> {
        runtime.ports().port::<Engine>().expect("the test wired the admission engine's double")
    }

    pub fn script(&self) -> MutexGuard<'_, Script> {
        self.script.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `session`'s mod reports a turn started at `since`, or none running.
    pub fn working(&self, session: &str, since: Option<Millis>) {
        self.script().reports.entry(session.to_owned()).or_default().working_since = since;
    }

    fn command_id(kind: &str) -> String {
        format!("double-{kind}-{}", COMMANDS.fetch_add(1, Ordering::SeqCst))
    }

    fn commit(&self, runtime: &dyn Runtime, kind: &str, command: Command) -> Result<bool, DomainError> {
        let state = runtime.state();
        runtime.system_command(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: Self::command_id(kind).into(),
            expected_revision: state.revision, command,
        }).map(|outcome| outcome.changed)
    }
}

#[derive(Default)]
struct Api(ApiWatch);

alondra_engine_ports::extension!(Api);

impl AdmissionEngine for Engine {
    fn report(&self, _: &dyn Runtime, session: &str) -> Option<NodeReport> {
        self.script().reports.get(session).cloned()
    }

    fn turn(&self, _: &dyn Runtime, session: &str) -> Option<u64> {
        self.script().turns.get(session).copied()
    }

    fn interrupt(&self, _: &dyn Runtime, session: &str) -> bool {
        let mut script = self.script();
        script.interrupted.push(session.to_owned());
        !script.unreachable.contains(session)
    }

    fn is_closed(&self, _: &dyn Runtime, session: &str) -> bool {
        self.script().closed.contains(session)
    }

    fn is_launching(&self, _: &dyn Runtime, task: &TaskId) -> bool {
        self.script().launching.contains(task)
    }

    fn usage_paused(&self, _: &dyn Runtime) -> bool {
        self.script().usage_paused
    }

    fn api_watch<'a>(&self, runtime: &'a dyn Runtime) -> &'a ApiWatch {
        &alondra_engine_ports::extension::<Api>(runtime).0
    }

    fn usage_ready(&self, _: &DomainState, task: &TaskId, _: Millis, now: Millis) -> bool {
        self.script().usage_ready_at.get(task).is_none_or(|at| now >= *at)
    }

    /// Gives the node its input in the domain, as the engine does.
    fn tell_node(&self, runtime: &dyn Runtime, task: &TaskId, text: &str, command: &str) -> Result<(), String> {
        self.script().told_nodes.push(Told { task: task.clone(), text: text.to_owned(), command: command.to_owned() });
        self.commit(runtime, "tell", Command::TellNode { task_id: task.clone(), text: text.to_owned(), from_user: false, answers: None }).map(|_| ()).map_err(|error| error.to_string())
    }

    fn tell_session(&self, _: &dyn Runtime, session: &str, text: &str, _: bool) -> bool {
        self.script().told_sessions.push((session.to_owned(), text.to_owned()));
        true
    }

    /// Moves the node in the domain when its machine takes the event, as the engine does.
    fn node_event(&self, runtime: &dyn Runtime, agent: &AgentId, event: NodeEvent) -> Result<bool, String> {
        self.script().events.push((agent.clone(), event.clone()));
        if !matches!(runtime.state().node_next(agent, &event), Ok(Some(_))) {
            return Ok(false);
        }
        self.commit(runtime, "move", Command::MoveNode { agent_id: agent.clone(), event }).map_err(|error| error.message)
    }

    fn launch<'a>(&'a self, _: &'a dyn Runtime, _: &'a Path, queued: serde_json::Value) -> Launched<'a> {
        let mut script = self.script();
        script.launches.push(queued.clone());
        let answer = script.launch_error.clone().map_or(Ok(queued), Err);
        Box::pin(async move { answer })
    }

    fn start_ended(&self, _: &dyn Runtime, task: &TaskId) -> Result<bool, String> {
        Ok(self.script().ended.contains(task))
    }

    fn start_failed(&self, _: &dyn Runtime, task: &TaskId, error: &DomainError, _: Millis) -> Result<(), String> {
        self.script().failed_starts.push((task.clone(), error.code.clone()));
        Ok(())
    }
}
