//! `alondra update`: install the newest release with Homebrew, then wait until the daemon runs it.
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use alondra_core::update;
use anyhow::{Result, bail};
use serde_json::Value;

use crate::client::Client;

const WAIT: Duration = Duration::from_secs(90);

pub fn run(check: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let exe = std::env::current_exe()?;
    let prefix = update::brew_prefix(&exe);
    let latest = latest_release();
    println!("{}", availability(current, latest.as_deref()));
    let Some(prefix) = prefix else {
        let path = exe.display();
        println!("{}", trf!(
            "{path} is not a Homebrew install: update it from its source checkout (git pull, then make install)",
            "{path} no está instalado con Homebrew: actualízalo desde su código (git pull y luego make install)"));
        return Ok(());
    };
    if check {
        return Ok(());
    }
    let daemon = prefix.join("opt/alondra/bin/alondrad");
    let before = installed_version(&daemon);
    let brew = prefix.join("bin/brew");
    let (shown, formula) = (brew.display(), update::FORMULA);
    println!("{}", trf!("Running {shown} upgrade {formula}", "Ejecutando {shown} upgrade {formula}"));
    let status = Command::new(&brew).args(["upgrade", update::FORMULA]).status()?;
    if !status.success() {
        bail!(trf!("brew upgrade failed ({status})", "brew upgrade falló ({status})"));
    }
    let Some(installed) = installed_version(&daemon) else {
        let shown = daemon.display();
        bail!(trf!("cannot tell which version {shown} is", "no se puede saber qué versión es {shown}"));
    };
    if before.as_deref() == Some(installed.as_str()) {
        println!("{}", trf!("Already up to date: {installed}", "Ya está al día: {installed}"));
    } else {
        println!("{}", trf!("Installed {installed}", "Instalada {installed}"));
    }
    let client = Client::new(Duration::from_secs(3));
    let outcome = wait_for_daemon(&installed, || daemon_info(&client), Instant::now, std::thread::sleep, WAIT, || restart_old_daemon(&daemon));
    println!("{}", describe(&outcome, &installed));
    println!();
    crate::doctor::run(false)?;
    if !matches!(outcome, Waited::Running) {
        std::process::exit(1);
    }
    Ok(())
}

pub fn availability(current: &str, latest: Option<&str>) -> String {
    match latest {
        None => trf!("alondra {current}; the latest release could not be checked", "alondra {current}; no se pudo consultar la última versión"),
        Some(latest) if update::newer(latest, current) => trf!("alondra {current}; {latest} is available", "alondra {current}; hay una versión {latest}"),
        Some(_) => trf!("alondra {current} is up to date", "alondra {current} está al día"),
    }
}

fn latest_release() -> Option<String> {
    let url = std::env::var("ALONDRA_UPDATE_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| update::RELEASES_URL.into());
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).build())
        .timeout_global(Some(Duration::from_secs(15))).build().into();
    let mut response = agent.get(&url).header("user-agent", concat!("alondra/", env!("CARGO_PKG_VERSION")))
        .header("accept", "application/vnd.github+json").call().ok()?;
    let releases: Value = response.body_mut().read_json().ok()?;
    update::latest_release(&releases)
}

fn installed_version(daemon: &Path) -> Option<String> {
    let out = Command::new(daemon).arg("--version").output().ok().filter(|o| o.status.success())?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().last().map(str::to_string)
}

fn daemon_info(client: &Client) -> Option<Value> {
    client.get::<Value>("/v2/daemon").ok()
}

/// A daemon older than self-relaunch never restarts on its own: check the new binary the way
/// the daemon would, then ask launchd to restart the job.
fn restart_old_daemon(daemon: &Path) -> Result<(), String> {
    let out = Command::new(daemon).arg("--preflight").output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        return Err(text.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("preflight failed").to_string());
    }
    let label = alondra_core::config::Config::load().map(|c| c.service_label).unwrap_or_else(|_| alondra_core::config::Config::defaults().service_label);
    let uid = unsafe { libc::getuid() };
    let status = Command::new("launchctl").args(["kickstart", "-k", &format!("gui/{uid}/{label}")]).status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("launchctl kickstart failed ({status})")) }
}

#[derive(Debug, PartialEq)]
pub enum Waited {
    Running,
    Rejected(String),
    Unreachable,
    Stale(String),
    RestartFailed(String),
}

pub fn wait_for_daemon(
    installed: &str,
    mut info: impl FnMut() -> Option<Value>,
    now: impl Fn() -> Instant,
    sleep: impl Fn(Duration),
    within: Duration,
    restart: impl FnOnce() -> Result<(), String>,
) -> Waited {
    let deadline = now() + within;
    let mut restart = Some(restart);
    let mut last = None;
    loop {
        if let Some(info) = info() {
            let version = info["version"].as_str().unwrap_or_default().to_string();
            if version == installed {
                return Waited::Running;
            }
            let rejected = info.get("preflight_rejected").filter(|r| r["version"].as_str() == Some(installed));
            if let Some(rejected) = rejected {
                return Waited::Rejected(rejected["reason"].as_str().unwrap_or_default().to_string());
            }
            if info.get("update").is_none() && let Some(restart) = restart.take() && let Err(e) = restart() {
                return Waited::RestartFailed(e);
            }
            last = Some(version);
        }
        if now() >= deadline {
            return last.map_or(Waited::Unreachable, Waited::Stale);
        }
        sleep(Duration::from_secs(1));
    }
}

