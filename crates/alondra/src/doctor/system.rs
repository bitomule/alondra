use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use alondra_core::config::{self, Config};
use alondra_core::startup::Record;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Action;
use crate::client::Client;
use crate::setup;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MacInfo {
    pub os: String,
    pub arch: String,
    pub install: String,
    pub dev_over_brew: bool,
}

/// Whether this HOME's daemon runs, read from `/daemon` and the HOME lock, never from launchctl.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DaemonState {
    Answers,
    /// It holds the HOME lock but `/daemon` does not answer.
    Silent,
    #[default]
    Stopped,
}

/// The service as launchd has it, and what `doctor --fix` would change in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServiceView {
    pub label: String,
    /// Its LaunchAgent is on disk.
    pub installed: bool,
    pub daemon: DaemonState,
    pub pending: bool,
    pub others: Vec<PathBuf>,
    pub binaries: bool,
    pub bin: PathBuf,
    pub dev_over_brew: bool,
    pub repo: Option<PathBuf>,
    pub marketplace_elsewhere: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginPass {
    pub fingerprint: String,
    pub at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TestPush {
    pub device: String,
    pub name: String,
    pub at: i64,
}

/// What doctor remembers between runs, in `doctor.json`: when the slow check of the node plugin last passed,
/// and the last test notification a device accepted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    #[serde(default, rename = "mods")]
    pub plugin: Option<PluginPass>,
    #[serde(default)]
    pub test_push: Option<TestPush>,
}

/// Everything a check reads or does outside this process, so each check runs against a double.
pub trait System {
    fn home(&self) -> PathBuf;
    fn user_home(&self) -> PathBuf;
    fn config_path(&self) -> PathBuf;
    fn config_text(&self) -> Option<String>;
    fn config(&self) -> Config;
    fn options_text(&self) -> Option<String>;
    fn cwd(&self) -> PathBuf;
    /// Writes the projects folder into `options.toml`, creating the folder if missing.
    fn set_projects(&self, folder: &Path) -> Result<()>;
    fn exists(&self, path: &Path) -> bool;
    fn mac(&self) -> MacInfo;
    fn health(&self) -> Result<Value, String>;
    fn devices(&self) -> Option<Vec<Value>>;
    fn offline_audit(&self) -> Option<Result<(u64, usize), String>>;
    fn startup(&self) -> Option<Record>;
    fn service(&self) -> ServiceView;
    fn service_config(&self) -> bool;
    fn relay(&self) -> bool;
    fn apns(&self) -> Option<String>;
    fn codex_version(&self) -> Option<String> { None }
    fn native_accounts(&self) -> Option<Value> { None }
    fn claude(&self, args: &[&str], timeout: Duration) -> Option<(bool, String)>;
    fn signed_in(&self) -> Result<bool>;
    fn profile(&self) -> Result<Option<String>, String>;
    fn plugins(&self) -> Option<Value>;
    fn marketplace(&self) -> bool;
    fn node_plugin(&self) -> Option<PathBuf>;
    fn fingerprint(&self, dir: &Path) -> Option<String>;
    fn tool(&self, program: &str, args: &[&str]) -> Option<String>;
    fn port_owner(&self, port: u16) -> Option<String>;
    fn log_tail(&self, lines: usize) -> Vec<String>;
    fn memory(&self) -> Memory;
    fn remember(&self, memory: &Memory);
    fn now(&self) -> i64;
    /// Applies one repair. Starting the service waits for the daemon to answer.
    fn apply(&self, action: Action) -> Result<()>;
    fn test_push(&self, device: &str) -> Result<()>;
    /// The program, marketplace and Claude profile `--fix` would install the service with.
    fn install_target(&self) -> Option<Value> {
        None
    }
}

pub fn command_output(mut command: Command, timeout: Duration) -> Option<(bool, String)> {
    let child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let Ok(Ok(out)) = rx.recv_timeout(timeout) else {
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        return None;
    };
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some((out.status.success(), text))
}

fn output(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args);
    command_output(command, timeout).filter(|(ok, _)| *ok).map(|(_, t)| t)
}

