//! The Mac a test's runtime reaches: the doubled git and disks, never the real ones.
use alondra_engine_ports::Registry;
use alondra_engine_ports::disk::Disk;
use alondra_engine_ports::worktrees::Worktrees;
use std::path::PathBuf;
use std::sync::Arc;

/// Gives `registry` the doubled git, which finds sessions' worktrees in the Claude folder
/// `claude_home` names, and the doubled disks.
pub fn wire(registry: &Registry, claude_home: fn() -> PathBuf) {
    registry.provide::<dyn Worktrees>(Arc::new(crate::worktrees::Worktrees::new(claude_home)));
    registry.provide::<dyn Disk>(Arc::new(crate::disk::Disks::default()));
}

