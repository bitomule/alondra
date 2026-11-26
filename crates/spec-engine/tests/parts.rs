use std::collections::BTreeSet;

use alondra_spec_engine::{Code, Kind, PartContent, Problem, Spec, read_spec, write_spec};
use proptest::prelude::*;

use crate::support::{REFUNDS_PATH, Tree, billing, is_fingerprint, line_of, problem, problem_with, problems, project};

const DIAGRAM: &str = "specs/billing/flow.mmd";
const MOCKUP: &str = "specs/billing/sheet.png";

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes
}

/// A JPEG with an application segment and a table segment before its frame header.
fn jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc4, 0x00, 0x02, 0xff, 0xff, 0xc2, 0x00, 0x11, 0x08];
    bytes.extend(height.to_be_bytes());
    bytes.extend(width.to_be_bytes());
    bytes
}

fn feature(feature_tags: &str, scenario_tags: &str) -> String {
    format!("@spec:billing.refunds\n{feature_tags}\nFeature: Refunds\n\n  @id:full {scenario_tags}\n  Scenario: Full refund\n    Given a paid order\n")
}

fn read(text: &str) -> Result<Spec, Vec<Problem>> {
    read_spec(REFUNDS_PATH, text.as_bytes())
}

/// `billing` with the tag `tag` on the scenario `full`.
fn citing(tag: &str) -> Tree {
    let mut tree = billing();
    tree.edit(REFUNDS_PATH, "@id:full", &format!("@id:full {tag}"));
    tree
}

fn with_part(tag: &str, path: &str, bytes: &[u8]) -> Tree {
    let mut tree = citing(tag);
    tree.put_bytes(path, bytes);
    tree
}

/// The one problem of a part, which names the feature, the line of the tag and the file.
fn refused(tree: &Tree, tag: &str, path: &str, code: Code) {
    let found = problem(tree);
    assert_eq!(found.code, code, "{found:?}");
    assert_eq!(found.path, REFUNDS_PATH);
    assert_eq!(found.line, line_of(&tree.text(REFUNDS_PATH), tag));
    assert!(found.message.contains(path), "{}", found.message);
}

// spec: spec-engine.parts/reads-part-tags
#[test]
fn reads_the_parts_a_feature_and_a_scenario_cite() {
    let spec = read(&feature("@diagram:flow.mmd", "@mockup:screens/sheet.png @mockup:empty.jpg")).unwrap();

    assert_eq!(spec.diagrams, ["flow.mmd"]);
    assert!(spec.mockups.is_empty());
    assert!(spec.scenarios[0].diagrams.is_empty());
    assert_eq!(spec.scenarios[0].mockups, ["screens/sheet.png", "empty.jpg"]);
}

// spec: spec-engine.parts/writes-part-tags-in-canonical-order
#[test]
fn writes_the_tags_of_parts_after_the_others() {
    let text = "@mockup:a.png @diagram:a.mmd @decision:keep-ledger @spec:billing.refunds\nFeature: Refunds\n\n  @mockup:b.png @reserved @diagram:b.mmd @boundary:data @evidence:shots/*.png @judged:capture @id:full\n  Scenario: Full refund\n    Given a paid order\n";

    let written = write_spec(&read(text).unwrap());

    assert!(written.starts_with("@spec:billing.refunds\n@decision:keep-ledger\n@diagram:a.mmd\n@mockup:a.png\nFeature: Refunds\n"), "{written}");
    assert!(
        written.contains("\n  @id:full @judged:capture @evidence:shots/*.png @boundary:data @reserved @diagram:b.mmd @mockup:b.png\n"),
        "{written}"
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x9a27),
        ..ProptestConfig::default()
    })]

    // spec: spec-engine.fingerprints/a-part-changes-only-what-cites-it
    #[test]
    fn a_part_changes_the_validations_of_what_cites_it_and_no_other(
        on_feature in prop::collection::btree_set(0usize..4, 0..3),
        on_full in prop::collection::btree_set(0usize..4, 0..3),
        on_partial in prop::collection::btree_set(0usize..4, 0..3),
        changed in 0usize..4,
    ) {
        prop_assume!(on_feature.contains(&changed) || on_full.contains(&changed) || on_partial.contains(&changed));
        let tags = |cited: &BTreeSet<usize>| cited.iter().map(|part| format!("@mockup:{part}.png")).collect::<Vec<_>>().join(" ");
        let mut tree = billing();
        tree.edit(REFUNDS_PATH, "Feature:", &format!("{}\nFeature:", tags(&on_feature)));
        tree.edit(REFUNDS_PATH, "@id:full", &format!("@id:full @judged {}", tags(&on_full)));
        tree.edit(REFUNDS_PATH, "@id:partial", &format!("@id:partial {}", tags(&on_partial)));
        for part in on_feature.iter().chain(&on_full).chain(&on_partial) {
            tree.put_bytes(&format!("specs/billing/{part}.png"), &png(1, 1));
        }

        let differ: BTreeSet<(String, Kind)> =
            changed_by(&mut tree, &format!("specs/billing/{changed}.png"), &png(2, 2)).into_iter().collect();

        let mut expected = BTreeSet::new();
        for (scenario, own) in [("full", Some(&on_full)), ("partial", Some(&on_partial)), ("stands-alone", None)] {
            let subject = format!("billing.refunds/{scenario}");
            let own_changed = own.is_some_and(|own| own.contains(&changed));
            if on_feature.contains(&changed) || own_changed {
                expected.insert((subject.clone(), Kind::Adequate));
            }
            if scenario == "full" && own_changed {
                expected.insert((subject, Kind::Judged));
            }
        }
        prop_assert_eq!(differ, expected);
    }
}

