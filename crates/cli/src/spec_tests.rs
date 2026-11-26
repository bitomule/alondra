use std::path::Path;
use std::time::Duration;

use alondra_spec_engine::Files;
use serde_json::Value;

use super::{CLEAN, Limits, Outcome, PENDING, RecordKind, RecordVerdict, SpecCmd, UNREADABLE, execute, run_in, standalone_command};
use crate::i18n::{Lang, set_for_this_thread};
use crate::test_support::Directory;

const SETTINGS: &str = "format = 1\n\n[specs]\nenabled = true\n";

const REFUNDS: &str = "@spec:billing.refunds
Feature: Refunds return the money

  @id:full @boundary:dependencies
  Scenario: Full refund
    Given a paid order of 30 EUR
    When the order is refunded
    Then the customer receives 30 EUR

  @id:looks-right @judged:capture
  Scenario: The receipt shows the refund
    Given a refunded order
    When its receipt is shown
    Then the refund is the first line
";

fn boundary(run: &[String]) -> String {
    let command: Vec<String> = run.iter().map(|argument| format!("{argument:?}")).collect();
    format!(
        "id = \"billing\"\nowns = [\"crates/billing/**\"]\n\n[[suite]]\nid = \"unit\"\nrun = [{}]\ntests = [\"crates/billing/tests/*.rs\"]\n",
        command.join(", ")
    )
}

/// A checkout with one component whose only suite runs `run`, and the paths git would list.
fn checkout(run: &[String]) -> (Directory, Vec<String>) {
    let root = Directory::new();
    let files = [
        ("alondra.toml", SETTINGS.to_owned()),
        ("specs/billing/boundary.toml", boundary(run)),
        ("specs/billing/refunds.feature", REFUNDS.to_owned()),
        ("crates/billing/src/lib.rs", "pub fn refund() {}\n".to_owned()),
        ("crates/billing/tests/full.rs", "// spec: billing.refunds/full\n#[test]\nfn refunds_in_full() {}\n".to_owned()),
    ];
    for (path, content) in &files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }
    (root, files.iter().map(|(path, _)| (*path).to_owned()).collect())
}

/// This test binary asked for its list of tests: a command that ends well and touches nothing.
fn passing() -> Vec<String> {
    vec![std::env::current_exe().unwrap().display().to_string(), "--list".to_owned()]
}

fn failing() -> Vec<String> {
    vec![std::env::current_exe().unwrap().display().to_string(), "--not-an-option-of-the-test-harness".to_owned()]
}

fn run(root: &Path, paths: &[String], command: SpecCmd, json: bool) -> (i32, String) {
    let mut out = vec![];
    let code = run_in(root, paths.to_vec(), command, json, &mut out).unwrap();
    (code, String::from_utf8(out).unwrap())
}

fn state(report: &Value, subject: &str, kind: &str) -> String {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["subject"] == subject && item["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} item for {subject}: {report:#}"))["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn validated(root: &Path, paths: &[String]) -> (i32, Value) {
    let (code, out) = run(root, paths, SpecCmd::Validate { junit: None }, true);
    (code, serde_json::from_str(&out).unwrap())
}

#[test]
fn validating_lists_what_is_missing_and_writes_nothing() {
    let (root, paths) = checkout(&passing());

    let (code, report) = validated(&root, &paths);

    assert_eq!(code, PENDING);
    assert_eq!(report["status"], "pending");
    assert_eq!(state(&report, "billing/unit", "suite"), "missing");
    assert_eq!(state(&report, "billing.refunds/looks-right", "judged"), "missing");
    assert!(!root.join("specs/ledger").exists(), "validating wrote a ledger");
}

#[test]
fn validating_in_words_names_each_missing_validation() {
    let (root, paths) = checkout(&passing());

    let (code, out) = run(&root, &paths, SpecCmd::Validate { junit: None }, false);

    assert_eq!(code, PENDING);
    assert!(out.lines().any(|line| line.split_whitespace().collect::<Vec<_>>() == ["missing", "suite", "billing/unit"]), "{out}");
}

#[test]
fn a_suite_that_ends_well_is_recorded() {
    let (root, mut paths) = checkout(&passing());

    let (code, _) = run(&root, &paths, SpecCmd::Run { component: "billing".into(), suite: None }, false);

    assert_eq!(code, CLEAN);
    let entry = "specs/ledger/billing/suites/unit.toml";
    assert!(root.join(entry).is_file(), "the suite was not recorded");
    paths.push(entry.to_owned());
    let (_, report) = validated(&root, &paths);
    assert_eq!(state(&report, "billing/unit", "suite"), "valid");
    assert_eq!(state(&report, "billing.refunds/full", "passes"), "valid");
}

#[test]
fn a_suite_that_fails_records_nothing() {
    let (root, paths) = checkout(&failing());

    let (code, _) = run(&root, &paths, SpecCmd::Run { component: "billing".into(), suite: None }, false);

    assert_eq!(code, PENDING);
    assert!(!root.join("specs/ledger").exists(), "a failed suite was recorded");
}

