use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use alondra_core::config::{self, Config, FileConfig};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const TEMPLATE: &str = include_str!("../../../launchd/alondra.plist.in");
pub const MARKETPLACE: &str = "alondra";

#[derive(clap::Args, Default)]
pub struct Args {
    #[arg(long, help = tr!("Name of the person the nodes talk to", "Nombre de la persona con la que hablan los nodos"))]
    pub name: Option<String>,
    #[arg(long, value_name = "DIRECTORY", help = tr!("Projects folder (created if missing; default ~/Projects)", "Carpeta de proyectos (se crea si falta; por defecto ~/Projects)"))]
    pub projects_root: Option<PathBuf>,
    #[arg(long, help = tr!("URL the iPhone uses to reach this Mac", "URL por la que el iPhone llega a este Mac"))]
    pub public_url: Option<String>,
    #[arg(long, help = tr!("Address alondrad listens on (0.0.0.0 to accept the LAN)", "Dirección en la que escucha alondrad (0.0.0.0 para aceptar la red local)"))]
    pub listen: Option<IpAddr>,
    #[arg(long)]
    pub port: Option<u16>,
    #[arg(long, value_name = "true|false", help = tr!("Bridge with the old open-questions.json", "Puente con el open-questions.json antiguo"))]
    pub legacy_bridge: Option<bool>,
    #[arg(long, value_name = "true|false", help = tr!("Let the service type messages into stopped nodes (default true)", "Deja que el servicio escriba los mensajes en nodos parados (por defecto true)"))]
    pub wake: Option<bool>,
    #[arg(long)]
    pub retention_hours: Option<f64>,
    #[arg(long, help = tr!("launchd label of the service", "Etiqueta de launchd del servicio"))]
    pub service_label: Option<String>,
    #[arg(long, help = tr!("APNs .p8 key", "Llave .p8 de APNs"))]
    pub apns_key_path: Option<PathBuf>,
    #[arg(long)]
    pub apns_key_id: Option<String>,
    #[arg(long)]
    pub apns_team_id: Option<String>,
    #[arg(long, help = tr!("Bundle ID of your iOS app", "Bundle ID de tu app de iOS"))]
    pub apns_topic: Option<String>,
    #[arg(long, value_name = "PLIST", help = tr!(
        "Take the settings from an existing launchd plist (migration from env vars)",
        "Toma los ajustes de un plist de launchd existente (migración desde variables de entorno)"
    ))]
    pub from_plist: Option<PathBuf>,
    #[arg(long, help = tr!("Alondra repo (default: found from the current directory)", "Repo de Alondra (por defecto: se busca desde el directorio actual)"))]
    pub repo: Option<PathBuf>,
    #[arg(long, help = tr!("Where alondrad and alondra are installed (default ~/.local/bin)", "Dónde están instalados alondrad y alondra (por defecto ~/.local/bin)"))]
    pub bin: Option<PathBuf>,
    #[arg(long)]
    pub skip_service: bool,
    #[arg(long)]
    pub skip_plugin: bool,
    #[arg(long)]
    pub skip_pair: bool,
    #[arg(long, help = "Explicitly recover the service identity; requires local confirmation and new client pairing")]
    pub recover_service: bool,
    #[arg(long, help = tr!("Overwrite config.toml / apns.json values that differ", "Sobrescribe los valores de config.toml / apns.json que cambien"))]
    pub force: bool,
    #[arg(long, help = tr!("Print what would be done without doing it", "Enseña lo que haría sin hacerlo"))]
    pub dry_run: bool,
    #[arg(long, help = tr!("Never ask; use flags and defaults", "No pregunta nunca; usa los parámetros y los valores por defecto"))]
    pub no_input: bool,
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
}

#[derive(Default, Clone)]
pub struct Observed {
    pub config: Option<String>,
    pub apns: Option<String>,
    pub plist: Option<String>,
    pub service_loaded: bool,
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
}

