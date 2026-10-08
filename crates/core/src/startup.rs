//! Consecutive failed daemon starts under launchd, kept in HOME so a start that dies before
//! it serves is still counted, and so `alondra doctor` can say why the daemon is not up.
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Millis;

pub const FILE: &str = "startup-failures.json";
/// Failures further apart than this are not a loop: the count starts again.
pub const WINDOW_MILLIS: Millis = 10 * 60 * 1000;
/// From this many failures in a row on, each failed start waits before it exits.
pub const LOOP_AFTER: u32 = 3;
const FIRST_WAIT: Duration = Duration::from_secs(30);
const LONGEST_WAIT: Duration = Duration::from_secs(5 * 60);
pub const UNFINISHED: &str = "did not finish starting";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub failures: u32,
    pub last_at: Millis,
    pub reason: String,
    pub version: String,
}

impl Record {
    pub fn looping(&self) -> bool {
        self.failures >= LOOP_AFTER
    }
}

pub fn path(home: &Path) -> PathBuf {
    home.join(FILE)
}

pub fn read(home: &Path) -> Option<Record> {
    serde_json::from_slice(&std::fs::read(path(home)).ok()?).ok()
}

fn write(home: &Path, record: &Record) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let tmp = home.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(record)?)?;
    std::fs::rename(tmp, path(home))
}

/// Counts this start as failed until `served` clears it, so a start killed half way counts too.
pub fn begin(home: &Path, version: &str, now: Millis) -> std::io::Result<Record> {
    let previous = read(home).filter(|r| now - r.last_at <= WINDOW_MILLIS).unwrap_or_default();
    let record = Record { failures: previous.failures + 1, last_at: now, reason: UNFINISHED.into(), version: version.into() };
    write(home, &record)?;
    Ok(record)
}

pub fn failed(home: &Path, reason: &str, now: Millis) -> std::io::Result<Record> {
    let mut record = read(home).unwrap_or_default();
    record.failures = record.failures.max(1);
    record.last_at = now;
    record.reason = reason.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
    write(home, &record)?;
    Ok(record)
}

pub fn served(home: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path(home)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// How long a failed start waits before exiting, so launchd's KeepAlive restarts it at this
/// pace instead of every 10 seconds.
pub fn wait_before_exit(failures: u32) -> Option<Duration> {
    let past = failures.checked_sub(LOOP_AFTER)?;
    Some(FIRST_WAIT.saturating_mul(1u32.checked_shl(past).unwrap_or(u32::MAX)).min(LONGEST_WAIT))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(_name: &str) -> crate::test_support::Directory {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn three_failed_starts_in_a_row_are_a_loop_that_waits_longer_each_time() {
        let home = home("loop");
        let mut now = 1_000_000;
        for expected in 1..=LOOP_AFTER {
            let started = begin(&home, "0.1.12", now).unwrap();
            assert_eq!(started.failures, expected);
            assert_eq!(started.reason, UNFINISHED);
            let failed = failed(&home, "bootstrap failed integrity audit:\n task t1", now).unwrap();
            assert_eq!(failed.reason, "bootstrap failed integrity audit: task t1");
            assert_eq!(failed.looping(), expected == LOOP_AFTER);
            now += 60_000;
        }
        assert_eq!(read(&home).unwrap().version, "0.1.12");
        assert_eq!(wait_before_exit(1), None);
        assert_eq!(wait_before_exit(2), None);
        assert_eq!(wait_before_exit(3), Some(Duration::from_secs(30)));
        assert_eq!(wait_before_exit(4), Some(Duration::from_secs(60)));
        assert_eq!(wait_before_exit(6), Some(Duration::from_secs(240)));
        assert_eq!(wait_before_exit(7), Some(LONGEST_WAIT));
        assert_eq!(wait_before_exit(200), Some(LONGEST_WAIT));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn serving_clears_the_count_and_old_failures_do_not_add_up() {
        let home = home("clear");
        begin(&home, "v", 0).unwrap();
        begin(&home, "v", 1).unwrap();
        served(&home).unwrap();
        assert!(read(&home).is_none());
        served(&home).unwrap();
        assert_eq!(begin(&home, "v", 2).unwrap().failures, 1);
        assert_eq!(begin(&home, "v", 2 + WINDOW_MILLIS).unwrap().failures, 2);
        assert_eq!(begin(&home, "v", 3 + 2 * WINDOW_MILLIS).unwrap().failures, 1, "a failure more than the window ago is forgotten");
        std::fs::remove_dir_all(home).unwrap();
    }
}
