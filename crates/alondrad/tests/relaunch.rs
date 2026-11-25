//! The launchd upgrade path, end to end with the real binary: preflight, self-relaunch after
//! the binary at the launch path is replaced, and the crash-loop guard.
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

const LABEL: &str = "test.alondra.relaunch";

/// Every wait here ends when its condition holds; only a daemon that never gets there reaches
/// this limit, so a slow machine never fails a test.
const HANG_LIMIT: Duration = Duration::from_secs(600);

/// How often the daemons here look at their binary for an upgrade.
const BINARY_CHECK: Duration = Duration::from_millis(200);

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    /// The launch path: a link to the binary, as a package manager's stable link is. macOS checks a
    /// new executable after it starts, later the busier the Mac, by reading the file at the path it
    /// ran from, and kills the process when that file is gone. Only the link moves, so no file a
    /// daemon ran is removed or replaced while the fixture lives.
    bin: PathBuf,
    port: u16,
    sandbox: bool,
    _installed: Arc<Installed>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let temporary = std::env::var_os("ALONDRA_SHORT_TMPDIR").or_else(|| std::env::var_os("TMPDIR")).map(PathBuf::from).expect("run through Bazel: its test wrapper gives the test a short temporary folder");
        let root = temporary.join(format!("ad-{name}-{}", nanos() % 1_000_000_000));
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let bin = root.join("bin/alondrad");
        let installed = Installed::shared(&temporary);
        std::os::unix::fs::symlink(&installed.0, &bin).unwrap();
        Self {
            root,
            home,
            bin,
            port: free_port(),
            sandbox: false,
            _installed: installed,
        }
    }

    fn command(&self, supervised: bool) -> Command {
        let mut command = Command::new(&self.bin);
        command
            .env_clear()
            .env("HOME", &self.root)
            .env("ALONDRA_HOME", &self.home)
            .env("ALONDRA_LISTEN", "127.0.0.1")
            .env("ALONDRA_PORT", self.port.to_string())
            .env("ALONDRA_SERVICE_LABEL", LABEL)
            .env("ALONDRA_BINARY_CHECK_SECS", BINARY_CHECK.as_secs_f64().to_string())
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if supervised {
            command.env("XPC_SERVICE_NAME", LABEL);
        }
        if self.sandbox {
            command.env("ALONDRA_SANDBOX", "1");
        }
        command
    }

    /// What `/daemon` says when this fixture's daemon answers: the port it picked can be another
    /// process's by the time the daemon binds it.
    fn ours(&self) -> Option<String> {
        self.get("/daemon").filter(|body| {
            serde_json::from_str::<serde_json::Value>(body).is_ok_and(|daemon| daemon["home"] == serde_json::json!(self.home))
        })
    }

    fn health(&self) -> Option<String> {
        self.get("/v1/health")
            .filter(|body| body.contains("version"))
            .or_else(|| self.get("/daemon"))
    }

    fn get(&self, path: &str) -> Option<String> {
        self.exchange(path).map(|(_, body)| body)
    }

    fn exchange(&self, path: &str) -> Option<(String, String)> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .ok()?;
        let mut response = String::new();
        stream.read_to_string(&mut response).ok()?;
        response
            .split_once("\r\n\r\n")
            .map(|(head, body)| (head.to_ascii_lowercase(), body.to_string()))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The daemon binary written once under the test's temporary folder: every fixture links its launch
/// path to it, so the first run of a freshly written executable, the slow one, is paid once for the
/// fixtures alive together, not once per fixture. A test that replaces its binary points its own link
/// at a new file and leaves this one alone. Removed when the last fixture holding it drops.
struct Installed(PathBuf);

static INSTALLED: Mutex<Weak<Installed>> = Mutex::new(Weak::new());

impl Installed {
    fn shared(temporary: &Path) -> Arc<Self> {
        let mut shared = INSTALLED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(installed) = shared.upgrade() {
            return installed;
        }
        let folder = temporary.join(format!("ad-bin-{}", nanos() % 1_000_000_000));
        std::fs::create_dir_all(&folder).unwrap();
        let installed = Arc::new(Self(folder.join("alondrad")));
        write_executable(&installed.0, Path::new(env!("CARGO_BIN_EXE_alondrad")));
        *shared = Arc::downgrade(&installed);
        installed
    }
}

