mod commands;
mod observations;

use alondra_foundation::{AgentId, InteractionId, MAX_REVISION, Revision, TaskId};
pub use alondra_foundation::{
    CallbackId, ContinuationId, ConversationId, ExecutionStamp, HostId, IdentityError, InputId,
    InstanceId, OperationId, TurnId, WorkId,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
    Answer {
        card: InteractionId,
        input: InputId,
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
    DependenciesResolved {
        input: InputId,
    },
    Pause {
        card: InteractionId,
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
    /// `RetireReleasedExecution`, for a fleet that queues the task again on its own; an execution
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
    /// The user settled the doubtful input of `card`: `resend` takes its place, or it is taken as
    /// received and leaves the queue without being consumed. When nothing is left to explain the
    /// task's next step, it waits on `unheard_card`.
    SettleHandoff {
        card: InteractionId,
        resend: Option<InputId>,
        unheard_card: InteractionId,
    },
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
    EffectAccepted {
        operation: OperationId,
    },
    EffectObserved {
        operation: OperationId,
    },
    Notification,
    /// The execution's host ended (no process holds its lock any more), and its native Claude
    /// with it: the turn, the work and the questions it had open ended too. When that leaves
    /// nothing to explain the task's next step, it waits on `unheard_card`.
    HostEnded {
        unheard_card: InteractionId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Card {
    id: InteractionId,
    reason: WaitReason,
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
    inbox: VecDeque<InputId>,
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
    Absent(VecDeque<InputId>),
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
        remaining: VecDeque<InputId>,
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
        let session = match &self.state {
            State::Unstarted(input) => return vec![input],
            State::InProgress(Progress::Queued {
                input, remaining, ..
            }) => return std::iter::once(input).chain(remaining).collect(),
            State::InProgress(
                Progress::Waiting { session, .. } | Progress::Dependency { session, .. },
            ) => Some(session),
            State::Delivered(delivered) => Some(&delivered.session),
            State::InProgress(Progress::Executing { execution, .. }) => {
                return execution_pending(&execution.0);
            }
            State::Finishing(_) | State::Completed | State::Cancelled => None,
        };
        match session {
            Some(Session::Absent(pending)) => pending.iter().collect(),
            Some(Session::Present(execution)) => execution_pending(execution),
            None => vec![],
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
        if expected_revision != self.revision {
            return Err(Refusal::StaleRevision);
        }
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
        *self = candidate;
        Ok(())
    }
}

fn callback_id(state: &CallbackState) -> &CallbackId {
    match state {
        CallbackState::AwaitingAnswer(id) | CallbackState::Answered(id) => id,
    }
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
        _ => return Err(Refusal::InvalidState),
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

fn execution_pending(execution: &Execution) -> Vec<&InputId> {
    let pending = match &execution.activity {
        Activity::Launching(input) | Activity::InputPending(input) => Some(input),
        _ => None,
    };
    pending.into_iter().chain(execution.inbox.iter()).collect()
}
