//! The admission gate as the rest of the engine sees it: the kernel's port, which `engine-admission`
//! provides when the daemon is put together. The engine's own tests open a runtime without it, and
//! their gate lets every start and turn through at once.
use crate::domain_runtime::DomainRuntime;
use crate::ports::admission::{AdmissionGate, Hold, Key, Paused, Pauses, Reason, Waiting};
use alondra_core::Millis;
use alondra_domain_rules::{DomainState, TaskId};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

pub trait GateOf {
    fn gate(&self) -> Gate<'_>;
}

impl GateOf for DomainRuntime {
    fn gate(&self) -> Gate<'_> {
        let port = self.port::<dyn AdmissionGate>();
        #[cfg(any(test, feature = "test-hooks"))]
        let port = port.or_else(|| Some(open::gate()));
        Gate { domain: self, port: port.expect("the daemon gives the runtime its admission gate") }
    }
}

/// The admission gate of one runtime.
pub struct Gate<'a> {
    domain: &'a DomainRuntime,
    port: Arc<dyn AdmissionGate>,
}

impl Gate<'_> {
    pub fn ask(&self, waiting: Waiting, now: Millis) -> Result<(), Hold> {
        self.port.ask(self.domain, waiting, now)
    }

    pub fn forget(&self, key: &Key) {
        self.port.forget(self.domain, key);
    }

    pub fn not_started(&self, key: &Key) {
        self.port.not_started(self.domain, key);
    }

    pub fn poke(&self) {
        self.port.poke(self.domain);
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.port.changes(self.domain)
    }

    pub fn may_take_turn(&self, task: &TaskId, now: Millis) -> bool {
        self.port.may_take_turn(self.domain, task, now)
    }

    pub fn waits_for_slot(&self, state: &DomainState, task: &TaskId) -> bool {
        self.port.waits_for_slot(self.domain, state, task)
    }

    pub fn owner_ended(&self, task: &TaskId) {
        self.port.owner_ended(self.domain, task);
    }

    pub fn hold_back(&self, session: &str, reason: Reason, now: Millis) -> bool {
        self.port.hold_back(self.domain, session, reason, now)
    }

    pub fn pause_by_user(&self, session: &str, now: Millis) -> Result<(), &'static str> {
        self.port.pause_by_user(self.domain, session, now)
    }

    pub fn resume_now(&self, session: &str, now: Millis) -> bool {
        self.port.resume_now(self.domain, session, now)
    }

    pub fn release_by_user_message(&self, session: &str) {
        self.port.release_by_user_message(self.domain, session);
    }

    pub fn paused(&self, session: &str) -> Option<Paused> {
        self.port.paused(self.domain, session)
    }

    pub fn paused_by_user(&self, session: &str) -> bool {
        self.port.paused_by_user(self.domain, session)
    }

    pub fn holding(&self, session: &str) -> Option<Reason> {
        self.port.holding(self.domain, session)
    }

    pub fn pauses(&self) -> Pauses {
        self.port.pauses(self.domain)
    }

    pub fn held_by_user(&self, task: &TaskId) -> bool {
        self.port.held_by_user(self.domain, task)
    }

    pub fn queued_text(&self, task: &TaskId, now: Millis) -> String {
        self.port.queued_text(self.domain, task, now)
    }

    pub fn refuse_launch(&self, cwd: &Path) -> Option<String> {
        self.port.refuse_launch(self.domain, cwd)
    }

    pub fn disk_low(&self) -> bool {
        self.port.disk_low(self.domain)
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn measure_machine(&self, machine: &alondra_core::Machine, disk: crate::disk::Space) {
        self.port.measure_machine(self.domain, machine, disk);
    }
}

/// The user's Resume on the card of a pause the gate holds a node of the Agent SDK in: the gate,
/// not the domain, ends that pause, so it is the Resume of the node's task.
pub struct PauseResume;

