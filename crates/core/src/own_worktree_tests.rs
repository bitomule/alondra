use super::*;
use crate::test_support::Directory;

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git").current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=Ana", "-c", "user.email=ana@example.com"]).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

/// The user's own checkout `main` and `wt`, a linked worktree of it that is the task's, with a
/// subagent's worktree nested in it and an `outside` folder that links in `wt` point to. The
/// repository's hooks live in `hooks`, empty.
struct Fixture {
    dir: Directory,
    roots: Vec<PathBuf>,
}

impl Fixture {
    fn new() -> Self {
        let dir = Directory::new();
        let at = |path: &str| dir.join(path);
        std::fs::create_dir_all(at("main")).unwrap();
        std::fs::create_dir_all(at("hooks")).unwrap();
        git(&at("main"), &["init", "-q", "-b", "main"]);
        git(&at("main"), &["commit", "-q", "--allow-empty", "-m", "Start"]);
        git(&at("main"), &["config", "core.hooksPath", &at("hooks").display().to_string()]);
        git(&at("main"), &["worktree", "add", "-q", "-b", "worktree-wt", &at("wt").display().to_string()]);
        for folder in ["outside/dir", "wt/.scratch/x", "wt/.scratch/empty", "wt/src", "wt/sub/deep", "wt/holder/nested"] {
            std::fs::create_dir_all(at(folder)).unwrap();
        }
        for file in ["outside/secret", "outside/shared", "outside/dir/f", "wt/.scratch/x/start", "wt/.scratch/a.log", "wt/src/a.rs", "wt/sub/f",
            "wt/sub/deep/g", "wt/s.sh", "wt/log", "wt/a", "main/f"] {
            std::fs::write(at(file), "a\n").unwrap();
        }
        std::fs::write(at("wt/holder/nested/.git"), "gitdir: /repo/.git/worktrees/nested\n").unwrap();
        std::os::unix::fs::symlink(at("outside"), at("wt/link-out")).unwrap();
        std::os::unix::fs::symlink(at("outside/secret"), at("wt/file-link-out")).unwrap();
        std::os::unix::fs::symlink(at("outside/missing"), at("wt/dangling")).unwrap();
        std::os::unix::fs::symlink(at("wt/sub"), at("wt/link-in")).unwrap();
        std::os::unix::fs::symlink(at("wt/sub/deep"), at("wt/deep-link")).unwrap();
        std::fs::hard_link(at("outside/shared"), at("wt/hard")).unwrap();
        let recorded = [at("wt"), at("wt/holder/nested"), at("main")].map(|p| p.display().to_string());
        let roots = roots(recorded.iter().map(String::as_str));
        Fixture { dir, roots }
    }

    fn path(&self, path: &str) -> String {
        std::fs::canonicalize(self.dir.join(path)).unwrap().display().to_string()
    }

    fn judged_in(&self, cwd: &str, command: &str) -> Result<(), Keeps> {
        judge(command, &self.path(cwd), &self.roots)
    }

    fn judged(&self, command: &str) -> Result<(), Keeps> {
        self.judged_in("wt", command)
    }
}

#[test]
fn only_linked_worktrees_count_as_the_tasks_own() {
    let f = Fixture::new();
    let own: Vec<String> = f.roots.iter().map(|p| p.display().to_string()).collect();
    assert_eq!(own, [f.path("wt"), f.path("wt/holder/nested")], "the user's own checkout, whose .git is a folder, never counts");
    assert_eq!(f.judged_in("main", "rm f"), Err(Keeps::Outside));
    assert_eq!(judge("rm .scratch/a.log", &f.path("wt"), &[]), Err(Keeps::Outside), "a task without worktrees asks");
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
        "sed -i -e 's|a|b|' -e '/^$/d' src/a.rs sub/f",
        "sed -i '' -E 's/(a)/\\1b/' .scratch/x/start",
        "sed --in-place 's/a/b/;2d' log",
        "git checkout --ours -- src/a.rs sub/f",
        "git checkout -- sub",
        "git restore --staged -- src/a.rs",
        "git restore --source=origin/main --worktree src/a.rs",
        "git restore -s HEAD~1 sub/f",
        "cd ./sub && rm f",
        "cd ./sub && git checkout -- .",
        "cd ./.scratch; rm -rf x",
        "cd ./link-in && rm -f ../a",
        "rm -f a 2>/dev/null",
        "rm -f a || true",
        "timeout 5 rm a",
        "LANG=C sed -i '' 's/a/b/' log",
    ] {
        assert_eq!(f.judged(command), Ok(()), "{command}");
    }
    assert_eq!(f.judged(&format!("cd {} && rm f", f.path("wt/sub"))), Ok(()));
}

