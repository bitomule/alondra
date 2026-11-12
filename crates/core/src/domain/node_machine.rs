//! The one state of every node. A node is in exactly one `NodePhase`, and only `next` moves it:
//! what Claude Code reports, the hooks, the gate, the listing of real sessions and the domain's own
//! commands are events it accepts or refuses. The gate, Tasks, the header and the TUI read
//! the phase; none keeps a state of its own.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, DomainError>;
pub use crate::model::Millis;

/// The cards that put the last words of a node whose turn ended with nothing for the user in front
/// of them.
pub const UNHEARD_PREFIX: &str = "unheard-";
/// The one option of such a card: the node carries on with its task.
pub const CARRY_ON: &str = "carry_on";
pub const CARRY_ON_LABEL: &str = "Carry on";
pub(super) const CARRY_ON_MEANS: &str = "It goes on with its task and asks you through a card when it needs you";
/// The cards of the pauses the gate holds a node of the Agent SDK in, which its task's lifecycle
/// keeps: who the pause waits for and since when are in the card's id, so the journal keeps them.
/// They stay out of Waiting for You: the task's row shows the pause and its Resume button.
pub const NODE_PAUSE_PREFIX: &str = "node-pause-";
/// The one option of such a card: the user resumes the node, as with the Resume of its task.
pub const NODE_PAUSE_RESUME: &str = "resume";

/// The card behind a pause of the gate of the node of `task`, which nobody sees.
fn node_pause_interaction(id: InteractionId, task: &TaskId, title: &str, system: SystemCard, revision: u64) -> Interaction {
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| format!("Resume “{title}”"));
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: need.clone(),
        blocking: false,
        subject: None,
        payload: InteractionPayload::Question {
            options: vec![QuestionOption { id: NODE_PAUSE_RESUME.into(), label: "Resume".into(), description: None }],
            answer: None,
        },
        revision,
        order: revision,
        withdrawn: false,
        card: CardSpec::new(CardKind::Decide, need),
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(system), shortened: false,
    }
}

/// The card a pause of the gate takes in the lifecycle of a node of the Agent SDK, and who it
/// waits for. An API error of such a node is its lifecycle's own hold, never the gate's.
fn node_pause_card(task: &TaskId, pause: &Pause) -> Option<(InteractionId, alondra_domain::Owner, SystemCard)> {
    use alondra_domain::{Capacity, Owner};
    let (kind, owner, pause_kind) = match pause.reason {
        PauseReason::ByUser => ("user", Owner::User, NodePauseKind::User),
        PauseReason::Declared { .. } | PauseReason::Memory => ("machine", Owner::Capacity(Capacity::Machine), NodePauseKind::Machine),
        PauseReason::Usage => ("usage", Owner::Capacity(Capacity::Usage), NodePauseKind::Usage),
        PauseReason::Disk => ("disk", Owner::Capacity(Capacity::Disk), NodePauseKind::Disk),
        PauseReason::ApiError(_) => return None,
    };
    let system = SystemCard::NodePause { pause: pause_kind, since: pause.since };
    Some((InteractionId(format!("{NODE_PAUSE_PREFIX}{kind}-{}-{task}", pause.since)), owner, system))
}

/// Added to what a node is told when its host ended during a pause whose text says nothing of what
/// was cut.
const HOST_ENDED_IN_PAUSE: &str = "\n\nYour host ended during the pause: whatever you ran in the background ended with it and will not report back. Run again what you still need and wait for its result.";

/// What a node of the Agent SDK that rested through the gate's pause is told once it ends.
fn go_on_after(kind: NodePauseKind, host_ended: bool) -> String {
    let cut = host_ended && matches!(kind, NodePauseKind::Disk | NodePauseKind::Usage);
    go_on_text(kind).to_owned() + if cut { HOST_ENDED_IN_PAUSE } else { "" }
}

