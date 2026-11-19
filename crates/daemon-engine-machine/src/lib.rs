//! Machine sampling, resource state and native probe retry primitives.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]
#[macro_use]
extern crate alondra_foundation;
use alondra_daemon_support::{locks, node_cost};
use alondra_engine_ports::journal as domain_store;
#[cfg(any(test, feature = "test-hooks"))]
#[path = "../../test_support.rs"]
pub mod test_support;
pub mod machine;
pub mod machine_state;
pub mod native_probe;
pub mod swap_disk;
