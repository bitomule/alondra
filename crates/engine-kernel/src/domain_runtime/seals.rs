//! What the store issued with commits whose state readers already see: dispatch and control
//! permits, claimed attempts, owner grants, recorded commands and observations. Each SDK owner takes only those
//! of its own execution: its task and its node. A node the user kept when its task closed still has
//! an owner after the task reopens, and the new execution's grant and permits are never its own.
//! A commit of the normal command path hands its permits here too.
use alondra_core::clock;
use crate::locks::{Poisonable as _, AfterPanic as _};
use alondra_domain_rules::{AgentId, TaskId};
use alondra_foundation::ExecutionStamp;
use alondra_store::Committed;

#[derive(Default)]
pub struct Seals {
    /// A holder that panics can leave it half-changed, so the daemon exits: `take_of` moves one-
    /// shot permits out by field; lost ones would strand a task, and a fresh start derives them
    /// again from the store.
    queued: clock::Monitor<Vec<Committed>>,
    #[cfg(test)]
    before_wake: std::sync::Mutex<Option<Box<dyn FnMut() + Send>>>,
}

impl Seals {
    /// Called by the runtime only after it published the commit that issued `committed`.
    pub fn put(&self, committed: Committed) {
        self.queued.lock_or_exit("the SDK seals").push(committed);
        self.queued.notify_all();
    }

    pub fn take(&self, task: &TaskId, node: &AgentId) -> Committed {
        take_of(&mut self.queued.lock_or_exit("the SDK seals"), task, node)
    }

    /// Blocks until something of `node`'s execution of `task` arrives, `stopped` says the owner
    /// ends, or `timeout` passes.
    pub fn wait_for(&self, task: &TaskId, node: &AgentId, timeout: std::time::Duration, stopped: impl Fn() -> bool) -> Committed {
        let deadline = clock::instant() + timeout;
        let mut queued = self.queued.lock_or_exit("the SDK seals");
        loop {
            let taken = take_of(&mut queued, task, node);
            let left = deadline.saturating_duration_since(clock::instant());
            if !empty(&taken) || stopped() || left.is_zero() {
                return taken;
            }
            queued = self.queued.wait(queued, left).or_exit("the SDK seals");
        }
    }

    /// Blocks for `timeout`, or until `stopped` says the owner ends, taking nothing.
    pub fn pause(&self, timeout: std::time::Duration, stopped: impl Fn() -> bool) {
        let deadline = clock::instant() + timeout;
        let mut queued = self.queued.lock_or_exit("the SDK seals");
        loop {
            let left = deadline.saturating_duration_since(clock::instant());
            if stopped() || left.is_zero() {
                return;
            }
            queued = self.queued.wait(queued, left).or_exit("the SDK seals");
        }
    }

    /// The observations of `task` committed from the journal of a host that ended: no host is left
    /// to acknowledge them, and every owner would scan them on each wake.
    pub fn forget_observations(&self, task: &TaskId) {
        let mut queued = self.queued.lock_or_exit("the SDK seals");
        for committed in queued.iter_mut() {
            committed.observations.retain(|observation| observation.execution().task() != task);
        }
        queued.retain(|committed| !empty(committed));
    }

    pub fn observations_of(&self, task: &TaskId) -> usize {
        self.queued.lock_or_exit("the SDK seals").iter()
            .map(|committed| committed.observations.iter().filter(|observation| observation.execution().task() == task).count())
            .sum()
    }

    /// Wakes every waiting owner, so one told to stop sees it.
    pub fn wake(&self) {
        let _queued = self.queued.lock_or_exit("the SDK seals");
        #[cfg(test)]
        if let Some(observe) = self.before_wake.lock().unwrap().as_mut() {
            observe();
        }
        self.queued.notify_all();
    }

    #[cfg(test)]
    pub(super) fn on_wake(&self, observe: impl FnMut() + Send + 'static) {
        *self.before_wake.lock().unwrap() = Some(Box::new(observe));
    }

    pub fn take_all(&self) -> Vec<Committed> {
        std::mem::take(&mut *self.queued.lock_or_exit("the SDK seals"))
    }
}

pub fn empty(committed: &Committed) -> bool {
    committed.permits.is_empty()
        && committed.attempts.is_empty()
        && committed.owners.is_empty()
        && committed.commands.is_empty()
        && committed.observations.is_empty()
        && committed.control_permits.is_empty()
        && committed.control_attempts.is_empty()
        && committed.control_commands.is_empty()
}

fn take_of(queued: &mut Vec<Committed>, task: &TaskId, node: &AgentId) -> Committed {
    fn split<T>(items: &mut Vec<T>, into: &mut Vec<T>, mine: impl Fn(&T) -> bool) {
        let (taken, kept): (Vec<T>, Vec<T>) = std::mem::take(items).into_iter().partition(|item| mine(item));
        *items = kept;
        into.extend(taken);
    }
    let ours = |execution: &ExecutionStamp| execution.task() == task && execution.node() == node;
    let mut taken = Committed::default();
    for committed in queued.iter_mut() {
        split(&mut committed.permits, &mut taken.permits, |permit| ours(permit.execution()));
        split(&mut committed.attempts, &mut taken.attempts, |attempt| ours(attempt.execution()));
        split(&mut committed.owners, &mut taken.owners, |owner| ours(owner.execution()));
        split(&mut committed.commands, &mut taken.commands, |command| ours(command.attempt().execution()));
        split(&mut committed.observations, &mut taken.observations, |observation| ours(observation.execution()));
        split(&mut committed.control_permits, &mut taken.control_permits, |permit| ours(permit.execution()));
        split(&mut committed.control_attempts, &mut taken.control_attempts, |attempt| ours(attempt.execution()));
        split(&mut committed.control_commands, &mut taken.control_commands, |command| ours(command.attempt().execution()));
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
        crate::locks::poison_monitor(&seals.queued);
        let _ = seals.take_all();
        unreachable!("the seals were taken while poisoned");
    }

    #[test]
    fn taking_seals_after_a_panic_holding_them_exits_the_daemon() {
        let home = crate::test_support::Directory::new();
        assert_exited_for(&in_child("domain_runtime::seals::tests::queued_child", &home), "the SDK seals");
    }
}
