//! Takes cards off the phone once they stop waiting for the user, wherever they were answered,
//! withdrawn or closed. Only a background push can do it without showing something new: an alert
//! push always shows a banner unless Apple grants the notification filtering entitlement.
//!
//! iOS throttles background pushes (Apple suggests no more than two or three an hour), drops them
//! for an app the user force-quit, and keeps only the latest one held for the app. So each push
//! carries the whole queue, not a delta: whichever one arrives leaves the phone right. The app
//! also reconciles its notifications with the queue every time it opens.
//!
//! The icon badge cannot wait for that: a badge only push sets it even for a force-quit app and
//! is not throttled, so it follows every drop once the burst settles. The system applies it
//! without the notification service extension, so the count travels in the clear.
use crate::domain_runtime::DomainRuntime;
use crate::service::Service;
use alondra_core::domain::ReadIndex;
use alondra_core::views;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Cards often leave in bursts (an answer closes a task and its other cards): one push for all.
const SETTLE: Duration = Duration::from_secs(10);
/// Stays well inside Apple's background budget; the push after it still carries every change.
const MIN_INTERVAL: Duration = Duration::from_secs(120);
/// What the payload may take of the 1,500-byte encrypted plaintext, leaving room for the
/// binding and the envelope's own fields.
const PAYLOAD_BUDGET: usize = 1_200;

pub async fn run(domain: Arc<DomainRuntime>, service: Arc<Service>) {
    let mut changed = domain.subscribe();
    let mut pending = Pending::new(waiting(domain.read().index()));
    loop {
        let wait = pending.next().map_or(Duration::from_secs(3600), |at| at.saturating_duration_since(Instant::now()));
        tokio::select! {
            result = changed.changed() => if result.is_err() { return },
            _ = tokio::time::sleep(wait) => {},
        }
        let read = domain.read();
        let index = read.index();
        let now = Instant::now();
        pending.observe(waiting(index), now);
        if pending.badge_due().is_some_and(|at| at <= now) {
            pending.take_badge();
            let badge = views::inbox_badge(index);
            match service.broadcast_badge_push(badge).await {
                Ok((attempted, accepted)) if attempted != accepted => {
                    log!("{} of {attempted} phones did not get the icon badge {badge}", attempted - accepted);
                }
                Ok(_) => {}
                Err(error) => log!("could not send the icon badge {badge} to the phones: {error:#}"),
            }
        }
        if pending.due().is_none_or(|at| at > now) {
            continue;
        }
        let left = pending.take(now);
        let push = payload(index, &left);
        match service.broadcast_background_push(|_| push.clone()).await {
            Ok((attempted, accepted)) if attempted != accepted => {
                log!("{} of {attempted} phones were not told that {} cards left the queue", attempted - accepted, left.len());
            }
            Ok(_) => {}
            Err(error) => log!("could not tell the phones that cards left the queue: {error:#}"),
        }
    }
}

fn waiting(index: &ReadIndex) -> BTreeSet<String> {
    views::waiting_cards(index).into_iter().collect()
}

/// Which cards left the queue since the phones were last told, and when to tell them: the
/// badge once the burst settles, the retraction also no faster than Apple's background budget.
struct Pending {
    waiting: BTreeSet<String>,
    left: BTreeSet<String>,
    since: Option<Instant>,
    sent: Option<Instant>,
    badge_since: Option<Instant>,
}

impl Pending {
    fn new(waiting: BTreeSet<String>) -> Self {
        Pending { waiting, left: BTreeSet::new(), since: None, sent: None, badge_since: None }
    }

    fn observe(&mut self, waiting: BTreeSet<String>, now: Instant) {
        let gone: Vec<String> = self.waiting.difference(&waiting).cloned().collect();
        self.left.retain(|card| !waiting.contains(card));
        if !gone.is_empty() {
            self.left.extend(gone);
            self.since.get_or_insert(now);
            self.badge_since.get_or_insert(now);
        }
        self.waiting = waiting;
    }

    fn badge_due(&self) -> Option<Instant> {
        Some(self.badge_since? + SETTLE)
    }

    fn take_badge(&mut self) {
        self.badge_since = None;
    }

    fn next(&self) -> Option<Instant> {
        [self.badge_due(), self.due()].into_iter().flatten().min()
    }

    fn due(&self) -> Option<Instant> {
        let settled = self.since? + SETTLE;
        Some(self.sent.map_or(settled, |sent| settled.max(sent + MIN_INTERVAL)))
    }

    fn take(&mut self, now: Instant) -> BTreeSet<String> {
        self.since = None;
        self.sent = Some(now);
        std::mem::take(&mut self.left)
    }
}

