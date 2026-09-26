use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub const MAX_PENDING: usize = 128;
pub const MAX_PER_DEVICE: usize = 32;
/// Full envelopes/results, including pending work. Metadata-only dedupe has its own bound.
pub const MAX_STORED: usize = 512;
pub const MAX_TOMBSTONES: usize = 8192;
pub const MAX_ENVELOPE_BYTES: usize = 256 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
pub const LEASE_MS: u64 = 30_000;
pub const MAX_LIFETIME_MS: u64 = 10 * 60_000;
const TOMBSTONE_MS: u64 = 2 * 60 * 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Dispatched,
    Received,
    Completed,
    Expired,
}

#[derive(Clone, Debug, Serialize)]
pub struct JobView {
    pub request_id: String,
    pub status: JobStatus,
    pub revision: u64,
    pub response: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
pub struct WorkItem {
    pub request_id: String,
    pub envelope: serde_json::Value,
}

#[derive(Clone, Debug)]
struct Job {
    id: String,
    owner_sub: String,
    phone_id: String,
    mac_id: String,
    request_hash: [u8; 32],
    envelope: serde_json::Value,
    response: Option<serde_json::Value>,
    status: JobStatus,
    revision: u64,
    sequence: u64,
    expires_at_ms: u64,
    dispatched_at_ms: Option<u64>,
    tombstone_at_ms: Option<u64>,
}

/// A terminal result can leave the ciphertext cache without making its ID reusable.
#[derive(Clone, Debug)]
struct Tombstone {
    id: String,
    owner_sub: String,
    phone_id: String,
    mac_id: String,
    request_hash: [u8; 32],
    response_hash: Option<[u8; 32]>,
    status: JobStatus,
    revision: u64,
    until_ms: u64,
}

impl Tombstone {
    fn view(&self) -> JobView {
        JobView { request_id: self.id.clone(), status: self.status, revision: self.revision, response: None }
    }
    fn client_result(&self) -> Result<JobView, QueueError> {
        if self.status == JobStatus::Completed { Err(QueueError::ResultEvicted) }
        else { Ok(self.view()) }
    }
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
    tombstones: HashMap<String, Tombstone>,
    revision: u64,
    sequence: u64,
}

#[derive(Default)]
pub struct Queue {
    inner: Mutex<Inner>,
    waiters: Waiters,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PollRoute {
    owner: String,
    mac: String,
    // None is a Mac work consumer; Some is a changes reader on that route.
    phone: Option<String>,
}

#[derive(Default)]
struct Waiters {
    routes: Mutex<HashMap<PollRoute, (Arc<Notify>, usize)>>,
}

/// A request owns its registration until return or cancellation. No historical
/// identifiers remain: the last subscriber removes the route synchronously.
/// HTTP admission bounds active registrations; notifications never insert keys.
pub struct QueueSubscription<'a> {
    waiters: &'a Waiters,
    route: PollRoute,
    notify: Arc<Notify>,
}

impl QueueSubscription<'_> {
    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }
}

impl Drop for QueueSubscription<'_> {
    fn drop(&mut self) {
        let mut routes = self.waiters.routes.lock().unwrap();
        let (_, subscribers) = routes.get_mut(&self.route).expect("registered poll route");
        *subscribers -= 1;
        if *subscribers == 0 {
            routes.remove(&self.route);
        }
    }
}

