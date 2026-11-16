//! `bazel run //crates/scenarios:from_report -- <package> --name <case> --out .scratch/<case>`:
//! drafts a scenario from the folder `alondra report` wrote on this Mac. The draft goes under the
//! `.scratch` folder of the worktree, never into `cases/`, and nothing is written when the package
//! or the draft carries a private value: the transcript guard, the report's own final check with
//! this Mac's private values and the repository's public data guard all read it first.

use alondra_scenarios::from_report::{Leak, convert};
use alondra_scenarios_transcript::{guard, scratch};
use std::path::{Path, PathBuf};

const PUBLIC_DATA_GUARD: &str = "tools/public_data_guard.py";

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

/// The report's final check, with `values` and those the report replaced as private.
fn report_check(files: &[(&str, &str)], originals: &[String], values: &[String]) -> Vec<Leak> {
    let values: Vec<String> = values.iter().chain(originals).cloned().collect();
    alondra::report_leaks(files, &values).into_iter().map(|hit| Leak { file: hit.section, line: hit.line, rule: format!("report check: {}", hit.what) }).collect()
}

/// The public data guard of the repository over the files, written for it into a new folder
/// inside `staging`.
fn public_data_guard(files: &[(&str, &str)], script: &Path, staging: &Path) -> Vec<Leak> {
    let refused = |why: String| vec![Leak { file: PUBLIC_DATA_GUARD.into(), line: 0, rule: format!("public data guard did not run: {why}") }];
    let folder = match tempfile::Builder::new().prefix("from-report-").tempdir_in(staging) {
        Ok(folder) => folder,
        Err(error) => return refused(error.to_string()),
    };
    for (name, text) in files {
        let path = folder.path().join(name);
        if let Err(error) = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, text)) {
            return refused(error.to_string());
        }
    }
    let output = match std::process::Command::new("python3").arg(script).arg("--files").arg(folder.path()).args(files.iter().map(|(name, _)| name)).output() {
        Ok(output) => output,
        Err(error) => return refused(error.to_string()),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let leaks: Vec<Leak> = stdout
        .lines()
        .filter_map(|line| {
            let (place, rule) = line.split_once(": ")?;
            let (file, number) = place.rsplit_once(':')?;
            Some(Leak { file: file.to_owned(), line: number.parse().ok()?, rule: format!("public data guard: {rule}") })
        })
        .collect();
    match (output.status.success(), leaks.is_empty()) {
        (false, true) => refused(String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("it failed").to_owned()),
        _ => leaks,
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let option = |name: &str| arguments.iter().position(|argument| argument == name).and_then(|at| arguments.get(at + 1));
    let (Some(package), Some(name), Some(out)) = (arguments.first().filter(|first| !first.starts_with("--")), option("--name"), option("--out")) else {
        fail("usage: from_report <package folder> --name <case> --out <folder under .scratch>")
    };
    let out = scratch::destination(Path::new(out)).unwrap_or_else(|why| fail(&format!("{why}; moving the draft into cases/ is for whoever completes it")));
    let workspace = scratch::workspace().unwrap_or_else(|why| fail(&why));
    let staging = workspace.join(scratch::SCRATCH);
    std::fs::create_dir_all(&staging).unwrap_or_else(|error| fail(&error.to_string()));
    let literals = guard::literals_from_env().unwrap_or_else(|error| fail(&error.to_string()));
    let values: Vec<String> = literals.iter().cloned().chain(alondra::private_values_of_this_mac()).collect();
    let script: PathBuf = workspace.join(PUBLIC_DATA_GUARD);
    let report = |files: &[(&str, &str)], originals: &[String]| report_check(files, originals, &values);
    let public = |files: &[(&str, &str)], _: &[String]| public_data_guard(files, &script, &staging);
    let draft = convert(Path::new(package), name, &out, &literals, &[&report, &public]).unwrap_or_else(|refusal| fail(&refusal.to_string()));
    println!("{} written with {} gap(s) to complete; read gaps.txt first", out.display(), draft.gaps.lines().count().saturating_sub(1));
}

#[cfg(test)]
#[path = "from_report_tests.rs"]
mod tests;
