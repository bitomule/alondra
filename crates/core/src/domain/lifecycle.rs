//! The one authority of a task's lifecycle: its `TaskLifecycle`, replayed inside the transaction
//! from the records its decisions left in the task's journal. The task's state, node, start and
//! ending in the DTO are read from it and checked against it before a transition is accepted, so
//! no other writer can move them. Every task has a journal; the store keeps it as the events these
//! records stand for, and `journal_writes`/`records_from_events` are the two ways across.
use super::*;
use super::rules::{stalled_before, stalled_in_place_of};
use alondra_domain::{
    ActivityView, Await, CLAUDE_BG_HOST, EMPTY_TURN_WAKES, CallbackId, Capacity, LiveState, Owner, Several, Worker, ContinuationId, Control, ConversationId, Decision, DrainObligation, Event,
    FinishIntent, HostId, InputId, InstanceId, OperationId, PermissionMode, PlanReviewOutcome as ReviewSettlement,
    PostMergeFact, Refusal, ResourceEvidence, RuntimeFact, TaskLifecycle, TaskPhase, Trigger, TurnId, WaitReason, WorkId,
    decision_callback, decision_continuation, decision_epoch,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

type Result<T> = std::result::Result<T, DomainError>;

/// The instance of a `claude --bg` execution: one per task and generation, even when a node that
/// stays alive takes the task again, so no two executions share one.
fn claude_bg_instance(task: &TaskId, generation: u64) -> String {
    format!("{CLAUDE_BG_HOST}:{task}:{generation}")
}

/// How a task is ending. Shown apart from its state, which stays Closing until the last receipt
/// of what the ending still owes: the node's session, its cleanup, work after the merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEnding {
    Completing,
    Cancelling,
}

/// What a receipt discharged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Drained {
    Execution { instance: String },
    Cleanup { operation: String },
    PostMerge { operation: String },
    /// Background work of the execution, gone with the node's session.
    Work { work: String },
    /// A callback the execution waited on, gone with the node's session.
    Callback { callback: String },
}

/// What ended the work a node declared it runs after its PR merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostMergeEnd {
    NodeDeclaredEnd,
    Cleanup,
    UserDecision,
    SessionOver,
    TurnEnded,
}

impl From<PostMergeEnd> for PostMergeFact {
    fn from(end: PostMergeEnd) -> Self {
        match end {
            PostMergeEnd::NodeDeclaredEnd => Self::NodeDeclaredEnd,
            PostMergeEnd::Cleanup => Self::Cleanup,
            PostMergeEnd::UserDecision => Self::UserDecision,
            PostMergeEnd::SessionOver => Self::SessionOver,
            PostMergeEnd::TurnEnded => Self::TurnEnded,
        }
    }
}
impl From<PostMergeFact> for PostMergeEnd {
    fn from(fact: PostMergeFact) -> Self {
        match fact {
            PostMergeFact::NodeDeclaredEnd => Self::NodeDeclaredEnd,
            PostMergeFact::Cleanup => Self::Cleanup,
            PostMergeFact::UserDecision => Self::UserDecision,
            PostMergeFact::SessionOver => Self::SessionOver,
            PostMergeFact::TurnEnded => Self::TurnEnded,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOutcome {
    Unchanged,
    PlanChanged,
    Cancelled,
    KeptByUser,
}

/// The text of the input that asks an SDK execution to compact its conversation.
pub const COMPACT: &str = "/compact";
/// The code of a takeover refused because its host never answered the last one.
pub const REPEATED_TAKEOVER: &str = "repeated_takeover";

/// The permission modes an SDK execution can be set to; none grants a permission by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionModeRecord {
    Default,
    Plan,
    DontAsk,
}

/// An operation on a running SDK execution, as the journal keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlRecord {
    Interrupt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<String>,
    },
    /// Claude's process is ended, after it accepted the interrupt of `turn` and kept it open.
    ForceStop { turn: String },
    SetModel { model: String },
    SetPermissionMode { mode: PermissionModeRecord },
    ReadMcpStatus,
    ReconnectMcp { server: String },
    StartMcpSignIn { server: String },
    ClearMcpSignIn { server: String },
}

/// What answers a decision Claude asked for through the task's SDK execution, as the journal
/// keeps it. Nothing is granted by default: only `Allow`, `Answers` and `Accept` let it go on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionRecord {
    Allow,
    /// Claude's questions answered, one answer for each, in the order it asked them.
    Answers { answers: Vec<String> },
    Deny { message: String },
    /// An MCP server's request for input accepted with what the user wrote and the values they
    /// filled in its form.
    Accept {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Map<String, serde_json::Value>>,
    },
    Decline,
    /// A newer plan of the same execution replaced this plan before anyone answered it.
    Superseded,
}
impl From<&DecisionRecord> for Decision {
    fn from(record: &DecisionRecord) -> Self {
        match record {
            DecisionRecord::Allow => Self::Allow,
            DecisionRecord::Answers { answers } => Self::Answers { answers: answers.clone() },
            DecisionRecord::Deny { message } => Self::Deny { message: message.clone() },
            DecisionRecord::Accept { text, content } => Self::Accept { text: text.clone(), content: content.clone() },
            DecisionRecord::Decline => Self::Decline,
            DecisionRecord::Superseded => Self::Superseded,
        }
    }
}
impl From<&Decision> for DecisionRecord {
    fn from(decision: &Decision) -> Self {
        match decision {
            Decision::Allow => Self::Allow,
            Decision::Answers { answers } => Self::Answers { answers: answers.clone() },
            Decision::Deny { message } => Self::Deny { message: message.clone() },
            Decision::Accept { text, content } => Self::Accept { text: text.clone(), content: content.clone() },
            Decision::Decline => Self::Decline,
            Decision::Superseded => Self::Superseded,
        }
    }
}

/// What the card of a decision waits for: a permission or a plan is allowed or denied, a question
/// or an MCP server's request for input is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    Permission,
    Question,
}

/// One decision in a task's journal, as it is kept. Data only: a task's lifecycle is what the
/// aggregate says after replaying its journal, never a field read from a record. The task's DTO
/// yields no lifecycle either, whatever it was deserialized from:
///
/// ```compile_fail,E0277
/// let task: alondra_core::domain::Task = serde_json::from_str("{}").unwrap();
/// let lifecycle: alondra_domain::TaskLifecycle = task.into();
/// ```
/// Who an SDK execution runs as. An execution of the `claude --bg` backend has none of its own:
/// its identity is Alondra's, by task and generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    pub conversation: String,
    pub instance: String,
    pub host: String,
}

/// What the SDK observed of an execution, normalized: the facts the domain applies to its task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeObservation {
    ResourceAvailable,
    TurnStarted { turn: String, input: String },
    /// A proven root turn ended; optional failure reactions could not be recorded.
    TurnEnded { turn: String, failure: Option<SdkApiFailure>, failed: Option<String> },
    /// Work that outlived its turn woke Claude, which took it up in a root turn of its own.
    TurnResumed { turn: String, continuation: String },
    /// Claude opened a root turn by itself, with nothing the task gave it or knows of.
    TurnUnprompted { turn: String },
    InputConsumed { input: String },
    WorkStarted { turn: String, work: String },
    WorkEnded { work: String, continuation: String },
    /// Work ended while the root turn that started it was still open: that turn goes on with it.
    WorkContinued { continuation: String },
    /// The model answered: the API and the account Claude Code uses work. A fact about Claude,
    /// not about the task: it records nothing in the task's journal.
    ModelAnswered,
    /// Alondra took over the execution, which had no host of its own, with a new one.
    TakenOver,
    /// The host of the last takeover showed nothing: the next takeover starts a fresh host.
    TakeoverUnanswered,
}

/// What a turn ended waiting for, as the journal keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum TurnAwait {
    Instruction,
    User { card: InteractionId },
    Orchestrator { card: InteractionId },
    Machine,
    Tasks { tasks: Vec<TaskId> },
}
impl TurnAwait {
    pub(in crate::domain) fn of(awaits: &Await) -> Self {
        match awaits {
            Await::Instruction => Self::Instruction,
            Await::User(card) => Self::User { card: card.clone() },
            Await::Orchestrator(card) => Self::Orchestrator { card: card.clone() },
            Await::Machine => Self::Machine,
            Await::Tasks(tasks) => Self::Tasks { tasks: tasks.iter().cloned().collect() },
        }
    }
    fn awaits(&self, task: &TaskId) -> Result<Await> {
        Ok(match self {
            Self::Instruction => Await::Instruction,
            Self::User { card } => Await::User(card.clone()),
            Self::Orchestrator { card } => Await::Orchestrator(card.clone()),
            Self::Machine => Await::Machine,
            Self::Tasks { tasks } => Await::Tasks(Several::new(tasks.iter().cloned()).ok_or_else(|| corrupt(task, "a turn that awaits no task"))?),
        })
    }
}

