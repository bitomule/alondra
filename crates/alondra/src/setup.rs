use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use alondra_core::config::{self, FileConfig};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const TEMPLATE: &str = include_str!("../../../launchd/alondra.plist.in");
pub const MARKETPLACE: &str = "alondra";

/// What the installation should look like beyond `config.toml`: inspect() leaves everything at
/// its default, and the tests ask for the rest.
#[derive(Default)]
pub struct Args {
    pub name: Option<String>,
    pub public_url: Option<String>,
    pub listen: Option<IpAddr>,
    pub port: Option<u16>,
    pub legacy_bridge: Option<bool>,
    pub wake: Option<bool>,
    pub retention_hours: Option<f64>,
    pub service_label: Option<String>,
    pub from_plist: Option<PathBuf>,
    pub skip_service: bool,
    pub skip_plugin: bool,
    pub force: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Apns {
    pub key_path: PathBuf,
    pub key_id: String,
    pub team_id: String,
    pub topic: String,
}

pub struct Paths {
    pub home: PathBuf,
    pub alondra_home: PathBuf,
    pub bin: PathBuf,
    pub repo: Option<PathBuf>,
    pub uid: u32,
}

impl Paths {
    fn config(&self) -> PathBuf {
        config::path_in(&self.alondra_home)
    }
    fn apns(&self) -> PathBuf {
        self.alondra_home.join("apns.json")
    }
    fn plist(&self, label: &str) -> PathBuf {
        self.home.join("Library/LaunchAgents").join(format!("{label}.plist"))
    }
    fn daemon(&self) -> PathBuf {
        self.bin.join("alondrad")
    }
    /// What launchd runs before alondrad: the launcher at its fixed path, when this build ships
    /// one. macOS keeps the Local Network permission for that path across updates.
    fn launcher(&self) -> Option<PathBuf> {
        alondra_core::launcher::bundled_path(&self.daemon()).is_file().then(|| alondra_core::launcher::installed_path(&self.alondra_home))
    }
}

#[derive(Default, Clone)]
pub struct Observed {
    pub connected: Vec<alondra_core::accounts::Provider>,
    pub config: Option<String>,
    pub apns: Option<String>,
    pub plist: Option<String>,
    /// A daemon holds this HOME's lock.
    pub daemon_running: bool,
    pub other_services: Vec<PathBuf>,
    pub binaries: bool,
    pub apns_key_exists: bool,
    pub claude: bool,
    pub marketplace: bool,
    pub marketplace_path: Option<PathBuf>,
    /// Alondra's plugins that a global install enables in every session, with the scope of
    /// each install.
    pub enabled_globally: Vec<(String, String)>,
    pub tuicr: bool,
    pub brew: bool,
    /// The release ships a launcher that its fixed path does not hold yet.
    pub launcher_outdated: bool,
}

#[derive(Debug, PartialEq)]
pub enum Step {
    Write { path: PathBuf, contents: String, private: bool },
    Unchanged { path: PathBuf },
    Run { program: String, args: Vec<String> },
    /// `launchctl bootout`, then waits until no daemon holds `home`.
    Unload { target: String, home: PathBuf },
    /// Copies the launcher a release ships to the fixed path launchd runs it from.
    Launcher { bundled: PathBuf, installed: PathBuf },
    Warn(String),
}

pub struct Plan {
    pub steps: Vec<Step>,
    pub config: FileConfig,
    pub conflicts: Vec<String>,
}

fn xml_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn claude_context(paths: &Paths, label: &str) -> crate::claude_auth::ContextEnv {
    let mut context = crate::claude_auth::ContextEnv::for_setup(&paths.home, &paths.bin);
    // A setup retry from another shell must not silently switch an installed Claude profile.
    if std::env::var_os("CLAUDE_CONFIG_DIR").is_none() && std::env::var_os("CLAUDE_HOME").is_none() {
        context.config_dir = read_plist(&paths.plist(label)).ok()
            .and_then(|v| crate::claude_auth::job_profile(&v)).map(|p| p.to_string_lossy().into_owned());
    }
    context
}

fn legacy_default_profile(plist: &serde_json::Value, home: &Path) -> bool {
    let Some(vars) = plist.get("EnvironmentVariables") else { return false; };
    let default = home.join(".claude");
    vars.get("ALONDRA_CLAUDE_PROFILE_MODE").is_none()
        && vars.get("CLAUDE_CONFIG_DIR").and_then(|v| v.as_str()) == default.to_str()
        && vars.get("CLAUDE_HOME").and_then(|v| v.as_str()) == default.to_str()
}

// A legacy default could have been intentional. Only repair it when the old
// namespace is unauthenticated and the implicit namespace is already authenticated.
// No login, credential copy or profile mutation is performed by these probes.
fn resolve_legacy_profile(context: crate::claude_auth::ContextEnv, legacy: bool) -> Result<crate::claude_auth::ContextEnv> {
    if !legacy || context.status()? { return Ok(context); }
    let mut implicit = context.clone();
    implicit.config_dir = None;
    if implicit.status()? { Ok(implicit) } else { Ok(context) }
}

pub fn render_plist(label: &str, paths: &Paths) -> Result<String> {
    render_plist_with_context(label, paths, &claude_context(paths, label))
}

fn render_plist_with_context(label: &str, paths: &Paths, context: &crate::claude_auth::ContextEnv) -> Result<String> {
    let username = crate::claude_auth::effective_username()?;
    let mut config = format!("<key>USER</key><string>{0}</string><key>LOGNAME</key><string>{0}</string>", xml_escape(&username));
    if let Some(profile) = &context.config_dir {
        config.push_str(&format!("<key>CLAUDE_CONFIG_DIR</key><string>{0}</string><key>CLAUDE_HOME</key><string>{0}</string><key>ALONDRA_CLAUDE_PROFILE_MODE</key><string>explicit</string>", xml_escape(profile)));
    }
    let installed = read_plist(&paths.plist(label)).ok();
    let codex_home = installed.as_ref().and_then(|job| job.get("EnvironmentVariables")).and_then(|vars| vars.get("CODEX_HOME")).and_then(Value::as_str).map(str::to_owned).or_else(|| std::env::var("CODEX_HOME").ok().filter(|value| !value.is_empty()));
    if let Some(home) = codex_home { config.push_str(&format!("<key>CODEX_HOME</key><string>{}</string>", xml_escape(&home))); }
    let launcher = paths.launcher().map(|launcher| format!("<string>{}</string>\n        ", xml_escape(&launcher.display().to_string()))).unwrap_or_default();
    Ok(TEMPLATE
        .replace("@LAUNCHER@", &launcher)
        .replace("@CLAUDE_CONFIG_ENV@", &config)
        .replace("@PATH@", &xml_escape(&context.path))
        .replace("@LABEL@", label)
        .replace("@BIN@", &xml_escape(&paths.bin.display().to_string()))
        .replace("@HOME@", &xml_escape(&paths.home.display().to_string()))
        .replace("@ALONDRA_HOME@", &xml_escape(&paths.alondra_home.display().to_string())))
}

/// Keys the user already has keep their value unless `force`; missing keys are filled in.
fn merge(existing: &FileConfig, wanted: &FileConfig, force: bool, conflicts: &mut Vec<String>) -> FileConfig {
    let existing_map = as_map(existing);
    for (k, v) in as_map(wanted) {
        if let Some(old) = existing_map.get(&k)
            && *old != v
            && !force
        {
            conflicts.push(format!("{k}: {old} → {v} (--force)"));
        }
    }
    if force { existing.clone().overlay(wanted) } else { wanted.clone().overlay(existing) }
}

fn as_map(c: &FileConfig) -> BTreeMap<String, String> {
    match serde_json::to_value(c) {
        Ok(Value::Object(m)) => m.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, v.to_string())).collect(),
        _ => BTreeMap::new(),
    }
}

pub fn plan(wanted: &FileConfig, apns: Option<&Apns>, args: &Args, paths: &Paths, seen: &Observed) -> Plan {
    plan_with_context(wanted, apns, args, paths, seen, None)
}

