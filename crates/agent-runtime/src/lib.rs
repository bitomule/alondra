#![forbid(unsafe_code)]
//! Owned local host processes. Readiness wakeups and child exit are technical observations.
pub mod owned_host;

#[cfg(test)]
#[path = "../tests/connect.rs"]
mod connect_tests;
