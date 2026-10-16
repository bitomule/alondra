//! The one authority of a task's lifecycle: its `TaskLifecycle`, replayed inside the transaction
//! from the records its decisions left in the task's journal. The task's state, node, start and
//! ending in the DTO are read from it and checked against it before a transition is accepted, so
//! no other writer can move them for a task that has a journal. A task created before the journal
//! keeps the old writers until the migration of phase 3c.
use super::*;
use alondra_domain::{
    ActivityView, ConversationId, DrainObligation, Event, FinishIntent, HostId, InputId, InstanceId, OperationId,
    PlanReviewOutcome as ReviewSettlement, Refusal, ResourceEvidence, RuntimeFact, TaskLifecycle,
    TaskPhase, Trigger, TurnId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

type Result<T> = std::result::Result<T, DomainError>;

/// The host of every node of the `claude --bg` backend: one process per session, so the session
/// names both the conversation and the instance.
const LEGACY_HOST: &str = "claude-bg";

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOutcome {
    Unchanged,
    PlanChanged,
    Cancelled,
    KeptByUser,
}

/// One decision in a task's journal, as it is kept. Data only: a task's lifecycle is what the
/// aggregate says after replaying its journal, never a field read from a record. The task's DTO
/// yields no lifecycle either, whatever it was deserialized from:
///
/// ```compile_fail,E0277
/// let task: alondra_core::domain::Task = serde_json::from_str("{}").unwrap();
/// let lifecycle: alondra_domain::TaskLifecycle = task.into();
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleRecord {
    Planned { input: String },
    Admitted { node: AgentId },
    ResourceAvailable,
    TurnStarted { turn: String, input: String },
    InputConsumed { input: String },
    ExecutionReleased { input: String },
    Presented { delivery: InteractionId },
    HandedBack { delivery: InteractionId, input: String },
    Accepted { delivery: InteractionId },
    Finished { intent: TaskEnding, cleanup: String, post_merge: Option<String> },
    Drained { obligation: Drained },
    Reopened { input: String },
    HeldForPlanReview { review: InteractionId },
    PlanReviewSettled { review: InteractionId, plan_revision: u64, outcome: ReviewOutcome },
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

/// The event a record stands for, against the aggregate it was decided on: runtime facts take
/// its current execution and the next cursor.
fn event(task: &TaskId, record: &LifecycleRecord, lifecycle: &TaskLifecycle) -> Result<Event> {
    let runtime = |fact: RuntimeFact| -> Result<Event> {
        let stamp = lifecycle.execution().ok_or_else(|| corrupt(task, "a runtime fact without an execution"))?.clone();
        Ok(Event::Runtime { stamp, cursor: lifecycle.cursor().unwrap_or(0) + 1, fact })
    };
    Ok(match record {
        LifecycleRecord::Planned { .. } => return Err(corrupt(task, "a second planned record")),
        LifecycleRecord::Admitted { node } => Event::Admit {
            node: node.clone(),
            conversation: ConversationId::new(node.0.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            instance: InstanceId::new(node.0.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            host: HostId::new(LEGACY_HOST).map_err(|e| corrupt(task, format!("{e:?}")))?,
        },
        LifecycleRecord::ResourceAvailable => runtime(RuntimeFact::ResourceAvailable)?,
        LifecycleRecord::TurnStarted { turn, input } => runtime(RuntimeFact::TurnStarted {
            turn: TurnId::new(turn.as_str()).map_err(|e| corrupt(task, format!("{e:?}")))?,
            trigger: Trigger::Input(input_id(task, input)?),
        })?,
        LifecycleRecord::InputConsumed { input } => runtime(RuntimeFact::InputConsumed { input: input_id(task, input)? })?,
        LifecycleRecord::ExecutionReleased { input } => Event::ExecutionReleased { input: input_id(task, input)? },
        LifecycleRecord::Presented { delivery } => Event::Present { delivery: delivery.clone() },
        LifecycleRecord::HandedBack { delivery, input } => Event::HandBack { delivery: delivery.clone(), input: input_id(task, input)? },
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
            },
        },
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
    })
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

/// What the ending of a finishing task still owes, as receipts discharge them.
pub fn owed(lifecycle: &TaskLifecycle) -> Vec<Drained> {
    lifecycle
        .drain()
        .filter_map(|obligation| match obligation {
            DrainObligation::Execution(instance) => Some(Drained::Execution { instance: instance.as_str().into() }),
            DrainObligation::Cleanup(operation) => Some(Drained::Cleanup { operation: operation.as_str().into() }),
            DrainObligation::PostMerge(operation) => Some(Drained::PostMerge { operation: operation.as_str().into() }),
            DrainObligation::Work(_) | DrainObligation::Callback(_) | DrainObligation::Merge(_) | DrainObligation::ExternalEffect(_) => None,
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
    /// A task whose lifecycle the aggregate owns: it has a journal. Any other task is one created
    /// before the journal, which the old writers still move.
    pub fn owned(&self, task: &TaskId) -> bool {
        self.lifecycle_journal.contains_key(task)
    }

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
            let journal = self.lifecycle_journal.get(task).ok_or_else(|| DomainError::new("not_owned", format!("Task {task} has no lifecycle journal")))?;
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

    /// A lifecycle decision on an owned task: the aggregate validates it, the journal keeps it and
    /// the task's DTO reads it, in one step. A refusal changes nothing.
    pub(super) fn decide(&mut self, lifecycles: &mut Lifecycles, task: &TaskId, record: LifecycleRecord) -> Result<()> {
        self.lifecycle(lifecycles, task)?;
        let lifecycle = lifecycles.replayed.get_mut(task).expect("replayed above");
        let event = event(task, &record, lifecycle)?;
        lifecycle.apply(lifecycle.revision(), event).map_err(|refusal| refused(task, &record, lifecycle, refusal))?;
        self.lifecycle_journal.get_mut(task).expect("owned").push(record);
        let lifecycle = &lifecycles.replayed[task];
        self.materialize(task, lifecycle);
        Ok(())
    }

    /// The task's DTO reads the aggregate: node, start, ending and the state its phase stands for.
    /// `derive_states` then picks the attention state within that phase, never across it. Once the
    /// end of a task's node is recorded, the node and its start stay as history of the task.
    fn materialize(&mut self, task: &TaskId, lifecycle: &TaskLifecycle) {
        let phase = lifecycle.phase();
        let t = self.tasks.get_mut(task).expect("an owned task exists");
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

    /// What the store keeps no column for: the ending of every owned task, read from its journal.
    /// The other lifecycle fields come from their columns and are checked by `validate_journal`.
    pub fn read_endings(&mut self) -> Result<()> {
        let owned: Vec<TaskId> = self.lifecycle_journal.keys().cloned().collect();
        for task in owned {
            let lifecycle = replay(&task, &self.lifecycle_journal[&task])?;
            let t = self.tasks.get_mut(&task).ok_or_else(|| DomainError::new("not_found", format!("Task {task} has a journal and no row")))?;
            t.ending = ending_of(&lifecycle);
        }
        Ok(())
    }

    /// Every owned task agrees with its journal: what a loaded state proves before it is used.
    pub fn validate_journal(&self) -> Result<()> {
        self.validate_lifecycles(self.lifecycle_journal.keys())
    }

    /// The aggregate rules: a task's DTO agrees with the replay of its journal, or the transition
    /// is refused. Checked for every task a transition touched.
    pub(super) fn validate_lifecycles<'a>(&self, tasks: impl Iterator<Item = &'a TaskId>) -> Result<()> {
        for task in tasks {
            let Some(journal) = self.lifecycle_journal.get(task) else { continue };
            let lifecycle = replay(task, journal)?;
            let t = self.tasks.get(task).ok_or_else(|| DomainError::new("not_found", format!("Task {task} has a journal and no row")))?;
            if let Err(detail) = self.agrees(t, &lifecycle) {
                return Err(DomainError::new("lifecycle_mismatch", format!(
                    "the aggregate rules: task {task} reads {:?} (started {}, node {:?}, ending {:?}) while its lifecycle is {:?}: {detail}",
                    t.state, t.started, t.agent_id, t.ending, lifecycle.phase())));
            }
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
        if !self.owned(task) {
            self.tasks.get_mut(task).expect("the task exists").agent_id = Some(node.clone());
            return Ok(());
        }
        if self.lifecycle(lifecycles, task)?.execution().is_some() {
            let record = LifecycleRecord::ExecutionReleased { input: self.next_input(task) };
            self.decide(lifecycles, task, record)?;
        }
        self.decide(lifecycles, task, LifecycleRecord::Admitted { node: node.clone() })?;
        let running = self.agents.get(node).is_some_and(|agent| agent.phase.live() && !matches!(agent.phase, NodePhase::Starting { .. } | NodePhase::Lost));
        if running {
            self.decide(lifecycles, task, LifecycleRecord::ResourceAvailable)?;
        }
        Ok(())
    }

    /// The task's node is gone and left nothing behind: the task waits for another.
    pub(super) fn release(&mut self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        if !self.owned(task) {
            let t = self.tasks.get_mut(task).expect("the task exists");
            t.agent_id = None;
            t.started = false;
            return Ok(());
        }
        let record = LifecycleRecord::ExecutionReleased { input: self.next_input(task) };
        self.decide(lifecycles, task, record)
    }

    /// A receipt of an ending task discharges what its ending still owes among `which`. A task
    /// that is not ending owes nothing yet.
    pub(super) fn drain(&mut self, task: &TaskId, lifecycles: &mut Lifecycles, which: impl Fn(&Drained) -> bool) -> Result<()> {
        if !self.owned(task) || !self.tasks[task].ending() {
            return Ok(());
        }
        let owed: Vec<Drained> = owed(self.lifecycle(lifecycles, task)?).into_iter().filter(|owed| which(owed)).collect();
        for obligation in owed {
            self.decide(lifecycles, task, LifecycleRecord::Drained { obligation })?;
        }
        Ok(())
    }

    /// The node ran with what it was given: its `claude --bg` process is up and its turn took
    /// every input the task had queued for it. Known when the launcher reports the start, when a
    /// turn of the node begins and when the node delivers or has a delivery taken back.
    pub(super) fn node_has_run(&mut self, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        let lifecycle = self.lifecycle(lifecycles, task)?;
        if lifecycle.execution().is_none() || matches!(lifecycle.phase(), TaskPhase::Finishing | TaskPhase::Completed | TaskPhase::Cancelled) {
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

    /// Every owned task whose final delivery settled with nothing left for the user, and whose
    /// node is over, is done now. One whose node still winds up stays delivered and closing.
    pub(super) fn finish_settled_deliveries(&mut self, lifecycles: &mut Lifecycles) -> Result<()> {
        let settled: Vec<TaskId> = self.tasks.values()
            .filter(|t| !t.over() && !t.archived && self.owned(&t.id))
            .filter(|t| self.finished(t) && !self.unanswered(&t.id) && !self.blockers(&t.id) && !self.wraps_up(t))
            .map(|t| t.id.clone())
            .collect();
        for task in settled {
            self.finish_task(&task, lifecycles)?;
        }
        Ok(())
    }

    /// The state of an owned task within the phase its lifecycle materialized: what it waits for
    /// or who has to act. Never a phase change: no Done, Cancelled or Closing comes from here.
    pub(super) fn owned_state(&self, t: &Task) -> TaskState {
        match t.ending {
            Some(TaskEnding::Cancelling) => return TaskState::Closing,
            Some(TaskEnding::Completing) => {
                return if t.wrap_up.post_merge_work.is_some() && !self.node_session_closed(t) { TaskState::Running } else { TaskState::Closing };
            }
            None => {}
        }
        if self.stack_ready(t).is_err() || (t.final_presentation.is_none() && self.waits_are_over(t).is_err()) {
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

fn refused(task: &TaskId, record: &LifecycleRecord, lifecycle: &TaskLifecycle, refusal: Refusal) -> DomainError {
    DomainError::new("conflict", format!(
        "The lifecycle of task {task} ({:?}) refuses {record:?}: {refusal:?}", lifecycle.phase()))
}