fn plan_with_context(wanted: &FileConfig, apns: Option<&Apns>, args: &Args, paths: &Paths, seen: &Observed, context: Option<&crate::claude_auth::ContextEnv>) -> Plan {
    let mut steps = vec![];
    let mut conflicts = vec![];

    let existing = seen.config.as_deref().map(FileConfig::parse);
    let existing = match existing {
        Some(Ok(c)) => c,
        Some(Err(e)) if !args.force => {
            conflicts.push(format!("{}: {e}", paths.config().display()));
            FileConfig::default()
        }
        _ => FileConfig::default(),
    };
    let mut wanted = wanted.clone();
    if wanted.wake.is_none() && existing.wake.is_none() {
        wanted.wake = Some(true);
    }
    let merged = merge(&existing, &wanted, args.force, &mut conflicts);
    let same = seen.config.as_deref().and_then(|t| FileConfig::parse(t).ok()).as_ref() == Some(&merged);
    if same {
        steps.push(Step::Unchanged { path: paths.config() });
    } else {
        steps.push(Step::Write { path: paths.config(), contents: merged.render(), private: false });
    }
    let mut restart = !matches!(steps.last(), Some(Step::Unchanged { .. }));

    match (apns, seen.apns.as_deref()) {
        (Some(a), existing) => {
            let text = serde_json::to_string_pretty(a).unwrap_or_default() + "\n";
            let same = existing.and_then(|e| serde_json::from_str::<Apns>(e).ok()).as_ref() == Some(a);
            if !seen.apns_key_exists {
                let key = a.key_path.display();
                steps.push(Step::Warn(trf!("The APNs key {key} does not exist yet.", "La llave de APNs {key} todavía no existe.")));
            }
            if same {
                steps.push(Step::Unchanged { path: paths.apns() });
            } else if existing.is_some() && !args.force {
                conflicts.push(format!("{}: {} (--force)", paths.apns().display(), tr!("already exists with other values", "ya existe con otros valores")));
            } else {
                steps.push(Step::Write { path: paths.apns(), contents: text, private: true });
                restart = true;
            }
        }
        (None, None) => {}
        (None, Some(_)) => steps.push(Step::Unchanged { path: paths.apns() }),
    }

    if !args.skip_service {
        for other in &seen.other_services {
            let o = other.display();
            conflicts.push(trf!(
                "another launchd service already runs alondrad: {o}. Migrate it with --from-plist {o}",
                "otro servicio de launchd ya arranca alondrad: {o}. Mígralo con --from-plist {o}"
            ));
        }
        let label = merged.service_label.clone().unwrap_or_else(|| config::DEFAULT_LABEL.into());
        let plist_path = paths.plist(&label);
        let rendered = match context {
            Some(context) => render_plist_with_context(&label, paths, context),
            None => render_plist(&label, paths),
        };
        let plist = match rendered {
            Ok(plist) => plist,
            Err(error) => {
                conflicts.push(error.to_string());
                return Plan { steps: vec![], config: merged, conflicts };
            }
        };
        let domain = format!("gui/{}", paths.uid);
        let target = format!("{domain}/{label}");
        if !seen.binaries {
            let bin = paths.bin.display();
            steps.push(Step::Warn(trf!(
                "{bin}/alondrad is missing, so the service is not started: run `make install` in the repo and then setup again.",
                "Falta {bin}/alondrad, así que no arranco el servicio: ejecuta `make install` en el repo y después setup otra vez."
            )));
        }
        if seen.binaries && seen.launcher_outdated && let Some(installed) = paths.launcher() {
            steps.push(Step::Launcher { bundled: alondra_core::launcher::bundled_path(&paths.daemon()), installed });
        }
        let plist_changed = seen.plist.as_deref() != Some(plist.as_str());
        if plist_changed {
            steps.push(Step::Write { path: plist_path.clone(), contents: plist, private: false });
        } else {
            steps.push(Step::Unchanged { path: plist_path.clone() });
        }
        let run = |args: &[&str]| Step::Run { program: "launchctl".into(), args: args.iter().map(|s| s.to_string()).collect() };
        if seen.binaries && (plist_changed || !seen.daemon_running) {
            if seen.daemon_running || seen.plist.is_some() {
                steps.push(Step::Unload { target: target.clone(), home: paths.alondra_home.clone() });
            }
            steps.push(run(&["bootstrap", &domain, &plist_path.display().to_string()]));
        } else if seen.binaries && restart {
            steps.push(run(&["kickstart", "-k", &target]));
        }
    }

    if !args.skip_plugin && seen.connected.contains(&alondra_core::accounts::Provider::Claude) {
        let claude = |args: &[&str]| Step::Run { program: "claude".into(), args: args.iter().map(|s| s.to_string()).collect() };
        if !seen.claude {
            steps.push(Step::Warn(
                tr!(
                    "Claude Code (`claude`) is not on PATH: Alondra's marketplace is not registered, so alondrad cannot find the plugin it launches nodes with.",
                    "Claude Code (`claude`) no está en el PATH: no se registra el marketplace de Alondra, así que alondrad no encuentra el plugin con el que lanza los nodos."
                )
                .into(),
            ));
        } else {
            if seen.marketplace_path.as_ref().is_some_and(|p| paths.repo.as_ref().is_some_and(|wanted| p != wanted)) {
                steps.push(claude(&["plugin", "marketplace", "remove", MARKETPLACE]));
                if let Some(repo) = &paths.repo {
                    steps.push(claude(&["plugin", "marketplace", "add", &repo.display().to_string()]));
                }
            } else if !seen.marketplace {
                match &paths.repo {
                    Some(repo) => steps.push(claude(&["plugin", "marketplace", "add", &repo.display().to_string()])),
                    None => steps.push(Step::Warn(
                        tr!("Alondra's repo not found: run setup from the repo or pass --repo.", "No encuentro el repo de Alondra: ejecuta setup desde el repo o pasa --repo.").into(),
                    )),
                }
            }
            for (id, scope) in &seen.enabled_globally {
                steps.push(claude(&["plugin", "disable", id, "--scope", scope]));
            }
        }
    }
    if !seen.tuicr {
        match seen.brew {
            true => steps.push(Step::Run { program: "brew".into(), args: vec!["install".into(), "tuicr".into()] }),
            false => steps.push(Step::Warn(
                tr!(
                    "tuicr is missing, so PR cards have no diff in the TUI: cargo install tuicr",
                    "Falta tuicr, así que las tarjetas de PR no tienen diff en la TUI: cargo install tuicr"
                )
                .into(),
            )),
        }
    }
    Plan { steps, config: merged, conflicts }
}

/// Settings the old plist carried as environment variables. The old daemon always bridged
/// with `open-questions.json` when it existed, so the migration keeps the bridge on.
pub fn migrate_env(label: Option<&str>, env: &BTreeMap<String, String>) -> FileConfig {
    let get = |k: &str| env.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    FileConfig {
        public_url: get("ALONDRA_PUBLIC_URL"),
        listen: get("ALONDRA_LISTEN").and_then(|v| v.parse().ok()),
        port: get("ALONDRA_PORT").and_then(|v| v.parse().ok()),
        wake: Some(get("ALONDRA_WAKE").as_deref() == Some("1")),
        retention_hours: get("ALONDRA_RETENTION_HOURS").and_then(|v| v.parse().ok()),
        legacy_bridge: Some(true),
        service_label: label.map(str::to_string),
        name: get("ALONDRA_NAME"),
        ..Default::default()
    }
}

