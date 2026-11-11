//! A node that keeps sending the same rejected card is stuck: after three rejections in a row
//! Alondra puts a Problem in front of the user with the last error.
use super::*;
use std::collections::BTreeMap;

pub(super) const REJECTIONS_BEFORE_PROBLEM: u32 = 3;

/// A holder that panics leaves it whole, so it is recovered: one remove or one increment of a
/// streak.
#[derive(Default)]
pub(crate) struct Rejections(Mutex<BTreeMap<(String, String), u32>>);

impl Rejections {
    /// True when this rejection is the one that makes the streak long enough for a Problem.
    fn record(&self, key: (String, String), rejected: bool) -> bool {
        let mut streaks = self.0.lock_or_recover("the card rejections");
        if !rejected {
            streaks.remove(&key);
            return false;
        }
        let count = streaks.entry(key).or_insert(0);
        *count += 1;
        *count == REJECTIONS_BEFORE_PROBLEM
    }
}

fn card_command(command: &Command) -> Option<(&InteractionId, Scope)> {
    match command {
        Command::AskQuestion { interaction_id, scope, .. }
        | Command::RequestAction { interaction_id, scope, .. }
        | Command::PublishInformation { interaction_id, scope, .. } => Some((interaction_id, scope.clone())),
        Command::PresentResult { interaction_id, task_id, .. } => {
            Some((interaction_id, Scope::Task { task_id: task_id.clone() }))
        }
        _ => None,
    }
}

impl DomainRuntime {
    pub(super) fn count_rejections(
        &self,
        bearer: &str,
        envelope: &CommandEnvelope,
        result: &Result<(CommandOutcome, bool), StoreError>,
    ) {
        let Some((interaction_id, scope)) = card_command(&envelope.command) else { return };
        let rejection = match result {
            Err(StoreError::Domain(error)) if error.code == "no_effect" => return,
            Err(StoreError::Domain(error)) => Some(error),
            Err(StoreError::Storage(_)) => return,
            Ok(_) => None,
        };
        let Ok(principal) = self.resolve_bearer(bearer) else { return };
        let owner = match principal {
            Principal::Agent { session_id, .. } => session_id,
            Principal::Orchestrator { .. } => "orchestrator".to_owned(),
        };
        let raise = self.rejections.record((owner.clone(), interaction_id.0.clone()), rejection.is_some());
        if let (true, Some(error)) = (raise, rejection) {
            self.raise_stuck_problem(&owner, interaction_id, scope, error);
        }
        let handled = rejection.is_none()
            && matches!(envelope.command, Command::AskQuestion { .. } | Command::PresentResult { .. });
        if handled {
            self.withdraw_stuck_problems(&owner);
        }
    }

    /// The node is handling what got its card refused (it asked or delivered), so the
    /// Problem Alondra raised for it would only be a second card for the same issue.
    fn withdraw_stuck_problems(&self, owner: &str) {
        let Ok(state) = self.state() else { return };
        let open: Vec<InteractionId> = state.interactions.values()
            .filter(|card| matches!(&card.system_card, Some(SystemCard::RefusedCards { owner: of }) if of == owner) && !card.withdrawn)
            .filter(|card| matches!(card.payload, InteractionPayload::Question { answer: None, .. }))
            .map(|card| card.id.clone())
            .collect();
        for id in open {
            let _ = self.withdraw_problem(&id);
        }
    }

    fn raise_stuck_problem(&self, owner: &str, card: &InteractionId, scope: Scope, last: &DomainError) {
        let Ok(state) = self.state() else { return };
        let name = state.agents.get(&AgentId(owner.to_owned())).map_or(owner, |agent| agent.name.as_str()).to_owned();
        let scope = match &scope {
            Scope::Task { task_id } if state.tasks.contains_key(task_id) => scope,
            _ => Scope::Orchestrator { request_id: None },
        };
        let detail = format!(
            "{name} sent card {card} and Alondra refused it {REJECTIONS_BEFORE_PROBLEM} times in a row. Last refusal ({}): {}",
            last.code, last.message
        );
        let need = clip(&format!("{name} is stuck: Alondra refused its card"), CardSpec::NEED_MAX);
        let why = clip(&format!("{} ({})", last.message, last.code), CardSpec::WHY_MAX);
        let _ = self.raise_problem(&format!("problem-{owner}-{card}"), Some(SystemCard::RefusedCards { owner: owner.to_owned() }), Some(&format!("refused-cards:{owner}")), scope, need, why, detail, vec![]);
    }
}