#[test]
fn paths_that_resolve_outside_the_worktree_keep_asking() {
    let f = Fixture::new();
    let outside = f.path("outside/secret");
    for (command, keeps) in [
        (format!("rm {outside}"), Keeps::Outside),
        ("rm ../outside/secret".into(), Keeps::Outside),
        ("rm ../main/f".into(), Keeps::Outside),
        (format!("cd {} && rm secret", f.path("outside")), Keeps::Outside),
        ("cd ./link-out && rm secret".into(), Keeps::Outside),
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
        ("git checkout -- link-out/secret".into(), Keeps::Outside),
        ("rm nowhere/x".into(), Keeps::Uncertain),
        ("rm ..".into(), Keeps::Uncertain),
    ] {
        assert_eq!(f.judged(&command), Err(keeps), "{command}");
    }
    assert_eq!(judge("rm a", "sub", &f.roots), Err(Keeps::Outside), "a folder that is not absolute");
    assert_eq!(judge("rm a", &f.dir.join("wt/nowhere").display().to_string(), &f.roots), Err(Keeps::Uncertain));
}

#[test]
fn the_worktree_its_git_and_the_worktrees_it_holds_are_never_changed() {
    let f = Fixture::new();
    for (command, keeps) in [
        ("rm -rf .", Keeps::Protected),
        ("rm -rf ./", Keeps::Protected),
        ("cd ./sub && rm -rf ..", Keeps::Uncertain),
        ("rm -rf *", Keeps::Protected),
        ("rm -rf .*", Keeps::Uncertain),
        ("rm -rf */f", Keeps::Uncertain),
        ("rm .git", Keeps::Protected),
        ("cd ./.scratch && rm -rf ../.git", Keeps::Protected),
        ("rm .GIT", Keeps::Protected),
        ("mv sub .GIT", Keeps::Protected),
        ("rm -rf HOLDER", Keeps::Protected),
        ("rm -rf holder/NESTED", Keeps::Protected),
        ("rm -rf holder", Keeps::Protected),
        ("rm -rf holder/nested", Keeps::Protected),
        ("rm holder/nested/.git", Keeps::Protected),
        ("git checkout -- .", Keeps::Protected),
        ("mv sub .git", Keeps::Protected),
        ("chmod 700 .", Keeps::Protected),
    ] {
        assert_eq!(f.judged(command), Err(keeps), "{command}");
    }
    assert_eq!(f.judged_in("wt/holder/nested", "git checkout -- ../../sub/f"), Err(Keeps::Outside),
        "git runs in the innermost worktree, which does not hold the path");
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
        ("sudo rm a", Keeps::NotFileChange),
        ("env -C /tmp rm a", Keeps::NotFileChange),
        ("xargs rm", Keeps::NotFileChange),
        ("./rm a", Keeps::NotFileChange),
        ("/bin/rm a", Keeps::NotFileChange),
        ("rm a > out.txt", Keeps::NotFileChange),
        ("rm a < list", Keeps::NotFileChange),
        ("rm a && curl https://example.com", Keeps::NotFileChange),
        ("rm a | cat", Keeps::NotFileChange),
        ("cd", Keeps::NotFileChange),
        ("cd - && rm a", Keeps::Uncertain),
        ("cd .. && rm outside/secret", Keeps::Uncertain),
        ("cd ./deep-link/../.. && rm -rf main", Keeps::Uncertain),
        ("cd sub && rm f", Keeps::Uncertain),
        ("rm =node", Keeps::Unreadable),
        ("chmod 000 =node", Keeps::Unreadable),
        ("truncate -s 0 =node", Keeps::Unreadable),
        ("chmod 600 -R sub", Keeps::NotFileChange),
        ("chmod 600 sub -L", Keeps::NotFileChange),
        ("cd s* && rm a", Keeps::Uncertain),
        ("rm", Keeps::NotFileChange),
        ("rm --no-preserve-root a", Keeps::NotFileChange),
        ("rmdir -p .scratch/empty", Keeps::NotFileChange),
        ("mv -t sub a", Keeps::NotFileChange),
        ("mv *.rs sub", Keeps::Uncertain),
        ("rm sub/?", Keeps::Uncertain),
        ("rm su*/f", Keeps::Uncertain),
        ("chmod -R 777 sub", Keeps::NotFileChange),
        ("chmod -h 777 link-in", Keeps::NotFileChange),
        ("chmod +x *.sh", Keeps::Uncertain),
        ("truncate log", Keeps::NotFileChange),
    ] {
        assert_eq!(f.judged(command), Err(keeps), "{command}");
    }
    assert_eq!(f.judged(&format!("rm {}", "a ".repeat(6_000))), Err(Keeps::Unreadable), "too long to read");
}

