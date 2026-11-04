//! The daemon's local action gates: one action in flight per device and request, replay receipts,
//! and the read/write locks on the resources an action touches.

use crate::locks::Poisonable as _;

pub struct LocalActionReceipt {
    pub body: Vec<u8>,
    pub reply: serde_json::Value,
    pub at: std::time::Instant,
}

pub type LocalActionKey = (String, String);
pub struct LocalActionGate {
    pub lock: tokio::sync::Mutex<Option<std::sync::Arc<LocalActionReceipt>>>,
    _slot: tokio::sync::OwnedSemaphorePermit,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocalActionResource {
    Interaction(String),
    Agent(String),
    Task(String),
    PullRequest(String),
    OrchestratorDecision,
    NodeLimitSettings,
    AccountSettings,
    AllowedRules,
    Mcp(String),
}
pub enum LocalResourceGuard {
    Read { _guard: tokio::sync::OwnedRwLockReadGuard<()> },
    Write { _guard: tokio::sync::OwnedRwLockWriteGuard<()> },
}

pub struct LocalActions {
    /// A holder that panics leaves it whole, so it is recovered: replay receipts, pruned and
    /// inserted one by one.
    receipts: std::sync::Mutex<std::collections::HashMap<LocalActionKey, std::sync::Arc<LocalActionReceipt>>>,
    /// A holder that panics leaves it whole, so it is recovered: weak pointers, pruned and inserted
    /// one by one.
    gates: std::sync::Mutex<std::collections::HashMap<LocalActionKey, std::sync::Weak<LocalActionGate>>>,
    pub slots: std::sync::Arc<tokio::sync::Semaphore>,
    pub workers: tokio::sync::Semaphore,
    /// A holder that panics leaves it whole, so it is recovered: weak pointers, pruned and inserted
    /// one by one.
    resources: std::sync::Mutex<std::collections::HashMap<LocalActionResource, std::sync::Weak<tokio::sync::RwLock<()>>>>,
    pub journal_admissions: std::sync::Arc<tokio::sync::Semaphore>,
    pub journal_worker: std::sync::Arc<tokio::sync::Semaphore>,
}
impl Default for LocalActions {
    fn default() -> Self {
        Self {
            receipts: Default::default(), gates: Default::default(),
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(1024)),
            workers: tokio::sync::Semaphore::new(2), resources: Default::default(),
            journal_admissions: std::sync::Arc::new(tokio::sync::Semaphore::new(16)),
            journal_worker: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}
impl LocalActions {
    pub fn gate(&self, key: &LocalActionKey) -> Option<std::sync::Arc<LocalActionGate>> {
        let mut gates = self.gates.lock_or_recover("the local action gates");
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(key).and_then(std::sync::Weak::upgrade) { return Some(gate); }
        let slot = self.slots.clone().try_acquire_owned().ok()?;
        let gate = std::sync::Arc::new(LocalActionGate { lock: Default::default(), _slot: slot });
        gates.insert(key.clone(), std::sync::Arc::downgrade(&gate));
        Some(gate)
    }
    pub fn resource(&self, key: LocalActionResource) -> std::sync::Arc<tokio::sync::RwLock<()>> {
        let mut resources = self.resources.lock_or_recover("the local action resources");
        resources.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = resources.get(&key).and_then(std::sync::Weak::upgrade) { return gate; }
        let gate = std::sync::Arc::new(tokio::sync::RwLock::new(()));
        resources.insert(key, std::sync::Arc::downgrade(&gate));
        gate
    }
    pub fn cached(&self, key: &LocalActionKey) -> Option<std::sync::Arc<LocalActionReceipt>> {
        let mut receipts = self.receipts.lock_or_recover("the local action receipts");
        receipts.retain(|_, entry| entry.at.elapsed() < std::time::Duration::from_secs(600));
        receipts.get(key).cloned()
    }
    pub fn remember(&self, key: LocalActionKey, receipt: LocalActionReceipt) -> std::sync::Arc<LocalActionReceipt> {
        let mut receipts = self.receipts.lock_or_recover("the local action receipts");
        if receipts.len() >= 1024 {
            let oldest = receipts.iter().min_by_key(|(_, entry)| entry.at).map(|(key, _)| key.clone());
            if let Some(oldest) = oldest { receipts.remove(&oldest); }
        }
        let receipt = std::sync::Arc::new(receipt);
        receipts.insert(key, receipt.clone());
        receipt
    }
}
