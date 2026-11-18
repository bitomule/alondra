//! The processes of a test's Mac: the table a test sets, and the signals the engine sends, recorded
//! instead of sent.
use alondra_engine_ports::Registry;
use alondra_engine_ports::processes::{Process, Processes as Port};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Default)]
pub struct Processes {
    table: Mutex<Vec<Process>>,
    signals: Mutex<Vec<(Vec<u32>, i32)>>,
}

/// Gives `registry` these processes, both as the port and as themselves for the test to set.
pub fn wire(registry: &Registry) -> Arc<Processes> {
    let processes = Arc::new(Processes::default());
    registry.provide::<dyn Port>(processes.clone());
    registry.provide::<Processes>(processes.clone());
    processes
}

/// The doubled processes `registry` was given.
pub fn of(registry: &Registry) -> Arc<Processes> {
    registry.port::<Processes>().expect("the test's runtime has the doubled processes")
}

impl Processes {
    /// The processes running from now on.
    pub fn set_table(&self, table: Vec<Process>) {
        *self.table.lock().unwrap_or_else(PoisonError::into_inner) = table;
    }

    /// The signals sent so far, with the processes each went to.
    pub fn signals(&self) -> Vec<(Vec<u32>, i32)> {
        self.signals.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Port for Processes {
    fn table(&self) -> Vec<Process> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    fn signal(&self, pids: &[u32], signal: i32) {
        self.signals.lock().unwrap_or_else(PoisonError::into_inner).push((pids.to_vec(), signal));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_doubled_processes_keep_the_contract_the_macs_keep() {
        let processes = Processes::default();
        let row = |pid: u32, parent: u32| Process { pid: pid as i32, parent: parent as i32, group: pid as i32, argv: vec![] };
        processes.set_table(vec![row(std::process::id(), 1), row(1, 0)]);
        crate::contract::processes(&processes);
        processes.signal(&[7, 8], libc::SIGKILL);
        assert_eq!(processes.signals().last(), Some(&(vec![7, 8], libc::SIGKILL)), "a signal is recorded, never sent");
    }
}
