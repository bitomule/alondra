//! The invariants every accepted command must keep, checked after each one when the check is on.
use alondra_domain_model::*;

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
pub(crate) fn assert_hold(before: &DomainState, actor: &Actor, envelope: &CommandEnvelope, next: &DomainState) {
    if !(cfg!(test) || invariants::checked()) {
        return;
    }
    let earlier = invariants::violations(before);
    let broken: Vec<String> = invariants::violations(next).into_iter().filter(|why| !earlier.contains(why)).collect();
    assert!(broken.is_empty(), "{:?} left the domain broken:\n{}", envelope.command, broken.join("\n"));
    if decides(&envelope.command)
        && let Ok((again, _)) = crate::rules::prepare_transition(next, actor, envelope)
    {
        assert!(again == *next, "{:?}, applied a second time, changed the state again", envelope.command);
    }
}

#[cfg(test)]
static DECLARED: std::sync::LazyLock<std::collections::BTreeSet<(String, String, String)>> =
    std::sync::LazyLock::new(|| transition_table::declared(include_str!("../../../docs/architecture/state-machines.md")));

/// In this crate's tests, a command that makes a move the inventory does not declare panics.
#[cfg(test)]
pub(crate) fn assert_declared(before: &DomainState, after: &DomainState, command: &Command) {
    let undeclared: Vec<String> = transition_table::moves(before, after).into_iter().filter(|made| !DECLARED.contains(made))
        .map(|(machine, from, to)| format!("{machine}: {from} -> {to}")).collect();
    assert!(undeclared.is_empty(), "{command:?} made moves docs/architecture/state-machines.md does not declare:\n{}", undeclared.join("\n"));
}

/// Every button any test projects on a valid state is also judged by running its command on a
/// copy: the guard must refuse exactly what the command refuses, with the same reason. A state
/// built by hand gets the journal its columns lead to first; one that breaks the domain's
/// invariants, or that no journal leads to, refuses every command, so it says nothing.
fn assert_guard_matches_execution(
    state: &DomainState,
    actor: &Actor,
    envelope: &CommandEnvelope,
    guarded: Option<&DomainError>,
) {
    if state.validate().is_err() {
        return;
    }
    let mut journaled = state.clone();
    if !journaled.journal_every_task() {
        return;
    }
    let executed = crate::rules::prepare_transition(&journaled, actor, envelope).err();
    assert_eq!(
        guarded,
        executed.as_ref(),
        "the guard and the command disagree on {:?}",
        envelope.command
    );
}

/// From now on, every button this process projects runs its command on a copy, which must refuse
/// exactly what the guard refuses.
pub fn check_every_button() {
    check_every_button_with(assert_guard_matches_execution);
}
