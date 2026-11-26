//! The domain's rules: what each command changes in the state the model (`alondra_domain_model`)
//! defines, and the passes every command runs after it. Only the roots and the tests that apply
//! commands link this crate; everything else reads the model and reaches the rules through the
//! `DomainRules` port, which the root hands to the store when it opens it.
use alondra_domain_model::*;

mod invariants;
mod rules;
#[cfg(test)]
use rules::*;
#[cfg(test)]
use std::sync::Arc;

pub use invariants::check_every_button;
#[cfg(test)]
mod answered_card_tests;
#[cfg(test)]
mod attention_tests;
#[cfg(test)]
mod card_contract_tests;
#[cfg(test)]
mod card_protocol_tests;
#[cfg(test)]
mod card_tests;
#[cfg(test)]
mod checklist_tests;
#[cfg(test)]
mod cited_files_tests;
#[cfg(test)]
mod close_after_acceptance_tests;
#[cfg(test)]
mod compaction_tests;
#[cfg(test)]
mod confirmed_effect_tests;
#[cfg(test)]
mod continuation_tests;
#[cfg(test)]
mod decision_tests;
#[cfg(test)]
mod invariants_tests;
#[cfg(test)]
mod journal_tests;
#[cfg(test)]
mod lifecycle_cache_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
pub(crate) mod mcp_wait_tests;
#[cfg(test)]
mod move_tests;
#[cfg(test)]
mod node_machine_tests;
#[cfg(test)]
mod open_pr_close_tests;
#[cfg(test)]
mod ownership_tests;
#[cfg(test)]
mod plan_phases_tests;
#[cfg(test)]
mod plan_review_tests;
#[cfg(test)]
mod plan_wait_tests;
#[cfg(test)]
mod post_merge_tests;
#[cfg(test)]
mod post_merge_wait_tests;
#[cfg(test)]
mod pr_identity_review_tests;
#[cfg(test)]
mod pr_identity_tests;
#[cfg(test)]
mod proposed_plan_tests;
#[cfg(test)]
mod queue_pause_tests;
#[cfg(test)]
mod node_changes_tests;
#[cfg(test)]
mod read_index_tests;
#[cfg(test)]
mod refusal_tests;
#[cfg(test)]
mod request_outcome_tests;
#[cfg(test)]
mod request_over_tests;
#[cfg(test)]
mod request_plan_close_tests;
#[cfg(test)]
mod sdk_node_tests;
#[cfg(test)]
mod settled_delivery_tests;
#[cfg(test)]
pub(crate) mod several_prs_tests;
#[cfg(test)]
mod situation_tests;
#[cfg(test)]
mod spec_tests;
#[cfg(test)]
mod specs_tests;
#[cfg(test)]
mod validation_tests;
#[cfg(test)]
mod stage_tests;
#[cfg(test)]
mod stall_tests;
#[cfg(test)]
mod suggestion_tests;
#[cfg(test)]
mod system_card_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transition_cost_tests;
#[cfg(test)]
mod turn_end_wait_tests;
#[cfg(test)]
mod usage_limit_tests;
#[cfg(test)]
mod worktree_close_tests;

/// The rules the root hands to the store.
pub struct Rules;

impl DomainRules for Rules {
    fn transition(&self, state: &DomainState, actor: &Actor, envelope: &CommandEnvelope) -> Result<(DomainState, CommandOutcome), DomainError> {
        rules::transition(state, actor, envelope)
    }
    fn anticipate(&self, state: &DomainState, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome, DomainError> {
        rules::prepare_transition(state, actor, envelope).map(|(_, outcome)| outcome)
    }
}

/// Commands applied straight on a state, for the tests that drive the domain without a store.
pub trait Apply {
    /// Applies `envelope`: the state changes only if the whole command is accepted.
    fn execute(&mut self, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome, DomainError>;
    /// The state `envelope` leaves and its outcome; this one stays as it was.
    fn transition(&self, actor: &Actor, envelope: &CommandEnvelope) -> Result<(DomainState, CommandOutcome), DomainError>;
    /// The outcome of `envelope`, or its refusal, without applying it.
    fn preview(&self, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome, DomainError>;
}

impl Apply for DomainState {
    fn execute(&mut self, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome, DomainError> {
        rules::execute(self, actor, envelope)
    }
    fn transition(&self, actor: &Actor, envelope: &CommandEnvelope) -> Result<(DomainState, CommandOutcome), DomainError> {
        rules::transition(self, actor, envelope)
    }
    fn preview(&self, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome, DomainError> {
        Rules.anticipate(self, actor, envelope)
    }
}
