//! The task worktrees Alondra makes, reopens and deletes with git, as the engine asks for them,
//! and the values that travel with them.
use alondra_core::options::WorktreesRoot;
use alondra_core::secret_destination::SecretDestination;
use alondra_domain_rules::{ChecklistMark, DeliveryTarget, DomainError, DomainState, Interaction, TaskId};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::Registry;

/// How long git may take, in all, on each check a final delivery makes in its worktree.
pub const CHECK_LIMIT: Duration = Duration::from_secs(60);

pub type Git<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How a session came to work in its worktree, which decides whether Alondra may delete it.
#[derive(Debug, PartialEq)]
pub enum WorktreeOrigin {
    /// Claude Code made it for the session.
    Made(PathBuf),
    /// The session was launched in it: the worktree of the node it replaced, or a folder the user
    /// gave, which can be a linked worktree too.
    LaunchedIn(PathBuf),
}

/// The folders a task's worktree can never be, whatever git says of them: the user's home and the
/// projects root, and every folder above them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Protected {
    pub home: PathBuf,
    pub projects_root: PathBuf,
}

impl Protected {
    pub fn covers(&self, path: &Path) -> bool {
        let forms = |path: &Path| [Some(path.to_path_buf()), path.canonicalize().ok()].into_iter().flatten().collect::<Vec<_>>();
        let kept: Vec<PathBuf> = forms(&self.home).into_iter().chain(forms(&self.projects_root)).collect();
        forms(path).iter().any(|path| path.parent().is_none() || kept.iter().any(|kept| kept.starts_with(path)))
    }
}

/// What became of the worktree Alondra made for a task once the task closed.
#[derive(Debug, PartialEq, Eq)]
pub enum TaskWorktree {
    /// It was deleted, or would be on a dry run, with the space its files took: none on a dry run
    /// or when a deletion cut halfway is finished.
    Removed { freed: u64 },
    /// It no longer exists.
    Absent,
    /// It is not `.claude/worktrees/<slug>` of a repository: the user's folder, never deleted.
    NotAlondras,
    /// It holds files not committed or commits no remote has.
    Unsaved(String),
    /// It is a link to a folder that cannot be reached now, as on a volume that is not mounted:
    /// left as it is, to be tried again.
    Unavailable(String),
}

#[derive(Debug)]
pub enum LaunchError {
    Workspace(String),
    Storage(String),
}
impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Workspace(why) | Self::Storage(why) => f.write_str(why) }
    }
}
impl From<String> for LaunchError { fn from(why: String) -> Self { Self::Workspace(why) } }
impl std::error::Error for LaunchError {}

/// Where a launched node works.
#[derive(Debug, PartialEq, Eq)]
pub enum Workplace {
    /// The folder as given: not a git checkout, or a linked worktree already.
    Given(PathBuf),
    /// The task's worktree an earlier node left, with its work.
    Reused(PathBuf),
    /// A worktree Alondra made for the task from the default branch, or from the task's branch.
    Made(PathBuf),
}

impl Workplace {
    pub fn dir(&self) -> &Path {
        match self {
            Workplace::Given(dir) | Workplace::Reused(dir) | Workplace::Made(dir) => dir,
        }
    }

}

/// The storage folder a worktree outside its repository was made in, recorded beside it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub repository: PathBuf,
    pub root: WorktreesRoot,
}

/// A worktree Claude Code noted it made for an isolated subagent, which may be gone since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotedWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
}

pub use alondra_provider_contract::subagent_id;

const NOTE_BYTES: u64 = 64 * 1024;

/// What Claude Code noted of the isolated subagents session `session_id` ran, nested ones included:
/// `projects/<folder>/<session>/subagents/agent-<id>.meta.json` in `claude_home`, each with the
/// `worktreePath` and `worktreeBranch` it made. Links and anything but a plain note are not read.
pub fn noted_subagent_worktrees(claude_home: &Path, session_id: &str) -> Vec<NotedWorktree> {
    let plain = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
    let session_like = !session_id.is_empty() && session_id.len() <= 64 && session_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let Some(projects) = session_like.then(|| claude_home.join("projects").canonicalize().ok()).flatten() else { return vec![] };
    let mut noted = vec![];
    for bucket in std::fs::read_dir(projects).into_iter().flatten().filter_map(Result::ok) {
        let folder = bucket.path().join(session_id).join("subagents");
        if !plain(&bucket.path()) || !plain(&bucket.path().join(session_id)) || !plain(&folder) {
            continue;
        }
        for entry in std::fs::read_dir(folder).into_iter().flatten().filter_map(Result::ok) {
            let path = entry.path();
            let named = path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("agent-") && name.ends_with(".meta.json"));
            if !named || !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() <= NOTE_BYTES) {
                continue;
            }
            let Some(note) = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok()) else { continue };
            if note["spawnedWithWorktree"].as_bool() != Some(true) {
                continue;
            }
            let Some(worktree) = note["worktreePath"].as_str().map(PathBuf::from) else { continue };
            if !worktree.is_absolute() || !worktree.components().all(|part| matches!(part, std::path::Component::RootDir | std::path::Component::Normal(_))) {
                continue;
            }
            noted.push(NotedWorktree { path: worktree, branch: note["worktreeBranch"].as_str().map(str::to_owned) });
        }
    }
    noted.sort_by(|a, b| a.path.cmp(&b.path));
    noted.dedup();
    noted
}

