//! Closing a session the way `claude rm` does, which keeps a worktree with work not yet saved
//! elsewhere, and refusing while the session still has something pending. An interactive
//! session is not a job `claude rm` knows: it only closes and its terminal stays open.
//! Shared by the close route and by Alondra closing the node of a finished task.
use crate::live::LiveOf as _;
use alondra_engine_ports::Runtime;
use alondra_domain_rules::AgentId;
use axum::http::StatusCode;
use serde_json::Value;
use std::path::{Path, PathBuf};
pub use alondra_engine_ports::node_close::{Answer, CloseOutcome, Ran, Reason, Sessions, agents_args, listing};
pub use alondra_engine_kernel::bg_sessions::{agents, claude};

/// Claude Code and git on this Mac.
pub struct Machine(pub std::sync::Arc<dyn alondra_engine_ports::worktrees::Worktrees>);

impl Sessions for Machine {
    fn claude<'a>(&'a self, args: Vec<String>) -> Answer<'a, Result<Ran, String>> {
        Box::pin(claude(args))
    }

    fn leftovers<'a>(&'a self, session_id: &'a str) -> Answer<'a, Option<String>> {
        self.0.leftovers(session_id)
    }
}

/// Real risks a close must not paper over: the session still doing something (busy or a live
/// permission hold), or work left uncommitted or unpushed in its worktree.
fn reasons(busy: bool, waiting_permission: bool, worktree_leftovers: Option<String>) -> Vec<Reason> {
    if busy {
        return vec![Reason { kind: "busy", detail: None }];
    }
    let mut reasons = vec![];
    if waiting_permission {
        reasons.push(Reason { kind: "permission", detail: None });
    }
    if let Some(left) = worktree_leftovers {
        reasons.push(Reason { kind: "worktree", detail: Some(left) });
    }
    reasons
}

pub(crate) fn listed(agents_json: &str, short: &str) -> bool {
    serde_json::from_str::<Value>(agents_json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|a| a.iter().any(|x| x.get("id").and_then(Value::as_str) == Some(short)))
}

fn failed(status: StatusCode, message: impl Into<String>) -> CloseOutcome {
    CloseOutcome::Failed { status: status.as_u16(), message: message.into() }
}

/// Closes the session of `agent`. A session `claude agents` does not list is left alone, and
/// closes only when `leave_if_not_listed` says so.
pub async fn close_node(
    domain: &dyn Runtime,
    sessions: &dyn Sessions,
    agent: &AgentId,
    leave_if_not_listed: bool,
) -> anyhow::Result<CloseOutcome> {
    let agent_id = agent.0.as_str();
    let node = domain.live().node(agent_id);
    let short: String = node.as_ref().map_or_else(|| agent_id.chars().take(8).collect(), |node| node.short_id.clone());
    let (busy, waiting_permission) = node.as_ref().map_or((false, false), |node| (node.busy, node.waiting_permission));
    let listing = match sessions.agents().await {
        Ok(listing) => listing,
        Err(e) => return Ok(failed(StatusCode::BAD_GATEWAY, e)),
    };
    if !listed(&listing, &short) {
        if leave_if_not_listed {
            crate::node_exit::user_closed(domain, agent_id)?;
        }
        return Ok(CloseOutcome::NotListed);
    }
    let leftovers = sessions.leftovers(agent_id).await;
    let reasons = reasons(busy, waiting_permission, leftovers);
    if !reasons.is_empty() {
        let message = reasons.iter().map(|r| r.detail.clone().unwrap_or_else(|| r.kind.to_string())).collect::<Vec<_>>().join("; ");
        return Ok(CloseOutcome::Refused { message: format!("{short} still has something pending, so it was not closed: {message}"), reasons });
    }
    let watched = domain.live().begin_watch(agent_id);
    let removed = remove(domain, sessions, agent_id, &short).await;
    if watched {
        domain.live().end_watch(agent_id);
    }
    removed
}

