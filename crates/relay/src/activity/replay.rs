//! What the relay sends to APNs for two weeks of one Mac's reports. The reports are what the
//! daemon's replay produced (`crates/alondrad/src/activity_push/replay.rs`) for synthetic work; the
//! phone is one that registered both tokens, whose Live Activity APNs always takes. The result is
//! the table in `docs/architecture/live-activity.md`.
//!
//! Regenerate it after a change of the limits with
//! `ALONDRA_WRITE_PUSH_FIXTURE=1 bazelisk run //crates/relay:alondra_relay_test -- activity::replay`.
use super::*;
use std::path::Path;

const MEASUREMENT_FIXTURE: &str = "fixtures/push/activity_measurement.json";
const HOUR_MS: u64 = 3_600_000;
/// An arbitrary Monday midnight in the phone's time zone, so a report at `t` ms happens at `START + t`.
const START: u64 = 1_790_208_000_000;

struct Reported {
    at_ms: u64,
    active: bool,
    may_start: bool,
    urgent: bool,
    unchanged: bool,
}

struct Pushed {
    at_ms: u64,
    push: Send,
    bytes: usize,
    waited_ms: u64,
}

fn reports() -> Vec<Reported> {
    let fixture: Value = serde_json::from_str(include_str!("../../../../fixtures/push/activity_reports.json")).unwrap();
    fixture["reports"].as_array().unwrap().iter()
        .map(|row| Reported {
            at_ms: row[0].as_u64().unwrap(),
            active: row[1].as_bool().unwrap(),
            may_start: row[2].as_bool().unwrap(),
            urgent: row[3].as_bool().unwrap(),
            unchanged: row[4].as_bool().unwrap(),
        })
        .collect()
}

fn mac_envelope() -> Envelope {
    let fixture: Value = serde_json::from_str(include_str!("../../../../fixtures/push/activity.json")).unwrap();
    serde_json::from_value(fixture["body"]["envelope"].clone()).unwrap()
}

fn phone() -> Phone {
    Phone {
        owner_sub: "owner".into(),
        environment: "production".into(),
        frequent_updates: true,
        start_token: Some("ab".repeat(80)),
        live: None,
        pairings: BTreeMap::from([("phone-device".to_owned(), "mac".to_owned())]),
        reports: BTreeMap::new(),
        last_start_ms: None,
        last_urgent_ms: None,
        last_sent_ms: None,
        urgent_pending: false,
        unsent: false,
    }
}

/// When the relay next tries to push: now, or once the wait after the last push ends.
fn flush_at(phone: &Phone, now: u64) -> Option<u64> {
    phone.unsent.then(|| phone.last_sent_ms.map_or(now, |last| last.saturating_add(phone.spacing(now)).max(now)))
}

fn replay(reports: &[Reported]) -> Vec<Pushed> {
    let envelope = mac_envelope();
    let mut phone = phone();
    let mut pushed = Vec::new();
    let mut pending_since = None;
    let mut next = 0;
    let mut now = 0;
    loop {
        let report_at = reports.get(next).map(|report| START + report.at_ms);
        let flush = flush_at(&phone, now);
        let Some(at) = [report_at, flush].into_iter().flatten().min() else { break };
        now = at;
        if report_at == Some(at) {
            let report = &reports[next];
            next += 1;
            phone.urgent_pending |= report.urgent;
            phone.unsent = true;
            pending_since.get_or_insert(at);
            phone.reports.insert("mac".into(), Report {
                device_id: "phone-device".into(),
                envelope: envelope.clone(),
                active: report.active,
                may_start: report.may_start,
                urgent: report.urgent,
                received_at_ms: at,
            });
            continue;
        }
        let Some(pushes) = phone.take_pushes(at) else { continue };
        for (push, token, body) in pushes {
            pushed.push(Pushed {
                at_ms: at - START,
                bytes: serde_json::to_vec(&body).unwrap().len(),
                waited_ms: at - pending_since.unwrap_or(at),
                push: push.clone(),
            });
            match push {
                Send::Start => {
                    phone.live.as_mut().expect("a start leaves the activity known").update_token = Some("cd".repeat(40));
                }
                Send::End if phone.live.as_ref().and_then(|live| live.update_token.as_ref()) == Some(&token) => phone.live = None,
                Send::End | Send::Update { .. } => {}
            }
        }
        pending_since = None;
    }
    pushed
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    sorted[(sorted.len() * percent).div_ceil(100).max(1) - 1]
}

fn per_hour(times: impl Iterator<Item = u64>) -> Value {
    let mut hours: BTreeMap<u64, u64> = BTreeMap::new();
    for at in times {
        *hours.entry(at / HOUR_MS).or_default() += 1;
    }
    let mut counts: Vec<u64> = hours.values().copied().collect();
    counts.sort();
    let total: u64 = counts.iter().sum();
    json!({
        "total": total,
        "hours_with_any": counts.len(),
        "mean_in_those_hours": (total as f64 / counts.len() as f64 * 10.0).round() / 10.0,
        "p95_in_those_hours": percentile(&counts, 95),
        "worst_hour": counts.last(),
    })
}

fn seconds(waits: &mut [u64]) -> Value {
    waits.sort();
    json!({"p95_s": percentile(waits, 95) / 1000, "worst_s": waits.last().map(|ms| ms / 1000)})
}

fn measurement() -> Value {
    let reports = reports();
    let pushed = replay(&reports);
    let of = |wanted: fn(&Send) -> bool| pushed.iter().filter(move |one| wanted(&one.push));
    let mut waits = |wanted: fn(&Send) -> bool| {
        let mut waits: Vec<u64> = of(wanted).map(|one| one.waited_ms).collect();
        seconds(&mut waits)
    };
    json!({
        "days": 14,
        "mac_reports": per_hour(reports.iter().map(|report| report.at_ms)),
        "mac_reports_repeating_the_last_state": reports.iter().filter(|report| report.unchanged).count(),
        "pushes": per_hour(pushed.iter().map(|one| one.at_ms)),
        "priority_10": per_hour(of(|push| matches!(push, Send::Update { priority: 10 })).map(|one| one.at_ms)),
        "starts": of(|push| matches!(push, Send::Start)).count(),
        "ends": of(|push| matches!(push, Send::End)).count(),
        "largest_push_bytes": pushed.iter().map(|one| one.bytes).max(),
        "wait_after_a_report": {
            "priority_5": waits(|push| matches!(push, Send::Update { priority: 5 })),
            "priority_10": waits(|push| matches!(push, Send::Update { priority: 10 })),
        },
    })
}

#[test]
fn the_pushes_of_two_weeks_of_reports_are_the_measurement_the_docs_quote() {
    let mut text = serde_json::to_string_pretty(&measurement()).unwrap();
    text.push('\n');
    if std::env::var_os("ALONDRA_WRITE_PUSH_FIXTURE").is_some() {
        let root = std::env::var("BUILD_WORKSPACE_DIRECTORY").expect("run through bazelisk run");
        std::fs::write(Path::new(&root).join(MEASUREMENT_FIXTURE), text).unwrap();
        return;
    }
    assert!(
        include_str!("../../../../fixtures/push/activity_measurement.json") == text,
        "the pushes changed: regenerate fixtures/push/activity_measurement.json and update docs/architecture/live-activity.md"
    );
}