fn all_fingerprints(tree: &Tree) -> Vec<(String, Kind, Option<String>)> {
    let project = project(tree);
    let mut subjects = vec![("billing/unit".to_string(), Kind::Suite)];
    for scenario in ["full", "partial", "stands-alone"] {
        for kind in [Kind::Adequate, Kind::Judged] {
            subjects.push((format!("billing.refunds/{scenario}"), kind));
        }
    }
    subjects.into_iter().map(|(subject, kind)| (subject.clone(), kind, project.fingerprint(&subject, kind))).collect()
}

/// The subjects whose fingerprint changes when the bytes of `path` become `bytes`.
fn changed_by(tree: &mut Tree, path: &str, bytes: &[u8]) -> Vec<(String, Kind)> {
    let before = all_fingerprints(tree);
    tree.put_bytes(path, bytes);
    before.into_iter().zip(all_fingerprints(tree)).filter(|(old, new)| old != new).map(|(old, _)| (old.0, old.1)).collect()
}

// spec: spec-engine.fingerprints/scenario-part-changes-its-validations
#[test]
fn a_part_of_a_scenario_changes_its_adequacy_and_judgment() {
    let mut tree = with_part("@judged @mockup:sheet.png", MOCKUP, &png(10, 20));

    let differ = changed_by(&mut tree, MOCKUP, &png(10, 21));

    let full = "billing.refunds/full".to_string();
    assert_eq!(differ, [(full.clone(), Kind::Adequate), (full, Kind::Judged)]);
}

// spec: spec-engine.fingerprints/feature-part-changes-every-adequacy
#[test]
fn a_part_of_the_feature_changes_every_adequacy_and_no_judgment() {
    let mut tree = citing("@judged");
    tree.edit(REFUNDS_PATH, "Feature:", "@diagram:flow.mmd\nFeature:");
    tree.put(DIAGRAM, "flowchart LR\n  a --> b\n");

    let differ = changed_by(&mut tree, DIAGRAM, b"flowchart LR\n  a --> c\n");

    let adequate: Vec<(String, Kind)> =
        ["full", "partial", "stands-alone"].map(|scenario| (format!("billing.refunds/{scenario}"), Kind::Adequate)).into();
    assert_eq!(differ, adequate);
}

// spec: spec-engine.parts/rejects-part-outside-component
#[test]
fn a_tag_cannot_leave_the_folder_of_the_component() {
    for file in ["/etc/flow.mmd", "../ledger/flow.mmd", "a/../flow.mmd", "./flow.mmd", "a//flow.mmd", "a\\flow.mmd"] {
        let text = feature("", &format!("@diagram:{file}"));

        let found = read(&text).unwrap_err();

        assert_eq!(found.len(), 1, "{file}: {found:?}");
        assert_eq!(
            (found[0].code, found[0].path.as_str(), found[0].line),
            (Code::PartOutsideComponent, REFUNDS_PATH, line_of(&text, "@id:full")),
            "{file}"
        );
    }
}

