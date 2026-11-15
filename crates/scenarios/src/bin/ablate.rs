//! `bazel run //crates/scenarios:ablate -- <case> --revert <file>...`: runs only that case without
//! the fix (the files put back as they were on the base of the branch) and with it, and prints the
//! block a pull request shows. The files are restored whatever happens.

use std::path::PathBuf;
use std::process::Command;

fn bazel(workspace: &PathBuf, case: &str) -> (bool, String) {
    let output = Command::new("bazelisk")
        .current_dir(workspace)
        .args(["test", "//crates/scenarios:scenarios_test", "--test_output=errors", "--test_arg"])
        .arg(case)
        .output()
        .expect("bazelisk runs");
    (output.status.success(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

fn git(workspace: &PathBuf, args: &[&str]) -> String {
    let output = Command::new("git").current_dir(workspace).args(args).output().expect("git runs");
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(60)..].join("\n")
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(case) = arguments.next() else {
        eprintln!("usage: ablate <case> --revert <file>...");
        std::process::exit(2);
    };
    let files: Vec<String> = arguments.skip_while(|argument| argument != "--revert").skip(1).collect();
    if files.is_empty() {
        eprintln!("name the files of the fix after --revert");
        std::process::exit(2);
    }
    let workspace = PathBuf::from(std::env::var_os("BUILD_WORKSPACE_DIRECTORY").expect("run it with `bazel run`"));
    let base = git(&workspace, &["merge-base", "HEAD", "origin/main"]);
    let fixed: Vec<(PathBuf, Vec<u8>)> = files
        .iter()
        .map(|file| {
            let path = workspace.join(file);
            let content = std::fs::read(&path).unwrap_or_else(|error| panic!("{file}: {error}"));
            (path, content)
        })
        .collect();
    let restore = |fixed: &[(PathBuf, Vec<u8>)]| {
        for (path, content) in fixed {
            std::fs::write(path, content).expect("a reverted file is put back");
        }
    };
    for file in &files {
        let old = git(&workspace, &["show", &format!("{base}:{file}")]);
        std::fs::write(workspace.join(file), format!("{old}\n")).expect("a file reverts");
    }
    let without = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bazel(&workspace, &case)));
    restore(&fixed);
    let without = without.unwrap_or_else(|_| std::process::exit(1));
    let with = bazel(&workspace, &case);
    println!("### `{case}` without the fix ({})\n\n```\n{}\n```\n", if without.0 { "PASSED: it proves nothing" } else { "fails" }, tail(&without.1));
    println!("### `{case}` with the fix ({})\n\n```\n{}\n```", if with.0 { "passes" } else { "FAILS" }, tail(&with.1));
    if without.0 || !with.0 {
        std::process::exit(1);
    }
}
