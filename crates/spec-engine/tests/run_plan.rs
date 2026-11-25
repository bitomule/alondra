use alondra_spec_engine::{Code, Kind, Run, State};

use crate::support::{Tree, add_contract_suite, billing, project, state};

fn two_suites() -> Tree {
    let mut tree = billing();
    add_contract_suite(&mut tree);
    tree
}

fn unit() -> Run {
    Run {
        component: "billing".to_string(),
        suite: "unit".to_string(),
        arguments: vec!["bazelisk".to_string(), "test".to_string(), "//crates/billing:test".to_string()],
        timeout_minutes: 30,
    }
}

fn contract() -> Run {
    Run {
        component: "billing".to_string(),
        suite: "contract".to_string(),
        arguments: vec!["python3".to_string(), "crates/billing/contract_test.py".to_string()],
        timeout_minutes: 10,
    }
}

// spec: spec-engine.run-plan/plans-every-suite
#[test]
fn plans_every_suite_in_the_order_of_the_boundary_file() {
    let plan = project(&two_suites()).plan("billing", None).unwrap();

    assert_eq!(plan, [unit(), contract()]);
}

// spec: spec-engine.run-plan/plans-one-suite
#[test]
fn plans_only_the_suite_asked_for() {
    let plan = project(&two_suites()).plan("billing", Some("contract")).unwrap();

    assert_eq!(plan, [contract()]);
}

// spec: spec-engine.run-plan/default-time-limit
#[test]
fn a_suite_without_a_time_limit_gets_thirty_minutes() {
    let plan = project(&billing()).plan("billing", Some("unit")).unwrap();

    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].timeout_minutes, 30);
}

// spec: spec-engine.run-plan/unknown-component-or-suite
#[test]
fn planning_what_does_not_exist_is_refused() {
    let loaded = project(&billing());

    let component = loaded.plan("ledger", None).unwrap_err();
    let suite = loaded.plan("billing", Some("smoke")).unwrap_err();

    assert_eq!(component.code, Code::UnknownComponent);
    assert_eq!(suite.code, Code::UnknownSuite);
    assert_eq!(suite.code.as_str(), "unknown_suite");
}

// spec: spec-engine.run-plan/passed-suite-is-recorded
#[test]
fn a_suite_recorded_as_passed_reports_valid() {
    let mut tree = billing();
    let loaded = project(&tree);
    assert_eq!(state(&loaded.report(None), "billing/unit", Kind::Suite), State::Missing);

    tree.apply(&loaded.suite_passed("billing", "unit").unwrap());

    assert_eq!(state(&project(&tree).report(None), "billing/unit", Kind::Suite), State::Valid);
}

// spec: spec-engine.run-plan/recording-never-touches-other-entries
#[test]
fn recording_one_suite_hands_back_only_its_entry() {
    let mut tree = two_suites();
    let entry = project(&tree).suite_passed("billing", "unit").unwrap();

    assert_eq!(entry.path, "specs/ledger/billing/suites/unit.toml");
    tree.apply(&entry);
    let report = project(&tree).report(None);
    assert_eq!(state(&report, "billing/unit", Kind::Suite), State::Valid);
    assert_eq!(state(&report, "billing/contract", Kind::Suite), State::Missing);
}
