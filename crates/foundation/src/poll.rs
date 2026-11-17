use std::io;
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Whether `stream` has bytes, or its end, to read within `timeout`. Nothing is read.
pub fn readable(stream: &UnixStream, timeout: Duration) -> io::Result<bool> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let timeout = Timespec::try_from(timeout).map_err(|_| io::ErrorKind::InvalidInput)?;
    loop {
        let mut fds = [PollFd::new(stream, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Ok(false),
            Ok(_) => return Ok(true),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}
