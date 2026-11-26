use alondra_spec_engine::{Code, EvidenceKind, Judgment, Keyword, Promise, Scenario, Spec, Step, read_spec, write_spec};
use proptest::prelude::*;

use crate::support::{codes, line_of, without_lines};

const PATH: &str = "specs/billing/refunds.feature";

const TWO_SCENARIOS: &str = "@spec:billing.refunds
Feature: Refunds return the money

  @id:full
  Scenario: Full refund
    Given a paid order of 30 EUR
    When the order is refunded
    Then the customer receives 30 EUR
    And the order is closed

  @id:partial
  Scenario: Partial refund
    When 10 EUR are refunded
    But the order stays open
";

fn one_scenario(tags: &str) -> String {
    format!("@spec:billing.refunds\nFeature: Refunds\n\n  {tags}\n  Scenario: Full refund\n    Given a paid order\n")
}

fn fails_with(text: &str, code: Code, needle: &str) {
    let problems = read_spec(PATH, text.as_bytes()).expect_err("the file reads and it should not");
    assert_eq!(codes(&problems), [code], "{problems:#?}");
    assert_eq!(problems[0].path, PATH);
    assert_eq!(problems[0].line, line_of(text, needle), "{problems:#?}");
}

// spec: spec-engine.feature-format/reads-feature-and-scenarios
#[test]
fn reads_a_feature_with_two_scenarios() {
    let spec = read_spec(PATH, TWO_SCENARIOS.as_bytes()).unwrap();

    assert_eq!(spec.id, "billing.refunds");
    assert_eq!(spec.title, "Refunds return the money");
    let ids: Vec<&str> = spec.scenarios.iter().map(|scenario| scenario.id.as_str()).collect();
    assert_eq!(ids, ["full", "partial"]);

    let full = &spec.scenarios[0];
    assert_eq!(full.title, "Full refund");
    assert_eq!(full.line, line_of(TWO_SCENARIOS, "Scenario: Full refund"));
    assert!(!full.outline);
    let steps: Vec<(Keyword, &str)> = full.steps.iter().map(|step| (step.keyword, step.text.as_str())).collect();
    assert_eq!(
        steps,
        [
            (Keyword::Given, "a paid order of 30 EUR"),
            (Keyword::When, "the order is refunded"),
            (Keyword::Then, "the customer receives 30 EUR"),
            (Keyword::And, "the order is closed"),
        ]
    );
    let partial = &spec.scenarios[1];
    assert_eq!(partial.line, line_of(TWO_SCENARIOS, "Scenario: Partial refund"));
    assert_eq!(partial.steps[1].keyword, Keyword::But);
    assert_eq!(partial.steps[1].text, "the order stays open");
}

// spec: spec-engine.feature-format/reads-judgment-tags
#[test]
fn reads_which_evidence_a_judgment_needs() {
    for (tags, kind, pattern) in [
        ("@judged", EvidenceKind::None, None),
        ("@judged:capture", EvidenceKind::Capture, None),
        ("@judged:capture @evidence:shots/**/*.png", EvidenceKind::Capture, Some("shots/**/*.png")),
        ("@judged:video", EvidenceKind::Video, None),
    ] {
        let spec = read_spec(PATH, one_scenario(&format!("@id:full {tags}")).as_bytes()).unwrap();
        let judgment = spec.scenarios[0].judgment.clone().unwrap_or_else(|| panic!("{tags} is not a judgment"));
        assert_eq!(judgment.evidence, kind, "{tags}");
        assert_eq!(judgment.pattern.as_deref(), pattern, "{tags}");
    }
    let plain = read_spec(PATH, one_scenario("@id:full").as_bytes()).unwrap();
    assert_eq!(plain.scenarios[0].judgment, None);
}

// spec: spec-engine.feature-format/reads-boundary-promise-tag
#[test]
fn reads_the_promise_a_scenario_states() {
    for (tag, promise) in [("surface", Promise::Surface), ("data", Promise::Data), ("dependencies", Promise::Dependencies)] {
        let spec = read_spec(PATH, one_scenario(&format!("@id:full @boundary:{tag}")).as_bytes()).unwrap();
        assert_eq!(spec.scenarios[0].promise, Some(promise));
    }
}

