//! A package manager replaces the binary but never restarts the running daemon, and launchd's
//! KeepAlive does nothing while the old process lives. Under launchd the daemon watches the path
//! it was started from; once a different binary there passes `--preflight`, it drains and exits
//! with `RELAUNCH` so launchd starts the new one. A binary that fails preflight is not run.
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

/// EX_TEMPFAIL: launchd's KeepAlive starts the job again.
pub const RELAUNCH: i32 = 75;
pub const EVERY: Duration = Duration::from_secs(60);
/// A rejection can come from HOME rather than the binary (a stray file, a full disk), so the
/// same binary is checked again once this long has passed.
const RETRY_REJECTED: Duration = Duration::from_secs(600);
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(300);

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

/// The path launchd ran, as written in the plist: a package manager's stable link, not the
/// versioned file it currently points to.
pub fn launch_path() -> Option<PathBuf> {
    std::env::args_os().next().map(PathBuf::from).filter(|p| p.is_absolute())
        .or_else(|| std::env::current_exe().ok())
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Rejection {
    pub path: String,
    pub version: Option<String>,
    pub reason: String,
    pub at: i64,
}

static REJECTED: Mutex<Option<Rejection>> = Mutex::new(None);

/// The newest binary that failed preflight, while this daemon keeps running.
pub fn rejected() -> Option<Rejection> {
    REJECTED.lock().expect("rejection lock poisoned").clone()
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
    running: Identity,
    rejected: Option<(Identity, std::time::Instant)>,
    retry_rejected: Duration,
}

impl Watcher {
    pub fn new(path: PathBuf) -> std::io::Result<Self> {
        let running = identity(&path)?;
        Ok(Self { path, running, rejected: None, retry_rejected: RETRY_REJECTED })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn replaced(&self) -> bool {
        identity(&self.path).is_ok_and(|now| now != self.running)
    }

    pub fn check(&mut self, preflight: impl FnOnce(&Path) -> Outcome) -> Check {
        let Ok(found) = identity(&self.path) else { return Check::Unchanged };
        let waiting_to_retry = self.rejected.is_some_and(|(rejected, at)| rejected == found && at.elapsed() < self.retry_rejected);
        if found == self.running || waiting_to_retry {
            return Check::Unchanged;
        }
        let outcome = preflight(&self.path);
        if identity(&self.path).ok() != Some(found) {
            return Check::Unchanged;
        }
        match outcome.passed {
            Ok(()) => Check::Relaunch { version: outcome.version },
            Err(reason) => {
                self.rejected = Some((found, std::time::Instant::now()));
                Check::Rejected(Rejection { path: self.path.display().to_string(), version: outcome.version, reason, at: alondra_core::now_millis() })
            }
        }
    }
}

/// Runs the binary found at `path` the way launchd would, with this daemon's environment.
pub fn run_preflight(path: &Path) -> Outcome {
    let version = output(path, "--version", Duration::from_secs(10))
        .and_then(|(ok, text)| ok.then(|| text.split_whitespace().last().map(str::to_string)).flatten());
    let passed = match output(path, "--preflight", PREFLIGHT_TIMEOUT) {
        None => Err(format!("{} --preflight did not finish", path.display())),
        Some((true, _)) => Ok(()),
        Some((false, text)) => Err(text.lines().map(str::trim).filter(|l| !l.is_empty()).last()
            .unwrap_or("preflight failed without a reason").to_string()),
    };
    Outcome { version, passed }
}

/// A binary just written can still be held open for writing by a sibling process forked at
/// that moment; exec then fails with ETXTBSY until the sibling execs away.
fn spawn_when_not_being_written(path: &Path, arg: &str) -> Option<std::process::Child> {
    const TEXT_FILE_BUSY: i32 = 26;
    for _ in 0..100 {
        match std::process::Command::new(path).arg(arg)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn()
        {
            Err(error) if error.raw_os_error() == Some(TEXT_FILE_BUSY) => std::thread::sleep(Duration::from_millis(50)),
            spawned => return spawned.ok(),
        }
    }
    None
}

fn output(path: &Path, arg: &str, timeout: Duration) -> Option<(bool, String)> {
    let child = spawn_when_not_being_written(path, arg)?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(child.wait_with_output()); });
    let Ok(Ok(out)) = rx.recv_timeout(timeout) else {
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
        tokio::time::sleep(every).await;
        let checked = tokio::task::spawn_blocking(move || {
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
                *REJECTED.lock().expect("rejection lock poisoned") = Some(rejection);
            }
        }
    }
}

/// Where the watcher starts from, when this daemon runs under launchd.
pub fn watcher(label: &str) -> Option<Watcher> {
    if !supervised(label, |k| std::env::var(k).ok()) { return None; }
    let path = launch_path()?;
    match Watcher::new(path.clone()) {
        Ok(watcher) => Some(watcher),
        Err(e) => { log!("cannot watch {} for upgrades: {e}", path.display()); None }
    }
}

pub fn every() -> Duration {
    std::env::var("ALONDRA_BINARY_CHECK_SECS").ok().and_then(|v| v.parse::<f64>().ok())
        .filter(|s| *s > 0.0).map(Duration::from_secs_f64).unwrap_or(EVERY)
}

#[cfg(test)]
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
        let dir = Dir(std::env::temp_dir().join(format!("alondra-relaunch-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "old").unwrap();
        let mut watcher = Watcher::new(bin.clone()).unwrap();
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
        let dir = Dir(std::env::temp_dir().join(format!("alondra-relaunch-retry-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "old").unwrap();
        let mut watcher = Watcher::new(bin.clone()).unwrap();
        watcher.retry_rejected = Duration::ZERO;
        replace(&bin, "new");
        assert!(matches!(watcher.check(|_| outcome(Err("this HOME still holds legacy data"))), Check::Rejected(_)));
        assert_eq!(watcher.check(|_| outcome(Ok(()))), Check::Relaunch { version: Some("9.9.9".into()) },
            "once whatever blocked it is gone, the same binary starts");
    }

    #[test]
    fn a_binary_replaced_again_during_preflight_is_checked_next_time() {
        let dir = Dir(std::env::temp_dir().join(format!("alondra-relaunch-race-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "old").unwrap();
        let mut watcher = Watcher::new(bin.clone()).unwrap();
        replace(&bin, "half written");
        assert_eq!(watcher.check(|path| { replace(path, "complete"); outcome(Err("truncated")) }), Check::Unchanged);
        assert_eq!(watcher.check(|_| outcome(Ok(()))), Check::Relaunch { version: Some("9.9.9".into()) });
    }

    #[test]
    fn a_missing_binary_mid_upgrade_is_not_a_change() {
        let dir = Dir(std::env::temp_dir().join(format!("alondra-relaunch-gone-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "old").unwrap();
        let mut watcher = Watcher::new(bin.clone()).unwrap();
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(watcher.check(|_| panic!("nothing to preflight")), Check::Unchanged);
    }

    #[test]
    fn preflight_reports_the_last_line_the_binary_printed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir(std::env::temp_dir().join(format!("alondra-relaunch-script-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&dir.0).unwrap();
        let bin = dir.0.join("alondrad");
        std::fs::write(&bin, "#!/bin/sh\n[ \"$1\" = --version ] && { echo 'alondrad 0.2.0'; exit 0; }\necho 'replayed 10 events' >&2\necho 'preflight failed: bootstrap failed integrity audit' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outcome = run_preflight(&bin);
        assert_eq!(outcome.version.as_deref(), Some("0.2.0"));
        assert_eq!(outcome.passed, Err("preflight failed: bootstrap failed integrity audit".into()));
    }
}
