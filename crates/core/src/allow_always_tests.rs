use super::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

const WORKTREE: &str = "/repo/.claude/worktrees/t-1";
const FILES: &str = "/home/u/.alondra/files";

fn place(mode: Mode) -> Place<'static> {
    Place { cwd: Path::new(WORKTREE), worktree: Path::new(WORKTREE), files: Path::new(FILES), mode }
}

fn rules(command: &str) -> Result<Vec<String>, Refusal> {
    rules_for(command, &place(Mode::Manual))
}

fn rules_in(command: &str, mode: Mode) -> Result<Vec<String>, Refusal> {
    rules_for(command, &place(mode))
}

fn ok(command: &str) -> Vec<String> {
    rules(command).unwrap_or_else(|refusal| panic!("{command}: {refusal:?}"))
}

fn why(command: &str) -> Why {
    match rules(command) {
        Ok(rules) => panic!("{command} got {rules:?}"),
        Err(refusal) => refusal.why,
    }
}

#[test]
fn a_program_and_its_subcommand_get_one_rule() {
    assert_eq!(ok("make build"), ["make build *"]);
    assert_eq!(ok("make build -j8"), ["make build *"]);
    assert_eq!(ok("bazelisk test //crates/..."), ["bazelisk test *"]);
}

#[test]
fn read_only_parts_and_descriptor_redirections_need_no_rule() {
    assert_eq!(ok("make build 2>&1 | tail -1"), ["make build *"]);
    assert_eq!(ok("make build 2>/dev/null >/dev/null"), ["make build *"]);
    assert_eq!(ok("make mac-snapshots-record 2>&1 | tail -1; git status --short"), ["make mac-snapshots-record *"]);
}

#[test]
fn a_compound_gets_one_rule_per_part_sorted() {
    assert_eq!(ok("cargo test -p x; git status --short"), ["cargo test *"]);
    assert_eq!(ok("cargo build -p x 2>&1 | tail -3 && cargo test -p x"), ["cargo build *", "cargo test *"]);
    assert_eq!(ok("make test; make build; make test"), ["make build *", "make test *"]);
}

#[test]
fn wrappers_and_harmless_variables_are_stripped_as_claude_code_does() {
    assert_eq!(ok("timeout 60 make build"), ["make build *"]);
    assert_eq!(ok("timeout --signal=KILL 1.5m make build"), ["make build *"]);
    assert_eq!(ok("NODE_ENV=test make build"), ["make build *"]);
    assert_eq!(ok("RUST_LOG=debug nice -n 5 time cargo test"), ["cargo test *"]);
    assert_eq!(why("FOO=1 make build"), Why::Unreadable);
    assert_eq!(why("timeout make build"), Why::Unreadable);
}

#[test]
fn a_program_without_a_subcommand_gets_no_rule() {
    for command in ["shasum -a 256 docs/x.md", "./tools/run x", "sed -n 1,5p f", "make -C sub build", "sleep 5"] {
        assert_eq!(why(command), Why::NoSubcommand, "{command}");
    }
}

#[test]
fn a_destructive_part_alone_or_in_a_compound_gets_no_rule() {
    let destructive = [
        "rm -rf out", "rmdir x", "git reset --hard", "git reset -q --hard", "git branch -D x", "git push --force",
        "git push -f", "git push --force-with-lease", "git clean -fd", "git checkout -- .", "chmod -R u+w .",
        "bazel clean", "cargo clean", "find . -delete", "xargs rm", "sed -i '' s/a/b/ f",
    ];
    for part in destructive {
        for command in [part.to_string(), format!("make build && {part}"), format!("make build; {part}; git status")] {
            let refusal = rules(&command).expect_err(&command);
            assert!(matches!(refusal.why, Why::Deletes | Why::Git | Why::RunsAnything), "{command}: {refusal:?}");
            assert!(!refusal.part.is_empty(), "{command}: the card names what keeps asking");
        }
    }
    assert_eq!(rules("make build && rm -rf out"), Err(Refusal::new("rm -rf out", Why::Deletes)));
}

#[test]
fn paths_outside_the_worktree_get_no_rule() {
    assert_eq!(why("make build > /tmp/x"), Why::Outside);
    assert_eq!(why("make build < ~/x"), Why::Unreadable);
    assert_eq!(why("make build ../../otro"), Why::Outside);
    assert_eq!(why("ls ../../otro"), Why::Outside);
    assert_eq!(why("make build OUT=/etc/x"), Why::Outside);
    assert_eq!(ok("make build ../t-1/sub"), ["make build *"]);
    assert_eq!(ok(&format!("make build OUT={FILES}/report.md")), ["make build *"]);
    assert_eq!(ok(&format!("make build {WORKTREE}/sub")), ["make build *"]);
    assert_eq!(why("make build > ../../../out/x.log"), Why::Outside, "the main checkout is outside the worktree");
}

