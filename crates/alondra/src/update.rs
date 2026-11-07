//! `alondra update`: install the newest release with Homebrew, then wait until the daemon runs it.
mod brew;

use std::io::{BufRead, BufReader, IsTerminal};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use alondra_core::update;
use anyhow::Result;
use serde_json::Value;

use crate::client::Client;
use crate::doctor::Status;

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
    let mut out = Steps::new();
    let daemon = prefix.join("opt/alondra/bin/alondrad");
    let before = installed_version(&daemon);
    let log = alondra_core::config::alondra_home(&|k| std::env::var(k).ok()).join(LOG);
    if let Err(lines) = upgrade(&prefix.join("bin/brew"), &log, &mut out) {
        out.lines(&lines);
        std::process::exit(1);
    }
    let Some(installed) = installed_version(&daemon) else {
        let shown = daemon.display();
        out.fail(&trf!("cannot tell which version {shown} is", "no se puede saber qué versión es {shown}"));
        std::process::exit(1);
    };
    if before.as_deref() == Some(installed.as_str()) {
        out.pass(&trf!("Already up to date: alondra {installed}", "Ya está al día: alondra {installed}"));
    } else {
        out.pass(&trf!("Installed alondra {installed}", "Instalada alondra {installed}"));
    }
    out.start(tr!("Bringing the service up to date", "Poniendo al día el servicio"));
    match apply_service(&prefix) {
        Ok(()) => out.pass(tr!("Service up to date", "Servicio al día")),
        Err(lines) => out.lines(&lines),
    }
    out.start(&trf!("Waiting for alondrad to run {installed}", "Esperando a que alondrad ejecute {installed}"));
    let client = Client::new(Duration::from_secs(3));
    let outcome = wait_for_daemon(&installed, || daemon_info(&client), Instant::now, std::thread::sleep, WAIT, || restart_old_daemon(&daemon));
    match outcome {
        Waited::Running => out.pass(&describe(&outcome, &installed)),
        _ => out.fail(&describe(&outcome, &installed)),
    }
    let code = after_update(&prefix)?;
    match (code, outcome) {
        (0, Waited::Running) => Ok(()),
        (0, _) => std::process::exit(1),
        (code, _) => std::process::exit(code),
    }
}

/// Homebrew's whole output of the last `alondra update`, in Alondra's home.
const LOG: &str = "update.log";

/// Runs `brew upgrade alondra` with its output captured: the steps it goes through, then
/// everything it said in `log`. On failure, the lines that say why.
fn upgrade(brew_path: &Path, log: &Path, out: &mut Steps) -> Result<(), Vec<String>> {
    let mut progress = brew::Progress::default();
    out.start(&progress.running(brew::Stage::Starting));
    let (reader, writer) = match std::io::pipe() {
        Ok(pipe) => pipe,
        Err(error) => return Err(failure(&progress.failed(), &[error.to_string()], "")),
    };
    let mut command = Command::new(brew_path);
    command.args(["upgrade", update::FORMULA]).env("HOMEBREW_NO_COLOR", "1").env("HOMEBREW_NO_ENV_HINTS", "1");
    let spawned = writer.try_clone().and_then(|stderr| command.stdout(writer).stderr(stderr).spawn());
    drop(command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => return Err(failure(&progress.failed(), &[error.to_string()], &brew_path.display().to_string())),
    };
    let mut reader = BufReader::new(reader);
    let mut bytes = Vec::new();
    let mut output = String::new();
    while reader.read_until(b'\n', &mut bytes).is_ok_and(|n| n > 0) {
        let line = String::from_utf8_lossy(&bytes).trim_end().to_string();
        bytes.clear();
        let previous = progress.stage();
        if let Some(stage) = progress.feed(&line) {
            if let Some(done) = progress.done(previous) {
                out.pass(&done);
            }
            out.start(&progress.running(stage));
        }
        output.push_str(&line);
        output.push('\n');
    }
    let status = child.wait().map_err(|e| failure(&progress.failed(), &[e.to_string()], ""))?;
    let saved = log.parent().is_some_and(|dir| std::fs::create_dir_all(dir).is_ok()) && std::fs::write(log, &output).is_ok();
    if status.success() {
        if let Some(done) = progress.done(progress.stage()) {
            out.pass(&done);
        }
        return Ok(());
    }
    let last = if saved {
        let shown = log.display();
        trf!("Homebrew's whole output: {shown}", "Toda la salida de Homebrew: {shown}")
    } else {
        trf!("brew upgrade ended with {status}", "brew upgrade terminó con {status}")
    };
    Err(failure(&progress.failed(), &brew::failure_tail(&output, TAIL), &last))
}

/// How many lines of a failed command's output `alondra update` shows.
const TAIL: usize = 8;

pub fn step(status: Status, text: &str) -> String {
    format!("  {} {text}", crate::doctor::mark(status))
}

/// A failed step, the output that says why and, last, where to look or what to run.
pub fn failure(title: &str, why: &[String], last: &str) -> Vec<String> {
    let mut lines = vec![step(Status::Fail, title)];
    lines.extend(why.iter().chain([last.to_string()].iter()).filter(|l| !l.is_empty()).map(|l| format!("    {l}")));
    lines
}

