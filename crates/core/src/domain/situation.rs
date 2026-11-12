//! The one state a task shows: seven situations every screen, count and the gate read, decided
//! once from its lifecycle, who owes its next step and the cards the inbox lists.
use super::*;
use alondra_domain::{Await, Capacity, FinishIntent, Owed, Owner, Party, TaskPhase, WaitReason};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Situation {
    Working(Doing),
    Waiting(WaitsFor),
    /// A card of the task that the inbox lists asks the user to answer or act.
    NeedsYou,
    /// The user paused it, or interrupted its turn.
    Paused,
    Delivered(Delivery),
    Finished,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doing {
    Turn,
    OwnWork,
    /// The work its node said comes after its PR merged.
    AfterMerge,
}

/// What a waiting task goes on with by itself once it arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitsFor {
    Slot,
    Machine,
    Disk,
    Usage,
    ProviderApi,
    Tasks,
    PlanReview,
    Step(AlondraStep),
}

/// A short step Alondra itself owes the task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlondraStep {
    Starting,
    Delivering,
    Waking,
    Answering,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    ForReview,
    Blocked,
    /// Accepted: its PR merges or the task cleans up before closing.
    Closing,
}

/// What the inbox lists of a task: whether a card of it other than its delivery asks the user to
/// answer or act, and whether a given card, or the one it is grouped under, is listed.
pub struct Listing<'a> {
    pub needs_you: bool,
    pub listed: &'a dyn Fn(&InteractionId) -> bool,
}

/// A task's situation, and the card its lifecycle waits on the user for that the inbox does not
/// list: a fault to show, never a «needs you» nobody can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reckoned {
    pub situation: Situation,
    pub unlisted: Option<InteractionId>,
}

impl From<Situation> for Reckoned {
    fn from(situation: Situation) -> Self {
        Self { situation, unlisted: None }
    }
}

impl DomainState {
    /// The one state `task` shows, given what the inbox lists of it.
    pub fn situation(&self, task: &Task, listing: &Listing<'_>) -> Reckoned {
        match task.state {
            TaskState::Done => return Situation::Finished.into(),
            TaskState::Cancelled => return Situation::Cancelled.into(),
            _ => {}
        }
        if listing.needs_you {
            return Situation::NeedsYou.into();
        }
        if self.task_activity(task) == Some(TaskActivity::NodeClosed) {
            return Situation::Waiting(WaitsFor::Slot).into();
        }
        let Ok(lifecycle) = self.lifecycle_of(&task.id) else { return without_lifecycle(task).into() };
        let Some(obligation) = lifecycle.obligation() else {
            return if lifecycle.phase() == TaskPhase::Cancelled { Situation::Cancelled } else { Situation::Finished }.into();
        };
        match obligation.owed {
            Owed::Turn if self.work_after_merge(&task.id, &lifecycle).is_some() => return Situation::Working(Doing::AfterMerge).into(),
            Owed::Turn => return Situation::Working(Doing::Turn).into(),
            Owed::OwnWork => return Situation::Working(Doing::OwnWork).into(),
            _ => {}
        }
        let user_paused = lifecycle.cards().iter()
            .any(|(card, reason)| *reason == WaitReason::Pause && lifecycle.card_owner(card) == Some(Owner::User) && self.node_pause_card(card));
        if user_paused || lifecycle.awaits() == Some(&Await::Instruction) {
            return Situation::Paused.into();
        }
        if obligation.party == Party::User {
            let waited: Vec<InteractionId> = lifecycle.cards().into_iter()
                .filter(|(card, _)| lifecycle.card_owner(card) == Some(Owner::User))
                .map(|(card, _)| card.clone())
                .chain(match lifecycle.awaits() {
                    Some(Await::User(card)) => Some(card.clone()),
                    _ => None,
                })
                .collect();
            if let Some(card) = waited.iter().find(|card| !(listing.listed)(card)).cloned() {
                return Reckoned { situation: Situation::Waiting(WaitsFor::Step(AlondraStep::Answering)), unlisted: Some(card) };
            }
            if !waited.is_empty() && lifecycle.phase() != TaskPhase::Delivered {
                return Situation::NeedsYou.into();
            }
        }
        if lifecycle.phase() == TaskPhase::Delivered {
            match obligation.owed {
                Owed::User if self.task_handed_back(&task.id).is_some() => return Situation::Waiting(WaitsFor::Step(AlondraStep::Delivering)).into(),
                Owed::User => return Situation::Delivered(Delivery::ForReview).into(),
                Owed::Tasks => return Situation::Delivered(Delivery::Blocked).into(),
                Owed::Merge | Owed::Close => return Situation::Delivered(Delivery::Closing).into(),
                _ => {}
            }
        }
        if lifecycle.finishing_intent() == Some(FinishIntent::Cancel) {
            return Situation::Cancelled.into();
        }
        Situation::Waiting(match obligation.owed {
            Owed::Room(Capacity::Gate) => WaitsFor::Slot,
            Owed::Room(Capacity::Machine) => WaitsFor::Machine,
            Owed::Room(Capacity::Disk) => WaitsFor::Disk,
            Owed::Room(Capacity::Usage) => WaitsFor::Usage,
            Owed::Room(Capacity::ClaudeApi) => WaitsFor::ProviderApi,
            Owed::Tasks => WaitsFor::Tasks,
            Owed::Answer if !lifecycle.plan_reviews().is_empty() => WaitsFor::PlanReview,
            Owed::Start => WaitsFor::Step(AlondraStep::Starting),
            Owed::HandInput | Owed::HandDecision => WaitsFor::Step(AlondraStep::Delivering),
            Owed::Wake => WaitsFor::Step(AlondraStep::Waking),
            Owed::Merge | Owed::Close => return Situation::Delivered(Delivery::Closing).into(),
            Owed::Answer | Owed::User | Owed::Turn | Owed::OwnWork => WaitsFor::Step(AlondraStep::Answering),
        }).into()
    }
}

