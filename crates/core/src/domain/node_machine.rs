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
pub(crate) const UNHEARD_WHY: &str = "Its turn ended without a result or a question for you. Answer it, or write to it, and it goes on.";
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

/// Whether a task in `live` has its node at work: its turn, its own work, the work it declared for
/// after the merge, or Alondra starting it, handing it an input or waking it. Only such a node
/// takes one of the gate's slots, and only while its lifecycle says its execution holds one.
pub fn node_at_work(live: &alondra_domain::LiveState) -> bool {
    use alondra_domain::{LiveState, Step, Worker};
    matches!(live, LiveState::Working(Worker::Turn | Worker::OwnWork | Worker::AfterMerge
        | Worker::Alondra(Step::Starting | Step::Delivering | Step::Waking)))
}

/// The longest last words a node's card carries.
pub(super) const LAST_WORDS_MAX: usize = 20_000;

/// What the card with a node's last words asks of the user, naming its task: whether it said
/// anything decides the words.
fn unheard_need(title: &str, said: bool) -> String {
    fit_title(title, CardSpec::TITLED_NEED_MAX, |title| if said { format!("“{title}” stopped and wrote to you") } else { format!("“{title}” stopped without a word") })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodePhase {
    /// Admitted, or let back by the gate and being restarted after its session was lost; no turn
    /// has begun yet. A listing that finds it without work may predate its start, so only the next
    /// one moves it: `idle_since` is when the first came.
    Starting {
        reviving: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle_since: Option<Millis>,
    },
    /// Its session is gone while its task still needs it, and it waits for the gate to let Alondra
    /// restart it. Nothing runs, so it holds no slot and is never shown as reconnecting.
    Lost,
    /// A turn is running.
    Working,
    /// Its turn ended behind work of its own that runs in the background (a command, a subagent):
    /// Claude Code wakes it with the result when that work ends. Its work uses the machine and it
    /// goes on without passing the gate, so it keeps its slot.
    WaitingBackground(Background),
    /// A card of its task waits for the user, whether its turn still runs or ended. Whatever it
    /// does meanwhile, the user moves it, so it holds no slot. Never without that card: a turn that
    /// ends on an open task with nothing for the user puts the node's last words in one first.
    WaitingUser { in_turn: bool },
    /// Stopped by Alondra (the machine, the usage limit) or by the user; it comes back through the gate.
    Paused(Pause),
    /// Its final result is in front of the user. A turn it still runs after delivering (final
    /// checks, a push) is real work, so it keeps its slot until that turn ends.
    Delivered {
        #[serde(default)]
        in_turn: bool,
    },
    /// The user answered its card or sent its result back while it rested: what they said is on
    /// its way and starts its next turn. The first listing that finds it idle may predate the
    /// answer, so only the next one ends the wait as a turn end. `idle_since` is when the first came.
    /// After a wait of its task ended (`after_wait`), what Alondra told it is on its way: no listing
    /// ends a turn it has not started, so the words it wrote into the wait never reach the user.
    Resuming {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle_since: Option<Millis>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        after_wait: bool,
    },
    /// Its turn ended while its task waits for another task: Alondra tells it when that one is done.
    WaitingDependency,
    /// Its turn ended and it has no open task: nothing waits on it.
    NoTask,
    /// Its task is over and it cleans up before its session closes.
    Closing,
    /// Its session is gone for good.
    Closed,
    /// It runs through the Agent SDK: what it does is its task's lifecycle (`DomainState::live_state`),
    /// and no event moves this phase but the end of its session.
    ThroughSdk,
}

impl NodePhase {
    /// Takes one of the node limit's slots: a node the gate admitted and that is starting, fresh or
    /// restarted after its session was lost, or one in a turn, delivered or not. A lost node waiting
    /// for the gate takes none.
    pub fn holds_slot(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::Working | Self::WaitingBackground(_) | Self::Resuming { .. } | Self::Delivered { in_turn: true })
    }
    /// Its session is gone and Alondra brings it back: lost and waiting for the gate, or being
    /// restarted. Its task keeps it, yet it is no connected node.
    pub fn coming_back(&self) -> bool {
        matches!(self, Self::Lost | Self::Starting { reviving: true, .. })
    }
    pub fn live(&self) -> bool {
        !matches!(self, Self::Closed)
    }
    /// A turn of its own runs, whatever it waits for inside it.
    pub fn in_turn(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::Working | Self::WaitingUser { in_turn: true } | Self::Delivered { in_turn: true })
    }
    pub fn key(&self) -> &'static str {
        match self {
            Self::Starting { reviving: false, .. } => "starting",
            Self::Starting { reviving: true, .. } => "reviving",
            Self::Lost => "lost",
            Self::Working => "working",
            Self::WaitingBackground(_) => "waiting_background",
            Self::WaitingUser { .. } => "waiting_user",
            Self::Paused(_) => "paused",
            Self::Delivered { .. } => "delivered",
            Self::Resuming { .. } => "resuming",
            Self::WaitingDependency => "waiting_dependency",
            Self::NoTask => "no_task",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::ThroughSdk => "through_sdk",
        }
    }
}