/// The steps `alondra update` shows: at a terminal a spinner on the one running, else only the
/// lines of the steps that ended.
struct Steps {
    spinner: Option<cliclack::ProgressBar>,
    terminal: bool,
}

impl Steps {
    fn new() -> Self {
        Self { spinner: None, terminal: std::io::stdout().is_terminal() && std::io::stderr().is_terminal() }
    }

    fn start(&mut self, text: &str) {
        self.clear();
        if self.terminal {
            let spinner = cliclack::spinner();
            spinner.start(text);
            self.spinner = Some(spinner);
        }
    }

    fn clear(&mut self) {
        if let Some(spinner) = self.spinner.take() {
            spinner.clear();
        }
    }

    fn pass(&mut self, text: &str) {
        self.lines(&[step(Status::Pass, text)]);
    }

    fn fail(&mut self, text: &str) {
        self.lines(&[step(Status::Fail, text)]);
    }

    fn lines(&mut self, lines: &[String]) {
        self.clear();
        for line in lines {
            println!("{line}");
        }
    }
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

pub(crate) fn installed_version(daemon: &Path) -> Option<String> {
    let out = Command::new(daemon).arg("--version").output().ok().filter(|o| o.status.success())?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().last().map(str::to_string)
}

/// The service's own repair, run by the CLI that was just installed: the release may launch the
/// daemon differently (through the launcher that keeps the Local Network permission), and only
/// the new CLI knows how.
fn service_repair(prefix: &Path) -> (std::path::PathBuf, [&'static str; 5]) {
    (prefix.join("opt/alondra/bin/alondra"), ["doctor", "--fix", "--yes", "--only", "service"])
}

fn after_update(prefix: &Path) -> Result<i32> {
    let cli = prefix.join("opt/alondra/bin/alondra");
    let status = Command::new(&cli).args(["doctor", "--after-update"]).status()
        .map_err(|error| anyhow::anyhow!("{}: {error}", cli.display()))?;
    status.code().ok_or_else(|| anyhow::anyhow!("{} doctor --after-update: {status}", cli.display()))
}

fn apply_service(prefix: &Path) -> Result<(), Vec<String>> {
    let (cli, args) = service_repair(prefix);
    let title = tr!("The service could not be brought up to date", "No se pudo poner al día el servicio");
    let fix = "→ alondra doctor --fix";
    match Command::new(&cli).args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            Err(failure(title, &brew::failure_tail(&text, TAIL), fix))
        }
        Err(error) => Err(failure(title, &[error.to_string()], fix)),
    }
}

fn daemon_info(client: &Client) -> Option<Value> {
    client.get::<Value>("/daemon").ok()
}

/// Runs the binary's own startup check on this HOME; the error is the last line it printed.
pub(crate) fn preflight(daemon: &Path) -> Result<(), String> {
    let out = Command::new(daemon).arg("--preflight").output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stderr);
    Err(text.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("preflight failed").to_string())
}

