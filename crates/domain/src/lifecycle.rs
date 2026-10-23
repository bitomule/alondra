mod commands;
mod observations;

use alondra_foundation::{AgentId, InteractionId, MAX_REVISION, Revision, TaskId};
pub use alondra_foundation::{
    CallbackId, ContinuationId, ConversationId, ExecutionStamp, HostId, IdentityError, InputId,
    InstanceId, OperationId, TurnId, WorkId,
};
use rpds::RedBlackTreeMapSync;
use std::collections::{BTreeMap, BTreeSet};

/// The host of every execution of the `claude --bg` backend: Alondra delivers its inputs itself,
/// so nothing dispatches to it through the SDK.
pub const CLAUDE_BG_HOST: &str = "claude-bg";

fn valid_id(value: &str) -> Result<(), Refusal> {
    if value.trim().is_empty() {
        Err(Refusal::EmptyIdentity)
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NonEmpty<T: Ord> {
    first: T,
    rest: BTreeSet<T>,
}
impl<T: Ord> NonEmpty<T> {
    fn new(values: impl IntoIterator<Item = T>) -> Result<Self, Refusal> {
        let mut values = values.into_iter().collect::<BTreeSet<_>>();
        let first = values.pop_first().ok_or(Refusal::EmptyObligation)?;
        Ok(Self {
            first,
            rest: values,
        })
    }
    fn iter(&self) -> impl Iterator<Item = &T> {
        std::iter::once(&self.first).chain(&self.rest)
    }
    fn contains(&self, value: &T) -> bool {
        &self.first == value || self.rest.contains(value)
    }
}
impl<T: Ord + Clone> NonEmpty<T> {
    fn values(&self) -> BTreeSet<T> {
        self.iter().cloned().collect()
    }
}

/// Inputs in the order they wait. Every copy of the aggregate shares their storage, so `apply`
/// copies a long queue for nothing, and each change or lookup costs the logarithm of its length.
#[derive(Clone)]
struct Inputs {
    order: RedBlackTreeMapSync<i64, InputId>,
    members: RedBlackTreeMapSync<InputId, usize>,
}
impl Inputs {
    fn new() -> Self {
        Self {
            order: RedBlackTreeMapSync::new_sync(),
            members: RedBlackTreeMapSync::new_sync(),
        }
    }
    fn one(input: InputId) -> Self {
        let mut inputs = Self::new();
        inputs.push_back(input);
        inputs
    }
    fn push_back(&mut self, input: InputId) {
        let next = self.order.last().map_or(0, |(position, _)| position + 1);
        self.enter(next, input);
    }
    fn push_front(&mut self, input: InputId) {
        let next = self.order.first().map_or(0, |(position, _)| position - 1);
        self.enter(next, input);
    }
    fn enter(&mut self, position: i64, input: InputId) {
        let count = self.members.get(&input).copied().unwrap_or(0);
        self.members.insert_mut(input.clone(), count + 1);
        self.order.insert_mut(position, input);
    }
    fn pop_front(&mut self) -> Option<InputId> {
        let (position, input) = self.order.first().map(|(position, input)| (*position, input.clone()))?;
        self.leave(position, &input);
        Some(input)
    }
    /// Puts `with` in the place of the first `input`, or takes `input` out without it: false if
    /// `input` does not wait here.
    fn replace(&mut self, input: &InputId, with: Option<InputId>) -> bool {
        if !self.contains(input) {
            return false;
        }
        let Some(position) = self.order.iter().find(|(_, held)| *held == input).map(|(position, _)| *position) else {
            return false;
        };
        self.leave(position, input);
        if let Some(with) = with {
            self.enter(position, with);
        }
        true
    }
    fn leave(&mut self, position: i64, input: &InputId) {
        self.order.remove_mut(&position);
        match self.members.get(input).copied() {
            Some(1) => {
                self.members.remove_mut(input);
            }
            Some(count) => self.members.insert_mut(input.clone(), count - 1),
            None => {}
        }
    }
    fn front(&self) -> Option<&InputId> {
        self.order.first().map(|(_, input)| input)
    }
    fn contains(&self, input: &InputId) -> bool {
        self.members.contains_key(input)
    }
    fn iter(&self) -> impl Iterator<Item = &InputId> {
        self.order.values()
    }
    fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}
impl FromIterator<InputId> for Inputs {
    fn from_iter<I: IntoIterator<Item = InputId>>(inputs: I) -> Self {
        let mut queue = Self::new();
        for input in inputs {
            queue.push_back(input);
        }
        queue
    }
}
impl PartialEq for Inputs {
    fn eq(&self, other: &Self) -> bool {
        self.order.size() == other.order.size() && self.iter().eq(other.iter())
    }
}
impl Eq for Inputs {}
impl std::fmt::Debug for Inputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkStage {
    Plan,
    Implement,
    ChangesRequested,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    Question,
    Permission,
    Pause,
    Recovery,
    Unheard,
    /// An input was handed to a host that ended before it showed the input was read: only the
    /// user settles it, by sending it again or taking it as received.
    Doubt,
    /// Claude will never read an input it was handed: the user sends it again or lets it go.
    Dropped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishIntent {
    Complete,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskPhase {
    Unstarted,
    InProgress,
    Delivered,
    Finishing,
    Completed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityView {
    None,
    Starting,
    Working,
    OwnWork,
    Continuation,
    Resting,
}

/// Card ids of the holds recorded before a pause named who it waits on. Only a hold on Claude's
/// API or on its usage limit waits on capacity; every other card waits on the user.
pub const CLAUDE_OUTAGE_CARD_PREFIX: &str = "claude-outage-";
pub const CLAUDE_USAGE_CARD_PREFIX: &str = "claude-usage-limit-";

/// Who a card waits on. Every card but a pause waits on the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    User,
    Capacity(Capacity),
}
impl Owner {
    /// Who a pause recorded before pauses named it waits on, read from the card it was recorded with.
    pub fn of_unnamed_pause(card: &InteractionId) -> Self {
        if card.0.starts_with(CLAUDE_OUTAGE_CARD_PREFIX) {
            Self::Capacity(Capacity::ClaudeApi)
        } else if card.0.starts_with(CLAUDE_USAGE_CARD_PREFIX) {
            Self::Capacity(Capacity::Usage)
        } else {
            Self::User
        }
    }
}

/// What Alondra holds a task back for until there is room: a place in the gate, the subscription's
/// usage, or Claude's API answering again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capacity {
    Gate,
    Usage,
    ClaudeApi,
}

/// Who does the work of a task that is working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Worker {
    /// The node's turn runs.
    Turn,
    /// Work the node started in the background runs, outside a turn.
    OwnWork,
    /// The node runs the work it declared for after its PR merged.
    AfterMerge,
    Alondra(Step),
}

/// What Alondra itself is doing for a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Starting,
    /// Handing the node an input it has yet to read.
    Delivering,
    /// Waking the node because work it waited on ended.
    Waking,
    /// The orchestrator answers what the node asked it.
    Answering,
    Merging,
    Finishing,
}

/// A set that holds at least one value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Several<T: Ord>(NonEmpty<T>);
impl<T: Ord> Several<T> {
    pub fn new(values: impl IntoIterator<Item = T>) -> Option<Self> {
        NonEmpty::new(values).ok().map(Self)
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.0.iter()
    }
    pub fn first(&self) -> &T {
        &self.0.first
    }
}

/// What a live task is doing, and who it waits on when it waits. Every live task is in exactly
/// one of these; none says that nobody owns its next step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveState {
    Working(Worker),
    WaitingUser(Several<InteractionId>),
    WaitingCapacity(Capacity),
    WaitingTask(Several<TaskId>),
}