/// A digest of every file under the node plugin, so a changed plugin is checked again.
pub fn fingerprint(dir: &Path) -> Option<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() { walk(&path, out)? } else { out.push(path) }
        }
        Ok(())
    }
    let mut files = vec![];
    walk(dir, &mut files).ok()?;
    files.sort();
    let mut hash = Sha256::new();
    for file in files {
        hash.update(file.strip_prefix(dir).ok()?.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(std::fs::read(&file).ok()?);
        hash.update([0]);
    }
    Some(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// How long `doctor --fix` waits for a started daemon to answer.
pub const START_WAIT: Duration = Duration::from_secs(30);

/// `make e2e-sandbox NEW_MAC=1` stands in for launchd and the relay account, so a development
/// build can walk a new Mac's setup without touching this Mac's service or Claude profile.
struct Sandbox {
    /// Starts or restarts this HOME's daemon.
    service: PathBuf,
    /// Enrolls this HOME with the sandbox relay.
    enroll: PathBuf,
    /// The node plugin the sandbox daemon loads.
    plugin: Option<PathBuf>,
}

impl Sandbox {
    fn from_env() -> Option<Self> {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if std::env::var("ALONDRA_SANDBOX").as_deref() != Ok("1") {
            return None;
        }
        let running_from_brew = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok())
            .is_some_and(|p| setup::brew_package_for_executable(&p).is_some());
        if running_from_brew {
            return None;
        }
        Some(Sandbox { service: var("ALONDRA_SANDBOX_SERVICE")?, enroll: var("ALONDRA_SANDBOX_ENROLL")?, plugin: var("ALONDRA_PLUGIN_SOURCE") })
    }

    fn run(script: &Path, home: &Path) -> Result<()> {
        let status = Command::new(script).env("ALONDRA_HOME", home).stdout(std::io::stderr()).status()
            .with_context(|| script.display().to_string())?;
        if !status.success() {
            let script = script.display();
            bail!(trf!("{script} failed", "{script} falló"));
        }
        Ok(())
    }
}

pub fn sandboxed() -> bool {
    Sandbox::from_env().is_some()
}

pub struct Real {
    env_home: PathBuf,
    home: PathBuf,
    sandbox: Option<Sandbox>,
    install: RefCell<Option<std::rc::Rc<setup::Installation>>>,
    health: RefCell<Option<Result<Value, String>>>,
    context: RefCell<Option<Result<crate::claude_auth::ContextEnv, String>>>,
}

impl Real {
    pub fn new() -> Self {
        let env = |k: &str| std::env::var(k).ok();
        Self {
            env_home: config::home_dir(&env),
            home: config::alondra_home(&env),
            sandbox: Sandbox::from_env(),
            install: RefCell::new(None),
            health: RefCell::new(None),
            context: RefCell::new(None),
        }
    }

    fn install(&self) -> std::rc::Rc<setup::Installation> {
        self.install.borrow_mut().get_or_insert_with(|| std::rc::Rc::new(setup::inspect())).clone()
    }

    /// The environment the daemon runs Claude with: its service file, or what setup would write.
    fn context(&self) -> Result<crate::claude_auth::ContextEnv, String> {
        if let Some(context) = self.context.borrow().as_ref() {
            return context.clone();
        }
        let label = self.config().service_label;
        let plist = self.env_home.join("Library/LaunchAgents").join(format!("{label}.plist"));
        let context = match setup::read_plist(&plist) {
            Ok(value) => {
                let vars = value.get("EnvironmentVariables").cloned().unwrap_or(Value::Null);
                serde_json::from_value(vars).map_err(|e| e.to_string())
                    .and_then(|vars| crate::claude_auth::ContextEnv::from_environment(&self.env_home, &vars).map_err(|e| e.to_string()))
            }
            Err(_) => Ok(self.install().context.clone()),
        };
        *self.context.borrow_mut() = Some(context.clone());
        context
    }

    fn forget(&self) {
        *self.install.borrow_mut() = None;
        *self.health.borrow_mut() = None;
        *self.context.borrow_mut() = None;
    }

    fn wait_for_daemon(&self) -> Result<()> {
        let started = Instant::now();
        let client = Client::new(Duration::from_millis(500));
        while started.elapsed() < START_WAIT {
            if client.get::<Value>("/daemon").is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        bail!(tr!("Alondra didn't start.", "Alondra no ha arrancado."))
    }

    fn wait_if(&self, started: bool) -> Result<()> {
        if started { self.wait_for_daemon() } else { Ok(()) }
    }

    fn daemon_answers(&self) -> bool {
        Client::new(Duration::from_millis(500)).get::<Value>("/daemon").is_ok()
    }

    fn start_service(&self, action: Action) -> Result<()> {
        if let Some(sandbox) = &self.sandbox {
            return Sandbox::run(&sandbox.service, &self.home).and_then(|_| self.wait_for_daemon());
        }
        let install = self.install();
        match action {
            Action::ReplaceOtherService => install.retire_other_services()
                .and_then(|_| setup::inspect().install_service())
                .and_then(|started| self.wait_if(started)),
            Action::RestartService => install.restart_service().and_then(|_| self.wait_for_daemon()),
            _ => install.install_service().and_then(|started| self.wait_if(started)),
        }
    }

    /// Restarts the daemon when it runs, so it reads what changed; a stopped one reads it on start.
    pub fn restart_if_running(&self) -> Result<()> {
        if self.daemon_answers() { self.start_service(Action::RestartService) } else { Ok(()) }
    }

    /// Installs the service under the label `config.toml` names now and retires the old one.
    pub fn relabel_service(&self) -> Result<()> {
        self.start_service(Action::ReplaceOtherService)
    }

    /// The daemon reads the relay only when it starts.
    pub fn enable_relay(&self) -> Result<()> {
        let running = self.daemon_answers();
        match &self.sandbox {
            Some(sandbox) => Sandbox::run(&sandbox.enroll, &self.home)?,
            None => {
                crate::service_setup::enroll(&self.home, false)?;
            }
        }
        if running { self.start_service(Action::RestartService) } else { Ok(()) }
    }

}

impl System for Real {
    fn codex_version(&self) -> Option<String> {
        let context = crate::claude_auth::installed_codex_context(&self.env_home).ok()?;
        let executable = alondra_core::codex_account::executable(&context.path)?;
        let username = alondra_core::claude_account::effective_username()?;
        let mut command = alondra_core::codex_account::command(&executable, &context.environment, &context.home, &context.path, &username, None);
        command.arg("--version");
        command_output(command, Duration::from_secs(10)).filter(|(ok, _)| *ok).map(|(_, version)| version)
    }
    fn native_accounts(&self) -> Option<Value> {
        let client = Client::new(Duration::from_secs(5));
        let listing: Value = client.get("/settings/accounts").ok()?;
        listing.get("accounts").cloned()
    }

    fn home(&self) -> PathBuf {
        self.home.clone()
    }

    fn install_target(&self) -> Option<Value> {
        self.sandbox.is_none().then(|| self.install().target())
    }

    fn user_home(&self) -> PathBuf {
        self.env_home.clone()
    }

    fn config_path(&self) -> PathBuf {
        config::path_in(&self.home)
    }

    fn config_text(&self) -> Option<String> {
        std::fs::read_to_string(self.config_path()).ok()
    }

    fn config(&self) -> Config {
        Config::load().unwrap_or_else(|_| Config::defaults())
    }

    fn options_text(&self) -> Option<String> {
        std::fs::read_to_string(self.home.join(alondra_core::options::FILE_NAME)).ok()
    }

    fn cwd(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| self.env_home.clone())
    }

    fn set_projects(&self, folder: &Path) -> Result<()> {
        let file = self.home.join(alondra_core::options::FILE_NAME);
        crate::setup_projects::select(file, &self.env_home, &self.cwd(), Some(folder), true, false, &mut std::io::empty(), &mut std::io::sink())?
            .store()
            .map(|_| ())
    }

    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn mac(&self) -> MacInfo {
        let os = output("/usr/bin/sw_vers", &["-productVersion"], Duration::from_secs(5)).map(|v| format!("macOS {}", v.trim())).unwrap_or_default();
        let arch = match std::env::consts::ARCH {
            "aarch64" => "Apple Silicon".to_string(),
            other => other.to_string(),
        };
        let path = std::env::current_exe().and_then(|p| p.canonicalize()).unwrap_or_default();
        let install = match crate::version::install(&path, &self.env_home) {
            "homebrew" => "Homebrew",
            "source" => tr!("built from source", "compilado desde el código"),
            _ => tr!("development build", "build de desarrollo"),
        };
        MacInfo { os, arch, install: install.into(), dev_over_brew: self.sandbox.is_none() && self.install().dev_over_brew }
    }

    fn health(&self) -> Result<Value, String> {
        if let Some(health) = self.health.borrow().as_ref() {
            return health.clone();
        }
        let health = Client::new(Duration::from_secs(5)).get::<Value>("/daemon").map_err(|e| e.to_string());
        *self.health.borrow_mut() = Some(health.clone());
        health
    }

    fn devices(&self) -> Option<Vec<Value>> {
        Client::new(Duration::from_secs(5)).get::<Value>("/devices").ok()
            .and_then(|v| v.get("devices").and_then(Value::as_array).cloned())
    }

    fn offline_audit(&self) -> Option<Result<(u64, usize), String>> {
        self.home.join("alondra.db").exists().then(|| crate::state::audit(&self.home).map_err(|e| format!("{e:#}")))
    }

    fn startup(&self) -> Option<Record> {
        alondra_core::startup::read(&self.home)
    }

    fn service(&self) -> ServiceView {
        let install = self.install();
        if self.sandbox.is_some() {
            return ServiceView {
                label: "e2e sandbox".into(),
                installed: true,
                daemon: if self.health().is_ok() { DaemonState::Answers } else { DaemonState::Stopped },
                binaries: true,
                bin: install.paths.bin.clone(),
                repo: install.paths.repo.clone(),
                ..Default::default()
            };
        }
        let label = install.label.clone();
        let daemon = if self.health().is_ok() {
            DaemonState::Answers
        } else if alondra_core::home_lock::in_use(&install.paths.alondra_home).unwrap_or(true) {
            DaemonState::Silent
        } else {
            DaemonState::Stopped
        };
        ServiceView {
            label,
            installed: install.seen.plist.is_some(),
            daemon,
            pending: install.service_pending(),
            others: install.seen.other_services.clone(),
            binaries: install.seen.binaries,
            bin: install.paths.bin.clone(),
            dev_over_brew: install.dev_over_brew,
            repo: install.paths.repo.clone(),
            marketplace_elsewhere: install.marketplace_elsewhere(),
        }
    }

    fn service_config(&self) -> bool {
        self.home.join("service.toml").symlink_metadata().is_ok()
    }

    fn relay(&self) -> bool {
        crate::service_setup::has_relay(&self.home).unwrap_or(false)
    }

    fn apns(&self) -> Option<String> {
        std::fs::read_to_string(self.home.join("apns.json")).ok()
    }

    fn claude(&self, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
        let mut command = self.context().ok()?.command().ok()?;
        command.args(args);
        command_output(command, timeout)
    }

    fn signed_in(&self) -> Result<bool> {
        self.context().map_err(anyhow::Error::msg)?.status()
    }

    fn profile(&self) -> Result<Option<String>, String> {
        if let (Ok(config), Ok(watcher)) = (std::env::var("CLAUDE_CONFIG_DIR"), std::env::var("CLAUDE_HOME"))
            && config != watcher
        {
            return Err(tr!("CLAUDE_CONFIG_DIR and CLAUDE_HOME disagree", "CLAUDE_CONFIG_DIR y CLAUDE_HOME no coinciden").into());
        }
        self.context().map(|c| c.config_dir)
    }

    fn plugins(&self) -> Option<Value> {
        if self.sandbox.is_some() {
            return Some(Value::Array(vec![]));
        }
        self.context().ok().and_then(|c| setup::plugin_list(&c))
    }

    fn marketplace(&self) -> bool {
        if self.sandbox.is_some() {
            return true;
        }
        self.context().is_ok_and(|c| setup::has_marketplace(&c))
    }

    fn node_plugin(&self) -> Option<PathBuf> {
        if let Some(sandbox) = &self.sandbox {
            return sandbox.plugin.clone().filter(|dir| dir.join(".claude-plugin/plugin.json").is_file());
        }
        self.context().ok().and_then(|c| setup::marketplace_path(&c)).map(|root| root.join("plugin"))
            .filter(|dir| dir.join(".claude-plugin/plugin.json").is_file())
    }

    fn fingerprint(&self, dir: &Path) -> Option<String> {
        fingerprint(dir)
    }

    fn tool(&self, program: &str, args: &[&str]) -> Option<String> {
        output(program, args, Duration::from_secs(5))
    }

    fn port_owner(&self, port: u16) -> Option<String> {
        let text = output("/usr/sbin/lsof", &["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fc"], Duration::from_secs(5))?;
        text.lines().find_map(|l| l.strip_prefix('c')).filter(|name| *name != "alondrad").map(str::to_string)
    }

    fn log_tail(&self, lines: usize) -> Vec<String> {
        alondra_core::daemon_log::tail(&self.home, lines, u64::MAX).unwrap_or_default()
    }

    fn memory(&self) -> Memory {
        std::fs::read_to_string(self.home.join("doctor.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn remember(&self, memory: &Memory) {
        let path = self.home.join("doctor.json");
        let tmp = path.with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string_pretty(memory)
            && std::fs::write(&tmp, text).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn now(&self) -> i64 {
        alondra_core::now_millis()
    }

    fn apply(&self, action: Action) -> Result<()> {
        let result = match action {
            Action::InstallService | Action::ReplaceOtherService | Action::RestartService => self.start_service(action),
            Action::PrepareIdentity => crate::service_setup::prepare_local_quietly(&self.home).map(|_| ()),
            Action::RepairPlugin if self.sandbox.is_some() => Ok(()),
            Action::RepairPlugin => self.install().repair_plugin(),
            Action::EnableRelay => self.enable_relay(),
            Action::SetProjects => Ok(()),
        };
        self.forget();
        result
    }

    fn test_push(&self, device: &str) -> Result<()> {
        let daemon = self.install().paths.bin.join("alondrad");
        let mut command = Command::new(&daemon);
        command.args(["--test-service-push", device]);
        match command_output(command, Duration::from_secs(60)) {
            Some((true, _)) => Ok(()),
            Some((false, text)) => bail!("{}", text.trim()),
            None => bail!(tr!("the test notification did not finish within a minute", "la notificación de prueba no terminó en un minuto")),
        }
    }
}