#[test]
fn sed_only_substitutes_or_deletes_lines_of_files_that_exist() {
    let f = Fixture::new();
    for (command, keeps) in [
        ("sed 's/a/b/' log", Keeps::NotFileChange),
        ("sed -i 's/a/b/w /tmp/x' log", Keeps::NotFileChange),
        ("sed -i 's/a/b/e' log", Keeps::NotFileChange),
        ("sed -i '1e date' log", Keeps::NotFileChange),
        ("sed -i 'w /tmp/x' log", Keeps::NotFileChange),
        ("sed -i 'r /etc/hosts' log", Keeps::NotFileChange),
        ("sed -i -f script.sed log", Keeps::NotFileChange),
        ("sed -i 's/a/b/' missing", Keeps::Uncertain),
        ("sed -i 's/a/b/' sub", Keeps::Uncertain),
        ("sed -i 's/a/b/' *.rs", Keeps::Uncertain),
        ("sed -i 's/a/b/'", Keeps::Uncertain),
        ("sed -i'../x/*' 's/a/b/' log", Keeps::Uncertain),
        ("sed -i ./../x 's/a/b/' log", Keeps::Uncertain),
        ("sed -i 's/a/b/' ../outside/secret", Keeps::Outside),
        ("sed --follow-symlinks -i 's/a/b/' log", Keeps::NotFileChange),
    ] {
        assert_eq!(f.judged(command), Err(keeps), "{command}");
    }
}

#[test]
fn git_changes_only_paths_and_keeps_asking_for_everything_else() {
    let f = Fixture::new();
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
        ("git push -f origin main", Keeps::NotFileChange),
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
    std::fs::write(f.dir.join("hooks/post-checkout"), "#!/bin/sh\n").unwrap();
    assert_eq!(f.judged("git checkout --theirs -- src/a.rs"), Err(Keeps::Uncertain), "git would run the hook");
    assert_eq!(f.judged("git restore src/a.rs"), Err(Keeps::Uncertain));
    assert_eq!(f.judged("rm src/a.rs"), Ok(()), "only git runs it");
}

#[test]
fn sed_scripts_are_read_command_by_command() {
    for script in ["s/a/b/", "s|a/b|c|g", "s/a\\/b/c/2p", "/^$/d", "1d", "$d", "1,3d", "/x/,/y/s/a/b/I", "s/a/b/; s/c/d/g", "s/;/,/g", "s/a/b/\n2d"] {
        assert!(safe_sed_script(script), "{script}");
    }
    for script in ["", ";", "s/a/b", "s/a/b/w x", "s/a/b/ w x", "s/a/b/e", "e date", "1e date", "w x", "r x", "p", "y/a/b/", "s\\a\\b\\", "sxaxbx",
        "1,d", "s/a/b/;w x", "{s/a/b/}", "s/a/b\\"] {
        assert!(!safe_sed_script(script), "{script}");
    }
}
