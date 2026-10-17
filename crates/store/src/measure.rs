//! Deterministic counts of the work the store does on the calling thread: how many times it
//! rebuilt a task's aggregate from its journal and how many journal events it decoded. Tests read
//! them to bound a transaction's work; nothing decides anything from them.
use std::cell::Cell;

thread_local! {
    static AGGREGATE_LOADS: Cell<u64> = const { Cell::new(0) };
    static EVENT_DECODES: Cell<u64> = const { Cell::new(0) };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub aggregate_loads: u64,
    pub event_decodes: u64,
}

impl std::ops::Sub for Counts {
    type Output = Counts;
    fn sub(self, earlier: Counts) -> Counts {
        Counts {
            aggregate_loads: self.aggregate_loads - earlier.aggregate_loads,
            event_decodes: self.event_decodes - earlier.event_decodes,
        }
    }
}

pub fn counts() -> Counts {
    Counts {
        aggregate_loads: AGGREGATE_LOADS.with(Cell::get),
        event_decodes: EVENT_DECODES.with(Cell::get),
    }
}

pub(crate) fn aggregate_loaded() {
    AGGREGATE_LOADS.with(|count| count.set(count.get() + 1));
}

pub(crate) fn event_decoded() {
    EVENT_DECODES.with(|count| count.set(count.get() + 1));
}
