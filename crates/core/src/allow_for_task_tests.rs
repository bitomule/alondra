use super::*;
use serde_json::json;

const WORKTREE: &str = "/Users/me/repo/.claude/worktrees/t-1";
const FILES: &str = "/Users/me/.alondra/files";

fn folders() -> Vec<PathBuf> {
    vec![PathBuf::from(WORKTREE), PathBuf::from(FILES)]
}

fn asked_from(cwd: &str, command: &str) -> AllowForTask {
    let input = json!({"command": command});
    assess(&Prompt { tool: Some("Bash"), input: &input, cwd: Some(cwd), persistent_refused: false, ask_rule: false }, &folders())
}

fn asked(command: &str) -> AllowForTask {
    asked_from(WORKTREE, command)
}

fn exact(command: &str) -> AllowForTask {
    AllowForTask::Exact { command: command.into(), cwd: WORKTREE.into() }
}

fn why(offer: &AllowForTask) -> Option<NotForTask> {
    match offer {
        AllowForTask::Refused { why, .. } => Some(*why),
        _ => None,
    }
}

#[test]
fn what_deletes_or_discards_is_offered_only_as_that_exact_command() {
    for command in [
        "git reset --hard origin/main",
        "git branch -D scratch",
        "git checkout -- src/lib.rs",
        "git restore src/lib.rs",
        "git clean -fdx",
        "rm -rf build",
        "cp bazel-bin/app /tmp/alondra-sbx-f0b/bin/app",
        "make test",
        "git commit -m wip",
        "cargo test && rm -rf target",
    ] {
        assert_eq!(asked(command), exact(command), "{command}");
    }
}

#[test]
fn an_exact_grant_covers_only_the_same_command_from_the_same_folder() {
    let grant = asked("git reset --hard origin/main");
    assert!(grant.covers(&asked("git reset --hard origin/main")));
    assert!(!grant.covers(&asked("git reset --hard HEAD~1")), "another target asks again");
    assert!(!grant.covers(&asked("git reset --hard origin/main ")), "not even a byte more");
    assert!(!grant.covers(&asked_from(&format!("{WORKTREE}/sub"), "git reset --hard origin/main")), "from another folder it asks again");
    let branch = asked("git branch -D scratch-a");
    assert!(!branch.covers(&asked("git branch -D scratch-b")), "another branch asks again");
    let rm = asked("rm -rf build/a");
    assert!(!rm.covers(&asked("rm -rf build/b")), "a deletion never covers another");
}

#[test]
fn reads_tests_and_builds_are_offered_by_their_shape() {
    let shape = |command: &str| match asked(command) {
        AllowForTask::Shape { shape, .. } => shape,
        other => panic!("{command}: {other:?}"),
    };
    assert_eq!(shape("bazelisk test //crates/core:alondra_core_test"), ["bazelisk", "test"]);
    assert_eq!(shape("timeout 600 bazelisk build //crates/..."), ["bazelisk", "build"]);
    assert_eq!(shape("cargo clippy --all-targets"), ["cargo", "clippy"]);
    assert_eq!(shape("git log --oneline -5"), ["git", "log"]);
    assert_eq!(shape("grep -rn allow crates"), ["grep"]);
    assert_eq!(shape("npm run typecheck"), ["npm", "run", "typecheck"]);
    assert_eq!(shape("python3 -m unittest discover -s deploy/brew/tests"), ["python3", "-m", "unittest"]);
    let grant = asked("bazelisk test //crates/core:alondra_core_test");
    assert!(grant.covers(&asked("bazelisk test --test_output=errors //crates/alondrad:alondrad_test")), "the same shape, other arguments");
    assert!(!grant.covers(&asked("bazelisk run //tools:release")), "another subcommand asks again");
    assert!(!grant.covers(&asked("bazelisk test //x > /Users/me/out.txt")), "a write outside the task's folders asks again");
}

