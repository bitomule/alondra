//! Deterministic counts of the work the aggregate does on the calling thread: how many events
//! it applied, accepted or refused. Tests read them to bound how often a journal is replayed;
//! nothing decides anything from them.
use std::cell::Cell;

thread_local! {
    static APPLIED: Cell<u64> = const { Cell::new(0) };
}

/// Events applied on this thread so far.
pub fn applied() -> u64 {
    APPLIED.with(Cell::get)
}

pub(crate) fn event_applied() {
    APPLIED.with(|count| count.set(count.get() + 1));
}
