//! The refusal catalog against every code the rules and the model's guards produce.
use super::*;
use alondra_domain_model::INTERNAL_CODES;

/// Every code the rules and their guards can produce is either one `for_code` gives its own meaning or one
/// the catalog lists as only reachable by a node; a new code must be put in one of them.
#[test]
fn every_code_the_rules_produce_is_known_to_the_refusal_catalog() {
    let source = [
        include_str!("rules.rs"), include_str!("rules/plan_review.rs"), include_str!("rules/pr_identity.rs"), include_str!("stage_rules.rs"), include_str!("rules/spec_rules.rs"), include_str!("rules/validation_rules.rs"), include_str!("rules/continuation_rules.rs"),
        include_str!("../../domain-model/src/checks.rs"), include_str!("../../domain-model/src/checks/plan_review.rs"), include_str!("../../domain-model/src/checks/pr_identity.rs"),
        include_str!("../../domain-model/src/checklist.rs"), include_str!("../../domain-model/src/stage_rules.rs"), include_str!("../../domain-model/src/checks/spec_rules.rs"),
        include_str!("../../domain-model/src/checks/open_pr.rs"),
    ].concat();
    let mut found = std::collections::BTreeSet::new();
    for marker in ["fail(\"", "refuse(\"", "DomainError::new(\"", "fail(\n", "Some((\""] {
        for part in source.split(marker).skip(1) {
            let code: String = if marker.ends_with('\n') {
                part.trim_start().strip_prefix('"').map(|rest| rest.chars().take_while(|c| *c != '"').collect()).unwrap_or_default()
            } else {
                part.chars().take_while(|c| *c != '"').collect()
            };
            if !code.is_empty() && code.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                found.insert(code);
            }
        }
    }
    assert!(found.len() > 20, "the scan found {found:?}");
    let unknown: Vec<_> = found
        .iter()
        .filter(|code| !INTERNAL_CODES.contains(&code.as_str()) && Refusal::for_code(code) == Refusal::NoLongerAvailable)
        .collect();
    assert!(unknown.is_empty(), "codes with no place in the refusal catalog: {unknown:?}");
}
