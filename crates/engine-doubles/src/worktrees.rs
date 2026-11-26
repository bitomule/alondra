//! Task worktrees without git: a folder whose `.git` is a folder is a repository's main checkout,
//! and the double makes a linked worktree as a folder whose `.git` file names that repository. A
//! worktree holds the files of its repository when it is made; a file written or changed after
//! that, until the test commits it, is work not saved. It keeps the same contract as the Mac's git
//! (`crate::contract::worktrees`), which `crates/system` passes with git itself.
use alondra_core::options::WorktreesRoot;
use alondra_core::secret_destination::SecretDestination;
use alondra_domain_model::{
    Author, ChecklistMark, DeliveryTarget, DomainError, DomainState, EvidenceKind, Interaction, Scope, TaskId,
};
use alondra_engine_ports::worktrees::{
    Git, LaunchError, Protected, Repository, TaskWorktree, WorktreeOrigin, Workplace, Worktrees as Port,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// A file as it was last committed: its length and when it changed.
type Committed = BTreeMap<PathBuf, (u64, std::time::SystemTime)>;

pub struct Worktrees {
    claude_home: fn() -> PathBuf,
}

#[derive(Default)]
struct State {
    committed: BTreeMap<PathBuf, Committed>,
    commits: BTreeMap<PathBuf, BTreeSet<String>>,
    slow: BTreeMap<PathBuf, Duration>,
    /// The branch `worktree-<name>` each worktree made in a repository leaves there, by the
    /// repository's main checkout and the worktree's name.
    branches: BTreeSet<(PathBuf, String)>,
    /// The branch a test checked out in a worktree, by the worktree's top.
    checked_out: BTreeMap<PathBuf, String>,
    /// The checkouts holding a commit their remote does not have yet.
    unpushed: BTreeSet<PathBuf>,
    /// The repositories, by their main checkout, that have no remote.
    remoteless: BTreeSet<PathBuf>,
    /// The commit each checkout's branch is at, once a commit of its own moved it: a branch with
    /// none is one its default branch holds.
    tips: BTreeMap<PathBuf, String>,
    next_commit: u64,
}

fn no_claude_home() -> PathBuf {
    PathBuf::from("/nonexistent/alondra-test-claude-home")
}

impl Default for Worktrees {
    fn default() -> Self {
        Self::new(no_claude_home)
    }
}

impl Worktrees {
    /// A double that finds the sessions' worktrees in the Claude folder `claude_home` names.
    pub fn new(claude_home: fn() -> PathBuf) -> Self {
        Self { claude_home }
    }

    fn state(&self) -> std::sync::MutexGuard<'static, State> {
        state()
    }

    fn uncommitted(&self, top: &Path) -> Vec<String> {
        let committed = self.state().committed.get(&key(top)).cloned().unwrap_or_default();
        let mut changed: Vec<String> = files(top).into_iter()
            .filter(|(file, seen)| committed.get(file) != Some(seen))
            .map(|(file, _)| file.to_string_lossy().into_owned())
            .collect();
        changed.extend(committed.keys().filter(|file| !top.join(file).exists()).map(|file| file.to_string_lossy().into_owned()));
        changed.sort();
        changed
    }

    /// What the checkout at `top` holds that no remote has, as the Mac's git words it.
    fn left(&self, top: &Path) -> Option<String> {
        let changed = self.uncommitted(top).len();
        let mut parts = vec![];
        if changed > 0 {
            parts.push(format!("{changed} uncommitted file(s)"));
        }
        let remoteless = repository_of(top).is_some_and(|repo| self.state().remoteless.contains(&canonical(&repo)));
        if self.state().unpushed.contains(&key(top)) && !remoteless {
            parts.push("1 commit(s) not pushed".to_owned());
        }
        (!parts.is_empty()).then(|| format!("{} in {}", parts.join(" and "), top.display()))
    }

    fn release(&self, dir: &Path, named: impl Fn(&Path) -> bool, give_up: bool, remove: bool) -> Result<TaskWorktree, String> {
        let Ok(meta) = std::fs::symlink_metadata(dir) else { return Ok(TaskWorktree::Absent) };
        let link = meta.file_type().is_symlink();
        if link && !dir.exists() {
            if !folder_of_target_is_there(dir) {
                return Ok(TaskWorktree::Unavailable(unreachable(dir)));
            }
            if remove {
                std::fs::remove_file(dir).map_err(|why| format!("could not delete the link {}: {why}", dir.display()))?;
            }
            return Ok(TaskWorktree::Removed { freed: 0 });
        }
        if self.is_empty_shell(dir) && alondras(dir) && named(dir) {
            if remove {
                let top = canonical(dir);
                std::fs::remove_dir(&top).map_err(|why| format!("could not delete {}: {why}", top.display()))?;
                if link {
                    std::fs::remove_file(dir).map_err(|why| format!("could not delete the link {}: {why}", dir.display()))?;
                }
            }
            return Ok(TaskWorktree::Removed { freed: 0 });
        }
        let place = if link { Some(dir.to_path_buf()) } else { linked_top(dir) };
        let Some(place) = place.filter(|place| alondras(place) && named(place)) else {
            return Ok(TaskWorktree::NotAlondras);
        };
        let top = canonical(&place);
        if !top.join(".git").is_file() {
            return Ok(TaskWorktree::NotAlondras);
        }
        if let Some(left) = self.left(&top).filter(|_| !give_up) {
            return Ok(TaskWorktree::Unsaved(left));
        }
        if remove {
            let key = key(&top);
            if let Some(admin) = admin(&top) {
                std::fs::remove_dir_all(&admin).map_err(|why| format!("could not delete {}: {why}", admin.display()))?;
            }
            std::fs::remove_dir_all(&top).map_err(|why| format!("could not delete {}: {why}", top.display()))?;
            if link {
                std::fs::remove_file(dir).map_err(|why| format!("could not delete the link {}: {why}", dir.display()))?;
            }
            let mut state = self.state();
            state.committed.remove(&key);
            state.unpushed.remove(&key);
            state.tips.remove(&key);
        }
        Ok(TaskWorktree::Removed { freed: 0 })
    }

    fn node_worktree(&self, state: &DomainState, session_id: &str, task: &TaskId) -> Option<PathBuf> {
        self.worktree_of(session_id).or_else(|| {
            let task = state.tasks.get(task)?;
            task.open_worktrees()
                .find(|(_, worktree)| worktree.origin == alondra_domain_model::WorktreeOrigin::Task)
                .map(|(path, _)| PathBuf::from(path))
        })
    }

    /// The task's worktree `original` was in, now gone, named `slug`: in its repository's
    /// `.claude/worktrees`, or in a worktrees folder beside the record of its repository.
    fn lost(&self, original: &Path, slug: &str) -> Option<(PathBuf, PathBuf, Option<Repository>)> {
        let worktree = original.ancestors().find(|path| path.file_name().is_some_and(|name| name == slug))?;
        if worktree.exists() {
            return None;
        }
        let parent = worktree.parent()?;
        if parent.ends_with(".claude/worktrees") {
            return Some((parent.parent()?.parent()?.to_path_buf(), worktree.to_path_buf(), None));
        }
        let record = self.stored(worktree)?;
        Some((record.repository.clone(), worktree.to_path_buf(), Some(record)))
    }

    fn checkouts(&self, state: &DomainState, session_id: &str, task: &TaskId) -> Result<Vec<PathBuf>, String> {
        let unknown = || "Alondra cannot tell which repository this task works in".to_owned();
        let worktree = self.node_worktree(state, session_id, task).ok_or_else(unknown)?;
        let own = linked_top(&worktree).or_else(|| main_top(&worktree)).ok_or_else(unknown)?;
        let main = repository_of(&own).ok_or_else(unknown)?;
        Ok(if main == own { vec![own] } else { vec![own, main] })
    }
}

