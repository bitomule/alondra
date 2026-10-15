use crate::owned_host::{connect_deadline, connect_socket};
use rustix::net::{SocketAddrUnix, bind, connect, listen};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};
#[path = "../../test_support.rs"]
mod test_support;

fn assert_nonblocking(socket: &std::os::fd::OwnedFd) {
    assert!(
        rustix::fs::fcntl_getfl(socket)
            .unwrap()
            .contains(rustix::fs::OFlags::NONBLOCK),
        "connect socket must be nonblocking BEFORE entering kernel connect"
    );
}

#[test]
fn actual_connect_descriptor_is_nonblocking_and_close_on_exec_before_connect() {
    let socket = connect_socket().unwrap();
    assert_nonblocking(&socket);
    assert!(
        rustix::io::fcntl_getfd(&socket)
            .unwrap()
            .contains(rustix::io::FdFlags::CLOEXEC)
    );
}

#[test]
fn saturated_unix_backlog_returns_unresolved_without_blocking_another_endpoint() {
    let directory = test_support::Directory::for_unix_socket();
    fs::create_dir(&*directory).unwrap();
    fs::set_permissions(&*directory, fs::Permissions::from_mode(0o700)).unwrap();
    let address = SocketAddrUnix::new(directory.join("full")).unwrap();
    let listener = connect_socket().unwrap();
    bind(&listener, &address).unwrap();
    listen(&listener, 1).unwrap();
    let mut pending = Vec::new();
    let mut saturated = false;
    for _ in 0..32 {
        let client = connect_socket().unwrap();
        assert_nonblocking(&client);
        let result = connect(&client, &address);
        pending.push(client);
        match result {
            Ok(()) => {}
            Err(
                rustix::io::Errno::INPROGRESS
                | rustix::io::Errno::AGAIN
                | rustix::io::Errno::CONNREFUSED,
            ) => {
                saturated = true;
                break;
            }
            Err(error) => panic!("unexpected backlog fixture error: {error}"),
        }
    }
    assert!(
        saturated,
        "fixture must prove a saturated kernel backlog before the deadline assertion"
    );
    let result = connect_deadline(
        &directory.join("full"),
        Instant::now() + Duration::from_millis(50),
    );
    assert!(
        matches!(
            result.as_ref().err().map(std::io::Error::kind),
            Some(
                std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::ConnectionRefused
            )
        ),
        "saturation retains uncertainty, never an unauthenticated successful stream: {result:?}"
    );
    let other_address = SocketAddrUnix::new(directory.join("other")).unwrap();
    let other = connect_socket().unwrap();
    bind(&other, &other_address).unwrap();
    listen(&other, 1).unwrap();
    let connected = connect_deadline(
        &directory.join("other"),
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    assert!(connected.peer_addr().unwrap().as_pathname().is_some());
    drop(connected);
    drop(other);
    drop(pending);
    drop(listener);
}
