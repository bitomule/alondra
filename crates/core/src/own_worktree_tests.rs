use super::*;
use crate::test_support::Directory;

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git").current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=Ana", "-c", "user.email=ana@example.com"]).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

/// In a repository's `.claude/worktrees`: the user's own checkout `main` and `wt`, a linked
/// worktree of it that is the task's, with a subagent's worktree nested in its own
/// `.claude/worktrees`, and an `outside` folder that links in `wt` point to. The repository's hooks
/// live in `hooks`, empty.
struct Fixture {
    dir: Directory,
    roots: Vec<PathBuf>,
}

impl Fixture {
    fn new() -> Self {
        let dir = Directory::new();
        let at = |path: &str| dir.join(".claude/worktrees").join(path);
        std::fs::create_dir_all(at("main")).unwrap();
        std::fs::create_dir_all(at("hooks")).unwrap();
        git(&at("main"), &["init", "-q", "-b", "main"]);
        git(&at("main"), &["commit", "-q", "--allow-empty", "-m", "Start"]);
        git(&at("main"), &["config", "core.hooksPath", &at("hooks").display().to_string()]);
        git(&at("main"), &["worktree", "add", "-q", "-b", "worktree-wt", &at("wt").display().to_string()]);
        for folder in ["outside/dir", "wt/.scratch/x", "wt/.scratch/empty", "wt/src", "wt/sub/deep", "wt/.claude/worktrees/nested"] {
            std::fs::create_dir_all(at(folder)).unwrap();
        }
        for file in ["outside/secret", "outside/shared", "outside/dir/f", "wt/.scratch/x/start", "wt/.scratch/a.log", "wt/src/a.rs", "wt/sub/f",
            "wt/sub/deep/g", "wt/s.sh", "wt/log", "wt/a", "main/f"] {
            std::fs::write(at(file), "a\n").unwrap();
        }
        std::fs::write(at("wt/.claude/worktrees/nested/.git"), "gitdir: /repo/.git/worktrees/nested\n").unwrap();
        std::os::unix::fs::symlink(at("outside"), at("wt/link-out")).unwrap();
        std::os::unix::fs::symlink(at("outside/secret"), at("wt/file-link-out")).unwrap();
        std::os::unix::fs::symlink(at("outside/missing"), at("wt/dangling")).unwrap();
        std::os::unix::fs::symlink(at("wt/sub"), at("wt/link-in")).unwrap();
        std::fs::hard_link(at("outside/shared"), at("wt/hard")).unwrap();
        assert!(std::process::Command::new("mkfifo").arg(at("wt/pipe")).status().unwrap().success());
        let recorded = [at("wt"), at("wt/.claude/worktrees/nested"), at("main")].map(|p| p.display().to_string());
        let roots = roots(recorded.iter().map(String::as_str));
        Fixture { dir, roots }
    }

    fn at(&self, path: &str) -> PathBuf {
        self.dir.join(".claude/worktrees").join(path)
    }

    fn path(&self, path: &str) -> String {
        std::fs::canonicalize(self.at(path)).unwrap().display().to_string()
    }

    fn judged_in(&self, cwd: &str, command: &str) -> Result<(), Keeps> {
        judge(command, &self.path(cwd), &self.roots)
    }

    fn judged(&self, command: &str) -> Result<(), Keeps> {
        self.judged_in("wt", command)
    }
}

#[test]
fn only_alondras_linked_worktrees_count_as_the_tasks_own() {
    let f = Fixture::new();
    let own: Vec<String> = f.roots.iter().map(|p| p.display().to_string()).collect();
    assert_eq!(own, [f.path("wt"), f.path("wt/.claude/worktrees/nested")], "the user's own checkout, whose .git is a folder, never counts");
    assert_eq!(f.judged_in("main", "rm f"), Err(Keeps::Outside));
    assert_eq!(judge("rm .scratch/a.log", &f.path("wt"), &[]), Err(Keeps::Outside), "a task without worktrees asks");
    let elsewhere = f.dir.join("elsewhere/wt2");
    let stored = f.dir.join("storage/wt3");
    std::fs::create_dir_all(f.dir.join("storage")).unwrap();
    std::fs::write(f.dir.join("storage").join(STORAGE_RECORD), "{}").unwrap();
    for (folder, branch) in [(&elsewhere, "wt2"), (&stored, "wt3")] {
        git(&f.at("main"), &["worktree", "add", "-q", "-b", branch, &folder.display().to_string()]);
    }
    let found = roots([elsewhere.display().to_string(), stored.display().to_string()].iter().map(String::as_str));
    assert_eq!(found, [std::fs::canonicalize(&stored).unwrap()], "a linked worktree a node entered elsewhere is not Alondra's");
}