/// What every double knows of the checkouts it made or a test made: the paths are each test's own.
fn state() -> std::sync::MutexGuard<'static, State> {
    static STATE: std::sync::OnceLock<Mutex<State>> = std::sync::OnceLock::new();
    STATE.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner)
}

/// Makes `path` a repository's main checkout, with the files already in it committed.
pub fn repository(path: &Path) {
    std::fs::create_dir_all(path.join(".git/info")).unwrap();
    std::fs::write(path.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    commit(path);
}

/// Commits every file of the checkout at `top` as it is now; the commit's id.
pub fn commit(top: &Path) -> String {
    let files = files(top);
    let mut state = state();
    state.next_commit += 1;
    let id = format!("{:040x}", state.next_commit);
    state.committed.insert(key(top), files);
    state.commits.entry(canonical(top)).or_default().insert(id.clone());
    state.tips.insert(key(top), id.clone());
    id
}

/// Takes every remote from the repository whose main checkout is `repo`.
pub fn without_remote(repo: &Path) {
    state().remoteless.insert(canonical(repo));
}

/// Commits every file of the checkout at `top` without pushing: its remote lacks the commit until
/// [`push`].
pub fn local_commit(top: &Path) -> String {
    let id = commit(top);
    state().unpushed.insert(key(top));
    id
}

/// Pushes the commits of the checkout at `top`.
pub fn push(top: &Path) {
    state().unpushed.remove(&key(top));
}

/// Whether the branch `worktree-<name>` a worktree left is still in the repository at `repo`.
pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    branch.strip_prefix("worktree-").is_some_and(|name| state().branches.contains(&(canonical(repo), name.to_owned())))
}