/// How far stopping the root turn went. Only a turn that stays open after Claude accepted its
/// interrupt can be force-stopped, once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnStop {
    Running,
    InterruptAccepted,
    ForceRequested,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceEvidence {
    Reserved,
    Available,
    ObservationPending,
    Released,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    EmptyIdentity,
    EmptyObligation,
    InvalidState,
    StaleRevision,
    RevisionExhausted,
    GenerationExhausted,
    StaleExecution,
    CursorGap,
    DuplicateFact,
    CorrelationMismatch,
    DuplicateObligation,
    UnknownObligation,
    SelfDependency,
    /// A decision of the user holds the task until the orchestrator reviews its plan.
    PlanReviewPending,
    /// Work after the merge ends only with the fact that ended it, never as a bare drain.
    DrainWithoutFact,
    /// A model name that is empty, blank, longer than 128 bytes or carries control characters.
    InvalidModel,
    /// A decision that answers nothing: no answers, a denial without its message, or one that
    /// does not answer what its card asks.
    InvalidDecision,
    /// An MCP server name that is empty, blank, longer than 256 bytes or carries control characters.
    InvalidMcpServer,
}

impl From<IdentityError> for Refusal {
    fn from(error: IdentityError) -> Self {
        match error {
            IdentityError::EmptyIdentity => Self::EmptyIdentity,
            IdentityError::InvalidGeneration => Self::InvalidState,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    Input(InputId),
    OwnedContinuation(ContinuationId),
    /// Claude started the turn by itself, with nothing the task gave it or knows of: a message a
    /// subagent handed back after the last turn ended.
    Unprompted,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DrainObligation {
    Execution(InstanceId),
    Work(WorkId),
    Callback(CallbackId),
    Merge(OperationId),
    Cleanup(OperationId),
    ExternalEffect(OperationId),
    PostMerge(OperationId),
}

/// What ended the work a node declared it runs after its PR merged. Time is not one of them: a
/// clock may schedule a question, never end the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostMergeFact {
    /// The node said the work ended.
    NodeDeclaredEnd,
    /// The node confirmed its cleanup.
    Cleanup,
    /// The user chose to close the node on the card that asks about the work.
    UserDecision,
    /// The node's session is over, so nothing it ran is left.
    SessionOver,
    /// After the merge, a turn the node began after declaring the work ended with nothing of its
    /// own running, and the node did not declare the work again in it.
    TurnEnded,
}

/// How the plan review of a held task was settled. `Cancelled` only releases the hold: the
/// cancellation itself is the `Finish` that follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanReviewOutcome {
    Unchanged,
    PlanChanged,
    Cancelled,
    KeptByUser,
}

/// The permission modes an execution can be set to. None grants a permission by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionMode {
    Default,
    Plan,
    DontAsk,
}

/// An operation on the running execution itself. `Interrupt` names the root turn it interrupts,
/// or none when only the execution's own work runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    Interrupt { turn: Option<TurnId> },
    /// Claude's process is ended: only for a root turn that stayed open after Claude accepted its
    /// interrupt.
    ForceStop { turn: TurnId },
    SetModel { model: String },
    SetPermissionMode { mode: PermissionMode },
    ReadMcpStatus,
    /// Claude connects the MCP server again, reading its stored sign-in anew.
    ReconnectMcp { server: String },
    /// Claude opens a sign-in to the MCP server; the user finishes it in a browser.
    StartMcpSignIn { server: String },
    /// Claude forgets its stored sign-in to the MCP server, which it revokes.
    ClearMcpSignIn { server: String },
}

/// What an SDK decision of the execution is answered with. Nothing is granted by default: a
/// request goes on only with `Allow`, `Answers` or `Accept`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Claude's questions answered, one answer for each, in the order it asked them.
    Answers { answers: Vec<String> },
    Deny { message: String },
    /// An MCP server's request for input accepted with what the user wrote.
    Accept { text: String },
    Decline,
}

fn valid_decision(decision: &Decision) -> Result<(), Refusal> {
    match decision {
        Decision::Answers { answers } if answers.is_empty() => Err(Refusal::InvalidDecision),
        Decision::Deny { message } if message.trim().is_empty() => Err(Refusal::InvalidDecision),
        _ => Ok(()),
    }
}