#[test]
fn a_command_that_cannot_start_is_not_runnable() {
    let (root, paths) = checkout(&["no-such-program-in-this-checkout".to_owned()]);

    let (code, _) = run(&root, &paths, SpecCmd::Run { component: "billing".into(), suite: None }, false);

    assert_eq!(code, UNREADABLE);
    assert!(!root.join("specs/ledger").exists());
}

#[test]
fn an_unknown_component_is_not_runnable() {
    let (root, paths) = checkout(&passing());

    let (code, out) = run(&root, &paths, SpecCmd::Run { component: "ledger".into(), suite: None }, false);

    assert_eq!(code, UNREADABLE);
    assert_eq!(out.split(':').next(), Some("unknown_component"));
}

#[test]
#[ignore = "only run by `a_command_is_stopped_at_its_time_limit`, which stops it"]
fn waits_until_it_is_stopped() {
    std::thread::sleep(Duration::from_secs(600));
}

#[test]
fn a_command_is_stopped_at_its_time_limit() {
    let root = Directory::new();
    std::fs::create_dir_all(&*root).unwrap();
    let waiting = [std::env::current_exe().unwrap().display().to_string(), "--ignored".to_owned(), "--exact".to_owned(), "spec::tests::waits_until_it_is_stopped".to_owned()];

    let outcome = execute(&root, &waiting, &Limits { time: Duration::from_millis(300), output: 1024 }, &mut vec![]);

    assert!(matches!(outcome, Outcome::TimedOut));
}

fn this_test(name: &str) -> Vec<String> {
    vec![std::env::current_exe().unwrap().display().to_string(), "--ignored".to_owned(), "--exact".to_owned(), format!("spec::tests::{name}")]
}

fn is_running(process: i32) -> bool {
    unsafe { libc::kill(process, 0) == 0 }
}

#[test]
#[ignore = "only run by `the_time_limit_stops_what_the_command_started`, which stops it"]
fn starts_a_child_and_waits() {
    let child = std::process::Command::new(std::env::current_exe().unwrap()).args(&this_test("waits_until_it_is_stopped")[1..]).spawn().unwrap();
    std::fs::write("started", child.id().to_string()).unwrap();
    std::thread::sleep(Duration::from_secs(600));
}

#[test]
fn the_time_limit_stops_what_the_command_started() {
    let root = Directory::new();
    std::fs::create_dir_all(&*root).unwrap();

    let outcome = execute(&root, &this_test("starts_a_child_and_waits"), &Limits { time: Duration::from_secs(2), output: 1024 }, &mut vec![]);

    assert!(matches!(outcome, Outcome::TimedOut));
    let started: i32 = std::fs::read_to_string(root.join("started")).expect("the command never started its child").parse().unwrap();
    let waited = std::time::Instant::now();
    while is_running(started) && waited.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!is_running(started), "the process the command started outlived the time limit");
}

#[test]
fn output_beyond_the_limit_is_cut() {
    let root = Directory::new();
    std::fs::create_dir_all(&*root).unwrap();
    let mut out = vec![];

    let outcome = execute(&root, &passing(), &Limits { time: Duration::from_secs(60), output: 64 }, &mut out);

    assert!(matches!(outcome, Outcome::Passed));
    let (copied, notice) = out.split_at(64);
    assert!(copied.is_ascii(), "{copied:?}");
    let notice = String::from_utf8(notice.to_vec()).unwrap();
    assert!(notice.starts_with('(') && notice.ends_with(" 64 bytes)\n") && notice.lines().count() == 1, "{notice:?}");
}

#[test]
fn output_within_the_limit_is_copied_whole() {
    let root = Directory::new();
    std::fs::create_dir_all(&*root).unwrap();
    let mut out = vec![];

    execute(&root, &passing(), &Limits { time: Duration::from_secs(60), output: 1024 * 1024 }, &mut out);

    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("spec::tests::output_within_the_limit_is_copied_whole: test"), "{out}");
    assert!(out.trim_end().ends_with(" benchmarks"), "{out}");
}

#[test]
fn a_ledger_folder_that_is_a_link_is_not_written_through() {
    let (root, paths) = checkout(&passing());
    let outside = Directory::new();
    std::fs::create_dir_all(&*outside).unwrap();
    std::os::unix::fs::symlink(&*outside, root.join("specs/ledger")).unwrap();

    let (code, out) = run(&root, &paths, SpecCmd::Run { component: "billing".into(), suite: None }, false);

    assert_eq!(code, UNREADABLE);
    assert!(out.lines().any(|line| line.starts_with("path_outside_repository: specs/ledger/billing/suites/unit.toml")), "{out}");
    assert_eq!(std::fs::read_dir(&*outside).unwrap().count(), 0, "something was written outside the checkout");
}

