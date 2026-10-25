use std::path::{Path, PathBuf};

use alondra_core::domain::{ChecklistMark, DeliveryTarget, DomainError, DomainState, EvidenceKind, TaskId};
use serde_json::Value;
use tokio::process::Command;

pub(crate) fn worktree_of(session_id: &str) -> Option<PathBuf> {
    worktree_in(&crate::watcher::claude_home(), session_id)
}

/// The worktree a session works in: the one Claude Code made for it, else the linked worktree it
/// was launched in, as the node that replaces another one is. A main checkout is not one.
pub(crate) fn worktree_in(claude_home: &Path, session_id: &str) -> Option<PathBuf> {
    origin_in(claude_home, session_id).map(|origin| match origin {
        WorktreeOrigin::Made(dir) | WorktreeOrigin::LaunchedIn(dir) => dir,
    })
}

/// How a session came to work in its worktree, which decides whether Alondra may delete it.
#[derive(Debug, PartialEq)]
pub(crate) enum WorktreeOrigin {
    /// Claude Code made it for the session.
    Made(PathBuf),
    /// The session was launched in it: the worktree of the node it replaced, or a folder the user
    /// gave, which can be a linked worktree too.
    LaunchedIn(PathBuf),
}

pub(crate) fn origin_in(claude_home: &Path, session_id: &str) -> Option<WorktreeOrigin> {
    let short = session_id.get(..8)?;
    let job: Value = serde_json::from_str(&std::fs::read_to_string(claude_home.join("jobs").join(short).join("state.json")).ok()?).ok()?;
    if let Some(dir) = job.get("worktreePath").and_then(Value::as_str).map(PathBuf::from) {
        return dir.exists().then_some(WorktreeOrigin::Made(dir));
    }
    let cwd = PathBuf::from(job.get("cwd")?.as_str()?);
    cwd.join(".git").is_file().then_some(WorktreeOrigin::LaunchedIn(cwd))
}

/// Where Claude Code puts the worktree of a subagent `agent` that the node working in `own`
/// launched: `agent-<agent>` in the `.claude/worktrees` of the repository `own` belongs to.
pub(crate) fn subagent_worktree(own: &Path, agent: &str) -> Option<PathBuf> {
    let worktrees = own.parent().filter(|dir| dir.file_name().is_some_and(|name| name == "worktrees"))?;
    let repository = worktrees.parent().filter(|dir| dir.file_name().is_some_and(|name| name == ".claude"))?.parent()?;
    let common = std::process::Command::new("git").arg("-C").arg(own)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).output().ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()));
    let repository = common.as_deref().and_then(Path::parent).unwrap_or(repository);
    Some(repository.join(".claude").join("worktrees").join(format!("agent-{agent}")))
}

/// The branch checked out in `dir`.
pub(crate) async fn branch_in(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await.filter(|branch| branch != "HEAD")
}

async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).kill_on_drop(true).output().await.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Where the commits of `dir`'s HEAD already live without a push.
#[derive(Debug, PartialEq)]
enum LocalHome {
    /// The repository has no remote to push them to.
    NoRemote,
    /// They are in the local default branch, which HEAD is not on.
    DefaultBranch,
}

/// The repository's default branch as a local branch: origin/HEAD's name, else `main` or `master`.
async fn local_default_branch(dir: &Path) -> Option<String> {
    if let Some(head) = git(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).await {
        let name = head.strip_prefix("origin/").unwrap_or(&head).to_owned();
        if git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).await.is_some() {
            return Some(name);
        }
    }
    for name in ["main", "master"] {
        if git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).await.is_some() {
            return Some(name.to_owned());
        }
    }
    None
}

async fn local_home(dir: &Path) -> Option<LocalHome> {
    if git(dir, &["remote"]).await?.is_empty() {
        return Some(LocalHome::NoRemote);
    }
    let default = local_default_branch(dir).await?;
    if branch_in(dir).await.as_deref() == Some(default.as_str()) {
        return None;
    }
    git(dir, &["merge-base", "--is-ancestor", "HEAD", &format!("refs/heads/{default}")]).await.map(|_| LocalHome::DefaultBranch)
}

/// The commits of HEAD that no remote has and that are not kept by a [`LocalHome`].
async fn unpushed_commits(dir: &Path) -> Option<usize> {
    if local_home(dir).await.is_some() {
        return Some(0);
    }
    git(dir, &["rev-list", "--count", "HEAD", "--not", "--remotes"]).await?.parse().ok()
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

/// The remote's default branch as `origin/<name>`: origin/HEAD, else what GitHub names it.
pub(crate) async fn default_branch(dir: &Path) -> Option<String> {
    if let Some(head) = git(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).await {
        return Some(head);
    }
    let gh = Command::new("gh").current_dir(dir).args(["repo", "view", "--json", "defaultBranchRef", "--jq", ".defaultBranchRef.name"])
        .kill_on_drop(true).output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(20), gh).await.ok()?.ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !name.is_empty()).then(|| format!("origin/{name}"))
}

