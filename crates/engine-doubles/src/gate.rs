//! The admission gate of a runtime that tests no admission: it holds nothing back and keeps no
//! pause.
use alondra_core::Millis;
use alondra_domain_model::{DomainState, TaskId};
use alondra_engine_ports::Runtime;
use alondra_engine_ports::admission::{AdmissionGate, Hold, Key, Paused, Pauses, Reason, Waiting};
use std::path::Path;
use tokio::sync::watch;

pub struct OpenGate(watch::Sender<u64>);

impl Default for OpenGate {
    fn default() -> Self {
        Self(watch::Sender::new(0))
    }
}

impl AdmissionGate for OpenGate {
    fn ask(&self, _: &dyn Runtime, _: Waiting, _: Millis) -> Result<(), Hold> {
        Ok(())
    }
    fn forget(&self, _: &dyn Runtime, _: &Key) {}
    fn not_started(&self, _: &dyn Runtime, _: &Key) {}
    fn poke(&self, _: &dyn Runtime) {}
    fn changes(&self, _: &dyn Runtime) -> watch::Receiver<u64> {
        self.0.subscribe()
    }
    fn may_take_turn(&self, _: &dyn Runtime, _: &TaskId, _: Millis) -> bool {
        true
    }
    fn waits_for_slot(&self, _: &dyn Runtime, _: &DomainState, _: &TaskId) -> bool {
        false
    }
    fn owner_ended(&self, _: &dyn Runtime, _: &TaskId) {}
    fn hold_back(&self, _: &dyn Runtime, _: &str, _: Reason, _: Millis) -> bool {
        false
    }
    fn pause_by_user(&self, _: &dyn Runtime, _: &str, _: Millis) -> Result<(), &'static str> {
        Err("This runtime has no admission gate")
    }
    fn resume_now(&self, _: &dyn Runtime, _: &str, _: Millis) -> bool {
        false
    }
    fn release_by_user_message(&self, _: &dyn Runtime, _: &str) {}
    fn paused(&self, _: &dyn Runtime, _: &str) -> Option<Paused> {
        None
    }
    fn paused_by_user(&self, _: &dyn Runtime, _: &str) -> bool {
        false
    }
    fn holding(&self, _: &dyn Runtime, _: &str) -> Option<Reason> {
        None
    }
    fn pauses(&self, _: &dyn Runtime) -> Pauses {
        Pauses::default()
    }
    fn held_by_user(&self, _: &dyn Runtime, _: &TaskId) -> bool {
        false
    }
    fn queued_text(&self, _: &dyn Runtime, _: &TaskId, _: Millis) -> String {
        "It waits in the queue for a free slot.".into()
    }
    fn refuse_launch(&self, _: &dyn Runtime, _: &Path) -> Option<String> {
        None
    }
    fn disk_low(&self, _: &dyn Runtime) -> bool {
        false
    }
}