/// Checks `branch` out in the linked worktree at `top`, in place of the one it was made on.
pub fn checkout(top: &Path, branch: &str) {
    state().checked_out.insert(canonical(top), branch.into());
}

/// Makes every check of the checkout at `top` take `delay`, as git does in a large repository.
pub fn slow(top: &Path, delay: Duration) {
    state().slow.insert(canonical(top), delay);
}

/// What `check` finds in the checkout at `top` once its delay has passed, or, past `limit`, the
/// refusal the Mac's git gives a check that did not finish.
async fn within(limit: Duration, top: &Path, what: &str, check: impl FnOnce() -> Option<DomainError>) -> Option<DomainError> {
    let delay = state().slow.get(top).copied().unwrap_or_default();
    if delay > limit {
        tokio::time::sleep(limit).await;
        return Some(DomainError::new("worktree_check_timed_out", format!(
            "Nothing was presented yet: git took more than {} s to {what} in {}. Send the same present_result again, \
             with the same interaction_id: it does not duplicate the card, and git usually answers faster the second time.",
            limit.as_secs(), top.display())));
    }
    tokio::time::sleep(delay).await;
    check()
}

/// Makes `dir` a linked worktree of the repository at `repo`, with its files committed.
pub fn make_worktree(repo: &Path, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|why| format!("could not make {}: {why}", dir.display()))?;
    for (file, _) in files(repo) {
        let to = dir.join(&file);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|why| why.to_string())?;
        }
        std::fs::copy(repo.join(&file), &to).map_err(|why| why.to_string())?;
    }
    let name = dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let admins = canonical(repo).join(".git/worktrees");
    let admin = (0..).map(|n| admins.join(if n == 0 { name.clone() } else { format!("{name}{n}") })).find(|admin| !admin.exists()).unwrap();
    std::fs::create_dir_all(&admin).map_err(|why| why.to_string())?;
    std::fs::write(dir.join(".git"), format!("gitdir: {}\n", admin.display())).map_err(|why| why.to_string())?;
    state().branches.insert((canonical(repo), name));
    commit(dir);
    state().tips.remove(&key(dir));
    Ok(())
}

/// The worktrees tests start from, as `crates/system`'s helpers of the same names make them with git.
pub mod testing {
    use std::path::{Path, PathBuf};

    /// A repository under `root` and the worktree Alondra makes for the task whose slug is `slug`,
    /// everything in it committed.
    pub fn task_worktree(root: &Path, slug: &str) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        super::repository(&repo);
        let dir = repo.join(".claude/worktrees").join(slug);
        super::make_worktree(&repo, &dir).unwrap();
        dir
    }

    /// The worktree Alondra makes for the task `slug` in a repository under `root` that has no remote.
    pub fn remoteless_task_worktree(root: &Path, slug: &str) -> PathBuf {
        let dir = task_worktree(root, slug);
        super::without_remote(&root.join("repo"));
        dir
    }

    /// The task's worktree moved by hand into `volume` and left as a link in its place: the link
    /// and the folder it points to.
    pub fn linked_task_worktree(root: &Path, slug: &str, volume: &Path) -> (PathBuf, PathBuf) {
        let link = task_worktree(root, slug);
        std::fs::create_dir_all(volume).unwrap();
        let moved = volume.canonicalize().unwrap().join(slug);
        std::fs::rename(&link, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &link).unwrap();
        super::commit(&moved);
        (link, moved)
    }
}