#[test]
fn the_two_cases_that_left_nodes_waiting_overnight_ask_nothing() {
    let f = Fixture::new();
    assert_eq!(f.judged("sed -i '' 's/a/b/' .scratch/x/start"), Ok(()));
    assert_eq!(f.judged("git checkout --theirs -- src/a.rs"), Ok(()));
}

#[test]
fn file_changes_inside_the_worktree_ask_nothing() {
    let f = Fixture::new();
    for command in [
        "rm link-out",
        "rm -f file-link-out",
        "rm dangling",
        "unlink link-out",
        "rm -rf .scratch/x",
        "rm -r -- .scratch/x",
        "rm .scratch/*.log",
        "rm -rf link-out",
        "rm -rf link-in/",
        "rm -f '^x' 'a~b'",
        "rmdir .scratch/empty",
        "mv a b",
        "mv a .scratch/x",
        "mv .scratch/a.log .",
        "mv -f src/a.rs src/b.rs",
        "truncate -s 0 log",
        "truncate --size=0 .scratch/new.log",
        "chmod +x s.sh",
        "chmod -x s.sh",
        "chmod 644 src/a.rs sub/f",
        "sed -i.bak 's/a/b/g' src/a.rs",
        "sed -i '' -e 's|a|b|' -e '/^$/d' src/a.rs sub/f",
        "sed -i '' -E 's/(a)/\\1b/' .scratch/x/start",
        "sed --in-place 's/a/b/;2d' log",
        "git checkout --ours -- src/a.rs sub/f",
        "git checkout -- sub",
        "git restore --staged -- src/a.rs",
        "git restore --source=origin/main --worktree src/a.rs",
        "rm -f a 2>/dev/null",
        "rm -f a || true",
        "timeout 5 rm a",
        "LANG=C sed -i '' 's/a/b/' log",
    ] {
        assert_eq!(f.judged(command), Ok(()), "{command}");
    }
    assert_eq!(f.judged_in("wt/sub", "rm f"), Ok(()));
    assert_eq!(f.judged_in("wt/sub", "git checkout -- ."), Ok(()));
}

#[test]
fn paths_that_resolve_outside_the_worktree_keep_asking() {
    let f = Fixture::new();
    let outside = f.path("outside/secret");
    for (command, keeps) in [
        (format!("rm {outside}"), Keeps::Outside),
        ("rm ../outside/secret".into(), Keeps::Outside),
        ("rm ../main/f".into(), Keeps::Outside),
        ("rm link-out/secret".into(), Keeps::Outside),
        ("rm -rf link-out/".into(), Keeps::Outside),
        ("rm -rf link-out/.".into(), Keeps::Outside),
        ("rm link-out/dir/../secret".into(), Keeps::Outside),
        ("rm sub/../link-out/secret".into(), Keeps::Outside),
        ("rm link-out/*".into(), Keeps::Outside),
        ("mv a link-out".into(), Keeps::Outside),
        ("mv link-out/secret a".into(), Keeps::Outside),
        ("sed -i '' 's/a/b/' file-link-out".into(), Keeps::Outside),
        ("chmod 777 file-link-out".into(), Keeps::Outside),
        ("truncate -s 0 dangling".into(), Keeps::Uncertain),
        ("truncate -s 0 hard".into(), Keeps::Uncertain),
        ("chmod 600 hard".into(), Keeps::Uncertain),
        ("truncate -s 0 pipe".into(), Keeps::Uncertain),
        ("chmod 600 pipe".into(), Keeps::Uncertain),
        ("git checkout -- link-out/secret".into(), Keeps::Outside),
        ("rm nowhere/x".into(), Keeps::Uncertain),
        ("rm ..".into(), Keeps::Uncertain),
    ] {
        assert_eq!(f.judged(&command), Err(keeps), "{command}");
    }
    assert_eq!(judge("rm a", "sub", &f.roots), Err(Keeps::Outside), "a folder that is not absolute");
    assert_eq!(judge("rm a", &f.at("wt/nowhere").display().to_string(), &f.roots), Err(Keeps::Uncertain));
}

