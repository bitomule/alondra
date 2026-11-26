//! A package manager replaces the binary but never restarts the running daemon, and launchd's
//! KeepAlive does nothing while the old process lives. Under launchd the daemon watches the path
//! it was started from; once a different binary there passes `--preflight`, it drains and exits
//! with `RELAUNCH` so launchd starts the new one. A binary that fails preflight is not run.
use alondra_core::clock;
use crate::locks::Poisonable as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;

/// EX_TEMPFAIL: launchd's KeepAlive starts the job again.
pub const RELAUNCH: i32 = 75;
pub const EVERY: Duration = Duration::from_secs(60);
/// A rejection can come from HOME rather than the binary (a stray file, a full disk), so the
/// same binary is checked again once this long has passed.
const RETRY_REJECTED: Duration = Duration::from_secs(600);
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(300);
/// Asked after preflight has run the binary once: the first start of a freshly installed binary is
/// the slow one (the system checks it before it runs), and that start gets the preflight budget.
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
}

pub fn identity(path: &Path) -> std::io::Result<Identity> {
    let meta = std::fs::metadata(path)?;
    Ok(Identity { dev: meta.dev(), ino: meta.ino(), size: meta.size(), mtime_ns: meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128 })
}

/// launchd names the job in `XPC_SERVICE_NAME`; a daemon started by hand is left alone.
pub fn supervised(label: &str, env: impl Fn(&str) -> Option<String>) -> bool {
    env("XPC_SERVICE_NAME").is_some_and(|name| !label.is_empty() && name == label)
}

static UNDER_LAUNCHD: AtomicBool = AtomicBool::new(false);

/// Keeps for `/daemon`, and says in the log, whether launchd runs this daemon: one outside it
/// hears of no upgrade, and nothing brings it back if it stops.
pub fn record_supervision(label: &str) {
    let under_launchd = supervised(label, |key| std::env::var(key).ok());
    UNDER_LAUNCHD.store(under_launchd, Ordering::Relaxed);
    if !under_launchd {
        log!("launchd does not run this daemon: an upgrade will not restart it and nothing brings it back if it stops");
    }
}

pub fn under_launchd() -> bool {
    UNDER_LAUNCHD.load(Ordering::Relaxed)
}

/// launchd can start this daemon while another still serves the same HOME: `doctor --fix` loads
/// the job under a daemon started by hand before asking that one to leave. Starting beside it
/// would take its work for interrupted, so this waits before touching anything.
pub fn wait_until_free(home: &Path, every: Duration) {
    let in_use = || alondra_core::home_lock::in_use(home).unwrap_or(false);
    if in_use() {
        log!("another daemon holds {}: waiting for it to leave", home.display());
    }
    while in_use() {
        clock::blocking_sleep(every);
    }
}

/// The path launchd ran, as written in the plist: a package manager's stable link, not the
/// versioned file it currently points to.
pub fn launch_path() -> Option<PathBuf> {
    std::env::args_os().next().map(PathBuf::from).filter(|p| p.is_absolute())
        .or_else(|| std::env::current_exe().ok())
}

