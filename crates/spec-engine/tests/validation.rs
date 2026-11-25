use alondra_spec_engine::{Code, Kind, Problem, Report, ReportStatus, State, Verdict, read_results};

use crate::support::{
    BOUNDARY, OVERVIEW, OVERVIEW_PATH, REFUNDS_PATH, SETTINGS, Tree, add_contract_suite, billing, codes, decision_file,
    decision_path, line_of, problem_with, problems, project, record_everything, rich, sorted_kinds, state, test_file,
};

const FULL: &str = "billing.refunds/full";

fn with_scenario(tree: &mut Tree, tags: &str, title: &str) {
    let text = tree.text(REFUNDS_PATH);
    tree.put(REFUNDS_PATH, &format!("{text}\n  {tags}\n  Scenario: {title}\n    Given a refunded order\n    Then {title} holds\n"));
}

fn junit(cases: &[String]) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n  <testsuite name=\"billing\">\n{}  </testsuite>\n</testsuites>\n",
        cases.concat()
    )
}

fn passed(name: &str) -> String {
    format!("    <testcase name=\"{name}\" classname=\"billing\"/>\n")
}

fn ended(name: &str, child: &str) -> String {
    format!("    <testcase name=\"{name}\" classname=\"billing\">{child}</testcase>\n")
}

// spec: spec-engine.validation/clean-project
#[test]
fn a_project_recorded_with_current_fingerprints_is_clean() {
    let mut tree = billing();
    with_scenario(&mut tree, "@id:audited @judged", "the refund is audited");
    record_everything(&mut tree);

    let report = project(&tree).report(None);

    assert_eq!(report.status, ReportStatus::Clean);
    assert!(report.items.iter().all(|item| item.state == State::Valid), "{:#?}", report.items);
    for kind in [Kind::Suite, Kind::Passes, Kind::Adequate, Kind::Judged] {
        assert!(report.items.iter().any(|item| item.kind == kind), "no {kind:?} item");
    }
}

// spec: spec-engine.validation/kinds-of-suite-are-all-the-same
#[test]
fn every_runner_is_a_suite_and_a_scenario_needs_the_suites_that_declare_it() {
    let mut tree = billing()
        .with("crates/billing/e2e/refund.spec.ts", "// spec: billing.refunds/partial\ntest('refunds in part', async ({ page }) => {\n});\n")
        .with("crates/billing/snapshots/RefundTests.swift", "// spec: billing.refunds/full\nfunc testFullRefund() {\n}\n");
    let text = tree.text("specs/billing/boundary.toml");
    tree.put(
        "specs/billing/boundary.toml",
        &format!(
            "{text}\n[[suite]]\nid = \"e2e\"\nrun = [\"npx\", \"playwright\", \"test\"]\ntests = [\"crates/billing/e2e/**/*.spec.ts\"]\n\n[[suite]]\nid = \"snapshot\"\nrun = [\"swift\", \"test\", \"--filter\", \"SnapshotTests\"]\ntests = [\"crates/billing/snapshots/**/*.swift\"]\n"
        ),
    );
    let loaded = project(&tree);
    for suite in ["unit", "e2e"] {
        tree.apply(&loaded.suite_passed("billing", suite).unwrap());
    }

    let report = project(&tree).report(None);

    for subject in ["billing/unit", "billing/e2e", "billing/snapshot"] {
        assert_eq!(sorted_kinds(&report, subject), [Kind::Suite], "{subject}");
    }
    assert_eq!(state(&report, "billing/snapshot", Kind::Suite), State::Missing);
    assert_eq!(state(&report, FULL, Kind::Passes), State::Missing);
    assert_eq!(state(&report, "billing.refunds/partial", Kind::Passes), State::Valid);
    assert_eq!(state(&report, "billing.refunds/stands-alone", Kind::Passes), State::Valid);
}

// spec: spec-engine.validation/items-of-a-scenario
#[test]
fn a_scenario_has_the_items_its_tests_and_judgment_call_for() {
    let mut tree = billing().with("crates/billing/tests/reviewed.rs", &test_file("billing.refunds/reviewed", "reviews_the_refund"));
    with_scenario(&mut tree, "@id:audited @judged", "the refund is audited");
    with_scenario(&mut tree, "@id:reviewed @judged", "the refund is reviewed");

    let report = project(&tree).report(None);

    assert_eq!(sorted_kinds(&report, FULL), [Kind::Passes, Kind::Adequate]);
    assert_eq!(sorted_kinds(&report, "billing.refunds/audited"), [Kind::Judged]);
    assert_eq!(sorted_kinds(&report, "billing.refunds/reviewed"), [Kind::Passes, Kind::Adequate, Kind::Judged]);
}