/// The work a node left running in the background when its turn ended: the oldest of it, and how
/// many more.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Background {
    pub work: crate::model::BackgroundKind,
    /// The command, or what the subagent does, as Claude Code lists it.
    pub label: String,
    pub since: Millis,
    #[serde(default)]
    pub others: u32,
    /// When Alondra saw that work gone (its list emptied, its shells exited) with no turn to take
    /// its result yet: Claude Code wakes the node with its notification, so only a later listing
    /// that finds it without work ends the wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gone: Option<Millis>,
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

/// What `claude agents` says of a session when Alondra lists them. `Blocked` is a session that
/// waits on the user (a question, a permission, an error only they clear, or a turn that ended
/// saying it cannot go on). It is never work: every wait inside a turn that Alondra can answer (a
/// permission, a plan, a question) is a held card that already moves the node to `WaitingUser`, so
/// a blocked session without one waits on something only its terminal shows: its turn is over,
/// and its last words go to the user in a card. A node waiting for work of its own is the
/// exception: Claude Code's model guesses `blocked` from the last words («Esperando el resto.»)
/// while that work runs, so only its work gone and a later listing without work ends that wait.
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
    /// Its turn ended behind work of its own still running in the background.
    TurnEndedBehind { work: Background },
    /// Alondra saw the work it waited for gone (its list emptied, its shells exited) with no turn
    /// to take its result yet.
    BackgroundEnded,
    /// A Claude API error ended its turn and Alondra does not send it back by itself.
    TurnFailed,
    /// The session ended or crashed while its task still needs it.
    SessionLost,
    /// The user or Alondra closed the session for good.
    SessionClosed,
    StartFailed,
    /// Restarting the lost session failed; a card asks the user what to do.
    RevivalFailed,
    /// The gate let a lost node back: Alondra restarts its session now.
    RevivalAdmitted,
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
            Self::TurnEndedBehind { .. } => "turn_ended_behind",
            Self::BackgroundEnded => "background_ended",
            Self::TurnFailed => "turn_failed",
            Self::SessionLost => "session_lost",
            Self::SessionClosed => "session_closed",
            Self::StartFailed => "start_failed",
            Self::RevivalFailed => "revival_failed",
            Self::RevivalAdmitted => "revival_admitted",
            Self::Paused { .. } => "paused",
            Self::PauseCleanup { .. } => "pause_cleanup",
            Self::PauseReleased => "pause_released",
            Self::Resumed => "resumed",
            Self::Listed { .. } => "listed",
        }
    }
    /// Reports that carry the reporter's clock: one older than the node's phase is refused.
    fn timed(&self) -> bool {
        matches!(self, Self::TurnStarted | Self::TurnEnded | Self::TurnEndedBehind { .. } | Self::TurnFailed | Self::SessionLost)
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
    /// Its task waits for another task to be done.
    pub awaits: bool,
}

fn refuse<T>(code: &str, message: &str) -> Result<T> {
    Err(DomainError::new(code, message))
}

/// The move ends what the node was doing for its task: a turn, or its work at all.
fn ends_attention(from: &NodePhase, to: &NodePhase) -> bool {
    use NodePhase as P;
    let in_turn = |phase: &NodePhase| matches!(phase, P::Starting { .. } | P::Working | P::WaitingBackground(_) | P::Resuming { .. }
        | P::WaitingUser { in_turn: true } | P::Delivered { in_turn: true });
    let gone = |phase: &NodePhase| matches!(phase, P::Paused(_) | P::Lost | P::Closing | P::Closed);
    (in_turn(from) && !in_turn(to)) || (!gone(from) && gone(to))
}

/// Where a move leaves a node: a phase, or the end of a turn on an open task with nothing for the
/// user, which only the domain can follow, by putting the node's last words in a card for the user
/// (`DomainState::move_node`). No phase stands for a node at rest that nobody hears.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    At(NodePhase),
    Unheard,
}

