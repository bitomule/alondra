//! The one state of every node. A node is in exactly one `NodePhase`, and only `next` moves it:
//! what Claude Code reports, the hooks, the gate, the listing of real sessions and the domain's own
//! commands are events it accepts or refuses. The gate, Fleet, Tasks, the header and the TUI read
//! the phase; none keeps a state of its own. See docs/fleet-pacing.md §11.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, DomainError>;
pub use crate::model::Millis;

/// How long a started node may go without a turn before an idle or missing session frees its slot.
pub const START_GRACE: Millis = 3 * 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodePhase {
    /// Admitted, or being restarted after its session was lost; no turn has begun yet.
    Starting { reviving: bool },
    /// A turn is running.
    Working,
    /// A card of its task waits for the user, whether its turn still runs or ended. Whatever it
    /// does meanwhile, the user moves it, so it holds no slot.
    WaitingUser { in_turn: bool },
    /// Stopped by Alondra (the machine, the usage limit) or by the user; it comes back through the gate.
    Paused(Pause),
    /// Its final result is in front of the user. A turn it still runs after delivering (final
    /// checks, a push) is real work, so it keeps its slot until that turn ends.
    Delivered {
        #[serde(default)]
        in_turn: bool,
    },
    /// Its turn ended with nothing for anyone: Alondra sends it back to its task or asks the user.
    Stopped,
    /// Its task is over and it cleans up before its session closes.
    Closing,
    /// Its session is gone for good.
    Closed,
}

