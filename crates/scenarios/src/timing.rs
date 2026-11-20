//! Where the real time of a case goes. The scenario's clock is virtual, so these read the system's
//! monotonic clock: they say how long the runner waited, never how long the scenario lasted.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Totals {
    assemble: Duration,
    accounts: Duration,
    steps: BTreeMap<String, Duration>,
    settle: Duration,
    checkpoints: Duration,
    teardown: Duration,
    advance: Duration,
    passes: u64,
    idle_passes: u64,
    settles: u64,
    polls: u64,
    resets: u64,
    owner_wakes: u64,
    probing: Duration,
}

/// The real time each phase of a case took.
#[derive(Clone, Default)]
pub struct Timings(Arc<Mutex<Totals>>);

impl Timings {
    fn totals(&self) -> std::sync::MutexGuard<'_, Totals> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn assembled(&self, since: Instant) {
        self.totals().assemble += since.elapsed();
    }

    pub fn accounts_checked(&self, since: Instant) {
        self.totals().accounts += since.elapsed();
    }

    pub fn stepped(&self, kind: String, since: Instant) {
        *self.totals().steps.entry(kind).or_default() += since.elapsed();
    }

    pub fn settled(&self, since: Instant) {
        self.totals().settle += since.elapsed();
    }

    pub fn checkpointed(&self, since: Instant) {
        self.totals().checkpoints += since.elapsed();
    }

    /// One advance of the virtual clock; `idle` when no pending deadline fell within it, so no settle
    /// followed.
    pub fn advanced(&self, since: Instant, idle: bool) {
        let mut totals = self.totals();
        totals.advance += since.elapsed();
        totals.passes += 1;
        totals.idle_passes += u64::from(idle);
    }

    /// What one wait for calm did: how many looks it took, how many times activity restarted the
    /// count, whether it had to outlast the owner's wake, and how long the looks themselves took.
    pub fn polled(&self, polls: u64, resets: u64, owner_wake: bool, probing: Duration) {
        let mut totals = self.totals();
        totals.settles += 1;
        totals.polls += polls;
        totals.resets += resets;
        totals.owner_wakes += u64::from(owner_wake);
        totals.probing += probing;
    }

    pub fn torn_down(&self, since: Instant) {
        self.totals().teardown += since.elapsed();
    }

    /// One line: each phase and the steps by kind, longest first. The checkpoints are steps too, so
    /// they are also counted inside the steps.
    pub fn line(&self) -> String {
        let totals = self.totals();
        let settling = format!(
            "\n  passes {} ({} idle, advance+yield {:.2} s) · settles {} · polls {} · resets {} · owner-wake settles {} · poll bodies {:.2} s",
            totals.passes,
            totals.idle_passes,
            totals.advance.as_secs_f64(),
            totals.settles,
            totals.polls,
            totals.resets,
            totals.owner_wakes,
            totals.probing.as_secs_f64(),
        );
        let mut kinds: Vec<_> = totals.steps.iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(a.1));
        let by_kind: Vec<String> = kinds.iter().map(|(kind, time)| format!("{kind} {:.2}", time.as_secs_f64())).collect();
        format!(
            "  assemble {:.2} s · accounts {:.2} s · steps {:.2} s ({}; settle {:.2}) · checkpoints {:.2} s · teardown {:.2} s",
            totals.assemble.as_secs_f64(),
            totals.accounts.as_secs_f64(),
            totals.steps.values().sum::<Duration>().as_secs_f64(),
            by_kind.join(", "),
            totals.settle.as_secs_f64(),
            totals.checkpoints.as_secs_f64(),
            totals.teardown.as_secs_f64(),
        ) + &settling
    }
}

/// The name of a step's kind, as the case file writes it.
pub fn kind(step: &impl std::fmt::Debug) -> String {
    let text = format!("{step:?}");
    let end = text.find([' ', '(', '{']).unwrap_or(text.len());
    text[..end].to_owned()
}
