//! The phone's Live Activity: three counts, added up across every Mac of the account, on the Lock
//! Screen and in the Dynamic Island while something waits or works.
//!
//! Each Mac seals its counts for each phone (envelope kind `activity`); the relay keeps the latest
//! envelope of every Mac for that phone and hands them all, unopened, to the Live Activity, whose
//! widget extension opens and adds them up. Beside the envelope the Mac says three things in the
//! clear: `active` (something waits or works there, or did less than 15 minutes ago), `may_start`
//! (the phone is outside its quiet hours) and `urgent` (its Inbox grew).
//!
//! A phone registers once per pairing with its own `phone_id`, so the relay knows which pairings
//! feed the same Live Activity. Everything lives in `live-activity.json`, beside the device
//! registry and apart from it: a relay from before this file starts on the same registry.

use crate::Server;
use crate::auth::write_atomic;
use crate::push::ApnsRejection;
use alondra_e2e::Envelope;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Apple stops push-to-start after about ten in a row; one every ten minutes stays well clear.
pub const START_SPACING_MS: u64 = 10 * 60_000;
/// iOS keeps a Live Activity in the Dynamic Island for 8 h; a new one replaces it before then.
pub const ROTATE_AFTER_MS: u64 = (7 * 60 + 45) * 60_000;
/// Priority 10 spends a budget Apple keeps per device and never publishes: only a growing Inbox
/// uses it, and at most this often.
pub const URGENT_SPACING_MS: u64 = 5 * 60_000;
/// Without news for this long the Live Activity says so. Macs repeat their state every 15 min.
pub const STALE_AFTER_MS: u64 = 30 * 60_000;
/// A start, an end or a priority 10 update waits this long, so reports that arrive together go
/// out as one push.
pub const GROUP_WINDOW_MS: u64 = 2_000;
/// A priority 5 update goes out at most this often per phone, however many Macs report: Apple
/// does not limit them, so this only folds a burst of changes into one wake of the phone.
pub const UPDATE_SPACING_MS: u64 = 60_000;
/// A real envelope takes about 800 bytes, so this many fit in Apple's 4 KB.
pub const MAX_MACS: usize = 4;
const APNS_MAX_BYTES: usize = 4096;
/// The `ActivityAttributes` type of the iOS app, which iOS decodes a push-to-start into.
pub const ATTRIBUTES_TYPE: &str = "AlondraActivityAttributes";
/// Localization keys of the push-to-start alert, translated on the phone. It never sounds.
pub const START_TITLE_KEY: &str = "Alondra";
pub const START_BODY_KEY: &str = "Work in progress";
const MIN_TOKEN_HEX: usize = 64;
const MAX_TOKEN_HEX: usize = 512;
const LIVE_ACTIVITY_FILE: &str = "live-activity.json";

