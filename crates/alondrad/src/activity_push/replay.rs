//! Two weeks of synthetic work, replayed through [`Schedule`] to see what a Mac reports to the
//! relay. The reports are the input of the relay's replay (`crates/relay/src/activity/replay.rs`),
//! which counts the pushes they cause; `docs/architecture/live-activity.md` quotes both.
//!
//! Regenerate both fixtures after a change of the rules with
//! `ALONDRA_WRITE_PUSH_FIXTURE=1 bazelisk run //crates/alondrad:alondrad_test -- activity_push::replay`.
use super::*;
use std::fmt::Write as _;
use std::path::Path;

const TIMELINE_FIXTURE: &str = "fixtures/push/activity_timeline.json";
const REPORTS_FIXTURE: &str = "fixtures/push/activity_reports.json";
const DAYS: u64 = 14;
const DAY_S: u64 = 24 * 3600;
/// The phone's quiet hours, as the relay is told in `may_start`: 23:00 to 8:00.
const QUIET_FROM_S: u64 = 23 * 3600;
const QUIET_UNTIL_S: u64 = 8 * 3600;

fn counts(inbox: u32, waiting: u32, working: u32) -> ActivityCounts {
    ActivityCounts { inbox, waiting, working }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }
}

/// One change of the three counts, in seconds from the start of the first day.
struct Delta {
    at: u64,
    inbox: i64,
    waiting: i64,
    working: i64,
}

/// A task runs for a while, asks a few questions that wait for an answer, and leaves a result in
/// the Inbox until it is looked at; days of the weekend carry far fewer.
fn tasks_of(day: u64, rng: &mut Rng) -> Vec<Delta> {
    let weekend = matches!(day % 7, 5 | 6);
    let mut deltas = Vec::new();
    let mut task = |start: u64, rng: &mut Rng| {
        let length = rng.between(10, 120) * 60;
        let mut questions: Vec<(u64, u64)> = (0..rng.between(0, 3))
            .map(|_| {
                let at = start + rng.between(60, length - 60);
                (at, at + rng.between(1, 45) * 60)
            })
            .collect();
        questions.sort();
        let mut cursor = start;
        deltas.push(Delta { at: start, inbox: 0, waiting: 0, working: 1 });
        for (asked, answered) in questions {
            if asked < cursor {
                continue;
            }
            deltas.push(Delta { at: asked, inbox: 1, waiting: 1, working: -1 });
            deltas.push(Delta { at: answered, inbox: -1, waiting: -1, working: 1 });
            cursor = answered;
        }
        let end = start + length.max(cursor - start + 60);
        let reviewed = end + rng.between(5, 180) * 60;
        let reviewed = if reviewed % DAY_S >= QUIET_FROM_S { reviewed - reviewed % DAY_S + DAY_S + QUIET_UNTIL_S } else { reviewed };
        deltas.push(Delta { at: end, inbox: 1, waiting: 0, working: -1 });
        deltas.push(Delta { at: reviewed, inbox: -1, waiting: 0, working: 0 });
    };
    let base = day * DAY_S;
    let daytime = if weekend { rng.between(2, 6) } else { rng.between(14, 26) };
    for _ in 0..daytime {
        let start = base + rng.between(8 * 3600 + 1800, 20 * 3600);
        task(start, rng);
    }
    if rng.between(0, 3) == 0 {
        let start = base + rng.between(3600, 4 * 3600);
        task(start, rng);
    }
    deltas
}

fn timeline() -> Vec<(u64, ActivityCounts)> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut deltas: Vec<Delta> = (0..DAYS).flat_map(|day| tasks_of(day, &mut rng)).collect();
    deltas.sort_by_key(|delta| delta.at);
    let (mut inbox, mut waiting, mut working) = (0i64, 0i64, 0i64);
    let mut timeline: Vec<(u64, ActivityCounts)> = Vec::new();
    for delta in deltas {
        inbox += delta.inbox;
        waiting += delta.waiting;
        working += delta.working;
        let counts = counts(inbox as u32, waiting as u32, working as u32);
        match timeline.last_mut() {
            Some((at, last)) if *at == delta.at => *last = counts,
            _ => timeline.push((delta.at, counts)),
        }
    }
    timeline
}

