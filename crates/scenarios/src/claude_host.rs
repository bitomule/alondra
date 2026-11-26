//! The Claude of a scenario: every host the daemon starts for a task, and every run of Claude's
//! command line or of the host's probes, goes through this one edge. The Claude host model of
//! `provider-claude` plays each host; the edge gives each one a process, which listens on the
//! host's socket and holds the lock of its folder. The runner decides when a turn ends and when a
//! message is read.
//!
//! A host outlives the daemon that started it, as a real one does: its process holds the lock of
//! the host's folder, and a daemon that comes back attaches to the same socket and is sent again
//! what it had not acknowledged.

pub use alondra_claude_host_model::{ApiError, ClaudeModel, Host, MODEL, Trouble, assistant_errors};
use alondra_agent_runtime::owned_host::{HostLauncher, HostSpawn, ProbeOutput, ProbeRun};
use serde::Deserialize;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process, Stdio};
use std::sync::{Arc, Mutex};

/// The edge itself, shared by the daemon (as its launcher) and the runner.
#[derive(Clone)]
pub struct ClaudeEdge(ClaudeModel, Arc<Mutex<Slowness>>, Arc<Cores>);

/// The hosts the edge started, each played by a relay process of its own, and how it reads how many
/// threads of a process run or wait for a core.
struct Cores {
    hosts: Mutex<Vec<Arc<Host>>>,
    runnable: Mutex<Runnable>,
}

type Runnable = Arc<dyn Fn(u32) -> usize + Send + Sync>;

impl Default for Cores {
    fn default() -> Self {
        Self { hosts: Mutex::default(), runnable: Mutex::new(Arc::new(crate::cores::runnable_threads)) }
    }
}

/// How slow the hosts started from now on are, in real time: what a loaded Mac does to them.
#[derive(Clone, Copy, Default)]
struct Slowness {
    exit: Option<std::time::Duration>,
    start: Option<std::time::Duration>,
    io_timeout_seconds: Option<u64>,
    probe: Option<std::time::Duration>,
    probe_late: Option<std::time::Duration>,
}

/// The I/O timeout of the hosts of a scenario's daemon, unless a test gives a shorter one.
const IO_TIMEOUT_SECONDS: u64 = 30;

/// The variable that tells a host's relay how many milliseconds it takes to exit once told to stop.
pub const SLOW_EXIT: &str = "SCENARIO_HOST_SLOW_EXIT_MS";

/// The variable that tells a host's relay how many milliseconds it takes to open its socket.
pub const SLOW_START: &str = "SCENARIO_HOST_SLOW_START_MS";

impl std::ops::Deref for ClaudeEdge {
    type Target = ClaudeModel;
    fn deref(&self) -> &ClaudeModel {
        &self.0
    }
}

impl ClaudeEdge {
    pub fn new(home: &Path, leaks: Arc<Mutex<Vec<String>>>) -> Self {
        Self(ClaudeModel::new(home, leaks), Arc::default(), Arc::default())
    }

    /// Reads how many threads of a process run or wait for a core with `runnable` instead of asking
    /// the kernel: how a test plays a process that a loaded Mac does not run.
    pub fn read_cores_with(&self, runnable: impl Fn(u32) -> usize + Send + Sync + 'static) {
        *self.2.runnable.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(runnable);
    }

    /// Whether something the virtual clock does not count still waits for a core: the relay of a
    /// host that runs, which passes frames both ways and starts the host, or a thread of this
    /// process, the daemon's, other than the one that asks.
    pub fn waiting_for_cores(&self) -> bool {
        let runnable = self.2.runnable.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if runnable(std::process::id()) > 1 {
            return true;
        }
        let hosts = {
            let mut hosts = self.2.hosts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            hosts.retain(|host| host.alive());
            hosts.clone()
        };
        hosts.iter().any(|host| runnable(host.process_id) > 0)
    }

