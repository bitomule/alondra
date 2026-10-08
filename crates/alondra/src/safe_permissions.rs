//! Permission prompts a node does not need to put to the user, because the plugin can prove the
//! command loses nothing: a `git reset --hard` in the node's own clean, fully pushed worktree, a
//! copy of a new file into Alondra's files folder, and copies inside the worktree that only
//! overwrite files git can restore. Anything the reader cannot follow keeps asking.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const READ_ONLY: &[&str] = &["grep", "head", "tail", "ls", "cat", "wc", "shasum", "echo", "pwd"];
const READ_ONLY_GIT: &[&str] = &["fetch", "log", "status", "rev-parse", "diff", "show"];

pub fn approve(payload: &Value, files_dir: &Path) -> bool {
    let field = |k: &str| payload.get(k).and_then(Value::as_str);
    if field("tool_name") != Some("Bash") {
        return false;
    }
    let (Some(command), Some(cwd)) = (payload.get("tool_input").and_then(|i| i.get("command")).and_then(Value::as_str), field("cwd")) else {
        return false;
    };
    let Some(segments) = segments(command) else {
        return false;
    };
    let Some(steps) = segments.iter().map(|words| step(words)).collect::<Option<Vec<_>>>() else {
        return false;
    };
    let cwd = Path::new(cwd);
    let Some(worktree) = own_worktree(cwd) else {
        return false;
    };
    let resets = steps.iter().any(|s| matches!(s, Step::Reset));
    let writes = steps.iter().any(|s| matches!(s, Step::Copy { .. } | Step::Chmod(_)));
    if resets && (writes || !loses_nothing(&worktree)) {
        return false;
    }
    steps.iter().all(|s| match s {
        Step::ReadOnly | Step::Reset => true,
        Step::Copy { sources, destination } => copy_targets(cwd, sources, destination)
            .is_some_and(|targets| targets.iter().all(|t| new_in(files_dir, t) || restorable_in(&worktree, t))),
        Step::Chmod(paths) => paths.iter().all(|p| chmod_inside(&worktree, &cwd.join(&p.text))),
    })
}

#[derive(Debug, PartialEq)]
struct Word {
    text: String,
    glob: bool,
}

enum Step {
    ReadOnly,
    Reset,
    Copy { sources: Vec<String>, destination: String },
    Chmod(Vec<Word>),
}

/// Splits a command at `&&`, `||`, `;`, `|` and newlines, honouring quotes, and refuses anything
/// that could run or write what the words do not show: expansions, substitutions, redirections,
/// subshells and background jobs.
fn segments(command: &str) -> Option<Vec<Vec<Word>>> {
    let mut all = vec![];
    let mut words = vec![];
    let mut word: Option<Word> = None;
    let mut chars = command.chars().peekable();
    fn push(word: &mut Option<Word>, c: char, glob: bool) {
        let w = word.get_or_insert(Word { text: String::new(), glob: false });
        w.text.push(c);
        w.glob |= glob;
    }
    fn end_segment(all: &mut Vec<Vec<Word>>, words: &mut Vec<Word>, word: &mut Option<Word>) {
        words.extend(word.take());
        if !words.is_empty() {
            all.push(std::mem::take(words));
        }
    }
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => words.extend(word.take()),
            ';' | '\n' => end_segment(&mut all, &mut words, &mut word),
            '|' => {
                chars.next_if_eq(&'|');
                end_segment(&mut all, &mut words, &mut word);
            }
            '&' => {
                chars.next_if_eq(&'&')?;
                end_segment(&mut all, &mut words, &mut word);
            }
            '\'' => {
                word.get_or_insert(Word { text: String::new(), glob: false });
                loop {
                    match chars.next()? {
                        '\'' => break,
                        q => push(&mut word, q, false),
                    }
                }
            }
            '"' => {
                word.get_or_insert(Word { text: String::new(), glob: false });
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => match chars.next()? {
                            e @ ('"' | '\\') => push(&mut word, e, false),
                            e => {
                                push(&mut word, '\\', false);
                                push(&mut word, e, false);
                            }
                        },
                        q => push(&mut word, q, false),
                    }
                }
            }
            '\\' => push(&mut word, chars.next()?, false),
            '$' | '`' | '>' | '<' | '(' | ')' | '{' | '}' | '#' | '~' | '!' => return None,
            '*' | '?' | '[' | ']' => push(&mut word, c, true),
            c => push(&mut word, c, false),
        }
    }
    end_segment(&mut all, &mut words, &mut word);
    Some(all)
}

