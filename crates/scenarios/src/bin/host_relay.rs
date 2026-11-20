//! The process a scenario's Claude host runs as: `host_relay <socket> <back>`. It listens on the
//! host's socket, as the real host does, and holds the lock of the host's folder on its input for as
//! long as it lives. Each connection the daemon opens is relayed, byte for byte, to the scenario's
//! host on `back`, which speaks the protocol. The daemon thus finds in this process what it finds
//! in a real host: the peer of its socket, the holder of the lock and the leader of the group it ends.
//! Like a real host it never writes to its output once it listens: the daemon that read it may be
//! gone.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// How long this relay takes to exit once told to stop, when the scenario's world says so.
static SLOW_EXIT_MS: AtomicU64 = AtomicU64::new(0);

extern "C" fn exit_slowly(_: libc::c_int) {
    let millis = SLOW_EXIT_MS.load(Ordering::SeqCst);
    let pause = libc::timespec { tv_sec: (millis / 1000) as libc::time_t, tv_nsec: ((millis % 1000) * 1_000_000) as libc::c_long };
    // SAFETY: nanosleep and _exit are async-signal-safe.
    unsafe {
        libc::nanosleep(&pause, std::ptr::null_mut());
        libc::_exit(0)
    }
}

fn pipe(mut from: UnixStream, mut to: UnixStream) {
    let _ = std::io::copy(&mut from, &mut to);
    let _ = to.shutdown(std::net::Shutdown::Both);
    let _ = from.shutdown(std::net::Shutdown::Both);
}

fn main() {
    // SAFETY: this process is still single-threaded; its socket is private from creation.
    unsafe { libc::umask(0o177) };
    let mut arguments = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(socket), Some(back)) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: host_relay <socket> <back>");
        std::process::exit(2);
    };
    if let Some(millis) = std::env::var("SCENARIO_HOST_SLOW_EXIT_MS").ok().and_then(|value| value.parse().ok()) {
        SLOW_EXIT_MS.store(millis, Ordering::SeqCst);
        // SAFETY: the handler only calls async-signal-safe functions.
        unsafe { libc::signal(libc::SIGTERM, exit_slowly as libc::sighandler_t) };
    }
    let Ok(listener) = UnixListener::bind(&socket) else { std::process::exit(1) };
    if std::env::var_os("SCENARIO_TEST_BOUND").is_some() {
        let _ = writeln!(std::io::stdout(), "bound");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_exact(&mut [0]);
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
