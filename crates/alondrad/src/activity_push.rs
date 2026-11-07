//! Keeps the phone's Live Activity up to date with this Mac's three counts: the cards in Waiting
//! for You, the nodes that wait for an answer and the nodes at work. Each phone gets them sealed
//! with its own key, so neither the relay nor Apple reads this copy: the relay keeps the latest
//! envelope of every Mac for the phone and hands them, unopened, to the Live Activity, whose
//! widget extension opens and adds them up. The first count also reaches the phone's icon badge
//! in the clear when it drops (`push_retract`).
//!
//! Outside the envelope travel three flags the relay acts on: `active` (something waits or works
//! here, or did less than [`GRACE`] ago), `may_start` (the phone is outside its quiet hours, so a
//! new Live Activity may appear) and `urgent` (the Inbox grew since the phone last heard).
//!
//! Nothing is ever queued: each phone keeps only what it was last told, and whatever goes out is
//! the state of that moment. A relay that fails is tried again with the backoff of the push
//! registration; one without the route (404) puts the reports to sleep for [`DORMANT`], so this
//! daemon can ship before the relay; and a phone the relay no longer has a registration for (410)
//! is asked again after [`DORMANT`], or at once with a new push binding: turning Live Activities
//! back on registers the same phone with the same binding.
use crate::domain_runtime::DomainRuntime;
use crate::service::Service;
use alondra_core::domain::ActivityCounts;
use alondra_core::views;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Changes come in bursts (an answer closes a task and its node): one report for all.
const SETTLE: Duration = Duration::from_secs(5);
/// How long the Mac stays active once everything is back to zero, so the Live Activity does not
/// leave and come back between one task ending and the next one starting.
const GRACE: Duration = Duration::from_secs(15 * 60);
/// While active, the last state goes out again this often, so the Live Activity never looks
/// stale while nothing changes.
const REPEAT: Duration = Duration::from_secs(15 * 60);
/// A relay without the route, or without the phone's registration, is asked again this often.
const DORMANT: Duration = Duration::from_secs(60 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const MAC_NAME_LIMIT: usize = 64;

/// The flags the relay reads in the clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Flags {
    pub active: bool,
    pub may_start: bool,
    pub urgent: bool,
}

/// What the relay answered to one report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sent {
    Accepted,
    /// It did not answer, failed (5xx) or asked to slow down (429): the state of then goes again.
    Retry,
    /// A relay older than the route.
    Missing,
    /// The relay no longer has a Live Activity registration for this phone.
    Gone,
}

pub(crate) fn sent(status: reqwest::StatusCode, code: &str) -> Sent {
    match status {
        status if status.is_success() => Sent::Accepted,
        reqwest::StatusCode::NOT_FOUND => Sent::Missing,
        reqwest::StatusCode::GONE if code == "activity_registration_gone" => Sent::Gone,
        _ => Sent::Retry,
    }
}

/// The state a phone is told: the counts and whether the Mac is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Report {
    counts: ActivityCounts,
    active: bool,
}

/// A report due for one phone.
#[derive(Debug, PartialEq, Eq)]
struct Due {
    phone: String,
    binding: String,
    counts: ActivityCounts,
    active: bool,
    urgent: bool,
}

#[derive(Debug)]
struct Phone {
    binding: String,
    key: String,
    told: Option<Report>,
    told_at: Option<Instant>,
    failures: u32,
    retry_at: Option<Instant>,
    gone_until: Option<Instant>,
}

impl Phone {
    fn new(binding: String, key: String) -> Self {
        Phone { binding, key, told: None, told_at: None, failures: 0, retry_at: None, gone_until: None }
    }
}

/// When each phone is told what: the clock is the caller's, so tests drive it.
struct Schedule {
    counts: ActivityCounts,
    /// While everything is at zero, when the Mac stops being active.
    active_until: Option<Instant>,
    /// The first change no phone was told of yet.
    changed: Option<Instant>,
    dormant_until: Option<Instant>,
    phones: BTreeMap<String, Phone>,
}