/// The badge and the whole queue: the app removes every notification of this Mac whose card is
/// not in `open`. A queue too long to fit names only the cards that left (`retracted`), as many
/// as fit; the app's own reconcile on opening catches the rest.
fn payload(index: &ReadIndex, left: &BTreeSet<String>) -> Value {
    let open = views::waiting_cards(index);
    let base = |key: &str, cards: &[&String]| json!({
        "aps": {"badge": open.len()},
        "alondra": {"protocol": views::VIEWS_PROTOCOL, "kind": "retract", "data_epoch": index.data_epoch(), key: cards},
    });
    let fits = |value: &Value| serde_json::to_vec(value).map_or(false, |bytes| bytes.len() <= PAYLOAD_BUDGET);
    let whole = base("open", &open.iter().collect::<Vec<_>>());
    if fits(&whole) {
        return whole;
    }
    let mut named: Vec<&String> = left.iter().collect();
    while !fits(&base("retracted", &named)) {
        named.pop();
    }
    base("retracted", &named)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cards(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn cards_that_leave_are_told_once_settled_and_never_faster_than_the_interval() {
        let start = Instant::now();
        let mut pending = Pending::new(cards(&["a", "b", "c"]));
        pending.observe(cards(&["a", "b", "c", "d"]), start);
        assert_eq!(pending.due(), None, "a new card is the alert's job");

        pending.observe(cards(&["b", "c", "d"]), start);
        pending.observe(cards(&["c", "d"]), start + Duration::from_secs(4));
        assert_eq!(pending.due(), Some(start + SETTLE), "a burst waits from its first card, not its last");
        assert_eq!(pending.take(start + SETTLE), cards(&["a", "b"]));
        assert_eq!(pending.due(), None);

        pending.observe(cards(&["d"]), start + SETTLE + Duration::from_secs(1));
        assert_eq!(pending.due(), Some(start + SETTLE + MIN_INTERVAL), "Apple throttles background pushes");
        pending.observe(cards(&[]), start + SETTLE + Duration::from_secs(60));
        assert_eq!(pending.take(start + SETTLE + MIN_INTERVAL), cards(&["c", "d"]), "changes in the interval all go in the next push");
    }

    #[test]
    fn the_badge_follows_every_drop_once_settled_while_the_retraction_waits_for_its_interval() {
        let start = Instant::now();
        let mut pending = Pending::new(cards(&["a", "b", "c"]));
        pending.observe(cards(&["a", "b", "c", "d"]), start);
        assert_eq!(pending.badge_due(), None, "a new card's alert carries the badge");

        pending.observe(cards(&["b", "c", "d"]), start);
        pending.observe(cards(&["c", "d"]), start + Duration::from_secs(4));
        assert_eq!(pending.badge_due(), Some(start + SETTLE), "one badge for the burst");
        pending.take_badge();
        pending.take(start + SETTLE);

        let later = start + SETTLE + Duration::from_secs(1);
        pending.observe(cards(&["d"]), later);
        assert_eq!(pending.badge_due(), Some(later + SETTLE), "not held back by the background budget");
        assert_eq!(pending.due(), Some(start + SETTLE + MIN_INTERVAL));
        assert_eq!(pending.next(), Some(later + SETTLE));
        pending.take_badge();
        assert_eq!(pending.badge_due(), None);
        assert_eq!(pending.next(), Some(start + SETTLE + MIN_INTERVAL), "the retraction still goes later");
    }

    #[test]
    fn a_card_that_comes_back_before_the_push_is_not_retracted() {
        let start = Instant::now();
        let mut pending = Pending::new(cards(&["a", "b"]));
        pending.observe(cards(&["b"]), start);
        pending.observe(cards(&["a", "b"]), start + Duration::from_secs(2));
        assert!(pending.take(start + SETTLE).is_empty(), "an answer the daemon refused puts the card back");
    }

    fn index_waiting(ids: &[String]) -> ReadIndex {
        use alondra_core::domain::*;
        let mut state = DomainState::new("epoch-1");
        for (order, id) in ids.iter().enumerate() {
            state.interactions.insert(id.as_str().into(), Interaction { system_card: None,
                attachments: vec![], id: id.as_str().into(),
                scope: Scope::Orchestrator { request_id: None }, author: Author::Orchestrator { generation: 1 },
                recipient: Recipient::User, body: "Read this".into(), blocking: false, subject: None,
                payload: InteractionPayload::Information { read_by: BTreeSet::new() },
                revision: 1, order: order as u64 + 1, withdrawn: false,
                card: CardSpec::new(CardKind::Know, "Read this"), clarification: None, snoozed_until: None, resurfaced_at_revision: None, shortened: false,
            });
        }
        ReadIndex::build(&state)
    }

    #[test]
    fn the_push_carries_the_whole_queue_and_the_badge_or_what_left_when_the_queue_is_long() {
        let open: Vec<String> = vec!["t-1-info-1".into(), "t-2-info-1".into()];
        let index = index_waiting(&open);
        assert_eq!(views::waiting_cards(&index), open, "the fixture's cards wait for the user");
        let push = payload(&index, &cards(&["t-1-question-1"]));
        assert_eq!(push, json!({
            "aps": {"badge": 2},
            "alondra": {"protocol": views::VIEWS_PROTOCOL, "kind": "retract", "data_epoch": "epoch-1", "open": open},
        }));
        assert_eq!(push["aps"]["badge"], views::inbox_badge(&index), "the same count as the alerts' badge");

        let long: Vec<String> = (0..60).map(|n| format!("t-push-group-retract-information-{n}")).collect();
        let index = index_waiting(&long);
        let left: BTreeSet<String> = (0..80).map(|n| format!("t-push-group-retract-question-{n}")).collect();
        let push = payload(&index, &left);
        assert!(push["alondra"].get("open").is_none());
        let named = push["alondra"]["retracted"].as_array().unwrap();
        assert!(!named.is_empty() && named.len() < left.len(), "as many as fit");
        assert!(serde_json::to_vec(&push).unwrap().len() <= PAYLOAD_BUDGET);
        assert_eq!(push["aps"]["badge"], 60);
    }

    const RETRACT_PUSH_FIXTURE: &str = "fixtures/push/retract.json";

    /// fixtures/push/retract.json is what the iPhone app opens in DeliveredNotificationSyncTests,
    /// and the relay checks it wraps the envelope the same way. Regenerate it after a format change
    /// with `ALONDRA_WRITE_PUSH_FIXTURE=1 bazelisk run //crates/alondrad:alondrad_test -- retract_push_fixture`.
    #[test]
    fn retract_push_fixture_is_what_the_daemon_sends() {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use p256::{SecretKey, ecdsa::SigningKey, elliptic_curve::sec1::ToSec1Point};
        let mac_signing = SigningKey::from_slice(&[0x51; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[0x52; 32]).unwrap();
        let (mac_id, device_id) = ("mac-fixture", "phone-fixture");
        let binding = URL_SAFE_NO_PAD.encode([7u8; 32]);
        let push = payload(&index_waiting(&["t-1-info-1".into()]), &cards(&["t-1-question-1"]));
        let body: Value = serde_json::from_slice(&crate::service::seal_push(mac_id, &mac_signing, device_id,
            &phone_agreement.public_key(), &binding, &push, false, crate::service::PushDelivery::Background).unwrap()).unwrap();
        assert_eq!(body["push_type"], "background");
        let point = mac_signing.verifying_key().to_sec1_point(false);
        let generated = json!({
            "mac_id": mac_id,
            "device_id": device_id,
            "mac_signing_public_key": URL_SAFE_NO_PAD.encode(&point.as_bytes()[1..]),
            "device_agreement_private_key": URL_SAFE_NO_PAD.encode(phone_agreement.to_bytes()),
            "payload": push,
            "apns": {"aps": {"content-available": 1}, "alondra": {"envelope": body["envelope"], "push_binding": binding}},
        });
        if std::env::var_os("ALONDRA_WRITE_PUSH_FIXTURE").is_some() {
            let root = std::env::var("BUILD_WORKSPACE_DIRECTORY").expect("run through bazelisk run");
            let mut text = serde_json::to_string_pretty(&generated).unwrap();
            text.push('\n');
            std::fs::write(std::path::Path::new(&root).join(RETRACT_PUSH_FIXTURE), text).unwrap();
            return;
        }
        let fixture: Value = serde_json::from_str(include_str!("../../../fixtures/push/retract.json")).unwrap();
        let stale = "retract push format changed: regenerate fixtures/push/retract.json";
        for key in ["mac_id", "device_id", "mac_signing_public_key", "device_agreement_private_key", "payload"] {
            assert_eq!(fixture[key], generated[key], "{stale} ({key})");
        }
        assert_eq!(fixture["apns"]["aps"], generated["apns"]["aps"], "{stale} (APNs wrapper)");
        let open = |envelope: &Value| -> Value {
            let envelope: alondra_e2e::Envelope = serde_json::from_value(envelope.clone()).unwrap();
            let plaintext = envelope
                .open(mac_signing.verifying_key(), &phone_agreement, mac_id, device_id, mac_id, envelope.expires_at_ms)
                .unwrap();
            serde_json::from_slice(&plaintext).unwrap()
        };
        assert_eq!(open(&fixture["apns"]["alondra"]["envelope"]), open(&body["envelope"]), "{stale} (plaintext)");
    }
}