impl crate::domain_runtime::CommitObserver for PauseResume {
    fn carries_out<'a>(&'a self, domain: &'a DomainRuntime, actor: &alondra_domain_rules::Actor, command: &alondra_domain_rules::Command) -> Option<crate::domain_runtime::Carried<'a>> {
        use alondra_domain_rules::{Actor, Command, Scope, NODE_PAUSE_RESUME};
        let (Actor::User { .. }, Command::AnswerQuestion { interaction_id, option_id, .. }) = (actor, command) else { return None };
        if option_id != NODE_PAUSE_RESUME {
            return None;
        }
        let state = domain.state().ok()?;
        let card = state.interactions.get(interaction_id).filter(|card| state.node_pause_card(&card.id) && state.open(card))?;
        let Scope::Task { task_id } = &card.scope else { return None };
        let agent = state.tasks.get(task_id)?.agent_id.clone()?;
        Some(Box::new(move || {
            if !domain.gate().resume_now(&agent.0, alondra_core::now_millis()) {
                return Err(crate::domain_store::StoreError::Domain(alondra_domain_rules::DomainError::new("conflict", "The node is no longer paused")));
            }
            let revision = domain.state().map_err(crate::domain_store::StoreError::Storage)?.revision;
            Ok(alondra_domain_rules::CommandOutcome { revision, changed: false, effects: vec![], created_ids: vec![], superseded_acceptances: vec![],
                effect: vec!["The paused node resumes now.".into()] })
        }))
    }
}

/// The gate of a runtime nobody gave one: it holds nothing back and keeps no pause.
#[cfg(any(test, feature = "test-hooks"))]
mod open {
    use super::*;
    use std::sync::LazyLock;

    struct Open(watch::Sender<u64>);

    static OPEN: LazyLock<Arc<Open>> = LazyLock::new(|| Arc::new(Open(watch::Sender::new(0))));

    pub(super) fn gate() -> Arc<dyn AdmissionGate> {
        OPEN.clone()
    }

    impl AdmissionGate for Open {
        fn ask(&self, _: &DomainRuntime, _: Waiting, _: Millis) -> Result<(), Hold> {
            Ok(())
        }
        fn forget(&self, _: &DomainRuntime, _: &Key) {}
        fn not_started(&self, _: &DomainRuntime, _: &Key) {}
        fn poke(&self, _: &DomainRuntime) {}
        fn changes(&self, _: &DomainRuntime) -> watch::Receiver<u64> {
            self.0.subscribe()
        }
        fn may_take_turn(&self, _: &DomainRuntime, _: &TaskId, _: Millis) -> bool {
            true
        }
        fn waits_for_slot(&self, _: &DomainRuntime, _: &DomainState, _: &TaskId) -> bool {
            false
        }
        fn owner_ended(&self, _: &DomainRuntime, _: &TaskId) {}
        fn hold_back(&self, _: &DomainRuntime, _: &str, _: Reason, _: Millis) -> bool {
            false
        }
        fn pause_by_user(&self, _: &DomainRuntime, _: &str, _: Millis) -> Result<(), &'static str> {
            Err("This runtime has no admission gate")
        }
        fn resume_now(&self, _: &DomainRuntime, _: &str, _: Millis) -> bool {
            false
        }
        fn release_by_user_message(&self, _: &DomainRuntime, _: &str) {}
        fn paused(&self, _: &DomainRuntime, _: &str) -> Option<Paused> {
            None
        }
        fn paused_by_user(&self, _: &DomainRuntime, _: &str) -> bool {
            false
        }
        fn holding(&self, _: &DomainRuntime, _: &str) -> Option<Reason> {
            None
        }
        fn pauses(&self, _: &DomainRuntime) -> Pauses {
            Pauses::default()
        }
        fn held_by_user(&self, _: &DomainRuntime, _: &TaskId) -> bool {
            false
        }
        fn queued_text(&self, _: &DomainRuntime, _: &TaskId, _: Millis) -> String {
            "It waits in the queue for a free slot.".into()
        }
        fn refuse_launch(&self, _: &DomainRuntime, _: &Path) -> Option<String> {
            None
        }
        fn disk_low(&self, _: &DomainRuntime) -> bool {
            false
        }
    }
}
