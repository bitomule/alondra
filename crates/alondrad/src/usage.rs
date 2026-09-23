use std::path::Path;

use alondra_core::usage::{self, Usage};
use serde_json::Value;

/// The subscription's usage as the usage hooks last wrote it; absent when they never ran here.
pub fn current(claude_home: &Path) -> Option<Usage> {
    let read = |name: &str| std::fs::read_to_string(claude_home.join(name)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let state = read(usage::STATE_FILE)?;
    let daily = read(usage::DAILY_FILE).unwrap_or(Value::Null);
    let (now, today) = local_today();
    usage::compute(&state, &daily, now, &today)
}

fn local_today() -> (i64, String) {
    let t: libc::time_t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    (t as i64, format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday))
}
