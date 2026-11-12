//! What Alondra does with an input its SDK session did not take, and with work of the node's own
//! that ended while no turn took up its notice. The owner of the host acts on a refusal at once;
//! the one clock (`obligation_watch`) acts on what is past its deadline. Both stage the same
//! commands, so a step repeated by either is a repeat the domain recognises by its id.
use crate::domain_store::{Attached, OwnerTransaction, StoreError};
use alondra_core::domain::*;
use alondra_domain::ActivityView;
use alondra_foundation::InputId;

pub(crate) const WORK_ENDED: &str = "Managed native work finished. Inspect its public result, preserve completed work, and continue this task through Alondra.";

fn envelope(owner: &OwnerTransaction<'_>, id: String, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: owner.state().data_epoch.clone(), command_id: id.as_str().into(), expected_revision: 0, command }
}

fn generation(owner: &OwnerTransaction<'_>, task: &TaskId) -> Result<Option<u64>, StoreError> {
    Ok(owner.lifecycle(task)?.execution().map(|stamp| stamp.generation()))
}

/// The host refused `input` before offering it: the same text goes again as a new input, in the
/// same step. The new input, if the domain recorded one.
pub(crate) fn resend_refused(owner: &mut OwnerTransaction<'_>, task: &TaskId, input: &InputId) -> Result<Option<InputId>, StoreError> {
    let Some(generation) = generation(owner, task)? else { return Ok(None) };
    let refused = owner.pending_handoffs(task)?.into_iter()
        .find(|(pending, status)| pending.id() == input && status == "rejected")
        .map(|(pending, _)| pending.text().to_owned());
    let Some(text) = refused else { return Ok(None) };
    let command = Command::ResendUntakenInput { task_id: task.clone(), generation, input: input.as_str().into(), text };
    owner.stage_command(&Actor::System, &envelope(owner, format!("refused:{task}:{generation}:{}", input.as_str()), command), Attached::None)?;
    Ok(owner.state().lifecycle_journal.get(task).and_then(|journal| journal.iter().rev().find_map(|record| match record {
        LifecycleRecord::HandoffResent { input: refused, resent, .. } if refused == input.as_str() => InputId::new(resent.clone()).ok(),
        _ => None,
    })))
}

/// Work of the node's own ended and no turn took up its notice: the node is told. Whether it was.
pub(crate) fn wake(owner: &mut OwnerTransaction<'_>, task: &TaskId, agent: &AgentId) -> Result<bool, StoreError> {
    let Some((id, command)) = wake_command(owner.state(), task, agent) else { return Ok(false) };
    owner.stage_command(&Actor::System, &envelope(owner, id, command), Attached::None)?;
    Ok(true)
}

/// The command that tells the node of `task` its own work ended, with the id that makes a repeat
/// recognisable, when work ended that no turn took up and nothing else holds the node.
pub(crate) fn wake_command(state: &DomainState, task: &TaskId, agent: &AgentId) -> Option<(String, Command)> {
    let lifecycle = state.lifecycle_of(task).ok()?;
    let stamp = lifecycle.execution().cloned()?;
    if lifecycle.resource() != Some(alondra_domain::ResourceEvidence::Available) || lifecycle.activity() != ActivityView::Continuation
        || !lifecycle.pending_inputs().is_empty() || !lifecycle.cards().is_empty() || lifecycle.awaits().is_some()
        || !matches!(lifecycle.phase(), alondra_domain::TaskPhase::InProgress | alondra_domain::TaskPhase::Delivered) {
        return None;
    }
    if state.node_pause(agent).is_some_and(|pause| !pause.released) {
        return None;
    }
    let mut pending = std::collections::BTreeSet::new();
    for record in state.lifecycle_journal.get(task).into_iter().flatten() {
        match record {
            LifecycleRecord::Admitted { .. } | LifecycleRecord::HostEnded { .. } | LifecycleRecord::HostStopped { .. } => pending.clear(),
            LifecycleRecord::WorkEnded { continuation, .. } => { pending.insert(continuation.clone()); }
            LifecycleRecord::WorkContinued { continuation } | LifecycleRecord::TurnResumed { continuation, .. } => { pending.remove(continuation); }
            _ => {}
        }
    }
    if pending.is_empty() {
        return None;
    }
    use sha2::Digest;
    let digest = sha2::Sha256::digest(pending.iter().flat_map(|id| id.as_bytes().iter().copied().chain(std::iter::once(0))).collect::<Vec<_>>());
    let key: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let command = Command::TellNode { task_id: task.clone(), text: WORK_ENDED.into(), from_user: false, answers: None };
    Some((format!("work-wake:{task}:{}:{key}", stamp.generation()), command))
}