// spec: spec-engine.feature-format/rejects-unsupported-keyword
#[test]
fn refuses_a_keyword_outside_the_subset() {
    let text = "@spec:billing.refunds\nFeature: Refunds\n\n  Rule: refunds need a receipt\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order\n";
    fails_with(text, Code::UnsupportedSyntax, "Rule:");
    for line in ["Example: a full refund", "Scenarios:", "Scenario Template: a full refund"] {
        let text = format!("@spec:billing.refunds\nFeature: Refunds\n  {line}\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order\n");
        fails_with(&text, Code::UnsupportedSyntax, line);
    }
}

// spec: spec-engine.feature-format/description-may-hold-a-colon
#[test]
fn a_description_line_may_start_with_a_word_and_a_colon() {
    let text = "@spec:billing.refunds\nFeature: Refunds\n  Note: refunds are rare\n  Why: the money must go back\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order\n";
    let spec = read_spec(PATH, text.as_bytes()).unwrap_or_else(|problems| panic!("{problems:#?}"));

    assert_eq!(spec.description, ["Note: refunds are rare", "Why: the money must go back"]);
}

// spec: spec-engine.feature-format/rejects-scenario-without-id
#[test]
fn refuses_a_scenario_without_its_identifier() {
    let text = "@spec:billing.refunds\nFeature: Refunds\n\n  Scenario: Full refund\n    Given a paid order\n";
    fails_with(text, Code::MissingScenarioId, "Scenario:");
}

// spec: spec-engine.feature-format/canonical-text-is-stable
#[test]
fn writing_what_was_read_from_canonical_text_changes_nothing() {
    for canonical in [TWO_SCENARIOS, CANONICAL] {
        let spec = read_spec(PATH, canonical.as_bytes()).unwrap();
        assert_eq!(write_spec(&spec), canonical);
    }
}

fn step(keyword: Keyword, text: &str) -> Step {
    Step { keyword, text: text.to_string(), doc: None, table: Vec::new() }
}

fn rows(cells: &[&[&str]]) -> Vec<Vec<String>> {
    cells.iter().map(|row| row.iter().map(|cell| cell.to_string()).collect()).collect()
}

// spec: spec-engine.feature-format/keeps-description
#[test]
fn keeps_the_description_lines_trimmed() {
    let text = "@spec:billing.refunds\nFeature: Refunds\n  A refund returns what the customer paid.   \n    It never exceeds the payment.\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order\n";
    let spec = read_spec(PATH, text.as_bytes()).unwrap();

    assert_eq!(spec.description, ["A refund returns what the customer paid.", "It never exceeds the payment."]);
}

// spec: spec-engine.feature-format/reads-decision-tags
#[test]
fn reads_the_decisions_a_feature_cites_in_order() {
    let text = "@spec:billing.refunds\n@decision:keep-ledger\n@decision:one-file-per-spec\nFeature: Refunds\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order\n";
    let spec = read_spec(PATH, text.as_bytes()).unwrap();

    assert_eq!(spec.decisions, ["keep-ledger", "one-file-per-spec"]);
}

// spec: spec-engine.feature-format/reads-background
#[test]
fn background_steps_belong_to_the_spec_and_not_to_its_scenarios() {
    let text = "@spec:billing.refunds
Feature: Refunds

  Background:
    Given a paid order of 30 EUR
    And a customer who asks for a refund

  @id:full
  Scenario: Full refund
    When the order is refunded
";
    let spec = read_spec(PATH, text.as_bytes()).unwrap();

    assert_eq!(
        spec.background,
        [step(Keyword::Given, "a paid order of 30 EUR"), step(Keyword::And, "a customer who asks for a refund")]
    );
    assert_eq!(spec.scenarios[0].steps, [step(Keyword::When, "the order is refunded")]);
}

// spec: spec-engine.feature-format/reads-outline-with-examples
#[test]
fn an_outline_keeps_its_examples_with_cells_trimmed() {
    let text = "@spec:billing.refunds
Feature: Refunds

  @id:partial
  Scenario Outline: Partial refund
    When <amount> EUR are refunded
    Then the order is <state>

    Examples:
      | amount | state  |
      |   10   | open   |
      | 30     | closed |
";
    let spec = read_spec(PATH, text.as_bytes()).unwrap();
    let scenario = &spec.scenarios[0];

    assert!(scenario.outline);
    assert_eq!(scenario.examples, rows(&[&["amount", "state"], &["10", "open"], &["30", "closed"]]));
    assert_eq!(scenario.steps.len(), 2);
}

