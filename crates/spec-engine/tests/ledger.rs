use alondra_spec_engine::{Code, EvidenceKind, Evidence, Kind, Recorded, State, Verdict, evidence_kind};

use crate::support::{REFUNDS_PATH, Tree, billing, padded, problem, project, rich, state};

const FULL: &str = "billing.refunds/full";
const FULL_ENTRY: &str = "specs/ledger/billing/scenarios/billing.refunds/full.toml";
const SUITE_ENTRY: &str = "specs/ledger/billing/suites/unit.toml";
const OVERVIEW_ENTRY: &str = "specs/ledger/reference/billing.overview.toml";
const SOME_FINGERPRINT: &str = "sha256:5f2b9c0e7a14d3368b1e4f0a9c7d2e5b8a3f6c1d0e9b8a7f6e5d4c3b2a190807";
const SHA256_OF_ABC: &str = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn tagged(tags: &str) -> Tree {
    let mut tree = billing();
    tree.edit(REFUNDS_PATH, "@id:full\n", &format!("@id:full {tags}\n"));
    tree
}

fn evidence(name: &str, bytes: &[u8]) -> Evidence {
    Evidence { name: name.to_string(), bytes: bytes.to_vec() }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn fingerprint(value: &str) -> String {
    format!("sha256:{}", value.repeat(64 / value.len()))
}

// spec: spec-engine.ledger/suite-entry
#[test]
fn a_passed_suite_is_one_line_with_its_fingerprint() {
    let loaded = project(&billing());
    let inputs = loaded.fingerprint("billing/unit", Kind::Suite).unwrap();

    let entry = loaded.suite_passed("billing", "unit").unwrap();

    assert_eq!(entry.path, SUITE_ENTRY);
    assert_eq!(text(&entry.bytes), format!("inputs = \"{inputs}\"\n"));
}

// spec: spec-engine.ledger/scenario-entry
#[test]
fn an_adequacy_verdict_is_the_adequate_table() {
    let loaded = project(&billing());
    let inputs = loaded.fingerprint(FULL, Kind::Adequate).unwrap();

    let entry = loaded.verdict(FULL, Kind::Adequate, Verdict::Met, &[]).unwrap();

    assert_eq!(entry.path, FULL_ENTRY);
    assert_eq!(text(&entry.bytes), format!("[adequate]\ninputs = \"{inputs}\"\nverdict = \"met\"\n"));
}

// spec: spec-engine.ledger/second-verdict-keeps-the-first
#[test]
fn a_judgment_keeps_the_adequacy_already_recorded() {
    let mut tree = tagged("@judged:capture");
    let first = project(&tree);
    let adequate = first.fingerprint(FULL, Kind::Adequate).unwrap();
    tree.apply(&first.verdict(FULL, Kind::Adequate, Verdict::Met, &[]).unwrap());

    let loaded = project(&tree);
    let judged = loaded.fingerprint(FULL, Kind::Judged).unwrap();
    let entry = loaded.verdict(FULL, Kind::Judged, Verdict::Met, &[evidence("refund.png", b"abc")]).unwrap();

    assert_eq!(entry.path, FULL_ENTRY);
    assert_eq!(
        text(&entry.bytes),
        format!(
            "[adequate]\ninputs = \"{adequate}\"\nverdict = \"met\"\n\n[judged]\ninputs = \"{judged}\"\nverdict = \"met\"\nevidence = [\"{SHA256_OF_ABC}\"]\n"
        )
    );
}

// spec: spec-engine.ledger/judgment-needs-its-evidence
#[test]
fn a_judgment_needs_the_kind_of_evidence_its_tag_names() {
    let refund_png = [evidence("refund.png", b"capture")];
    let refund_mov = [evidence("refund.mov", b"video")];
    let refund_mp4 = [evidence("refund.mp4", b"video")];
    let rows: [(&str, &[Evidence], bool); 6] = [
        ("@judged:capture", &[], false),
        ("@judged:capture", &refund_mov, false),
        ("@judged:capture", &refund_png, true),
        ("@judged:video", &refund_png, false),
        ("@judged:video", &refund_mp4, true),
        ("@judged", &[], true),
    ];
    for (tag, given, recorded) in rows {
        let answer = project(&tagged(tag)).verdict(FULL, Kind::Judged, Verdict::Met, given);
        let names: Vec<&str> = given.iter().map(|evidence| evidence.name.as_str()).collect();
        if recorded {
            assert_eq!(answer.unwrap().path, FULL_ENTRY, "{tag} with {names:?}");
        } else {
            let refused = answer.expect_err(&format!("{tag} with {names:?} was recorded"));
            assert_eq!(refused.code, Code::EvidenceRequired, "{tag} with {names:?}");
            assert_eq!(refused.code.as_str(), "evidence_required");
        }
    }
}

// spec: spec-engine.ledger/evidence-kinds-by-extension
#[test]
fn the_extension_says_whether_a_file_is_a_capture_or_a_video() {
    for (name, kind) in [
        ("a.png", EvidenceKind::Capture),
        ("a.jpg", EvidenceKind::Capture),
        ("a.jpeg", EvidenceKind::Capture),
        ("a.webp", EvidenceKind::Capture),
        ("a.mov", EvidenceKind::Video),
        ("a.mp4", EvidenceKind::Video),
        ("a.webm", EvidenceKind::Video),
        ("a.txt", EvidenceKind::None),
        ("a.PNG", EvidenceKind::None),
    ] {
        assert_eq!(evidence_kind(name), kind, "{name}");
    }
}

// spec: spec-engine.ledger/unmet-verdict-is-recorded
#[test]
fn an_unmet_verdict_is_recorded_and_reported() {
    let mut tree = billing();
    let loaded = project(&tree);
    let inputs = loaded.fingerprint(FULL, Kind::Adequate).unwrap();
    let entry = loaded.verdict(FULL, Kind::Adequate, Verdict::Unmet, &[]).unwrap();
    assert_eq!(text(&entry.bytes), format!("[adequate]\ninputs = \"{inputs}\"\nverdict = \"unmet\"\n"));

    tree.apply(&entry);
    let reloaded = project(&tree);

    assert_eq!(reloaded.recorded(FULL, Kind::Adequate).unwrap().verdict, Some(Verdict::Unmet));
    assert_eq!(state(&reloaded.report(None), FULL, Kind::Adequate), State::Unmet);
}

// spec: spec-engine.ledger/reference-entry
#[test]
fn a_reference_verdict_is_written_at_the_section_path() {
    let mut tree = rich();
    let loaded = project(&tree);
    let inputs = loaded.fingerprint("billing.overview", Kind::Reference).unwrap();

    let entry = loaded.verdict("billing.overview", Kind::Reference, Verdict::Met, &[]).unwrap();
    assert_eq!(entry.path, OVERVIEW_ENTRY);
    assert!(text(&entry.bytes).starts_with("[reference]\n"), "{}", text(&entry.bytes));
    tree.apply(&entry);

    assert_eq!(
        project(&tree).recorded("billing.overview", Kind::Reference),
        Some(Recorded { inputs, verdict: Some(Verdict::Met), evidence: Vec::new() })
    );
}

// spec: spec-engine.ledger/reads-entries-written-by-format-1
#[test]
fn entries_written_by_format_1_still_read() {
    let mut tree = rich();
    tree.edit(REFUNDS_PATH, "@id:full\n", "@id:full @judged:capture\n");
    tree.put(SUITE_ENTRY, include_str!("../fixtures/ledger/format-1/suite.toml"));
    tree.put(FULL_ENTRY, include_str!("../fixtures/ledger/format-1/scenario.toml"));
    tree.put(OVERVIEW_ENTRY, include_str!("../fixtures/ledger/format-1/reference.toml"));

    let loaded = project(&tree);

    assert_eq!(
        loaded.recorded("billing/unit", Kind::Suite),
        Some(Recorded { inputs: fingerprint("0123456789abcdef"), verdict: None, evidence: Vec::new() })
    );
    assert_eq!(
        loaded.recorded(FULL, Kind::Adequate),
        Some(Recorded { inputs: fingerprint("fedcba9876543210"), verdict: Some(Verdict::Met), evidence: Vec::new() })
    );
    assert_eq!(
        loaded.recorded(FULL, Kind::Judged),
        Some(Recorded {
            inputs: fingerprint("00112233445566778899aabbccddeeff"),
            verdict: Some(Verdict::Unmet),
            evidence: vec![fingerprint("a1b2c3d4e5f60718"), fingerprint("9a8b7c6d5e4f3021")],
        })
    );
    assert_eq!(
        loaded.recorded("billing.overview", Kind::Reference),
        Some(Recorded { inputs: fingerprint("13579bdf02468ace"), verdict: Some(Verdict::Met), evidence: Vec::new() })
    );
}

// spec: spec-engine.ledger/unreadable-entry-is-a-problem
#[test]
fn a_ledger_file_the_engine_cannot_trust_is_refused() {
    for (path, bytes) in [
        (FULL_ENTRY, "[adequate\ninputs = \"\"\n".to_string()),
        (SUITE_ENTRY, format!("inputs = \"{SOME_FINGERPRINT}\"\nnote = \"looked fine\"\n")),
        (FULL_ENTRY, format!("[adequate]\ninputs = \"{SOME_FINGERPRINT}\"\nverdict = \"maybe\"\n")),
        (SUITE_ENTRY, "inputs = \"abc\"\n".to_string()),
    ] {
        let tree = billing().with(path, &bytes);

        let found = problem(&tree);

        assert_eq!(found.code, Code::InvalidLedger, "{bytes}: {found:#?}");
        assert_eq!(found.path, path, "{bytes}");
    }
}

// spec: spec-engine.ledger/judged-entry-needs-its-evidence
#[test]
fn a_judged_entry_written_without_evidence_is_refused_when_its_judgment_needs_some() {
    for (tag, refused) in [("@judged:capture", true), ("@judged:video", true), ("@judged", false)] {
        let mut tree = tagged(tag);
        let inputs = project(&tree).fingerprint(FULL, Kind::Judged).unwrap();
        tree.put(FULL_ENTRY, &format!("[judged]\ninputs = \"{inputs}\"\nverdict = \"met\"\n"));

        if refused {
            let found = problem(&tree);
            assert_eq!(found.code, Code::EvidenceRequired, "{tag}: {found:#?}");
            assert_eq!(found.path, FULL_ENTRY, "{tag}");
        } else {
            assert_eq!(state(&project(&tree).report(None), FULL, Kind::Judged), State::Valid, "{tag}");
        }
    }
}

// spec: spec-engine.ledger/oversized-entry-is-a-problem
#[test]
fn a_ledger_file_beyond_the_size_limit_is_refused() {
    let entry = format!("inputs = \"{SOME_FINGERPRINT}\"\n");
    assert!(project(&billing().with(SUITE_ENTRY, &padded(&entry, '#', 65_536))).recorded("billing/unit", Kind::Suite).is_some());

    let found = problem(&billing().with(SUITE_ENTRY, &padded(&entry, '#', 65_537)));

    assert_eq!(found.code, Code::TooLarge, "{found:#?}");
    assert_eq!(found.path, SUITE_ENTRY);
}

// spec: spec-engine.ledger/entry-of-nothing-is-ignored
#[test]
fn an_entry_of_a_scenario_that_no_longer_exists_is_listed_as_leftover() {
    let gone = "specs/ledger/billing/scenarios/billing.refunds/gone.toml";
    let tree = billing().with(gone, &format!("[adequate]\ninputs = \"{SOME_FINGERPRINT}\"\nverdict = \"met\"\n"));

    let report = project(&tree).report(None);

    assert_eq!(report.leftover, [gone]);
}

// spec: spec-engine.ledger/recording-unknown-subject-is-a-problem
#[test]
fn recording_a_verdict_for_nothing_is_refused() {
    let refused = project(&billing()).verdict("billing.refunds/gone", Kind::Adequate, Verdict::Met, &[]).unwrap_err();

    assert_eq!(refused.code, Code::UnknownSubject);
    assert_eq!(refused.code.as_str(), "unknown_subject");
}

// spec: spec-engine.ledger/judgment-of-plain-scenario-is-a-problem
#[test]
fn recording_a_judgment_for_a_scenario_that_is_not_judged_is_refused() {
    let refused = project(&billing()).verdict(FULL, Kind::Judged, Verdict::Met, &[]).unwrap_err();

    assert_eq!(refused.code, Code::NotJudged);
    assert_eq!(refused.code.as_str(), "not_judged");
}
