//! What the store issued with commits whose state readers already see: dispatch and control
//! permits, claimed attempts, owner grants, recorded commands and observations. Each SDK owner takes only those
//! of its own task; a commit of the normal command path hands its permits here too.
use alondra_core::domain::TaskId;
use alondra_store::Committed;
use std::sync::{Condvar, Mutex};

#[derive(Default)]
pub(crate) struct Seals {
    queued: Mutex<Vec<Committed>>,
    arrived: Condvar,
}

impl Seals {
    /// Called by the runtime only after it published the commit that issued `committed`.
    pub(crate) fn put(&self, committed: Committed) {
        self.queued.lock().expect("seals poisoned").push(committed);
        self.arrived.notify_all();
    }

    pub(crate) fn take(&self, task: &TaskId) -> Committed {
        take_of(&mut self.queued.lock().expect("seals poisoned"), task)
    }

    /// Blocks until something of `task` arrives, `stopped` says the owner ends, or `timeout` passes.
    pub(crate) fn wait_for(&self, task: &TaskId, timeout: std::time::Duration, stopped: impl Fn() -> bool) -> Committed {
        let deadline = std::time::Instant::now() + timeout;
        let mut queued = self.queued.lock().expect("seals poisoned");
        loop {
            let taken = take_of(&mut queued, task);
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if !empty(&taken) || stopped() || left.is_zero() {
                return taken;
            }
            queued = self.arrived.wait_timeout(queued, left).expect("seals poisoned").0;
        }
    }

    /// Wakes every waiting owner, so one told to stop sees it.
    pub(crate) fn wake(&self) {
        let _queued = self.queued.lock().expect("seals poisoned");
        self.arrived.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn take_all(&self) -> Vec<Committed> {
        std::mem::take(&mut *self.queued.lock().expect("seals poisoned"))
    }
}

pub(crate) fn empty(committed: &Committed) -> bool {
    committed.permits.is_empty()
        && committed.attempts.is_empty()
        && committed.owners.is_empty()
        && committed.commands.is_empty()
        && committed.observations.is_empty()
        && committed.control_permits.is_empty()
        && committed.control_attempts.is_empty()
        && committed.control_commands.is_empty()
}

fn take_of(queued: &mut Vec<Committed>, task: &TaskId) -> Committed {
    fn split<T>(items: &mut Vec<T>, into: &mut Vec<T>, mine: impl Fn(&T) -> bool) {
        let (taken, kept): (Vec<T>, Vec<T>) = std::mem::take(items).into_iter().partition(|item| mine(item));
        *items = kept;
        into.extend(taken);
    }
    let mut taken = Committed::default();
    for committed in queued.iter_mut() {
        split(&mut committed.permits, &mut taken.permits, |permit| permit.execution().task() == task);
        split(&mut committed.attempts, &mut taken.attempts, |attempt| attempt.execution().task() == task);
        split(&mut committed.owners, &mut taken.owners, |owner| owner.execution().task() == task);
        split(&mut committed.commands, &mut taken.commands, |command| command.attempt().execution().task() == task);
        split(&mut committed.observations, &mut taken.observations, |observation| observation.execution().task() == task);
        split(&mut committed.control_permits, &mut taken.control_permits, |permit| permit.execution().task() == task);
        split(&mut committed.control_attempts, &mut taken.control_attempts, |attempt| attempt.execution().task() == task);
        split(&mut committed.control_commands, &mut taken.control_commands, |command| command.attempt().execution().task() == task);
    }
    queued.retain(|committed| !empty(committed));
    taken
}
