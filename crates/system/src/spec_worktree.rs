//! The spec files of a task's own worktree, read and written with git and the disk.
use alondra_domain_model::specs::{self, Files, SpecFiles};
use alondra_domain_model::{DomainState, TaskId, WorktreeOrigin};
use alondra_engine_ports::spec_worktrees::{SpecSource, SpecWorktrees};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

pub struct GitSpecWorktrees;

/// The worktrees the task still holds, its own first, each as the folder it resolves to.
fn worktrees(state: &DomainState, task: &TaskId) -> Vec<PathBuf> {
    let Some(task) = state.tasks.get(task) else { return vec![] };
    let mut open: Vec<_> = task.open_worktrees().collect();
    open.sort_by_key(|(_, worktree)| worktree.origin != WorktreeOrigin::Task);
    open.into_iter().filter_map(|(path, _)| PathBuf::from(path).canonicalize().ok()).collect()
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git").arg("-C").arg(dir).args(args).stdin(Stdio::null()).stderr(Stdio::piped()).output()
        .map_err(|error| format!("git could not run: {error}"))?;
    if !output.status.success() {
        return Err(format!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(output.stdout)
}

fn names(bytes: Vec<u8>) -> Vec<String> {
    let mut paths: Vec<String> = String::from_utf8_lossy(&bytes).split('\0').filter(|path| !path.is_empty()).map(str::to_owned).collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The files of a working tree that git lists. A path that leaves it through a link is not read.
pub struct Checkout {
    root: PathBuf,
    paths: Vec<String>,
}

impl Checkout {
    pub fn of(root: &Path) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|error| format!("{} cannot be read: {error}", root.display()))?;
        let listed = git(&root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"])?;
        let paths = names(listed).into_iter().filter(|path| root.join(path).is_file()).collect();
        Ok(Self { root, paths })
    }
}

impl Files for Checkout {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let real = self.root.join(path).canonicalize().ok()?;
        if !real.starts_with(&self.root) {
            return None;
        }
        std::fs::read(real).ok()
    }
}

/// The files of one commit, read from git's objects.
pub struct Commit {
    root: PathBuf,
    commit: String,
    paths: Vec<String>,
    read: Mutex<BTreeMap<String, Option<Vec<u8>>>>,
}

impl Commit {
    pub fn of(root: &Path, revision: &str) -> Result<Self, String> {
        let resolved = git(root, &["rev-parse", "--verify", "--quiet", &format!("{revision}^{{commit}}")])?;
        let commit = String::from_utf8_lossy(&resolved).trim().to_owned();
        let paths = names(git(root, &["ls-tree", "-r", "-z", "--name-only", &commit])?);
        Ok(Self { root: root.to_path_buf(), commit, paths, read: Mutex::new(BTreeMap::new()) })
    }
}

impl Files for Commit {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        if !self.paths.iter().any(|known| known == path) {
            return None;
        }
        let mut read = self.read.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        read.entry(path.to_owned()).or_insert_with(|| git(&self.root, &["cat-file", "blob", &format!("{}:{path}", self.commit)]).ok()).clone()
    }
}

/// Where the branch of the worktree at `root` leaves the remote's default branch.
fn fork_point(root: &Path) -> Result<String, String> {
    let base = git(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| ["origin/main", "origin/master", "main", "master"].into_iter()
            .find(|name| git(root, &["rev-parse", "--verify", "--quiet", name]).is_ok()).map(str::to_owned))
        .ok_or("the repository has no default branch to compare with")?;
    let found = git(root, &["merge-base", "HEAD", &base])?;
    Ok(String::from_utf8_lossy(&found).trim().to_owned())
}

/// Whether `path` stays inside `root` without going through a link: every folder of it that exists
/// is a real folder, and the file itself, when it exists, is not a link.
fn safe_target(root: &Path, path: &str) -> Result<PathBuf, String> {
    if let Some(problem) = specs::path_problem(path) {
        return Err(format!("{path}: {problem}"));
    }
    let mut at = root.to_path_buf();
    let segments: Vec<&str> = path.split('/').collect();
    for (index, segment) in segments.iter().enumerate() {
        at.push(segment);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(format!("{path} goes through a link; Alondra never writes through one")),
            Ok(meta) if index + 1 < segments.len() && !meta.is_dir() => return Err(format!("{path}: {} is not a folder", at.display())),
            _ => {}
        }
    }
    Ok(at)
}

/// Writes each of `files` into `root`, the new text replacing the old one at once; `None` removes
/// it. Every path is checked before any is written.
pub fn write_into(root: &Path, files: &SpecFiles) -> Result<BTreeMap<String, String>, String> {
    let targets: Vec<(PathBuf, &String, &Option<String>)> = files.iter()
        .map(|(path, text)| safe_target(root, path).map(|target| (target, path, text)))
        .collect::<Result<_, _>>()?;
    let mut written = BTreeMap::new();
    for (target, path, text) in targets {
        match text {
            Some(text) => {
                if let Some(folder) = target.parent() {
                    std::fs::create_dir_all(folder).map_err(|error| format!("{path}: its folder cannot be made: {error}"))?;
                }
                safe_target(root, path)?;
                alondra_foundation::save::atomic(&target, text.as_bytes()).map_err(|error| format!("{path} cannot be written: {error}"))?;
            }
            None => match std::fs::remove_file(&target) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{path} cannot be removed: {error}")),
            },
        }
        written.insert(path.clone(), specs::fingerprint(text.as_deref()));
    }
    Ok(written)
}

