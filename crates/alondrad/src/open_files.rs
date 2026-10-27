//! launchd starts agents with a soft limit of 256 open files. Every SDK host the daemon owns holds
//! several sockets, so with a few dozen nodes the daemon ran out of descriptors and could no
//! longer open its databases: every write answered 503.

/// macOS refuses a soft limit above `OPEN_MAX` even when the hard limit is unlimited.
const CEILING: libc::rlim_t = 10_240;

/// Raises the soft limit of open files towards the hard one; returns the limit before and after.
pub(crate) fn raise() -> std::io::Result<(u64, u64)> {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit writes into the struct it is given.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let before = limit.rlim_cur;
    let wanted = soft_limit(before, limit.rlim_max);
    if wanted > before {
        limit.rlim_cur = wanted;
        // SAFETY: setrlimit reads the struct it is given.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok((before as u64, wanted as u64))
}

fn soft_limit(soft: libc::rlim_t, hard: libc::rlim_t) -> libc::rlim_t {
    hard.min(CEILING).max(soft)
}

#[cfg(test)]
mod tests {
    use super::{soft_limit, CEILING};

    #[test]
    fn launchds_256_open_files_grow_to_what_macos_allows_and_never_shrink() {
        assert_eq!(soft_limit(256, libc::RLIM_INFINITY), CEILING);
        assert_eq!(soft_limit(256, 4_096), 4_096);
        assert_eq!(soft_limit(20_000, libc::RLIM_INFINITY), 20_000);
        assert_eq!(soft_limit(256, 256), 256);
    }

    #[test]
    fn raising_the_limit_keeps_it_at_least_where_it_was() {
        let (before, after) = super::raise().unwrap();
        assert!(after >= before);
    }
}
