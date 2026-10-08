use std::path::Path;
use std::process::Command;
use std::time::Duration;

use alondra_core::config::{self, Config, FileConfig};
use anyhow::Result;
use serde_json::{Value, json};

use crate::client::Client;
use crate::setup;
use crate::state;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    pub fix: String,
}

fn check(name: &'static str, status: Status, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check { name, status, detail: detail.into(), fix: fix.into() }
}

fn output(program: &str, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
    let mut command = Command::new(program);
    command.args(args);
    command_output(command, timeout)
}

fn command_output(mut command: Command, timeout: Duration) -> Option<(bool, String)> {
    let child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
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

pub fn config_check(path: &Path, text: Option<&str>) -> Check {
    let p = path.display();
    match text.map(FileConfig::parse) {
        None => check(
            "config",
            Status::Warn,
            trf!("{p} does not exist: running on defaults", "{p} no existe: funciona con los valores por defecto"),
            tr!("alondra setup", "alondra setup"),
        ),
        Some(Ok(_)) => check("config", Status::Pass, trf!("{p} read", "{p} leído"), ""),
        Some(Err(e)) => check("config", Status::Fail, trf!("{p} is not valid: {e}", "{p} no es válido: {e}"), trf!("fix or remove {p}", "corrige o borra {p}")),
    }
}

/// The daemon reports the settings it started with; a difference means it needs a restart.
pub fn drift(running: &Value, local: &Config) -> Vec<String> {
    let want = json!({"name": local.name, "port": local.port, "listen": local.listen.to_string(), "public_url": local.public_url,
                      "legacy_bridge": local.legacy_bridge, "wake": local.wake});
    let mut out = vec![];
    if let Some(want) = want.as_object() {
        for (k, v) in want {
            if let Some(r) = running.get(k)
                && r != v
            {
                out.push(format!("{k}: {r} ≠ {v}"));
            }
        }
    }
    out
}

pub fn daemon_check(health: Result<Value, String>, local: &Config) -> Check {
    let label = &local.service_label;
    let uid = unsafe { libc::getuid() };
    let restart = format!("launchctl kickstart -k gui/{uid}/{label}");
    match health {
        Err(e) => check("alondrad", Status::Fail, e, trf!("alondra setup (or {restart})", "alondra setup (o {restart})")),
        Ok(h) => {
            let version = h.get("version").and_then(Value::as_str).unwrap_or("?").to_string();
            let port = local.port;
            match h.get("config").map(|c| drift(c, local)) {
                Some(d) if !d.is_empty() => {
                    let d = d.join(", ");
                    check(
                        "alondrad",
                        Status::Warn,
                        trf!("answers ({version}) but runs with other settings: {d}", "responde ({version}) pero con otros ajustes: {d}"),
                        restart,
                    )
                }
                _ => check("alondrad", Status::Pass, trf!("answers on port {port} ({version})", "responde en el puerto {port} ({version})"), ""),
            }
        }
    }
}

/// Why the daemon is down, when launchd keeps failing to start it.
pub fn startup_check(record: Option<&alondra_core::startup::Record>, up: bool, home: &Path) -> Option<Check> {
    let record = record.filter(|_| !up)?;
    let (failures, reason) = (record.failures, &record.reason);
    let log = home.join("alondrad.log");
    let log = log.display();
    let detail = if record.looping() {
        trf!("the last {failures} starts failed, now retried every few minutes: {reason}",
            "los últimos {failures} arranques fallaron; ahora se reintenta cada pocos minutos: {reason}")
    } else {
        trf!("the last start failed: {reason}", "el último arranque falló: {reason}")
    };
    Some(check("startup", Status::Fail, detail, trf!("see {log}; alondra update installs a fixed release", "mira {log}; alondra update instala una versión corregida")))
}

/// A newer binary was installed but failed its startup check, so the daemon stayed on its own.
pub fn preflight_check(health: &Value) -> Option<Check> {
    let rejected = health.get("preflight_rejected").filter(|v| !v.is_null())?;
    let version = rejected.get("version").and_then(Value::as_str).unwrap_or("?");
    let reason = rejected.get("reason").and_then(Value::as_str).unwrap_or("?");
    let running = health.get("version").and_then(Value::as_str).unwrap_or("?");
    Some(check("upgrade", Status::Warn,
        trf!("installed {version} failed its startup check, so {running} keeps running: {reason}",
            "la versión instalada {version} no pasó la comprobación de arranque; sigue {running}: {reason}"),
        tr!("wait for a fixed release, then alondra update", "espera a una versión corregida y ejecuta alondra update")))
}

/// Whether a newer release exists, as the daemon last saw it, and how an automatic update went.
pub fn update_check(health: &Value) -> Option<Check> {
    let status: alondra_core::update::Status = serde_json::from_value(health.get("update")?.clone()).ok()?;
    let current = &status.current;
    if let Some(attempt) = status.last_auto_update.as_ref().filter(|a| !a.ok && status.available && status.latest.as_deref() == Some(a.version.as_str())) {
        let (version, detail) = (&attempt.version, &attempt.detail);
        return Some(check("update", Status::Warn,
            trf!("automatic update to {version} failed: {detail}", "la actualización automática a {version} falló: {detail}"), "alondra update"));
    }
    Some(match status.latest.as_deref() {
        Some(latest) if status.available => check("update", Status::Warn,
            trf!("{latest} is available; this is {current}", "hay una versión {latest}; esta es {current}"), "alondra update"),
        Some(_) => check("update", Status::Pass, trf!("{current} is the latest release", "{current} es la última versión"), ""),
        None => check("update", Status::Pass, trf!("{current}; releases not checked yet", "{current}; aún no se han consultado las versiones"), ""),
    })
}

pub fn state_integrity_check(health: &Value) -> Check {
    let Some(issues) = health.get("integrity_issues").and_then(Value::as_array) else {
        return check("state", Status::Warn, tr!("daemon does not report state integrity", "el servidor no informa de la integridad del estado"), "alondra setup");
    };
    if issues.is_empty() {
        check("state", Status::Pass, tr!("node and task state passes structural checks", "el estado de nodos y tareas supera las comprobaciones estructurales"), "")
    } else {
        let detail = issues.iter().filter_map(Value::as_str).take(3).collect::<Vec<_>>().join("; ");
        check("state", Status::Fail, detail, tr!("save alondra.db and inspect the daemon log before repair", "guarda alondra.db y revisa el log del servidor antes de reparar"))
    }
}

pub fn service_check(print: Option<&str>, label: &str) -> Check {
    match print {
        None => check(
            "launchd",
            Status::Fail,
            trf!("service {label} is not loaded", "el servicio {label} no está cargado"),
            tr!("alondra setup", "alondra setup"),
        ),
        Some(text) if text.contains("state = running") => check("launchd", Status::Pass, trf!("{label} running", "{label} en marcha"), ""),
        Some(_) => check(
            "launchd",
            Status::Warn,
            trf!("{label} loaded but not running", "{label} cargado pero parado"),
            "tail ~/.alondra/alondrad.log",
        ),
    }
}

/// Alondra loads its plugin with `--plugin-dir` into the sessions it launches, from the repo
/// registered as its marketplace; a global install would load it into every other session too.
pub fn plugin_check(list: Option<&Value>, marketplace: bool, repo: Option<&Path>) -> Check {
    let add = repo.map_or_else(|| "<repo>".to_string(), |r| r.display().to_string());
    if !marketplace {
        return check(
            "plugin",
            Status::Fail,
            tr!(
                "Alondra's marketplace is not registered: alondrad cannot load its plugin into the nodes it launches",
                "el marketplace de Alondra no está registrado: alondrad no puede cargar su plugin en los nodos que lanza"
            ),
            format!("claude plugin marketplace add {add}"),
        );
    }
    let Some(list) = list else {
        return check("plugin", Status::Fail, tr!("could not list Claude Code plugins", "no pude listar los plugins de Claude Code"), "claude plugin list --json");
    };
    match setup::enabled_globally(list).as_slice() {
        [] => check(
            "plugin",
            Status::Pass,
            tr!("loaded only into the sessions Alondra launches", "cargado solo en las sesiones que lanza Alondra"),
            "",
        ),
        enabled => {
            let ids = enabled.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>().join(", ");
            let fix = enabled.iter().map(|(id, scope)| format!("claude plugin disable {id} --scope {scope}")).collect::<Vec<_>>().join(" && ");
            check(
                "plugin",
                Status::Warn,
                trf!(
                    "{ids} is enabled in every Claude session: sessions Alondra did not launch send it their hooks (alondra setup turns it off)",
                    "{ids} está activado en todas las sesiones de Claude: las que no lanza Alondra le mandan sus hooks (alondra setup lo desactiva)"
                ),
                fix,
            )
        }
    }
}

pub fn hooks_port_check(port: u16) -> Option<Check> {
    (port != alondra_core::DEFAULT_PORT).then(|| {
        let default = alondra_core::DEFAULT_PORT;
        check(
            "hooks",
            Status::Warn,
            trf!(
                "alondrad uses port {port}, but the plugin's hooks post to {default}: nodes will not report their state",
                "alondrad usa el puerto {port}, pero los hooks del plugin llaman al {default}: los nodos no informarán de su estado"
            ),
            trf!("use port {default} (remove `port` from config.toml and ALONDRA_PORT)", "usa el puerto {default} (quita `port` de config.toml y ALONDRA_PORT)"),
        )
    })
}

pub fn apns_file_check(text: Option<&str>, key_exists: impl Fn(&Path) -> bool) -> Check {
    let fix = tr!(
        "alondra setup --apns-key-path … --apns-key-id … --apns-team-id … --apns-topic <your bundle ID>",
        "alondra setup --apns-key-path … --apns-key-id … --apns-team-id … --apns-topic <tu bundle ID>"
    );
    let Some(text) = text else {
        return check("apns", Status::Warn, tr!("no apns.json: no notifications", "sin apns.json: no hay notificaciones"), fix);
    };
    match serde_json::from_str::<setup::Apns>(text) {
        Err(e) => check("apns", Status::Fail, trf!("apns.json is not valid: {e}", "apns.json no es válido: {e}"), fix),
        Ok(a) if !key_exists(&a.key_path) => {
            let k = a.key_path.display();
            check("apns", Status::Fail, trf!("the key {k} does not exist", "la llave {k} no existe"), fix)
        }
        Ok(a) => {
            let topic = a.topic;
            check("apns", Status::Pass, trf!("apns.json for {topic}", "apns.json para {topic}"), "")
        }
    }
}

fn notification_checks(home: &Path) -> Vec<Check> {
    // A present but invalid service configuration must never select legacy APNs.
    if home.join("service.toml").symlink_metadata().is_ok() {
        return vec![check("service push", Status::Warn,
            tr!("Service configuration present; encrypted push delivery needs a paired-device test",
                "Configuración de servicio presente; la entrega cifrada requiere probar un dispositivo vinculado"),
            "alondrad --test-service-push <device-id>")];
    }
    let text = std::fs::read_to_string(home.join("apns.json")).ok();
    let apns = apns_file_check(text.as_deref(), |p| p.is_file());
    vec![apns]
}

pub fn claude_version_check(out: Option<&str>) -> Check {
    let min = alondra_core::mods::CLAUDE_MIN_TEXT;
    match out.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) if alondra_core::mods::runs_mods(v) => check("claude", Status::Pass, v.to_string(), ""),
        Some(v) => check("claude", Status::Fail,
            trf!("{v} cannot run Alondra's node mod: nodes need Claude Code {min} or later",
                "{v} no puede ejecutar el mod de nodo de Alondra: los nodos necesitan Claude Code {min} o posterior"),
            "claude update"),
        None => check("claude", Status::Fail, tr!("Claude Code (`claude`) is not on PATH", "Claude Code (`claude`) no está en el PATH"), "https://docs.claude.com/claude-code"),
    }
}