#[test]
fn a_file_write_needs_a_mode_that_approves_it() {
    assert_eq!(why("make build > out/log.txt"), Why::WritesFile);
    assert_eq!(rules_in("make build > out/log.txt", Mode::AcceptEdits).unwrap(), ["make build *"]);
    assert_eq!(rules_in("make build > out/log.txt", Mode::Auto).unwrap(), ["make build *"]);
    assert_eq!(rules_in("make build | tee out/t.log", Mode::AcceptEdits).unwrap(), ["make build *"]);
    assert_eq!(why("make build | tee out/t.log"), Why::WritesFile);
    assert_eq!(rules_in("make build > /tmp/x.log", Mode::AcceptEdits).unwrap_err().why, Why::Outside);
    assert_eq!(rules_in("cd sub && make build > out.log", Mode::AcceptEdits).unwrap_err().why, Why::ChangesDirectory);
    assert_eq!(why("cp bazel-bin/x apple/y/"), Why::Deletes, "copies are excluded even inside the repo");
}

#[test]
fn syntax_whose_effect_the_words_do_not_show_gets_no_rule() {
    for command in [
        "make $X", "echo $(make build)", "echo `make build`", "(make build)", "{ make build; }", "make build &",
        "make build &&", "make build ||", "make build |", "cat <<EOF\nx\nEOF", "ma*e build", "J=x; cp a $J",
        "J=out; make build > $J/x.log", "make build # note", "for i in 1 2; do make build; done", "diff <(ls) <(ls)",
        "echo \"$HOME\"", "!make",
    ] {
        assert!(rules(command).is_err(), "{command}");
    }
}

#[test]
fn runners_and_interpreters_get_no_rule() {
    for command in ["npx x", "bash -c 'x'", "python3 x.py", "docker exec c ls", "env X=1 make", "node x.js", "uvx tool", "pnpm dlx x"] {
        assert_eq!(why(command), Why::RunsAnything, "{command}");
    }
    assert_eq!(why("git -C ../otro status"), Why::Git);
    assert_eq!(why("git -c core.x=1 diff"), Why::Git);
    assert_eq!(why("curl https://example.com"), Why::System);
    assert_eq!(why("gh release create v1"), Why::System);
    assert_eq!(why("alondra status"), Why::Alondra);
    assert_eq!(ok("npm run build"), ["npm run *"]);
    assert_eq!(rules_in("npm run build", Mode::Auto).unwrap_err().why, Why::AutoIgnores);
}

#[test]
fn a_directory_change_is_read_only_once_and_never_before_git() {
    assert_eq!(ok("cd sub && make build"), ["make build *"]);
    assert_eq!(why("cd sub && git status"), Why::ChangesDirectory);
    assert_eq!(why("cd a && cd b && make build"), Why::ChangesDirectory);
    assert_eq!(why("cd /tmp && make build"), Why::Outside);
}

#[test]
fn rules_are_never_wide() {
    for command in ["make build", "cargo test -p x; make lint", "NODE_ENV=x npm test", "go test ./..."] {
        for rule in ok(command) {
            let head = rule.strip_suffix(" *").expect("ends with ` *`");
            assert!(!head.contains('*'), "{rule}");
            assert_eq!(head.split(' ').count(), 2, "{rule}: program and subcommand");
        }
    }
    assert_eq!(why("make"), Why::NoSubcommand);
}

#[test]
fn more_rules_than_claude_code_keeps_get_none() {
    assert_eq!(ok("make a; make b; make c; make d; make e").len(), 5);
    assert_eq!(why("make a; make b; make c; make d; make e; make f"), Why::TooMany);
    assert_eq!(why(&format!("make build {}", "x".repeat(MAX_COMMAND_CHARS))), Why::TooLong);
}

