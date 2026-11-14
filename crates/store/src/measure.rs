//! Deterministic counts of the work the store does on the calling thread: how many lifecycle
//! transactions it attached, how many times it rebuilt a task's aggregate from its journal, how
//! many journal events it decoded and how many stored inputs it looked up. Tests read
//! them to bound a transaction's work; nothing decides anything from them.
use std::cell::Cell;

thread_local! {
    static TRANSACTIONS: Cell<u64> = const { Cell::new(0) };
    static AGGREGATE_LOADS: Cell<u64> = const { Cell::new(0) };
    static EVENT_DECODES: Cell<u64> = const { Cell::new(0) };
    static INPUT_CHECKS: Cell<u64> = const { Cell::new(0) };
    static SCHEMA_VERIFICATIONS: Cell<u64> = const { Cell::new(0) };
    static SCHEMA_QUERIES: Cell<u64> = const { Cell::new(0) };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub transactions: u64,
    pub aggregate_loads: u64,
    pub event_decodes: u64,
    pub input_checks: u64,
    /// Full validations of the journal's table, index and trigger definitions.
    pub schema_verifications: u64,
    /// Reads of SQLite's schema catalog during those validations.
    pub schema_queries: u64,
}

impl std::ops::Sub for Counts {
    type Output = Counts;
    fn sub(self, earlier: Counts) -> Counts {
        Counts {
            transactions: self.transactions - earlier.transactions,
            aggregate_loads: self.aggregate_loads - earlier.aggregate_loads,
            event_decodes: self.event_decodes - earlier.event_decodes,
            input_checks: self.input_checks - earlier.input_checks,
            schema_verifications: self.schema_verifications - earlier.schema_verifications,
            schema_queries: self.schema_queries - earlier.schema_queries,
        }
    }
}

pub fn counts() -> Counts {
    Counts {
        transactions: TRANSACTIONS.with(Cell::get),
        aggregate_loads: AGGREGATE_LOADS.with(Cell::get),
        event_decodes: EVENT_DECODES.with(Cell::get),
        input_checks: INPUT_CHECKS.with(Cell::get),
        schema_verifications: SCHEMA_VERIFICATIONS.with(Cell::get),
        schema_queries: SCHEMA_QUERIES.with(Cell::get),
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

pub(crate) fn input_checked() {
    INPUT_CHECKS.with(|count| count.set(count.get() + 1));
}

pub(crate) fn schema_verified() {
    SCHEMA_VERIFICATIONS.with(|count| count.set(count.get() + 1));
}

pub(crate) fn schema_queried() {
    SCHEMA_QUERIES.with(|count| count.set(count.get() + 1));
}