fn go_on_text(kind: NodePauseKind) -> &'static str {
    match kind {
        NodePauseKind::Disk => "Alondra held your next turn while the disk was nearly full. There is room again: carry on with your task.",
        NodePauseKind::Machine => "Alondra paused you while the Mac lacked room. The Mac is fine again: carry on with your task and run again what was cut.",
        NodePauseKind::Usage => "Your usage limit has reset. Carry on with what you were doing.",
        NodePauseKind::User => "You were resumed. Carry on with your task and run again what was cut.",
    }
}

/// The pause a card of `node_pause_card` stands for.
fn pause_of_card(card: &Interaction) -> Option<Pause> {
    let Some(SystemCard::NodePause { pause, since }) = &card.system_card else { return None };
    let reason = match pause {
        NodePauseKind::User => PauseReason::ByUser,
        NodePauseKind::Machine => PauseReason::Declared { reason: String::new() },
        NodePauseKind::Usage => PauseReason::Usage,
        NodePauseKind::Disk => PauseReason::Disk,
    };
    Some(Pause { reason, since: *since, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false })
}

/// Whether `card` is the card of a pause of the gate.
pub fn is_node_pause(card: &Interaction) -> bool {
    matches!(card.system_card, Some(SystemCard::NodePause { .. }))
}

/// The longest last words a node's card carries.
pub(super) const LAST_WORDS_MAX: usize = 20_000;

/// A node runs through the Agent SDK until its session ends: what it does is its task's lifecycle
/// (`DomainState::live_state`), and no event moves its phase but the end of its session. A phase an
/// older release stored for a node of `claude --bg` reads as `Closed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodePhase {
    ThroughSdk,
    #[serde(other)]
    Closed,
}

impl NodePhase {
    pub fn live(&self) -> bool {
        !matches!(self, Self::Closed)
    }
    pub fn key(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::ThroughSdk => "through_sdk",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    pub reason: PauseReason,
    pub since: Millis,
    /// The command its turn was running when it was stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<String>,
    /// When its interrupted turn started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_since: Option<Millis>,
    #[serde(default)]
    pub cleanup: PauseCleanup,
    /// The user released it ("I've fixed it"): it no longer waits out the grace of a declared wait.
    #[serde(default)]
    pub released: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PauseReason {
    Memory,
    /// The node said it waits for the machine.
    Declared { reason: String },
    Usage,
    ByUser,
    /// A Claude API error ended its turn: Alondra sends it back to its task at the retry's time.
    ApiError(ApiRetry),
    /// The disk fell under its floor while it built: its next turn waits for room.
    Disk,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseCleanup {
    #[default]
    Pending,
    Terminated,
    Done,
}

/// What the gate and the end of a session do to a node: a pause becomes a pause of its task's
/// lifecycle, a resume ends it, and the end of its session closes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeEvent {
    /// The user or Alondra closed the session for good.
    SessionClosed,
    Paused { pause: Pause },
    PauseCleanup { cleanup: PauseCleanup },
    PauseReleased,
    Resumed,
}

impl NodeEvent {
    pub fn key(&self) -> &'static str {
        match self {
            Self::SessionClosed => "session_closed",
            Self::Paused { .. } => "paused",
            Self::PauseCleanup { .. } => "pause_cleanup",
            Self::PauseReleased => "pause_released",
            Self::Resumed => "resumed",
        }
    }
}

/// The parts of every node's `TaskFacts` that need a scan, by node and by task.
struct FactIndex<'a> {
    held: BTreeSet<&'a AgentId>,
    asked: BTreeSet<&'a AgentId>,
    carded: BTreeSet<&'a TaskId>,
    /// The task `node_task` picks, with its rank: open, closing, or any (the last one).
    tasks: BTreeMap<&'a AgentId, (u8, &'a Task)>,
}

/// What the domain knows of a node's task.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskFacts {
    /// It has an open task that still needs it.
    pub open: bool,
    /// Its task is over or cleaning up.
    pub closing: bool,
    /// A call of its turn is held on a card of the user.
    pub held: bool,
    /// A question or request of its task waits for the user.
    pub card: bool,
    /// Its final result is in front of the user or settled for review.
    pub delivered: bool,
    /// Its task waits for another task to be done.
    pub awaits: bool,
}

