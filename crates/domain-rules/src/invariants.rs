//! What every state an accepted command leaves must hold, beyond the structure `validate` checks:
//! the kinds of failure the state machine audit found (docs/architecture/state-machines.md).
//!
//! The tests of this crate check them after every command; another crate's tests turn the check on
//! with [`check_after_every_command`]. The installed daemon never turns it on.
use std::sync::atomic::{AtomicBool, Ordering};

use alondra_domain::Await;

use super::*;

static CHECKED: AtomicBool = AtomicBool::new(cfg!(test));

/// From now on, every command this process accepts panics if the state it leaves breaks an
/// invariant. For the tests of the crates that drive the domain.
pub fn check_after_every_command() {
    CHECKED.store(true, Ordering::Relaxed);
}

/// The invariants `state` breaks, each with what breaks it.
pub fn violations(state: &DomainState) -> Vec<String> {
    let mut found = vec![];
    for task in state.tasks.values() {
        let obligation = state.obligation_of(&task.id);
        if !task.state.terminal() && obligation.is_none() {
            found.push(format!("task {} is {:?} and nobody owes its next step", task.id, task.state));
        }
        if task.state.terminal() && obligation.is_some() {
            found.push(format!("task {} ended as {:?} but its lifecycle still owes {obligation:?}", task.id, task.state));
        }
        if !task.over()
            && let Some(card) = &task.final_presentation
            && state.interactions.get(card).is_none_or(|delivery| delivery.withdrawn)
        {
            found.push(format!("task {} stands delivered on card {card}, which is gone", task.id));
        }
        let Ok(lifecycle) = state.lifecycle_of(&task.id) else { continue };
        let answered_or_interrupted = match lifecycle.awaits() {
            Some(Await::Instruction) => true,
            Some(Await::User(card)) => state.interactions.get(card).is_some_and(DomainState::answered_question),
            _ => false,
        };
        if obligation.is_some_and(|owed| owed.party == alondra_domain::Party::User) && !answered_or_interrupted && !has_a_card_for_the_user(state, task) {
            found.push(format!("task {} waits on the user and no card asks them anything", task.id));
        }
        if let Some(Await::User(card) | Await::Orchestrator(card)) = lifecycle.awaits() {
            match state.interactions.get(card) {
                None => found.push(format!("task {} waits on card {card}, which does not exist", task.id)),
                Some(interaction) if interaction.withdrawn => {
                    found.push(format!("task {} waits on card {card}, which was withdrawn", task.id))
                }
                Some(_) => {}
            }
        }
    }
    for interaction in state.interactions.values() {
        let Scope::Task { task_id } = &interaction.scope else { continue };
        let open = !interaction.withdrawn && interaction.blocking && !DomainState::answered_question(interaction);
        if open && state.tasks.get(task_id).is_some_and(|task| task.state.terminal()) {
            found.push(format!("card {} still asks for an answer for task {task_id}, which ended", interaction.id));
        }
    }
    found
}

/// A card the user can act on for `task`: its delivery, or a question or request still open.
fn has_a_card_for_the_user(state: &DomainState, task: &Task) -> bool {
    let delivered = task.final_presentation.as_ref().and_then(|card| state.interactions.get(card)).is_some_and(|card| !card.withdrawn);
    delivered || state.interactions.values().any(|i| {
        i.recipient == Recipient::User && !i.withdrawn && matches!(&i.scope, Scope::Task { task_id } if *task_id == task.id)
            && matches!(i.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })
    })
}

/// A command whose acceptance applies a decision: applied again, it must change nothing.
fn decides(command: &Command) -> bool {
    matches!(
        command,
        Command::AnswerQuestion { .. }
            | Command::AcceptDelivery { .. }
            | Command::DecideProposedPlan { .. }
            | Command::DecideDocumentReview { .. }
            | Command::SettleSuggestion { .. }
            | Command::SettleStall { .. }
            | Command::SettlePlanReview { .. }
            | Command::AuthorizeMerge { .. }
            | Command::RecordVerdict { .. }
    )
}

/// Panics, when the check is on, if `next`, the state `envelope` left from `before`, breaks an
/// invariant, or if the decision it applied would change `next` again.
pub(super) fn assert_hold(before: &DomainState, actor: &Actor, envelope: &CommandEnvelope, next: &DomainState) {
    if !CHECKED.load(Ordering::Relaxed) {
        return;
    }
    let earlier = violations(before);
    let broken: Vec<String> = violations(next).into_iter().filter(|why| !earlier.contains(why)).collect();
    assert!(broken.is_empty(), "{:?} left the domain broken:\n{}", envelope.command, broken.join("\n"));
    if decides(&envelope.command)
        && let Ok((again, _)) = next.prepare_transition(actor, envelope)
    {
        assert!(again == *next, "{:?}, applied a second time, changed the state again", envelope.command);
    }
}