impl Schedule {
    /// The first report after a start tells every phone where things stand, even at zero: the
    /// relay may still hold a report from before the restart.
    fn new(counts: ActivityCounts, now: Instant) -> Self {
        Schedule { counts, active_until: None, changed: Some(now), dormant_until: None, phones: BTreeMap::new() }
    }

    fn observe(&mut self, counts: ActivityCounts, now: Instant) {
        if counts == self.counts {
            return;
        }
        if !counts.any() && self.counts.any() {
            self.active_until = Some(now + GRACE);
        } else if counts.any() {
            self.active_until = None;
        }
        self.counts = counts;
        self.changed.get_or_insert(now);
    }

    fn active(&self, now: Instant) -> bool {
        self.counts.any() || self.active_until.is_some_and(|until| now < until)
    }

    fn report(&self, now: Instant) -> Report {
        Report { counts: self.counts, active: self.active(now) }
    }

    /// The phones that may hear from this Mac now, by id with their push binding and Live
    /// Activity key. A new binding or key is a new start for its phone, one the relay had ended
    /// included: what it was told before it could not open.
    fn phones(&mut self, phones: Vec<(String, String, String)>) {
        let phones: BTreeMap<String, (String, String)> = phones.into_iter().map(|(id, binding, key)| (id, (binding, key))).collect();
        self.phones.retain(|id, _| phones.contains_key(id));
        for (id, (binding, key)) in phones {
            match self.phones.get(&id) {
                Some(phone) if phone.binding == binding && phone.key == key => {}
                _ => {
                    self.phones.insert(id, Phone::new(binding, key));
                }
            }
        }
    }

    fn due_at(&self, phone: &Phone, now: Instant) -> Option<Instant> {
        if phone.gone_until.is_some() {
            return phone.gone_until;
        }
        let report = self.report(now);
        let at = if let Some(retry) = phone.retry_at {
            retry
        } else if phone.told != Some(report) {
            self.changed.map_or(now, |changed| changed + SETTLE)
        } else if report.active {
            phone.told_at? + REPEAT
        } else {
            return None;
        };
        Some(self.dormant_until.map_or(at, |until| at.max(until)))
    }

    /// When to look again: the next report due, or the moment the Mac stops being active.
    fn next(&self, now: Instant) -> Option<Instant> {
        let expiry = self.active_until.filter(|until| *until > now);
        self.phones.values().filter_map(|phone| self.due_at(phone, now)).chain(expiry).min()
    }

    /// The reports due now. The burst they carry counts as told; a phone that fails gets the
    /// state of its retry.
    fn take(&mut self, now: Instant) -> Vec<Due> {
        if self.dormant_until.is_some_and(|until| now < until) {
            return Vec::new();
        }
        let report = self.report(now);
        let due: Vec<Due> = self.phones.iter()
            .filter(|(_, phone)| self.due_at(phone, now).is_some_and(|at| at <= now))
            .map(|(id, phone)| Due {
                phone: id.clone(),
                binding: phone.binding.clone(),
                counts: report.counts,
                active: report.active,
                urgent: report.counts.inbox > phone.told.map_or(0, |told| told.counts.inbox),
            })
            .collect();
        if self.changed.is_some_and(|changed| changed + SETTLE <= now) {
            self.changed = None;
        }
        due
    }

    /// Records what the relay answered; false when nothing more should go out now.
    fn record(&mut self, due: &Due, sent: Sent, now: Instant) -> bool {
        let Some(phone) = self.phones.get_mut(&due.phone).filter(|phone| phone.binding == due.binding) else {
            return true;
        };
        match sent {
            Sent::Accepted => {
                self.dormant_until = None;
                phone.told = Some(Report { counts: due.counts, active: due.active });
                phone.told_at = Some(now);
                phone.failures = 0;
                phone.retry_at = None;
                phone.gone_until = None;
            }
            Sent::Retry => {
                phone.retry_at = Some(now + backoff(phone.failures));
                phone.failures = phone.failures.saturating_add(1);
            }
            Sent::Missing => {
                self.dormant_until = Some(now + DORMANT);
                phone.retry_at = None;
                return false;
            }
            Sent::Gone => {
                phone.gone_until = Some(now + DORMANT);
                phone.retry_at = None;
            }
        }
        true
    }
}

