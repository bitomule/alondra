//! What the orchestrator reads with the case of a stopped node: the task's mission, the node's
//! last words and its worktree, read when the case reaches it.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{DomainState, Interaction, Scope, Task, WorktreeOrigin, is_stall_case};
use std::path::Path;
use std::process::Command;

const MISSION_SHOWN: usize = 4_000;
const WORDS_SHOWN: usize = 2_000;
const STATUS_LINES: usize = 30;

/// The case's context, or nothing for a message that is not a case.
pub(crate) fn context(domain: &DomainRuntime, state: &DomainState, case: &Interaction) -> String {
    if !is_stall_case(case) || !state.open(case) {
        return String::new();
    }
    let Scope::Task { task_id } = &case.scope else { return String::new() };
    let Some(task) = state.tasks.get(task_id) else { return String::new() };
    let plan = domain.launch_plan(task_id).ok().flatten();
    let mission = plan.as_ref().and_then(|plan| plan.mission.clone())
        .or_else(|| state.requests.get(&task.request_id).map(|request| request.body.clone()))
        .unwrap_or_default();
    let words = task.agent_id.as_ref()
        .and_then(|agent| domain.live.mods.session(&agent.0))
        .and_then(|reported| reported.answer)
        .map(|answer| answer.text)
        .filter(|text| !text.trim().is_empty());
    let worktree = own_worktree(task).or_else(|| plan.as_ref().map(|plan| plan.cwd.clone()));
    format!(
        "\nIts mission:\n{}\n\nIts last words:\n{}\n\nIts worktree:\n{}",
        shown(&mission, MISSION_SHOWN),
        words.map_or_else(|| "(none recorded)".to_owned(), |words| shown(&words, WORDS_SHOWN)),
        worktree.map_or_else(|| "(unknown)".to_owned(), |path| worktree_state(Path::new(&path))))
}

fn own_worktree(task: &Task) -> Option<String> {
    task.worktrees.iter()
        .find(|(_, worktree)| worktree.end.is_none() && matches!(worktree.origin, WorktreeOrigin::Task | WorktreeOrigin::Inherited))
        .map(|(path, _)| path.clone())
}

/// Its branch against its upstream, what is not committed, and its last commits.
pub(crate) fn worktree_state(dir: &Path) -> String {
    let Some(status) = git(dir, &["status", "--porcelain=v1", "--branch"]) else {
        return format!("{} (not a readable git worktree)", dir.display());
    };
    let mut lines = status.lines();
    let branch = lines.next().unwrap_or_default().trim_start_matches("## ").to_owned();
    let changes: Vec<&str> = lines.collect();
    let uncommitted = match changes.len() {
        0 => "nothing uncommitted".to_owned(),
        n if n <= STATUS_LINES => format!("{n} uncommitted:\n{}", changes.join("\n")),
        n => format!("{n} uncommitted, the first {STATUS_LINES}:\n{}", changes[..STATUS_LINES].join("\n")),
    };
    let commits = git(dir, &["log", "--oneline", "-5"]).unwrap_or_default();
    format!("{}\nbranch {branch}\n{uncommitted}\nlast commits:\n{}", dir.display(), commits.trim_end())
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn shown(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn the_worktree_state_names_its_branch_what_is_uncommitted_and_its_last_commits() {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "work"]);
        run(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "Half the fix"]);
        std::fs::write(dir.join("left.txt"), "x").unwrap();
        let state = worktree_state(&dir);
        assert!(state.contains("branch work"), "{state}");
        assert!(state.contains("1 uncommitted:\n?? left.txt"), "{state}");
        assert!(state.contains("Half the fix"), "{state}");
    }

    #[test]
    fn a_folder_that_is_not_a_worktree_says_so() {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(worktree_state(&dir).ends_with("(not a readable git worktree)"));
    }
}