/// The node plugin's mod as Claude Code reads it: `claude plugin validate --strict` and its own
/// tests under `claude plugin test`, which also fail when hooks modules are turned off.
pub fn mods_check(plugin: Option<&Path>, validate: Option<(bool, String)>, test: Option<(bool, String)>) -> Check {
    let Some(plugin) = plugin else {
        return check("mods", Status::Fail, tr!("Alondra's node plugin was not found", "No se encuentra el plugin de nodo de Alondra"), "alondra setup");
    };
    let p = plugin.display();
    let (validate_fix, test_fix) = (format!("claude plugin validate --strict {p}"), format!("claude plugin test {p}"));
    match (validate, test) {
        (None, _) => check("mods", Status::Fail, tr!("claude plugin validate did not finish", "claude plugin validate no terminó"), validate_fix),
        (Some((false, text)), _) => {
            let why = alondra_core::mods::tail(&text);
            check("mods", Status::Fail, trf!("the node mod does not validate: {why}", "el mod de nodo no pasa la validación: {why}"), validate_fix)
        }
        (_, None) => check("mods", Status::Fail, tr!("claude plugin test did not finish", "claude plugin test no terminó"), test_fix),
        (_, Some((false, text))) => {
            let why = alondra_core::mods::tail(&text);
            check("mods", Status::Fail, trf!("the node mod's tests fail: {why}", "las pruebas del mod de nodo fallan: {why}"), test_fix)
        }
        _ => check("mods", Status::Pass, tr!("the node mod validates and its tests pass", "el mod de nodo pasa la validación y sus pruebas"), ""),
    }
}

