//! What launchd runs for Alondra: it starts alondrad as its child and stays its parent.
//!
//! macOS records a launchd agent's Local Network permission against the signing identifier and
//! the exact path of the process responsible for the traffic. Homebrew puts every release in a
//! new Cellar directory, so a daemon launchd ran directly asked again after each update. This
//! launcher lives at a fixed path and stays responsible for alondrad's traffic, so the permission
//! survives updates. It is kept tiny and its `VERSION` only changes with its own code, so the
//! copy at the fixed path is rarely replaced.
//!
//! `alondrad-launcher PROGRAM [ARGS…]` runs PROGRAM with PROGRAM as argv[0] and the same
//! environment, forwards SIGHUP, SIGINT and SIGTERM, and exits with PROGRAM's status.
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicI32, Ordering};

const VERSION: &str = "1";
const EX_USAGE: u8 = 64;
const EX_UNAVAILABLE: u8 = 69;
const FORWARDED: [i32; 3] = [1, 2, 15];

static CHILD: AtomicI32 = AtomicI32::new(0);

unsafe extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
    fn kill(pid: i32, sig: i32) -> i32;
}

extern "C" fn forward(sig: i32) {
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe { kill(pid, sig) };
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(program) = args.next() else {
        eprintln!("usage: alondrad-launcher PROGRAM [ARGS…]");
        return ExitCode::from(EX_USAGE);
    };
    if program == "--version" {
        println!("alondrad-launcher {VERSION}");
        return ExitCode::SUCCESS;
    }
    for sig in FORWARDED {
        unsafe { signal(sig, forward) };
    }
    let mut child = match Command::new(&program).args(args).spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("alondrad-launcher: cannot start {}: {error}", program.to_string_lossy());
            return ExitCode::from(EX_UNAVAILABLE);
        }
    };
    CHILD.store(child.id() as i32, Ordering::SeqCst);
    let status = loop {
        match child.wait() {
            Ok(status) => break status,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                eprintln!("alondrad-launcher: lost {}: {error}", program.to_string_lossy());
                return ExitCode::from(EX_UNAVAILABLE);
            }
        }
    };
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => ExitCode::from(code as u8),
        (None, Some(sig)) => ExitCode::from(128u8.wrapping_add(sig as u8)),
        _ => ExitCode::FAILURE,
    }
}