fn refuse<T>(code: &str, message: &str) -> Result<T> {
    Err(DomainError::new(code, message))
}

/// What an open task's node does for it: what every reader says of the task beside its lifecycle
/// state, so none says it works while its node is paused or waits. A task whose node runs through
/// the Agent SDK reads its lifecycle's `LiveState`; one whose node's session is closed reads that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskActivity {
    Live(alondra_domain::LiveState),
    /// Its node's session is closed while the task is still open.
    NodeClosed,
}

impl TaskActivity {
    pub fn key(&self) -> &'static str {
        use alondra_domain::{Capacity, LiveState, Step, Worker};
        match self {
            Self::Live(LiveState::Working(Worker::Turn)) => "working",
            Self::Live(LiveState::Working(Worker::OwnWork)) => "own_work",
            Self::Live(LiveState::Working(Worker::AfterMerge)) => "after_merge",
            Self::Live(LiveState::Working(Worker::Alondra(Step::Starting))) => "starting",
            Self::Live(LiveState::Working(Worker::Alondra(Step::Delivering))) => "delivering",
            Self::Live(LiveState::Working(Worker::Alondra(Step::Waking))) => "waking",
            Self::Live(LiveState::Working(Worker::Alondra(Step::Answering))) => "answering",
            Self::Live(LiveState::Working(Worker::Alondra(Step::Merging))) => "merging",
            Self::Live(LiveState::Working(Worker::Alondra(Step::Finishing))) => "finishing",
            Self::Live(LiveState::WaitingUser(_)) => "waiting_user",
            Self::Live(LiveState::WaitingCapacity(Capacity::Gate | Capacity::Machine)) => "waiting_machine",
            Self::Live(LiveState::WaitingCapacity(Capacity::Disk)) => "waiting_disk",
            Self::Live(LiveState::WaitingCapacity(Capacity::Usage)) => "waiting_usage",
            Self::Live(LiveState::WaitingCapacity(Capacity::ClaudeApi)) => "waiting_claude_api",
            Self::Live(LiveState::WaitingTask(_)) => "waiting_dependency",
            Self::Live(LiveState::Paused) => "paused",
            Self::NodeClosed => "node_closed",
        }
    }
}

impl DomainState {
    /// What the node of an open task does for it; `None` for a task that ended, is closing, or has
    /// no node.
    pub fn task_activity(&self, task: &Task) -> Option<TaskActivity> {
        if task.state.terminal() || task.state == TaskState::Closing || task.archived {
            return None;
        }
        let agent = task.agent_id.as_ref()?;
        if super::read_index::admitted_through_sdk(self, &task.id, agent) || self.node_phase(agent) == Some(&NodePhase::ThroughSdk) {
            return self.live_state(&task.id).map(TaskActivity::Live);
        }
        Some(TaskActivity::NodeClosed)
    }

    /// The task a node works on: its open one, else the last it had.
    pub fn node_task(&self, agent: &AgentId) -> Option<&Task> {
        let mine = || self.tasks.values().filter(move |t| t.agent_id.as_ref() == Some(agent));
        mine().find(|t| !t.state.terminal() && !t.archived && t.state != TaskState::Closing)
            .or_else(|| mine().find(|t| !t.state.terminal() && !t.archived))
            .or_else(|| mine().last())
    }

    pub fn task_facts(&self, agent: &AgentId) -> TaskFacts {
        self.facts_in(&self.fact_index(), agent)
    }

    fn facts_in(&self, index: &FactIndex<'_>, agent: &AgentId) -> TaskFacts {
        let held = index.held.contains(agent);
        let asked = index.asked.contains(agent);
        let Some(t) = index.tasks.get(agent).map(|(_, task)| *task) else {
            return TaskFacts { held, card: asked, ..TaskFacts::default() };
        };
        let closing = t.state.terminal() || t.state == TaskState::Closing || t.wrap_up.asked_at.is_some();
        let delivered = t.final_presentation.as_ref().and_then(|p| self.interactions.get(p))
            .is_some_and(|p| self.open(p) && self.handed_back(p).is_none())
            || matches!(t.state, TaskState::ForYourReview | TaskState::AwaitingBlocker);
        TaskFacts { open: !t.state.terminal() && !t.archived, closing, held, card: asked || index.carded.contains(&t.id), delivered,
            awaits: t.state == TaskState::WaitingDependency }
    }