pub(crate) fn read_plist(path: &Path) -> Result<Value> {
    let out = Command::new("/usr/bin/plutil").args(["-convert", "json", "-o", "-"]).arg(path).output().context("plutil")?;
    if !out.status.success() {
        bail!("{}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

fn from_plist(path: &Path) -> Result<FileConfig> {
    let v = read_plist(path)?;
    let env: BTreeMap<String, String> = v
        .get("EnvironmentVariables")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default();
    Ok(migrate_env(v.get("Label").and_then(Value::as_str), &env))
}

pub fn find_repo(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|d| d.join(".claude-plugin/marketplace.json").is_file() && d.join("plugin").is_dir()).map(Path::to_path_buf)
}

/// A release can carry the marketplace next to its binaries, without a source checkout.
fn installed_package() -> Option<(PathBuf, PathBuf)> {
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    brew_package_for_executable(&executable)
}

pub(crate) fn brew_package_for_executable(executable: &Path) -> Option<(PathBuf, PathBuf)> {
    // Homebrew launches the real executable from its versioned Cellar keg. Persist
    // paths through opt/alondra so launchd and Claude survive a formula upgrade.
    let bin = executable.parent()?;
    let keg = bin.parent()?;
    let rack = keg.parent()?;
    let cellar = rack.parent()?;
    if executable.file_name()? != "alondra"
        || bin.file_name()? != "bin"
        || rack.file_name()? != "alondra"
        || cellar.file_name()? != "Cellar"
    {
        return None;
    }
    let stable = cellar.parent()?.join("opt/alondra");
    let assets = stable.join("libexec/alondra");
    (stable.join("bin/alondrad").is_file())
        .then(|| find_repo(&assets))
        .flatten()
        .map(|assets| (stable.join("bin"), assets))
}



fn wanted_from(args: &Args, existing: Option<&FileConfig>) -> Result<FileConfig> {
    let mut wanted = match &args.from_plist {
        Some(p) => from_plist(p)?,
        None => FileConfig::default(),
    };
    let flags = FileConfig {
        name: args.name.clone(),
        port: args.port,
        listen: args.listen,
        public_url: args.public_url.clone(),
        legacy_bridge: args.legacy_bridge,
        wake: args.wake,
        retention_hours: args.retention_hours,
        service_label: args.service_label.clone(),
        ..Default::default()
    };
    wanted = wanted.overlay(&flags);
    let known = |w: &FileConfig, f: fn(&FileConfig) -> bool| f(w) || existing.is_some_and(f);
    if !known(&wanted, |c| c.name.is_some()) {
        wanted.name = config::system_name().filter(|n| !n.is_empty());
    }
    Ok(wanted)
}

fn command_ok(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn launchctl(args: &[&str]) -> Result<Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .context("launchctl")
}

/// Asks launchd to unload `target` and waits until no daemon holds `home`. bootout also fails when
/// the job was not loaded, so its result proves nothing: the HOME lock says whether the daemon left.
fn unload(
    target: &str,
    home: &Path,
    invoke: impl FnOnce(&[&str]) -> Result<Output>,
    polls: usize,
    interval: Duration,
) -> Result<()> {
    invoke(&["bootout", target])?;
    for _ in 0..polls {
        if !alondra_core::home_lock::in_use(home)? {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
    bail!(tr!("this HOME's daemon did not stop after launchctl bootout", "el daemon de este HOME no se paró tras launchctl bootout"))
}

fn ensure_no_running_daemons(mut pids: impl FnMut() -> Result<Vec<i32>>) -> Result<()> {
    let running = pids()?;
    anyhow::ensure!(
        running.is_empty(),
        "another alondrad is still running; stop any manually started daemon or daemon for another HOME before retrying setup"
    );
    Ok(())
}

fn release_home_lock_then(
    home_lock: &mut Option<alondra_core::home_lock::HomeLock>,
    launch: impl FnOnce() -> Result<()>,
) -> Result<()> {
    drop(home_lock.take());
    launch()
}

fn bootstrap_then_record(
    home_lock: &mut Option<alondra_core::home_lock::HomeLock>,
    bootstrap: impl FnOnce() -> Result<()>,
    record: impl FnOnce() -> Result<()>,
) -> Result<()> {
    release_home_lock_then(home_lock, bootstrap)?;
    record()
}

fn is_launchd_lifecycle_step(step: &Step) -> bool {
    matches!(step, Step::Unload { .. })
        || matches!(step, Step::Run { program, args } if program == "launchctl"
            && matches!(args.first().map(String::as_str), Some("bootout" | "bootstrap" | "kickstart")))
}

fn claude_output(context: &crate::claude_auth::ContextEnv, args: &[&str]) -> Option<String> {
    let out = context.command().ok()?.args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn plugin_list(context: &crate::claude_auth::ContextEnv) -> Option<Value> {
    claude_output(context, &["plugin", "list", "--json"]).and_then(|t| serde_json::from_str(&t).ok())
}

/// Alondra's plugins that `claude plugin list --json` shows enabled, with the scope of each
/// install. Alondra loads its plugin only into the sessions it launches, so none should be.
pub fn enabled_globally(list: &Value) -> Vec<(String, String)> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|p| p.get("enabled") == Some(&Value::Bool(true)))
        .filter_map(|p| {
            let id = p.get("id")?.as_str().filter(|id| alondra_core::GLOBAL_PLUGINS.contains(id))?;
            Some((id.to_string(), p.get("scope").and_then(Value::as_str).unwrap_or("user").to_string()))
        })
        .collect()
}

pub fn has_marketplace(context: &crate::claude_auth::ContextEnv) -> bool {
    claude_output(context, &["plugin", "marketplace", "list", "--json"])
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|a| a.iter().any(|m| m.get("name").and_then(Value::as_str) == Some(MARKETPLACE)))
}

pub fn marketplace_path(context: &crate::claude_auth::ContextEnv) -> Option<PathBuf> {
    claude_output(context, &["plugin", "marketplace", "list", "--json"])
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_array().cloned())
        .and_then(|a| a.into_iter().find(|m| m.get("name").and_then(Value::as_str) == Some(MARKETPLACE)))
        .and_then(|m| m.get("path").and_then(Value::as_str).map(PathBuf::from))
}

fn other_services(paths: &Paths, label: &str) -> Vec<PathBuf> {
    let own = paths.plist(label);
    let Ok(dir) = std::fs::read_dir(paths.home.join("Library/LaunchAgents")) else { return vec![] };
    let mut found: Vec<PathBuf> = dir
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| *p != own && p.extension().is_some_and(|e| e == "plist"))
        .filter(|p| read_plist(p).is_ok_and(|job| starts_alondrad(&job)))
        .collect();
    found.sort();
    found
}

/// The program a LaunchAgent, read as data, runs: `Program`, else `ProgramArguments[0]`.
fn job_program(job: &Value) -> Option<&Path> {
    job.get("Program").or_else(|| job.pointer("/ProgramArguments/0")).and_then(Value::as_str).map(Path::new)
}

/// Whether a LaunchAgent runs alondrad, directly or through its launcher.
pub(crate) fn starts_alondrad(job: &Value) -> bool {
    job_program(job).and_then(Path::file_name).is_some_and(|name| name == "alondrad" || name == alondra_core::launcher::NAME)
}

fn observe(paths: &Paths, label: &str, apns: Option<&Apns>, args: &Args, context: &crate::claude_auth::ContextEnv) -> Observed {
    let read = |p: PathBuf| std::fs::read_to_string(p).ok();
    let connected = read(paths.alondra_home.join(alondra_core::options::FILE_NAME)).and_then(|text| alondra_core::options::OptionsFile::parse(&text).ok()).map(|options| options.accounts.connected_providers().to_vec()).unwrap_or_default();
    let claude_enabled = connected.contains(&alondra_core::accounts::Provider::Claude) && !args.skip_plugin;
    let plugins = if claude_enabled { plugin_list(&context) } else { None };
    Observed {
        connected,
        config: read(paths.config()),
        apns: read(paths.apns()),
        plist: read(paths.plist(label)),
        daemon_running: !args.skip_service && alondra_core::home_lock::in_use(&paths.alondra_home).unwrap_or(true),
        other_services: other_services(paths, label),
        binaries: paths.bin.join("alondrad").is_file() && paths.bin.join("alondra").is_file(),
        apns_key_exists: apns.is_some_and(|a| a.key_path.is_file()),
        claude: claude_enabled && claude_output(&context, &["--version"]).is_some(),
        marketplace: claude_enabled && has_marketplace(&context),
        marketplace_path: if claude_enabled { marketplace_path(&context) } else { None },
        enabled_globally: plugins.as_ref().map(enabled_globally).unwrap_or_default(),
        tuicr: command_ok("tuicr", &["--version"]).is_some(),
        brew: command_ok("brew", &["--version"]).is_some(),
        launcher_outdated: launcher_outdated(paths),
    }
}

