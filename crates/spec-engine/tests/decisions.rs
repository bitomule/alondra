use alondra_spec_engine::{Code, Problem, Status};

use crate::support::{
    REFUNDS_PATH, Tree, billing, decision_file, decision_path, line_of, padded, problem, project, section_block, section_text,
    settings,
};

fn citing(tree: &mut Tree, decision: &str) -> String {
    tree.edit(REFUNDS_PATH, "@spec:billing.refunds\n", &format!("@spec:billing.refunds\n@decision:{decision}\n"));
    tree.text(REFUNDS_PATH)
}

fn decision_problem(tree: &Tree, code: Code) -> Problem {
    let found = problem(tree);
    assert_eq!(found.code, code, "{found:#?}");
    assert_eq!(found.path, decision_path("keep-ledger"));
    found
}

// spec: spec-engine.decisions/reads-decision
#[test]
fn reads_every_field_and_section_of_a_decision() {
    let tree = billing().with(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", "accepted", &[], ""));

    let loaded = project(&tree);
    let decision = loaded.decisions().iter().find(|decision| decision.id == "keep-ledger").unwrap();

    assert_eq!(decision.path, decision_path("keep-ledger"));
    assert_eq!(decision.title, "Keep a ledger");
    assert_eq!(decision.status, Status::Accepted);
    assert_eq!(decision.date, "2026-11-25");
    assert_eq!(decision.context.trim(), section_text("keep-ledger", "Context"));
    assert_eq!(decision.decision.trim(), section_text("keep-ledger", "Decision"));
    assert_eq!(decision.consequences.trim(), section_text("keep-ledger", "Consequences"));
    assert_eq!(decision.confirmation.trim(), section_text("keep-ledger", "Confirmation"));
}

// spec: spec-engine.decisions/file-name-and-id-must-agree
#[test]
fn a_decision_whose_id_is_not_its_file_name_is_refused() {
    let tree = billing().with(&decision_path("keep-ledger"), &decision_file("drop-ledger", "Drop the ledger", "accepted", &[], ""));

    let found = decision_problem(&tree, Code::DecisionIdMismatch);

    assert_eq!(found.code.as_str(), "decision_id_mismatch");
}

// spec: spec-engine.decisions/unknown-status-is-a-problem
#[test]
fn a_status_outside_the_five_is_refused_at_its_line() {
    let text = decision_file("keep-ledger", "Keep a ledger", "approved", &[], "");
    let tree = billing().with(&decision_path("keep-ledger"), &text);

    let found = decision_problem(&tree, Code::InvalidDecision);

    assert_eq!(found.line, line_of(&text, "status ="));
}

// spec: spec-engine.decisions/missing-section-is-a-problem
#[test]
fn a_decision_without_one_of_its_four_sections_is_refused() {
    for section in ["Context", "Decision", "Consequences", "Confirmation"] {
        let text = decision_file("keep-ledger", "Keep a ledger", "accepted", &[], "")
            .replace(&section_block("keep-ledger", section), "");
        let tree = billing().with(&decision_path("keep-ledger"), &text);

        let found = problem(&tree);

        assert_eq!(found.code, Code::MissingSection, "{section}: {found:#?}");
        assert_eq!(found.path, decision_path("keep-ledger"), "{section}");
    }
}

// spec: spec-engine.decisions/missing-header-is-a-problem
#[test]
fn a_decision_without_its_header_is_refused_at_line_one() {
    let text = decision_file("keep-ledger", "Keep a ledger", "accepted", &[], "");
    let body = text.split("+++\n").nth(2).unwrap();
    let tree = billing().with(&decision_path("keep-ledger"), &format!("# Keep a ledger\n{body}"));

    let found = decision_problem(&tree, Code::InvalidDecision);

    assert_eq!(found.line, 1);
}

// spec: spec-engine.decisions/citing-unknown-decision-is-a-problem
#[test]
fn citing_a_decision_nobody_wrote_is_refused_at_the_tag() {
    let mut tree = billing();
    let text = citing(&mut tree, "keep-ledger");

    let found = problem(&tree);

    assert_eq!(found.code, Code::UnknownDecision);
    assert_eq!(found.path, REFUNDS_PATH);
    assert_eq!(found.line, line_of(&text, "@decision:keep-ledger"));
}

// spec: spec-engine.decisions/citing-unaccepted-decision-is-a-problem
#[test]
fn citing_a_decision_that_is_not_accepted_is_refused_at_the_tag() {
    for status in ["proposed", "rejected", "superseded", "deprecated"] {
        let mut tree = billing();
        let text = citing(&mut tree, "keep-ledger");
        if status == "superseded" {
            tree.put(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", status, &[], "per-test-ledger"));
            tree.put(
                &decision_path("per-test-ledger"),
                &decision_file("per-test-ledger", "A ledger entry per test", "accepted", &["keep-ledger"], ""),
            );
        } else {
            tree.put(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", status, &[], ""));
        }

        let found = problem(&tree);

        assert_eq!(found.code, Code::DecisionNotAccepted, "{status}: {found:#?}");
        assert_eq!(found.path, REFUNDS_PATH, "{status}");
        assert_eq!(found.line, line_of(&text, "@decision:keep-ledger"), "{status}");
    }
    assert_eq!(Code::DecisionNotAccepted.as_str(), "decision_not_accepted");
}

// spec: spec-engine.decisions/replacement-links-both-ways
#[test]
fn a_replacement_acknowledged_by_the_old_decision_loads() {
    let tree = billing()
        .with(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", "superseded", &[], "per-test-ledger"))
        .with(
            &decision_path("per-test-ledger"),
            &decision_file("per-test-ledger", "A ledger entry per test", "accepted", &["keep-ledger"], ""),
        );

    let loaded = project(&tree);
    let old = loaded.decisions().iter().find(|decision| decision.id == "keep-ledger").unwrap();
    let new = loaded.decisions().iter().find(|decision| decision.id == "per-test-ledger").unwrap();

    assert_eq!(old.status, Status::Superseded);
    assert_eq!(old.superseded_by.as_deref(), Some("per-test-ledger"));
    assert_eq!(new.supersedes, ["keep-ledger"]);
}

// spec: spec-engine.decisions/one-sided-replacement-is-a-problem
#[test]
fn a_replacement_the_old_decision_does_not_acknowledge_is_refused() {
    let tree = billing()
        .with(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", "accepted", &[], ""))
        .with(
            &decision_path("per-test-ledger"),
            &decision_file("per-test-ledger", "A ledger entry per test", "accepted", &["keep-ledger"], ""),
        );

    decision_problem(&tree, Code::SupersessionMismatch);
}

// spec: spec-engine.decisions/superseded-decision-names-its-replacement
#[test]
fn a_superseded_decision_whose_replacement_does_not_name_it_back_is_refused() {
    let unaware = decision_file("per-test-ledger", "A ledger entry per test", "accepted", &[], "");
    for (replacement, other) in [("", None), ("gone", None), ("per-test-ledger", Some(unaware))] {
        let text = decision_file("keep-ledger", "Keep a ledger", "superseded", &[], replacement);
        let mut tree = billing().with(&decision_path("keep-ledger"), &text);
        if let Some(other) = &other {
            tree.put(&decision_path("per-test-ledger"), other);
        }

        let found = decision_problem(&tree, Code::SupersessionMismatch);

        assert_eq!(found.line, line_of(&text, "superseded_by ="), "{replacement:?}");
    }
}

// spec: spec-engine.decisions/oversized-decision-is-a-problem
#[test]
fn a_decision_beyond_the_size_limit_is_refused() {
    let text = decision_file("keep-ledger", "Keep a ledger", "accepted", &[], "");
    let fits = billing().with(&decision_path("keep-ledger"), &padded(&text, 'x', 262_144));
    assert_eq!(project(&fits).decisions().len(), 1);

    let beyond = billing().with(&decision_path("keep-ledger"), &padded(&text, 'x', 262_145));

    decision_problem(&beyond, Code::TooLarge);
}

// spec: spec-engine.decisions/decisions-off-reads-no-file
#[test]
fn with_decisions_off_no_decision_file_is_read() {
    let tree = billing()
        .with("alondra.toml", &settings(false, true))
        .with(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", "accepted", &[], ""));

    let loaded = project(&tree);

    assert!(loaded.decisions().is_empty(), "{:#?}", loaded.decisions());
    assert!(!loaded.specs().is_empty());
    assert!(!tree.was_read(&decision_path("keep-ledger")));
}

// spec: spec-engine.decisions/citing-with-decisions-off-is-a-problem
#[test]
fn citing_a_decision_with_decisions_off_is_refused_at_the_tag() {
    let mut tree = billing()
        .with("alondra.toml", &settings(false, true))
        .with(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", "accepted", &[], ""));
    let text = citing(&mut tree, "keep-ledger");

    let found = problem(&tree);

    assert_eq!(found.code, Code::UnknownDecision);
    assert_eq!(found.path, REFUNDS_PATH);
    assert_eq!(found.line, line_of(&text, "@decision:keep-ledger"));
}