/// The wait before retry `n` (from 0): `min(5·2^n, 300)` seconds, as the push registration.
fn backoff(failures: u32) -> Duration {
    Duration::from_secs(5u64.saturating_mul(1u64 << failures.min(16))).min(MAX_BACKOFF)
}

/// The envelope's plaintext: the counts, which Mac they are from and when they were measured.
/// `seq` grows with every report of this Mac, across restarts too, so the extension keeps the
/// newest one.
fn payload(counts: ActivityCounts, mac_name: &str, measured_at_ms: u64, seq: u64) -> Value {
    json!({
        "inbox": counts.inbox,
        "waiting": counts.waiting,
        "working": counts.working,
        "mac_name": mac_name,
        "measured_at_ms": measured_at_ms,
        "seq": seq,
    })
}

fn now_ms() -> u64 {
    u64::try_from(alondra_core::now_millis()).unwrap_or(0)
}

fn mac_name() -> String {
    let name = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Mac".into());
    name.chars().take(MAC_NAME_LIMIT).collect()
}

pub async fn run(domain: Arc<DomainRuntime>, service: Arc<Service>) {
    let mac_name = mac_name();
    let mut changed = domain.subscribe();
    let mut schedule = Schedule::new(views::activity_counts(domain.read().index()), Instant::now());
    let mut seq = now_ms();
    let mut failing = false;
    loop {
        let now = Instant::now();
        schedule.observe(views::activity_counts(domain.read().index()), now);
        match service.activity_phones() {
            Ok(phones) => schedule.phones(phones),
            Err(error) => log!("could not list the phones for the Live Activity: {error:#}"),
        }
        let mut epoch = None;
        for due in schedule.take(now) {
            let may_start = service.may_start_activity(&due.phone, crate::push::local_minutes()).unwrap_or(false);
            let flags = Flags { active: due.active, may_start, urgent: due.urgent };
            seq = seq.max(now_ms()).saturating_add(1);
            let body = payload(due.counts, &mac_name, now_ms(), seq);
            let outcome = match service.send_device_activity(&due.phone, &due.binding, &body, flags, &mut epoch).await {
                Ok((status, code)) => sent(status, &code),
                Err(error) => {
                    if !failing {
                        log!("could not send the Live Activity counts to {}: {error:#}", due.phone);
                    }
                    Sent::Retry
                }
            };
            match outcome {
                Sent::Accepted if failing => log!("the relay takes the Live Activity counts again"),
                Sent::Missing => log!("the relay has no Live Activity route yet; asking again in an hour"),
                Sent::Gone => log!("the relay has no Live Activity for {}; asking again in an hour", due.phone),
                _ => {}
            }
            failing = outcome == Sent::Retry;
            if !schedule.record(&due, outcome, Instant::now()) {
                break;
            }
        }
        let wait = schedule.next(Instant::now())
            .map_or(REPEAT, |at| at.saturating_duration_since(Instant::now()))
            .min(REPEAT);
        tokio::select! {
            result = changed.changed() => if result.is_err() { return },
            _ = service.activity_key_given() => {},
            _ = tokio::time::sleep(wait) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(inbox: u32, waiting: u32, working: u32) -> ActivityCounts {
        ActivityCounts { inbox, waiting, working }
    }

    fn phone() -> Vec<(String, String, String)> {
        vec![("phone".into(), "binding".into(), "key".into())]
    }

    /// Takes what is due at `at` and answers each with `answer`.
    fn tick(schedule: &mut Schedule, at: Instant, answer: Sent) -> Vec<Due> {
        let due = schedule.take(at);
        for one in &due {
            if !schedule.record(one, answer, at) {
                break;
            }
        }
        due
    }

    /// A schedule that already told the phone `start`.
    fn told(start: ActivityCounts, at: Instant) -> Schedule {
        let mut schedule = Schedule::new(start, at);
        schedule.phones(phone());
        assert_eq!(tick(&mut schedule, at + SETTLE, Sent::Accepted).len(), 1);
        schedule
    }

    #[test]
    fn a_burst_of_changes_within_five_seconds_is_one_report() {
        let start = Instant::now();
        let mut schedule = told(counts(0, 0, 1), start);
        let at = start + Duration::from_secs(60);
        schedule.observe(counts(1, 0, 1), at);
        schedule.observe(counts(1, 1, 1), at + Duration::from_secs(2));
        schedule.observe(counts(2, 1, 2), at + Duration::from_secs(4));
        assert!(tick(&mut schedule, at + Duration::from_millis(4_999), Sent::Accepted).is_empty(), "the burst settles first");
        assert_eq!(schedule.next(at + Duration::from_secs(4)), Some(at + SETTLE), "from its first change, not its last");
        let due = tick(&mut schedule, at + SETTLE, Sent::Accepted);
        assert_eq!(due, vec![Due { phone: "phone".into(), binding: "binding".into(), counts: counts(2, 1, 2), active: true, urgent: true }]);
        assert!(tick(&mut schedule, at + SETTLE + Duration::from_secs(1), Sent::Accepted).is_empty());
    }

    #[test]
    fn the_mac_stays_active_fifteen_minutes_after_everything_is_back_to_zero_and_not_one_moment_more() {
        let start = Instant::now();
        let mut schedule = told(counts(0, 0, 1), start);
        let zero = start + Duration::from_secs(60);
        schedule.observe(counts(0, 0, 0), zero);
        let due = tick(&mut schedule, zero + SETTLE, Sent::Accepted);
        assert_eq!(due.len(), 1);
        assert!(due[0].active, "still active in its grace");
        assert_eq!(schedule.next(zero + SETTLE), Some(zero + GRACE), "it wakes when the grace ends");
        assert!(tick(&mut schedule, zero + GRACE - Duration::from_millis(1), Sent::Accepted).is_empty());
        let due = tick(&mut schedule, zero + GRACE, Sent::Accepted);
        assert_eq!(due.len(), 1);
        assert!(!due[0].active, "inactive exactly at the end of the grace");
        assert_eq!(schedule.next(zero + GRACE), None, "nothing more while inactive");
    }

    #[test]
    fn a_task_that_starts_within_the_grace_keeps_the_mac_active() {
        let start = Instant::now();
        let mut schedule = told(counts(0, 0, 1), start);
        let zero = start + Duration::from_secs(60);
        schedule.observe(counts(0, 0, 0), zero);
        tick(&mut schedule, zero + SETTLE, Sent::Accepted);
        schedule.observe(counts(0, 0, 1), zero + Duration::from_secs(120));
        let due = tick(&mut schedule, zero + Duration::from_secs(125), Sent::Accepted);
        assert!(due[0].active);
        assert!(tick(&mut schedule, zero + GRACE, Sent::Accepted).is_empty(), "the old grace no longer ends it");
    }

    #[test]
    fn the_last_state_is_repeated_every_fifteen_minutes_while_active_and_never_while_inactive() {
        let start = Instant::now();
        let mut schedule = told(counts(1, 0, 0), start);
        let told_at = start + SETTLE;
        assert_eq!(schedule.next(told_at), Some(told_at + REPEAT));
        assert!(tick(&mut schedule, told_at + REPEAT - Duration::from_millis(1), Sent::Accepted).is_empty());
        let due = tick(&mut schedule, told_at + REPEAT, Sent::Accepted);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].counts, counts(1, 0, 0));
        assert!(!due[0].urgent, "a repeat is not news");
        assert_eq!(schedule.next(told_at + REPEAT), Some(told_at + 2 * REPEAT));

        let idle = told(counts(0, 0, 0), start);
        assert!(!idle.phones["phone"].told.unwrap().active);
        assert_eq!(idle.next(told_at), None, "an inactive Mac says nothing more");
    }

    #[test]
    fn urgent_only_when_the_inbox_grew_since_the_phone_last_heard() {
        let start = Instant::now();
        let mut schedule = told(counts(2, 0, 1), start);
        let mut at = start + Duration::from_secs(60);
        for (next, urgent) in [(counts(2, 1, 3), false), (counts(1, 1, 3), false), (counts(3, 1, 3), true), (counts(3, 0, 0), false)] {
            schedule.observe(next, at);
            let due = tick(&mut schedule, at + SETTLE, Sent::Accepted);
            assert_eq!(due[0].urgent, urgent, "{next:?}");
            at += Duration::from_secs(60);
        }
    }

    #[test]
    fn a_failing_relay_gets_only_the_latest_state_with_the_registration_backoff() {
        let start = Instant::now();
        let mut schedule = told(counts(1, 0, 0), start);
        let mut at = start + Duration::from_secs(60);
        schedule.observe(counts(2, 0, 0), at);
        at += SETTLE;
        let mut tries = Vec::new();
        for n in 0..8u32 {
            let due = tick(&mut schedule, at, Sent::Retry);
            assert_eq!(due.len(), 1, "try {n}");
            tries.push(due[0].counts.inbox);
            if n == 2 {
                schedule.observe(counts(3, 0, 0), at + Duration::from_secs(1));
                schedule.observe(counts(4, 1, 0), at + Duration::from_secs(2));
            }
            let wait = schedule.next(at).unwrap() - at;
            assert_eq!(wait, Duration::from_secs((5u64 << n).min(300)), "try {n}");
            assert!(schedule.take(at + wait - Duration::from_millis(1)).is_empty(), "nothing before the backoff, try {n}");
            at += wait;
        }
        assert_eq!(tries, vec![2, 2, 2, 4, 4, 4, 4, 4], "never a queue: the state of each try");
        let due = tick(&mut schedule, at, Sent::Accepted);
        assert_eq!(due, vec![Due { phone: "phone".into(), binding: "binding".into(), counts: counts(4, 1, 0), active: true, urgent: true }]);
        assert_eq!(schedule.next(at), Some(at + REPEAT), "back to the repeat once the relay answers");
    }

    #[test]
    fn a_relay_without_the_route_puts_the_reports_to_sleep_and_is_asked_again_in_an_hour() {
        let start = Instant::now();
        let mut schedule = Schedule::new(counts(1, 0, 0), start);
        schedule.phones(vec![("a".into(), "binding-a".into(), "key".into()), ("b".into(), "binding-b".into(), "key".into())]);
        let due = tick(&mut schedule, start + SETTLE, Sent::Missing);
        assert_eq!(due.len(), 2);
        assert!(schedule.phones.values().all(|phone| phone.told.is_none()), "the first 404 stops the round");
        let asleep = start + SETTLE;
        assert_eq!(schedule.next(asleep), Some(asleep + DORMANT));
        schedule.observe(counts(5, 0, 0), asleep + Duration::from_secs(60));
        assert!(schedule.take(asleep + DORMANT - Duration::from_millis(1)).is_empty(), "changes do not wake it");
        let due = tick(&mut schedule, asleep + DORMANT, Sent::Accepted);
        assert_eq!(due.iter().map(|due| due.counts.inbox).collect::<Vec<_>>(), vec![5, 5], "the state of then, to every phone");
        assert_eq!(schedule.next(asleep + DORMANT), Some(asleep + DORMANT + REPEAT));
    }

    #[test]
    fn a_gone_registration_stops_that_phone_for_an_hour_or_until_it_sends_a_new_binding() {
        let start = Instant::now();
        let mut schedule = Schedule::new(counts(1, 0, 0), start);
        schedule.phones(vec![("a".into(), "binding-a".into(), "key".into()), ("b".into(), "binding-b".into(), "key".into())]);
        let due = schedule.take(start + SETTLE);
        schedule.record(&due[0], Sent::Gone, start + SETTLE);
        schedule.record(&due[1], Sent::Accepted, start + SETTLE);
        let at = start + Duration::from_secs(60);
        schedule.observe(counts(2, 0, 0), at);
        let due = tick(&mut schedule, at + SETTLE, Sent::Accepted);
        assert_eq!(due.iter().map(|due| due.phone.as_str()).collect::<Vec<_>>(), vec!["b"]);
        assert!(tick(&mut schedule, at + SETTLE + REPEAT, Sent::Accepted).iter().all(|due| due.phone == "b"));

        schedule.phones(vec![("a".into(), "binding-a".into(), "key".into()), ("b".into(), "binding-b".into(), "key".into())]);
        assert!(schedule.phones["a"].gone_until.is_some(), "the same binding is the same ended registration");
        schedule.phones(vec![("a".into(), "binding-a2".into(), "key".into()), ("b".into(), "binding-b".into(), "key".into())]);
        let later = at + SETTLE + REPEAT + Duration::from_secs(1);
        let due = tick(&mut schedule, later, Sent::Accepted);
        assert_eq!(due.iter().map(|due| due.phone.as_str()).collect::<Vec<_>>(), vec!["a"], "a new binding starts again");
    }

    #[test]
    fn a_new_key_starts_its_phone_over_with_the_same_binding() {
        let start = Instant::now();
        let mut schedule = Schedule::new(counts(1, 0, 0), start);
        schedule.phones(vec![("a".into(), "binding-a".into(), "key".into())]);
        tick(&mut schedule, start + SETTLE, Sent::Accepted);
        let later = start + SETTLE + Duration::from_secs(1);
        assert!(schedule.take(later).is_empty(), "the phone was told");
        schedule.phones(vec![("a".into(), "binding-a".into(), "rotated".into())]);
        assert_eq!(schedule.take(later).len(), 1, "what it was told before it can no longer open");
    }

    #[test]
    fn a_phone_that_comes_back_with_the_same_binding_hears_again_within_the_hour() {
        let start = Instant::now();
        let mut schedule = Schedule::new(counts(1, 0, 0), start);
        schedule.phones(vec![("a".into(), "binding-a".into(), "key".into())]);
        let due = schedule.take(start + SETTLE);
        schedule.record(&due[0], Sent::Gone, start + SETTLE);
        assert!(schedule.take(start + SETTLE + DORMANT - Duration::from_secs(1)).is_empty());
        let back = start + SETTLE + DORMANT;
        let due = tick(&mut schedule, back, Sent::Accepted);
        assert_eq!(due.iter().map(|due| due.phone.as_str()).collect::<Vec<_>>(), vec!["a"]);
        assert_eq!(schedule.phones["a"].gone_until, None);
    }

    #[test]
    fn a_start_tells_every_phone_where_things_stand_even_at_zero() {
        let start = Instant::now();
        let mut schedule = Schedule::new(counts(0, 0, 0), start);
        schedule.phones(phone());
        let due = tick(&mut schedule, start + SETTLE, Sent::Accepted);
        assert_eq!(due.len(), 1);
        assert!(!due[0].active);
        assert_eq!(schedule.next(start + SETTLE), None);
    }

    #[test]
    fn the_relay_answers_map_to_what_happens_next() {
        use reqwest::StatusCode;
        assert_eq!(sent(StatusCode::ACCEPTED, ""), Sent::Accepted);
        assert_eq!(sent(StatusCode::NOT_FOUND, "unknown"), Sent::Missing);
        assert_eq!(sent(StatusCode::GONE, "activity_registration_gone"), Sent::Gone);
        assert_eq!(sent(StatusCode::GONE, "unknown"), Sent::Retry, "only the relay's own word ends a registration");
        for status in [StatusCode::TOO_MANY_REQUESTS, StatusCode::INTERNAL_SERVER_ERROR, StatusCode::BAD_GATEWAY, StatusCode::SERVICE_UNAVAILABLE] {
            assert_eq!(sent(status, "unknown"), Sent::Retry, "{status}");
        }
    }

    #[test]
    fn the_activity_envelope_opens_with_the_phones_key_and_fits_the_budget() {
        use p256::{SecretKey, ecdsa::SigningKey};
        let mac_signing = SigningKey::from_slice(&[0x61; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[0x62; 32]).unwrap();
        let binding = "b".repeat(43);
        let name: String = "M".repeat(200).chars().take(MAC_NAME_LIMIT).collect();
        let body = payload(counts(u32::MAX, u32::MAX, u32::MAX), &name, u64::MAX, u64::MAX);
        let flags = Flags { active: true, may_start: false, urgent: true };
        let sealed: Value = serde_json::from_slice(&crate::service::seal_activity(
            "mac-1", &mac_signing, "phone-1", &phone_agreement.public_key(), &binding, &body, flags,
        ).unwrap()).unwrap();
        assert_eq!(sealed["device_id"], "phone-1");
        assert_eq!((sealed["active"].clone(), sealed["may_start"].clone(), sealed["urgent"].clone()), (json!(true), json!(false), json!(true)));
        let envelope: alondra_e2e::Envelope = serde_json::from_value(sealed["envelope"].clone()).unwrap();
        assert_eq!(envelope.kind, "activity");
        let plaintext = envelope
            .open(mac_signing.verifying_key(), &phone_agreement, "mac-1", "phone-1", "mac-1", envelope.expires_at_ms)
            .unwrap();
        assert!(plaintext.len() <= 1_500, "{} bytes", plaintext.len());
        let opened: Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(opened, json!({"push_binding": binding, "payload": body}));
        let other = SecretKey::from_slice(&[0x63; 32]).unwrap();
        assert!(envelope.open(mac_signing.verifying_key(), &other, "mac-1", "phone-1", "mac-1", envelope.expires_at_ms).is_err(), "only that phone opens it");
    }

    const ACTIVITY_PUSH_FIXTURE: &str = "fixtures/push/activity.json";

    /// fixtures/push/activity.json is the `/v1/mac/activity` body the relay and the iPhone's
    /// Live Activity extension decode. Regenerate it after a format change with
    /// `ALONDRA_WRITE_PUSH_FIXTURE=1 bazelisk run //crates/alondrad:alondrad_test -- activity_push_fixture`.
    #[test]
    fn activity_push_fixture_is_what_the_daemon_sends() {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use p256::{SecretKey, ecdsa::SigningKey};
        let mac_signing = SigningKey::from_slice(&[0x51; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[0x52; 32]).unwrap();
        let (mac_id, device_id) = ("mac-fixture", "phone-fixture");
        let binding = URL_SAFE_NO_PAD.encode([7u8; 32]);
        let push = payload(counts(3, 1, 2), "Mac mini", 1_790_000_000_000, 1_790_000_000_001);
        let flags = Flags { active: true, may_start: true, urgent: true };
        let body: Value = serde_json::from_slice(&crate::service::seal_activity(mac_id, &mac_signing, device_id,
            &phone_agreement.public_key(), &binding, &push, flags).unwrap()).unwrap();
        let point = mac_signing.verifying_key().to_sec1_point(false);
        let generated = json!({
            "mac_id": mac_id,
            "device_id": device_id,
            "mac_signing_public_key": URL_SAFE_NO_PAD.encode(&point.as_bytes()[1..]),
            "device_agreement_private_key": URL_SAFE_NO_PAD.encode(phone_agreement.to_bytes()),
            "push_binding": binding,
            "payload": push,
            "body": body,
        });
        if std::env::var_os("ALONDRA_WRITE_PUSH_FIXTURE").is_some() {
            let root = std::env::var("BUILD_WORKSPACE_DIRECTORY").expect("run through bazelisk run");
            let mut text = serde_json::to_string_pretty(&generated).unwrap();
            text.push('\n');
            std::fs::write(std::path::Path::new(&root).join(ACTIVITY_PUSH_FIXTURE), text).unwrap();
            return;
        }
        let fixture: Value = serde_json::from_str(include_str!("../../../fixtures/push/activity.json")).unwrap();
        let stale = "activity push format changed: regenerate fixtures/push/activity.json";
        for key in ["mac_id", "device_id", "mac_signing_public_key", "device_agreement_private_key", "push_binding", "payload"] {
            assert_eq!(fixture[key], generated[key], "{stale} ({key})");
        }
        for key in ["device_id", "active", "may_start", "urgent"] {
            assert_eq!(fixture["body"][key], generated["body"][key], "{stale} (body {key})");
        }
        let open = |envelope: &Value| -> Value {
            let envelope: alondra_e2e::Envelope = serde_json::from_value(envelope.clone()).unwrap();
            assert_eq!(envelope.kind, "activity");
            let plaintext = envelope
                .open(mac_signing.verifying_key(), &phone_agreement, mac_id, device_id, mac_id, envelope.expires_at_ms)
                .unwrap();
            serde_json::from_slice(&plaintext).unwrap()
        };
        assert_eq!(open(&fixture["body"]["envelope"]), open(&body["envelope"]), "{stale} (plaintext)");
    }
}
