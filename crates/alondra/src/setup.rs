use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
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

pub fn render_plist(label: &str, paths: &Paths) -> String {
    TEMPLATE
        .replace("@LABEL@", label)
        .replace("@BIN@", &paths.bin.display().to_string())
        .replace("@HOME@", &paths.home.display().to_string())
        .replace("@ALONDRA_HOME@", &paths.alondra_home.display().to_string())
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
        (None, None) => steps.push(Step::Warn(
            tr!(
                "No apns.json: the iPhone gets no notifications. Rerun with --apns-key-path --apns-key-id --apns-team-id --apns-topic.",
                "Sin apns.json: el iPhone no recibe notificaciones. Vuelve a ejecutarlo con --apns-key-path --apns-key-id --apns-team-id --apns-topic."
            )
            .into(),
        )),
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
            if !seen.marketplace {
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

fn read_plist(path: &Path) -> Result<Value> {
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

fn prompt(question: &str, default: &str) -> Result<String> {
    let mut out = std::io::stderr();
    if default.is_empty() {
        write!(out, "{question}: ")?;
    } else {
        write!(out, "{question} [{default}]: ")?;
    }
    out.flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() { default.to_string() } else { line.to_string() })
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
    let interactive = !args.no_input && std::io::stdin().is_terminal();
    if !known(&wanted, |c| c.name.is_some()) {
        let fallback = config::system_name().unwrap_or_default();
        let name = if interactive { prompt(tr!("Your name (the nodes address you by it)", "Tu nombre (los nodos te llaman así)"), &fallback)? } else { fallback };
        wanted.name = Some(name).filter(|n| !n.is_empty());
    }
    if interactive && !known(&wanted, |c| c.public_url.is_some()) {
        let port = wanted.port.or(existing.and_then(|e| e.port)).unwrap_or(alondra_core::DEFAULT_PORT);
        let suggestion = lan_url(port).unwrap_or_default();
        let url = prompt(
            tr!(
                "URL the iPhone uses to reach this Mac (LAN, Tailscale or a tunnel; empty for none)",
                "URL por la que el iPhone llega a este Mac (red local, Tailscale o un túnel; vacío para ninguna)"
            ),
            &suggestion,
        )?;
        if !url.is_empty() {
            if url == suggestion && !known(&wanted, |c| c.listen.is_some()) {
                wanted.listen = Some([0, 0, 0, 0].into());
            }
            wanted.public_url = Some(url);
        }
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

pub fn plugin_list() -> Option<Value> {
    command_ok("claude", &["plugin", "list", "--json"]).and_then(|t| serde_json::from_str(&t).ok())
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

pub fn has_marketplace() -> bool {
    command_ok("claude", &["plugin", "marketplace", "list", "--json"])
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|a| a.iter().any(|m| m.get("name").and_then(Value::as_str) == Some(MARKETPLACE)))
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
    let plugins = if args.skip_plugin { None } else { plugin_list() };
    Observed {
        config: read(paths.config()),
        apns: read(paths.apns()),
        plist: read(paths.plist(label)),
        service_loaded: !args.skip_service && service_loaded(paths.uid, label),
        other_services: other_services(paths, label),
        binaries: paths.bin.join("alondrad").is_file() && paths.bin.join("alondra").is_file(),
        apns_key_exists: apns.is_some_and(|a| a.key_path.is_file()),
        claude: args.skip_plugin || command_ok("claude", &["--version"]).is_some(),
        marketplace: !args.skip_plugin && has_marketplace(),
        enabled_globally: plugins.as_ref().map(enabled_globally).unwrap_or_default(),
        tuicr: command_ok("tuicr", &["--version"]).is_some(),
        brew: command_ok("brew", &["--version"]).is_some(),
    }
}

pub fn run(args: Args) -> Result<()> {
    let env = |k: &str| std::env::var(k).ok();
    let home = config::home_dir(&env);
    let alondra_home = config::alondra_home(&env);
    let paths = Paths {
        bin: args.bin.clone().unwrap_or_else(|| home.join(".local/bin")),
        repo: args.repo.clone().or_else(|| std::env::current_dir().ok().and_then(|d| find_repo(&d))),
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
    let seen = observe(&paths, &label, apns.as_ref(), &args);
    let plan = plan(&wanted, apns.as_ref(), &args, &paths, &seen);

    if !plan.conflicts.is_empty() {
        eprintln!("{}", tr!("Not touching anything, because:", "No toco nada, por esto:"));
        for c in &plan.conflicts {
            eprintln!("  {c}");
        }
        bail!(tr!("setup stopped", "setup detenido"));
    }
    for step in &plan.steps {
        print_step(step, args.dry_run);
        if !args.dry_run {
            execute(step)?;
        }
    }
    let config = Config::resolve(paths.alondra_home.clone(), &plan.config, env, config::system_name);
    if args.dry_run {
        println!("{}", tr!("(dry run: nothing was written or run)", "(ensayo: no se ha escrito ni ejecutado nada)"));
        return Ok(());
    }
    if args.skip_pair || args.skip_service {
        return Ok(());
    }
    pair_when_ready(&config)
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

fn execute(step: &Step) -> Result<()> {
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
            let out = Command::new(program).args(args).output().with_context(|| program.clone())?;
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

fn pair_when_ready(config: &Config) -> Result<()> {
    let client = crate::client::Client::new(Duration::from_secs(2));
    let up = (0..20).any(|_| {
        let ok = client.get::<Value>("/v1/health").is_ok();
        if !ok {
            std::thread::sleep(Duration::from_millis(500));
        }
        ok
    });
    if !up {
        println!("{}", tr!("! alondrad did not answer yet; pair later with `alondra pair`.", "! alondrad aún no responde; vincula después con `alondra pair`."));
        return Ok(());
    }
    match pairing_url(config) {
        Some(url) => crate::pair(&url),
        None => {
            println!("{}", tr!(
                "! No URL the iPhone can reach: set --public-url (and --listen 0.0.0.0 for the LAN), then run `alondra pair`.",
                "! No hay URL a la que llegue el iPhone: pon --public-url (y --listen 0.0.0.0 para la red local) y ejecuta `alondra pair`."
            ));
            Ok(())
        }
    }
}

pub fn pairing_url(config: &Config) -> Option<String> {
    config.public_url.clone().or_else(|| (!config.listen.is_loopback()).then(|| lan_url(config.port)).flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(p.steps.iter().any(|s| matches!(s, Step::Warn(w) if w.contains("apns.json"))));
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
