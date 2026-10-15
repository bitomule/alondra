use std::path::Path;
use std::time::Duration;

use alondra_core::config::{Config, FileConfig};
use serde_json::{Value, json};

use super::system::{MacInfo, Memory, ServiceView};
use super::{Action, Check, Env, Fix, Group, Outcome, Status};
use crate::setup;

fn outcome(id: &'static str, name: &'static str, group: Group, status: Status, detail: impl Into<String>, fix: Option<Fix>) -> Outcome {
    Outcome { id, name, group, status, detail: detail.into(), fix }
}

fn manual(text: impl Into<String>) -> Option<Fix> {
    Some(Fix::Manual(text.into()))
}

fn auto(action: Action) -> Option<Fix> {
    Some(Fix::Auto(action))
}

/// The day a millisecond timestamp falls on, in this Mac's time zone.
pub fn day(millis: i64) -> String {
    let seconds = (millis / 1000) as libc::time_t;
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    if unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null() {
        return String::new();
    }
    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
}

pub fn mac_check(info: &MacInfo) -> Outcome {
    let detail = [info.os.as_str(), info.arch.as_str(), info.install.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
    if info.dev_over_brew {
        return outcome("mac", "this Mac", Group::Service, Status::Warn,
            trf!("{detail}, next to the Homebrew install: --fix leaves the Homebrew service alone",
                "{detail}, junto a la instalación de Homebrew: --fix no toca el servicio de Homebrew"),
            manual("make e2e-sandbox"));
    }
    outcome("mac", "this Mac", Group::Service, Status::Pass, detail, None)
}

/// How `--fix` starts the service on this Mac, or the step a person takes when it must not:
/// a development build leaves a Homebrew install alone, and nothing starts without alondrad.
pub fn service_repair(view: &ServiceView, start: Action) -> Option<Fix> {
    if view.dev_over_brew {
        manual(tr!(
            "Alondra is installed with Homebrew; use the e2e sandbox (make e2e-sandbox) to try a development build",
            "Alondra está instalada con Homebrew; usa el sandbox e2e (make e2e-sandbox) para probar una build de desarrollo"))
    } else if !view.binaries {
        manual("brew reinstall alondra")
    } else {
        auto(start)
    }
}

/// `install`: what writes the file, from `service_repair`.
pub fn config_check(path: &Path, text: Option<&str>, install: Option<Fix>) -> Outcome {
    let p = path.display();
    match text.map(FileConfig::parse) {
        None => outcome("config", "config", Group::Service, Status::Warn,
            trf!("{p} does not exist: running on defaults", "{p} no existe: funciona con los valores por defecto"), install),
        Some(Ok(_)) => outcome("config", "config", Group::Service, Status::Pass, trf!("{p} read", "{p} leído"), None),
        Some(Err(e)) => outcome("config", "config", Group::Service, Status::Fail,
            trf!("{p} is not valid: {e}", "{p} no es válido: {e}"), manual(trf!("fix or remove {p}", "corrige o borra {p}"))),
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

/// `start`: how `--fix` brings the daemon up (`service_repair`). `port_owner`: another process
/// holds the port, which no restart fixes.
pub fn daemon_check(health: Result<Value, String>, local: &Config, start: Option<Fix>, port_owner: Option<&str>) -> Outcome {
    let port = local.port;
    match health {
        Err(_) if port_owner.is_some() => {
            let process = port_owner.unwrap_or_default();
            outcome("alondrad", "alondrad", Group::Service, Status::Fail,
                trf!("Port {port} is in use by {process}. Close it and try again.", "El puerto {port} lo está usando {process}. Ciérralo y vuelve a intentarlo."),
                manual(format!("lsof -iTCP:{port} -sTCP:LISTEN")))
        }
        Err(e) => outcome("alondrad", "alondrad", Group::Service, Status::Fail, e, start),
        Ok(h) => {
            let version = h.get("version").and_then(Value::as_str).unwrap_or("?").to_string();
            match h.get("config").map(|c| drift(c, local)) {
                Some(d) if !d.is_empty() => {
                    let d = d.join(", ");
                    outcome("alondrad", "alondrad", Group::Service, Status::Warn,
                        trf!("answers ({version}) but runs with other settings: {d}", "responde ({version}) pero con otros ajustes: {d}"), start)
                }
                _ => outcome("alondrad", "alondrad", Group::Service, Status::Pass,
                    trf!("answers on port {port} ({version})", "responde en el puerto {port} ({version})"), None),
            }
        }
    }
}

/// Why the daemon is down, when launchd keeps failing to start it.
pub fn startup_check(record: Option<&alondra_core::startup::Record>, up: bool, home: &Path) -> Option<Outcome> {
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
    Some(outcome("startup", "startup", Group::Service, Status::Fail, detail,
        manual(trf!("see {log}; alondra update installs a fixed release", "mira {log}; alondra update instala una versión corregida"))))
}

/// A newer binary was installed but failed its startup check, so the daemon stayed on its own.
pub fn preflight_check(health: &Value) -> Option<Outcome> {
    let rejected = health.get("preflight_rejected").filter(|v| !v.is_null())?;
    let version = rejected.get("version").and_then(Value::as_str).unwrap_or("?");
    let reason = rejected.get("reason").and_then(Value::as_str).unwrap_or("?");
    let running = health.get("version").and_then(Value::as_str).unwrap_or("?");
    Some(outcome("upgrade", "upgrade", Group::Service, Status::Warn,
        trf!("installed {version} failed its startup check, so {running} keeps running: {reason}",
            "la versión instalada {version} no pasó la comprobación de arranque; sigue {running}: {reason}"),
        manual(tr!("wait for a fixed release, then alondra update", "espera a una versión corregida y ejecuta alondra update"))))
}

/// Whether a newer release exists, as the daemon last saw it, and how an automatic update went.
pub fn update_check(health: &Value) -> Option<Outcome> {
    let status: alondra_core::update::Status = serde_json::from_value(health.get("update")?.clone()).ok()?;
    let current = &status.current;
    if let Some(attempt) = status.last_auto_update.as_ref().filter(|a| !a.ok && status.available && status.latest.as_deref() == Some(a.version.as_str())) {
        let (version, detail) = (&attempt.version, &attempt.detail);
        return Some(outcome("update", "update", Group::Service, Status::Warn,
            trf!("automatic update to {version} failed: {detail}", "la actualización automática a {version} falló: {detail}"), manual("alondra update")));
    }
    Some(match status.latest.as_deref() {
        Some(latest) if status.available => outcome("update", "update", Group::Service, Status::Warn,
            trf!("{latest} is available; this is {current}", "hay una versión {latest}; esta es {current}"), manual("alondra update")),
        Some(_) => outcome("update", "update", Group::Service, Status::Pass, trf!("{current} is the latest release", "{current} es la última versión"), None),
        None => outcome("update", "update", Group::Service, Status::Pass,
            trf!("{current}; releases not checked yet", "{current}; aún no se han consultado las versiones"), None),
    })
}

pub fn state_integrity_check(health: &Value) -> Outcome {
    let Some(issues) = health.get("integrity_issues").and_then(Value::as_array) else {
        return outcome("state", "state", Group::Service, Status::Warn,
            tr!("daemon does not report state integrity", "el servidor no informa de la integridad del estado"), manual("alondra update"));
    };
    if issues.is_empty() {
        outcome("state", "state", Group::Service, Status::Pass,
            tr!("node and task state passes structural checks", "el estado de nodos y tareas supera las comprobaciones estructurales"), None)
    } else {
        let detail = issues.iter().filter_map(Value::as_str).take(3).collect::<Vec<_>>().join("; ");
        outcome("state", "state", Group::Service, Status::Fail, detail,
            manual(tr!("save alondra.db and inspect the daemon log before repair", "guarda alondra.db y revisa el log del servidor antes de reparar")))
    }
}

pub fn offline_state_check(audit: Result<(u64, usize), String>) -> Outcome {
    match audit {
        Ok((seq, tasks)) => outcome("state", "state", Group::Service, Status::Pass,
            trf!("offline replay valid through event {seq}; {tasks} tasks", "reproducción sin servidor válida hasta el evento {seq}; {tasks} tareas"), None),
        Err(e) => outcome("state", "state", Group::Service, Status::Fail, e,
            manual(tr!("alondra state repair (only with a complete event log and the daemon stopped)",
                "alondra state repair (solo con el historial de eventos completo y el servidor parado)"))),
    }
}

fn label_of(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn service_check(view: &ServiceView) -> Outcome {
    let label = &view.label;
    let running = view.print.as_deref().map(|t| t.contains("state = running"));
    if let Some(other) = view.others.first() {
        let (path, other_label) = (other.display(), label_of(other));
        let replace = service_repair(view, Action::ReplaceOtherService).map(|fix| match fix {
            Fix::Auto(action) => Fix::Ask {
                question: trf!("Another Alondra service is installed ({other_label}). Replace it with this one? Your tasks and devices are kept.",
                    "Hay otro servicio de Alondra instalado ({other_label}). ¿Lo sustituyo por este? Tus tareas y dispositivos se conservan."),
                default: false,
                action,
                instead: tr!("run `alondra doctor --fix` in a terminal to replace it", "ejecuta `alondra doctor --fix` en una terminal para sustituirlo").into(),
            },
            manual => manual,
        });
        return outcome("service", "launchd", Group::Service, Status::Fail,
            trf!("another one is installed: {path} also starts alondrad, and two services would fight over the same data",
                "hay otro instalado: {path} también arranca alondrad, y dos servicios se pelearían por los mismos datos"),
            replace);
    }
    if !view.dev_over_brew && !view.binaries {
        let bin = view.bin.display();
        return outcome("service", "launchd", Group::Service, Status::Fail,
            trf!("alondrad is missing from {bin}", "falta alondrad en {bin}"), service_repair(view, Action::InstallService));
    }
    match running {
        None => outcome("service", "launchd", Group::Service, Status::Fail,
            trf!("service {label} is not loaded", "el servicio {label} no está cargado"), service_repair(view, Action::InstallService)),
        Some(_) if view.pending && !view.dev_over_brew => outcome("service", "launchd", Group::Service, Status::Warn,
            trf!("{label} runs with an outdated service file or config.toml", "{label} funciona con un archivo de servicio o un config.toml desactualizado"),
            service_repair(view, Action::InstallService)),
        Some(true) => outcome("service", "launchd", Group::Service, Status::Pass, trf!("{label} running", "{label} en marcha"), None),
        Some(false) => outcome("service", "launchd", Group::Service, Status::Warn,
            trf!("{label} loaded but not running", "{label} cargado pero parado"), service_repair(view, Action::RestartService)),
    }
}

pub fn identity_check(service_config: bool) -> Outcome {
    if service_config {
        outcome("identity", "identity", Group::Devices, Status::Pass,
            tr!("ready for your devices", "listo para tus dispositivos"), None)
    } else {
        outcome("identity", "identity", Group::Devices, Status::Fail,
            tr!("this Mac has no service identity yet, so no device can pair with it", "este Mac aún no tiene identidad de servicio, así que ningún dispositivo puede emparejarse"),
            auto(Action::PrepareIdentity))
    }
}

/// Alondra loads its plugin with `--plugin-dir` into the sessions it launches, from the repo
/// registered as its marketplace; a global install would load it into every other session too.
pub fn plugin_check(list: Option<&Value>, marketplace: bool, repo: Option<&Path>, elsewhere: Option<&Path>) -> Outcome {
    if !marketplace {
        return outcome("plugin", "plugin", Group::Claude, Status::Fail,
            tr!(
                "Alondra's marketplace is not registered: alondrad cannot load its plugin into the nodes it launches",
                "el marketplace de Alondra no está registrado: alondrad no puede cargar su plugin en los nodos que lanza"
            ),
            match repo {
                Some(_) => auto(Action::RepairPlugin),
                None => manual("claude plugin marketplace add <repo>"),
            });
    }
    let Some(list) = list else {
        return outcome("plugin", "plugin", Group::Claude, Status::Fail,
            tr!("could not list Claude Code plugins", "no pude listar los plugins de Claude Code"), manual("claude plugin list --json"));
    };
    let enabled = setup::enabled_globally(list);
    if !enabled.is_empty() {
        let ids = enabled.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>().join(", ");
        return outcome("plugin", "plugin", Group::Claude, Status::Warn,
            trf!(
                "{ids} is enabled in every Claude session: sessions Alondra did not launch send it their hooks (alondra doctor --fix turns it off)",
                "{ids} está activado en todas las sesiones de Claude: las que no lanza Alondra le mandan sus hooks (alondra doctor --fix lo desactiva)"
            ),
            auto(Action::RepairPlugin));
    }
    if let Some(path) = elsewhere {
        let path = path.display();
        return outcome("plugin", "plugin", Group::Claude, Status::Warn,
            trf!("Alondra's marketplace is registered from {path}, not from this installation",
                "el marketplace de Alondra está registrado desde {path}, no desde esta instalación"),
            auto(Action::RepairPlugin));
    }
    outcome("plugin", "plugin", Group::Claude, Status::Pass,
        tr!("loaded only into the sessions Alondra launches", "cargado solo en las sesiones que lanza Alondra"), None)
}

pub fn hooks_port_check(port: u16) -> Option<Outcome> {
    (port != alondra_core::DEFAULT_PORT).then(|| {
        let default = alondra_core::DEFAULT_PORT;
        outcome("hooks", "hooks", Group::Service, Status::Warn,
            trf!(
                "alondrad uses port {port}, but the plugin's hooks post to {default}: nodes will not report their state",
                "alondrad usa el puerto {port}, pero los hooks del plugin llaman al {default}: los nodos no informarán de su estado"
            ),
            manual(trf!("use port {default} (remove `port` from config.toml and ALONDRA_PORT)", "usa el puerto {default} (quita `port` de config.toml y ALONDRA_PORT)")))
    })
}

/// Older installs without `service.toml` send notifications with their own APNs key; nothing
/// here proves delivery.
pub fn apns_file_check(text: Option<&str>, key_exists: impl Fn(&Path) -> bool) -> Outcome {
    let Some(text) = text else {
        return outcome("notifications", "notifications", Group::Devices, Status::Warn,
            tr!("off: no relay and no APNs key", "desactivadas: sin relay ni llave de APNs"), manual("alondra relay on"));
    };
    let fix = || manual(tr!("alondra relay on", "alondra relay on"));
    match serde_json::from_str::<setup::Apns>(text) {
        Err(e) => outcome("notifications", "notifications", Group::Devices, Status::Fail,
            trf!("apns.json is not valid: {e}", "apns.json no es válido: {e}"), fix()),
        Ok(a) if !key_exists(&a.key_path) => {
            let k = a.key_path.display();
            outcome("notifications", "notifications", Group::Devices, Status::Fail, trf!("the key {k} does not exist", "la llave {k} no existe"), fix())
        }
        Ok(a) => {
            let topic = a.topic;
            outcome("notifications", "notifications", Group::Devices, Status::Pass,
                trf!("with your own APNs key for {topic}, never tested here", "con tu propia llave de APNs para {topic}, sin probar aquí"), None)
        }
    }
}

/// With the relay, a test push the device accepted is the only proof that notifications arrive.
pub fn notifications_check(relay: bool, tested: Option<&super::system::TestPush>, devices: Option<&[Value]>) -> Outcome {
    if !relay {
        return outcome("notifications", "notifications", Group::Devices, Status::Warn,
            tr!("off — local network only", "desactivadas: solo red local"), manual("alondra relay on"));
    }
    let still_paired = |id: &str| devices.is_none_or(|list| list.iter().any(|d| d.get("id").and_then(Value::as_str) == Some(id)));
    match tested.filter(|t| still_paired(&t.device)) {
        Some(t) => {
            let (name, date) = (&t.name, day(t.at));
            outcome("notifications", "notifications", Group::Devices, Status::Pass,
                trf!("tested on {name} ({date})", "probadas en {name} ({date})"), None)
        }
        None => outcome("notifications", "notifications", Group::Devices, Status::Warn,
            tr!("never tested on a device", "nunca probadas en un dispositivo"), manual("alondra doctor --test-push")),
    }
}

/// Asked on a first install, yes by default: without the relay there are no notifications.
pub fn relay_check(on: bool) -> Outcome {
    if on {
        return outcome("relay", "relay", Group::Devices, Status::Pass, tr!("on", "activado"), None);
    }
    outcome("relay", "relay", Group::Devices, Status::Warn, tr!("off — local network only", "desactivado: solo red local"), Some(Fix::Ask {
        question: tr!("Turn on notifications and access away from home? It needs an Alondra account and opens your browser.",
            "¿Activas las notificaciones y el acceso desde fuera de casa? Necesita una cuenta de Alondra y abre el navegador.").into(),
        default: true,
        action: Action::EnableRelay,
        instead: "alondra relay on".into(),
    }))
}

fn direct_reason(code: &str) -> Option<&'static str> {
    Some(match code {
        "local_network_denied" => tr!("macOS doesn't let Alondra (alondrad) use the local network", "macOS no deja a Alondra (alondrad) usar la red local"),
        "relay_off" => tr!("the relay is off", "el relay está desactivado"),
        "relay_disconnected" => tr!("not connected to the relay", "sin conexión con el relay"),
        "captive_portal" => tr!("this network wants a sign-in page first", "esta red pide antes una página de inicio de sesión"),
        "relay_on_same_network" => tr!("the relay is on this same network, so this Mac can't see its public address",
            "el relay está en esta misma red, así que este Mac no ve su dirección pública"),
        "no_udp" => tr!("this network doesn't let UDP out", "esta red no deja salir UDP"),
        "symmetric_nat" => tr!("the router changes the port for every destination", "el router cambia el puerto para cada destino"),
        "router_offers_no_mapping" => tr!("the router doesn't open ports by itself (UPnP, NAT-PMP or PCP)",
            "el router no abre puertos por sí solo (UPnP, NAT-PMP o PCP)"),
        "no_port_mapping" => tr!("the router can open ports but didn't open one", "el router puede abrir puertos pero no ha abierto uno"),
        "no_ipv6" => tr!("no IPv6", "sin IPv6"),
        "port_not_fixed" => tr!("another app uses the usual port, so this one changes on restart",
            "otra app usa el puerto habitual, así que este cambia al reiniciar"),
        _ => return None,
    })
}

/// Whether devices away from home reach this Mac directly, as the daemon diagnoses its iroh
/// endpoint. Going through the relay still works, so this only warns.
pub fn direct_check(health: &Value) -> Option<Outcome> {
    let found = health.get("iroh")?.get("reachability").filter(|v| v.is_object())?;
    let codes: Vec<&str> = found["reasons"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let why = codes.iter().filter_map(|code| direct_reason(code)).collect::<Vec<_>>().join("; ");
    let denied = codes.contains(&"local_network_denied");
    let local_network = manual(tr!("turn on Alondra (alondrad) in System Settings → Privacy & Security → Local Network",
        "activa Alondra (alondrad) en Ajustes del Sistema → Privacidad y seguridad → Red local"));
    let check = |status, detail: String, fix| Some(outcome("direct", "direct connection", Group::Devices, status, detail, fix));
    match found["verdict"].as_str()? {
        "reachable" if denied => check(Status::Warn, why, local_network),
        "reachable" if found["confirmed"] == true => check(Status::Pass,
            tr!("devices away from home connect directly", "los dispositivos fuera de casa conectan directo").into(), None),
        "reachable" => check(Status::Pass, tr!("devices away from home can connect directly", "los dispositivos fuera de casa pueden conectar directo").into(), None),
        "relay_only" => {
            let forward = match (found["port"].as_u64(), found["lan_ip"].as_str()) {
                (Some(port), Some(ip)) => trf!(", or forward UDP port {port} to {ip}", ", o reenvía el puerto UDP {port} a {ip}"),
                _ => String::new(),
            };
            check(Status::Warn, trf!("away from home, devices go through the relay: {why}", "fuera de casa, los dispositivos van por el relay: {why}"),
                if denied { local_network } else if codes.contains(&"no_port_mapping") {
                    manual(trf!("check in your router's settings that UPnP or NAT-PMP is fully on for this Mac{forward}",
                        "comprueba en los ajustes del router que UPnP o NAT-PMP está activado del todo para este Mac{forward}"))
                } else {
                    manual(trf!("turn on UPnP or NAT-PMP in your router's settings{forward}",
                        "activa UPnP o NAT-PMP en los ajustes del router{forward}"))
                })
        }
        "checking" => check(Status::Pass, tr!("checking", "comprobando").into(), None),
        _ if why.is_empty() => check(Status::Pass, tr!("not checked", "sin comprobar").into(), None),
        _ => check(Status::Pass, trf!("not checked: {why}", "sin comprobar: {why}"), None),
    }
}

pub fn devices_check(devices: &[Value]) -> Outcome {
    let n = devices.len();
    if n > 0 {
        outcome("devices", "devices", Group::Devices, Status::Pass, trf!("{n} paired", "{n} emparejados"), None)
    } else {
        outcome("devices", "devices", Group::Devices, Status::Warn, tr!("no paired device", "ningún dispositivo emparejado"),
            manual(tr!("open Alondra on your iPhone", "abre Alondra en tu iPhone")))
    }
}

pub fn claude_version_check(out: Option<&str>) -> Outcome {
    let min = alondra_core::mods::CLAUDE_MIN_TEXT;
    match out.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) if alondra_core::mods::runs_mods(v) => outcome("claude", "claude", Group::Claude, Status::Pass, v.to_string(), None),
        Some(v) => outcome("claude", "claude", Group::Claude, Status::Fail,
            trf!("{v} cannot run Alondra's node mod: nodes need Claude Code {min} or later",
                "{v} no puede ejecutar el mod de nodo de Alondra: los nodos necesitan Claude Code {min} o posterior"),
            manual("claude update")),
        None => outcome("claude", "claude", Group::Claude, Status::Fail,
            tr!("Claude Code isn't installed. Install it, then run alondra doctor --fix again.",
                "Claude Code no está instalado. Instálalo y vuelve a ejecutar alondra doctor --fix."),
            manual("https://docs.claude.com/claude-code")),
    }
}

/// Inspect capabilities rather than assuming a version string has the required flags.
pub fn orchestrator_capability_check(help: Option<&str>) -> Outcome {
    let supported = help.is_some_and(|text| {
        ["--print", "--resume", "--output-format", "stream-json", "--permission-mode", "auto", "--permission-prompts"].iter().all(|flag| text.contains(flag))
    });
    if supported {
        outcome("orchestrator", "orchestrator", Group::Claude, Status::Pass, tr!("Claude supports on-demand orchestration", "Claude admite la orquestación bajo demanda"), None)
    } else {
        outcome("orchestrator", "orchestrator", Group::Claude, Status::Fail,
            tr!("Claude is missing required on-demand runtime capabilities", "A Claude le faltan capacidades necesarias para la ejecución bajo demanda"), manual("claude update"))
    }
}

/// The Claude profile the daemon runs with, shown without any account data.
pub fn profile_check(profile: Result<Option<String>, String>, home: &Path) -> Outcome {
    match profile {
        Ok(None) => outcome("claude.profile", "profile", Group::Claude, Status::Pass, tr!("Claude profile: default", "Perfil de Claude: por defecto"), None),
        Ok(Some(dir)) => {
            let shown = Path::new(&dir).strip_prefix(home).map_or(dir.clone(), |rest| format!("~/{}", rest.display()));
            outcome("claude.profile", "profile", Group::Claude, Status::Pass,
                trf!("Claude profile: {shown} (CLAUDE_CONFIG_DIR)", "Perfil de Claude: {shown} (CLAUDE_CONFIG_DIR)"), None)
        }
        Err(e) => outcome("claude.profile", "profile", Group::Claude, Status::Fail, e,
            manual(tr!("give CLAUDE_CONFIG_DIR and CLAUDE_HOME the same folder, or unset one", "pon la misma carpeta en CLAUDE_CONFIG_DIR y CLAUDE_HOME, o quita una"))),
    }
}

pub fn signin_check(result: anyhow::Result<bool>) -> Outcome {
    match result {
        Ok(true) => outcome("claude.signin", "sign-in", Group::Claude, Status::Pass,
            tr!("signed in for the daemon", "sesión iniciada para el servidor"), None),
        Ok(false) => outcome("claude.signin", "sign-in", Group::Claude, Status::Fail, tr!("not signed in", "sin sesión iniciada"),
            Some(Fix::Ask {
                question: tr!("Sign in to Claude Code now? It opens your browser.", "¿Inicias sesión en Claude Code ahora? Se abre el navegador.").into(),
                default: true,
                action: Action::SignIn,
                instead: tr!("claude auth login, then alondra doctor --fix", "claude auth login, y después alondra doctor --fix").into(),
            })),
        Err(error) => outcome("claude.signin", "sign-in", Group::Claude, Status::Fail, error.to_string(), manual("claude auth login")),
    }
}

/// The node plugin's mod as Claude Code reads it: `claude plugin validate --strict` and its own
/// tests under `claude plugin test`, which also fail when hooks modules are turned off.
pub fn mods_check(plugin: Option<&Path>, validate: Option<(bool, String)>, test: Option<(bool, String)>) -> Outcome {
    let Some(plugin) = plugin else {
        return outcome("mods", "mods", Group::Claude, Status::Fail,
            tr!("Alondra's node plugin was not found", "No se encuentra el plugin de nodo de Alondra"), manual("brew reinstall alondra"));
    };
    let p = plugin.display();
    let (validate_fix, test_fix) = (format!("claude plugin validate --strict {p}"), format!("claude plugin test {p}"));
    let fail = |detail: String, fix: &str| outcome("mods", "mods", Group::Claude, Status::Fail, detail, manual(fix));
    match (validate, test) {
        (None, _) => fail(tr!("claude plugin validate did not finish", "claude plugin validate no terminó").into(), &validate_fix),
        (Some((false, text)), _) => {
            let why = alondra_core::mods::tail(&text);
            fail(trf!("the node mod does not validate: {why}", "el mod de nodo no pasa la validación: {why}"), &validate_fix)
        }
        (_, None) => fail(tr!("claude plugin test did not finish", "claude plugin test no terminó").into(), &test_fix),
        (_, Some((false, text))) => {
            let why = alondra_core::mods::tail(&text);
            fail(trf!("the node mod's tests fail: {why}", "las pruebas del mod de nodo fallan: {why}"), &test_fix)
        }
        _ => outcome("mods", "mods", Group::Claude, Status::Pass,
            tr!("the node mod validates and its tests pass", "el mod de nodo pasa la validación y sus pruebas"), None),
    }
}

pub fn mods_skipped(at: i64) -> Outcome {
    let date = day(at);
    outcome("mods", "mods", Group::Claude, Status::Pass,
        trf!("validated on {date} · --deep checks again", "validado el {date} · --deep lo vuelve a comprobar"), None)
}

pub fn tmux_check(out: Option<&str>, wake: bool) -> Outcome {
    match out.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => outcome("tmux", "tmux", Group::Tools, Status::Pass, v.to_string(), None),
        None => outcome("tmux", "tmux", Group::Tools, if wake { Status::Fail } else { Status::Warn },
            tr!("tmux is missing: Alondra cannot attach a node's session", "falta tmux: Alondra no puede engancharse a la sesión de un nodo"),
            manual("brew install tmux")),
    }
}

/// The oldest tuicr whose `--stdout pr <n>` Alondra relies on.
pub const TUICR_MIN: (u32, u32) = (0, 26);

pub fn tuicr_check(out: Option<&str>) -> Outcome {
    let Some(version) = out.map(str::trim).filter(|v| !v.is_empty()) else {
        return outcome("tuicr", "tuicr", Group::Tools, Status::Warn,
            tr!("tuicr is missing: PR cards have no diff in the TUI", "falta tuicr: las tarjetas de PR no tienen diff en la TUI"), manual("brew install tuicr"));
    };
    let numbers: Vec<u32> = version.rsplit(' ').next().unwrap_or("").split('.').filter_map(|n| n.parse().ok()).collect();
    match numbers.as_slice() {
        [major, minor, ..] if (*major, *minor) >= TUICR_MIN => outcome("tuicr", "tuicr", Group::Tools, Status::Pass, version.to_string(), None),
        _ => outcome("tuicr", "tuicr", Group::Tools, Status::Warn,
            trf!("{version} is older than Alondra needs (0.26)", "{version} es más antiguo de lo que Alondra necesita (0.26)"), manual("brew upgrade tuicr")),
    }
}

fn run_mac(env: &Env) -> Option<Outcome> {
    Some(mac_check(&env.sys.mac()))
}

fn run_config(env: &Env) -> Option<Outcome> {
    Some(config_check(&env.sys.config_path(), env.sys.config_text().as_deref(), service_repair(&env.sys.service(), Action::InstallService)))
}

fn run_claude(env: &Env) -> Option<Outcome> {
    Some(claude_version_check(env.sys.claude(&["--version"], Duration::from_secs(10)).filter(|(ok, _)| *ok).map(|(_, t)| t).as_deref()))
}

fn run_orchestrator(env: &Env) -> Option<Outcome> {
    Some(orchestrator_capability_check(env.sys.claude(&["--help"], Duration::from_secs(10)).filter(|(ok, _)| *ok).map(|(_, t)| t).as_deref()))
}

fn run_profile(env: &Env) -> Option<Outcome> {
    Some(profile_check(env.sys.profile(), &env.sys.user_home()))
}

fn run_signin(env: &Env) -> Option<Outcome> {
    Some(signin_check(env.sys.signed_in()))
}

fn run_projects(env: &Env) -> Option<Outcome> {
    let file = env.sys.home().join(alondra_core::options::FILE_NAME);
    Some(super::projects::projects_check(env.sys.options_text().as_deref(), &file, &env.sys.user_home(), &env.sys.cwd(), env.first_run))
}

fn run_relay(env: &Env) -> Option<Outcome> {
    env.first_run.then(|| relay_check(env.sys.relay()))
}

fn run_direct(env: &Env) -> Option<Outcome> {
    env.sys.health().ok().and_then(|h| direct_check(&h))
}

fn run_identity(env: &Env) -> Option<Outcome> {
    Some(identity_check(env.sys.service_config()))
}

fn run_service(env: &Env) -> Option<Outcome> {
    Some(service_check(&env.sys.service()))
}

fn run_daemon(env: &Env) -> Option<Outcome> {
    let health = env.sys.health();
    let config = env.sys.config();
    let owner = if health.is_err() { env.sys.port_owner(config.port) } else { None };
    let view = env.sys.service();
    let start = service_repair(&view, if view.print.is_some() { Action::RestartService } else { Action::InstallService });
    Some(daemon_check(health, &config, start, owner.as_deref()))
}

fn run_state(env: &Env) -> Option<Outcome> {
    match env.sys.health() {
        Ok(health) if health.get("integrity_issues").is_some() => Some(state_integrity_check(&health)),
        Ok(_) => None,
        Err(_) => env.sys.offline_audit().map(offline_state_check),
    }
}

fn run_startup(env: &Env) -> Option<Outcome> {
    startup_check(env.sys.startup().as_ref(), env.sys.health().is_ok(), &env.sys.home())
}

fn run_upgrade(env: &Env) -> Option<Outcome> {
    env.sys.health().ok().and_then(|h| preflight_check(&h))
}

fn run_update(env: &Env) -> Option<Outcome> {
    env.sys.health().ok().and_then(|h| update_check(&h))
}

fn run_hooks(env: &Env) -> Option<Outcome> {
    hooks_port_check(env.sys.config().port)
}

fn run_plugin(env: &Env) -> Option<Outcome> {
    let view = env.sys.service();
    Some(plugin_check(env.sys.plugins().as_ref(), env.sys.marketplace(), view.repo.as_deref(), view.marketplace_elsewhere.as_deref()))
}

/// `claude plugin validate` and `test` take up to three minutes, so they run only with `--deep`
/// or when the node plugin changed since they last passed.
fn run_mods(env: &Env) -> Option<Outcome> {
    if !env.sys.marketplace() {
        return None;
    }
    let plugin = env.sys.node_plugin();
    let fingerprint = plugin.as_deref().and_then(|dir| env.sys.fingerprint(dir));
    let memory = env.sys.memory();
    if !env.deep
        && let (Some(fingerprint), Some(passed)) = (&fingerprint, &memory.mods)
        && passed.fingerprint == *fingerprint
    {
        return Some(mods_skipped(passed.at));
    }
    let (validate, test) = match &plugin {
        Some(dir) => {
            let dir = dir.display().to_string();
            (env.sys.claude(&["plugin", "validate", "--strict", &dir], Duration::from_secs(60)),
                env.sys.claude(&["plugin", "test", &dir], Duration::from_secs(120)))
        }
        None => (None, None),
    };
    let checked = mods_check(plugin.as_deref(), validate, test);
    if checked.status == Status::Pass
        && let Some(fingerprint) = fingerprint
    {
        env.sys.remember(&Memory { mods: Some(super::system::ModsPass { fingerprint, at: env.sys.now() }), ..memory });
    }
    Some(checked)
}

fn run_devices(env: &Env) -> Option<Outcome> {
    env.sys.health().ok()?;
    Some(devices_check(&env.sys.devices().unwrap_or_default()))
}

fn run_notifications(env: &Env) -> Option<Outcome> {
    if !env.sys.service_config() {
        return Some(apns_file_check(env.sys.apns().as_deref(), |p| env.sys.exists(p)));
    }
    let devices = env.sys.health().ok().and(env.sys.devices());
    Some(notifications_check(env.sys.relay(), env.sys.memory().test_push.as_ref(), devices.as_deref()))
}

fn run_tmux(env: &Env) -> Option<Outcome> {
    Some(tmux_check(env.sys.tool("tmux", &["-V"]).as_deref(), env.sys.config().wake))
}

fn run_tuicr(env: &Env) -> Option<Outcome> {
    Some(tuicr_check(env.sys.tool("tuicr", &["--version"]).as_deref()))
}

/// In the order `--fix` walks them: what a later check needs (Claude, the service identity, the
/// service itself) is repaired first.
pub const CHECKS: &[Check] = &[
    Check { id: "mac", run: run_mac },
    Check { id: "claude", run: run_claude },
    Check { id: "orchestrator", run: run_orchestrator },
    Check { id: "claude.profile", run: run_profile },
    Check { id: "claude.signin", run: run_signin },
    Check { id: "projects", run: run_projects },
    Check { id: "identity", run: run_identity },
    Check { id: "config", run: run_config },
    Check { id: "service", run: run_service },
    Check { id: "alondrad", run: run_daemon },
    Check { id: "state", run: run_state },
    Check { id: "startup", run: run_startup },
    Check { id: "upgrade", run: run_upgrade },
    Check { id: "update", run: run_update },
    Check { id: "hooks", run: run_hooks },
    Check { id: "plugin", run: run_plugin },
    Check { id: "mods", run: run_mods },
    Check { id: "relay", run: run_relay },
    Check { id: "direct", run: run_direct },
    Check { id: "devices", run: run_devices },
    Check { id: "notifications", run: run_notifications },
    Check { id: "tmux", run: run_tmux },
    Check { id: "tuicr", run: run_tuicr },
];