fn without_lifecycle(task: &Task) -> Situation {
    match task.state {
        TaskState::NotStarted => Situation::Waiting(WaitsFor::Slot),
        TaskState::Running => Situation::Waiting(WaitsFor::Step(AlondraStep::Starting)),
        TaskState::NeedsYou => Situation::Waiting(WaitsFor::Step(AlondraStep::Answering)),
        TaskState::AwaitingBlocker => Situation::Delivered(Delivery::Blocked),
        TaskState::ForYourReview => Situation::Delivered(Delivery::ForReview),
        TaskState::WaitingDependency => Situation::Waiting(WaitsFor::Tasks),
        TaskState::Closing => Situation::Delivered(Delivery::Closing),
        TaskState::Done => Situation::Finished,
        TaskState::Cancelled => Situation::Cancelled,
    }
}

#[cfg(test)]
impl Situation {
    /// The situation a task in `live` reads, so a screen test reaches every state without building its history.
    pub(crate) fn of_live(live: &alondra_domain::LiveState) -> Self {
        use alondra_domain::{LiveState, Step, Worker};
        match live {
            LiveState::Working(Worker::Turn) => Self::Working(Doing::Turn),
            LiveState::Working(Worker::OwnWork) => Self::Working(Doing::OwnWork),
            LiveState::Working(Worker::AfterMerge) => Self::Working(Doing::AfterMerge),
            LiveState::Working(Worker::Alondra(Step::Starting)) => Self::Waiting(WaitsFor::Step(AlondraStep::Starting)),
            LiveState::Working(Worker::Alondra(Step::Delivering)) => Self::Waiting(WaitsFor::Step(AlondraStep::Delivering)),
            LiveState::Working(Worker::Alondra(Step::Waking)) => Self::Waiting(WaitsFor::Step(AlondraStep::Waking)),
            LiveState::Working(Worker::Alondra(Step::Answering)) => Self::Waiting(WaitsFor::Step(AlondraStep::Answering)),
            LiveState::Working(Worker::Alondra(Step::Merging | Step::Finishing)) => Self::Delivered(Delivery::Closing),
            LiveState::WaitingUser(cards) if cards.iter().all(|card| card.0.starts_with("delivery")) => Self::Delivered(Delivery::ForReview),
            LiveState::WaitingUser(cards) if cards.iter().all(|card| card.0.starts_with(crate::domain::NODE_PAUSE_PREFIX)) => Self::Paused,
            LiveState::WaitingUser(_) => Self::NeedsYou,
            LiveState::WaitingCapacity(Capacity::Gate) => Self::Waiting(WaitsFor::Slot),
            LiveState::WaitingCapacity(Capacity::Machine) => Self::Waiting(WaitsFor::Machine),
            LiveState::WaitingCapacity(Capacity::Disk) => Self::Waiting(WaitsFor::Disk),
            LiveState::WaitingCapacity(Capacity::Usage) => Self::Waiting(WaitsFor::Usage),
            LiveState::WaitingCapacity(Capacity::ClaudeApi) => Self::Waiting(WaitsFor::ProviderApi),
            LiveState::WaitingTask(_) => Self::Waiting(WaitsFor::Tasks),
            LiveState::Paused => Self::Paused,
        }
    }
}
