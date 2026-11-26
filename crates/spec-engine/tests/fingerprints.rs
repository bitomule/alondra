use alondra_spec_engine::Kind;

use crate::support::{
    BOUNDARY_PATH, REFUNDS_PATH, Tree, add_component, add_contract_suite, billing, boundary_keys, fingerprints, is_fingerprint,
    project, rich,
};

const LIB: &str = "crates/billing/src/lib.rs";

fn of(tree: &Tree, subject: &str, kind: Kind) -> String {
    project(tree).fingerprint(subject, kind).unwrap_or_else(|| panic!("no {kind:?} fingerprint for {subject}"))
}

fn suite(tree: &Tree) -> String {
    of(tree, "billing/unit", Kind::Suite)
}

// spec: spec-engine.fingerprints/owned-file-changes-the-suite
#[test]
fn editing_an_owned_file_changes_the_suite() {
    let mut tree = billing();
    let before = suite(&tree);

    tree.put(LIB, "pub fn refund() { todo!() }\n");

    assert_ne!(suite(&tree), before);
}

// spec: spec-engine.fingerprints/input-file-changes-the-suite
#[test]
fn editing_an_input_changes_the_suite() {
    let mut tree = billing().with("Cargo.toml", "[workspace]\n");
    boundary_keys(&mut tree, "billing", "inputs = [\"Cargo.toml\"]\n");
    let before = suite(&tree);

    tree.put("Cargo.toml", "[workspace]\nmembers = []\n");

    assert_ne!(suite(&tree), before);
}

// spec: spec-engine.fingerprints/unrelated-file-keeps-the-suite
#[test]
fn editing_a_file_the_component_neither_owns_nor_names_keeps_the_suite() {
    let mut tree = billing().with("crates/ledger/src/lib.rs", "pub fn keep() {}\n");
    let before = suite(&tree);

    tree.put("crates/ledger/src/lib.rs", "pub fn keep() { todo!() }\n");

    assert_eq!(suite(&tree), before);
}

// spec: spec-engine.fingerprints/dependency-changes-the-suite
#[test]
fn editing_a_component_it_depends_on_through_another_changes_the_suite() {
    let mut tree = billing();
    add_component(&mut tree, "money", "");
    add_component(&mut tree, "accounts", "depends_on = [\"money\"]\n");
    boundary_keys(&mut tree, "billing", "depends_on = [\"accounts\"]\n");
    let before = suite(&tree);

    tree.put("crates/money/src/lib.rs", "pub fn run() { todo!() }\n");

    assert_ne!(suite(&tree), before);
}

// spec: spec-engine.fingerprints/command-changes-the-suite
#[test]
fn changing_a_command_changes_only_that_suite() {
    let mut tree = billing();
    add_contract_suite(&mut tree);
    let unit = suite(&tree);
    let contract = of(&tree, "billing/contract", Kind::Suite);

    tree.edit(BOUNDARY_PATH, "\"//crates/billing:test\"]", "\"//crates/billing:test\", \"--config=ci\"]");

    assert_ne!(suite(&tree), unit);
    assert_eq!(of(&tree, "billing/contract", Kind::Suite), contract);
}

// spec: spec-engine.fingerprints/new-file-changes-the-suite
#[test]
fn adding_an_owned_file_changes_the_suite_and_removing_it_restores_it() {
    let mut tree = billing();
    let first = suite(&tree);

    tree.put("crates/billing/src/tax.rs", "pub fn tax() {}\n");
    let added = suite(&tree);
    tree.remove("crates/billing/src/tax.rs");

    assert_ne!(added, first);
    assert_eq!(suite(&tree), first);
}

const SUITE_TESTS: &str = "tests = [\"crates/billing/tests/**/*.rs\"]";
const WIDER_TESTS: &str = "tests = [\"crates/billing/tests/**/*.rs\", \"tests/billing/*.rs\"]";
const EXTRA: &str = "tests/billing/extra.rs";

fn with_tests_outside_the_component() -> Tree {
    let mut tree = billing().with(EXTRA, "#[test]\nfn refunds_twice() {}\n");
    tree.edit(BOUNDARY_PATH, SUITE_TESTS, WIDER_TESTS);
    tree
}

// spec: spec-engine.fingerprints/suite-test-file-changes-the-suite
#[test]
fn editing_or_emptying_a_test_file_of_the_suite_changes_the_suite() {
    let mut tree = with_tests_outside_the_component();
    let first = suite(&tree);

    tree.put(EXTRA, "#[test]\nfn refunds_twice() {\n    assert!(false);\n}\n");
    let edited = suite(&tree);
    tree.put(EXTRA, "");
    let emptied = suite(&tree);

    assert_ne!(edited, first);
    assert_ne!(emptied, edited);
    assert_ne!(emptied, first);
}

// spec: spec-engine.fingerprints/new-suite-test-file-changes-the-suite
#[test]
fn adding_a_test_file_of_the_suite_or_changing_its_patterns_changes_the_suite() {
    let mut tree = with_tests_outside_the_component();
    let first = suite(&tree);

    tree.put("tests/billing/more.rs", "#[test]\nfn refunds_again() {}\n");
    let added = suite(&tree);
    tree.edit(BOUNDARY_PATH, "\"tests/billing/*.rs\"", "\"tests/billing/**/*.rs\"");

    assert_ne!(added, first);
    assert_ne!(suite(&tree), added);
}