fn launcher_outdated(paths: &Paths) -> bool {
    let Some(installed) = paths.launcher() else { return false };
    let bundled = alondra_core::launcher::version(&alondra_core::launcher::bundled_path(&paths.daemon()));
    bundled.is_some() && bundled != alondra_core::launcher::version(&installed)
}

const BREW_PREFIXES: [&str; 2] = ["/opt/homebrew/opt/alondra", "/usr/local/opt/alondra"];

fn dev_setup_over_brew(running_from_brew: bool, brew_installed: bool, forced: bool) -> bool {
    !running_from_brew && brew_installed && !forced
}

fn execute(step: &Step, context: &crate::claude_auth::ContextEnv) -> Result<()> {
    match step {
        Step::Write { path, contents, private } => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, contents)?;
            if *private {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&tmp, path)?;
        }
        Step::Run { program, args } => {
            let mut command = if program == "claude" { context.command()? } else { Command::new(program) };
            let out = command.args(args).output().with_context(|| program.clone())?;
            if !out.status.success() {
                let cmd = format!("{program} {}", args.join(" "));
                let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
                bail!(trf!("`{cmd}` failed: {why}", "`{cmd}` falló: {why}"));
            }
        }
        Step::Unload { target, home } => unload(target, home, launchctl, 50, Duration::from_millis(100))?,
        Step::Launcher { bundled, installed } => {
            alondra_core::launcher::install(bundled, installed).with_context(|| installed.display().to_string())?;
        }
        Step::Unchanged { .. } | Step::Warn(_) => {}
    }
    Ok(())
}


/// Where a development build's binaries live: `ALONDRA_BIN` (the `Makefile`), else `~/.local/bin`.
fn dev_bin(dir: Option<String>, home: &Path) -> PathBuf {
    dir.filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".local/bin"))
}

/// The installation as it should be: what is there, and the steps that would change it.
/// `alondra doctor` reads it and `doctor --fix` applies parts of it.
pub(crate) struct Installation {
    pub paths: Paths,
    pub label: String,
    pub context: crate::claude_auth::ContextEnv,
    pub seen: Observed,
    pub plan: Plan,
    pub dev_over_brew: bool,
    brew_executable: Option<PathBuf>,
}

pub(crate) fn inspect() -> Installation {
    let env = |k: &str| std::env::var(k).ok();
    let brew_executable = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok())
        .filter(|p| brew_package_for_executable(p).is_some());
    let dev_over_brew = dev_setup_over_brew(
        brew_executable.is_some(),
        BREW_PREFIXES.iter().any(|p| Path::new(p).exists()),
        env("FORCE_DEV_INSTALL").is_some_and(|v| !v.is_empty()),
    );
    let home = config::home_dir(&env);
    let packaged = installed_package();
    let paths = Paths {
        bin: packaged.as_ref().map(|(bin, _)| bin.clone()).unwrap_or_else(|| dev_bin(env("ALONDRA_BIN"), &home)),
        repo: packaged.as_ref().map(|(_, assets)| assets.clone()).or_else(|| std::env::current_dir().ok().and_then(|d| find_repo(&d))),
        uid: unsafe { libc::getuid() },
        home,
        alondra_home: config::alondra_home(&env),
    };
    let existing = std::fs::read_to_string(paths.config()).ok().and_then(|t| FileConfig::parse(&t).ok());
    let args = Args::default();
    let wanted = wanted_from(&args, existing.as_ref()).unwrap_or_default();
    let label = existing.as_ref().and_then(|e| e.service_label.clone()).unwrap_or_else(|| config::DEFAULT_LABEL.into());
    let context = claude_context(&paths, &label);
    let mut seen = observe(&paths, &label, None, &args, &context);
    // Another service is a question for the user, never a reason to stop planning.
    let others = std::mem::take(&mut seen.other_services);
    let plan = plan_with_context(&wanted, None, &args, &paths, &seen, Some(&context));
    seen.other_services = others;
    Installation { paths, label, context, seen, plan, dev_over_brew, brew_executable }
}

impl Installation {
    /// What the LaunchAgent would run and with which Claude profile, for `alondra doctor --json`.
    pub(crate) fn target(&self) -> Value {
        let profile = self.context.config_dir.clone();
        serde_json::json!({
            "program": self.paths.daemon(),
            "launcher": self.paths.launcher(),
            "marketplace": self.paths.repo,
            "claude_profile": profile,
            "profile_mode": if profile.is_some() { "explicit" } else { "implicit" },
        })
    }

    fn is_service_step(&self, step: &Step) -> bool {
        match step {
            Step::Write { path, .. } => *path == self.paths.config() || *path == self.paths.plist(&self.label),
            Step::Run { program, .. } => program == "launchctl",
            Step::Unload { .. } | Step::Launcher { .. } => true,
            _ => false,
        }
    }

    /// Whether `config.toml` or the service file differ from what setup writes, or launchd
    /// would have to load or restart the service.
    pub(crate) fn service_pending(&self) -> bool {
        self.plan.steps.iter().any(|s| self.is_service_step(s))
    }

    /// Where Alondra's marketplace is registered, when that is not this installation.
    pub(crate) fn marketplace_elsewhere(&self) -> Option<PathBuf> {
        self.seen.marketplace_path.clone().filter(|p| self.paths.repo.as_ref().is_some_and(|wanted| p != wanted))
    }

    /// Writes the files while sharing HOME, then hands HOME back before launchd starts the
    /// daemon, which needs it. Returns whether launchd was asked to start it.
    pub(crate) fn install_service(&self) -> Result<bool> {
        let lock = alondra_core::home_lock::HomeLock::shared(&self.paths.alondra_home)?;
        alondra_core::home_lock::ensure_ready(&self.paths.alondra_home)?;
        let steps: Vec<&Step> = self.plan.steps.iter().filter(|s| self.is_service_step(s)).collect();
        for step in steps.iter().filter(|s| !is_launchd_lifecycle_step(s)) {
            execute(step, &self.context)?;
        }
        drop(lock);
        let mut started = false;
        for step in steps.iter().filter(|s| is_launchd_lifecycle_step(s)) {
            execute(step, &self.context)?;
            started = true;
        }
        if let Some(executable) = &self.brew_executable {
            execute(&Step::Write {
                path: self.paths.alondra_home.join("brew-keg-path"),
                contents: executable.to_string_lossy().into_owned(),
                private: true,
            }, &self.context)?;
        }
        Ok(started)
    }

    pub(crate) fn repair_plugin(&self) -> Result<()> {
        for step in self.plan.steps.iter().filter(|s| matches!(s, Step::Run { program, .. } if program == "claude")) {
            execute(step, &self.context)?;
        }
        Ok(())
    }

    /// Stops every other service that starts alondrad and sets its file aside, where launchd no
    /// longer loads it.
    pub(crate) fn retire_other_services(&self) -> Result<()> {
        for other in &self.seen.other_services {
            let label = other.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let _ = launchctl(&["bootout", &format!("gui/{}/{label}", self.paths.uid)]);
            std::fs::rename(other, other.with_extension("plist.replaced"))?;
        }
        Ok(())
    }