/// The folder name of a task's worktree, from its id or title.
pub fn slug(name: &str) -> String {
    let slug: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let slug = slug.trim_matches('-').chars().take(64).collect::<String>();
    if slug.is_empty() { "node".into() } else { slug }
}

/// What the engine asks of git and the disk about task worktrees.
pub trait Worktrees: Send + Sync {
    /// The worktree a session works in.
    fn worktree_of(&self, session_id: &str) -> Option<PathBuf>;
    /// How a session came to work in its worktree.
    fn origin(&self, session_id: &str) -> Option<WorktreeOrigin>;
    /// Where Claude Code puts the worktree of subagent `agent` of the node working in `own`.
    fn subagent_worktree(&self, own: &Path, agent: &str) -> Option<PathBuf>;
    /// The worktrees Claude Code noted it made for the isolated subagents of session `session_id`.
    fn noted_subagent_worktrees(&self, session_id: &str) -> Vec<NotedWorktree>;
    /// Why `path` cannot be a task's worktree, if it cannot.
    fn registrable(&self, path: &Path, protected: &Protected) -> Result<(), String>;
    fn branch_in<'a>(&'a self, dir: &'a Path) -> Git<'a, Option<String>>;
    /// What a node would leave behind in its worktree.
    fn leftovers<'a>(&'a self, session_id: &'a str) -> Git<'a, Option<String>>;
    fn release_task_worktree<'a>(&'a self, dir: &'a Path, slug: &'a str, remove: bool) -> Git<'a, Result<TaskWorktree, String>>;
    fn release_recorded_worktree<'a>(&'a self, dir: &'a Path, give_up: bool) -> Git<'a, Result<TaskWorktree, String>>;
    fn discard_worktree<'a>(&'a self, dir: &'a Path, keep_branch: bool) -> Git<'a, Result<TaskWorktree, String>>;
    /// Why the link `dir` is cannot be followed now, if it cannot.
    /// Whether anything is at `dir` still: a worktree the user deleted is gone.
    fn exists(&self, dir: &Path) -> bool;
    /// Whether `dir`, or the folder the link `dir` is points to, is a folder with nothing in it: all
    /// that is left of a worktree git already removed. Nothing in it can be lost.
    fn is_empty_shell(&self, dir: &Path) -> bool {
        dir.is_dir() && std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none())
    }
    fn unreachable_link(&self, dir: &Path) -> Option<String>;
    fn uncommitted_files_of_alondras_worktree<'a>(&'a self, dir: &'a Path) -> Git<'a, Result<Option<usize>, String>>;
    /// What the worktree at `top` holds that no remote has.
    fn unsaved<'a>(&'a self, top: &'a Path) -> Git<'a, Result<Option<String>, String>>;
    /// Where a node launched in `cwd` works.
    fn isolate<'a>(&'a self, cwd: &'a Path, slug: &'a str, known: Option<&'a Path>, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<Workplace, LaunchError>>;
    /// The task's worktree reopened, and whether its original folder came back.
    fn reopen<'a>(&'a self, cwd: &'a Path, slug: &'a str, original: &'a Path, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<(Workplace, bool), LaunchError>>;
    /// The volume a launch will write.
    fn storage_for<'a>(&'a self, cwd: &'a Path, slug: &'a str, known: Option<&'a Path>, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<PathBuf, LaunchError>>;
    fn recovery_directory_matches<'a>(&'a self, cwd: &'a Path, original: &'a Path) -> Git<'a, bool>;
    /// The worktree Alondra chose for the node, which it may delete with it.
    fn chosen(&self, workplace: &Workplace) -> Option<PathBuf>;
    /// The storage record beside a worktree made outside its repository.
    fn stored(&self, worktree: &Path) -> Option<Repository>;
    /// Puts back the record of a worktree's storage folder.
    fn restore(&self, worktree: &Path, record: &Repository) -> Result<(), String>;
    /// The worktrees folder at `path`, pinned to the volume it is on.
    fn capture_root(&self, path: &Path) -> Result<WorktreesRoot, String>;
    /// Why a node's final delivery cannot be taken yet, git given `limit` in all.
    fn final_refusal<'a>(&'a self, state: &'a DomainState, session_id: &'a str, task: &'a TaskId, targets: &'a [DeliveryTarget], limit: Duration) -> Git<'a, Option<DomainError>>;
    /// Why the commits a final delivery cites cannot be taken.
    fn evidence_refusal<'a>(&'a self, state: &'a DomainState, session_id: &'a str, task: &'a TaskId, checklist: &'a [ChecklistMark], limit: Duration) -> Git<'a, Option<DomainError>>;
    /// Why a node cannot ask for a secret written to `destination`.
    fn secret_refusal(&self, state: &DomainState, session_id: &str, task: &TaskId, destination: &SecretDestination) -> Option<DomainError>;
    /// Writes the secret `value` where the open card `interaction` says; what the node is told.
    fn write_secret(&self, state: &DomainState, interaction: &Interaction, value: &str) -> Result<String, String>;
}

