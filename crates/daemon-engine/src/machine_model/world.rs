//! The world around Alondra: each task's host, the provider session in it and the work that session
//! left running, the facts the host still owes the daemon, and the Mac's slots, memory and disk, the
//! usage limit, the orchestrator and the clock. It only assumes what the providers document: a
//! session that runs ends with its result or its process dies where Alondra sees it, and a fact the
//! daemon did not take is sent again.
use alondra_core::domain::{RuntimeObservation, TaskId};
use alondra_foundation::Millis;
use proptest::prelude::*;
use std::collections::{BTreeMap, VecDeque};

pub(super) const TASKS: [&str; 3] = ["t-a", "t-b", "t-c"];
pub(super) const PASS: Millis = 15_000;
pub(super) const MINUTE: Millis = 60_000;
/// How long a turn of the provider runs on its own before it ends, once nobody acts.
pub(super) const TURN_LASTS: Millis = MINUTE;
/// How long work a session left running lasts on its own.
pub(super) const WORK_LASTS: Millis = 2 * MINUTE;

/// What a turn of the node does before it ends, in the order the session's script gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Turn {
    Deliver,
    AskUser,
    AskPermission,
    AskOrchestrator,
    Background,
    /// The node opens a pull request and ends its turn.
    OpenPr,
    /// The node delivers its pull request if it has one, its result otherwise.
    DeliverPr,
    Empty,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum UserAct {
    Answer,
    Tell,
    Pause,
    Resume,
    Cancel,
    Move,
    Accept,
    Return,
    /// The user puts the task first in the queue.
    Prioritize,
}

#[derive(Clone, Debug)]
pub(super) enum Action {
    User(UserAct, usize),
    /// The running turn of the task does what its script says next, and ends.
    TurnEnds(usize),
    /// Work the session left running ends; `notifies`: the session opens a turn to take it up.
    WorkEnds { task: usize, notifies: bool },
    /// The session leaves the next input it is offered untaken.
    IgnoresNextInput(usize),
    HostDies(usize),
    /// The host is stopped on purpose: no new one takes its place on its own.
    StopsHost(usize),
    /// What the host observed reaches the daemon only a minute later.
    FactsLate(usize),
    /// The host sends what it last sent again.
    FactsAgain(usize),
    Restart,
    Slots(usize),
    MemoryTight(bool),
    DiskFull(bool),
    /// The usage limit holds new work and the orchestrator for this long, or ends.
    Usage(Option<Millis>),
    OrchestratorAnswers(bool),
    Clock(Millis),
}

#[derive(Clone, Debug)]
pub(super) enum Fact {
    Runtime(RuntimeObservation),
    TurnEnded(String),
}

#[derive(Clone, Debug)]
pub(super) struct Running {
    pub id: String,
    pub since: Millis,
    /// The permission request the turn waits on for its decision.
    pub decision: Option<u64>,
}

#[derive(Clone, Debug)]
pub(super) struct Work {
    pub id: String,
    pub since: Millis,
    pub notifies: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Host {
    pub generation: u64,
    pub alive: bool,
    pub turn: Option<Running>,
    pub work: Vec<Work>,
    pub offered: Option<String>,
    /// The inputs the session read inside a turn.
    pub read: std::collections::BTreeSet<String>,
    pub ignores: bool,
    /// What the host observed and the daemon has not taken, each with its place in the order.
    pub outbox: VecDeque<(u64, Fact)>,
    pub sent: Vec<(u64, Fact)>,
    pub late_until: Millis,
}

impl Host {
    pub fn new(generation: u64) -> Self {
        Self { generation, alive: true, turn: None, work: Vec::new(), offered: None, read: Default::default(), ignores: false, outbox: VecDeque::new(), sent: Vec::new(), late_until: 0 }
    }

    /// Notes `fact` for the daemon after everything the host observed before it.
    pub fn emit(&mut self, fact: Fact) {
        static CURSOR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.outbox.push_back((CURSOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed), fact));
    }

    /// The provider still runs something for the task: a turn or work of its own.
    pub fn runs(&self) -> bool {
        self.alive && (self.turn.is_some() || !self.work.is_empty())
    }
}

