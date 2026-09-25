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
    let child = Command::new(program)
        .args(args)
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

pub fn jevi_doctor_has_key(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim();
        l.starts_with("key:") && l.trim_start_matches("key:").trim() == "present"
    })
}

pub fn jevi_ask_ok(stdout: &str) -> Result<(), String> {
    let v: Value = serde_json::from_str(stdout.lines().find(|l| l.trim_start().starts_with('{')).unwrap_or("")).map_err(|e| e.to_string())?;
    if v.get("ok") == Some(&Value::Bool(true)) && v.pointer("/answers/answer").is_some() {
        Ok(())
    } else {
        Err(v.get("error").map(|e| e.to_string()).unwrap_or_else(|| v.to_string()).chars().take(200).collect())
    }
}

pub fn jevi_checks() -> Vec<Check> {
    let install = tr!("install jevi from its repo (see INSTALL.md) and put it on PATH", "instala jevi desde su repo (ver INSTALL.md) y ponlo en el PATH");
    let Some((_, version)) = output("jevi", &["--version"], Duration::from_secs(5)) else {
        return vec![check("jevi", Status::Fail, tr!("jevi is not on PATH", "jevi no está en el PATH"), install)];
    };
    let version = version.trim().to_string();
    let set_key = tr!("pbpaste | jevi config set-key openrouter", "pbpaste | jevi config set-key openrouter");
    let mut out = vec![check("jevi", Status::Pass, version, "")];
    match output("jevi", &["doctor"], Duration::from_secs(10)) {
        Some((_, text)) if jevi_doctor_has_key(&text) => out.push(check("jevi key", Status::Pass, tr!("jevi has a key", "jevi tiene clave"), "")),
        _ => {
            out.push(check("jevi key", Status::Fail, tr!("jevi has no key", "jevi no tiene clave"), set_key));
            return out;
        }
    }
    let ask = output("jevi", &["ask", "Is this a greeting?", "-s", "hello", "--json", "--soft", "--timeout", "8000"], Duration::from_secs(12));
    out.push(match ask.map(|(_, t)| jevi_ask_ok(&t)) {
        Some(Ok(())) => check("jevi ask", Status::Pass, tr!("jevi answers", "jevi responde"), ""),
        Some(Err(e)) => check("jevi ask", Status::Fail, trf!("jevi does not answer: {e}", "jevi no responde: {e}"), "jevi doctor --live"),
        None => check("jevi ask", Status::Fail, tr!("jevi did not answer in 12 s", "jevi no respondió en 12 s"), "jevi doctor --live"),
    });
    out
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

/// `/v1/push/check` sends to a made-up token: Apple answering BadDeviceToken proves the key,
/// team and topic are accepted; 403 means the key or team is wrong.
pub fn push_round_trip(result: Result<Value, String>) -> Check {
    let restart = tr!("check key_id, team_id and topic in apns.json, then restart alondrad", "revisa key_id, team_id y topic en apns.json y reinicia alondrad");
    match result {
        Err(e) => check("apns push", Status::Fail, e, restart),
        Ok(v) => {
            let status = v.get("status").and_then(Value::as_u64).unwrap_or(0);
            let body = v.get("body").and_then(Value::as_str).unwrap_or("").to_string();
            match status {
                400 if body.contains("BadDeviceToken") => {
                    check("apns push", Status::Pass, tr!("Apple accepts the key (test token rejected as expected)", "Apple acepta la llave (rechaza el token de prueba, como debe)"), "")
                }
                403 => check("apns push", Status::Fail, trf!("Apple rejects the key: {body}", "Apple rechaza la llave: {body}"), restart),
                _ => check("apns push", Status::Warn, trf!("unexpected answer from Apple: {status} {body}", "respuesta inesperada de Apple: {status} {body}"), restart),
            }
        }
    }
}

pub fn claude_version_check(out: Option<&str>) -> Check {
    match out.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => check("claude", Status::Pass, v.to_string(), ""),
        None => check("claude", Status::Fail, tr!("Claude Code (`claude`) is not on PATH", "Claude Code (`claude`) no está en el PATH"), "https://docs.claude.com/claude-code"),
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
    let health = client.get::<Value>("/v1/health").map_err(|e| e.to_string());
    let up = health.is_ok();
    if let Ok(value) = &health {
        checks.push(state_integrity_check(value));
    } else if home.join("alondra.db").exists() {
        checks.push(match state::audit(&home) {
            Ok((seq, tasks)) => check("state", Status::Pass, trf!("offline replay valid through event {seq}; {tasks} tasks", "reproducción sin servidor válida hasta el evento {seq}; {tasks} tareas"), ""),
            Err(e) => check("state", Status::Fail, format!("{e:#}"), "alondra state repair (only with a complete event log and the daemon stopped)"),
        });
    }
    checks.push(daemon_check(health, &local));
    checks.extend(hooks_port_check(local.port));

    let uid = unsafe { libc::getuid() };
    let print = output("launchctl", &["print", &format!("gui/{uid}/{}", local.service_label)], Duration::from_secs(5)).filter(|(ok, _)| *ok).map(|(_, t)| t);
    checks.push(service_check(print.as_deref(), &local.service_label));

    let claude = output("claude", &["--version"], Duration::from_secs(10)).filter(|(ok, _)| *ok).map(|(_, t)| t);
    checks.push(claude_version_check(claude.as_deref()));
    let repo = std::env::current_dir().ok().and_then(|d| setup::find_repo(&d));
    checks.push(plugin_check(setup::plugin_list().as_ref(), setup::has_marketplace(), repo.as_deref()));

    let tmux = output("tmux", &["-V"], Duration::from_secs(5)).filter(|(ok, _)| *ok).map(|(_, t)| t);
    checks.push(tmux_check(tmux.as_deref(), local.wake));
    checks.extend(jevi_checks());
    let tuicr = output("tuicr", &["--version"], Duration::from_secs(5)).filter(|(ok, _)| *ok).map(|(_, t)| t);
    checks.push(tuicr_check(tuicr.as_deref()));

    let apns_text = std::fs::read_to_string(home.join("apns.json")).ok();
    let apns = apns_file_check(apns_text.as_deref(), |p| p.is_file());
    let apns_ok = apns.status == Status::Pass;
    checks.push(apns);
    if apns_ok && up {
        checks.push(push_round_trip(client.post("/v1/push/check", &json!({})).map_err(|e| e.to_string())));
    }
    if up {
        let devices = client.get::<Value>("/v1/devices").ok().and_then(|v| v.get("devices").and_then(Value::as_array).map(Vec::len)).unwrap_or(0);
        checks.push(if devices > 0 {
            check("pairing", Status::Pass, trf!("{devices} paired device(s)", "{devices} dispositivo(s) vinculado(s)"), "")
        } else {
            check("pairing", Status::Warn, tr!("no paired iPhone", "ningún iPhone vinculado"), "alondra pair")
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
    fn jevi_output_is_read_without_the_key() {
        assert!(jevi_doctor_has_key("provider:  openrouter\nkey:       present\nmodel: x"));
        assert!(!jevi_doctor_has_key("key:       MISSING"));
        assert!(!jevi_doctor_has_key(""));
        assert!(jevi_ask_ok(r#"{"ok":true,"answers":{"answer":{"verdict":"yes"}}}"#).is_ok());
        assert!(jevi_ask_ok(r#"{"ok":false,"error":"no key"}"#).unwrap_err().contains("no key"));
        assert!(jevi_ask_ok("garbage").is_err());
    }

    #[test]
    fn push_check_reads_apples_answer() {
        assert_eq!(push_round_trip(Ok(json!({"status": 400, "body": "{\"reason\":\"BadDeviceToken\"}"}))).status, Status::Pass);
        assert_eq!(push_round_trip(Ok(json!({"status": 403, "body": "{\"reason\":\"InvalidProviderToken\"}"}))).status, Status::Fail);
        assert_eq!(push_round_trip(Ok(json!({"status": 500, "body": ""}))).status, Status::Warn);
        assert_eq!(push_round_trip(Err("APNs no configurado".into())).status, Status::Fail);
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
        assert_eq!(claude_version_check(Some("2.1.280 (Claude Code)\n")).detail, "2.1.280 (Claude Code)");
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
