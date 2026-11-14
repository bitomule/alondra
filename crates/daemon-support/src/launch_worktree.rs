//! A node never works in a main checkout: a launch whose folder is one starts in the task's
//! worktree instead, the one an earlier node left or a new one in the configured storage folder
//! (by default `.claude/worktrees/`). Alondra does not leave isolation to the model.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

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

    /// The worktree Alondra chose for the node, which it may delete with it.
    pub fn chosen(&self) -> Option<&Path> {
        match self {
            Workplace::Given(_) => None,
            Workplace::Reused(dir) | Workplace::Made(dir) => Some(dir.ancestors()
                .find(|path| path.join(".git").exists())
                .filter(|path| crate::worktrees_root::stored(path).is_some())
                .unwrap_or(dir)),
        }
    }
}

/// Where a node launched in `cwd` works. In a main checkout, or a folder inside one, that is a
/// linked worktree of its repository: the task's `known` one, the one with the task's pull
/// request branch, `.claude/worktrees/<slug>`, or that folder made new. Never a main checkout.
pub async fn isolate(cwd: &Path, slug: &str, known: Option<&Path>, pr_head: Option<&str>) -> Result<Workplace, String> {
    isolate_in(cwd, slug, known, pr_head, None).await.map_err(|why| why.to_string())
}

pub async fn isolate_in(cwd: &Path, slug: &str, known: Option<&Path>, pr_head: Option<&str>, root: Option<&alondra_core::options::WorktreesRoot>) -> Result<Workplace, LaunchError> {
    let workplace = choose(cwd, slug, known, pr_head, root).await?;
    if main_checkout(workplace.dir()) {
        return Err(LaunchError::Workspace(format!(
            "{} is the main checkout of a repository, and no node works there: Alondra could not give it a worktree of its own",
            workplace.dir().display())));
    }
    Ok(workplace)
}

