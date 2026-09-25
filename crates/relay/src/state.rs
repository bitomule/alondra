use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::Notify;

pub const MAX_PENDING: usize = 128;
pub const MAX_PER_DEVICE: usize = 32;
pub const MAX_STORED: usize = 512;
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

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
    revision: u64,
    sequence: u64,
}

#[derive(Default)]
pub struct Queue {
    inner: Mutex<Inner>,
    pub changed: Notify,
}

#[derive(Debug, Eq, PartialEq)]
pub enum QueueError {
    Full,
    Conflict,
    NotFound,
    WrongDevice,
    NotReceived,
    Expired,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
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
        expire(&mut inner, now_ms);
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
        let pending = inner.jobs.values().filter(|j| !terminal(j.status)).count();
        let per_device = inner
            .jobs
            .values()
            .filter(|j| j.phone_id == phone_id && !terminal(j.status))
            .count();
        if pending >= MAX_PENDING || per_device >= MAX_PER_DEVICE || inner.jobs.len() >= MAX_STORED
        {
            return Err(QueueError::Full);
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
        self.changed.notify_waiters();
        Ok(result)
    }

    pub fn next_work(&self, mac_id: &str, now_ms: u64) -> Option<WorkItem> {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, now_ms);
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
        j.status = JobStatus::Dispatched;
        j.dispatched_at_ms = Some(now_ms);
        j.revision = revision;
        let item = WorkItem {
            request_id: id,
            envelope: j.envelope.clone(),
        };
        drop(inner);
        self.changed.notify_waiters();
        Some(item)
    }

    pub fn mark_received(
        &self,
        mac_id: &str,
        id: &str,
        now_ms: u64,
    ) -> Result<JobView, QueueError> {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, now_ms);
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
            self.changed.notify_waiters();
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
        expire(&mut inner, now_ms);
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
        drop(inner);
        self.changed.notify_waiters();
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
        expire(&mut inner, now_ms);
        let j = inner.jobs.get(id).ok_or(QueueError::NotFound)?;
        if j.owner_sub != owner_sub || j.phone_id != phone_id {
            return Err(QueueError::NotFound);
        }
        Ok(view(j))
    }

    pub fn phone_for_mac(&self, mac_id: &str, id: &str, now_ms: u64) -> Result<String, QueueError> {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, now_ms);
        let j = inner.jobs.get(id).ok_or(QueueError::NotFound)?;
        if j.mac_id != mac_id {
            return Err(QueueError::WrongDevice);
        }
        Ok(j.phone_id.clone())
    }

    pub fn changes(
        &self,
        owner_sub: &str,
        phone_id: &str,
        since: u64,
        now_ms: u64,
    ) -> (u64, Vec<JobView>) {
        let mut inner = self.inner.lock().unwrap();
        expire(&mut inner, now_ms);
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
        response: None,
        ..view(j)
    }
}

fn expire(inner: &mut Inner, now_ms: u64) {
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
    }
    inner
        .jobs
        .retain(|_, j| j.tombstone_at_ms.is_none_or(|until| now_ms < until));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
        q.mark_received("mac", "a", LEASE_MS * 2 + 5).unwrap();
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
}
