use std::path::{Path, PathBuf};

use alondra_core::domain::{DeliveryTarget, DomainError, DomainState, TaskId};
use serde_json::Value;
use tokio::process::Command;

pub(crate) fn worktree_of(session_id: &str) -> Option<PathBuf> {
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
    leftovers_in(&worktree_of(session_id)?).await
}

pub async fn leftovers_in(dir: &Path) -> Option<String> {
    let dir = dir.to_path_buf();
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

/// Why a node's final delivery cannot be taken yet: its worktree holds work no remote has, or
/// work that is neither on the default branch nor in the PR registered for its Task. Nothing
/// when the node has no worktree of its own.
pub(crate) async fn final_refusal(state: &DomainState, session_id: &str, task: &TaskId, targets: &[DeliveryTarget]) -> Option<DomainError> {
    final_refusal_in(state, &worktree_of(session_id)?, task, targets).await
}

pub(crate) async fn final_refusal_in(state: &DomainState, dir: &Path, task: &TaskId, targets: &[DeliveryTarget]) -> Option<DomainError> {
    let user = crate::domain_messages::user_name();
    if let Some(left) = leftovers_in(dir).await {
        return Some(DomainError::new("worktree_not_clean", format!(
            "Nothing was presented: your worktree has {left}. Commit and push your work (open a PR and register it with \
             register_pr if the work belongs in the code), then present the final result again. If some of it should be thrown \
             away, ask {user} with ask before discarding anything."
        )));
    }
    let in_pr = targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { .. }))
        || state.tasks.get(task).is_some_and(|task| task.pr_id.is_some());
    if in_pr {
        return None;
    }
    let unmerged = unmerged_in(dir).await?;
    Some(DomainError::new("work_not_delivered", format!(
        "Nothing was presented: {unmerged} and this task has no registered pull request. Open a pull request for your branch, \
         register it with register_pr, and present the final result again with that pull request in targets."
    )))
}

/// Deletes the node's worktree with whatever it holds, so `claude rm` closes the node: its
/// uncommitted changes are lost, its commits stay on its branch.
pub async fn discard(session_id: &str, permitted: impl Fn() -> bool) -> Result<(), String> {
    let Some(dir) = worktree_of(session_id) else { return Ok(()) };
    discard_in(&dir, permitted).await
}
async fn discard_in(dir: &Path, permitted: impl Fn() -> bool) -> Result<(), String> {
    let common = git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await.ok_or("its worktree is not a git checkout")?;
    if !permitted() { return Err("node changed while preparing discard".into()); }
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

    #[tokio::test]
    async fn discard_revalidates_after_git_preparation_before_removing_real_worktree() {
        let root = std::env::temp_dir().join(format!("alondra-discard-{}-{}", std::process::id(), alondra_core::now_millis()));
        std::fs::create_dir_all(&root).unwrap();
        run(&root, &["init", "-q", "repo"]);
        let repo = root.join("repo");
        run(&repo, &["config", "user.email", "test@example.invalid"]);
        run(&repo, &["config", "user.name", "Test"]);
        run(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let wt = root.join("owned-worktree");
        run(&repo, &["worktree", "add", "-q", "-b", "disposable", wt.to_str().unwrap()]);
        std::fs::write(wt.join("unsaved"), b"keep until authorized").unwrap();
        let observed = std::sync::atomic::AtomicBool::new(false);
        let result = discard_in(&wt, || {
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
            false
        }).await;
        assert!(result.is_err());
        assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(std::fs::read(wt.join("unsaved")).unwrap(), b"keep until authorized");
        discard_in(&wt, || true).await.unwrap();
        assert!(!wt.exists());
        std::fs::remove_dir_all(root).unwrap();
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

    #[tokio::test]
    async fn a_final_delivery_needs_the_work_pushed_and_on_the_default_branch_or_in_the_tasks_pr() {
        let root = std::env::temp_dir().join(format!("alondra-final-{}-{}", std::process::id(), alondra_core::now_millis()));
        let (remote, clone) = (root.join("remote.git"), root.join("clone"));
        std::fs::create_dir_all(&root).unwrap();
        run(&root, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
        run(&root, &["clone", "-q", remote.to_str().unwrap(), clone.to_str().unwrap()]);
        for args in [&["config", "user.email", "t@t"][..], &["config", "user.name", "t"], &["commit", "-q", "--allow-empty", "-m", "base"], &["push", "-q", "origin", "HEAD:main"]] {
            run(&clone, args);
        }
        run(&clone, &["remote", "set-head", "origin", "main"]);
        let state = DomainState::new("epoch");
        let task = TaskId::from("t");
        let text = [DeliveryTarget::Text { text: "Report".into() }];
        let pr = [DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "h".into() }];
        let code = |refusal: Option<DomainError>| refusal.map(|error| (error.code, error.message));
        assert_eq!(code(final_refusal_in(&state, &clone, &task, &text).await), None, "nothing left, nothing to merge");

        std::fs::write(clone.join("notes.txt"), "draft").unwrap();
        let (refused, message) = code(final_refusal_in(&state, &clone, &task, &pr).await).unwrap();
        assert_eq!(refused, "worktree_not_clean");
        assert!(message.starts_with("Nothing was presented: your worktree has 1 uncommitted file(s)"), "{message}");

        run(&clone, &["checkout", "-q", "-b", "feature"]);
        run(&clone, &["add", "notes.txt"]);
        run(&clone, &["commit", "-q", "-m", "work"]);
        let (refused, message) = code(final_refusal_in(&state, &clone, &task, &pr).await).unwrap();
        assert_eq!(refused, "worktree_not_clean");
        assert!(message.contains("1 commit(s) not pushed"), "{message}");

        run(&clone, &["push", "-q", "origin", "feature"]);
        assert_eq!(code(final_refusal_in(&state, &clone, &task, &pr).await), None, "pushed and delivered in its PR");
        let (refused, message) = code(final_refusal_in(&state, &clone, &task, &text).await).unwrap();
        assert_eq!(refused, "work_not_delivered");
        assert!(message.contains("is not on origin/main") && message.contains("register_pr"), "{message}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
