macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!("{} alondrad: {}", $crate::log::stamp(), format_args!($($arg)*))
    };
}

pub fn stamp() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    local_iso(now.as_secs() as libc::time_t, now.subsec_millis())
}

fn local_iso(secs: libc::time_t, millis: u32) -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return format!("@{secs}");
    }
    let offset = tm.tm_gmtoff / 60;
    let sign = if offset < 0 { '-' } else { '+' };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{millis:03}{sign}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        offset.abs() / 60,
        offset.abs() % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_line_starts_with_the_local_time_and_its_offset() {
        let s = local_iso(1_790_000_000, 7);
        assert_eq!(s.len(), "2026-09-21T14:13:20.007+02:00".len(), "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[19..23], ".007");
        assert!(matches!(&s[23..24], "+" | "-"), "{s}");
        assert!(stamp().starts_with("20"));
    }
}
