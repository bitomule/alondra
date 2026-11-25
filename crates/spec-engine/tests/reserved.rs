use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use alondra_spec_engine::matches;
use proptest::prelude::*;

use crate::support::{fingerprints, project, rich};

fn answers_in_time(pattern: String, path: String) -> bool {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(matches(&pattern, &path));
    });
    receiver.recv_timeout(Duration::from_secs(10)).expect("the pattern took more than ten seconds")
}

// spec: spec-engine.patterns/matching-time-stays-polynomial
#[test]
fn patterns_full_of_stars_answer_at_once() {
    let mut stars = vec!["**"; 30];
    stars.push("x");
    let path = vec!["a"; 40].join("/");
    assert!(!answers_in_time(stars.join("/"), path));

    assert!(!answers_in_time("*a*a*a*a*a*a*a*a*a*a*b".to_string(), "a".repeat(60)));
    assert!(answers_in_time("*a*a*a*a*a*a*a*a*a*a*b".to_string(), format!("{}b", "a".repeat(60))));
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x5bec),
        ..ProptestConfig::default()
    })]

    // spec: spec-engine.fingerprints/listing-order-does-not-matter
    #[test]
    fn listing_the_files_in_another_order_changes_no_fingerprint(
        files in prop::collection::btree_map("[a-z]{1,8}", "[a-z ]{0,24}", 0..12),
        seed in any::<u64>(),
    ) {
        let mut tree = rich();
        for (name, text) in &files {
            tree.put(&format!("crates/billing/src/{name}.rs"), text);
            tree.put(&format!("shots/{name}.png"), text);
        }
        let listed = fingerprints(&project(&tree));

        prop_assert_eq!(&fingerprints(&project(&tree.clone().shuffled(seed))), &listed);
        prop_assert_eq!(&fingerprints(&project(&tree.reversed())), &listed);
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x5bec),
        ..ProptestConfig::default()
    })]

    // spec: spec-engine.validation/report-is-deterministic
    #[test]
    fn the_report_does_not_depend_on_the_listing_order(
        files in prop::collection::btree_map("[a-z]{1,8}", "[a-z ]{0,24}", 0..12),
        seed in any::<u64>(),
    ) {
        let mut tree = rich()
            .with("specs/ledger/billing/scenarios/billing.refunds/gone.toml", "[adequate]\ninputs = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\nverdict = \"met\"\n")
            .with("specs/ledger/billing/suites/gone.toml", "inputs = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\n");
        for (name, text) in &files {
            tree.put(&format!("crates/billing/src/{name}.rs"), text);
            tree.put(&format!("shots/{name}.png"), text);
        }
        tree.apply(&project(&tree).suite_passed("billing", "unit").unwrap());
        let listed = project(&tree).report(None).to_json();

        prop_assert_eq!(&project(&tree.clone().shuffled(seed)).report(None).to_json(), &listed);
        prop_assert_eq!(&project(&tree.reversed()).report(None).to_json(), &listed);
    }
}
