//! Every case of `cases/`, each one a test of its own: `--test_arg=<case>` runs the ones whose name
//! contains it, and the shards Bazel asks for split the rest, one case each.

use std::path::PathBuf;

fn cases_root() -> PathBuf {
    let runfiles = PathBuf::from(std::env::var_os("TEST_SRCDIR").expect("Bazel runfiles root"));
    let workspace = std::env::var_os("TEST_WORKSPACE").expect("Bazel workspace");
    runfiles.join(workspace).join(env!("SCENARIO_CASES"))
}

fn main() {
    let filters: Vec<String> = std::env::args().skip(1).filter(|argument| !argument.starts_with("--")).collect();
    let shard: usize = std::env::var("TEST_SHARD_INDEX").ok().and_then(|value| value.parse().ok()).unwrap_or(0);
    let shards: usize = std::env::var("TEST_TOTAL_SHARDS").ok().and_then(|value| value.parse().ok()).unwrap_or(1);
    if let Some(status) = std::env::var_os("TEST_SHARD_STATUS_FILE") {
        let _ = std::fs::write(status, "");
    }
    let cases = alondra_scenarios::cases(&cases_root()).expect("the cases folder is readable");
    assert!(!cases.is_empty(), "there is no scenario in {}", cases_root().display());
    // One case per shard: the edit loop waits for the longest case, never for two of them.
    if std::env::var_os("TEST_TOTAL_SHARDS").is_some() && filters.is_empty() && shards != cases.len() {
        println!("scenarios_test runs in {shards} shards for {} cases: set shard_count in crates/scenarios/BUILD.bazel to {}", cases.len(), cases.len());
        std::process::exit(1);
    }
    let mut failed = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let name = case.file_name().expect("a case has a name").to_string_lossy().into_owned();
        if index % shards != shard || !(filters.is_empty() || filters.iter().any(|filter| name.contains(filter.as_str()))) {
            continue;
        }
        let started = std::time::Instant::now();
        match alondra_scenarios::check(case) {
            Ok(outcome) => println!("scenario {name} ... ok ({:.2} s)\n{}", started.elapsed().as_secs_f64(), outcome.timings.line()),
            Err(error) => {
                println!("scenario {name} ... FAILED ({:.2} s)\n{error}", started.elapsed().as_secs_f64());
                failed.push(name);
            }
        }
    }
    if !failed.is_empty() {
        println!("\nfailed: {}", failed.join(", "));
        std::process::exit(1);
    }
}