// spec: spec-engine.validation/never-validated-is-missing
#[test]
fn with_an_empty_ledger_everything_is_missing() {
    let report = project(&billing()).report(None);

    assert_eq!(report.status, ReportStatus::Pending);
    assert_eq!(state(&report, "billing/unit", Kind::Suite), State::Missing);
    assert_eq!(state(&report, FULL, Kind::Passes), State::Missing);
    assert_eq!(state(&report, FULL, Kind::Adequate), State::Missing);
}

// spec: spec-engine.validation/changed-inputs-are-stale
#[test]
fn editing_an_owned_source_makes_the_suite_and_passes_stale_but_not_adequacy() {
    let mut tree = billing();
    record_everything(&mut tree);

    tree.put("crates/billing/src/lib.rs", "pub fn refund() { todo!() }\n");
    let report = project(&tree).report(None);

    assert_eq!(state(&report, "billing/unit", Kind::Suite), State::Stale);
    assert_eq!(state(&report, FULL, Kind::Passes), State::Stale);
    assert_eq!(state(&report, FULL, Kind::Adequate), State::Valid);
}

// spec: spec-engine.validation/unmet-verdict-is-reported
#[test]
fn an_unmet_adequacy_is_reported_as_unmet_and_pending() {
    let mut tree = billing();
    record_everything(&mut tree);
    tree.apply(&project(&tree).verdict(FULL, Kind::Adequate, Verdict::Unmet, &[]).unwrap());

    let report = project(&tree).report(None);

    assert_eq!(state(&report, FULL, Kind::Adequate), State::Unmet);
    assert_eq!(report.status, ReportStatus::Pending);
}

// spec: spec-engine.validation/uncovered-scenario-is-reported
#[test]
fn a_scenario_nothing_verifies_is_uncovered_and_pending() {
    let mut tree = billing();
    tree.remove("crates/billing/tests/partial.rs");
    record_everything(&mut tree);

    let report = project(&tree).report(None);

    assert_eq!(sorted_kinds(&report, "billing.refunds/partial"), [Kind::Passes]);
    assert_eq!(state(&report, "billing.refunds/partial", Kind::Passes), State::Uncovered);
    assert_eq!(report.status, ReportStatus::Pending);
}

// spec: spec-engine.validation/scenario-passes-with-all-its-suites
#[test]
fn a_scenario_declared_in_two_suites_needs_both_recorded() {
    let mut tree = billing();
    add_contract_suite(&mut tree);
    tree.put("crates/billing/contract_test.py", "# spec: billing.refunds/full\ndef test_full_refund(self):\n    pass\n");
    tree.apply(&project(&tree).suite_passed("billing", "unit").unwrap());

    let report = project(&tree).report(None);

    assert_eq!(state(&report, FULL, Kind::Passes), State::Missing);
    assert_eq!(state(&report, "billing.refunds/partial", Kind::Passes), State::Valid);
}

// spec: spec-engine.validation/problems-make-it-unreadable
#[test]
fn a_report_of_problems_is_unreadable_with_no_items() {
    let mut tree = billing()
        .with(
            "specs/billing/invoices.feature",
            "@spec:billing.invoices\nFeature: Invoices\n\n  @id:sent @wip\n  Scenario: An invoice is sent\n    Given a paid order\n",
        )
        .reversed();
    tree.edit(REFUNDS_PATH, "@id:partial\n", "@id:partial @draft\n");
    tree.edit(REFUNDS_PATH, "@id:full\n", "@id:full @later\n");
    let mut found = problems(&tree);
    assert_eq!(codes(&found), [Code::UnknownTag, Code::UnknownTag, Code::UnknownTag], "{found:#?}");
    found.reverse();

    let report = Report::unreadable(found);

    assert_eq!(report.status, ReportStatus::Unreadable);
    assert!(report.items.is_empty());
    let refunds = tree.text(REFUNDS_PATH);
    let places: Vec<(&str, usize)> = report.problems.iter().map(|problem| (problem.path.as_str(), problem.line)).collect();
    assert_eq!(
        places,
        [("specs/billing/invoices.feature", 4), (REFUNDS_PATH, line_of(&refunds, "@later")), (REFUNDS_PATH, line_of(&refunds, "@draft"))]
    );
}