// spec: spec-engine.feature-format/reads-doc-string-and-table
#[test]
fn a_step_keeps_its_doc_string_and_another_its_table() {
    let text = r#"@spec:billing.refunds
Feature: Refunds

  @id:full
  Scenario: Full refund
    Given a receipt
      """
      Refund of 30 EUR
      to the customer
      """
    Then the ledger holds
      | currency | amount |
      | EUR      | 30     |
"#;
    let spec = read_spec(PATH, text.as_bytes()).unwrap();
    let steps = &spec.scenarios[0].steps;

    assert_eq!(steps[0].doc.as_deref(), Some("Refund of 30 EUR\nto the customer"));
    assert!(steps[0].table.is_empty());
    assert_eq!(steps[1].table, rows(&[&["currency", "amount"], &["EUR", "30"]]));
    assert_eq!(steps[1].doc, None);
}

// spec: spec-engine.feature-format/ignores-comments-and-blank-lines
#[test]
fn comments_and_blank_lines_change_nothing_but_lines() {
    let commented = "# Refunds, as agreed

@spec:billing.refunds
# the title follows
Feature: Refunds return the money


  @id:full
  # the whole amount
  Scenario: Full refund

    Given a paid order of 30 EUR
    # nothing else happens before
    When the order is refunded

    Then the customer receives 30 EUR
    And the order is closed
  #

  @id:partial
  Scenario: Partial refund
    When 10 EUR are refunded
    But the order stays open
# end
";
    let with = read_spec(PATH, commented.as_bytes()).unwrap();
    let without = read_spec(PATH, TWO_SCENARIOS.as_bytes()).unwrap();

    assert_eq!(without_lines(with), without_lines(without));
}

// spec: spec-engine.feature-format/rejects-unknown-evidence-kind
#[test]
fn refuses_a_kind_of_evidence_it_does_not_know() {
    fails_with(&one_scenario("@id:full @judged:audio"), Code::UnknownTag, "@judged:audio");
}

// spec: spec-engine.feature-format/evidence-pattern-needs-captures
#[test]
fn refuses_an_evidence_pattern_without_captures() {
    for tags in ["@evidence:shots/a.png", "@judged @evidence:shots/a.png", "@judged:video @evidence:clips/a.mov"] {
        fails_with(&one_scenario(&format!("@id:full {tags}")), Code::EvidenceWithoutCapture, "@evidence:");
    }
}

// spec: spec-engine.feature-format/reads-reserved-tag
#[test]
fn reads_a_reserved_scenario() {
    let reserved = read_spec(PATH, one_scenario("@id:full @reserved").as_bytes()).unwrap();
    let plain = read_spec(PATH, one_scenario("@id:full").as_bytes()).unwrap();

    assert!(reserved.scenarios[0].reserved);
    assert!(!plain.scenarios[0].reserved);
}

// spec: spec-engine.feature-format/rejects-unknown-boundary-promise
#[test]
fn refuses_a_boundary_promise_it_does_not_know() {
    fails_with(&one_scenario("@id:full @boundary:speed"), Code::UnknownTag, "@boundary:speed");
}

// spec: spec-engine.feature-format/rejects-unknown-tag
#[test]
fn refuses_a_tag_it_does_not_know() {
    fails_with(&one_scenario("@id:full @wip"), Code::UnknownTag, "@wip");
}

// spec: spec-engine.feature-format/rejects-step-without-keyword
#[test]
fn refuses_a_line_inside_a_scenario_that_is_not_a_step() {
    let text = "@spec:billing.refunds\nFeature: Refunds\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order\n    the customer is refunded\n";
    fails_with(text, Code::UnsupportedSyntax, "the customer is refunded");
}

// spec: spec-engine.feature-format/rejects-missing-spec-tag
#[test]
fn refuses_a_feature_without_its_identifier() {
    let text = "Feature: Refunds\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order\n";
    fails_with(text, Code::MissingSpecTag, "Feature:");
}

// spec: spec-engine.feature-format/rejects-invalid-identifier
#[test]
fn refuses_an_identifier_outside_lowercase_digits_dots_and_hyphens() {
    let text = "@spec:Billing_Refunds\nFeature: Refunds\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order\n";
    fails_with(text, Code::InvalidIdentifier, "@spec:Billing_Refunds");
    assert_eq!(Code::InvalidIdentifier.as_str(), "invalid_identifier");
}

fn padded_to(size: usize, extra: &str) -> String {
    let mut text = format!("{}{extra}# ", one_scenario("@id:full"));
    let filler = size - text.len() - 1;
    text.push_str(&"x".repeat(filler));
    text.push('\n');
    assert_eq!(text.len(), size);
    text
}

