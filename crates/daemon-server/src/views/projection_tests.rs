use super::feed::sample_changed;
use super::live::Snapshot;
use super::observation::{Metric, Observation};
use alondra_view_model::{Gauge, GaugePart, Level, LiveSnapshot};

fn projection(value: f64, part_value: f64) -> LiveSnapshot {
    LiveSnapshot {
        gauges: vec![Gauge {
            id: "fixture".into(), label: "Fixture".into(), value,
            text: "Same reading".into(), detail: None, level: Level::Calm,
            parts: vec![GaugePart {
                label: "Account".into(), value: part_value, text: "Same reading".into(),
                detail: None, level: Level::Calm, group: None, lines: vec![],
            }],
        }],
        ..Default::default()
    }
}

fn observing() -> Observation {
    let observation = Observation::armed();
    observation.start();
    observation
}

#[test]
fn signed_zero_changes_the_wire_payload_in_gauges_and_their_parts() {
    for (value, part_value) in [(-0.0, 0.0), (0.0, -0.0)] {
        let positive = Snapshot::new(projection(0.0, 0.0));
        let negative = Snapshot::new(projection(value, part_value));
        assert_eq!(positive, negative, "numeric equality alone cannot decide wire equality");
        assert_ne!(serde_json::to_vec(&positive).unwrap(), serde_json::to_vec(&negative).unwrap());
        assert!(!positive.same_payload(&negative));
        assert_ne!(positive.digest(), negative.digest());
        let observation = observing();
        let mut last = None;
        assert!(sample_changed(positive.clone(), &mut last, &observation));
        assert!(sample_changed(negative, &mut last, &observation));
        assert!(sample_changed(positive, &mut last, &observation));
        assert_eq!(observation.totals().metrics[Metric::SamplerSerializations as usize], 0,
            "prepared component hashes need no further serialization");
    }
}

#[test]
fn nonfinite_readings_keep_the_existing_json_digest_without_extra_changes() {
    for in_part in [false, true] {
        let reading = |value| Snapshot::new(if in_part { projection(0.5, value) } else { projection(value, 0.5) });
        let first = reading(f64::NAN);
        let repeated = reading(f64::NAN);
        let infinity = reading(f64::INFINITY);
        assert!(!first.same_payload(&repeated), "NaN takes the existing serialization fallback");
        assert!(!first.same_payload(&infinity));
        assert_eq!(serde_json::to_vec(&first).unwrap(), serde_json::to_vec(&infinity).unwrap());
        assert_eq!(first.digest(), repeated.digest());
        assert_eq!(first.digest(), infinity.digest());
        let observation = observing();
        let mut last = None;
        assert!(sample_changed(first, &mut last, &observation));
        assert!(!sample_changed(repeated, &mut last, &observation));
        assert!(!sample_changed(infinity, &mut last, &observation));
        assert_eq!(observation.totals().metrics[Metric::SamplerSerializations as usize], 0,
            "prepared component hashes need no further serialization");
    }
}

#[test]
fn a_new_reading_minute_changes_the_payload_and_sampler_revision() {
    let mut first = projection(0.5, 0.25);
    first.observed_at = Some(60_000);
    let mut later = first.clone();
    later.observed_at = Some(120_000);
    let first = Snapshot::new(first);
    let later = Snapshot::new(later);
    assert!(!first.same_payload(&later));
    assert_ne!(first.digest(), later.digest());
    let observation = observing();
    let mut last = None;
    assert!(sample_changed(first, &mut last, &observation));
    assert!(sample_changed(later.clone(), &mut last, &observation));
    let before = observation.totals();
    assert!(!sample_changed(later, &mut last, &observation));
    let after = observation.totals();
    for metric in [Metric::SamplerSerializations, Metric::SamplerBytes] {
        assert_eq!(after.metrics[metric as usize] - before.metrics[metric as usize], 0);
    }
}

#[test]
fn freshly_rebuilt_equal_projections_do_not_serialize_in_the_sampler() {
    let live = projection(0.5, 0.25);
    let observation = observing();
    let mut last = None;
    assert!(sample_changed(Snapshot::new(live.clone()), &mut last, &observation));
    let before = observation.totals();
    assert_eq!(before.metrics[Metric::SamplerSerializations as usize],
        serde_json::to_value(&live).unwrap().as_object().unwrap().len() as u64 - 1,
        "the empty node tree needs no JSON serialization");
    assert!(before.metrics[Metric::SamplerBytes as usize] > 0);
    for _ in 0..32 {
        assert!(!sample_changed(Snapshot::new(live.clone()), &mut last, &observation));
    }
    let after = observation.totals();
    for metric in [Metric::SamplerSerializations, Metric::SamplerBytes] {
        assert_eq!(after.metrics[metric as usize] - before.metrics[metric as usize], 0,
            "fresh allocation with the same payload must not repeat JSON serialization");
    }
}

#[test]
fn changing_the_clock_does_not_reserialize_unchanged_nodes() {
    let mut live: LiveSnapshot = serde_json::from_value(serde_json::json!({
        "nodes": (0..32).map(|id| serde_json::json!({
            "agent_id": format!("fixture-{id}"), "name": "Node", "working": false,
            "last_message": "unchanged content ".repeat(128)
        })).collect::<Vec<_>>()
    })).unwrap();
    let observation = observing();
    let mut last = None;
    assert!(sample_changed(Snapshot::new(live.clone()), &mut last, &observation));
    let warm = observation.totals();
    for minute in 1..=32 {
        live.observed_at = Some(minute * 60_000);
        assert!(sample_changed(Snapshot::new(live.clone()), &mut last, &observation));
    }
    let repeated = observation.totals();
    assert_eq!(repeated.metrics[Metric::SamplerNodesBytes as usize]
        - warm.metrics[Metric::SamplerNodesBytes as usize], 0,
        "changing a clock must not serialize the unchanged node collection again");
    assert_eq!(repeated.metrics[Metric::ComponentSerializations as usize]
        - warm.metrics[Metric::ComponentSerializations as usize], 32);
    assert_eq!(repeated.metrics[Metric::HashOperations as usize]
        - warm.metrics[Metric::HashOperations as usize], 64,
        "each change hashes one scalar component and one bounded root manifest");
    assert!(repeated.metrics[Metric::HashBytes as usize] - warm.metrics[Metric::HashBytes as usize] < 32 * 4096);
    assert_eq!(repeated.metrics[Metric::ComponentRowsCompared as usize]
        - warm.metrics[Metric::ComponentRowsCompared as usize], 32 * 32,
        "this phase still compares reconstructed nodes; the counter must expose that work");
    assert_eq!(repeated.metrics[Metric::Materializations as usize], 0);
    live.nodes[7].last_message = Some("Changed content".into());
    assert!(sample_changed(Snapshot::new(live), &mut last, &observation));
    assert!(observation.totals().metrics[Metric::SamplerNodesBytes as usize]
        > repeated.metrics[Metric::SamplerNodesBytes as usize]);
}