/// Every file under `top` but git's own and the worktrees beside it, by its path inside `top`.
fn files(top: &Path) -> Committed {
    let mut found = Committed::new();
    let mut folders = vec![top.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = path.strip_prefix(top).unwrap().to_path_buf();
            if relative == Path::new(".git") || relative == Path::new(".claude") {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            if meta.is_dir() {
                folders.push(path);
            } else {
                found.insert(relative, (meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH)));
            }
        }
    }
    found
}

/// What the double knows a checkout by: a linked worktree by its folder in the repository, which
/// a move by hand keeps, a main checkout by its path.
fn key(top: &Path) -> PathBuf {
    admin(top).unwrap_or_else(|| canonical(top))
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The top of the main checkout `path` is in.
fn main_top(path: &Path) -> Option<PathBuf> {
    let path = canonical(path);
    path.ancestors().find(|folder| folder.join(".git").is_dir()).map(Path::to_path_buf)
}

/// The top of the linked worktree `path` is in, when the closest checkout above it is one.
fn linked_top(path: &Path) -> Option<PathBuf> {
    let path = canonical(path);
    let top = path.ancestors().find(|folder| folder.join(".git").exists())?;
    top.join(".git").is_file().then(|| top.to_path_buf())
}

/// The main checkout of the repository the checkout at `top` belongs to.
fn repository_of(top: &Path) -> Option<PathBuf> {
    if top.join(".git").is_dir() {
        return Some(top.to_path_buf());
    }
    Some(admin(top)?.parent()?.parent()?.parent()?.to_path_buf())
}

/// The folder in its repository's `.git/worktrees` that the linked worktree at `top` names.
fn admin(top: &Path) -> Option<PathBuf> {
    let named = std::fs::read_to_string(top.join(".git")).ok()?;
    Some(PathBuf::from(named.trim().strip_prefix("gitdir: ")?))
}

/// Git gives paths back trimmed: a checkout whose path ends in white space is one it cannot name.
fn unnamed(top: &Path) -> Result<(), String> {
    match top.to_string_lossy().ends_with(char::is_whitespace) {
        true => Err(format!("git cannot name the checkout {}", top.display())),
        false => Ok(()),
    }
}

/// What git answers: a process of its own, so the answer never comes in the poll that asked.
fn answer<'a, T>(work: impl std::future::Future<Output = T> + Send + 'a) -> Git<'a, T> {
    Box::pin(async move {
        tokio::task::yield_now().await;
        work.await
    })
}

/// Why the link `dir` cannot be followed, as the Mac's git words it.
fn unreachable(dir: &Path) -> String {
    let target = std::fs::read_link(dir).unwrap_or_default();
    format!("{} is a link to {}, which cannot be reached now", dir.display(), target.display())
}

/// The folder that holds what the link `dir` points to can be reached, so a target that is not
/// there is gone for good, and not on a volume that is not mounted.
fn folder_of_target_is_there(dir: &Path) -> bool {
    let target = std::fs::read_link(dir).unwrap_or_default();
    dir.parent().map(|parent| parent.join(target)).and_then(|target| target.parent().map(Path::to_path_buf)).is_some_and(|folder| folder.is_dir())
}

const RECORD: &str = ".alondra-repository.json";

/// The worktrees folder, while its volume is there: a test takes the volume away by removing it.
fn ready(root: &WorktreesRoot) -> Result<PathBuf, String> {
    root.path.is_dir().then(|| root.path.clone()).ok_or_else(|| format!("Worktree folder unavailable: {}", root.path.display()))
}

/// A worktree Alondra made: `.claude/worktrees/<name>` of its repository, or a task's folder in a
/// worktrees folder, beside the record of its repository.
fn alondras(top: &Path) -> bool {
    top.parent().is_some_and(|parent| parent.ends_with(".claude/worktrees") || parent.join(RECORD).is_file())
}

impl Port for Worktrees {
    fn worktree_of(&self, session_id: &str) -> Option<PathBuf> {
        self.origin(session_id).map(|origin| match origin {
            WorktreeOrigin::Made(dir) | WorktreeOrigin::LaunchedIn(dir) => dir,
        })
    }