/// Who a pause waits on, as the journal keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseOwner {
    User,
    Gate,
    Machine,
    Disk,
    Usage,
    ClaudeApi,
    Orchestrator,
}
impl From<Owner> for PauseOwner {
    fn from(owner: Owner) -> Self {
        match owner {
            Owner::User => Self::User,
            Owner::Capacity(Capacity::Gate) => Self::Gate,
            Owner::Capacity(Capacity::Machine) => Self::Machine,
            Owner::Capacity(Capacity::Disk) => Self::Disk,
            Owner::Capacity(Capacity::Usage) => Self::Usage,
            Owner::Capacity(Capacity::ClaudeApi) => Self::ClaudeApi,
            Owner::Orchestrator => Self::Orchestrator,
        }
    }
}
impl From<PauseOwner> for Owner {
    fn from(owner: PauseOwner) -> Self {
        match owner {
            PauseOwner::User => Self::User,
            PauseOwner::Gate => Self::Capacity(Capacity::Gate),
            PauseOwner::Machine => Self::Capacity(Capacity::Machine),
            PauseOwner::Disk => Self::Capacity(Capacity::Disk),
            PauseOwner::Usage => Self::Capacity(Capacity::Usage),
            PauseOwner::ClaudeApi => Self::Capacity(Capacity::ClaudeApi),
            PauseOwner::Orchestrator => Self::Orchestrator,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleRecord {
    Planned { input: String },
    Admitted {
        node: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution: Option<ExecutionIdentity>,
    },
    ResourceAvailable,
    TurnStarted { turn: String, input: String },
    /// A root turn Claude opened by itself to take up the `continuation` of work that outlived
    /// its turn.
    TurnResumed { turn: String, continuation: String },
    /// A root turn Claude opened by itself, with nothing the task gave it or knows of: the cards
    /// that asked how it goes on no longer hold.
    TurnUnprompted { turn: String },
    InputConsumed { input: String },
    WorkStarted { turn: String, work: String },
    WorkEnded { work: String, continuation: String },
    /// The root turn still open when work ended goes on with the work's `continuation`.
    WorkContinued { continuation: String },
    /// The root turn of an SDK execution ended. When nothing explains the task's next step and it
    /// `awaits` nothing, the first such turn wakes the node with `wake`, told `text`, and the next
    /// one, or one recorded before turns woke the node, requires `unheard_card`: the
    /// orchestrator's when the turn `stalled`, with nothing for the user to decide.
    TurnEnded {
        turn: String,
        unheard_card: InteractionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        awaits: Option<TurnAwait>,
        #[serde(default, skip_serializing_if = "is_false")]
        stalled: bool,
    },
    /// The user answered a card the lifecycle requires: the task goes on with `input`, whose text
    /// is what it is told.
    Answered {
        card: InteractionId,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    ExecutionReleased { input: String },
    /// The execution of a delivery the user reviews is gone and nothing is queued for a next one.
    ExecutionLeftForReview,
    /// Someone told the task's node `text` outside a card, as `input`. The store lets the text
    /// go once the task ended, so a journal read back may have none.
    Told {
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// The node's own card whose answer the input carries.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<InteractionId>,
    },
    Presented { delivery: InteractionId },
    /// The standing delivery left: the task goes on with `input`. An SDK execution is told `text`,
    /// what happened and that it delivers again; a `claude --bg` node is told through its mod.
    HandedBack {
        delivery: InteractionId,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    Accepted { delivery: InteractionId },
    Finished { intent: TaskEnding, cleanup: String, post_merge: Option<String> },
    Drained { obligation: Drained },
    /// The node declared work it runs after its PR merges; only `PostMergeEnded` ends it.
    PostMergeDeclared { operation: String },
    PostMergeEnded { operation: String, fact: PostMergeEnd },
    Reopened { input: String },
    HeldForPlanReview { review: InteractionId },
    PlanReviewSettled { review: InteractionId, plan_revision: u64, outcome: ReviewOutcome },
    /// Held by `card` until something outside the task changes: Claude's sign-in or the API.
    Paused {
        card: InteractionId,
        /// Who the pause waits on; a pause recorded before this was named reads it from its card.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<PauseOwner>,
    },
    /// The hold `card` ended: the task goes on with `input`, whose text is what it is told.
    Resumed {
        card: InteractionId,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// The hold `card` ended with nothing new to tell: what the task goes on with already waits.
    PauseEnded { card: InteractionId },
    /// The user stopped the SDK host; the task retains its place without a decision card.
    HostStopped { unheard_card: InteractionId },
    /// The SDK execution's host ended, and its native Claude with it. When that leaves nothing to
    /// explain the task's next step, the session a new host resumes goes on with `resume`, told
    /// `text`, or, without one, the aggregate requires `unheard_card`.
    HostEnded {
        unheard_card: InteractionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// Claude accepted the interrupt of the root `turn`, which is still open.
    InterruptAccepted { turn: String },
    /// `input` was handed to a host that ended before it showed the input was read: `card` asks
    /// the user whether to send it again.
    HandoffDoubted { card: InteractionId, input: String },
    /// `input` provably never reached the ended host: `resent`, told the same `text`, takes its place.
    HandoffResent {
        input: String,
        resent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// `input` was handed to a host that ended before it showed the input was read, and the session
    /// a new host resumes gets `resent`, told `text`, in its place, without asking anyone.
    DoubtResent {
        input: String,
        resent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// The user settled the doubtful input of `card`: `resent`, with `text`, takes its place, or
    /// it is taken as received. `unheard_card` is required when nothing else explains what follows.
    HandoffSettled {
        card: InteractionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        unheard_card: InteractionId,
    },
    /// Claude will never read the pending `input`: it leaves the queue and `card` asks the user
    /// whether to send it again.
    InputDropped { input: String, card: InteractionId },
    /// Alondra took over the execution, which had no host of its own, with a new one.
    TakenOver,
    /// The host of the last takeover showed nothing: what the execution waits for goes to a
    /// fresh host.
    TakeoverUnanswered,
    /// The user let go of the input `card` asked about. `unheard_card` is required when nothing
    /// else explains what follows, the orchestrator's when `stalled`.
    DroppedDiscarded {
        card: InteractionId,
        unheard_card: InteractionId,
        #[serde(default, skip_serializing_if = "is_false")]
        stalled: bool,
    },
    /// What the last turn ended waiting for no longer holds and nothing reached the node for it:
    /// it is woken with `wake`, told `text`, or, woken already with nothing new since, the
    /// aggregate requires `unheard_card`, the orchestrator's when `stalled`.
    AwaitEnded {
        unheard_card: InteractionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        stalled: bool,
    },
    /// The delivery's node rested with a step it owes after it, the answer it was told to its own
    /// card or its work after the merge: it is woken with `wake`, told `text`, or, woken already
    /// with nothing new since, the orchestrator settles `unheard_card`.
    Unfinished {
        unheard_card: InteractionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// The intent to run `operation` on the task's SDK execution, recorded before it reaches it.
    ControlRequested { control: String, operation: ControlRecord },
    /// The resting SDK execution is asked to compact its conversation with `input`.
    Compacted { input: String },
    /// The SDK execution asks the user, on `card`, for the decision its host persisted at `epoch`.
    DecisionAsked { card: InteractionId, epoch: u64, reason: DecisionReason },
    /// Alondra's own rules decided the request at `epoch` as it was asked: no card.
    DecidedByPolicy { epoch: u64, decision: DecisionRecord },
    /// The user answered `from`, one of the questions asked together, while others are open: the
    /// request waits on `to`, one of them.
    CardMoved { from: InteractionId, to: InteractionId },
    /// The question `card` outlives the execution that asked it, because the task moved to
    /// another account: its answer goes to the next execution as what the task is told.
    QuestionKept { card: InteractionId },
    /// The user decided `card`, the card of the request at `epoch`.
    Decided { card: InteractionId, epoch: u64, decision: DecisionRecord },
    /// The decision of the request at `epoch` reached Claude.
    DecisionSettled { epoch: u64 },
    /// The turn that asked at `epoch` goes on with the decision.
    DecisionContinued { epoch: u64 },
    /// The request at `epoch` ended without a decision reaching Claude. `unheard_card` is the card
    /// the aggregate requires when nothing else explains the task's next step.
    DecisionAborted { epoch: u64, unheard_card: InteractionId },
}

/// The aggregates of the tasks one transaction decides on, taken from the lifecycles the state
/// keeps when first needed and dropped with the transaction. Nothing clones or restores one from
/// the DTO.
///
/// ```compile_fail,E0277
/// fn duplicate<T: Clone>() {}
/// duplicate::<alondra_core::domain::Lifecycles>();
/// ```
///
/// ```compile_fail,E0277
/// fn restore<T: serde::de::DeserializeOwned>() {}
/// restore::<alondra_core::domain::Lifecycles>();
/// ```
#[derive(Debug)]
pub struct Lifecycles {
    replayed: BTreeMap<TaskId, Arc<TaskLifecycle>>,
}

impl Lifecycles {
    pub(super) fn new() -> Self {
        Self { replayed: BTreeMap::new() }
    }
}

/// Each task's lifecycle as its journal leads to it, kept with the state and moved on by the same
/// decision that journals a record: reading a task never replays its journal. Neither stored nor
/// compared, since it follows from the journal, which is both.
#[derive(Clone, Default)]
pub(super) struct LifecycleCache(BTreeMap<TaskId, Arc<TaskLifecycle>>);

impl PartialEq for LifecycleCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for LifecycleCache {}
impl std::fmt::Debug for LifecycleCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleCache").field("tasks", &self.0.len()).finish()
    }
}

fn corrupt(task: &TaskId, detail: impl std::fmt::Display) -> DomainError {
    DomainError::new("lifecycle_journal", format!("The lifecycle journal of task {task} cannot be replayed: {detail}"))
}

fn input_id(task: &TaskId, value: &str) -> Result<InputId> {
    InputId::new(value).map_err(|e| corrupt(task, format!("input {value:?}: {e:?}")))
}
fn operation_id(task: &TaskId, value: &str) -> Result<OperationId> {
    OperationId::new(value).map_err(|e| corrupt(task, format!("operation {value:?}: {e:?}")))
}
fn control_of(task: &TaskId, record: &ControlRecord) -> Result<Control> {
    Ok(match record {
        ControlRecord::Interrupt { turn } => Control::Interrupt {
            turn: turn.as_deref().map(|turn| TurnId::new(turn).map_err(|e| corrupt(task, format!("{e:?}")))).transpose()?,
        },
        ControlRecord::ForceStop { turn } => Control::ForceStop { turn: TurnId::new(turn.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))? },
        ControlRecord::SetModel { model } => Control::SetModel { model: model.clone() },
        ControlRecord::SetPermissionMode { mode } => Control::SetPermissionMode {
            mode: match mode {
                PermissionModeRecord::Default => PermissionMode::Default,
                PermissionModeRecord::Plan => PermissionMode::Plan,
                PermissionModeRecord::DontAsk => PermissionMode::DontAsk,
            },
        },
        ControlRecord::ReadMcpStatus => Control::ReadMcpStatus,
        ControlRecord::ReconnectMcp { server } => Control::ReconnectMcp { server: server.clone() },
        ControlRecord::StartMcpSignIn { server } => Control::StartMcpSignIn { server: server.clone() },
        ControlRecord::ClearMcpSignIn { server } => Control::ClearMcpSignIn { server: server.clone() },
    })
}
fn record_of_control(control: &Control) -> ControlRecord {
    match control {
        Control::Interrupt { turn } => ControlRecord::Interrupt { turn: turn.as_ref().map(|turn| turn.as_str().into()) },
        Control::ForceStop { turn } => ControlRecord::ForceStop { turn: turn.as_str().into() },
        Control::SetModel { model } => ControlRecord::SetModel { model: model.clone() },
        Control::SetPermissionMode { mode } => ControlRecord::SetPermissionMode {
            mode: match mode {
                PermissionMode::Default => PermissionModeRecord::Default,
                PermissionMode::Plan => PermissionModeRecord::Plan,
                PermissionMode::DontAsk => PermissionModeRecord::DontAsk,
            },
        },
        Control::ReadMcpStatus => ControlRecord::ReadMcpStatus,
        Control::ReconnectMcp { server } => ControlRecord::ReconnectMcp { server: server.clone() },
        Control::StartMcpSignIn { server } => ControlRecord::StartMcpSignIn { server: server.clone() },
        Control::ClearMcpSignIn { server } => ControlRecord::ClearMcpSignIn { server: server.clone() },
    }
}

/// The event a record stands for, against the aggregate it was decided on: runtime facts take
/// its current execution and the next cursor.
fn event(task: &TaskId, record: &LifecycleRecord, lifecycle: &TaskLifecycle) -> Result<Event> {
    let runtime = |fact: RuntimeFact| -> Result<Event> {
        let stamp = lifecycle.execution().ok_or_else(|| corrupt(task, "a runtime fact without an execution"))?.clone();
        Ok(Event::Runtime { stamp, cursor: lifecycle.cursor().unwrap_or(0) + 1, fact })
    };
    Ok(match record {
        LifecycleRecord::Planned { .. } => return Err(corrupt(task, "a second planned record")),
        LifecycleRecord::Admitted { node, execution: None } => Event::Admit {
            node: node.clone(),
            conversation: ConversationId::new(node.0.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            instance: InstanceId::new(claude_bg_instance(task, lifecycle.generation() + 1)).map_err(|e| corrupt(task, format!("{e:?}")))?,
            host: HostId::new(CLAUDE_BG_HOST).map_err(|e| corrupt(task, format!("{e:?}")))?,
        },
        LifecycleRecord::Admitted { node, execution: Some(identity) } => {
            if identity.host == CLAUDE_BG_HOST {
                return Err(corrupt(task, "an SDK execution on the claude --bg host"));
            }
            Event::Admit {
                node: node.clone(),
                conversation: ConversationId::new(identity.conversation.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
                instance: InstanceId::new(identity.instance.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
                host: HostId::new(identity.host.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            }
        }
        LifecycleRecord::WorkStarted { turn, work } => runtime(RuntimeFact::WorkStarted {
            turn: TurnId::new(turn.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            work: WorkId::new(work.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
        })?,
        LifecycleRecord::TurnEnded { turn, unheard_card, wake, awaits, stalled, .. } => runtime(RuntimeFact::TurnEnded {
            turn: TurnId::new(turn.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            unheard_card: unheard_card.clone(),
            wake: wake.as_deref().map(|input| input_id(task, input)).transpose()?,
            awaits: awaits.as_ref().map(|awaits| awaits.awaits(task)).transpose()?,
            stalled: *stalled,
        })?,
        LifecycleRecord::Answered { card, input, .. } => Event::Answer { card: card.clone(), input: input_id(task, input)? },
        LifecycleRecord::HostStopped { unheard_card } => runtime(RuntimeFact::HostStopped { unheard_card: unheard_card.clone() })?,
        LifecycleRecord::HostEnded { unheard_card, resume, .. } => runtime(RuntimeFact::HostEnded {
            unheard_card: unheard_card.clone(),
            resume: resume.as_deref().map(|input| input_id(task, input)).transpose()?,
        })?,
        LifecycleRecord::InterruptAccepted { turn } => runtime(RuntimeFact::InterruptAccepted {
            turn: TurnId::new(turn.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
        })?,
        LifecycleRecord::HandoffDoubted { card, input } => Event::DoubtHandoff { card: card.clone(), input: input_id(task, input)? },
        LifecycleRecord::HandoffResent { input, resent, .. } => Event::ResendUntaken { input: input_id(task, input)?, resend: input_id(task, resent)? },
        LifecycleRecord::DoubtResent { input, resent, .. } => Event::ResendDoubtful { input: input_id(task, input)?, resend: input_id(task, resent)? },
        LifecycleRecord::HandoffSettled { card, resent, unheard_card, .. } => Event::SettleHandoff {
            card: card.clone(),
            resend: resent.as_deref().map(|input| input_id(task, input)).transpose()?,
            unheard_card: unheard_card.clone(),
        },
        LifecycleRecord::Paused { card, owner } => Event::Pause {
            card: card.clone(),
            owner: owner.map_or_else(|| Owner::of_unnamed_pause(card), Owner::from),
        },
        LifecycleRecord::Resumed { card, input, .. } => Event::Resume { card: card.clone(), input: input_id(task, input)? },
        LifecycleRecord::PauseEnded { card } => Event::EndPause { card: card.clone() },
        LifecycleRecord::WorkEnded { work, continuation } => runtime(RuntimeFact::WorkEnded {
            work: WorkId::new(work.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            continuation: ContinuationId::new(continuation.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
        })?,
        LifecycleRecord::WorkContinued { continuation } => {
            if is_decision(lifecycle.generation(), continuation) {
                return Err(corrupt(task, format!("{continuation} is a decision's continuation, not work's")));
            }
            runtime(RuntimeFact::ContinuationConsumed {
                continuation: ContinuationId::new(continuation.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            })?
        }
        LifecycleRecord::ResourceAvailable => runtime(RuntimeFact::ResourceAvailable)?,
        LifecycleRecord::TurnStarted { turn, input } => runtime(RuntimeFact::TurnStarted {
            turn: TurnId::new(turn.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            trigger: Trigger::Input(input_id(task, input)?),
        })?,
        LifecycleRecord::TurnResumed { turn, continuation } => {
            if is_decision(lifecycle.generation(), continuation) {
                return Err(corrupt(task, format!("{continuation} is a decision's continuation, not work's")));
            }
            runtime(RuntimeFact::TurnStarted {
                turn: TurnId::new(turn.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
                trigger: Trigger::OwnedContinuation(ContinuationId::new(continuation.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?),
            })?
        }
        LifecycleRecord::TurnUnprompted { turn } => runtime(RuntimeFact::TurnStarted {
            turn: TurnId::new(turn.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            trigger: Trigger::Unprompted,
        })?,
        LifecycleRecord::InputConsumed { input } => runtime(RuntimeFact::InputConsumed { input: input_id(task, input)? })?,
        LifecycleRecord::InputDropped { input, card } => runtime(RuntimeFact::InputDropped { input: input_id(task, input)?, card: card.clone() })?,
        LifecycleRecord::TakenOver => runtime(RuntimeFact::TakenOver)?,
        LifecycleRecord::TakeoverUnanswered => runtime(RuntimeFact::TakeoverUnanswered)?,
        LifecycleRecord::DroppedDiscarded { card, unheard_card, stalled } => Event::Discard { card: card.clone(), unheard_card: unheard_card.clone(), stalled: *stalled },
        LifecycleRecord::AwaitEnded { unheard_card, wake, stalled, .. } => Event::AwaitEnded {
            unheard_card: unheard_card.clone(),
            wake: wake.as_deref().map(|input| input_id(task, input)).transpose()?,
            stalled: *stalled,
        },
        LifecycleRecord::Unfinished { unheard_card, wake, .. } => Event::Unfinished {
            unheard_card: unheard_card.clone(),
            wake: wake.as_deref().map(|input| input_id(task, input)).transpose()?,
        },
        LifecycleRecord::ExecutionReleased { input } => Event::ExecutionReleased { input: input_id(task, input)? },
        LifecycleRecord::ExecutionLeftForReview => Event::ExecutionLeftForReview,
        LifecycleRecord::Told { input, answers, .. } => Event::Tell { input: input_id(task, input)?, answers: answers.clone() },
        LifecycleRecord::Presented { delivery } => Event::Present { delivery: delivery.clone() },
        LifecycleRecord::HandedBack { delivery, input, .. } => Event::HandBack { delivery: delivery.clone(), input: input_id(task, input)? },
        LifecycleRecord::Accepted { delivery } => Event::Accept { delivery: delivery.clone(), merge: None },
        LifecycleRecord::Finished { intent, cleanup, post_merge } => Event::Finish {
            intent: match intent {
                TaskEnding::Completing => FinishIntent::Complete,
                TaskEnding::Cancelling => FinishIntent::Cancel,
            },
            cleanup: operation_id(task, cleanup)?,
            post_merge: post_merge.as_deref().map(|op| operation_id(task, op)).transpose()?,
        },
        LifecycleRecord::Drained { obligation } => Event::DrainObserved {
            obligation: match obligation {
                Drained::Execution { instance } => DrainObligation::Execution(
                    InstanceId::new(instance.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
                ),
                Drained::Cleanup { operation } => DrainObligation::Cleanup(operation_id(task, operation)?),
                Drained::PostMerge { operation } => DrainObligation::PostMerge(operation_id(task, operation)?),
                Drained::Work { work } => DrainObligation::Work(WorkId::new(work.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?),
                Drained::Callback { callback } => {
                    DrainObligation::Callback(CallbackId::new(callback.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?)
                }
            },
        },
        LifecycleRecord::PostMergeDeclared { operation } => Event::PostMergeDeclared { operation: operation_id(task, operation)? },
        LifecycleRecord::PostMergeEnded { operation, fact } => {
            Event::PostMergeEnded { operation: operation_id(task, operation)?, fact: (*fact).into() }
        }
        LifecycleRecord::Reopened { input } => Event::Reopen { input: input_id(task, input)? },
        LifecycleRecord::HeldForPlanReview { review } => Event::HoldForPlanReview { review: review.clone() },
        LifecycleRecord::PlanReviewSettled { review, plan_revision, outcome } => Event::PlanReviewSettled {
            review: review.clone(),
            plan_revision: *plan_revision,
            outcome: match outcome {
                ReviewOutcome::Unchanged => ReviewSettlement::Unchanged,
                ReviewOutcome::PlanChanged => ReviewSettlement::PlanChanged,
                ReviewOutcome::Cancelled => ReviewSettlement::Cancelled,
                ReviewOutcome::KeptByUser => ReviewSettlement::KeptByUser,
            },
        },
        LifecycleRecord::ControlRequested { control, operation } => Event::RequestControl {
            control: operation_id(task, control)?,
            kind: control_of(task, operation)?,
        },
        LifecycleRecord::Compacted { input } => Event::Compact { input: input_id(task, input)? },
        LifecycleRecord::DecisionAsked { card, epoch, reason } => runtime(RuntimeFact::Ask {
            card: card.clone(),
            callback: decision_callback(lifecycle.generation(), *epoch),
            reason: match reason {
                DecisionReason::Permission => WaitReason::Permission,
                DecisionReason::Question => WaitReason::Question,
            },
        })?,
        LifecycleRecord::DecidedByPolicy { epoch, decision } => runtime(RuntimeFact::AnsweredByPolicy {
            callback: decision_callback(lifecycle.generation(), *epoch),
            decision: decision.into(),
        })?,
        LifecycleRecord::Decided { card, epoch, decision } => {
            if lifecycle.card_callback(card).is_some_and(|callback| *callback != decision_callback(lifecycle.generation(), *epoch)) {
                return Err(corrupt(task, format!("card {card} decides another request than the one at epoch {epoch}")));
            }
            Event::Decide { card: card.clone(), decision: decision.into() }
        }
        LifecycleRecord::CardMoved { from, to } => Event::MoveCard { from: from.clone(), to: to.clone() },
        LifecycleRecord::QuestionKept { card } => Event::KeepQuestion { card: card.clone() },
        LifecycleRecord::DecisionSettled { epoch } => runtime(RuntimeFact::CallbackSettled {
            callback: decision_callback(lifecycle.generation(), *epoch),
            continuation: decision_continuation(lifecycle.generation(), *epoch),
        })?,
        LifecycleRecord::DecisionContinued { epoch } => runtime(RuntimeFact::ContinuationConsumed {
            continuation: decision_continuation(lifecycle.generation(), *epoch),
        })?,
        LifecycleRecord::DecisionAborted { epoch, unheard_card } => runtime(RuntimeFact::CallbackAborted {
            callback: decision_callback(lifecycle.generation(), *epoch),
            unheard_card: unheard_card.clone(),
        })?,
    })
}

/// Whether `continuation` is one a decision of the current execution resumes with.
fn is_decision(generation: u64, continuation: &str) -> bool {
    CallbackId::new(continuation).is_ok_and(|id| decision_epoch(generation, &id).is_some())
}

/// The epoch of a decision callback or continuation of the current execution.
fn epoch_of(task: &TaskId, lifecycle: &TaskLifecycle, id: &str) -> Result<u64> {
    let callback = CallbackId::new(id).map_err(|e| corrupt(task, format!("{e:?}")))?;
    decision_epoch(lifecycle.generation(), &callback).ok_or_else(|| corrupt(task, format!("{id} is no decision of execution {}", lifecycle.generation())))
}

/// The aggregate a journal yields. A record the aggregate refuses makes the whole journal
/// unreadable: nothing repairs it from the DTO.
pub fn replay<'a>(task: &TaskId, journal: impl IntoIterator<Item = &'a LifecycleRecord>) -> Result<TaskLifecycle> {
    let mut records = journal.into_iter();
    let Some(LifecycleRecord::Planned { input }) = records.next() else {
        return Err(corrupt(task, "it does not start with the planned record"));
    };
    let mut lifecycle = TaskLifecycle::planned(task.clone(), input_id(task, input)?).map_err(|r| corrupt(task, format!("{r:?}")))?;
    for (n, record) in records.enumerate() {
        let event = event(task, record, &lifecycle)?;
        lifecycle
            .apply(lifecycle.revision(), event)
            .map_err(|refusal| corrupt(task, format!("record {} {record:?} refused: {refusal:?}", n + 1)))?;
    }
    Ok(lifecycle)
}

/// What the store writes for a task's records from `from` on, in order: each event with the
/// revision it applies at, after creating the task or naming the input the record introduces.
#[derive(Debug, PartialEq, Eq)]
pub struct JournalWrite {
    pub revision: u64,
    pub event: Option<Event>,
    pub input: Option<InputId>,
    /// The text of `input`, when the record has one to tell the task.
    pub text: Option<String>,
}

/// The input a record names for the first time, which the store keeps before the event, and its
/// text when it has one.
fn introduced(record: &LifecycleRecord) -> Option<(&str, Option<&str>)> {
    match record {
        LifecycleRecord::Planned { input }
        | LifecycleRecord::ExecutionReleased { input }
        | LifecycleRecord::Reopened { input } => Some((input, None)),
        LifecycleRecord::Compacted { input } => Some((input, Some(COMPACT))),
        LifecycleRecord::Told { input, text, .. } => Some((input, text.as_deref())),
        LifecycleRecord::Answered { input, text, .. }
        | LifecycleRecord::Resumed { input, text, .. }
        | LifecycleRecord::HandedBack { input, text, .. } => Some((input, text.as_deref())),
        LifecycleRecord::HandoffSettled { resent: Some(input), text, .. }
        | LifecycleRecord::HandoffResent { resent: input, text, .. }
        | LifecycleRecord::DoubtResent { resent: input, text, .. }
        | LifecycleRecord::HostEnded { resume: Some(input), text, .. }
        | LifecycleRecord::TurnEnded { wake: Some(input), text, .. }
        | LifecycleRecord::AwaitEnded { wake: Some(input), text, .. }
        | LifecycleRecord::Unfinished { wake: Some(input), text, .. } => Some((input, text.as_deref())),
        _ => None,
    }
}

/// The store's writes for a task's journal from record `from` on. The first record creates the
/// task with its initial input and has no event.
pub fn journal_writes<'a>(task: &TaskId, journal: impl IntoIterator<Item = &'a LifecycleRecord>, from: usize) -> Result<Vec<JournalWrite>> {
    let mut records = journal.into_iter();
    if from == 0 {
        return journal_writes_after(task, None, records);
    }
    let known = replay(task, records.by_ref().take(from))?;
    journal_writes_after(task, Some(&known), records)
}

/// The store's writes for `records`, the ones a journal adds after the lifecycle `known` its
/// earlier records lead to; with no `known`, `records` is the whole journal. Only `records` are
/// applied.
pub fn journal_writes_after<'a>(task: &TaskId, known: Option<&TaskLifecycle>, records: impl IntoIterator<Item = &'a LifecycleRecord>) -> Result<Vec<JournalWrite>> {
    let mut records = records.into_iter();
    let mut writes = vec![];
    let planned;
    let base = match known {
        Some(known) => known,
        None => {
            let Some(LifecycleRecord::Planned { input }) = records.next() else {
                return Err(corrupt(task, "it does not start with the planned record"));
            };
            planned = TaskLifecycle::planned(task.clone(), input_id(task, input)?).map_err(|r| corrupt(task, format!("{r:?}")))?;
            writes.push(JournalWrite { revision: 0, event: None, input: Some(input_id(task, input)?), text: None });
            &planned
        }
    };
    let mut lifecycle: Option<TaskLifecycle> = None;
    for record in records {
        let current = lifecycle.as_ref().unwrap_or(base);
        let event = event(task, record, current)?;
        let revision = current.revision();
        let (input, text) = match introduced(record) {
            Some((input, text)) => (Some(input_id(task, input)?), text.map(str::to_owned)),
            None => (None, None),
        };
        writes.push(JournalWrite { revision, event: Some(event.clone()), input, text });
        let next = current.advanced(revision, event).map_err(|refusal| corrupt(task, format!("record {} {record:?} refused: {refusal:?}", revision + 1)))?;
        lifecycle = Some(next);
    }
    Ok(writes)
}

/// The record an event of the store stands for, given the aggregate before it. An event the
/// domain does not decide yet has none: the journal cannot be read.
fn record_of(task: &TaskId, event: &Event, texts: &BTreeMap<InputId, String>, lifecycle: &TaskLifecycle) -> Result<LifecycleRecord> {
    let drained = |obligation: &DrainObligation| -> Result<Drained> {
        Ok(match obligation {
            DrainObligation::Execution(instance) => Drained::Execution { instance: instance.as_str().into() },
            DrainObligation::Cleanup(operation) => Drained::Cleanup { operation: operation.as_str().into() },
            DrainObligation::PostMerge(operation) => Drained::PostMerge { operation: operation.as_str().into() },
            DrainObligation::Work(work) => Drained::Work { work: work.as_str().into() },
            DrainObligation::Callback(callback) => Drained::Callback { callback: callback.as_str().into() },
            other => return Err(corrupt(task, format!("a drained obligation the domain does not decide: {other:?}"))),
        })
    };
    Ok(match event {
        Event::Admit { node, host, .. } if host.as_str() == CLAUDE_BG_HOST => LifecycleRecord::Admitted { node: node.clone(), execution: None },
        Event::Admit { node, conversation, instance, host } => LifecycleRecord::Admitted {
            node: node.clone(),
            execution: Some(ExecutionIdentity {
                conversation: conversation.as_str().into(),
                instance: instance.as_str().into(),
                host: host.as_str().into(),
            }),
        },
        Event::Runtime { fact: RuntimeFact::WorkStarted { turn, work }, .. } => {
            LifecycleRecord::WorkStarted { turn: turn.as_str().into(), work: work.as_str().into() }
        }
        Event::Runtime { fact: RuntimeFact::TurnEnded { turn, unheard_card, wake, awaits, stalled }, .. } => LifecycleRecord::TurnEnded {
            turn: turn.as_str().into(),
            unheard_card: unheard_card.clone(),
            wake: wake.as_ref().map(|input| input.as_str().into()),
            text: wake.as_ref().and_then(|input| texts.get(input).cloned()),
            awaits: awaits.as_ref().map(TurnAwait::of),
            stalled: *stalled,
        },
        Event::Tell { input, answers } => LifecycleRecord::Told {
            input: input.as_str().into(),
            text: texts.get(input).cloned(),
            answers: answers.clone(),
        },
        Event::Answer { card, input } => LifecycleRecord::Answered {
            card: card.clone(),
            input: input.as_str().into(),
            text: texts.get(input).cloned(),
        },
        Event::Pause { card, owner } => LifecycleRecord::Paused { card: card.clone(), owner: Some((*owner).into()) },
        Event::Runtime { fact: RuntimeFact::HostStopped { unheard_card }, .. } => LifecycleRecord::HostStopped { unheard_card: unheard_card.clone() },
        Event::Runtime { fact: RuntimeFact::HostEnded { unheard_card, resume }, .. } => LifecycleRecord::HostEnded {
            unheard_card: unheard_card.clone(),
            resume: resume.as_ref().map(|input| input.as_str().into()),
            text: resume.as_ref().and_then(|input| texts.get(input).cloned()),
        },
        Event::Runtime { fact: RuntimeFact::InterruptAccepted { turn }, .. } => LifecycleRecord::InterruptAccepted { turn: turn.as_str().into() },
        Event::DoubtHandoff { card, input } => LifecycleRecord::HandoffDoubted { card: card.clone(), input: input.as_str().into() },
        Event::ResendUntaken { input, resend } => LifecycleRecord::HandoffResent {
            input: input.as_str().into(),
            resent: resend.as_str().into(),
            text: texts.get(resend).cloned(),
        },
        Event::ResendDoubtful { input, resend } => LifecycleRecord::DoubtResent {
            input: input.as_str().into(),
            resent: resend.as_str().into(),
            text: texts.get(resend).cloned(),
        },
        Event::SettleHandoff { card, resend, unheard_card } => LifecycleRecord::HandoffSettled {
            card: card.clone(),
            resent: resend.as_ref().map(|input| input.as_str().into()),
            text: resend.as_ref().and_then(|input| texts.get(input).cloned()),
            unheard_card: unheard_card.clone(),
        },
        Event::Resume { card, input } => LifecycleRecord::Resumed {
            card: card.clone(),
            input: input.as_str().into(),
            text: texts.get(input).cloned(),
        },
        Event::EndPause { card } => LifecycleRecord::PauseEnded { card: card.clone() },
        Event::Runtime { fact: RuntimeFact::WorkEnded { work, continuation }, .. } => {
            LifecycleRecord::WorkEnded { work: work.as_str().into(), continuation: continuation.as_str().into() }
        }
        Event::Runtime { fact: RuntimeFact::ResourceAvailable, .. } => LifecycleRecord::ResourceAvailable,
        Event::Runtime { fact: RuntimeFact::TurnStarted { turn, trigger: Trigger::Input(input) }, .. } => {
            LifecycleRecord::TurnStarted { turn: turn.as_str().into(), input: input.as_str().into() }
        }
        Event::Runtime { fact: RuntimeFact::TurnStarted { turn, trigger: Trigger::OwnedContinuation(continuation) }, .. } => {
            LifecycleRecord::TurnResumed { turn: turn.as_str().into(), continuation: continuation.as_str().into() }
        }
        Event::Runtime { fact: RuntimeFact::TurnStarted { turn, trigger: Trigger::Unprompted }, .. } => {
            LifecycleRecord::TurnUnprompted { turn: turn.as_str().into() }
        }
        Event::Runtime { fact: RuntimeFact::InputConsumed { input }, .. } => LifecycleRecord::InputConsumed { input: input.as_str().into() },
        Event::Runtime { fact: RuntimeFact::InputDropped { input, card }, .. } => {
            LifecycleRecord::InputDropped { input: input.as_str().into(), card: card.clone() }
        }
        Event::Runtime { fact: RuntimeFact::TakenOver, .. } => LifecycleRecord::TakenOver,
        Event::Runtime { fact: RuntimeFact::TakeoverUnanswered, .. } => LifecycleRecord::TakeoverUnanswered,
        Event::Discard { card, unheard_card, stalled } => LifecycleRecord::DroppedDiscarded { card: card.clone(), unheard_card: unheard_card.clone(), stalled: *stalled },
        Event::AwaitEnded { unheard_card, wake, stalled } => LifecycleRecord::AwaitEnded {
            unheard_card: unheard_card.clone(),
            wake: wake.as_ref().map(|input| input.as_str().into()),
            text: wake.as_ref().and_then(|input| texts.get(input).cloned()),
            stalled: *stalled,
        },
        Event::Unfinished { unheard_card, wake } => LifecycleRecord::Unfinished {
            unheard_card: unheard_card.clone(),
            wake: wake.as_ref().map(|input| input.as_str().into()),
            text: wake.as_ref().and_then(|input| texts.get(input).cloned()),
        },
        Event::ExecutionReleased { input } => LifecycleRecord::ExecutionReleased { input: input.as_str().into() },
        Event::ExecutionLeftForReview => LifecycleRecord::ExecutionLeftForReview,
        Event::Present { delivery } => LifecycleRecord::Presented { delivery: delivery.clone() },
        Event::HandBack { delivery, input } => LifecycleRecord::HandedBack {
            delivery: delivery.clone(),
            input: input.as_str().into(),
            text: texts.get(input).cloned(),
        },
        Event::Accept { delivery, merge: None } => LifecycleRecord::Accepted { delivery: delivery.clone() },
        Event::Finish { intent, cleanup, post_merge } => LifecycleRecord::Finished {
            intent: match intent {
                FinishIntent::Complete => TaskEnding::Completing,
                FinishIntent::Cancel => TaskEnding::Cancelling,
            },
            cleanup: cleanup.as_str().into(),
            post_merge: post_merge.as_ref().map(|op| op.as_str().into()),
        },
        Event::DrainObserved { obligation } => LifecycleRecord::Drained { obligation: drained(obligation)? },
        Event::PostMergeDeclared { operation } => LifecycleRecord::PostMergeDeclared { operation: operation.as_str().into() },
        Event::PostMergeEnded { operation, fact } => {
            LifecycleRecord::PostMergeEnded { operation: operation.as_str().into(), fact: (*fact).into() }
        }
        Event::Reopen { input } => LifecycleRecord::Reopened { input: input.as_str().into() },
        Event::HoldForPlanReview { review } => LifecycleRecord::HeldForPlanReview { review: review.clone() },
        Event::PlanReviewSettled { review, plan_revision, outcome } => LifecycleRecord::PlanReviewSettled {
            review: review.clone(),
            plan_revision: *plan_revision,
            outcome: match outcome {
                ReviewSettlement::Unchanged => ReviewOutcome::Unchanged,
                ReviewSettlement::PlanChanged => ReviewOutcome::PlanChanged,
                ReviewSettlement::Cancelled => ReviewOutcome::Cancelled,
                ReviewSettlement::KeptByUser => ReviewOutcome::KeptByUser,
            },
        },
        Event::RequestControl { control, kind } => LifecycleRecord::ControlRequested {
            control: control.as_str().into(),
            operation: record_of_control(kind),
        },
        Event::Compact { input } => LifecycleRecord::Compacted { input: input.as_str().into() },
        Event::Runtime { fact: RuntimeFact::Ask { card, callback, reason }, .. } => LifecycleRecord::DecisionAsked {
            card: card.clone(),
            epoch: epoch_of(task, lifecycle, callback.as_str())?,
            reason: match reason {
                WaitReason::Permission => DecisionReason::Permission,
                WaitReason::Question => DecisionReason::Question,
                other => return Err(corrupt(task, format!("a decision that waits for {other:?}"))),
            },
        },
        Event::Runtime { fact: RuntimeFact::AnsweredByPolicy { callback, decision }, .. } => LifecycleRecord::DecidedByPolicy {
            epoch: epoch_of(task, lifecycle, callback.as_str())?,
            decision: decision.into(),
        },
        Event::Decide { card, decision } => LifecycleRecord::Decided {
            card: card.clone(),
            epoch: epoch_of(task, lifecycle, lifecycle.card_callback(card).ok_or_else(|| corrupt(task, format!("card {card} decides nothing")))?.as_str())?,
            decision: decision.into(),
        },
        Event::MoveCard { from, to } => LifecycleRecord::CardMoved { from: from.clone(), to: to.clone() },
        Event::KeepQuestion { card } => LifecycleRecord::QuestionKept { card: card.clone() },
        Event::Runtime { fact: RuntimeFact::CallbackSettled { callback, .. }, .. } => {
            LifecycleRecord::DecisionSettled { epoch: epoch_of(task, lifecycle, callback.as_str())? }
        }
        Event::Runtime { fact: RuntimeFact::ContinuationConsumed { continuation }, .. } if !is_decision(lifecycle.generation(), continuation.as_str()) => {
            LifecycleRecord::WorkContinued { continuation: continuation.as_str().into() }
        }
        Event::Runtime { fact: RuntimeFact::ContinuationConsumed { continuation }, .. } => {
            LifecycleRecord::DecisionContinued { epoch: epoch_of(task, lifecycle, continuation.as_str())? }
        }
        Event::Runtime { fact: RuntimeFact::CallbackAborted { callback, unheard_card }, .. } => LifecycleRecord::DecisionAborted {
            epoch: epoch_of(task, lifecycle, callback.as_str())?,
            unheard_card: unheard_card.clone(),
        },
        other => return Err(corrupt(task, format!("an event the domain does not decide: {other:?}"))),
    })
}

/// The records a task's journal in the store stands for. Each record must give back exactly the
/// event the store kept, identities and cursors included, or the journal cannot be read.
pub fn records_from_events(task: &TaskId, initial: &InputId, events: &[Event], texts: &BTreeMap<InputId, String>) -> Result<Vec<LifecycleRecord>> {
    let mut lifecycle = TaskLifecycle::planned(task.clone(), initial.clone()).map_err(|r| corrupt(task, format!("{r:?}")))?;
    let mut records = vec![LifecycleRecord::Planned { input: initial.as_str().into() }];
    for stored in events {
        let record = record_for(&lifecycle, stored, texts)?;
        let n = lifecycle.revision() + 1;
        lifecycle.apply(lifecycle.revision(), stored.clone()).map_err(|refusal| corrupt(task, format!("event {n} refused: {refusal:?}")))?;
        records.push(record);
    }
    Ok(records)
}

/// The record a stored event stands for, read from the lifecycle it applies to. The record must
/// give back exactly that event, identities and cursors included, or the journal cannot be read.
pub fn record_for(lifecycle: &TaskLifecycle, stored: &Event, texts: &BTreeMap<InputId, String>) -> Result<LifecycleRecord> {
    let task = lifecycle.task();
    let record = record_of(task, stored, texts, lifecycle)?;
    let derived = event(task, &record, lifecycle)?;
    if derived != *stored {
        return Err(corrupt(task, format!("event {} {stored:?} is not the one its record stands for ({derived:?})", lifecycle.revision() + 1)));
    }
    Ok(record)
}

/// A journal kept while an execution's instance was its node's id: the same decisions, each
/// drained execution named by the instance the execution has now.
pub fn with_execution_instances(task: &TaskId, journal: Vec<LifecycleRecord>) -> Result<Vec<LifecycleRecord>> {
    let mut records = journal.into_iter();
    let Some(LifecycleRecord::Planned { input }) = records.next() else {
        return Err(corrupt(task, "it does not start with the planned record"));
    };
    let mut lifecycle = TaskLifecycle::planned(task.clone(), input_id(task, &input)?).map_err(|r| corrupt(task, format!("{r:?}")))?;
    let mut renamed = vec![LifecycleRecord::Planned { input }];
    let mut node = None;
    for (n, record) in records.enumerate() {
        if let LifecycleRecord::Admitted { node: admitted, .. } = &record {
            node = Some(admitted.0.clone());
        }
        let record = match record {
            LifecycleRecord::Drained { obligation: Drained::Execution { instance } } => {
                let owed = owed(&lifecycle).into_iter().find(|owed| matches!(owed, Drained::Execution { .. }));
                match owed {
                    Some(owed) if node.as_deref() == Some(instance.as_str()) => LifecycleRecord::Drained { obligation: owed },
                    _ => return Err(corrupt(task, format!("record {} drains execution {instance}, which the task does not owe", n + 1))),
                }
            }
            other => other,
        };
        let event = event(task, &record, &lifecycle)?;
        lifecycle.apply(lifecycle.revision(), event).map_err(|refusal| corrupt(task, format!("record {} {record:?} refused: {refusal:?}", n + 1)))?;
        renamed.push(record);
    }
    Ok(renamed)
}

/// What the ending of a finishing task still owes that a receipt discharges. Work and callbacks
/// go with the node's session; work after the merge ends only with its own fact, never here.
pub fn owed(lifecycle: &TaskLifecycle) -> Vec<Drained> {
    lifecycle
        .drain()
        .filter_map(|obligation| match obligation {
            DrainObligation::Execution(instance) => Some(Drained::Execution { instance: instance.as_str().into() }),
            DrainObligation::Cleanup(operation) => Some(Drained::Cleanup { operation: operation.as_str().into() }),
            DrainObligation::Work(work) => Some(Drained::Work { work: work.as_str().into() }),
            DrainObligation::Callback(callback) => Some(Drained::Callback { callback: callback.as_str().into() }),
            DrainObligation::PostMerge(_) | DrainObligation::Merge(_) | DrainObligation::ExternalEffect(_) => None,
        })
        .collect()
}

fn ending_of(lifecycle: &TaskLifecycle) -> Option<TaskEnding> {
    lifecycle.finishing_intent().map(|intent| match intent {
        FinishIntent::Complete => TaskEnding::Completing,
        FinishIntent::Cancel => TaskEnding::Cancelling,
    })
}

impl Task {
    /// The task is ending and waits for the receipts of what its ending still owes.
    pub fn ending(&self) -> bool {
        self.ending.is_some()
    }
    /// Over, or ending and waiting for its receipts: nothing starts, assigns, launches or
    /// revives it, and its node is told to stop.
    pub fn over(&self) -> bool {
        self.state.terminal() || self.ending.is_some()
    }
    /// The user cancelled it: over, or ending because of that cancellation.
    pub fn cancelled(&self) -> bool {
        self.state == TaskState::Cancelled || self.ending == Some(TaskEnding::Cancelling)
    }
}

impl DomainState {
    /// The first execution after this reopening has not been admitted yet.
    pub fn pending_reopening(&self, task: &TaskId) -> Option<&str> {
        let task_state = self.tasks.get(task)?;
        if task_state.started || task_state.agent_id.is_some()
            || !matches!(task_state.state, TaskState::NotStarted | TaskState::WaitingDependency) {
            return None;
        }
        self.lifecycle_journal.get(task)?.iter().rev().find_map(|record| match record {
            LifecycleRecord::Reopened { input } => Some(Some(input.as_str())),
            LifecycleRecord::Admitted { .. } => Some(None),
            _ => None,
        }).flatten()
    }

    /// The input a task waits with, named after the revision that queued it: an identity of
    /// Alondra's own, never one of the SDK's.
    pub(super) fn next_input(&self, task: &TaskId) -> String {
        format!("input:{task}:{}", self.revision + 1)
    }
    /// Whether the host of `task` ended since its last pause: nothing of what it ran is left to stop.
    pub fn host_ended_since_pause(&self, task: &TaskId) -> bool {
        self.lifecycle_journal.get(task).is_some_and(|journal| journal.iter().rev()
            .take_while(|record| !matches!(record, LifecycleRecord::Paused { .. }))
            .any(|record| matches!(record, LifecycleRecord::HostEnded { .. } | LifecycleRecord::HostStopped { .. })))
    }
    /// Whether the node of `task` was told something, since its last delivery, by a command at or
    /// after `revision`: each input carries the revision that made it (`next_input`).
    pub(super) fn told_since(&self, task: &TaskId, revision: u64) -> bool {
        let prefix = format!("input:{task}:");
        self.lifecycle_journal.get(task).is_some_and(|journal| journal.iter().rev()
            .take_while(|record| !matches!(record, LifecycleRecord::Presented { .. }))
            .any(|record| matches!(record, LifecycleRecord::Told { input, .. }
                if input.strip_prefix(&prefix).and_then(|at| at.parse::<u64>().ok()).is_some_and(|at| at >= revision))))
    }
    pub(super) fn next_operation(&self, kind: &str, task: &TaskId) -> String {
        format!("{kind}:{task}:{}", self.revision + 1)
    }

    pub(super) fn lifecycle<'a>(&self, lifecycles: &'a mut Lifecycles, task: &TaskId) -> Result<&'a TaskLifecycle> {
        if !lifecycles.replayed.contains_key(task) {
            lifecycles.replayed.insert(task.clone(), self.lifecycle_of(task)?);
        }
        Ok(&lifecycles.replayed[task])
    }

    /// The lifecycle `task`'s journal leads to: the one the domain keeps, or a replay of the
    /// journal when the kept one is missing or behind it.
    pub fn lifecycle_of(&self, task: &TaskId) -> Result<Arc<TaskLifecycle>> {
        let journal = self.lifecycle_journal.get(task).ok_or_else(|| DomainError::new("lifecycle_journal", format!("Task {task} has no lifecycle journal")))?;
        match self.lifecycles.0.get(task) {
            Some(kept) if usize::try_from(kept.revision()).is_ok_and(|revision| revision + 1 == journal.len()) => Ok(Arc::clone(kept)),
            _ => Ok(Arc::new(replay(task, journal)?)),
        }
    }

    /// Whether every journaled task keeps the lifecycle a full replay of its journal gives: what
    /// tests hold the cache to, without the replay `lifecycle_of` falls back on.
    pub fn kept_lifecycles_match_journals(&self) -> std::result::Result<(), String> {
        match self.lifecycle_journal.iter().find(|(task, _)| !self.lifecycles.0.contains_key(*task)) {
            Some((task, journal)) => Err(format!("task {task} keeps no lifecycle for its journal of {} records", journal.len())),
            None => self.kept_lifecycles_agree_with_journals(),
        }
    }

    /// Whether every lifecycle the domain keeps is the one a full replay of its task's journal
    /// gives. A state read without them, as from JSON, keeps none until a decision moves a task.
    pub fn kept_lifecycles_agree_with_journals(&self) -> std::result::Result<(), String> {
        for (task, kept) in &self.lifecycles.0 {
            let journal = self.lifecycle_journal.get(task).ok_or_else(|| format!("task {task} keeps a lifecycle and has no journal"))?;
            let replayed = replay(task, journal).map_err(|error| error.message)?;
            if **kept != replayed {
                return Err(format!("task {task} keeps revision {} of its lifecycle; its journal of {} records replays to revision {}: {kept:?} ≠ {replayed:?}", kept.revision(), journal.len(), replayed.revision()));
            }
        }
        Ok(())
    }

    fn keep(&mut self, lifecycles: &mut Lifecycles, task: &TaskId, lifecycle: Arc<TaskLifecycle>) {
        lifecycles.replayed.insert(task.clone(), Arc::clone(&lifecycle));
        self.lifecycles.0.insert(task.clone(), lifecycle);
    }

    /// Opens the journal of a task that was just created: from here on only decisions move it.
    pub(super) fn plan_lifecycle(&mut self, lifecycles: &mut Lifecycles, task: &TaskId) -> Result<()> {
        let record = LifecycleRecord::Planned { input: self.next_input(task) };
        let lifecycle = Arc::new(replay(task, std::slice::from_ref(&record))?);
        self.lifecycle_journal.insert(task.clone(), vec![record]);
        self.materialize(task, &lifecycle);
        self.keep(lifecycles, task, lifecycle);
        Ok(())
    }

    /// A lifecycle decision on a task: the aggregate validates it, the journal keeps it and
    /// the task's DTO reads it, in one step. A refusal changes nothing.
    pub(super) fn decide(&mut self, lifecycles: &mut Lifecycles, task: &TaskId, record: LifecycleRecord) -> Result<()> {
        let lifecycle = self.lifecycle(lifecycles, task)?;
        let event = event(task, &record, lifecycle)?;
        let next = Arc::new(lifecycle.advanced(lifecycle.revision(), event).map_err(|refusal| refused(task, &record, lifecycle, refusal))?);
        self.lifecycle_journal.get_mut(task).expect("replayed above").push(record);
        self.materialize(task, &next);
        self.keep(lifecycles, task, next);
        Ok(())
    }

    /// Whether `record` would send `input` to the node: the store keeps an input with its text only
    /// for one that reaches the node, so a record names it only then. Nothing is recorded here.
    fn sends(&self, lifecycles: &mut Lifecycles, task: &TaskId, record: &LifecycleRecord, input: &str) -> bool {
        let Ok(lifecycle) = self.lifecycle(lifecycles, task) else { return false };
        let Ok(event) = event(task, record, lifecycle) else { return false };
        lifecycle.advanced(lifecycle.revision(), event).is_ok_and(|next| next.pending_inputs().iter().any(|pending| pending.as_str() == input))
    }

    /// The task's DTO reads the aggregate: node, start, ending and the state its phase stands for.
    /// `derive_states` then picks the attention state within that phase, never across it. Once the
    /// end of a task's node is recorded, the node and its start stay as history of the task.
    fn materialize(&mut self, task: &TaskId, lifecycle: &TaskLifecycle) {
        let phase = lifecycle.phase();
        let t = self.tasks.get_mut(task).expect("a task with a journal exists");
        let terminal = matches!(phase, TaskPhase::Completed | TaskPhase::Cancelled);
        if !terminal && t.wrap_up.node.is_none() {
            t.agent_id = lifecycle.execution().map(|stamp| stamp.node().clone());
            t.started = lifecycle.resource().is_some_and(|resource| resource != ResourceEvidence::Reserved);
        }
        t.ending = ending_of(lifecycle);
        if t.ending.is_some() || matches!(lifecycle.phase(),TaskPhase::Completed|TaskPhase::Cancelled) { t.pending_selection=None; }
        t.state = match phase {
            TaskPhase::Unstarted | TaskPhase::InProgress => if t.started { TaskState::Running } else { TaskState::NotStarted },
            TaskPhase::Delivered => TaskState::ForYourReview,
            TaskPhase::Finishing => TaskState::Closing,
            TaskPhase::Completed => TaskState::Done,
            TaskPhase::Cancelled => TaskState::Cancelled,
        };
    }

    /// What the store keeps no column for: the ending of every task, read from its journal. The
    /// other lifecycle fields come from their columns and are checked by `validate_journal`.
    pub fn read_endings(&mut self) -> Result<()> {
        let tasks: Vec<TaskId> = self.lifecycle_journal.keys().cloned().collect();
        for task in tasks {
            let lifecycle = replay(&task, &self.lifecycle_journal[&task])?;
            self.read_ending(&task, &lifecycle)?;
            self.lifecycles.0.insert(task, Arc::new(lifecycle));
        }
        Ok(())
    }

    fn read_ending(&mut self, task: &TaskId, lifecycle: &TaskLifecycle) -> Result<()> {
        let t = self.tasks.get_mut(task).ok_or_else(|| DomainError::new("not_found", format!("Task {task} has a journal and no row")))?;
        t.ending = ending_of(lifecycle);
        if t.ending.is_some() || matches!(lifecycle.phase(),TaskPhase::Completed|TaskPhase::Cancelled) { t.pending_selection=None; }
        Ok(())
    }

    /// The journals the store read, each with the lifecycle its replay left: every task's ending
    /// is read from it and every task is checked against it, as `read_endings` and
    /// `validate_journal` do, without replaying any journal again. The lifecycles are kept for
    /// `validate_loaded`.
    pub fn load_journal<R: Into<Journal>>(&mut self, journals: impl IntoIterator<Item = (TaskId, R, TaskLifecycle)>) -> Result<Lifecycles> {
        let mut lifecycles = Lifecycles::new();
        for (task, records, lifecycle) in journals {
            self.lifecycle_journal.insert(task.clone(), records);
            self.keep(&mut lifecycles, &task, Arc::new(lifecycle));
        }
        for (task, lifecycle) in &lifecycles.replayed {
            self.read_ending(task, lifecycle)?;
        }
        self.validate_loaded(&lifecycles)?;
        Ok(lifecycles)
    }

    /// `validate_journal`, with the lifecycles `load_journal` kept.
    pub fn validate_loaded(&self, lifecycles: &Lifecycles) -> Result<()> {
        self.every_task_journaled()?;
        for (task, journal) in &self.lifecycle_journal {
            match lifecycles.replayed.get(task) {
                Some(lifecycle) => self.validate_lifecycle(task, lifecycle)?,
                None => self.validate_lifecycle(task, &replay(task, journal)?)?,
            }
        }
        Ok(())
    }

    /// Every task has a journal and agrees with it: what a loaded state proves before it is used.
    pub fn validate_journal(&self) -> Result<()> {
        self.every_task_journaled()?;
        self.validate_lifecycles(self.lifecycle_journal.keys())
    }

    fn every_task_journaled(&self) -> Result<()> {
        match self.tasks.keys().find(|task| !self.lifecycle_journal.contains_key(*task)) {
            Some(task) => Err(DomainError::new("lifecycle_journal", format!("Task {task} has no lifecycle journal"))),
            None => Ok(()),
        }
    }

    /// The aggregate rules: a task's DTO agrees with the replay of its journal, or the transition
    /// is refused. Checked for every task a transition touched.
    pub(super) fn validate_lifecycles<'a>(&self, tasks: impl Iterator<Item = &'a TaskId>) -> Result<()> {
        for task in tasks {
            self.validate_lifecycle(task, &*self.lifecycle_of(task)?)?;
        }
        Ok(())
    }

    fn validate_lifecycle(&self, task: &TaskId, lifecycle: &TaskLifecycle) -> Result<()> {
        let t = self.tasks.get(task).ok_or_else(|| DomainError::new("not_found", format!("Task {task} has a journal and no row")))?;
        if let Err(detail) = self.agrees(t, lifecycle) {
            return Err(DomainError::new("lifecycle_mismatch", format!(
                "the aggregate rules: task {task} reads {:?} (started {}, node {:?}, ending {:?}) while its lifecycle is {:?}: {detail}",
                t.state, t.started, t.agent_id, t.ending, lifecycle.phase())));
        }
        Ok(())
    }

    fn agrees(&self, t: &Task, lifecycle: &TaskLifecycle) -> std::result::Result<(), String> {
        use TaskState as S;
        for (id, _) in lifecycle.cards() {
            let recipient = match lifecycle.card_owner(id) {
                Some(alondra_domain::Owner::Orchestrator) => Recipient::Orchestrator,
                _ => Recipient::User,
            };
            let actionable = self.interactions.get(id).is_some_and(|card| {
                let need = card.card.need.trim();
                card.scope == (Scope::Task { task_id: t.id.clone() }) && card.recipient == recipient
                    && !card.withdrawn
                    && matches!(card.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })
                    && matches!(card.card.kind, CardKind::Decide | CardKind::Review | CardKind::Authorize | CardKind::Provide | CardKind::Test | CardKind::Do | CardKind::Problem)
                    && !need.is_empty() && need.chars().count() <= 90 && !need.contains('\n') && !card.card.need.contains('\0')
            });
            if !actionable {
                return Err(format!("required card {id} is not actionable"));
            }
        }
        let phase = lifecycle.phase();
        let node = lifecycle.execution().map(|stamp| stamp.node());
        let started = lifecycle.resource().is_some_and(|resource| resource != ResourceEvidence::Reserved);
        let history = matches!(phase, TaskPhase::Completed | TaskPhase::Cancelled) || t.wrap_up.node.is_some();
        if !history && t.agent_id.as_ref() != node {
            return Err("the node differs from the execution".into());
        }
        if !history && t.started != started {
            return Err("the start differs from the execution".into());
        }
        if t.ending != ending_of(lifecycle) {
            return Err("the ending differs".into());
        }
        match phase {
            TaskPhase::Unstarted | TaskPhase::InProgress => {
                if t.final_presentation.is_some() {
                    return Err("a final delivery stands while the lifecycle is in progress".into());
                }
                if matches!(t.state, S::Closing | S::Done | S::Cancelled | S::ForYourReview | S::AwaitingBlocker) || (!started && t.state == S::Running) {
                    return Err("the state is not one of a task in progress".into());
                }
            }
            TaskPhase::Delivered => {
                if t.final_presentation.as_ref() != lifecycle.delivery() {
                    return Err("the final delivery differs".into());
                }
                if matches!(t.state, S::Done | S::Cancelled) || (t.state == S::Closing && !self.finished(t)) {
                    return Err("the state is not one of a delivered task".into());
                }
            }
            TaskPhase::Finishing => {
                let after_merge = lifecycle.finishing_intent() == Some(FinishIntent::Complete) && t.wrap_up.post_merge_work.is_some();
                if !(t.state == S::Closing || (after_merge && t.state == S::Running)) {
                    return Err("the state is not one of an ending task".into());
                }
            }
            TaskPhase::Completed => {
                if t.state != S::Done {
                    return Err("the state is not Done".into());
                }
            }
            TaskPhase::Cancelled => {
                if t.state != S::Cancelled {
                    return Err("the state is not Cancelled".into());
                }
            }
        }
        Ok(())
    }
}

impl DomainState {
    /// A node takes the task: a new execution of its lifecycle, after releasing one it still had.
    pub(super) fn admit_node(&mut self, task: &TaskId, node: &AgentId, lifecycles: &mut Lifecycles) -> Result<()> {
        if self.lifecycle(lifecycles, task)?.execution().is_some() {
            let record = LifecycleRecord::ExecutionReleased { input: self.next_input(task) };
            self.decide(lifecycles, task, record)?;
        }
        self.decide(lifecycles, task, LifecycleRecord::Admitted { node: node.clone(), execution: None })
    }

    /// The current SDK execution of `generation`: facts about another, or about one of the
    /// `claude --bg` backend, are refused.
    pub(super) fn sdk_execution(&self, task: &TaskId, generation: u64, lifecycles: &mut Lifecycles) -> Result<()> {
        current_sdk(task, generation, self.lifecycle(lifecycles, task)?)
    }

    /// The task's lifecycle, when its current execution is the SDK execution of `generation`: what
    /// a guard judges facts about that execution on.
    pub(super) fn sdk_lifecycle_at(&self, task: &TaskId, generation: u64) -> Result<Arc<TaskLifecycle>> {
        let lifecycle = self.lifecycle_of(task)?;
        current_sdk(task, generation, &lifecycle)?;
        Ok(lifecycle)
    }

    /// Moves the lifecycle a guard judges on by `record`, as `decide` would.
    pub(super) fn judge(&self, task: &TaskId, lifecycle: &mut Arc<TaskLifecycle>, record: &LifecycleRecord) -> Result<()> {
        let event = event(task, record, lifecycle)?;
        *lifecycle = Arc::new(lifecycle.advanced(lifecycle.revision(), event).map_err(|refusal| refused(task, record, lifecycle, refusal))?);
        Ok(())
    }

    /// The root turn of the task's SDK execution ended. A result of the SDK is evidence, never
    /// success: nothing is delivered, finished or released. A turn that ends with nothing that
    /// explains what the task does next (no work of its own, no continuation, no input waiting) is
    /// not empty when its node waits on something Alondra recorded (`turn_await`): the task waits
    /// for that. Otherwise the turn is empty, a stop that is Alondra's to fix and never the user's:
    /// the lifecycle hands the node back its turn at once with what is missing, up to
    /// `EMPTY_TURN_WAKES` times in a row since the task last moved on, and the next one opens a case
    /// the orchestrator settles (`open_stall`), created in this same step. The lifecycle counts
    /// them, by turns, never by time.
    ///
    /// A turn a Claude API error ended is held instead when waiting helps: on the one card that
    /// asks the user to sign in to Claude, or until the API or the usage limit lets it go on; when
    /// it is not held, it is a stop like an empty turn. A turn Claude Code ended as `failed`, the
    /// API refused asks for intervention. An interruption waits for a new instruction, without a card.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_turn_boundary(&mut self, task: &TaskId, generation: u64, turn: &str, failure: Option<&SdkApiFailure>, failed: Option<&str>, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        if self.turn_end_recorded(task, turn) || self.interactions.contains_key(&turn_ended_card_id(task, generation, turn)) || self.interactions.contains_key(&stalled_card_id(task, generation, turn)) {
            return Err(DomainError::new("conflict", format!("The end of turn {turn} of task {task} is already recorded")));
        }
        if let Some(failure) = failure {
            self.hold_after_failure(task, failure, effects, lifecycles)?;
        }
        let end = self.observed_turn_end(task, turn, failure, failed);
        let unexplained = end.is_none() && failure.is_none();
        self.finish_observed_turn(task, generation, turn, end, unexplained, effects, lifecycles)
    }

    /// The wait `failure` asks for, when it asks for one.
    pub(super) fn hold_after_failure(&mut self, task: &TaskId, failure: &SdkApiFailure, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        match classify_sdk(&failure.error, failure.status).and_then(ClaudeHold::for_failure) {
            Some(hold) => self.hold_on_claude(task, hold, failure.resets_at, effects, lifecycles),
            None => Ok(()),
        }
    }

    /// Whether the current execution already recorded the end of `turn`.
    fn turn_end_recorded(&self, task: &TaskId, turn: &str) -> bool {
        self.lifecycle_journal.get(task).is_some_and(|records| records.iter().rev()
            .take_while(|record| !matches!(record, LifecycleRecord::Admitted { .. }))
            .any(|record| matches!(record, LifecycleRecord::TurnEnded { turn: known, .. } if known == turn)))
    }

    fn observed_turn_end<'a>(&self, task: &TaskId, turn: &str, failure: Option<&'a SdkApiFailure>, failed: Option<&'a str>) -> Option<TurnEnd<'a>> {
        let kind = failure.and_then(|failure| classify_sdk(&failure.error, failure.status));
        let interrupted = self.lifecycle_journal.get(task).is_some_and(|journal| interrupted(journal, turn));
        let provider = self.tasks[task].provider;
        match failure.filter(|_| kind == Some(SdkFailureKind::NeedsUser)) {
            Some(failure) => Some(TurnEnd::Refused(failure)),
            None if interrupted => Some(TurnEnd::Interrupted),
            None => failed.map(|code| match code {
                "interrupted" => TurnEnd::Interrupted,
                "codex_request_rejected" if provider == crate::accounts::Provider::Codex => TurnEnd::NativeRequestRejected,
                "codex_context_exceeded" if provider == crate::accounts::Provider::Codex => TurnEnd::NativeContextExceeded,
                code if provider == crate::accounts::Provider::Codex => TurnEnd::NativeFailed(code),
                code => TurnEnd::Failed(code),
            }),
        }
    }

    /// A failed reaction cannot erase evidence that the current root turn ended. Repeated
    /// evidence of an end already recorded in this execution changes nothing.
    #[allow(clippy::too_many_arguments)]
    fn record_observed_turn_end(&mut self, task: &TaskId, generation: u64, turn: &str, failure: Option<&SdkApiFailure>, failed: Option<&str>,
        effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        if self.turn_end_recorded(task, turn) {
            return Ok(());
        }
        let end = self.observed_turn_end(task, turn, failure, failed).or_else(|| failure.map(TurnEnd::Refused));
        let unexplained = end.is_none() && failure.is_none();
        self.finish_observed_turn(task, generation, turn, end, unexplained, effects, lifecycles)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_observed_turn(&mut self, task: &TaskId, generation: u64, turn: &str,
        end: Option<TurnEnd<'_>>, unexplained: bool, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let stalled = end.is_none();
        let card = if stalled { stalled_card_id(task, generation, turn) } else { turn_ended_card_id(task, generation, turn) };
        let paused = self.tasks.get(task).and_then(|task| task.agent_id.as_ref())
            .and_then(|agent| self.node_pause(agent)).is_some_and(|pause| !pause.released);
        let awaits = if matches!(end, Some(TurnEnd::Interrupted)) && !paused {
            Some(TurnAwait::Instruction)
        } else {
            self.turn_await(task).filter(|_| unexplained)
        };
        let empty = unexplained && awaits.is_none();
        let wake = (empty && self.lifecycle(lifecycles, task)?.hands_back_an_empty_turn()).then(|| self.next_input(task));
        if wake.is_none() && awaits.is_none() {
            self.consume_notifications_read(task, generation, lifecycles)?;
        }
        let wakes = self.lifecycle(lifecycles, task)?.empty_turns() + 1;
        let text = wake.as_ref().map(|_| self.empty_turn(task).wake_text_at(wakes));
        self.decide(lifecycles, task, LifecycleRecord::TurnEnded { turn: turn.into(), unheard_card: card.clone(), text, wake, awaits, stalled })?;
        let owed = if empty { self.owed_step(task, self.lifecycle(lifecycles, task)?) } else { None };
        if let Some(owed) = &owed {
            let wake = self.wake_for(task, lifecycles)?;
            let wakes = self.lifecycle(lifecycles, task)?.empty_turns() + 1;
            let text = wake.as_ref().map(|_| owed.wake_text_at(wakes));
            self.decide(lifecycles, task, LifecycleRecord::Unfinished { unheard_card: card.clone(), text, wake })?;
        }
        let lifecycle = self.lifecycle(lifecycles, task)?;
        if lifecycle.phase() == TaskPhase::Delivered && lifecycle.activity() == ActivityView::Resting
            && let Some(agent) = self.tasks[task].agent_id.clone() {
            self.return_replied_delivery(&agent);
        }
        if self.lifecycle(lifecycles, task)?.cards().iter().any(|(required, _)| **required == card) {
            match end {
                None => {
                    let why = owed.unwrap_or_else(|| self.empty_turn(task));
                    self.open_stall_on(task, card, why, effects);
                }
                Some(end) => {
                    let title = self.tasks[task].title.clone();
                    self.interactions.insert(card.clone(), turn_ended_card(card.clone(), task, &title, end, SystemCard::TurnEnded, self.revision + 1));
                    effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
                }
            }
        }
        Ok(())
    }

    /// The input that hands an empty turn of `task` back to its node, while its empty turns in a
    /// row have not reached `EMPTY_TURN_WAKES`; the lifecycle decides whether it reaches the node.
    fn wake_for(&self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<Option<String>> {
        Ok((!stalled_before(self.lifecycle(lifecycles, task)?)).then(|| self.next_input(task)))
    }

    /// A turn that ends without waking the node or waiting on anything read the notifications it
    /// found: they are consumed before it ends, or the task would stay woken for work that nobody
    /// is going to tell the node about again, with no card for the stop.
    fn consume_notifications_read(&mut self, task: &TaskId, generation: u64, lifecycles: &mut Lifecycles) -> Result<()> {
        let read: Vec<String> = self.lifecycle(lifecycles, task)?.notifications_read().into_iter()
            .map(|continuation| continuation.as_str().to_owned())
            .filter(|continuation| !is_decision(generation, continuation))
            .collect();
        for continuation in read {
            self.decide(lifecycles, task, LifecycleRecord::WorkContinued { continuation })?;
        }
        Ok(())
    }

    /// What the node of `task` waits for as its turn ends, as Alondra recorded it: its question or
    /// request to the user still open, a pause that holds it (`wait_for_machine`), its question or
    /// message to the orchestrator still open, or the tasks its task depends on that are not done.
    /// A suggestion is no wait: the node goes on without its answer. The input that ends the wait
    /// reaches the node and moves the task on.
    fn turn_await(&self, task: &TaskId) -> Option<TurnAwait> {
        let node = self.tasks.get(task)?.agent_id.clone()?;
        if let Some(card) = self.node_waits_on(task, &node, &Recipient::User, self.revision) {
            return Some(TurnAwait::User { card: card.clone() });
        }
        if let Some(card) = self.node_waits_on(task, &node, &Recipient::Orchestrator, self.revision) {
            return Some(TurnAwait::Orchestrator { card: card.clone() });
        }
        let unmet: Vec<TaskId> = self.unmet_dependency_ids(self.tasks.get(task)?).into_iter().cloned().collect();
        (!unmet.is_empty()).then_some(TurnAwait::Tasks { tasks: unmet })
    }

    /// A wait a turn ended on that ended with nothing told to the node (its question or message
    /// withdrawn unanswered, the tasks it waited on no longer its dependencies) leaves that turn
    /// empty after all: the node is woken, or the orchestrator gets the case, as for a turn that
    /// ends empty. A wait the node is told the end of (an answer, the machine's room, a dependency
    /// done) ends with what it is told.
    pub(super) fn end_lapsed_awaits(&mut self, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let candidates: Vec<(TaskId, InteractionId)> = self.lifecycle_journal.iter()
            .filter(|(task, _)| self.tasks.get(*task).is_some_and(|t| !t.state.terminal()))
            .filter_map(|(task, journal)| journal.iter().rev().find_map(|record| match record {
                LifecycleRecord::TurnEnded { unheard_card, awaits, .. } => Some(awaits.as_ref().map(|_| unheard_card.clone())),
                LifecycleRecord::AwaitEnded { .. } => Some(None),
                _ => None,
            }).flatten().map(|card| (task.clone(), card)))
            .collect();
        for (task, card) in candidates {
            let Some(awaits) = self.lifecycle(lifecycles, &task)?.awaits().cloned() else { continue };
            let card = stalled_in_place_of(&card);
            if !self.lapsed(&task, &awaits) || self.interactions.contains_key(&card) {
                continue;
            }
            let lifecycle = self.lifecycle(lifecycles, &task)?;
            let executing = lifecycle.phase() == TaskPhase::InProgress && lifecycle.cards().is_empty() && lifecycle.dependencies().is_empty();
            let wake = if executing { self.wake_for(&task, lifecycles)? } else { None };
            let wakes = self.lifecycle(lifecycles, &task)?.empty_turns() + 1;
            let text = wake.as_ref().map(|_| self.empty_turn(&task).wake_text_at(wakes));
            self.decide(lifecycles, &task, LifecycleRecord::AwaitEnded { unheard_card: card.clone(), text, wake, stalled: true })?;
            if self.lifecycle(lifecycles, &task)?.cards().iter().any(|(required, _)| **required == card) {
                self.open_stall(&task, card, effects);
            }
        }
        Ok(())
    }

    /// Why a turn of `task` that ended with nothing explaining its next step is empty.
    pub fn empty_turn(&self, task: &TaskId) -> EmptyTurn {
        let Some(t) = self.tasks.get(task) else { return EmptyTurn::Unexplained };
        self.undelivered_pr(t).map_or(EmptyTurn::Unexplained, |pr| EmptyTurn::UnpresentedPr { number: pr.number })
    }

    /// The step the node of `task` still owes its delivery while it rests: the answer to its own
    /// card it was told and took up, with nothing told to it since, or, once a turn ran after the
    /// delivery, the work it said runs after the merge. None once Alondra left the case of such a
    /// rest unsettled: the delivery waits for the user.
    pub fn owed_step(&self, task: &TaskId, lifecycle: &TaskLifecycle) -> Option<EmptyTurn> {
        if lifecycle.phase() != TaskPhase::Delivered || lifecycle.activity() != ActivityView::Resting
            || !lifecycle.cards().is_empty() || lifecycle.resource() != Some(ResourceEvidence::Available) {
            return None;
        }
        let since = self.since_delivery(task)?;
        if self.delivery_left_to_the_user(&since) {
            return None;
        }
        self.unacted_answer(task).or_else(|| {
            let ran = since.iter().any(|record| matches!(record, LifecycleRecord::TurnStarted { .. } | LifecycleRecord::InputConsumed { .. }));
            let reason = self.work_after_merge(task, lifecycle).filter(|_| ran)?;
            Some(EmptyTurn::AfterMerge { reason })
        })
    }

    /// What the journal of `task` holds since its last delivery, newest first.
    fn since_delivery(&self, task: &TaskId) -> Option<Vec<&LifecycleRecord>> {
        Some(self.lifecycle_journal.get(task)?.iter().rev()
            .take_while(|record| !matches!(record, LifecycleRecord::Presented { .. }))
            .collect())
    }

    /// Whether Alondra left the case of a rest unsettled in `since`: the delivery waits for the user.
    fn delivery_left_to_the_user(&self, since: &[&LifecycleRecord]) -> bool {
        since.iter().any(|record| matches!(record, LifecycleRecord::Answered { card, .. } if self.left_unsettled(card)))
    }

    /// Whether the node of `task`, as `lifecycle` has it, owes work on what the user sent back of
    /// its delivery and nothing will start a turn: it runs nothing, nothing waits to reach it, it
    /// waits for nobody and no card holds it. A task only waits for capacity or the user, so this
    /// state never lasts.
    pub fn stranded(&self, task: &TaskId, lifecycle: &TaskLifecycle) -> bool {
        lifecycle.phase() == TaskPhase::Delivered
            && lifecycle.activity() == ActivityView::Resting
            && lifecycle.resource() == Some(ResourceEvidence::Available)
            && lifecycle.awaits().is_none()
            && lifecycle.pending_inputs().is_empty()
            && lifecycle.cards().is_empty()
            && self.turn_await(task).is_none()
            && self.owes_what_was_sent_back(task)
            && self.since_delivery(task).is_some_and(|since| !self.delivery_left_to_the_user(&since))
    }

    /// The user's hand-back of the delivery of `task` still stands, or their reply has no answer.
    fn owes_what_was_sent_back(&self, task: &TaskId) -> bool {
        let delivery = self.tasks.get(task).and_then(|t| t.final_presentation.as_ref()).and_then(|delivery| self.interactions.get(delivery));
        self.task_handed_back(task).is_some() || delivery.is_some_and(|delivery| self.open_reply(delivery))
    }

    /// The answer to its own card the node of `task` was told last, when a turn took it up after
    /// the last delivery and nothing else was told to the node or opened a case after it. The
    /// answer may have been told before that delivery: a node that presented again without having
    /// read it did not act on it.
    fn unacted_answer(&self, task: &TaskId) -> Option<EmptyTurn> {
        let mut taken: BTreeSet<&str> = BTreeSet::new();
        let mut presented = false;
        for record in self.lifecycle_journal.get(task)?.iter().rev() {
            match record {
                LifecycleRecord::Presented { .. } => presented = true,
                LifecycleRecord::HandedBack { .. } if presented => {}
                LifecycleRecord::TurnStarted { input, .. } | LifecycleRecord::InputConsumed { input } => {
                    if !presented {
                        taken.insert(input);
                    }
                }
                LifecycleRecord::HandoffResent { input, resent, .. } | LifecycleRecord::DoubtResent { input, resent, .. } => {
                    if taken.contains(resent.as_str()) {
                        taken.insert(input);
                    }
                }
                LifecycleRecord::Told { input, answers: Some(card), .. } => {
                    return self.answered_card(task, card).filter(|_| taken.contains(input.as_str()));
                }
                LifecycleRecord::Unfinished { wake: None, .. } => return None,
                LifecycleRecord::TurnEnded { .. } | LifecycleRecord::AwaitEnded { .. } | LifecycleRecord::Unfinished { .. } => {}
                record if introduced(record).is_some() => return None,
                _ => {}
            }
        }
        None
    }

    /// `card`, a card the node of `task` asked the user on it, with the answer it has.
    fn answered_card(&self, task: &TaskId, card: &InteractionId) -> Option<EmptyTurn> {
        let node = self.tasks.get(task)?.agent_id.as_ref()?;
        let i = self.interactions.get(card)?;
        let theirs = matches!(&i.author, Author::Agent { agent_id } if agent_id == node)
            && i.recipient == Recipient::User
            && matches!(&i.scope, Scope::Task { task_id } if task_id == task);
        let said = answer_said(i).filter(|_| theirs)?;
        Some(EmptyTurn::UnactedAnswer { card: card.clone(), need: i.card.effective_title().to_owned(), said: said.trim_end_matches('.').to_owned() })
    }

    /// Whether `card` is a case about a stopped node that Alondra settled itself, for nobody settled it.
    fn left_unsettled(&self, card: &InteractionId) -> bool {
        self.interactions.get(card).is_some_and(|i| is_stall_case(i) && matches!(&i.payload,
            InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == STALL_CLOSE))
    }

    /// Whether what a turn ended waiting for ended with nothing told to the node.
    fn lapsed(&self, task: &TaskId, awaits: &Await) -> bool {
        match awaits {
            Await::User(card) | Await::Orchestrator(card) => self.interactions.get(card).is_none_or(|i| i.withdrawn && match &i.payload {
                InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. } => answer.is_none(),
                _ => true,
            }),
            Await::Machine | Await::Instruction => false,
            Await::Tasks(tasks) => self.tasks.get(task).is_none_or(|t| !tasks.iter().any(|dependency|
                t.dependencies.contains(dependency) || t.waiting_for_tasks.contains(dependency) || t.delivered_dependencies.contains(dependency) || t.stack.iter().any(|s| s.upstream_task == *dependency))),
        }
    }

    /// What a live task is doing, or who it waits on, from its lifecycle alone. A delivery whose PR
    /// merged waits on nobody while its node runs the work it declared for after the merge, and one
    /// under review while its node takes up what it was told after it.
    pub fn live_state(&self, task: &TaskId) -> Option<LiveState> {
        let lifecycle = self.lifecycle_of(task).ok()?;
        if lifecycle.awaits() == Some(&Await::Instruction) && lifecycle.cards().is_empty() {
            return Some(LiveState::Paused);
        }
        let after_merge = self.work_after_merge(task, &lifecycle).is_some();
        if let Some(worker) = lifecycle.working_on_delivery() {
            return Some(LiveState::Working(if after_merge && worker == Worker::Turn { Worker::AfterMerge } else { worker }));
        }
        if self.task_handed_back(task) == Some(Instruction::Reply) && let Some(worker) = lifecycle.working_on_reply() {
            return Some(LiveState::Working(worker));
        }
        let live = lifecycle.live()?;
        let LiveState::WaitingUser(cards) = &live else { return Some(live) };
        if cards.iter().eq(lifecycle.delivery()) && after_merge {
            return Some(LiveState::Working(Worker::AfterMerge));
        }
        Some(live)
    }

    /// The node's question or request to `recipient` (a message too) about `task` that waits for an
    /// answer: still open, or answered after `revision`, so the answer is on its way to the node. A
    /// suggestion never waits.
    fn node_waits_on(&self, task: &TaskId, node: &AgentId, recipient: &Recipient, revision: u64) -> Option<&InteractionId> {
        self.interactions.values().find(|i| {
            matches!(&i.scope, Scope::Task { task_id } if task_id == task)
                && i.author == Author::Agent { agent_id: node.clone() }
                && i.recipient == *recipient
                && i.card.kind != CardKind::Propose
                && !i.withdrawn
                && !self.holds.contains_key(&i.id)
                && match &i.payload {
                    InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. } =>
                        answer.as_ref().is_none_or(|answer| answer.at_revision > revision),
                    _ => false,
                }
        }).map(|i| &i.id)
    }

    /// What the node of `task` waits for, as Alondra recorded it, for a turn that ended at
    /// `revision`: a question or request of its own to the user or the orchestrator (a message
    /// too, never a suggestion) still open, or answered after `revision` so the answer is on its way
    /// to the node; a pause that holds it (`wait_for_machine`); or another task its task depends
    /// on, not done yet or done after `revision`, so the word that it is done is on its way.
    /// Only facts end it, never time.
    pub fn recorded_wait(&self, task: &TaskId, revision: u64) -> Option<RecordedWait> {
        let node = self.tasks.get(task)?.agent_id.clone()?;
        let waits_on = |recipient: &Recipient| self.node_waits_on(task, &node, recipient, revision).is_some();
        let paused = self.node_pause(&node).is_some_and(|pause| !pause.released);
        if waits_on(&Recipient::User) {
            Some(RecordedWait::User)
        } else if paused {
            Some(RecordedWait::Machine)
        } else if waits_on(&Recipient::Orchestrator) {
            Some(RecordedWait::Orchestrator)
        } else if self.tasks.get(task).is_some_and(|t| !self.unmet_dependencies(t).is_empty()
            || t.dependencies.iter().filter_map(|dep| self.tasks.get(dep)).any(|dep| self.landed(dep) && dep.revision > revision)
            || t.waiting_for_tasks.iter().filter_map(|dep| self.tasks.get(dep)).any(|dep| Self::task_wait_finished(dep) && dep.revision > revision)) {
            Some(RecordedWait::Tasks)
        } else {
            None
        }
    }

    /// What the node of `task` waits for now: since the card that asks the user how it goes on,
    /// when one is open, or else on something still unanswered.
    pub fn waiting_on(&self, task: &TaskId) -> Option<RecordedWait> {
        let since = self.interactions.values()
            .filter(|i| matches!(&i.scope, Scope::Task { task_id } if task_id == task) && i.system_card == Some(SystemCard::TurnEnded) && self.open(i))
            .map(|i| i.revision)
            .max()
            .unwrap_or(u64::MAX);
        self.recorded_wait(task, since)
    }

    /// The user answered `card`, one the lifecycle of the task requires: cancelling ends the
    /// task, any other answer tells it how to go on. False for a card the lifecycle does not hold.
    pub(super) fn answer_lifecycle_card(&mut self, actor: &Actor, card: &InteractionId, option: &str, written: Option<&str>, effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<bool> {
        let Some(Scope::Task { task_id }) = self.interactions.get(card).map(|i| i.scope.clone()) else { return Ok(false) };
        if !self.lifecycle_journal.contains_key(&task_id) || !self.lifecycle(lifecycles, &task_id)?.cards().iter().any(|(held, _)| *held == card) {
            return Ok(false);
        }
        if option == CANCEL_TASK {
            self.close_task(actor, &task_id, None, effects, created, lifecycles)?;
            return Ok(true);
        }
        let after_merge = self.work_after_merge(&task_id, self.lifecycle(lifecycles, &task_id)?);
        if option == CLOSE_TASK && after_merge.is_some() {
            self.end_post_merge(&task_id, PostMergeEnd::UserDecision, lifecycles)?;
            return Ok(true);
        }
        if self.lifecycle(lifecycles, &task_id)?.cards().iter().any(|(held, reason)| *held == card && *reason == alondra_domain::WaitReason::Doubt) {
            self.settle_handoff(&task_id, card, option, written, lifecycles)?;
            return Ok(true);
        }
        if self.lifecycle(lifecycles, &task_id)?.cards().iter().any(|(held, reason)| *held == card && *reason == alondra_domain::WaitReason::Dropped) {
            self.settle_dropped(&task_id, card, option, written, effects, lifecycles)?;
            return Ok(true);
        }
        if option == super::NODE_PAUSE_RESUME && self.node_pause_card(card) {
            let record = self.end_of_pause(&task_id, card, lifecycles)?;
            self.decide(lifecycles, &task_id, record)?;
            return Ok(true);
        }
        let kept = self.lifecycle_journal[&task_id].iter().any(|record| matches!(record, LifecycleRecord::QuestionKept { card: kept } if kept == card));
        let mut text = match (option, after_merge.as_deref()) {
            _ if kept => self.kept_answer(card, option),
            (CARRY_ON, Some(reason)) => post_merge_carry_on_instruction(reason),
            (CARRY_ON, None) => CARRY_ON_INSTRUCTION.to_owned(),
            _ => String::new(),
        };
        if let Some(written) = written.map(str::trim).filter(|written| !written.is_empty()) {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(written);
        }
        if text.is_empty() {
            return Err(DomainError::new("invalid_input", "Tell the task how it goes on, or cancel it"));
        }
        let record = LifecycleRecord::Answered { card: card.clone(), input: self.next_input(&task_id), text: Some(text) };
        self.decide(lifecycles, &task_id, record)?;
        Ok(true)
    }

    /// The task's SDK host ended (no process holds its lock): its native Claude, and the turn, work
    /// and questions it had open, ended with it. The questions that ended with the host no longer
    /// wait for an answer. When that leaves nothing to explain the task's next step and a host that
    /// resumes the same Claude session takes its place (`resumes`), Alondra tells that session to
    /// go on with `resume` on its next turn: a host's end is not the user's
    /// decision. Only when it cannot (the launch never confirmed, or the session already ended
    /// again before a turn of it ended) does a card ask the user, saying why; a turn the user
    /// forced to stop ends as the interrupt they asked for. A delivery the user still reviews is
    /// left as it is unless the host cut what the node was doing for them: then it goes on too, or,
    /// when it cannot, the delivery comes back to the user.
    /// The resumed session is told which background work (`ended_work`) ended with the host, so
    /// it never waits for a notification no process will send.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_host_ended(&mut self, task: &TaskId, generation: u64, resumes: bool, by_alondra: bool, ended_work: &[String], effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        let forced = self.lifecycle_journal[task].iter().rev()
            .take_while(|record| !matches!(record, LifecycleRecord::HostEnded { .. } | LifecycleRecord::HostStopped { .. }
                | LifecycleRecord::TurnStarted { .. } | LifecycleRecord::TurnResumed { .. } | LifecycleRecord::TurnUnprompted { .. }))
            .any(|record| matches!(record, LifecycleRecord::ControlRequested { operation: ControlRecord::ForceStop { .. }, .. }));
        let moving = self.moving_to(task).cloned();
        let lifecycle = self.lifecycle(lifecycles, task)?;
        let card = session_ended_card_id(task, generation, lifecycle.cursor().unwrap_or(0) + 1);
        let held: Vec<InteractionId> = lifecycle.cards().into_iter().map(|(card, _)| card.clone()).collect();
        let kept: Vec<InteractionId> = if moving.is_some() { self.questions_to_keep(lifecycle) } else { vec![] };
        let resumes = (resumes || moving.is_some()) && kept.is_empty();
        let available = lifecycle.resource() == Some(ResourceEvidence::Available);
        let looped = resumes && moving.is_none() && keeps_closing(self.lifecycle_journal[task].iter(), by_alondra);
        let after_merge = self.work_after_merge(task, lifecycle);
        let resumed_text = match &moving {
            Some(account) => moved_instruction(account),
            None => after_merge.as_deref().map_or_else(|| HOST_RESUMED_INSTRUCTION.to_owned(), post_merge_resumed_instruction),
        } + &ended_work_note(ended_work);
        for question in &kept {
            self.decide(lifecycles, task, LifecycleRecord::QuestionKept { card: question.clone() })?;
            self.holds.remove(question);
        }
        let lifecycle = self.lifecycle(lifecycles, task)?;
        let ended = |resume: Option<String>| if forced { LifecycleRecord::HostStopped { unheard_card: card.clone() } } else { LifecycleRecord::HostEnded {
            unheard_card: card.clone(),
            text: resume.as_ref().map(|_| resumed_text.clone()),
            resume,
        } };
        let delivered = lifecycle.phase() == TaskPhase::Delivered && after_merge.is_none();
        let cut = matches!(lifecycle.activity(), ActivityView::Working | ActivityView::OwnWork | ActivityView::Continuation);
        let for_the_user = delivered && cut && self.task_handed_back(task).is_some();
        let under_review = delivered && !for_the_user;
        let resume = self.next_input(task);
        let resumed = resumes && !forced && !looped && available && !under_review && self.sends(lifecycles, task, &ended(Some(resume.clone())), &resume);
        self.decide(lifecycles, task, ended(resumed.then_some(resume)))?;
        if !resumed {
            if let Some(agent) = self.tasks[task].agent_id.clone().filter(|_| for_the_user) {
                self.return_replied_delivery(&agent);
            }
        }
        let now: Vec<InteractionId> = self.lifecycle(lifecycles, task)?.cards().into_iter().map(|(card, _)| card.clone()).collect();
        for gone in held.iter().filter(|card| !now.contains(card)) {
            self.retire(gone);
        }
        if now.contains(&card) {
            let title = self.tasks[task].title.clone();
            let ended = if let Some(reason) = &after_merge {
                post_merge_ended_card(card.clone(), task, &title, reason, forced, self.revision + 1)
            } else if forced {
                turn_ended_card(card.clone(), task, &title, TurnEnd::Interrupted, SystemCard::SessionEnded, self.revision + 1)
            } else {
                session_ended_card(card.clone(), task, &title, if looped { SESSION_ENDED_AGAIN_WHY } else { SESSION_ENDED_WHY }, self.revision + 1)
            };
            self.interactions.insert(card.clone(), ended);
            effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
        }
        if !resumed {
            self.go_on_if_stranded(task, effects, lifecycles)?;
        }
        Ok(())
    }

    /// A host that ended with nothing to take its place leaves the node of `task` owing what the
    /// user sent back with nothing that starts a turn: the node is told to go on at once, and if
    /// it stops again the orchestrator gets the case. Nothing changes for a task not left so.
    fn go_on_if_stranded(&mut self, task: &TaskId, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let lifecycle = self.lifecycle(lifecycles, task)?;
        if !self.stranded(task, lifecycle) {
            return Ok(());
        }
        let card = stalled_card_id(task, lifecycle.generation(), &format!("stranded-{}", self.revision + 1));
        let wake = self.wake_for(task, lifecycles)?;
        let asked = self.tasks.get(task).map(|task| asked_changes(self, task)).unwrap_or_default();
        let told = if asked.is_empty() { STRANDED_INSTRUCTION.to_owned() } else { format!("{STRANDED_INSTRUCTION}\n\n{asked}") };
        let text = wake.as_ref().map(|_| told);
        self.decide(lifecycles, task, LifecycleRecord::Unfinished { unheard_card: card.clone(), text, wake })?;
        if self.lifecycle(lifecycles, task)?.cards().iter().any(|(required, _)| **required == card) {
            self.open_stall(task, card, effects);
        }
        Ok(())
    }

    /// Claude accepted the interrupt of `turn`, which is still open: from now on the user can force
    /// it to stop.
    pub(super) fn record_interrupt_accepted(&mut self, task: &TaskId, generation: u64, turn: &str, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        if self.lifecycle(lifecycles, task)?.turn().map(|open| open.as_str()) != Some(turn) {
            return Err(DomainError::new("conflict", format!("Turn {turn} of task {task} is no longer open")));
        }
        self.decide(lifecycles, task, LifecycleRecord::InterruptAccepted { turn: turn.into() })
    }

    /// An uncertain handoff is retried with its original text and a transcript warning. The
    /// owner bounds host recovery; a failed recovery still asks for intervention on its own card.
    pub(super) fn record_doubtful_handoff(&mut self, task: &TaskId, generation: u64, input: &str, text: &str, _resumes: bool, _effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        let moving = self.moving_to(task).is_some();
        let noted = self.lifecycle_journal[task].iter().any(|record| matches!(record, LifecycleRecord::DoubtResent { resent, .. } if resent == input));
        let text = if moving || noted { text.to_owned() } else { format!("{RESENT_NOTE}\n\n{text}") };
        self.decide(lifecycles, task, LifecycleRecord::DoubtResent { input: input.into(), resent: self.next_input(task), text: Some(text) })
    }

    /// The task's SDK execution could not be taken over and has no host: the task holds on a card
    /// that says why and offers to try again or to cancel it. Nothing runs it until the user says.
    /// A card that only asked how the task goes on becomes this one, so the task has one card.
    pub(super) fn record_host_lost(&mut self, task: &TaskId, reason: &str, quiet: bool, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let generation = self.lifecycle(lifecycles, task)?.execution().map(|stamp| stamp.generation())
            .ok_or_else(|| DomainError::new("conflict", format!("Task {task} has no execution to lose")))?;
        self.sdk_execution(task, generation, lifecycles)?;
        let cards: Vec<(InteractionId, WaitReason)> = self.lifecycle(lifecycles, task)?.cards().into_iter().map(|(card, reason)| (card.clone(), reason)).collect();
        if cards.iter().any(|(card, _)| self.interactions.get(card).is_some_and(retakes)) {
            return Ok(());
        }
        if cards.iter().any(|(card, _)| self.claude_hold(card) == Some(ClaudeHold::Usage)) {
            return Ok(());
        }
        let title = self.tasks[task].title.clone();
        let unheard = cards.into_iter().find(|(card, reason)| *reason == WaitReason::Unheard && self.interactions.get(card).is_some_and(|i| i.author == Author::System && self.open(i)));
        if let Some((card, _)) = unheard {
            let order = self.interactions[&card].order;
            let mut lost = host_lost_card(card.clone(), task, &title, reason, self.revision + 1);
            lost.order = order;
            lost.system_card = self.interactions[&card].system_card.clone();
            self.interactions.insert(card.clone(), lost);
            if !quiet {
                effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
            }
            return Ok(());
        }
        let card = InteractionId(format!("{HOST_LOST_PREFIX}{task}-{}", self.revision + 1));
        self.decide(lifecycles, task, LifecycleRecord::Paused { card: card.clone(), owner: Some(PauseOwner::User) })?;
        self.interactions.insert(card.clone(), host_lost_card(card.clone(), task, &title, reason, self.revision + 1));
        if !quiet {
            effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
        }
        Ok(())
    }

    /// Claude will never read `input`, told `text`: it leaves what the task waits on, and a card
    /// asks the user whether to send it again.
    pub(super) fn record_dropped_input(&mut self, task: &TaskId, generation: u64, input: &str, text: &str, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        let card = dropped_card_id(task, generation, input);
        if self.interactions.contains_key(&card) {
            return Err(DomainError::new("conflict", format!("The dropped input {input} of task {task} is already recorded")));
        }
        if !self.lifecycle(lifecycles, task)?.pending_inputs().iter().any(|pending| pending.as_str() == input) {
            return Err(DomainError::new("conflict", format!("The input {input} of task {task} is not pending: it cannot be dropped")));
        }
        self.decide(lifecycles, task, LifecycleRecord::InputDropped { input: input.into(), card: card.clone() })?;
        if self.lifecycle(lifecycles, task)?.cards().iter().any(|(held, _)| **held == card) {
            self.interactions.insert(card.clone(), dropped_card(card.clone(), task, text, self.revision + 1));
            effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
        }
        Ok(())
    }

    /// The user answered the card of a task whose host was lost: «Try again» lets it go on, told to
    /// carry on with what they wrote after it, and the daemon takes its execution over again. False
    /// for any other card, or for «Cancel the task», which cancels it as any lifecycle card does.
    pub(super) fn settle_host_lost(&mut self, card: &InteractionId, option: &str, written: Option<&str>, lifecycles: &mut Lifecycles) -> Result<bool> {
        if option != RETAKE {
            return Ok(false);
        }
        let Some(Scope::Task { task_id }) = self.interactions.get(card).map(|i| i.scope.clone()) else { return Ok(false) };
        let Some(reason) = self.lifecycle(lifecycles, &task_id)?.cards().into_iter().find(|(held, _)| *held == card).map(|(_, reason)| reason) else {
            return Ok(false);
        };
        let mut text = HOST_RESUMED_INSTRUCTION.to_owned();
        if let Some(written) = written.map(str::trim).filter(|written| !written.is_empty()) {
            text.push_str("\n\n");
            text.push_str(written);
        }
        let input = self.next_input(&task_id);
        let record = if reason == WaitReason::Pause {
            LifecycleRecord::Resumed { card: card.clone(), input, text: Some(text) }
        } else {
            LifecycleRecord::Answered { card: card.clone(), input, text: Some(text) }
        };
        self.decide(lifecycles, &task_id, record)?;
        Ok(true)
    }

    /// The user's answer to the card of an input Claude never read: send the same text again (with
    /// what they wrote after it), send what they wrote instead, or let it go.
    fn settle_dropped(&mut self, task: &TaskId, card: &InteractionId, option: &str, written: Option<&str>, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let written = written.map(str::trim).filter(|written| !written.is_empty());
        let text = match (option, written) {
            (SEND_AGAIN, written) => {
                let mut text = self.interactions.get(card).and_then(|i| i.card.content.as_ref()).map(|content| content.body.clone())
                    .ok_or_else(|| DomainError::new("not_found", "The card holds no text to send again"))?;
                if let Some(written) = written {
                    text.push_str("\n\n");
                    text.push_str(written);
                }
                Some(text)
            }
            (_, Some(written)) => Some(written.to_owned()),
            (DISCARD, None) => None,
            _ => return Err(DomainError::new("invalid_input", "Send the message again, discard it, or write what to send instead")),
        };
        if let Some(text) = text {
            let record = LifecycleRecord::Answered { card: card.clone(), input: self.next_input(task), text: Some(text) };
            return self.decide(lifecycles, task, record);
        }
        let unheard_card = InteractionId(format!("{STALLED_PREFIX}{}", card.0.trim_start_matches(DROPPED_PREFIX)));
        self.decide(lifecycles, task, LifecycleRecord::DroppedDiscarded { card: card.clone(), unheard_card: unheard_card.clone(), stalled: true })?;
        if self.lifecycle(lifecycles, task)?.cards().iter().any(|(held, _)| **held == unheard_card) {
            self.open_stall(task, unheard_card, effects);
        }
        Ok(())
    }

    /// `input`, told `text`, provably never reached the ended host: the same text goes to the next
    /// host in its place, without asking anyone.
    pub(super) fn resend_untaken(&mut self, task: &TaskId, generation: u64, input: &str, text: &str, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        let record = LifecycleRecord::HandoffResent { input: input.into(), resent: self.next_input(task), text: Some(text.into()) };
        self.decide(lifecycles, task, record)
    }

    /// The user's answer to the card of a doubtful input: send the same text again in its place
    /// (with what they wrote after it), take it as received, or send what they wrote instead. The
    /// text is the one the card shows.
    fn settle_handoff(&mut self, task: &TaskId, card: &InteractionId, option: &str, written: Option<&str>, lifecycles: &mut Lifecycles) -> Result<()> {
        let written = written.map(str::trim).filter(|written| !written.is_empty());
        let text = match (option, written) {
            (RESEND, written) => {
                let mut text = self.interactions.get(card).and_then(|i| i.card.content.as_ref()).map(|content| content.body.clone())
                    .ok_or_else(|| DomainError::new("not_found", "The card holds no text to send again"))?;
                if let Some(written) = written {
                    text.push_str("\n\n");
                    text.push_str(written);
                }
                Some(text)
            }
            (_, Some(written)) => Some(written.to_owned()),
            (TAKE_AS_RECEIVED, None) => None,
            _ => return Err(DomainError::new("invalid_input", "Send the message again, take it as received, or write what to send instead")),
        };
        let resent = text.map(|text| (self.next_input(task), text));
        let unheard_card = InteractionId(format!("{SESSION_ENDED_PREFIX}{}", card.0.trim_start_matches(HANDOFF_DOUBT_PREFIX)));
        let (resent, text) = resent.map_or((None, None), |(input, text)| (Some(input), Some(text)));
        let record = LifecycleRecord::HandoffSettled { card: card.clone(), resent, text, unheard_card: unheard_card.clone() };
        self.decide(lifecycles, task, record)?;
        if self.lifecycle(lifecycles, task)?.cards().iter().any(|(held, _)| **held == unheard_card) {
            let title = self.tasks[task].title.clone();
            self.interactions.insert(unheard_card.clone(), session_ended_card(unheard_card, task, &title, SESSION_ENDED_WHY, self.revision + 1));
        }
        Ok(())
    }

    /// The SDK host takes the task with an execution of its own, after releasing one it still had.
    pub(super) fn admit_execution(&mut self, task: &TaskId, node: &AgentId, identity: ExecutionIdentity, lifecycles: &mut Lifecycles) -> Result<()> {
        if identity.host == CLAUDE_BG_HOST {
            return Err(DomainError::new("invalid_input", "An execution of the claude --bg backend is admitted by assigning its node"));
        }
        if self.lifecycle(lifecycles, task)?.execution().is_some() {
            let record = LifecycleRecord::ExecutionReleased { input: self.next_input(task) };
            self.decide(lifecycles, task, record)?;
        }
        let held: Vec<InteractionId> = self.lifecycle(lifecycles, task)?.cards().into_iter().map(|(card, _)| card.clone()).collect();
        self.decide(lifecycles, task, LifecycleRecord::Admitted { node: node.clone(), execution: Some(identity) })?;
        for card in &held {
            self.retire(card);
        }
        Ok(())
    }

    /// What the SDK observed of the task's execution of `generation`, applied in order. Facts about
    /// another execution, or about one of the `claude --bg` backend, are refused.
    pub(super) fn record_runtime_facts(&mut self, task: &TaskId, generation: u64, facts: &[RuntimeObservation], effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        if facts.is_empty() {
            return Err(DomainError::new("invalid_input", "An observation without facts records nothing"));
        }
        for fact in facts {
            let record = match fact.clone() {
                RuntimeObservation::ModelAnswered => {
                    let account = self.task_account_key(task);
                    self.claude_answered(task, &account, lifecycles)?;
                    continue;
                }
                RuntimeObservation::ResourceAvailable => LifecycleRecord::ResourceAvailable,
                RuntimeObservation::TurnStarted { turn, input } => LifecycleRecord::TurnStarted { turn, input },
                RuntimeObservation::TurnEnded { turn, failure, failed } => {
                    self.record_observed_turn_end(task, generation, &turn, failure.as_ref(), failed.as_deref(), effects, lifecycles)?;
                    continue;
                }
                RuntimeObservation::TurnResumed { turn, continuation } => {
                    if is_decision(generation, &continuation) {
                        return Err(DomainError::new("invalid_input", "A decision's turn goes on only when its decision reaches Claude"));
                    }
                    LifecycleRecord::TurnResumed { turn, continuation }
                }
                RuntimeObservation::TurnUnprompted { turn } => {
                    let held: Vec<InteractionId> = self.lifecycle(lifecycles, task)?.cards().into_iter().map(|(card, _)| card.clone()).collect();
                    self.decide(lifecycles, task, LifecycleRecord::TurnUnprompted { turn })?;
                    let kept = self.lifecycle(lifecycles, task)?;
                    for card in held.iter().filter(|card| !kept.cards().iter().any(|(still, _)| still == card)) {
                        self.retire(card);
                    }
                    continue;
                }
                RuntimeObservation::InputConsumed { input } => LifecycleRecord::InputConsumed { input },
                RuntimeObservation::TakenOver => LifecycleRecord::TakenOver,
                RuntimeObservation::TakeoverUnanswered => LifecycleRecord::TakeoverUnanswered,
                RuntimeObservation::WorkStarted { turn, work } => LifecycleRecord::WorkStarted { turn, work },
                RuntimeObservation::WorkEnded { work, continuation } => LifecycleRecord::WorkEnded { work, continuation },
                RuntimeObservation::WorkContinued { continuation } => {
                    if is_decision(generation, &continuation) {
                        return Err(DomainError::new("invalid_input", "A decision's turn goes on only when its decision reaches Claude"));
                    }
                    LifecycleRecord::WorkContinued { continuation }
                }
            };
            self.decide(lifecycles, task, record)?;
        }
        Ok(())
    }

    /// The open task whose current execution `agent` runs through the SDK, with its lifecycle.
    pub(super) fn sdk_task_of(&self, agent: &AgentId) -> Result<(TaskId, Arc<TaskLifecycle>)> {
        for (id, t) in &self.tasks {
            if t.agent_id.as_ref() != Some(agent) || t.over() || !self.lifecycle_journal.contains_key(id) {
                continue;
            }
            let lifecycle = self.lifecycle_of(id)?;
            if lifecycle.execution().is_some_and(|stamp| stamp.node() == agent && stamp.host().as_str() != CLAUDE_BG_HOST) {
                return Ok((id.clone(), lifecycle));
            }
        }
        Err(DomainError::new("no_task", format!("Node {agent} has no task running through the Agent SDK")))
    }

    /// The lifecycle of an open task whose current execution runs through the SDK.
    pub(super) fn sdk_lifecycle(&self, task: &TaskId) -> Result<Arc<TaskLifecycle>> {
        let t = self.tasks.get(task).ok_or_else(|| DomainError::new("not_found", "Task missing"))?;
        if t.over() {
            return Err(DomainError::new("task_not_active", format!("Task {task} is ending")));
        }
        let lifecycle = self.lifecycle_of(task)?;
        if !lifecycle.execution().is_some_and(|stamp| stamp.host().as_str() != CLAUDE_BG_HOST) {
            return Err(DomainError::new("no_task", format!("Task {task} has no execution running through the Agent SDK")));
        }
        Ok(lifecycle)
    }

    /// `record` if the task's lifecycle accepts it, judged on a copy that is then dropped.
    pub(super) fn judged(&self, task: &TaskId, lifecycle: Arc<TaskLifecycle>, record: LifecycleRecord) -> Result<LifecycleRecord> {
        let event = event(task, &record, &lifecycle)?;
        lifecycle.advanced(lifecycle.revision(), event).map_err(|refusal| refused(task, &record, &lifecycle, refusal))?;
        Ok(record)
    }

    /// The task's node is gone and left nothing behind: the task waits for another.
    pub(super) fn release(&mut self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        let record = LifecycleRecord::ExecutionReleased { input: self.next_input(task) };
        self.decide(lifecycles, task, record)
    }

    /// The execution a deferred move ends leaves the task: a delivery the user reviews rests with
    /// the user until it is sent back, and anything else waits for its next execution.
    pub(super) fn leave_for_the_move(&mut self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        if self.lifecycle(lifecycles, task)?.in_review() {
            return self.decide(lifecycles, task, LifecycleRecord::ExecutionLeftForReview);
        }
        self.release(task, lifecycles)
    }

    /// A receipt of an ending task discharges what its ending still owes among `which`. A task
    /// that is not ending owes nothing yet, and its cleanup is owed while one of its worktrees is
    /// still there.
    pub(super) fn drain(&mut self, task: &TaskId, lifecycles: &mut Lifecycles, which: impl Fn(&Drained) -> bool) -> Result<()> {
        if !self.tasks[task].ending() {
            return Ok(());
        }
        let worktrees_left = self.tasks[task].open_worktrees().next().is_some();
        let owed: Vec<Drained> = owed(self.lifecycle(lifecycles, task)?).into_iter()
            .filter(|owed| which(owed) && !(worktrees_left && matches!(owed, Drained::Cleanup { .. })))
            .collect();
        for obligation in owed {
            self.decide(lifecycles, task, LifecycleRecord::Drained { obligation })?;
        }
        Ok(())
    }

    /// The node declared work it runs after its PR merges: the lifecycle owes it from now until a
    /// fact ends it. Declaring again only updates what Alondra shows.
    pub(super) fn declare_post_merge(&mut self, task: &TaskId, work: PostMergeWork, lifecycles: &mut Lifecycles) -> Result<()> {
        if self.lifecycle(lifecycles, task)?.post_merge().is_none() {
            if self.tasks[task].ending() {
                return Err(DomainError::new("task_not_active", format!("Task {} is ending: no work after the merge starts now", task.0)));
            }
            let record = LifecycleRecord::PostMergeDeclared { operation: self.next_operation("post-merge", task) };
            self.decide(lifecycles, task, record)?;
        }
        self.tasks.get_mut(task).unwrap().wrap_up.post_merge_work = Some(work);
        Ok(())
    }

    /// The work after the merge ends, with the fact that ended it. A task that owes none is left
    /// as it is.
    pub(super) fn end_post_merge(&mut self, task: &TaskId, fact: PostMergeEnd, lifecycles: &mut Lifecycles) -> Result<()> {
        self.tasks.get_mut(task).unwrap().wrap_up.post_merge_work = None;
        let Some(operation) = self.lifecycle(lifecycles, task)?.post_merge().map(|operation| operation.as_str().to_owned()) else {
            return Ok(());
        };
        self.decide(lifecycles, task, LifecycleRecord::PostMergeEnded { operation, fact })
    }

    /// The user ended the task's work after the merge on one of its cards: its node closes now,
    /// without waiting for a cleanup it may never confirm.
    pub fn post_merge_closed_by_user(&self, task: &TaskId) -> bool {
        self.lifecycle_journal.get(task).is_some_and(|journal| journal.iter().rev()
            .find_map(|record| match record {
                LifecycleRecord::PostMergeEnded { fact, .. } => Some(*fact == PostMergeEnd::UserDecision),
                _ => None,
            })
            .unwrap_or(false))
    }

    /// Work after the merge a node declared under a daemon from before the lifecycle owed it: the
    /// lifecycle takes it over, so from then on only a fact ends it.
    pub(super) fn adopt_declared_post_merge(&mut self, lifecycles: &mut Lifecycles) -> Result<()> {
        let declared: Vec<TaskId> = self.tasks.values()
            .filter(|t| t.wrap_up.post_merge_work.is_some() && !t.over())
            .map(|t| t.id.clone())
            .collect();
        for task in declared {
            if self.lifecycle(lifecycles, &task)?.post_merge().is_none() {
                let record = LifecycleRecord::PostMergeDeclared { operation: self.next_operation("post-merge", &task) };
                self.decide(lifecycles, &task, record)?;
            }
        }
        Ok(())
    }

    /// Whether the work after the merge the task's node declared still holds it. A declaration
    /// from before the lifecycle owed it holds it too, until the next commit adopts it.
    pub fn post_merge_owed(&self, task: &Task) -> bool {
        let owed = self.lifecycle_of(&task.id).is_ok_and(|lifecycle| lifecycle.post_merge().is_some());
        owed || (task.wrap_up.post_merge_work.is_some() && !task.over())
    }

    /// The node ran with what it was given: its `claude --bg` process is up and its turn took
    /// every input the task had queued for it. An SDK execution reports its own facts instead. Known when the launcher reports the start, when a
    /// turn of the node begins and when the node delivers or has a delivery taken back.
    pub(super) fn node_has_run(&mut self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        let lifecycle = self.lifecycle(lifecycles, task)?;
        if lifecycle.execution().is_none_or(|stamp| stamp.host().as_str() != CLAUDE_BG_HOST)
            || matches!(lifecycle.phase(), TaskPhase::Finishing | TaskPhase::Completed | TaskPhase::Cancelled)
        {
            return Ok(());
        }
        if lifecycle.resource() == Some(ResourceEvidence::Reserved) {
            self.decide(lifecycles, task, LifecycleRecord::ResourceAvailable)?;
        }
        loop {
            let lifecycle = self.lifecycle(lifecycles, task)?;
            let Some(input) = lifecycle.pending_inputs().first().map(|input| input.as_str().to_owned()) else { break };
            let record = match lifecycle.activity() {
                ActivityView::Starting => LifecycleRecord::TurnStarted { turn: format!("turn:{task}:{}", lifecycle.cursor().unwrap_or(0) + 1), input },
                ActivityView::Working => LifecycleRecord::InputConsumed { input },
                _ => break,
            };
            self.decide(lifecycles, task, record)?;
        }
        Ok(())
    }

    /// The standing delivery is settled, nothing of the task waits for the user and its node is
    /// gone or it never had one: the task is done. The acceptance, the end and its receipts are
    /// one decision, because until the node's end is recorded the user can still take the
    /// delivery back with feedback, and the aggregate records no acceptance that can be undone.
    pub(super) fn finish_task(&mut self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        let lifecycle = self.lifecycle(lifecycles, task)?;
        if lifecycle.phase() != TaskPhase::Delivered {
            return Ok(());
        }
        let delivery = lifecycle.delivery().cloned().expect("a delivered task has its delivery");
        self.end_node_pause(task, lifecycles)?;
        if !self.lifecycle(lifecycles, task)?.accepted() {
            self.decide(lifecycles, task, LifecycleRecord::Accepted { delivery })?;
        }
        let record = LifecycleRecord::Finished {
            intent: TaskEnding::Completing,
            cleanup: self.next_operation("cleanup", task),
            post_merge: None,
        };
        self.decide(lifecycles, task, record)?;
        self.drain(task, lifecycles, |_| true)
    }

    /// Every task whose final delivery settled with nothing left for the user, and whose
    /// node is over, is done now. One whose node still winds up stays delivered and closing.
    pub(super) fn finish_settled_deliveries(&mut self, lifecycles: &mut Lifecycles) -> Result<()> {
        let settled: Vec<TaskId> = self.tasks.values()
            .filter(|t| !t.over() && !t.archived)
            .filter(|t| self.finished(t) && !self.unanswered(&t.id) && !self.blockers(&t.id) && !self.wraps_up(t))
            .map(|t| t.id.clone())
            .collect();
        for task in settled {
            self.finish_task(&task, lifecycles)?;
        }
        Ok(())
    }

    /// A task whose PR merged: its node still works on what comes after, or waits for what that
    /// work needs from other Tasks, until its session is over.
    fn after_merge_state(&self, t: &Task) -> TaskState {
        if t.wrap_up.post_merge_work.is_none() || self.node_session_closed(t) {
            TaskState::Closing
        } else if self.waiting_after_merge(t).is_some() {
            TaskState::WaitingDependency
        } else {
            TaskState::Running
        }
    }

    /// Its PR merged and its node still works on what it declared comes after.
    pub fn works_after_merge(&self, t: &Task) -> bool {
        t.wrap_up.post_merge_work.is_some() && (t.ending == Some(TaskEnding::Completing) || self.finished(t))
    }

    /// The first Task a task's work after the merge still waits for: one that has not landed and
    /// was not cancelled, since a cancelled one never will and its node decides how to go on.
    pub fn waiting_after_merge(&self, t: &Task) -> Option<TaskId> {
        t.dependencies.iter().find(|dep| self.tasks.get(*dep).is_some_and(|d| !self.landed(d) && !d.cancelled())).cloned()
    }

    /// The state of a task within the phase its lifecycle materialized: what it waits for
    /// or who has to act. Never a phase change: no Done, Cancelled or Closing comes from here.
    pub(super) fn state_within_phase(&self, t: &Task, cards: &TaskCards<'_>) -> TaskState {
        match t.ending {
            Some(TaskEnding::Cancelling) => return TaskState::Closing,
            Some(TaskEnding::Completing) => {
                return self.after_merge_state(t);
            }
            None => {}
        }
        let goes_on = t.final_presentation.as_ref().is_none_or(|p| {
            self.interactions.get(p).is_some_and(|p| self.handed_back(p).is_some() && !self.accepted(p) && !self.settled(t, p))
        });
        if self.stack_ready(t).is_err() || (goes_on && self.waits_are_over(t).is_err()) {
            TaskState::WaitingDependency
        } else if cards.blockers(&t.id) {
            TaskState::NeedsYou
        } else if let Some(p) = t.final_presentation.as_ref().and_then(|p| self.interactions.get(p)) {
            if (self.settled(t, p) || self.accepted(p)) && cards.unanswered(&t.id) {
                TaskState::NeedsYou
            } else if self.settled(t, p) {
                self.after_merge_state(t)
            } else if self.accepted(p) {
                TaskState::WaitingDependency
            } else if self.handed_back(p).is_some() || !t.blocks.is_empty() {
                if t.started || t.agent_id.is_some() { TaskState::Running } else { TaskState::NotStarted }
            } else if !self.open_blockers(&t.id).is_empty() {
                TaskState::AwaitingBlocker
            } else if matches!(self.attention_given(t, cards), Attention::Wait(_)) {
                TaskState::WaitingDependency
            } else {
                TaskState::ForYourReview
            }
        } else if t.started {
            TaskState::Running
        } else {
            TaskState::NotStarted
        }
    }
}

/// What a task's columns show, for a journal that cannot be written for them.
pub fn task_shape(task: &Task) -> String {
    format!(
        "state {:?}, {}, {}, {}",
        task.state,
        if task.started { "started" } else { "not started" },
        if task.agent_id.is_some() { "with a node" } else { "without a node" },
        if task.final_presentation.is_some() { "with a final delivery" } else { "without a final delivery" },
    )
}

/// The shortest journal that leads to what a task's columns show, for a task kept before every
/// task had one. Its inputs and executions are Alondra's own, of the `claude --bg` backend:
/// nothing of the SDK is made up. A shape no journal leads to is refused.
pub fn journal_for(task: &Task) -> Result<Vec<LifecycleRecord>> {
    let refused = |detail: &str| DomainError::new("lifecycle_journal", format!(
        "task {} has a shape no lifecycle journal leads to ({}){detail}", task.id, task_shape(task)));
    let input = format!("input:{}:migrated", task.id);
    let mut records = vec![LifecycleRecord::Planned { input: input.clone() }];
    if let Some(node) = &task.agent_id {
        records.push(LifecycleRecord::Admitted { node: node.clone(), execution: None });
        if task.started {
            records.push(LifecycleRecord::ResourceAvailable);
            records.push(LifecycleRecord::TurnStarted { turn: format!("turn:{}:migrated", task.id), input });
        }
    } else if task.started && !task.state.terminal() {
        return Err(refused(""));
    }
    let delivery = task.final_presentation.clone();
    let ending = match task.state {
        TaskState::Done => {
            let delivery = delivery.ok_or_else(|| refused(""))?;
            records.push(LifecycleRecord::Presented { delivery: delivery.clone() });
            records.push(LifecycleRecord::Accepted { delivery });
            Some(TaskEnding::Completing)
        }
        TaskState::Cancelled => Some(TaskEnding::Cancelling),
        TaskState::ForYourReview | TaskState::AwaitingBlocker | TaskState::Closing => {
            records.push(LifecycleRecord::Presented { delivery: delivery.ok_or_else(|| refused(""))? });
            None
        }
        TaskState::NotStarted | TaskState::Running | TaskState::NeedsYou | TaskState::WaitingDependency => {
            if let Some(delivery) = delivery {
                records.push(LifecycleRecord::Presented { delivery });
            }
            None
        }
    };
    if let Some(intent) = ending {
        records.push(LifecycleRecord::Finished { intent, cleanup: format!("cleanup:{}:migrated", task.id), post_merge: None });
        let lifecycle = replay(&task.id, &records).map_err(|error| refused(&format!(": {}", error.message)))?;
        records.extend(owed(&lifecycle).into_iter().map(|obligation| LifecycleRecord::Drained { obligation }));
    }
    replay(&task.id, &records).map_err(|error| refused(&format!(": {}", error.message)))?;
    Ok(records)
}

#[cfg(test)]
impl DomainState {
    /// A state built by hand, as if it had been migrated: every task without a journal gets the one
    /// its columns lead to. False when a task has a shape no journal leads to.
    pub(crate) fn journal_every_task(&mut self) -> bool {
        let unjournaled: Vec<TaskId> = self.tasks.keys().filter(|id| !self.lifecycle_journal.contains_key(*id)).cloned().collect();
        for id in unjournaled {
            let Ok(records) = journal_for(&self.tasks[&id]) else { return false };
            self.lifecycle_journal.insert(id, records);
        }
        self.read_endings().is_ok() && self.validate_journal().is_ok()
    }
}

fn current_sdk(task: &TaskId, generation: u64, lifecycle: &TaskLifecycle) -> Result<()> {
    let Some(stamp) = lifecycle.execution().filter(|stamp| stamp.generation() == generation) else {
        return Err(DomainError::new("stale_execution", format!("Task {task} has no execution of generation {generation}")));
    };
    if stamp.host().as_str() == CLAUDE_BG_HOST {
        return Err(DomainError::new("invalid_input", "The claude --bg backend reports its facts through its node"));
    }
    Ok(())
}

/// The card of the decision the SDK execution of `generation` asked for at `epoch`; Claude's
/// questions asked together are cards of a group under this id.
pub fn decision_card_id(task: &TaskId, generation: u64, epoch: u64) -> InteractionId {
    InteractionId(format!("decision-{task}-{generation}-{epoch}"))
}
/// The cards that ask the user how a task goes on when a decision it waited for was cancelled
/// and nothing else explains its next step.
pub const DECISION_CANCELLED_PREFIX: &str = "decision-cancelled-";
pub fn decision_cancelled_card_id(task: &TaskId, generation: u64, epoch: u64) -> InteractionId {
    InteractionId(format!("{DECISION_CANCELLED_PREFIX}{task}-{generation}-{epoch}"))
}
const DECISION_CANCELLED_WHY: &str = "Claude Code stopped waiting for the decision it asked you for. Tell it to go on, write to it what to do, or cancel the task.";

pub(super) fn decision_cancelled_card(id: InteractionId, task: &TaskId, title: &str, revision: u64) -> Interaction {
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| format!("“{title}” stopped waiting for your decision: tell it how to go on"));
    asks_how_it_goes_on(id, task, need, DECISION_CANCELLED_WHY, SystemCard::DecisionCancelled, revision)
}

/// The cards that ask the user how a task goes on when its SDK turn ended with nothing that
/// explains its next step.
pub const TURN_ENDED_PREFIX: &str = "turn-ended-";

/// Who a node at rest waits for, by what Alondra recorded of its task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordedWait {
    User,
    Orchestrator,
    Machine,
    Tasks,
}

/// The card a turn end asks with. Clients name cards in view keys and action ids, whose grammar
/// takes letters, digits, `-`, `_` and `.`: an SDK turn such as `claude-message:<uuid>` keeps its
/// other characters, with `-` in place of the rest.
pub fn turn_ended_card_id(task: &TaskId, generation: u64, turn: &str) -> InteractionId {
    let turn: String = turn.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).collect();
    InteractionId(format!("{TURN_ENDED_PREFIX}{task}-{generation}-{turn}"))
}
/// The option of such a card that ends the task.
pub const CANCEL_TASK: &str = "cancel_task";
pub const CANCEL_TASK_LABEL: &str = "Cancel the task";
const CANCEL_TASK_MEANS: &str = "The task ends and its execution is closed";
const CARRY_ON_INSTRUCTION: &str = "Go on with your task: deliver your result, or ask the user through a card when you need them.";
/// Why a card asked the user how a task goes on after two empty turns, before stops went to the
/// orchestrator: clients still word the cards recorded then.
pub const TURN_ENDED_TWICE_WHY: &str = "A turn of it ended without a result, a question or work of its own, Alondra told it so, and its next turn ended the same way. Tell it to go on, write to it what to do, or cancel the task.";
/// Why a turn ended empty: what the node is told when Alondra wakes it, and what the orchestrator
/// reads when it settles the next stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmptyTurn {
    /// Nothing explains the task's next step.
    Unexplained,
    /// The node registered pull request `number`, still open, and never presented it.
    UnpresentedPr { number: u64 },
    /// The node presented its delivery, asked the user on `card` what `need` says, was told the
    /// answer, `said`, and rested on it.
    UnactedAnswer { card: InteractionId, need: String, said: String },
    /// The node's PR merged and it rests with the work it said runs after the merge, `reason`, owed.
    AfterMerge { reason: String },
}
impl EmptyTurn {
    /// What the node is told the first time Alondra hands it back its turn after one that ended empty.
    pub fn wake_text(&self) -> String {
        match self {
            Self::Unexplained => WAKE_INSTRUCTION.to_owned(),
            Self::UnpresentedPr { number } => format!("Your turn ended and you registered pull request #{number} without presenting it: present it with `present_result` (is_final true, the pull request in targets) and end your turn. {WAKE_LIMIT}"),
            Self::UnactedAnswer { card, need, said } => format!("You asked “{need}” (card {card}) and the user answered: “{said}”. That answer is final and already yours: act on it now. If your delivery changes, present it again with `present_result`; if it stands as it is, present it again as it is. {WAKE_LIMIT}"),
            Self::AfterMerge { reason } => format!("{} {WAKE_LIMIT}", post_merge_carry_on_instruction(reason)),
        }
    }
    /// What the node is told on the `wake`th wake in a row, counted from 1.
    pub fn wake_text_at(&self, wake: u8) -> String {
        match wake {
            0 | 1 => self.wake_text(),
            wake => format!("{} This is reminder {wake} of {EMPTY_TURN_WAKES}.", self.wake_text()),
        }
    }
    /// What the orchestrator reads of why the node stopped.
    pub fn stall_text(&self) -> String {
        match self {
            Self::Unexplained => "Nothing Alondra knows of explains it.".to_owned(),
            Self::UnpresentedPr { number } => format!("It registered pull request #{number}, still open, and never presented it."),
            Self::UnactedAnswer { card, need, said } => format!("Its delivery waits for the user's review. After presenting it, it asked the user “{need}” (card {card}), the user answered “{said}”. It took that answer and rested without acting on it."),
            Self::AfterMerge { reason } => format!("Its PR is merged and it rests with the work it said runs after the merge (“{reason}”) still owed."),
        }
    }
}
/// What a node is told when its host ended and left it owing what the user sent back.
pub const STRANDED_INSTRUCTION: &str = "Nothing is running for your task, and the user still waits for what they sent back on your delivery. Check where you were and go on with it, then present your result again. Whatever you ran in the background ended and will not report back: run again what you still need.";
pub const WAKE_INSTRUCTION: &str = "Your turn ended without a result, a question or work of your own running, and your task is still open. Deliver your result with `present_result`, ask the user with `ask`, or, if you wait for something, wait on it through its Alondra tool (`wait_for_machine`, `wait_for_tasks`, or `post_merge_work` with `waits_for`) and end your turn. If your turns keep ending this way, Alondra reminds you up to 3 times in a row; after that the orchestrator decides how your task goes on, and it may close it.";
/// What a wake says follows when the node's turns keep ending empty.
const WAKE_LIMIT: &str = "If your turns keep ending this way, Alondra reminds you up to 3 times in a row; after that the orchestrator decides how your task goes on, and it may close it.";

pub const REFUSED_WHY: &str = "Claude Code cannot use the Claude API until this is fixed outside Alondra. Fix it, then tell the task to go on, or cancel it.";
pub const INTERRUPTED_WHY: &str = "You stopped its turn. Tell it to carry on, write what to do, or cancel the task.";
pub const FAILED_WHY: &str = "Claude Code ended its turn as failed. Tell it how to go on, write to it what to do, or cancel the task.";
pub const NATIVE_FAILED_WHY: &str = "Codex ended its turn with an error. Tell it how to continue, write what to do next, or cancel the task.";
pub const NATIVE_REQUEST_REJECTED_WHY: &str = "Codex rejected this task's request. Check the selected model, reasoning effort and input before explicitly continuing on this account or cancelling. A listed model is not proof that this request will be accepted.";
pub const NATIVE_CONTEXT_EXCEEDED_WHY: &str = "Codex reports that this request exceeds the model's context window. Compact this session or reduce the next input before explicitly continuing. Model changes use this node's supported controls on the same account.";
/// The longest error code a card names.
pub const FAILED_CODE_MAX: usize = 40;

/// Why a root turn ended with a card for the user: something only they decide.
enum TurnEnd<'a> {
    Interrupted,
    Refused(&'a SdkApiFailure),
    /// Claude Code ended it as failed with this code.
    Failed(&'a str),
    NativeFailed(&'a str),
    NativeRequestRejected,
    NativeContextExceeded,
}

/// Whether the user asked to interrupt `turn` after it started: the journal holds the request.
fn interrupted(journal: &Journal, turn: &str) -> bool {
    journal.iter().rev()
        .take_while(|record| !matches!(record, LifecycleRecord::TurnStarted { turn: started, .. } | LifecycleRecord::TurnResumed { turn: started, .. } | LifecycleRecord::TurnUnprompted { turn: started } if started == turn))
        .any(|record| matches!(record, LifecycleRecord::ControlRequested { operation: ControlRecord::Interrupt { turn: Some(stopped) }, .. } if stopped == turn))
}

fn turn_ended_card(id: InteractionId, task: &TaskId, title: &str, end: TurnEnd, system: SystemCard, revision: u64) -> Interaction {
    let (need, why) = match end {
        TurnEnd::Refused(failure) => {
            let refusal = api_refusal(&failure.error);
            (fit_title(title, CardSpec::TITLED_NEED_MAX, |title| format!("“{title}” stopped: {refusal}")), REFUSED_WHY)
        }
        TurnEnd::Interrupted => (fit_title(title, CardSpec::TITLED_NEED_MAX, |title| format!("You interrupted “{title}”: tell it how to go on")), INTERRUPTED_WHY),
        TurnEnd::NativeFailed(code) => (format!("Codex's turn ended with an error (`{}`): tell it how to go on", clip(code, FAILED_CODE_MAX)), NATIVE_FAILED_WHY),
        TurnEnd::NativeRequestRejected => ("Codex rejected this task's request: check it before continuing".into(), NATIVE_REQUEST_REJECTED_WHY),
        TurnEnd::NativeContextExceeded => ("This request exceeds the Codex model's context window".into(), NATIVE_CONTEXT_EXCEEDED_WHY),
        TurnEnd::Failed(code) => (format!("Claude's turn ended with an error (`{}`): tell it how to go on", clip(code, FAILED_CODE_MAX)), FAILED_WHY),
    };
    asks_how_it_goes_on(id, task, need, why, system, revision)
}

fn asks_how_it_goes_on(id: InteractionId, task: &TaskId, need: String, why: &str, system: SystemCard, revision: u64) -> Interaction {
    let need = clip(&need, CardSpec::NEED_MAX);
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: need.clone(),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question {
            options: vec![
                QuestionOption { id: CARRY_ON.into(), label: CARRY_ON_LABEL.into(), description: Some(CARRY_ON_MEANS.into()) },
                QuestionOption { id: CANCEL_TASK.into(), label: CANCEL_TASK_LABEL.into(), description: Some(CANCEL_TASK_MEANS.into()) },
            ],
            answer: None,
        },
        revision,
        order: revision,
        withdrawn: false,
        card: CardSpec::new(CardKind::Decide, need).why(why),
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(system), shortened: false,
    }
}

/// The cards that ask the user how a task goes on after its SDK host ended mid-way.
pub const SESSION_ENDED_PREFIX: &str = "session-ended-";
/// The cards that ask the user about an input handed to a host that ended before reading it.
pub const HANDOFF_DOUBT_PREFIX: &str = "handoff-doubt-";
/// What the first node on the account a task moved to is told, after the task's record.
fn moved_instruction(account: &crate::accounts::AccountKey) -> String {
    format!("The user moved this task to {}: you are its new node and carry on with it from its record above. Its worktree and branch are the same as before, with whatever was left uncommitted: check them and go on where the work stopped. Answers to questions still open reach you as messages: do not ask them again.", account.wire_id())
}

impl DomainState {
    /// What the node a task moved to is told when the user answers `card`, a question its
    /// previous node asked: the question and the option chosen.
    fn kept_answer(&self, card: &InteractionId, option: &str) -> String {
        let Some(asked) = self.interactions.get(card) else { return String::new() };
        let InteractionPayload::Question { options, .. } = &asked.payload else { return String::new() };
        let chosen = options.iter().find(|choice| choice.id == option).map_or(option, |choice| choice.label.as_str());
        format!("The user answered the question the previous node of this task asked, «{}»: {chosen}", asked.card.effective_title())
    }

    /// The questions the ending execution waits on that the task keeps when it moves: single
    /// questions the user can still answer. A permission belongs to the session that asked it.
    fn questions_to_keep(&self, lifecycle: &alondra_domain::TaskLifecycle) -> Vec<InteractionId> {
        lifecycle.cards().into_iter()
            .filter(|(card, reason)| *reason == alondra_domain::WaitReason::Question && lifecycle.card_callback(card).is_some())
            .filter(|(card, _)| self.interactions.get(*card).is_some_and(|asked| asked.card.group.is_none() && !asked.withdrawn && !Self::answered_question(asked)))
            .map(|(card, _)| card.clone())
            .collect()
    }
}

/// The cards of a task whose SDK execution could not be taken over: no host runs it.
pub const HOST_LOST_PREFIX: &str = "host-lost-";
pub const RETAKE: &str = "retake";
pub const RETAKE_LABEL: &str = "Try again";
const RETAKE_MEANS: &str = "Alondra starts its session again where it left off";
const HOST_RESUMED_INSTRUCTION: &str = "Your session was cut off when its host stopped, and Alondra resumed it. Check where you were and go on with your task: deliver your result, or ask the user through a card when you need them.";
const RESENT_NOTE: &str = "Your session was cut off when its host stopped, and Alondra resumed it. The message below was sent to you just before, and your conversation may already have it: if it does, do not act on it twice.";
pub const RESEND: &str = "resend";
pub const RESEND_LABEL: &str = "Send it again";
#[cfg(test)]
const RESEND_MEANS: &str = "It gets the message again; if it had read it, it sees it twice";
pub const TAKE_AS_RECEIVED: &str = "take_as_received";
pub const TAKE_AS_RECEIVED_LABEL: &str = "Take it as received";
#[cfg(test)]
const TAKE_AS_RECEIVED_MEANS: &str = "The message is not sent again and the task goes on";

fn session_ended_card_id(task: &TaskId, generation: u64, cursor: u64) -> InteractionId {
    InteractionId(format!("{SESSION_ENDED_PREFIX}{task}-{generation}-{cursor}"))
}

/// How many ends of a host in a row, with no turn finished between them, Alondra's own ends may
/// reach before the session is taken for one that keeps closing.
const ALONDRA_ENDS_IN_A_ROW: usize = 3;

/// Whether a session whose host just ended, after `journal`, would only end the same way if
/// resumed. An end the session caused itself counts when it already ended once since its last
/// finished turn; an end Alondra caused (`by_alondra`) says nothing about the session, and counts
/// only once `ALONDRA_ENDS_IN_A_ROW` ends came with no finished turn, so an error of Alondra's own
/// that repeats does not resume it for ever.
fn keeps_closing<'a>(journal: impl DoubleEndedIterator<Item = &'a LifecycleRecord>, by_alondra: bool) -> bool {
    if !by_alondra {
        return resumed_without_progress(journal);
    }
    let ended = journal.rev().take_while(|record| !matches!(record, LifecycleRecord::TurnEnded { .. } | LifecycleRecord::HostStopped { .. }))
        .filter(|record| matches!(record, LifecycleRecord::HostEnded { .. })).count();
    ended + 1 >= ALONDRA_ENDS_IN_A_ROW
}

/// Whether Alondra already resumed the task's session by itself after its host ended, and no turn
/// of it has ended since: resuming it once more would only end the same way. Read from the
/// journal's facts.
fn resumed_without_progress<'a>(journal: impl DoubleEndedIterator<Item = &'a LifecycleRecord>) -> bool {
    journal
        .rev()
        .find_map(|record| match record {
            LifecycleRecord::TurnEnded { .. } | LifecycleRecord::HostStopped { .. } => Some(false),
            LifecycleRecord::HostEnded { resume: Some(_), .. } | LifecycleRecord::DoubtResent { .. } => Some(true),
            _ => None,
        })
        .unwrap_or(false)
}

/// Whether answering `card` takes the task's execution over again.
pub fn retakes(card: &Interaction) -> bool {
    matches!(&card.payload, InteractionPayload::Question { options, .. } if options.iter().any(|option| option.id == RETAKE))
}

fn host_lost_card(id: InteractionId, task: &TaskId, title: &str, reason: &str, revision: u64) -> Interaction {
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| format!("“{title}” stopped: Alondra could not resume its session"));
    let mut card = CardSpec::new(CardKind::Problem, need).why(HOST_LOST_WHY).content(ContentKind::Diagnosis, &capitalized(reason));
    card.recommended = Some(RETAKE.into());
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: capitalized(reason),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question {
            options: vec![
                QuestionOption { id: RETAKE.into(), label: RETAKE_LABEL.into(), description: Some(RETAKE_MEANS.into()) },
                QuestionOption { id: CANCEL_TASK.into(), label: CANCEL_TASK_LABEL.into(), description: Some(CANCEL_TASK_MEANS.into()) },
            ],
            answer: None,
        },
        revision,
        order: revision,
        withdrawn: false,
        card,
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(SystemCard::HostLost), shortened: false,
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.trim().chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

impl DomainState {
    /// What the node said it runs after its PR merged, once its delivery is settled and waits for
    /// that work to end: a host that ends then leaves the task on a card about that work.
    pub(super) fn work_after_merge(&self, task: &TaskId, lifecycle: &TaskLifecycle) -> Option<String> {
        if lifecycle.phase() != TaskPhase::Delivered || lifecycle.post_merge().is_none() {
            return None;
        }
        let task = self.tasks.get(task).filter(|task| self.finished(task))?;
        task.wrap_up.post_merge_work.as_ref().map(|work| work.reason.clone())
    }
}

/// The option of a card about the work after the merge that takes that work as done.
pub const CLOSE_TASK: &str = "close_task";
pub const CLOSE_TASK_LABEL: &str = "Close the task";
const CLOSE_TASK_MEANS: &str = "Its work after the merge counts as done: the task is completed and Alondra closes its session";
pub const POST_MERGE_ENDED_WHY: &str = "Its PR is merged. Its session closed while it ran what it said comes after the merge, and whatever ran in the background stopped. Tell it to go on, with all its context, or close the task if that work is done.";
pub const POST_MERGE_STOPPED_WHY: &str = "Its PR is merged and you stopped what it ran after the merge. Tell it to go on, with all its context, or close the task if that work is done.";

/// What the resumed session is told about the background work that ended with its host: nothing
/// will report it back, so it runs again what it still needs.
fn ended_work_note(ended_work: &[String]) -> String {
    if ended_work.is_empty() {
        return String::new();
    }
    let list: String = ended_work.iter().map(|work| format!("\n- {work}")).collect();
    format!("\n\nThis background work ended with that host and will not report back:{list}\nRun again what you still need and wait for its result.")
}

fn post_merge_resumed_instruction(reason: &str) -> String {
    format!("Your session was cut off when its host stopped, and Alondra resumed it. Your PR is merged: check where you were and go on with the work you said runs after the merge (“{reason}”). When it is done, call post_merge_work without a reason, clean up and call cleanup_done.")
}

fn post_merge_carry_on_instruction(reason: &str) -> String {
    format!("Go on with the work you said runs after the merge (“{reason}”). When it is done, call post_merge_work without a reason, clean up and call cleanup_done.")
}

fn post_merge_ended_card(id: InteractionId, task: &TaskId, title: &str, reason: &str, forced: bool, revision: u64) -> Interaction {
    let (need, why) = if forced {
        (fit_title(title, CardSpec::TITLED_NEED_MAX, |title| format!("You stopped “{title}” during its work after the merge: tell it how to go on")), POST_MERGE_STOPPED_WHY)
    } else {
        (fit_title(title, CardSpec::TITLED_NEED_MAX, |title| format!("“{title}” stopped its work after the merge when its session closed: tell it how to go on")), POST_MERGE_ENDED_WHY)
    };
    let need = clip(&need, CardSpec::NEED_MAX);
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: need.clone(),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question {
            options: vec![
                QuestionOption { id: CARRY_ON.into(), label: CARRY_ON_LABEL.into(), description: Some(CARRY_ON_MEANS.into()) },
                QuestionOption { id: CLOSE_TASK.into(), label: CLOSE_TASK_LABEL.into(), description: Some(CLOSE_TASK_MEANS.into()) },
            ],
            answer: None,
        },
        revision,
        order: revision,
        withdrawn: false,
        card: CardSpec::new(CardKind::Decide, need).why(why).content(ContentKind::Diagnosis, reason),
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(SystemCard::SessionEnded), shortened: false,
    }
}

fn session_ended_card(id: InteractionId, task: &TaskId, title: &str, why: &str, revision: u64) -> Interaction {
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| format!("“{title}” was interrupted when its session closed: tell it how to go on"));
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: need.clone(),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question {
            options: vec![
                QuestionOption { id: CARRY_ON.into(), label: CARRY_ON_LABEL.into(), description: Some(CARRY_ON_MEANS.into()) },
                QuestionOption { id: CANCEL_TASK.into(), label: CANCEL_TASK_LABEL.into(), description: Some(CANCEL_TASK_MEANS.into()) },
            ],
            answer: None,
        },
        revision,
        order: revision,
        withdrawn: false,
        card: CardSpec::new(CardKind::Decide, need).why(why),
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(SystemCard::SessionEnded), shortened: false,
    }
}
const SESSION_ENDED_WHY: &str = "Its session closed mid-way, as when the Mac shuts down. It resumes with all its context; whatever ran in the background stopped.";
const HOST_LOST_WHY: &str = "Its session could not be started again; the details say why. Try again once that is fixed, or cancel the task.";
pub const SESSION_ENDED_AGAIN_WHY: &str = "Alondra resumed its session after it closed, and it closed again before it could finish a turn. It resumes with all its context if you tell it to go on.";

#[cfg(test)]
pub(super) fn handoff_doubt_card(id: InteractionId, task: &TaskId, title: &str, text: &str, launch: bool, revision: u64) -> Interaction {
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| format!("“{title}” closed before confirming it read your message: send it again?"));
    let mut options = vec![QuestionOption { id: RESEND.into(), label: RESEND_LABEL.into(), description: Some(RESEND_MEANS.into()) }];
    options.push(if launch {
        QuestionOption { id: CANCEL_TASK.into(), label: CANCEL_TASK_LABEL.into(), description: Some(CANCEL_TASK_MEANS.into()) }
    } else {
        QuestionOption { id: TAKE_AS_RECEIVED.into(), label: TAKE_AS_RECEIVED_LABEL.into(), description: Some(TAKE_AS_RECEIVED_MEANS.into()) }
    });
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: need.clone(),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question { options, answer: None },
        revision,
        order: revision,
        withdrawn: false,
        card: CardSpec::new(CardKind::Decide, need).why(if launch { HANDOFF_DOUBT_LAUNCH_WHY } else { HANDOFF_DOUBT_WHY }).content(ContentKind::Document, text),
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(SystemCard::HandoffDoubt), shortened: false,
    }
}
/// The cards that ask the user about an input Claude will never read.
pub const DROPPED_PREFIX: &str = "dropped-";
pub const SEND_AGAIN: &str = "send_again";
pub const SEND_AGAIN_LABEL: &str = "Send it again";
const SEND_AGAIN_MEANS: &str = "It gets the same message again";
pub const DISCARD: &str = "discard";
pub const DISCARD_LABEL: &str = "Discard";
const DISCARD_MEANS: &str = "The message is not sent and the task goes on";
pub const DROPPED_WHY: &str = "Claude Code finished or cancelled your message without reading it, so it will not act on it. Nothing is sent until you decide.";

fn dropped_card_id(task: &TaskId, generation: u64, input: &str) -> InteractionId {
    let input: String = input.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).collect();
    InteractionId(format!("{DROPPED_PREFIX}{task}-{generation}-{input}"))
}

fn dropped_card(id: InteractionId, task: &TaskId, text: &str, revision: u64) -> Interaction {
    let need = "Claude didn't get to read your message".to_owned();
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: need.clone(),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question {
            options: vec![
                QuestionOption { id: SEND_AGAIN.into(), label: SEND_AGAIN_LABEL.into(), description: Some(SEND_AGAIN_MEANS.into()) },
                QuestionOption { id: DISCARD.into(), label: DISCARD_LABEL.into(), description: Some(DISCARD_MEANS.into()) },
            ],
            answer: None,
        },
        revision,
        order: revision,
        withdrawn: false,
        card: CardSpec::new(CardKind::Decide, need).why(DROPPED_WHY).content(ContentKind::Document, text),
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(SystemCard::Dropped), shortened: false,
    }
}

const HANDOFF_DOUBT_WHY: &str = "Its session closed right after the message was handed over and before it showed it had read it. Nothing is sent until you decide.";
const HANDOFF_DOUBT_LAUNCH_WHY: &str = "Its session closed before it started on the task, right after the message was handed over. Nothing is sent until you decide.";

fn refused(task: &TaskId, record: &LifecycleRecord, lifecycle: &TaskLifecycle, refusal: Refusal) -> DomainError {
    let code = if refusal == Refusal::RepeatedTakeover { REPEATED_TAKEOVER } else { "conflict" };
    DomainError::new(code, format!(
        "The lifecycle of task {task} ({:?}) refuses {record:?}: {refusal:?}", lifecycle.phase()))
}

fn is_false(value: &bool) -> bool {
    !value
}

/// The changes the user asked for on a result the task's earlier node delivered: they go to the
/// node that takes the task over, since the one they were sent to is gone.
pub fn asked_changes(state: &DomainState, task: &Task) -> String {
    let Some(InteractionPayload::Delivery { feedback, acceptance, .. }) = task.final_presentation.as_ref()
        .and_then(|id| state.interactions.get(id)).map(|result| &result.payload)
    else {
        return String::new();
    };
    if feedback.is_empty() || acceptance.as_ref().is_some_and(|accepted| accepted.valid) {
        return String::new();
    }
    let asked: Vec<String> = feedback.iter().map(|change| format!("- {}", change.body.trim())).collect();
    format!("## Changes the user asked for on the result you take over\n\n{}", asked.join("\n"))
}