impl SpecWorktrees for GitSpecWorktrees {
    fn own(&self, state: &DomainState, task: &TaskId) -> Option<PathBuf> {
        worktrees(state, task).into_iter().next()
    }

    fn all(&self, state: &DomainState, task: &TaskId) -> Vec<PathBuf> {
        worktrees(state, task)
    }

    fn head(&self, root: &Path) -> Result<SpecSource, String> {
        Ok(Box::new(Commit::of(root, "HEAD")?))
    }

    fn base(&self, root: &Path) -> Result<SpecSource, String> {
        Ok(Box::new(Commit::of(root, &fork_point(root)?)?))
    }

    fn settings(&self, root: &Path) -> Result<Option<Vec<u8>>, String> {
        let path = safe_target(root, specs::SETTINGS_PATH)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{} cannot be read: {error}", specs::SETTINGS_PATH)),
        }
    }

    fn write(&self, root: &Path, files: &SpecFiles) -> Result<BTreeMap<String, String>, String> {
        write_into(root, files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Directory;

    fn changes(entries: &[(&str, Option<&str>)]) -> SpecFiles {
        entries.iter().map(|(path, text)| ((*path).to_owned(), text.map(str::to_owned))).collect()
    }

    #[test]
    fn approved_files_are_written_and_removed_inside_the_worktree() {
        let dir = Directory::new();
        std::fs::create_dir_all(&*dir).unwrap();
        let root = dir.canonicalize().unwrap();
        std::fs::create_dir_all(root.join("specs/billing")).unwrap();
        std::fs::write(root.join("specs/billing/old.feature"), "old").unwrap();
        let written = write_into(&root, &changes(&[("specs/billing/new.feature", Some("new")), ("specs/billing/old.feature", None), ("docs/decisions/keep.md", Some("kept"))])).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("specs/billing/new.feature")).unwrap(), "new");
        assert!(!root.join("specs/billing/old.feature").exists());
        assert_eq!(std::fs::read_to_string(root.join("docs/decisions/keep.md")).unwrap(), "kept");
        assert_eq!(written["specs/billing/new.feature"], specs::fingerprint(Some("new")));
        assert_eq!(written["specs/billing/old.feature"], specs::fingerprint(None));
    }

    fn run(dir: &Path, args: &[&str]) {
        let output = Command::new("git").current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "user.name=Ana", "-c", "user.email=ana@example.com"]).args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }

    const SETTINGS: &str = "format = 1\n\n[specs]\nenabled = true\n";
    const BOUNDARY: &str = "id = \"billing\"\nowns = [\"crates/billing/**\"]\n";

    #[test]
    fn a_branch_that_changes_a_spec_of_its_base_without_approval_drifts_and_ignored_files_never_count() {
        let dir = Directory::new();
        std::fs::create_dir_all(&*dir).unwrap();
        let root = dir.canonicalize().unwrap();
        run(&root, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join("specs/billing")).unwrap();
        std::fs::write(root.join("alondra.toml"), SETTINGS).unwrap();
        std::fs::write(root.join("specs/billing/boundary.toml"), BOUNDARY).unwrap();
        std::fs::write(root.join(".gitignore"), "specs/billing/scratch.feature\n").unwrap();
        run(&root, &["add", "-A"]);
        run(&root, &["commit", "-q", "-m", "Start"]);
        run(&root, &["checkout", "-q", "-b", "work"]);
        std::fs::write(root.join("specs/billing/boundary.toml"), BOUNDARY.replace("billing/**", "billing/src/**")).unwrap();
        std::fs::write(root.join("specs/billing/scratch.feature"), "not a spec").unwrap();
        run(&root, &["commit", "-q", "-a", "-m", "Change the boundary"]);
        let listed = Checkout::of(&root).unwrap();
        assert!(!listed.paths().iter().any(|path| path.ends_with("scratch.feature")), "a file git ignores is never listed");
        let head = Commit::of(&root, "HEAD").unwrap();
        std::fs::write(root.join("specs/billing/boundary.toml"), BOUNDARY).unwrap();
        assert_ne!(head.read("specs/billing/boundary.toml").as_deref(), Some(BOUNDARY.as_bytes()), "what counts is the commit, never a file put back on disk");
        let base = Commit::of(&root, &fork_point(&root).unwrap()).unwrap();
        assert_eq!(base.read("specs/billing/boundary.toml").as_deref(), Some(BOUNDARY.as_bytes()), "the base is where the branch left main");
        let found = specs::drift(&head, &base, &SpecFiles::new()).unwrap();
        assert_eq!(found, [specs::SpecDrift { path: "specs/billing/boundary.toml".into(), approved: false }]);
        let approved: SpecFiles = [("specs/billing/boundary.toml".to_owned(), Some(BOUNDARY.replace("billing/**", "billing/src/**")))].into();
        assert_eq!(specs::drift(&head, &base, &approved).unwrap(), [], "the change the user approved, as approved");
    }

    #[test]
    fn nothing_is_written_through_a_link_or_outside_the_worktree() {
        let dir = Directory::new();
        let root = dir.join("tree");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("specs")).unwrap();
        let root = root.canonicalize().unwrap();
        let refused = write_into(&root, &changes(&[("docs/decisions/fine.md", Some("fine")), ("specs/billing/a.feature", Some("x"))])).unwrap_err();
        assert!(refused.contains("link"), "{refused}");
        assert!(!root.join("docs/decisions/fine.md").exists(), "a refused path stops the whole write before anything is written");
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        for path in ["../escape.feature", "/etc/escape", "specs/./a.feature"] {
            assert!(write_into(&root, &changes(&[(path, Some("x"))])).is_err(), "{path} is refused");
        }
    }
}
