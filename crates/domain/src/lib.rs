#![forbid(unsafe_code)]
//! Task-owned execution algebra. Persistence and authenticated adapters validate external facts.
//! No SDK, process, storage or view implementation belongs to this crate.
mod lifecycle;
pub use lifecycle::*;
