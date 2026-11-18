//! The Mac as the engine reaches it: git and the task worktrees, the disks, and the files and
//! checks only the daemon itself makes. The engine asks for them through the `Worktrees` and
//! `Disk` ports of `engine-ports`; the root gives the registry this crate's implementations.
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[macro_use]
extern crate alondra_foundation;

pub mod claude_check;
pub mod disk;
pub mod iroh_terminals;
pub mod launch_worktree;
pub mod secret_files;
pub mod worktree;
pub mod worktrees_root;

use alondra_core::options::WorktreesRoot;
use alondra_engine_ports::Registry;
use alondra_engine_ports::disk::{Disk, Mounts, Space};
use alondra_engine_ports::worktrees::{Git, LaunchError, Protected, Repository, TaskWorktree, WorktreeOrigin, Workplace, Worktrees};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Gives the registry the Mac's disks and its git, which finds the sessions' worktrees in the
/// Claude folder `claude_home` names.
pub fn wire(registry: &Registry, claude_home: fn() -> PathBuf) {
    registry.provide::<dyn Worktrees>(Arc::new(GitWorktrees { claude_home }));
    registry.provide::<dyn Disk>(Arc::new(Disks));
}

pub struct Disks;

impl Disk for Disks {
    fn space(&self, path: &Path) -> Option<Space> {
        disk::space(path)
    }
    fn held(&self, path: &Path) -> Option<u64> {
        disk::held(path)
    }
    fn mount_point(&self, path: &Path) -> Option<PathBuf> {
        disk::mount_point(path)
    }
    fn same_volume(&self, a: &Path, b: &Path) -> bool {
        disk::same_volume(a, b)
    }
    fn mounted_volumes(&self) -> std::io::Result<Mounts> {
        disk::mounted_volumes()
    }
}

pub struct GitWorktrees {
    claude_home: fn() -> PathBuf,
}

impl Worktrees for GitWorktrees {
    fn worktree_of(&self, session_id: &str) -> Option<PathBuf> {
        worktree::worktree_of(&(self.claude_home)(), session_id)
    }
    fn origin(&self, session_id: &str) -> Option<WorktreeOrigin> {
        worktree::origin_in(&(self.claude_home)(), session_id)
    }
    fn subagent_worktree(&self, own: &Path, agent: &str) -> Option<PathBuf> {
        worktree::subagent_worktree(own, agent)
    }
    fn registrable(&self, path: &Path, protected: &Protected) -> Result<(), String> {
        worktree::registrable(path, protected)
    }
    fn branch_in<'a>(&'a self, dir: &'a Path) -> Git<'a, Option<String>> {
        Box::pin(worktree::branch_in(dir))
    }
    fn leftovers<'a>(&'a self, session_id: &'a str) -> Git<'a, Option<String>> {
        Box::pin(async move { worktree::leftovers(&(self.claude_home)(), session_id).await })
    }
    fn release_task_worktree<'a>(&'a self, dir: &'a Path, slug: &'a str, remove: bool) -> Git<'a, Result<TaskWorktree, String>> {
        Box::pin(worktree::release_task_worktree(dir, slug, remove))
    }
    fn release_recorded_worktree<'a>(&'a self, dir: &'a Path, give_up: bool) -> Git<'a, Result<TaskWorktree, String>> {
        Box::pin(worktree::release_recorded_worktree(dir, give_up))
    }
    fn discard_worktree<'a>(&'a self, dir: &'a Path, keep_branch: bool) -> Git<'a, Result<TaskWorktree, String>> {
        Box::pin(worktree::discard_worktree(dir, keep_branch))
    }
    fn unreachable_link(&self, dir: &Path) -> Option<String> {
        worktree::unreachable_link(dir)
    }
    fn uncommitted_files_of_alondras_worktree<'a>(&'a self, dir: &'a Path) -> Git<'a, Result<Option<usize>, String>> {
        Box::pin(worktree::uncommitted_files_of_alondras_worktree(dir))
    }
    fn unsaved<'a>(&'a self, top: &'a Path) -> Git<'a, Result<Option<String>, String>> {
        Box::pin(worktree::unsaved(top))
    }
    fn isolate<'a>(&'a self, cwd: &'a Path, slug: &'a str, known: Option<&'a Path>, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<Workplace, LaunchError>> {
        Box::pin(launch_worktree::isolate_in(cwd, slug, known, pr_head, root))
    }
    fn reopen<'a>(&'a self, cwd: &'a Path, slug: &'a str, original: &'a Path, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<(Workplace, bool), LaunchError>> {
        Box::pin(launch_worktree::reopen_in(cwd, slug, original, pr_head, root))
    }
    fn storage_for<'a>(&'a self, cwd: &'a Path, slug: &'a str, known: Option<&'a Path>, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<PathBuf, LaunchError>> {
        Box::pin(launch_worktree::storage_for(cwd, slug, known, pr_head, root))
    }
    fn recovery_directory_matches<'a>(&'a self, cwd: &'a Path, original: &'a Path) -> Git<'a, bool> {
        Box::pin(launch_worktree::recovery_directory_matches(cwd, original))
    }
    fn chosen(&self, workplace: &Workplace) -> Option<PathBuf> {
        launch_worktree::chosen(workplace).map(Path::to_path_buf)
    }
    fn stored(&self, worktree: &Path) -> Option<Repository> {
        worktrees_root::stored(worktree)
    }
    fn restore(&self, worktree: &Path, record: &Repository) -> Result<(), String> {
        worktrees_root::restore(worktree, record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    static CLAUDE_HOME: OnceLock<PathBuf> = OnceLock::new();

    fn claude_home() -> PathBuf {
        CLAUDE_HOME.get().cloned().unwrap_or_default()
    }

    #[test]
    fn the_wired_worktrees_find_a_sessions_worktree_in_the_claude_folder_the_root_names() {
        let scratch = crate::test_support::Directory::new();
        let made = scratch.join("repo/.claude/worktrees/t");
        std::fs::create_dir_all(&made).unwrap();
        let home = scratch.join("claude");
        let job = home.join("jobs/0123abcd");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("state.json"), serde_json::json!({"worktreePath": made}).to_string()).unwrap();
        CLAUDE_HOME.set(home).unwrap();
        let registry = Registry::default();
        wire(&registry, claude_home);
        let worktrees = registry.worktrees();
        assert_eq!(worktrees.worktree_of("0123abcd-session"), Some(made.clone()));
        assert_eq!(worktrees.origin("0123abcd-session"), Some(WorktreeOrigin::Made(made)));
        assert_eq!(worktrees.worktree_of("ffffffff-other"), None, "a session without a job has none");
        assert!(registry.missing().iter().all(|port| !matches!(*port, "Disk" | "Worktrees")), "{:?}", registry.missing());
    }
}