/// One report to the relay: when, and the three flags it carries.
struct Reported {
    at_ms: u64,
    active: bool,
    may_start: bool,
    urgent: bool,
    /// The same state the phone was last told: only the 15 minute repeat sends it.
    unchanged: bool,
}

fn may_start(at_s: u64) -> bool {
    let time = at_s % DAY_S;
    !(time >= QUIET_FROM_S || time < QUIET_UNTIL_S)
}

/// What the daemon does in `run`, with a clock of its own and a relay that answers every report.
fn replay(timeline: &[(u64, ActivityCounts)]) -> Vec<Reported> {
    let base = Instant::now();
    let mut schedule = Schedule::new(counts(0, 0, 0), base);
    schedule.phones(vec![("phone".into(), "binding".into(), "key".into())]);
    let end = Duration::from_secs(DAYS * DAY_S + GRACE.as_secs() + REPEAT.as_secs());
    let mut reported: Vec<Reported> = Vec::new();
    let mut last_told = None;
    let mut now = Duration::ZERO;
    let mut events = timeline.iter().peekable();
    while now <= end {
        while let Some((_, counts)) = events.next_if(|(at, _)| Duration::from_secs(*at) <= now) {
            schedule.observe(*counts, base + now);
        }
        for due in schedule.take(base + now) {
            let at_s = now.as_secs();
            let unchanged = last_told == Some((due.counts, due.active));
            last_told = Some((due.counts, due.active));
            reported.push(Reported { at_ms: now.as_millis() as u64, active: due.active, may_start: may_start(at_s), urgent: due.urgent, unchanged });
            schedule.record(&due, Sent::Accepted, base + now);
        }
        let next_event = events.peek().map(|(at, _)| Duration::from_secs(*at));
        let next_due = schedule.next(base + now).map(|at| at.duration_since(base));
        let Some(next) = [next_event, next_due].into_iter().flatten().min() else { break };
        now = next.max(now + Duration::from_millis(1));
    }
    reported
}

fn timeline_json(timeline: &[(u64, ActivityCounts)]) -> String {
    let mut text = String::from("{\n  \"note\": \"Two weeks of synthetic work: [seconds from midnight of day 1, inbox, waiting, working]\",\n  \"events\": [\n");
    for (index, (at, counts)) in timeline.iter().enumerate() {
        let comma = if index + 1 == timeline.len() { "" } else { "," };
        writeln!(text, "    [{at}, {}, {}, {}]{comma}", counts.inbox, counts.waiting, counts.working).unwrap();
    }
    text.push_str("  ]\n}\n");
    text
}

fn reports_json(reported: &[Reported]) -> String {
    let mut text = String::from("{\n  \"note\": \"What a Mac reports to the relay for that work: [milliseconds, active, may_start, urgent, unchanged]\",\n  \"reports\": [\n");
    for (index, report) in reported.iter().enumerate() {
        let comma = if index + 1 == reported.len() { "" } else { "," };
        writeln!(text, "    [{}, {}, {}, {}, {}]{comma}", report.at_ms, report.active, report.may_start, report.urgent, report.unchanged).unwrap();
    }
    text.push_str("  ]\n}\n");
    text
}

#[test]
fn the_fixtures_are_two_weeks_of_work_and_what_a_mac_reports_for_it() {
    let timeline = timeline();
    let timeline_text = timeline_json(&timeline);
    let reports_text = reports_json(&replay(&timeline));
    if std::env::var_os("ALONDRA_WRITE_PUSH_FIXTURE").is_some() {
        let root = std::env::var("BUILD_WORKSPACE_DIRECTORY").expect("run through bazelisk run");
        std::fs::write(Path::new(&root).join(TIMELINE_FIXTURE), &timeline_text).unwrap();
        std::fs::write(Path::new(&root).join(REPORTS_FIXTURE), &reports_text).unwrap();
        return;
    }
    let stale = "the replay changed: regenerate fixtures/push/activity_timeline.json and activity_reports.json";
    assert!(include_str!("../../../../fixtures/push/activity_timeline.json") == timeline_text, "{stale}");
    assert!(include_str!("../../../../fixtures/push/activity_reports.json") == reports_text, "{stale}");
}
