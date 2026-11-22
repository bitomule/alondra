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

fn poll_once(stream: &UnixStream, timeout: Duration) -> io::Result<bool> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let timeout = Timespec::try_from(timeout).map_err(|_| io::ErrorKind::InvalidInput)?;
    loop {
        let mut fds = [PollFd::new(stream, PollFlags::IN)];
        let polled = poll(&mut fds, Some(&timeout));
        #[cfg(test)]
        WORKING_AFTER_POLL.with(|seen| seen.borrow_mut().push(crate::clock::working()));
        match polled {
            Ok(0) => return Ok(false),
            Ok(_) => return Ok(true),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}
