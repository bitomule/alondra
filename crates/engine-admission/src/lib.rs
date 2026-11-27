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

use alondra_daemon_support::{locks, node_cost, runtime_mutex, save};
pub mod process_watch;

pub mod disk_peaks;
#[cfg(test)]
use alondra_engine_kernel::{domain_runtime, domain_runtime::DomainRuntime, domain_store};

pub mod admission;
mod engine;
pub mod machine_hooks;
pub mod machine_state;
pub mod machine_watch;
pub mod pauses;
pub mod swap_disk;

use admission::{Admission, Hold, Key, Waiting};
use alondra_core::Millis;
use alondra_domain_model::{DomainState, TaskId};
use alondra_engine_ports::admission::AdmissionGate;
use alondra_engine_ports::{AsRuntime as _, Registry, Runtime};
use machine_watch::MachineWatch;
use pauses::{Paused, Pauses, Reason};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

alondra_engine_ports::extension!(Admission, |runtime: &dyn alondra_engine_ports::Runtime| Admission::persisted_in(runtime.home()));
alondra_engine_ports::extension!(MachineWatch, |runtime: &dyn alondra_engine_ports::Runtime| MachineWatch::persisted_in(runtime.home(), runtime.ports().disk()));

/// The gate and the machine watch of a runtime, kept as its extensions.
pub trait AdmissionOf {
    fn admission(&self) -> &Admission;
    fn machine_watch(&self) -> &MachineWatch;
}

impl<R: Runtime + ?Sized> AdmissionOf for R {
    fn admission(&self) -> &Admission {
        alondra_engine_ports::extension(self.as_runtime())
    }

    fn machine_watch(&self) -> &MachineWatch {
        alondra_engine_ports::extension(self.as_runtime())
    }
}

/// Takes `machine` as a sample of the Mac and `disk` as its own disk's space: a test's fixture
/// makes the Mac healthy, short or full with it.
pub fn measure_machine(domain: &dyn Runtime, machine: &alondra_core::Machine, disk: alondra_engine_ports::disk::Space) {
    machine_watch::pace(domain, machine);
    domain.machine_watch().measure_disk(disk);
}

/// The gate of a test that is not about pacing: unpaced, and its launches see the disk the watch
/// measured, never the free space of the Mac running the test. Production never calls it.
pub fn for_tests(domain: &dyn Runtime) {
    domain.admission().tune(admission::Tuning::UNPACED);
    domain.machine_watch().launch_on_measured_disk();
}

/// Gives the registry the gate the rest of the engine asks, and its controls the server asks.
pub fn wire(registry: &Registry) {
    registry.provide::<dyn AdmissionGate>(Arc::new(Gate));
    registry.provide::<dyn alondra_engine_ports::domain_api::AdmissionApi>(Arc::new(Gate));
}

impl alondra_engine_ports::domain_api::AdmissionApi for Gate {
    fn pacing(&self, runtime: &dyn Runtime, now: Millis) -> Option<alondra_view_model::LivePacing> {
        runtime.admission().live(runtime, now)
    }

    fn hold_queued(&self, runtime: &dyn Runtime, task: &TaskId, now: Millis) -> Result<(), &'static str> {
        admission::hold_queued(runtime, task, now)
    }

    fn release_queued(&self, runtime: &dyn Runtime, task: &TaskId) -> Result<(), &'static str> {
        admission::release_queued(runtime, task)
    }

    fn most_working_nodes(&self, runtime: &dyn Runtime) -> u32 {
        admission::most_working_nodes(runtime)
    }

    fn set_node_limit(&self, home: &Path, limit: alondra_core::options::NodeLimit) -> Result<(), &'static str> {
        admission::set_node_limit(home, limit)
    }

    fn declare_machine_wait(&self, runtime: &dyn Runtime, session: &str, resource: Option<alondra_core::machine_incident::MachineResource>, reason: &str, turn: u64, now: Millis) {
        runtime.machine_watch().declare(session, resource, reason, turn, now);
    }
}

struct Gate;