/// A daemon older than self-relaunch never restarts on its own, and one that rejected the new
/// binary does not try it again for a while: check the new binary the way the daemon would,
/// so a rejection whose cause is gone does not stand, then ask launchd to restart the job.
fn restart_old_daemon(daemon: &Path) -> Result<(), String> {
    preflight(daemon)?;
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
    OutsideLaunchd(String),
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
            if info["supervised"] == false {
                return Waited::OutsideLaunchd(version);
            }
            let rejected = info.get("preflight_rejected").is_some_and(|r| r["version"].as_str() == Some(installed));
            if rejected && let Some(restart) = restart.take() {
                if let Err(reason) = restart() {
                    return Waited::Rejected(reason);
                }
            } else if info.get("update").is_none() && let Some(restart) = restart.take() && let Err(e) = restart() {
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
        Waited::OutsideLaunchd(version) => trf!(
            "alondrad still runs {version}: it was started by hand, so it cannot restart into {installed}. Run `alondra doctor --fix`",
            "alondrad sigue ejecutando {version}: se arrancó a mano, así que no puede reiniciarse en {installed}. Ejecuta `alondra doctor --fix`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    #[test]
    fn a_daemon_outside_launchd_is_said_at_once_instead_of_waited_for() {
        let info = || Some(json!({"version": "0.1.0", "update": {}, "supervised": false}));
        let clock = Instant::now();
        let waited = wait_for_daemon("0.2.0", info, || clock, |_| panic!("no restart will come"), Duration::from_secs(90), || panic!("launchd holds no job to restart"));
        assert_eq!(waited, Waited::OutsideLaunchd("0.1.0".into()));
        assert!(describe(&waited, "0.2.0").contains("alondra doctor --fix"));
    }

    #[test]
    fn the_service_is_repaired_by_the_cli_just_installed_and_nothing_else() {
        let (cli, args) = service_repair(Path::new("/opt/homebrew"));
        assert_eq!(cli, Path::new("/opt/homebrew/opt/alondra/bin/alondra"));
        assert_eq!(args, ["doctor", "--fix", "--yes", "--only", "service"]);
    }

    #[test]
    fn the_final_report_runs_the_installed_cli_and_preserves_its_exit_code() {
        use std::os::unix::fs::PermissionsExt;
        let prefix = crate::test_support::Directory::new();
        let cli = prefix.join("opt/alondra/bin/alondra");
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, "#!/bin/sh\n[ \"$#\" = 2 ] && [ \"$1\" = doctor ] && [ \"$2\" = --after-update ] || exit 99\nexit 7\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(after_update(&prefix).unwrap(), 7);
        std::fs::write(&cli, "#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(after_update(&prefix).unwrap(), 0);
        std::fs::write(&cli, "#!/bin/sh\nkill -TERM $$\n").unwrap();
        assert!(after_update(&prefix).unwrap_err().to_string().contains("doctor --after-update"));
        std::fs::remove_file(&cli).unwrap();
        assert!(after_update(&prefix).unwrap_err().to_string().contains("opt/alondra/bin/alondra"));
    }

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
        let rejected = json!({"version": "0.1.0", "update": {}, "preflight_rejected": {"version": "0.2.0", "reason": "stale reason"}});
        assert_eq!(wait(vec![Some(rejected)], || Err("bootstrap failed integrity audit".into())).0, Waited::Rejected("bootstrap failed integrity audit".into()));
        let older_rejection = json!({"version": "0.1.0", "update": {}, "preflight_rejected": {"version": "0.1.5", "reason": "x"}});
        let (waited, took) = wait(vec![Some(older_rejection); 200], never);
        assert_eq!(waited, Waited::Stale("0.1.0".into()));
        assert_eq!(took, WAIT);
    }

    #[test]
    fn a_remembered_rejection_is_checked_again_before_it_is_reported() {
        let rejected = Some(json!({"version": "0.1.0", "update": {}, "preflight_rejected": {"version": "0.2.0", "reason": "this HOME still holds legacy data"}}));
        let rechecks = Cell::new(0);
        let (waited, _) = wait(vec![rejected.clone(), rejected, Some(json!({"version": "0.2.0", "update": {}}))], || { rechecks.set(rechecks.get() + 1); Ok(()) });
        assert_eq!(waited, Waited::Running, "a rejection the daemon remembers from before is not the final word");
        assert_eq!(rechecks.get(), 1);
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
    fn a_failed_step_shows_why_and_then_where_to_look() {
        assert_eq!(step(Status::Pass, "Installed alondra 0.3.6"), "  ✓ Installed alondra 0.3.6");
        let why = ["Error: alondra: Failed to download resource".to_string()];
        assert_eq!(failure("Could not download alondra 0.3.6", &why, "Homebrew's whole output: /h/update.log"), [
            "  ✗ Could not download alondra 0.3.6",
            "    Error: alondra: Failed to download resource",
            "    Homebrew's whole output: /h/update.log",
        ]);
        assert_eq!(failure("Homebrew could not upgrade alondra", &[], ""), ["  ✗ Homebrew could not upgrade alondra"]);
    }

    #[test]
    fn homebrew_runs_with_its_output_captured_and_kept_in_the_log() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*dir).unwrap();
        let brew_path = dir.join("brew");
        std::fs::write(&brew_path, "#!/bin/sh\necho '==> Downloading https://example/alondra.tar.gz'\necho 'Error: alondra: Failed to download resource' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&brew_path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let log = dir.join("home/update.log");
        let lines = upgrade(&brew_path, &log, &mut Steps { spinner: None, terminal: false }).unwrap_err();
        let shown = log.display();
        assert_eq!(lines, [
            "  ✗ Could not download alondra".to_string(),
            "    Error: alondra: Failed to download resource".to_string(),
            format!("    Homebrew's whole output: {shown}"),
        ]);
        let saved = std::fs::read_to_string(&log).unwrap();
        assert!(saved.contains("==> Downloading https://example/alondra.tar.gz") && saved.contains("Error: alondra"), "{saved}");
    }

    #[test]
    fn homebrew_output_is_kept_in_the_order_it_was_written() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*dir).unwrap();
        let brew_path = dir.join("brew");
        std::fs::write(&brew_path, "#!/bin/sh\ni=0\nwhile [ $i -lt 200 ]; do echo \"out $i\"; echo \"err $i\" >&2; i=$((i+1)); done\nexit 1\n").unwrap();
        std::fs::set_permissions(&brew_path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let log = dir.join("home/update.log");
        upgrade(&brew_path, &log, &mut Steps { spinner: None, terminal: false }).unwrap_err();
        let written: String = (0..200).map(|i| format!("out {i}\nerr {i}\n")).collect();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), written);
    }

    #[test]
    fn availability_says_what_to_do() {
        assert!(availability("0.1.12", Some("0.1.13")).contains("0.1.13"));
        assert!(!availability("0.1.13", Some("0.1.13")).contains("0.1.12"));
        assert!(availability("0.1.13", None).contains("0.1.13"));
    }

    #[test]
    fn the_daemon_is_asked_at_the_root_resource_route() {
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
        assert_eq!(server.join().unwrap(), ["/daemon"]);
    }
}