pub fn describe(outcome: &Waited, installed: &str) -> String {
    match outcome {
        Waited::Running => trf!("alondrad now runs {installed}", "alondrad ya ejecuta {installed}"),
        Waited::Rejected(reason) => trf!("alondrad kept its version: {installed} failed its startup check: {reason}",
            "alondrad mantiene su versión: {installed} no pasó la comprobación de arranque: {reason}"),
        Waited::RestartFailed(reason) => trf!("alondrad was not restarted: {reason}", "alondrad no se reinició: {reason}"),
        Waited::Unreachable => tr!("alondrad does not answer", "alondrad no responde").into(),
        Waited::Stale(version) => trf!("alondrad still runs {version}", "alondrad sigue ejecutando {version}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    struct Clock(Cell<Duration>, Instant);
    impl Clock {
        fn new() -> Self { Self(Cell::new(Duration::ZERO), Instant::now()) }
        fn now(&self) -> Instant { self.1 + self.0.get() }
        fn sleep(&self, d: Duration) { self.0.set(self.0.get() + d) }
    }

    fn wait(answers: Vec<Option<Value>>, restart: impl FnOnce() -> Result<(), String>) -> (Waited, Duration) {
        let clock = Clock::new();
        let answers = RefCell::new(answers.into_iter());
        let waited = wait_for_daemon("0.2.0", || answers.borrow_mut().next().flatten(), || clock.now(), |d| clock.sleep(d), WAIT, restart);
        (waited, clock.0.get())
    }

    fn never() -> Result<(), String> { panic!("a self-relaunching daemon is not restarted from outside") }

    #[test]
    fn waits_until_the_daemon_reports_the_installed_version() {
        let old = Some(json!({"version": "0.1.0", "update": {}}));
        let (waited, took) = wait(vec![None, old.clone(), old, Some(json!({"version": "0.2.0", "update": {}}))], never);
        assert_eq!(waited, Waited::Running);
        assert_eq!(took, Duration::from_secs(3));
    }

    #[test]
    fn a_rejected_binary_ends_the_wait_with_the_reason() {
        let rejected = json!({"version": "0.1.0", "update": {}, "preflight_rejected": {"version": "0.2.0", "reason": "bootstrap failed integrity audit"}});
        assert_eq!(wait(vec![Some(rejected)], never).0, Waited::Rejected("bootstrap failed integrity audit".into()));
        let older_rejection = json!({"version": "0.1.0", "update": {}, "preflight_rejected": {"version": "0.1.5", "reason": "x"}});
        let (waited, took) = wait(vec![Some(older_rejection); 200], never);
        assert_eq!(waited, Waited::Stale("0.1.0".into()));
        assert_eq!(took, WAIT);
    }

    #[test]
    fn a_daemon_without_self_relaunch_is_restarted_once() {
        let restarts = Cell::new(0);
        let old = Some(json!({"version": "0.1.0"}));
        let (waited, _) = wait(vec![old.clone(), old, Some(json!({"version": "0.2.0", "update": {}}))], || { restarts.set(restarts.get() + 1); Ok(()) });
        assert_eq!(waited, Waited::Running);
        assert_eq!(restarts.get(), 1);
        let (waited, _) = wait(vec![Some(json!({"version": "0.1.0"}))], || Err("preflight failed: audit".into()));
        assert_eq!(waited, Waited::RestartFailed("preflight failed: audit".into()));
    }

    #[test]
    fn a_daemon_that_never_answers_is_reported() {
        assert_eq!(wait(vec![], never), (Waited::Unreachable, WAIT));
    }

    #[test]
    fn availability_says_what_to_do() {
        assert!(availability("0.1.12", Some("0.1.13")).contains("0.1.13"));
        assert!(!availability("0.1.13", Some("0.1.13")).contains("0.1.12"));
        assert!(availability("0.1.13", None).contains("0.1.13"));
    }

    #[test]
    fn the_daemon_is_asked_on_v2_only() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
        let server = std::thread::spawn(move || {
            let mut requested = vec![];
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut bytes = [0; 4096];
                let n = stream.read(&mut bytes).unwrap();
                if n == 0 {
                    break;
                }
                let request = String::from_utf8_lossy(&bytes[..n]).into_owned();
                requested.push(request.split_whitespace().nth(1).unwrap().to_owned());
                let body = br#"{"version": "0.2.0"}"#;
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(body).unwrap();
                break;
            }
            requested
        });
        assert_eq!(daemon_info(&client), Some(json!({"version": "0.2.0"})));
        assert_eq!(server.join().unwrap(), ["/v2/daemon"]);
    }
}