/// The folders a task's worktree can never be, and the git that tells whether a folder is one: what
/// the domain store checks before it records a worktree.
#[derive(Clone)]
pub struct WorktreeRules {
    pub protected: Protected,
    ports: Arc<Registry>,
}

impl WorktreeRules {
    pub fn new(protected: Protected, ports: Arc<Registry>) -> Self {
        Self { protected, ports }
    }

    /// The same git, keeping `protected` instead.
    pub fn protecting(&self, protected: Protected) -> Self {
        Self { protected, ports: self.ports.clone() }
    }

    /// Why `path` cannot be a task's worktree, if it cannot.
    pub fn registrable(&self, path: &Path) -> Result<(), String> {
        self.ports.worktrees().registrable(path, &self.protected)
    }

    pub fn subagent_worktree(&self, own: &Path, agent: &str) -> Option<PathBuf> {
        self.ports.worktrees().subagent_worktree(own, agent)
    }

    pub fn stored(&self, worktree: &Path) -> Option<Repository> {
        self.ports.worktrees().stored(worktree)
    }
}

/// Worktrees of a task that are links to folders that cannot be reached now, as on a volume that is
/// not mounted: the close leaves them as they are and tries again later, without a card.
#[derive(Debug)]
pub struct Unreachable(pub String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; Alondra leaves it as it is and tries again later", self.0)
    }
}

impl std::error::Error for Unreachable {}

/// What the node of a task that cannot close yet is told: each worktree with work left, and how
/// to settle it.
pub fn settle_text(task: &TaskId, left: &[(String, String)]) -> String {
    let list: String = left.iter().map(|(path, what)| format!("\n- {path}: {what}")).collect();
    format!("Alondra cannot close task {task} yet: these worktrees of the task, yours or those of the subagents you \
launched, hold work no remote has:{list}\nFor each one, commit the work and push it to its branch, or give it up. \
Then call cleanup_done with task_id {task}, listing in discard the paths you give up: Alondra deletes those as they \
are and closes the task once no worktree of it holds work no remote has.")
}

#[cfg(test)]
mod noted_tests {
    use super::*;

    fn note(home: &Path, session: &str, name: &str, body: serde_json::Value) {
        let folder = home.join("projects").join("-Users-ana-repo").join(session).join("subagents");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(name), body.to_string()).unwrap();
    }

    #[test]
    fn the_worktrees_claude_code_noted_for_a_sessions_subagents_are_read_and_nothing_else() {
        let home = tempfile::tempdir().unwrap();
        let session = "4e335a6e-67f5-4ec2-9ea4-95c262b7dd56";
        note(home.path(), session, "agent-a1.meta.json", serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-a1", "worktreeBranch": "worktree-agent-a1", "spawnedWithWorktree": true}));
        note(home.path(), session, "agent-a2.meta.json", serde_json::json!({"agentType": "Explore", "spawnedWithWorktree": false}));
        note(home.path(), session, "agent-a3.meta.json", serde_json::json!({"worktreePath": "r/agent-a3", "spawnedWithWorktree": true}));
        note(home.path(), session, "agent-a4.meta.json", serde_json::json!({"worktreePath": "/r/../etc", "spawnedWithWorktree": true}));
        note(home.path(), session, "agent-a5.jsonl", serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-a5", "spawnedWithWorktree": true}));
        note(home.path(), "other-session", "agent-a6.meta.json", serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-a6", "spawnedWithWorktree": true}));
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("x.json"), serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-a7", "spawnedWithWorktree": true}).to_string()).unwrap();
        let folder = home.path().join("projects/-Users-ana-repo").join(session).join("subagents");
        std::os::unix::fs::symlink(outside.path().join("x.json"), folder.join("agent-a7.meta.json")).unwrap();
        assert_eq!(noted_subagent_worktrees(home.path(), session),
            vec![NotedWorktree { path: "/r/.claude/worktrees/agent-a1".into(), branch: Some("worktree-agent-a1".into()) }]);
        assert!(noted_subagent_worktrees(home.path(), "../-Users-ana-repo").is_empty(), "a session is one name, never a path");
    }

    #[test]
    fn a_projects_folder_that_links_to_the_shared_one_is_read_through_its_link() {
        let shared = tempfile::tempdir().unwrap();
        note(shared.path(), "s1", "agent-b1.meta.json", serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-b1", "spawnedWithWorktree": true}));
        let account = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(shared.path().join("projects"), account.path().join("projects")).unwrap();
        assert_eq!(noted_subagent_worktrees(account.path(), "s1").len(), 1);
    }
}
