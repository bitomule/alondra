use alondra_spec_engine::{Code, write_spec};
use proptest::prelude::*;

use crate::support::{
    BOUNDARY_PATH, REFUNDS_PATH, Tree, billing, boundary_keys, decision_file, decision_path, project, promised, rich, section_text,
};

const HEADINGS: [&str; 6] = [
    "What you may write",
    "What you must not change",
    "Boundary",
    "Decisions",
    "Specs",
    "Reference (context, not a requirement)",
];

fn packet(tree: &Tree) -> String {
    project(tree).packet("billing").unwrap()
}

fn heading_at(packet: &str, heading: &str) -> usize {
    packet.find(&format!("## {heading}\n")).unwrap_or_else(|| panic!("no section {heading} in:\n{packet}"))
}

/// The text of a section of the packet, from its heading to the next section of the packet.
fn section<'a>(packet: &'a str, heading: &str) -> &'a str {
    let start = heading_at(packet, heading);
    let end = HEADINGS
        .iter()
        .filter_map(|other| packet[start + 1..].find(&format!("\n## {other}\n")).map(|at| start + 1 + at))
        .min()
        .unwrap_or(packet.len());
    &packet[start..end]
}

// spec: spec-engine.packet/states-that-specs-win
#[test]
fn the_second_paragraph_says_that_specs_win() {
    let text = packet(&billing());
    let paragraphs: Vec<&str> = text.split("\n\n").collect();

    assert_eq!(
        paragraphs[1],
        "Specs are the only source of truth. Where any other text disagrees with a spec, the spec wins."
    );
}

// spec: spec-engine.packet/says-what-may-be-written
#[test]
fn says_which_files_may_be_written_and_which_must_not_change() {
    let mut tree = billing();
    boundary_keys(&mut tree, "billing", "fixtures = [\"crates/billing/fixtures/**\"]\n");
    let text = packet(&tree);

    let may = section(&text, "What you may write");
    assert!(may.contains("crates/billing/src/**"), "{may}");

    let must_not = section(&text, "What you must not change");
    for named in [BOUNDARY_PATH, "crates/billing/tests/**/*.rs", "crates/billing/fixtures/**", "specs/ledger"] {
        assert!(must_not.contains(named), "{named} is not in:\n{must_not}");
    }
    assert!(must_not.contains("refunds.feature") || must_not.contains("*.feature"), "no specs in:\n{must_not}");
}

// spec: spec-engine.packet/carries-the-specs-in-canonical-form
#[test]
fn the_specs_section_holds_each_spec_in_canonical_form_in_id_order() {
    let audit = "@spec:billing.audit\nFeature: Every refund is audited\n\n  @id:logged\n  Scenario: A refund is logged\n    Given a refund\n    Then it is logged\n";
    let tree = billing().with("specs/billing/zz-audit.feature", audit);
    let loaded = project(&tree);
    let text = loaded.packet("billing").unwrap();
    let specs = section(&text, "Specs");

    let canonical = |id: &str| write_spec(&loaded.specs().iter().find(|loaded| loaded.spec.id == id).unwrap().spec);
    let audit_at = specs.find(&canonical("billing.audit")).unwrap_or_else(|| panic!("billing.audit is not in:\n{specs}"));
    let refunds_at = specs.find(&canonical("billing.refunds")).unwrap_or_else(|| panic!("billing.refunds is not in:\n{specs}"));
    assert!(audit_at < refunds_at);
}

// spec: spec-engine.packet/leaves-reserved-scenarios-out
#[test]
fn a_reserved_scenario_never_reaches_the_packet() {
    let mut tree = billing();
    let text = tree.text(REFUNDS_PATH);
    tree.put(
        REFUNDS_PATH,
        &format!(
            "{text}\n  @id:closed-account @reserved\n  Scenario: Refund to a closed account\n    Given an account closed last year\n    Then the refund waits for a manual review\n"
        ),
    );

    let text = packet(&tree);

    assert!(text.contains("Full refund"), "{text}");
    for hidden in ["Refund to a closed account", "an account closed last year", "the refund waits for a manual review", "closed-account"] {
        assert!(!text.contains(hidden), "{hidden} is in the packet:\n{text}");
    }
}

