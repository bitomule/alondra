//! What Alondra reads and writes of the spec files in a task's worktrees. The rules about them are
//! the domain's (`alondra_domain_model::specs`); this port only hands it the files.
use alondra_domain_model::specs::{Files, SpecFiles};
use alondra_domain_model::{DomainState, TaskId};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A worktree's files as the spec engine reads them.
pub type SpecSource = Box<dyn Files + Send + Sync>;

pub trait SpecWorktrees: Send + Sync {
    /// The worktree Alondra made for the task, where its specs are proposed and written.
    fn own(&self, state: &DomainState, task: &TaskId) -> Option<PathBuf>;
    /// Every worktree the task still holds: its own and those of the subagents it launched.
    fn all(&self, state: &DomainState, task: &TaskId) -> Vec<PathBuf>;
    /// The files of the commit checked out at `root`, read from git's objects, never from the disk.
    fn head(&self, root: &Path) -> Result<SpecSource, String>;
    /// The files of the commit where the branch at `root` leaves its base branch.
    fn base(&self, root: &Path) -> Result<SpecSource, String>;
    /// The full sha of the commit checked out at `root`.
    fn head_commit(&self, root: &Path) -> Result<String, String>;
    /// The files of `commit`, a full sha, in the repository at `root`, read from git's objects;
    /// fetched from `origin` first when the repository does not have it.
    fn commit(&self, root: &Path, commit: &str) -> Result<SpecSource, String>;
    /// The full sha of the commit where `commit` leaves the base branch of the repository at `root`,
    /// as the repository's own references say: only for what nothing is delivered from.
    fn base_commit(&self, root: &Path, commit: &str) -> Result<String, String>;
    /// The full sha of the best common ancestor of the commits `a` and `b`, read from git's objects
    /// alone: no reference and no replacement object of the repository changes it.
    fn merge_base(&self, root: &Path, a: &str, b: &str) -> Result<String, String>;
    /// The paths that differ between the commits `from` and `to`, in path order.
    fn changed(&self, root: &Path, from: &str, to: &str) -> Result<Vec<String>, String>;
    /// The paths of the worktree at `root` a commit of the whole tree would change: modified,
    /// added, removed, and new files git does not ignore.
    fn uncommitted(&self, root: &Path) -> Result<Vec<String>, String>;
    /// The bytes of the settings file at `root`, `None` when it has none.
    fn settings(&self, root: &Path) -> Result<Option<Vec<u8>>, String>;
    /// Writes `files` into the worktree at `root`, `None` removing one, never outside it nor
    /// through a link: the path and fingerprint of each file it wrote.
    fn write(&self, root: &Path, files: &SpecFiles) -> Result<BTreeMap<String, String>, String>;
}
