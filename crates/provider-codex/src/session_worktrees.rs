//! Codex makes worktrees only when asked to: `codex --worktree` and its worktree tools in the
//! terminal UI, `codex exec --worktree`, and the desktop app. Alondra runs Codex as
//! `codex app-server` in the task's folder, which offers none of them, and Codex subagents work in
//! their parent's folder, so a task's Codex sessions make no worktree of their own.
//! https://github.com/openai/codex/tree/main/codex-rs/worktree
use alondra_provider_contract::{Conversations, SessionWorktree, SessionWorktrees};

pub struct NoSessionWorktrees;

impl SessionWorktrees for NoSessionWorktrees {
    fn made_by(&self, _: &Conversations<'_>) -> Vec<SessionWorktree> {
        vec![]
    }
}
