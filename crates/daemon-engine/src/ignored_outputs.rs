//! What git ignores in the worktrees of tasks that are not working, given back when the disk is low.
//! Alondra knows no build system: an ignored top-level folder (`target/`, `node_modules/`, a build
//! folder) is regenerable by definition, and `git clean -X` deletes only ignored paths, so a file git
//! tracks or does not ignore, and every uncommitted change, stay where they are.
//!
//! A folder that is the output base of a Bazel server still running is never touched either.
//!
//! Conservative on top of that: a folder holding a file that looks like a secret (`.env*`, keys,
//! certificates, credentials) is never touched, nor one smaller than `LIMITS.min`, where such
//! files live, nor one larger than `LIMITS.max`, which is more likely data than build output.
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use alondra_core::domain::DomainState;

use crate::disk::{Space, Tally};
use crate::domain_runtime::DomainRuntime;
use crate::log;

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub min: u64,
    pub max: u64,
}

pub(crate) const LIMITS: Limits = Limits { min: 100 << 20, max: 20 << 30 };

const PROTECTED_PREFIXES: [&str; 2] = [".env", ".envrc"];
const PROTECTED_EXTENSIONS: [&str; 9] = ["pem", "key", "p12", "p8", "pfx", "mobileprovision", "keystore", "jks", "kdbx"];
const PROTECTED_NAMES: [&str; 11] = [
    "credentials", "credentials.json", "id_rsa", "id_ed25519", "id_ecdsa", ".npmrc", ".netrc", ".pypirc", ".git-credentials", "secrets.json", "secrets.yml",
];

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Freed {
    pub worktree: PathBuf,
    pub entries: Vec<(String, u64)>,
}

impl Freed {
    pub(crate) fn bytes(&self) -> u64 {
        self.entries.iter().map(|(_, bytes)| bytes).sum()
    }
}

/// One pass over `worktrees`: each one whose own volume is low (`space_of`) loses its largest ignored
/// folders until the volume is past its recovery line.
pub(crate) fn free(worktrees: &[PathBuf], limits: Limits, space_of: impl Fn(&Path) -> Option<Space>) -> Vec<Freed> {
    let mut freed = vec![];
    for worktree in worktrees {
        if !space_of(worktree).is_some_and(|space| space.low(false)) {
            continue;
        }
        let mut entries = vec![];
        for (entry, _) in candidates(worktree, limits) {
            if !entries.is_empty() && !space_of(worktree).is_some_and(|space| space.low(true)) {
                break;
            }
            if let Some(released) = clean(worktree, &entry) {
                entries.push((entry, released));
            }
        }
        if !entries.is_empty() {
            freed.push(Freed { worktree: worktree.clone(), entries });
        }
    }
    freed
}

/// The open worktrees of tasks whose node is not working: waiting, paused or between turns.
pub(crate) fn idle_worktrees(domain: &DomainRuntime, state: &DomainState) -> Vec<PathBuf> {
    let working = crate::admission::working_tasks(domain, state);
    state.tasks.values().filter(|task| !task.over() && !working.contains(&task.id))
        .flat_map(|task| task.open_worktrees().map(|(path, _)| PathBuf::from(path)))
        .filter(|path| path.join(".git").exists())
        .collect()
}

pub(crate) fn free_idle_worktrees(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    report(&free(&idle_worktrees(domain, &state), LIMITS, |path| crate::disk::space(path)));
}

pub(crate) fn report(freed: &[Freed]) {
    if freed.is_empty() {
        return;
    }
    let total: u64 = freed.iter().map(Freed::bytes).sum();
    let from = freed.iter()
        .map(|each| format!("{} ({} bytes: {})", each.worktree.display(), each.bytes(),
            each.entries.iter().map(|(entry, bytes)| format!("{entry} {bytes}")).collect::<Vec<_>>().join(", ")))
        .collect::<Vec<_>>().join("; ");
    log!("ignored build outputs: the disk is low, freed {total} bytes from {} worktree(s) of tasks that are not working: {from}", freed.len());
}

/// The ignored top-level folders of `worktree` that may go, largest first, with their size.
fn candidates(worktree: &Path, limits: Limits) -> Vec<(String, u64)> {
    let Some(listed) = git(worktree, &["ls-files", "--others", "--ignored", "--exclude-standard", "--directory", "-z"]) else { return vec![] };
    let mut found: Vec<(String, u64)> = listed.split('\0').filter_map(|entry| entry.strip_suffix('/'))
        .filter(|entry| !entry.is_empty() && !entry.contains('/'))
        .filter_map(|entry| {
            let bytes = size_unless_protected(&worktree.join(entry))?;
            (limits.min..=limits.max).contains(&bytes).then(|| (entry.to_owned(), bytes))
        })
        .collect();
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    found
}

/// Deletes the ignored files under `entry` and returns the bytes that gave back; `None` when git
/// refused or nothing went.
fn clean(worktree: &Path, entry: &str) -> Option<u64> {
    let dir = worktree.join(entry);
    let before = size_unless_protected(&dir)?;
    let literal = format!(":(literal){entry}");
    git(worktree, &["clean", "-X", "-d", "-f", "-q", "--", &literal])?;
    let after = size_unless_protected(&dir).unwrap_or(0);
    let released = before.saturating_sub(after);
    (released > 0).then_some(released)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(dir).args(args).stdin(Stdio::null()).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The bytes `dir` takes, or `None` when it is not a plain folder, holds a repository of its own or
/// holds a file that looks like a secret.
fn size_unless_protected(dir: &Path) -> Option<u64> {
    let top = std::fs::symlink_metadata(dir).ok()?;
    if !top.is_dir() || protected(dir.file_name()?.to_str()?) || holds_live_bazel_server(dir) {
        return None;
    }
    let mut tally = Tally::default();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let meta: Metadata = entry.metadata().ok()?;
            if meta.is_dir() {
                if name == ".git" {
                    return None;
                }
                tally.add(&meta);
                pending.push(entry.path());
            } else {
                if protected(name.to_str()?) {
                    return None;
                }
                tally.add(&meta);
            }
        }
    }
    Some(tally.bytes())
}

/// Whether `dir` is the output base of a Bazel server that is still running: deleting it under the
/// server breaks the build it serves.
fn holds_live_bazel_server(dir: &Path) -> bool {
    let Ok(recorded) = std::fs::read_to_string(dir.join("server/server.pid.txt")) else { return false };
    let Ok(pid) = recorded.trim().parse::<i32>() else { return false };
    if pid <= 0 {
        return false;
    }
    let signalled = unsafe { libc::kill(pid, 0) };
    signalled == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn protected(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let extension = Path::new(&name).extension().and_then(|extension| extension.to_str());
    PROTECTED_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        || PROTECTED_NAMES.contains(&name.as_str())
        || extension.is_some_and(|extension| PROTECTED_EXTENSIONS.contains(&extension))
}

#[cfg(test)]
mod tests;
