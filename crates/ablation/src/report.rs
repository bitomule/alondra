use std::collections::HashMap;
use std::fmt::Write;

use crate::plan::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    Ignored,
}

/// `test <name> ... ok|FAILED|ignored` lines of a libtest run.
pub fn parse_libtest(log: &str) -> HashMap<String, Outcome> {
    log.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("test ")?;
            let (name, result) = rest.split_once(" ... ")?;
            let outcome = match result.trim() {
                "ok" => Outcome::Passed,
                "FAILED" => Outcome::Failed,
                r if r.starts_with("ignored") => Outcome::Ignored,
                _ => return None,
            };
            Some((name.trim().to_string(), outcome))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Without {
    Failed,
    DoesNotCompile,
    DoesNotExist,
    PassedExempt,
    Passed,
    Ignored,
    BinaryBroken,
    NotRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum With {
    Passed,
    Ignored,
    Failed,
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub target: Target,
    /// The Bazel test target the outcomes come from.
    pub label: String,
    pub without: Without,
    pub with: With,
}

impl Row {
    /// An ignored test proves nothing either way, so it does not pass the check.
    pub fn ok(&self) -> bool {
        let without = matches!(
            self.without,
            Without::Failed | Without::DoesNotCompile | Without::DoesNotExist | Without::PassedExempt
        );
        without && self.with == With::Passed
    }
}

/// How the test ended without the change: `removed` when its own code did not compile
/// there and was taken out, `binary_broken` when its test binary still did not compile.
pub fn judge(
    target: &Target,
    label: &str,
    without: Option<Outcome>,
    removed: bool,
    binary_broken: bool,
    with: Option<Outcome>,
) -> Row {
    let without = match without {
        _ if !target.exists_without_change => Without::DoesNotExist,
        _ if removed => Without::DoesNotCompile,
        Some(Outcome::Failed) => Without::Failed,
        Some(Outcome::Passed) if target.passes_on_base.is_some() => Without::PassedExempt,
        Some(Outcome::Passed) => Without::Passed,
        Some(Outcome::Ignored) => Without::Ignored,
        None if binary_broken => Without::BinaryBroken,
        None => Without::NotRun,
    };
    let with = match with {
        Some(Outcome::Passed) => With::Passed,
        Some(Outcome::Ignored) => With::Ignored,
        Some(Outcome::Failed) => With::Failed,
        None => With::NotRun,
    };
    Row { target: target.clone(), label: label.to_string(), without, with }
}

/// `path:line` of every rustc error in a build log (the first `-->` after each `error`).
pub fn compile_errors(log: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut in_error = false;
    for line in log.lines() {
        if line.starts_with("error") {
            in_error = !line.starts_with("error: aborting")
                && !line.starts_with("error: could not compile");
            continue;
        }
        if !in_error {
            continue;
        }
        if let Some(at) = line.trim_start().strip_prefix("--> ") {
            let mut parts = at.rsplitn(3, ':');
            let _column = parts.next();
            let line_no = parts.next().and_then(|l| l.parse().ok());
            if let (Some(line_no), Some(path)) = (line_no, parts.next()) {
                out.push((path.to_string(), line_no));
            }
            in_error = false;
        }
    }
    out
}

fn without_text(row: &Row) -> String {
    match row.without {
        Without::Failed => "fails".into(),
        Without::DoesNotCompile => "fails (its code does not compile)".into(),
        Without::DoesNotExist => "fails (code under test does not exist)".into(),
        Without::PassedExempt => format!(
            "passes, exempt: {}",
            row.target.passes_on_base.as_deref().unwrap_or_default()
        ),
        Without::Passed => "PASSES: the test does not catch the change".into(),
        Without::Ignored => "IGNORED (#[ignore]): not proven".into(),
        Without::BinaryBroken => "NOT RUN: errors outside test code".into(),
        Without::NotRun => "DID NOT RUN".into(),
    }
}

fn with_text(with: With) -> &'static str {
    match with {
        With::Passed => "passes",
        With::Ignored => "IGNORED (#[ignore])",
        With::Failed => "FAILS",
        With::NotRun => "DID NOT RUN",
    }
}

pub fn render(rows: &[Row], uncovered: &[String]) -> String {
    let mut out = String::new();
    let name = |r: &Row| format!("{} {}", r.label, r.target.name);
    let width = rows.iter().map(|r| name(r).len()).max().unwrap_or(4).max(4);
    let _ = writeln!(out, "{:<width$}  {:<45}  with the change", "target test", "without the change");
    for row in rows {
        let mark = if row.ok() { " " } else { "✗" };
        let _ = writeln!(
            out,
            "{:<width$}  {:<45}  {} {mark}",
            name(row),
            without_text(row),
            with_text(row.with),
        );
    }
    if !uncovered.is_empty() {
        let _ = writeln!(out, "\nChanged tests this check does not ablate (prove them by hand):");
        for path in uncovered {
            let _ = writeln!(out, "  {path}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::Change;

    fn target(exists: bool, exempt: Option<&str>) -> Target {
        Target {
            crate_dir: "crates/c".into(),
            name: "t".into(),
            file: "crates/c/src/a.rs".into(),
            change: Change::New,
            passes_on_base: exempt.map(String::from),
            exists_without_change: exists,
        }
    }

    #[test]
    fn compile_errors_reads_the_location_of_each_error() {
        let log = "\
warning: unused variable
  --> crates/c/src/a.rs:3:9
error[E0425]: cannot find function `x` in this scope
   --> crates/c/src/a/tests.rs:261:16
    |
note: defined here
   --> crates/c/src/a.rs:10:1
error: aborting due to 1 previous error
error[E0061]: this function takes 3 arguments
  --> crates/c/tests/flow.rs:7:5
";
        assert_eq!(
            compile_errors(log),
            vec![
                ("crates/c/src/a/tests.rs".to_string(), 261),
                ("crates/c/tests/flow.rs".to_string(), 7),
            ]
        );
    }

    #[test]
    fn parses_libtest_lines() {
        let log = "running 3 tests\ntest a::b ... ok\ntest a::c ... FAILED\ntest d ... ignored, slow\ntest result: FAILED. 1 passed\n";
        let got = parse_libtest(log);
        assert_eq!(got.len(), 3);
        assert_eq!(got["a::b"], Outcome::Passed);
        assert_eq!(got["a::c"], Outcome::Failed);
        assert_eq!(got["d"], Outcome::Ignored);
    }

    #[test]
    fn judge_accepts_only_tests_that_fail_without_and_pass_with_the_change() {
        use Outcome::*;
        type Case = (Target, Option<Outcome>, bool, bool, Option<Outcome>, Without, bool);
        let cases: [Case; 11] = [
            (target(true, None), Some(Ignored), false, false, Some(Ignored), Without::Ignored, false),
            (target(false, None), None, false, false, Some(Ignored), Without::DoesNotExist, false),
            (target(true, None), Some(Failed), false, false, Some(Passed), Without::Failed, true),
            (target(true, None), Some(Passed), false, false, Some(Passed), Without::Passed, false),
            (target(true, Some("rename")), Some(Passed), false, false, Some(Passed), Without::PassedExempt, true),
            (target(true, None), None, true, false, Some(Passed), Without::DoesNotCompile, true),
            (target(true, None), None, false, true, Some(Passed), Without::BinaryBroken, false),
            (target(true, None), None, false, false, Some(Passed), Without::NotRun, false),
            (target(false, None), None, false, false, Some(Passed), Without::DoesNotExist, true),
            (target(true, None), Some(Failed), false, false, Some(Failed), Without::Failed, false),
            (target(true, None), Some(Failed), false, false, None, Without::Failed, false),
        ];
        for (i, (t, without, removed, broken, with, want, ok)) in cases.into_iter().enumerate() {
            let row = judge(&t, "//crates/c:c_test", without, removed, broken, with);
            assert_eq!(row.without, want, "case {i}");
            assert_eq!(row.ok(), ok, "case {i}");
        }
    }
}