/// Inspect capabilities rather than assuming a version string has the required flags.
pub fn orchestrator_capability_check(help: Option<&str>) -> Check {
    let supported = help.is_some_and(|text| {
        ["--print", "--resume", "--output-format", "stream-json", "--permission-mode", "auto", "--permission-prompts"].iter().all(|flag| text.contains(flag))
    });
    if supported {
        check("orchestrator", Status::Pass, tr!("Claude supports on-demand orchestration", "Claude admite la orquestación bajo demanda"), "")
    } else {
        check("orchestrator", Status::Fail, tr!("Claude is missing required on-demand runtime capabilities", "A Claude le faltan capacidades necesarias para la ejecución bajo demanda"), "claude update")
    }
}

fn claude_auth_check(result: Result<bool>) -> Check {
    match result {
        Ok(true) => check("claude auth", Status::Pass, "Claude Code is signed in for the daemon environment", ""),
        Ok(false) => check("claude auth", Status::Fail, "Claude Code is not signed in", "alondra setup"),
        Err(error) => check("claude auth", Status::Fail, error.to_string(), "alondra setup"),
    }
}

pub fn tmux_check(out: Option<&str>, wake: bool) -> Check {
    match out.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => check("tmux", Status::Pass, v.to_string(), ""),
        None => check(
            "tmux",
            if wake { Status::Fail } else { Status::Warn },
            tr!("tmux is missing: Alondra cannot attach a node's session", "falta tmux: Alondra no puede engancharse a la sesión de un nodo"),
            "brew install tmux",
        ),
    }
}