#[test]
fn the_worktree_its_git_and_the_worktrees_it_holds_are_never_changed() {
    let f = Fixture::new();
    for (command, keeps) in [
        ("rm -rf .", Keeps::Protected),
        ("rm -rf ./", Keeps::Protected),
        ("rm -rf ../wt", Keeps::Protected),
        ("rm -rf *", Keeps::Protected),
        ("rm -rf .*", Keeps::Uncertain),
        ("rm -rf */f", Keeps::Uncertain),
        ("rm .git", Keeps::Protected),
        ("rm .GIT", Keeps::Protected),
        ("rm -rf .scratch/../.git", Keeps::Protected),
        ("rm -rf .claude", Keeps::Protected),
        ("rm -rf .CLAUDE", Keeps::Protected),
        ("rm -rf .claude/worktrees/nested", Keeps::Protected),
        ("rm -rf .claude/worktrees/NESTED", Keeps::Protected),
        ("rm .claude/worktrees/nested/.git", Keeps::Protected),
        ("git checkout -- .", Keeps::Protected),
        ("mv sub .git", Keeps::Protected),
        ("mv sub .GIT", Keeps::Protected),
        ("mv link-in/ d", Keeps::Uncertain),
        ("chmod 700 .", Keeps::Protected),
    ] {
        assert_eq!(f.judged(command), Err(keeps), "{command}");
    }
    assert_eq!(f.judged_in("wt/.claude/worktrees/nested", "git checkout -- ../../../sub/f"), Err(Keeps::Outside),
        "git runs in the innermost worktree, which does not hold the path");
}

// A `cd` may not take effect (in a pipe, behind a wrapper, when it fails), and an earlier change may
// remake what a later one names: a command holds one change, beside nothing but `true`.
#[test]
fn one_change_with_nothing_beside_it_but_true() {
    let f = Fixture::new();
    for command in [
        "cd ./sub && rm f",
        "cd ./sub | true; truncate -s 0 file-link-out",
        "nice cd ./sub; chmod 666 file-link-out",
        "rm -rf b; mv link-out b; truncate -s 0 b/secret",
        "rm a; rm b",
        "rm a && sed -i '' 's/a/b/' log",
        "true",
        "cd",
    ] {
        assert_eq!(f.judged(command), Err(Keeps::NotFileChange), "{command}");
    }
}

