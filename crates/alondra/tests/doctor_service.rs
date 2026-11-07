//! `alondra doctor --fix` on the service, end to end: the real CLI against a `launchctl` and an
//! `sfltool` that are scripts on the PATH, and a fake daemon that holds the HOME lock, answers
//! `/daemon` and, on SIGUSR1, exits with 75 after starting the successor launchd would start.
//! One test per case; each Mac owns its processes and its folder, removed on success and failure.

#[path = "../../test_support.rs"]
mod test_support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const LABEL: &str = "dev.alondra.alondrad";

const LAUNCHCTL: &str = r#"
echo "$*" >> "$STATE/launchctl.log"
case "$1" in
  print) [ -f "$STATE/loaded" ] ;;
  print-disabled)
    state=enabled
    [ -f "$STATE/disabled" ] && state=disabled
    printf 'disabled services = {\n\t"%s" => %s\n}\n' "$LABEL" "$state" ;;
  enable) rm -f "$STATE/disabled" ;;
  bootstrap)
    if [ -f "$STATE/disabled" ]; then echo "Bootstrap failed: 5: Input/output error" >&2; exit 5; fi
    touch "$STATE/loaded"
    "$FAKE_DAEMON" $(cat "$STATE/launch.args") </dev/null >/dev/null 2>&1 & ;;
  bootout) rm -f "$STATE/loaded" ;;
  *) ;;
esac
"#;

const SFLTOOL: &str = r#"
echo "$*" >> "$STATE/sfltool.log"
[ -f "$STATE/btm.hang" ] && exec sleep 600
cat "$STATE/btm.txt" 2>/dev/null
exit 0
"#;

const ALONDRAD: &str = r#"
case "$1" in
  --version) echo "alondrad $(cat "$STATE/installed")" ;;
  --preflight)
    if [ -f "$STATE/preflight_fails" ]; then echo "preflight failed: $(cat "$STATE/preflight_fails")" >&2; exit 1; fi
    echo "preflight ok: fake" ;;
esac
"#;

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Mac {
    root: test_support::Directory,
    port: u16,
    children: std::cell::RefCell<Vec<Child>>,
}

