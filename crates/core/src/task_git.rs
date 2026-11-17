//! The git commands Alondra itself tells a node to run when its PR falls behind or conflicts
//! (`BranchUpdate::instruction`): they are allowed without a card, but only in the task's own
//! worktree and on the branch Alondra made for it. What would throw work away (`reset --hard`,
//! a force-push to any other branch) is denied with the reason the node needs to change course.
//! Anything else is left to the user: the verdict never reads the command's words loosely, it
//! reads the folder and the branch the command resolves to.

use std::path::{Path, PathBuf};

use crate::allow_always::{self, RiskKind, Word};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(String),
}

/// An open worktree of the task and the branch checked out in it.
#[derive(Clone, Debug)]
pub struct TaskBranch {
    pub worktree: PathBuf,
    pub branch: String,
}

const RESET_DENIED: &str = "Do not run `git reset --hard`: it throws work away. Rebase on the base branch instead (`git fetch origin main`, `git rebase origin/main`, resolve the conflicts, then `git push --force-with-lease` to your task's own branch).";

fn push_denied(destination: &str, branch: &str) -> String {
    format!(
        "Never force-push to `{destination}`: only your task's own branch (`{branch}`) can be force-pushed. Rebase on the base branch and run \
         `git push --force-with-lease` to `{branch}`."
    )
}

/// What Alondra decides for a Bash `command` run from `cwd` by the node of a task whose open
/// worktrees are `branches`; `None` puts it to the user.
pub fn judge(command: &str, cwd: Option<&str>, branches: &[TaskBranch]) -> Option<Verdict> {
    if allow_always::risks(command).iter().any(|risk| risk.kind == RiskKind::ResetHard) {
        return Some(Verdict::Deny(RESET_DENIED.into()));
    }
    let parts = allow_always::parse(command).filter(|parts| !parts.is_empty())?;
    let cwd = cwd.filter(|cwd| Path::new(cwd).is_absolute())?;
    let mut base = allow_always::lexical(Path::new("/"), cwd);
    let here = |base: &Path| branches.iter().filter(|b| base.starts_with(&b.worktree)).max_by_key(|b| b.worktree.components().count());
    let mut verdicts = vec![];
    for part in &parts {
        let Some(task) = here(&base) else { return None };
        if !part.redirects.is_empty() {
            verdicts.push(None);
            continue;
        }
        let words = leading_editor_stripped(&part.words);
        if words.first().is_some_and(|w| w.text == "cd") {
            let [_, target] = words else { return None };
            if target.glob || target.text == "-" {
                return None;
            }
            base = allow_always::lexical(&base, &target.text);
            verdicts.push(here(&base).map(|_| Verdict::Allow));
            continue;
        }
        verdicts.push(git_verdict(words, &task.branch));
    }
    if let Some(deny) = verdicts.iter().flatten().find(|v| matches!(v, Verdict::Deny(_))) {
        return Some(deny.clone());
    }
    verdicts.iter().all(|v| v.is_some()).then_some(Verdict::Allow)
}

/// `GIT_EDITOR=true git rebase --continue` keeps a rebase from opening an editor.
fn leading_editor_stripped(words: &[Word]) -> &[Word] {
    let mut words = words;
    while words.first().is_some_and(|w| !w.glob && matches!(w.text.as_str(), "GIT_EDITOR=true" | "GIT_SEQUENCE_EDITOR=true")) {
        words = &words[1..];
    }
    words
}

fn git_verdict(words: &[Word], branch: &str) -> Option<Verdict> {
    let text: Vec<&str> = words.iter().map(|w| if w.glob { "*" } else { w.text.as_str() }).collect();
    let [git, sub, args @ ..] = text.as_slice() else { return None };
    if *git != "git" || sub.starts_with('-') {
        return None;
    }
    let allowed = match *sub {
        "fetch" => fetch(args),
        "rebase" => matches!(args, ["--continue"] | ["--abort"]) || matches!(args, [target] if remote_branch(target)),
        "merge" => {
            matches!(args, ["--continue"] | ["--abort"])
                || matches!(args, [target] | [target, "--no-edit"] | ["--no-edit", target] if remote_branch(target))
        }
        "push" => return push(args, branch),
        _ => false,
    };
    allowed.then_some(Verdict::Allow)
}

fn fetch(args: &[&str]) -> bool {
    matches!(args, ["origin", refs @ ..] if refs.iter().all(|r| safe_ref(r)))
}

fn safe_ref(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with(['-', '/'])
        && !text.contains("..")
        && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
}

/// `origin/main`: a branch the remote already has, which only brings work in.
fn remote_branch(text: &str) -> bool {
    text.strip_prefix("origin/").is_some_and(safe_ref)
}

/// `git push` of the task's own branch to `origin`, forced only with a lease; a force to any
/// other branch is denied, and every other shape is the user's to decide.
fn push(args: &[&str], branch: &str) -> Option<Verdict> {
    let (mut lease, mut hard_force, mut plus) = (false, false, false);
    let mut positional = vec![];
    for arg in args {
        match *arg {
            "--force-with-lease" => lease = true,
            "--force-if-includes" | "-u" | "--set-upstream" => {}
            "--force" | "-f" => hard_force = true,
            lease_arg if lease_arg.strip_prefix("--force-with-lease=").is_some_and(|value| value.split(':').next() == Some(branch)) => lease = true,
            option if option.starts_with('-') => return None,
            other => positional.push(other),
        }
    }
    let (remote, refspecs) = match positional.as_slice() {
        [] => (None, &[][..]),
        [remote, refspecs @ ..] => (Some(*remote), refspecs),
    };
    plus |= refspecs.iter().any(|r| r.starts_with('+'));
    let forced = lease || hard_force || plus;
    let mut foreign = None;
    for refspec in refspecs {
        let refspec = refspec.trim_start_matches('+');
        let (source, destination) = refspec.split_once(':').map_or((None, refspec), |(s, d)| (Some(s), d));
        let destination = destination.strip_prefix("refs/heads/").unwrap_or(destination);
        let own_source = source.is_none_or(|s| s == "HEAD" || s == branch || s == format!("refs/heads/{branch}"));
        let own_destination = destination == branch || (source.is_none() && destination == "HEAD");
        if !own_destination && safe_ref(destination) {
            foreign.get_or_insert(destination.to_owned());
        } else if !own_destination || !own_source {
            return None;
        }
    }
    if let (true, Some(destination)) = (forced, &foreign) {
        return Some(Verdict::Deny(push_denied(destination, branch)));
    }
    if foreign.is_some() || remote.is_some_and(|remote| remote != "origin") || hard_force || plus {
        return None;
    }
    Some(Verdict::Allow)
}

#[cfg(test)]
#[path = "task_git_tests.rs"]
mod tests;