impl NodePhase {
    /// Takes one of the node limit's slots: a node admitted and starting, or in a turn, delivered or
    /// not. One being
    /// brought back after its session was lost already had its slot counted when it was admitted.
    pub fn holds_slot(&self) -> bool {
        matches!(self, Self::Starting { reviving: false } | Self::Working | Self::Delivered { in_turn: true })
    }
    pub fn live(&self) -> bool {
        !matches!(self, Self::Closed)
    }
    pub fn key(&self) -> &'static str {
        match self {
            Self::Starting { reviving: false } => "starting",
            Self::Starting { reviving: true } => "reviving",
            Self::Working => "working",
            Self::WaitingUser { .. } => "waiting_user",
            Self::Paused(_) => "paused",
            Self::Delivered { .. } => "delivered",
            Self::Stopped => "stopped",
            Self::Closing => "closing",
            Self::Closed => "closed",
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
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseCleanup {
    #[default]
    Pending,
    Terminated,
    Done,
}

/// What `claude agents` says of a session when Alondra lists them. `Blocked` is a session that
/// waits on the user (a question, a permission, an error only they clear, or a turn that ended
/// saying it cannot go on): inside a turn that is still work, after its end it is no work at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Listed {
    Working,
    Idle,
    Blocked,
    /// Its turn ended and it waits for its next prompt, its process alive or not: a message resumes
    /// it. A node whose task still needs it rests as when idle; any other has finished.
    Done,
    Absent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeEvent {
    TurnStarted,
    TurnEnded,
    /// A Claude API error ended its turn and Alondra does not send it back by itself.
    TurnFailed,
    /// The session ended or crashed while its task still needs it.
    SessionLost,
    /// The user or Alondra closed the session for good.
    SessionClosed,
    StartFailed,
    /// Restarting the lost session failed; a card asks the user what to do.
    RevivalFailed,
    Paused { pause: Pause },
    PauseCleanup { cleanup: PauseCleanup },
    PauseReleased,
    Resumed,
    Listed { listed: Listed },
}

impl NodeEvent {
    pub fn key(&self) -> &'static str {
        match self {
            Self::TurnStarted => "turn_started",
            Self::TurnEnded => "turn_ended",
            Self::TurnFailed => "turn_failed",
            Self::SessionLost => "session_lost",
            Self::SessionClosed => "session_closed",
            Self::StartFailed => "start_failed",
            Self::RevivalFailed => "revival_failed",
            Self::Paused { .. } => "paused",
            Self::PauseCleanup { .. } => "pause_cleanup",
            Self::PauseReleased => "pause_released",
            Self::Resumed => "resumed",
            Self::Listed { .. } => "listed",
        }
    }
    /// Reports that carry the reporter's clock: one older than the node's phase is refused.
    fn timed(&self) -> bool {
        matches!(self, Self::TurnStarted | Self::TurnEnded | Self::TurnFailed | Self::SessionLost)
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

/// What the domain knows of a node's task when its phase moves.
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
}

fn refuse<T>(code: &str, message: &str) -> Result<T> {
    Err(DomainError::new(code, message))
}

/// Where a node rests once its turn is over.
fn rest(f: &TaskFacts) -> NodePhase {
    if f.closing {
        NodePhase::Closing
    } else if f.card || f.held {
        NodePhase::WaitingUser { in_turn: false }
    } else if f.delivered {
        NodePhase::Delivered { in_turn: false }
    } else {
        NodePhase::Stopped
    }
}

/// The phase the domain's own facts move a node to, whatever event came: a card opened or
/// answered, a result delivered or withdrawn, its task closing.
pub fn settle(phase: NodePhase, f: &TaskFacts) -> NodePhase {
    match phase {
        NodePhase::Closed => NodePhase::Closed,
        NodePhase::Closing if f.open && !f.closing => rest(f),
        NodePhase::Closing => NodePhase::Closing,
        _ if f.closing => NodePhase::Closing,
        NodePhase::Working | NodePhase::Starting { reviving: false } if f.held || f.card => NodePhase::WaitingUser { in_turn: true },
        NodePhase::Working | NodePhase::Starting { reviving: false } if f.delivered => NodePhase::Delivered { in_turn: true },
        NodePhase::WaitingUser { in_turn: true } if !f.held && !f.card => if f.delivered { NodePhase::Delivered { in_turn: true } } else { NodePhase::Working },
        NodePhase::Delivered { in_turn: true } if f.held || f.card => NodePhase::WaitingUser { in_turn: true },
        NodePhase::Delivered { in_turn: true } if !f.delivered => NodePhase::Working,
        NodePhase::WaitingUser { in_turn: false } | NodePhase::Delivered { in_turn: false } | NodePhase::Stopped => rest(f),
        phase => phase,
    }
}

/// The one transition function. `since` is when the node's phase last moved and `at` when the
/// event happened.
pub fn next(phase: &NodePhase, since: Millis, event: &NodeEvent, f: &TaskFacts, at: Millis) -> Result<NodePhase> {
    use NodePhase as P;
    if *phase == P::Closed {
        return refuse("node_closed", "The node's session is closed");
    }
    if event.timed() && at < since {
        return refuse("stale", "The report is older than the node's state");
    }
    let lost = || if f.open && !f.closing { P::Starting { reviving: true } } else { P::Closed };
    let moved = match (phase, event) {
        (P::Paused(Pause { reason: PauseReason::ByUser, .. }), NodeEvent::TurnStarted) =>
            return refuse("paused_by_user", "Only the user resumes a node they paused"),
        (P::Paused(Pause { reason: PauseReason::Memory, .. }), NodeEvent::TurnStarted) =>
            return refuse("paused_for_memory", "Alondra brings back a node it paused for memory once the Mac has room"),
        (P::Closing, NodeEvent::TurnStarted | NodeEvent::TurnEnded | NodeEvent::TurnFailed) => P::Closing,
        (_, NodeEvent::TurnStarted) => P::Working,
        (P::Paused(_), NodeEvent::TurnEnded | NodeEvent::TurnFailed) => phase.clone(),
        (_, NodeEvent::TurnEnded | NodeEvent::TurnFailed) => rest(f),
        (P::Paused(_), NodeEvent::SessionLost) => phase.clone(),
        (P::Closing, NodeEvent::SessionLost) => P::Closed,
        (_, NodeEvent::SessionLost) => lost(),
        (_, NodeEvent::SessionClosed) => P::Closed,
        (P::Starting { .. }, NodeEvent::StartFailed | NodeEvent::RevivalFailed) => P::Closed,
        (_, NodeEvent::StartFailed | NodeEvent::RevivalFailed) => return refuse("not_starting", "The node is not starting"),
        (P::Starting { .. } | P::Working | P::Delivered { in_turn: true } | P::Stopped, NodeEvent::Paused { pause }) => P::Paused(pause.clone()),
        (P::Paused(_), NodeEvent::Paused { .. }) => return refuse("already_paused", "The node is already paused"),
        (_, NodeEvent::Paused { .. }) => return refuse("not_working", "Only a node at work, or one whose turn ended with nothing pending, can be paused"),
        (P::Paused(pause), NodeEvent::PauseCleanup { cleanup }) => P::Paused(Pause { cleanup: *cleanup, ..pause.clone() }),
        (P::Paused(pause), NodeEvent::PauseReleased) => P::Paused(Pause { released: true, ..pause.clone() }),
        (P::Paused(_), NodeEvent::Resumed) => P::Starting { reviving: false },
        (_, NodeEvent::PauseCleanup { .. } | NodeEvent::PauseReleased | NodeEvent::Resumed) =>
            return refuse("not_paused", "The node is not paused"),
        (phase, NodeEvent::Listed { listed }) => observe(phase, since, *listed, f, at),
    };
    Ok(settle(moved, f))
}

/// What the listing of real sessions says moves a node. It carries no turn times, so it never
/// overrides a pause, and it leaves a node that has only just started alone. A node being brought
/// back stays so until its revival ends: it holds no slot, and the user's "try again" restarts it.
fn observe(phase: &NodePhase, since: Millis, listed: Listed, f: &TaskFacts, at: Millis) -> NodePhase {
    use NodePhase as P;
    let young = at - since < START_GRACE;
    match (phase, listed) {
        (P::Closing, Listed::Absent | Listed::Done) => P::Closed,
        (P::Closing, _) => P::Closing,
        (P::Paused(_), _) => phase.clone(),
        (P::Starting { reviving: false }, Listed::Absent | Listed::Idle | Listed::Done) if young => phase.clone(),
        (P::Starting { reviving: true }, Listed::Absent) => phase.clone(),
        (_, Listed::Absent) => if f.open && !f.closing { P::Starting { reviving: true } } else { P::Closed },
        (_, Listed::Done) if !f.open || f.closing => P::Closed,
        (P::WaitingUser { .. } | P::Delivered { in_turn: false } | P::Stopped, Listed::Idle | Listed::Done) => phase.clone(),
        (_, Listed::Idle | Listed::Done) => rest(f),
        (P::WaitingUser { .. } | P::Delivered { .. }, Listed::Working | Listed::Blocked) => phase.clone(),
        (P::Stopped, Listed::Blocked) => phase.clone(),
        (_, Listed::Working | Listed::Blocked) => P::Working,
    }
}

impl DomainState {
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
        TaskFacts { open: !t.state.terminal() && !t.archived, closing, held, card: asked || index.carded.contains(&t.id), delivered }
    }

    /// What `task_facts` reads, in one pass over tasks and cards: every command settles every
    /// node, so a pass per node made each command cost nodes × cards.
    fn fact_index(&self) -> FactIndex<'_> {
        let held = self.holds.keys().filter_map(|id| self.interactions.get(id))
            .filter(|i| !i.withdrawn && matches!(i.payload, InteractionPayload::Question { answer: None, .. }))
            .filter_map(|i| match &i.author { Author::Agent { agent_id } => Some(agent_id), _ => None })
            .collect();
        let (mut asked, mut carded) = (BTreeSet::new(), BTreeSet::new());
        for i in self.interactions.values().filter(|i| i.recipient == Recipient::User
            && matches!(i.payload, InteractionPayload::Question { .. } | InteractionPayload::ActionRequest { .. })
            && self.open(i))
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
    pub fn node_next(&self, agent: &AgentId, event: &NodeEvent, at: Millis) -> Result<Option<NodePhase>> {
        let a = self.agents.get(agent).ok_or_else(|| DomainError::new("not_found", "Agent missing"))?;
        let moved = next(&a.phase, a.since, event, &self.task_facts(agent), at)?;
        Ok((moved != a.phase).then_some(moved))
    }

    pub(super) fn move_node(&mut self, agent: &AgentId, event: &NodeEvent, at: Millis) -> Result<()> {
        if let Some(moved) = self.node_next(agent, event, at)? {
            let a = self.agents.get_mut(agent).expect("checked by node_next");
            a.phase = moved;
            a.since = a.since.max(at);
        }
        Ok(())
    }

    /// Every node the listing of real sessions speaks of takes what it says: listed as Claude Code
    /// shows it, or absent. A closed node, or one the listing cannot move, is left as it is.
    pub(super) fn reconcile_nodes(&mut self, listed: &BTreeMap<AgentId, Listed>, at: Millis) {
        for (id, listed) in listed {
            if self.agents.get(id).is_some_and(|a| a.phase.live()) {
                let _ = self.move_node(id, &NodeEvent::Listed { listed: *listed }, at);
            }
        }
    }

    /// Whether the listing would move any node: an adapter submits it only then.
    pub fn reconciling_moves(&self, listed: &BTreeMap<AgentId, Listed>, at: Millis) -> bool {
        let mut next = self.clone();
        next.reconcile_nodes(listed, at);
        next.agents != self.agents
    }

    /// Applies what the last command changed in the nodes' tasks.
    pub fn settle_nodes(&mut self) {
        let index = self.fact_index();
        let facts: Vec<(AgentId, TaskFacts)> = self.agents.keys().map(|id| (id.clone(), self.facts_in(&index, id))).collect();
        for (id, facts) in facts {
            let a = self.agents.get_mut(&id).expect("listed above");
            a.phase = settle(a.phase.clone(), &facts);
        }
    }

    /// The nodes that take a slot: the one count the gate, Fleet, Tasks and the header share.
    pub fn slots(&self) -> Vec<&Agent> {
        self.agents.values().filter(|a| a.phase.holds_slot() && Some(&a.id) != self.orchestrator_session.as_ref()).collect()
    }

    pub fn node_phase(&self, agent: &AgentId) -> Option<&NodePhase> {
        self.agents.get(agent).map(|a| &a.phase)
    }

    /// The paused nodes, by agent.
    pub fn paused_nodes(&self) -> BTreeMap<&AgentId, &Pause> {
        self.agents.values().filter_map(|a| match &a.phase {
            NodePhase::Paused(pause) => Some((&a.id, pause)),
            _ => None,
        }).collect()
    }

    pub(super) fn validate_nodes(&self) -> Result<()> {
        let index = self.fact_index();
        for a in self.agents.values() {
            if settle(a.phase.clone(), &self.facts_in(&index, &a.id)) != a.phase {
                return refuse("node_phase_mismatch", "A node's phase contradicts its task");
            }
        }
        Ok(())
    }
}