fn scenarios(count: usize) -> String {
    let mut text = String::from("@spec:billing.refunds\nFeature: Refunds\n");
    for index in 0..count {
        text.push_str(&format!("\n  @id:s{index}\n  Scenario: Refund {index}\n    Given a paid order\n"));
    }
    text
}

// spec: spec-engine.feature-format/rejects-oversized-input
#[test]
fn refuses_files_beyond_the_size_and_scenario_limits() {
    assert!(read_spec(PATH, padded_to(262_144, "").as_bytes()).is_ok());
    let too_large = read_spec(PATH, padded_to(262_145, "  Rule: refunds need a receipt\n").as_bytes()).unwrap_err();
    assert_eq!(codes(&too_large), [Code::TooLarge], "{too_large:#?}");
    assert_eq!(too_large[0].path, PATH);

    assert_eq!(read_spec(PATH, scenarios(200).as_bytes()).unwrap().scenarios.len(), 200);
    let too_many = read_spec(PATH, scenarios(201).as_bytes()).unwrap_err();
    assert_eq!(codes(&too_many), [Code::TooManyScenarios], "{too_many:#?}");
    assert_eq!(too_many[0].path, PATH);
}

// spec: spec-engine.feature-format/scenario-limit-counts-scenarios
#[test]
fn scenario_lines_inside_a_doc_string_are_not_scenarios() {
    let doc = "      \"\"\"\n      Scenario: one\n      Scenario: two\n      Scenario: three\n      \"\"\"\n";
    let text = format!("{}{doc}", scenarios(200));

    let spec = read_spec(PATH, text.as_bytes()).unwrap_or_else(|problems| panic!("{problems:#?}"));

    assert_eq!(spec.scenarios.len(), 200);
    let last = spec.scenarios.last().unwrap();
    assert_eq!(last.steps[0].doc.as_deref(), Some("Scenario: one\nScenario: two\nScenario: three"));
}

fn round_trip(spec: &Spec) -> Spec {
    let text = write_spec(spec);
    without_lines(read_spec(PATH, text.as_bytes()).unwrap_or_else(|problems| panic!("{text}\n{problems:#?}")))
}

fn with_step(step: Step) -> Spec {
    let mut spec = without_lines(read_spec(PATH, one_scenario("@id:full").as_bytes()).unwrap());
    spec.scenarios[0].steps = vec![step];
    spec
}

const QUOTES: &str = "\"\"\"";
const ESCAPED_QUOTES: &str = r#"\"\"\""#;
const TWICE_ESCAPED_QUOTES: &str = r#"\\\"\\\"\\\""#;
const THRICE_ESCAPED_QUOTES: &str = r#"\\\\\\\"\\\\\\\"\\\\\\\""#;

