use super::*;

/// Who owes a live task its next step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Party {
    User,
    Capacity(Capacity),
    /// Another task, or the merge of a PR, has to land first.
    Tasks,
    Provider,
    Alondra,
}

/// The next step a live task waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owed {
    /// The node's session starts.
    Start,
    /// An input reaches the node.
    HandInput,
    /// An answer to a permission or a question reaches the turn that asked it.
    HandDecision,
    /// The node hears that work it waited on ended.
    Wake,
    Turn,
    OwnWork,
    /// The orchestrator settles a stall, answers a node or reviews a plan.
    Answer,
    Merge,
    Close,
    Room(Capacity),
    Tasks,
    User,
}

/// The next step of a live task, who owes it, and the revision since which it is owed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Obligation {
    pub party: Party,
    pub owed: Owed,
    pub since: Revision,
}

impl TaskLifecycle {
    /// Who owes the next step of the task. `None` only once it completed or was cancelled.
    pub fn obligation(&self) -> Option<Obligation> {
        self.owes().map(|(party, owed)| Obligation { party, owed, since: self.owed_since })
    }

    pub(super) fn owes(&self) -> Option<(Party, Owed)> {
        match &self.state {
            State::Completed | State::Cancelled => None,
            State::Finishing(_) => Some((Party::Alondra, Owed::Close)),
            State::Unstarted { .. } | State::InProgress(Progress::Queued { .. }) => Some(if self.plan_reviews.is_empty() {
                room(Capacity::Gate)
            } else {
                (Party::Alondra, Owed::Answer)
            }),
            State::InProgress(Progress::Executing { execution, .. }) => {
                Some(answer_on_its_way(&execution.0).unwrap_or_else(|| by_activity(&execution.0)))
            }
            State::InProgress(Progress::Waiting { cards, session, .. }) => {
                cards_owe(cards.iter()).or_else(|| session.execution().and_then(answer_on_its_way))
            }
            State::InProgress(Progress::Dependency { decisions, session, .. }) => Some(
                cards_owe(decisions.values())
                    .or_else(|| session.execution().and_then(answer_on_its_way))
                    .unwrap_or((Party::Tasks, Owed::Tasks)),
            ),
            State::Delivered(delivered) => Some(delivered_owes(delivered)),
        }
    }
}

fn delivered_owes(delivered: &Delivered) -> (Party, Owed) {
    let execution = delivered.session.execution().filter(|execution| execution.resource != ResourceEvidence::Released);
    let session = || {
        execution.and_then(answer_on_its_way).or_else(|| {
            execution
                .filter(|execution| matches!(execution.activity, Activity::Launching(_) | Activity::InputPending(_) | Activity::Turn { .. } | Activity::Continuation(_)))
                .map(by_activity)
        })
    };
    let gate = || match &delivered.gate {
        DeliveryGate::Review => (Party::User, Owed::User),
        DeliveryGate::Blocked(_) => (Party::Tasks, Owed::Tasks),
        DeliveryGate::AwaitingMerge(_) => (Party::Alondra, Owed::Merge),
        DeliveryGate::Accepted => (Party::Alondra, Owed::Close),
    };
    let sent_back = || waits_for_an_execution(delivered).then_some((Party::Alondra, Owed::Start));
    match delivered.gate {
        DeliveryGate::Review => session().or_else(sent_back).unwrap_or_else(gate),
        _ => cards_owe(delivered.decisions.values()).or_else(session).unwrap_or_else(gate),
    }
}

/// What the user told a delivery under review waits for an execution to take it, the user's own
/// cards aside: no session runs, or the one there ended, and inputs are pending.
fn waits_for_an_execution(delivered: &Delivered) -> bool {
    let pending = match &delivered.session {
        Session::Absent(pending) => !pending.is_empty(),
        Session::Present(execution) => execution.resource == ResourceEvidence::Released && !execution_pending(execution).is_empty(),
    };
    pending && !delivered.decisions.values().any(|card| card.owner == Owner::User)
}

/// An answer the user or a policy gave that has not reached the turn yet owes its handing over,
/// whatever the execution is doing.
fn answer_on_its_way(execution: &Execution) -> Option<(Party, Owed)> {
    execution
        .callbacks
        .iter()
        .any(|state| matches!(state, CallbackState::Decided(_) | CallbackState::Answered(_)))
        .then_some((Party::Alondra, Owed::HandDecision))
}

/// The cards in the order `waits_on` reads them: the user's, then the narrowest capacity a pause
/// waits on, then the orchestrator's.
fn cards_owe<'a>(cards: impl Iterator<Item = &'a Card>) -> Option<(Party, Owed)> {
    match waits_on(cards)? {
        LiveState::WaitingUser(_) => Some((Party::User, Owed::User)),
        LiveState::WaitingCapacity(capacity) => Some(room(capacity)),
        _ => Some((Party::Alondra, Owed::Answer)),
    }
}

/// What the execution owes by what it does. Ended work of its own whose node was handed an input
/// meanwhile owes that input, which the turn the work ending opens would also take up.
fn by_activity(execution: &Execution) -> (Party, Owed) {
    match &execution.activity {
        Activity::Continuation(_) if !execution.inbox.is_empty() => (Party::Alondra, Owed::HandInput),
        Activity::Launching(_) => (Party::Alondra, Owed::Start),
        Activity::InputPending(_) => (Party::Alondra, Owed::HandInput),
        Activity::Turn { .. } => (Party::Provider, Owed::Turn),
        Activity::OwnWork { .. } => (Party::Provider, Owed::OwnWork),
        Activity::Continuation(_) => (Party::Alondra, Owed::Wake),
        Activity::Awaiting(Await::Orchestrator(_)) => (Party::Alondra, Owed::Answer),
        Activity::Awaiting(Await::Machine) => room(Capacity::Machine),
        Activity::Awaiting(Await::Tasks(_)) => (Party::Tasks, Owed::Tasks),
        Activity::Awaiting(Await::User(_) | Await::Instruction) | Activity::Resting => (Party::User, Owed::User),
    }
}

fn room(capacity: Capacity) -> (Party, Owed) {
    (Party::Capacity(capacity), Owed::Room(capacity))
}