#[test]
fn a_ledger_entry_that_is_a_link_is_not_written_through() {
    let (root, paths) = checkout(&passing());
    let outside = Directory::new();
    std::fs::create_dir_all(&*outside).unwrap();
    std::fs::write(outside.join("kept"), "kept").unwrap();
    let folder = root.join("specs/ledger/billing/scenarios/billing.refunds");
    std::fs::create_dir_all(&folder).unwrap();
    std::os::unix::fs::symlink(outside.join("kept"), folder.join("full.toml")).unwrap();
    let record = SpecCmd::Record { subject: "billing.refunds/full".into(), kind: RecordKind::Adequate, verdict: RecordVerdict::Met, evidence: vec![] };

    let (code, out) = run(&root, &paths, record, false);

    assert_eq!(code, UNREADABLE);
    assert_eq!(out.split(':').next(), Some("path_outside_repository"));
    assert_eq!(std::fs::read_to_string(outside.join("kept")).unwrap(), "kept");
}

#[test]
fn a_verdict_is_written_to_the_ledger() {
    let (root, mut paths) = checkout(&passing());
    let capture = root.join("receipt.png");
    std::fs::write(&capture, b"capture").unwrap();
    let record = || SpecCmd::Record { subject: "billing.refunds/looks-right".into(), kind: RecordKind::Judged, verdict: RecordVerdict::Met, evidence: vec![capture.clone()] };

    let (code, out) = run(&root, &paths, record(), false);

    assert_eq!(code, CLEAN);
    let entry = "specs/ledger/billing/scenarios/billing.refunds/looks-right.toml";
    assert_eq!(out.trim_end(), entry);
    paths.push(entry.to_owned());
    let (_, report) = validated(&root, &paths);
    assert_eq!(state(&report, "billing.refunds/looks-right", "judged"), "valid");
}

#[test]
fn a_judgment_without_its_evidence_is_refused() {
    let (root, paths) = checkout(&passing());
    let record = SpecCmd::Record { subject: "billing.refunds/looks-right".into(), kind: RecordKind::Judged, verdict: RecordVerdict::Met, evidence: vec![] };

    let (code, out) = run(&root, &paths, record, false);

    assert_eq!(code, UNREADABLE);
    assert_eq!(out.split(':').next(), Some("evidence_required"));
    assert!(!root.join("specs/ledger").exists());
}

#[test]
fn specs_that_cannot_be_read_exit_with_two() {
    let (root, paths) = checkout(&passing());
    std::fs::write(root.join("alondra.toml"), "format = 1\n\n[specs]\nenable = true\n").unwrap();

    let (code, report) = validated(&root, &paths);

    assert_eq!(code, UNREADABLE);
    assert_eq!(report["status"], "unreadable");
    assert_eq!(report["problems"][0]["code"], "invalid_settings");
    assert_eq!(report["problems"][0]["path"], "alondra.toml");
}

#[test]
fn a_file_that_leaves_the_checkout_through_a_link_is_not_read() {
    let (root, _) = checkout(&passing());
    let outside = Directory::new();
    std::fs::create_dir_all(&*outside).unwrap();
    std::fs::write(outside.join("stolen.rs"), "pub fn stolen() {}\n").unwrap();
    std::os::unix::fs::symlink(outside.join("stolen.rs"), root.join("crates/billing/src/stolen.rs")).unwrap();
    let checkout = super::Checkout { root: root.canonicalize().unwrap(), paths: vec![] };

    assert_eq!(checkout.read("crates/billing/src/stolen.rs"), None);
    assert_eq!(checkout.read("crates/billing/src/lib.rs"), Some(b"pub fn refund() {}\n".to_vec()));
}

#[test]
fn the_packet_of_a_component_opens_with_the_rule() {
    let (root, paths) = checkout(&passing());

    let (code, packet) = run(&root, &paths, SpecCmd::Packet { component: "billing".into() }, false);

    assert_eq!(code, CLEAN);
    assert_eq!(packet.lines().next(), Some("# Component billing"));
}

#[test]
fn test_results_are_read_from_a_junit_file() {
    let (root, mut paths) = checkout(&passing());
    run(&root, &paths, SpecCmd::Run { component: "billing".into(), suite: None }, false);
    paths.push("specs/ledger/billing/suites/unit.toml".to_owned());
    let results = root.join("results.xml");
    std::fs::write(&results, "<testsuites><testsuite><testcase name=\"refunds_in_full\"><failure/></testcase></testsuite></testsuites>").unwrap();

    let (_, out) = run(&root, &paths, SpecCmd::Validate { junit: Some(results) }, true);

    let report: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(state(&report, "billing.refunds/full", "passes"), "failed");
}

#[test]
fn the_binary_of_its_own_prints_its_help_in_the_language_of_the_user() {
    set_for_this_thread(Lang::Es);
    let mut command = standalone_command();
    let mut pages = vec![command.render_long_help().to_string()];
    let names: Vec<String> = command.get_subcommands().map(|sub| sub.get_name().to_string()).collect();
    for name in names {
        pages.push(command.find_subcommand_mut(&name).unwrap().render_long_help().to_string());
    }

    assert!(pages[0].contains("Uso: alondra-spec"), "{}", pages[0]);
    for page in &pages {
        for english in ["Usage:", "Commands:", "Arguments:", "Options:", "Print help", "Print this message"] {
            assert!(!page.contains(english), "{english:?} in:\n{page}");
        }
    }
}