    /// What `task_facts` reads, in one pass over tasks and cards: every command settles every
    /// node, so a pass per node made each command cost nodes × cards.
    fn fact_index(&self) -> FactIndex<'_> {
        let held = self.holds.keys().filter_map(|id| self.interactions.get(id))
            .filter(|i| !i.withdrawn && matches!(i.payload, InteractionPayload::Question { answer: None, .. }))
            .filter_map(|i| match &i.author { Author::Agent { agent_id } => Some(agent_id), _ => None })
            .collect();
        let (mut asked, mut carded) = (BTreeSet::new(), BTreeSet::new());
        for i in self.interactions.values().filter(|i| (i.recipient == Recipient::User || Self::asks_the_orchestrator(i))
            && matches!(i.payload, InteractionPayload::Question { .. } | InteractionPayload::ActionRequest { .. })
            && self.open(i) && !self.out_of_sight(i))
        {
            if let Author::Agent { agent_id } = &i.author {
                asked.insert(agent_id);
            }
            if let Scope::Task { task_id } = &i.scope {
                carded.insert(task_id);
            }
        }
        let mut tasks: BTreeMap<&AgentId, (u8, &Task)> = BTreeMap::new();
        for t in self.tasks.values() {
            let Some(agent) = &t.agent_id else { continue };
            let rank = match (!t.state.terminal() && !t.archived, t.state == TaskState::Closing) {
                (true, false) => 0,
                (true, true) => 1,
                (false, _) => 2,
            };
            if tasks.get(agent).is_none_or(|(kept, _)| rank < *kept || (rank, *kept) == (2, 2)) {
                tasks.insert(agent, (rank, t));
            }
        }
        FactIndex { held, asked, carded, tasks }
    }

    /// Whether `event` would move the node, and where: what an adapter checks before it records one.
    pub fn node_next(&self, agent: &AgentId, event: &NodeEvent) -> Result<Option<NodePhase>> {
        let a = self.agents.get(agent).ok_or_else(|| DomainError::new("not_found", "Agent missing"))?;
        if a.phase == NodePhase::ThroughSdk || self.through_sdk(agent) {
            let paused = self.node_pause(agent).is_some();
            return Ok(match event {
                NodeEvent::Paused { .. } if !paused => Some(NodePhase::ThroughSdk),
                NodeEvent::Resumed if paused => Some(NodePhase::ThroughSdk),
                NodeEvent::SessionClosed if a.phase != NodePhase::Closed => Some(NodePhase::Closed),
                _ => None,
            });
        }
        match event {
            NodeEvent::SessionClosed => Ok(None),
            _ => refuse("node_closed", "The node's session is closed"),
        }
    }

    /// A delivery the user replied to comes back to them, the same one, once its node is done
    /// with the reply: it ended its turn without presenting again, or it cannot act on it.
    pub(super) fn return_replied_delivery(&mut self, agent: &AgentId) {
        let delivery = self.tasks.values()
            .filter(|t| !t.state.terminal() && t.agent_id.as_ref() == Some(agent))
            .filter_map(|t| t.final_presentation.as_ref().and_then(|p| self.interactions.get(p)))
            .find(|p| self.open_reply(p))
            .map(|p| p.id.clone());
        if let Some(delivery) = delivery {
            self.decided(&delivery);
        }
    }

    /// The nodes that take a slot: the one count the gate, Tasks and the header share.
    pub fn slots(&self) -> Vec<&Agent> {
        let index = self.fact_index();
        self.agents.values().filter(|a| Some(&a.id) != self.orchestrator_session.as_ref() && self.holds_slot(a, &index)).collect()
    }

    /// A node takes a slot while its task's lifecycle has it at work and says its execution holds one.
    fn holds_slot(&self, agent: &Agent, index: &FactIndex<'_>) -> bool {
        self.tasks.values()
            .find(|t| t.agent_id.as_ref() == Some(&agent.id) && !t.state.terminal() && super::read_index::admitted_through_sdk(self, &t.id, &agent.id))
            .is_some_and(|task| self.node_pause(&agent.id).is_none() && self.sdk_task_holds_slot(task, &self.facts_in(index, &agent.id)))
    }

    /// Whether the node of `task`, one of the Agent SDK, takes a slot of the gate.
    pub fn task_holds_slot(&self, task: &TaskId) -> bool {
        let Some(t) = self.tasks.get(task) else { return false };
        let facts = t.agent_id.as_ref().map(|agent| self.task_facts(agent)).unwrap_or_default();
        t.agent_id.as_ref().is_none_or(|agent| self.node_pause(agent).is_none()) && self.sdk_task_holds_slot(t, &facts)
    }

    /// The task's situation has its node working, or Alondra starting it, handing it an input or
    /// waking it; its task is not ending; and its lifecycle says its execution holds a slot. A turn
    /// kept open on a plan or a question for the user, or held by a pause, holds none.
    fn sdk_task_holds_slot(&self, task: &Task, facts: &TaskFacts) -> bool {
        use super::situation::{AlondraStep, Doing, Listing, Situation, WaitsFor};
        let listing = Listing { needs_you: facts.card || facts.held, listed: &|_| true };
        let takes_slot = matches!(self.situation(task, &listing).situation,
            Situation::Working(Doing::Turn | Doing::OwnWork | Doing::AfterMerge)
            | Situation::Waiting(WaitsFor::Step(AlondraStep::Starting | AlondraStep::Delivering | AlondraStep::Waking)));
        takes_slot && !facts.closing && self.lifecycle_of(&task.id).is_ok_and(|lifecycle| lifecycle.holds_slot())
    }

    /// What `task`'s node goes on with passes the gate first. A node that closes does not: cleaning
    /// up frees what the others wait for. Nor one a pause holds, which comes back through the gate.
    pub fn turn_passes_gate(&self, task: &TaskId) -> bool {
        let Some(t) = self.tasks.get(task).filter(|t| !t.over()) else { return false };
        let Some(agent) = t.agent_id.as_ref().filter(|agent| Some(*agent) != self.orchestrator_session.as_ref()) else { return false };
        self.through_sdk(agent) && !self.task_facts(agent).closing && self.node_pause(agent).is_none_or(|pause| pause.released)
    }

    /// `task`'s node has an input or an answer that waits for a slot of the gate.
    pub fn waits_for_turn(&self, task: &TaskId) -> bool {
        self.turn_passes_gate(task) && self.lifecycle_of(task).is_ok_and(|lifecycle| lifecycle.waits_for_slot())
    }

    /// A node waits on the user: a card of its task is in front of them. Its task's lifecycle holds
    /// that card (a plan, a permission, a question inside its turn), or it asked one through
    /// Alondra; its delivery and a pause are no such wait.
    pub fn node_waits_on_user(&self, agent: &AgentId) -> bool {
        use alondra_domain::LiveState;
        if self.node_phase(agent) != Some(&NodePhase::ThroughSdk) {
            return false;
        }
        let facts = self.task_facts(agent);
        let delivery = self.node_task(agent).and_then(|t| t.final_presentation.clone());
        facts.card || facts.held || matches!(self.sdk_live(agent), Some(LiveState::WaitingUser(cards))
            if cards.iter().any(|card| !self.node_pause_card(card) && Some(card) != delivery.as_ref()))
    }

    pub fn node_phase(&self, agent: &AgentId) -> Option<&NodePhase> {
        self.agents.get(agent).map(|a| &a.phase)
    }

    /// A node of the Agent SDK: its task's latest admission gave it an execution of its own.
    pub fn through_sdk(&self, agent: &AgentId) -> bool {
        self.node_task(agent).is_some_and(|t| super::read_index::admitted_through_sdk(self, &t.id, agent))
    }

    /// What the open task of a node does, from its lifecycle alone; `None` for a closed node that
    /// never ran through the Agent SDK, or one whose task is over.
    pub fn sdk_live(&self, agent: &AgentId) -> Option<alondra_domain::LiveState> {
        if self.node_phase(agent) != Some(&NodePhase::ThroughSdk) && !self.through_sdk(agent) {
            return None;
        }
        self.node_task(agent).filter(|t| !t.over()).and_then(|t| self.live_state(&t.id))
    }

    /// Whether the session of a node of the Agent SDK runs: its task's execution has a host that
    /// was not released. A task back in the gate's queue, or whose host was lost, has none.
    pub fn sdk_session_runs(&self, agent: &AgentId) -> bool {
        self.node_task(agent).filter(|t| !t.over())
            .and_then(|t| self.lifecycle_of(&t.id).ok())
            .and_then(|lifecycle| lifecycle.resource())
            .is_some_and(|resource| resource != alondra_domain::ResourceEvidence::Released)
    }

    /// The pause the gate holds a node in, as its open task's lifecycle keeps it.
    pub fn node_pause(&self, agent: &AgentId) -> Option<Pause> {
        if self.node_phase(agent) != Some(&NodePhase::ThroughSdk) {
            return None;
        }
        self.node_task(agent).filter(|t| !t.over())
            .and_then(|t| self.held_pause(&t.id)).and_then(|card| pause_of_card(self.interactions.get(&card)?))
    }

    /// The card of the gate's pause the lifecycle of `task` holds.
    fn held_pause(&self, task: &TaskId) -> Option<InteractionId> {
        let lifecycle = self.lifecycle_of(task).ok()?;
        lifecycle.cards().into_iter()
            .find(|(card, reason)| *reason == alondra_domain::WaitReason::Pause && self.node_pause_card(card))
            .map(|(card, _)| card.clone())
    }

    /// The paused nodes, by agent.
    pub fn paused_nodes(&self) -> BTreeMap<AgentId, Pause> {
        let mut paused = BTreeMap::new();
        for t in self.tasks.values().filter(|t| !t.over()) {
            let Some(agent) = t.agent_id.as_ref().filter(|agent| self.node_phase(agent) == Some(&NodePhase::ThroughSdk)) else { continue };
            if let Some(pause) = self.held_pause(&t.id).and_then(|card| pause_of_card(self.interactions.get(&card)?)) {
                paused.insert(agent.clone(), pause);
            }
        }
        paused
    }

    /// What the gate's events do to a node: a pause becomes a pause of its task's lifecycle and a
    /// resume ends it, and nothing moves its phase but the end of its session.
    pub(super) fn move_node(&mut self, agent: &AgentId, event: &NodeEvent, lifecycles: &mut Lifecycles) -> Result<()> {
        if !self.agents.contains_key(agent) {
            return refuse("not_found", "Agent missing");
        }
        if !self.through_sdk(agent) && self.node_phase(agent) != Some(&NodePhase::ThroughSdk) {
            return match event {
                NodeEvent::SessionClosed => Ok(()),
                _ => refuse("node_closed", "The node's session is closed"),
            };
        }
        let task = self.node_task(agent).filter(|t| !t.over()).map(|t| t.id.clone());
        match event {
            NodeEvent::Paused { pause } => {
                let task = task.ok_or_else(|| DomainError::new("not_found", "The node has no open task"))?;
                if self.held_pause(&task).is_some() {
                    return refuse("already_paused", "The node is already paused");
                }
                let (card, owner, system) = node_pause_card(&task, pause)
                    .ok_or_else(|| DomainError::new("invalid_input", "A Claude API error of a node of the Agent SDK is held by its task's lifecycle"))?;
                self.hold_node_pause(&task, card, owner, system, lifecycles)
            }
            NodeEvent::Resumed => {
                let task = task.as_ref();
                let card = task.and_then(|task| self.held_pause(task)).ok_or_else(|| DomainError::new("not_paused", "The node is not paused"))?;
                let task = task.expect("held above");
                let record = self.end_of_pause(task, &card, lifecycles)?;
                self.decide(lifecycles, task, record)?;
                self.retire(&card);
                Ok(())
            }
            NodeEvent::PauseCleanup { .. } | NodeEvent::PauseReleased => Ok(()),
            NodeEvent::SessionClosed => {
                self.agents.get_mut(agent).expect("checked above").phase = NodePhase::Closed;
                Ok(())
            }
        }
    }

    /// The gate's pause of the node `task` leaves ends with it: the gate sees pauses through
    /// nodes, so nothing else would end it. The user's own pause stays until the user resumes it.
    pub(super) fn end_gate_pause(&mut self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        let Some(card) = self.held_pause(task) else { return Ok(()) };
        if self.interactions.get(&card).and_then(pause_of_card).is_none_or(|pause| pause.reason == PauseReason::ByUser) {
            return Ok(());
        }
        let record = self.end_of_pause(task, &card, lifecycles)?;
        self.decide(lifecycles, task, record)?;
        self.retire(&card);
        Ok(())
    }

    /// How the gate's pause `card` of `task` ends: with nothing new when what the task goes on with
    /// already waits for its node, or, when its node rested through the pause with nothing left to
    /// do (its turn ended, or its host did), told to carry on in the same session. A delivery takes
    /// the end of a pause with nothing to go on with, so one its node still owes work on is told too.
    pub(super) fn end_of_pause(&self, task: &TaskId, card: &InteractionId, lifecycles: &mut Lifecycles) -> Result<super::LifecycleRecord> {
        let lifecycle = self.lifecycle(lifecycles, task)?;
        if lifecycle.advanced(lifecycle.revision(), alondra_domain::Event::EndPause { card: card.clone() }).is_ok_and(|ended| !self.stranded(task, &ended)) {
            return Ok(super::LifecycleRecord::PauseEnded { card: card.clone() });
        }
        let kind = match self.interactions.get(card).and_then(|card| card.system_card.as_ref()) {
            Some(SystemCard::NodePause { pause, .. }) => *pause,
            _ => NodePauseKind::User,
        };
        Ok(super::LifecycleRecord::Resumed { card: card.clone(), input: self.next_input(task), text: Some(go_on_after(kind, self.host_ended_since_pause(task))) })
    }

    /// The task ends while the gate holds its node of the Agent SDK paused: nothing is left to resume.
    pub(super) fn end_node_pause(&mut self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        let Some(card) = self.held_pause(task) else { return Ok(()) };
        self.decide(lifecycles, task, super::LifecycleRecord::PauseEnded { card: card.clone() })?;
        self.retire(&card);
        Ok(())
    }

    /// The lifecycle of `task` holds the gate's pause `card`, with the card behind it.
    fn hold_node_pause(&mut self, task: &TaskId, card: InteractionId, owner: alondra_domain::Owner, system: SystemCard, lifecycles: &mut Lifecycles) -> Result<()> {
        self.decide(lifecycles, task, super::LifecycleRecord::Paused { card: card.clone(), owner: Some(owner.into()) })?;
        let title = self.tasks[task].title.clone();
        self.interactions.insert(card.clone(), node_pause_interaction(card, task, &title, system, self.revision + 1));
        Ok(())
    }

    /// A card nobody sees: the gate's pause of a node of the Agent SDK, and the wait for a usage
    /// limit, which resumes by itself when it resets and leaves the user nothing to do.
    pub fn out_of_sight(&self, card: &Interaction) -> bool {
        card.author == Author::System && (is_node_pause(card) || matches!(card.system_card, Some(SystemCard::ClaudeUsage { .. })))
    }

    /// Whether the card named `card` is the card of a pause of the gate.
    pub fn node_pause_card(&self, card: &InteractionId) -> bool {
        self.interactions.get(card).is_some_and(is_node_pause)
    }
}