#[test]
fn what_could_run_or_write_anything_is_never_a_shape() {
    for command in [
        "bazelisk test --run_under=strace //x",
        "find . -delete",
        "find . -name x -exec grep -l y \\;",
        "git diff --output=patch",
        "cargo test > log.txt",
        "npm run release",
        "make build",
        "./bazel-bin/app",
        "cargo test | tee log.txt",
    ] {
        assert!(matches!(asked(command), AllowForTask::Exact { .. }), "{command}: {:?}", asked(command));
    }
}

#[test]
fn a_folder_or_path_outside_the_task_is_not_offered_and_says_which() {
    let refused = |cwd: &str, command: &str| match asked_from(cwd, command) {
        AllowForTask::Refused { why: NotForTask::Outside, part } => part,
        other => panic!("{command}: {other:?}"),
    };
    assert_eq!(refused(WORKTREE, "rm -rf /Users/me/Downloads/x"), "/Users/me/Downloads/x");
    assert_eq!(refused(WORKTREE, "cp a ../../../other/a"), "../../../other/a");
    assert_eq!(refused(WORKTREE, "cat a > /Users/me/out.txt"), "/Users/me/out.txt");
    assert_eq!(refused(WORKTREE, "cd /Users/me/other && git reset --hard"), "/Users/me/other");
    assert_eq!(refused("/Users/me/other", "git reset --hard origin/main"), "/Users/me/other");
    assert_eq!(refused(WORKTREE, "rm -rf /tmp/*"), "/tmp/*");
    assert_eq!(refused(WORKTREE, "rm -rf /tmp/alondra-sbx-*"), "/tmp/alondra-sbx-*");
    assert_eq!(refused(WORKTREE, "cp a /tmp/elsewhere/a"), "/tmp/elsewhere/a");
}

#[test]
fn the_task_folders_alondras_files_and_the_sandbox_temporaries_are_inside() {
    for command in [
        "cp bazel-bin/app /tmp/alondra-sbx-f0b/bin/app.next",
        "rm -rf /private/tmp/asb.x1y2/mac-1",
        "cp report.md /Users/me/.alondra/files/report.md",
        "cd crates/core && cargo test",
        "rm -rf bazel-*",
        "bazelisk test --test_output=errors //crates/...",
    ] {
        assert!(asked(command).offered(), "{command}: {:?}", asked(command));
    }
}

#[test]
fn a_prompt_alondra_cannot_judge_is_not_offered() {
    let input = json!({"command": "git reset --hard"});
    let prompt = |tool, cwd, persistent_refused, ask_rule| Prompt { tool, input: &input, cwd, persistent_refused, ask_rule };
    assert_eq!(why(&assess(&prompt(Some("Write"), Some(WORKTREE), false, false), &folders())), Some(NotForTask::NotCommand));
    assert_eq!(why(&assess(&prompt(Some("Bash"), None, false, false), &folders())), Some(NotForTask::NoFolder));
    assert_eq!(why(&assess(&prompt(Some("Bash"), Some("relative"), false, false), &folders())), Some(NotForTask::NoFolder));
    assert_eq!(why(&assess(&prompt(Some("Bash"), Some(WORKTREE), true, false), &folders())), Some(NotForTask::Persistent));
    assert_eq!(why(&assess(&prompt(Some("Bash"), Some(WORKTREE), false, true), &folders())), Some(NotForTask::AskRule));
    for command in ["rm -rf $DIR", "rm -rf ~/x", "echo $(rm x)", "git reset --hard `cat ref`"] {
        assert_eq!(why(&asked(command)), Some(NotForTask::Unreadable), "{command}");
    }
    assert_eq!(why(&asked(&"x".repeat(allow_always::MAX_COMMAND_CHARS + 1))), Some(NotForTask::TooLong));
}

#[test]
fn a_refused_prompt_is_never_covered() {
    let grant = AllowForTask::Shape { command: "cat a".into(), cwd: WORKTREE.into(), shape: vec!["cat".into()] };
    assert!(!grant.covers(&asked("cat /Users/me/.ssh/id_rsa")));
    let exact = exact("git reset --hard origin/main");
    assert!(!exact.covers(&asked_from("/Users/me/other", "git reset --hard origin/main")));
}