    fn origin(&self, session_id: &str) -> Option<WorktreeOrigin> {
        let short = session_id.get(..8)?;
        let state = std::fs::read_to_string((self.claude_home)().join("jobs").join(short).join("state.json")).ok()?;
        let job: serde_json::Value = serde_json::from_str(&state).ok()?;
        if let Some(dir) = job.get("worktreePath").and_then(serde_json::Value::as_str).map(PathBuf::from) {
            return dir.exists().then_some(WorktreeOrigin::Made(dir));
        }
        let cwd = PathBuf::from(job.get("cwd")?.as_str()?);
        cwd.join(".git").is_file().then_some(WorktreeOrigin::LaunchedIn(cwd))
    }

    fn subagent_worktree(&self, own: &Path, agent: &str) -> Option<PathBuf> {
        let top = linked_top(own).or_else(|| main_top(own))?;
        Some(repository_of(&top)?.join(".claude").join("worktrees").join(format!("agent-{agent}")))
    }

    fn registrable(&self, path: &Path, protected: &Protected) -> Result<(), String> {
        let shown = path.display();
        if !path.is_absolute() {
            return Err(format!("{shown} is not an absolute path"));
        }
        if protected.covers(path) {
            return Err(format!("{shown} is the user's home, the projects folder or a folder above them, never a task's worktree"));
        }
        let canonical = path.canonicalize().map_err(|e| format!("{shown} cannot be read: {e}"))?;
        let top = linked_top(&canonical).or_else(|| main_top(&canonical)).ok_or_else(|| format!("{shown} is not in a git repository"))?;
        if top != canonical {
            return Err(format!("{shown} is not the top of a git worktree: its worktree is {}", top.display()));
        }
        Ok(())
    }

    fn branch_in<'a>(&'a self, dir: &'a Path) -> Git<'a, Option<String>> {
        answer(async move {
            let top = linked_top(dir).or_else(|| main_top(dir))?;
            if let Some(branch) = self.state().checked_out.get(&top) {
                return Some(branch.clone());
            }
            Some(if top.join(".git").is_dir() { "main".into() } else { format!("worktree-{}", top.file_name()?.to_string_lossy()) })
        })
    }

    fn summary(&self, dir: &Path) -> String {
        let Some(top) = linked_top(dir).or_else(|| main_top(dir)) else {
            return format!("{} (not a readable git worktree)", dir.display());
        };
        let checked_out = self.state().checked_out.get(&top).cloned();
        let branch = checked_out.unwrap_or_else(|| match top.join(".git").is_dir() {
            true => "main".into(),
            false => format!("worktree-{}", top.file_name().unwrap_or_default().to_string_lossy()),
        });
        let changed = self.uncommitted(&top);
        let uncommitted = match changed.len() {
            0 => "nothing uncommitted".to_owned(),
            n => format!("{n} uncommitted:\n{}", changed.join("\n")),
        };
        let commits: Vec<String> = self.state().commits.get(&top).into_iter().flatten().rev().take(5).cloned().collect();
        format!("{}\nbranch {branch}\n{uncommitted}\nlast commits:\n{}", dir.display(), commits.join("\n"))
    }

    fn leftovers<'a>(&'a self, session_id: &'a str) -> Git<'a, Option<String>> {
        answer(async move {
            self.left(&self.worktree_of(session_id)?)
        })
    }

