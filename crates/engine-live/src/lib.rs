//! The engine's live view of each node: what its mod reports or its Agent SDK or Codex host
//! observes (the call it runs, what it said last, its subagents, workflows and task list), and what
//! each SDK task measured of its conversation. Nothing here moves a node's state, which the domain
//! alone decides. It sits on `engine-kernel` and names no other engine subsystem.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]

#[cfg(any(test, feature = "test-hooks"))]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_foundation;

use alondra_daemon_support::{hook_text, job_state, locks};
use alondra_engine_kernel::{domain_runtime, extension, metrics_capture};

pub mod codex_live;
pub mod contexts;
pub mod node_reports;
pub mod node_work;
pub mod sdk_live;
