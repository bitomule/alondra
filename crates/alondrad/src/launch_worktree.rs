//! A node never works in a main checkout: a launch whose folder is one starts in the task's
//! worktree under `.claude/worktrees/` instead, the one an earlier node of the task left or a new
//! one. Claude Code only asks a session to isolate itself; Alondra does not leave it to the model.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

/// Where a launched node works.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Workplace {
    /// The folder as given: not a git checkout, or a linked worktree already.
    Given(PathBuf),
    /// The task's worktree an earlier node left, with its work.
    Reused(PathBuf),
    /// A worktree Alondra made for the task from the default branch, or from the task's branch.
    Made(PathBuf),
}

impl Workplace {
    pub(crate) fn dir(&self) -> &Path {
        match self {
            Workplace::Given(dir) | Workplace::Reused(dir) | Workplace::Made(dir) => dir,
        }
    }

    /// The worktree Alondra chose for the node, which it may delete with it.
    pub(crate) fn chosen(&self) -> Option<&Path> {
        match self {
            Workplace::Given(_) => None,
            Workplace::Reused(dir) | Workplace::Made(dir) => Some(dir),
        }
    }
}

/// Where a node launched in `cwd` works. In a main checkout, or a folder inside one, that is a
/// linked worktree of its repository: the task's `known` one, the one with the task's pull
/// request branch, `.claude/worktrees/<slug>`, or that folder made new. Never a main checkout.
pub(crate) async fn isolate(cwd: &Path, slug: &str, known: Option<&Path>, pr_head: Option<&str>) -> Result<Workplace, String> {
    let workplace = choose(cwd, slug, known, pr_head).await?;
    if main_checkout(workplace.dir()) {
        return Err(format!(
            "{} is the main checkout of a repository, and no node works there: Alondra could not give it a worktree of its own",
            workplace.dir().display()));
    }
    Ok(workplace)
}

/// The folder is the top of a main checkout, whose `.git` is a directory; in a linked worktree it
/// is a file.
pub(crate) fn main_checkout(dir: &Path) -> bool {
    dir.join(".git").is_dir()
}

async fn choose(cwd: &Path, slug: &str, known: Option<&Path>, pr_head: Option<&str>) -> Result<Workplace, String> {
    let given = || Ok(Workplace::Given(cwd.to_path_buf()));
    let Some(top) = git(cwd, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await else { return given() };
    let (Some(git_dir), Some(common)) = (
        git(cwd, &["rev-parse", "--path-format=absolute", "--git-dir"]).await,
        git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await,
    ) else { return given() };
    if !same_dir(Path::new(&git_dir), Path::new(&common)) {
        return given();
    }
    let top = PathBuf::from(top);
    let inside = relative(cwd, &top);
    let linked = linked_worktrees(&top).await?;
    let find = |wanted: &Path| linked.iter().find(|(dir, _)| same_dir(dir, wanted)).map(|(dir, _)| dir.clone());
    let reused = known.and_then(find)
        .or_else(|| pr_head.and_then(|head| linked.iter().find(|(_, branch)| branch.as_deref() == Some(head)).map(|(dir, _)| dir.clone())))
        .or_else(|| find(&top.join(".claude/worktrees").join(slug)));
    if let Some(dir) = reused {
        return Ok(Workplace::Reused(within(dir, &inside)));
    }
    let dir = make(&top, slug).await?;
    Ok(Workplace::Made(within(dir, &inside)))
}

/// Makes `.claude/worktrees/<slug>` on branch `worktree-<slug>`: that branch when it exists, which
/// keeps an earlier node's commits, else a new one from the remote's default branch.
async fn make(top: &Path, slug: &str) -> Result<PathBuf, String> {
    let dir = top.join(".claude/worktrees").join(slug);
    if dir.exists() {
        return Err(format!("{} exists but is not a worktree of {}, so Alondra did not start a node in it", dir.display(), top.display()));
    }
    let _ = git(top, &["worktree", "prune"]).await;
    let branch = format!("worktree-{slug}");
    let path = dir.to_str().ok_or("the worktree path is not valid UTF-8")?;
    let exists = git(top, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]).await.is_some();
    let added = if exists {
        run(top, &["worktree", "add", "--quiet", path, &branch]).await
    } else {
        let base = fetched_default(top).await.unwrap_or_else(|| "HEAD".into());
        run(top, &["worktree", "add", "--quiet", "--no-track", "-b", &branch, path, &base]).await
    };
    added.map_err(|why| format!("Alondra could not make the worktree {}: {why}", dir.display()))?;
    Ok(dir)
}