#[test]
fn claude_code_suggestions_pass_only_when_every_rule_is_one_alondra_builds() {
    let suggestion = |rules: &[&str], destination: &str| serde_json::json!({
        "type": "addRules", "behavior": "allow", "destination": destination,
        "rules": rules.iter().map(|r| serde_json::json!({"toolName": "Bash", "ruleContent": r})).collect::<Vec<_>>(),
    });
    assert!(suggestion_passes(&suggestion(&["make build *"], "userSettings"), Mode::Manual));
    assert!(!suggestion_passes(&suggestion(&["rm -rf build *"], "localSettings"), Mode::Manual));
    assert!(!suggestion_passes(&suggestion(&["make build *", "rm -rf out"], "localSettings"), Mode::Manual));
    assert!(!suggestion_passes(&suggestion(&["git reset *"], "localSettings"), Mode::Manual));
    assert!(!suggestion_passes(&suggestion(&["timeout 60 make build"], "localSettings"), Mode::Manual), "exact rules are not built");
    assert!(!suggestion_passes(&suggestion(&["make *"], "localSettings"), Mode::Manual));
    assert!(!suggestion_passes(&suggestion(&[], "localSettings"), Mode::Manual));
    let update = permission_update(&["make build *".into()]);
    assert_eq!(update["destination"], "localSettings", "the update Alondra hands back always stays in the repo's local settings");
    assert_eq!(update["rules"][0], serde_json::json!({"toolName": "Bash", "ruleContent": "make build *"}));
}

#[test]
fn everything_the_destructive_mark_flags_the_builder_refuses() {
    let marked = [
        "git worktree remove --force x", "git branch -D feature", "rm -rf build", "cd x && rm file", "git reset --hard origin/main",
        "git push --force-with-lease", "git push -f origin main", "git clean -fdx", "brew upgrade --force node", "cp a b",
        "mv a b", "chmod -R u+w .", "bazel clean --expunge", "git checkout -- .", "git restore x", "git stash drop",
        "find . -name x -exec rm {} \\;", "sudo rm -rf /", "echo $(rm x)", "xargs -0 rm", "sed -i '' s/a/b/ f",
        "git reflog expire --all", "make build -D X",
    ];
    for command in marked {
        assert!(!risks(command).is_empty(), "{command} is marked");
        for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Auto] {
            assert!(rules_in(command, mode).is_err(), "{command} gets no rule in {mode:?}");
        }
    }
    for command in ["git push origin main", "git branch -d merged", "cargo test --release", "git clean -n", "ls -f", "git rm old.txt"] {
        assert!(risks(command).is_empty(), "{command} is not marked");
    }
}

#[test]
fn claude_code_fills_in_only_the_parts_alondra_cannot_name() {
    let suggests = |rules: &[&str]| serde_json::json!([{
        "type": "addRules", "behavior": "allow", "destination": "localSettings",
        "rules": rules.iter().map(|r| serde_json::json!({"toolName": "Bash", "ruleContent": r})).collect::<Vec<_>>(),
    }]);
    let manual = place(Mode::Manual);
    assert_eq!(offer("make build; sort -n x", &manual, &suggests(&["make build *"])).unwrap(), ["make build *"],
        "Claude Code asked only for `make build`, so `sort` does not ask");
    assert_eq!(offer("make build; sort -n x", &manual, &serde_json::Value::Null).unwrap_err().why, Why::NoSubcommand);
    assert_eq!(offer("make build; sort -o out x", &manual, &suggests(&["make build *", "sort -o out x"])).unwrap_err().why,
        Why::NoSubcommand, "an exact rule throws the whole suggestion away");
    assert_eq!(offer("make build && rm -rf out", &manual, &suggests(&["make build *"])).unwrap_err().why, Why::Deletes,
        "a suggestion never outweighs what always asks");
    assert_eq!(offer("make build ../../x; sort -n x", &manual, &suggests(&["make build *"])).unwrap_err().why, Why::Outside);
    assert_eq!(offer("make build", &manual, &suggests(&["make *"])).unwrap(), ["make build *"], "Alondra's own rules win when it has them");
    assert_eq!(offer("sort -n x", &manual, &suggests(&["make a *", "make b *", "make c *", "make d *", "make e *"])).unwrap_err().why,
        Why::NoSubcommand, "a suggestion at Claude Code's limit may have been cut");
}

// ---------------------------------------------------------------------------------------------
// Properties.