// spec: spec-engine.feature-format/doc-string-escapes-delimiter-lines
#[test]
fn a_doc_string_line_that_would_close_it_is_escaped_and_read_back() {
    let doc = [QUOTES, ESCAPED_QUOTES, TWICE_ESCAPED_QUOTES, "  \"\"\"", r#"\""""#, r"\\"].join("\n");
    let spec = with_step(Step { keyword: Keyword::Given, text: "a receipt".to_string(), doc: Some(doc), table: Vec::new() });

    let text = write_spec(&spec);

    let written = format!(
        "\n      {ESCAPED_QUOTES}\n      {TWICE_ESCAPED_QUOTES}\n      {THRICE_ESCAPED_QUOTES}\n        {ESCAPED_QUOTES}\n      {}\n      {}\n",
        r#"\""""#, r"\\"
    );
    assert!(text.contains(&written), "{text}");
    assert_eq!(round_trip(&spec), spec);
}

// spec: spec-engine.feature-format/table-cells-escape-pipes-and-backslashes
#[test]
fn a_table_cell_with_a_pipe_or_a_backslash_is_escaped_and_read_back() {
    let table = rows(&[&["a|b", r"a\b"], &[r"\|", "two  words"], &[r"ends\", "|"]]);
    let spec = with_step(Step { keyword: Keyword::Then, text: "the ledger holds".to_string(), doc: None, table });

    let text = write_spec(&spec);

    assert!(text.contains(r"| a\|b   | a\\b       |"), "{text}");
    assert!(text.contains(r"| ends\\ | \|         |"), "{text}");
    assert_eq!(round_trip(&spec), spec);
}

// spec: spec-engine.feature-format/rejects-invalid-utf8
#[test]
fn refuses_bytes_that_are_not_utf8() {
    let mut bytes = one_scenario("@id:full").into_bytes();
    bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);

    let problems = read_spec(PATH, &bytes).unwrap_err();

    assert_eq!(codes(&problems), [Code::InvalidUtf8], "{problems:#?}");
    assert_eq!(problems[0].path, PATH);
}

// spec: spec-engine.feature-format/reports-every-problem
#[test]
fn reports_every_problem_of_a_file_ordered_by_line() {
    let text = "Feature: Refunds\n\n  Scenario: Full refund\n    Given a paid order\n";
    let problems = read_spec(PATH, text.as_bytes()).unwrap_err();

    assert_eq!(codes(&problems), [Code::MissingSpecTag, Code::MissingScenarioId], "{problems:#?}");
    assert_eq!(problems[0].line, line_of(text, "Feature:"));
    assert_eq!(problems[1].line, line_of(text, "Scenario:"));
}

const CANONICAL: &str = r#"@spec:billing.refunds
@decision:keep-ledger
@decision:one-file-per-spec
Feature: Refunds return the money
  A refund returns what the customer paid.
  It never exceeds the payment.

  Background:
    Given a paid order of 30 EUR

  @id:full @judged:capture @evidence:shots/**/*.png
  Scenario: Full refund
    When the order is refunded
    Then the customer receives 30 EUR
      | currency | amount |
      | EUR      | 30     |

  @id:partial
  Scenario Outline: Partial refund
    When <amount> EUR are refunded
    Then the customer receives <amount> EUR
      """
      receipt
      """

    Examples:
      | amount |
      | 10     |
"#;

// spec: spec-engine.feature-format/writes-canonical-form
#[test]
fn writes_a_spec_in_the_canonical_form() {
    let spec = Spec {
        id: "billing.refunds".to_string(),
        decisions: vec!["keep-ledger".to_string(), "one-file-per-spec".to_string()],
        title: "Refunds return the money".to_string(),
        description: vec!["A refund returns what the customer paid.".to_string(), "It never exceeds the payment.".to_string()],
        background: vec![step(Keyword::Given, "a paid order of 30 EUR")],
        diagrams: Vec::new(),
        mockups: Vec::new(),
        scenarios: vec![
            Scenario {
                id: "full".to_string(),
                title: "Full refund".to_string(),
                line: 0,
                outline: false,
                judgment: Some(Judgment { evidence: EvidenceKind::Capture, pattern: Some("shots/**/*.png".to_string()) }),
                promise: None,
                reserved: false,
                diagrams: Vec::new(),
                mockups: Vec::new(),
                steps: vec![
                    step(Keyword::When, "the order is refunded"),
                    Step {
                        keyword: Keyword::Then,
                        text: "the customer receives 30 EUR".to_string(),
                        doc: None,
                        table: rows(&[&["currency", "amount"], &["EUR", "30"]]),
                    },
                ],
                examples: Vec::new(),
            },
            Scenario {
                id: "partial".to_string(),
                title: "Partial refund".to_string(),
                line: 0,
                outline: true,
                judgment: None,
                promise: None,
                reserved: false,
                diagrams: Vec::new(),
                mockups: Vec::new(),
                steps: vec![
                    step(Keyword::When, "<amount> EUR are refunded"),
                    Step {
                        keyword: Keyword::Then,
                        text: "the customer receives <amount> EUR".to_string(),
                        doc: Some("receipt".to_string()),
                        table: Vec::new(),
                    },
                ],
                examples: rows(&[&["amount"], &["10"]]),
            },
        ],
    };

    assert_eq!(write_spec(&spec), CANONICAL);
}

const WORD: &str = "[a-z0-9,.()'<>-]{1,8}";

fn words() -> impl Strategy<Value = String> {
    prop::string::string_regex(&format!("{WORD}( {WORD}){{0,4}}")).unwrap()
}

fn keyword() -> impl Strategy<Value = Keyword> {
    prop_oneof![Just(Keyword::Given), Just(Keyword::When), Just(Keyword::Then), Just(Keyword::And), Just(Keyword::But)]
}

fn cell() -> impl Strategy<Value = String> {
    prop_oneof!["[a-z0-9]{1,6}", r"[a-z0-9|\\]([a-z0-9| \\]{0,5}[a-z0-9|\\])?"]
}

fn table() -> impl Strategy<Value = Vec<Vec<String>>> {
    (1usize..4).prop_flat_map(|columns| prop::collection::vec(prop::collection::vec(cell(), columns), 1..4))
}

fn doc_line() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => words(),
        1 => Just(QUOTES.to_string()),
        1 => Just(ESCAPED_QUOTES.to_string()),
        1 => Just(TWICE_ESCAPED_QUOTES.to_string()),
        1 => Just(format!("  {QUOTES}")),
        1 => r#"[\\"]{1,8}"#,
    ]
}

fn description_line() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => words(),
        1 => ("(Note|Why|Warning|See also)", words()).prop_map(|(word, rest)| format!("{word}: {rest}")),
    ]
}

