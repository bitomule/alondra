//! The cases of the shared domain v2 catalog the SQLite store's runners execute: the store runner's and
//! the schedule runner's. alondrad checks the catalog against both lists, because the rest of its
//! evidence names tests of crates above the store.
pub const CASES: &[&str] = &[
    "V01", "V03", "V06", "V07", "V08", "V09", "V10", "V11", "V16", "V17", "V18", "V20", "V21",
    "V23", "V24", "V25", "V45", "V50", "V52",
];
pub const SCHEDULE_CASES: &[&str] = &["V54", "V55", "V56", "V57", "V58", "V62"];
