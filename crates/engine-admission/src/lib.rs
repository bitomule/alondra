//! The admission gate every start, resume and turn passes through (`admission`), the pauses it
//! holds nodes in (`pauses`) and the watch over the Mac's memory and disk it decides on
//! (`machine_watch`). It knows the rest of the engine only through the ports of engine-ports: the
//! engine asks it through `AdmissionGate`, and it asks the engine through `AdmissionEngine` and the
//! hosts' owner through `SdkHosts`. `alondrad` gives the runtime the gate with `wire`.

#[cfg(test)]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_foundation;

use alondra_daemon_support::{auth, locks, node_cost, runtime_mutex, save};
pub mod process_watch;

pub mod disk_peaks;
use alondra_daemon_engine_machine::{machine_state, swap_disk};
pub use alondra_engine_kernel::{domain_database_migration, domain_runtime, domain_store};

pub mod admission;
mod engine;
pub mod machine_hooks;
pub mod machine_watch;
pub mod pauses;

use admission::{Admission, Hold, Key, Waiting};
use alondra_core::Millis;
use alondra_domain_rules::{DomainState, TaskId};
use alondra_engine_kernel::domain_runtime::DomainRuntime;
use alondra_engine_ports::admission::AdmissionGate;
use alondra_engine_ports::{Registry, Runtime};
use machine_watch::MachineWatch;
use pauses::{Paused, Pauses, Reason};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

alondra_engine_kernel::extension!(Admission, |runtime: &dyn alondra_engine_ports::Runtime| Admission::persisted_in(runtime.home()));
alondra_engine_kernel::extension!(MachineWatch, |runtime: &dyn alondra_engine_ports::Runtime| MachineWatch::persisted_in(runtime.home(), runtime.ports().disk()));

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

/// Takes `machine` as a sample of the Mac and `disk` as its own disk's space: a test's fixture
/// makes the Mac healthy, short or full with it.
pub fn measure_machine(domain: &DomainRuntime, machine: &alondra_core::Machine, disk: alondra_engine_ports::disk::Space) {
    machine_watch::pace(domain, machine);
    domain.machine_watch().measure_disk(disk);
}

/// The gate of a test that is not about pacing: unpaced, and its launches see the disk the watch
/// measured, never the free space of the Mac running the test. Production never calls it.
pub fn for_tests(domain: &DomainRuntime) {
    domain.admission().tune(admission::Tuning::UNPACED);
    domain.machine_watch().launch_on_measured_disk();
}

/// Gives the registry the gate the rest of the engine asks.
pub fn wire(registry: &Registry) {
    registry.provide::<dyn AdmissionGate>(Arc::new(Gate));
}

struct Gate;

impl AdmissionGate for Gate {
    fn ask(&self, runtime: &dyn Runtime, waiting: Waiting, now: Millis) -> Result<(), Hold> {
        let domain = DomainRuntime::of(runtime);
        domain.admission().ask(domain, waiting, now)
    }

    fn forget(&self, runtime: &dyn Runtime, key: &Key) {
        let domain = DomainRuntime::of(runtime);
        domain.admission().forget(key);
    }

    fn not_started(&self, runtime: &dyn Runtime, key: &Key) {
        let domain = DomainRuntime::of(runtime);
        domain.admission().not_started(key);
    }

    fn poke(&self, runtime: &dyn Runtime) {
        let domain = DomainRuntime::of(runtime);
        domain.admission().poke();
    }

    fn changes(&self, runtime: &dyn Runtime) -> watch::Receiver<u64> {
        let domain = DomainRuntime::of(runtime);
        domain.admission().subscribe()
    }

    fn may_take_turn(&self, runtime: &dyn Runtime, task: &TaskId, now: Millis) -> bool {
        let domain = DomainRuntime::of(runtime);
        domain.admission().may_take_turn(domain, task, now)
    }

    fn waits_for_slot(&self, runtime: &dyn Runtime, state: &DomainState, task: &TaskId) -> bool {
        let domain = DomainRuntime::of(runtime);
        domain.admission().waits_for_slot(domain, state, task)
    }

    fn owner_ended(&self, runtime: &dyn Runtime, task: &TaskId) {
        let domain = DomainRuntime::of(runtime);
        domain.admission().owner_ended(task);
    }

    fn hold_back(&self, runtime: &dyn Runtime, session: &str, reason: Reason, now: Millis) -> bool {
        let domain = DomainRuntime::of(runtime);
        domain.admission().hold_back(domain, session, reason, now)
    }

    fn pause_by_user(&self, runtime: &dyn Runtime, session: &str, now: Millis) -> Result<(), &'static str> {
        let domain = DomainRuntime::of(runtime);
        domain.admission().pause_by_user(domain, session, now)
    }

    fn resume_now(&self, runtime: &dyn Runtime, session: &str, now: Millis) -> bool {
        let domain = DomainRuntime::of(runtime);
        domain.admission().resume_now(domain, session, now)
    }

    fn release_by_user_message(&self, runtime: &dyn Runtime, session: &str) {
        let domain = DomainRuntime::of(runtime);
        domain.admission().release_by_user_message(domain, session);
    }

    fn paused(&self, runtime: &dyn Runtime, session: &str) -> Option<Paused> {
        let domain = DomainRuntime::of(runtime);
        domain.admission().paused(domain, session)
    }

    fn paused_by_user(&self, runtime: &dyn Runtime, session: &str) -> bool {
        let domain = DomainRuntime::of(runtime);
        domain.admission().paused_by_user(domain, session)
    }

    fn holding(&self, runtime: &dyn Runtime, session: &str) -> Option<Reason> {
        let domain = DomainRuntime::of(runtime);
        domain.admission().holding(domain, session)
    }

    fn pauses(&self, runtime: &dyn Runtime) -> Pauses {
        let domain = DomainRuntime::of(runtime);
        domain.admission().pauses(domain)
    }

    fn held_by_user(&self, runtime: &dyn Runtime, task: &TaskId) -> bool {
        let domain = DomainRuntime::of(runtime);
        admission::held_by_user(domain, task)
    }

    fn queued_text(&self, runtime: &dyn Runtime, task: &TaskId, now: Millis) -> String {
        let domain = DomainRuntime::of(runtime);
        admission::queued_text(domain, task, now)
    }

    fn refuse_launch(&self, runtime: &dyn Runtime, cwd: &Path) -> Option<String> {
        let domain = DomainRuntime::of(runtime);
        domain.machine_watch().refuse_launch(cwd)
    }

    fn disk_low(&self, runtime: &dyn Runtime) -> bool {
        let domain = DomainRuntime::of(runtime);
        domain.machine_watch().disk_low()
    }
}

/// The gate's tests open a runtime with the gate and doubles of the rest of the engine: hosts that
/// run nothing and an engine scripted by the test.
#[cfg(test)]
mod wiring {
    use alondra_engine_doubles::admission::Engine;
    use std::sync::Arc;

    pub fn open(home: &std::path::Path) -> anyhow::Result<super::DomainRuntime> {
        let domain = super::DomainRuntime::open_unwired_at(home, &crate::domain_database_migration::active_path(home))?;
        crate::wire(domain.ports());
        domain.ports().provide::<dyn alondra_engine_ports::hosts::SdkHosts>(Arc::new(alondra_engine_doubles::hosts::NoHosts));
        Engine::wire(domain.ports());
        crate::for_tests(&domain);
        Ok(domain)
    }

    pub fn engine(domain: &super::DomainRuntime) -> Arc<Engine> {
        Engine::of(domain)
    }
}
