//! The engine's worktrees: the one each task's node works in, recorded as the task's and released
//! once it is gone, those of cancelled tasks, and the startup moves of HOME files to their current
//! names. It sits on `engine-kernel` and names no other engine subsystem.

#[cfg(test)]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_foundation;

use alondra_daemon_support::{domain_cutover, locks};

pub mod cancelled_worktrees;
pub mod home_renames;
pub mod task_worktrees;