impl AdmissionGate for Gate {
    fn ask(&self, runtime: &dyn Runtime, waiting: Waiting, now: Millis) -> Result<(), Hold> {
        runtime.admission().ask(runtime, waiting, now)
    }

    fn forget(&self, runtime: &dyn Runtime, key: &Key) {
        runtime.admission().forget(key);
    }

    fn not_started(&self, runtime: &dyn Runtime, key: &Key) {
        runtime.admission().not_started(key);
    }

    fn poke(&self, runtime: &dyn Runtime) {
        runtime.admission().poke();
    }

    fn changes(&self, runtime: &dyn Runtime) -> watch::Receiver<u64> {
        runtime.admission().subscribe()
    }

    fn may_take_turn(&self, runtime: &dyn Runtime, task: &TaskId, now: Millis) -> bool {
        runtime.admission().may_take_turn(runtime, task, now)
    }

    fn waits_for_slot(&self, runtime: &dyn Runtime, state: &DomainState, task: &TaskId) -> bool {
        runtime.admission().waits_for_slot(runtime, state, task)
    }

    fn owner_ended(&self, runtime: &dyn Runtime, task: &TaskId) {
        runtime.admission().owner_ended(task);
    }

    fn hold_back(&self, runtime: &dyn Runtime, session: &str, reason: Reason, now: Millis) -> bool {
        runtime.admission().hold_back(runtime, session, reason, now)
    }

    fn pause_by_user(&self, runtime: &dyn Runtime, session: &str, now: Millis) -> Result<(), &'static str> {
        runtime.admission().pause_by_user(runtime, session, now)
    }

    fn resume_now(&self, runtime: &dyn Runtime, session: &str, now: Millis) -> bool {
        runtime.admission().resume_now(runtime, session, now)
    }

    fn release_by_user_message(&self, runtime: &dyn Runtime, session: &str) {
        runtime.admission().release_by_user_message(runtime, session);
    }

    fn paused(&self, runtime: &dyn Runtime, session: &str) -> Option<Paused> {
        runtime.admission().paused(runtime, session)
    }

    fn paused_by_user(&self, runtime: &dyn Runtime, session: &str) -> bool {
        runtime.admission().paused_by_user(runtime, session)
    }

    fn holding(&self, runtime: &dyn Runtime, session: &str) -> Option<Reason> {
        runtime.admission().holding(runtime, session)
    }

    fn pauses(&self, runtime: &dyn Runtime) -> Pauses {
        runtime.admission().pauses(runtime)
    }

    fn held_by_user(&self, runtime: &dyn Runtime, task: &TaskId) -> bool {
        admission::held_by_user(runtime, task)
    }

    fn queued_text(&self, runtime: &dyn Runtime, task: &TaskId, now: Millis) -> String {
        admission::queued_text(runtime, task, now)
    }

    fn refuse_launch(&self, runtime: &dyn Runtime, cwd: &Path) -> Option<String> {
        runtime.machine_watch().refuse_launch(cwd)
    }

    fn disk_low(&self, runtime: &dyn Runtime) -> bool {
        runtime.machine_watch().disk_low()
    }
}

/// The gate's tests open a runtime with the gate and doubles of the rest of the engine: hosts that
/// run nothing and an engine scripted by the test.
#[cfg(test)]
mod wiring {
    use alondra_engine_doubles::admission::Engine;
    use std::sync::Arc;

    pub fn open(home: &std::path::Path) -> anyhow::Result<super::DomainRuntime> {
        alondra_foundation::durability::relax();
        alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules));
        let domain = super::DomainRuntime::open_unwired_at(home, &crate::domain_store::active_path(home))?;
        alondra_engine_doubles::machine::wire(domain.ports(), alondra_daemon_support::watcher::claude_home);
        crate::wire(domain.ports());
        domain.ports().provide::<dyn alondra_engine_ports::hosts::SdkHosts>(Arc::new(alondra_engine_doubles::hosts::NoHosts));
        Engine::wire(domain.ports());
        crate::for_tests(&domain);
        Ok(domain)
    }

    pub fn engine(domain: &dyn alondra_engine_ports::Runtime) -> Arc<Engine> {
        Engine::of(domain)
    }
}