// spec: spec-engine.packet/carries-the-parts
#[test]
fn carries_the_text_of_each_diagram_and_the_path_of_each_mockup() {
    let mut tree = billing();
    assert!(!packet(&tree).contains("diagrams and mockups"));
    tree.edit(REFUNDS_PATH, "Feature:", "@diagram:flow.mmd\nFeature:");
    tree.edit(REFUNDS_PATH, "@id:full", "@id:full @mockup:sheet.png");
    tree.edit(REFUNDS_PATH, "@id:partial", "@id:partial @mockup:sheet.png");
    tree.edit(REFUNDS_PATH, "@id:stands-alone", "@id:stands-alone @reserved @mockup:secret.png");
    tree.put("specs/billing/flow.mmd", "graph TD\n  a --> b\n%% ``` fenced\n");
    let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    png.extend([0, 0, 0, 10, 0, 0, 0, 20]);
    tree.put_bytes("specs/billing/sheet.png", &png);
    tree.put_bytes("specs/billing/secret.png", &png);

    let text = packet(&tree);

    let specs = section(&text, "Specs");
    let diagram = "Diagram specs/billing/flow.mmd:\n\n````mermaid\ngraph TD\n  a --> b\n%% ``` fenced\n````";
    let mockup = "Mockup specs/billing/sheet.png (10×20 pixels)";
    assert_eq!(specs.matches(diagram).count(), 1, "{specs}");
    assert_eq!(specs.matches(mockup).count(), 1, "{specs}");
    for part in [diagram, mockup] {
        assert!(specs.find("@spec:billing.refunds").unwrap() < specs.find(part).unwrap());
    }
    assert!(!text.contains("secret.png"), "{text}");
    assert!(section(&text, "What you must not change").contains("*.feature and the diagrams and mockups they cite"));
}

// spec: spec-engine.packet/carries-the-boundary
#[test]
fn the_boundary_section_lists_every_value_and_each_suite_with_its_command() {
    let text = packet(&promised());
    let boundary = section(&text, "Boundary");

    for value in [
        "Cargo.toml",
        "accounts",
        "crates/billing/data/**",
        "crates/billing/fixtures/**",
        "specs/billing/surface.txt",
        "suite unit: bazelisk test //crates/billing:test",
        "suite contract: python3 crates/billing/contract_test.py",
    ] {
        assert!(boundary.contains(value), "{value} is not in:\n{boundary}");
    }
}

// spec: spec-engine.packet/carries-cited-decisions
#[test]
fn the_decisions_section_holds_only_the_decisions_the_specs_cite() {
    let tree = rich().with(&decision_path("drop-cache"), &decision_file("drop-cache", "Drop the cache", "accepted", &[], ""));
    let text = packet(&tree);
    let decisions = section(&text, "Decisions");

    assert!(decisions.contains("Keep a ledger"), "{decisions}");
    assert!(decisions.contains(&section_text("keep-ledger", "Decision")), "{decisions}");
    assert!(!text.contains("Drop the cache"), "{text}");
    assert!(!text.contains(&section_text("drop-cache", "Decision")), "{text}");
}

// spec: spec-engine.packet/marks-reference-as-context
#[test]
fn reference_comes_last_and_is_labelled_as_context() {
    let mut tree = rich();
    boundary_keys(&mut tree, "billing", "fixtures = [\"crates/billing/fixtures/**\"]\n");
    let text = packet(&tree);

    let positions: Vec<usize> = HEADINGS.iter().map(|heading| heading_at(&text, heading)).collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "sections out of order:\n{text}");
    let reference = section(&text, "Reference (context, not a requirement)");
    assert!(reference.contains("A refund returns what the customer paid."), "{reference}");
}

// spec: spec-engine.packet/unknown-component
#[test]
fn the_packet_of_a_missing_component_is_refused() {
    let refused = project(&billing()).packet("ledger").unwrap_err();

    assert_eq!(refused.code, Code::UnknownComponent);
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x5bec),
        ..ProptestConfig::default()
    })]

    // spec: spec-engine.packet/packet-is-deterministic
    #[test]
    fn the_packet_does_not_depend_on_the_listing_order(
        names in prop::collection::btree_set("[a-z]{1,6}", 0..5),
        seed in any::<u64>(),
    ) {
        let mut tree = rich();
        for name in &names {
            tree.put(
                &format!("specs/billing/extra-{name}.feature"),
                &format!("@spec:billing.extra-{name}\nFeature: Extra {name}\n\n  @id:works\n  Scenario: It works\n    Given {name}\n"),
            );
            tree.put(&format!("crates/billing/src/{name}.rs"), name);
        }
        let listed = packet(&tree);

        prop_assert_eq!(&packet(&tree.clone().shuffled(seed)), &listed);
        prop_assert_eq!(&packet(&tree.reversed()), &listed);
    }
}