impl Waiters {
    fn subscribe(&self, owner: &str, mac: &str, phone: Option<&str>) -> QueueSubscription<'_> {
        let route = PollRoute { owner: owner.into(), mac: mac.into(), phone: phone.map(str::to_owned) };
        let mut routes = self.routes.lock().unwrap();
        let (notify, subscribers) = routes.entry(route.clone()).or_default();
        *subscribers += 1;
        QueueSubscription { waiters: self, route, notify: notify.clone() }
    }

    fn notify(&self, owner: &str, mac: &str, phone: Option<&str>) {
        let route = PollRoute { owner: owner.into(), mac: mac.into(), phone: phone.map(str::to_owned) };
        if let Some((notify, _)) = self.routes.lock().unwrap().get(&route) {
            notify.notify_waiters();
        }
    }

    fn notify_matching(&self, matches: impl Fn(&PollRoute) -> bool) {
        for (route, (notify, _)) in self.routes.lock().unwrap().iter() {
            if matches(route) {
                notify.notify_waiters();
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum QueueError {
    Full,
    Conflict,
    NotFound,
    WrongDevice,
    NotReceived,
    Expired,
    ResultEvicted,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe before the live authorization/snapshot check, then enable a
    /// Notified future before each check. A wake between check and await is retained.
    /// The authenticated binding is immutable for the lifetime of a device ID.
    pub fn subscribe_changes(&self, owner: &str, phone: &str, mac: &str) -> QueueSubscription<'_> {
        self.waiters.subscribe(owner, mac, Some(phone))
    }

    pub fn subscribe_work(&self, owner: &str, mac: &str) -> QueueSubscription<'_> {
        self.waiters.subscribe(owner, mac, None)
    }

    pub fn notify_owner_revoked(&self, owner: &str) {
        self.waiters.notify_matching(|route| route.owner == owner);
    }

    pub fn notify_mac_revoked(&self, mac: &str) {
        self.waiters.notify_matching(|route| route.mac == mac);
    }

    pub fn notify_phone_revoked(&self, phone: &str) {
        self.waiters.notify_matching(|route| route.phone.as_deref() == Some(phone));
    }

    #[allow(clippy::too_many_arguments)] // Submission binds identity, route, idempotency hash, expiry and arrival time.
    pub fn submit(
        &self,
        owner_sub: &str,
        phone_id: &str,
        mac_id: &str,
        request_id: &str,
        request_hash: [u8; 32],
        envelope: serde_json::Value,
        expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<JobView, QueueError> {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, &self.waiters, now_ms);
        if let Some(previous) = inner.jobs.get(request_id) {
            return if previous.owner_sub == owner_sub
                && previous.phone_id == phone_id
                && previous.mac_id == mac_id
                && previous.request_hash == request_hash
            {
                Ok(view(previous))
            } else {
                Err(QueueError::Conflict)
            };
        }
        if let Some(previous) = inner.tombstones.get(request_id) {
            return if previous.owner_sub == owner_sub && previous.phone_id == phone_id
                && previous.mac_id == mac_id && previous.request_hash == request_hash {
                previous.client_result()
            } else { Err(QueueError::Conflict) };
        }
        let pending = inner.jobs.values().filter(|j| !terminal(j.status)).count();
        let per_device = inner
            .jobs
            .values()
            .filter(|j| j.phone_id == phone_id && !terminal(j.status))
            .count();
        if pending >= MAX_PENDING || per_device >= MAX_PER_DEVICE {
            return Err(QueueError::Full);
        }
        if inner.jobs.len() >= MAX_STORED {
            compact_oldest_terminal(&mut inner, &self.waiters)?;
        }
        inner.revision += 1;
        inner.sequence += 1;
        let job = Job {
            id: request_id.into(),
            owner_sub: owner_sub.into(),
            phone_id: phone_id.into(),
            mac_id: mac_id.into(),
            request_hash,
            envelope,
            response: None,
            status: JobStatus::Queued,
            revision: inner.revision,
            sequence: inner.sequence,
            expires_at_ms,
            dispatched_at_ms: None,
            tombstone_at_ms: None,
        };
        let result = view(&job);
        inner.jobs.insert(request_id.into(), job);
        drop(inner);
        self.waiters.notify(owner_sub, mac_id, Some(phone_id));
        self.waiters.notify(owner_sub, mac_id, None);
        Ok(result)
    }

    pub fn next_work(&self, mac_id: &str, now_ms: u64) -> Option<WorkItem> {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, &self.waiters, now_ms);
        let id = inner
            .jobs
            .iter()
            .filter(|(_, j)| {
                j.mac_id == mac_id
                    && (j.status == JobStatus::Queued
                        || (matches!(j.status, JobStatus::Dispatched | JobStatus::Received)
                            && j.dispatched_at_ms
                                .is_some_and(|t| now_ms.saturating_sub(t) >= LEASE_MS)))
            })
            .min_by_key(|(_, j)| j.sequence)
            .map(|(id, _)| id.clone())?;
        inner.revision += 1;
        let revision = inner.revision;
        let j = inner.jobs.get_mut(&id).unwrap();
        // A durable Mac receipt survives a lease retry; never downgrade it.
        if j.status == JobStatus::Queued || j.status == JobStatus::Dispatched {
            j.status = JobStatus::Dispatched;
        }
        j.dispatched_at_ms = Some(now_ms);
        j.revision = revision;
        let item = WorkItem {
            request_id: id,
            envelope: j.envelope.clone(),
        };
        self.waiters.notify(&j.owner_sub, &j.mac_id, Some(&j.phone_id));
        Some(item)
    }

    pub fn mark_received(
        &self,
        mac_id: &str,
        id: &str,
        now_ms: u64,
    ) -> Result<JobView, QueueError> {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, &self.waiters, now_ms);
        if let Some(t) = inner.tombstones.get(id) {
            if t.mac_id != mac_id { return Err(QueueError::WrongDevice); }
            if t.status == JobStatus::Expired { return Err(QueueError::NotFound); }
            return Ok(t.view());
        }
        let status = inner.jobs.get(id).ok_or(QueueError::NotFound)?;
        if status.mac_id != mac_id {
            return Err(QueueError::WrongDevice);
        }
        if status.status == JobStatus::Expired {
            return Err(QueueError::NotFound);
        }
        if status.status == JobStatus::Dispatched {
            inner.revision += 1;
            let revision = inner.revision;
            let j = inner.jobs.get_mut(id).unwrap();
            j.status = JobStatus::Received;
            j.revision = revision;
            self.waiters.notify(&j.owner_sub, &j.mac_id, Some(&j.phone_id));
        }
        Ok(view(inner.jobs.get(id).unwrap()))
    }

    pub fn respond(
        &self,
        mac_id: &str,
        phone_id: &str,
        id: &str,
        response: serde_json::Value,
        now_ms: u64,
    ) -> Result<JobView, QueueError> {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, &self.waiters, now_ms);
        if let Some(t) = inner.tombstones.get(id) {
            if t.mac_id != mac_id || t.phone_id != phone_id { return Err(QueueError::WrongDevice); }
            if t.status == JobStatus::Expired { return Err(QueueError::Expired); }
            return if t.response_hash == Some(response_hash(&response)) { Ok(t.view()) }
                else { Err(QueueError::Conflict) };
        }
        let j = inner.jobs.get(id).ok_or(QueueError::NotFound)?;
        if j.mac_id != mac_id {
            return Err(QueueError::WrongDevice);
        }
        if j.phone_id != phone_id {
            return Err(QueueError::WrongDevice);
        }
        if j.status == JobStatus::Completed {
            return if j.response.as_ref() == Some(&response) {
                Ok(view(j))
            } else {
                Err(QueueError::Conflict)
            };
        }
        if j.status == JobStatus::Expired {
            return Err(QueueError::Expired);
        }
        if j.status != JobStatus::Received {
            return Err(QueueError::NotReceived);
        }
        inner.revision += 1;
        let revision = inner.revision;
        let j = inner.jobs.get_mut(id).unwrap();
        j.response = Some(response);
        j.envelope = serde_json::Value::Null;
        j.status = JobStatus::Completed;
        j.revision = revision;
        j.tombstone_at_ms = Some(now_ms.saturating_add(TOMBSTONE_MS));
        let result = view(j);
        self.waiters.notify(&j.owner_sub, &j.mac_id, Some(&j.phone_id));
        Ok(result)
    }

    pub fn get(
        &self,
        owner_sub: &str,
        phone_id: &str,
        id: &str,
        now_ms: u64,
    ) -> Result<JobView, QueueError> {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, &self.waiters, now_ms);
        if let Some(t) = inner.tombstones.get(id) {
            if t.owner_sub != owner_sub || t.phone_id != phone_id { return Err(QueueError::NotFound); }
            return t.client_result();
        }
        let j = inner.jobs.get(id).ok_or(QueueError::NotFound)?;
        if j.owner_sub != owner_sub || j.phone_id != phone_id {
            return Err(QueueError::NotFound);
        }
        Ok(view(j))
    }

    pub fn phone_for_mac(&self, mac_id: &str, id: &str, now_ms: u64) -> Result<String, QueueError> {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, &self.waiters, now_ms);
        if let Some(t) = inner.tombstones.get(id) {
            if t.mac_id != mac_id { return Err(QueueError::WrongDevice); }
            return Ok(t.phone_id.clone());
        }
        let j = inner.jobs.get(id).ok_or(QueueError::NotFound)?;
        if j.mac_id != mac_id {
            return Err(QueueError::WrongDevice);
        }
        Ok(j.phone_id.clone())
    }

    pub fn purge_owner(&self,owner:&str) {
        let mut inner=self.inner.lock().unwrap();
        inner.jobs.retain(|_,job|job.owner_sub!=owner);
        inner.tombstones.retain(|_,job|job.owner_sub!=owner);
        inner.revision+=1;
        drop(inner);self.notify_owner_revoked(owner);
    }

    pub fn purge_mac(&self,mac_id:&str) {
        let mut inner=self.inner.lock().unwrap();
        inner.jobs.retain(|_,j|j.mac_id!=mac_id);inner.tombstones.retain(|_,t|t.mac_id!=mac_id);
        inner.revision+=1;drop(inner);self.notify_mac_revoked(mac_id);
    }

    /// Drop pending and completed ciphertext for a revoked phone immediately.
    pub fn purge_phone(&self, phone_id: &str) -> usize {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.jobs.len() + inner.tombstones.len();
        inner.jobs.retain(|_, j| j.phone_id != phone_id);
        inner.tombstones.retain(|_, t| t.phone_id != phone_id);
        let removed = before - inner.jobs.len() - inner.tombstones.len();
        if removed > 0 {
            inner.revision += 1;
        }
        drop(inner);
        // Revocation must wake idle clients even when no ciphertext was queued.
        self.notify_phone_revoked(phone_id);
        removed
    }

    /// Wake a client whose result was compacted even when no retained job changed.
    /// The client rechecks its known request ID; do not synthesize a completed result.
    pub fn compacted_since(&self, owner_sub: &str, phone_id: &str, since: u64) -> bool {
        self.inner.lock().unwrap().tombstones.values().any(|t| {
            t.owner_sub == owner_sub && t.phone_id == phone_id && t.revision > since
        })
    }

    pub fn changes(
        &self,
        owner_sub: &str,
        phone_id: &str,
        since: u64,
        now_ms: u64,
    ) -> (u64, Vec<JobView>) {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, &self.waiters, now_ms);
        let views = inner
            .jobs
            .iter()
            .filter(|(_, j)| {
                j.owner_sub == owner_sub && j.phone_id == phone_id && j.revision > since
            })
            .map(|(_, j)| view_without_response(j))
            .collect();
        (inner.revision, views)
    }
}

