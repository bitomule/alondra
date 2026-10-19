//! Deterministic counts of the work the store does on the calling thread: how many lifecycle
//! transactions it attached, how many times it rebuilt a task's aggregate from its journal and how
//! many journal events it decoded. Tests read
//! them to bound a transaction's work; nothing decides anything from them.
use std::cell::Cell;

thread_local! {
    static TRANSACTIONS: Cell<u64> = const { Cell::new(0) };
    static AGGREGATE_LOADS: Cell<u64> = const { Cell::new(0) };
    static EVENT_DECODES: Cell<u64> = const { Cell::new(0) };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub transactions: u64,
    pub aggregate_loads: u64,
    pub event_decodes: u64,
}

impl std::ops::Sub for Counts {
    type Output = Counts;
    fn sub(self, earlier: Counts) -> Counts {
        Counts {
            transactions: self.transactions - earlier.transactions,
            aggregate_loads: self.aggregate_loads - earlier.aggregate_loads,
            event_decodes: self.event_decodes - earlier.event_decodes,
        }
    }
}

pub fn counts() -> Counts {
    Counts {
        transactions: TRANSACTIONS.with(Cell::get),
        aggregate_loads: AGGREGATE_LOADS.with(Cell::get),
        event_decodes: EVENT_DECODES.with(Cell::get),
    }
}

pub(crate) fn transaction_attached() {
    TRANSACTIONS.with(|count| count.set(count.get() + 1));
}

pub(crate) fn aggregate_loaded() {
    AGGREGATE_LOADS.with(|count| count.set(count.get() + 1));
}

pub(crate) fn event_decoded() {
    EVENT_DECODES.with(|count| count.set(count.get() + 1));
}