/// Whether `decision` answers a card that waits for `reason`: a permission or a plan is allowed
/// or denied; a question or an MCP server's request is answered, accepted, declined or denied.
fn decides(reason: WaitReason, decision: &Decision) -> bool {
    match decision {
        Decision::Allow => reason == WaitReason::Permission,
        Decision::Answers { .. } | Decision::Accept { .. } | Decision::Decline => reason == WaitReason::Question,
        Decision::Deny { .. } => matches!(reason, WaitReason::Permission | WaitReason::Question),
    }
}

/// The callback of the SDK decision its host persisted at `epoch`, in the execution of `generation`.
pub fn decision_callback(generation: u64, epoch: u64) -> CallbackId {
    CallbackId::new(format!("decision-{generation}-{epoch}")).expect("a decision callback is never empty")
}
/// The continuation the turn of `generation` resumes with once the decision at `epoch` settled.
pub fn decision_continuation(generation: u64, epoch: u64) -> ContinuationId {
    ContinuationId::new(format!("decision-{generation}-{epoch}")).expect("a decision continuation is never empty")
}
/// The epoch of a decision callback of the execution of `generation`.
pub fn decision_epoch(generation: u64, callback: &CallbackId) -> Option<u64> {
    let epoch = callback.as_str().strip_prefix(&format!("decision-{generation}-"))?;
    let parsed = epoch.parse::<u64>().ok()?;
    (parsed.to_string() == epoch).then_some(parsed)
}

/// Where a callback of the current execution stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackView {
    AwaitingAnswer,
    Answered,
}

fn valid_mcp_server(server: &str) -> Result<(), Refusal> {
    if server.trim().is_empty() || server.len() > 256 || server.chars().any(char::is_control) {
        Err(Refusal::InvalidMcpServer)
    } else {
        Ok(())
    }
}

fn valid_model(model: &str) -> Result<(), Refusal> {
    if model.trim().is_empty() || model.len() > 128 || model.chars().any(char::is_control) {
        Err(Refusal::InvalidModel)
    } else {
        Ok(())
    }
}

/// Requests and observations are validated by the aggregate, not assigned as state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Admit {
        node: AgentId,
        conversation: ConversationId,
        instance: InstanceId,
        host: HostId,
    },
    Runtime {
        stamp: ExecutionStamp,
        cursor: u64,
        fact: RuntimeFact,
    },
    Ask {
        card: InteractionId,
    },
    /// The user answered a card that waits for no callback: the task goes on with `input`.
    Answer {
        card: InteractionId,
        input: InputId,
    },
    /// The user decided the card of a callback the execution waits on. Nothing is queued: the
    /// execution resumes once its callback settles.
    Decide {
        card: InteractionId,
        decision: Decision,
    },
    /// The user answered `from`, the card a callback still awaiting its answer is held by, while
    /// the decision needs more: `to` holds the callback instead, for the same reason.
    MoveCard {
        from: InteractionId,
        to: InteractionId,
    },
    PlanApproved,
    BlockDelivery {
        delivery: InteractionId,
        dependencies: Vec<TaskId>,
    },
    DeliveryUnblocked {
        delivery: InteractionId,
    },
    Present {
        delivery: InteractionId,
    },
    Accept {
        delivery: InteractionId,
        merge: Option<OperationId>,
    },
    MergeObserved {
        operation: OperationId,
    },
    HandBack {
        delivery: InteractionId,
        input: InputId,
    },
    WaitForDependencies {
        dependencies: Vec<TaskId>,
    },
    /// Someone tells the task's node something outside a card: the input reaches its execution
    /// as it is, inside the turn that runs or as a turn of its own, and waits for the host that
    /// resumes the session when the execution has none.
    Tell {
        input: InputId,
    },
    DependenciesResolved {
        input: InputId,
    },
    Pause {
        card: InteractionId,
        owner: Owner,
    },
    Resume {
        card: InteractionId,
        input: InputId,
    },
    Finish {
        intent: FinishIntent,
        cleanup: OperationId,
        post_merge: Option<OperationId>,
    },
    DrainObserved {
        obligation: DrainObligation,
    },
    /// The node said it runs work after its PR merges, under `operation`.
    PostMergeDeclared {
        operation: OperationId,
    },
    /// The work after the merge under `operation` ended, with the fact that ended it.
    PostMergeEnded {
        operation: OperationId,
        fact: PostMergeFact,
    },
    RetireReleasedExecution {
        problem_card: InteractionId,
        input: InputId,
    },
    /// The execution is gone and left nothing behind, and no card tells the user: the task waits
    /// for its next execution with `input`, or with what it had not consumed. The silent form of
    /// `RetireReleasedExecution`, for a scheduler that queues the task again on its own; an execution
    /// that still owns work, callbacks or accepted effects cannot be released this way.
    ExecutionReleased {
        input: InputId,
    },
    Reopen {
        input: InputId,
    },
    /// The user answered `review`, a decision on a task this one waits for, before it started.
    HoldForPlanReview {
        review: InteractionId,
    },
    /// The review of the plan at `plan_revision` the hold `review` asked for.
    PlanReviewSettled {
        review: InteractionId,
        plan_revision: u64,
        outcome: PlanReviewOutcome,
    },
    /// `input` is pending, but it was handed to a host that ended before showing it was read:
    /// the task waits on `card` for the user to settle it.
    DoubtHandoff {
        card: InteractionId,
        input: InputId,
    },
    /// `input` provably never reached the ended host (it never took its command, or refused it
    /// before offering it): `resend`, with the same text, takes its place for the next host.
    ResendUntaken {
        input: InputId,
        resend: InputId,
    },
    /// `input` was handed to a host that ended before it showed the input was read, and a host that
    /// resumes the same session took its place: `resend` takes its place without asking anyone, and
    /// `input` is settled.
    ResendDoubtful {
        input: InputId,
        resend: InputId,
    },
    /// The user settled the doubtful input of `card`: `resend` takes its place, or it is taken as
    /// received and leaves the queue without being consumed. When nothing is left to explain the
    /// task's next step, it waits on `unheard_card`.
    SettleHandoff {
        card: InteractionId,
        resend: Option<InputId>,
        unheard_card: InteractionId,
    },
    /// The user let go of the input the `Dropped` card asked about. When nothing is left to explain
    /// the task's next step, it waits on `unheard_card`.
    Discard {
        card: InteractionId,
        unheard_card: InteractionId,
    },
    /// The intent to run `kind` on the current execution, recorded before anything reaches it.
    RequestControl {
        control: OperationId,
        kind: Control,
    },
    /// A resting execution waiting for the user is asked to compact its conversation with `input`;
    /// the cards it waits on stay.
    Compact {
        input: InputId,
    },
}

