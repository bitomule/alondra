#![forbid(unsafe_code)]
//! Owned local host processes. Readiness wakeups and child exit are technical observations.
pub mod owned_host;
pub mod artifact_digest;

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "../tests/connect.rs"]
mod connect_tests;
