//! The one door of the daemon to what is outside its process: every program it runs (`gh`, `git`,
//! `claude`, the Mac's tools) and every host it connects to goes through here, so a test can run
//! the daemon in an isolated world, where reaching out is refused and reported as a leak instead
//! of touching the real Mac, GitHub or the network.
//!
//! The world is real unless an [`Isolation`] is current. An isolation is current for a thread it
//! entered, and a thread that entered none falls back to every isolation alive in the process: a
//! thread the daemon starts on its own cannot slip out of the world of the test that runs it.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::future::Future;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

type Leaks = Mutex<Vec<String>>;

/// A world with nothing outside: what reaches out is refused and written down.
#[derive(Clone)]
pub struct Isolation {
    leaks: Arc<Arc<Leaks>>,
    mac: Arc<Mac>,
}

/// How the Mac of an isolated world names itself and its account.
pub struct Mac {
    pub account: String,
    pub computer: String,
}

thread_local! {
    static CURRENT: RefCell<Option<Isolation>> = const { RefCell::new(None) };
}

/// Every isolation alive in the process, for the threads that entered none.
static ALIVE: Mutex<Vec<(Weak<Arc<Leaks>>, Arc<Mac>)>> = Mutex::new(Vec::new());

fn alive() -> Vec<Isolation> {
    let alive = ALIVE.lock().unwrap_or_else(PoisonError::into_inner);
    alive.iter().filter_map(|(leaks, mac)| Some(Isolation { leaks: leaks.upgrade()?, mac: mac.clone() })).collect()
}

pub struct Entered {
    previous: Option<Isolation>,
}

impl Drop for Entered {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = self.previous.take());
    }
}

impl Isolation {
    /// An isolation that writes what reached out into `leaks`, alive while any clone of it is.
    /// While one is alive, every thread of the process that entered none is isolated too: only a
    /// process that runs nothing but isolated worlds creates one.
    pub fn new(leaks: Arc<Leaks>, mac: Mac) -> Self {
        let leaks = Arc::new(leaks);
        let mac = Arc::new(mac);
        let mut alive = ALIVE.lock().unwrap_or_else(PoisonError::into_inner);
        alive.retain(|(leaks, _)| leaks.strong_count() > 0);
        alive.push((Arc::downgrade(&leaks), mac.clone()));
        Self { leaks, mac }
    }

    /// Makes this isolation the world of the calling thread until the guard drops.
    pub fn enter(&self) -> Entered {
        Entered { previous: CURRENT.with(|current| current.borrow_mut().replace(self.clone())) }
    }

    fn record(&self, what: &str) {
        self.leaks.lock().unwrap_or_else(PoisonError::into_inner).push(what.to_owned());
    }
}

/// Lets `what` out, or refuses it with [`io::ErrorKind::PermissionDenied`] in an isolated world,
/// after writing it down where the isolation keeps its leaks.
fn reach(what: impl FnOnce() -> String) -> io::Result<()> {
    let refused = |what: &str| Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("an isolated world reaches nothing outside: {what}")));
    if let Some(isolation) = CURRENT.with(|current| current.borrow().clone()) {
        let what = what();
        isolation.record(&what);
        return refused(&what);
    }
    let alive = alive();
    if alive.is_empty() {
        return Ok(());
    }
    let what = what();
    for isolation in alive {
        isolation.record(&what);
    }
    refused(&what)
}

fn isolated() -> Option<Isolation> {
    CURRENT.with(|current| current.borrow().clone()).or_else(|| alive().into_iter().next())
}

fn answer(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok().filter(|output| output.status.success())?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|answer| !answer.is_empty())
}

/// The full name of the Mac's account, as `id -F` gives it; in an isolated world, the name of its
/// own account, which reaches nothing outside.
pub fn account_name() -> Option<String> {
    match isolated() {
        Some(isolation) => Some(isolation.mac.account.clone()),
        None => answer("/usr/bin/id", &["-F"]),
    }
}

/// The name the Mac shows on the network, as `scutil` gives it; in an isolated world, its own.
pub fn computer_name() -> Option<String> {
    match isolated() {
        Some(isolation) => Some(isolation.mac.computer.clone()),
        None => answer("scutil", &["--get", "ComputerName"]),
    }
}

fn described(program: &OsStr, args: std::process::CommandArgs<'_>) -> String {
    let mut line = format!("process {}", program.to_string_lossy());
    for arg in args {
        line.push(' ');
        line.push_str(&arg.to_string_lossy());
    }
    line
}

