use alondra_spec_engine::{Code, Component, Files, Kind, Problem, ReportStatus, State, Suite};

use crate::support::{
    BOUNDARY, BOUNDARY_PATH, REFUNDS_PATH, Tree, add_component, billing, boundary_keys, line_of, padded, problem, problem_with, problems,
    project, promised, record_everything, state,
};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn boundary_problem(tree: &Tree, code: Code) -> Problem {
    let found = problem(tree);
    assert_eq!(found.code, code, "{found:#?}");
    assert_eq!(found.path, BOUNDARY_PATH);
    found
}

// spec: spec-engine.boundary/reads-boundary
#[test]
fn reads_every_value_of_a_complete_boundary() {
    let mut tree = promised();
    tree.edit(
        BOUNDARY_PATH,
        "tests = [\"crates/billing/tests/**/*.rs\"]\n",
        "tests = [\"crates/billing/tests/**/*.rs\"]\nresults = \"bazel-testlogs/**/test.xml\"\ntimeout_minutes = 20\n",
    );

    let loaded = project(&tree);
    let billing = loaded.components().iter().find(|component| component.id == "billing").unwrap();

    assert_eq!(
        *billing,
        Component {
            id: "billing".to_string(),
            path: BOUNDARY_PATH.to_string(),
            owns: strings(&["crates/billing/**"]),
            regenerates: strings(&["crates/billing/src/**"]),
            inputs: strings(&["Cargo.toml"]),
            depends_on: strings(&["accounts"]),
            data: strings(&["crates/billing/data/**"]),
            fixtures: strings(&["crates/billing/fixtures/**"]),
            surface: Some("specs/billing/surface.txt".to_string()),
            suites: vec![
                Suite {
                    id: "unit".to_string(),
                    run: strings(&["bazelisk", "test", "//crates/billing:test"]),
                    tests: strings(&["crates/billing/tests/**/*.rs"]),
                    results: Some("bazel-testlogs/**/test.xml".to_string()),
                    timeout_minutes: 20,
                },
                Suite {
                    id: "contract".to_string(),
                    run: strings(&["python3", "crates/billing/contract_test.py"]),
                    tests: strings(&["crates/billing/contract_test.py"]),
                    results: None,
                    timeout_minutes: 10,
                },
            ],
        }
    );
}

// spec: spec-engine.boundary/spec-belongs-to-the-component-of-its-folder
#[test]
fn a_feature_file_belongs_to_the_component_of_its_folder() {
    let mut tree = billing();
    add_component(&mut tree, "accounts", "");

    let loaded = project(&tree);
    let owners: Vec<(&str, &str)> =
        loaded.specs().iter().map(|loaded| (loaded.path.as_str(), loaded.component.as_str())).collect();

    assert!(owners.contains(&(REFUNDS_PATH, "billing")), "{owners:#?}");
    assert!(owners.contains(&("specs/accounts/core.feature", "accounts")), "{owners:#?}");
}

// spec: spec-engine.boundary/spec-without-component-is-a-problem
#[test]
fn a_feature_file_in_a_folder_without_boundary_is_refused() {
    let tree = billing().with(
        "specs/orphan/refunds.feature",
        "@spec:orphan.refunds\nFeature: Orphan refunds\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order\n",
    );

    let found = problem(&tree);

    assert_eq!(found.code, Code::SpecWithoutComponent);
    assert_eq!(found.path, "specs/orphan/refunds.feature");
}

// spec: spec-engine.boundary/folder-and-id-must-agree
#[test]
fn a_boundary_whose_id_is_not_its_folder_is_refused() {
    let mut tree = billing();
    tree.edit(BOUNDARY_PATH, "id = \"billing\"", "id = \"payments\"");

    let found = problem_with(&tree, Code::ComponentIdMismatch);

    assert_eq!(found.path, BOUNDARY_PATH);
    assert_eq!(found.line, line_of(&tree.text(BOUNDARY_PATH), "id = \"payments\""));
}