fn any_step() -> impl Strategy<Value = Step> {
    (keyword(), words(), 0u8..3, prop::collection::vec(doc_line(), 1..4), table()).prop_map(
        |(keyword, text, argument, lines, table)| Step {
            keyword,
            text,
            doc: (argument == 1).then(|| lines.join("\n")),
            table: if argument == 2 { table } else { Vec::new() },
        },
    )
}

fn judgment() -> impl Strategy<Value = Option<Judgment>> {
    prop_oneof![
        Just(None::<Judgment>),
        Just(Some(Judgment { evidence: EvidenceKind::None, pattern: None })),
        prop::option::of("[a-z]{1,6}/(\\*\\*/)?[a-z]{0,4}\\*\\.png")
            .prop_map(|pattern| Some(Judgment { evidence: EvidenceKind::Capture, pattern })),
        Just(Some(Judgment { evidence: EvidenceKind::Video, pattern: None })),
    ]
}

fn promise() -> impl Strategy<Value = Option<Promise>> {
    prop::option::of(prop_oneof![Just(Promise::Surface), Just(Promise::Data), Just(Promise::Dependencies)])
}

fn part_files(extension: &str) -> impl Strategy<Value = Vec<String>> {
    let file = prop::string::string_regex(&format!("([a-z]{{1,4}}/)?[a-z0-9_-][a-z0-9._-]{{0,5}}\\.{extension}")).unwrap();
    prop::collection::btree_set(file, 0..2).prop_map(|files| files.into_iter().collect())
}

fn any_parts() -> impl Strategy<Value = (Vec<String>, Vec<String>)> {
    (part_files("mmd"), prop_oneof![part_files("png"), part_files("jpe?g")])
}

fn any_scenario() -> impl Strategy<Value = Scenario> {
    (
        "[a-z][a-z0-9]{0,6}",
        words(),
        any::<bool>(),
        judgment(),
        promise(),
        any::<bool>(),
        prop::collection::vec(any_step(), 1..4),
        table(),
        any_parts(),
    )
        .prop_map(|(id, title, outline, judgment, promise, reserved, steps, examples, (diagrams, mockups))| Scenario {
            diagrams,
            mockups,
            id,
            title,
            line: 0,
            outline,
            judgment,
            promise,
            reserved,
            steps,
            examples: if outline { examples } else { Vec::new() },
        })
}

fn any_spec() -> impl Strategy<Value = Spec> {
    (
        "[a-z][a-z0-9]{0,6}(\\.[a-z0-9]{1,5}){0,2}",
        prop::collection::vec("[a-z][a-z0-9-]{0,8}", 0..3),
        words(),
        prop::collection::vec(description_line(), 0..3),
        prop::collection::vec(any_step(), 0..3),
        prop::collection::vec(any_scenario(), 1..4),
        any_parts(),
    )
        .prop_map(|(id, decisions, title, description, background, mut scenarios, (diagrams, mockups))| {
            for (index, scenario) in scenarios.iter_mut().enumerate() {
                scenario.id = format!("{}{index}", scenario.id);
            }
            Spec { id, decisions, title, description, background, diagrams, mockups, scenarios }
        })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x5bec),
        ..ProptestConfig::default()
    })]

    // spec: spec-engine.feature-format/round-trips-any-spec
    #[test]
    fn reading_what_was_written_gives_back_the_same_spec(spec in any_spec()) {
        let text = write_spec(&spec);
        let read = read_spec(PATH, text.as_bytes()).unwrap_or_else(|problems| panic!("{text}\n{problems:#?}"));
        prop_assert_eq!(without_lines(read), spec);
    }

    // spec: spec-engine.feature-format/canonical-text-is-stable
    #[test]
    fn writing_what_was_read_from_any_canonical_text_changes_nothing(spec in any_spec()) {
        let canonical = write_spec(&spec);
        let read = read_spec(PATH, canonical.as_bytes()).unwrap_or_else(|problems| panic!("{canonical}\n{problems:#?}"));
        prop_assert_eq!(write_spec(&read), canonical);
    }
}
