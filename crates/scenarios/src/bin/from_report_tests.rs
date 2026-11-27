use super::*;
use serde_json::json;

fn script() -> PathBuf {
    let runfiles = PathBuf::from(std::env::var_os("TEST_SRCDIR").expect("Bazel runfiles root"));
    runfiles.join(std::env::var_os("TEST_WORKSPACE").expect("Bazel workspace")).join(PUBLIC_DATA_GUARD)
}

/// The interpreter the test wrapper names: `python3` is an Xcode shim that asks xcodebuild where
/// python3 is when its cache has no entry for the HOME, which takes most of a minute on a loaded
/// Mac, and the wrapper gives every test a new cache. Outside the wrapper there is no such name.
fn python() -> std::ffi::OsString {
    std::env::var_os("ALONDRA_TEST_PYTHON").unwrap_or_else(|| "python3".into())
}

/// A package whose nodes carry what only one of the two checks knows: an address, which the
/// report's check finds, and the id of an issue of a tracker, which the public data guard finds.
fn package(node: &str) -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    let task = json!({"task": "canary-task-1", "request": "canary-request", "state": "working", "states_in_window": [{"at": "2026-11-10T10:00:00Z", "state": "working"}]});
    std::fs::write(folder.path().join("tasks.jsonl"), format!("{task}\n")).unwrap();
    std::fs::write(folder.path().join("nodes.jsonl"), format!("{}\n", json!({"node": node}))).unwrap();
    folder
}

fn convert_with_both_checks(package: &Path, target: &Path) -> Result<alondra_scenarios::from_report::Draft, alondra_scenarios::from_report::Refusal> {
    let staging = tempfile::tempdir().unwrap();
    let values = vec!["canaryhorizon".to_owned()];
    let report = |files: &[(&str, &str)], originals: &[String]| report_check(files, originals, &values);
    let public = |files: &[(&str, &str)], _: &[String]| public_data_guard(files, &python(), &script(), staging.path());
    convert(package, "canary-case", target, &[], &[&report, &public])
}

#[test]
fn the_report_check_and_the_public_data_guard_each_refuse_what_only_they_find() {
    let tracker_issue = format!("{}-{}", "BIT", 4821);
    let source = package(&format!("canary node at 203.0.113.7 working on {tracker_issue}"));
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("draft");
    let Err(alondra_scenarios::from_report::Refusal::Leaks(leaks)) = convert_with_both_checks(source.path(), &target) else { panic!("the canaries must refuse the draft") };
    let rules: Vec<&str> = leaks.iter().map(|leak| leak.rule.as_str()).collect();
    assert!(rules.iter().any(|rule| rule.starts_with("report check: an IP address")), "{rules:?}");
    assert!(rules.contains(&"public data guard: tracker-id"), "{rules:?}");
    assert!(leaks.iter().all(|leak| leak.file == "nodes.jsonl"), "{leaks:?}");
    assert!(!target.exists(), "nothing is written when the draft is refused");
}

#[test]
fn a_private_value_of_the_report_check_refuses_the_draft() {
    let source = package("CanaryHorizon-builder");
    let out = tempfile::tempdir().unwrap();
    let Err(alondra_scenarios::from_report::Refusal::Leaks(leaks)) = convert_with_both_checks(source.path(), &out.path().join("draft")) else { panic!("refused") };
    assert!(leaks.iter().any(|leak| leak.rule.starts_with("report check: a private value")), "{leaks:?}");
}

#[test]
fn a_clean_package_passes_both_checks_and_is_written() {
    let source = package("node-1");
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("draft");
    convert_with_both_checks(source.path(), &target).expect("a clean package drafts");
    assert!(target.join("scenario.toml").is_file());
}

#[test]
fn the_guard_runs_without_asking_xcodebuild_where_python_is() {
    let Some(cache) = std::env::var_os("xcrun_db").map(PathBuf::from).filter(|_| std::env::var_os("ALONDRA_TEST_PYTHON").is_some()) else { return };
    let source = package("node-1");
    let out = tempfile::tempdir().unwrap();
    convert_with_both_checks(source.path(), &out.path().join("draft")).expect("a clean package drafts");
    assert!(!cache.exists(), "the guard went through the python3 shim, which asked xcodebuild and cached the answer in {}", cache.display());
}