// spec: spec-engine.boundary/unknown-key-is-a-problem
#[test]
fn an_unknown_boundary_key_is_refused_at_its_line() {
    let mut tree = billing();
    boundary_keys(&mut tree, "billing", "owner = \"payments team\"\n");

    let found = boundary_problem(&tree, Code::InvalidBoundary);

    assert_eq!(found.line, line_of(&tree.text(BOUNDARY_PATH), "owner ="));
}

// spec: spec-engine.boundary/ledger-inside-component-is-a-problem
#[test]
fn a_component_that_owns_or_reads_the_ledger_is_refused() {
    let mut owning = billing();
    owning.edit(BOUNDARY_PATH, "owns = [\"crates/billing/**\"]", "owns = [\"crates/billing/**\", \"specs/**\"]");
    let mut reading = billing();
    boundary_keys(&mut reading, "billing", "inputs = [\"specs/**\"]\n");

    for tree in [owning, reading] {
        boundary_problem(&tree, Code::LedgerInsideComponent);
    }
}

// spec: spec-engine.boundary/ledger-reach-is-read-from-the-pattern
#[test]
fn whether_a_pattern_reaches_the_ledger_is_read_from_the_pattern_itself() {
    let rows = [
        ("owns", "specs/ledger/billing/**", true),
        ("owns", "specs/*/billing/*.toml", true),
        ("inputs", "**/*.toml", true),
        ("owns", "specs/ledger", false),
        ("owns", "specs/notes/**", false),
        ("data", "specs/ledger/**", false),
    ];
    for (list, pattern, refused) in rows {
        let mut tree = if list == "data" { promised() } else { billing() };
        match list {
            "owns" => tree.edit(BOUNDARY_PATH, "owns = [\"crates/billing/**\"]", &format!("owns = [\"crates/billing/**\", \"{pattern}\"]")),
            "inputs" => boundary_keys(&mut tree, "billing", &format!("inputs = [\"{pattern}\"]\n")),
            _ => tree.edit(BOUNDARY_PATH, "data = [\"crates/billing/data/**\"]", &format!("data = [\"{pattern}\"]")),
        }
        assert!(tree.paths().iter().all(|path| !path.starts_with("specs/ledger/")), "{pattern}");

        if refused {
            let found = boundary_problem(&tree, Code::LedgerInsideComponent);
            assert_eq!(found.line, line_of(&tree.text(BOUNDARY_PATH), pattern), "{pattern}");
        } else {
            project(&tree);
        }
    }
}

// spec: spec-engine.boundary/oversized-boundary-is-a-problem
#[test]
fn a_boundary_beyond_the_size_limit_is_refused() {
    let mut fits = billing();
    fits.put(BOUNDARY_PATH, &padded(BOUNDARY, '#', 65_536));
    project(&fits);

    let mut beyond = billing();
    beyond.put(BOUNDARY_PATH, &padded(BOUNDARY, '#', 65_537));

    boundary_problem(&beyond, Code::TooLarge);
}

// spec: spec-engine.boundary/regenerated-files-must-be-owned
#[test]
fn regenerating_files_the_component_does_not_own_is_refused() {
    let mut tree = billing();
    tree.edit(BOUNDARY_PATH, "regenerates = [\"crates/billing/src/**\"]", "regenerates = [\"crates/ledger/src/**\"]");

    boundary_problem(&tree, Code::RegeneratesOutsideOwns);
}

// spec: spec-engine.boundary/unknown-dependency-is-a-problem
#[test]
fn a_dependency_on_a_missing_component_is_refused() {
    let mut tree = billing();
    boundary_keys(&mut tree, "billing", "depends_on = [\"ledger\"]\n");

    let found = boundary_problem(&tree, Code::UnknownComponent);

    assert_eq!(found.code.as_str(), "unknown_component");
    assert_eq!(found.line, line_of(&tree.text(BOUNDARY_PATH), "depends_on ="));
}

// spec: spec-engine.boundary/dependency-cycle-is-a-problem
#[test]
fn two_components_that_depend_on_each_other_are_refused() {
    let mut tree = billing();
    add_component(&mut tree, "accounts", "depends_on = [\"billing\"]\n");
    boundary_keys(&mut tree, "billing", "depends_on = [\"accounts\"]\n");

    let found = problems(&tree);
    let cycles: Vec<(&str, &str)> = found
        .iter()
        .filter(|problem| problem.code == Code::DependencyCycle)
        .map(|problem| (problem.path.as_str(), problem.message.as_str()))
        .collect();

    assert_eq!(
        cycles,
        [("specs/accounts/boundary.toml", "the components accounts, billing depend on each other")],
        "{found:#?}"
    );
}

