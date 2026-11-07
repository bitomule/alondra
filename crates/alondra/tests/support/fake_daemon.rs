//! Stands in for `alondrad` and for launchd's KeepAlive in the doctor service tests: it holds the
//! HOME lock, answers `/daemon` with a version, and on SIGUSR1 exits with 75 after starting the
//! successor launchd would start.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static LEAVE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_usr1(_: libc::c_int) {
    LEAVE.store(true, Ordering::SeqCst);
}

struct Options {
    state: PathBuf,
    port: u16,
    version: String,
    lock: Option<PathBuf>,
    unsupervised: bool,
    silent: bool,
    wait_lock: bool,
    ignore_usr1: bool,
    successor_version: Option<String>,
    successor_unsupervised: bool,
}

fn options(args: &[String]) -> Options {
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    Options {
        state: PathBuf::from(value("--state").expect("--state")),
        port: value("--port").and_then(|p| p.parse().ok()).expect("--port"),
        version: value("--version").expect("--version"),
        lock: value("--lock").map(PathBuf::from),
        unsupervised: has("--unsupervised"),
        silent: has("--silent"),
        wait_lock: has("--wait-lock"),
        ignore_usr1: has("--ignore-usr1"),
        successor_version: value("--after-usr1"),
        successor_unsupervised: has("--successor-unsupervised"),
    }
}

fn flock(file: &std::fs::File, operation: libc::c_int) -> bool {
    use std::os::fd::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), operation) == 0 }
}

fn open_lock(path: &PathBuf) -> std::fs::File {
    std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).expect("open the lock file")
}

fn wait_until_free(path: &PathBuf, stopped: &dyn Fn() -> bool) {
    let probe = open_lock(path);
    while !stopped() {
        if flock(&probe, libc::LOCK_EX | libc::LOCK_NB) {
            flock(&probe, libc::LOCK_UN);
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn successor(options: &Options, version: &str) {
    let mut command = Command::new(std::env::current_exe().expect("current exe"));
    command.args(["--state", &options.state.display().to_string(), "--port", &options.port.to_string(), "--version", version, "--wait-lock"]);
    if let Some(lock) = &options.lock {
        command.args(["--lock", &lock.display().to_string()]);
    }
    if options.successor_unsupervised {
        command.arg("--unsupervised");
    }
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
    let _ = command.spawn();
}

fn answer(listener: &TcpListener, options: &Options) -> bool {
    let Ok((mut stream, _)) = listener.accept() else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_nonblocking(false);
    let mut request = [0u8; 2048];
    let read = stream.read(&mut request).unwrap_or(0);
    let found = String::from_utf8_lossy(&request[..read]).starts_with("GET /daemon");
    let body = format!(r#"{{"version":"{}","supervised":{},"integrity_issues":[]}}"#, options.version, !options.unsupervised);
    let reply = if found {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    } else {
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
    };
    let _ = stream.write_all(reply.as_bytes());
    true
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = options(&args);
    let stopped = || options.state.join("stop").exists() || !options.state.exists();
    if !options.ignore_usr1 {
        unsafe { libc::signal(libc::SIGUSR1, on_usr1 as *const () as libc::sighandler_t) };
    } else {
        unsafe { libc::signal(libc::SIGUSR1, libc::SIG_IGN) };
    }
    std::fs::create_dir_all(options.state.join("pids")).expect("pids folder");
    std::fs::write(options.state.join("pids").join(std::process::id().to_string()), "").expect("record pid");
    if options.wait_lock && let Some(lock) = &options.lock {
        wait_until_free(lock, &stopped);
    }
    let _held = options.lock.as_ref().map(|path| {
        let file = open_lock(path);
        assert!(flock(&file, libc::LOCK_SH), "hold the HOME lock");
        file
    });
    let listener = (!options.silent).then(|| {
        let started = Instant::now();
        loop {
            match TcpListener::bind(("127.0.0.1", options.port)) {
                Ok(listener) => break listener,
                Err(error) if started.elapsed() > Duration::from_secs(10) => panic!("bind: {error}"),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    });
    if let Some(listener) = &listener {
        listener.set_nonblocking(true).expect("nonblocking");
    }
    while !stopped() {
        if LEAVE.load(Ordering::SeqCst) {
            drop(listener);
            drop(_held);
            if let Some(version) = &options.successor_version {
                successor(&options, version);
            }
            std::process::exit(75);
        }
        let served = listener.as_ref().is_some_and(|listener| answer(listener, &options));
        if !served {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
