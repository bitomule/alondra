//! How the subsystems hear of the runtime's commits without the runtime knowing them: each one
//! registers a [`CommitObserver`] when the engine opens the runtime, and every commit tells them
//! in that order.
use crate::domain_store::StoreError;
use alondra_domain_rules::{Actor, Command, CommandOutcome, DomainState};

/// What carries out a command in place of the store, once the runtime authorized it.
pub type Carried<'a> = Box<dyn FnOnce() -> Result<CommandOutcome, StoreError> + 'a>;

pub trait CommitObserver: Send + Sync {
    /// Its type's name, so a test can tell two lists of observers apart.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// A command this subsystem carries out itself instead of committing it, such as Resume on the
    /// card of a pause it holds.
    fn carries_out<'a>(&'a self, _domain: &'a dyn alondra_engine_ports::Runtime, _actor: &Actor, _command: &Command) -> Option<Carried<'a>> {
        None
    }

    /// Readers are about to see `after` in place of `before`; the writer lock is still held.
    fn published(&self, _domain: &dyn alondra_engine_ports::Runtime, _before: &DomainState, _after: &DomainState) {}

    /// A command changed the domain to `state`; the writer lock is released.
    fn changed(&self, _domain: &dyn alondra_engine_ports::Runtime, _state: &DomainState) {}

    /// `command`, issued by the daemon or a client rather than through a bearer, changed the domain
    /// to `state`, after every observer heard of the change.
    fn committed(&self, _domain: &dyn alondra_engine_ports::Runtime, _command: &Command, _state: &DomainState) {}
}