/// Whether the worktree's HEAD is on the remote's default branch, after fetching it; `None` when
/// that cannot be told (no origin, no default branch, a fetch or check that fails).
async fn on_default_branch(dir: &Path) -> Option<(bool, String)> {
    let default = default_branch(dir).await?;
    let branch = default.strip_prefix("origin/").unwrap_or(&default).to_owned();
    let fetch = Command::new("git").arg("-C").arg(dir).args(["fetch", "--quiet", "origin", &branch]).kill_on_drop(true).output();
    if !tokio::time::timeout(std::time::Duration::from_secs(60), fetch).await.ok()?.ok()?.status.success() {
        return None;
    }
    let check = Command::new("git").arg("-C").arg(dir).args(["merge-base", "--is-ancestor", "HEAD", &default]).output().await.ok()?;
    match check.status.code() {
        Some(0) => Some((true, default)),
        Some(1) => Some((false, default)),
        _ => None,
    }
}

const LISTED_FILES: usize = 20;

fn listed(files: &[String]) -> String {
    let mut list = files.iter().take(LISTED_FILES).map(|file| format!("`{file}`")).collect::<Vec<_>>().join(", ");
    if files.len() > LISTED_FILES {
        list.push_str(&format!(" and {} more", files.len() - LISTED_FILES));
    }
    list
}

/// Why a node's final delivery cannot be taken yet: files it did not commit, commits no remote
/// has, or work that is neither on the default branch nor in the PR registered for its Task. Work in
/// a repository without a remote, or already in the local default branch, needs no push and no PR.
/// Nothing when the node has no worktree of its own, or when git cannot tell.
pub(crate) async fn final_refusal(state: &DomainState, session_id: &str, task: &TaskId, targets: &[DeliveryTarget]) -> Option<DomainError> {
    final_refusal_in(state, &node_worktree(state, session_id, task)?, task, targets).await
}

/// The worktree the node of `task` works in: the one its `claude --bg` job names, else the one the
/// task recorded as its own, where a node of the Agent SDK, which has no job, works.
pub(crate) fn node_worktree(state: &DomainState, session_id: &str, task: &TaskId) -> Option<PathBuf> {
    worktree_of(session_id).or_else(|| {
        let task = state.tasks.get(task)?;
        task.open_worktrees().find(|(_, worktree)| worktree.origin == alondra_core::domain::WorktreeOrigin::Task).map(|(path, _)| PathBuf::from(path))
    })
}

pub(crate) async fn final_refusal_in(state: &DomainState, dir: &Path, task: &TaskId, targets: &[DeliveryTarget]) -> Option<DomainError> {
    let user = crate::domain_messages::user_name();
    let status = git(dir, &["status", "--porcelain", "--untracked-files=all"]).await.unwrap_or_default();
    let (untracked, changed): (Vec<String>, Vec<String>) = status.lines().filter(|line| line.len() > 3)
        .map(|line| (line.starts_with("??"), line[3..].to_owned()))
        .fold((vec![], vec![]), |(mut untracked, mut changed), (new, file)| {
            if new { untracked.push(file) } else { changed.push(file) }
            (untracked, changed)
        });
    let unpushed = unpushed_commits(dir).await.unwrap_or(0);
    let mut left = vec![];
    if !changed.is_empty() {
        left.push(format!("uncommitted changes to {}", listed(&changed)));
    }
    if !untracked.is_empty() {
        left.push(format!("files git does not track: {}", listed(&untracked)));
    }
    if unpushed > 0 {
        left.push(format!("{unpushed} commit(s) not pushed"));
    }
    if !left.is_empty() {
        return Some(DomainError::new("worktree_not_clean", format!(
            "Nothing was presented: your worktree in {} has {}. Commit each of these files, or delete it if it should not be \
             kept (build outputs, scratch files), push, and present the final result again. If you are unsure whether something \
             can go, ask {user} with ask before deleting it.",
            dir.display(), left.join("; ")
        )));
    }
    let in_pr = targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { .. }))
        || state.tasks.get(task).is_some_and(|task| task.pr_id.is_some());
    if in_pr || local_home(dir).await.is_some() {
        return None;
    }
    match on_default_branch(dir).await {
        Some((true, _)) => None,
        Some((false, default)) => Some(DomainError::new("work_not_delivered", format!(
            "Nothing was presented: your work in {} is not on {default} and this task has no registered pull request. Open a \
             pull request for your branch, register it with register_pr, and present the final result again with that pull \
             request in targets.", dir.display()
        ))),
        None => {
            log!("could not tell whether the work in {} is on its default branch; taking the final result", dir.display());
            None
        }
    }
}

/// Why the commits a final delivery cites as evidence cannot be taken: one its worktree does not
/// have in the history of its HEAD. Nothing when the node has no worktree of its own.
pub(crate) async fn evidence_refusal(state: &DomainState, session_id: &str, task: &TaskId, checklist: &[ChecklistMark]) -> Option<DomainError> {
    evidence_refusal_in(&node_worktree(state, session_id, task)?, checklist).await
}

