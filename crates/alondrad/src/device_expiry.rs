//! Unpairs on its own a device nobody uses: one unused for the configured days, or one whose push
//! registration the relay keeps saying it no longer has. A device used in the last day, or one
//! this Mac signs with itself, is never unpaired. Nothing asks the user: the device list says
//! which went and why, and the device itself hears it was revoked when it comes back.
use crate::auth::Device;
use crate::device_usage::{DeviceUse, Expired, ExpiryReason, Usage};
use crate::domain_bootstrap::DomainDaemon;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const DAY_MS: i64 = 86_400_000;
const IN_USE_MS: i64 = DAY_MS;
const RELAY_FORGOT_COUNT: u32 = 5;
const RELAY_FORGOT_SPAN_MS: i64 = DAY_MS;
const FLUSH_EVERY: Duration = Duration::from_secs(60);
const SWEEP_EVERY_FLUSHES: u32 = 60;

/// Why `device` should be unpaired now, if it should.
pub(crate) fn reason(device: &Device, used: Option<&DeviceUse>, tracking_since_ms: i64, now: i64, days: f64) -> Option<ExpiryReason> {
    let last_used = used.and_then(|used| used.last_used_ms).max(device.last_seen);
    if used.is_some_and(|used| used.local) || last_used.is_some_and(|at| now - at < IN_USE_MS) {
        return None;
    }
    if let Some(streak) = used.and_then(|used| used.push_missing.as_ref())
        && streak.count >= RELAY_FORGOT_COUNT
        && now - streak.since_ms >= RELAY_FORGOT_SPAN_MS
        && last_used.is_none_or(|at| at < streak.since_ms)
    {
        return Some(ExpiryReason::RelayForgot);
    }
    let since = last_used.unwrap_or(device.created_at).max(tracking_since_ms);
    (now - since >= (days * DAY_MS as f64) as i64).then_some(ExpiryReason::Unused)
}

/// The device this Mac's own terminal signs with.
fn own_terminal(home: &Path) -> Option<String> {
    std::fs::read_to_string(home.join("tui-device")).ok().map(|id| id.trim().to_owned())
}

/// The devices to unpair now, with why.
pub(crate) fn due(devices: &[Device], usage: &Usage, own: Option<&str>, now: i64, days: f64) -> Vec<(Device, ExpiryReason)> {
    devices
        .iter()
        .filter(|device| own != Some(device.id.as_str()))
        .filter_map(|device| {
            reason(device, usage.devices.get(&device.id), usage.tracking_since_ms, now, days).map(|why| (device.clone(), why))
        })
        .collect()
}

/// Unpairs every device that is due and records it; blocks on the approval lease.
pub(crate) fn sweep(daemon: &DomainDaemon, home: &Path, now: i64, days: f64) -> Vec<Expired> {
    let auth = daemon.auth();
    let usage = auth.usage().snapshot(now);
    let mut expired = Vec::new();
    for (device, why) in due(&auth.list(), &usage, own_terminal(home).as_deref(), now, days) {
        match daemon.revoke_device(&device.id) {
            Ok(true) => {
                log!("unpaired device {} ({}) on its own: {why:?}", device.id, device.name);
                let record = Expired {
                    id: device.id,
                    name: device.name,
                    reason: why,
                    days: (why == ExpiryReason::Unused).then_some(days),
                    expired_at_ms: now,
                };
                auth.usage().expire(record.clone());
                expired.push(record);
            }
            Ok(false) => {}
            Err(error) => log!("could not unpair unused device {}: {error:#}", device.id),
        }
    }
    expired
}

pub(crate) async fn run(daemon: Arc<DomainDaemon>, home: std::path::PathBuf, days: Option<f64>) {
    let mut ticks = 0u32;
    loop {
        tokio::time::sleep(FLUSH_EVERY).await;
        ticks += 1;
        let daemon = daemon.clone();
        let home = home.clone();
        let sweep_now = days.filter(|_| ticks.is_multiple_of(SWEEP_EVERY_FLUSHES));
        let done = tokio::task::spawn_blocking(move || {
            let now = alondra_core::now_millis();
            if let Some(days) = sweep_now {
                sweep(&daemon, &home, now, days);
            }
            daemon.auth().usage().flush(now)
        })
        .await;
        if let Ok(Err(error)) = done {
            log!("could not record device use: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_usage::Streak;

    const NOW: i64 = 100 * DAY_MS;

    fn device(created_at: i64) -> Device {
        Device { id: "phone-1".into(), name: "iPhone".into(), public_key: String::new(), created_at, last_seen: None }
    }

    fn used(at: Option<i64>) -> DeviceUse {
        DeviceUse { last_used_ms: at, local: false, push_missing: None }
    }

    #[test]
    fn a_device_unused_for_the_configured_days_goes() {
        let old = device(0);
        assert_eq!(reason(&old, Some(&used(Some(NOW - 31 * DAY_MS))), 0, NOW, 30.0), Some(ExpiryReason::Unused));
        assert_eq!(reason(&old, Some(&used(Some(NOW - 29 * DAY_MS))), 0, NOW, 30.0), None);
        assert_eq!(reason(&old, None, 0, NOW, 30.0), Some(ExpiryReason::Unused), "never used since it paired long ago");
        assert_eq!(reason(&device(NOW - 2 * DAY_MS), None, 0, NOW, 30.0), None, "paired two days ago");
    }

    #[test]
    fn use_from_before_recording_began_is_not_held_against_a_device() {
        assert_eq!(reason(&device(0), None, NOW - 3 * DAY_MS, NOW, 30.0), None);
    }

    #[test]
    fn the_device_in_use_is_never_unpaired() {
        let mut gone = used(Some(NOW - 60_000));
        gone.push_missing = Some(Streak { count: 50, since_ms: NOW - 10 * DAY_MS });
        assert_eq!(reason(&device(0), Some(&gone), 0, NOW, 0.0001), None);
        let local = DeviceUse { last_used_ms: Some(0), local: true, push_missing: None };
        assert_eq!(reason(&device(0), Some(&local), 0, NOW, 30.0), None, "this Mac's own app or terminal");
        let legacy = Device { last_seen: Some(NOW - 60_000), ..device(0) };
        assert_eq!(reason(&legacy, None, 0, NOW, 30.0), None);
    }

    #[test]
    fn a_device_the_relay_keeps_saying_it_no_longer_knows_goes() {
        let streak = |count, since_ms| DeviceUse { last_used_ms: Some(NOW - 5 * DAY_MS), local: false, push_missing: Some(Streak { count, since_ms }) };
        assert_eq!(reason(&device(0), Some(&streak(5, NOW - 2 * DAY_MS)), 0, NOW, 30.0), Some(ExpiryReason::RelayForgot));
        assert_eq!(reason(&device(0), Some(&streak(4, NOW - 2 * DAY_MS)), 0, NOW, 30.0), None, "too few answers");
        assert_eq!(reason(&device(0), Some(&streak(9, NOW - 3_600_000)), 0, NOW, 30.0), None, "too short a span");
        assert_eq!(reason(&device(0), Some(&streak(9, NOW - 6 * DAY_MS)), 0, NOW, 30.0), None, "used after the streak began");
    }

    #[test]
    fn this_macs_own_terminal_is_never_due() {
        let usage = Usage::default();
        assert_eq!(due(&[device(0)], &usage, None, NOW, 30.0).len(), 1);
        assert!(due(&[device(0)], &usage, Some("phone-1"), NOW, 30.0).is_empty());
    }
}