// spec: spec-engine.parts/rejects-part-tag-that-cites-no-part
#[test]
fn a_tag_whose_file_cannot_be_that_part_is_refused() {
    for tags in [
        "@diagram:flow.png",
        "@diagram:",
        "@diagram:.mmd",
        "@mockup:sheet.webp",
        "@mockup:sheet.mmd",
        "@mockup:sheet.png @mockup:sheet.png",
        "@diagram:flow.mmd @diagram:flow.mmd",
    ] {
        let text = feature("", tags);

        let found = read(&text).unwrap_err();

        assert_eq!(found.len(), 1, "{tags}: {found:?}");
        assert_eq!((found[0].code, found[0].line), (Code::InvalidPart, line_of(&text, "@id:full")), "{tags}");
    }
}

// spec: spec-engine.parts/limits-parts-per-spec
#[test]
fn a_spec_cites_at_most_sixteen_parts() {
    let feature_tags = (0..8).map(|n| format!("@diagram:{n}.mmd")).collect::<Vec<_>>().join("\n");
    let scenario_tags = |count: usize| (0..count).map(|n| format!("@mockup:{n}.png")).collect::<Vec<_>>().join(" ");

    assert!(read(&feature(&feature_tags, &scenario_tags(8))).is_ok());
    assert_eq!(read(&feature(&feature_tags, &scenario_tags(9))).unwrap_err().len(), 1);
    let text = feature(&feature_tags, &format!("{}\n  @mockup:later.png", scenario_tags(9)));
    let found = read(&text).unwrap_err();

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].code, found[0].line), (Code::TooManyParts, line_of(&text, "@id:full")));
}

// spec: spec-engine.parts/missing-part-is-a-problem
#[test]
fn a_cited_file_that_is_not_listed_is_missing() {
    refused(&citing("@mockup:sheet.png"), "@mockup:sheet.png", MOCKUP, Code::MissingPart);
}

// spec: spec-engine.parts/unreadable-part-is-a-problem
#[test]
fn a_cited_file_that_cannot_be_read_is_named() {
    let tree = with_part("@mockup:sheet.png", MOCKUP, &png(1, 1)).unreadable(MOCKUP);

    let found = problem(&tree);

    assert_eq!((found.code, found.path.as_str()), (Code::UnreadableFile, MOCKUP));
}

// spec: spec-engine.parts/rejects-oversized-part
#[test]
fn a_part_beyond_its_limit_is_refused() {
    let diagram = |size: usize| {
        let mut bytes = b"graph TD\n".to_vec();
        bytes.resize(size, b' ');
        bytes
    };
    let mockup = |size: usize| {
        let mut bytes = png(1, 1);
        bytes.resize(size, 0);
        bytes
    };

    project(&with_part("@diagram:flow.mmd", DIAGRAM, &diagram(65_536)));
    refused(&with_part("@diagram:flow.mmd", DIAGRAM, &diagram(65_537)), "@diagram:", DIAGRAM, Code::TooLarge);
    project(&with_part("@mockup:sheet.png", MOCKUP, &mockup(2_097_152)));
    refused(&with_part("@mockup:sheet.png", MOCKUP, &mockup(2_097_153)), "@mockup:", MOCKUP, Code::TooLarge);
}

// spec: spec-engine.parts/diagram-is-of-a-drawable-kind
#[test]
fn reads_every_kind_of_diagram_the_clients_draw() {
    for first in ["flowchart LR", "graph TD;", "sequenceDiagram", "stateDiagram", "stateDiagram-v2", "classDiagram"] {
        let text = format!("\n%% how it works\n  {first}\n  a --> b\n");

        let loaded = project(&with_part("@diagram:flow.mmd", DIAGRAM, text.as_bytes()));

        assert_eq!(loaded.part(DIAGRAM).map(|part| part.content.clone()), Some(PartContent::Diagram { text }), "{first}");
    }
}