/// Where a node rests once its turn is over.
fn rest(f: &TaskFacts) -> Settled {
    Settled::At(if f.closing {
        NodePhase::Closing
    } else if f.card || f.held {
        NodePhase::WaitingUser { in_turn: false }
    } else if f.delivered {
        NodePhase::Delivered { in_turn: false }
    } else if !f.open {
        NodePhase::NoTask
    } else if f.awaits {
        NodePhase::WaitingDependency
    } else {
        return Settled::Unheard;
    })
}

/// Where a resting node goes when the domain's facts change under it: what moved it (an answer, a
/// result sent back, a task handed to it) is on its way to it.
fn resume(f: &TaskFacts) -> NodePhase {
    resume_after(f, false)
}

fn resume_after(f: &TaskFacts, after_wait: bool) -> NodePhase {
    match rest(f) {
        Settled::At(phase) => phase,
        Settled::Unheard => NodePhase::Resuming { idle_since: None, after_wait },
    }
}

/// The phase the domain's own facts move a node to, whatever event came: a card opened or
/// answered, a result delivered or withdrawn, its task closing.
pub fn settle(phase: NodePhase, f: &TaskFacts) -> NodePhase {
    match phase {
        NodePhase::Closed => NodePhase::Closed,
        NodePhase::ThroughSdk => NodePhase::ThroughSdk,
        NodePhase::Closing if f.open && !f.closing => resume(f),
        NodePhase::Closing => NodePhase::Closing,
        _ if f.closing => NodePhase::Closing,
        NodePhase::WaitingBackground(_) if f.held || f.card || f.delivered => resume(f),
        NodePhase::Working | NodePhase::Starting { reviving: false, .. } if f.held || f.card => NodePhase::WaitingUser { in_turn: true },
        NodePhase::Working | NodePhase::Starting { reviving: false, .. } if f.delivered => NodePhase::Delivered { in_turn: true },
        NodePhase::WaitingUser { in_turn: true } if !f.held && !f.card => if f.delivered { NodePhase::Delivered { in_turn: true } } else { NodePhase::Working },
        NodePhase::Delivered { in_turn: true } if f.held || f.card => NodePhase::WaitingUser { in_turn: true },
        NodePhase::Delivered { in_turn: true } if !f.delivered => NodePhase::Working,
        NodePhase::Resuming { .. } if f.held || f.card || f.delivered || !f.open || f.awaits => resume(f),
        NodePhase::WaitingDependency => resume_after(f, true),
        NodePhase::WaitingUser { in_turn: false } | NodePhase::Delivered { in_turn: false } | NodePhase::NoTask => resume(f),
        phase => phase,
    }
}

