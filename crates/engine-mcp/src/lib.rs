//! The engine's MCP servers: what a node waits for while a server signs in, the sign-in's return,
//! and the checks and deletions the user asks for on the MCP servers screen. It sits on
//! `engine-kernel` and names no other engine subsystem: the SDK backend reaches it as a trait.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]

#[cfg(any(test, feature = "test-hooks"))]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_daemon_support;

use alondra_daemon_support::{auth, locks, log};
use alondra_engine_kernel::{domain_runtime, domain_store, extension};

pub mod mcp_host;
pub mod mcp_probe;
pub mod mcp_remove;
pub mod mcp_sign_in;
pub mod mcp_waits;
