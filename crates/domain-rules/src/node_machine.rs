//! The one state of every node. A node is in exactly one `NodePhase`, and only `next` moves it:
//! what Claude Code reports, the hooks, the gate, the listing of real sessions and the domain's own
//! commands are events it accepts or refuses. The gate, Tasks, the header and the TUI read
//! the phase; none keeps a state of its own.
use super::*;

#[cfg(test)]
pub(crate) fn task_holds_slot(domain: &DomainState, task: &TaskId) -> bool {
    domain.tasks.get(task).and_then(|task| task.agent_id.as_ref()).is_some_and(|agent| domain.slots().contains(agent))
}
/// What the gate's events do to a node: a pause becomes a pause of its task's lifecycle and a
/// resume ends it, and nothing moves its phase but the end of its session.
pub(crate) fn move_node(domain: &mut DomainState, agent: &AgentId, event: &NodeEvent, lifecycles: &mut Lifecycles) -> Result<()> {
    if !domain.agents.contains_key(agent) {
        return fail("not_found", "Agent missing");
    }
    if !domain.through_sdk(agent) && domain.node_phase(agent) != Some(&NodePhase::ThroughSdk) {
        return match event {
            NodeEvent::SessionClosed => Ok(()),
            _ => fail("node_closed", "The node's session is closed"),
        };
    }
    let task = domain.node_task(agent).filter(|t| !t.over()).map(|t| t.id.clone());
    match event {
        NodeEvent::Paused { pause } => {
            let task = task.ok_or_else(|| DomainError::new("not_found", "The node has no open task"))?;
            let by_user = pause.reason == PauseReason::ByUser;
            let held_by_alondra = domain.node_pause(agent).is_some_and(|held| held.reason != PauseReason::ByUser);
            if domain.held_pause(&task).is_some() && !(by_user && held_by_alondra) {
                return fail("already_paused", "The node is already paused");
            }
            if by_user {
                end_pauses_of_alondra(domain, &task, lifecycles)?;
            }
            let (card, owner, system) = node_pause_card(&task, pause)
                .ok_or_else(|| DomainError::new("invalid_input", "A Claude API error of a node of the Agent SDK is held by its task's lifecycle"))?;
            hold_node_pause(domain, &task, card, owner, system, lifecycles)
        }
        NodeEvent::Resumed => {
            let task = task.as_ref();
            let card = task.and_then(|task| domain.held_pause(task)).ok_or_else(|| DomainError::new("not_paused", "The node is not paused"))?;
            let task = task.expect("held above");
            let record = domain.end_of_pause(task, &card, lifecycles)?;
            domain.decide(lifecycles, task, record)?;
            domain.retire(&card);
            Ok(())
        }
        NodeEvent::PauseCleanup { .. } | NodeEvent::PauseReleased => Ok(()),
        NodeEvent::SessionClosed => {
            domain.agents.get_mut(agent).expect("checked above").phase = NodePhase::Closed;
            Ok(())
        }
    }
}
/// The user pauses a node a pause of Alondra's own holds (the gate's, a usage limit): theirs
/// takes its place, so that nothing but the user's resume can put the node back to work.
pub(crate) fn end_pauses_of_alondra(domain: &mut DomainState, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
    let cards: Vec<InteractionId> = {
        let lifecycle = domain.lifecycle(lifecycles, task)?;
        lifecycle.cards().into_iter()
            .filter(|(card, reason)| *reason == alondra_domain::WaitReason::Pause && lifecycle.card_owner(card) != Some(alondra_domain::Owner::User))
            .map(|(card, _)| card.clone()).collect()
    };
    for card in cards {
        if let Some(held) = domain.claude_held().into_iter().find(|held| held.card == card) {
            domain.withdraw_hold(&card);
            domain.resume_held(&held, None, lifecycles)?;
        } else {
            let record = domain.end_of_pause(task, &card, lifecycles)?;
            domain.decide(lifecycles, task, record)?;
            domain.retire(&card);
        }
    }
    Ok(())
}
/// The gate's pause of the node `task` leaves ends with it: the gate sees pauses through
/// nodes, so nothing else would end it. The user's own pause stays until the user resumes it.
pub(crate) fn end_gate_pause(domain: &mut DomainState, task: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
    let Some(card) = domain.held_pause(task) else { return Ok(()) };
    if domain.interactions.get(&card).and_then(pause_of_card).is_none_or(|pause| pause.reason == PauseReason::ByUser) {
        return Ok(());
    }
    let record = domain.end_of_pause(task, &card, lifecycles)?;
    domain.decide(lifecycles, task, record)?;
    domain.retire(&card);
    Ok(())
}
/// The lifecycle of `task` holds the gate's pause `card`, with the card behind it.
pub(crate) fn hold_node_pause(domain: &mut DomainState, task: &TaskId, card: InteractionId, owner: alondra_domain::Owner, system: SystemCard, lifecycles: &mut Lifecycles) -> Result<()> {
    domain.decide(lifecycles, task, super::LifecycleRecord::Paused { card: card.clone(), owner: Some(owner.into()) })?;
    let title = domain.tasks[task].title.clone();
    domain.insert_system_card(node_pause_interaction(card, task, &title, system, domain.revision + 1))?;
    Ok(())
}