/// The one transition function. `since` is when the node's phase last moved and `at` when the
/// event happened.
pub fn next(phase: &NodePhase, since: Millis, event: &NodeEvent, f: &TaskFacts, at: Millis) -> Result<Settled> {
    use NodePhase as P;
    if *phase == P::Closed {
        return match event {
            NodeEvent::SessionClosed | NodeEvent::SessionLost | NodeEvent::Listed { listed: Listed::Absent | Listed::Done } => Ok(Settled::At(P::Closed)),
            _ => refuse("node_closed", "The node's session is closed"),
        };
    }
    if *phase == P::ThroughSdk {
        return match event {
            NodeEvent::SessionClosed => Ok(Settled::At(P::Closed)),
            _ => refuse("through_sdk", "A node of the Agent SDK does what its task's lifecycle says: no node event moves it"),
        };
    }
    if event.timed() && at < since {
        return refuse("stale", "The report is older than the node's state");
    }
    let lost = || if f.open && !f.closing { P::Lost } else { P::Closed };
    let moved = match (phase, event) {
        (P::Paused(Pause { reason: PauseReason::ByUser, .. }), NodeEvent::TurnStarted) =>
            return refuse("paused_by_user", "Only the user resumes a node they paused"),
        (P::Paused(Pause { reason: PauseReason::Memory, .. }), NodeEvent::TurnStarted) =>
            return refuse("paused_for_memory", "Alondra brings back a node it paused for memory once the Mac has room"),
        (P::Paused(Pause { reason: PauseReason::Declared { .. }, .. }), NodeEvent::TurnStarted) =>
            return refuse("waits_for_machine", "A node that said it waits for the machine comes back when the gate resumes it"),
        (P::Closing, NodeEvent::TurnStarted | NodeEvent::TurnEnded | NodeEvent::TurnEndedBehind { .. } | NodeEvent::TurnFailed) => P::Closing,
        (_, NodeEvent::TurnStarted) => P::Working,
        (P::Paused(_), NodeEvent::TurnEnded | NodeEvent::TurnEndedBehind { .. } | NodeEvent::TurnFailed) => phase.clone(),
        (_, NodeEvent::TurnEnded | NodeEvent::TurnFailed) => return Ok(settled(rest(f), f)),
        (_, NodeEvent::TurnEndedBehind { work }) => P::WaitingBackground(work.clone()),
        (P::WaitingBackground(work), NodeEvent::BackgroundEnded) => P::WaitingBackground(Background { gone: work.gone.or(Some(at)), ..work.clone() }),
        (_, NodeEvent::BackgroundEnded) => return refuse("not_waiting_background", "The node waits for no work of its own"),
        (P::Paused(_), NodeEvent::SessionLost) => phase.clone(),
        (P::Closing, NodeEvent::SessionLost) => P::Closed,
        (_, NodeEvent::SessionLost) => lost(),
        (_, NodeEvent::SessionClosed) => P::Closed,
        (P::Starting { .. }, NodeEvent::StartFailed | NodeEvent::RevivalFailed) => P::Closed,
        (_, NodeEvent::StartFailed | NodeEvent::RevivalFailed) => return refuse("not_starting", "The node is not starting"),
        (P::Lost, NodeEvent::RevivalAdmitted) if f.awaits =>
            return refuse("waits_dependency", "A node whose task waits for other tasks comes back once they are done"),
        (P::Lost, NodeEvent::RevivalAdmitted) => P::Starting { reviving: true, idle_since: None },
        (_, NodeEvent::RevivalAdmitted) => return refuse("not_lost", "Only a node whose session was lost is let back"),
        (P::Starting { .. } | P::Working | P::WaitingBackground(_) | P::Resuming { .. } | P::Delivered { in_turn: true }, NodeEvent::Paused { pause }) => P::Paused(pause.clone()),
        (P::Paused(_), NodeEvent::Paused { .. }) => return refuse("already_paused", "The node is already paused"),
        (_, NodeEvent::Paused { .. }) => return refuse("not_working", "Only a node at work can be paused"),
        (P::Paused(pause), NodeEvent::PauseCleanup { cleanup }) => P::Paused(Pause { cleanup: *cleanup, ..pause.clone() }),
        (P::Paused(pause), NodeEvent::PauseReleased) => P::Paused(Pause { released: true, ..pause.clone() }),
        (P::Paused(_), NodeEvent::Resumed) => P::Starting { reviving: false, idle_since: None },
        (_, NodeEvent::PauseCleanup { .. } | NodeEvent::PauseReleased | NodeEvent::Resumed) =>
            return refuse("not_paused", "The node is not paused"),
        (phase, NodeEvent::Listed { listed }) => return Ok(settled(observe(phase, since, *listed, f, at), f)),
    };
    Ok(Settled::At(settle(moved, f)))
}

fn settled(moved: Settled, f: &TaskFacts) -> Settled {
    match moved {
        Settled::At(phase) => Settled::At(settle(phase, f)),
        Settled::Unheard => Settled::Unheard,
    }
}