/// The oldest tuicr whose `--stdout pr <n>` Alondra relies on.
pub const TUICR_MIN: (u32, u32) = (0, 26);

pub fn tuicr_check(out: Option<&str>) -> Check {
    let fix = "brew install tuicr";
    let Some(version) = out.map(str::trim).filter(|v| !v.is_empty()) else {
        return check("tuicr", Status::Warn, tr!("tuicr is missing: PR cards have no diff in the TUI", "falta tuicr: las tarjetas de PR no tienen diff en la TUI"), fix);
    };
    let numbers: Vec<u32> = version.rsplit(' ').next().unwrap_or("").split('.').filter_map(|n| n.parse().ok()).collect();
    match numbers.as_slice() {
        [major, minor, ..] if (*major, *minor) >= TUICR_MIN => check("tuicr", Status::Pass, version.to_string(), ""),
        _ => check("tuicr", Status::Warn, trf!("{version} is older than Alondra needs (0.26)", "{version} es más antiguo de lo que Alondra necesita (0.26)"), "brew upgrade tuicr"),
    }
}

pub fn run(json_out: bool) -> Result<()> {
    let env = |k: &str| std::env::var(k).ok();
    let home = config::alondra_home(&env);
    let path = config::path_in(&home);
    let text = std::fs::read_to_string(&path).ok();
    let mut checks = vec![config_check(&path, text.as_deref())];
    let local = Config::load().unwrap_or_else(|_| Config::defaults());

    let client = Client::new(Duration::from_secs(5));
    let health = client.get::<Value>("/daemon").map_err(|e| e.to_string());
    let up = health.is_ok();
    if let Some(value) = health.as_ref().ok().filter(|v| v.get("integrity_issues").is_some()) {
        checks.push(state_integrity_check(value));
    } else if health.is_err() && home.join("alondra.db").exists() {
        checks.push(match state::audit(&home) {
            Ok((seq, tasks)) => check("state", Status::Pass, trf!("offline replay valid through event {seq}; {tasks} tasks", "reproducción sin servidor válida hasta el evento {seq}; {tasks} tareas"), ""),
            Err(e) => check("state", Status::Fail, format!("{e:#}"), "alondra state repair (only with a complete event log and the daemon stopped)"),
        });
    }
    checks.extend(startup_check(alondra_core::startup::read(&home).as_ref(), up, &home));
    if let Ok(value) = &health {
        checks.extend(preflight_check(value));
        checks.extend(update_check(value));
    }
    checks.push(daemon_check(health, &local));
    checks.extend(hooks_port_check(local.port));

    let uid = unsafe { libc::getuid() };
    let print = output("launchctl", &["print", &format!("gui/{uid}/{}", local.service_label)], Duration::from_secs(5)).filter(|(ok, _)| *ok).map(|(_, t)| t);
    checks.push(service_check(print.as_deref(), &local.service_label));

    let user_home = config::home_dir(&env);
    let plist = user_home.join("Library/LaunchAgents").join(format!("{}.plist", local.service_label));
    let context = setup::read_plist(&plist).and_then(|v| {
        let vars = v.get("EnvironmentVariables").cloned().unwrap_or(Value::Null);
        let vars = serde_json::from_value(vars)?;
        crate::claude_auth::ContextEnv::from_environment(&user_home, &vars)
    });
    let claude = context.as_ref().ok().and_then(|c| c.command().ok())
        .and_then(|mut cmd| { cmd.arg("--version"); command_output(cmd, Duration::from_secs(10)) })
        .filter(|(ok, _)| *ok).map(|(_, text)| text);
    checks.push(claude_version_check(claude.as_deref()));
    let help = context.as_ref().ok().and_then(|c| c.command().ok())
        .and_then(|mut cmd| { cmd.arg("--help"); command_output(cmd, Duration::from_secs(10)) })
        .filter(|(ok, _)| *ok).map(|(_, text)| text);
    checks.push(orchestrator_capability_check(help.as_deref()));
    checks.push(claude_auth_check(context.as_ref().map_err(|e| anyhow::anyhow!("{e}")).and_then(|c| c.status())));
    let repo = std::env::current_dir().ok().and_then(|d| setup::find_repo(&d));
    let plugins = context.as_ref().ok().and_then(setup::plugin_list);
    let marketplace = context.as_ref().is_ok_and(setup::has_marketplace);
    checks.push(plugin_check(plugins.as_ref(), marketplace, repo.as_deref()));
    let node_plugin = context.as_ref().ok().and_then(setup::marketplace_path).map(|root| root.join("plugin"))
        .filter(|dir| dir.join(".claude-plugin/plugin.json").is_file());
    let claude_on = |args: &[&str], timeout: Duration| context.as_ref().ok().and_then(|c| c.command().ok())
        .and_then(|mut cmd| { cmd.args(args); command_output(cmd, timeout) });
    let (validate, test) = match &node_plugin {
        Some(dir) => {
            let dir = dir.display().to_string();
            (claude_on(&["plugin", "validate", "--strict", &dir], Duration::from_secs(60)),
                claude_on(&["plugin", "test", &dir], Duration::from_secs(120)))
        }
        None => (None, None),
    };
    checks.push(mods_check(node_plugin.as_deref(), validate, test));

    let tmux = output("tmux", &["-V"], Duration::from_secs(5)).filter(|(ok, _)| *ok).map(|(_, t)| t);
    checks.push(tmux_check(tmux.as_deref(), local.wake));
    let tuicr = output("tuicr", &["--version"], Duration::from_secs(5)).filter(|(ok, _)| *ok).map(|(_, t)| t);
    checks.push(tuicr_check(tuicr.as_deref()));

    checks.extend(notification_checks(&home));
    if up {
        let devices = client.get::<Value>("/devices").ok().and_then(|v| v.get("devices").and_then(Value::as_array).map(Vec::len)).unwrap_or(0);
        checks.push(if devices > 0 {
            check("pairing", Status::Pass, trf!("{devices} paired device(s)", "{devices} dispositivo(s) vinculado(s)"), "")
        } else {
            check("pairing", Status::Warn, tr!("no paired iPhone", "ningún iPhone vinculado"),
                tr!("open Alondra on your iPhone", "abre Alondra en tu iPhone"))
        });
    }

    let failed = checks.iter().any(|c| c.status == Status::Fail);
    if json_out {
        let list: Vec<Value> = checks
            .iter()
            .map(|c| json!({"check": c.name, "status": format!("{:?}", c.status).to_lowercase(), "detail": c.detail, "fix": c.fix}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!({"ok": !failed, "checks": list}))?);
    } else {
        for c in &checks {
            let mark = match c.status {
                Status::Pass => "✓",
                Status::Warn => "!",
                Status::Fail => "✗",
            };
            println!("{mark} {:<10} {}", c.name, c.detail);
            if c.status != Status::Pass && !c.fix.is_empty() {
                println!("  {:<10} → {}", "", c.fix);
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_present_service_configuration_replaces_the_apns_file_check() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-doctor-service-{}-{}", std::process::id(), alondra_core::now_millis()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("service.toml"), "invalid configuration").unwrap();
        let key = home.join("test-key.p8");
        std::fs::write(&key, "test fixture, not a real key").unwrap();
        std::fs::write(home.join("apns.json"), serde_json::to_vec(&json!({
            "key_path": key, "key_id": "test", "team_id": "test", "topic": "test"
        })).unwrap()).unwrap();
        let checks = notification_checks(&home);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "service push");
        assert_eq!(checks[0].status, Status::Warn, "configuration presence cannot prove delivery");
        std::fs::remove_file(home.join("service.toml")).unwrap();
        let checks = notification_checks(&home);
        assert_eq!(checks[0].name, "apns");
        assert_eq!(checks[0].status, Status::Pass);
        assert_eq!(checks.len(), 1);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn configured_command_preserves_environment_and_times_out() {
        let mut command = Command::new("/bin/sh");
        command.env_clear().env("ALONDRA_DOCTOR_TEST", "configured").args(["-c", "printf '%s' \"$ALONDRA_DOCTOR_TEST\""]);
        assert_eq!(command_output(command, Duration::from_secs(2)), Some((true, "configured".into())));
        let mut blocked = Command::new("/bin/sleep");
        blocked.arg("10");
        let started = std::time::Instant::now();
        assert!(command_output(blocked, Duration::from_millis(100)).is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn tuicr_is_checked_for_presence_and_version() {
        assert_eq!(tuicr_check(Some("tuicr 0.26.0")).status, Status::Pass);
        assert_eq!(tuicr_check(Some("tuicr 1.2.0\n")).status, Status::Pass);
        assert_eq!(tuicr_check(Some("tuicr 0.18.3")).status, Status::Warn);
        assert_eq!(tuicr_check(None).fix, "brew install tuicr");
    }

    fn local() -> Config {
        Config::resolve("/h".into(), &FileConfig { name: Some("Ana".into()), wake: Some(true), ..Default::default() }, |_| None, || None)
    }

    #[test]
    fn the_running_daemon_is_compared_with_the_file() {
        let l = local();
        let same = json!({"name": "Ana", "port": 47800, "listen": "127.0.0.1", "public_url": null, "legacy_bridge": false, "wake": true});
        assert!(drift(&same, &l).is_empty());
        assert_eq!(daemon_check(Ok(json!({"version": "0.1.0", "config": same})), &l).status, Status::Pass);
        let stale = json!({"name": "Bea", "port": 47800, "listen": "127.0.0.1", "public_url": null, "legacy_bridge": false, "wake": true});
        assert_eq!(drift(&stale, &l), vec!["name: \"Bea\" ≠ \"Ana\""]);
        assert_eq!(daemon_check(Ok(json!({"config": stale})), &l).status, Status::Warn);
        assert_eq!(daemon_check(Ok(json!({"version": "old"})), &l).status, Status::Pass);
        assert_eq!(daemon_check(Err("no responde".into()), &l).status, Status::Fail);
    }

    #[test]
    fn service_plugin_and_tools() {
        assert_eq!(service_check(Some("gui/501/x = {\n\tstate = running\n\tpid = 12\n}"), "x").status, Status::Pass);
        assert_eq!(service_check(Some("state = not running"), "x").status, Status::Warn);
        assert_eq!(service_check(None, "x").status, Status::Fail);

        let global = json!([{"id": "alondra@alondra", "version": "0.1.2", "scope": "user", "enabled": true}]);
        let c = plugin_check(Some(&global), true, None);
        assert_eq!(c.status, Status::Warn, "a global install reaches sessions Alondra did not launch");
        assert_eq!(c.fix, "claude plugin disable alondra@alondra --scope user");
        assert_eq!(plugin_check(Some(&json!([{"id": "alondra@alondra", "enabled": false}])), true, None).status, Status::Pass);
        assert_eq!(plugin_check(Some(&json!([])), true, None).status, Status::Pass);
        assert!(plugin_check(Some(&json!([])), false, Some(Path::new("/src/alondra"))).fix.contains("marketplace add /src/alondra"));

        assert!(hooks_port_check(alondra_core::DEFAULT_PORT).is_none());
        assert_eq!(hooks_port_check(47897).map(|c| c.status), Some(Status::Warn));
        assert_eq!(tmux_check(None, true).status, Status::Fail);
        assert_eq!(tmux_check(None, false).status, Status::Warn);
        assert_eq!(claude_version_check(Some("2.1.287 (Claude Code)\n")).detail, "2.1.287 (Claude Code)");
        let old = claude_version_check(Some("2.1.280 (Claude Code)\n"));
        assert_eq!((old.status, old.fix.as_str()), (Status::Fail, "claude update"), "nodes need mods");
        assert!(old.detail.contains("2.1.287"), "{}", old.detail);
    }

    #[test]
    fn the_node_mod_must_validate_and_pass_its_tests() {
        let plugin = Path::new("/r/plugin");
        let ok = || Some((true, String::new()));
        assert_eq!(mods_check(Some(plugin), ok(), ok()).status, Status::Pass);
        let invalid = mods_check(Some(plugin), Some((false, "❯ author: No author\n✘ Validation failed".into())), ok());
        assert_eq!((invalid.status, invalid.fix.as_str()), (Status::Fail, "claude plugin validate --strict /r/plugin"));
        assert!(invalid.detail.contains("Validation failed"), "{}", invalid.detail);
        let off = mods_check(Some(plugin), ok(), Some((false, "claude plugin test: hooks modules are turned off: disableAllHooks".into())));
        assert_eq!((off.status, off.fix.as_str()), (Status::Fail, "claude plugin test /r/plugin"));
        assert!(off.detail.contains("hooks modules are turned off"), "{}", off.detail);
        assert_eq!(mods_check(Some(plugin), ok(), None).status, Status::Fail);
        assert_eq!(mods_check(None, ok(), ok()).fix, "alondra setup");
    }

    #[test]
    fn config_and_apns_files() {
        let p = Path::new("/h/config.toml");
        assert_eq!(config_check(p, None).status, Status::Warn);
        assert_eq!(config_check(p, Some("name = \"Ana\"")).status, Status::Pass);
        assert_eq!(config_check(p, Some("nmae = 1")).status, Status::Fail);
        let apns = r#"{"key_path": "/k.p8", "key_id": "K", "team_id": "T", "topic": "a.b"}"#;
        assert_eq!(apns_file_check(Some(apns), |_| true).status, Status::Pass);
        assert_eq!(apns_file_check(Some(apns), |_| false).status, Status::Fail);
        assert_eq!(apns_file_check(Some("{}"), |_| true).status, Status::Fail);
        assert_eq!(apns_file_check(None, |_| true).status, Status::Warn);
    }
}

#[cfg(test)]
mod orchestrator_tests {
    use super::*;
    #[test]
    fn missing_auto_or_streaming_support_is_actionable() {
        let help = "--print --resume --output-format stream-json --permission-mode auto --permission-prompts";
        assert_eq!(orchestrator_capability_check(Some(help)).status, Status::Pass);
        assert_eq!(orchestrator_capability_check(Some(&help.replace("auto", "default"))).status, Status::Fail);
        assert_eq!(orchestrator_capability_check(None).status, Status::Fail);
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use alondra_core::startup::Record;

    #[test]
    fn a_failing_start_is_reported_only_while_the_daemon_is_down() {
        let home = Path::new("/h");
        let record = Record { failures: 3, last_at: 1, reason: "bootstrap failed integrity audit: t1".into(), version: "0.1.12".into() };
        assert!(startup_check(Some(&record), true, home).is_none());
        assert!(startup_check(None, false, home).is_none());
        let down = startup_check(Some(&record), false, home).unwrap();
        assert_eq!(down.status, Status::Fail);
        assert!(down.detail.contains("bootstrap failed integrity audit: t1") && down.detail.contains('3'), "{}", down.detail);
        assert!(down.fix.contains("/h/alondrad.log"));
    }

    #[test]
    fn a_newer_release_or_a_failed_automatic_update_is_a_warning() {
        let health = |update: Value| json!({"version": "0.1.12", "update": update});
        assert!(update_check(&json!({"version": "0.1.12"})).is_none(), "an older daemon reports nothing");
        let newer = update_check(&health(json!({"current": "0.1.12", "latest": "0.1.13", "available": true}))).unwrap();
        assert_eq!((newer.status, newer.fix.as_str()), (Status::Warn, "alondra update"));
        assert!(newer.detail.contains("0.1.13"));
        let latest = update_check(&health(json!({"current": "0.1.13", "latest": "0.1.13", "available": false}))).unwrap();
        assert_eq!(latest.status, Status::Pass);
        let failed = update_check(&health(json!({"current": "0.1.12", "latest": "0.1.13", "available": true,
            "last_auto_update": {"version": "0.1.13", "at": 1, "ok": false, "detail": "Error: no bottle"}}))).unwrap();
        assert_eq!(failed.status, Status::Warn);
        assert!(failed.detail.contains("Error: no bottle"), "{}", failed.detail);
    }

    #[test]
    fn a_rejected_upgrade_names_the_version_and_why() {
        assert!(preflight_check(&json!({"version": "0.1.12", "preflight_rejected": null})).is_none());
        let warned = preflight_check(&json!({"version": "0.1.12", "preflight_rejected": {"version": "0.1.13", "reason": "preflight failed: audit"}})).unwrap();
        assert_eq!(warned.status, Status::Warn);
        assert!(warned.detail.contains("0.1.13") && warned.detail.contains("0.1.12") && warned.detail.contains("preflight failed: audit"), "{}", warned.detail);
    }
}