#[test]
fn what_the_words_do_not_show_keeps_asking() {
    let f = Fixture::new();
    for (command, keeps) in [
        ("rm -rf \"$PWD\"", Keeps::Unreadable),
        ("rm -rf ~/x", Keeps::Unreadable),
        ("rm $(cat list)", Keeps::Unreadable),
        ("rm `cat list`", Keeps::Unreadable),
        ("rm a &", Keeps::Unreadable),
        ("(cd sub; rm f)", Keeps::Unreadable),
        ("FOO=1 rm a", Keeps::Unreadable),
        ("rm =node", Keeps::Unreadable),
        ("chmod 000 =node", Keeps::Unreadable),
        ("truncate -s 0 =node", Keeps::Unreadable),
        ("rm -rf ^x/../v2", Keeps::Unreadable),
        ("rm a~b", Keeps::Unreadable),
        ("git restore -s HEAD~1 sub/f", Keeps::Unreadable),
        ("sudo rm a", Keeps::NotFileChange),
        ("env -C /tmp rm a", Keeps::NotFileChange),
        ("xargs rm", Keeps::NotFileChange),
        ("./rm a", Keeps::NotFileChange),
        ("/bin/rm a", Keeps::NotFileChange),
        ("rm a > out.txt", Keeps::NotFileChange),
        ("rm a < list", Keeps::NotFileChange),
        ("rm a && curl https://example.com", Keeps::NotFileChange),
        ("rm a | cat", Keeps::NotFileChange),
        ("rm", Keeps::NotFileChange),
        ("rm --no-preserve-root a", Keeps::NotFileChange),
        ("rmdir -p .scratch/empty", Keeps::NotFileChange),
        ("mv -t sub a", Keeps::NotFileChange),
        ("mv *.rs sub", Keeps::Uncertain),
        ("rm sub/?", Keeps::Uncertain),
        ("rm su*/f", Keeps::Uncertain),
        ("chmod -R 777 sub", Keeps::NotFileChange),
        ("chmod -h 777 link-in", Keeps::NotFileChange),
        ("chmod 600 -R sub", Keeps::NotFileChange),
        ("chmod 600 sub -L", Keeps::NotFileChange),
        ("chmod +x *.sh", Keeps::Uncertain),
        ("truncate log", Keeps::NotFileChange),
    ] {
        assert_eq!(f.judged(command), Err(keeps), "{command}");
    }
    assert_eq!(f.judged(&format!("rm {}", "a ".repeat(6_000))), Err(Keeps::Unreadable), "too long to read");
}

// BSD sed, the one macOS ships, takes the word after `-i` as the backup suffix, stops reading
// options at its first operand and skips a delimiter inside `[...]`; GNU sed does none of these.
#[test]
fn sed_only_substitutes_or_deletes_lines_of_files_that_exist() {
    let f = Fixture::new();
    for (command, keeps) in [
        ("sed 's/a/b/' log", Keeps::NotFileChange),
        ("sed -i 's/a/b/' log", Keeps::Uncertain),
        ("sed -i 's/a/b/' 'w ../outside/pwned' log", Keeps::Uncertain),
        ("sed -i -e 's/a/b/' log", Keeps::Uncertain),
        ("sed -i ./../x 's/a/b/' log", Keeps::Uncertain),
        ("sed -i'../x/*' 's/a/b/' log", Keeps::Uncertain),
        ("sed -i '' -e 's/a/b/' log -e '/x/d'", Keeps::Uncertain),
        ("sed -i '' 's/[/]/;s|x|/w ../outside/br|' log", Keeps::NotFileChange),
        ("sed -i '' 's/[0-9]/x/' log", Keeps::NotFileChange),
        ("sed -i '' 's/a/b/w /tmp/x' log", Keeps::NotFileChange),
        ("sed -i '' 's/a/b/e' log", Keeps::NotFileChange),
        ("sed -i '' '1e date' log", Keeps::NotFileChange),
        ("sed -i '' 'w /tmp/x' log", Keeps::NotFileChange),
        ("sed -i '' 'r /etc/hosts' log", Keeps::NotFileChange),
        ("sed -i '' -f script.sed log", Keeps::NotFileChange),
        ("sed --follow-symlinks -i '' 's/a/b/' log", Keeps::NotFileChange),
        ("sed -i '' 's/a/b/' missing", Keeps::Uncertain),
        ("sed -i '' 's/a/b/' sub", Keeps::Uncertain),
        ("sed -i '' 's/a/b/' pipe", Keeps::Uncertain),
        ("sed -i '' 's/a/b/' *.rs", Keeps::Uncertain),
        ("sed -i '' 's/a/b/'", Keeps::Uncertain),
        ("sed -i '' 's/a/b/' ../outside/secret", Keeps::Outside),
    ] {
        assert_eq!(f.judged(command), Err(keeps), "{command}");
    }
}