// spec: spec-engine.parts/rejects-undrawable-diagram
#[test]
fn a_diagram_no_client_draws_is_refused() {
    for bytes in [&b"gantt\n  title x\n"[..], b"flowcharts LR\n", b"%% nothing\n", b"", b"graph TD\n\xff\n"] {
        let tree = with_part("@diagram:flow.mmd", DIAGRAM, bytes);

        refused(&tree, "@diagram:", DIAGRAM, Code::InvalidPart);
    }
}

// spec: spec-engine.parts/reads-mockup-size
#[test]
fn reads_the_size_of_a_png_and_of_a_jpeg() {
    for bytes in [png(1170, 2532), jpeg(1170, 2532)] {
        let loaded = project(&with_part("@mockup:sheet.png", MOCKUP, &bytes));

        let part = loaded.part(MOCKUP).unwrap();
        assert_eq!(part.content, PartContent::Mockup { width: 1170, height: 2532 });
        assert_eq!(part.bytes, bytes.len());
    }
}

// spec: spec-engine.parts/rejects-mockup-that-is-no-image
#[test]
fn a_mockup_that_is_no_image_is_refused() {
    let mut headless = png(1, 1);
    headless[12..16].copy_from_slice(b"IDAT");
    let frameless = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xd9];
    let signature = png(1, 1)[..8].to_vec();

    for bytes in [b"not an image".to_vec(), signature, headless, png(0, 10), frameless] {
        let tree = with_part("@mockup:sheet.png", MOCKUP, &bytes);

        refused(&tree, "@mockup:", MOCKUP, Code::InvalidPart);
    }
}

// spec: spec-engine.parts/uncited-part-is-a-problem
#[test]
fn a_file_of_the_folder_that_nothing_cites_is_a_problem() {
    let mut tree = citing("@judged:capture @evidence:specs/billing/shots/*.png");
    for path in [
        "specs/billing/a.mmd",
        "specs/billing/b.png",
        "specs/billing/d.jpeg",
        "specs/billing/more/c.jpg",
        "specs/billing/shots/proof.png",
        "specs/billing/notes.md",
        "specs/loose.png",
        "specs/nobody/e.png",
    ] {
        tree.put(path, "x");
    }

    let found = problems(&tree);

    let named: Vec<(&str, Code, usize)> = found.iter().map(|found| (found.path.as_str(), found.code, found.line)).collect();
    let uncited = ["specs/billing/a.mmd", "specs/billing/b.png", "specs/billing/d.jpeg", "specs/billing/more/c.jpg"];
    assert_eq!(named, uncited.map(|path| (path, Code::UncitedPart, 0)));

    tree.edit(REFUNDS_PATH, "@id:partial", "@id:partial @unknown");
    let unknown = problem_with(&tree, Code::UnknownTag);
    assert_eq!(problems(&tree), [unknown]);
}

// spec: spec-engine.parts/listing-names-parts
#[test]
fn the_listing_names_the_parts_of_a_spec_and_of_each_scenario() {
    let diagram = "graph TD\n";
    let mockup = png(1170, 2532);
    let mut tree = with_part("@mockup:sheet.png", MOCKUP, &mockup);
    tree.edit(REFUNDS_PATH, "Feature:", "@diagram:flow.mmd\nFeature:");
    tree.put(DIAGRAM, diagram);

    let loaded = project(&tree);
    let listing: serde_json::Value = serde_json::from_str(&loaded.listing()).unwrap();

    let spec = &listing["specs"][0];
    let fingerprint = |path: &str| loaded.part(path).unwrap().fingerprint.clone();
    assert_eq!(
        spec["parts"],
        serde_json::json!([{ "path": DIAGRAM, "kind": "diagram", "fingerprint": fingerprint(DIAGRAM), "bytes": diagram.len(), "width": null, "height": null }])
    );
    assert_eq!(
        spec["scenarios"][0]["parts"],
        serde_json::json!([{ "path": MOCKUP, "kind": "mockup", "fingerprint": fingerprint(MOCKUP), "bytes": mockup.len(), "width": 1170, "height": 2532 }])
    );
    assert_eq!(spec["scenarios"][1]["parts"], serde_json::json!([]));
    assert_eq!(fingerprint(DIAGRAM), "sha256:228e1f41f3c2674cf342c6df608dd87f63c034682c6575017585c9eec08ceb37");
    assert!(is_fingerprint(&fingerprint(MOCKUP)));
}
