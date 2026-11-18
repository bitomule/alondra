//! `mcp-calls.log` in Alondra's home: every command a node's MCP server sends, when it got its
//! answer or gave up on it, and how long that took. It keeps one older file once it fills.
use std::io::Write as _;
use std::path::PathBuf;

const FILE: &str = "mcp-calls.log";
const ROTATED: &str = "mcp-calls.log.1";
const ROTATE_AT: u64 = 2 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn home() -> Option<PathBuf> {
    #[cfg(test)]
    return TEST_HOME.with(|home| home.borrow().clone());
    #[cfg(not(test))]
    Some(alondra_core::config::alondra_home(&|key| std::env::var(key).ok()))
}

/// Appends `text` with the local time and this process. A log that cannot be written is skipped:
/// it never fails the call.
pub(crate) fn line(text: &str) {
    let Some(home) = home() else { return };
    let path = home.join(FILE);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= ROTATE_AT) {
        let _ = std::fs::rename(&path, home.join(ROTATED));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} pid {} {text}", stamp(), std::process::id());
    }
}

fn stamp() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return format!("@{secs}");
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday,
        tm.tm_hour, tm.tm_min, tm.tm_sec, now.subsec_millis())
}