    pub(crate) fn restart_service(&self) -> Result<()> {
        execute(&Step::Run {
            program: "launchctl".into(),
            args: vec!["kickstart".into(), "-k".into(), format!("gui/{}/{}", self.paths.uid, self.label)],
        }, &self.context)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dev_setup_is_refused_only_over_an_unforced_brew_install() {
        assert!(dev_setup_over_brew(false, true, false));
        assert!(!dev_setup_over_brew(true, true, false));
        assert!(!dev_setup_over_brew(false, false, false));
        assert!(!dev_setup_over_brew(false, true, true));
    }

    use super::*;
    use alondra_core::config::Config;

    // Operational gate only: reads an explicitly authorized installed plist and
    // invokes Claude auth status. Never logs in, writes profiles, or runs setup.
    #[test]
    #[ignore = "requires explicit authorization for the current user's Claude auth status"]
    fn real_claude_legacy_profile_status_read_only() {
        assert_eq!(std::env::var("ALONDRA_REAL_CLAUDE_PROFILE_CHECK").as_deref(), Ok("1"));
        let path = PathBuf::from(std::env::var_os("ALONDRA_REAL_CLAUDE_PLIST").expect("explicit installed plist path"));
        assert!(path.is_absolute());
        let before = std::fs::read(&path).unwrap();
        let value = read_plist(&path).unwrap();
        let vars = value["EnvironmentVariables"].as_object().unwrap().iter().map(|(k,v)| (k.clone(), v.as_str().unwrap().to_owned())).collect::<std::collections::BTreeMap<_,_>>();
        let home = PathBuf::from(vars.get("HOME").expect("installed HOME"));
        assert!(home.is_absolute());
        assert!(legacy_default_profile(&value, &home), "requires the legacy forced-default shape");
        let original = crate::claude_auth::ContextEnv::from_environment(&home, &vars).unwrap();
        assert!(!original.status().unwrap(), "legacy namespace must reproduce the diagnosed failure");
        let selected = resolve_legacy_profile(original, true).unwrap();
        assert!(selected.config_dir.is_none());
        assert!(selected.status().unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), before, "operational gate must not rewrite the installed plist");
        println!("legacy_authenticated=false selected_implicit=true selected_authenticated=true plist_unchanged=true");
    }

