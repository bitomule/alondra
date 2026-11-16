//! The engine's worktrees: the one each task's node works in, recorded as the task's and released
//! once it is gone, those of cancelled tasks, and the startup moves of HOME files to their current
//! names. It sits on `engine-kernel` and names no other engine subsystem.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]

#[cfg(any(test, feature = "test-hooks"))]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_daemon_support;

use alondra_daemon_support::{auth, domain_cutover, locks, worktree};
use alondra_engine_kernel::{domain_database_migration, domain_runtime};

pub mod cancelled_worktrees;
pub mod home_renames;
pub mod task_worktrees;