/// What the listing of real sessions says moves a node. It carries no turn times, so it never
/// overrides a pause. A listing that finds a node without work right after it started, was
/// answered or saw its own work gone may predate that, so only a later one moves it: facts in
/// their order, never the time between them; one asked for before the node's phase began says
/// nothing of it. A node whose session is absent is lost: it waits for the gate again, holding no
/// slot. A listing that says working never puts back to work a node resting on the user or on its
/// task's dependencies: only the start of its turn does.
fn observe(phase: &NodePhase, since: Millis, listed: Listed, f: &TaskFacts, at: Millis) -> Settled {
    use NodePhase as P;
    let keep = || Settled::At(phase.clone());
    if at < since {
        return keep();
    }
    let unworked = matches!(listed, Listed::Absent | Listed::Idle | Listed::Blocked | Listed::Done);
    match (phase, listed) {
        (P::Closing, Listed::Absent | Listed::Done) => Settled::At(P::Closed),
        (P::Closing, _) => keep(),
        (P::Paused(_), _) => keep(),
        (P::Starting { reviving: false, idle_since: None }, _) if unworked => Settled::At(P::Starting { reviving: false, idle_since: Some(at) }),
        (P::Starting { reviving: true, idle_since: None }, Listed::Absent) => Settled::At(P::Starting { reviving: true, idle_since: Some(at) }),
        (P::Starting { idle_since: Some(first), .. }, _) if unworked && at <= *first => keep(),
        (_, Listed::Absent) => Settled::At(if f.open && !f.closing { P::Lost } else { P::Closed }),
        (_, Listed::Done) if !f.open || f.closing => Settled::At(P::Closed),
        (P::WaitingUser { .. } | P::Delivered { in_turn: false } | P::WaitingDependency | P::NoTask, Listed::Idle | Listed::Blocked | Listed::Done) => keep(),
        (P::WaitingBackground(Background { gone: Some(gone), .. }), Listed::Idle | Listed::Blocked | Listed::Done) if at > *gone => rest(f),
        (P::WaitingBackground(_), _) => keep(),
        (P::Resuming { after_wait: true, .. }, Listed::Idle | Listed::Blocked | Listed::Done) => keep(),
        (P::Resuming { idle_since: None, after_wait }, Listed::Idle | Listed::Blocked | Listed::Done) =>
            Settled::At(P::Resuming { idle_since: Some(at), after_wait: *after_wait }),
        (P::Resuming { idle_since: Some(first), .. }, Listed::Idle | Listed::Blocked | Listed::Done) if at <= *first => keep(),
        (_, Listed::Idle | Listed::Blocked | Listed::Done) => rest(f),
        (P::WaitingUser { .. } | P::Delivered { .. } | P::WaitingDependency, Listed::Working) => keep(),
        (_, Listed::Working) => Settled::At(P::Working),
    }
}

