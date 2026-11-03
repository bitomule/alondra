//! Crash reports `alondra report` could not send because GitHub could not take them then: every
//! minute (every second in a sandbox with fast timing) the daemon asks its own CLI to send again
//! each one whose next try is due, in silence.
use std::path::{Path, PathBuf};
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(60);
const ONE_TRY: Duration = Duration::from_secs(15 * 60);

/// A report id as `alondra report` makes them: `r-<8 digits>-<8 hex>`.
pub fn is_report_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 3
        && parts[0] == "r"
        && parts[1].len() == 8
        && parts[1].bytes().all(|b| b.is_ascii_digit())
        && parts[2].len() == 8
        && parts[2].bytes().all(|b| b.is_ascii_hexdigit())
}

/// The pending reports in `reports` whose next try is due at `now`, oldest first.
pub fn due(reports: &Path, now: i64) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(reports) else { return Vec::new() };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().into_owned();
            let sending: serde_json::Value = serde_json::from_slice(&std::fs::read(entry.path().join("sending.json")).ok()?).ok()?;
            let pending = sending["state"].as_str() == Some("pending") && sending["next_at"].as_i64().is_some_and(|at| at <= now);
            (is_report_id(&id) && pending).then_some(id)
        })
        .collect();
    ids.sort();
    ids
}

fn send_again(cli: &Path, home: &Path, id: &str) {
    let child = std::process::Command::new(cli)
        .args(["report", "send", id, "--retry"])
        .env("ALONDRA_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return };
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    if rx.recv_timeout(ONE_TRY).is_err() {
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
}

pub async fn retry_pending(home: PathBuf, cli: Option<PathBuf>, fast: bool) {
    let Some(cli) = cli else { return };
    let reports = home.join("reports");
    loop {
        tokio::time::sleep(if fast { Duration::from_secs(1) } else { EVERY }).await;
        for id in due(&reports, alondra_core::now_millis()) {
            let (cli, home) = (cli.clone(), home.clone());
            let _ = tokio::task::spawn_blocking(move || send_again(&cli, &home, &id)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(reports: &Path, id: &str, sending: Option<serde_json::Value>) {
        let dir = reports.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(sending) = sending {
            std::fs::write(dir.join("sending.json"), sending.to_string()).unwrap();
        }
    }

    #[test]
    fn only_pending_reports_whose_try_is_due_are_sent_again() {
        let dir = crate::test_support::Directory::new();
        let reports = dir.join("reports");
        report(&reports, "r-20261026-00000001", Some(serde_json::json!({"state": "pending", "next_at": 1_000})));
        report(&reports, "r-20261026-00000002", Some(serde_json::json!({"state": "pending", "next_at": 5_000})));
        report(&reports, "r-20261026-00000003", Some(serde_json::json!({"state": "sent", "next_at": 0})));
        report(&reports, "r-20261026-00000004", None);
        report(&reports, "received", Some(serde_json::json!({"state": "pending", "next_at": 0})));
        report(&reports, "r-20261026-00000005", Some(serde_json::json!({"state": "pending", "next_at": i64::MAX})));
        assert_eq!(due(&reports, 2_000), vec!["r-20261026-00000001".to_owned()]);
        assert_eq!(due(&reports, 5_000), vec!["r-20261026-00000001".to_owned(), "r-20261026-00000002".to_owned()]);
        assert!(due(&dir.join("nothing"), 5_000).is_empty());
    }
}
