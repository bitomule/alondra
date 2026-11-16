//! Deterministic counts of the work the domain's rules do on the calling thread. Tests read them
//! to bound how often a transition reads the cards; nothing decides anything from them.
use std::cell::Cell;

thread_local! {
    static CARDS_READ: Cell<u64> = const { Cell::new(0) };
}

/// Cards the rules read on this thread so far: each one a rule examined or indexed.
pub fn cards_read() -> u64 {
    CARDS_READ.with(Cell::get)
}

pub(super) fn card_read() {
    CARDS_READ.with(|count| count.set(count.get() + 1));
}