// spec: spec-engine.fingerprints/scenario-text-changes-only-that-scenario
#[test]
fn rewording_a_step_changes_only_the_adequacy_of_its_scenario() {
    let mut tree = billing();
    let full = of(&tree, "billing.refunds/full", Kind::Adequate);
    let partial = of(&tree, "billing.refunds/partial", Kind::Adequate);
    let unit = suite(&tree);

    tree.edit(REFUNDS_PATH, "When the order is refunded", "When the whole order is refunded");

    assert_ne!(of(&tree, "billing.refunds/full", Kind::Adequate), full);
    assert_eq!(of(&tree, "billing.refunds/partial", Kind::Adequate), partial);
    assert_eq!(suite(&tree), unit);
}

// spec: spec-engine.fingerprints/spec-header-changes-every-scenario
#[test]
fn changing_the_description_or_the_background_changes_every_scenario() {
    let header = "@spec:billing.refunds\nFeature: Refunds return the money\n  A refund returns what the customer paid.\n\n  Background:\n    Given a paid order of 30 EUR\n";
    let mut base = billing();
    base.edit(REFUNDS_PATH, "@spec:billing.refunds\nFeature: Refunds return the money\n", header);

    for (from, to) in [
        ("A refund returns what the customer paid.", "A refund returns all the customer paid."),
        ("  Background:\n    Given a paid order of 30 EUR\n", "  Background:\n    Given a paid order of 40 EUR\n"),
    ] {
        let mut tree = base.clone();
        let full = of(&tree, "billing.refunds/full", Kind::Adequate);
        let partial = of(&tree, "billing.refunds/partial", Kind::Adequate);

        tree.edit(REFUNDS_PATH, from, to);

        assert_ne!(of(&tree, "billing.refunds/full", Kind::Adequate), full, "{from}");
        assert_ne!(of(&tree, "billing.refunds/partial", Kind::Adequate), partial, "{from}");
    }
}

// spec: spec-engine.fingerprints/test-file-changes-the-scenarios-it-declares
#[test]
fn editing_a_test_file_changes_only_the_scenarios_it_declares() {
    let mut tree = billing();
    let full = of(&tree, "billing.refunds/full", Kind::Adequate);
    let partial = of(&tree, "billing.refunds/partial", Kind::Adequate);

    tree.edit("crates/billing/tests/full.rs", "fn refunds_in_full() {}", "fn refunds_in_full() {\n    assert!(true);\n}");

    assert_ne!(of(&tree, "billing.refunds/full", Kind::Adequate), full);
    assert_eq!(of(&tree, "billing.refunds/partial", Kind::Adequate), partial);
}

// spec: spec-engine.fingerprints/evidence-changes-the-judgment
#[test]
fn replacing_a_capture_changes_the_judgment_and_editing_sources_does_not() {
    let mut tree = billing().with("shots/refund-full.png", "full capture").with("shots/refund-partial.png", "partial capture");
    tree.edit(REFUNDS_PATH, "@id:full\n", "@id:full @judged:capture @evidence:shots/refund*.png\n");
    let before = of(&tree, "billing.refunds/full", Kind::Judged);

    tree.put(LIB, "pub fn refund() { todo!() }\n");
    assert_eq!(of(&tree, "billing.refunds/full", Kind::Judged), before);

    tree.put("shots/refund-partial.png", "partial capture, retaken");
    assert_ne!(of(&tree, "billing.refunds/full", Kind::Judged), before);
}

// spec: spec-engine.fingerprints/judgment-without-evidence-follows-the-component
#[test]
fn a_judgment_without_an_evidence_pattern_follows_the_component() {
    for tag in ["@judged", "@judged:capture", "@judged:video"] {
        let mut tree = billing();
        tree.edit(REFUNDS_PATH, "@id:full\n", &format!("@id:full {tag}\n"));
        let before = of(&tree, "billing.refunds/full", Kind::Judged);

        tree.put(LIB, "pub fn refund() { todo!() }\n");

        assert_ne!(of(&tree, "billing.refunds/full", Kind::Judged), before, "{tag}");
    }
}

// spec: spec-engine.fingerprints/unlisted-file-never-counts
#[test]
fn a_file_the_caller_does_not_list_changes_no_fingerprint() {
    let listed = fingerprints(&project(&rich()));
    let with_unlisted = rich().unlisted("crates/billing/target/debug/out.bin", "build output");

    assert!(!listed.is_empty());
    assert_eq!(fingerprints(&project(&with_unlisted)), listed);
}

// spec: spec-engine.fingerprints/fingerprint-format
#[test]
fn every_fingerprint_is_sha256_and_64_lowercase_hex_digits() {
    let found = fingerprints(&project(&rich()));
    let kinds: Vec<Kind> = found.keys().map(|(_, kind)| *kind).collect();

    for kind in [Kind::Suite, Kind::Adequate, Kind::Judged, Kind::Reference] {
        assert!(kinds.contains(&kind), "no {kind:?} among {found:#?}");
    }
    for ((subject, kind), fingerprint) in &found {
        let fingerprint = fingerprint.as_deref().unwrap_or_else(|| panic!("no {kind:?} fingerprint for {subject}"));
        assert!(is_fingerprint(fingerprint), "{subject} {kind:?}: {fingerprint}");
    }
}
