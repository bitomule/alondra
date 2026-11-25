//! Antigravity makes no worktrees of its own when Alondra runs it as `agy acp` in the task's
//! folder: its subagents work in their parent's workspace, so a task's Antigravity sessions name
//! no worktree.
use alondra_provider_contract::{Conversations, SessionWorktree, SessionWorktrees};

pub struct NoSessionWorktrees;

impl SessionWorktrees for NoSessionWorktrees {
    fn made_by(&self, _: &Conversations<'_>) -> Vec<SessionWorktree> {
        vec![]
    }
}
