//! What the store issued with commits whose state readers already see: dispatch and control
//! permits, claimed attempts, owner grants, recorded commands and observations. Each SDK owner takes only those
//! of its own task; a commit of the normal command path hands its permits here too.
use crate::locks::{Poisonable as _, AfterPanic as _};
use alondra_core::domain::TaskId;
use alondra_store::Committed;
use std::sync::{Condvar, Mutex};

#[derive(Default)]
pub(crate) struct Seals {
    /// A holder that panics can leave it half-changed, so the daemon exits: `take_of` moves one-
    /// shot permits out by field; lost ones would strand a task, and a fresh start derives them
    /// again from the store.
    queued: Mutex<Vec<Committed>>,
    arrived: Condvar,
}

impl Seals {
    /// Called by the runtime only after it published the commit that issued `committed`.
    pub(crate) fn put(&self, committed: Committed) {
        self.queued.lock_or_exit("the SDK seals").push(committed);
        self.arrived.notify_all();
    }

    pub(crate) fn take(&self, task: &TaskId) -> Committed {
        take_of(&mut self.queued.lock_or_exit("the SDK seals"), task)
    }

    /// Blocks until something of `task` arrives, `stopped` says the owner ends, or `timeout` passes.
    pub(crate) fn wait_for(&self, task: &TaskId, timeout: std::time::Duration, stopped: impl Fn() -> bool) -> Committed {
        let deadline = std::time::Instant::now() + timeout;
        let mut queued = self.queued.lock_or_exit("the SDK seals");
        loop {
            let taken = take_of(&mut queued, task);
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if !empty(&taken) || stopped() || left.is_zero() {
                return taken;
            }
            queued = self.arrived.wait_timeout(queued, left).or_exit("the SDK seals").0;
        }
    }

    /// Blocks for `timeout`, or until `stopped` says the owner ends, taking nothing.
    pub(crate) fn pause(&self, timeout: std::time::Duration, stopped: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + timeout;
        let mut queued = self.queued.lock_or_exit("the SDK seals");
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if stopped() || left.is_zero() {
                return;
            }
            queued = self.arrived.wait_timeout(queued, left).or_exit("the SDK seals").0;
        }
    }

    /// The observations of `task` committed from the journal of a host that ended: no host is left
    /// to acknowledge them, and every owner would scan them on each wake.
    pub(crate) fn forget_observations(&self, task: &TaskId) {
        let mut queued = self.queued.lock_or_exit("the SDK seals");
        for committed in queued.iter_mut() {
            committed.observations.retain(|observation| observation.execution().task() != task);
        }
        queued.retain(|committed| !empty(committed));
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn observations_of(&self, task: &TaskId) -> usize {
        self.queued.lock_or_exit("the SDK seals").iter()
            .map(|committed| committed.observations.iter().filter(|observation| observation.execution().task() == task).count())
            .sum()
    }

    /// Wakes every waiting owner, so one told to stop sees it.
    pub(crate) fn wake(&self) {
        let _queued = self.queued.lock_or_exit("the SDK seals");
        self.arrived.notify_all();
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn take_all(&self) -> Vec<Committed> {
        std::mem::take(&mut *self.queued.lock_or_exit("the SDK seals"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locks::{assert_exited_for, child_home, in_child};

    #[test]
    #[ignore = "run by taking_seals_after_a_panic_holding_them_exits_the_daemon"]
    fn queued_child() {
        if child_home().is_none() { return; }
        let seals = Seals::default();
        crate::locks::poison(&seals.queued);
        let _ = seals.take_all();
        unreachable!("the seals were taken while poisoned");
    }

    #[test]
    fn taking_seals_after_a_panic_holding_them_exits_the_daemon() {
        let home = crate::test_support::Directory::new();
        assert_exited_for(&in_child("sdk_owner::seals::tests::queued_child", &home), "the SDK seals");
    }
}