// spec: spec-engine.validation/results-require-the-declared-test
#[test]
fn with_results_the_declared_test_itself_must_have_passed() {
    let others = [passed("refunds_in_part"), passed("stands_alone")];
    let rows = [
        (passed("refunds_in_full"), State::Valid),
        (passed("billing::tests::refunds_in_full"), State::Valid),
        (passed("BillingTests.refunds_in_full"), State::Valid),
        (ended("refunds_in_full", "<failure message=\"expected 30\"/>"), State::Failed),
        (ended("refunds_in_full", "<skipped/>"), State::Failed),
        (passed("also_refunds_in_full"), State::Failed),
        (
            format!("{}{}", ended("billing::refunds_in_full", "<failure message=\"expected 30\"/>"), passed("ledger::refunds_in_full")),
            State::Failed,
        ),
    ];
    let mut tree = billing();
    record_everything(&mut tree);
    let loaded = project(&tree);

    for (case, expected) in rows {
        let document = junit(&[case.clone(), others[0].clone(), others[1].clone()]);
        let results = read_results(document.as_bytes()).unwrap();

        let report = loaded.report(Some(&results));

        assert_eq!(state(&report, FULL, Kind::Passes), expected, "{case}");
        assert_eq!(state(&report, "billing.refunds/partial", Kind::Passes), State::Valid, "{case}");
    }
}

// spec: spec-engine.validation/results-need-a-test-name
#[test]
fn with_results_a_marker_above_no_named_test_fails() {
    let mut tree = billing();
    tree.put("crates/billing/tests/full.rs", "// spec: billing.refunds/full\n#[test]\nconst NOT_A_FUNCTION: () = ();\n");
    record_everything(&mut tree);
    let loaded = project(&tree);
    let document = junit(&[passed("refunds_in_full"), passed("refunds_in_part"), passed("stands_alone")]);
    let results = read_results(document.as_bytes()).unwrap();

    assert_eq!(loaded.markers().iter().find(|marker| marker.scenario == FULL).unwrap().test, None);
    assert_eq!(state(&loaded.report(Some(&results)), FULL, Kind::Passes), State::Failed);
    assert_eq!(state(&loaded.report(Some(&results)), "billing.refunds/partial", Kind::Passes), State::Valid);
    assert_eq!(state(&loaded.report(None), FULL, Kind::Passes), State::Valid);
}

// spec: spec-engine.validation/unreadable-listed-file-is-a-problem
#[test]
fn a_listed_file_the_caller_cannot_read_is_refused() {
    let mut base = rich();
    base.apply(&project(&base).suite_passed("billing", "unit").unwrap());
    for path in [
        "alondra.toml",
        "specs/billing/boundary.toml",
        REFUNDS_PATH,
        "docs/decisions/keep-ledger.md",
        OVERVIEW_PATH,
        "specs/ledger/billing/suites/unit.toml",
        "crates/billing/tests/full.rs",
    ] {
        let tree = base.clone().unreadable(path);

        let found = problem_with(&tree, Code::UnreadableFile);

        assert_eq!(found.path, path, "{found:#?}");
        assert_eq!(found.code.as_str(), "unreadable_file");
    }
}

// spec: spec-engine.validation/oversized-results-are-a-problem
#[test]
fn results_beyond_the_size_limit_are_refused() {
    let document = junit(&[passed("refunds_in_full")]);
    let comment_at = document.rfind("</testsuites>").unwrap();
    let sized = |size: usize| {
        let filler = size - document.len() - "<!--".len() - "-->".len();
        format!("{}<!--{}-->{}", &document[..comment_at], "x".repeat(filler), &document[comment_at..])
    };

    let results = read_results(sized(16_777_216).as_bytes()).unwrap();
    assert!(results.passed("refunds_in_full"));

    let refused = read_results(sized(16_777_217).as_bytes()).unwrap_err();
    assert_eq!(refused.code, Code::TooLarge, "{refused:#?}");
}

// spec: spec-engine.validation/reads-junit-results
#[test]
fn only_a_case_without_failure_error_or_skip_counts_as_passed() {
    let document = junit(&[
        passed("pays"),
        ended("fails", "<failure message=\"expected 30\">assertion failed</failure>"),
        ended("breaks", "<error message=\"panicked\"/>"),
        ended("waits", "<skipped/>"),
    ]);

    let results = read_results(document.as_bytes()).unwrap();

    assert!(results.passed("pays"));
    for name in ["fails", "breaks", "waits"] {
        assert!(!results.passed(name), "{name}");
    }
}

// spec: spec-engine.validation/unreadable-results-are-a-problem
#[test]
fn results_that_are_not_well_formed_xml_are_refused() {
    let refused = read_results(b"<testsuites><testsuite><testcase name=\"pays\"></testsuite></testsuites>").unwrap_err();

    assert_eq!(refused.code, Code::InvalidResults);
    assert_eq!(refused.code.as_str(), "invalid_results");
}