/// The volume a launch will write, before creation; existing worktrees retain their location.
pub async fn storage_for(cwd: &Path, slug: &str, known: Option<&Path>, pr_head: Option<&str>, root: Option<&alondra_core::options::WorktreesRoot>) -> Result<PathBuf, LaunchError> {
    if let Some(path) = known.filter(|path| !path.exists())
        && let Some(storage) = crate::worktrees_root::stored(path) {
        return crate::worktrees_root::ready(&storage.root).await.map_err(LaunchError::Storage);
    }
    if !cwd.exists()
        && let Some(storage) = cwd.ancestors().find_map(crate::worktrees_root::stored) {
        return crate::worktrees_root::ready(&storage.root).await.map_err(LaunchError::Storage);
    }
    let (Some(top), Some(git_dir), Some(common)) = (
        git(cwd, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await,
        git(cwd, &["rev-parse", "--path-format=absolute", "--git-dir"]).await,
        git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await,
    ) else { return Ok(cwd.to_path_buf()); };
    if !same_dir(Path::new(&git_dir), Path::new(&common)) {
        for ancestor in cwd.ancestors() {
            crate::worktrees_root::ready_recorded(ancestor).await.map_err(LaunchError::Storage)?;
        }
        return Ok(cwd.to_path_buf());
    }
    let top = PathBuf::from(top);
    let linked = linked_worktrees(&top).await?;
    if let Some(dir) = reusable(&top, slug, known, pr_head, &linked) {
        crate::worktrees_root::ready_recorded(&dir).await.map_err(LaunchError::Storage)?;
        return Ok(dir);
    }
    match root {
        Some(root) => crate::worktrees_root::ready(root).await.map_err(LaunchError::Storage),
        None => Ok(top),
    }
}

/// The folder is the top of a main checkout, whose `.git` is a directory; in a linked worktree it
/// is a file.
pub fn main_checkout(dir: &Path) -> bool {
    dir.join(".git").is_dir()
}

pub async fn recovery_directory_matches(cwd: &Path, original: &Path) -> bool {
    if !original.is_dir() { return false; }
    let original_git = git(original, &["rev-parse", "--path-format=absolute", "--git-dir"]).await;
    let original_common = git(original, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await;
    if let (Some(git), Some(common)) = (&original_git, &original_common)
        && same_dir(Path::new(git), Path::new(common)) { return false; }
    if same_dir(cwd, original) { return true; }
    let (Some(original_common), Some(original_top), Some(top), Some(candidate_git), Some(candidate_common)) = (
        original_common,
        git(original, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await,
        git(cwd, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await,
        git(cwd, &["rev-parse", "--path-format=absolute", "--git-dir"]).await,
        git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await,
    ) else { return false; };
    same_dir(Path::new(&candidate_git), Path::new(&candidate_common))
        && same_dir(Path::new(&original_common), Path::new(&candidate_common))
        && relative(cwd, Path::new(&top)) == relative(original, Path::new(&original_top))
}

/// A reopened task recovers only a demonstrated original folder. An unusable native folder
/// starts a fresh session in a separate worktree, leaving the old path untouched.
pub async fn reopen(cwd: &Path, slug: &str, original: &Path, pr_head: Option<&str>) -> Result<(Workplace, bool), String> {
    reopen_in(cwd, slug, original, pr_head, None).await.map_err(|why| why.to_string())
}

pub async fn reopen_in(cwd: &Path, slug: &str, original: &Path, pr_head: Option<&str>, root: Option<&alondra_core::options::WorktreesRoot>) -> Result<(Workplace, bool), LaunchError> {
    let location = reopen_location(cwd, slug, original).await;
    if normal_absolute(original) && without_links(original) && original.is_dir() {
        let direct = recovery_directory_matches(cwd, original).await;
        let managed = if let Some(location) = &location {
            recovery_directory_matches(&location.top, &location.worktree).await
        } else { false };
        if direct || managed {
            let launch_cwd = if direct { cwd } else { original }.canonicalize()
                .map_err(|why| format!("Could not resolve the launch folder {}: {why}", cwd.display()))?;
            let original_top = git(original, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await;
            let known = original_top.as_deref().map(Path::new).unwrap_or(original);
            let workplace = isolate_in(&launch_cwd, slug, Some(known), pr_head, root).await?;
            if same_dir(workplace.dir(), original) {
                return Ok((workplace, true));
            }
        }
    }
    if let Some(ReopenLocation { top, worktree, inside, storage }) = &location {
        let task_branch = format!("worktree-{}", worktree.file_name().and_then(|name| name.to_str()).expect("managed task folder"));
        let branch = if git(top, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{task_branch}")]).await.is_some() {
            Some(task_branch.as_str())
        } else if let Some(head) = pr_head
            && git(top, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{head}")]).await.is_some() {
            Some(head)
        } else { None };
        if std::fs::symlink_metadata(worktree).is_err() && without_links(worktree)
            && let Some(branch) = branch {
            if let Some(record) = storage { crate::worktrees_root::ready(&record.root).await.map_err(LaunchError::Storage)?; }
            forget_missing(top, worktree).await;
            let held = match storage {
                Some(record) => Some(crate::worktrees_root::hold(worktree, record).await.map_err(LaunchError::Storage)?),
                None => None,
            };
            let destination = if held.is_some() { worktree.file_name().and_then(|name| name.to_str()) } else { worktree.to_str() };
            if let Some(path) = destination {
                match add_worktree(top, &["worktree", "add", "--quiet", path, branch], held.as_ref()).await {
                    Err(error @ LaunchError::Storage(_)) => return Err(error),
                    Err(LaunchError::Workspace(_)) => {},
                    Ok(()) => {
                        include_ignored(top, worktree).await;
                        let restored = within(worktree.clone(), inside);
                        if restored.is_dir() && without_links(&restored)
                            && recovery_directory_matches(top, worktree).await {
                            return Ok((Workplace::Made(restored), true));
                        }
                    },
                }
            }
        }
    }
    let base = location.as_ref().map(|location| location.top.clone()).unwrap_or_else(|| cwd.to_path_buf());
    if !normal_absolute(&base) || !without_links(&base) {
        return Err(format!("The launch folder {} contains a symbolic link or is not an absolute directory", base.display()).into());
    }
    if !base.is_dir() {
        return Err(format!("The launch folder {} no longer exists", base.display()).into());
    }
    if let Some(top) = git(&base, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await
        && main_checkout(Path::new(&top)) && !without_links(&Path::new(&top).join(".claude/worktrees")) {
        return Err(LaunchError::Workspace("The managed worktree folder contains a symbolic link".into()));
    }
    isolate_in(&base, &format!("{slug}-reopened-{}", crate::auth::random_hex(4)), None, None, root).await.map(|workplace| (workplace, false))
}

struct ReopenLocation {
    top: PathBuf,
    worktree: PathBuf,
    inside: PathBuf,
    storage: Option<crate::worktrees_root::Repository>,
}

fn managed_reopen_location(path: &Path, slug: &str) -> Option<ReopenLocation> {
    if slug.is_empty() || slug != self::slug(slug) || !normal_absolute(path) { return None; }
    for root in path.ancestors() {
        let managed = root.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
            name == slug || name.strip_prefix(&format!("{slug}-reopened-")).is_some_and(|suffix|
                suffix.len() == 8 && suffix.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        });
        if !managed
            || root.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new("worktrees"))
            || root.parent().and_then(Path::parent).and_then(Path::file_name) != Some(std::ffi::OsStr::new(".claude")) { continue; }
        let top = root.parent()?.parent()?.parent()?;
        return Some(ReopenLocation { top: top.to_path_buf(), worktree: root.to_path_buf(), inside: path.strip_prefix(root).ok()?.to_path_buf(), storage: None });
    }
    None
}

async fn reopen_location(cwd: &Path, slug: &str, original: &Path) -> Option<ReopenLocation> {
    let location = managed_reopen_location(original, slug).or_else(|| {
        original.ancestors().find_map(|worktree| {
            let name = worktree.file_name()?.to_str()?;
            if name != slug && !name.strip_prefix(&format!("{slug}-reopened-")).is_some_and(|suffix|
                suffix.len() == 8 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())) { return None; }
            let storage = crate::worktrees_root::stored(worktree)?;
            Some(ReopenLocation { top: storage.repository.clone(), worktree: worktree.to_path_buf(), inside: original.strip_prefix(worktree).ok()?.to_path_buf(), storage: Some(storage) })
        })
    })?;
    if !without_links(&location.top) || !main_checkout(&location.top) { return None; }
    let actual = git(&location.top, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await?;
    if !same_dir(Path::new(&actual), &location.top) { return None; }
    if cwd.starts_with(&location.worktree) && normal_absolute(cwd) {
        return Some(location);
    }
    if let Some(plan) = managed_reopen_location(cwd, slug)
        && same_dir(&plan.top, &location.top) {
        return Some(location);
    }
    if let Some(top) = git(cwd, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await {
        let top = PathBuf::from(top);
        if !without_links(&top) { return None; }
        let (Some(git_dir), Some(common)) = (
            git(cwd, &["rev-parse", "--path-format=absolute", "--git-dir"]).await,
            git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await,
        ) else { return None; };
        if !same_dir(Path::new(&git_dir), Path::new(&common)) { return None; }
        return same_dir(&top, &location.top).then_some(location);
    }
    None
}

fn normal_absolute(path: &Path) -> bool {
    path.is_absolute() && path.components().all(|part| matches!(part, std::path::Component::RootDir | std::path::Component::Normal(_)))
}

fn without_links(path: &Path) -> bool {
    path.ancestors().all(|parent| match std::fs::symlink_metadata(parent) {
        Ok(meta) => !meta.file_type().is_symlink(),
        Err(why) => why.kind() == std::io::ErrorKind::NotFound,
    })
}

fn reusable(top: &Path, slug: &str, known: Option<&Path>, pr_head: Option<&str>, linked: &[(PathBuf, Option<String>)]) -> Option<PathBuf> {
    let find = |wanted: &Path| linked.iter().find(|(dir, _)| same_dir(dir, wanted)).map(|(dir, _)| dir.clone());
    known.and_then(find)
        .or_else(|| pr_head.and_then(|head| linked.iter().find(|(_, branch)| branch.as_deref() == Some(head)).map(|(dir, _)| dir.clone())))
        .or_else(|| find(&top.join(".claude/worktrees").join(slug)))
        .or_else(|| linked.iter().find(|(dir, branch)| {
            dir.file_name().is_some_and(|name| name == slug) && branch.as_deref() == Some(&format!("worktree-{slug}"))
                && crate::worktrees_root::repository(dir).is_some_and(|repository| same_dir(&repository, top))
        }).map(|(dir, _)| dir.clone()))
}

async fn choose(cwd: &Path, slug: &str, known: Option<&Path>, pr_head: Option<&str>, root: Option<&alondra_core::options::WorktreesRoot>) -> Result<Workplace, LaunchError> {
    if let Some(root) = root
        && (cwd.starts_with(&root.path) || known.is_some_and(|path| path.starts_with(&root.path))) {
        crate::worktrees_root::ready(root).await.map_err(LaunchError::Storage)?;
    }
    let given = || Ok(Workplace::Given(cwd.to_path_buf()));
    let Some(top) = git(cwd, &["rev-parse", "--path-format=absolute", "--show-toplevel"]).await else { return given() };
    let (Some(git_dir), Some(common)) = (
        git(cwd, &["rev-parse", "--path-format=absolute", "--git-dir"]).await,
        git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await,
    ) else { return given() };
    if !same_dir(Path::new(&git_dir), Path::new(&common)) {
        for ancestor in cwd.ancestors() {
            crate::worktrees_root::ready_recorded(ancestor).await.map_err(LaunchError::Storage)?;
        }
        return given();
    }
    let top = PathBuf::from(top);
    let inside = relative(cwd, &top);
    let linked = linked_worktrees(&top).await?;
    let reused = reusable(&top, slug, known, pr_head, &linked);
    if let Some(dir) = reused {
        crate::worktrees_root::ready_recorded(&dir).await.map_err(LaunchError::Storage)?;
        return Ok(Workplace::Reused(within(dir, &inside)));
    }
    if let Some(known) = known
        && let Some(storage) = crate::worktrees_root::stored(known)
        && same_dir(&storage.repository, &top) {
        crate::worktrees_root::ready(&storage.root).await.map_err(LaunchError::Storage)?;
        let dir = make_at(&top, slug, known.to_path_buf(), Some(storage)).await?;
        include_ignored(&top, &dir).await;
        return Ok(Workplace::Made(within(dir, &inside)));
    }
    let (dir, storage) = match root {
        Some(root) => {
            let storage = crate::worktrees_root::Repository { repository: top.canonicalize().map_err(|why| LaunchError::Storage(why.to_string()))?, root: root.clone() };
            let (root, repository) = (root.clone(), top.clone());
            let dir = tokio::task::spawn_blocking(move || crate::worktrees_root::directory(&root, &repository))
                .await.map_err(|why| LaunchError::Storage(why.to_string()))?.map_err(LaunchError::Storage)?.join(slug);
            (dir, Some(storage))
        },
        None => (top.join(".claude/worktrees").join(slug), None),
    };
    let dir = make_at(&top, slug, dir, storage).await?;
    include_ignored(&top, &dir).await;
    Ok(Workplace::Made(within(dir, &inside)))
}

/// Copies into a new worktree the files of the main checkout that git ignores and `.worktreeinclude`
/// lists, in `.gitignore` syntax, as Claude Code does for its own worktrees: `.env` and the like,
/// which no checkout brings. A file the worktree already has is kept, and a link is never followed
/// or copied, so nothing is written outside the worktree.
async fn include_ignored(top: &Path, worktree: &Path) {
    let list = top.join(".worktreeinclude");
    if !std::fs::symlink_metadata(&list).is_ok_and(|meta| meta.is_file()) {
        return;
    }
    let Some(list) = list.to_str() else { return };
    let Some(listed) = git_bytes(top, &["ls-files", "-z", "--others", "--ignored", "--exclude-from", list], None).await else {
        log!("git could not list the files .worktreeinclude names in {}", top.display());
        return;
    };
    if listed.is_empty() {
        return;
    }
    let ignored = git_bytes(top, &["check-ignore", "-z", "--stdin"], Some(listed)).await.unwrap_or_default();
    for path in ignored.split(|byte| *byte == 0).filter(|path| !path.is_empty()) {
        let path = Path::new(std::str::from_utf8(path).unwrap_or_default());
        if let Err(why) = copy_included(top, worktree, path) {
            log!("could not copy {} into the worktree {}: {why}", path.display(), worktree.display());
        }
    }
}

/// Copies `path`, relative to the checkout `top`, to the same place in `worktree` unless the
/// worktree has it already or a link stands anywhere on either side of the way.
fn copy_included(top: &Path, worktree: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let parts: Vec<_> = path.components().collect();
    if parts.is_empty() || !parts.iter().all(|part| matches!(part, std::path::Component::Normal(_))) {
        return Ok(());
    }
    let mut from = top.to_path_buf();
    for part in &parts {
        from.push(part);
        if std::fs::symlink_metadata(&from)?.file_type().is_symlink() {
            return Ok(());
        }
    }
    let mut to = worktree.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        to.push(part);
        match std::fs::symlink_metadata(&to) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Ok(()),
            Err(why) if why.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&to)?,
            Err(why) => return Err(why),
        }
    }
    to.push(parts[parts.len() - 1]);
    let mut source = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&from)?;
    let meta = source.metadata()?;
    if !meta.is_file() {
        return Ok(());
    }
    let mut copy = match std::fs::OpenOptions::new().write(true).create_new(true).mode(meta.permissions().mode() & 0o777).custom_flags(libc::O_NOFOLLOW).open(&to) {
        Ok(copy) => copy,
        Err(why) if why.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(why) => return Err(why),
    };
    std::io::copy(&mut source, &mut copy).map(drop)
}

/// Makes `.claude/worktrees/<slug>` on branch `worktree-<slug>`: that branch when it exists, which
/// keeps an earlier node's commits, else a new one from the remote's default branch.
async fn make_at(top: &Path, slug: &str, dir: PathBuf, storage: Option<crate::worktrees_root::Repository>) -> Result<PathBuf, LaunchError> {
    if dir.exists() {
        return Err(format!("{} exists but is not a worktree of {}, so Alondra did not start a node in it", dir.display(), top.display()).into());
    }
    if std::fs::symlink_metadata(&dir).is_ok() {
        return Err(format!("{} is a link to a folder that cannot be reached now, so Alondra did not start a node in it", dir.display()).into());
    }
    forget_missing(top, &dir).await;
    let branch = format!("worktree-{slug}");
    let exists = git(top, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]).await.is_some();
    let base = if exists { None } else { Some(fetched_default(top).await.unwrap_or_else(|| "HEAD".into())) };
    let held = match storage {
        Some(record) => Some(crate::worktrees_root::hold(&dir, &record).await.map_err(LaunchError::Storage)?),
        None => None,
    };
    let path = if held.is_some() { dir.file_name().and_then(|name| name.to_str()) } else { dir.to_str() }
        .ok_or_else(|| LaunchError::Workspace("the worktree path is not valid UTF-8".into()))?;
    let added = if let Some(base) = base {
        add_worktree(top, &["worktree", "add", "--quiet", "--no-track", "-b", &branch, path, &base], held.as_ref()).await
    } else {
        add_worktree(top, &["worktree", "add", "--quiet", path, &branch], held.as_ref()).await
    };
    match added {
        Err(LaunchError::Workspace(why)) => return Err(LaunchError::Workspace(format!("Alondra could not make the worktree {}: {why}", dir.display()))),
        Err(error) => return Err(error),
        Ok(()) => {},
    }
    Ok(dir)
}