    /// Waits for calm (`clock::until_calm`) until `settled` holds, with `patience` of real time in
    /// which nothing of the daemon moved and nothing it waits for waits for a core.
    pub fn until_calm(&self, patience: std::time::Duration, settled: impl Fn() -> bool) -> bool {
        alondra_core::clock::until_calm(patience, settled, || self.waiting_for_cores())
    }

    /// Every host started from now on takes `by` to exit once the daemon tells it to stop, as a
    /// Claude Code that finishes writing before it goes; without it, a host exits at once.
    pub fn set_slow_exit(&self, by: std::time::Duration) {
        self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).exit = Some(by);
    }

    /// Every run of Claude's command line or of a host's probe takes `by` of real time, as a
    /// process a loaded Mac starts late; without it, they end at once.
    pub fn set_slow_probe(&self, by: std::time::Duration) {
        self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).probe = Some(by);
    }

    /// Every run of Claude's command line or of a host's probe starts `by` of real time after the
    /// daemon submitted the job that runs it, as a blocking job a loaded Mac schedules late; the
    /// model sees no probe under way until it starts.
    pub fn set_late_probe(&self, by: std::time::Duration) {
        self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).probe_late = Some(by);
    }

    /// Every host started from now on takes `by` to open its socket, as a host a loaded Mac starts
    /// late; without it, a host listens at once.
    pub fn set_slow_start(&self, by: std::time::Duration) {
        self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).start = Some(by);
    }

    /// The daemon's I/O timeout for its hosts: shorter than a slow start when a test says so, so the
    /// start outlasts it in real time.
    pub fn set_io_timeout(&self, seconds: u64) {
        self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).io_timeout_seconds = Some(seconds);
    }

    pub fn io_timeout_seconds(&self) -> u64 {
        self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).io_timeout_seconds.unwrap_or(IO_TIMEOUT_SECONDS)
    }

    pub fn trouble(&self) -> Option<Trouble> {
        self.0.trouble()
    }

    /// The host the relay process `pid` plays, from its `bootstrap`, told when that process exits
    /// however it goes; `back` is where the relay reaches the host.
    pub fn start_host(&self, bootstrap: &[u8], pid: u32, back: PathBuf) -> std::io::Result<Arc<Host>> {
        let host = self.0.start(bootstrap, pid, Box::new(Relay { group: pid as i32, back }))?;
        if let Err(error) = tell_exit(pid, host.clone()) {
            host.end(false);
            return Err(error);
        }
        self.2.hosts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(host.clone());
        Ok(host)
    }
}

/// The part of a host's bootstrap the edge reads: where its socket is and which task it runs.
#[derive(Deserialize)]
struct Bootstrap {
    socket_path: PathBuf,
    execution: alondra_agent_protocol::Execution,
}

/// The relay process of a host: it ends with its whole group, and its socket stops listening.
struct Relay {
    group: i32,
    back: PathBuf,
}

impl alondra_claude_host_model::Process for Relay {
    fn alive(&self) -> bool {
        running(self.group)
    }
    fn end(&self, _closed: bool) {
        unsafe { libc::killpg(self.group, libc::SIGKILL) };
        let _ = UnixStream::connect(&self.back);
    }
}

/// Whether the process `pid` runs: a zombie nobody reaped yet has ended.
fn running(pid: i32) -> bool {
    const ZOMBIE: u32 = 5;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into the record it is given.
    let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    // SAFETY: a read of the whole record initialized it.
    read == size && unsafe { info.assume_init() }.pbi_status != ZOMBIE
}

