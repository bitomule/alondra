use alondra_spec_engine::{Code, Kind, State};

use crate::support::{
    BOUNDARY_PATH, OVERVIEW, OVERVIEW_PATH, REFUNDS_PATH, Tree, billing, decision_file, decision_path, line_of, padded, problem,
    project, record_everything, settings, state, test_file,
};

const TWO_SECTIONS: &str = "# Billing

## How refunds work
<!-- section: billing.overview explains: billing.refunds, boundary:billing -->

A refund returns what the customer paid.

## Where the money is kept
<!-- section: billing.money explains: decision:keep-ledger -->

Every movement is kept in the ledger.
";

fn with_document(text: &str) -> Tree {
    billing()
        .with(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", "accepted", &[], ""))
        .with(OVERVIEW_PATH, text)
}

fn fails_at(text: &str, code: Code, needle: &str) {
    let found = problem(&with_document(text));
    assert_eq!(found.code, code, "{found:#?}");
    assert_eq!(found.path, OVERVIEW_PATH);
    assert_eq!(found.line, line_of(text, needle), "{found:#?}");
}

// spec: spec-engine.reference/reads-sections
#[test]
fn reads_each_section_with_what_it_explains() {
    let loaded = project(&with_document(TWO_SECTIONS));
    let sections = loaded.sections();

    let ids: Vec<&str> = sections.iter().map(|section| section.id.as_str()).collect();
    assert_eq!(ids, ["billing.overview", "billing.money"]);

    let refunds = &sections[0];
    assert_eq!(refunds.path, OVERVIEW_PATH);
    assert_eq!(refunds.line, line_of(TWO_SECTIONS, "## How refunds work"));
    assert_eq!(refunds.heading, "How refunds work");
    assert_eq!(refunds.explains, ["billing.refunds", "boundary:billing"]);
    assert!(refunds.text.contains("## How refunds work"), "{:?}", refunds.text);
    assert!(refunds.text.contains("A refund returns what the customer paid."), "{:?}", refunds.text);
    assert!(!refunds.text.contains("<!--"), "{:?}", refunds.text);
    assert!(!refunds.text.contains("Every movement"), "{:?}", refunds.text);

    let money = &sections[1];
    assert_eq!(money.heading, "Where the money is kept");
    assert_eq!(money.explains, ["decision:keep-ledger"]);
    assert!(money.text.contains("Every movement is kept in the ledger."), "{:?}", money.text);
}

// spec: spec-engine.reference/text-before-the-first-section-is-free
#[test]
fn text_before_the_first_section_belongs_to_none() {
    let text = format!("{}An introduction that explains nothing.\n{}", "# Billing\n\n", &OVERVIEW["# Billing\n".len()..]);
    let loaded = project(&with_document(&text));

    assert_eq!(loaded.sections().len(), 1);
    assert!(!loaded.sections()[0].text.contains("An introduction"), "{:?}", loaded.sections()[0].text);
}

// spec: spec-engine.reference/section-without-comment-is-a-problem
#[test]
fn a_section_without_its_comment_is_refused_at_the_heading() {
    let text = "# Billing\n\n## How refunds work\n\nA refund returns what the customer paid.\n";
    fails_at(text, Code::SectionExplainsNothing, "## How refunds work");
}

// spec: spec-engine.reference/malformed-comment-is-a-problem
#[test]
fn a_comment_after_the_heading_that_is_not_a_section_comment_is_refused_at_the_comment() {
    for comment in [
        "<!--section: billing.overview explains: billing.refunds -->",
        "<!-- section billing.overview explains: billing.refunds -->",
        "<!-- a note -->",
    ] {
        let text = format!("## How refunds work\n{comment}\n\nA refund returns what the customer paid.\n");
        fails_at(&text, Code::SectionExplainsNothing, comment);
    }
}

// spec: spec-engine.reference/oversized-document-is-a-problem
#[test]
fn a_document_beyond_the_size_limit_is_refused() {
    assert_eq!(project(&with_document(&padded(OVERVIEW, 'x', 262_144))).sections().len(), 1);

    let found = problem(&with_document(&padded(OVERVIEW, 'x', 262_145)));

    assert_eq!(found.code, Code::TooLarge, "{found:#?}");
    assert_eq!(found.path, OVERVIEW_PATH);
}

// spec: spec-engine.reference/empty-explains-is-a-problem
#[test]
fn a_section_that_explains_an_empty_list_is_refused_at_the_comment() {
    let text = "## How refunds work\n<!-- section: billing.overview explains: -->\n\nA refund returns what the customer paid.\n";
    fails_at(text, Code::SectionExplainsNothing, "<!-- section:");
}

// spec: spec-engine.reference/unknown-target-is-a-problem
#[test]
fn a_section_that_explains_something_missing_is_refused_at_the_comment() {
    for target in ["billing.gone", "boundary:gone", "decision:gone"] {
        let text = format!("## How refunds work\n<!-- section: billing.overview explains: billing.refunds, {target} -->\n\nText.\n");
        let found = problem(&with_document(&text));
        assert_eq!(found.code, Code::UnknownReferenceTarget, "{target}: {found:#?}");
        assert_eq!(found.path, OVERVIEW_PATH, "{target}");
        assert_eq!(found.line, 2, "{target}");
    }
    assert_eq!(Code::UnknownReferenceTarget.as_str(), "unknown_reference_target");
}

// spec: spec-engine.reference/duplicate-section-is-a-problem
#[test]
fn two_sections_with_one_id_are_refused_in_the_second_document() {
    let tree = with_document(OVERVIEW).with("docs/reference/billing-faq.md", OVERVIEW);

    let found = problem(&tree);

    assert_eq!(found.code, Code::DuplicateSectionId);
    assert_eq!(found.path, OVERVIEW_PATH);
}

// spec: spec-engine.reference/section-follows-what-it-explains
#[test]
fn a_section_goes_stale_when_anything_it_explains_changes() {
    let changes: [(&str, &str, &str); 4] = [
        (REFUNDS_PATH, "Given a paid order of 30 EUR", "Given a paid order of 40 EUR"),
        (BOUNDARY_PATH, "\"//crates/billing:test\"", "\"//crates/billing:tests\""),
        ("docs/decisions/keep-ledger.md", "The decision of keep-ledger.", "The decision of keep-ledger, reworded."),
        (OVERVIEW_PATH, "A refund returns what the customer paid.", "A refund gives back what the customer paid."),
    ];
    for (path, from, to) in changes {
        let mut tree = with_document(OVERVIEW);
        record_everything(&mut tree);
        assert_eq!(state(&project(&tree).report(None), "billing.overview", Kind::Reference), State::Valid);

        tree.edit(path, from, to);

        assert_eq!(state(&project(&tree).report(None), "billing.overview", Kind::Reference), State::Stale, "{path}");
    }
}

// spec: spec-engine.reference/section-ignores-what-it-does-not-explain
#[test]
fn a_section_stays_valid_when_a_spec_it_does_not_explain_changes() {
    let invoices = "@spec:billing.invoices\nFeature: Invoices\n\n  @id:sent\n  Scenario: An invoice is sent\n    Given a paid order\n    Then an invoice is sent\n";
    let section = "## How refunds work\n<!-- section: billing.overview explains: billing.refunds -->\n\nA refund returns what the customer paid.\n";
    let mut tree = with_document(section)
        .with("specs/billing/invoices.feature", invoices)
        .with("crates/billing/tests/invoices.rs", &test_file("billing.invoices/sent", "sends_an_invoice"));
    record_everything(&mut tree);

    tree.edit("specs/billing/invoices.feature", "Then an invoice is sent", "Then an invoice is sent by mail");

    assert_eq!(state(&project(&tree).report(None), "billing.overview", Kind::Reference), State::Valid);
}

// spec: spec-engine.reference/reference-off-reads-no-file
#[test]
fn with_reference_off_no_document_is_read_and_no_section_is_listed() {
    let tree = billing()
        .with("alondra.toml", &settings(true, false))
        .with(OVERVIEW_PATH, "## A section that would be refused\n\nNo comment.\n");

    let loaded = project(&tree);
    let report = loaded.report(None);

    assert!(loaded.sections().is_empty());
    assert!(report.items.iter().all(|item| item.kind != Kind::Reference), "{:#?}", report.items);
    assert!(!report.items.is_empty());
    assert!(!tree.was_read(OVERVIEW_PATH));
}
