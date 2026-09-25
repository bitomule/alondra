use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::process::Command;

fn worktree_of(session_id: &str) -> Option<PathBuf> {
    let short = session_id.get(..8)?;
    let job = std::fs::read_to_string(crate::watcher::claude_home().join("jobs").join(short).join("state.json")).ok()?;
    let dir = serde_json::from_str::<Value>(&job).ok()?.get("worktreePath")?.as_str().map(PathBuf::from)?;
    dir.exists().then_some(dir)
}

async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).kill_on_drop(true).output().await.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What a node would leave behind in its worktree: uncommitted files or commits no remote has.
/// None when the node has no worktree of its own or it is clean.
pub async fn leftovers(session_id: &str) -> Option<String> {
    let dir = worktree_of(session_id)?;
    let changed = git(&dir, &["status", "--porcelain"]).await?.lines().count();
    let unpushed: usize = git(&dir, &["rev-list", "--count", "HEAD", "--not", "--remotes"]).await?.parse().ok()?;
    let mut parts = vec![];
    if changed > 0 {
        parts.push(format!("{changed} uncommitted file(s)"));
    }
    if unpushed > 0 {
        parts.push(format!("{unpushed} commit(s) not pushed"));
    }
    (!parts.is_empty()).then(|| format!("{} in {}", parts.join(" and "), dir.display()))
}

/// The node's worktree holds commits the remote's default branch does not, after fetching it.
/// A fetch or a check that fails counts as not merged, so Alondra asks rather than closes.
pub async fn unmerged(session_id: &str) -> Option<String> {
    let dir = worktree_of(session_id)?;
    unmerged_in(&dir).await
}

pub async fn unmerged_in(dir: &Path) -> Option<String> {
    let default = git(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).await.unwrap_or_else(|| "origin/main".into());
    let branch = default.strip_prefix("origin/").unwrap_or(&default).to_string();
    let fetch = Command::new("git").arg("-C").arg(dir).args(["fetch", "--quiet", "origin", &branch]).kill_on_drop(true).output();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(60), fetch).await;
    let merged = Command::new("git").arg("-C").arg(dir).args(["merge-base", "--is-ancestor", "HEAD", &default]).output().await.is_ok_and(|o| o.status.success());
    (!merged).then(|| format!("its work in {} is not on {default}", dir.display()))
}

/// Deletes the node's worktree with whatever it holds, so `claude rm` closes the node: its
/// uncommitted changes are lost, its commits stay on its branch.
pub async fn discard(session_id: &str) -> Result<(), String> {
    let Some(dir) = worktree_of(session_id) else { return Ok(()) };
    let common = git(&dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await.ok_or("its worktree is not a git checkout")?;
    let out = Command::new("git")
        .arg(format!("--git-dir={common}"))
        .args(["worktree", "remove", "--force"])
        .arg(&dir)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

/// What in a node's worktree stops Alondra from closing it by itself, so a test can stand in for
/// git. `merged_by_pr`: every PR the node opened is merged, which covers a squash merge that
/// leaves its commits off the default branch.
pub trait Worktrees: Sync {
    fn unfinished(&self, session_id: &str, merged_by_pr: bool) -> impl Future<Output = Vec<String>> + Send;
}

pub struct Git;

impl Worktrees for Git {
    async fn unfinished(&self, session_id: &str, merged_by_pr: bool) -> Vec<String> {
        let mut found: Vec<String> = leftovers(session_id).await.into_iter().collect();
        if !merged_by_pr {
            found.extend(unmerged(session_id).await);
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Pushed is not enough to close a node: its commits must be on the remote's default branch.
    #[tokio::test]
    async fn work_counts_as_merged_only_once_the_default_branch_on_the_remote_has_it() {
        let root = std::env::temp_dir().join(format!("alondra-unmerged-{}-{}", std::process::id(), alondra_core::now_millis()));
        let (remote, clone) = (root.join("remote.git"), root.join("clone"));
        std::fs::create_dir_all(&root).unwrap();
        run(&root, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
        run(&root, &["clone", "-q", remote.to_str().unwrap(), clone.to_str().unwrap()]);
        for args in [&["config", "user.email", "t@t"][..], &["config", "user.name", "t"], &["commit", "-q", "--allow-empty", "-m", "base"], &["push", "-q", "origin", "HEAD:main"]] {
            run(&clone, args);
        }
        run(&clone, &["remote", "set-head", "origin", "main"]);
        assert_eq!(unmerged_in(&clone).await, None, "HEAD is main");

        for args in [&["checkout", "-q", "-b", "feature"][..], &["commit", "-q", "--allow-empty", "-m", "work"], &["push", "-q", "origin", "feature"]] {
            run(&clone, args);
        }
        let said = unmerged_in(&clone).await.expect("pushed to its branch, not merged");
        assert!(said.contains("not on origin/main"), "{said}");

        run(&clone, &["push", "-q", "origin", "feature:main"]);
        assert_eq!(unmerged_in(&clone).await, None, "merged on the remote, fetched before the check");
        std::fs::remove_dir_all(root).unwrap();
    }
}