/// The remote's default branch as `origin/<name>`, fetched; `None` without a remote that has it.
async fn fetched_default(top: &Path) -> Option<String> {
    let default = crate::worktree::default_branch(top).await?;
    let branch = default.strip_prefix("origin/").unwrap_or(&default).to_owned();
    let fetch = Command::new("git").arg("-C").arg(top).args(["fetch", "--quiet", "origin", &branch]).kill_on_drop(true).output();
    let _ = tokio::time::timeout(Duration::from_secs(60), fetch).await;
    git(top, &["rev-parse", "--verify", "--quiet", &default]).await.map(|_| default)
}

/// The repository's linked worktrees that still exist, with the branch each has checked out.
async fn linked_worktrees(top: &Path) -> Result<Vec<(PathBuf, Option<String>)>, String> {
    let listed = git(top, &["worktree", "list", "--porcelain"]).await.ok_or_else(|| format!("git could not list the worktrees of {}", top.display()))?;
    Ok(listed.split("\n\n").filter_map(|entry| {
        let dir = PathBuf::from(entry.lines().find_map(|line| line.strip_prefix("worktree "))?);
        let branch = entry.lines().find_map(|line| line.strip_prefix("branch refs/heads/")).map(str::to_owned);
        (dir.join(".git").is_file()).then_some((dir, branch))
    }).collect())
}

/// Where `cwd` sits inside the checkout at `top`, so a node launched in a subfolder works in the
/// same subfolder of its worktree.
fn relative(cwd: &Path, top: &Path) -> PathBuf {
    match (cwd.canonicalize(), top.canonicalize()) {
        (Ok(cwd), Ok(top)) => cwd.strip_prefix(&top).map(Path::to_path_buf).unwrap_or_default(),
        _ => PathBuf::new(),
    }
}

fn within(dir: PathBuf, inside: &Path) -> PathBuf {
    if inside.as_os_str().is_empty() { dir } else { dir.join(inside) }
}