/// `claude rm` until `claude agents` no longer lists the session. Its connection drops on the way,
/// so the caller holds the session's watch: the revival must not restart what is being closed.
async fn remove(domain: &dyn Runtime, sessions: &dyn Sessions, agent_id: &str, short: &str) -> anyhow::Result<CloseOutcome> {
    let short = short.to_owned();
    let job = std::fs::read_to_string(crate::watcher::claude_home().join("jobs").join(&short).join("state.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let worktree = job.as_ref().and_then(|v| v.get("worktreePath").and_then(Value::as_str).map(PathBuf::from));
    let branch = job.as_ref().and_then(|v| v.get("worktreeBranch").and_then(Value::as_str).map(str::to_string));
    let mut cleared_broken = false;
    let mut said = String::new();
    for _ in 0..3 {
        let ran = match sessions.claude(vec!["rm".into(), short.clone()]).await {
            Ok(ran) => ran,
            Err(e) => return Ok(failed(StatusCode::BAD_GATEWAY, e)),
        };
        if !ran.success {
            let text = ran.text();
            let (Some(dir), Some(branch)) = (worktree.as_ref().filter(|dir| !cleared_broken && dir.exists()), branch.as_deref()) else {
                return Ok(failed(StatusCode::CONFLICT, text));
            };
            match registered(dir).await {
                Ok(false) => {}
                Ok(true) => return Ok(failed(StatusCode::CONFLICT, text)),
                Err(why) => return Ok(failed(StatusCode::CONFLICT, format!("{text}\n\nAlondra did not delete it: {why}"))),
            }
            if let Err(why) = discard_if_identical(dir, branch).await {
                return Ok(failed(StatusCode::CONFLICT, format!("{text}\n\nAlondra did not delete it: {why}")));
            }
            cleared_broken = true;
            continue;
        }
        said = ran.text();
        match sessions.agents().await {
            Ok(list) if !listed(&list, &short) => {
                if let Some(dir) = worktree.as_ref().filter(|d| d.exists()) {
                    return Ok(failed(StatusCode::CONFLICT, format!("{short} is gone from claude agents but its worktree is still at {}", dir.display())));
                }
                crate::node_exit::user_closed(domain, agent_id)?;
                return Ok(CloseOutcome::Closed { output: said });
            }
            Ok(_) => continue,
            Err(e) => return Ok(failed(StatusCode::BAD_GATEWAY, e)),
        }
    }
    Ok(failed(StatusCode::CONFLICT, format!("claude rm said '{said}' but {short} is still listed by claude agents")))
}

/// The git folder of the repository a worktree belongs to, from its `.git` link.
fn repository_of(dir: &Path) -> Result<PathBuf, String> {
    let link = std::fs::read_to_string(dir.join(".git")).map_err(|e| format!("no .git link: {e}"))?;
    let gitdir = link.trim().strip_prefix("gitdir: ").ok_or("unexpected .git link")?;
    PathBuf::from(gitdir).parent().and_then(|p| p.parent()).map(PathBuf::from).ok_or_else(|| "unexpected gitdir".to_owned())
}

/// Whether the repository of `dir` still lists it in `git worktree list --porcelain -z`.
async fn registered(dir: &Path) -> Result<bool, String> {
    let repo_git = repository_of(dir)?;
    let out = tokio::process::Command::new("git")
        .arg(format!("--git-dir={}", repo_git.display()))
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let wanted = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
    Ok(worktrees(&out.stdout).any(|path| std::fs::canonicalize(&path).is_ok_and(|path| path == wanted)))
}

/// The paths of `git worktree list --porcelain -z`: NUL-terminated attributes, one `worktree` each.
fn worktrees(porcelain: &[u8]) -> impl Iterator<Item = PathBuf> + '_ {
    use std::os::unix::ffi::OsStrExt;
    porcelain.split(|byte| *byte == 0).filter_map(|field| field.strip_prefix(b"worktree ")).map(|path| PathBuf::from(std::ffi::OsStr::from_bytes(path)))
}

/// A worktree whose git link is broken can only be deleted by hand. It is deleted only when
/// its files are exactly its branch, it has no new files and the branch is fully pushed, or kept
/// in the branch itself when the repository has no remote.
async fn discard_if_identical(dir: &Path, branch: &str) -> Result<(), String> {
    let repo_git = repository_of(dir)?;
    let index = scratch_index();
    let git = |args: Vec<String>| {
        let repo_git = repo_git.clone();
        let index = index.clone();
        async move {
            let out = tokio::process::Command::new("git")
                .env("GIT_INDEX_FILE", &index)
                .arg(format!("--git-dir={}", repo_git.display()))
                .args(&args)
                .output()
                .await
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
            }
            Ok::<String, String>(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
    };
    let tree = format!("--work-tree={}", dir.display());
    git(vec!["read-tree".into(), branch.into()]).await?;
    let differs = git(vec![tree.clone(), "diff".into(), "--stat".into()]).await;
    let _ = std::fs::remove_file(&index);
    if !differs?.is_empty() {
        return Err(format!("its files differ from the branch {branch}"));
    }
    git(vec!["read-tree".into(), branch.into()]).await?;
    let untracked = git(vec![tree, "ls-files".into(), "--others".into(), "--exclude-standard".into()]).await;
    let _ = std::fs::remove_file(&index);
    if !untracked?.is_empty() {
        return Err("it has files that are not in git".into());
    }
    let no_remote = git(vec!["remote".into()]).await?.is_empty();
    if !no_remote && git(vec!["rev-list".into(), "--count".into(), format!("origin/{branch}..{branch}")]).await? != "0" {
        return Err(format!("the branch {branch} has commits that are not pushed"));
    }
    tokio::fs::remove_dir_all(dir).await.map_err(|e| e.to_string())
}

/// A private index per call: two closes in the same millisecond must not share one.
#[expect(clippy::disallowed_methods, reason = "Production Git scratch indexes are removed by the close operation.")]
fn scratch_index() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("alondra-index-{}-{}-{n}", std::process::id(), alondra_core::now_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A repository under `root`, and the worktree of task `t` on branch `worktree-t` with one
    /// commit of its own.
    fn task_worktree(root: &Path) -> (PathBuf, PathBuf) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"], &["commit", "-q", "--allow-empty", "-m", "base"]] {
            run(&repo, args);
        }
        let dir = repo.join(".claude/worktrees/t");
        run(&repo, &["worktree", "add", "-q", "-b", "worktree-t", dir.to_str().unwrap()]);
        std::fs::write(dir.join("work.txt"), "work").unwrap();
        run(&dir, &["add", "work.txt"]);
        run(&dir, &["commit", "-q", "-m", "work"]);
        (repo, dir)
    }

    #[tokio::test]
    async fn a_worktree_of_a_repository_without_a_remote_is_deleted_and_its_branch_keeps_the_commits() {
        let root = crate::test_support::Directory::new();
        let (repo, dir) = task_worktree(&root);
        let head = run(&dir, &["rev-parse", "HEAD"]);

        assert_eq!(discard_if_identical(&dir, "worktree-t").await, Ok(()));

        assert!(!dir.exists(), "the folder is deleted");
        assert_eq!(run(&repo, &["rev-parse", "refs/heads/worktree-t"]), head, "the branch keeps the task's commits");
        assert_eq!(run(&repo, &["show", "worktree-t:work.txt"]), "work");
    }

    #[tokio::test]
    async fn a_worktree_of_a_repository_without_a_remote_with_uncommitted_files_is_kept() {
        let root = crate::test_support::Directory::new();
        let (_repo, dir) = task_worktree(&root);
        std::fs::write(dir.join("notes.txt"), "draft").unwrap();

        assert_eq!(discard_if_identical(&dir, "worktree-t").await, Err("it has files that are not in git".into()));
        assert!(dir.join("notes.txt").exists());
    }

    #[tokio::test]
    async fn a_worktree_whose_branch_has_commits_its_remote_lacks_is_kept() {
        let root = crate::test_support::Directory::new();
        let (repo, dir) = task_worktree(&root);
        let remote = root.join("remote.git");
        run(&root, &["init", "-q", "--bare", remote.to_str().unwrap()]);
        run(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        run(&repo, &["push", "-q", "origin", "worktree-t"]);
        run(&dir, &["commit", "-q", "--allow-empty", "-m", "local"]);

        assert_eq!(discard_if_identical(&dir, "worktree-t").await, Err("the branch worktree-t has commits that are not pushed".into()));
        assert!(dir.exists());
    }
}