    #[test]
    fn legacy_default_profile_resolution_uses_actual_child_environment() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::PermissionsExt;
        let root = scratch_directory.parent().unwrap().join(format!("alondra-profile-resolution-{}", std::process::id()));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let root = root.canonicalize().unwrap();
        let script = root.join("bin/claude");
        std::fs::write(&script, r#"#!/bin/sh
[ "$1 $2 $3" = 'auth status --json' ] || exit 17
if [ "${CLAUDE_CONFIG_DIR+x}" = x ]; then
  [ "$CLAUDE_CONFIG_DIR" = "$HOME/.claude" ] && [ "$CLAUDE_HOME" = "$CLAUDE_CONFIG_DIR" ] || exit 18
  state=$(/bin/cat "$HOME/explicit-state")
else
  [ "${CLAUDE_HOME+x}" != x ] || exit 19
  state=$(/bin/cat "$HOME/implicit-state")
fi
[ "$state" = error ] && exit 20
printf '{"loggedIn":%s}' "$state"
[ "$state" = true ]
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let context = crate::claude_auth::ContextEnv { home: root.clone(), path: root.join("bin").to_string_lossy().into_owned(), config_dir: Some(root.join(".claude").to_string_lossy().into_owned()) };
        let mut paths = paths(); paths.home = root.clone(); paths.bin = root.join("bin");
        for (explicit, implicit, migrated) in [("false", "true", true), ("true", "true", false), ("false", "false", false)] {
            std::fs::write(root.join("explicit-state"), explicit).unwrap();
            std::fs::write(root.join("implicit-state"), implicit).unwrap();
            let selected = resolve_legacy_profile(context.clone(), true).unwrap();
            assert_eq!(selected.config_dir.is_none(), migrated);
            let plist = paths.plist("profile-probe");
            std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
            std::fs::write(&plist, render_plist_with_context("profile-probe", &paths, &selected).unwrap()).unwrap();
            let value = read_plist(&plist).unwrap();
            let vars = value["EnvironmentVariables"].as_object().unwrap().iter().map(|(k,v)| (k.clone(), v.as_str().unwrap().to_owned())).collect();
            let installed = crate::claude_auth::ContextEnv::from_environment(&root, &vars).unwrap();
            assert_eq!(installed.config_dir, selected.config_dir);
            assert_eq!(installed.status().unwrap(), if migrated { implicit } else { explicit } == "true");
            assert!(!legacy_default_profile(&value, &root));
        }
        std::fs::write(root.join("explicit-state"), "false").unwrap();
        std::fs::write(root.join("implicit-state"), "true").unwrap();
        assert!(resolve_legacy_profile(context.clone(), false).unwrap().config_dir.is_some(), "explicit selection must not fall back");
        for (explicit, implicit) in [("error", "true"), ("false", "error")] {
            std::fs::write(root.join("explicit-state"), explicit).unwrap();
            std::fs::write(root.join("implicit-state"), implicit).unwrap();
            assert!(resolve_legacy_profile(context.clone(), true).is_err());
        }
        let legacy = serde_json::json!({"EnvironmentVariables": {"CLAUDE_CONFIG_DIR": context.config_dir, "CLAUDE_HOME": context.config_dir}});
        assert!(legacy_default_profile(&legacy, &root));
        let mut explicit = legacy.clone(); explicit["EnvironmentVariables"]["ALONDRA_CLAUDE_PROFILE_MODE"] = "explicit".into();
        assert!(!legacy_default_profile(&explicit, &root));
        let custom = serde_json::json!({"EnvironmentVariables": {"CLAUDE_CONFIG_DIR": root.join("custom"), "CLAUDE_HOME": root.join("custom")}});
        assert!(!legacy_default_profile(&custom, &root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn custom_profile_retry_keeps_auth_plugins_and_watcher_together() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::PermissionsExt;
        let root = scratch_directory.parent().unwrap().join(format!("alondra-profile-retry-{}", std::process::id()));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let root = root.canonicalize().unwrap();
        let mut paths = paths();
        paths.home = root.clone();
        paths.bin = root.join("bin");
        let plist = paths.plist("test-profile");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        let profile = root.join("profile").to_string_lossy().into_owned();
        std::fs::write(&plist, format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>EnvironmentVariables</key><dict><key>CLAUDE_CONFIG_DIR</key><string>{profile}</string></dict></dict></plist>")).unwrap();
        // No environment mutation: retry must recover the persisted profile.
        let context = claude_context(&paths, "test-profile");
        assert_eq!(context.config_dir.as_deref(), Some(profile.as_str()));
        std::fs::write(&plist, render_plist("test-profile", &paths).unwrap()).unwrap();
        let value = read_plist(&plist).unwrap();
        assert_eq!(value["EnvironmentVariables"]["USER"], crate::claude_auth::effective_username().unwrap());
        assert_eq!(value["EnvironmentVariables"]["LOGNAME"], value["EnvironmentVariables"]["USER"]);
        assert_eq!(value["EnvironmentVariables"]["CLAUDE_HOME"], profile);
        assert_eq!(value["EnvironmentVariables"]["CLAUDE_CONFIG_DIR"], profile);
        let script = paths.bin.join("claude");
        std::fs::write(&script, r#"#!/bin/sh
[ "$CLAUDE_HOME" = "$CLAUDE_CONFIG_DIR" ] && [ "$CLAUDE_CONFIG_DIR" = "$HOME/profile" ] || exit 12
case "$1 $2" in
  'plugin list') printf '[{"id":"alondra@alondra","enabled":false}]' ;;
  'plugin marketplace') printf '%s' "$CLAUDE_CONFIG_DIR" > "$HOME/plugin-profile" ;;
  *) exit 13 ;;
esac
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(plugin_list(&context).unwrap().is_array());
        execute(&Step::Run { program: "claude".into(), args: vec!["plugin".into(), "marketplace".into(), "add".into(), "/assets".into()] }, &context).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("plugin-profile")).unwrap(), profile);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn homebrew_formula_uses_stable_opt_paths_for_daemon_and_plugins() {
        let scratch_directory = crate::test_support::Directory::new();
        let root = scratch_directory.parent().unwrap().join(format!("alondra-brew-package-test-{}", std::process::id()));
        let keg = root.join("Cellar/alondra/0.1.0");
        let bin = keg.join("bin");
        let assets = keg.join("libexec/alondra");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(assets.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(assets.join("plugin")).unwrap();
        std::fs::write(bin.join("alondra"), "").unwrap();
        std::fs::write(bin.join("alondrad"), "").unwrap();
        std::fs::write(assets.join(".claude-plugin/marketplace.json"), "{}").unwrap();
        std::fs::create_dir_all(root.join("opt")).unwrap();
        std::os::unix::fs::symlink(&keg, root.join("opt/alondra")).unwrap();
        let stable = root.join("opt/alondra");
        assert_eq!(brew_package_for_executable(&bin.join("alondra")),
            Some((stable.join("bin"), stable.join("libexec/alondra"))));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_new_package_replaces_a_marketplace_at_the_old_location() {
        let seen = Observed { marketplace: true, marketplace_path: Some("/old/alondra".into()), ..fresh() };
        let p = plan(&named("Ana"), None, &Args::default(), &paths(), &seen);
        let commands = runs(&p);
        assert!(commands.iter().any(|c| c == "claude plugin marketplace remove alondra"));
        assert!(commands.iter().any(|c| c == "claude plugin marketplace add /src/alondra"));
    }

    fn paths() -> Paths {
        Paths {
            home: "/Users/ana".into(),
            alondra_home: "/Users/ana/.alondra".into(),
            bin: "/Users/ana/.local/bin".into(),
            repo: Some("/src/alondra".into()),
            uid: 501,
        }
    }

    fn fresh() -> Observed {
        Observed { connected: vec![alondra_core::accounts::Provider::Claude], binaries: true, claude: true, tuicr: true, ..Default::default() }
    }

    fn named(n: &str) -> FileConfig {
        FileConfig { name: Some(n.into()), ..Default::default() }
    }

    #[test]
    fn a_job_that_may_be_loaded_is_unloaded_before_bootstrap_even_with_its_daemon_gone() {
        let seen = Observed {
            plist: Some(render_plist("dev.alondra.alondrad", &paths()).unwrap()),
            daemon_running: false,
            marketplace: true,
            ..fresh()
        };
        let p = plan(&FileConfig::default(), None, &Args::default(), &paths(), &seen);
        assert_eq!(
            runs(&p),
            vec!["launchctl bootout gui/501/dev.alondra.alondrad", "launchctl bootstrap gui/501 /Users/ana/Library/LaunchAgents/dev.alondra.alondrad.plist"]
        );
        assert!(p.steps.iter().filter(|s| matches!(s, Step::Unload { .. })).all(is_launchd_lifecycle_step));
    }

    fn runs(p: &Plan) -> Vec<String> {
        p.steps
            .iter()
            .filter_map(|s| match s {
                Step::Run { program, args } => Some(format!("{program} {}", args.join(" "))),
                Step::Unload { target, .. } => Some(format!("launchctl bootout {target}")),
                _ => None,
            })
            .collect()
    }

    fn writes(p: &Plan) -> Vec<PathBuf> {
        p.steps.iter().filter_map(|s| if let Step::Write { path, .. } = s { Some(path.clone()) } else { None }).collect()
    }

    #[test]
    fn a_fresh_mac_gets_config_service_and_plugin() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let p = plan(&named("Ana"), None, &Args::default(), &paths(), &fresh());
        assert!(p.conflicts.is_empty());
        assert_eq!(p.config, FileConfig { name: Some("Ana".into()), wake: Some(true), ..Default::default() });
        assert_eq!(
            writes(&p),
            vec![PathBuf::from("/Users/ana/.alondra/config.toml"), PathBuf::from("/Users/ana/Library/LaunchAgents/dev.alondra.alondrad.plist")]
        );
        assert_eq!(
            runs(&p),
            vec![
                "launchctl bootstrap gui/501 /Users/ana/Library/LaunchAgents/dev.alondra.alondrad.plist",
                "claude plugin marketplace add /src/alondra",
            ],
            "the plugin is not installed for every session: alondrad loads it into the ones it launches"
        );
        assert!(!p.steps.iter().any(|s| matches!(s, Step::Warn(w) if w.contains("apns.json"))));
    }

    #[test]
    fn an_upgrade_turns_off_the_plugin_a_global_install_enables_once() {
        let list = serde_json::json!([
            {"id": "alondra@alondra", "scope": "user", "enabled": true, "version": "0.1.23"},
            {"id": "alondra-orchestrator@alondra", "scope": "local", "enabled": false},
            {"id": "other@market", "scope": "user", "enabled": true}
        ]);
        let seen = Observed {
            config: Some(plan(&named("Ana"), None, &Args::default(), &paths(), &fresh()).config.render()),
            apns: Some("{}".into()),
            plist: Some(render_plist("dev.alondra.alondrad", &paths()).unwrap()),
            daemon_running: true,
            marketplace: true,
            enabled_globally: enabled_globally(&list),
            ..fresh()
        };
        let p = plan(&FileConfig::default(), None, &Args::default(), &paths(), &seen);
        assert_eq!(runs(&p), vec!["claude plugin disable alondra@alondra --scope user"]);
        assert!(writes(&p).is_empty(), "Claude Code writes its own settings: {:?}", p.steps);

        let after = serde_json::json!([{"id": "alondra@alondra", "scope": "user", "enabled": false}]);
        let again = plan(&FileConfig::default(), None, &Args::default(), &paths(), &Observed { enabled_globally: enabled_globally(&after), ..seen });
        assert!(runs(&again).is_empty(), "a second run changes nothing: {:?}", again.steps);
        assert!(runs(&plan(&FileConfig::default(), None, &Args { skip_plugin: true, ..Default::default() }, &paths(), &Observed { enabled_globally: enabled_globally(&list), ..fresh() })).iter().all(|r| !r.starts_with("claude")));
    }

    #[test]
    fn running_it_again_changes_nothing() {
        let first = plan(&named("Ana"), None, &Args::default(), &paths(), &fresh());
        let seen = Observed {
            config: Some(first.config.render()),
            apns: Some("{}".into()),
            plist: Some(render_plist("dev.alondra.alondrad", &paths()).unwrap()),
            daemon_running: true,
            marketplace: true,
            ..fresh()
        };
        let again = plan(&FileConfig::default(), None, &Args::default(), &paths(), &seen);
        assert!(again.conflicts.is_empty());
        assert!(writes(&again).is_empty(), "{:?}", again.steps);
        assert!(runs(&again).is_empty(), "{:?}", again.steps);
    }

    #[test]
    fn a_changed_config_restarts_the_loaded_service_and_a_changed_plist_reloads_it() {
        let seen = Observed {
            config: Some(named("Ana").render()),
            plist: Some(render_plist("dev.alondra.alondrad", &paths()).unwrap()),
            daemon_running: true,
            marketplace: true,
            ..fresh()
        };
        let p = plan(&FileConfig { public_url: Some("https://x.example".into()), ..Default::default() }, None, &Args::default(), &paths(), &seen);
        assert_eq!(runs(&p), vec!["launchctl kickstart -k gui/501/dev.alondra.alondrad"]);

        let apns = Apns { key_path: "/k.p8".into(), key_id: "K".into(), team_id: "T".into(), topic: "a.b".into() };
        let settled = Observed { config: Some(FileConfig { wake: Some(true), ..named("Ana") }.render()), apns_key_exists: true, ..seen.clone() };
        assert!(runs(&plan(&FileConfig::default(), None, &Args::default(), &paths(), &settled)).is_empty());
        let p = plan(&FileConfig::default(), Some(&apns), &Args::default(), &paths(), &settled);
        assert_eq!(runs(&p), vec!["launchctl kickstart -k gui/501/dev.alondra.alondrad"]);

        let seen = Observed { plist: Some("old".into()), ..seen };
        let p = plan(&FileConfig::default(), None, &Args::default(), &paths(), &seen);
        assert_eq!(
            runs(&p),
            vec!["launchctl bootout gui/501/dev.alondra.alondrad", "launchctl bootstrap gui/501 /Users/ana/Library/LaunchAgents/dev.alondra.alondrad.plist"]
        );
    }

    #[test]
    fn existing_values_are_not_clobbered_without_force() {
        let seen = Observed { config: Some(FileConfig { name: Some("Ana".into()), port: Some(47801), ..Default::default() }.render()), ..fresh() };
        let p = plan(&named("Bea"), None, &Args::default(), &paths(), &seen);
        assert_eq!(p.conflicts, vec!["name: \"Ana\" → \"Bea\" (--force)"]);
        let p = plan(&named("Bea"), None, &Args { force: true, ..Default::default() }, &paths(), &seen);
        assert!(p.conflicts.is_empty());
        assert_eq!((p.config.name.as_deref(), p.config.port), (Some("Bea"), Some(47801)));

        let apns = Apns { key_path: "/k.p8".into(), key_id: "K".into(), team_id: "T".into(), topic: "a.b".into() };
        let other = Apns { key_id: "OTHER".into(), ..apns.clone() };
        let seen = Observed { apns: Some(serde_json::to_string(&other).unwrap()), apns_key_exists: true, ..fresh() };
        let p = plan(&named("Ana"), Some(&apns), &Args::default(), &paths(), &seen);
        assert_eq!(p.conflicts.len(), 1);
        let seen = Observed { apns: Some(serde_json::to_string(&apns).unwrap()), apns_key_exists: true, ..fresh() };
        let p = plan(&named("Ana"), Some(&apns), &Args::default(), &paths(), &seen);
        assert!(p.conflicts.is_empty() && !writes(&p).contains(&PathBuf::from("/Users/ana/.alondra/apns.json")));
    }

    #[test]
    fn apns_is_written_private_and_skips_are_honoured() {
        let apns = Apns { key_path: "/k.p8".into(), key_id: "K".into(), team_id: "T".into(), topic: "a.b".into() };
        let args = Args { skip_service: true, skip_plugin: true, ..Default::default() };
        let p = plan(&named("Ana"), Some(&apns), &args, &paths(), &Observed { apns_key_exists: true, ..fresh() });
        assert!(p.steps.iter().any(|s| matches!(s, Step::Write { path, private: true, .. } if path.ends_with("apns.json"))));
        assert!(runs(&p).is_empty());
        assert!(!p.steps.iter().any(|s| matches!(s, Step::Warn(_))));
    }

    #[test]
    fn the_plist_points_at_this_user_and_label() {
        let plist = render_plist("com.example.alondra", &paths()).unwrap();
        assert!(plist.contains("<string>com.example.alondra</string>"));
        assert!(plist.contains("<string>/Users/ana/.local/bin/alondrad</string>"));
        assert!(plist.contains("<string>/Users/ana/.alondra/alondrad.log</string>"));
        assert!(!plist.contains('@'));
        assert!(!plist.contains("alondrad-launcher"), "a build without a launcher runs alondrad directly");
    }

    fn shipped_launcher(root: &Path) -> Paths {
        let bin = root.join("opt/alondra/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("alondrad"), "").unwrap();
        std::fs::write(bin.join("alondra"), "").unwrap();
        std::fs::write(bin.join(alondra_core::launcher::NAME), "").unwrap();
        Paths { bin, alondra_home: root.join("home/.alondra"), ..paths() }
    }

    #[test]
    fn launchd_runs_the_launcher_at_its_fixed_path_with_alondrad_through_the_stable_link() {
        let root = crate::test_support::Directory::new();
        let paths = shipped_launcher(&root);
        let plist = render_plist("com.example.alondra", &paths).unwrap();
        let launcher = alondra_core::launcher::installed_path(&paths.alondra_home);
        let arguments = format!("<array>\n        <string>{}</string>\n        <string>{}</string>\n    </array>", launcher.display(), paths.bin.join("alondrad").display());
        assert!(plist.contains(&arguments), "{plist}");
        assert!(!plist.contains('@'));
    }

    #[test]
    fn an_outdated_launcher_is_copied_before_launchd_loads_the_service() {
        let root = crate::test_support::Directory::new();
        let paths = shipped_launcher(&root);
        let p = plan(&named("Ana"), None, &Args::default(), &paths, &Observed { launcher_outdated: true, ..fresh() });
        let copy = p.steps.iter().position(|s| matches!(s, Step::Launcher { installed, .. } if *installed == alondra_core::launcher::installed_path(&paths.alondra_home)));
        let bootstrap = p.steps.iter().position(|s| matches!(s, Step::Run { args, .. } if args.first().map(String::as_str) == Some("bootstrap")));
        assert!(copy.is_some() && copy < bootstrap, "{:?}", p.steps);
        let current = plan(&named("Ana"), None, &Args::default(), &paths, &fresh());
        assert!(!current.steps.iter().any(|s| matches!(s, Step::Launcher { .. })));
    }

    #[test]
    fn migrating_an_old_plist_keeps_every_behaviour() {
        let env: BTreeMap<String, String> = [
            ("PATH", "/x"),
            ("ALONDRA_PUBLIC_URL", "https://alondra.example.dev"),
            ("ALONDRA_LISTEN", "0.0.0.0"),
            ("ALONDRA_WAKE", "1"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let c = migrate_env(Some("com.example.alondra"), &env);
        assert_eq!(
            c,
            FileConfig {
                public_url: Some("https://alondra.example.dev".into()),
                listen: Some([0, 0, 0, 0].into()),
                wake: Some(true),
                legacy_bridge: Some(true),
                service_label: Some("com.example.alondra".into()),
                ..Default::default()
            }
        );
        let r = Config::resolve("/h".into(), &c.overlay(&named("Ana")), |_| None, || None);
        assert!(r.wake && r.legacy_bridge && r.listen.is_unspecified());
        assert_eq!((r.port, r.service_label.as_str()), (alondra_core::DEFAULT_PORT, "com.example.alondra"));
    }

    #[test]
    fn the_repo_is_found_from_any_subdirectory() {
        let scratch_directory = crate::test_support::Directory::new();
        let root = scratch_directory.parent().unwrap().join(format!("alondra-repo-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("plugin")).unwrap();
        std::fs::create_dir_all(root.join("crates/x")).unwrap();
        std::fs::write(root.join(".claude-plugin/marketplace.json"), "{}").unwrap();
        assert_eq!(find_repo(&root.join("crates/x")), Some(root.clone()));
        assert_eq!(find_repo(Path::new("/")), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_older_service_blocks_a_second_one() {
        let seen = Observed { other_services: vec!["/Users/ana/Library/LaunchAgents/old.plist".into()], ..fresh() };
        let p = plan(&named("Ana"), None, &Args::default(), &paths(), &seen);
        assert_eq!(p.conflicts.len(), 1);
        assert!(p.conflicts[0].contains("--from-plist /Users/ana/Library/LaunchAgents/old.plist"));
        let p = plan(&named("Ana"), None, &Args { skip_service: true, ..Default::default() }, &paths(), &seen);
        assert!(p.conflicts.is_empty());
    }

    #[test]
    fn without_binaries_the_service_is_not_started() {
        let p = plan(&named("Ana"), None, &Args { skip_plugin: true, ..Default::default() }, &paths(), &Observed { binaries: false, ..fresh() });
        assert!(runs(&p).is_empty());
        assert!(p.steps.iter().any(|s| matches!(s, Step::Warn(w) if w.contains("make install"))));
    }
}


/// Whether the LaunchAgent runs this HOME's alondrad, directly or through the launcher.
fn recovery_job_matches(job: &Value, home: &Path, daemon: &Path) -> bool {
    let job_home = job.pointer("/EnvironmentVariables/ALONDRA_HOME").and_then(Value::as_str);
    let launcher = alondra_core::launcher::installed_path(home);
    job_home.is_some_and(|h| Path::new(h) == home) && job_program(job).is_some_and(|p| p == daemon || p == launcher)
}

fn stop_for_recovery(paths: &Paths, label: &str) -> Result<()> {
    if !alondra_core::home_lock::in_use(&paths.alondra_home)? { return Ok(()); }
    let job = read_plist(&paths.plist(label)).context("cannot inspect daemon ownership before recovery")?;
    anyhow::ensure!(recovery_job_matches(&job, &paths.alondra_home, &paths.bin.join("alondrad")),
        "cannot prove launchd job belongs to this HOME and executable; stop its daemon explicitly before recovery");
    // Never kill arbitrary PIDs or infer ownership from a TCP port.
    unload(&format!("gui/{}/{label}", paths.uid), &paths.alondra_home, launchctl, 50, Duration::from_millis(100))
}

#[cfg(test)] mod recovery_job_tests {
    use super::*;
    fn job(program: &str, home: &str) -> Value {
        serde_json::json!({"ProgramArguments": [program], "EnvironmentVariables": {"ALONDRA_HOME": home}})
    }
    #[test] fn only_exact_home_and_executable_are_owned() {
        let owned = job("/example/bin/alondrad", "/example/home");
        assert!(recovery_job_matches(&owned,Path::new("/example/home"),Path::new("/example/bin/alondrad")));
        assert!(!recovery_job_matches(&owned,Path::new("/other/home"),Path::new("/example/bin/alondrad")));
        assert!(!recovery_job_matches(&owned,Path::new("/example/home"),Path::new("/other/bin/alondrad")));
        assert!(!recovery_job_matches(&serde_json::json!({"ProgramArguments": ["/example/bin/alondrad"]}),Path::new("/example/home"),Path::new("/example/bin/alondrad")));
    }
    #[test] fn the_launcher_of_the_same_home_is_owned_too() {
        let owned = job("/example/home/bin/alondrad-launcher", "/example/home");
        assert!(recovery_job_matches(&owned,Path::new("/example/home"),Path::new("/example/bin/alondrad")));
        assert!(!recovery_job_matches(&owned,Path::new("/other/home"),Path::new("/example/bin/alondrad")));
    }
}

#[cfg(test)]
mod brew_upgrade_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn output(status: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(status << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn fixture_home() -> crate::test_support::Directory {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        fixture_home_with_nonce(nonce)
    }

    fn fixture_home_with_nonce(_nonce: u128) -> crate::test_support::Directory {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        home
    }

    #[test]
    fn fixture_homes_do_not_collide_when_clock_repeats() {
        struct Homes(Vec<crate::test_support::Directory>);
        impl Drop for Homes {
            fn drop(&mut self) { for path in &self.0 { let _ = std::fs::remove_dir_all(path); } }
        }
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let mut homes = Homes(vec![fixture_home_with_nonce(nonce)]);
        homes.0.push(fixture_home_with_nonce(nonce));
        assert_ne!(homes.0[0].to_path_buf(), homes.0[1].to_path_buf());
        assert!(homes.0.iter().all(|path| path.is_dir()));
    }

    #[test]
    fn unloading_asks_launchd_once_and_trusts_only_the_home_lock() {
        let home = fixture_home();
        for bootout in [0, 113] {
            let mut calls = Vec::new();
            unload("gui/501/dev.alondra.alondrad", &home, |args| {
                calls.push(args.iter().map(|value| (*value).to_owned()).collect::<Vec<_>>());
                Ok(output(bootout, "", "any text at all"))
            }, 3, Duration::ZERO).unwrap();
            assert_eq!(calls, vec![vec!["bootout", "gui/501/dev.alondra.alondrad"]]);
        }
    }

    #[test]
    fn unloading_fails_while_a_daemon_still_holds_the_home() {
        let home = fixture_home();
        let _daemon = alondra_core::home_lock::HomeLock::shared(&home).unwrap();
        let result = unload("gui/501/dev.alondra.alondrad", &home, |_| Ok(output(0, "", "")), 3, Duration::ZERO);
        assert!(result.is_err());
    }

    #[test]
    fn deferred_lifecycle_steps_are_only_launchd_service_changes() {
        for verb in ["bootout", "bootstrap", "kickstart"] {
            assert!(is_launchd_lifecycle_step(&Step::Run {
                program: "launchctl".into(),
                args: vec![verb.into()],
            }));
        }
        assert!(!is_launchd_lifecycle_step(&Step::Run {
            program: "claude".into(),
            args: vec!["plugin".into()],
        }));
    }

    #[test]
    fn setup_releases_shared_home_lock_before_bootstrap() {
        let home = fixture_home();
        let mut lock = Some(alondra_core::home_lock::HomeLock::shared(&home).unwrap());
        release_home_lock_then(&mut lock, || {
            let exclusive = alondra_core::home_lock::HomeLock::recovery(&home)?;
            drop(exclusive);
            Ok(())
        }).unwrap();
        assert!(lock.is_none());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn global_running_daemon_blocks_bootstrap_and_setup_retry() {
        let called = std::cell::Cell::new(false);
        let result = bootstrap_then_record(&mut None, || {
            ensure_no_running_daemons(|| Ok(vec![42]))?;
            called.set(true);
            Ok(())
        }, || Ok(()));
        assert!(result.is_err());
        assert!(!called.get());
    }

    #[test]
    fn daemon_probe_error_fails_closed_without_bootstrap() {
        let called = std::cell::Cell::new(false);
        let result = bootstrap_then_record(&mut None, || {
            ensure_no_running_daemons(|| anyhow::bail!("pgrep unavailable"))?;
            called.set(true);
            Ok(())
        }, || Ok(()));
        assert!(result.is_err());
        assert!(!called.get());
    }

    #[test]
    fn failed_bootstrap_preserves_old_stamp_for_safe_retry() {
        let stamp = std::cell::RefCell::new("old-keg");
        let failed = bootstrap_then_record(&mut None, || anyhow::bail!("bootstrap failed"), || {
            *stamp.borrow_mut() = "new-keg";
            Ok(())
        });
        assert!(failed.is_err());
        assert_eq!(*stamp.borrow(), "old-keg");

        bootstrap_then_record(&mut None, || Ok(()), || {
            *stamp.borrow_mut() = "new-keg";
            Ok(())
        }).unwrap();
        assert_eq!(*stamp.borrow(), "new-keg");
    }
}


/// `alondra mac reset-identity`: once the person types this Mac's name, this Mac's service gets a
/// new identity, then `repair` finishes the setup. Cancelling changes nothing.
pub(crate) fn reset_identity(repair: impl FnOnce() -> Result<()>) -> Result<()> {
    let install = inspect();
    if install.dev_over_brew {
        bail!(tr!("Alondra is installed with Homebrew; a dev build would point the user's daemon at itself. Use the e2e sandbox (make e2e-sandbox), or set FORCE_DEV_INSTALL=1 if you really mean it", "Alondra está instalada con Homebrew; una build de desarrollo apuntaría el daemon del usuario a sí misma. Usa el sandbox e2e (make e2e-sandbox), o define FORCE_DEV_INSTALL=1 si de verdad lo quieres"));
    }
    let outcome = crate::service_setup::recover(&install.paths.alondra_home, &crate::client::computer_name(),
        || stop_for_recovery(&install.paths, &install.label), &mut std::io::stdin().lock(), &mut std::io::stdout().lock())?;
    continue_after_recovery(outcome, repair)
}

// Keep the decision in the caller: cancellation must not enter normal setup,
// which can otherwise log in, enroll, launch services and modify plugins.
fn continue_after_recovery(outcome: crate::service_setup::RecoveryOutcome, resume: impl FnOnce() -> Result<()>) -> Result<()> {
    match outcome {
        crate::service_setup::RecoveryOutcome::Cancelled => Ok(()),
        crate::service_setup::RecoveryOutcome::Committed => resume(),
    }
}

#[cfg(test)] mod recovery_continuation_tests {
    use super::*;
    #[test] fn cancelled_recovery_never_enters_normal_setup_but_committed_does() {
        continue_after_recovery(crate::service_setup::RecoveryOutcome::Cancelled,
            || panic!("cancelled recovery must not run auth/enrollment/launchd/plugins")).unwrap();
        let mut entered = false;
        continue_after_recovery(crate::service_setup::RecoveryOutcome::Committed,
            || { entered = true; Ok(()) }).unwrap();
        assert!(entered, "control: completed recovery must continue ordinary setup");
    }
    #[test]
    fn codex_only_and_unconnected_installations_do_not_plan_claude_commands() {
        for connected in [vec![], vec![alondra_core::accounts::Provider::Codex]] {
            let observed = Observed { connected, claude: false, binaries: true, tuicr: true, ..Default::default() };
            let paths = Paths { home: "/fixture/user".into(), alondra_home: "/fixture/user/.alondra".into(), bin: "/fixture/bin".into(), repo: Some("/fixture/assets".into()), uid: 501 };
            let planned = plan(&FileConfig::default(), None, &Args::default(), &paths, &observed);
            assert!(planned.steps.iter().all(|step| !matches!(step, Step::Run { program, .. } if program == "claude")));
            assert!(!planned.steps.iter().any(|step| matches!(step, Step::Warn(text) if text.contains("Claude Code"))));
        }
    }

}