    fn release_task_worktree<'a>(&'a self, dir: &'a Path, slug: &'a str, remove: bool) -> Git<'a, Result<TaskWorktree, String>> {
        answer(async move { self.release(dir, |top| top.file_name().is_some_and(|name| name == slug), false, remove) })
    }

    fn release_finished_worktree<'a>(&'a self, dir: &'a Path, slug: Option<&'a str>, give_up: bool, pr_merged: bool) -> Git<'a, Result<TaskWorktree, String>> {
        answer(async move {
            let top = linked_top(dir);
            let facts = top.as_ref().map(|top| {
                let state = self.state();
                let in_default = !state.tips.contains_key(&key(top));
                let remoteless = repository_of(top).is_some_and(|repo| state.remoteless.contains(&canonical(&repo)));
                (in_default, !remoteless && !state.unpushed.contains(&key(top)))
            });
            let goes = facts.is_none_or(|(in_default, on_its_remote)| in_default || (pr_merged && on_its_remote));
            let branch = top.and_then(|top| Some((repository_of(&top)?, top.file_name()?.to_string_lossy().into_owned())));
            let released = self.release(dir, |top| slug.is_none_or(|slug| top.file_name().is_some_and(|name| name == slug)), give_up && slug.is_none(), true)?;
            if let (TaskWorktree::Removed { .. }, true, Some(branch)) = (&released, goes, branch) {
                self.state().branches.remove(&branch);
            }
            Ok(released)
        })
    }

    fn release_recorded_worktree<'a>(&'a self, dir: &'a Path, give_up: bool) -> Git<'a, Result<TaskWorktree, String>> {
        answer(async move { self.release(dir, |_| true, give_up, true) })
    }

    fn discard_worktree<'a>(&'a self, dir: &'a Path, keep_branch: bool) -> Git<'a, Result<TaskWorktree, String>> {
        answer(async move {
            let top = linked_top(dir);
            let pushed = top.as_ref().is_none_or(|top| !self.state().unpushed.contains(&key(top)));
            let branch = top.and_then(|top| Some((repository_of(&top)?, top.file_name()?.to_string_lossy().into_owned())));
            let released = self.release(dir, |_| true, true, true)?;
            if let (TaskWorktree::Removed { .. }, false, true, Some(branch)) = (&released, keep_branch, pushed, branch) {
                self.state().branches.remove(&branch);
            }
            Ok(released)
        })
    }

    fn exists(&self, dir: &Path) -> bool {
        dir.exists()
    }
    fn unreachable_link(&self, dir: &Path) -> Option<String> {
        let meta = std::fs::symlink_metadata(dir).ok()?;
        (meta.file_type().is_symlink() && !dir.exists() && !folder_of_target_is_there(dir)).then(|| unreachable(dir))
    }

    fn uncommitted_files_of_alondras_worktree<'a>(&'a self, dir: &'a Path) -> Git<'a, Result<Option<usize>, String>> {
        answer(async move {
            Ok(linked_top(dir).filter(|top| alondras(top)).map(|top| self.uncommitted(&top).len()))
        })
    }

    fn unsaved<'a>(&'a self, top: &'a Path) -> Git<'a, Result<Option<String>, String>> {
        answer(async move {
            Ok(self.left(top))
        })
    }

    fn isolate<'a>(&'a self, cwd: &'a Path, slug: &'a str, known: Option<&'a Path>, _pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<Workplace, LaunchError>> {
        answer(async move {
            if let Some(known) = known.filter(|known| known.join(".git").is_file()) {
                let inside = linked_top(cwd).or_else(|| main_top(cwd))
                    .and_then(|top| canonical(cwd).strip_prefix(top).ok().map(Path::to_path_buf))
                    .filter(|inside| !inside.as_os_str().is_empty());
                return Ok(Workplace::Reused(inside.map_or(known.to_path_buf(), |inside| known.join(inside))));
            }
            let Some(top) = main_top(cwd).filter(|_| linked_top(cwd).is_none()) else {
                return Ok(Workplace::Given(cwd.to_path_buf()));
            };
            unnamed(&top).map_err(LaunchError::Workspace)?;
            if !top.join(".git/HEAD").is_file() {
                return Err(LaunchError::Workspace(format!("git cannot read the repository at {}, so Alondra could not give the node a worktree of its own", top.display())));
            }
            let inside = canonical(cwd).strip_prefix(&top).ok().filter(|inside| !inside.as_os_str().is_empty()).map(Path::to_path_buf);
            let dir = match root {
                Some(root) => {
                    let folder = ready(root).map_err(LaunchError::Storage)?.join(top.file_name().unwrap_or_default());
                    std::fs::create_dir_all(&folder).map_err(|why| LaunchError::Storage(why.to_string()))?;
                    let record = Repository { repository: top.clone(), root: root.clone() };
                    std::fs::write(folder.join(RECORD), serde_json::to_vec(&record).unwrap()).map_err(|why| LaunchError::Storage(why.to_string()))?;
                    folder.join(slug)
                }
                None => top.join(".claude").join("worktrees").join(slug),
            };
            if dir.join(".git").is_file() {
                return Ok(Workplace::Reused(inside.map_or(dir.clone(), |inside| dir.join(inside))));
            }
            make_worktree(&top, &dir).map_err(LaunchError::Workspace)?;
            Ok(Workplace::Made(inside.map_or(dir.clone(), |inside| dir.join(inside))))
        })
    }

    fn reopen<'a>(&'a self, cwd: &'a Path, slug: &'a str, original: &'a Path, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<(Workplace, bool), LaunchError>> {
        answer(async move {
            if original.is_dir() && self.recovery_directory_matches(cwd, original).await {
                let known = linked_top(original).unwrap_or_else(|| original.to_path_buf());
                let workplace = self.isolate(cwd, slug, Some(&known), pr_head, root).await?;
                if canonical(workplace.dir()) == canonical(original) {
                    return Ok((workplace, true));
                }
            }
            if let Some((repository, worktree, record)) = self.lost(original, slug) {
                let name = worktree.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if self.state().branches.contains(&(canonical(&repository), name)) {
                    if let Some(record) = &record {
                        ready(&record.root).map_err(LaunchError::Storage)?;
                    }
                    make_worktree(&repository, &worktree).map_err(LaunchError::Workspace)?;
                    return Ok((Workplace::Made(original.to_path_buf()), true));
                }
            }
            self.isolate(cwd, slug, None, pr_head, root).await.map(|workplace| (workplace, false))
        })
    }

    fn storage_for<'a>(&'a self, cwd: &'a Path, _slug: &'a str, _known: Option<&'a Path>, _pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<PathBuf, LaunchError>> {
        answer(async move {
            if let Some(top) = main_top(cwd) {
                unnamed(&top).map_err(LaunchError::Workspace)?;
            }
            match (root, main_top(cwd)) {
                (Some(root), Some(_)) => ready(root).map_err(LaunchError::Storage),
                (_, Some(top)) => Ok(top),
                (_, None) => Ok(cwd.to_path_buf()),
            }
        })
    }

    fn recovery_directory_matches<'a>(&'a self, cwd: &'a Path, original: &'a Path) -> Git<'a, bool> {
        answer(async move {
            if !original.is_dir() || (main_top(original).is_some() && linked_top(original).is_none()) {
                return false;
            }
            if canonical(cwd) == canonical(original) {
                return true;
            }
            let Some(original_top) = linked_top(original) else { return false };
            let Some(top) = main_top(cwd).filter(|_| linked_top(cwd).is_none()) else { return false };
            repository_of(&original_top).as_deref() == Some(top.as_path())
                && canonical(cwd).strip_prefix(&top).ok() == canonical(original).strip_prefix(&original_top).ok()
        })
    }

    fn chosen(&self, workplace: &Workplace) -> Option<PathBuf> {
        match workplace {
            Workplace::Given(_) => None,
            Workplace::Reused(dir) | Workplace::Made(dir) => Some(dir.ancestors()
                .find(|path| path.join(".git").exists())
                .filter(|path| self.stored(path).is_some())
                .unwrap_or(dir).to_path_buf()),
        }
    }

    fn stored(&self, worktree: &Path) -> Option<Repository> {
        let folder = worktree.parent()?;
        let record: Repository = serde_json::from_slice(&std::fs::read(folder.join(RECORD)).ok()?).ok()?;
        (folder.parent()? == record.root.path).then_some(record)
    }

    fn restore(&self, worktree: &Path, record: &Repository) -> Result<(), String> {
        ready(&record.root)?;
        let folder = worktree.parent().ok_or("A worktree folder has a parent")?;
        std::fs::create_dir_all(folder).map_err(|why| why.to_string())?;
        std::fs::write(folder.join(RECORD), serde_json::to_vec(record).map_err(|why| why.to_string())?).map_err(|why| why.to_string())
    }

    fn capture_root(&self, path: &Path) -> Result<WorktreesRoot, String> {
        let path = path.canonicalize().map_err(|why| format!("Choose an existing worktree folder: {why}"))?;
        if !path.is_dir() || path.parent().is_none() {
            return Err("Choose a worktree folder below the volume root".into());
        }
        Ok(WorktreesRoot { path, volume_id: "test-volume".into() })
    }

    fn final_refusal<'a>(&'a self, state: &'a DomainState, session_id: &'a str, task: &'a TaskId, _targets: &'a [DeliveryTarget], limit: Duration) -> Git<'a, Option<DomainError>> {
        answer(async move {
            let dir = self.node_worktree(state, session_id, task)?;
            let top = linked_top(&dir).or_else(|| main_top(&dir))?;
            within(limit, &top, "tell whether everything is committed and pushed", || {
                let left = self.uncommitted(&top);
                (!left.is_empty()).then(|| DomainError::new("worktree_not_clean", format!(
                    "Nothing was presented yet: {} has files not committed: {}. Commit and push them, then present the result again.",
                    top.display(), left.join(", "))))
            }).await
        })
    }

    fn evidence_refusal<'a>(&'a self, state: &'a DomainState, session_id: &'a str, task: &'a TaskId, checklist: &'a [ChecklistMark], _limit: Duration) -> Git<'a, Option<DomainError>> {
        answer(async move {
            let dir = self.node_worktree(state, session_id, task)?;
            let top = linked_top(&dir).or_else(|| main_top(&dir))?;
            let known = self.state().commits.get(&top).cloned().unwrap_or_default();
            for mark in checklist {
                for evidence in mark.evidence.iter().filter(|evidence| evidence.kind == EvidenceKind::Commit) {
                    let sha = evidence.value.trim();
                    if !known.iter().any(|commit| !sha.is_empty() && commit.starts_with(sha)) {
                        let mut error = DomainError::new("evidence_commit_not_in_work", format!(
                            "checklist: commit {sha} (point {}) is not in your work: cite a commit of your branch, pushed, or commit and push the work it stands for",
                            mark.item));
                        error.subject = Some("checklist".into());
                        return Some(error);
                    }
                }
            }
            None
        })
    }

    fn secret_refusal(&self, state: &DomainState, session_id: &str, task: &TaskId, destination: &SecretDestination) -> Option<DomainError> {
        let problem = self.checkouts(state, session_id, task)
            .and_then(|roots| roots.iter().find_map(|root| ignored_problem(destination, root)).map_or(Ok(()), Err))
            .err()?;
        let mut refusal = DomainError::new("invalid_input", problem);
        refusal.subject = Some("destination".into());
        Some(refusal)
    }

    fn write_secret(&self, state: &DomainState, interaction: &Interaction, value: &str) -> Result<String, String> {
        let destination = interaction.card.secret_destination.as_ref().ok_or("This card does not take a secret")?;
        let (Author::Agent { agent_id }, Scope::Task { task_id }) = (&interaction.author, &interaction.scope) else {
            return Err("Only a task's node asks for a secret".into());
        };
        let roots = self.checkouts(state, &agent_id.0, task_id)?;
        if let Some(problem) = roots.iter().find_map(|root| ignored_problem(destination, root)) {
            return Err(problem);
        }
        let value = value.trim();
        if value.is_empty() {
            return Err("The secret is empty".into());
        }
        for root in &roots {
            let path = root.join(&destination.file);
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let prefix = format!("{}=", destination.variable);
            let mut lines: Vec<String> = existing.lines().filter(|line| !line.starts_with(&prefix)).map(str::to_owned).collect();
            lines.push(format!("{prefix}{value}"));
            std::fs::write(&path, lines.join("\n") + "\n").map_err(|why| why.to_string())?;
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).map_err(|why| why.to_string())?;
        }
        Ok(destination.written())
    }
}

