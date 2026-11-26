use alondra_spec_engine::{Code, Kind, read_spec};

use crate::support::{REFUNDS_PATH, billing, codes, line_of, problem, project, record_everything, sorted_kinds};

// spec: spec-engine.scenario-ids/title-change-keeps-id
#[test]
fn rewording_a_title_keeps_the_scenario_and_its_record() {
    let mut tree = billing();
    record_everything(&mut tree);
    tree.edit(REFUNDS_PATH, "Scenario: Full refund", "Scenario: Complete refund");

    let loaded = project(&tree);
    let report = loaded.report(None);

    let spec = &loaded.specs().iter().find(|loaded| loaded.spec.id == "billing.refunds").unwrap().spec;
    assert_eq!(spec.scenarios[0].id, "full");
    assert_eq!(spec.scenarios[0].title, "Complete refund");
    assert_eq!(sorted_kinds(&report, "billing.refunds/full"), [Kind::Passes, Kind::Adequate]);
    assert!(loaded.recorded("billing.refunds/full", Kind::Adequate).is_some());
    assert!(report.leftover.is_empty(), "{:#?}", report.leftover);
}

// spec: spec-engine.scenario-ids/duplicate-scenario-id-is-a-problem
#[test]
fn two_scenarios_with_one_identifier_are_refused_at_the_second() {
    let text = "@spec:billing.refunds
Feature: Refunds

  @id:first-run
  Scenario: First run
    Given a paid order

  @id:first-run
  Scenario: Second run
    Given another paid order
";
    let problems = read_spec(REFUNDS_PATH, text.as_bytes()).expect_err("the file reads and it should not");

    assert_eq!(codes(&problems), [Code::DuplicateScenarioId], "{problems:#?}");
    assert_eq!(problems[0].path, REFUNDS_PATH);
    assert_eq!(problems[0].line, line_of(text, "Scenario: Second run"));
    assert_eq!(problems[0].code.as_str(), "duplicate_scenario_id");
}

// spec: spec-engine.scenario-ids/duplicate-spec-id-is-a-problem
#[test]
fn two_files_with_one_spec_identifier_are_refused_at_the_second() {
    let tree = billing().with(
        "specs/billing/returns.feature",
        "@spec:billing.refunds\nFeature: Returns\n\n  @id:returned\n  Scenario: A returned order\n    Given a returned order\n",
    );

    let found = problem(&tree);

    assert_eq!(found.code, Code::DuplicateSpecId);
    assert_eq!(found.path, "specs/billing/returns.feature");
}