/// What an open task's node does for it: what every reader says of the task beside its lifecycle
/// state, so none says it works while its node is paused or waits. A task whose node runs through
/// the Agent SDK reads its lifecycle's `LiveState`; a node of `claude --bg` reads its one phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskActivity {
    Live(alondra_domain::LiveState),
    Starting,
    /// Its session was lost and it waits for the gate to be restarted.
    ComingBack,
    Reconnecting,
    Working,
    WaitingBackground(Background),
    WaitingUser,
    WaitingDependency,
    Paused(Pause),
    Delivered,
    /// Its PR merged and its node finishes the work it said comes after the merge.
    AfterMerge { reason: String },
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
            Self::Starting => "starting",
            Self::ComingBack => "coming_back",
            Self::Reconnecting => "reconnecting",
            Self::Working => "working",
            Self::WaitingBackground(_) => "waiting_background",
            Self::WaitingUser => "waiting_user",
            Self::WaitingDependency => "waiting_dependency",
            Self::Paused(Pause { reason: PauseReason::ByUser, .. }) => "paused_by_user",
            Self::Paused(_) => "paused_by_machine",
            Self::Delivered => "delivered",
            Self::AfterMerge { .. } => "after_merge",
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
        if super::read_index::admitted_through_sdk(self, &task.id, agent) {
            return self.live_state(&task.id).map(TaskActivity::Live);
        }
        if self.node_phase(agent) == Some(&NodePhase::Closing) && let Some(work) = &task.wrap_up.post_merge_work {
            if task.state == TaskState::WaitingDependency {
                return Some(TaskActivity::WaitingDependency);
            }
            return Some(TaskActivity::AfterMerge { reason: work.reason.clone() });
        }
        Some(match self.node_phase(agent) {
            Some(NodePhase::ThroughSdk) => return self.live_state(&task.id).map(TaskActivity::Live),
            None | Some(NodePhase::Closing | NodePhase::Closed | NodePhase::NoTask) => TaskActivity::NodeClosed,
            Some(NodePhase::Starting { reviving: true, .. }) => TaskActivity::Reconnecting,
            Some(NodePhase::Starting { reviving: false, .. }) => TaskActivity::Starting,
            Some(NodePhase::Lost) if task.state == TaskState::WaitingDependency => TaskActivity::WaitingDependency,
            Some(NodePhase::Lost) => TaskActivity::ComingBack,
            Some(NodePhase::Working | NodePhase::Resuming { .. }) => TaskActivity::Working,
            Some(NodePhase::WaitingBackground(work)) => TaskActivity::WaitingBackground(work.clone()),
            Some(NodePhase::WaitingUser { .. }) => TaskActivity::WaitingUser,
            Some(NodePhase::WaitingDependency) => TaskActivity::WaitingDependency,
            Some(NodePhase::Paused(pause)) => TaskActivity::Paused(pause.clone()),
            Some(NodePhase::Delivered { .. }) => TaskActivity::Delivered,
        })
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
            && self.open(i) && !is_node_pause(i))
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
        if a.phase == NodePhase::ThroughSdk || self.through_sdk(agent) {
            let paused = self.node_pause(agent).is_some();
            return Ok(match event {
                NodeEvent::Paused { .. } if !paused => Some(NodePhase::ThroughSdk),
                NodeEvent::Resumed if paused => Some(NodePhase::ThroughSdk),
                NodeEvent::SessionClosed => Some(NodePhase::Closed),
                _ => None,
            });
        }
        let moved = match next(&a.phase, a.since, event, &self.task_facts(agent), at)? {
            Settled::At(phase) => phase,
            Settled::Unheard => NodePhase::WaitingUser { in_turn: false },
        };
        Ok((moved != a.phase).then_some(moved))
    }

    pub(super) fn move_node(&mut self, agent: &AgentId, event: &NodeEvent, at: Millis) -> Result<()> {
        let a = self.agents.get(agent).ok_or_else(|| DomainError::new("not_found", "Agent missing"))?;
        let (phase, since) = (a.phase.clone(), a.since);
        if *event == NodeEvent::TurnStarted {
            for card in self.unheard_cards(agent) {
                self.retire(&card);
            }
        }
        let mut settled = next(&phase, since, event, &self.task_facts(agent), at)?;
        if settled == Settled::Unheard {
            self.return_replied_delivery(agent);
            settled = next(&phase, since, event, &self.task_facts(agent), at)?;
        }
        let moved = match settled {
            Settled::At(phase) => phase,
            Settled::Unheard => {
                self.unheard(agent)?;
                settle(NodePhase::WaitingUser { in_turn: false }, &self.task_facts(agent))
            }
        };
        if moved != phase {
            let ends_attention = ends_attention(&phase, &moved);
            let a = self.agents.get_mut(agent).expect("read above");
            a.phase = moved;
            a.since = a.since.max(at);
            if ends_attention {
                self.return_replied_delivery(agent);
            }
        }
        Ok(())
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

    /// The open cards that hold a node's last words for the user.
    fn unheard_cards(&self, agent: &AgentId) -> Vec<InteractionId> {
        self.interactions.values()
            .filter(|i| i.system_card == Some(SystemCard::Unheard) && i.author == Author::Agent { agent_id: agent.clone() } && self.open(i))
            .map(|i| i.id.clone())
            .collect()
    }

    /// Puts the last words of a node whose turn ended on its open task with nothing for the user in
    /// a card for them: it answers it or writes to it, and that reaches the node.
    fn unheard(&mut self, agent: &AgentId) -> Result<()> {
        let task = self.node_task(agent).filter(|t| !t.state.terminal() && !t.archived)
            .ok_or_else(|| DomainError::new("not_found", "The node has no open task"))?;
        let (task_id, title) = (task.id.clone(), task.title.clone());
        let mut id = InteractionId(format!("{UNHEARD_PREFIX}{task_id}-{}", self.revision + 1));
        while self.interactions.contains_key(&id) {
            id.0.push('-');
        }
        let said = self.last_words.get(agent).map(|text| text.trim()).filter(|text| !text.is_empty()).map(str::to_owned);
        let need = unheard_need(&title, said.is_some());
        let mut card = CardSpec::new(CardKind::Decide, need.clone()).why(said.as_deref().map_or(UNHEARD_WHY.into(), |said| clip(said, CardSpec::WHY_MAX)));
        if let Some(said) = said.as_ref().filter(|said| said.chars().count() > CardSpec::WHY_MAX) {
            card = card.content(ContentKind::Report, said.clone());
        }
        self.interactions.insert(id.clone(), Interaction {
            attachments: vec![],
            id: id.clone(),
            scope: Scope::Task { task_id },
            author: Author::Agent { agent_id: agent.clone() },
            recipient: Recipient::User,
            body: said.unwrap_or(need),
            blocking: false,
            subject: None,
            payload: InteractionPayload::Question {
                options: vec![QuestionOption { id: CARRY_ON.into(), label: CARRY_ON_LABEL.into(), description: Some(CARRY_ON_MEANS.into()) }],
                answer: None,
            },
            revision: self.revision + 1,
            order: self.revision + 1,
            withdrawn: false,
            card,
            clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(SystemCard::Unheard), shortened: false,
        });
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

    /// The nodes that take a slot: the one count the gate, Tasks and the header share.
    pub fn slots(&self) -> Vec<&Agent> {
        let index = self.fact_index();
        self.agents.values().filter(|a| Some(&a.id) != self.orchestrator_session.as_ref() && self.holds_slot(a, &index)).collect()
    }

    /// A node of the Agent SDK takes a slot while its task's lifecycle has it at work and says its
    /// execution holds one; a node of `claude --bg` while its phase does.
    fn holds_slot(&self, agent: &Agent, index: &FactIndex<'_>) -> bool {
        let sdk = self.tasks.values()
            .find(|t| t.agent_id.as_ref() == Some(&agent.id) && !t.state.terminal() && super::read_index::admitted_through_sdk(self, &t.id, &agent.id));
        match sdk {
            Some(task) => self.node_pause(&agent.id).is_none() && self.sdk_task_holds_slot(task, &self.facts_in(index, &agent.id)),
            None => agent.phase.holds_slot(),
        }
    }

    /// Whether the node of `task`, one of the Agent SDK, takes a slot of the gate.
    pub fn task_holds_slot(&self, task: &TaskId) -> bool {
        let Some(t) = self.tasks.get(task) else { return false };
        let facts = t.agent_id.as_ref().map(|agent| self.task_facts(agent)).unwrap_or_default();
        t.agent_id.as_ref().is_none_or(|agent| self.node_pause(agent).is_none()) && self.sdk_task_holds_slot(t, &facts)
    }

    /// Its `LiveState` has the node at work, or in a turn it runs after delivering, which is real
    /// work; it waits on no card of the user's or the orchestrator's; its task is not ending; and
    /// its lifecycle says its execution holds a slot. A turn kept open on a plan or a question for
    /// the user holds none.
    fn sdk_task_holds_slot(&self, task: &Task, facts: &TaskFacts) -> bool {
        use alondra_domain::LiveState;
        let at_work = match self.live_state(&task.id) {
            Some(LiveState::Working(_)) => true,
            Some(LiveState::WaitingUser(cards)) => cards.iter().all(|card| task.final_presentation.as_ref() == Some(card)),
            _ => false,
        };
        at_work && !(facts.card || facts.held || facts.closing) && self.lifecycle_of(&task.id).is_ok_and(|lifecycle| lifecycle.holds_slot())
    }

    /// A node waits on the user: a card of its task is in front of them. For a node of the Agent
    /// SDK, its task's lifecycle holds that card (a plan, a permission, a question inside its turn),
    /// or it asked one through Alondra; its delivery and a pause are no such wait.
    pub fn node_waits_on_user(&self, agent: &AgentId) -> bool {
        use alondra_domain::LiveState;
        match self.node_phase(agent) {
            Some(NodePhase::WaitingUser { .. }) => true,
            Some(NodePhase::ThroughSdk) => {
                let facts = self.task_facts(agent);
                let delivery = self.node_task(agent).and_then(|t| t.final_presentation.clone());
                facts.card || facts.held || matches!(self.sdk_live(agent), Some(LiveState::WaitingUser(cards))
                    if cards.iter().any(|card| !self.node_pause_card(card) && Some(card) != delivery.as_ref()))
            }
            _ => false,
        }
    }

    pub fn node_phase(&self, agent: &AgentId) -> Option<&NodePhase> {
        self.agents.get(agent).map(|a| &a.phase)
    }

    /// A node of the Agent SDK: its task's latest admission gave it an execution of its own.
    pub fn through_sdk(&self, agent: &AgentId) -> bool {
        self.node_task(agent).is_some_and(|t| super::read_index::admitted_through_sdk(self, &t.id, agent))
    }

    /// What the open task of a node of the Agent SDK does, from its lifecycle alone; `None` for a
    /// node of `claude --bg`, or one whose task is over.
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

    /// The pause the gate holds a node in: its phase says it for a node of `claude --bg`, and its
    /// open task's lifecycle for one of the Agent SDK.
    pub fn node_pause(&self, agent: &AgentId) -> Option<Pause> {
        match self.node_phase(agent)? {
            NodePhase::Paused(pause) => Some(pause.clone()),
            NodePhase::ThroughSdk => self.node_task(agent).filter(|t| !t.over())
                .and_then(|t| self.held_pause(&t.id)).and_then(|card| pause_of_card(self.interactions.get(&card)?)),
            _ => None,
        }
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
        let mut paused: BTreeMap<AgentId, Pause> = self.agents.values().filter_map(|a| match &a.phase {
            NodePhase::Paused(pause) => Some((a.id.clone(), pause.clone())),
            _ => None,
        }).collect();
        for t in self.tasks.values().filter(|t| !t.over()) {
            let Some(agent) = t.agent_id.as_ref().filter(|agent| self.node_phase(agent) == Some(&NodePhase::ThroughSdk)) else { continue };
            if let Some(pause) = self.held_pause(&t.id).and_then(|card| pause_of_card(self.interactions.get(&card)?)) {
                paused.insert(agent.clone(), pause);
            }
        }
        paused
    }

    /// What the gate's events do to a node of the Agent SDK: a pause becomes a pause of its task's
    /// lifecycle and a resume ends it, and nothing moves its phase but the end of its session. The
    /// pause's cleanup and release concern only the processes of a `claude --bg` node.
    pub(super) fn move_sdk_node(&mut self, agent: &AgentId, event: &NodeEvent, at: Millis, lifecycles: &mut Lifecycles) -> Result<()> {
        self.adopt_sdk_node(agent, lifecycles)?;
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
            event => self.move_node(agent, event, at),
        }
    }

    /// How the gate's pause `card` of `task` ends: with nothing new when what the task goes on with
    /// already waits for its node, or, when its node rested through the pause with nothing left to
    /// do (its turn ended, or its host did), told to carry on in the same session. A delivery takes
    /// the end of a pause with nothing to go on with, so one its node still owes work on is told too.
    fn end_of_pause(&self, task: &TaskId, card: &InteractionId, lifecycles: &mut Lifecycles) -> Result<super::LifecycleRecord> {
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

    /// A node of the Agent SDK an older daemon moved through its phase takes `ThroughSdk`, and a
    /// pause its phase held becomes the same pause of its task's lifecycle, so the gate sees it
    /// unchanged: the same reason and the same start, the same place in its queue. A pause the
    /// lifecycle cannot take (its task is ending, or another pause holds it) has nothing left to
    /// hold.
    pub(super) fn adopt_sdk_node(&mut self, agent: &AgentId, lifecycles: &mut Lifecycles) -> Result<()> {
        if self.node_phase(agent).is_none_or(|phase| *phase == NodePhase::ThroughSdk) || !self.through_sdk(agent) {
            return Ok(());
        }
        let pause = match self.node_phase(agent) {
            Some(NodePhase::Paused(pause)) => Some(pause.clone()),
            _ => None,
        };
        let task = self.node_task(agent).filter(|t| !t.over()).map(|t| t.id.clone());
        if let (Some(pause), Some(task)) = (pause, task)
            && self.held_pause(&task).is_none()
            && let Some((card, owner, system)) = node_pause_card(&task, &pause)
        {
            let _ = self.hold_node_pause(&task, card, owner, system, lifecycles);
        }
        self.agents.get_mut(agent).expect("read above").phase = NodePhase::ThroughSdk;
        Ok(())
    }

    /// The nodes of the Agent SDK whose open task an older daemon left in a phase of their own.
    pub fn unadopted_sdk_nodes(&self) -> BTreeSet<AgentId> {
        self.tasks.values().filter(|t| !t.over())
            .filter_map(|t| t.agent_id.as_ref().filter(|agent| self.node_phase(agent).is_some_and(|phase| !matches!(phase, NodePhase::ThroughSdk | NodePhase::Closed))
                && super::read_index::admitted_through_sdk(self, &t.id, agent)))
            .cloned().collect()
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

    /// A card nobody sees: the gate's pause of a node of the Agent SDK.
    pub fn out_of_sight(&self, card: &Interaction) -> bool {
        card.author == Author::System && is_node_pause(card)
    }

    /// Whether the card named `card` is the card of a pause of the gate.
    pub fn node_pause_card(&self, card: &InteractionId) -> bool {
        self.interactions.get(card).is_some_and(is_node_pause)
    }

    /// Every node of the Agent SDK still in a phase an older daemon gave it takes `ThroughSdk`.
    pub(super) fn adopt_sdk_nodes(&mut self, lifecycles: &mut Lifecycles) -> Result<()> {
        for agent in self.unadopted_sdk_nodes() {
            self.adopt_sdk_node(&agent, lifecycles)?;
        }
        Ok(())
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