/// Object order is irrelevant to Value equality; canonicalize recursively before hashing.
fn response_hash(value: &serde_json::Value) -> [u8; 32] {
    fn canonical(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => {
                let mut keys: Vec<_> = object.keys().collect();
                keys.sort_unstable();
                serde_json::Value::Object(keys.into_iter().map(|key| (key.clone(), canonical(&object[key]))).collect())
            }
            serde_json::Value::Array(values) => serde_json::Value::Array(values.iter().map(canonical).collect()),
            // JSON Number equality treats floating -0.0 and 0.0 as equal.
            serde_json::Value::Number(number) if number.is_f64() && number.as_f64() == Some(0.0) => {
                serde_json::json!(0.0)
            }
            value => value.clone(),
        }
    }
    Sha256::digest(serde_json::to_vec(&canonical(value)).expect("JSON Value serializes")).into()
}

fn compact_oldest_terminal(inner: &mut Inner, waiters: &Waiters) -> Result<(), QueueError> {
    if inner.tombstones.len() >= MAX_TOMBSTONES { return Err(QueueError::Full); }
    let id = inner.jobs.values().filter(|j| terminal(j.status))
        .min_by_key(|j| (j.tombstone_at_ms, j.sequence))
        .map(|j| j.id.clone()).ok_or(QueueError::Full)?;
    let job = inner.jobs.remove(&id).unwrap();
    inner.revision += 1;
    let tombstone = Tombstone {
        id: job.id, owner_sub: job.owner_sub, phone_id: job.phone_id, mac_id: job.mac_id,
        request_hash: job.request_hash, response_hash: job.response.as_ref().map(response_hash),
        status: job.status, revision: inner.revision,
        // Pressure or retries must never extend or shorten the original dedupe deadline.
        until_ms: job.tombstone_at_ms.expect("terminal job has retention deadline"),
    };
    waiters.notify(&tombstone.owner_sub, &tombstone.mac_id, Some(&tombstone.phone_id));
    inner.tombstones.insert(id, tombstone);
    Ok(())
}

