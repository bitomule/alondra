//! Where a development tool writes what it drafts from this Mac's data: the `.scratch` folder of
//! the worktree it runs from, never the cases, never Bazel's own folders. A path is read from the
//! folder the tool was started in and resolved before it is accepted, so `..` and links cannot
//! take it out.

use std::path::{Component, Path, PathBuf};

pub const SCRATCH: &str = ".scratch";
/// What `bazel run` sets: the worktree, and the folder the command was typed in.
const WORKSPACE_ENV: &str = "BUILD_WORKSPACE_DIRECTORY";
const WORKING_ENV: &str = "BUILD_WORKING_DIRECTORY";

fn lexical(path: &Path) -> Option<PathBuf> {
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if !normal.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    Some(normal)
}

/// The real path `path` would have: its deepest folder that exists, with links resolved, and the
/// rest of it as written.
fn real(path: &Path) -> std::io::Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else { break };
        rest.push(name.to_owned());
        existing.pop();
    }
    let mut resolved = existing.canonicalize()?;
    resolved.extend(rest.iter().rev());
    Ok(resolved)
}

/// `out`, read from `working`, as a path inside `workspace/.scratch`, or why it is not one.
pub fn resolve(workspace: &Path, working: &Path, out: &Path) -> Result<PathBuf, String> {
    let outside = || format!("{} is not inside the {SCRATCH} folder of the worktree", out.display());
    let joined = if out.is_absolute() { out.to_path_buf() } else { working.join(out) };
    let normal = lexical(&joined).ok_or_else(outside)?;
    let scratch = real(&workspace.join(SCRATCH)).map_err(|error| error.to_string())?;
    let resolved = real(&normal).map_err(|error| error.to_string())?;
    if resolved == scratch || !resolved.starts_with(&scratch) {
        return Err(outside());
    }
    Ok(resolved)
}

/// The worktree the tool runs from, as `bazel run` says.
pub fn workspace() -> Result<PathBuf, String> {
    std::env::var_os(WORKSPACE_ENV).map(PathBuf::from).ok_or_else(|| format!("{WORKSPACE_ENV} is not set: run it with `bazel run` from the worktree it writes into"))
}

/// Where `out` goes when the tool runs with `bazel run`: inside the worktree's `.scratch`.
pub fn destination(out: &Path) -> Result<PathBuf, String> {
    let workspace = workspace()?;
    let working = std::env::var_os(WORKING_ENV).map(PathBuf::from).unwrap_or_else(|| workspace.clone());
    resolve(&workspace, &working, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_inside_the_scratch_folder_of_the_worktree_is_accepted_from_wherever_it_was_typed() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        std::fs::create_dir_all(workspace.join("crates/scenarios")).unwrap();
        let scratch = workspace.join(SCRATCH);
        assert_eq!(resolve(&workspace, &workspace, Path::new(".scratch/case/a.ndjson")), Ok(scratch.join("case/a.ndjson")));
        assert_eq!(resolve(&workspace, &workspace.join("crates/scenarios"), Path::new("../../.scratch/a")), Ok(scratch.join("a")));
        assert_eq!(resolve(&workspace, &workspace, &scratch.join("b")), Ok(scratch.join("b")));
    }

    #[test]
    fn a_path_that_leaves_the_scratch_folder_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.join(SCRATCH)).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), workspace.join(".scratch/link")).unwrap();
        for out in [".scratch/../crates/scenarios/cases/x", ".scratch", "cases/.scratch/x", "../.scratch/x", ".scratch/link/x", "/tmp/.scratch/x"] {
            assert!(resolve(&workspace, &workspace, Path::new(out)).is_err(), "{out} was accepted");
        }
    }
}
