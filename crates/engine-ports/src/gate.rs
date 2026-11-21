//! The admission gate as the rest of the engine sees it: the port `AdmissionGate`, which
//! `engine-admission` provides when the daemon is put together. A test registers a double of it.
use crate::Runtime;
use crate::admission::{AdmissionGate, Hold, Key, Paused, Pauses, Reason, Waiting};
use alondra_core::Millis;
use alondra_domain_rules::{DomainState, TaskId};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

pub trait GateOf {
    fn gate(&self) -> Gate<'_>;
}

impl<R: Runtime> GateOf for R {
    fn gate(&self) -> Gate<'_> {
        gate_of(self)
    }
}

impl GateOf for dyn Runtime {
    fn gate(&self) -> Gate<'_> {
        gate_of(self)
    }
}

fn gate_of(runtime: &dyn Runtime) -> Gate<'_> {
    let port = runtime.ports().port::<dyn AdmissionGate>();
    Gate { domain: runtime, port: port.expect("the daemon gives the runtime its admission gate") }
}

/// The admission gate of one runtime.
pub struct Gate<'a> {
    domain: &'a dyn Runtime,
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
}
