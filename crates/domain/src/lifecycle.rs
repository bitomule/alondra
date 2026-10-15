mod commands;
mod observations;

use alondra_foundation::{AgentId, InteractionId, MAX_REVISION, Revision, TaskId};
pub use alondra_foundation::{
    CallbackId, ContinuationId, ConversationId, ExecutionStamp, HostId, IdentityError, InputId,
    InstanceId, OperationId, TurnId, WorkId,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
pub enum ProcessEvidence {
    Reserved,
    Running,
    ObservationPending,
    Exited,
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
    Process(InstanceId),
    Work(WorkId),
    Callback(CallbackId),
    Merge(OperationId),
    Cleanup(OperationId),
    ExternalEffect(OperationId),
    PostMerge(OperationId),
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
    RetireExitedExecution {
        problem_card: InteractionId,
        input: InputId,
    },
    Reopen {
        input: InputId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeFact {
    Ask {
        card: InteractionId,
        callback: CallbackId,
        reason: WaitReason,
    },
    Initialized,
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
    ProcessExited {
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
    process: ProcessEvidence,
    activity: Activity,
    callbacks: Vec<CallbackState>,
    effects: BTreeSet<OperationId>,
    inbox: VecDeque<InputId>,
}
impl Execution {
    fn holds_slot(&self) -> bool {
        matches!(
            self.process,
            ProcessEvidence::Reserved | ProcessEvidence::ObservationPending
        ) || self.activity.owns_work()
            || (self.process == ProcessEvidence::Running
                && !matches!(self.activity, Activity::Resting))
    }
}

// Startup has a reservation; subsequent activity requires an observed running execution.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveExecution(Execution);
impl ActiveExecution {
    fn new(execution: Execution) -> Result<Self, Refusal> {
        let reserved = execution.process == ProcessEvidence::Reserved
            && matches!(execution.activity, Activity::Launching(_));
        if !reserved
            && (execution.process != ProcessEvidence::Running
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
}

impl TaskLifecycle {
    pub fn planned(task: TaskId, input: InputId) -> Result<Self, Refusal> {
        valid_id(&task.0)?;
        Ok(Self {
            task,
            revision: 0,
            generation: 0,
            state: State::Unstarted(input),
        })
    }
    pub fn task(&self) -> &TaskId {
        &self.task
    }
    pub fn revision(&self) -> Revision {
        self.revision
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
            if execution.process == ProcessEvidence::Exited {
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
    pub fn process(&self) -> Option<ProcessEvidence> {
        self.session().map(|execution| execution.process)
    }
    pub fn holds_slot(&self) -> bool {
        if let State::Finishing(finishing) = &self.state {
            return finishing.remaining.iter().any(|obligation| {
                matches!(
                    obligation,
                    DrainObligation::Process(_)
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
            if execution.process != ProcessEvidence::Running {
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