impl Drop for Installed {
    fn drop(&mut self) {
        if let Some(folder) = self.0.parent() {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn write_executable(path: &Path, from: &Path) {
    let tmp = path.with_extension(format!("tmp{}", nanos()));
    std::fs::copy(from, &tmp).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(tmp, path).unwrap();
}

/// Replaces the binary at the launch path `link` the way a package manager does: the new version is
/// a file of its own and the link moves to it.
fn install(link: &Path, from: &Path) {
    let version = link.with_extension(format!("v{}", nanos()));
    write_executable(&version, from);
    point(link, &version);
}

fn point(link: &Path, target: &Path) {
    let tmp = link.with_extension(format!("link{}", nanos()));
    std::os::unix::fs::symlink(target, &tmp).unwrap();
    std::fs::rename(tmp, link).unwrap();
}

fn install_script(path: &Path, script: &str) {
    let source = path.with_extension(format!("src{}", nanos()));
    std::fs::write(&source, script).unwrap();
    install(path, &source);
    std::fs::remove_file(source).unwrap();
}

fn until<T>(within: Duration, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stderr(child: &mut Child) -> String {
    let mut text = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The probe port is released before the daemon binds it, so a concurrent test can take
/// it first. That daemon exits at once with "Address already in use"; try another port.
fn serve(fixture: &mut Fixture, supervised: bool) -> Running {
    for _ in 0..5 {
        let mut daemon = Running(fixture.command(supervised).spawn().unwrap());
        let taken = until(HANG_LIMIT, "the daemon to serve", || {
            if let Some(status) = daemon.0.try_wait().unwrap() {
                let log = stderr(&mut daemon.0);
                assert!(log.contains("Address already in use"), "daemon {} exited {status}: {log}", daemon.0.id());
                return Some(true);
            }
            fixture.ours().map(|_| false)
        });
        if !taken {
            return daemon;
        }
        fixture.port = free_port();
    }
    panic!("no free port after 5 attempts");
}

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn preflight_checks_the_home_without_changing_it() {
    let fixture = Fixture::new("preflight");
    let out = fixture.command(false).arg("--preflight").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("preflight ok"));
    assert_eq!(
        std::fs::read_dir(&fixture.home).unwrap().count(),
        0,
        "preflight wrote into HOME"
    );

    std::fs::write(fixture.home.join("config.toml"), "not valid TOML {{{").unwrap();
    let out = fixture.command(false).arg("--preflight").output().unwrap();
    assert!(!out.status.success());
    let reason = String::from_utf8_lossy(&out.stderr);
    assert_eq!(reason.trim().lines().count(), 1, "{reason}");
    assert!(
        reason.starts_with("preflight failed: config.toml unreadable"),
        "{reason}"
    );
}

#[test]
fn a_launchd_daemon_restarts_into_a_replacement_only_once_it_passes_preflight() {
    let mut fixture = Fixture::new("relaunch");
    let mut daemon = serve(&mut fixture, true);
    assert!(
        !fixture.home.join("startup-failures.json").exists(),
        "a start that serves clears its record"
    );

    install_script(
        &fixture.bin,
        "#!/bin/sh\n[ \"$1\" = --version ] && { echo 'alondrad 99.0.0'; exit 0; }\necho 'preflight failed: bootstrap failed integrity audit: synthetic' >&2\nexit 1\n",
    );
    let health = until(HANG_LIMIT, "the rejection to show", || {
        fixture
            .health()
            .filter(|h| h.contains("bootstrap failed integrity audit: synthetic"))
    });
    assert!(health.contains("99.0.0"), "{health}");
    assert!(
        daemon.0.try_wait().unwrap().is_none(),
        "a binary that fails preflight must not replace the running daemon"
    );

    install(&fixture.bin, Path::new(env!("CARGO_BIN_EXE_alondrad")));
    let give_up = Instant::now() + HANG_LIMIT - Duration::from_secs(10);
    let status = until(
        HANG_LIMIT,
        "the daemon to exit for relaunch",
        || {
            if Instant::now() > give_up {
                let health = fixture.health();
                let _ = daemon.0.kill();
                panic!(
                    "daemon did not relaunch; health {health:?}; log {}",
                    stderr(&mut daemon.0)
                );
            }
            daemon.0.try_wait().unwrap()
        },
    );
    let log = stderr(&mut daemon.0);
    assert_eq!(status.code(), Some(75), "{log}");
    assert!(log.contains("passed preflight"), "{log}");
    assert!(TcpStream::connect(("127.0.0.1", fixture.port)).is_err());
}

/// A package manager can swap the binary while the daemon is still starting: its port already
/// answers but recovery has not finished. The swap still counts as an upgrade.
/// The port it first picks is already another listener's, as one can be on a busy Mac: that daemon
/// cannot bind it, and the test starts one again on another port.
#[test]
fn a_binary_replaced_as_soon_as_the_port_opens_is_still_noticed() {
    let mut fixture = Fixture::new("early");
    let stranger = TcpListener::bind("127.0.0.1:0").unwrap();
    fixture.port = stranger.local_addr().unwrap().port();
    for _ in 0..5 {
        let staged = fixture.bin.with_extension(format!("staged{}", nanos()));
        write_executable(&staged, Path::new(env!("CARGO_BIN_EXE_alondrad")));
        let mut daemon = Running(fixture.command(true).spawn().unwrap());
        let deadline = Instant::now() + HANG_LIMIT;
        let mut exited = None;
        while exited.is_none() && TcpStream::connect(("127.0.0.1", fixture.port)).is_err() {
            exited = daemon.0.try_wait().unwrap();
            assert!(Instant::now() < deadline, "timed out waiting for the port to open");
            std::thread::sleep(Duration::from_millis(2));
        }
        let status = exited.unwrap_or_else(|| {
            point(&fixture.bin, &staged);
            until(HANG_LIMIT, "the daemon to exit for relaunch", || daemon.0.try_wait().unwrap())
        });
        let log = stderr(&mut daemon.0);
        if status.code() == Some(75) {
            assert!(log.contains("passed preflight"), "{log}");
            return;
        }
        assert!(log.contains("Address already in use"), "daemon {} exited {status}: {log}", daemon.0.id());
        fixture.port = free_port();
    }
    panic!("no free port after 5 attempts");
}

#[test]
fn the_daemon_says_which_views_protocols_it_serves() {
    let mut fixture = Fixture::new("protocol");
    let _daemon = serve(&mut fixture, false);
    let body: serde_json::Value = serde_json::from_str(&fixture.get("/daemon").expect("/daemon answers")).unwrap();
    let range = serde_json::json!({"min": alondra_views::VIEWS_PROTOCOL_MIN, "max": alondra_views::VIEWS_PROTOCOL});
    assert_eq!(body["views_protocol"], range);
}

#[test]
fn the_daemon_says_its_home_and_whether_it_is_a_sandbox() {
    for sandbox in [false, true] {
        let mut fixture = Fixture::new(if sandbox { "sandboxed" } else { "plain" });
        fixture.sandbox = sandbox;
        let _daemon = serve(&mut fixture, false);
        let body: serde_json::Value = serde_json::from_str(&fixture.get("/daemon").expect("/daemon answers")).unwrap();
        assert_eq!(body["home"], serde_json::json!(fixture.home), "{body}");
        assert_eq!(body["sandbox"], serde_json::json!(sandbox), "{body}");
    }
}

#[test]
fn every_daemon_answer_says_its_version_even_a_refusal() {
    let mut fixture = Fixture::new("stamp");
    let _daemon = serve(&mut fixture, false);
    let stamp = format!("x-alondra-version: {}", env!("CARGO_PKG_VERSION"));
    for path in ["/daemon", "/views/home?protocol=1&client=tui&lang=en", "/no-such-route"] {
        let (head, _) = fixture.exchange(path).expect("the daemon answers");
        assert!(head.lines().any(|line| line.trim() == stamp), "{path}: {head}");
    }
}

#[test]
fn a_launchd_daemon_waits_for_one_started_by_hand_and_takes_over_when_it_leaves() {
    let mut fixture = Fixture::new("handover");
    let mut by_hand = serve(&mut fixture, false);
    assert!(fixture.get("/daemon").unwrap().contains("\"supervised\":false"));

    let mut launchd = Running(fixture.command(true).spawn().unwrap());
    let mut log = std::io::BufReader::new(launchd.0.stderr.take().unwrap());
    let mut line = String::new();
    while !line.contains("waiting for it to leave") {
        line.clear();
        assert_ne!(log.read_line(&mut line).unwrap(), 0, "the daemon of launchd ended instead of waiting");
    }
    assert!(by_hand.0.try_wait().unwrap().is_none());

    assert_eq!(unsafe { libc::kill(by_hand.0.id() as i32, libc::SIGUSR1) }, 0);
    let status = by_hand.0.wait().unwrap();
    let left = stderr(&mut by_hand.0);
    assert_eq!(status.code(), Some(75), "{left}");
    assert!(!left.contains("stopping the SDK hosts"), "it leaves them for the next daemon: {left}");

    let daemon = until(HANG_LIMIT, "the daemon of launchd to serve", || {
        assert!(launchd.0.try_wait().unwrap().is_none(), "the daemon of launchd must not exit");
        fixture.ours()
    });
    assert!(daemon.contains("\"supervised\":true"), "{daemon}");
}

#[test]
fn a_daemon_started_by_hand_never_relaunches() {
    let mut fixture = Fixture::new("manual");
    let mut daemon = serve(&mut fixture, false);
    let mut log = std::io::BufReader::new(daemon.0.stderr.take().unwrap());
    let mut line = String::new();
    while !line.contains("launchd does not run this daemon") {
        line.clear();
        assert_ne!(log.read_line(&mut line).unwrap(), 0, "the daemon ended before saying launchd does not run it");
    }
    install_script(&fixture.bin, "#!/bin/sh\nexit 0\n");
    std::thread::sleep(BINARY_CHECK * 3);
    assert!(
        daemon.0.try_wait().unwrap().is_none(),
        "only a launchd job restarts itself"
    );
    assert!(fixture.health().is_some());
}

#[test]
fn repeated_failed_starts_stop_hot_looping_until_the_binary_changes() {
    let fixture = Fixture::new("crashloop");
    std::fs::write(fixture.home.join("config.toml"), "not valid TOML {{{").unwrap();
    for attempt in 1..=2 {
        let out = fixture.command(true).output().unwrap();
        assert!(!out.status.success());
        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.home.join("startup-failures.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(record["failures"], attempt);
        assert!(
            record["reason"]
                .as_str()
                .unwrap()
                .contains("config.toml unreadable"),
            "{record}"
        );
    }
    let mut third = Running(fixture.command(true).spawn().unwrap());
    let mut reader = std::io::BufReader::new(third.0.stderr.take().unwrap());
    let mut log = String::new();
    while !log.contains("before exiting so launchd does not restart it") {
        assert_ne!(reader.read_line(&mut log).unwrap(), 0, "the third start ended without saying it waits: {log}");
    }
    until(
        HANG_LIMIT,
        "the third start to record itself",
        || {
            std::fs::read_to_string(fixture.home.join("startup-failures.json"))
                .ok()
                .filter(|t| t.contains("\"failures\":3") && t.contains("config.toml unreadable"))
        },
    );
    assert!(
        third.0.try_wait().unwrap().is_none(),
        "the third failure in a row waits instead of exiting at once"
    );

    install(&fixture.bin, Path::new(env!("CARGO_BIN_EXE_alondrad")));
    let status = until(
        HANG_LIMIT,
        "the waiting start to exit after the binary changed",
        || third.0.try_wait().unwrap(),
    );
    reader.read_to_string(&mut log).unwrap();
    assert!(!status.success());
    assert!(log.contains("3 times in a row"), "{log}");
    assert!(log.contains("the binary was replaced"), "{log}");

    let out = fixture.command(false).output().unwrap();
    assert!(!out.status.success());
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.home.join("startup-failures.json")).unwrap())
            .unwrap();
    assert_eq!(record["failures"], 3, "a start by hand does not count");
}

#[test]
fn a_legacy_only_home_fails_closed_without_creating_domain_database() {
    let fixture = Fixture::new("legacy-only");
    std::fs::write(fixture.home.join("alondra.db"), b"preserve legacy store").unwrap();
    let out = fixture.command(false).output().unwrap();
    assert!(!out.status.success());
    let reason = String::from_utf8_lossy(&out.stderr);
    assert!(reason.contains("legacy data"), "{reason}");
    assert!(!fixture.home.join("domain.db").exists());
    assert_eq!(
        std::fs::read(fixture.home.join("alondra.db")).unwrap(),
        b"preserve legacy store"
    );
}