#[derive(Debug, PartialEq)]
pub enum Step {
    Write { path: PathBuf, contents: String, private: bool },
    Unchanged { path: PathBuf },
    Run { program: String, args: Vec<String> },
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
            .and_then(|v| v.get("EnvironmentVariables")?.get("CLAUDE_CONFIG_DIR").or_else(|| v.get("EnvironmentVariables")?.get("CLAUDE_HOME"))?.as_str().map(str::to_owned));
    }
    context
}

pub fn render_plist(label: &str, paths: &Paths) -> String {
    let context = claude_context(paths, label);
    let profile = context.config_dir.clone().unwrap_or_else(|| paths.home.join(".claude").to_string_lossy().into_owned());
    let config = format!("<key>CLAUDE_CONFIG_DIR</key><string>{0}</string><key>CLAUDE_HOME</key><string>{0}</string>", xml_escape(&profile));
    TEMPLATE
        .replace("@CLAUDE_CONFIG_ENV@", &config)
        .replace("@PATH@", &xml_escape(&context.path))
        .replace("@LABEL@", label)
        .replace("@BIN@", &xml_escape(&paths.bin.display().to_string()))
        .replace("@HOME@", &xml_escape(&paths.home.display().to_string()))
        .replace("@ALONDRA_HOME@", &xml_escape(&paths.alondra_home.display().to_string()))
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
        let plist = render_plist(&label, paths);
        let domain = format!("gui/{}", paths.uid);
        let target = format!("{domain}/{label}");
        if !seen.binaries {
            let bin = paths.bin.display();
            steps.push(Step::Warn(trf!(
                "{bin}/alondrad is missing, so the service is not started: run `make install` in the repo and then setup again.",
                "Falta {bin}/alondrad, así que no arranco el servicio: ejecuta `make install` en el repo y después setup otra vez."
            )));
        }
        let plist_changed = seen.plist.as_deref() != Some(plist.as_str());
        if plist_changed {
            steps.push(Step::Write { path: plist_path.clone(), contents: plist, private: false });
        } else {
            steps.push(Step::Unchanged { path: plist_path.clone() });
        }
        let run = |args: &[&str]| Step::Run { program: "launchctl".into(), args: args.iter().map(|s| s.to_string()).collect() };
        if seen.binaries && (plist_changed || !seen.service_loaded) {
            if seen.service_loaded {
                steps.push(run(&["bootout", &target]));
            }
            steps.push(run(&["bootstrap", &domain, &plist_path.display().to_string()]));
        } else if seen.binaries && restart {
            steps.push(run(&["kickstart", "-k", &target]));
        }
    }

    if !args.skip_plugin {
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

fn brew_package_for_executable(executable: &Path) -> Option<(PathBuf, PathBuf)> {
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

pub fn local_host_name() -> Option<String> {
    let out = Command::new("/usr/sbin/scutil").args(["--get", "LocalHostName"]).output().ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !name.is_empty()).then_some(name)
}

pub fn lan_url(port: u16) -> Option<String> {
    local_host_name().map(|h| format!("http://{h}.local:{port}"))
}

fn apns_from(args: &Args) -> Result<Option<Apns>> {
    let parts = (&args.apns_key_path, &args.apns_key_id, &args.apns_team_id, &args.apns_topic);
    match parts {
        (None, None, None, None) => Ok(None),
        (Some(p), Some(k), Some(t), Some(topic)) => {
            let key_path = if p.is_absolute() { p.clone() } else { std::env::current_dir()?.join(p) };
            Ok(Some(Apns { key_path, key_id: k.clone(), team_id: t.clone(), topic: topic.clone() }))
        }
        _ => Err(anyhow!(tr!(
            "APNs needs the four values: --apns-key-path --apns-key-id --apns-team-id --apns-topic",
            "APNs necesita los cuatro valores: --apns-key-path --apns-key-id --apns-team-id --apns-topic"
        ))),
    }
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

pub fn service_loaded(uid: u32, label: &str) -> bool {
    command_ok("launchctl", &["print", &format!("gui/{uid}/{label}")]).is_some()
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

fn marketplace_path(context: &crate::claude_auth::ContextEnv) -> Option<PathBuf> {
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
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|t| t.contains("/alondrad</string>")))
        .collect();
    found.sort();
    found
}

fn observe(paths: &Paths, label: &str, apns: Option<&Apns>, args: &Args) -> Observed {
    let read = |p: PathBuf| std::fs::read_to_string(p).ok();
    let context = claude_context(paths, label);
    let plugins = if args.skip_plugin { None } else { plugin_list(&context) };
    Observed {
        config: read(paths.config()),
        apns: read(paths.apns()),
        plist: read(paths.plist(label)),
        service_loaded: !args.skip_service && service_loaded(paths.uid, label),
        other_services: other_services(paths, label),
        binaries: paths.bin.join("alondrad").is_file() && paths.bin.join("alondra").is_file(),
        apns_key_exists: apns.is_some_and(|a| a.key_path.is_file()),
        claude: args.skip_plugin || claude_output(&context, &["--version"]).is_some(),
        marketplace: !args.skip_plugin && has_marketplace(&context),
        marketplace_path: if args.skip_plugin { None } else { marketplace_path(&context) },
        enabled_globally: plugins.as_ref().map(enabled_globally).unwrap_or_default(),
        tuicr: command_ok("tuicr", &["--version"]).is_some(),
        brew: command_ok("brew", &["--version"]).is_some(),
    }
}

pub fn run(args: Args) -> Result<()> {
    let env = |k: &str| std::env::var(k).ok();
    let home = config::home_dir(&env);
    let alondra_home = config::alondra_home(&env);
    let packaged = installed_package();
    let brew_executable = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok())
        .filter(|p| brew_package_for_executable(p).is_some());
    let paths = Paths {
        bin: args.bin.clone().or_else(|| packaged.as_ref().map(|(bin, _)| bin.clone())).unwrap_or_else(|| home.join(".local/bin")),
        repo: args.repo.clone().or_else(|| packaged.as_ref().map(|(_, assets)| assets.clone())).or_else(|| std::env::current_dir().ok().and_then(|d| find_repo(&d))),
        uid: unsafe { libc::getuid() },
        home,
        alondra_home,
    };
    let existing = std::fs::read_to_string(paths.config()).ok().and_then(|t| FileConfig::parse(&t).ok());
    let wanted = wanted_from(&args, existing.as_ref())?;
    let apns = apns_from(&args)?;
    let label = wanted
        .service_label
        .clone()
        .or_else(|| existing.as_ref().and_then(|e| e.service_label.clone()))
        .unwrap_or_else(|| config::DEFAULT_LABEL.into());
    if args.recover_service {
        if args.dry_run {
            println!("would confirm service recovery, stop only this HOME's daemon, preserve tasks/receipts, replace service identity and require new pairings");
            return Ok(());
        }
        if args.no_input { bail!("--recover-service requires local confirmation; remove --no-input"); }
        if args.skip_service { bail!("--recover-service cannot be combined with --skip-service"); }
        crate::service_setup::recover(&paths.alondra_home, || stop_for_recovery(&paths, &label),
            &mut std::io::stdin().lock(), &mut std::io::stdout().lock())?;
        let mut resumed = args;
        resumed.recover_service = false;
        return run(resumed);
    }
    // Shared ownership excludes a recovery writer; check pending after locking,
    // before setup can load or change any service trust or launch the daemon.
    let _home_lock = if args.dry_run { None } else {
        let lock = alondra_core::home_lock::HomeLock::shared(&paths.alondra_home)?;
        alondra_core::home_lock::ensure_ready(&paths.alondra_home)?;
        Some(lock)
    };
    if let (Ok(config), Ok(watcher)) = (std::env::var("CLAUDE_CONFIG_DIR"), std::env::var("CLAUDE_HOME")) {
        if config != watcher { bail!("CLAUDE_CONFIG_DIR and CLAUDE_HOME disagree; set one Claude profile before alondra setup"); }
    }
    let context = claude_context(&paths, &label);
    let seen = observe(&paths, &label, apns.as_ref(), &args);
    let plan = plan(&wanted, apns.as_ref(), &args, &paths, &seen);

    if !plan.conflicts.is_empty() {
        eprintln!("{}", tr!("Not touching anything, because:", "No toco nada, por esto:"));
        for c in &plan.conflicts {
            eprintln!("  {c}");
        }
        bail!(tr!("setup stopped", "setup detenido"));
    }
    if !args.dry_run && !args.skip_service && !seen.binaries {
        bail!("alondrad is missing from the configured installation; install the Homebrew formula or pass --bin, then rerun `alondra setup`");
    }
    let projects = crate::setup_projects::select(
        paths.alondra_home.join(alondra_core::options::FILE_NAME), &paths.home,
        &std::env::current_dir()?, args.projects_root.as_deref(), args.no_input,
        args.dry_run, &mut std::io::stdin().lock(), &mut std::io::stdout().lock(),
    )?;
    projects.apply(args.dry_run)?;
    if !args.dry_run {
        std::fs::create_dir_all(paths.alondra_home.join("runtime/orchestrator"))?;
    }
    if args.dry_run {
        println!("would verify Claude Code authentication in the daemon environment (no login during dry run)");
    } else {
        context.ensure(args.no_input)?;
    }
    let service_created = if !args.dry_run {
        crate::service_setup::enroll(&paths.alondra_home)?
    } else {
        println!("would sign in if needed and enroll this Mac with the Alondra service");
        false
    };
    for step in &plan.steps {
        print_step(step, args.dry_run);
        if !args.dry_run {
            execute(step, &context)?;
        }
    }
    let launched_by_plan = plan.steps.iter().any(|step| {
        matches!(step, Step::Run { program, args } if program == "launchctl"
            && matches!(args.first().map(String::as_str), Some("bootstrap" | "kickstart")))
    });
    let restarted_for_enrollment = service_created && seen.binaries && seen.service_loaded && !launched_by_plan;
    if restarted_for_enrollment {
        let target = format!("gui/{}/{}", paths.uid, label);
        execute(&Step::Run {
            program: "launchctl".into(),
            args: vec!["kickstart".into(), "-k".into(), target],
        }, &context)?;
    }
    if !args.dry_run && !args.skip_service && seen.binaries
        && let Some(executable) = brew_executable
    {
        let stamp = paths.alondra_home.join("brew-keg-path");
        let current = executable.to_string_lossy().into_owned();
        let changed = std::fs::read_to_string(&stamp).ok().as_deref() != Some(current.as_str());
        if changed && seen.service_loaded && !launched_by_plan && !restarted_for_enrollment {
            let target = format!("gui/{}/{}", paths.uid, label);
            execute(&Step::Run {
                program: "launchctl".into(),
                args: vec!["kickstart".into(), "-k".into(), target],
            }, &context)?;
        }
        if changed {
            execute(&Step::Write { path: stamp, contents: current, private: true }, &context)?;
        }
    }
    if args.dry_run {
        println!("{}", tr!("(dry run: nothing was written or run)", "(ensayo: no se ha escrito ni ejecutado nada)"));
        return Ok(());
    }
    if args.skip_pair || args.skip_service {
        return Ok(());
    }
    if !crate::service_pair::has_paired_device(&paths.alondra_home)? {
        crate::service_pair::run(&paths.alondra_home)?;
    }
    Ok(())
}

