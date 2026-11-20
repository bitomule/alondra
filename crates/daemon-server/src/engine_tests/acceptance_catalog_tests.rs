//! The shared domain v2 acceptance catalog against the tests of every crate that gives evidence for
//! it: the store's runners, the support crate's cutover, alondrad's API and parity tests, and the
//! domain's. Only alondrad sees all of them.
use alondra_domain_rules::acceptance_cases::{CASES, SCHEDULE_CASES};
use serde_json::Value;
use std::collections::BTreeSet;

const CATALOG: &str = include_str!("../../../../fixtures/domain-v2/acceptance.json");

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .get(key)
        .unwrap_or_else(|| panic!("catalog field {key} missing in {value}"))
}
fn string(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("expected string: {value}"))
}

/// Every catalog scenario states how it is executed, and the statement holds:
/// runner cases are exactly the listed ones, evidence names existing tests, and
/// pending scenarios say which layer they wait for.
#[test]
fn catalog_execution_status_is_explicit_and_evidenced() {
    super::wired();
    let catalog: Value = serde_json::from_str(CATALOG).unwrap();
    let legend = field(&catalog, "execution_status_legend")
        .as_object()
        .unwrap();
    let sources = [
        include_str!("../../../store-sqlite/src/tests.rs"),
        include_str!("../../../store-sqlite/src/schedule_acceptance_tests.rs"),
        include_str!("../../../daemon-support/src/domain_cutover.rs"),
        include_str!("../../../daemon-support/src/domain_authority.rs"),
        include_str!("../domain_api.rs"),
        include_str!("../service/domain_parity_tests.rs"),
        include_str!("../../../domain-rules/src/tests.rs"),
    ];
    let mut runner = BTreeSet::new();
    let mut schedule_runner = BTreeSet::new();
    for case in field(&catalog, "scenarios").as_array().unwrap() {
        let id = string(field(case, "id"));
        let status = string(field(case, "execution_status"));
        assert!(
            legend.contains_key(status),
            "{id}: status {status} is not in the legend"
        );
        match status {
            "store_runner" => {
                runner.insert(id);
            }
            "schedule_runner" => {
                schedule_runner.insert(id);
            }
            "test_evidence" => {
                let evidence = field(case, "cs_evidence").as_array().unwrap();
                assert!(
                    !evidence.is_empty(),
                    "{id}: test_evidence needs cs_evidence"
                );
                for name in evidence {
                    let function = string(name).rsplit("::").next().unwrap();
                    assert!(
                        sources
                            .iter()
                            .any(|source| source.contains(&format!("fn {function}("))),
                        "{id}: evidence {} names no existing test",
                        string(name)
                    );
                }
            }
            _ => {
                assert!(
                    case.get("cs_evidence")
                        .is_none_or(|e| e.as_array().unwrap().is_empty()),
                    "{id}: a pending scenario cannot also claim evidence"
                );
            }
        }
    }
    assert_eq!(runner, CASES.iter().copied().collect::<BTreeSet<_>>());
    assert_eq!(
        schedule_runner,
        SCHEDULE_CASES.iter().copied().collect::<BTreeSet<_>>()
    );
}
