//! `bazel run //crates/scenarios:ablate -- <case> --revert <file>...`: runs only that case without
//! the fix (the files put back as they were on the base of the branch, a file the fix added taken
//! away) and with it, and prints the block a pull request shows. Every file goes back exactly as
//! it was, byte for byte, whatever happens: a failed run, a panic or an interruption.

use std::fs::Permissions;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn interrupted(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// A file as the fix leaves it: what is in it and who may write it, or that it is not there.
type Saved = Option<(Vec<u8>, Permissions)>;

/// The files of the fix taken back to the base of the branch, and put back as they were when this
/// is dropped.
struct Reverted {
    saved: Vec<(PathBuf, Saved)>,
}

impl Reverted {
    /// Reverts each of `files` (paths inside `workspace`) to what `base` returns for it: its bytes,
    /// or `None` when the base never had it, which takes the file away.
    fn apply(workspace: &Path, files: &[String], base: impl Fn(&str) -> Option<Vec<u8>>) -> Result<Self, String> {
        let mut plan = Vec::new();
        for file in files {
            if Path::new(file).components().any(|part| !matches!(part, Component::Normal(_))) {
                return Err(format!("{file}: name each file by its path inside the repository"));
            }
            let path = workspace.join(file);
            let fixed = std::fs::read(&path).ok().zip(std::fs::metadata(&path).ok()).map(|(bytes, meta)| (bytes, meta.permissions()));
            let old = base(file);
            if fixed.is_none() && old.is_none() {
                return Err(format!("{file}: neither the branch nor its base has this file"));
            }
            plan.push((path, fixed, old));
        }
        let mut reverted = Self { saved: Vec::new() };
        for (path, fixed, old) in plan {
            reverted.saved.push((path.clone(), fixed));
            match old {
                Some(bytes) => {
                    if let Some(folder) = path.parent() {
                        std::fs::create_dir_all(folder).map_err(|error| format!("{}: {error}", path.display()))?;
                    }
                    std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
                }
                None => std::fs::remove_file(&path).map_err(|error| format!("{}: {error}", path.display()))?,
            }
        }
        Ok(reverted)
    }
}

impl Drop for Reverted {
    fn drop(&mut self) {
        for (path, saved) in self.saved.drain(..) {
            match saved {
                Some((bytes, permissions)) => {
                    if std::fs::read(&path).ok().as_deref() != Some(&bytes) {
                        if let Some(folder) = path.parent() {
                            drop(std::fs::create_dir_all(folder));
                        }
                        if let Err(error) = std::fs::write(&path, &bytes) {
                            eprintln!("could not put {} back: {error}", path.display());
                        }
                    }
                    drop(std::fs::set_permissions(&path, permissions));
                }
                None => drop(std::fs::remove_file(&path)),
            }
        }
    }
}

fn bazel(workspace: &Path, case: &str) -> Result<(bool, String), String> {
    let output = Command::new("bazelisk")
        .current_dir(workspace)
        .args(["test", "//crates/scenarios:scenarios_test", "--test_output=errors", "--test_arg"])
        .arg(case)
        .output()
        .map_err(|error| format!("bazelisk did not run: {error}"))?;
    Ok((output.status.success(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))))
}

fn git(workspace: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git").current_dir(workspace).args(args).output().ok()?;
    output.status.success().then_some(output.stdout)
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(60)..].join("\n")
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

/// The block a pull request shows, and whether the case proved something.
fn ablate(workspace: &Path, case: &str, files: &[String]) -> Result<(String, bool), String> {
    let base = git(workspace, &["merge-base", "HEAD", "origin/main"])
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
        .ok_or("git has no merge base of HEAD and origin/main")?;
    let without = {
        let _reverted = Reverted::apply(workspace, files, |file| git(workspace, &["cat-file", "blob", &format!("{base}:{file}")]))?;
        bazel(workspace, case)?
    };
    if INTERRUPTED.load(Ordering::SeqCst) {
        return Err("interrupted: the files are back as they were".into());
    }
    let with = bazel(workspace, case)?;
    let report = format!(
        "### `{case}` without the fix ({})\n\n```\n{}\n```\n\n### `{case}` with the fix ({})\n\n```\n{}\n```",
        if without.0 { "PASSED: it proves nothing" } else { "fails" },
        tail(&without.1),
        if with.0 { "passes" } else { "FAILS" },
        tail(&with.1),
    );
    Ok((report, !without.0 && with.0))
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(case) = arguments.next() else {
        fail("usage: ablate <case> --revert <file>...");
    };
    let files: Vec<String> = arguments.skip_while(|argument| argument != "--revert").skip(1).collect();
    if files.is_empty() {
        fail("name the files of the fix after --revert");
    }
    let workspace = PathBuf::from(std::env::var_os("BUILD_WORKSPACE_DIRECTORY").unwrap_or_else(|| fail("run it with `bazel run`")));
    let handler = interrupted as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: the handler only stores into an atomic.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    match ablate(&workspace, &case, &files) {
        Ok((report, proved)) => {
            println!("{report}");
            if !proved {
                std::process::exit(1);
            }
        }
        Err(message) => fail(&message),
    }
}

#[cfg(test)]
#[path = "ablate_tests.rs"]
mod tests;