pub(crate) async fn evidence_refusal_in(dir: &Path, checklist: &[ChecklistMark]) -> Option<DomainError> {
    for mark in checklist {
        for evidence in mark.evidence.iter().filter(|evidence| evidence.kind == EvidenceKind::Commit) {
            let sha = evidence.value.trim();
            let commit = format!("{sha}^{{commit}}");
            let known = git(dir, &["rev-parse", "--verify", "--quiet", &commit]).await.is_some();
            if !known || git(dir, &["merge-base", "--is-ancestor", sha, "HEAD"]).await.is_none() {
                let mut error = DomainError::new("evidence_commit_not_in_work", format!(
                    "checklist: commit {sha} (point {}) is not in your work: cite a commit of your branch, pushed, or commit and push the work it stands for",
                    mark.item));
                error.subject = Some("checklist".into());
                return Some(error);
            }
        }
    }
    None
}

/// What became of the worktree Alondra made for a task once the task closed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TaskWorktree {
    /// It was deleted, or would be on a dry run, with the space its files took: none on a dry run
    /// or when a deletion cut halfway is finished.
    Removed { freed: u64 },
    /// It no longer exists.
    Absent,
    /// It is not `.claude/worktrees/<slug>` of a repository: the user's folder, never deleted.
    NotAlondras,
    /// It holds files not committed or commits no remote has.
    Unsaved(String),
}

/// Deletes the worktree Alondra made for a closed task, `.claude/worktrees/<slug>`, which
/// `claude rm` leaves because Claude Code did not make it. Its branch stays for a relaunch. What
/// git ignores goes with it, as with `git worktree remove`: the node was asked to clean up first.
/// With `remove` false it only says what it would do.
pub(crate) async fn release_task_worktree(dir: &Path, slug: &str, remove: bool) -> Result<TaskWorktree, String> {
    release(dir, |top| top.file_name().is_some_and(|name| name == slug), false, remove).await
}

/// Deletes a worktree a closing task recorded as its own, its node's or one of its subagents',
/// all in a repository's `.claude/worktrees`. One the node gave up (`give_up`) goes as it is;
/// any other only with nothing left in it. Its branch stays.
pub(crate) async fn release_recorded_worktree(dir: &Path, give_up: bool) -> Result<TaskWorktree, String> {
    release(dir, |_| true, give_up, true).await
}