/// What a phone sends, signed by one of its pairings, every time something about its Live
/// Activity changes: the tokens it holds now and the activity it shows now. A token or activity
/// left out is one the phone no longer has.
#[derive(Clone, Debug, Deserialize)]
pub struct Registration {
    pub phone_id: String,
    #[serde(default)]
    pub start_token: Option<String>,
    #[serde(default)]
    pub update_token: Option<String>,
    #[serde(default)]
    pub activity_id: Option<String>,
    #[serde(default)]
    pub started_at_ms: Option<u64>,
    pub environment: String,
    pub frequent_updates: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Unregistration {
    pub phone_id: String,
}

/// One Mac's report for one phone, as the daemon sends it to `/v1/mac/activity`.
#[derive(Clone, Debug, Deserialize)]
pub struct ReportRequest {
    pub device_id: String,
    pub envelope: Envelope,
    pub active: bool,
    pub may_start: bool,
    pub urgent: bool,
}

pub fn valid_phone_id(id: &str) -> bool {
    (16..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Apple does not publish how long ActivityKit tokens are; the relay takes any even run of
/// 64 to 512 hexadecimal digits.
pub fn valid_token(token: &str) -> bool {
    (MIN_TOKEN_HEX..=MAX_TOKEN_HEX).contains(&token.len())
        && token.len() % 2 == 0
        && token.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_activity_id(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Registration {
    pub fn validate(&self) -> Result<()> {
        if !valid_phone_id(&self.phone_id) {
            bail!("invalid phone id");
        }
        if !matches!(self.environment.as_str(), "sandbox" | "production") {
            bail!("invalid APNs environment");
        }
        if [&self.start_token, &self.update_token].into_iter().flatten().any(|token| !valid_token(token)) {
            bail!("invalid ActivityKit token");
        }
        if self.activity_id.as_deref().is_some_and(|id| !valid_activity_id(id)) {
            bail!("invalid activity id");
        }
        if self.update_token.is_none() && (self.activity_id.is_some() || self.started_at_ms.is_some()) {
            bail!("an activity without its update token");
        }
        Ok(())
    }
}

/// The latest report of one Mac, kept until a newer one, the pairing's revocation or the
/// phone's unregistration.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub device_id: String,
    pub envelope: Envelope,
    pub active: bool,
    pub may_start: bool,
    pub urgent: bool,
    pub received_at_ms: u64,
}

/// The Live Activity the phone shows. One the relay started has no update token until the app
/// wakes and registers it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Live {
    pub started_at_ms: u64,
    #[serde(default)]
    pub update_token: Option<String>,
    #[serde(default)]
    pub activity_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Phone {
    pub owner_sub: String,
    pub environment: String,
    pub frequent_updates: bool,
    #[serde(default)]
    pub start_token: Option<String>,
    #[serde(default)]
    pub live: Option<Live>,
    /// The pairings this phone registered through, by device id, with the Mac each reaches.
    pub pairings: BTreeMap<String, String>,
    /// The latest report of each Mac, by Mac id.
    #[serde(default)]
    pub reports: BTreeMap<String, Report>,
    #[serde(default)]
    pub last_start_ms: Option<u64>,
    #[serde(default)]
    pub last_urgent_ms: Option<u64>,
    #[serde(default)]
    pub last_sent_ms: Option<u64>,
    /// A Mac said `urgent` since the last push.
    #[serde(default)]
    pub urgent_pending: bool,
    /// Something changed that no push carried yet.
    #[serde(default)]
    pub unsent: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    phones: BTreeMap<String, Phone>,
    /// Pairings whose phone unregistered: their Mac hears `activity_registration_gone` and stops.
    #[serde(default)]
    ended: BTreeSet<String>,
    /// Reports for pairings no phone registered yet, so a phone that registers has them at once.
    #[serde(default)]
    held: BTreeMap<String, (String, Report)>,
}

/// One push the relay sends to APNs for a phone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Send {
    Start,
    Update { priority: u8 },
    End,
}

/// The pushes due for a phone, with everything needed to send them.
#[derive(Debug)]
pub struct Outgoing {
    pub environment: String,
    pub pushes: Vec<(Send, String, Value)>,
}

/// What APNs answered to one push.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// Apple will never take this token again.
    DeadToken,
    /// Throttled or failing: the next report or the Mac's 15 minute repeat sends it again, and
    /// an end that no report will follow is tried again by [`retry_owed`].
    Failed,
}

impl Outcome {
    pub fn of(result: &Result<()>) -> Self {
        match result {
            Ok(()) => Outcome::Delivered,
            Err(failure) if failure.downcast_ref::<ApnsRejection>().is_some_and(ApnsRejection::registration_is_dead) => {
                Outcome::DeadToken
            }
            Err(_) => Outcome::Failed,
        }
    }
}

/// What became of a Mac's report.
#[derive(Debug, PartialEq, Eq)]
pub enum Recorded {
    /// The phone registered: its Live Activity may change.
    Stored { phone_id: String },
    /// No phone registered this pairing yet: the report waits for it.
    Held,
    /// The phone unregistered this pairing.
    Gone,
}

impl Phone {
    fn active(&self) -> bool {
        self.reports.values().any(|report| report.active)
    }

    /// Quiet hours are the phone's, and every Mac knows them: the newest report decides.
    fn may_start(&self) -> bool {
        self.reports.values().max_by_key(|report| report.received_at_ms).is_some_and(|report| report.may_start)
    }

    fn can_start(&self, now_ms: u64) -> bool {
        self.active()
            && self.may_start()
            && self.start_token.is_some()
            && self.last_start_ms.is_none_or(|at| now_ms.saturating_sub(at) >= START_SPACING_MS)
    }

    /// Quiet hours are no time to spend the budget: the update waits for iOS like any other.
    fn update_priority(&self, now_ms: u64) -> u8 {
        let urgent_allowed = self.last_urgent_ms.is_none_or(|at| now_ms.saturating_sub(at) >= URGENT_SPACING_MS);
        if self.urgent_pending && self.frequent_updates && self.may_start() && urgent_allowed { 10 } else { 5 }
    }

    /// How long after the last push the next one may go out.
    fn spacing(&self, now_ms: u64) -> u64 {
        match self.plan(now_ms).as_slice() {
            [Send::Update { priority: 5 }] => UPDATE_SPACING_MS,
            _ => GROUP_WINDOW_MS,
        }
    }

    /// The pushes the phone's state calls for now.
    pub fn plan(&self, now_ms: u64) -> Vec<Send> {
        let active = self.active();
        match &self.live {
            None if self.can_start(now_ms) => vec![Send::Start],
            None => vec![],
            Some(live) if !active => live.update_token.iter().map(|_| Send::End).collect(),
            Some(live) if now_ms.saturating_sub(live.started_at_ms) >= ROTATE_AFTER_MS && self.can_start(now_ms) => {
                live.update_token.iter().map(|_| Send::End).chain([Send::Start]).collect()
            }
            Some(live) => live.update_token.iter().map(|_| Send::Update { priority: self.update_priority(now_ms) }).collect(),
        }
    }

    /// The state after the pushes of [`Phone::plan`] go out, before APNs answers. An activity
    /// stays known until APNs takes its end. One with no update token cannot be ended, as when
    /// iOS started it for an app closed by force: it stays too, so nothing starts on top of it.
    fn apply(&mut self, pushes: &[Send], now_ms: u64) {
        for push in pushes {
            match push {
                Send::Start => {
                    self.live = Some(Live { started_at_ms: now_ms, update_token: None, activity_id: None });
                    self.last_start_ms = Some(now_ms);
                }
                Send::Update { priority: 10 } => self.last_urgent_ms = Some(now_ms),
                Send::End | Send::Update { .. } => {}
            }
        }
        if !pushes.is_empty() {
            self.urgent_pending = false;
            self.last_sent_ms = Some(now_ms);
        }
    }

    fn can_only_end(&self) -> bool {
        !self.active() && self.live.as_ref().is_some_and(|live| live.update_token.is_some())
    }

    /// The newest Macs first, as many as fit in one push, and how many were left out.
    pub fn content_state(&self, fits: impl Fn(&Value) -> bool) -> Value {
        let mut reports: Vec<&Report> = self.reports.values().collect();
        reports.sort_by(|a, b| b.received_at_ms.cmp(&a.received_at_ms));
        reports.truncate(MAX_MACS);
        loop {
            let state = json!({
                "macs": reports.iter().map(|report| &report.envelope).collect::<Vec<_>>(),
                "omitted": self.reports.len() - reports.len(),
            });
            if reports.len() <= 1 || fits(&state) {
                return state;
            }
            reports.pop();
        }
    }
}

/// The exact `aps` payload of each push.
pub fn payload(push: &Send, content_state: &Value, now_ms: u64) -> Value {
    let now_s = now_ms / 1000;
    let stale_s = (now_ms + STALE_AFTER_MS) / 1000;
    match push {
        Send::Start => json!({"aps": {
            "timestamp": now_s,
            "event": "start",
            "content-state": content_state,
            "stale-date": stale_s,
            "attributes-type": ATTRIBUTES_TYPE,
            "attributes": {"started_at_ms": now_ms},
            "input-push-token": 1,
            "alert": {"title": {"loc-key": START_TITLE_KEY}, "body": {"loc-key": START_BODY_KEY}},
        }}),
        Send::Update { .. } => json!({"aps": {
            "timestamp": now_s,
            "event": "update",
            "content-state": content_state,
            "stale-date": stale_s,
        }}),
        Send::End => json!({"aps": {
            "timestamp": now_s,
            "event": "end",
            "content-state": content_state,
            "dismissal-date": now_s,
        }}),
    }
}

pub fn priority(push: &Send) -> u8 {
    match push {
        Send::Update { priority } => *priority,
        Send::Start | Send::End => 5,
    }
}

fn fits(push: &Send, content_state: &Value, now_ms: u64) -> bool {
    serde_json::to_vec(&payload(push, content_state, now_ms)).is_ok_and(|bytes| bytes.len() <= APNS_MAX_BYTES)
}

pub struct Store {
    path: PathBuf,
    state: Mutex<State>,
    /// Phones with a grouped push waiting to go out.
    scheduled: Mutex<HashSet<String>>,
    #[cfg(test)]
    clock: Mutex<Option<u64>>,
}

impl Store {
    pub fn open(device_registry_path: &Path) -> Result<Self> {
        let path = device_registry_path.with_file_name(LIVE_ACTIVITY_FILE);
        let state = if path.exists() { serde_json::from_slice(&std::fs::read(&path)?)? } else { State::default() };
        Ok(Self {
            path,
            state: Mutex::new(state),
            scheduled: Mutex::default(),
            #[cfg(test)]
            clock: Mutex::new(None),
        })
    }

    pub fn now_ms(&self) -> u64 {
        #[cfg(test)]
        if let Some(now) = *self.clock.lock().unwrap() {
            return now;
        }
        crate::now_ms()
    }

    #[cfg(test)]
    pub fn set_clock(&self, now_ms: u64) {
        *self.clock.lock().unwrap() = Some(now_ms);
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub fn phone(&self, phone_id: &str) -> Option<Phone> {
        self.state.lock().unwrap().phones.get(phone_id).cloned()
    }

    /// Changes the state and writes it; on a failed write the change is undone.
    fn change<T>(&self, change: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let mut state = self.state.lock().unwrap();
        let before = serde_json::to_vec(&*state)?;
        let result = change(&mut state)?;
        let after = serde_json::to_vec(&*state)?;
        if after != before
            && let Err(failure) = write_atomic(&self.path, &after)
        {
            *state = serde_json::from_slice(&before)?;
            return Err(failure);
        }
        Ok(result)
    }

    /// Returns the phone, which owes its Live Activity a push when [`Store::owes`] says so.
    pub fn register(&self, owner_sub: &str, device_id: &str, mac_id: &str, registration: Registration) -> Result<String> {
        let now = self.now_ms();
        self.change(|state| {
            if state.phones.get(&registration.phone_id).is_some_and(|phone| phone.owner_sub != owner_sub) {
                bail!(Conflict);
            }
            for (id, phone) in state.phones.iter_mut() {
                if *id != registration.phone_id && phone.pairings.remove(device_id).is_some() {
                    phone.reports.retain(|_, report| report.device_id != device_id);
                }
            }
            state.ended.remove(device_id);
            let phone = state.phones.entry(registration.phone_id.clone()).or_insert_with(|| Phone {
                owner_sub: owner_sub.to_owned(),
                environment: registration.environment.clone(),
                frequent_updates: registration.frequent_updates,
                start_token: None,
                live: None,
                pairings: BTreeMap::new(),
                reports: BTreeMap::new(),
                last_start_ms: None,
                last_urgent_ms: None,
                last_sent_ms: None,
                urgent_pending: false,
                unsent: false,
            });
            phone.environment = registration.environment;
            phone.frequent_updates = registration.frequent_updates;
            phone.start_token = registration.start_token;
            phone.live = registration.update_token.map(|update_token| Live {
                started_at_ms: registration.started_at_ms.unwrap_or(now).min(now),
                update_token: Some(update_token),
                activity_id: registration.activity_id,
            });
            phone.pairings.insert(device_id.to_owned(), mac_id.to_owned());
            let held = state.held.remove(device_id);
            phone.unsent |= held.is_some() || phone.can_only_end();
            if let Some((held_mac, report)) = held {
                phone.reports.insert(held_mac, report);
            }
            Ok(registration.phone_id)
        })
    }

    /// The phone stops its Live Activity: it unpaired the last Mac, signed out or turned Live
    /// Activities off. Only one of its own pairings may say so.
    pub fn unregister(&self, owner_sub: &str, device_id: &str, phone_id: &str) -> Result<()> {
        self.change(|state| {
            let Some(phone) = state.phones.get(phone_id) else { return Ok(()) };
            if phone.owner_sub != owner_sub || !phone.pairings.contains_key(device_id) {
                bail!(Conflict);
            }
            let phone = state.phones.remove(phone_id).expect("checked above");
            state.ended.extend(phone.pairings.into_keys());
            Ok(())
        })
    }

    pub fn record(&self, mac_id: &str, request: ReportRequest) -> Result<Recorded> {
        let now = self.now_ms();
        let device_id = request.device_id.clone();
        let report = Report {
            device_id: request.device_id,
            envelope: request.envelope,
            active: request.active,
            may_start: request.may_start,
            urgent: request.urgent,
            received_at_ms: now,
        };
        self.change(|state| {
            if state.ended.contains(&device_id) {
                return Ok(Recorded::Gone);
            }
            let Some((phone_id, phone)) = state.phones.iter_mut().find(|(_, phone)| phone.pairings.get(&device_id).is_some_and(|mac| mac == mac_id)) else {
                state.held.insert(device_id, (mac_id.to_owned(), report));
                return Ok(Recorded::Held);
            };
            phone.urgent_pending |= report.urgent;
            phone.unsent = true;
            phone.reports.insert(mac_id.to_owned(), report);
            Ok(Recorded::Stored { phone_id: phone_id.clone() })
        })
    }

    /// When the phone's next push may go out: `None` for now, or the end of the wait after the
    /// last one.
    pub fn due_at(&self, phone_id: &str) -> Option<u64> {
        let now = self.now_ms();
        let state = self.state.lock().unwrap();
        let phone = state.phones.get(phone_id)?;
        let due = phone.last_sent_ms?.saturating_add(phone.spacing(now));
        (now < due).then_some(due)
    }

    pub fn owes(&self, phone_id: &str) -> bool {
        self.state.lock().unwrap().phones.get(phone_id).is_some_and(|phone| phone.unsent)
    }

    /// The phones with a change no push carried, or an activity whose end APNs has not taken.
    fn owing(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        state.phones.iter().filter(|(_, phone)| phone.unsent || phone.can_only_end()).map(|(id, _)| id.clone()).collect()
    }

    /// Forgets the reports and pairings that no longer reach their Mac. Returns the phones that
    /// changed, which owe their Live Activity a push.
    pub fn prune(&self, pairing_is_live: impl Fn(&str, &str) -> bool) -> Result<Vec<String>> {
        self.change(|state| {
            let mut changed = Vec::new();
            for (phone_id, phone) in state.phones.iter_mut() {
                let before = (phone.pairings.len(), phone.reports.len());
                phone.pairings.retain(|device_id, mac_id| pairing_is_live(device_id, mac_id));
                phone.reports.retain(|mac_id, report| pairing_is_live(&report.device_id, mac_id));
                if (phone.pairings.len(), phone.reports.len()) != before {
                    phone.unsent = true;
                    changed.push(phone_id.clone());
                }
            }
            state.held.retain(|device_id, (mac_id, _)| pairing_is_live(device_id, mac_id));
            Ok(changed)
        })
    }

    /// The pushes due for a phone now, recorded as sent. Reports of two Macs that arrive together
    /// both ask: the wait after a push is checked here, where only one of them can pass.
    pub fn take(&self, phone_id: &str) -> Result<Option<Outgoing>> {
        let now = self.now_ms();
        self.change(|state| {
            let Some(phone) = state.phones.get_mut(phone_id) else { return Ok(None) };
            if phone.last_sent_ms.is_some_and(|last| now < last.saturating_add(phone.spacing(now))) {
                return Ok(None);
            }
            let plan = phone.plan(now);
            let mut pushes = Vec::new();
            for push in &plan {
                let token = match push {
                    Send::Start => phone.start_token.clone(),
                    Send::Update { .. } | Send::End => phone.live.as_ref().and_then(|live| live.update_token.clone()),
                };
                let Some(token) = token else { continue };
                let content_state = phone.content_state(|state| fits(push, state, now));
                pushes.push((push.clone(), token, payload(push, &content_state, now)));
            }
            phone.apply(&plan, now);
            phone.unsent = false;
            let outgoing = (!pushes.is_empty())
                .then(|| Outgoing { environment: phone.environment.clone(), pushes });
            if phone.pairings.is_empty() && phone.live.is_none() {
                state.phones.remove(phone_id);
            }
            Ok(outgoing)
        })
    }

    /// Takes in what APNs answered. A dead update token while a Mac is still active leaves the
    /// phone owing a push, so a new Live Activity may start.
    pub fn settle(&self, phone_id: &str, answers: &[(Send, String, Outcome)]) -> Result<()> {
        self.change(|state| {
            let Some(phone) = state.phones.get_mut(phone_id) else { return Ok(()) };
            for (push, token, outcome) in answers {
                match (push, outcome) {
                    (Send::Start, Outcome::DeadToken) => {
                        if phone.start_token.as_ref() == Some(token) {
                            phone.start_token = None;
                        }
                        phone.live = phone.live.take().filter(|live| live.update_token.is_some());
                    }
                    (Send::Start, Outcome::Failed) => {
                        phone.live = phone.live.take().filter(|live| live.update_token.is_some());
                    }
                    (Send::Update { .. } | Send::End, Outcome::DeadToken) => {
                        if phone.live.as_ref().and_then(|live| live.update_token.as_ref()) == Some(token) {
                            phone.live = None;
                            phone.unsent |= phone.active();
                        }
                    }
                    (Send::End, Outcome::Delivered) => {
                        if phone.live.as_ref().and_then(|live| live.update_token.as_ref()) == Some(token) {
                            phone.live = None;
                        }
                    }
                    (Send::Update { priority: 10 }, Outcome::Failed) => {
                        phone.urgent_pending = true;
                        phone.last_urgent_ms = None;
                    }
                    _ => {}
                }
            }
            if phone.pairings.is_empty() && phone.live.is_none() {
                state.phones.remove(phone_id);
            }
            Ok(())
        })
    }

    /// Marks a grouped push for the phone; false when one is already waiting.
    fn schedule(&self, phone_id: &str) -> bool {
        self.scheduled.lock().unwrap().insert(phone_id.to_owned())
    }

    /// False when a push that could not wait already took the place of the grouped one.
    fn unschedule(&self, phone_id: &str) -> bool {
        self.scheduled.lock().unwrap().remove(phone_id)
    }
}

#[derive(Debug)]
pub struct Conflict;
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the phone belongs to another account or pairing")
    }
}
impl std::error::Error for Conflict {}

/// Sends the phone what its Live Activity needs now, or once the wait after the last push ends.
/// The wait is looked at again every grouping window, because a push that cannot wait may arrive
/// behind a long one.
pub async fn push(server: &Arc<Server>, phone_id: &str) {
    if server.activity.due_at(phone_id).is_none() {
        server.activity.unschedule(phone_id);
        flush(server, phone_id).await;
    }
    if !server.activity.owes(phone_id) || !server.activity.schedule(phone_id) {
        return;
    }
    let server = server.clone();
    let phone_id = phone_id.to_owned();
    tokio::spawn(async move {
        while let Some(due) = server.activity.due_at(&phone_id) {
            let left = due.saturating_sub(server.activity.now_ms());
            tokio::time::sleep(Duration::from_millis(left.min(GROUP_WINDOW_MS))).await;
        }
        if server.activity.unschedule(&phone_id) {
            let _gate = server.device_gate.read().await;
            flush(&server, &phone_id).await;
        }
    });
}

/// Picks up what no report will: a push lost to a restart, or an end APNs did not take.
pub async fn retry_owed(server: Arc<Server>) {
    loop {
        for phone_id in server.activity.owing() {
            let _gate = server.device_gate.read().await;
            push(&server, &phone_id).await;
        }
        tokio::time::sleep(Duration::from_millis(UPDATE_SPACING_MS)).await;
    }
}

/// APNs failures are never retried here: the next report, the Mac's 15 minute repeat or
/// [`retry_owed`] sends the state of then.
async fn flush(server: &Server, phone_id: &str) {
    let Some(apns) = &server.apns else { return };
    let outgoing = match server.activity.take(phone_id) {
        Ok(Some(outgoing)) => outgoing,
        Ok(None) => return,
        Err(failure) => {
            eprintln!("alondra-relay: could not record a Live Activity push: {failure:#}");
            return;
        }
    };
    let mut answers = Vec::new();
    for (push, token, payload) in outgoing.pushes {
        let expires_s = (server.activity.now_ms() + STALE_AFTER_MS) / 1000;
        let result = apns.send_live_activity(&token, &outgoing.environment, &payload, priority(&push), expires_s).await;
        if let Err(failure) = &result {
            eprintln!("alondra-relay: Live Activity {push:?} push failed: {failure:#}");
        }
        answers.push((push, token, Outcome::of(&result)));
    }
    if let Err(failure) = server.activity.settle(phone_id, &answers) {
        eprintln!("alondra-relay: could not record what APNs answered: {failure:#}");
    }
}

/// After a revocation: the phones that lost a Mac get a push without it, or an end.
pub async fn after_revocation(server: &Server) {
    let changed = match server.activity.prune(|device_id, mac_id| server.auth.pairing_is_live(device_id, mac_id)) {
        Ok(changed) => changed,
        Err(failure) => {
            eprintln!("alondra-relay: could not forget revoked Live Activity reports: {failure:#}");
            return;
        }
    };
    for phone_id in changed {
        flush(server, &phone_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(mac: &str) -> Envelope {
        Envelope {
            v: 1,
            route: mac.into(),
            sender: mac.into(),
            recipient: "client-phone".into(),
            request_id: "r".repeat(22),
            kind: "activity".into(),
            expires_at_ms: 1_790_000_600_000,
            ephemeral_key: "k".repeat(87),
            nonce: "n".repeat(16),
            ciphertext: "c".repeat(300),
            signature: "s".repeat(86),
        }
    }

    fn report(mac: &str, active: bool, may_start: bool, at: u64) -> Report {
        Report { device_id: format!("client-{mac}"), envelope: envelope(mac), active, may_start, urgent: false, received_at_ms: at }
    }

    fn phone(reports: &[Report]) -> Phone {
        Phone {
            owner_sub: "owner".into(),
            environment: "sandbox".into(),
            frequent_updates: true,
            start_token: Some("ab".repeat(80)),
            live: None,
            pairings: reports.iter().map(|report| (report.device_id.clone(), report.envelope.sender.clone())).collect(),
            reports: reports.iter().map(|report| (report.envelope.sender.clone(), report.clone())).collect(),
            last_start_ms: None,
            last_urgent_ms: None,
            last_sent_ms: None,
            urgent_pending: false,
            unsent: false,
        }
    }

    const T: u64 = 1_790_000_000_000;

    #[test]
    fn starts_with_the_first_active_mac_only_outside_quiet_hours_and_with_a_start_token() {
        assert_eq!(phone(&[report("mac-a", true, true, T)]).plan(T), vec![Send::Start]);
        assert_eq!(phone(&[report("mac-a", false, true, T)]).plan(T), vec![], "nothing waits or works");
        assert_eq!(phone(&[report("mac-a", true, false, T)]).plan(T), vec![], "quiet hours");
        let mut without_token = phone(&[report("mac-a", true, true, T)]);
        without_token.start_token = None;
        assert_eq!(without_token.plan(T), vec![]);
        assert_eq!(
            phone(&[report("mac-a", true, false, T), report("mac-b", false, true, T + 1)]).plan(T + 1),
            vec![Send::Start],
            "the newest report says whether the phone is in its quiet hours; any Mac makes it active"
        );
    }

    #[test]
    fn starts_at_most_once_every_ten_minutes() {
        let mut p = phone(&[report("mac-a", true, true, T)]);
        p.apply(&p.plan(T), T);
        assert!(p.live.is_some());
        p.live = None;
        assert_eq!(p.plan(T + START_SPACING_MS - 1), vec![], "a start cut by the limit waits for the next report");
        assert_eq!(p.plan(T + START_SPACING_MS), vec![Send::Start]);
    }

    #[test]
    fn rotates_at_seven_hours_forty_five_but_never_in_quiet_hours() {
        let mut p = phone(&[report("mac-a", true, true, T)]);
        p.live = Some(Live { started_at_ms: T, update_token: Some("cd".repeat(40)), activity_id: None });
        assert_eq!(p.plan(T + ROTATE_AFTER_MS - 1), vec![Send::Update { priority: 5 }]);
        assert_eq!(p.plan(T + ROTATE_AFTER_MS), vec![Send::End, Send::Start]);
        let at = T + ROTATE_AFTER_MS;
        p.apply(&[Send::End, Send::Start], at);
        assert_eq!(p.live, Some(Live { started_at_ms: at, update_token: None, activity_id: None }));
        let mut quiet = phone(&[report("mac-a", true, false, T)]);
        quiet.live = Some(Live { started_at_ms: T, update_token: Some("cd".repeat(40)), activity_id: None });
        assert_eq!(quiet.plan(T + ROTATE_AFTER_MS), vec![Send::Update { priority: 5 }]);
    }

    #[test]
    fn ends_when_no_mac_is_active_and_stays_known_until_apns_takes_the_end() {
        let mut p = phone(&[report("mac-a", false, true, T), report("mac-b", false, true, T)]);
        p.live = Some(Live { started_at_ms: T, update_token: Some("cd".repeat(40)), activity_id: None });
        assert_eq!(p.plan(T), vec![Send::End]);
        p.apply(&[Send::End], T);
        assert!(p.live.is_some(), "until APNs takes the end");
    }

    #[test]
    fn an_activity_with_no_token_is_never_started_over_until_it_rotates() {
        let mut p = phone(&[report("mac-a", false, true, T)]);
        p.live = Some(Live { started_at_ms: T, update_token: None, activity_id: None });
        assert_eq!(p.plan(T), vec![]);
        p.apply(&[], T);
        assert!(p.live.is_some());
        p.reports.get_mut("mac-a").unwrap().active = true;
        assert_eq!(p.plan(T + START_SPACING_MS), vec![], "a second one could not be ended either");
        assert_eq!(p.plan(T + ROTATE_AFTER_MS), vec![Send::Start]);
    }

    #[test]
    fn priority_ten_only_for_urgent_reports_with_frequent_updates_every_five_minutes() {
        let mut p = phone(&[report("mac-a", true, true, T)]);
        p.live = Some(Live { started_at_ms: T, update_token: Some("cd".repeat(40)), activity_id: None });
        assert_eq!(p.plan(T), vec![Send::Update { priority: 5 }]);
        p.urgent_pending = true;
        assert_eq!(p.plan(T), vec![Send::Update { priority: 10 }]);
        p.apply(&[Send::Update { priority: 10 }], T);
        assert!(!p.urgent_pending);
        p.urgent_pending = true;
        assert_eq!(p.plan(T + URGENT_SPACING_MS - 1), vec![Send::Update { priority: 5 }]);
        assert_eq!(p.plan(T + URGENT_SPACING_MS), vec![Send::Update { priority: 10 }]);
        p.frequent_updates = false;
        assert_eq!(p.plan(T + URGENT_SPACING_MS), vec![Send::Update { priority: 5 }]);
        for push in [Send::Start, Send::End] {
            assert_eq!(priority(&push), 5);
        }
    }

    #[test]
    fn quiet_hours_never_spend_priority_ten() {
        let mut p = phone(&[report("mac-a", true, false, T)]);
        p.live = Some(Live { started_at_ms: T, update_token: Some("cd".repeat(40)), activity_id: None });
        p.urgent_pending = true;
        assert_eq!(p.plan(T), vec![Send::Update { priority: 5 }]);
    }

    #[test]
    fn only_a_priority_five_update_waits_the_long_spacing() {
        let mut p = phone(&[report("mac-a", true, true, T)]);
        p.live = Some(Live { started_at_ms: T, update_token: Some("cd".repeat(40)), activity_id: None });
        assert_eq!(p.spacing(T), UPDATE_SPACING_MS);
        p.urgent_pending = true;
        assert_eq!(p.spacing(T), GROUP_WINDOW_MS, "a growing Inbox goes out at once");
        p.last_urgent_ms = Some(T);
        assert_eq!(p.spacing(T + 1), UPDATE_SPACING_MS, "without budget it travels as any other update");
        p.reports.get_mut("mac-a").unwrap().active = false;
        assert_eq!(p.spacing(T + 1), GROUP_WINDOW_MS, "ending never waits");
        let mut stopped = phone(&[report("mac-a", true, true, T)]);
        stopped.live = None;
        assert_eq!(stopped.spacing(T), GROUP_WINDOW_MS, "starting never waits");
    }

    #[test]
    fn payloads_are_what_activitykit_decodes() {
        let state = json!({"macs": []});
        assert_eq!(
            payload(&Send::Start, &state, T + 999),
            json!({"aps": {
                "timestamp": T / 1000, "event": "start", "content-state": {"macs": []},
                "stale-date": (T + STALE_AFTER_MS) / 1000,
                "attributes-type": "AlondraActivityAttributes", "attributes": {"started_at_ms": T + 999}, "input-push-token": 1,
                "alert": {"title": {"loc-key": "Alondra"}, "body": {"loc-key": "Work in progress"}},
            }})
        );
        assert!(payload(&Send::Start, &state, T)["aps"]["alert"].get("sound").is_none(), "it never sounds");
        assert_eq!(
            payload(&Send::Update { priority: 10 }, &state, T),
            json!({"aps": {"timestamp": T / 1000, "event": "update", "content-state": {"macs": []}, "stale-date": (T + STALE_AFTER_MS) / 1000}})
        );
        assert_eq!(
            payload(&Send::End, &state, T),
            json!({"aps": {"timestamp": T / 1000, "event": "end", "content-state": {"macs": []}, "dismissal-date": T / 1000}})
        );
    }

    #[test]
    fn content_state_keeps_the_newest_macs_that_fit() {
        let reports: Vec<Report> = (0..10).map(|i| report(&format!("mac-{i}"), true, true, T + i)).collect();
        let p = phone(&reports);
        let all = p.content_state(|_| true);
        let senders: Vec<&str> = all["macs"].as_array().unwrap().iter().map(|e| e["sender"].as_str().unwrap()).collect();
        assert_eq!(senders, ["mac-9", "mac-8", "mac-7", "mac-6"]);
        assert_eq!(all["omitted"], 6);
        let small = p.content_state(|state| state["macs"].as_array().unwrap().len() <= 3);
        assert_eq!(small["macs"].as_array().unwrap().len(), 3);
        assert_eq!(small["macs"][0]["sender"], "mac-9");
        assert_eq!(small["omitted"], 7, "the Live Activity says how many Macs it does not count");
    }

    #[test]
    fn tokens_and_phone_ids_are_validated() {
        assert!(valid_token(&"ab".repeat(32)));
        assert!(valid_token(&"AB".repeat(256)));
        for bad in ["ab".repeat(31), "ab".repeat(257), format!("{}a", "ab".repeat(40)), "g0".repeat(40)] {
            assert!(!valid_token(&bad), "{bad}");
        }
        assert!(valid_phone_id("0123456789abcdef"));
        assert!(!valid_phone_id("short"));
        assert!(!valid_phone_id(&"a".repeat(65)));
        assert!(!valid_phone_id("0123456789abcdef/"));
        let registration = |environment: &str, update: Option<String>, activity: Option<&str>| Registration {
            phone_id: "0123456789abcdef".into(),
            start_token: Some("ab".repeat(40)),
            update_token: update,
            activity_id: activity.map(str::to_owned),
            started_at_ms: None,
            environment: environment.into(),
            frequent_updates: true,
        };
        assert!(registration("sandbox", Some("cd".repeat(40)), Some("4F1C-AA")).validate().is_ok());
        assert!(registration("staging", None, None).validate().is_err());
        assert!(registration("production", None, Some("4F1C")).validate().is_err(), "an activity without its token");
        assert!(registration("production", Some("cd".repeat(40)), Some("bad id")).validate().is_err());
    }

    #[test]
    fn a_start_groups_the_reports_of_the_next_seconds_and_an_update_waits_its_spacing() {
        let directory = crate::test_support::Directory::new();
        let store = Store::open(&directory.join("device-registry.json")).unwrap();
        store.set_clock(T);
        let phone_id = "0123456789abcdef";
        store
            .register("owner", "client-a", "mac-a", Registration {
                phone_id: phone_id.into(),
                start_token: Some("ab".repeat(40)),
                update_token: None,
                activity_id: None,
                started_at_ms: None,
                environment: "sandbox".into(),
                frequent_updates: true,
            })
            .unwrap();
        assert_eq!(store.due_at(phone_id), None, "nothing sent yet");
        store
            .record("mac-a", ReportRequest { device_id: "client-a".into(), envelope: envelope("mac-a"), active: true, may_start: true, urgent: false })
            .unwrap();
        assert!(store.take(phone_id).unwrap().is_some());
        store
            .register("owner", "client-a", "mac-a", Registration {
                phone_id: phone_id.into(),
                start_token: None,
                update_token: Some("cd".repeat(40)),
                activity_id: None,
                started_at_ms: Some(T),
                environment: "sandbox".into(),
                frequent_updates: true,
            })
            .unwrap();
        store.set_clock(T + UPDATE_SPACING_MS - 1);
        assert_eq!(store.due_at(phone_id), Some(T + UPDATE_SPACING_MS));
        assert!(store.take(phone_id).unwrap().is_none(), "two reports at once send one push");
        store.set_clock(T + UPDATE_SPACING_MS);
        assert_eq!(store.due_at(phone_id), None);
    }

    fn live_store(directory: &crate::test_support::Directory) -> (Store, &'static str) {
        let store = Store::open(&directory.join("device-registry.json")).unwrap();
        store.set_clock(T);
        let phone_id = "0123456789abcdef";
        store
            .register("owner", "client-a", "mac-a", Registration {
                phone_id: phone_id.into(),
                start_token: None,
                update_token: Some("cd".repeat(40)),
                activity_id: None,
                started_at_ms: Some(T),
                environment: "sandbox".into(),
                frequent_updates: true,
            })
            .unwrap();
        (store, phone_id)
    }

    fn mac_says(store: &Store, active: bool, urgent: bool) {
        store
            .record("mac-a", ReportRequest { device_id: "client-a".into(), envelope: envelope("mac-a"), active, may_start: true, urgent })
            .unwrap();
    }

    #[test]
    fn an_end_apns_did_not_take_is_owed_until_it_does() {
        let directory = crate::test_support::Directory::new();
        let (store, phone_id) = live_store(&directory);
        mac_says(&store, false, false);
        let token = "cd".repeat(40);
        assert!(store.take(phone_id).unwrap().is_some());
        store.settle(phone_id, &[(Send::End, token.clone(), Outcome::Failed)]).unwrap();
        assert_eq!(store.owing(), [phone_id], "no report will follow");
        store.set_clock(T + GROUP_WINDOW_MS);
        assert!(store.take(phone_id).unwrap().is_some());
        store.settle(phone_id, &[(Send::End, token, Outcome::Delivered)]).unwrap();
        assert!(store.owing().is_empty());
        assert_eq!(store.phone(phone_id).unwrap().live, None);
    }

    #[test]
    fn a_priority_ten_that_failed_keeps_its_budget() {
        let directory = crate::test_support::Directory::new();
        let (store, phone_id) = live_store(&directory);
        mac_says(&store, true, true);
        assert!(store.take(phone_id).unwrap().is_some());
        store.settle(phone_id, &[(Send::Update { priority: 10 }, "cd".repeat(40), Outcome::Failed)]).unwrap();
        let phone = store.phone(phone_id).unwrap();
        assert_eq!(phone.plan(T + GROUP_WINDOW_MS), vec![Send::Update { priority: 10 }]);
    }

    #[test]
    fn a_report_held_for_a_phone_is_owed_when_it_registers() {
        let directory = crate::test_support::Directory::new();
        let store = Store::open(&directory.join("device-registry.json")).unwrap();
        store.set_clock(T);
        assert_eq!(
            store.record("mac-a", ReportRequest { device_id: "client-a".into(), envelope: envelope("mac-a"), active: true, may_start: true, urgent: false }).unwrap(),
            Recorded::Held
        );
        let phone_id = store
            .register("owner", "client-a", "mac-a", Registration {
                phone_id: "0123456789abcdef".into(),
                start_token: Some("ab".repeat(40)),
                update_token: None,
                activity_id: None,
                started_at_ms: None,
                environment: "sandbox".into(),
                frequent_updates: true,
            })
            .unwrap();
        assert!(store.owes(&phone_id));
    }
}