const REPORT_JSON: &str = r#"{
  "format": 1,
  "status": "pending",
  "items": [
    {
      "subject": "billing.refunds/full",
      "component": "billing",
      "kind": "adequate",
      "state": "missing"
    },
    {
      "subject": "billing.refunds/full",
      "component": "billing",
      "kind": "passes",
      "state": "valid"
    },
    {
      "subject": "billing/unit",
      "component": "billing",
      "kind": "suite",
      "state": "valid"
    }
  ],
  "leftover": [],
  "problems": []
}"#;

// spec: spec-engine.validation/report-as-json
#[test]
fn the_report_as_json_is_exactly_the_documented_text() {
    let mut tree = Tree::new()
        .with("alondra.toml", SETTINGS)
        .with("specs/billing/boundary.toml", BOUNDARY)
        .with(
            REFUNDS_PATH,
            "@spec:billing.refunds\nFeature: Refunds return the money\n\n  @id:full @boundary:dependencies\n  Scenario: Full refund\n    Given a paid order of 30 EUR\n    Then the customer receives 30 EUR\n",
        )
        .with("crates/billing/src/lib.rs", "pub fn refund() {}\n")
        .with("crates/billing/tests/full.rs", &test_file(FULL, "refunds_in_full"));
    tree.apply(&project(&tree).suite_passed("billing", "unit").unwrap());

    assert_eq!(project(&tree).report(None).to_json(), REPORT_JSON);
}

// spec: spec-engine.validation/problem-as-json
#[test]
fn a_problem_as_json_holds_code_path_line_and_message_in_that_order() {
    let report = Report::unreadable(vec![Problem {
        code: Code::MissingSpecTag,
        path: REFUNDS_PATH.to_string(),
        line: 3,
        message: "the feature has no spec tag".to_string(),
    }]);

    let json = report.to_json();

    assert!(json.contains("\"status\": \"unreadable\""), "{json}");
    let at = |field: &str| json.find(field).unwrap_or_else(|| panic!("no {field} in {json}"));
    let positions = [
        at("\"code\": \"missing_spec_tag\""),
        at(&format!("\"path\": \"{REFUNDS_PATH}\"")),
        at("\"line\": 3"),
        at("\"message\": \""),
    ];
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{json}");
}

// spec: spec-engine.validation/reads-each-file-at-most-once
#[test]
fn loading_and_reporting_read_each_file_at_most_once() {
    let mut feature = String::from(
        "@spec:billing.refunds\n@decision:keep-ledger\nFeature: Refunds return the money\n\n  @id:stands-alone @boundary:dependencies\n  Scenario: Billing uses no other component\n    Given the billing crate\n    Then none is another component\n",
    );
    let mut tests: Vec<String> = (0..12).map(|_| String::new()).collect();
    tests[0].push_str(&test_file("billing.refunds/stands-alone", "stands_alone"));
    for index in 0..38 {
        feature.push_str(&format!(
            "\n  @id:s{index}\n  Scenario: Refund {index}\n    Given a paid order of {index} EUR\n    Then the customer receives {index} EUR\n"
        ));
        tests[index % 12].push_str(&test_file(&format!("billing.refunds/s{index}"), &format!("refund_{index}")));
    }
    feature.push_str(crate::support::AUDITED);
    let mut tree = Tree::new()
        .with("alondra.toml", SETTINGS)
        .with("specs/billing/boundary.toml", BOUNDARY)
        .with(REFUNDS_PATH, &feature)
        .with(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", "accepted", &[], ""))
        .with(OVERVIEW_PATH, OVERVIEW)
        .with("shots/refund.png", "refund capture")
        .with("README.md", "# A project\n")
        .with("crates/other/src/lib.rs", "pub fn other() {}\n");
    for index in 0..300 {
        tree.put(&format!("crates/billing/src/f{index}.rs"), &format!("pub fn f{index}() {{}}\n"));
    }
    for (index, text) in tests.iter().enumerate() {
        tree.put(&format!("crates/billing/tests/t{index}.rs"), text);
    }
    record_everything(&mut tree);
    tree.forget_reads();

    let report = project(&tree).report(None);

    assert_eq!(report.status, ReportStatus::Clean, "{:#?}", report.items);
    let reads = tree.reads();
    let twice: Vec<(&String, &usize)> = reads.iter().filter(|(_, count)| **count > 1).collect();
    assert!(twice.is_empty(), "read more than once: {twice:#?}");
    assert!(tree.was_read("crates/billing/src/f299.rs"));
    assert!(tree.was_read("crates/billing/tests/t11.rs"));
    assert!(!tree.was_read("README.md"));
    assert!(!tree.was_read("crates/other/src/lib.rs"));
}
