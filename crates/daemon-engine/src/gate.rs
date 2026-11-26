//! The admission gate's port and its wrapper live in `engine-ports`; the engine adds the Resume of a
//! node the gate pauses.
use alondra_engine_ports::Runtime;
pub use alondra_engine_ports::gate::{Gate, GateOf};

/// The user's Resume on the card of a pause the gate holds a node of the Agent SDK in: the gate,
/// not the domain, ends that pause, so it is the Resume of the node's task.
pub struct PauseResume;

impl crate::domain_runtime::CommitObserver for PauseResume {
    fn carries_out<'a>(&'a self, domain: &'a dyn Runtime, actor: &alondra_domain_model::Actor, command: &alondra_domain_model::Command) -> Option<crate::domain_runtime::Carried<'a>> {
        use alondra_domain_model::{Actor, Command, Scope, NODE_PAUSE_RESUME};
        let (Actor::User { .. }, Command::AnswerQuestion { interaction_id, option_id, .. }) = (actor, command) else { return None };
        if option_id != NODE_PAUSE_RESUME {
            return None;
        }
        let state = domain.state();
        let card = state.interactions.get(interaction_id).filter(|card| state.node_pause_card(&card.id) && state.open(card))?;
        let Scope::Task { task_id } = &card.scope else { return None };
        let agent = state.tasks.get(task_id)?.agent_id.clone()?;
        Some(Box::new(move || {
            if !domain.gate().resume_now(&agent.0, alondra_core::now_millis()) {
                return Err(crate::domain_store::StoreError::Domain(alondra_domain_model::DomainError::new("conflict", "The node is no longer paused")));
            }
            let revision = domain.state().revision;
            Ok(alondra_domain_model::CommandOutcome { revision, changed: false, effects: vec![], created_ids: vec![], superseded_acceptances: vec![],
                effect: vec!["The paused node resumes now.".into()] })
        }))
    }
}
