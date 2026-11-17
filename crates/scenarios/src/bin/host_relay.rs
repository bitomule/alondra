//! The process a scenario's Claude host runs as: `host_relay <socket> <back>`. It listens on the
//! host's socket, as the real host does, and holds the lock of the host's folder on its input for as
//! long as it lives. Each connection the daemon opens is relayed, byte for byte, to the scenario's
//! host on `back`, which speaks the protocol. The daemon thus finds in this process what it finds
//! in a real host: the peer of its socket, the holder of the lock and the leader of the group it ends.
//! Like a real host it never writes to its output once it listens: the daemon that read it may be
//! gone.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

fn pipe(mut from: UnixStream, mut to: UnixStream) {
    let _ = std::io::copy(&mut from, &mut to);
    let _ = to.shutdown(std::net::Shutdown::Both);
    let _ = from.shutdown(std::net::Shutdown::Both);
}

fn main() {
    let mut arguments = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(socket), Some(back)) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: host_relay <socket> <back>");
        std::process::exit(2);
    };
    let Ok(listener) = UnixListener::bind(&socket) else { std::process::exit(1) };
    if std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).is_err() {
        std::process::exit(1);
    }
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "Alondra host: socket listening");
    let _ = stdout.flush();
    for front in listener.incoming() {
        let Ok(front) = front else { continue };
        let Ok(rear) = UnixStream::connect(&back) else { continue };
        let (Ok(front_copy), Ok(rear_copy)) = (front.try_clone(), rear.try_clone()) else { continue };
        std::thread::spawn(move || pipe(front, rear));
        std::thread::spawn(move || pipe(rear_copy, front_copy));
    }
}
