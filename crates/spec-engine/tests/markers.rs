use alondra_spec_engine::{Code, Kind, Marker, Project, State};

use crate::support::{add_component, billing, line_of, problem, project, state};

const FULL_TEST: &str = "crates/billing/tests/full.rs";

fn markers_in<'a>(loaded: &'a Project, path: &str) -> Vec<&'a Marker> {
    loaded.markers().iter().filter(|marker| marker.path == path).collect()
}

// spec: spec-engine.markers/finds-markers-in-comments
#[test]
fn finds_a_marker_in_each_kind_of_comment() {
    for line in ["// spec: billing.refunds/full", "# spec: billing.refunds/full", "    /// spec: billing.refunds/full"] {
        let mut tree = billing();
        let text = format!("use super::*;\n\n{line}\n#[test]\nfn refunds_in_full() {{}}\n");
        tree.put(FULL_TEST, &text);

        let loaded = project(&tree);
        let found = markers_in(&loaded, FULL_TEST);

        assert_eq!(found.len(), 1, "{line}: {found:#?}");
        assert_eq!(found[0].scenario, "billing.refunds/full", "{line}");
        assert_eq!(found[0].line, 3, "{line}");
    }
}

// spec: spec-engine.markers/names-the-test-under-the-marker
#[test]
fn names_the_test_declared_after_the_attribute_lines() {
    for (declaration, name) in [
        ("fn refunds_in_full() {", "refunds_in_full"),
        ("async fn refunds_in_full() {", "refunds_in_full"),
        ("func refundsInFull() throws {", "refundsInFull"),
        ("def test_refunds_in_full(self):", "test_refunds_in_full"),
        ("test('refunds in full', async ({ page }) => {", "refunds in full"),
        ("it(\"refunds in full\", () => {", "refunds in full"),
    ] {
        let mut tree = billing();
        tree.put(FULL_TEST, &format!("// spec: billing.refunds/full\n#[test]\n#[should_panic]\n{declaration}\n}}\n"));

        let loaded = project(&tree);
        let found = markers_in(&loaded, FULL_TEST);

        assert_eq!(found.len(), 1, "{declaration}");
        assert_eq!(found[0].test.as_deref(), Some(name), "{declaration}");
    }
}

// spec: spec-engine.markers/one-test-declares-several-scenarios
#[test]
fn several_markers_above_one_test_declare_each_scenario() {
    let mut tree = billing();
    tree.remove("crates/billing/tests/partial.rs");
    tree.put(FULL_TEST, "// spec: billing.refunds/full\n// spec: billing.refunds/partial\n#[test]\nfn refunds() {}\n");

    let loaded = project(&tree);
    let found: Vec<(&str, Option<&str>)> =
        markers_in(&loaded, FULL_TEST).iter().map(|marker| (marker.scenario.as_str(), marker.test.as_deref())).collect();

    assert_eq!(found, [("billing.refunds/full", Some("refunds")), ("billing.refunds/partial", Some("refunds"))]);
    assert_eq!(state(&loaded.report(None), "billing.refunds/partial", Kind::Passes), State::Missing);
}

// spec: spec-engine.markers/several-tests-declare-one-scenario
#[test]
fn a_scenario_declared_in_two_files_lists_both_in_path_order() {
    let tree = billing()
        .with("crates/billing/tests/again.rs", "// spec: billing.refunds/full\n#[test]\nfn refunds_again() {}\n")
        .reversed();

    let loaded = project(&tree);
    let paths: Vec<&str> = loaded
        .markers()
        .iter()
        .filter(|marker| marker.scenario == "billing.refunds/full")
        .map(|marker| marker.path.as_str())
        .collect();

    assert_eq!(paths, ["crates/billing/tests/again.rs", FULL_TEST]);
}

// spec: spec-engine.markers/orphan-marker-is-a-problem
#[test]
fn a_marker_for_a_scenario_that_does_not_exist_is_refused() {
    let text = "use super::*;\n// spec: billing.refunds/gone\n#[test]\nfn refunds_nothing() {}\n";
    let tree = billing().with("crates/billing/tests/gone.rs", text);

    let found = problem(&tree);

    assert_eq!(found.code, Code::OrphanMarker);
    assert_eq!(found.code.as_str(), "orphan_marker");
    assert_eq!(found.path, "crates/billing/tests/gone.rs");
    assert_eq!(found.line, line_of(text, "spec: billing.refunds/gone"));
}

// spec: spec-engine.markers/only-suite-tests-are-searched
#[test]
fn a_file_no_suite_names_is_never_read() {
    let outside = "tools/scratch/gone.rs";
    let tree = billing().with(outside, "// spec: billing.refunds/gone\n#[test]\nfn refunds_nothing() {}\n");

    let loaded = project(&tree);

    assert!(loaded.markers().iter().all(|marker| marker.path != outside));
    assert!(tree.was_read(FULL_TEST));
    assert!(!tree.was_read(outside));
}

// spec: spec-engine.markers/malformed-marker-is-a-problem
#[test]
fn a_marker_without_spec_and_scenario_is_refused() {
    let text = "use super::*;\n// spec: billing.refunds\n#[test]\nfn refunds_something() {}\n";
    let tree = billing().with("crates/billing/tests/bad.rs", text);

    let found = problem(&tree);

    assert_eq!(found.code, Code::InvalidMarker);
    assert_eq!(found.path, "crates/billing/tests/bad.rs");
    assert_eq!(found.line, line_of(text, "spec: billing.refunds"));
}

// spec: spec-engine.markers/test-of-another-component-is-a-problem
#[test]
fn a_test_that_declares_a_scenario_of_another_component_is_refused() {
    let text = "use super::*;\n// spec: accounts.core/stands-alone\n#[test]\nfn accounts_stand_alone() {}\n";
    let mut tree = billing().with("crates/billing/tests/accounts.rs", text);
    add_component(&mut tree, "accounts", "");

    let found = problem(&tree);

    assert_eq!(found.code, Code::MarkerOutsideComponent);
    assert_eq!(found.path, "crates/billing/tests/accounts.rs");
    assert_eq!(found.line, line_of(text, "spec: accounts.core/stands-alone"));
}