pub(super) struct World {
    pub hosts: BTreeMap<TaskId, Host>,
    /// Hosts of executions that ended, whose facts may still arrive late.
    pub retired: Vec<(TaskId, Host)>,
    pub slots: usize,
    pub memory_tight: bool,
    pub disk_full: bool,
    pub usage_until: Option<Millis>,
    pub orchestrator_answers: bool,
    pub now: Millis,
    pub script: Vec<Turn>,
    pub turns_ended: BTreeMap<TaskId, usize>,
    seq: u64,
}

impl World {
    pub fn new(script: Vec<Turn>, slots: usize) -> Self {
        Self {
            hosts: BTreeMap::new(), retired: Vec::new(), slots, memory_tight: false, disk_full: false, usage_until: None,
            orchestrator_answers: true, now: 1_800_000_000_000, script, turns_ended: BTreeMap::new(), seq: 0,
        }
    }

    pub fn id(&mut self, kind: &str) -> String {
        self.seq += 1;
        format!("{kind}-{}", self.seq)
    }

    pub fn usage_holds(&self) -> bool {
        self.usage_until.is_some_and(|until| self.now < until)
    }

    /// What the next turn of `task` to end does.
    pub fn next_turn(&mut self, task: &TaskId) -> Turn {
        let ended = self.turns_ended.entry(task.clone()).or_default();
        let turn = self.script[*ended % self.script.len()];
        *ended += 1;
        turn
    }

    pub fn host(&mut self, task: &TaskId) -> Option<&mut Host> {
        self.hosts.get_mut(task)
    }
}

fn task() -> impl Strategy<Value = usize> {
    0..TASKS.len()
}

fn user_act() -> impl Strategy<Value = UserAct> {
    prop_oneof![
        3 => Just(UserAct::Answer),
        1 => Just(UserAct::Tell),
        1 => Just(UserAct::Pause),
        1 => Just(UserAct::Resume),
        1 => Just(UserAct::Cancel),
        1 => Just(UserAct::Move),
        2 => Just(UserAct::Accept),
        1 => Just(UserAct::Return),
        1 => Just(UserAct::Prioritize),
    ]
}

pub(super) fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        6 => (user_act(), task()).prop_map(|(act, task)| Action::User(act, task)),
        8 => task().prop_map(Action::TurnEnds),
        2 => (task(), any::<bool>()).prop_map(|(task, notifies)| Action::WorkEnds { task, notifies }),
        1 => task().prop_map(Action::IgnoresNextInput),
        2 => task().prop_map(Action::HostDies),
        2 => task().prop_map(Action::StopsHost),
        1 => task().prop_map(Action::FactsLate),
        1 => task().prop_map(Action::FactsAgain),
        1 => Just(Action::Restart),
        1 => (1..=3usize).prop_map(Action::Slots),
        1 => any::<bool>().prop_map(Action::MemoryTight),
        1 => any::<bool>().prop_map(Action::DiskFull),
        1 => prop::option::of((1..=60 as Millis).prop_map(|minutes| minutes * MINUTE)).prop_map(Action::Usage),
        1 => any::<bool>().prop_map(Action::OrchestratorAnswers),
        3 => (1..=25 as Millis).prop_map(|minutes| Action::Clock(minutes * MINUTE)),
    ]
}

pub(super) fn script() -> impl Strategy<Value = Vec<Turn>> {
    let turn = prop_oneof![
        Just(Turn::Deliver),
        Just(Turn::AskUser),
        Just(Turn::AskPermission),
        Just(Turn::AskOrchestrator),
        Just(Turn::Background),
        Just(Turn::OpenPr),
        Just(Turn::DeliverPr),
        Just(Turn::Empty),
    ];
    prop::collection::vec(turn, 1..6)
}

/// A history: the script every session follows, the slots the gate starts with, and what happens.
#[derive(Clone, Debug)]
pub(super) struct History {
    pub script: Vec<Turn>,
    pub slots: usize,
    pub actions: Vec<Action>,
}

pub(super) fn history() -> impl Strategy<Value = History> {
    (script(), 1..=3usize, prop::collection::vec(action(), 1..40)).prop_map(|(script, slots, actions)| History { script, slots, actions })
}