/// Lets a connection to `host` out. The loopback is inside the world: it is the daemon itself and
/// the processes of its test.
pub fn connect(host: &str) -> io::Result<()> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = bare == "localhost" || bare.parse::<std::net::IpAddr>().is_ok_and(|address| address.is_loopback());
    if loopback { Ok(()) } else { reach(|| format!("network {host}")) }
}

pub fn tcp_connect_timeout(address: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
    connect(&address.ip().to_string())?;
    TcpStream::connect_timeout(address, timeout)
}

/// Sends `bytes` to `target` from a socket bound to `local`.
pub fn udp_send(local: SocketAddr, target: SocketAddr, bytes: &[u8]) -> io::Result<usize> {
    connect(&target.ip().to_string())?;
    std::net::UdpSocket::bind(local)?.send_to(bytes, target)
}

macro_rules! builder {
    ($inner:ident) => {
        pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
            self.$inner.arg(arg);
            self
        }
        pub fn args<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(&mut self, args: I) -> &mut Self {
            self.$inner.args(args);
            self
        }
        pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
            self.$inner.current_dir(dir);
            self
        }
        pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
            self.$inner.env(key, value);
            self
        }
        pub fn envs<I: IntoIterator<Item = (K, V)>, K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, vars: I) -> &mut Self {
            self.$inner.envs(vars);
            self
        }
        pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
            self.$inner.env_remove(key);
            self
        }
        pub fn env_clear(&mut self) -> &mut Self {
            self.$inner.env_clear();
            self
        }
        pub fn stdin(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
            self.$inner.stdin(cfg);
            self
        }
        pub fn stdout(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
            self.$inner.stdout(cfg);
            self
        }
        pub fn stderr(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
            self.$inner.stderr(cfg);
            self
        }
        pub fn process_group(&mut self, group: i32) -> &mut Self {
            self.$inner.process_group(group);
            self
        }
        /// # Safety
        /// The same as `std::os::unix::process::CommandExt::pre_exec`.
        pub unsafe fn pre_exec<F>(&mut self, hook: F) -> &mut Self
        where
            F: FnMut() -> io::Result<()> + Send + Sync + 'static,
        {
            unsafe { self.$inner.pre_exec(hook) };
            self
        }
    };
}

/// A program the daemon runs and waits for on a thread that may block.
pub struct Command {
    command: std::process::Command,
}

impl Command {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self { command: std::process::Command::new(program) }
    }

    builder!(command);

    fn admit(&self) -> io::Result<()> {
        reach(|| described(self.command.get_program(), self.command.get_args()))
    }

    /// The command itself, for a program that is not the daemon (the CLI) and runs it its own way.
    pub fn into_std(self) -> std::process::Command {
        self.command
    }

    pub fn get_program(&self) -> &OsStr {
        self.command.get_program()
    }

    pub fn get_args(&self) -> std::process::CommandArgs<'_> {
        self.command.get_args()
    }

    pub fn get_envs(&self) -> std::process::CommandEnvs<'_> {
        self.command.get_envs()
    }

    pub fn get_current_dir(&self) -> Option<&Path> {
        self.command.get_current_dir()
    }

    pub fn output(&mut self) -> io::Result<Output> {
        self.admit()?;
        self.command.output()
    }

    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.admit()?;
        self.command.status()
    }

    pub fn spawn(&mut self) -> io::Result<std::process::Child> {
        self.admit()?;
        self.command.spawn()
    }
}

/// A program the daemon runs and waits for on its async runtime.
pub struct AsyncCommand {
    command: tokio::process::Command,
}

impl AsyncCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self { command: tokio::process::Command::new(program) }
    }

    builder!(command);

    pub fn kill_on_drop(&mut self, kill: bool) -> &mut Self {
        self.command.kill_on_drop(kill);
        self
    }

    fn admit(&self) -> io::Result<()> {
        let command = self.command.as_std();
        reach(|| described(command.get_program(), command.get_args()))
    }

    /// Starts the program at once, as tokio's `output` does: the future holds no borrow of the command.
    pub fn output(&mut self) -> impl Future<Output = io::Result<Output>> + Send + use<> {
        let running = self.admit().map(|()| self.command.output());
        async move { running?.await }
    }

    pub fn status(&mut self) -> impl Future<Output = io::Result<ExitStatus>> + Send + use<> {
        let running = self.admit().map(|()| self.command.status());
        async move { running?.await }
    }

    pub fn spawn(&mut self) -> io::Result<tokio::process::Child> {
        self.admit()?;
        self.command.spawn()
    }
}
