//! The one authority of a task's lifecycle: its `TaskLifecycle`, replayed inside the transaction
//! from the records its decisions left in the task's journal. The task's state, node, start and
//! ending in the DTO are read from it and checked against it before a transition is accepted, so
//! no other writer can move them. Every task has a journal; the store keeps it as the events these
//! records stand for, and `journal_writes`/`records_from_events` are the two ways across.
use super::*;
use alondra_domain::{
    ActivityView, CLAUDE_BG_HOST, CallbackId, ContinuationId, Control, ConversationId, Decision, DrainObligation, Event,
    FinishIntent, HostId, InputId, InstanceId, OperationId, PermissionMode, PlanReviewOutcome as ReviewSettlement,
    PostMergeFact, Refusal, ResourceEvidence, RuntimeFact, TaskLifecycle, TaskPhase, Trigger, TurnId, WaitReason, WorkId,
    decision_callback, decision_continuation, decision_epoch,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
}

impl From<PostMergeEnd> for PostMergeFact {
    fn from(end: PostMergeEnd) -> Self {
        match end {
            PostMergeEnd::NodeDeclaredEnd => Self::NodeDeclaredEnd,
            PostMergeEnd::Cleanup => Self::Cleanup,
            PostMergeEnd::UserDecision => Self::UserDecision,
            PostMergeEnd::SessionOver => Self::SessionOver,
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
    /// An MCP server's request for input accepted with what the user wrote.
    Accept { text: String },
    Decline,
}
impl From<&DecisionRecord> for Decision {
    fn from(record: &DecisionRecord) -> Self {
        match record {
            DecisionRecord::Allow => Self::Allow,
            DecisionRecord::Answers { answers } => Self::Answers { answers: answers.clone() },
            DecisionRecord::Deny { message } => Self::Deny { message: message.clone() },
            DecisionRecord::Accept { text } => Self::Accept { text: text.clone() },
            DecisionRecord::Decline => Self::Decline,
        }
    }
}
impl From<&Decision> for DecisionRecord {
    fn from(decision: &Decision) -> Self {
        match decision {
            Decision::Allow => Self::Allow,
            Decision::Answers { answers } => Self::Answers { answers: answers.clone() },
            Decision::Deny { message } => Self::Deny { message: message.clone() },
            Decision::Accept { text } => Self::Accept { text: text.clone() },
            Decision::Decline => Self::Decline,
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
    /// Work that outlived its turn woke Claude, which took it up in a root turn of its own.
    TurnResumed { turn: String, continuation: String },
    InputConsumed { input: String },
    WorkStarted { turn: String, work: String },
    WorkEnded { work: String, continuation: String },
    /// Work ended while the root turn that started it was still open: that turn goes on with it.
    WorkContinued { continuation: String },
    /// The model answered: the API and the account Claude Code uses work. A fact about Claude,
    /// not about the task: it records nothing in the task's journal.
    ModelAnswered,
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
    InputConsumed { input: String },
    WorkStarted { turn: String, work: String },
    WorkEnded { work: String, continuation: String },
    /// The root turn still open when work ended goes on with the work's `continuation`.
    WorkContinued { continuation: String },
    /// The root turn of an SDK execution ended. `unheard_card` is the card the aggregate requires
    /// when nothing else explains the task's next step; otherwise it stays unused.
    TurnEnded { turn: String, unheard_card: InteractionId },
    /// The user answered a card the lifecycle requires: the task goes on with `input`, whose text
    /// is what it is told.
    Answered {
        card: InteractionId,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    ExecutionReleased { input: String },
    /// Someone told the task's node `text` outside a card, as `input`.
    Told { input: String, text: String },
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
    Paused { card: InteractionId },
    /// The hold `card` ended: the task goes on with `input`, whose text is what it is told.
    Resumed {
        card: InteractionId,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
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

/// The aggregates of the tasks one transaction decides on, replayed from their journals when first
/// needed and dropped with the transaction. Nothing clones or restores one from the DTO.
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
    replayed: BTreeMap<TaskId, TaskLifecycle>,
}

impl Lifecycles {
    pub(super) fn new() -> Self {
        Self { replayed: BTreeMap::new() }
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
        LifecycleRecord::TurnEnded { turn, unheard_card } => runtime(RuntimeFact::TurnEnded {
            turn: TurnId::new(turn.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            unheard_card: unheard_card.clone(),
        })?,
        LifecycleRecord::Answered { card, input, .. } => Event::Answer { card: card.clone(), input: input_id(task, input)? },
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
        LifecycleRecord::Paused { card } => Event::Pause { card: card.clone() },
        LifecycleRecord::Resumed { card, input, .. } => Event::Resume { card: card.clone(), input: input_id(task, input)? },
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
        LifecycleRecord::InputConsumed { input } => runtime(RuntimeFact::InputConsumed { input: input_id(task, input)? })?,
        LifecycleRecord::ExecutionReleased { input } => Event::ExecutionReleased { input: input_id(task, input)? },
        LifecycleRecord::Told { input, .. } => Event::Tell { input: input_id(task, input)? },
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
pub fn replay(task: &TaskId, journal: &[LifecycleRecord]) -> Result<TaskLifecycle> {
    let mut records = journal.iter();
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
        LifecycleRecord::Told { input, text } => Some((input, Some(text.as_str()))),
        LifecycleRecord::Answered { input, text, .. }
        | LifecycleRecord::Resumed { input, text, .. }
        | LifecycleRecord::HandedBack { input, text, .. } => Some((input, text.as_deref())),
        LifecycleRecord::HandoffSettled { resent: Some(input), text, .. }
        | LifecycleRecord::HandoffResent { resent: input, text, .. }
        | LifecycleRecord::DoubtResent { resent: input, text, .. }
        | LifecycleRecord::HostEnded { resume: Some(input), text, .. } => Some((input, text.as_deref())),
        _ => None,
    }
}

/// The store's writes for a task's journal from record `from` on. The first record creates the
/// task with its initial input and has no event.
pub fn journal_writes(task: &TaskId, journal: &[LifecycleRecord], from: usize) -> Result<Vec<JournalWrite>> {
    let mut records = journal.iter();
    let Some(LifecycleRecord::Planned { input }) = records.next() else {
        return Err(corrupt(task, "it does not start with the planned record"));
    };
    let mut lifecycle = TaskLifecycle::planned(task.clone(), input_id(task, input)?).map_err(|r| corrupt(task, format!("{r:?}")))?;
    let mut writes = vec![];
    if from == 0 {
        writes.push(JournalWrite { revision: 0, event: None, input: Some(input_id(task, input)?), text: None });
    }
    for (n, record) in records.enumerate() {
        let event = event(task, record, &lifecycle)?;
        let revision = lifecycle.revision();
        if n + 1 >= from {
            let (input, text) = match introduced(record) {
                Some((input, text)) => (Some(input_id(task, input)?), text.map(str::to_owned)),
                None => (None, None),
            };
            writes.push(JournalWrite { revision, event: Some(event.clone()), input, text });
        }
        lifecycle.apply(revision, event).map_err(|refusal| corrupt(task, format!("record {} {record:?} refused: {refusal:?}", n + 1)))?;
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
        Event::Runtime { fact: RuntimeFact::TurnEnded { turn, unheard_card }, .. } => {
            LifecycleRecord::TurnEnded { turn: turn.as_str().into(), unheard_card: unheard_card.clone() }
        }
        Event::Tell { input } => LifecycleRecord::Told {
            input: input.as_str().into(),
            text: texts.get(input).cloned().ok_or_else(|| corrupt(task, format!("what it was told as {input:?} has no text")))?,
        },
        Event::Answer { card, input } => LifecycleRecord::Answered {
            card: card.clone(),
            input: input.as_str().into(),
            text: texts.get(input).cloned(),
        },
        Event::Pause { card } => LifecycleRecord::Paused { card: card.clone() },
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
        Event::Runtime { fact: RuntimeFact::InputConsumed { input }, .. } => LifecycleRecord::InputConsumed { input: input.as_str().into() },
        Event::ExecutionReleased { input } => LifecycleRecord::ExecutionReleased { input: input.as_str().into() },
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
    /// The input a task waits with, named after the revision that queued it: an identity of
    /// Alondra's own, never one of the SDK's.
    pub(super) fn next_input(&self, task: &TaskId) -> String {
        format!("input:{task}:{}", self.revision + 1)
    }
    pub(super) fn next_operation(&self, kind: &str, task: &TaskId) -> String {
        format!("{kind}:{task}:{}", self.revision + 1)
    }

    pub(super) fn lifecycle<'a>(&self, lifecycles: &'a mut Lifecycles, task: &TaskId) -> Result<&'a TaskLifecycle> {
        if !lifecycles.replayed.contains_key(task) {
            let journal = self.lifecycle_journal.get(task).ok_or_else(|| DomainError::new("lifecycle_journal", format!("Task {task} has no lifecycle journal")))?;
            lifecycles.replayed.insert(task.clone(), replay(task, journal)?);
        }
        Ok(&lifecycles.replayed[task])
    }

    /// Opens the journal of a task that was just created: from here on only decisions move it.
    pub(super) fn plan_lifecycle(&mut self, lifecycles: &mut Lifecycles, task: &TaskId) -> Result<()> {
        let record = LifecycleRecord::Planned { input: self.next_input(task) };
        let lifecycle = replay(task, std::slice::from_ref(&record))?;
        self.lifecycle_journal.insert(task.clone(), vec![record]);
        self.materialize(task, &lifecycle);
        lifecycles.replayed.insert(task.clone(), lifecycle);
        Ok(())
    }

    /// A lifecycle decision on a task: the aggregate validates it, the journal keeps it and
    /// the task's DTO reads it, in one step. A refusal changes nothing.
    pub(super) fn decide(&mut self, lifecycles: &mut Lifecycles, task: &TaskId, record: LifecycleRecord) -> Result<()> {
        self.lifecycle(lifecycles, task)?;
        let lifecycle = lifecycles.replayed.get_mut(task).expect("replayed above");
        let event = event(task, &record, lifecycle)?;
        lifecycle.apply(lifecycle.revision(), event).map_err(|refusal| refused(task, &record, lifecycle, refusal))?;
        self.lifecycle_journal.get_mut(task).expect("replayed above").push(record);
        let lifecycle = &lifecycles.replayed[task];
        self.materialize(task, lifecycle);
        Ok(())
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
        }
        Ok(())
    }

    fn read_ending(&mut self, task: &TaskId, lifecycle: &TaskLifecycle) -> Result<()> {
        let t = self.tasks.get_mut(task).ok_or_else(|| DomainError::new("not_found", format!("Task {task} has a journal and no row")))?;
        t.ending = ending_of(lifecycle);
        Ok(())
    }

    /// The journals the store read, each with the lifecycle its replay left: every task's ending
    /// is read from it and every task is checked against it, as `read_endings` and
    /// `validate_journal` do, without replaying any journal again. The lifecycles are kept for
    /// `validate_loaded`.
    pub fn load_journal(&mut self, journals: impl IntoIterator<Item = (TaskId, Vec<LifecycleRecord>, TaskLifecycle)>) -> Result<Lifecycles> {
        let mut lifecycles = Lifecycles::new();
        for (task, records, lifecycle) in journals {
            self.lifecycle_journal.insert(task.clone(), records);
            lifecycles.replayed.insert(task, lifecycle);
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
            let journal = self.lifecycle_journal.get(task).ok_or_else(|| DomainError::new("lifecycle_journal", format!("Task {task} has no lifecycle journal")))?;
            self.validate_lifecycle(task, &replay(task, journal)?)?;
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
        self.decide(lifecycles, task, LifecycleRecord::Admitted { node: node.clone(), execution: None })?;
        let running = self.agents.get(node).is_some_and(|agent| agent.phase.live() && !matches!(agent.phase, NodePhase::Starting { .. } | NodePhase::Lost));
        if running {
            self.decide(lifecycles, task, LifecycleRecord::ResourceAvailable)?;
        }
        Ok(())
    }

    /// The current SDK execution of `generation`: facts about another, or about one of the
    /// `claude --bg` backend, are refused.
    pub(super) fn sdk_execution(&self, task: &TaskId, generation: u64, lifecycles: &mut Lifecycles) -> Result<()> {
        current_sdk(task, generation, self.lifecycle(lifecycles, task)?)
    }

    /// The task's lifecycle replayed, when its current execution is the SDK execution of
    /// `generation`: what a guard judges facts about that execution on.
    pub(super) fn sdk_replay(&self, task: &TaskId, generation: u64) -> Result<TaskLifecycle> {
        let journal = self.lifecycle_journal.get(task).ok_or_else(|| DomainError::new("lifecycle_journal", format!("Task {task} has no lifecycle journal")))?;
        let lifecycle = replay(task, journal)?;
        current_sdk(task, generation, &lifecycle)?;
        Ok(lifecycle)
    }

    /// Applies `record` to a replay a guard judges on, as `decide` would apply it.
    pub(super) fn judge(&self, task: &TaskId, lifecycle: &mut TaskLifecycle, record: &LifecycleRecord) -> Result<()> {
        let event = event(task, record, lifecycle)?;
        lifecycle.apply(lifecycle.revision(), event).map_err(|refusal| refused(task, record, lifecycle, refusal))
    }

    /// The root turn of the task's SDK execution ended. A result of the SDK is evidence, never
    /// success: nothing is delivered, finished or released. When nothing explains what the task
    /// does next (no work of its own, no continuation, no input waiting), the lifecycle requires
    /// the card that asks the user how it goes on, and it is created in this same step.
    ///
    /// A turn a Claude API error ended is held instead when waiting helps: on the one card that
    /// asks the user to sign in to Claude, or until the API or the usage limit lets it go on.
    pub(super) fn record_turn_boundary(&mut self, task: &TaskId, generation: u64, turn: &str, failure: Option<&SdkApiFailure>, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        let card = turn_ended_card_id(task, generation, turn);
        if self.interactions.contains_key(&card) {
            return Err(DomainError::new("conflict", format!("The end of turn {turn} of task {task} is already recorded")));
        }
        let kind = failure.and_then(|failure| classify_sdk(&failure.error, failure.status));
        let interrupted = self.lifecycle_journal.get(task).is_some_and(|journal| interrupted(journal, turn));
        if let Some(hold) = kind.and_then(ClaudeHold::for_failure) {
            self.hold_on_claude(task, hold, failure.and_then(|failure| failure.resets_at), effects, lifecycles)?;
        }
        self.decide(lifecycles, task, LifecycleRecord::TurnEnded { turn: turn.into(), unheard_card: card.clone() })?;
        if self.lifecycle(lifecycles, task)?.cards().iter().any(|(required, _)| **required == card) {
            let title = self.tasks[task].title.clone();
            let end = match failure.filter(|_| kind == Some(SdkFailureKind::NeedsUser)) {
                Some(failure) => TurnEnd::Refused(failure),
                None if interrupted => TurnEnd::Interrupted,
                None => TurnEnd::Unexplained,
            };
            self.interactions.insert(card.clone(), turn_ended_card(card.clone(), task, &title, end, self.revision + 1));
            if !self.covered_by_recorded_wait(&self.interactions[&card]) {
                effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
            }
        }
        Ok(())
    }

    /// A card that says a turn ended with nothing that explains the task's next step, while
    /// something Alondra recorded does (`recorded_wait`). The card waits out of sight until what
    /// ends that wait reaches the node and ends the card, and shows only once nothing covers it.
    pub(super) fn covered_by_recorded_wait(&self, card: &Interaction) -> bool {
        let Scope::Task { task_id } = &card.scope else { return false };
        card.author == Author::System && card.id.0.starts_with(TURN_ENDED_PREFIX) && card.card.why == TURN_ENDED_WHY
            && self.recorded_wait(task_id, card.revision).is_some()
    }

    /// What the node of `task` waits for, as Alondra recorded it, for a turn that ended at
    /// `revision`: a question or request of its own to the user or the orchestrator (a message
    /// or a suggestion too) still open, or answered after `revision` so the answer is on its way
    /// to the node; a pause that holds it (`wait_for_machine`); or another task its task depends
    /// on, not done yet or done after `revision`, so the word that it is done is on its way.
    /// Only facts end it, never time.
    pub fn recorded_wait(&self, task: &TaskId, revision: u64) -> Option<RecordedWait> {
        let node = self.tasks.get(task)?.agent_id.clone()?;
        let waits_on = |recipient: &Recipient| self.interactions.values().any(|i| {
            matches!(&i.scope, Scope::Task { task_id } if task_id == task)
                && i.author == Author::Agent { agent_id: node.clone() }
                && i.recipient == *recipient
                && !i.withdrawn
                && !self.holds.contains_key(&i.id)
                && match &i.payload {
                    InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. } =>
                        answer.as_ref().is_none_or(|answer| answer.at_revision > revision),
                    _ => false,
                }
        });
        let paused = matches!(self.node_phase(&node), Some(NodePhase::Paused(pause)) if !pause.released);
        if waits_on(&Recipient::User) {
            Some(RecordedWait::User)
        } else if paused {
            Some(RecordedWait::Machine)
        } else if waits_on(&Recipient::Orchestrator) {
            Some(RecordedWait::Orchestrator)
        } else if self.tasks.get(task).is_some_and(|t| !self.unmet_dependencies(t).is_empty()
            || t.dependencies.iter().filter_map(|dep| self.tasks.get(dep)).any(|dep| dep.state == TaskState::Done && dep.revision > revision)) {
            Some(RecordedWait::Tasks)
        } else {
            None
        }
    }

    /// What the node of `task` waits for now, if its last turn ended on a card that asks the
    /// user how it goes on and that wait covers it, or else on something still unanswered.
    pub fn waiting_on(&self, task: &TaskId) -> Option<RecordedWait> {
        let since = self.interactions.values()
            .filter(|i| matches!(&i.scope, Scope::Task { task_id } if task_id == task) && i.id.0.starts_with(TURN_ENDED_PREFIX) && self.open(i))
            .map(|i| i.revision)
            .max()
            .unwrap_or(u64::MAX);
        self.recorded_wait(task, since)
    }

    /// The user answered `card`, one the lifecycle of the task requires: cancelling ends the
    /// task, any other answer tells it how to go on. False for a card the lifecycle does not hold.
    pub(super) fn answer_lifecycle_card(&mut self, card: &InteractionId, option: &str, written: Option<&str>, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<bool> {
        let Some(Scope::Task { task_id }) = self.interactions.get(card).map(|i| i.scope.clone()) else { return Ok(false) };
        if !self.lifecycle_journal.contains_key(&task_id) || !self.lifecycle(lifecycles, &task_id)?.cards().iter().any(|(held, _)| *held == card) {
            return Ok(false);
        }
        if option == CANCEL_TASK {
            self.cancel(&task_id, effects, lifecycles)?;
            return Ok(true);
        }
        if self.lifecycle(lifecycles, &task_id)?.cards().iter().any(|(held, reason)| *held == card && *reason == alondra_domain::WaitReason::Doubt) {
            self.settle_handoff(&task_id, card, option, written, lifecycles)?;
            return Ok(true);
        }
        let mut text = if option == CARRY_ON { CARRY_ON_INSTRUCTION.to_owned() } else { String::new() };
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
    /// go on, as a session reopens with `resume` on its next turn: a host's end is not the user's
    /// decision. Only when it cannot (the launch never confirmed, or the session already ended
    /// again before a turn of it ended) does a card ask the user, saying why; a turn the user
    /// forced to stop ends as the interrupt they asked for.
    pub(super) fn record_host_ended(&mut self, task: &TaskId, generation: u64, resumes: bool, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        let lifecycle = self.lifecycle(lifecycles, task)?;
        let card = session_ended_card_id(task, generation, lifecycle.cursor().unwrap_or(0) + 1);
        let forced = lifecycle.turn_stop() == Some(alondra_domain::TurnStop::ForceRequested);
        let held: Vec<InteractionId> = lifecycle.cards().into_iter().map(|(card, _)| card.clone()).collect();
        let available = lifecycle.resource() == Some(ResourceEvidence::Available);
        let looped = resumes && resumed_without_progress(&self.lifecycle_journal[task]);
        let ended = |resume: Option<String>| LifecycleRecord::HostEnded {
            unheard_card: card.clone(),
            text: resume.as_ref().map(|_| HOST_RESUMED_INSTRUCTION.to_owned()),
            resume,
        };
        let resumed = resumes && !forced && !looped && available && self.decide(lifecycles, task, ended(Some(self.next_input(task)))).is_ok();
        if !resumed {
            self.decide(lifecycles, task, ended(None))?;
        }
        let now: Vec<InteractionId> = self.lifecycle(lifecycles, task)?.cards().into_iter().map(|(card, _)| card.clone()).collect();
        for gone in held.iter().filter(|card| !now.contains(card)) {
            self.retire(gone);
        }
        if now.contains(&card) {
            let title = self.tasks[task].title.clone();
            let ended = if forced {
                turn_ended_card(card.clone(), task, &title, TurnEnd::Interrupted, self.revision + 1)
            } else {
                session_ended_card(card.clone(), task, &title, if looped { SESSION_ENDED_AGAIN_WHY } else { SESSION_ENDED_WHY }, self.revision + 1)
            };
            self.interactions.insert(card.clone(), ended);
            effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
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

    /// `input`, told `text`, was handed to a host that ended before it showed the input was read.
    /// When a host that resumes the same Claude session takes its place (`resumes`), the same text
    /// goes to it with a note that its conversation may already have it: Claude reads its own
    /// transcript, so it sees whether it did. Otherwise, or when the session already ended again
    /// before a turn of it ended, the task holds on a card that asks the user to send it again or
    /// take it as received.
    pub(super) fn record_doubtful_handoff(&mut self, task: &TaskId, generation: u64, input: &str, text: &str, resumes: bool, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        let card = handoff_doubt_card_id(task, generation, input);
        if self.interactions.contains_key(&card) {
            return Err(DomainError::new("conflict", format!("The doubtful input {input} of task {task} is already recorded")));
        }
        let journal = &self.lifecycle_journal[task];
        let before_end = journal.iter().rposition(|record| matches!(record, LifecycleRecord::HostEnded { .. })).unwrap_or(journal.len());
        if resumes && !resumed_without_progress(&journal[..before_end]) {
            let record = LifecycleRecord::DoubtResent { input: input.into(), resent: self.next_input(task), text: Some(format!("{RESENT_NOTE}\n\n{text}")) };
            return self.decide(lifecycles, task, record);
        }
        let launch = self.lifecycle(lifecycles, task)?.resource() == Some(ResourceEvidence::Reserved);
        self.decide(lifecycles, task, LifecycleRecord::HandoffDoubted { card: card.clone(), input: input.into() })?;
        let title = self.tasks[task].title.clone();
        self.interactions.insert(card.clone(), handoff_doubt_card(card.clone(), task, &title, text, launch, self.revision + 1));
        effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
        Ok(())
    }

    /// The task's SDK execution could not be taken over and has no host: the task holds on a card
    /// that says why and offers to try again or to cancel it. Nothing runs it until the user says.
    /// A card that only asked how the task goes on becomes this one, so the task has one card.
    pub(super) fn record_host_lost(&mut self, task: &TaskId, reason: &str, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let generation = self.lifecycle(lifecycles, task)?.execution().map(|stamp| stamp.generation())
            .ok_or_else(|| DomainError::new("conflict", format!("Task {task} has no execution to lose")))?;
        self.sdk_execution(task, generation, lifecycles)?;
        let cards: Vec<(InteractionId, WaitReason)> = self.lifecycle(lifecycles, task)?.cards().into_iter().map(|(card, reason)| (card.clone(), reason)).collect();
        if cards.iter().any(|(card, _)| self.interactions.get(card).is_some_and(retakes)) {
            return Ok(());
        }
        let title = self.tasks[task].title.clone();
        let unheard = cards.into_iter().find(|(card, reason)| *reason == WaitReason::Unheard && self.interactions.get(card).is_some_and(|i| i.author == Author::System && self.open(i)));
        if let Some((card, _)) = unheard {
            let order = self.interactions[&card].order;
            let mut lost = host_lost_card(card.clone(), task, &title, reason, self.revision + 1);
            lost.order = order;
            self.interactions.insert(card.clone(), lost);
            effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
            return Ok(());
        }
        let card = InteractionId(format!("{HOST_LOST_PREFIX}{task}-{}", self.revision + 1));
        self.decide(lifecycles, task, LifecycleRecord::Paused { card: card.clone() })?;
        self.interactions.insert(card.clone(), host_lost_card(card.clone(), task, &title, reason, self.revision + 1));
        effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
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
    pub(super) fn record_runtime_facts(&mut self, task: &TaskId, generation: u64, facts: &[RuntimeObservation], lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        if facts.is_empty() {
            return Err(DomainError::new("invalid_input", "An observation without facts records nothing"));
        }
        for fact in facts {
            let record = match fact.clone() {
                RuntimeObservation::ModelAnswered => {
                    self.claude_answered(lifecycles)?;
                    continue;
                }
                RuntimeObservation::ResourceAvailable => LifecycleRecord::ResourceAvailable,
                RuntimeObservation::TurnStarted { turn, input } => LifecycleRecord::TurnStarted { turn, input },
                RuntimeObservation::TurnResumed { turn, continuation } => {
                    if is_decision(generation, &continuation) {
                        return Err(DomainError::new("invalid_input", "A decision's turn goes on only when its decision reaches Claude"));
                    }
                    LifecycleRecord::TurnResumed { turn, continuation }
                }
                RuntimeObservation::InputConsumed { input } => LifecycleRecord::InputConsumed { input },
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

    /// The open task whose current execution `agent` runs through the SDK, its lifecycle replayed.
    pub(super) fn sdk_task_of(&self, agent: &AgentId) -> Result<(TaskId, TaskLifecycle)> {
        for (id, t) in &self.tasks {
            if t.agent_id.as_ref() != Some(agent) || t.over() || !self.lifecycle_journal.contains_key(id) {
                continue;
            }
            let lifecycle = replay(id, &self.lifecycle_journal[id])?;
            if lifecycle.execution().is_some_and(|stamp| stamp.node() == agent && stamp.host().as_str() != CLAUDE_BG_HOST) {
                return Ok((id.clone(), lifecycle));
            }
        }
        Err(DomainError::new("no_task", format!("Node {agent} has no task running through the Agent SDK")))
    }

    /// The lifecycle of an open task whose current execution runs through the SDK.
    pub(super) fn sdk_lifecycle(&self, task: &TaskId) -> Result<TaskLifecycle> {
        let t = self.tasks.get(task).ok_or_else(|| DomainError::new("not_found", "Task missing"))?;
        if t.over() {
            return Err(DomainError::new("task_not_active", format!("Task {task} is ending")));
        }
        let journal = self.lifecycle_journal.get(task).ok_or_else(|| DomainError::new("lifecycle_journal", format!("Task {task} has no lifecycle journal")))?;
        let lifecycle = replay(task, journal)?;
        if !lifecycle.execution().is_some_and(|stamp| stamp.host().as_str() != CLAUDE_BG_HOST) {
            return Err(DomainError::new("no_task", format!("Task {task} has no execution running through the Agent SDK")));
        }
        Ok(lifecycle)
    }

    /// `record` if the task's lifecycle accepts it, judged on a replay that is then dropped.
    pub(super) fn judged(&self, task: &TaskId, mut lifecycle: TaskLifecycle, record: LifecycleRecord) -> Result<LifecycleRecord> {
        let event = event(task, &record, &lifecycle)?;
        lifecycle.apply(lifecycle.revision(), event).map_err(|refusal| refused(task, &record, &lifecycle, refusal))?;
        Ok(record)
    }

    /// The task's node is gone and left nothing behind: the task waits for another.
    pub(super) fn release(&mut self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        let record = LifecycleRecord::ExecutionReleased { input: self.next_input(task) };
        self.decide(lifecycles, task, record)
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
        let owed = self.lifecycle_journal.get(&task.id)
            .and_then(|journal| replay(&task.id, journal).ok())
            .is_some_and(|lifecycle| lifecycle.post_merge().is_some());
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
        if !lifecycle.accepted() {
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

    /// The state of a task within the phase its lifecycle materialized: what it waits for
    /// or who has to act. Never a phase change: no Done, Cancelled or Closing comes from here.
    pub(super) fn state_within_phase(&self, t: &Task) -> TaskState {
        match t.ending {
            Some(TaskEnding::Cancelling) => return TaskState::Closing,
            Some(TaskEnding::Completing) => {
                return if t.wrap_up.post_merge_work.is_some() && !self.node_session_closed(t) { TaskState::Running } else { TaskState::Closing };
            }
            None => {}
        }
        let goes_on = t.final_presentation.as_ref().is_none_or(|p| {
            self.interactions.get(p).is_some_and(|p| self.handed_back(p).is_some() && !self.accepted(p) && !self.settled(t, p))
        });
        if self.stack_ready(t).is_err() || (goes_on && self.waits_are_over(t).is_err()) {
            TaskState::WaitingDependency
        } else if self.blockers(&t.id) {
            TaskState::NeedsYou
        } else if let Some(p) = t.final_presentation.as_ref().and_then(|p| self.interactions.get(p)) {
            if (self.settled(t, p) || self.accepted(p)) && self.unanswered(&t.id) {
                TaskState::NeedsYou
            } else if self.settled(t, p) {
                if t.wrap_up.post_merge_work.is_some() && !self.node_session_closed(t) { TaskState::Running } else { TaskState::Closing }
            } else if self.accepted(p) {
                TaskState::WaitingDependency
            } else if self.handed_back(p).is_some() || !t.blocks.is_empty() {
                if t.started || t.agent_id.is_some() { TaskState::Running } else { TaskState::NotStarted }
            } else if !self.open_blockers(&t.id).is_empty() {
                TaskState::AwaitingBlocker
            } else if matches!(self.attention(t), Attention::Wait(_)) {
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
    let mut card = turn_ended_card(id, task, title, TurnEnd::Unexplained, revision);
    let need = clip(&format!("“{title}” stopped waiting for your decision: tell it how to go on"), CardSpec::NEED_MAX);
    card.body = need.clone();
    card.card = CardSpec::new(CardKind::Decide, need).why(DECISION_CANCELLED_WHY);
    card
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
const TURN_ENDED_WHY: &str = "Its turn ended with nothing that explains its next step. Tell it to go on, write to it what to do, or cancel the task.";

pub const REFUSED_WHY: &str = "Claude Code cannot use the Claude API until this is fixed outside Alondra. Fix it, then tell the task to go on, or cancel it.";
pub const INTERRUPTED_WHY: &str = "You stopped its turn. Tell it to carry on, write what to do, or cancel the task.";

/// Why a root turn ended with nothing that explains the task's next step.
enum TurnEnd<'a> {
    Unexplained,
    Interrupted,
    Refused(&'a SdkApiFailure),
}

/// Whether the user asked to interrupt `turn` after it started: the journal holds the request.
fn interrupted(journal: &[LifecycleRecord], turn: &str) -> bool {
    journal.iter().rev()
        .take_while(|record| !matches!(record, LifecycleRecord::TurnStarted { turn: started, .. } | LifecycleRecord::TurnResumed { turn: started, .. } if started == turn))
        .any(|record| matches!(record, LifecycleRecord::ControlRequested { operation: ControlRecord::Interrupt { turn: Some(stopped) }, .. } if stopped == turn))
}

fn turn_ended_card(id: InteractionId, task: &TaskId, title: &str, end: TurnEnd, revision: u64) -> Interaction {
    let (need, why) = match end {
        TurnEnd::Refused(failure) => (format!("“{title}” stopped: {}", api_refusal(&failure.error)), REFUSED_WHY),
        TurnEnd::Interrupted => (format!("You interrupted “{title}”: tell it how to go on"), INTERRUPTED_WHY),
        TurnEnd::Unexplained => (format!("“{title}” stopped without delivering or asking: tell it how to go on"), TURN_ENDED_WHY),
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
                QuestionOption { id: CANCEL_TASK.into(), label: CANCEL_TASK_LABEL.into(), description: Some(CANCEL_TASK_MEANS.into()) },
            ],
            answer: None,
        },
        revision,
        order: revision,
        withdrawn: false,
        card: CardSpec::new(CardKind::Decide, need).why(why),
        clarification: None,
    }
}

/// The cards that ask the user how a task goes on after its SDK host ended mid-way.
pub const SESSION_ENDED_PREFIX: &str = "session-ended-";
/// The cards that ask the user about an input handed to a host that ended before reading it.
pub const HANDOFF_DOUBT_PREFIX: &str = "handoff-doubt-";
/// The cards of a task whose SDK execution could not be taken over: no host runs it.
pub const HOST_LOST_PREFIX: &str = "host-lost-";
pub const RETAKE: &str = "retake";
pub const RETAKE_LABEL: &str = "Try again";
const RETAKE_MEANS: &str = "Alondra starts its session again where it left off";
const HOST_RESUMED_INSTRUCTION: &str = "Your session was cut off when its host stopped, and Alondra resumed it. Check where you were and go on with your task: deliver your result, or ask the user through a card when you need them.";
const RESENT_NOTE: &str = "Your session was cut off when its host stopped, and Alondra resumed it. The message below was sent to you just before, and your conversation may already have it: if it does, do not act on it twice.";
pub const RESEND: &str = "resend";
pub const RESEND_LABEL: &str = "Send it again";
const RESEND_MEANS: &str = "It gets the message again; if it had read it, it sees it twice";
pub const TAKE_AS_RECEIVED: &str = "take_as_received";
pub const TAKE_AS_RECEIVED_LABEL: &str = "Take it as received";
const TAKE_AS_RECEIVED_MEANS: &str = "The message is not sent again and the task goes on";

fn session_ended_card_id(task: &TaskId, generation: u64, cursor: u64) -> InteractionId {
    InteractionId(format!("{SESSION_ENDED_PREFIX}{task}-{generation}-{cursor}"))
}
fn handoff_doubt_card_id(task: &TaskId, generation: u64, input: &str) -> InteractionId {
    let input: String = input.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).collect();
    InteractionId(format!("{HANDOFF_DOUBT_PREFIX}{task}-{generation}-{input}"))
}

/// Whether Alondra already resumed the task's session by itself after its host ended, and no turn
/// of it has ended since: resuming it once more would only end the same way. Read from the
/// journal's facts.
fn resumed_without_progress(journal: &[LifecycleRecord]) -> bool {
    journal
        .iter()
        .rev()
        .find_map(|record| match record {
            LifecycleRecord::TurnEnded { .. } => Some(false),
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
    let need = clip(&format!("“{title}” stopped: Alondra could not resume its session"), CardSpec::NEED_MAX);
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
        clarification: None,
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.trim().chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn session_ended_card(id: InteractionId, task: &TaskId, title: &str, why: &str, revision: u64) -> Interaction {
    let need = clip(&format!("“{title}” was interrupted when its session closed: tell it how to go on"), CardSpec::NEED_MAX);
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
        clarification: None,
    }
}
const SESSION_ENDED_WHY: &str = "Its session closed mid-way, as when the Mac shuts down. It resumes with all its context; whatever ran in the background stopped.";
const HOST_LOST_WHY: &str = "Its session could not be started again; the details say why. Try again once that is fixed, or cancel the task.";
pub const SESSION_ENDED_AGAIN_WHY: &str = "Alondra resumed its session after it closed, and it closed again before it could finish a turn. It resumes with all its context if you tell it to go on.";

fn handoff_doubt_card(id: InteractionId, task: &TaskId, title: &str, text: &str, launch: bool, revision: u64) -> Interaction {
    let need = clip(&format!("“{title}” may not have got your message: send it again?"), CardSpec::NEED_MAX);
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
        clarification: None,
    }
}
const HANDOFF_DOUBT_WHY: &str = "Its session closed right after the message was handed over and before it showed it had read it. Nothing is sent until you decide.";
const HANDOFF_DOUBT_LAUNCH_WHY: &str = "Its session closed before it started on the task, right after the message was handed over. Nothing is sent until you decide.";

fn refused(task: &TaskId, record: &LifecycleRecord, lifecycle: &TaskLifecycle, refusal: Refusal) -> DomainError {
    DomainError::new("conflict", format!(
        "The lifecycle of task {task} ({:?}) refuses {record:?}: {refusal:?}", lifecycle.phase()))
}