fn terminal(s: JobStatus) -> bool {
    matches!(s, JobStatus::Completed | JobStatus::Expired)
}
fn view(j: &Job) -> JobView {
    JobView {
        request_id: j.id.clone(),
        status: j.status,
        revision: j.revision,
        response: j.response.clone(),
    }
}
fn view_without_response(j: &Job) -> JobView {
    JobView {
        request_id: j.id.clone(),
        status: j.status,
        revision: j.revision,
        response: None,
    }
}

fn expire(inner: &mut Inner, waiters: &Waiters, now_ms: u64) {
    let mut expired = Vec::new();
    for (id, j) in &inner.jobs {
        if !terminal(j.status) && j.expires_at_ms <= now_ms {
            expired.push(id.clone());
        }
    }
    for id in expired {
        inner.revision += 1;
        let j = inner.jobs.get_mut(&id).unwrap();
        j.status = JobStatus::Expired;
        j.revision = inner.revision;
        j.tombstone_at_ms = Some(now_ms.saturating_add(TOMBSTONE_MS));
        j.envelope = serde_json::Value::Null;
        waiters.notify(&j.owner_sub, &j.mac_id, Some(&j.phone_id));
    }
    inner
        .jobs
        .retain(|_, j| j.tombstone_at_ms.is_none_or(|until| now_ms < until));
    inner.tombstones.retain(|_, t| now_ms < t.until_ms);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Wake, Waker};

    #[derive(Default)]
    struct WakeCount(AtomicUsize);

    impl Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Poll every future before the transition and count actual task wakes. An
    /// unrelated future staying Pending alone would not detect a spurious wake.
    fn assert_wakes(subscriptions: &[QueueSubscription<'_>], expected: &[usize], transition: impl FnOnce()) {
        let counts: Vec<_> = subscriptions.iter().map(|_| Arc::new(WakeCount::default())).collect();
        let wakers: Vec<_> = counts.iter().cloned().map(Waker::from).collect();
        let mut futures: Vec<_> = subscriptions.iter().map(|s| Box::pin(s.notified())).collect();
        for (future, waker) in futures.iter_mut().zip(&wakers) {
            future.as_mut().enable();
            assert!(future.as_mut().poll(&mut Context::from_waker(waker)).is_pending());
        }
        transition();
        for (index, (future, waker)) in futures.iter_mut().zip(&wakers).enumerate() {
            let affected = expected.contains(&index);
            assert_eq!(counts[index].0.load(Ordering::SeqCst) > 0, affected, "task wake for route {index}");
            assert_eq!(future.as_mut().poll(&mut Context::from_waker(waker)).is_ready(), affected,
                "notification for route {index}");
        }
    }

    #[test]
    fn transitions_wake_only_the_affected_changes_readers_and_work_consumers() {
        let q = Queue::new();
        let subscriptions = [
            q.subscribe_changes("user", "phone", "mac"),
            q.subscribe_changes("user", "phone", "mac"), // concurrent readers must both wake
            q.subscribe_work("user", "mac"),
            q.subscribe_work("user", "mac"), // concurrent consumers compete for one item
            q.subscribe_changes("user", "sibling", "mac"),
            q.subscribe_changes("user", "other-phone", "other-mac"),
            q.subscribe_work("user", "other-mac"),
            q.subscribe_changes("foreign", "phone", "mac"),
            q.subscribe_work("foreign", "mac"),
        ];
        assert_wakes(&subscriptions, &[0, 1, 2, 3], || { submit(&q, "a"); });
        assert_wakes(&subscriptions, &[], || { submit(&q, "a"); }); // idempotent retry
        let cursor = q.changes("user", "phone", 0, 0).0;
        assert_wakes(&subscriptions, &[0, 1], || {
            let item = q.next_work("mac", 1).unwrap();
            assert_eq!(item.request_id, "a");
            assert_eq!(item.envelope, env("a"));
            assert!(q.next_work("mac", 1).is_none());
        });
        assert_wakes(&subscriptions, &[0, 1], || { q.mark_received("mac", "a", 2).unwrap(); });
        assert_wakes(&subscriptions, &[], || { q.mark_received("mac", "a", 2).unwrap(); });
        assert_wakes(&subscriptions, &[0, 1], || { q.respond("mac", "phone", "a", env("result"), 3).unwrap(); });
        assert_wakes(&subscriptions, &[], || { q.respond("mac", "phone", "a", env("result"), 3).unwrap(); });
        let (_, events) = q.changes("user", "phone", cursor, 3);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, JobStatus::Completed);
        assert!(events[0].response.is_none(), "changes remain metadata-only");
        assert_eq!(q.get("user", "phone", "a", 3).unwrap().response, Some(env("result")));
    }

    #[test]
    fn transition_fanout_stays_targeted_with_256_changes_and_32_work_waiters() {
        let q = Queue::new();
        let mut subscriptions = Vec::new();
        for account in 0..16 {
            let owner = format!("owner-{account}");
            for mac_index in 0..2 {
                let mac = format!("mac-{account}-{mac_index}");
                for phone_index in 0..8 {
                    let phone = format!("phone-{account}-{mac_index}-{phone_index}");
                    subscriptions.push(q.subscribe_changes(&owner, &phone, &mac));
                }
                subscriptions.push(q.subscribe_work(&owner, &mac));
            }
        }
        // A deterministic fanout assertion, not an HTTP capacity/load measurement.
        assert_eq!(subscriptions.len(), 288);
        assert_wakes(&subscriptions, &[0, 8], || {
            q.submit("owner-0", "phone-0-0-0", "mac-0-0", "a", [1; 32], env("a"), 100_000, 0).unwrap();
        });
        assert_wakes(&subscriptions, &[0], || { q.next_work("mac-0-0", 1).unwrap(); });
        assert_wakes(&subscriptions, &[0], || { q.mark_received("mac-0-0", "a", 2).unwrap(); });
        assert_wakes(&subscriptions, &[0], || { q.respond("mac-0-0", "phone-0-0-0", "a", env("result"), 3).unwrap(); });
    }

    #[test]
    fn empty_queue_revocations_wake_only_affected_bindings() {
        let q = Queue::new();
        let subscriptions = [
            q.subscribe_changes("user", "phone", "mac"),
            q.subscribe_changes("user", "sibling", "mac"),
            q.subscribe_work("user", "mac"),
            q.subscribe_changes("user", "other-phone", "other-mac"),
            q.subscribe_work("user", "other-mac"),
            q.subscribe_changes("foreign", "foreign-phone", "foreign-mac"),
            q.subscribe_work("foreign", "foreign-mac"),
        ];
        // SQL revocation notification and subsequent registry purge use the same scope.
        assert_wakes(&subscriptions, &[0], || q.notify_phone_revoked("phone"));
        assert_wakes(&subscriptions, &[0], || { assert_eq!(q.purge_phone("phone"), 0); });
        assert_wakes(&subscriptions, &[0, 1, 2], || q.notify_mac_revoked("mac"));
        assert_wakes(&subscriptions, &[0, 1, 2], || q.purge_mac("mac"));
        assert_wakes(&subscriptions, &[0, 1, 2, 3, 4], || q.notify_owner_revoked("user"));
        assert_wakes(&subscriptions, &[0, 1, 2, 3, 4], || q.purge_owner("user"));
    }

    #[test]
    fn expiry_from_an_unrelated_failed_lookup_wakes_the_expired_jobs_reader() {
        let q = Queue::new();
        submit(&q, "a");
        let cursor = q.changes("user", "phone", 0, 0).0;
        let subscriptions = [
            q.subscribe_changes("user", "phone", "mac"),
            q.subscribe_work("user", "mac"),
            q.subscribe_changes("other", "other-phone", "other-mac"),
        ];
        assert_wakes(&subscriptions, &[0], || {
            assert_eq!(q.get("other", "other-phone", "missing", 100_000).unwrap_err(), QueueError::NotFound);
        });
        let (_, events) = q.changes("user", "phone", cursor, 100_000);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, JobStatus::Expired);
    }

    #[test]
    fn notification_before_snapshot_or_between_snapshot_and_await_is_not_lost() {
        for before_snapshot in [false, true] {
            let q = Queue::new();
            let changes = q.subscribe_changes("user", "phone", "mac");
            let work = q.subscribe_work("user", "mac");
            let mut changed = std::pin::pin!(changes.notified());
            let mut available = std::pin::pin!(work.notified());
            changed.as_mut().enable();
            available.as_mut().enable();
            if before_snapshot { submit(&q, "a"); }
            assert_eq!(q.changes("user", "phone", 0, 0).1.is_empty(), !before_snapshot);
            if !before_snapshot {
                assert!(q.next_work("mac", 0).is_none());
                submit(&q, "a");
            }
            // No executor poll occurred before the transition: enable must retain it.
            let mut cx = Context::from_waker(Waker::noop());
            assert!(changed.as_mut().poll(&mut cx).is_ready());
            assert!(available.as_mut().poll(&mut cx).is_ready());
            assert_eq!(q.next_work("mac", 1).unwrap().request_id, "a");
            assert_eq!(q.changes("user", "phone", 0, 1).1.len(), 1);
        }
        // An event before subscription is recovered by the mandatory snapshot.
        let q = Queue::new();
        submit(&q, "early");
        let subscriptions = [q.subscribe_changes("user", "phone", "mac"), q.subscribe_work("user", "mac")];
        assert_wakes(&subscriptions, &[0], || {
            assert_eq!(q.changes("user", "phone", 0, 0).1.len(), 1);
            assert_eq!(q.next_work("mac", 1).unwrap().request_id, "early");
        });
    }

    #[test]
    fn canceled_subscriptions_remove_identifiers_without_detaching_surviving_waiters() {
        let q = Queue::new();
        let survivor = q.subscribe_work("user", "mac");
        let canceled = q.subscribe_work("user", "mac");
        assert_eq!(q.waiters.routes.lock().unwrap().len(), 1);
        {
            let mut pending = std::pin::pin!(canceled.notified());
            pending.as_mut().enable();
        }
        drop(canceled);
        assert_wakes(&[survivor], &[0], || { submit(&q, "a"); });
        assert!(q.waiters.routes.lock().unwrap().is_empty());
        for index in 0..1024 {
            let id = format!("transient-{index}");
            q.notify_owner_revoked(&id);
            q.notify_mac_revoked(&id);
            q.notify_phone_revoked(&id);
            let subscription = q.subscribe_work("user", &id);
            assert_eq!(q.waiters.routes.lock().unwrap().len(), 1);
            drop(subscription);
            assert!(q.waiters.routes.lock().unwrap().is_empty());
        }
        // Recreating a removed route still attaches to the notifier used by submit.
        let subscription = q.subscribe_work("user", "mac");
        assert_wakes(&[subscription], &[0], || { submit(&q, "b"); });
        assert!(q.waiters.routes.lock().unwrap().is_empty());
    }

    fn env(id: &str) -> serde_json::Value {
        json!({"request_id":id,"ciphertext":"opaque"})
    }
    fn submit(q: &Queue, id: &str) -> JobView {
        q.submit("user", "phone", "mac", id, [1; 32], env(id), 100_000, 0)
            .unwrap()
    }

    #[test]
    fn idempotent_submission_and_conflict() {
        let q = Queue::new();
        let first = submit(&q, "a");
        assert_eq!(submit(&q, "a").revision, first.revision);
        assert_eq!(
            q.submit("user", "phone", "mac", "a", [2; 32], env("a"), 100_000, 0)
                .unwrap_err(),
            QueueError::Conflict
        );
    }

    #[test]
    fn mac_receipt_gates_response_and_lease_redelivery() {
        let q = Queue::new();
        submit(&q, "a");
        assert!(q.next_work("other", 0).is_none());
        assert_eq!(q.next_work("mac", 1).unwrap().request_id, "a");
        assert!(q.next_work("mac", 2).is_none());
        assert_eq!(
            q.respond("mac", "phone", "a", env("a"), 3).unwrap_err(),
            QueueError::NotReceived
        );
        assert_eq!(q.next_work("mac", LEASE_MS + 2).unwrap().request_id, "a");
        q.mark_received("mac", "a", LEASE_MS + 3).unwrap();
        assert_eq!(
            q.next_work("mac", LEASE_MS * 2 + 4).unwrap().request_id,
            "a",
            "a Mac crash after receipt must permit replay from its durable receipt"
        );
        assert_eq!(
            q.get("user", "phone", "a", LEASE_MS * 2 + 5)
                .unwrap()
                .status,
            JobStatus::Received,
            "redelivery must retain the Mac's durable receipt"
        );
        assert_eq!(
            q.respond("mac", "phone", "a", env("a"), LEASE_MS * 2 + 6)
                .unwrap()
                .status,
            JobStatus::Completed
        );
        assert_eq!(
            q.respond("mac", "phone", "a", env("a"), LEASE_MS * 2 + 7)
                .unwrap()
                .status,
            JobStatus::Completed
        );
        assert_eq!(
            q.respond("mac", "phone", "a", env("different"), LEASE_MS * 2 + 7)
                .unwrap_err(),
            QueueError::Conflict
        );
    }

    #[test]
    fn expiry_keeps_tombstone_then_frees_slot() {
        let q = Queue::new();
        submit(&q, "a");
        assert_eq!(
            q.get("user", "phone", "a", 100_000).unwrap().status,
            JobStatus::Expired
        );
        assert!(q.get("user", "phone", "a", 100_000 + TOMBSTONE_MS).is_err());
    }

    fn complete(q: &Queue, id: &str, owner: &str, phone: &str, mac: &str, now: u64) {
        q.submit(owner, phone, mac, id, [1; 32], env(id), now + MAX_LIFETIME_MS, now).unwrap();
        assert_eq!(q.next_work(mac, now).unwrap().request_id, id);
        q.mark_received(mac, id, now).unwrap();
        q.respond(mac, phone, id, env(id), now).unwrap();
    }

    #[test]
    fn six_hundred_forty_two_serial_results_fit_without_unbounded_ciphertext() {
        let q = Queue::new();
        for index in 0..642 {
            let id = format!("chunk-{index}");
            complete(&q, &id, "owner", "phone", "mac", 10);
            assert_eq!(q.get("owner", "phone", &id, 10).unwrap().response, Some(env(&id)));
            let inner = q.inner.lock().unwrap();
            assert!(inner.jobs.len() <= MAX_STORED);
            assert!(inner.tombstones.len() <= MAX_TOMBSTONES);
            assert!(inner.jobs.values().all(|j| j.envelope.is_null()));
            let bytes: usize = inner.jobs.values().filter_map(|j| j.response.as_ref())
                .map(|v| serde_json::to_vec(v).unwrap().len()).sum();
            assert!(bytes <= MAX_STORED * 64, "only cached responses retain ciphertext");
        }
        let inner = q.inner.lock().unwrap();
        assert_eq!(inner.jobs.len(), MAX_STORED);
        assert_eq!(inner.tombstones.len(), 642 - MAX_STORED);
        drop(inner);
        assert_eq!(q.get("owner", "phone", "chunk-0", 11).unwrap_err(), QueueError::ResultEvicted);
        assert!(q.next_work("mac", LEASE_MS + 11).is_none(), "compacted completed work must never return to the Mac");
    }

    #[test]
    fn compacted_result_replays_remain_bound_and_never_restore_bytes_or_extend_retention() {
        let q = Queue::new();
        for index in 0..=MAX_STORED { complete(&q, &format!("done-{index}"), "owner", "phone", "mac", 10); }
        let until = q.inner.lock().unwrap().tombstones["done-0"].until_ms;
        assert_eq!(until, 10 + TOMBSTONE_MS);
        assert_eq!(q.submit("owner", "phone", "mac", "done-0", [1; 32], env("done-0"), 100_000, 11).unwrap_err(), QueueError::ResultEvicted);
        for (owner, phone, mac, digest) in [
            ("other", "phone", "mac", [1; 32]), ("owner", "other", "mac", [1; 32]),
            ("owner", "phone", "other", [1; 32]), ("owner", "phone", "mac", [2; 32]),
        ] {
            assert_eq!(q.submit(owner, phone, mac, "done-0", digest, env("done-0"), 100_000, 11).unwrap_err(), QueueError::Conflict);
        }
        assert_eq!(q.get("other", "phone", "done-0", 11).unwrap_err(), QueueError::NotFound);
        assert_eq!(q.get("owner", "other", "done-0", 11).unwrap_err(), QueueError::NotFound);
        assert_eq!(q.respond("other", "phone", "done-0", env("done-0"), 11).unwrap_err(), QueueError::WrongDevice);
        assert_eq!(q.respond("mac", "other", "done-0", env("done-0"), 11).unwrap_err(), QueueError::WrongDevice);
        assert_eq!(q.respond("mac", "phone", "done-0", env("changed"), 11).unwrap_err(), QueueError::Conflict);
        let reordered = serde_json::from_str(r#"{"ciphertext":"opaque","request_id":"done-0"}"#).unwrap();
        let repeated = q.respond("mac", "phone", "done-0", reordered, 11).unwrap();
        assert_eq!(repeated.status, JobStatus::Completed);
        assert!(repeated.response.is_none());
        assert_eq!(q.mark_received("mac", "done-0", 11).unwrap().status, JobStatus::Completed);
        assert_eq!(q.phone_for_mac("other", "done-0", 11).unwrap_err(), QueueError::WrongDevice);
        assert_eq!(q.phone_for_mac("mac", "done-0", 11).unwrap(), "phone");
        let inner = q.inner.lock().unwrap();
        assert!(!inner.jobs.contains_key("done-0"));
        assert_eq!(inner.tombstones["done-0"].until_ms, until);
        drop(inner);
        assert_eq!(q.get("owner", "phone", "done-0", until - 1).unwrap_err(), QueueError::ResultEvicted);
        assert_eq!(q.get("owner", "phone", "done-0", until).unwrap_err(), QueueError::NotFound);
    }

    #[test]
    fn response_hash_matches_value_equality_for_nested_object_order() {
        let a: serde_json::Value = serde_json::from_str(r#"{"z":[{"b":2,"a":1}],"a":"value"}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"a":"value","z":[{"a":1,"b":2}]}"#).unwrap();
        assert_eq!(a, b);
        assert_eq!(response_hash(&a), response_hash(&b));
        let zero = json!(0.0);
        let negative_zero = json!(-0.0);
        assert_eq!(zero, negative_zero);
        assert_eq!(response_hash(&zero), response_hash(&negative_zero));
        assert_ne!(response_hash(&a), response_hash(&json!({"a":"different","z":[{"a":1,"b":2}]})));
    }

    #[test]
    fn pressure_never_evicts_pending_work_and_retention_capacity_fails_closed() {
        let q = Queue::new();
        q.submit("owner", "waiting-phone", "waiting-mac", "waiting", [1; 32], env("waiting"), 100_000, 10).unwrap();
        for index in 0..MAX_STORED { complete(&q, &format!("done-{index}"), "owner", "phone", "mac", 10); }
        assert_eq!(q.get("owner", "waiting-phone", "waiting", 11).unwrap().status, JobStatus::Queued);
        let mut inner = q.inner.lock().unwrap();
        let original = inner.tombstones["done-0"].clone();
        // Fill only the metadata boundary; the 642-job test exercises real compaction.
        for index in 1..MAX_TOMBSTONES {
            let mut tombstone = original.clone();
            tombstone.id = format!("retained-{index}");
            inner.tombstones.insert(tombstone.id.clone(), tombstone);
        }
        let before_revision = inner.revision;
        drop(inner);
        assert_eq!(q.submit("owner", "new-phone", "mac", "new", [1; 32], env("new"), 100_000, 11).unwrap_err(), QueueError::Full);
        let inner = q.inner.lock().unwrap();
        assert_eq!(inner.revision, before_revision);
        assert_eq!(inner.jobs.len(), MAX_STORED);
        assert_eq!(inner.tombstones.len(), MAX_TOMBSTONES);
        assert!(inner.tombstones.contains_key("done-0"));
        drop(inner);
        assert_eq!(q.next_work("waiting-mac", 12).unwrap().request_id, "waiting");
        assert_eq!(q.submit("owner", "phone", "mac", "done-0", [2; 32], env("done-0"), 100_000, 12).unwrap_err(), QueueError::Conflict);
    }

    #[test]
    fn compacted_cursor_is_scoped_advances_once_and_revocation_purges_both_stores() {
        let q = Queue::new();
        for index in 0..MAX_STORED { complete(&q, &format!("done-{index}"), "owner", "phone", "mac", 10); }
        let cursor = q.changes("owner", "phone", 0, 11).0;
        let subscriptions = [
            q.subscribe_changes("owner", "phone", "mac"),
            q.subscribe_work("owner", "mac"),
            q.subscribe_changes("other", "other-phone", "other-mac"),
            q.subscribe_work("other", "other-mac"),
            q.subscribe_changes("unrelated", "idle-phone", "idle-mac"),
        ];
        assert_wakes(&subscriptions, &[0, 2, 3], || {
            q.submit("other", "other-phone", "other-mac", "new", [1; 32], env("new"), 100_000, 12).unwrap();
        });
        let (revision, events) = q.changes("owner", "phone", cursor, 12);
        assert!(events.is_empty(), "compaction must not invent a successful result");
        assert!(revision > cursor);
        assert!(q.compacted_since("owner", "phone", cursor));
        assert!(!q.compacted_since("other", "phone", cursor));
        assert!(!q.compacted_since("owner", "other-phone", cursor));
        assert!(!q.compacted_since("owner", "phone", revision), "the returned cursor must not spin");
        assert_eq!(q.purge_phone("phone"), MAX_STORED);
        assert!(!q.compacted_since("owner", "phone", cursor));
        assert_eq!(q.get("owner", "phone", "done-0", 12).unwrap_err(), QueueError::NotFound);
        assert!(q.get("other", "other-phone", "new", 12).is_ok());
    }

    #[test]
    fn compacted_expired_work_keeps_its_terminal_status_and_original_deadline() {
        let q = Queue::new();
        q.submit("owner", "phone", "mac", "expired", [1; 32], env("expired"), 10, 0).unwrap();
        assert_eq!(q.get("owner", "phone", "expired", 10).unwrap().status, JobStatus::Expired);
        for index in 0..MAX_STORED { complete(&q, &format!("done-{index}"), "owner", "phone", "mac", 11); }
        assert_eq!(q.get("owner", "phone", "expired", 12).unwrap().status, JobStatus::Expired);
        assert_eq!(q.mark_received("mac", "expired", 12).unwrap_err(), QueueError::NotFound);
        assert_eq!(q.respond("mac", "phone", "expired", env("expired"), 12).unwrap_err(), QueueError::Expired);
        assert_eq!(q.inner.lock().unwrap().tombstones["expired"].until_ms, 10 + TOMBSTONE_MS);
    }

}
