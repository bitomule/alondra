//! The one rule that says whether a node's turn ended in a closed result. A turn that did not is
//! empty, and Alondra hands it back to the node at once with what is missing, up to
//! `EMPTY_TURN_WAKES` times in a row; after that the orchestrator settles it.
use super::*;
use alondra_domain::{ActivityView, Owner, TaskLifecycle, TaskPhase, WaitReason};

/// What a turn of a node ended in, when it ended in something closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnResult {
    /// A card of the node to the user is open: a question or a request.
    UserCard(InteractionId),
    /// The user decides a card the node's turn raised: a plan to approve, a permission.
    Decision(InteractionId),
    /// Its delivery waits for the user's review.
    Delivery,
    /// It waits on something it declared: the orchestrator, other tasks, an instruction.
    Waits(TurnAwait),
    /// Work of its own still runs, or what it was told or what its work reported starts its next turn.
    OwnWork,
    /// Alondra holds it, for the Mac's capacity, Claude's limits or because the user paused it.
    Paused,
}

impl DomainState {
    /// What the turn of `task` ended in, judged on `ended`, its lifecycle once the turn ended
    /// without a wake, which waits on `unheard` when nothing else holds it. `Err` is an empty
    /// turn, with what its node is missing.
    pub fn turn_result(&self, task: &TaskId, ended: &TaskLifecycle, unheard: &InteractionId) -> std::result::Result<TurnResult, EmptyTurn> {
        if let Some(awaits) = ended.awaits() {
            return Ok(match TurnAwait::of(awaits) {
                TurnAwait::User { card } => TurnResult::UserCard(card),
                TurnAwait::Machine => TurnResult::Paused,
                awaits => TurnResult::Waits(awaits),
            });
        }
        if matches!(ended.activity(), ActivityView::Working | ActivityView::Starting | ActivityView::OwnWork | ActivityView::Continuation)
            || !ended.pending_inputs().is_empty() {
            return Ok(TurnResult::OwnWork);
        }
        let held: Vec<(&InteractionId, WaitReason)> = ended.cards().into_iter().filter(|(card, _)| *card != unheard).collect();
        if held.iter().any(|(_, reason)| *reason == WaitReason::Pause) {
            return Ok(TurnResult::Paused);
        }
        if let Some((card, _)) = held.iter().find(|(card, _)| ended.card_owner(card) == Some(Owner::User) && Some(*card) != ended.delivery()) {
            return Ok(TurnResult::Decision((*card).clone()));
        }
        if ended.phase() == TaskPhase::Delivered {
            return self.owed_step(task, ended).map_or(Ok(TurnResult::Delivery), Err);
        }
        if !ended.dependencies().is_empty() {
            return Ok(TurnResult::Waits(TurnAwait::Tasks { tasks: ended.dependencies().into_iter().cloned().collect() }));
        }
        if let Some((card, _)) = held.first() {
            return Ok(TurnResult::Waits(TurnAwait::Orchestrator { card: (*card).clone() }));
        }
        Err(self.empty_turn(task))
    }
}