/// A task id as a worktree folder name.
pub(crate) fn slug(name: &str) -> String {
    let slug: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let slug = slug.trim_matches('-').chars().take(64).collect::<String>();
    if slug.is_empty() { "node".into() } else { slug }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).kill_on_drop(true).output().await.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn run(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).kill_on_drop(true).output().await.map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A main checkout with one commit, cloned from a bare `origin` so it has a default branch.
    fn repository(tag: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().canonicalize().unwrap()
            .join(format!("alondra-launch-wt-{tag}-{}-{}", std::process::id(), crate::auth::random_hex(4)));
        std::fs::create_dir_all(&root).unwrap();
        sh(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        sh(&root, &["clone", "-q", "origin.git", "repo"]);
        let repo = root.join("repo");
        sh(&repo, &["config", "user.email", "test@example.invalid"]);
        sh(&repo, &["config", "user.name", "Test"]);
        sh(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        sh(&repo, &["push", "-q", "origin", "HEAD:main"]);
        sh(&repo, &["remote", "set-head", "origin", "main"]);
        (root, repo)
    }

    #[tokio::test]
    async fn a_launch_in_a_main_checkout_gets_a_new_worktree_for_the_task_from_the_default_branch() {
        let (root, repo) = repository("made");
        let workplace = isolate(&repo, "t-cards", None, None).await.unwrap();
        let dir = repo.join(".claude/worktrees/t-cards");
        assert_eq!(workplace, Workplace::Made(dir.clone()));
        assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "worktree-t-cards");
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), sh(&repo, &["rev-parse", "origin/main"]));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The bug: a task whose node was stopped is launched again in the repository's root while
    /// its earlier worktree, with uncommitted work, is still there.
    #[tokio::test]
    async fn a_relaunch_works_in_the_worktree_the_tasks_earlier_node_left() {
        let (root, repo) = repository("known");
        let earlier = repo.join(".claude/worktrees/safe-permissions");
        sh(&repo, &["worktree", "add", "-q", "-b", "worktree-safe-permissions", earlier.to_str().unwrap()]);
        std::fs::write(earlier.join("half-done.rs"), "work").unwrap();
        let workplace = isolate(&repo, "t-imp-safe-permissions", Some(&earlier), None).await.unwrap();
        assert_eq!(workplace, Workplace::Reused(earlier.clone()));
        assert!(earlier.join("half-done.rs").exists(), "the earlier node's work is where the new node starts");
        assert!(!repo.join(".claude/worktrees/t-imp-safe-permissions").exists(), "no second worktree");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_relaunch_finds_the_worktree_of_the_tasks_pull_request_or_its_own_folder() {
        let (root, repo) = repository("found");
        let pr = repo.join(".claude/worktrees/model-rules-resume");
        sh(&repo, &["worktree", "add", "-q", "-b", "model-rules-763", pr.to_str().unwrap()]);
        assert_eq!(isolate(&repo, "t-imp-model-rules", None, Some("model-rules-763")).await.unwrap(), Workplace::Reused(pr));
        let own = repo.join(".claude/worktrees/t-list");
        sh(&repo, &["worktree", "add", "-q", "-b", "worktree-t-list", own.to_str().unwrap()]);
        assert_eq!(isolate(&repo, "t-list", Some(&root.join("gone")), None).await.unwrap(), Workplace::Reused(own),
            "a known worktree that is gone does not count");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_worktree_deleted_with_its_branch_kept_comes_back_on_that_branch() {
        let (root, repo) = repository("branch");
        let dir = repo.join(".claude/worktrees/t-pacing");
        isolate(&repo, "t-pacing", None, None).await.unwrap();
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "earlier work"]);
        let earlier = sh(&dir, &["rev-parse", "HEAD"]);
        sh(&repo, &["worktree", "remove", dir.to_str().unwrap()]);
        assert_eq!(isolate(&repo, "t-pacing", None, None).await.unwrap(), Workplace::Made(dir.clone()));
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), earlier, "the earlier node's commits are there");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_subfolder_of_a_main_checkout_is_the_same_subfolder_of_the_worktree() {
        let (root, repo) = repository("subfolder");
        std::fs::create_dir_all(repo.join("crates")).unwrap();
        std::fs::write(repo.join("crates/keep"), "").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-q", "-m", "crates"]);
        sh(&repo, &["push", "-q", "origin", "HEAD:main"]);
        let workplace = isolate(&repo.join("crates"), "t-sub", None, None).await.unwrap();
        assert_eq!(workplace, Workplace::Made(repo.join(".claude/worktrees/t-sub/crates")));
        assert!(workplace.dir().is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_folder_that_is_not_a_repository_or_is_a_linked_worktree_is_kept() {
        let (root, repo) = repository("given");
        let plain = root.join("notes");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(isolate(&plain, "t", None, None).await.unwrap(), Workplace::Given(plain));
        let linked = root.join("linked");
        sh(&repo, &["worktree", "add", "-q", "-b", "linked", linked.to_str().unwrap()]);
        assert_eq!(isolate(&linked, "t", None, None).await.unwrap(), Workplace::Given(linked));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_main_checkout_git_cannot_read_is_refused_never_launched_in() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!("alondra-launch-wt-broken-{}", crate::auth::random_hex(4)));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let refused = isolate(&root, "t", None, None).await.unwrap_err();
        assert!(refused.contains("main checkout"), "{refused}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_task_id_is_a_folder_name() {
        assert_eq!(slug("t-imp-safe-permissions"), "t-imp-safe-permissions");
        assert_eq!(slug("App/Fix Login!"), "app-fix-login");
        assert_eq!(slug("///"), "node");
    }
}