async fn release(dir: &Path, named: impl Fn(&Path) -> bool, give_up: bool, remove: bool) -> Result<TaskWorktree, String> {
    if !dir.exists() {
        let trash = trash_of(dir).filter(|trash| named(dir) && trash.exists());
        return match trash {
            Some(trash) if remove => empty_trash(dir.to_path_buf(), trash).await.map(|_| TaskWorktree::Removed { freed: 0 }),
            Some(_) => Ok(TaskWorktree::Removed { freed: 0 }),
            None => Ok(TaskWorktree::Absent),
        };
    }
    let Some(top) = git(dir, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await.map(PathBuf::from) else {
        return Ok(TaskWorktree::NotAlondras);
    };
    let alondras = top.join(".git").is_file()
        && named(&top)
        && top.parent().is_some_and(|parent| parent.ends_with(".claude/worktrees"));
    if !alondras {
        return Ok(TaskWorktree::NotAlondras);
    }
    if !give_up && let Some(left) = unsaved(&top).await? {
        return Ok(TaskWorktree::Unsaved(left));
    }
    if !remove {
        return Ok(TaskWorktree::Removed { freed: 0 });
    }
    let trash = trash_of(&top).ok_or_else(|| format!("{} has no folder name", top.display()))?;
    std::fs::rename(&top, &trash).map_err(|e| format!("could not set {} aside to delete it: {e}", top.display()))?;
    let freed = empty_trash(top, trash).await?;
    Ok(TaskWorktree::Removed { freed })
}

/// Where a worktree is moved before it is deleted, beside it: in one rename it is either whole,
/// with its git metadata, or set aside to go, so a deletion cut halfway is finished on the next
/// try instead of leaving a folder git no longer knows.
fn trash_of(top: &Path) -> Option<PathBuf> {
    let name = top.file_name()?.to_str()?;
    top.parent().filter(|parent| parent.ends_with(".claude/worktrees")).map(|parent| parent.join(format!(".{name}.removing")))
}

/// Deletes the worktree that was at `top`, already moved to `trash`: git's record of it, the
/// Bazel output base built from it, then its files, whose space on disk it returns.
async fn empty_trash(top: PathBuf, trash: PathBuf) -> Result<u64, String> {
    let shown = top.display().to_string();
    let emptied = tokio::task::spawn_blocking(move || {
        let failed = |what: &str, e: std::io::Error| format!("could not delete {what} of {}: {e}", top.display());
        if let Some(admin) = git_admin_dir(&trash) {
            remove_tree(&admin).map_err(|e| failed("git's record", e))?;
        }
        if let Some(base) = bazel_output_base(&top, &trash) {
            remove_output_base(&base).map_err(|e| failed("the Bazel output base", e))?;
        }
        let mut tally = crate::disk::Tally::default();
        remove_tree_counting(&trash, &mut tally).map_err(|e| failed("the files", e))?;
        Ok(tally.bytes())
    })
    .await
    .unwrap_or_else(|e| Err(format!("the deletion of {shown} stopped: {e}")));
    if let Err(why) = &emptied {
        log!("{why}");
    }
    emptied
}

/// `<common>/worktrees/<id>`, which the worktree's `.git` file names.
fn git_admin_dir(worktree: &Path) -> Option<PathBuf> {
    let link = std::fs::read_to_string(worktree.join(".git")).ok()?;
    let admin = PathBuf::from(link.trim().strip_prefix("gitdir:")?.trim());
    (admin.is_absolute() && admin.parent().is_some_and(|parent| parent.ends_with("worktrees")) && admin.exists()).then_some(admin)
}

/// The output base Bazel made for the workspace at `top`, found through the `bazel-out` link it
/// left in `worktree` and taken only when the base says it was built from `top`.
fn bazel_output_base(top: &Path, worktree: &Path) -> Option<PathBuf> {
    let out = std::fs::read_link(worktree.join("bazel-out")).ok()?;
    let base = out.ancestors().find(|dir| dir.file_name().is_some_and(|name| name == "execroot"))?.parent()?.to_path_buf();
    let built_from = PathBuf::from(std::fs::read_to_string(base.join(BAZEL_WORKSPACE_MARK)).ok()?.trim());
    (base.is_absolute() && resolved(&built_from).is_some_and(|built| Some(built) == resolved(top))).then_some(base)
}

/// `path` with its parent's links resolved: `path` itself may be gone already.
fn resolved(path: &Path) -> Option<PathBuf> {
    Some(std::fs::canonicalize(path.parent()?).ok()?.join(path.file_name()?))
}

/// The file in an output base that names the workspace it was built from.
const BAZEL_WORKSPACE_MARK: &str = "DO_NOT_BUILD_HERE";

/// Deletes an output base with the file that names its workspace last, so one cut halfway is
/// still found as this worktree's on the next try.
fn remove_output_base(base: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        if entry.file_name() != BAZEL_WORKSPACE_MARK {
            remove_tree(&entry.path())?;
        }
    }
    remove_tree(base)
}

/// Deletes `path` and everything under it, also folders made read-only, as Bazel makes its
/// outputs. Links are deleted, never followed.
fn remove_tree(path: &Path) -> std::io::Result<()> {
    remove_tree_counting(path, &mut crate::disk::Tally::default())
}

/// `remove_tree`, adding to `tally` the space each file it deletes took.
fn remove_tree_counting(path: &Path, tally: &mut crate::disk::Tally) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    tally.add(&meta);
    if !meta.is_dir() {
        return std::fs::remove_file(path);
    }
    if meta.permissions().mode() & 0o700 != 0o700 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(meta.permissions().mode() | 0o700))?;
    }
    for entry in std::fs::read_dir(path)? {
        remove_tree_counting(&entry?.path(), tally)?;
    }
    std::fs::remove_dir(path)
}