fn worktree_command(top: &Path, args: &[&str], folder: &crate::worktrees_root::Folder) -> Command {
    let mut command = Command::new("git");
    command.current_dir(top).arg("--git-dir").arg(top.join(".git")).args(args).kill_on_drop(true);
    let descriptor = folder.descriptor();
    // The descriptor is held through spawn; fchdir runs before close-on-exec closes it.
    unsafe { command.pre_exec(move || {
        if libc::fchdir(descriptor) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }); }
    command
}

async fn add_worktree(top: &Path, args: &[&str], folder: Option<&crate::worktrees_root::Folder>) -> Result<(), LaunchError> {
    let Some(folder) = folder else { return run(top, args).await.map_err(LaunchError::Workspace) };
    folder.check().map_err(LaunchError::Storage)?;
    let result = worktree_command(top, args, folder).output().await.map_err(|why| LaunchError::Storage(why.to_string()))?;
    folder.check().map_err(LaunchError::Storage)?;
    if result.status.success() { Ok(()) } else { Err(LaunchError::Workspace(String::from_utf8_lossy(&result.stderr).trim().to_string())) }
}

/// Forgets git's record of a worktree that was at `dir` and is gone, so a new one can be made
/// there. The records of the repository's other worktrees stay, also those whose folder is not
/// reachable now.
async fn forget_missing(top: &Path, dir: &Path) {
    let Some(common) = git(top, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await else { return };
    if let Err(why) = crate::worktree::forget_missing(Path::new(&common), &[dir]) {
        log!("could not delete git's record of the worktree {}: {why}", dir.display());
    }
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
pub fn slug(name: &str) -> String {
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

async fn git_bytes(dir: &Path, args: &[&str], input: Option<Vec<u8>>) -> Option<Vec<u8>> {
    use tokio::io::AsyncWriteExt;
    let mut child = Command::new("git").arg("-C").arg(dir).args(args)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null())
        .kill_on_drop(true).spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let feed = async move {
        if let Some(input) = input {
            let _ = stdin.write_all(&input).await;
        }
    };
    let (_, out) = tokio::join!(feed, child.wait_with_output());
    let out = out.ok()?;
    // `git check-ignore` exits 1 when none of the paths is ignored.
    matches!(out.status.code(), Some(0 | 1)).then_some(out.stdout)
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
    fn repository(_tag: &str) -> (crate::test_support::Directory, PathBuf) {
        let root = crate::test_support::Directory::new();
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

    fn configured_root(root: &Path) -> alondra_core::options::WorktreesRoot {
        let path = root.join("storage");
        std::fs::create_dir_all(&path).unwrap();
        crate::worktrees_root::capture(&path).unwrap()
    }

    #[tokio::test]
    async fn worktrees_root_disappearing_during_fetch_is_not_recreated_by_git() {
        use std::os::unix::fs::PermissionsExt;
        let (fixture, repo) = repository("fetch-storage");
        let root = configured_root(&fixture);
        let hook = fixture.join("upload-pack");
        let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
        std::fs::write(&hook, format!("#!/bin/sh\n/bin/mv {} {}\nexec git-upload-pack \"$@\"\n", quote(&root.path), quote(&fixture.join("unmounted")))).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
        sh(&repo, &["config", "remote.origin.uploadpack", hook.to_str().unwrap()]);
        let result = isolate_in(&repo, "t-fetch", None, None, Some(&root)).await;
        assert!(!root.path.exists(), "Git must never recreate the missing storage root");
        assert!(matches!(result, Err(LaunchError::Storage(_))), "{result:?}");
        assert!(fixture.join("unmounted/repo/.alondra-repository.json").is_file());
    }

    #[tokio::test]
    async fn worktrees_root_git_writes_relative_to_the_held_folder_even_if_its_path_changes_at_spawn() {
        let (fixture, repo) = repository("held-storage");
        let root = configured_root(&fixture);
        let namespace = crate::worktrees_root::directory(&root, &repo).unwrap();
        let path = namespace.join("t-held");
        let record = crate::worktrees_root::stored(&path).unwrap();
        let folder = crate::worktrees_root::hold(&path, &record).await.unwrap();
        let mut command = worktree_command(&repo, &["worktree", "add", "--quiet", "--detach", "t-held", "HEAD"], &folder);
        let from = std::ffi::CString::new(root.path.to_str().unwrap()).unwrap();
        let to = std::ffi::CString::new(fixture.join("unmounted").to_str().unwrap()).unwrap();
        unsafe { command.pre_exec(move || {
            if libc::rename(from.as_ptr(), to.as_ptr()) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
        }); }
        let result = command.output().await.unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        assert!(!root.path.exists(), "Git never creates a replacement root");
        assert!(fixture.join("unmounted/repo/t-held/.git").is_file());
        assert!(folder.check().is_err(), "the launch notices that its saved path changed");
    }

    #[tokio::test]
    async fn worktrees_root_creation_keeps_its_record_if_the_namespace_disappears_before_fetch() {
        let (fixture, repo) = repository("recorded-creation");
        let root = configured_root(&fixture);
        let namespace = crate::worktrees_root::directory(&root, &repo).unwrap();
        let path = namespace.join("t-recorded");
        let record = crate::worktrees_root::stored(&path).unwrap();
        std::fs::rename(&root.path, fixture.join("unmounted")).unwrap();
        let result = make_at(&repo, "t-recorded", path, Some(record)).await;
        assert!(matches!(result, Err(LaunchError::Storage(_))), "{result:?}");
        assert!(!root.path.exists());
    }

    #[tokio::test]
    async fn worktrees_root_unavailable_new_setting_does_not_hold_existing_legacy_tasks() {
        let (fixture, repo) = repository("existing-storage");
        let original = isolate(&repo, "t-existing", None, None).await.unwrap();
        let missing = alondra_core::options::WorktreesRoot { path: fixture.join("missing"), volume_id: "00000000-0000-0000-0000-000000000001".into() };
        let reused = isolate_in(&repo, "t-existing", Some(original.dir()), None, Some(&missing)).await.unwrap();
        assert_eq!(reused, Workplace::Reused(original.dir().to_path_buf()));
        assert!(!missing.path.exists());
    }

    #[tokio::test]
    async fn worktrees_root_does_not_claim_an_enclosing_worktree_for_a_nested_repository() {
        let (root, repo) = repository("nested");
        let storage = configured_root(&root);
        let outer = isolate_in(&repo, "outer", None, None, Some(&storage)).await.unwrap();
        let inner = outer.dir().join("inner");
        std::fs::create_dir(&inner).unwrap();
        sh(&inner, &["init", "-q", "-b", "main"]);
        sh(&inner, &["config", "user.email", "test@example.invalid"]);
        sh(&inner, &["config", "user.name", "Test"]);
        sh(&inner, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let task = isolate(&inner, "inner", None, None).await.unwrap();
        assert_eq!(task.dir(), inner.join(".claude/worktrees/inner"));
        assert_eq!(task.chosen(), Some(task.dir()), "the first Git boundary belongs to the nested repository");
    }

    #[tokio::test]
    async fn worktrees_root_creates_new_tasks_there_reuses_old_ones_and_recovers_the_original_folder() {
        let (root, repo) = repository("configured");
        let storage = configured_root(&root);
        let legacy = isolate(&repo, "t-old", None, None).await.unwrap();
        assert_eq!(legacy.dir(), repo.join(".claude/worktrees/t-old"));
        assert!(matches!(isolate_in(&repo, "t-old", Some(legacy.dir()), None, Some(&storage)).await.unwrap(), Workplace::Reused(_)));
        let external = isolate_in(&repo, "t-new", None, None, Some(&storage)).await.unwrap();
        let path = storage.path.join("repo/t-new");
        assert_eq!(external, Workplace::Made(path.clone()));
        assert!(!repo.join(".claude/worktrees/t-new").exists());
        assert_eq!(storage_for(&repo, "t-next", None, None, Some(&storage)).await.unwrap(), storage.path);
        assert_eq!(storage_for(&repo, "t-new", Some(&path), None, None).await.unwrap(), path);
        assert_eq!(isolate_in(&repo, "t-new", Some(&path), None, None).await.unwrap(), Workplace::Reused(path.clone()));
        assert_eq!(alondra_core::options::project_of(&root, &path).as_deref(), Some("repo"));
        assert_eq!(crate::worktree::subagent_worktree(&path, "a1234567"), Some(repo.join(".claude/worktrees/agent-a1234567")));
        let (reopened, recovered) = reopen_in(&repo, "t-new", &path, None, None).await.unwrap();
        assert!(recovered);
        assert_eq!(reopened.dir(), path);
        sh(&repo, &["worktree", "remove", path.to_str().unwrap()]);
        let (restored, recovered) = reopen_in(&repo, "t-new", &path, None, None).await.unwrap();
        assert!(recovered);
        assert_eq!(restored.dir(), path);
    }

    #[tokio::test]
    async fn worktrees_root_separates_equal_repository_names_and_keeps_their_paths_stable() {
        let (root, repo) = repository("first");
        let (_second, other) = repository("second");
        let storage = configured_root(&root);
        let first = isolate_in(&repo, "t-same", None, None, Some(&storage)).await.unwrap();
        let second = isolate_in(&other, "t-same", None, None, Some(&storage)).await.unwrap();
        assert_ne!(first.dir(), second.dir());
        let name = second.dir().parent().unwrap().file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("repo-") && name.len() == 13);
        assert_eq!(isolate_in(&other, "t-same", None, None, Some(&storage)).await.unwrap().dir(), second.dir());
        assert_eq!(crate::worktrees_root::directory(&storage, &repo).unwrap(), first.dir().parent().unwrap());
    }

    #[tokio::test]
    async fn worktrees_root_cleanup_cancel_and_interrupted_deletion_remove_only_the_owned_worktree() {
        use crate::worktree::{release_task_worktree, discard_worktree, TaskWorktree};
        let (root, repo) = repository("cleanup");
        let storage = configured_root(&root);
        for (slug, interrupted, cancel) in [("t-close", false, false), ("t-trash", true, false), ("t-cancel", false, true)] {
            let path = isolate_in(&repo, slug, None, None, Some(&storage)).await.unwrap().dir().to_path_buf();
            let admin = sh(&path, &["rev-parse", "--absolute-git-dir"]);
            sh(&path, &["push", "-q", "origin", &format!("worktree-{slug}")]);
            let trash = path.parent().unwrap().join(format!(".{slug}.removing"));
            if interrupted { std::fs::rename(&path, &trash).unwrap(); }
            let result = if cancel { discard_worktree(&path, false).await } else { release_task_worktree(&path, slug, true).await };
            assert!(matches!(result, Ok(TaskWorktree::Removed { .. })), "{result:?}");
            assert!(!path.exists() && !trash.exists() && !Path::new(&admin).exists());
            assert!(storage.path.is_dir() && repo.join(".git").is_dir());
        }
    }

    #[tokio::test]
    async fn worktrees_root_does_not_claim_user_worktrees_without_the_setting() {
        let (root, repo) = repository("user-worktree");
        let users = root.join("user/t-user");
        sh(&repo, &["worktree", "add", "-q", "-b", "worktree-t-user", users.to_str().unwrap()]);
        assert!(isolate(&repo, "t-user", None, None).await.is_err());
        assert!(users.join(".git").is_file());
        assert!(!repo.join(".claude/worktrees/t-user/.git").exists());
        assert_eq!(crate::worktree::release_recorded_worktree(&root.join("missing/t-user"), true).await, Ok(crate::worktree::TaskWorktree::Absent));
    }

    #[test]
    fn worktrees_root_admission_selects_the_same_existing_worktree_as_the_launcher() {
        let top = Path::new("/repo");
        let legacy = top.join(".claude/worktrees/t");
        let external = PathBuf::from("/storage/repo/t");
        let linked = [(legacy.clone(), Some("other".into())), (external.clone(), Some("worktree-t".into()))];
        assert_eq!(reusable(top, "t", Some(&external), None, &linked), Some(external));
        assert_eq!(reusable(top, "t", None, None, &linked), Some(legacy));
    }

    #[tokio::test]
    async fn worktrees_root_project_and_subagents_follow_git_even_after_a_manual_worktree_move() {
        let (root, repo) = repository("git-identity");
        let original = isolate(&repo, "t-moved", None, None).await.unwrap().dir().to_path_buf();
        let destination = root.join("separate-volume/tasks/t-moved");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        sh(&repo, &["worktree", "move", original.to_str().unwrap(), destination.to_str().unwrap()]);
        assert_eq!(alondra_core::options::project_of(&root, &destination).as_deref(), Some("repo"));
        assert_eq!(crate::worktree::subagent_worktree(&destination, "a1234567"), Some(repo.join(".claude/worktrees/agent-a1234567")));
    }

    #[tokio::test]
    async fn worktrees_root_creation_and_reconstruction_keep_volume_errors_typed() {
        let (fixture, repo) = repository("typed-storage");
        let root = configured_root(&fixture);
        let worktree = isolate_in(&repo, "t-retained", None, None, Some(&root)).await.unwrap();
        let path = worktree.dir().to_path_buf();
        let record_path = path.parent().unwrap().join(".alondra-repository.json");
        let mut record: crate::worktrees_root::Repository = serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
        record.root.volume_id = "00000000-0000-0000-0000-000000000001".into();
        std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let error = reopen_in(&repo, "t-retained", &path, None, None).await.unwrap_err();
        assert!(matches!(error, LaunchError::Storage(_)), "{error}");
        let error = reopen_in(&path, "t-retained", &path, None, None).await.unwrap_err();
        assert!(matches!(error, LaunchError::Storage(_)), "{error}");
        sh(&repo, &["worktree", "remove", path.to_str().unwrap()]);
        let error = reopen_in(&repo, "t-retained", &path, None, None).await.unwrap_err();
        assert!(matches!(error, LaunchError::Storage(_)), "{error}");
        assert!(!path.exists(), "an incorrect volume never receives the reconstructed worktree");
        let error = isolate_in(&repo, "t-next", None, None, Some(&record.root)).await.unwrap_err();
        assert!(matches!(error, LaunchError::Storage(_)), "{error}");
        assert!(!root.path.join("repo/t-next").exists());
    }

    #[tokio::test]
    async fn worktrees_root_reopens_from_a_deleted_native_directory() {
        let (root, repo) = repository("native-reopen");
        let storage = configured_root(&root);
        let path = isolate_in(&repo, "t-native", None, None, Some(&storage)).await.unwrap().dir().to_path_buf();
        sh(&repo, &["worktree", "remove", path.to_str().unwrap()]);
        let (restored, recovered) = reopen_in(&path, "t-native", &path, None, None).await.unwrap();
        assert!(recovered);
        assert_eq!(restored.dir(), path);
        assert!(path.join(".git").is_file());
    }

    #[tokio::test]
    async fn a_second_reopening_recovers_the_exact_fallback_directory_branch_and_subfolder() {
        for subfolder in ["", "crates"] {
            let (_root, repo) = repository("reopen-again");
            if !subfolder.is_empty() {
                std::fs::create_dir(repo.join(subfolder)).unwrap();
                publish(&repo, &[("crates/keep", "keep")]);
            }
            let standard = isolate(&repo, "t-again", None, None).await.unwrap().dir().to_path_buf();
            sh(&repo, &["worktree", "remove", standard.to_str().unwrap()]);
            sh(&repo, &["branch", "-D", "worktree-t-again"]);
            let plan = within(standard.clone(), Path::new(subfolder));
            let (first, recovered) = reopen(&plan, "t-again", &plan, None).await.unwrap();
            assert!(!recovered);
            let fallback = first.dir().to_path_buf();
            let branch = sh(&fallback, &["branch", "--show-current"]);
            sh(&fallback, &["commit", "-q", "--allow-empty", "-m", "work from the first reopening"]);
            let head = sh(&fallback, &["rev-parse", "HEAD"]);
            let original = within(fallback.clone(), Path::new(subfolder));
            let (existing, recovered) = reopen(&plan, "t-again", &original, None).await.unwrap();
            assert!(recovered, "the retained plan may name the deleted standard worktree");
            assert_eq!(existing.dir(), original);
            sh(&repo, &["worktree", "remove", fallback.to_str().unwrap()]);
            let (second, recovered) = reopen(&plan, "t-again", &original, None).await.unwrap();
            assert!(recovered);
            assert_eq!(second.dir(), original);
            assert!(original.is_dir());
            assert_eq!(sh(&original, &["branch", "--show-current"]), branch);
            assert_eq!(sh(&original, &["rev-parse", "HEAD"]), head);
            assert!(!standard.exists());
            sh(&repo, &["worktree", "remove", fallback.to_str().unwrap()]);
            let (from_main, recovered) = reopen(&repo, "t-again", &original, None).await.unwrap();
            assert!(recovered, "the same original suffix is recoverable when the plan names main");
            assert_eq!(from_main.dir(), original);
        }
    }

    #[tokio::test]
    async fn losing_a_fallback_branch_still_allows_the_next_reopening_with_a_deleted_plan_directory() {
        let (_root, repo) = repository("reopen-lost-again");
        let standard = isolate(&repo, "t-again", None, None).await.unwrap().dir().to_path_buf();
        sh(&repo, &["worktree", "remove", standard.to_str().unwrap()]);
        sh(&repo, &["branch", "-D", "worktree-t-again"]);
        let (first, recovered) = reopen(&standard, "t-again", &standard, None).await.unwrap();
        assert!(!recovered);
        let first = first.dir().to_path_buf();
        let branch = sh(&first, &["branch", "--show-current"]);
        sh(&repo, &["worktree", "remove", first.to_str().unwrap()]);
        sh(&repo, &["branch", "-D", &branch]);
        let (second, recovered) = reopen(&standard, "t-again", &first, None).await.unwrap();
        assert!(!recovered);
        assert!(matches!(second, Workplace::Made(_)));
        assert!(second.dir().join(".git").is_file());
        assert_ne!(second.dir(), first);
        assert!(!standard.exists());
        assert!(!first.exists());
    }

    #[tokio::test]
    async fn reopening_never_restores_a_matching_task_suffix_in_another_repository() {
        let (_left, left) = repository("reopen-left");
        let (_right, right) = repository("reopen-right");
        let plan = left.join(".claude/worktrees/t-again");
        let original = right.join(".claude/worktrees/t-again-reopened-deadbeef");
        sh(&right, &["branch", "worktree-t-again-reopened-deadbeef"]);
        assert!(reopen(&plan, "t-again", &original, None).await.is_err());
        assert!(!original.exists());
        assert!(!plan.exists());
        assert_eq!(sh(&right, &["worktree", "list", "--porcelain"]).lines().filter(|line| line.starts_with("worktree ")).count(), 1);
    }

    #[tokio::test]
    async fn only_the_exact_generated_suffix_is_a_recoverable_managed_task_directory() {
        let (_root, repo) = repository("reopen-suffix");
        let plan = repo.join(".claude/worktrees/t-again");
        for name in ["t-again-reopened-abcdefg", "t-again-reopened-123456789", "t-again-reopened-deadbeef-extra", "t-again-reopened-DEADBEEF", "t-again-other-deadbeef"] {
            let original = repo.join(".claude/worktrees").join(name);
            sh(&repo, &["branch", &format!("worktree-{name}")]);
            assert!(reopen(&plan, "t-again", &original, None).await.is_err(), "{name} must not prove the original managed path");
            assert!(!original.exists());
        }
    }

    #[tokio::test]
    async fn a_fallback_suffix_collision_or_link_is_left_untouched_when_reopening_again() {
        let (root, repo) = repository("reopen-suffix-occupied");
        let plan = repo.join(".claude/worktrees/t-again");
        let original = repo.join(".claude/worktrees/t-again-reopened-deadbeef");
        std::fs::create_dir_all(&original).unwrap();
        std::fs::write(original.join("sentinel"), "untouched").unwrap();
        sh(&repo, &["branch", "worktree-t-again-reopened-deadbeef"]);
        let (fresh, recovered) = reopen(&plan, "t-again", &original, None).await.unwrap();
        assert!(!recovered);
        assert!(fresh.dir().join(".git").is_file());
        assert_ne!(fresh.dir(), original);
        assert_eq!(std::fs::read_to_string(original.join("sentinel")).unwrap(), "untouched");
        let target = root.join("foreign");
        std::fs::rename(&original, &target).unwrap();
        std::os::unix::fs::symlink(&target, &original).unwrap();
        let (fresh, recovered) = reopen(&plan, "t-again", &original, None).await.unwrap();
        assert!(!recovered);
        assert!(fresh.dir().join(".git").is_file());
        assert_ne!(fresh.dir(), original);
        assert_eq!(std::fs::read_to_string(target.join("sentinel")).unwrap(), "untouched");
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
        assert!(std::fs::symlink_metadata(&original).unwrap().file_type().is_symlink());
    }

    #[tokio::test]
    async fn reopening_from_an_alias_of_main_recovers_the_canonical_original_worktree() {
        let (root, repo) = repository("reopen-main-alias");
        let original = isolate(&repo, "t-alias", None, None).await.unwrap().dir().to_path_buf();
        let alias = root.join("main-alias");
        std::os::unix::fs::symlink(&repo, &alias).unwrap();
        let (workplace, recovered) = reopen(&alias, "t-alias", &original, None).await.unwrap();
        assert!(recovered);
        assert_eq!(workplace.dir(), original);
        assert!(without_links(workplace.dir()));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn reopening_from_the_system_temporary_folder_alias_recovers_the_canonical_directory() {
        let (_root, repo) = repository("reopen-system-alias");
        let original = isolate(&repo, "t-system", None, None).await.unwrap().dir().to_path_buf();
        let alias = [("/private/tmp", "/tmp"), ("/private/var", "/var")].into_iter().find_map(|(canonical, alias)| {
            repo.strip_prefix(canonical).ok().map(|inside| Path::new(alias).join(inside))
        });
        if let Some(alias) = alias {
            let (workplace, recovered) = reopen(&alias, "t-system", &original, None).await.unwrap();
            assert!(recovered);
            assert_eq!(workplace.dir(), original);
            assert!(without_links(workplace.dir()));
        }
    }

    #[tokio::test]
    async fn reopening_an_alias_of_the_original_worktree_returns_its_canonical_directory() {
        let (root, repo) = repository("reopen-original-alias");
        let original = isolate(&repo, "t-alias", None, None).await.unwrap().dir().to_path_buf();
        let alias = root.join("worktree-alias");
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let (workplace, recovered) = reopen(&alias, "t-alias", &original, None).await.unwrap();
        assert!(recovered);
        assert_eq!(workplace.dir(), original);
        assert!(without_links(workplace.dir()));
    }

    #[tokio::test]
    async fn reopening_a_lost_subfolder_falls_back_to_a_fresh_checkout_root() {
        let (_root, repo) = repository("reopen-lost-subfolder");
        let worktree = isolate(&repo, "t-sub", None, None).await.unwrap().dir().to_path_buf();
        let original = worktree.join("only-on-task");
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("keep"), "work").unwrap();
        sh(&worktree, &["add", "."]);
        sh(&worktree, &["commit", "-q", "-m", "task folder"]);
        sh(&repo, &["worktree", "remove", worktree.to_str().unwrap()]);
        sh(&repo, &["branch", "-D", "worktree-t-sub"]);
        let (workplace, recovered) = reopen(&original, "t-sub", &original, None).await.unwrap();
        assert!(!recovered);
        assert!(workplace.dir().is_dir());
        assert!(workplace.dir().join(".git").is_file());
        assert!(!workplace.dir().join("only-on-task").exists());
        assert!(workplace.dir().starts_with(repo.join(".claude/worktrees")));
    }

    #[tokio::test]
    async fn reopening_an_existing_given_folder_does_not_take_ownership_of_it() {
        let root = crate::test_support::Directory::new();
        std::fs::create_dir_all(&root).unwrap();
        let (workplace, recovered) = reopen(&root, "t-given", &root, None).await.unwrap();
        assert!(recovered);
        assert_eq!(workplace, Workplace::Given(root.to_path_buf()));
        assert!(workplace.chosen().is_none());
    }

    #[tokio::test]
    async fn reopening_an_existing_custom_worktree_subfolder_keeps_that_exact_path() {
        let (_root, repo) = repository("reopen-custom-subfolder");
        std::fs::create_dir(repo.join("crates")).unwrap();
        publish(&repo, &[("crates/keep", "keep")]);
        let custom = repo.join(".claude/worktrees/custom");
        sh(&repo, &["worktree", "add", "-q", "-b", "custom", custom.to_str().unwrap()]);
        let original = custom.join("crates");
        let (workplace, recovered) = reopen(&repo.join("crates"), "t-other", &original, None).await.unwrap();
        assert!(recovered);
        assert_eq!(workplace.dir(), original);
        assert!(!repo.join(".claude/worktrees/t-other").exists());
    }

    #[tokio::test]
    async fn reopening_reconstructs_the_exact_original_directory_with_its_retained_commits() {
        let (_root, repo) = repository("reopen");
        let original = isolate(&repo, "t-reopen", None, None).await.unwrap().dir().to_path_buf();
        sh(&original, &["commit", "-q", "--allow-empty", "-m", "approved work"]);
        let head = sh(&original, &["rev-parse", "HEAD"]);
        sh(&repo, &["worktree", "remove", original.to_str().unwrap()]);
        let (workplace, recovered) = reopen(&repo, "t-reopen", &original, Some(&head)).await.unwrap();
        assert!(recovered);
        assert_eq!(workplace, Workplace::Made(original.clone()));
        assert_eq!(sh(&original, &["rev-parse", "HEAD"]), head);
    }

    #[tokio::test]
    async fn reopening_a_plan_that_names_its_deleted_worktree_finds_the_main_checkout() {
        let (_root, repo) = repository("reopen-plan");
        publish(&repo, &[("nested", "keep")]);
        let original = isolate(&repo, "t-plan", None, None).await.unwrap().dir().to_path_buf();
        sh(&repo, &["worktree", "remove", original.to_str().unwrap()]);
        let (workplace, recovered) = reopen(&original, "t-plan", &original, None).await.unwrap();
        assert!(recovered);
        assert_eq!(workplace.dir(), original);
    }

    #[tokio::test]
    async fn reopening_a_subfolder_reconstructs_the_same_relative_directory() {
        let (_root, repo) = repository("reopen-subfolder");
        std::fs::create_dir(repo.join("crates")).unwrap();
        publish(&repo, &[("crates/keep", "keep")]);
        let original = isolate(&repo.join("crates"), "t-sub", None, None).await.unwrap().dir().to_path_buf();
        sh(&repo, &["worktree", "remove", original.parent().unwrap().to_str().unwrap()]);
        let (workplace, recovered) = reopen(&original, "t-sub", &original, None).await.unwrap();
        assert!(recovered);
        assert_eq!(workplace.dir(), original);
    }

    #[tokio::test]
    async fn reopening_without_the_original_branch_starts_fresh_in_another_directory() {
        let (_root, repo) = repository("reopen-lost-branch");
        let original = isolate(&repo, "t-lost", None, None).await.unwrap().dir().to_path_buf();
        sh(&repo, &["worktree", "remove", original.to_str().unwrap()]);
        sh(&repo, &["branch", "-D", "worktree-t-lost"]);
        let (workplace, recovered) = reopen(&repo, "t-lost", &original, None).await.unwrap();
        assert!(!recovered);
        assert_ne!(workplace.dir(), original);
        assert!(workplace.dir().is_dir());
        assert!(!original.exists());
    }

    #[tokio::test]
    async fn reopening_never_reconstructs_an_arbitrary_directory_or_overwrites_a_collision() {
        let (root, repo) = repository("reopen-foreign");
        sh(&repo, &["branch", "worktree-t-foreign"]);
        let foreign = root.join("foreign");
        let (_, recovered) = reopen(&repo, "t-foreign", &foreign, None).await.unwrap();
        assert!(!recovered);
        assert!(!foreign.exists());
        let occupied = repo.join(".claude/worktrees/t-foreign");
        std::fs::create_dir_all(&occupied).unwrap();
        std::fs::write(occupied.join("keep"), "untouched").unwrap();
        let (workplace, recovered) = reopen(&repo, "t-foreign", &occupied, None).await.unwrap();
        assert!(!recovered);
        assert_ne!(workplace.dir(), occupied);
        assert_eq!(std::fs::read_to_string(occupied.join("keep")).unwrap(), "untouched");
        let (workplace, recovered) = reopen(&occupied, "t-foreign", &occupied, None).await.unwrap();
        assert!(!recovered);
        assert!(matches!(workplace, Workplace::Made(_)));
        assert!(workplace.dir().is_dir());
        assert_ne!(workplace.dir(), occupied);
        assert_eq!(std::fs::read_to_string(occupied.join("keep")).unwrap(), "untouched");
        assert_eq!(std::fs::read_dir(&occupied).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn reopening_never_recovers_a_symbolic_link_as_the_original_worktree() {
        let (_root, repo) = repository("reopen-link");
        let original = isolate(&repo, "t-link", None, None).await.unwrap().dir().to_path_buf();
        let other = repo.join(".claude/worktrees/other");
        sh(&repo, &["worktree", "move", original.to_str().unwrap(), other.to_str().unwrap()]);
        std::os::unix::fs::symlink(&other, &original).unwrap();
        let (workplace, recovered) = reopen(&repo, "t-link", &original, None).await.unwrap();
        assert!(!recovered);
        assert_ne!(workplace.dir(), original);
        assert!(std::fs::symlink_metadata(&original).unwrap().file_type().is_symlink());
    }

    #[tokio::test]
    async fn reopening_a_plan_naming_a_linked_worktree_alias_uses_the_verified_main_checkout() {
        let (_root, repo) = repository("reopen-plan-link");
        let original = isolate(&repo, "t-alias", None, None).await.unwrap().dir().to_path_buf();
        let other = repo.join(".claude/worktrees/other");
        sh(&repo, &["worktree", "move", original.to_str().unwrap(), other.to_str().unwrap()]);
        std::fs::write(other.join("sentinel"), "untouched").unwrap();
        let head = sh(&other, &["rev-parse", "HEAD"]);
        std::os::unix::fs::symlink(&other, &original).unwrap();
        let (workplace, recovered) = reopen(&original, "t-alias", &original, None).await.unwrap();
        assert!(!recovered);
        assert!(matches!(workplace, Workplace::Made(_)));
        assert_ne!(workplace.dir(), original);
        assert_ne!(workplace.dir(), other);
        assert_eq!(sh(&other, &["rev-parse", "HEAD"]), head);
        assert_eq!(std::fs::read_to_string(other.join("sentinel")).unwrap(), "untouched");
        assert_eq!(sh(&other, &["status", "--porcelain"]), "?? sentinel");
    }

    #[tokio::test]
    async fn reopening_a_plan_naming_a_plain_folder_alias_never_launches_in_its_target() {
        let (root, repo) = repository("reopen-plain-alias");
        let other = root.join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("sentinel"), "untouched").unwrap();
        let original = repo.join(".claude/worktrees/t-alias");
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&other, &original).unwrap();
        let (workplace, recovered) = reopen(&original, "t-alias", &original, None).await.unwrap();
        assert!(!recovered);
        assert!(matches!(workplace, Workplace::Made(_)));
        assert_ne!(workplace.dir(), original);
        assert_eq!(std::fs::read_to_string(other.join("sentinel")).unwrap(), "untouched");
        assert_eq!(std::fs::read_dir(&other).unwrap().count(), 1);
        let arbitrary_alias = root.join("alias");
        std::os::unix::fs::symlink(&other, &arbitrary_alias).unwrap();
        assert!(reopen(&arbitrary_alias, "t-alias", &arbitrary_alias, None).await.is_err());
        assert_eq!(std::fs::read_dir(&other).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn reopening_never_launches_through_an_ancestor_alias() {
        let root = crate::test_support::Directory::new();
        std::fs::create_dir_all(root.join("other/inner")).unwrap();
        std::fs::write(root.join("other/inner/sentinel"), "untouched").unwrap();
        std::os::unix::fs::symlink(root.join("other"), root.join("alias")).unwrap();
        let cwd = root.join("alias/inner");
        assert!(reopen(&cwd, "t-alias", &cwd, None).await.is_err());
        assert_eq!(std::fs::read_to_string(root.join("other/inner/sentinel")).unwrap(), "untouched");
        assert_eq!(std::fs::read_dir(root.join("other/inner")).unwrap().count(), 1);
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

    /// Making a task's worktree forgets git's record of the one gone from its folder, and only that
    /// one: a worktree whose folder is missing now, as on a volume that is not mounted, keeps its own.
    #[tokio::test]
    async fn making_a_worktree_forgets_only_the_record_of_the_one_gone_from_its_folder() {
        let (root, repo) = repository("records");
        let dir = repo.join(".claude/worktrees/t-again");
        sh(&repo, &["worktree", "add", "-q", "-b", "worktree-t-again", dir.to_str().unwrap()]);
        let away = root.join("volume/t-away");
        sh(&repo, &["worktree", "add", "-q", "-b", "worktree-t-away", away.to_str().unwrap()]);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::rename(root.join("volume"), root.join("unmounted")).unwrap();

        assert_eq!(isolate(&repo, "t-again", None, None).await.unwrap(), Workplace::Made(dir.clone()));
        assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "worktree-t-again");
        let listed = sh(&repo, &["worktree", "list", "--porcelain"]);
        assert!(listed.contains("branch refs/heads/worktree-t-away"), "the missing worktree keeps its record: {listed}");
        std::fs::rename(root.join("unmounted"), root.join("volume")).unwrap();
        assert_eq!(sh(&away, &["rev-parse", "--abbrev-ref", "HEAD"]), "worktree-t-away", "and works again once its folder is back");
    }

    #[tokio::test]
    async fn a_task_folder_that_is_a_link_to_a_folder_not_reachable_is_never_replaced() {
        let (root, repo) = repository("dangling");
        std::fs::create_dir_all(repo.join(".claude/worktrees")).unwrap();
        let link = repo.join(".claude/worktrees/t-away");
        std::os::unix::fs::symlink(root.join("unmounted/t-away"), &link).unwrap();
        let refused = isolate(&repo, "t-away", None, None).await.unwrap_err();
        assert!(refused.contains("cannot be reached now"), "{refused}");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
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
        let root = crate::test_support::Directory::new();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let refused = isolate(&root, "t", None, None).await.unwrap_err();
        assert!(refused.contains("main checkout"), "{refused}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Commits `files` and pushes them to the default branch the new worktrees start from.
    fn publish(repo: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            std::fs::write(repo.join(path), text).unwrap();
            sh(repo, &["add", "-f", path]);
        }
        sh(repo, &["commit", "-q", "-m", "files"]);
        sh(repo, &["push", "-q", "origin", "HEAD:main"]);
    }

    #[tokio::test]
    async fn a_new_worktree_brings_the_ignored_files_worktreeinclude_lists_and_no_others() {
        use std::os::unix::fs::PermissionsExt;
        let (root, repo) = repository("include");
        publish(&repo, &[(".gitignore", ".env\n.secret\nconfig/\n"), (".worktreeinclude", ".env\nconfig/*.json\nnotes.txt\n")]);
        std::fs::write(repo.join(".env"), "TOKEN=abc\n").unwrap();
        std::fs::set_permissions(repo.join(".env"), std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(repo.join(".secret"), "not listed").unwrap();
        std::fs::write(repo.join("notes.txt"), "listed but not ignored").unwrap();
        std::fs::create_dir_all(repo.join("config")).unwrap();
        std::fs::write(repo.join("config/keys.json"), "{}").unwrap();
        isolate(&repo, "t-env", None, None).await.unwrap();
        let dir = repo.join(".claude/worktrees/t-env");
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "TOKEN=abc\n");
        assert_eq!(std::fs::metadata(dir.join(".env")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(dir.join("config/keys.json")).unwrap(), "{}");
        assert!(!dir.join(".secret").exists(), "an ignored file .worktreeinclude does not list stays out");
        assert!(!dir.join("notes.txt").exists(), "a listed file git does not ignore stays out");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The main checkout is behind its remote, which tracks `.env` now: the worktree keeps the
    /// repository's file.
    #[tokio::test]
    async fn a_file_of_the_repository_is_never_overwritten() {
        let (root, repo) = repository("overwrite");
        publish(&repo, &[(".gitignore", ".env\n"), (".worktreeinclude", ".env\n")]);
        publish(&repo, &[(".env", "FROM=repo\n")]);
        sh(&repo, &["reset", "-q", "HEAD~1"]);
        std::fs::write(repo.join(".env"), "FROM=main checkout\n").unwrap();
        isolate(&repo, "t-keep", None, None).await.unwrap();
        let dir = repo.join(".claude/worktrees/t-keep");
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "FROM=repo\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_pattern_that_points_outside_the_checkout_copies_nothing() {
        let (root, repo) = repository("outside");
        publish(&repo, &[(".gitignore", "*.env\n"), (".worktreeinclude", "../outside.env\n/../outside.env\n../*\n")]);
        std::fs::write(root.join("outside.env"), "SECRET=1\n").unwrap();
        isolate(&repo, "t-out", None, None).await.unwrap();
        let dir = repo.join(".claude/worktrees/t-out");
        assert!(!dir.join("outside.env").exists());
        assert!(!repo.join(".claude/worktrees/outside.env").exists());
        assert_eq!(sh(&dir, &["status", "--porcelain", "--ignored"]), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The remote tracks `config` as a link out of the repository while the main checkout has an
    /// ignored `config/` folder: copying `config/.env` would write through the link.
    #[tokio::test]
    async fn a_link_in_the_worktree_is_never_written_through_and_a_link_is_never_copied() {
        let (root, repo) = repository("links");
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        publish(&repo, &[(".gitignore", ".env\nconfig/\nlinked.env\n"), (".worktreeinclude", ".env\nconfig/.env\nlinked.env\n")]);
        std::os::unix::fs::symlink(&elsewhere, repo.join("config")).unwrap();
        sh(&repo, &["add", "-f", "config"]);
        sh(&repo, &["commit", "-q", "-m", "link"]);
        sh(&repo, &["push", "-q", "origin", "HEAD:main"]);
        sh(&repo, &["reset", "-q", "HEAD~1"]);
        std::fs::remove_file(repo.join("config")).unwrap();
        std::fs::create_dir_all(repo.join("config")).unwrap();
        std::fs::write(repo.join("config/.env"), "SECRET=1\n").unwrap();
        std::fs::write(elsewhere.join("target.env"), "OUTSIDE=1\n").unwrap();
        std::os::unix::fs::symlink(elsewhere.join("target.env"), repo.join("linked.env")).unwrap();
        isolate(&repo, "t-links", None, None).await.unwrap();
        let dir = repo.join(".claude/worktrees/t-links");
        assert!(std::fs::symlink_metadata(dir.join("config")).unwrap().file_type().is_symlink());
        assert!(!elsewhere.join(".env").exists(), "nothing was written outside the worktree");
        assert!(std::fs::symlink_metadata(dir.join("linked.env")).is_err(), "a link is not copied");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_task_id_is_a_folder_name() {
        assert_eq!(slug("t-imp-safe-permissions"), "t-imp-safe-permissions");
        assert_eq!(slug("App/Fix Login!"), "app-fix-login");
        assert_eq!(slug("///"), "node");
    }
}
