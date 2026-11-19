//! Deterministic counts of the work the domain's rules do on the calling thread. Tests read them
//! to bound how often a transition reads the cards, or a screen the tasks; nothing decides
//! anything from them.
use std::cell::Cell;

thread_local! {
    static CARDS_READ: Cell<u64> = const { Cell::new(0) };
    static PAUSE_QUERIES: Cell<u64> = const { Cell::new(0) };
    static TASKS_READ: Cell<u64> = const { Cell::new(0) };
    static NODE_CHANGE_ENTRIES_READ: Cell<u64> = const { Cell::new(0) };
}

/// Tasks looked at on this thread so far to find the task of a node.
pub fn tasks_read() -> u64 {
    TASKS_READ.with(Cell::get)
}

pub(super) fn task_read() {
    TASKS_READ.with(|count| count.set(count.get() + 1));
}

/// Retained node-change entries examined by snapshot delta queries on this thread.
pub fn node_change_entries_read() -> u64 {
    NODE_CHANGE_ENTRIES_READ.with(Cell::get)
}

pub(super) fn node_change_entry_read() {
    NODE_CHANGE_ENTRIES_READ.with(|count| count.set(count.get() + 1));
}

/// Cards the rules read on this thread so far: each one a rule examined or indexed.
pub fn cards_read() -> u64 {
    CARDS_READ.with(Cell::get)
}

pub(super) fn card_read() {
    CARDS_READ.with(|count| count.set(count.get() + 1));
}

/// Indexed pause resolutions on this thread.
pub fn pause_queries() -> u64 {
    PAUSE_QUERIES.with(Cell::get)
}

pub(super) fn pause_query() {
    PAUSE_QUERIES.with(|count| count.set(count.get() + 1));
}