fn step(words: &[Word]) -> Option<Step> {
    let (first, args) = words.split_first()?;
    if first.glob {
        return None;
    }
    let args: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
    match first.text.as_str() {
        "git" => git_step(&args),
        "cp" => {
            let (destination, sources) = args.split_last()?;
            let plain = !sources.is_empty() && words[1..].iter().all(|w| !w.glob && !w.text.starts_with('-'));
            plain.then(|| Step::Copy { sources: sources.iter().map(|s| s.to_string()).collect(), destination: destination.to_string() })
        }
        "chmod" if args.first() == Some(&"u+w") && args.len() > 1 => {
            let paths: Vec<Word> = words[2..].iter().map(|w| Word { text: w.text.clone(), glob: w.glob }).collect();
            paths.iter().all(|p| !p.text.starts_with('-')).then_some(Step::Chmod(paths))
        }
        name if READ_ONLY.contains(&name) => Some(Step::ReadOnly),
        _ => None,
    }
}

fn git_step(args: &[&str]) -> Option<Step> {
    let (sub, rest) = args.split_first()?;
    if rest.iter().any(|a| a.starts_with("--output")) {
        return None;
    }
    match *sub {
        "reset" => {
            let refs: Vec<&&str> = rest.iter().filter(|a| !matches!(**a, "-q" | "--quiet" | "--hard")).collect();
            let plain = rest.contains(&"--hard") && refs.len() <= 1 && refs.iter().all(|r| !r.starts_with('-'));
            plain.then_some(Step::Reset)
        }
        "fetch" if rest.iter().any(|a| a.starts_with("--prune") || *a == "-p" || a.starts_with("--force") || *a == "-f" || a.contains(':')) => None,
        sub if READ_ONLY_GIT.contains(&sub) => Some(Step::ReadOnly),
        _ => None,
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_OPTIONAL_LOCKS", "0").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The node's own worktree: a linked worktree under `<repo>/.claude/worktrees/`, never the
/// repository's main checkout.
fn own_worktree(cwd: &Path) -> Option<PathBuf> {
    let out = git(cwd, &["rev-parse", "--show-toplevel", "--git-dir", "--git-common-dir"])?;
    let mut lines = out.lines().map(|l| cwd.join(l).canonicalize());
    let (top, git_dir, common) = (lines.next()?.ok()?, lines.next()?.ok()?, lines.next()?.ok()?);
    let repo = common.parent()?;
    (git_dir != common && top.starts_with(repo.join(".claude").join("worktrees"))).then_some(top)
}

fn loses_nothing(worktree: &Path) -> bool {
    git(worktree, &["status", "--porcelain"]).is_some_and(|s| s.trim().is_empty())
        && git(worktree, &["rev-list", "HEAD", "--not", "--remotes"]).is_some_and(|s| s.trim().is_empty())
}

fn copy_targets(cwd: &Path, sources: &[String], destination: &str) -> Option<Vec<PathBuf>> {
    let destination = cwd.join(destination);
    if destination.is_dir() || destination.as_os_str().to_string_lossy().ends_with('/') {
        sources.iter().map(|s| Some(destination.join(Path::new(s).file_name()?))).collect()
    } else {
        (sources.len() == 1).then(|| vec![destination])
    }
}

/// The real folder a path would land in, refusing `..` and a path that is itself a symlink.
fn real_parent(path: &Path) -> Option<PathBuf> {
    if path.components().any(|c| c == Component::ParentDir) || path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        return None;
    }
    path.parent()?.canonicalize().ok()
}

fn new_in(folder: &Path, target: &Path) -> bool {
    let Ok(folder) = folder.canonicalize() else {
        return false;
    };
    real_parent(target).is_some_and(|p| p == folder) && !target.exists()
}

/// A target inside the worktree that is new, or tracked and unchanged, so git can restore it.
fn restorable_in(worktree: &Path, target: &Path) -> bool {
    let Some(parent) = real_parent(target).filter(|p| p.starts_with(worktree)) else {
        return false;
    };
    if !target.exists() {
        return true;
    }
    let Some(file) = target.file_name() else {
        return false;
    };
    let relative = parent.join(file);
    let Ok(relative) = relative.strip_prefix(worktree) else {
        return false;
    };
    let Some(relative) = relative.to_str() else {
        return false;
    };
    git(worktree, &["ls-files", "--error-unmatch", "--", relative]).is_some()
        && git(worktree, &["status", "--porcelain", "--", relative]).is_some_and(|s| s.trim().is_empty())
}

fn chmod_inside(worktree: &Path, path: &Path) -> bool {
    let parent_is_plain = path.parent().is_some_and(|p| !p.to_string_lossy().contains(['*', '?', '[', ']']));
    parent_is_plain && path.components().all(|c| c != Component::ParentDir) && path.parent().and_then(|p| p.canonicalize().ok()).is_some_and(|p| p.starts_with(worktree))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Repo {
        root: PathBuf,
        main: PathBuf,
        worktree: PathBuf,
        files: PathBuf,
    }

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn repo() -> Repo {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!("alondra-safe-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        run(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        run(&root, &["init", "-q", "-b", "main", "repo"]);
        let main = root.join("repo");
        std::fs::create_dir_all(main.join("snaps")).unwrap();
        std::fs::write(main.join("a.txt"), "a").unwrap();
        std::fs::write(main.join("snaps/shot.png"), "old").unwrap();
        run(&main, &["add", "."]);
        run(&main, &["commit", "-q", "-m", "first"]);
        run(&main, &["remote", "add", "origin", "../remote.git"]);
        run(&main, &["push", "-q", "-u", "origin", "main"]);
        run(&main, &["worktree", "add", "-q", "-b", "wx", ".claude/worktrees/x", "origin/main"]);
        let files = root.join("files");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(root.join("new.png"), "new").unwrap();
        Repo { worktree: main.join(".claude/worktrees/x"), main, files, root }
    }

    fn payload(cwd: &Path, command: &str) -> Value {
        json!({"session_id": "s", "hook_event_name": "PermissionRequest", "tool_name": "Bash",
               "cwd": cwd.to_str().unwrap(), "tool_input": {"command": command}})
    }

    fn approves(repo: &Repo, command: &str) -> bool {
        approve(&payload(&repo.worktree, command), &repo.files)
    }

    #[test]
    fn a_reset_in_a_fresh_clean_worktree_is_approved() {
        let r = repo();
        for command in [
            "git fetch -q origin main; git reset -q --hard origin/main; git status -sb | head -2",
            "git reset -q --hard origin/main && git log --oneline -1 && grep -n -i \"group\\|Private Pilot\" a.txt",
            "git fetch -q origin main && git reset -q --hard origin/main && git log --oneline -1 && grep -rn \"present_result\\|is_final\" . --include=*.rs -l | head -20",
            "git reset --hard",
        ] {
            assert!(approves(&r, command), "{command}");
        }
        let head = git(&r.worktree, &["rev-parse", "HEAD"]).unwrap();
        assert!(approves(&r, &format!("git reset -q --hard {} && git status --short && git log --oneline -1", head.trim())));
    }

    #[test]
    fn a_reset_with_uncommitted_changes_keeps_asking() {
        let r = repo();
        std::fs::write(r.worktree.join("a.txt"), "changed").unwrap();
        assert!(!approves(&r, "git reset -q --hard origin/main"));
    }

    #[test]
    fn a_reset_with_an_untracked_file_keeps_asking() {
        let r = repo();
        std::fs::write(r.worktree.join("draft.md"), "notes").unwrap();
        assert!(!approves(&r, "git reset -q --hard origin/main"));
    }

    #[test]
    fn a_reset_with_an_unpushed_commit_keeps_asking() {
        let r = repo();
        std::fs::write(r.worktree.join("a.txt"), "work").unwrap();
        run(&r.worktree, &["commit", "-q", "-am", "work"]);
        assert!(!approves(&r, "git reset -q --hard origin/main"));
    }

    #[test]
    fn a_reset_in_the_main_checkout_keeps_asking() {
        let r = repo();
        assert!(!approve(&payload(&r.main, "git reset -q --hard origin/main"), &r.files));
    }

    #[test]
    fn a_reset_next_to_anything_unknown_keeps_asking() {
        let r = repo();
        for command in [
            "cd .. && git reset --hard origin/main",
            "git -C .. reset --hard origin/main",
            "git reset --hard origin/main > out.txt",
            "git reset --hard $(git rev-parse origin/main)",
            "git reset --hard origin/main; ls | xargs rm",
            "git reset --hard origin/main && rm -rf build",
            "git reset --hard origin/main -- a.txt",
            "git reset --hard origin/main &",
            "git fetch --prune && git reset --hard origin/main",
            "git reset --hard origin/main; cp ../../../../new.png snaps/shot.png",
        ] {
            assert!(!approves(&r, command), "{command}");
        }
    }

    #[test]
    fn a_new_file_copied_into_alondras_files_folder_is_approved() {
        let r = repo();
        let command = format!(
            "cp {plan} {files}/plan.md; shasum -a 256 {files}/plan.md; wc -c {files}/plan.md",
            plan = r.root.join("new.png").display(),
            files = r.files.display()
        );
        assert!(approves(&r, &command));
    }

    #[test]
    fn a_copy_over_a_file_already_in_alondras_files_folder_keeps_asking() {
        let r = repo();
        std::fs::write(r.files.join("plan.md"), "reviewed").unwrap();
        assert!(!approves(&r, &format!("cp {} {}/plan.md", r.root.join("new.png").display(), r.files.display())));
        assert!(!approves(&r, &format!("cp {} {}/plan.md", r.root.join("new.png").display(), r.root.display())));
    }

    #[test]
    fn a_copy_over_a_clean_tracked_file_in_the_worktree_is_approved() {
        let r = repo();
        std::fs::write(r.root.join("shot.png"), "recorded").unwrap();
        assert!(approves(&r, &format!("cp {} snaps/; chmod u+w snaps/*.png", r.root.join("shot.png").display())));
        assert!(approves(&r, &format!("cp {} snaps/fresh.png", r.root.join("new.png").display())));
    }

    #[test]
    fn a_copy_over_a_file_with_uncommitted_changes_keeps_asking() {
        let r = repo();
        std::fs::write(r.worktree.join("snaps/shot.png"), "edited").unwrap();
        assert!(!approves(&r, &format!("cp {} snaps/shot.png", r.root.join("new.png").display())));
    }

    #[test]
    fn a_copy_out_of_the_worktree_keeps_asking() {
        let r = repo();
        std::os::unix::fs::symlink(&r.root, r.worktree.join("out")).unwrap();
        for command in [
            format!("cp a.txt {}/copy.txt", r.root.display()),
            "cp a.txt out/copy.txt".to_string(),
            "cp a.txt ../copy.txt".to_string(),
            "cp -f a.txt snaps/b.txt".to_string(),
            "cp *.txt snaps/".to_string(),
            format!("chmod u+w {}/new.png", r.root.display()),
            "chmod -R u+w .".to_string(),
        ] {
            assert!(!approves(&r, &command), "{command}");
        }
    }

    #[test]
    fn the_inventory_commands_that_were_not_safe_keep_asking() {
        let r = repo();
        for command in [
            "grep -v '\\.swift$' /tmp/revert.txt | xargs git checkout --",
            "bazelisk test //apple:Snapshots --test_output=errors 2>&1 | tail -1; cp bazel-testlogs/shot.png /tmp/ax5.png",
            "bazelisk test //apple:Snapshots | tail -1; cp bazel-testlogs/shot.png snaps/; chmod u+w snaps/*.png",
            "cp bazel-bin/tools/sandbox/alondra-sandbox /tmp/alondra-sbx-f0b/bin/alondra-sandbox.next",
            "rm -rf /Volumes/Backup/alondra-release-clone /Volumes/Backup/alondra-beta-0.1.2; ls /Volumes/Backup | grep -i alondra",
        ] {
            assert!(!approves(&r, command), "{command}");
        }
    }

    #[test]
    fn only_bash_with_a_cwd_is_considered() {
        let r = repo();
        let mut edit = payload(&r.worktree, "git reset --hard");
        edit["tool_name"] = json!("Write");
        assert!(!approve(&edit, &r.files));
        let mut no_cwd = payload(&r.worktree, "git reset --hard");
        no_cwd.as_object_mut().unwrap().remove("cwd");
        assert!(!approve(&no_cwd, &r.files));
    }

    #[test]
    fn only_a_session_alondra_launched_skips_the_card() {
        let r = repo();
        let safe = payload(&r.worktree, "git reset -q --hard origin/main");
        assert_eq!(crate::node::provably_safe(&safe, false, &r.files), None);
        assert_eq!(
            crate::node::provably_safe(&safe, true, &r.files),
            Some(json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}}))
        );
    }

    #[test]
    fn a_pipe_inside_quotes_does_not_split_the_command() {
        let words = segments("grep -rn \"a\\|b\" src | head").unwrap();
        assert_eq!(words.len(), 2);
        assert_eq!(words[0][2].text, "a\\|b");
        assert_eq!(segments("echo 'x $y'").unwrap()[0][1].text, "x $y");
        assert!(segments("echo \"$HOME\"").is_none());
    }
}
