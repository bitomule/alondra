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

pub use alondra_provider_contract::{Conversations, SessionWorktree, SessionWorktrees, subagent_id};

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