impl Event {
    /// The inputs the event names: an input only becomes pending through an event that names it.
    pub fn inputs(&self) -> Vec<&InputId> {
        match self {
            Self::Answer { input, .. }
            | Self::HandBack { input, .. }
            | Self::Tell { input }
            | Self::DependenciesResolved { input }
            | Self::Resume { input, .. }
            | Self::RetireReleasedExecution { input, .. }
            | Self::ExecutionReleased { input }
            | Self::Reopen { input }
            | Self::DoubtHandoff { input, .. }
            | Self::Compact { input }
            | Self::Runtime {
                fact: RuntimeFact::TurnStarted { trigger: Trigger::Input(input), .. }
                    | RuntimeFact::InputConsumed { input }
                    | RuntimeFact::InputDropped { input, .. }
                    | RuntimeFact::HostEnded { resume: Some(input), .. },
                ..
            } => vec![input],
            Self::ResendUntaken { input, resend } | Self::ResendDoubtful { input, resend } => vec![input, resend],
            Self::SettleHandoff { resend, .. } => resend.iter().collect(),
            Self::Admit { .. }
            | Self::Runtime { .. }
            | Self::Ask { .. }
            | Self::Decide { .. }
            | Self::MoveCard { .. }
            | Self::PlanApproved
            | Self::BlockDelivery { .. }
            | Self::DeliveryUnblocked { .. }
            | Self::Present { .. }
            | Self::Accept { .. }
            | Self::MergeObserved { .. }
            | Self::WaitForDependencies { .. }
            | Self::Pause { .. }
            | Self::Finish { .. }
            | Self::DrainObserved { .. }
            | Self::PostMergeDeclared { .. }
            | Self::PostMergeEnded { .. }
            | Self::HoldForPlanReview { .. }
            | Self::PlanReviewSettled { .. }
            | Self::Discard { .. }
            | Self::RequestControl { .. } => vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeFact {
    Ask {
        card: InteractionId,
        callback: CallbackId,
        reason: WaitReason,
    },
    ResourceAvailable,
    TurnStarted {
        turn: TurnId,
        trigger: Trigger,
    },
    InputConsumed {
        input: InputId,
    },
    /// Claude will never read `input`, still pending: it leaves the queue and `card` asks the user
    /// whether to send it again.
    InputDropped {
        input: InputId,
        card: InteractionId,
    },
    WorkStarted {
        turn: TurnId,
        work: WorkId,
    },
    WorkEnded {
        work: WorkId,
        continuation: ContinuationId,
    },
    ContinuationConsumed {
        continuation: ContinuationId,
    },
    TurnEnded {
        turn: TurnId,
        unheard_card: InteractionId,
    },
    ConnectionLost {
        problem_card: InteractionId,
    },
    ResourceReleased {
        problem_card: InteractionId,
    },
    CallbackSettled {
        callback: CallbackId,
        continuation: ContinuationId,
    },
    CallbackAborted {
        callback: CallbackId,
        unheard_card: InteractionId,
    },
    /// A callback Alondra's own rules decided as it was asked: it opens already answered, with no
    /// card for the user.
    AnsweredByPolicy {
        callback: CallbackId,
        decision: Decision,
    },
    EffectAccepted {
        operation: OperationId,
    },
    /// Claude accepted the interrupt of the root `turn`, which is still open.
    InterruptAccepted {
        turn: TurnId,
    },
    EffectObserved {
        operation: OperationId,
    },
    Notification,
    /// The execution's host ended (no process holds its lock any more), and its native Claude
    /// with it: the turn, the work and the questions it had open ended too. When that leaves
    /// nothing to explain the task's next step, a host that resumes the same session goes on with
    /// `resume` when there is one, and the task waits on `unheard_card` otherwise.
    HostEnded {
        unheard_card: InteractionId,
        resume: Option<InputId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Card {
    id: InteractionId,
    reason: WaitReason,
    owner: Owner,
    callback: Option<CallbackId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Decisions {
    first: Card,
    others: BTreeMap<InteractionId, Card>,
}
impl Decisions {
    fn one(card: Card) -> Self {
        Self {
            first: card,
            others: BTreeMap::new(),
        }
    }
    fn new(mut cards: BTreeMap<InteractionId, Card>) -> Result<Self, Refusal> {
        let (_, first) = cards.pop_first().ok_or(Refusal::EmptyObligation)?;
        Ok(Self {
            first,
            others: cards,
        })
    }
    fn iter(&self) -> impl Iterator<Item = &Card> {
        std::iter::once(&self.first).chain(self.others.values())
    }
    fn map(&self) -> BTreeMap<InteractionId, Card> {
        self.iter()
            .map(|card| (card.id.clone(), card.clone()))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CallbackState {
    AwaitingAnswer(CallbackId),
    Answered(CallbackId),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Wake {
    Owned(ContinuationId),
    Callback(CallbackId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Activity {
    Launching(InputId),
    InputPending(InputId),
    Turn {
        id: TurnId,
        work: BTreeSet<WorkId>,
        continuations: BTreeSet<Wake>,
        stop: TurnStop,
    },
    OwnWork {
        work: NonEmpty<WorkId>,
        continuations: BTreeSet<Wake>,
    },
    Continuation(NonEmpty<Wake>),
    Resting,
}
impl Activity {
    fn view(&self) -> ActivityView {
        match self {
            Self::Launching(_) | Self::InputPending(_) => ActivityView::Starting,
            Self::Turn { .. } => ActivityView::Working,
            Self::OwnWork { .. } => ActivityView::OwnWork,
            Self::Continuation(_) => ActivityView::Continuation,
            Self::Resting => ActivityView::Resting,
        }
    }
    fn owns_work(&self) -> bool {
        match self {
            Self::OwnWork { .. } => true,
            Self::Turn { work, .. } => !work.is_empty(),
            _ => false,
        }
    }
    fn work(&self) -> BTreeSet<WorkId> {
        match self {
            Self::Turn { work, .. } => work.clone(),
            Self::OwnWork { work, .. } => work.values(),
            _ => BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Execution {
    stamp: ExecutionStamp,
    cursor: u64,
    resource: ResourceEvidence,
    activity: Activity,
    callbacks: Vec<CallbackState>,
    effects: BTreeSet<OperationId>,
    inbox: Inputs,
}
impl Execution {
    fn holds_slot(&self) -> bool {
        matches!(
            self.resource,
            ResourceEvidence::Reserved | ResourceEvidence::ObservationPending
        ) || self.activity.owns_work()
            || (self.resource == ResourceEvidence::Available
                && !matches!(self.activity, Activity::Resting))
    }
}

// Startup has a reservation; subsequent activity requires an observed running execution.
impl Execution {
    /// Who works for an execution that is not resting.
    fn worker(&self) -> Option<Worker> {
        match &self.activity {
            Activity::Launching(_) => Some(Worker::Alondra(Step::Starting)),
            Activity::InputPending(_) => Some(Worker::Alondra(Step::Delivering)),
            Activity::Turn { .. } => Some(Worker::Turn),
            Activity::OwnWork { .. } => Some(Worker::OwnWork),
            Activity::Continuation(_) => Some(Worker::Alondra(Step::Waking)),
            Activity::Resting => None,
        }
    }
}

/// The user, when a card waits on them; else the capacity a pause waits on.
fn waits_on<'a>(cards: impl Iterator<Item = &'a Card>) -> Option<LiveState> {
    let mut capacity = None;
    let mut users = vec![];
    for card in cards {
        match card.owner {
            Owner::User => users.push(card.id.clone()),
            Owner::Capacity(held) => capacity = Some(capacity.map_or(held, |kept: Capacity| kept.min(held))),
        }
    }
    Several::new(users).map(LiveState::WaitingUser).or(capacity.map(LiveState::WaitingCapacity))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveExecution(Execution);
impl ActiveExecution {
    fn new(execution: Execution) -> Result<Self, Refusal> {
        let reserved = execution.resource == ResourceEvidence::Reserved
            && matches!(execution.activity, Activity::Launching(_));
        if !reserved
            && (execution.resource != ResourceEvidence::Available
                || matches!(execution.activity, Activity::Resting))
        {
            return Err(Refusal::InvalidState);
        }
        Ok(Self(execution))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Session {
    Absent(Inputs),
    Present(Execution),
}
impl Session {
    fn execution(&self) -> Option<&Execution> {
        match self {
            Self::Absent(_) => None,
            Self::Present(execution) => Some(execution),
        }
    }
    fn execution_mut(&mut self) -> Option<&mut Execution> {
        match self {
            Self::Absent(_) => None,
            Self::Present(execution) => Some(execution),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Progress {
    Queued {
        stage: WorkStage,
        input: InputId,
        remaining: Inputs,
    },
    Executing {
        stage: WorkStage,
        execution: ActiveExecution,
    },
    Waiting {
        stage: WorkStage,
        cards: Decisions,
        session: Session,
    },
    Dependency {
        stage: WorkStage,
        tasks: NonEmpty<TaskId>,
        decisions: BTreeMap<InteractionId, Card>,
        session: Session,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DeliveryGate {
    Review,
    Blocked(NonEmpty<TaskId>),
    AwaitingMerge(OperationId),
    Accepted,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Delivered {
    delivery: InteractionId,
    gate: DeliveryGate,
    decisions: BTreeMap<InteractionId, Card>,
    session: Session,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Finishing {
    intent: FinishIntent,
    remaining: NonEmpty<DrainObligation>,
    session: Session,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Unstarted(InputId),
    InProgress(Progress),
    Delivered(Delivered),
    Finishing(Finishing),
    Completed,
    Cancelled,
}

/// The aggregate cannot be deserialized, cloned, or independently edited as task/node phases.
/// Cross-record card ownership and commit/dispatch fencing are responsibilities of the store port.
#[derive(Debug, PartialEq, Eq)]
/// ```compile_fail,E0616
/// use alondra_domain::TaskLifecycle;
/// use alondra_foundation::TaskId;
/// use alondra_domain::InputId;
/// let mut task = TaskLifecycle::planned(TaskId::from("task"), InputId::new("initial").unwrap()).unwrap();
/// task.revision = 0;
/// ```
///
/// ```compile_fail,E0277
/// use alondra_domain::TaskLifecycle;
/// fn duplicate<T: Clone>() {}
/// duplicate::<TaskLifecycle>();
/// ```
pub struct TaskLifecycle {
    task: TaskId,
    revision: Revision,
    generation: u64,
    state: State,
    /// Which launch plan the task would start with; a reviewed new plan moves it on.
    plan_revision: u64,
    /// Decisions holding the task, each with the plan revision it held. Kept apart from `state`,
    /// so no phase change, dependency or reopening releases one: only its settlement does.
    plan_reviews: BTreeMap<InteractionId, u64>,
    /// Work the node declared it runs after the merge, until its fact ends it. Kept apart from
    /// `state` until the task finishes, when it becomes one of the obligations the ending owes.
    post_merge: Option<OperationId>,
    /// The doubtful inputs, by the card that holds each one until the user settles it.
    doubts: BTreeMap<InteractionId, InputId>,
    /// Inputs the user settled (sent again, taken as received, or dropped with the task), whose
    /// uncertain handoff no longer holds the task.
    settled: BTreeSet<InputId>,
}

impl TaskLifecycle {
    pub fn planned(task: TaskId, input: InputId) -> Result<Self, Refusal> {
        valid_id(&task.0)?;
        Ok(Self {
            task,
            revision: 0,
            generation: 0,
            state: State::Unstarted(input),
            plan_revision: 0,
            plan_reviews: BTreeMap::new(),
            post_merge: None,
            doubts: BTreeMap::new(),
            settled: BTreeSet::new(),
        })
    }
    pub fn task(&self) -> &TaskId {
        &self.task
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    /// How many executions the task has had: the next one is admitted with the one after.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn plan_revision(&self) -> u64 {
        self.plan_revision
    }
    /// The decisions holding the task, with the plan revision each held.
    pub fn plan_reviews(&self) -> Vec<(&InteractionId, u64)> {
        self.plan_reviews.iter().map(|(review, revision)| (review, *revision)).collect()
    }
    pub fn phase(&self) -> TaskPhase {
        match self.state {
            State::Unstarted(_) => TaskPhase::Unstarted,
            State::InProgress(_) => TaskPhase::InProgress,
            State::Delivered(_) => TaskPhase::Delivered,
            State::Finishing(_) => TaskPhase::Finishing,
            State::Completed => TaskPhase::Completed,
            State::Cancelled => TaskPhase::Cancelled,
        }
    }
    fn session(&self) -> Option<&Execution> {
        match &self.state {
            State::InProgress(Progress::Executing { execution, .. }) => Some(&execution.0),
            State::InProgress(
                Progress::Waiting { session, .. } | Progress::Dependency { session, .. },
            ) => session.execution(),
            State::Delivered(delivered) => delivered.session.execution(),
            State::Finishing(finishing) => finishing.session.execution(),
            State::Unstarted(_)
            | State::InProgress(Progress::Queued { .. })
            | State::Completed
            | State::Cancelled => None,
        }
    }
    fn session_mut(&mut self) -> Option<&mut Execution> {
        match &mut self.state {
            State::InProgress(Progress::Executing { execution, .. }) => Some(&mut execution.0),
            State::InProgress(
                Progress::Waiting { session, .. } | Progress::Dependency { session, .. },
            ) => session.execution_mut(),
            State::Delivered(delivered) => delivered.session.execution_mut(),
            State::Finishing(finishing) => finishing.session.execution_mut(),
            State::Unstarted(_)
            | State::InProgress(Progress::Queued { .. })
            | State::Completed
            | State::Cancelled => None,
        }
    }
    pub fn execution(&self) -> Option<&ExecutionStamp> {
        self.session().map(|execution| &execution.stamp)
    }
    pub fn activity(&self) -> ActivityView {
        self.session().map_or(ActivityView::None, |execution| {
            if execution.resource == ResourceEvidence::Released {
                if execution.activity.owns_work() {
                    ActivityView::OwnWork
                } else {
                    ActivityView::Resting
                }
            } else {
                execution.activity.view()
            }
        })
    }
    /// The root turn the execution is running, if it is in one.
    pub fn turn(&self) -> Option<&TurnId> {
        match &self.session()?.activity {
            Activity::Turn { id, .. } => Some(id),
            _ => None,
        }
    }
    /// How far stopping the root turn the execution runs went, if it is in one.
    pub fn turn_stop(&self) -> Option<TurnStop> {
        match &self.session()?.activity {
            Activity::Turn { stop, .. } => Some(*stop),
            _ => None,
        }
    }
    /// Where the callback `id` of the current execution stands, if it waits on it.
    pub fn callback(&self, id: &CallbackId) -> Option<CallbackView> {
        self.session()?.callbacks.iter().find_map(|state| match state {
            CallbackState::AwaitingAnswer(callback) if callback == id => Some(CallbackView::AwaitingAnswer),
            CallbackState::Answered(callback) if callback == id => Some(CallbackView::Answered),
            _ => None,
        })
    }
    /// The callback a card the task holds decides, if it decides one.
    pub fn card_callback(&self, card: &InteractionId) -> Option<&CallbackId> {
        let held = match &self.state {
            State::InProgress(Progress::Waiting { cards, .. }) => cards.iter().find(|held| &held.id == card),
            State::InProgress(Progress::Dependency { decisions, .. }) => decisions.get(card),
            State::Delivered(delivered) => delivered.decisions.get(card),
            _ => None,
        };
        held?.callback.as_ref()
    }
    pub fn cursor(&self) -> Option<u64> {
        self.session().map(|execution| execution.cursor)
    }
    pub fn resource(&self) -> Option<ResourceEvidence> {
        self.session().map(|execution| execution.resource)
    }
    pub fn holds_slot(&self) -> bool {
        if let State::Finishing(finishing) = &self.state {
            return finishing.remaining.iter().any(|obligation| {
                matches!(
                    obligation,
                    DrainObligation::Execution(_)
                        | DrainObligation::Work(_)
                        | DrainObligation::PostMerge(_)
                )
            });
        }
        self.session().is_some_and(Execution::holds_slot)
    }
    pub fn cards(&self) -> Vec<(&InteractionId, WaitReason)> {
        match &self.state {
            State::InProgress(Progress::Waiting { cards, .. }) => {
                cards.iter().map(|card| (&card.id, card.reason)).collect()
            }
            State::Delivered(delivered) => delivered
                .decisions
                .values()
                .map(|card| (&card.id, card.reason))
                .collect(),
            State::InProgress(Progress::Dependency { decisions, .. }) => decisions
                .values()
                .map(|card| (&card.id, card.reason))
                .collect(),
            _ => vec![],
        }
    }
    /// What the task is doing, or who it waits on. `None` only before it starts and once it ended.
    pub fn live(&self) -> Option<LiveState> {
        match &self.state {
            State::Unstarted(_) | State::Completed | State::Cancelled => None,
            State::InProgress(Progress::Queued { .. }) => Some(LiveState::WaitingCapacity(Capacity::Gate)),
            State::InProgress(Progress::Executing { execution, .. }) => execution.0.worker().map(LiveState::Working),
            State::InProgress(Progress::Waiting { cards, .. }) => waits_on(cards.iter()),
            State::InProgress(Progress::Dependency { tasks, decisions, .. }) => {
                waits_on(decisions.values()).or_else(|| Several::new(tasks.iter().cloned()).map(LiveState::WaitingTask))
            }
            State::Delivered(delivered) => match &delivered.gate {
                DeliveryGate::Review => {
                    let users = delivered.decisions.values().filter(|card| card.owner == Owner::User).map(|card| card.id.clone());
                    Several::new(std::iter::once(delivered.delivery.clone()).chain(users)).map(LiveState::WaitingUser)
                }
                gate => waits_on(delivered.decisions.values()).or_else(|| match gate {
                    DeliveryGate::Blocked(tasks) => Several::new(tasks.iter().cloned()).map(LiveState::WaitingTask),
                    DeliveryGate::AwaitingMerge(_) => Some(LiveState::Working(Worker::Alondra(Step::Merging))),
                    DeliveryGate::Accepted | DeliveryGate::Review => Some(LiveState::Working(Worker::Alondra(Step::Finishing))),
                }),
            },
            State::Finishing(finishing) => Some(LiveState::Working(
                if finishing.session.execution().is_some_and(|execution| matches!(execution.activity, Activity::Turn { .. })) {
                    Worker::Turn
                } else if finishing.remaining.iter().any(|obligation| matches!(obligation, DrainObligation::PostMerge(_))) {
                    Worker::AfterMerge
                } else if finishing.remaining.iter().any(|obligation| matches!(obligation, DrainObligation::Work(_))) {
                    Worker::OwnWork
                } else {
                    Worker::Alondra(Step::Finishing)
                },
            )),
        }
    }
    pub fn stage(&self) -> Option<WorkStage> {
        match &self.state {
            State::Unstarted(_) => Some(WorkStage::Plan),
            State::InProgress(
                Progress::Queued { stage, .. }
                | Progress::Executing { stage, .. }
                | Progress::Waiting { stage, .. }
                | Progress::Dependency { stage, .. },
            ) => Some(*stage),
            _ => None,
        }
    }
    pub fn dependencies(&self) -> Vec<&TaskId> {
        match &self.state {
            State::InProgress(Progress::Dependency { tasks, .. }) => tasks.iter().collect(),
            State::Delivered(Delivered {
                gate: DeliveryGate::Blocked(tasks),
                ..
            }) => tasks.iter().collect(),
            _ => vec![],
        }
    }
    pub fn pending_inputs(&self) -> Vec<&InputId> {
        let (first, queue) = self.waiting();
        first.into_iter().chain(queue.into_iter().flat_map(Inputs::iter)).collect()
    }
    /// Whether `input` is one of `pending_inputs`, without listing them.
    pub fn is_pending(&self, input: &InputId) -> bool {
        let (first, queue) = self.waiting();
        first == Some(input) || queue.is_some_and(|queue| queue.contains(input))
    }
    fn has_pending(&self) -> bool {
        let (first, queue) = self.waiting();
        first.is_some() || queue.is_some_and(|queue| !queue.is_empty())
    }
    /// The pending inputs: the one the task starts or resumes with, then the queue behind it.
    fn waiting(&self) -> (Option<&InputId>, Option<&Inputs>) {
        let session = match &self.state {
            State::Unstarted(input) => return (Some(input), None),
            State::InProgress(Progress::Queued {
                input, remaining, ..
            }) => return (Some(input), Some(remaining)),
            State::InProgress(
                Progress::Waiting { session, .. } | Progress::Dependency { session, .. },
            ) => session,
            State::Delivered(delivered) => &delivered.session,
            State::Finishing(finishing) => &finishing.session,
            State::InProgress(Progress::Executing { execution, .. }) => {
                return execution_waiting(&execution.0);
            }
            State::Completed | State::Cancelled => return (None, None),
        };
        match session {
            Session::Absent(pending) => (None, Some(pending)),
            Session::Present(execution) => execution_waiting(execution),
        }
    }
    pub fn delivery(&self) -> Option<&InteractionId> {
        match &self.state {
            State::Delivered(delivered) => Some(&delivered.delivery),
            _ => None,
        }
    }
    /// The user accepted the standing delivery and nothing of it waits for a merge.
    pub fn accepted(&self) -> bool {
        matches!(&self.state, State::Delivered(delivered) if delivered.gate == DeliveryGate::Accepted)
    }
    /// What a finishing task ends as, once what it owes is drained.
    pub fn finishing_intent(&self) -> Option<FinishIntent> {
        match &self.state {
            State::Finishing(finishing) => Some(finishing.intent),
            _ => None,
        }
    }
    /// The work after the merge that still holds the task, declared or owed by its ending.
    pub fn post_merge(&self) -> Option<&OperationId> {
        self.post_merge.as_ref().or_else(|| {
            self.drain().find_map(|obligation| match obligation {
                DrainObligation::PostMerge(operation) => Some(operation),
                _ => None,
            })
        })
    }
    /// The doubtful inputs, each with the card that holds it.
    pub fn doubts(&self) -> Vec<(&InteractionId, &InputId)> {
        self.doubts.iter().collect()
    }
    /// Whether the user settled `input`, so an uncertain handoff of it no longer holds the task.
    pub fn settled(&self, input: &InputId) -> bool {
        self.settled.contains(input)
    }
    pub fn drain(&self) -> impl Iterator<Item = &DrainObligation> {
        let obligations = match &self.state {
            State::Finishing(finishing) => Some(&finishing.remaining),
            _ => None,
        };
        obligations.into_iter().flat_map(NonEmpty::iter)
    }

    /// Rejection leaves the entire aggregate, including the runtime cursor, unchanged.
    pub fn apply(&mut self, expected_revision: Revision, event: Event) -> Result<(), Refusal> {
        *self = self.advanced(expected_revision, event)?;
        Ok(())
    }

    /// The aggregate `event` leads to, leaving this one as it was for whoever still reads it.
    pub fn advanced(&self, expected_revision: Revision, event: Event) -> Result<Self, Refusal> {
        if expected_revision != self.revision {
            return Err(Refusal::StaleRevision);
        }
        crate::measure::event_applied();
        if self.revision >= MAX_REVISION {
            return Err(Refusal::RevisionExhausted);
        }
        let mut candidate = Self {
            task: self.task.clone(),
            revision: self.revision,
            generation: self.generation,
            state: self.state.clone(),
            plan_revision: self.plan_revision,
            plan_reviews: self.plan_reviews.clone(),
            post_merge: self.post_merge.clone(),
            doubts: self.doubts.clone(),
            settled: self.settled.clone(),
        };
        candidate.transition(event)?;
        candidate.revision += 1;
        Ok(candidate)
    }
}

fn callback_id(state: &CallbackState) -> &CallbackId {
    match state {
        CallbackState::AwaitingAnswer(id) | CallbackState::Answered(id) => id,
    }
}

/// The task goes on with `input` while `decisions` still wait: the input reaches the session, or
/// waits in its inbox behind a pause, and the task waits on the cards.
fn waiting_with_input(
    mut session: Session,
    stage: WorkStage,
    decisions: BTreeMap<InteractionId, Card>,
    input: InputId,
) -> Result<State, Refusal> {
    match &mut session {
        Session::Absent(pending) => {
            if pending.contains(&input) {
                return Err(Refusal::DuplicateObligation);
            }
            pending.push_back(input);
        }
        Session::Present(execution) => {
            if execution.resource != ResourceEvidence::Available {
                return Err(Refusal::InvalidState);
            }
            if decisions.values().any(|card| card.reason == WaitReason::Pause) {
                if execution.inbox.contains(&input) {
                    return Err(Refusal::DuplicateObligation);
                }
                execution.inbox.push_back(input);
            } else {
                queue_input(execution, input)?;
            }
        }
    }
    Ok(State::InProgress(Progress::Waiting { stage, cards: Decisions::new(decisions)?, session }))
}

fn progress_with_input(
    mut session: Session,
    stage: WorkStage,
    input: InputId,
) -> Result<State, Refusal> {
    match &mut session {
        Session::Absent(pending) => {
            if pending.contains(&input) {
                return Err(Refusal::DuplicateObligation);
            }
            pending.push_back(input);
            let first = pending.pop_front().ok_or(Refusal::EmptyObligation)?;
            Ok(State::InProgress(Progress::Queued {
                stage,
                input: first,
                remaining: pending.clone(),
            }))
        }
        Session::Present(execution) => {
            if execution.resource != ResourceEvidence::Available {
                return Err(Refusal::InvalidState);
            }
            queue_input(execution, input)?;
            Ok(State::InProgress(Progress::Executing {
                stage,
                execution: ActiveExecution::new(execution.clone())?,
            }))
        }
    }
}

fn tell(session: &mut Session, input: InputId) -> Result<(), Refusal> {
    match session {
        Session::Present(execution) => queue_input(execution, input),
        Session::Absent(pending) => {
            pending.push_back(input);
            Ok(())
        }
    }
}

fn queue_input(execution: &mut Execution, input: InputId) -> Result<(), Refusal> {
    if execution.inbox.contains(&input)
        || matches!(&execution.activity, Activity::Launching(id) | Activity::InputPending(id) if id == &input)
    {
        return Err(Refusal::DuplicateObligation);
    }
    execution.inbox.push_back(input);
    if matches!(execution.activity, Activity::Resting) {
        let first = execution
            .inbox
            .pop_front()
            .ok_or(Refusal::EmptyObligation)?;
        execution.activity = Activity::InputPending(first);
    }
    Ok(())
}

fn add_callback_wake(activity: &mut Activity, callback: CallbackId) -> Result<(), Refusal> {
    let wake = Wake::Callback(callback);
    match activity {
        Activity::Turn { continuations, .. } | Activity::OwnWork { continuations, .. } => {
            if !continuations.insert(wake) {
                return Err(Refusal::DuplicateObligation);
            }
        }
        Activity::Continuation(pending) => {
            let mut values = pending.values();
            if !values.insert(wake) {
                return Err(Refusal::DuplicateObligation);
            }
            *pending = NonEmpty::new(values)?;
        }
        Activity::Resting => *activity = Activity::Continuation(NonEmpty::new([wake])?),
        // Claude asks from a turn the task did not see start, before it reads the pending input:
        // it goes on inside that turn, so nothing has to wake it.
        Activity::InputPending(_) => {}
        Activity::Launching(_) => return Err(Refusal::InvalidState),
    }
    Ok(())
}

fn remove_callback_wake(activity: &mut Activity, callback: &CallbackId) -> Result<(), Refusal> {
    let wake = Wake::Callback(callback.clone());
    match activity {
        Activity::Turn { continuations, .. } | Activity::OwnWork { continuations, .. } => {
            continuations.remove(&wake);
        }
        Activity::Continuation(pending) => {
            let mut values = pending.values();
            values.remove(&wake);
            *activity = if values.is_empty() {
                Activity::Resting
            } else {
                Activity::Continuation(NonEmpty::new(values)?)
            };
        }
        _ => {}
    }
    Ok(())
}

fn execution_waiting(execution: &Execution) -> (Option<&InputId>, Option<&Inputs>) {
    let first = match &execution.activity {
        Activity::Launching(input) | Activity::InputPending(input) => Some(input),
        _ => None,
    };
    (first, Some(&execution.inbox))
}

/// What the execution has not consumed, in the order it waits.
fn execution_pending(execution: &Execution) -> Inputs {
    let mut pending = execution.inbox.clone();
    if let Activity::Launching(input) | Activity::InputPending(input) = &execution.activity {
        pending.push_front(input.clone());
    }
    pending
}
