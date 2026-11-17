//! The admission gate every start, resume and turn passes through (`admission`), the pauses it
//! holds nodes in (`pauses`) and the watch over the Mac's memory and disk it decides on
//! (`machine_watch`). It sits above `daemon-engine`, which asks it through the kernel's port
//! (`AdmissionGate`); `alondrad` gives the runtime that port with `wire`.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]

#[cfg(any(test, feature = "test-hooks"))]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_daemon_support;

use alondra_daemon_engine::{
    api_failures, domain_agent_lifecycle, hosts, launch_plans, options, domain_messages, domain_orchestrator_tools, live, machine, node_push, node_reports, sdk_handover, start_failures, usage_pause,
};
#[cfg(test)]
use alondra_daemon_engine::{domain_holds, domain_transport, home_renames, usage_probe};
use alondra_daemon_support::{auth, disk, disk_peaks, locks, node_cost, process_watch, runtime_mutex, save};
use alondra_daemon_engine_machine::{machine_state, swap_disk};
pub use alondra_engine_kernel::{domain_database_migration, domain_runtime, domain_store};

pub mod admission;
#[cfg(test)]
mod engine_tests;
#[cfg(any(test, feature = "test-hooks"))]
pub mod machine_hooks;
pub mod machine_watch;
pub mod pauses;

use admission::{Admission, Hold, Key, Waiting};
use alondra_core::Millis;
use alondra_domain_rules::{DomainState, TaskId};
use alondra_engine_kernel::domain_runtime::DomainRuntime;
use alondra_engine_kernel::ports::admission::AdmissionGate;
use machine_watch::MachineWatch;
use pauses::{Paused, Pauses, Reason};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

alondra_engine_kernel::extension!(Admission, |domain: &DomainRuntime| Admission::persisted_in(domain.home()));
alondra_engine_kernel::extension!(MachineWatch, |domain: &DomainRuntime| MachineWatch::persisted_in(domain.home()));

/// The gate and the machine watch of a runtime, kept as its extensions.
pub trait AdmissionOf {
    fn admission(&self) -> &Admission;
    fn machine_watch(&self) -> &MachineWatch;
}

impl AdmissionOf for DomainRuntime {
    fn admission(&self) -> &Admission {
        self.extension()
    }

    fn machine_watch(&self) -> &MachineWatch {
        self.extension()
    }
}

/// Gives `domain` the gate the rest of the engine asks.
pub fn wire(domain: &DomainRuntime) {
    domain.provide::<dyn AdmissionGate>(Arc::new(Gate));
}

struct Gate;

impl AdmissionGate for Gate {
    fn ask(&self, domain: &DomainRuntime, waiting: Waiting, now: Millis) -> Result<(), Hold> {
        domain.admission().ask(domain, waiting, now)
    }

    fn forget(&self, domain: &DomainRuntime, key: &Key) {
        domain.admission().forget(key);
    }

    fn not_started(&self, domain: &DomainRuntime, key: &Key) {
        domain.admission().not_started(key);
    }

    fn poke(&self, domain: &DomainRuntime) {
        domain.admission().poke();
    }

    fn changes(&self, domain: &DomainRuntime) -> watch::Receiver<u64> {
        domain.admission().subscribe()
    }

    fn may_take_turn(&self, domain: &DomainRuntime, task: &TaskId, now: Millis) -> bool {
        domain.admission().may_take_turn(domain, task, now)
    }

    fn waits_for_slot(&self, domain: &DomainRuntime, state: &DomainState, task: &TaskId) -> bool {
        domain.admission().waits_for_slot(domain, state, task)
    }

    fn owner_ended(&self, domain: &DomainRuntime, task: &TaskId) {
        domain.admission().owner_ended(task);
    }

    fn hold_back(&self, domain: &DomainRuntime, session: &str, reason: Reason, now: Millis) -> bool {
        domain.admission().hold_back(domain, session, reason, now)
    }

    fn pause_by_user(&self, domain: &DomainRuntime, session: &str, now: Millis) -> Result<(), &'static str> {
        domain.admission().pause_by_user(domain, session, now)
    }

    fn resume_now(&self, domain: &DomainRuntime, session: &str, now: Millis) -> bool {
        domain.admission().resume_now(domain, session, now)
    }

    fn release_by_user_message(&self, domain: &DomainRuntime, session: &str) {
        domain.admission().release_by_user_message(domain, session);
    }

    fn paused(&self, domain: &DomainRuntime, session: &str) -> Option<Paused> {
        domain.admission().paused(domain, session)
    }

    fn paused_by_user(&self, domain: &DomainRuntime, session: &str) -> bool {
        domain.admission().paused_by_user(domain, session)
    }

    fn holding(&self, domain: &DomainRuntime, session: &str) -> Option<Reason> {
        domain.admission().holding(domain, session)
    }

    fn pauses(&self, domain: &DomainRuntime) -> Pauses {
        domain.admission().pauses(domain)
    }

    fn held_by_user(&self, domain: &DomainRuntime, task: &TaskId) -> bool {
        admission::held_by_user(domain, task)
    }

    fn queued_text(&self, domain: &DomainRuntime, task: &TaskId, now: Millis) -> String {
        admission::queued_text(domain, task, now)
    }

    fn refuse_launch(&self, domain: &DomainRuntime, cwd: &Path) -> Option<String> {
        domain.machine_watch().refuse_launch(cwd)
    }

    fn disk_low(&self, domain: &DomainRuntime) -> bool {
        domain.machine_watch().disk_low()
    }

    #[cfg(any(test, feature = "test-hooks"))]
    fn measure_machine(&self, domain: &DomainRuntime, machine: &alondra_core::Machine, disk: disk::Space) {
        machine_watch::pace(domain, machine);
        domain.machine_watch().measure_disk(disk);
    }
}

/// The engine as `alondrad` puts it together, for the gate's tests.
#[cfg(test)]
mod wiring {
    pub fn open(home: &std::path::Path) -> anyhow::Result<super::DomainRuntime> {
        let domain = alondra_daemon_engine::wiring::open(home)?;
        crate::wire(&domain);
        Ok(domain)
    }
}