// spec: spec-engine.boundary/duplicate-suite-is-a-problem
#[test]
fn two_suites_with_one_id_are_refused() {
    let mut tree = billing();
    let text = tree.text(BOUNDARY_PATH);
    tree.put(
        BOUNDARY_PATH,
        &format!("{text}\n[[suite]]\nid = \"unit\"\nrun = [\"cargo\", \"test\"]\ntests = [\"crates/billing/src/**/*.rs\"]\n"),
    );

    let found = boundary_problem(&tree, Code::DuplicateSuiteId);

    assert_eq!(found.line, line_of(&tree.text(BOUNDARY_PATH), "run = [\"cargo\"") - 1);
}

// spec: spec-engine.boundary/suite-needs-a-command
#[test]
fn a_suite_with_nothing_to_run_is_refused() {
    let mut tree = billing();
    tree.edit(BOUNDARY_PATH, "run = [\"bazelisk\", \"test\", \"//crates/billing:test\"]", "run = []");

    let found = boundary_problem(&tree, Code::InvalidBoundary);

    assert_eq!(found.line, line_of(&tree.text(BOUNDARY_PATH), "run = []"));
}

// spec: spec-engine.boundary/pattern-outside-repository-is-a-problem
#[test]
fn a_pattern_that_leaves_the_repository_is_refused() {
    for pattern in ["../other/**", "/usr/lib/**", "crates/../../x"] {
        let mut tree = billing();
        tree.edit(BOUNDARY_PATH, "owns = [\"crates/billing/**\"]", &format!("owns = [\"crates/billing/**\", \"{pattern}\"]"));

        let found = problem(&tree);

        assert_eq!(found.code, Code::PathOutsideRepository, "{pattern}");
        assert_eq!(found.path, BOUNDARY_PATH, "{pattern}");
        assert_eq!(found.line, line_of(&tree.text(BOUNDARY_PATH), "owns ="), "{pattern}");
    }
}

// spec: spec-engine.boundary/promise-without-scenario-is-a-problem
#[test]
fn a_promise_no_scenario_states_is_refused() {
    let mut surface = billing().with("specs/billing/surface.txt", "pub fn refund()\n");
    boundary_keys(&mut surface, "billing", "surface = \"specs/billing/surface.txt\"\n");
    let mut data = billing();
    boundary_keys(&mut data, "billing", "data = [\"crates/billing/data/**\"]\n");
    let mut dependencies = billing();
    dependencies.edit(REFUNDS_PATH, "@id:stands-alone @boundary:dependencies", "@id:stands-alone");

    for (promise, tree) in [("surface", surface), ("data", data), ("dependencies", dependencies)] {
        let found = problem(&tree);
        assert_eq!(found.code, Code::BoundaryWithoutScenario, "{promise}: {found:#?}");
        assert_eq!(found.path, BOUNDARY_PATH, "{promise}");
    }
    assert_eq!(Code::BoundaryWithoutScenario.as_str(), "boundary_without_scenario");
}

// spec: spec-engine.boundary/promise-with-scenario-loads
#[test]
fn promises_stated_by_scenarios_load_and_are_validated_like_any_other() {
    let mut tree = promised();
    let before = project(&tree).report(None);
    for subject in ["billing.promises/surface", "billing.promises/data", "billing.refunds/stands-alone"] {
        assert_eq!(state(&before, subject, Kind::Adequate), State::Missing, "{subject}");
    }

    record_everything(&mut tree);
    let after = project(&tree).report(None);

    assert_eq!(after.status, ReportStatus::Clean);
    for subject in ["billing.promises/surface", "billing.promises/data", "billing.refunds/stands-alone"] {
        assert_eq!(state(&after, subject, Kind::Passes), State::Valid, "{subject}");
        assert_eq!(state(&after, subject, Kind::Adequate), State::Valid, "{subject}");
    }
}
