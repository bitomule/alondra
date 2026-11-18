//! One implementation of each port, by its type. A holder that panics leaves the map whole: one
//! insert or one lookup each, so a poisoned lock is recovered.
use crate::accounts::Accounts;
use crate::admission::{AdmissionEngine, AdmissionGate};
use crate::agent_host::AgentHost;
use crate::disk::Disk;
use crate::github::GitHub;
use crate::hosts::SdkHosts;
use crate::machine::MachineProbe;
use crate::orchestrator::{OrchestratorAnswers, OrchestratorEngine};
use crate::plugins::Plugins;
use crate::processes::Processes;
use crate::sign_in_returns::SignInReturns;
use alondra_view_model::Screens;
use crate::worktrees::Worktrees;
use alondra_domain_rules::{CommandEnvelope, CommandOutcome, DomainError, DomainState};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};

/// The runtime the ports are called on: the one place the engine keeps its state, and what a piece
/// of the engine asks of it.
pub trait Runtime: Any + Send + Sync {
    fn ports(&self) -> &Registry;
    /// The state each piece of the engine keeps in this runtime: read it with [`crate::extension`].
    fn extensions(&self) -> &crate::extensions::Extensions;
    /// Alondra's home.
    fn home(&self) -> &Path;
    /// The domain's last committed state.
    fn state(&self) -> Arc<DomainState>;
    /// Commits `envelope` as Alondra itself, at whatever revision the domain is at; a storage
    /// failure comes back as the refusal a producer gets.
    fn system_command(&self, envelope: CommandEnvelope) -> Result<CommandOutcome, DomainError>;
    /// Wakes the owners of the SDK hosts to what waits for them.
    fn wake_seals(&self);
    /// The plan `task` starts with, if it has one.
    fn launch_plan(&self, task: &alondra_domain_rules::TaskId) -> Option<alondra_domain_rules::LaunchPlan>;
}

#[derive(Default)]
pub struct Registry(RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync>>>);

impl Registry {
    /// Makes `port` the implementation of `P`, in place of any before it.
    pub fn provide<P: ?Sized + Send + Sync + 'static>(&self, port: Arc<P>) {
        self.0.write().unwrap_or_else(PoisonError::into_inner).insert(TypeId::of::<P>(), Box::new(port));
    }

    /// The implementation of `P` it was given, if any.
    pub fn port<P: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<P>> {
        self.0.read().unwrap_or_else(PoisonError::into_inner).get(&TypeId::of::<P>())?.downcast_ref::<Arc<P>>().cloned()
    }

    /// The Mac's git and task worktrees.
    pub fn worktrees(&self) -> Arc<dyn Worktrees> {
        self.port::<dyn Worktrees>().expect("the root gives the registry the Mac's worktrees")
    }

    /// The provider accounts as the Mac keeps them.
    pub fn accounts(&self) -> Arc<dyn Accounts> {
        self.port::<dyn Accounts>().expect("the root gives the registry the Mac's accounts")
    }

    /// Where Alondra's plugins are installed on the Mac.
    pub fn plugins(&self) -> Arc<dyn Plugins> {
        self.port::<dyn Plugins>().expect("the root gives the registry the Mac's plugins")
    }

    /// Claude's listeners for the returns of MCP sign-ins.
    pub fn sign_in_returns(&self) -> Arc<dyn SignInReturns> {
        self.port::<dyn SignInReturns>().expect("the root gives the registry the Mac's sign-in listeners")
    }

    /// The processes running on the Mac.
    pub fn processes(&self) -> Arc<dyn Processes> {
        self.port::<dyn Processes>().expect("the root gives the registry the Mac's processes")
    }

    /// The Mac's disks.
    pub fn disk(&self) -> Arc<dyn Disk> {
        self.port::<dyn Disk>().expect("the root gives the registry the Mac's disks")
    }

    /// The Mac's gauges.
    pub fn machine(&self) -> Arc<dyn MachineProbe> {
        self.port::<dyn MachineProbe>().expect("the root gives the registry the Mac's gauges")
    }

    /// The ports nothing implements yet, by name.
    pub fn missing(&self) -> Vec<&'static str> {
        let provided = self.0.read().unwrap_or_else(PoisonError::into_inner);
        every_port().into_iter().filter(|(port, _)| !provided.contains_key(port)).map(|(_, name)| name).collect()
    }
}

fn every_port() -> [(TypeId, &'static str); 15] {
    [
        (TypeId::of::<dyn Accounts>(), "Accounts"),
        (TypeId::of::<dyn AgentHost>(), "AgentHost"),
        (TypeId::of::<dyn AdmissionEngine>(), "AdmissionEngine"),
        (TypeId::of::<dyn AdmissionGate>(), "AdmissionGate"),
        (TypeId::of::<dyn Disk>(), "Disk"),
        (TypeId::of::<GitHub>(), "GitHub"),
        (TypeId::of::<dyn MachineProbe>(), "MachineProbe"),
        (TypeId::of::<dyn OrchestratorAnswers>(), "OrchestratorAnswers"),
        (TypeId::of::<dyn OrchestratorEngine>(), "OrchestratorEngine"),
        (TypeId::of::<dyn Plugins>(), "Plugins"),
        (TypeId::of::<dyn Processes>(), "Processes"),
        (TypeId::of::<dyn SdkHosts>(), "SdkHosts"),
        (TypeId::of::<dyn Screens>(), "Screens"),
        (TypeId::of::<dyn SignInReturns>(), "SignInReturns"),
        (TypeId::of::<dyn Worktrees>(), "Worktrees"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Greets: Send + Sync {
        fn greet(&self) -> &'static str;
    }

    struct Hello;

    impl Greets for Hello {
        fn greet(&self) -> &'static str {
            "hello"
        }
    }

    #[test]
    fn a_registry_hands_out_the_port_it_was_given_and_none_it_was_not() {
        let registry = Registry::default();
        assert!(registry.port::<dyn Greets>().is_none());
        registry.provide::<dyn Greets>(Arc::new(Hello));
        assert_eq!(registry.port::<dyn Greets>().unwrap().greet(), "hello");
    }

    #[test]
    fn a_registry_names_every_port_nothing_implements() {
        let registry = Registry::default();
        assert_eq!(registry.missing(), ["Accounts", "AgentHost", "AdmissionEngine", "AdmissionGate", "Disk", "GitHub", "MachineProbe", "OrchestratorAnswers", "OrchestratorEngine", "Plugins", "Processes", "SdkHosts", "Screens", "SignInReturns", "Worktrees"]);
        struct Answers;
        impl OrchestratorAnswers for Answers {
            fn settle_answers(&self, _: &dyn Runtime, _: &alondra_domain_rules::DomainState, _: &std::collections::BTreeSet<alondra_domain_rules::TaskId>) {}
        }
        registry.provide::<dyn OrchestratorAnswers>(Arc::new(Answers));
        assert_eq!(registry.missing(), ["Accounts", "AgentHost", "AdmissionEngine", "AdmissionGate", "Disk", "GitHub", "MachineProbe", "OrchestratorEngine", "Plugins", "Processes", "SdkHosts", "Screens", "SignInReturns", "Worktrees"]);
    }
}