/// Keeps the launcher at its fixed path in step with the one this release ships beside `daemon`,
/// so the next start under launchd runs it. Copied only when its version changed.
pub fn keep_launcher(daemon: &Path, alondra_home: &Path) {
    use alondra_host::launcher::{Installed, bundled_path, install, installed_path};
    let installed = installed_path(alondra_home);
    match install(&bundled_path(daemon), &installed) {
        Ok(Installed::Copied) => log!("launcher updated at {}", installed.display()),
        Ok(Installed::Current | Installed::NotBundled) => {}
        Err(error) => log!("could not update the launcher at {}: {error}", installed.display()),
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Rejection {
    pub path: String,
    pub version: Option<String>,
    pub reason: String,
    pub at: i64,
}

/// A holder that panics leaves it whole, so it is recovered: one assignment.
static REJECTED: Mutex<Option<Rejection>> = Mutex::new(None);

/// The newest binary that failed preflight, while this daemon keeps running.
pub fn rejected() -> Option<Rejection> {
    REJECTED.lock_or_recover("the card rejections").clone()
}

#[derive(Debug, PartialEq)]
pub enum Check {
    Unchanged,
    Relaunch { version: Option<String> },
    Rejected(Rejection),
}

pub struct Outcome {
    pub version: Option<String>,
    pub passed: Result<(), String>,
}

pub struct Watcher {
    path: PathBuf,
    /// What this daemon was started from. Unknown while the path cannot be read: the first
    /// identity read then is taken as the running one, so a failed read at start never leaves the
    /// daemon deaf to upgrades for as long as it lives.
    running: Option<Identity>,
    rejected: Option<(Identity, clock::Instant)>,
    retry_rejected: Duration,
}

impl Watcher {
    pub fn new(path: PathBuf) -> Self {
        let mut watcher = Self { path, running: None, rejected: None, retry_rejected: RETRY_REJECTED };
        if let Err(e) = watcher.baseline() {
            log!("cannot read {} yet: {e}; it is watched as soon as it can be read", watcher.path.display());
        }
        watcher
    }

    /// The identity this daemon runs from, read now if it never could be.
    fn baseline(&mut self) -> std::io::Result<Identity> {
        if let Some(running) = self.running {
            return Ok(running);
        }
        let running = identity(&self.path)?;
        log!("watching {} for a replacement", self.path.display());
        self.running = Some(running);
        Ok(running)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn replaced(&mut self) -> bool {
        let Ok(running) = self.baseline() else { return false };
        identity(&self.path).is_ok_and(|now| now != running)
    }

    pub fn check(&mut self, preflight: impl FnOnce(&Path) -> Outcome) -> Check {
        let Ok(found) = identity(&self.path) else { return Check::Unchanged };
        let Ok(running) = self.baseline() else { return Check::Unchanged };
        let waiting_to_retry = self.rejected.is_some_and(|(rejected, at)| rejected == found && alondra_core::clock::since(at) < self.retry_rejected);
        if found == running || waiting_to_retry {
            return Check::Unchanged;
        }
        let outcome = preflight(&self.path);
        if identity(&self.path).ok() != Some(found) {
            return Check::Unchanged;
        }
        match outcome.passed {
            Ok(()) => Check::Relaunch { version: outcome.version },
            Err(reason) => {
                self.rejected = Some((found, clock::instant()));
                Check::Rejected(Rejection { path: self.path.display().to_string(), version: outcome.version, reason, at: alondra_core::now_millis() })
            }
        }
    }
}

/// Runs the binary found at `path` the way launchd would, with this daemon's environment.
pub fn run_preflight(path: &Path) -> Outcome {
    let preflight = output(path, "--preflight", PREFLIGHT_TIMEOUT);
    let passed = match &preflight {
        None => Err(format!("{} --preflight did not finish", path.display())),
        Some((true, _)) => Ok(()),
        Some((false, text)) => Err(text.lines().map(str::trim).filter(|l| !l.is_empty()).last()
            .unwrap_or("preflight failed without a reason").to_string()),
    };
    let version = preflight.and_then(|_| output(path, "--version", VERSION_TIMEOUT))
        .and_then(|(ok, text)| ok.then(|| text.split_whitespace().last().map(str::to_string)).flatten());
    Outcome { version, passed }
}

fn output(path: &Path, arg: &str, timeout: Duration) -> Option<(bool, String)> {
    let child = std::process::Command::new(path).arg(arg)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = alondra_core::clock::channel();
    alondra_core::clock::spawn_thread(move || { let _ = tx.send(child.wait_with_output()); });
    let Ok(Ok(out)) = alondra_core::clock::recv_timeout(&rx, timeout) else {
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        return None;
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some((out.status.success(), text))
}

/// Returns once a replacement binary passed preflight; never returns for one that did not.
pub async fn watch(mut watcher: Watcher, every: Duration) {
    loop {
        clock::sleep(every).await;
        let checked = alondra_core::clock::spawn_blocking_unwatched(move || {
            let check = watcher.check(run_preflight);
            (watcher, check)
        }).await;
        let check;
        (watcher, check) = match checked {
            Ok(done) => done,
            Err(e) => { log!("binary check crashed: {e}"); return std::future::pending().await; }
        };
        match check {
            Check::Unchanged => (),
            Check::Relaunch { version } => {
                log!("{} was replaced by {}, which passed preflight; restarting into it",
                    watcher.path().display(), version.as_deref().unwrap_or("an unknown version"));
                return;
            }
            Check::Rejected(rejection) => {
                log!("{} was replaced by {}, which failed preflight; staying on {}: {}",
                    rejection.path, rejection.version.as_deref().unwrap_or("an unknown version"),
                    env!("CARGO_PKG_VERSION"), rejection.reason);
                *REJECTED.lock_or_recover("the card rejections") = Some(rejection);
            }
        }
    }
}

/// Where the watcher starts from, when this daemon runs under launchd.
pub fn watcher(label: &str) -> Option<Watcher> {
    if !supervised(label, |k| std::env::var(k).ok()) { return None; }
    Some(Watcher::new(launch_path()?))
}

pub fn every() -> Duration {
    std::env::var("ALONDRA_BINARY_CHECK_SECS").ok().and_then(|v| v.parse::<f64>().ok())
        .filter(|s| *s > 0.0).map(Duration::from_secs_f64).unwrap_or(EVERY)
}

#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Dir(PathBuf);
    impl Drop for Dir { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    fn replace(path: &Path, contents: &str) {
        let tmp = path.with_extension("new");
        std::fs::write(&tmp, contents).unwrap();
        std::fs::rename(tmp, path).unwrap();
    }

    fn outcome(passed: Result<(), &str>) -> Outcome {
        Outcome { version: Some("9.9.9".into()), passed: passed.map_err(str::to_string) }
    }

    #[test]
    fn only_a_launchd_job_with_this_label_is_supervised() {
        let env = |value: Option<&'static str>| move |k: &str| (k == "XPC_SERVICE_NAME").then_some(value).flatten().map(str::to_string);
        assert!(supervised("com.bitomule.alondra", env(Some("com.bitomule.alondra"))));
        assert!(!supervised("com.bitomule.alondra", env(None)));
        assert!(!supervised("com.bitomule.alondra", env(Some("0"))));
        assert!(!supervised("com.bitomule.alondra", env(Some("application.com.apple.Terminal"))));
        assert!(!supervised("", env(Some(""))));
    }

    #[test]
    fn a_replacement_relaunches_only_after_preflight_passes() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = Dir(scratch_directory.parent().unwrap().join(format!("alondra-relaunch-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "old").unwrap();
        let mut watcher = Watcher::new(bin.clone());
        let ran = &Cell::new(0);
        let preflight = |passed| move |_: &Path| { ran.set(ran.get() + 1); outcome(passed) };

        assert_eq!(watcher.check(preflight(Ok(()))), Check::Unchanged);
        assert_eq!(ran.get(), 0, "an unchanged binary is never preflighted");

        replace(&bin, "broken new");
        assert!(watcher.replaced());
        let Check::Rejected(rejection) = watcher.check(preflight(Err("bootstrap failed integrity audit"))) else { panic!("expected a rejection") };
        assert_eq!(rejection.reason, "bootstrap failed integrity audit");
        assert_eq!(rejection.version.as_deref(), Some("9.9.9"));
        assert_eq!(watcher.check(preflight(Ok(()))), Check::Unchanged, "the rejected file is not retried");
        assert_eq!(ran.get(), 1);

        replace(&bin, "fixed newer");
        assert_eq!(watcher.check(preflight(Ok(()))), Check::Relaunch { version: Some("9.9.9".into()) });
        assert_eq!(ran.get(), 2);
    }

    #[test]
    fn a_rejected_binary_is_checked_again_once_the_retry_interval_passes() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = Dir(scratch_directory.parent().unwrap().join(format!("alondra-relaunch-retry-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "old").unwrap();
        let mut watcher = Watcher::new(bin.clone());
        watcher.retry_rejected = Duration::ZERO;
        replace(&bin, "new");
        assert!(matches!(watcher.check(|_| outcome(Err("this HOME still holds legacy data"))), Check::Rejected(_)));
        assert_eq!(watcher.check(|_| outcome(Ok(()))), Check::Relaunch { version: Some("9.9.9".into()) },
            "once whatever blocked it is gone, the same binary starts");
    }

    #[test]
    fn a_binary_replaced_again_during_preflight_is_checked_next_time() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = Dir(scratch_directory.parent().unwrap().join(format!("alondra-relaunch-race-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "old").unwrap();
        let mut watcher = Watcher::new(bin.clone());
        replace(&bin, "half written");
        assert_eq!(watcher.check(|path| { replace(path, "complete"); outcome(Err("truncated")) }), Check::Unchanged);
        assert_eq!(watcher.check(|_| outcome(Ok(()))), Check::Relaunch { version: Some("9.9.9".into()) });
    }

    #[test]
    fn a_binary_that_cannot_be_read_at_start_is_watched_once_it_can() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = Dir(scratch_directory.parent().unwrap().join(format!("alondra-relaunch-unread-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        let mut watcher = Watcher::new(bin.clone());
        assert!(!watcher.replaced());
        assert_eq!(watcher.check(|_| panic!("nothing to preflight")), Check::Unchanged);
        std::fs::write(&bin, "old").unwrap();
        assert!(!watcher.replaced(), "the first binary read is the one running");
        replace(&bin, "new");
        assert!(watcher.replaced());
        assert_eq!(watcher.check(|_| outcome(Ok(()))), Check::Relaunch { version: Some("9.9.9".into()) });
    }

    #[test]
    fn a_missing_binary_mid_upgrade_is_not_a_change() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = Dir(scratch_directory.parent().unwrap().join(format!("alondra-relaunch-gone-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "old").unwrap();
        let mut watcher = Watcher::new(bin.clone());
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(watcher.check(|_| panic!("nothing to preflight")), Check::Unchanged);
    }

    /// A child another thread forks holds a copy of every descriptor this process had open, the
    /// write descriptor of a binary just written included, until its `exec`. macOS still runs the
    /// binary meanwhile: its preflight needs no wait for that child.
    #[test]
    fn a_binary_a_paused_fork_holds_open_for_writing_runs_its_preflight() {
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::process::CommandExt;
        let scratch_directory = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*scratch_directory).unwrap();
        let bin = scratch_directory.join("alondrad");
        let mut written = std::fs::File::create(&bin).unwrap();
        let (mut forked, announce) = std::os::unix::net::UnixStream::pair().unwrap();
        let (mut release, held) = std::os::unix::net::UnixStream::pair().unwrap();
        let child = std::thread::spawn(move || {
            let (announcing, holding) = (announce.as_raw_fd(), held.as_raw_fd());
            let mut command = std::process::Command::new("/usr/bin/true");
            // SAFETY: the closure runs in the forked child before its exec and calls only write and
            // read, which are async-signal-safe.
            unsafe {
                command.pre_exec(move || {
                    let mut byte = 0_u8;
                    libc::write(announcing, (&raw const byte).cast(), 1);
                    libc::read(holding, (&raw mut byte).cast(), 1);
                    Ok(())
                });
            }
            assert!(command.status().unwrap().success());
            drop((announce, held));
        });
        forked.read_exact(&mut [0_u8]).unwrap();
        written.write_all(&std::fs::read("/usr/bin/true").unwrap()).unwrap();
        drop(written);
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outcome = run_preflight(&bin);
        release.write_all(&[0]).unwrap();
        child.join().unwrap();
        assert_eq!(outcome.passed, Ok(()));
    }

    #[test]
    fn preflight_reports_the_last_line_the_binary_printed() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir(scratch_directory.parent().unwrap().join(format!("alondra-relaunch-script-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "#!/bin/sh\n[ \"$1\" = --version ] && { echo 'alondrad 0.2.0'; exit 0; }\necho 'replayed 10 events' >&2\necho 'preflight failed: bootstrap failed integrity audit' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outcome = run_preflight(&bin);
        assert_eq!(outcome.version.as_deref(), Some("0.2.0"));
        assert_eq!(outcome.passed, Err("preflight failed: bootstrap failed integrity audit".into()));
    }

    #[test]
    fn a_slow_first_start_of_a_new_binary_still_reports_its_version() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir: &Path = &scratch_directory;
        std::fs::create_dir_all(dir).unwrap();
        let bin = dir.join("alondrad");
        let started = dir.join("started");
        replace(&bin, &format!(
            "#!/bin/sh\n[ -e '{0}' ] || {{ touch '{0}'; sleep 12; }}\n[ \"$1\" = --version ] && echo 'alondrad 99.0.0'\nexit 0\n",
            started.display()));
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let outcome = run_preflight(&bin);
        assert_eq!(outcome.passed, Ok(()));
        assert_eq!(outcome.version.as_deref(), Some("99.0.0"));
    }

    /// The daemon's start waits on a thread of its own for an older daemon to leave its home. On a
    /// virtual clock that thread polls on virtual time, so two hours of an older daemon pass at once.
    #[test]
    fn two_virtual_hours_waiting_for_the_home_pass_on_the_thread_that_waits() {
        const EPOCH: alondra_core::Millis = 1_700_000_000_000;
        const TWO_HOURS: Duration = Duration::from_secs(2 * 3600);
        const EVERY: Duration = Duration::from_millis(500);
        let home = crate::test_support::Directory::new();
        let runtime = clock::VirtualRuntime::new(EPOCH).unwrap();
        let freed = runtime.block_on(async {
            let older = alondra_core::home_lock::HomeLock::shared(&home).unwrap();
            let leaving = tokio::spawn(async move {
                clock::sleep(TWO_HOURS).await;
                drop(older);
            });
            let (done, finished) = tokio::sync::oneshot::channel();
            let path = home.to_path_buf();
            clock::spawn_thread(move || {
                wait_until_free(&path, EVERY);
                done.send(alondra_core::now_millis()).ok();
            });
            let freed = finished.await.unwrap();
            leaving.await.unwrap();
            freed
        });
        let left = EPOCH + TWO_HOURS.as_millis() as alondra_core::Millis;
        assert!((left..=left + EVERY.as_millis() as alondra_core::Millis).contains(&freed), "freed at {freed}, the older daemon left at {left}");
    }
}