impl Mac {
    fn new() -> Self {
        let root = test_support::Directory::new();
        for folder in ["home", "bin", "tools", "state/pids", "Library/LaunchAgents"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        script(&root.join("bin/alondrad"), ALONDRAD);
        script(&root.join("bin/alondra"), "exit 0");
        script(&root.join("tools/launchctl"), LAUNCHCTL);
        script(&root.join("tools/sfltool"), SFLTOOL);
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mac = Self { root, port, children: Default::default() };
        mac.set_installed("0.3.20");
        mac
    }

    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    fn lock(&self) -> PathBuf {
        self.root.join("home/service-home.lock")
    }

    fn set_installed(&self, version: &str) {
        std::fs::write(self.state().join("installed"), version).unwrap();
    }

    /// What launchd starts when the job is loaded.
    fn launches(&self, version: &str, extra: &[&str]) {
        let mut args = vec![
            "--state".to_string(), self.state().display().to_string(), "--port".into(), self.port.to_string(),
            "--version".into(), version.into(), "--lock".into(), self.lock().display().to_string(), "--wait-lock".into(),
        ];
        args.extend(extra.iter().map(|a| a.to_string()));
        std::fs::write(self.state().join("launch.args"), args.join(" ")).unwrap();
    }

    fn plist(&self) -> PathBuf {
        self.root.join(format!("Library/LaunchAgents/{LABEL}.plist"))
    }

    fn command(&self, binary: &str, args: &[&str]) -> Command {
        let mut command = Command::new(std::fs::canonicalize(binary).unwrap());
        command.args(args).current_dir(&*self.root).env_clear()
            .env("HOME", &*self.root)
            .env("ALONDRA_HOME", self.root.join("home"))
            .env("ALONDRA_PORT", self.port.to_string())
            .env("ALONDRA_BIN", self.root.join("bin"))
            .env("ALONDRA_LANG", "en")
            .env("ALONDRA_SERVICE_WAIT_SECS", "6")
            .env("FORCE_DEV_INSTALL", "1")
            .env("STATE", self.state())
            .env("LABEL", LABEL)
            .env("FAKE_DAEMON", std::fs::canonicalize(env!("CARGO_BIN_EXE_fake_daemon")).unwrap())
            .env("PATH", format!("{}:/usr/bin:/bin", self.root.join("tools").display()));
        command
    }

    fn doctor(&self, args: &[&str]) -> (Output, Value) {
        let mut all = vec!["doctor", "--only", "service,alondrad,login-item", "--json"];
        all.extend(args);
        let output = self.command(env!("CARGO_BIN_EXE_alondra"), &all).output().unwrap();
        let doc = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {output:?}"));
        (output, doc)
    }

    /// The service as `doctor --fix` installs it: its LaunchAgent written and the job loaded, the
    /// daemon launchd starts running `version`.
    fn installed_service(&self, version: &str, extra: &[&str]) {
        self.set_installed(version);
        self.launches(version, extra);
        let (output, doc) = self.doctor(&["--fix", "--yes"]);
        assert_eq!(check(&doc, "service")["status"], "pass", "{doc}\n{output:?}");
        assert_eq!(self.daemon_version().as_deref(), Some(version));
        std::fs::write(self.state().join("launchctl.log"), "").unwrap();
    }

    fn launchctl_log(&self) -> String {
        std::fs::read_to_string(self.state().join("launchctl.log")).unwrap_or_default()
    }

    fn daemon_version(&self) -> Option<String> {
        let mut stream = TcpStream::connect_timeout(&([127, 0, 0, 1], self.port).into(), Duration::from_secs(1)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        stream.write_all(b"GET /daemon HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        let body: Value = serde_json::from_str(reply.split_once("\r\n\r\n")?.1).ok()?;
        body["version"].as_str().map(str::to_string)
    }

    /// A daemon started by the test, not by the fake launchd.
    fn daemon(&self, version: &str, extra: &[&str]) -> u32 {
        let child = Command::new(std::fs::canonicalize(env!("CARGO_BIN_EXE_fake_daemon")).unwrap())
            .args(["--state", &self.state().display().to_string(), "--port", &self.port.to_string(), "--version", version, "--lock", &self.lock().display().to_string()])
            .args(extra).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn().unwrap();
        let pid = child.id();
        self.children.borrow_mut().push(child);
        let started = Instant::now();
        while !self.state().join("pids").join(pid.to_string()).exists() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        while !self.holds_home() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        pid
    }

    fn holds_home(&self) -> bool {
        use std::os::fd::AsRawFd;
        let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(self.lock()) else { return false };
        let free = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        !free
    }

    /// A child the daemon ended stays a zombie until it is waited for, which `try_wait` does.
    fn alive(&self, pid: u32) -> bool {
        match self.children.borrow_mut().iter_mut().find(|child| child.id() == pid) {
            Some(child) => child.try_wait().ok().flatten().is_none(),
            None => unsafe { libc::kill(pid as i32, 0) == 0 },
        }
    }

    fn pids(&self) -> Vec<i32> {
        std::fs::read_dir(self.state().join("pids")).into_iter().flatten().flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok()).collect()
    }
}

impl Drop for Mac {
    fn drop(&mut self) {
        let _ = std::fs::write(self.state().join("stop"), "");
        for pid in self.pids() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        for child in self.children.borrow_mut().iter_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let started = Instant::now();
        while self.pids().iter().any(|pid| unsafe { libc::kill(*pid, 0) == 0 }) && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn check<'a>(doc: &'a Value, id: &str) -> &'a Value {
    doc["checks"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("no {id}: {doc}"))
}

fn absent(doc: &Value, id: &str) -> bool {
    !doc["checks"].as_array().unwrap().iter().any(|c| c["id"] == id)
}

fn with_a_service_file(mac: &Mac) {
    std::fs::write(mac.plist(), "<plist/>").unwrap();
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_service_behind_the_installed_binary_restarts_into_it_with_sigusr1_and_never_with_kickstart() {
    let mac = Mac::new();
    mac.installed_service("0.3.19", &["--after-usr1", "0.3.20"]);
    mac.set_installed("0.3.20");
    let (_, before) = mac.doctor(&[]);
    assert_eq!(check(&before, "service")["status"], "warn", "{before}");
    assert!(check(&before, "service")["detail"].as_str().unwrap().contains("runs 0.3.19, but 0.3.20 is installed"), "{before}");
    assert_eq!(check(&before, "service")["fixable"], true);

    let (output, after) = mac.doctor(&["--fix", "--yes"]);
    assert_eq!(check(&after, "service")["status"], "pass", "{after}\n{}", stderr(&output));
    assert_eq!(mac.daemon_version().as_deref(), Some("0.3.20"));
    let log = mac.launchctl_log();
    assert!(!log.contains("kickstart") && !log.contains("bootout"), "SIGTERM would stop the SDK hosts: {log}");
}

#[test]
fn a_binary_that_fails_its_startup_check_is_not_run_and_the_reason_is_said() {
    let mac = Mac::new();
    mac.installed_service("0.3.19", &["--after-usr1", "0.3.20"]);
    mac.set_installed("0.3.20");
    std::fs::write(mac.state().join("preflight_fails"), "cannot open the database").unwrap();

    let (output, after) = mac.doctor(&["--fix", "--yes"]);
    assert!(stderr(&output).contains("failed its startup check: preflight failed: cannot open the database"), "{}", stderr(&output));
    assert_eq!(check(&after, "service")["status"], "warn", "{after}");
    assert_eq!(mac.daemon_version().as_deref(), Some("0.3.19"), "the running daemon was left alone");
}

#[test]
fn fix_fails_when_the_restarted_service_answers_with_another_version() {
    let mac = Mac::new();
    mac.installed_service("0.3.19", &["--after-usr1", "0.3.19"]);
    mac.set_installed("0.3.20");

    let (output, after) = mac.doctor(&["--fix", "--yes"]);
    assert!(stderr(&output).contains("alondrad answers with 0.3.19, not the installed 0.3.20"), "{}", stderr(&output));
    assert_eq!(check(&after, "service")["status"], "warn", "{after}");
}

#[test]
fn fix_fails_when_the_restarted_service_is_not_run_by_launchd() {
    let mac = Mac::new();
    mac.installed_service("0.3.19", &["--after-usr1", "0.3.20", "--successor-unsupervised"]);
    mac.set_installed("0.3.20");

    let (output, _) = mac.doctor(&["--fix", "--yes"]);
    assert!(stderr(&output).contains("alondrad answers, but launchd does not run it"), "{}", stderr(&output));
}

#[test]
fn a_disabled_service_under_a_running_daemon_is_enabled_and_nothing_restarts() {
    let mac = Mac::new();
    mac.installed_service("0.3.20", &[]);
    std::fs::write(mac.state().join("disabled"), "").unwrap();
    let (_, before) = mac.doctor(&[]);
    assert_eq!(check(&before, "service")["status"], "warn", "{before}");
    assert!(check(&before, "service")["detail"].as_str().unwrap().contains("will not start again after this Mac restarts"), "{before}");

    let (output, after) = mac.doctor(&["--fix", "--yes"]);
    assert_eq!(check(&after, "service")["status"], "pass", "{after}\n{}", stderr(&output));
    assert_eq!(mac.launchctl_log().lines().filter(|line| !line.starts_with("print")).collect::<Vec<_>>(), vec![format!("enable gui/{}/{LABEL}", unsafe { libc::getuid() })]);
}

fn btm(identifier: &str, disposition: &str) -> String {
    format!("========================\n Records for UID 501\n  #1:\n   UUID: AAAA\n   Disposition: {disposition}\n   Identifier: {identifier}\n")
}

#[test]
fn a_login_item_macos_has_off_is_named_with_the_way_back_on() {
    let mac = Mac::new();
    with_a_service_file(&mac);
    std::fs::write(mac.state().join("btm.txt"), btm(&format!("8.{LABEL}"), "[disabled, allowed, visible, notified] (0xe)")).unwrap();
    let (_, doc) = mac.doctor(&[]);
    let item = check(&doc, "login-item");
    assert_eq!(item["status"], "warn", "{doc}");
    assert!(item["detail"].as_str().unwrap().contains("turned off in System Settings > General > Login Items & Extensions"), "{doc}");
    assert!(item["fix"].as_str().unwrap().contains("Allow in the Background"), "{doc}");
    assert_eq!(item["fixable"], false);
}

#[test]
fn a_login_item_macos_has_on_says_nothing() {
    let mac = Mac::new();
    with_a_service_file(&mac);
    std::fs::write(mac.state().join("btm.txt"), btm(&format!("8.{LABEL}"), "[enabled, allowed, visible, notified] (0xf)")).unwrap();
    let (_, doc) = mac.doctor(&[]);
    assert!(absent(&doc, "login-item"), "{doc}");
}

#[test]
fn a_dump_that_never_finishes_says_nothing_and_does_not_hold_doctor_up() {
    let mac = Mac::new();
    with_a_service_file(&mac);
    std::fs::write(mac.state().join("btm.hang"), "").unwrap();
    let (output, doc) = mac.doctor(&[]);
    assert!(output.status.success() || output.status.code().is_some(), "{output:?}");
    assert!(absent(&doc, "login-item"), "{doc}");
    assert!(mac.state().join("sfltool.log").exists(), "sfltool was asked");
}

#[test]
fn a_daemon_that_holds_the_home_without_answering_and_has_no_job_is_reclaimed_after_a_yes() {
    let mac = Mac::new();
    with_a_service_file(&mac);
    mac.set_installed("0.3.20");
    mac.launches("0.3.20", &[]);
    let holder = mac.daemon("0.3.19", &["--silent"]);
    let (_, before) = mac.doctor(&[]);
    assert_eq!(check(&before, "service")["status"], "warn", "{before}");
    assert!(check(&before, "service")["detail"].as_str().unwrap().contains("holds this HOME but does not answer"), "{before}");

    let (_, declined) = mac.doctor(&["--fix"]);
    assert_eq!(check(&declined, "service")["status"], "warn", "{declined}");
    assert!(mac.alive(holder), "nothing is ended without a yes");

    let (output, after) = mac.doctor(&["--fix", "--yes"]);
    assert_eq!(check(&after, "service")["status"], "pass", "{after}\n{}", stderr(&output));
    assert!(!mac.alive(holder));
    assert_eq!(mac.daemon_version().as_deref(), Some("0.3.20"));
    let log = mac.launchctl_log();
    assert!(log.contains("bootstrap") && !log.contains("kickstart"), "kickstart fails without a loaded job: {log}");
}

#[test]
fn a_holder_that_ignores_sigusr1_is_ended_so_the_service_takes_the_home() {
    let mac = Mac::new();
    with_a_service_file(&mac);
    mac.set_installed("0.3.20");
    mac.launches("0.3.20", &[]);
    let holder = mac.daemon("0.3.19", &["--silent", "--ignore-usr1"]);

    let (output, after) = mac.doctor(&["--fix", "--yes"]);
    assert_eq!(check(&after, "service")["status"], "pass", "{after}\n{}", stderr(&output));
    assert!(!mac.alive(holder));
    assert_eq!(mac.daemon_version().as_deref(), Some("0.3.20"));
}