/// Tells `host` once its process `pid` exits, whoever ends it: the daemon kills a host's group with
/// frames it wrote still unread in the relay, which go with it. The exit is registered with the
/// kernel before this returns, while the process cannot have been reaped and its number reused.
/// The watch rests on the current clock until the exit and works while it tells the host, so a
/// wait for calm that looked while the process still ran looks again once the host ended.
fn tell_exit(pid: u32, host: Arc<Host>) -> std::io::Result<()> {
    // SAFETY: kqueue takes nothing and returns a new descriptor, or -1.
    let queue = unsafe { libc::kqueue() };
    if queue < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the descriptor is new and nothing else owns it.
    let queue = unsafe { OwnedFd::from_raw_fd(queue) };
    let exit = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one change read from `exit`, no event written.
    if unsafe { libc::kevent(queue.as_raw_fd(), &exit, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
        host.exited();
        return Ok(());
    }
    alondra_core::clock::spawn_named_thread(Some(format!("exit-of-{pid}")), move || {
        let exited = alondra_core::clock::resting(|| {
            let mut event = std::mem::MaybeUninit::<libc::kevent>::zeroed();
            loop {
                // SAFETY: no change read, at most one event written into `event`.
                let seen = unsafe { libc::kevent(queue.as_raw_fd(), std::ptr::null(), 0, event.as_mut_ptr(), 1, std::ptr::null()) };
                match seen {
                    1.. => return true,
                    0 => continue,
                    _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => continue,
                    _ => return false,
                }
            }
        });
        if exited {
            host.exited();
        }
    })?;
    Ok(())
}

/// The relay a host's process runs, which Bazel builds beside the scenarios.
fn relay() -> Result<PathBuf, String> {
    let path = std::env::var_os("SCENARIO_HOST_RELAY").ok_or("SCENARIO_HOST_RELAY names no relay: run the scenarios through Bazel")?;
    std::fs::canonicalize(&path).map_err(|error| format!("the host relay {}: {error}", PathBuf::from(path).display()))
}

impl HostLauncher for ClaudeEdge {
    fn spawn(&self, mut spawn: HostSpawn<'_>) -> std::io::Result<Child> {
        let invalid = |why: String| std::io::Error::new(std::io::ErrorKind::InvalidData, why);
        let mut text = String::new();
        spawn.stdin.read_to_string(&mut text)?;
        let bootstrap: Bootstrap = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
        let alondra_agent_protocol::Execution::Task(execution) = &bootstrap.execution else {
            return Err(invalid("the scenario runs no orchestrator host".into()));
        };
        if self.unstartable(&execution.task_id.to_string()) {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "the host's executable cannot start"));
        }
        let back = bootstrap.socket_path.with_file_name("b");
        let listener = UnixListener::bind(&back)?;
        std::fs::set_permissions(&back, std::fs::Permissions::from_mode(0o600))?;
        let interrupts = bootstrap.socket_path.with_file_name("i");
        let interrupted = UnixListener::bind(&interrupts)?;
        std::fs::set_permissions(&interrupts, std::fs::Permissions::from_mode(0o600))?;
        // The process the daemon supervises listens on the host's socket and holds the lock of the
        // host's folder, which the daemon hands over as its input, for as long as it lives: a
        // daemon that comes back finds it held, and finds it at the other end of the socket.
        let relay = relay().map_err(std::io::Error::other)?;
        let mut relay = Process::new(relay);
        let slowness = *self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(by) = slowness.exit {
            relay.env(SLOW_EXIT, by.as_millis().to_string());
        }
        if let Some(by) = slowness.start {
            relay.env(SLOW_START, by.as_millis().to_string());
        }
        let mut child = relay
            .arg(&bootstrap.socket_path)
            .arg(&back)
            .arg(&interrupts)
            .stdin(Stdio::from(spawn.stdin))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let host = match self.start_host(text.as_bytes(), child.id(), back) {
            Ok(host) => host,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let model = self.0.clone();
        let (interrupting, stopped) = (self.0.clone(), host.clone());
        self.0.spawn(move || {
            for stream in interrupted.incoming() {
                let Ok(mut stream) = stream else { return };
                let _ = std::io::Read::read(&mut stream, &mut [0]);
                interrupting.interrupt_and_close(&stopped);
            }
        });
        self.0.spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                if !host.alive() {
                    return;
                }
                model.serve(&host, stream);
                if !host.alive() {
                    return;
                }
            }
        });
        Ok(child)
    }

    fn run(&self, run: ProbeRun) -> std::io::Result<ProbeOutput> {
        let Slowness { probe, probe_late, .. } = *self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(late) = probe_late {
            std::thread::sleep(late);
        }
        self.0.probe_after(&run, probe)
    }
}
