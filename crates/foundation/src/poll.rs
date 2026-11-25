use std::io;
use std::os::unix::net::UnixStream;
use std::time::Duration;

#[cfg(test)]
thread_local! {
    /// How many threads of the clock worked as each poll of this thread returned.
    pub(crate) static WORKING_AFTER_POLL: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Whether `stream` has bytes, or its end, to read within `timeout`. Nothing is read. Only a wait
/// for the stream takes the thread out of the clock's working threads: a look that asks for no
/// time, or finds the stream ready, keeps it working, so virtual time cannot move while it goes on
/// with what it found.
pub fn readable(stream: &UnixStream, timeout: Duration) -> io::Result<bool> {
    let ready = poll_once(stream, Duration::ZERO)?;
    if ready || timeout.is_zero() {
        return Ok(ready);
    }
    let _waiting = crate::clock::stream_wait();
    poll_once(stream, timeout)
}

/// `readable`, ended early by `bell`: false when a ring came first. The thread rests while it
/// waits, and a ring counts it as working again before it runs.
pub fn readable_unless_rung(stream: &UnixStream, bell: &crate::clock::Bell, timeout: Duration) -> io::Result<bool> {
    let work = bell.arm();
    let mut ready = poll_both(stream, bell, Duration::ZERO);
    let rested = matches!(ready, Ok((false, false))) && !timeout.is_zero();
    if rested {
        crate::clock::rest();
        ready = poll_both(stream, bell, timeout);
    }
    crate::clock::woke(work, rested, bell.disarm());
    let (readable, rung) = ready?;
    if rung {
        bell.silence();
    }
    Ok(readable)
}

fn poll_both(stream: &UnixStream, bell: &crate::clock::Bell, timeout: Duration) -> io::Result<(bool, bool)> {
    let mut fds = [rustix::event::PollFd::new(stream, rustix::event::PollFlags::IN), rustix::event::PollFd::new(&bell.clapper, rustix::event::PollFlags::IN)];
    poll_fds(&mut fds, timeout)?;
    Ok((!fds[0].revents().is_empty(), !fds[1].revents().is_empty()))
}

fn poll_once(stream: &UnixStream, timeout: Duration) -> io::Result<bool> {
    let mut fds = [rustix::event::PollFd::new(stream, rustix::event::PollFlags::IN)];
    poll_fds(&mut fds, timeout).map(|ready| ready > 0)
}

fn poll_fds(fds: &mut [rustix::event::PollFd<'_>], timeout: Duration) -> io::Result<usize> {
    let timeout = rustix::event::Timespec::try_from(timeout).map_err(|_| io::ErrorKind::InvalidInput)?;
    loop {
        let polled = rustix::event::poll(fds, Some(&timeout));
        #[cfg(test)]
        WORKING_AFTER_POLL.with(|seen| seen.borrow_mut().push(crate::clock::working()));
        match polled {
            Err(rustix::io::Errno::INTR) => continue,
            polled => return polled.map_err(io::Error::from),
        }
    }
}