/// The work `top` holds that no remote has: uncommitted files and commits not pushed that no
/// [`LocalHome`] keeps.
pub(crate) async fn unsaved(top: &Path) -> Result<Option<String>, String> {
    let status = git(top, &["status", "--porcelain", "--untracked-files=all"]).await;
    let unpushed = unpushed_commits(top).await;
    let (Some(status), Some(unpushed)) = (status, unpushed) else {
        return Err(format!("git could not tell whether {} holds work not saved", top.display()));
    };
    let changed = status.lines().filter(|line| !line.trim().is_empty()).count();
    let mut parts = vec![];
    if changed > 0 {
        parts.push(format!("{changed} uncommitted file(s)"));
    }
    if unpushed > 0 {
        parts.push(format!("{unpushed} commit(s) not pushed"));
    }
    Ok((!parts.is_empty()).then(|| format!("{} in {}", parts.join(" and "), top.display())))
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};

    pub(crate) fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A repository under `root` with a remote, and the worktree Alondra makes for the task whose
    /// slug is `slug`, its branch pushed: what a task leaves once its PR is up.
    pub(crate) fn task_worktree(root: &Path, slug: &str) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        git(root, &["init", "-q", "--bare", "remote.git"]);
        git(root, &["init", "-q", "repo"]);
        let repo = root.join("repo");
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&repo, &["remote", "add", "origin", root.join("remote.git").to_str().unwrap()]);
        let dir = repo.join(".claude/worktrees").join(slug);
        let branch = format!("worktree-{slug}");
        git(&repo, &["worktree", "add", "-q", "-b", &branch, dir.to_str().unwrap()]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "work"]);
        git(&dir, &["push", "-q", "origin", &branch]);
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// The node that replaced another one was launched in that one's worktree, which Claude Code
    /// records only on the old session's job: the successor's own launch folder names it.
    #[tokio::test]
    async fn a_node_launched_in_an_inherited_worktree_keeps_its_protections() {
        let scratch_directory = crate::test_support::Directory::new();
        let root = scratch_directory.parent().unwrap().join(format!("alondra-inherited-{}-{}", std::process::id(), alondra_core::now_millis()));
        std::fs::create_dir_all(&root).unwrap();
        run(&root, &["init", "-q", "repo"]);
        let repo = root.join("repo");
        run(&repo, &["config", "user.email", "test@example.invalid"]);
        run(&repo, &["config", "user.name", "Test"]);
        run(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let wt = repo.join(".claude/worktrees/t-cards-3");
        run(&repo, &["worktree", "add", "-q", "-b", "worktree-t-cards-3", wt.to_str().unwrap()]);
        let home = root.join("claude");
        let job = |short: &str, cwd: &Path| {
            std::fs::create_dir_all(home.join("jobs").join(short)).unwrap();
            std::fs::write(home.join("jobs").join(short).join("state.json"),
                serde_json::json!({"state": "working", "cwd": cwd.to_str().unwrap()}).to_string()).unwrap();
        };
        job("succ0000", &wt);
        job("main0000", &repo);
        let successor = "succ0000-0000-4000-8000-000000000000";
        assert_eq!(worktree_in(&home, successor).as_deref(), Some(wt.as_path()));
        assert_eq!(worktree_in(&home, "main0000-0000-4000-8000-000000000000"), None, "a main checkout is nobody's worktree");

        std::fs::write(wt.join("tokens.swift"), b"let accent = 1").unwrap();
        let dir = worktree_in(&home, successor).unwrap();
        let state = DomainState::new("e");
        let refused = final_refusal_in(&state, &dir, &TaskId::from("t"), &[]).await.unwrap();
        assert_eq!(refused.code, "worktree_not_clean", "a final result with an uncommitted file is refused: {}", refused.message);
        assert!(refused.message.contains("tokens.swift"), "{}", refused.message);
        assert!(leftovers_in(&dir).await.is_some_and(|left| left.contains("1 uncommitted file(s)")), "closing it keeps the work");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A node of the Agent SDK has no `claude --bg` job: its final delivery is checked in the
    /// worktree its task recorded as its own.
    #[tokio::test]
    async fn an_sdk_final_delivery_with_a_dirty_worktree_is_refused() {
        use alondra_core::domain::{Actor, Command, CommandEnvelope, LaunchPlan, PROTOCOL_VERSION, WorktreeOrigin};
        let root = crate::test_support::Directory::new();
        let dir = testing::task_worktree(&root, "t");
        let mut state = DomainState::new("e");
        let mut run = |actor: Actor, command: Command| {
            let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
                command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command };
            state.execute(&actor, &envelope).unwrap();
        };
        run(Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        run(Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t".into(), request_id: "r".into(),
            title: "Task".into(), agent_id: None, review_prs: false, dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")) });
        run(Actor::System, Command::RecordTaskWorktree { task_id: "t".into(), path: dir.to_str().unwrap().into(), origin: WorktreeOrigin::Task, branch: None });
        let session = "sdk-0123abcd";
        assert_eq!(worktree_of(session), None, "an SDK node has no job");
        assert_eq!(final_refusal(&state, session, &TaskId::from("t"), &[]).await.map(|refusal| refusal.code), None, "clean and pushed");
        std::fs::write(dir.join("tokens.swift"), b"let accent = 1").unwrap();
        let refused = final_refusal(&state, session, &TaskId::from("t"), &[]).await.expect("a dirty worktree is refused");
        assert_eq!(refused.code, "worktree_not_clean", "{}", refused.message);
        assert!(refused.message.contains("tokens.swift"), "{}", refused.message);
    }

    #[tokio::test]
    async fn a_final_delivery_needs_the_work_pushed_and_on_the_default_branch_or_in_the_tasks_pr() {
        let scratch_directory = crate::test_support::Directory::new();
        let root = scratch_directory.parent().unwrap().join(format!("alondra-final-{}-{}", std::process::id(), alondra_core::now_millis()));
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

        std::fs::create_dir(clone.join("out")).unwrap();
        std::fs::write(clone.join("out/notes.txt"), "draft").unwrap();
        let (refused, message) = code(final_refusal_in(&state, &clone, &task, &pr).await).unwrap();
        assert_eq!(refused, "worktree_not_clean");
        assert!(message.contains("files git does not track: `out/notes.txt`") && message.contains("or delete it"), "{message}");
        std::fs::rename(clone.join("out/notes.txt"), clone.join("notes.txt")).unwrap();

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

        run(&clone, &["remote", "set-url", "origin", root.join("gone.git").to_str().unwrap()]);
        run(&clone, &["remote", "set-head", "origin", "-d"]);
        assert_eq!(code(final_refusal_in(&state, &clone, &task, &text).await), None,
            "when it cannot be told where the default branch is, the result is taken");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A repository under `root` with no remote, and the worktree of task `t` with one commit of
    /// its own on its branch.
    fn worktree_without_remote(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"], &["commit", "-q", "--allow-empty", "-m", "base"]] {
            run(&repo, args);
        }
        let dir = repo.join(".claude/worktrees/t");
        run(&repo, &["worktree", "add", "-q", "-b", "worktree-t", dir.to_str().unwrap()]);
        run(&dir, &["commit", "-q", "--allow-empty", "-m", "work"]);
        dir
    }

    fn refusal_code(refusal: Option<DomainError>) -> Option<String> {
        refusal.map(|error| error.code)
    }

    #[tokio::test]
    async fn a_final_text_result_of_a_clean_worktree_without_a_remote_is_taken() {
        let root = crate::test_support::Directory::new();
        let dir = worktree_without_remote(&root);
        let text = [DeliveryTarget::Text { text: "Report".into() }];
        assert_eq!(refusal_code(final_refusal_in(&DomainState::new("e"), &dir, &TaskId::from("t"), &text).await), None);
        assert_eq!(unsaved(&dir).await, Ok(None), "closing the task keeps nothing back: the branch stays");
    }

    #[tokio::test]
    async fn a_final_result_of_a_dirty_worktree_without_a_remote_is_refused() {
        let root = crate::test_support::Directory::new();
        let dir = worktree_without_remote(&root);
        std::fs::write(dir.join("notes.txt"), "draft").unwrap();
        let text = [DeliveryTarget::Text { text: "Report".into() }];
        let refused = final_refusal_in(&DomainState::new("e"), &dir, &TaskId::from("t"), &text).await.expect("an untracked file is refused");
        assert_eq!(refused.code, "worktree_not_clean", "{}", refused.message);
        assert!(refused.message.contains("`notes.txt`") && !refused.message.contains("not pushed"), "{}", refused.message);
    }

    #[tokio::test]
    async fn a_final_result_with_commits_not_pushed_to_its_remote_is_refused() {
        let root = crate::test_support::Directory::new();
        let dir = testing::task_worktree(&root, "t");
        run(&dir, &["commit", "-q", "--allow-empty", "-m", "local"]);
        let text = [DeliveryTarget::Text { text: "Report".into() }];
        let refused = final_refusal_in(&DomainState::new("e"), &dir, &TaskId::from("t"), &text).await.expect("an unpushed commit is refused");
        assert_eq!(refused.code, "worktree_not_clean", "{}", refused.message);
        assert!(refused.message.contains("1 commit(s) not pushed"), "{}", refused.message);
    }

    #[tokio::test]
    async fn a_final_result_whose_commits_are_in_the_local_default_branch_is_taken() {
        let root = crate::test_support::Directory::new();
        let text = [DeliveryTarget::Text { text: "Report".into() }];
        let state = DomainState::new("e");

        let dir = worktree_without_remote(&root.join("local"));
        run(&root.join("local/repo"), &["merge", "-q", "--ff-only", "worktree-t"]);
        assert_eq!(refusal_code(final_refusal_in(&state, &dir, &TaskId::from("t"), &text).await), None, "no remote, already on main");

        let dir = testing::task_worktree(&root.join("remote"), "t");
        run(&dir, &["commit", "-q", "--allow-empty", "-m", "local"]);
        let repo = root.join("remote/repo");
        let main = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await.unwrap();
        run(&repo, &["merge", "-q", "--ff-only", "worktree-t"]);
        assert_eq!(refusal_code(final_refusal_in(&state, &dir, &TaskId::from("t"), &text).await), None, "merged into local {main}");
        assert_eq!(unsaved(&dir).await, Ok(None));
    }

    #[tokio::test]
    async fn a_commit_cited_as_evidence_must_be_in_the_history_of_the_node_s_work() {
        let root = crate::test_support::Directory::new();
        let dir = testing::task_worktree(&root, "t-evidence");
        let head = git(&dir, &["rev-parse", "HEAD"]).await.unwrap();
        let cite = |sha: &str| vec![ChecklistMark { item: "2".into(), evidence: vec![alondra_core::domain::Evidence { kind: EvidenceKind::Commit, value: sha.into(), detail: None }] }];
        assert_eq!(evidence_refusal_in(&dir, &cite(&head)).await, None);
        assert_eq!(evidence_refusal_in(&dir, &cite(&head[..7])).await, None, "an abbreviated sha is the same commit");
        let refused = evidence_refusal_in(&dir, &cite("0123456789abcdef0123456789abcdef01234567")).await.unwrap();
        assert_eq!(refused.code, "evidence_commit_not_in_work");
        assert!(refused.message.starts_with("checklist: commit 0123456789abcdef0123456789abcdef01234567 (point 2) is not in your work"), "{}", refused.message);
        testing::git(&dir, &["checkout", "-q", "-b", "elsewhere"]);
        testing::git(&dir, &["commit", "-q", "--allow-empty", "-m", "aside"]);
        let aside = git(&dir, &["rev-parse", "HEAD"]).await.unwrap();
        testing::git(&dir, &["checkout", "-q", "-"]);
        assert_eq!(evidence_refusal_in(&dir, &cite(&aside)).await.map(|e| e.code), Some("evidence_commit_not_in_work".into()),
            "a commit on another branch is not this work");
    }

    #[tokio::test]
    async fn the_worktree_alondra_made_for_a_closed_task_is_deleted_with_what_git_ignores_and_its_branch_stays() {
        let root = crate::test_support::Directory::new();
        let dir = testing::task_worktree(&root, "t-done");
        std::fs::write(dir.join(".gitignore"), ".state/\n").unwrap();
        testing::git(&dir, &["add", ".gitignore"]);
        testing::git(&dir, &["commit", "-q", "-m", "ignore"]);
        testing::git(&dir, &["push", "-q", "origin", "worktree-t-done"]);
        std::fs::create_dir(dir.join(".state")).unwrap();
        std::fs::write(dir.join(".state/db"), "x").unwrap();
        assert_eq!(release_task_worktree(&dir, "t-done", false).await, Ok(TaskWorktree::Removed { freed: 0 }));
        assert!(dir.exists(), "a dry run deletes nothing");
        assert!(matches!(release_task_worktree(&dir, "t-done", true).await, Ok(TaskWorktree::Removed { .. })));
        assert!(!dir.exists());
        testing::git(&root.join("repo"), &["rev-parse", "--verify", "--quiet", "refs/heads/worktree-t-done"]);
        assert_eq!(release_task_worktree(&dir, "t-done", true).await, Ok(TaskWorktree::Absent));
    }

    /// What deleting a worktree freed is what its files took on disk, a file with two links once:
    /// the size of a typical task the gate keeps room for.
    #[tokio::test]
    async fn deleting_a_worktree_says_the_space_its_files_took() {
        use std::os::unix::fs::MetadataExt;
        let root = crate::test_support::Directory::new();
        let dir = testing::task_worktree(&root, "t-sized");
        std::fs::write(dir.join(".gitignore"), "/out/\n").unwrap();
        testing::git(&dir, &["add", ".gitignore"]);
        testing::git(&dir, &["commit", "-q", "-m", "ignore"]);
        testing::git(&dir, &["push", "-q", "origin", "worktree-t-sized"]);
        std::fs::create_dir(dir.join("out")).unwrap();
        std::fs::write(dir.join("out/big"), vec![7u8; 3_000_000]).unwrap();
        std::fs::hard_link(dir.join("out/big"), dir.join("out/same")).unwrap();
        let mut expected = 0;
        let mut walk = vec![dir.clone()];
        while let Some(path) = walk.pop() {
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if path.ends_with("out/same") {
                continue;
            }
            expected += meta.blocks() * 512;
            if meta.is_dir() {
                walk.extend(std::fs::read_dir(&path).unwrap().map(|entry| entry.unwrap().path()));
            }
        }
        let Ok(TaskWorktree::Removed { freed }) = release_task_worktree(&dir, "t-sized", true).await else { panic!("deleted") };
        assert!(expected > 3_000_000, "{expected}");
        assert_eq!(freed, expected);
    }

    #[tokio::test]
    async fn a_closed_tasks_worktree_with_work_not_saved_is_kept() {
        let root = crate::test_support::Directory::new();
        let dir = testing::task_worktree(&root, "t-dirty");
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let Ok(TaskWorktree::Unsaved(left)) = release_task_worktree(&dir, "t-dirty", true).await else { panic!("kept for its file") };
        assert!(left.contains("1 uncommitted file(s)"), "{left}");
        std::fs::remove_file(dir.join("a.txt")).unwrap();
        testing::git(&dir, &["commit", "-q", "--allow-empty", "-m", "local"]);
        let Ok(TaskWorktree::Unsaved(left)) = release_task_worktree(&dir, "t-dirty", true).await else { panic!("kept for its commit") };
        assert!(left.contains("1 commit(s) not pushed"), "{left}");
        assert!(dir.join(".git").is_file());
    }

    /// git deletes its record of a worktree even when it cannot delete all of its files, and what
    /// is left is a folder no git knows: the task's worktree goes whole, never left as the user's.
    #[tokio::test]
    async fn a_closed_tasks_worktree_with_a_read_only_folder_goes_whole_with_gits_record_of_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_support::Directory::new();
        let dir = testing::task_worktree(&root, "t-locked");
        std::fs::write(dir.join(".gitignore"), "/out/\n").unwrap();
        testing::git(&dir, &["add", ".gitignore"]);
        testing::git(&dir, &["commit", "-q", "-m", "ignore"]);
        testing::git(&dir, &["push", "-q", "origin", "worktree-t-locked"]);
        std::fs::create_dir_all(dir.join("out/snapshots")).unwrap();
        std::fs::write(dir.join("out/snapshots/a.png"), "x").unwrap();
        std::fs::set_permissions(dir.join("out/snapshots"), std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(matches!(release_task_worktree(&dir, "t-locked", true).await, Ok(TaskWorktree::Removed { .. })));
        assert!(!dir.exists());
        assert!(!root.join("repo/.claude/worktrees/.t-locked.removing").exists());
        assert!(!root.join("repo/.git/worktrees/t-locked").exists(), "git's record of it goes too");
        assert_eq!(release_task_worktree(&dir, "t-locked", true).await, Ok(TaskWorktree::Absent));
    }

    /// A deletion cut after the worktree was set aside is finished on the next try.
    #[tokio::test]
    async fn a_worktree_set_aside_by_a_deletion_that_was_cut_is_deleted_on_the_next_try() {
        let root = crate::test_support::Directory::new();
        let dir = testing::task_worktree(&root, "t-cut");
        let trash = root.join("repo/.claude/worktrees/.t-cut.removing");
        std::fs::rename(&dir, &trash).unwrap();
        assert_eq!(release_task_worktree(&dir, "t-cut", false).await, Ok(TaskWorktree::Removed { freed: 0 }), "a dry run deletes nothing");
        assert!(trash.exists());
        assert!(matches!(release_task_worktree(&dir, "t-cut", true).await, Ok(TaskWorktree::Removed { .. })));
        assert!(!trash.exists());
        assert!(!root.join("repo/.git/worktrees/t-cut").exists());
    }

    /// The output base Bazel built from the worktree goes with it, found through the worktree's
    /// `bazel-out` link; one built from another workspace is never touched.
    #[tokio::test]
    async fn a_closed_tasks_worktree_goes_with_the_bazel_output_base_built_from_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_support::Directory::new();
        let output = root.join("bazel-root");
        let base = |hash: &str, workspace: &Path, read_only: bool| {
            let base = output.join(hash);
            std::fs::create_dir_all(base.join("execroot/_main/bazel-out/bin")).unwrap();
            std::fs::create_dir_all(base.join("external/rules")).unwrap();
            std::fs::write(base.join("external/rules/BUILD"), "x").unwrap();
            if read_only {
                std::fs::set_permissions(base.join("external/rules"), std::fs::Permissions::from_mode(0o555)).unwrap();
            }
            std::fs::write(base.join(BAZEL_WORKSPACE_MARK), workspace.to_str().unwrap()).unwrap();
            base
        };
        let built = |slug: &str| {
            let dir = testing::task_worktree(&root.join(slug), slug);
            std::fs::write(dir.join(".gitignore"), "/bazel-*\n").unwrap();
            testing::git(&dir, &["add", ".gitignore"]);
            testing::git(&dir, &["commit", "-q", "-m", "ignore"]);
            testing::git(&dir, &["push", "-q", "origin", &format!("worktree-{slug}")]);
            dir
        };
        let dir = built("t-built");
        let own = base("1111", &dir, true);
        std::os::unix::fs::symlink(own.join("execroot/_main/bazel-out"), dir.join("bazel-out")).unwrap();
        let other = built("t-borrowed");
        let foreign = base("2222", &root.join("elsewhere"), false);
        std::os::unix::fs::symlink(foreign.join("execroot/_main/bazel-out"), other.join("bazel-out")).unwrap();

        assert!(matches!(release_task_worktree(&dir, "t-built", true).await, Ok(TaskWorktree::Removed { .. })));
        assert!(!dir.exists());
        assert!(!own.exists(), "its output base goes with it");
        assert!(matches!(release_task_worktree(&other, "t-borrowed", true).await, Ok(TaskWorktree::Removed { .. })));
        assert!(foreign.join("external/rules/BUILD").exists(), "a base built from another workspace stays");
    }

    #[tokio::test]
    async fn a_folder_that_is_not_the_tasks_own_worktree_is_never_deleted() {
        let root = crate::test_support::Directory::new();
        let dir = testing::task_worktree(&root, "t-own");
        assert_eq!(release_task_worktree(&dir, "t-other", true).await, Ok(TaskWorktree::NotAlondras), "another task's name");
        assert_eq!(release_task_worktree(&root.join("repo"), "repo", true).await, Ok(TaskWorktree::NotAlondras), "a main checkout");
        let elsewhere = root.join("t-own");
        testing::git(&root.join("repo"), &["worktree", "add", "-q", "-b", "elsewhere", elsewhere.to_str().unwrap()]);
        assert_eq!(release_task_worktree(&elsewhere, "t-own", true).await, Ok(TaskWorktree::NotAlondras), "outside .claude/worktrees");
        assert!(dir.exists() && elsewhere.exists());
    }
}
