//! The providers' notes of the worktrees a task's sessions made, as a test writes them.
use alondra_engine_ports::worktrees::{Conversations, SessionWorktree, SessionWorktrees};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

/// Each worktree noted for the conversations whose node runs as an agent, or whose record is
/// byte for byte the one the test names.
#[derive(Clone, Default)]
pub struct Noted(Arc<Mutex<Vec<(Vec<u8>, SessionWorktree)>>>);

impl Noted {
    /// Notes `path`, on `branch`, for the conversations named by `agent_or_record`.
    pub fn note(&self, agent_or_record: impl AsRef<[u8]>, path: &Path, branch: Option<&str>) {
        let worktree = SessionWorktree { path: path.to_owned(), branch: branch.map(str::to_owned) };
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push((agent_or_record.as_ref().to_vec(), worktree));
    }
}

impl SessionWorktrees for Noted {
    fn made_by(&self, conversations: &Conversations<'_>) -> Vec<SessionWorktree> {
        let names = |key: &[u8]| conversations.agent.is_some_and(|agent| agent.as_bytes() == key) || conversations.records.iter().any(|record| record == key);
        self.0.lock().unwrap_or_else(PoisonError::into_inner).iter().filter(|(key, _)| names(key)).map(|(_, worktree)| worktree.clone()).collect()
    }
}
