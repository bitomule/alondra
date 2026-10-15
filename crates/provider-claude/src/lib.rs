#![forbid(unsafe_code)]
//! Private Claude worker transport behind committed application dispatch authority.
//!
//! Terminal observations still require the real Task/card application transaction;
//! the controller deliberately leaves those observations unacknowledged.
mod adapter;
mod normalizer;
mod records;
mod wire;

pub use adapter::{ClaudeConfiguration, ClaudeWorker};

#[cfg(test)]
mod controller_tests;