/// Why `destination` cannot take a secret in the checkout at `root`: a file neither `.gitignore`
/// there nor the repository's `.git/info/exclude` names, as `git check-ignore` would say.
fn ignored_problem(destination: &SecretDestination, root: &Path) -> Option<String> {
    if let Some(problem) = destination.shape_problem() {
        return Some(problem.into());
    }
    let exclude = repository_of(root).map(|main| main.join(".git/info/exclude"));
    let ignore = [Some(root.join(".gitignore")), exclude].into_iter().flatten()
        .map(|file| std::fs::read_to_string(file).unwrap_or_default()).collect::<Vec<_>>().join("\n");
    if ignore.lines().map(str::trim).any(|line| line == destination.file || line.trim_start_matches('/') == destination.file) {
        return None;
    }
    Some(format!("git does not ignore {} in this repository: add it to .gitignore, or choose an ignored file, so the secret is never committed", destination.file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_double_keeps_the_contract_of_the_macs_git() {
        let double = Worktrees::default();
        crate::contract::worktrees(&double, &repository);
        crate::contract::final_checks(&double, &repository, &slow);
        crate::contract::saved_work(&double, &crate::contract::Saving {
            task_worktree: &testing::task_worktree, local_commit: &|top| { local_commit(top); }, push: &push, branch_exists: &branch_exists,
        });
        crate::contract::finished_branches(&double, &crate::contract::Finishing {
            remoteless_worktree: &testing::remoteless_task_worktree, local_commit: &|top| { local_commit(top); }, branch_exists: &branch_exists,
        });
    }
}
