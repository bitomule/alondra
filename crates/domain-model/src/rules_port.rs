//! The port to the domain's rules. The model defines the state, its reads and the guards every
//! screen shares; what a command changes lives in `alondra_domain_rules`, which only the roots and
//! the tests that apply commands link. The root hands its rules to the store when it opens it, and
//! everything else reaches them through the store. Loading a stored state needs no rule: it
//! replays each task's journal through the lifecycle authority, which the model keeps.
use super::*;

/// The rules that apply commands to the domain's state.
pub trait DomainRules: Send + Sync {
    /// The state `envelope` leaves and its outcome, decided on a copy of `state`, which stays as
    /// it was.
    fn transition(&self, state: &DomainState, actor: &Actor, envelope: &CommandEnvelope) -> Result<(DomainState, CommandOutcome), DomainError>;
    /// What `envelope` would do to `state` without applying it: its outcome, or why it is refused.
    fn anticipate(&self, state: &DomainState, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome, DomainError>;
}
