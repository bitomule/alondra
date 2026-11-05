//! When each paired device last made a signed request, how often the relay said it no longer
//! knows the device's push registration, and which devices Alondra unpaired on its own. Kept
//! apart from `devices.json`, which stays the approval list and changes only on pairing and
//! revocation. Loaded on first use, so a short-lived `Auth` never reads it.
use crate::locks::Poisonable as _;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const FILE: &str = "device-usage.json";
pub const EXPIRED_KEPT_MS: i64 = 30 * 86_400_000;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Streak {
    pub count: u32,
    pub since_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct DeviceUse {
    #[serde(default)]
    pub last_used_ms: Option<i64>,
    /// Signed from this Mac's loopback: the Mac's own app or terminal, never unpaired on its own.
    #[serde(default)]
    pub local: bool,
    /// Consecutive `push_registration_not_found` answers from the relay.
    #[serde(default)]
    pub push_missing: Option<Streak>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryReason {
    Unused,
    RelayForgot,
    AccountRemoved,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Expired {
    pub id: String,
    pub name: String,
    pub reason: ExpiryReason,
    /// The days without use that unpaired it; only for `Unused`.
    #[serde(default)]
    pub days: Option<f64>,
    pub expired_at_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Usage {
    /// When Alondra started recording use: a device not seen since then is measured from here.
    pub tracking_since_ms: i64,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceUse>,
    #[serde(default)]
    pub expired: Vec<Expired>,
}

pub struct DeviceUsage {
    path: PathBuf,
    /// A holder that panics leaves it whole, so it is recovered: memory is the source and the file
    /// a copy written later; no step between its changes can panic.
    state: Mutex<Option<(Usage, bool)>>,
}

impl DeviceUsage {
    pub fn at(home: &Path) -> Self {
        Self { path: home.join(FILE), state: Mutex::new(None) }
    }

    fn with<T>(&self, now: i64, change: impl FnOnce(&mut Usage, &mut bool) -> T) -> T {
        let mut state = self.state.lock_or_recover("the device usage");
        let (usage, dirty) = state.get_or_insert_with(|| match std::fs::read(&self.path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(usage) => (usage, false),
                Err(error) => {
                    log!("device usage unreadable, starting over: {error}");
                    (Usage { tracking_since_ms: now, ..Usage::default() }, true)
                }
            },
            Err(_) => (Usage { tracking_since_ms: now, ..Usage::default() }, true),
        });
        change(usage, dirty)
    }

    pub fn touch(&self, id: &str, now: i64, local: bool) {
        self.with(now, |usage, dirty| {
            let entry = usage.devices.entry(id.to_owned()).or_default();
            entry.last_used_ms = Some(now);
            entry.local |= local;
            *dirty = true;
        });
    }

    pub fn push_not_found(&self, id: &str, now: i64) {
        self.with(now, |usage, dirty| {
            let streak = usage.devices.entry(id.to_owned()).or_default().push_missing.get_or_insert(Streak { count: 0, since_ms: now });
            streak.count += 1;
            *dirty = true;
        });
    }

    pub fn push_ok(&self, id: &str, now: i64) {
        self.with(now, |usage, dirty| {
            if let Some(entry) = usage.devices.get_mut(id).filter(|entry| entry.push_missing.is_some()) {
                entry.push_missing = None;
                *dirty = true;
            }
        });
    }

    /// Forgets `id` and records that Alondra unpaired it; expiries older than 30 days go.
    pub fn expire(&self, expired: Expired) {
        let now = expired.expired_at_ms;
        self.with(now, |usage, dirty| {
            usage.devices.remove(&expired.id);
            usage.expired.retain(|old| old.id != expired.id && now - old.expired_at_ms < EXPIRED_KEPT_MS);
            usage.expired.push(expired);
            *dirty = true;
        });
    }

    pub fn snapshot(&self, now: i64) -> Usage {
        self.with(now, |usage, _| usage.clone())
    }

    /// Writes the record when something changed since the last write.
    pub fn flush(&self, now: i64) -> Result<()> {
        let mut state = self.state.lock_or_recover("the device usage");
        let Some((usage, dirty)) = state.as_mut() else { return Ok(()) };
        if !*dirty {
            return Ok(());
        }
        usage.expired.retain(|old| now - old.expired_at_ms < EXPIRED_KEPT_MS);
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(usage)?).context("write device usage")?;
        std::fs::rename(&temporary, &self.path).context("replace device usage")?;
        *dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_survives_a_restart_once_flushed() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let usage = DeviceUsage::at(&home);
        usage.touch("phone-1", 1_000, false);
        usage.touch("client-1", 2_000, true);
        usage.flush(2_000).unwrap();
        let reopened = DeviceUsage::at(&home).snapshot(9_000);
        assert_eq!(reopened.tracking_since_ms, 1_000);
        assert_eq!(reopened.devices["phone-1"].last_used_ms, Some(1_000));
        assert!(!reopened.devices["phone-1"].local);
        assert!(reopened.devices["client-1"].local);
    }

    #[test]
    fn an_accepted_push_ends_the_missing_registration_streak() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let usage = DeviceUsage::at(&home);
        usage.push_not_found("phone-1", 10);
        usage.push_not_found("phone-1", 20);
        assert_eq!(usage.snapshot(30).devices["phone-1"].push_missing, Some(Streak { count: 2, since_ms: 10 }));
        usage.push_ok("phone-1", 30);
        assert_eq!(usage.snapshot(30).devices["phone-1"].push_missing, None);
    }

    #[test]
    fn expiries_older_than_thirty_days_are_forgotten() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let usage = DeviceUsage::at(&home);
        let expired = |id: &str, at| Expired { id: id.into(), name: id.into(), reason: ExpiryReason::Unused, days: Some(30.0), expired_at_ms: at };
        usage.touch("old", 0, false);
        usage.expire(expired("old", 0));
        assert!(!usage.snapshot(0).devices.contains_key("old"));
        usage.expire(expired("new", EXPIRED_KEPT_MS + 1));
        usage.flush(EXPIRED_KEPT_MS + 1).unwrap();
        let ids: Vec<_> = DeviceUsage::at(&home).snapshot(0).expired.into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["new"]);
    }
}