#[test]
fn git_changes_only_paths_and_keeps_asking_for_everything_else() {
    let f = Fixture::new();
    let forced_push = ["git", "push", "-f", "origin", "main"].join(" ");
    for (command, keeps) in [
        ("git checkout --theirs src/a.rs", Keeps::NotFileChange),
        ("git checkout -f", Keeps::NotFileChange),
        ("git checkout main", Keeps::NotFileChange),
        ("git checkout -b x -- src/a.rs", Keeps::NotFileChange),
        ("git checkout --", Keeps::NotFileChange),
        ("git checkout -- ':(top)src'", Keeps::Uncertain),
        ("git checkout -- '*.rs'", Keeps::Uncertain),
        ("git -C ../main checkout -- f", Keeps::NotFileChange),
        ("git restore --source=$(x) a", Keeps::Unreadable),
        ("git restore --source=-x a", Keeps::NotFileChange),
        ("git restore --patch a", Keeps::NotFileChange),
        ("git clean -fdx", Keeps::NotFileChange),
        ("git reset --hard", Keeps::NotFileChange),
        ("git stash drop", Keeps::NotFileChange),
        ("git branch -D x", Keeps::NotFileChange),
        (forced_push.as_str(), Keeps::NotFileChange),
        ("git rm src/a.rs", Keeps::NotFileChange),
        ("find . -delete", Keeps::NotFileChange),
        ("cp a b", Keeps::NotFileChange),
        ("ln -s /etc e", Keeps::NotFileChange),
        ("perl -pi -e 's/a/b/' log", Keeps::NotFileChange),
        ("shred log", Keeps::NotFileChange),
    ] {
        assert_eq!(f.judged(command), Err(keeps), "{command}");
    }
}

#[test]
fn checking_out_paths_asks_when_the_repository_has_a_post_checkout_hook() {
    let f = Fixture::new();
    assert_eq!(f.judged("git checkout --theirs -- src/a.rs"), Ok(()));
    std::fs::write(f.at("hooks/post-checkout"), "#!/bin/sh\n").unwrap();
    assert_eq!(f.judged("git checkout --theirs -- src/a.rs"), Err(Keeps::Uncertain), "git would run the hook");
    assert_eq!(f.judged("git restore src/a.rs"), Err(Keeps::Uncertain));
    assert_eq!(f.judged("rm src/a.rs"), Ok(()), "only git runs it");
}

// A repository inside the worktree, or a `.git` pointed at a git folder the node wrote, would have
// git read a configuration the node chose, such as a `core.fsmonitor` command it runs.
#[test]
fn git_runs_only_with_the_worktrees_own_repository() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.at("wt/.scratch/n")).unwrap();
    git(&f.at("wt/.scratch/n"), &["init", "-q"]);
    std::fs::write(f.at("wt/.scratch/n/f"), "a\n").unwrap();
    assert_eq!(f.judged_in("wt/.scratch/n", "git checkout -- f"), Err(Keeps::Uncertain), "a repository nested in the worktree");
    assert_eq!(f.judged_in("wt/.scratch/n", "rm f"), Ok(()), "its files are still the worktree's");
    std::fs::write(f.at("wt/.git"), format!("gitdir: {}\n", f.at("wt/.scratch/n/.git").display())).unwrap();
    assert_eq!(f.judged("git checkout -- src/a.rs"), Err(Keeps::Uncertain), "a git folder inside the worktree");
}

#[test]
fn sed_scripts_are_read_command_by_command() {
    for script in ["s/a/b/", "s|a/b|c|g", "s/a\\/b/c/2p", "/^$/d", "1d", "$d", "1,3d", "/x/,/y/s/a/b/I", "s/a/b/; s/c/d/g", "s/;/,/g", "s/a/b/\n2d"] {
        assert!(safe_sed_script(script), "{script}");
    }
    for script in ["", ";", "s/a/b", "s/a/b/w x", "s/a/b/ w x", "s/a/b/e", "e date", "1e date", "w x", "r x", "p", "y/a/b/", "s\\a\\b\\", "sxaxbx",
        "1,d", "s/a/b/;w x", "{s/a/b/}", "s/a/b\\", "s/[/]/;s|x|/w y|", "s/[0-9]/x/"] {
        assert!(!safe_sed_script(script), "{script}");
    }
}
