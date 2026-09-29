//! The launchd upgrade path, end to end with the real binary: preflight, self-relaunch after
//! the binary at the launch path is replaced, and the crash-loop guard.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const LABEL: &str = "test.alondra.relaunch";

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    port: u16,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = PathBuf::from(format!("/tmp/ad-{name}-{}", nanos() % 1_000_000_000));
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let bin = root.join("bin/alondrad");
        install(&bin, Path::new(env!("CARGO_BIN_EXE_alondrad")));
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Self {
            root,
            home,
            bin,
            port,
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
            .env("ALONDRA_BINARY_CHECK_SECS", "0.2")
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if supervised {
            command.env("XPC_SERVICE_NAME", LABEL);
        }
        command
    }

    fn health(&self) -> Option<String> {
        self.get("/v1/health")
            .filter(|body| body.contains("version"))
            .or_else(|| self.get("/daemon"))
    }

    fn get(&self, path: &str) -> Option<String> {
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
            .map(|(_, body)| body.to_string())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// Replaces `path` the way a package manager does: a new file moved over the old name.
fn install(path: &Path, from: &Path) {
    let tmp = path.with_extension(format!("tmp{}", nanos()));
    std::fs::copy(from, &tmp).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(tmp, path).unwrap();
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

fn serving(fixture: &Fixture, daemon: &mut Running) {
    until(Duration::from_secs(30), "the daemon to serve", || {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            panic!("daemon exited {status}: {}", stderr(&mut daemon.0));
        }
        fixture.health()
    });
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
    let fixture = Fixture::new("relaunch");
    let mut daemon = Running(fixture.command(true).spawn().unwrap());
    serving(&fixture, &mut daemon);
    assert!(
        !fixture.home.join("startup-failures.json").exists(),
        "a start that serves clears its record"
    );

    install_script(
        &fixture.bin,
        "#!/bin/sh\n[ \"$1\" = --version ] && { echo 'alondrad 99.0.0'; exit 0; }\necho 'preflight failed: bootstrap failed integrity audit: synthetic' >&2\nexit 1\n",
    );
    let health = until(Duration::from_secs(30), "the rejection to show", || {
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
    let give_up = Instant::now() + Duration::from_secs(50);
    let status = until(
        Duration::from_secs(60),
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

#[test]
fn a_daemon_started_by_hand_never_relaunches() {
    let fixture = Fixture::new("manual");
    let mut daemon = Running(fixture.command(false).spawn().unwrap());
    serving(&fixture, &mut daemon);
    install_script(&fixture.bin, "#!/bin/sh\nexit 0\n");
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "only a launchd job restarts itself"
        );
    }
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
    until(
        Duration::from_secs(10),
        "the third start to record itself",
        || {
            std::fs::read_to_string(fixture.home.join("startup-failures.json"))
                .ok()
                .filter(|t| t.contains("\"failures\":3") && t.contains("config.toml unreadable"))
        },
    );
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        third.0.try_wait().unwrap().is_none(),
        "the third failure in a row waits instead of exiting at once"
    );

    install(&fixture.bin, Path::new(env!("CARGO_BIN_EXE_alondrad")));
    let status = until(
        Duration::from_secs(15),
        "the waiting start to exit after the binary changed",
        || third.0.try_wait().unwrap(),
    );
    let log = stderr(&mut third.0);
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