fn print_step(step: &Step, dry: bool) {
    match step {
        Step::Write { path, contents, private } => {
            let p = path.display();
            if dry {
                println!("{}", trf!("would write {p}:", "escribiría {p}:"));
                if !private {
                    for l in contents.lines() {
                        println!("    {l}");
                    }
                }
            } else {
                println!("{}", trf!("✓ wrote {p}", "✓ escrito {p}"));
            }
        }
        Step::Unchanged { path } => {
            let p = path.display();
            println!("{}", trf!("= {p} unchanged", "= {p} sin cambios"));
        }
        Step::Run { program, args } => {
            let cmd = format!("{program} {}", args.join(" "));
            if dry {
                println!("{}", trf!("would run: {cmd}", "ejecutaría: {cmd}"));
            } else {
                println!("{}", trf!("✓ {cmd}", "✓ {cmd}"));
            }
        }
        Step::Warn(w) => println!("! {w}"),
    }
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
            if program == "launchctl"
                && let [verb, target] = args.as_slice()
                && verb == "bootout"
            {
                for _ in 0..50 {
                    if command_ok("launchctl", &["print", target]).is_none() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        Step::Unchanged { .. } | Step::Warn(_) => {}
    }
    Ok(())
}

pub fn pairing_url(config: &Config) -> Option<String> {
    config.public_url.clone().or_else(|| (!config.listen.is_loopback()).then(|| lan_url(config.port)).flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_profile_retry_keeps_auth_plugins_and_watcher_together() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("alondra-profile-retry-{}", std::process::id()));
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
        std::fs::write(&plist, render_plist("test-profile", &paths)).unwrap();
        let value = read_plist(&plist).unwrap();
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
        let root = std::env::temp_dir().join(format!("alondra-brew-package-test-{}", std::process::id()));
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
        Observed { binaries: true, claude: true, tuicr: true, ..Default::default() }
    }

    fn named(n: &str) -> FileConfig {
        FileConfig { name: Some(n.into()), ..Default::default() }
    }

    fn runs(p: &Plan) -> Vec<String> {
        p.steps
            .iter()
            .filter_map(|s| match s {
                Step::Run { program, args } => Some(format!("{program} {}", args.join(" "))),
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
            plist: Some(render_plist("dev.alondra.alondrad", &paths())),
            service_loaded: true,
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
            plist: Some(render_plist("dev.alondra.alondrad", &paths())),
            service_loaded: true,
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
            plist: Some(render_plist("dev.alondra.alondrad", &paths())),
            service_loaded: true,
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
        let plist = render_plist("com.example.alondra", &paths());
        assert!(plist.contains("<string>com.example.alondra</string>"));
        assert!(plist.contains("<string>/Users/ana/.local/bin/alondrad</string>"));
        assert!(plist.contains("<string>/Users/ana/.alondra/alondrad.log</string>"));
        assert!(!plist.contains('@'));
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
        let root = std::env::temp_dir().join(format!("alondra-repo-{}", std::process::id()));
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


fn recovery_job_matches(text: &str, home: &Path, daemon: &Path) -> bool {
    fn values<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
        text.lines().filter_map(|line| line.trim().strip_prefix(key)).map(str::trim).collect()
    }
    let homes = values(text, "ALONDRA_HOME => ");
    let programs = values(text, "program = ");
    homes.len() == 1 && programs.len() == 1
        && Path::new(homes[0]) == home && Path::new(programs[0]) == daemon
}

fn stop_for_recovery(paths: &Paths, label: &str) -> Result<()> {
    if !service_loaded(paths.uid, label) { return Ok(()); }
    let target = format!("gui/{}/{label}", paths.uid);
    let state = Command::new("launchctl").args(["print", &target]).output()?;
    anyhow::ensure!(state.status.success(), "cannot inspect daemon ownership before recovery");
    let state = String::from_utf8(state.stdout)?;
    anyhow::ensure!(recovery_job_matches(&state, &paths.alondra_home, &paths.bin.join("alondrad")),
        "cannot prove launchd job belongs to this HOME and executable; stop its daemon explicitly before recovery");
    let stopped = Command::new("launchctl").args(["bootout", &target]).output()?;
    anyhow::ensure!(stopped.status.success(), "could not unload this HOME's daemon; recovery not started");
    // bootout removes KeepAlive before we attempt the exclusive HOME lock. Never
    // kill arbitrary PIDs or infer ownership from a TCP port.
    Ok(())
}

#[cfg(test)] mod recovery_job_tests {
    use super::*;
    #[test] fn only_exact_home_and_executable_are_owned() {
        let text="program = /example/bin/alondrad\n environment = {\n ALONDRA_HOME => /example/home\n }";
        assert!(recovery_job_matches(text,Path::new("/example/home"),Path::new("/example/bin/alondrad")));
        assert!(!recovery_job_matches(text,Path::new("/other/home"),Path::new("/example/bin/alondrad")));
        assert!(!recovery_job_matches(text,Path::new("/example/home"),Path::new("/other/bin/alondrad")));
        assert!(!recovery_job_matches(&(text.to_owned()+"\n ALONDRA_HOME => /other/home"),Path::new("/example/home"),Path::new("/example/bin/alondrad")));
    }
}
