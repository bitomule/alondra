use super::*;

const WORKTREE: &str = "/repo/.claude/worktrees/t";
const BRANCH: &str = "worktree-t";

fn judged(command: &str) -> Option<Verdict> {
    judged_from(command, WORKTREE)
}

fn judged_from(command: &str, cwd: &str) -> Option<Verdict> {
    judge(command, Some(cwd), &[TaskBranch { worktree: WORKTREE.into(), branch: BRANCH.into() }])
}

fn denied(command: &str) -> String {
    match judged(command) {
        Some(Verdict::Deny(message)) => message,
        other => panic!("{command}: {other:?}"),
    }
}

#[test]
fn the_conflict_flow_runs_without_asking_in_the_task_worktree() {
    for command in [
        "git fetch origin main",
        "git fetch origin",
        "git rebase origin/main",
        "git rebase --continue",
        "git rebase --abort",
        "GIT_EDITOR=true git rebase --continue",
        "git merge origin/main",
        "git merge --no-edit origin/main",
        "git merge --abort",
        "git push",
        "git push --force-with-lease",
        "git push --force-with-lease origin HEAD",
        "git push --force-with-lease origin worktree-t",
        "git push --force-with-lease origin HEAD:worktree-t",
        "git push --force-with-lease=worktree-t origin worktree-t",
        "git push -u origin worktree-t",
        "git fetch origin main && git rebase origin/main",
        "cd /repo/.claude/worktrees/t/crates && git rebase origin/main && git push --force-with-lease",
    ] {
        assert_eq!(judged(command), Some(Verdict::Allow), "{command}");
    }
}

#[test]
fn a_subfolder_of_the_worktree_counts_as_the_worktree() {
    assert_eq!(judged_from("git rebase origin/main", "/repo/.claude/worktrees/t/crates/core"), Some(Verdict::Allow));
}

#[test]
fn reset_hard_is_denied_and_the_node_is_told_to_rebase() {
    for command in ["git reset --hard origin/main", "git reset --hard", "git -C /elsewhere reset --hard HEAD~1", "git fetch origin main && git reset --hard origin/main"] {
        assert!(denied(command).contains("Rebase on the base branch"), "{command}");
    }
    assert!(matches!(judge("git reset --hard", None, &[]), Some(Verdict::Deny(_))), "also with no known folder");
}

#[test]
fn a_force_push_to_main_or_another_branch_is_denied() {
    for command in [
        "git push --force origin main",
        "git push --force-with-lease origin main",
        "git push --force-with-lease origin HEAD:main",
        "git push -f origin HEAD:refs/heads/main",
        "git push origin +main",
        "git push --force-with-lease origin someone-elses-branch",
        "git push --force-with-lease origin worktree-t:main",
    ] {
        assert!(denied(command).contains("only your task's own branch"), "{command}");
    }
}

#[test]
fn what_does_not_resolve_to_the_task_branch_in_the_task_worktree_is_left_to_the_user() {
    for command in [
        "git push --force origin worktree-t",
        "git push --force",
        "git push origin +worktree-t",
        "git push origin main",
        "git push --force-with-lease upstream worktree-t",
        "git push --force-with-lease=main origin worktree-t",
        "git push --delete origin worktree-t",
        "git push --mirror",
        "git -C /elsewhere rebase origin/main",
        "git --git-dir=/elsewhere/.git rebase origin/main",
        "git -c core.hooksPath=/x rebase origin/main",
        "git rebase origin/main --exec 'make'",
        "git rebase -i origin/main",
        "git rebase main",
        "git rebase --onto origin/main a b",
        "git fetch origin +main:main",
        "git fetch https://example.com/x.git",
        "git rebase origin/main > out.txt",
        "git rebase origin/main; rm -rf build",
        "git rebase $(echo origin/main)",
        "cd /elsewhere && git rebase origin/main",
        "git checkout main",
        "git branch -D other",
    ] {
        assert_eq!(judged(command), None, "{command}");
    }
}

#[test]
fn outside_every_task_worktree_nothing_is_allowed_or_force_denied() {
    for cwd in ["/repo", "/repo/.claude/worktrees/other", "/repo/.claude/worktrees/t-longer"] {
        assert_eq!(judged_from("git rebase origin/main", cwd), None, "{cwd}");
        assert_eq!(judged_from("git push --force-with-lease origin main", cwd), None, "{cwd}");
    }
    assert_eq!(judge("git rebase origin/main", None, &[TaskBranch { worktree: WORKTREE.into(), branch: BRANCH.into() }]), None, "no folder reported");
}

#[test]
fn the_branch_is_the_one_of_the_worktree_the_command_runs_in() {
    let branches = [
        TaskBranch { worktree: WORKTREE.into(), branch: BRANCH.into() },
        TaskBranch { worktree: "/repo/.claude/worktrees/t/sub".into(), branch: "sub-branch".into() },
    ];
    let from_sub = |command: &str| judge(command, Some("/repo/.claude/worktrees/t/sub"), &branches);
    assert_eq!(from_sub("git push --force-with-lease origin sub-branch"), Some(Verdict::Allow));
    assert!(matches!(from_sub("git push --force-with-lease origin worktree-t"), Some(Verdict::Deny(_))));
}