const SAFE_PARTS: &[&str] = &[
    "make build", "make test -j2", "cargo test -p x", "git status --short", "tail -1", "ls", "bazelisk test //x:y",
    "NODE_ENV=test npm test", "timeout 60 make lint", "grep -n foo src/x.rs",
];
const DESTRUCTIVE_PARTS: &[&str] = &[
    "rm -rf out", "rmdir x", "git reset -q --hard HEAD", "git branch -D x", "git push -f", "git clean -fd",
    "git checkout -- .", "chmod -R u+w .", "bazel clean", "cargo clean", "find . -delete", "xargs rm", "mv a b", "cp a b",
    "sed -i s/a/b/ f", "git stash drop", "unlink f", "dd if=a of=b",
];
const SEPARATORS: &[&str] = &[" && ", " || ", "; ", " | ", "\n", " |& "];
const ODD: &[&str] = &["$X", "$(ls)", "`ls`", "(", ")", "${X}"];

fn runner() -> TestRunner {
    TestRunner::new_with_rng(Config { cases: 512, ..Config::default() }, TestRng::deterministic_rng(RngAlgorithm::ChaCha))
}

fn compound(parts: Vec<&'static str>, separators: Vec<&'static str>) -> String {
    let mut command = String::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            command.push_str(separators[index % separators.len()]);
        }
        command.push_str(part);
    }
    command
}

fn strategy(extra: &'static [&'static str]) -> impl Strategy<Value = (String, usize)> {
    (prop::collection::vec(prop::sample::select(SAFE_PARTS), 0..5), prop::sample::select(extra), 0usize..6,
        prop::collection::vec(prop::sample::select(SEPARATORS), 1..4))
        .prop_map(|(mut parts, odd, at, separators)| {
            let at = at.min(parts.len());
            parts.insert(at, odd);
            (compound(parts, separators), at)
        })
}

#[test]
fn a_destructive_part_anywhere_means_no_rules() {
    runner().run(&strategy(DESTRUCTIVE_PARTS), |(command, _)| {
        for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Auto] {
            prop_assert!(rules_in(&command, mode).is_err(), "{command:?} in {mode:?}");
        }
        Ok(())
    }).unwrap();
}

#[test]
fn expansions_substitutions_and_parentheses_mean_no_rules() {
    runner().run(&strategy(ODD).prop_map(|(command, _)| command.replacen("make build", "make build $X", 1)), |command| {
        prop_assert!(rules(&command).is_err(), "{command:?}");
        Ok(())
    }).unwrap();
    runner().run(&(prop::sample::select(SAFE_PARTS), prop::sample::select(ODD)), |(part, odd)| {
        let command = format!("{part} {odd}");
        prop_assert!(rules(&command).is_err(), "{command:?}");
        Ok(())
    }).unwrap();
}

#[test]
fn every_rule_that_comes_out_is_narrow_and_allowed() {
    let words = prop::sample::select(&["make", "cargo", "npm", "go", "rm", "git", "bazel", "python3", "docker", "gh", "swift"][..]);
    let subs = prop::sample::select(&["build", "test", "clean", "reset", "run", "push", "exec", "release", "status", "*", "-x", "lint"][..]);
    let parts = prop::collection::vec((words, subs, prop::sample::select(&["", " -v", " ./x", " --force", " 2>&1"][..])), 1..5)
        .prop_map(|parts| parts.into_iter().map(|(p, s, a)| format!("{p} {s}{a}")).collect::<Vec<_>>().join("; "));
    runner().run(&parts, |command| {
        for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Auto] {
            if let Ok(rules) = rules_in(&command, mode) {
                for rule in &rules {
                    let head = rule.strip_suffix(" *").expect("ends with ` *`");
                    let mut split = head.split(' ');
                    let (program, sub) = (split.next().unwrap(), split.next().unwrap());
                    prop_assert!(split.next().is_none() && !head.contains('*'), "{rule}");
                    prop_assert!(!sub.starts_with('-'), "{rule}");
                    prop_assert!(rule_passes(rule, mode), "{rule} from {command:?} passes its own filter");
                    prop_assert!(excluded(&[Word::plain(program), Word::plain(sub)], mode).is_none(), "{rule}");
                }
            }
        }
        Ok(())
    }).unwrap();
}

#[test]
fn rules_are_deterministic_and_ignore_the_order_of_repeated_parts() {
    let parts = prop::collection::vec(prop::sample::select(SAFE_PARTS), 1..6);
    runner().run(&(parts, prop::sample::select(SEPARATORS)), |(mut parts, separator)| {
        let first = rules(&parts.join(separator));
        prop_assert_eq!(&first, &rules(&parts.join(separator)));
        let mut doubled = parts.clone();
        doubled.extend(parts.iter().rev());
        parts.reverse();
        prop_assert_eq!(&first, &rules(&parts.join(separator)));
        prop_assert_eq!(&first, &rules(&doubled.join(separator)));
        Ok(())
    }).unwrap();
}
