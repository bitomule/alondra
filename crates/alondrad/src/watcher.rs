//! What alondrad still needs from the machine around Claude Code: where its files live, whether a
//! process is alive, and Claude's timestamps. Node state comes from the node mod's events
//! (`node_reports`), not from files Claude writes.

use std::path::PathBuf;

#[cfg(not(test))]
pub fn claude_home() -> PathBuf {
    std::env::var("CLAUDE_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude")
    })
}

/// A unit test never reads the Mac's own Claude home: it gets the one it set on its thread with
/// `use_claude_home_for_test`, else an empty folder of its own.
#[cfg(test)]
pub fn claude_home() -> PathBuf {
    TEST_CLAUDE_HOME.with(|home| home.borrow().clone())
        .unwrap_or_else(|| EMPTY_CLAUDE_HOME.with(|home| home.to_path_buf()))
}

#[cfg(test)]
thread_local! {
    static EMPTY_CLAUDE_HOME: crate::test_support::Directory = crate::test_support::Directory::new();
    static TEST_CLAUDE_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn use_claude_home_for_test(home: &std::path::Path) {
    TEST_CLAUDE_HOME.with(|current| *current.borrow_mut() = Some(home.to_path_buf()));
}

pub fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

pub(crate) fn parse_rfc3339_millis(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.trim_end_matches('Z');
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, sec) = (t.next()??, t.next()??, t.next()??);
    let ms: i64 = format!("{:0<3}", &frac[..frac.len().min(3)]).parse().ok()?;
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 86400 + h * 3600 + mi * 60 + sec) * 1000) + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_timestamps() {
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:01.500Z"), Some(1500));
        assert_eq!(parse_rfc3339_millis("2026-09-22T20:46:20.398Z"), Some(1790109980398));
    }
}
