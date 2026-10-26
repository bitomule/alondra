//! The noun commands: `device`, `mac`, `account`, `relay` and `config`. Each verb is one function;
//! `main.rs` only parses and dispatches.

use std::time::Duration;

use alondra_core::now_millis;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::client::{self, Client};

/// Something only a person can do (sign in, confirm, type a name): `main` exits with 3.
#[derive(Debug)]
pub struct NeedsPerson(pub String);

impl std::fmt::Display for NeedsPerson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NeedsPerson {}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn daemon() -> Client {
    Client::new(Duration::from_secs(10))
}

// device

pub fn device_list(json: bool) -> Result<()> {
    let client = daemon();
    let pending: Value = client.get("/devices/pending")?;
    let devices: Value = client.get("/devices")?;
    if json {
        return print_json(&json!({
            "schema": "alondra.device/1", "ok": true,
            "devices": devices.get("devices").cloned().unwrap_or(json!([])),
            "pending": pending.get("pending").cloned().unwrap_or(json!([])),
            "expired": devices.get("expired").cloned().unwrap_or(json!([])),
        }));
    }
    pending_lines(&pending).iter().chain(&device_lines(&devices, now_millis())).for_each(|line| println!("{line}"));
    Ok(())
}

pub fn device_decide(id: &str, approve: bool) -> Result<()> {
    let verb = if approve { "approve" } else { "deny" };
    daemon().post(&format!("/devices/pending/{}/{verb}", crate::mcp::urlencode(id)), &json!({}))?;
    println!("{}", if approve {
        trf!("Pairing request {id} approved.", "Petición de emparejamiento {id} aprobada.")
    } else {
        trf!("Pairing request {id} denied.", "Petición de emparejamiento {id} rechazada.")
    });
    Ok(())
}

pub fn device_remove(id: &str) -> Result<()> {
    daemon().delete(&format!("/devices/{}", crate::mcp::urlencode(id)))?;
    println!("{}", trf!("Device {id} unpaired.", "Dispositivo {id} desvinculado."));
    Ok(())
}

pub fn device_invite() -> Result<()> {
    crate::service_pair::run(&client::config().home)
}

fn pending_lines(pending: &Value) -> Vec<String> {
    pending["pending"].as_array().into_iter().flatten().map(|request| {
        let (id, name, code) = (
            request["id"].as_str().unwrap_or(""),
            request["name"].as_str().unwrap_or(""),
            request["code"].as_str().unwrap_or(""),
        );
        trf!(
            "Waiting: “{name}” wants to use Alondra. Code {code}. Approve: alondra device approve {id}",
            "Pendiente: «{name}» quiere usar Alondra. Código {code}. Aprobar: alondra device approve {id}"
        )
    }).collect()
}

/// How long ago `then` was, the way people say it: minutes, then hours, then days.
fn ago(then: i64, now: i64) -> String {
    let minutes = (now - then).max(0) / 60_000;
    let (hours, days) = (minutes / 60, minutes / 1_440);
    match (days, hours) {
        (0, 0) => trf!("{minutes} min ago", "hace {minutes} min"),
        (0, _) => trf!("{hours} h ago", "hace {hours} h"),
        (1, _) => tr!("1 day ago", "hace 1 día").into(),
        _ => trf!("{days} days ago", "hace {days} días"),
    }
}

/// The paired devices with their last use, then the ones Alondra unpaired on its own lately.
pub fn device_lines(v: &Value, now: i64) -> Vec<String> {
    let list = v.get("devices").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    let mut lines = Vec::new();
    if list.is_empty() {
        lines.push(tr!("No paired devices. Devices on the same network pair on their own: open the app, or run alondra mac connect on the other computer.", "No hay dispositivos vinculados. Los dispositivos de la misma red se emparejan solos: abre la app, o ejecuta alondra mac connect en el otro ordenador.").to_string());
    }
    for d in list {
        let mut last = match d.get("last_seen").and_then(|x| x.as_i64()) {
            Some(at) => { let ago = ago(at, now); trf!("used {ago}", "usado {ago}") }
            None => {
                let paired = d.get("created_at").and_then(|x| x.as_i64()).map_or_else(String::new, |at| ago(at, now));
                trf!("never used, paired {paired}", "sin uso, vinculado {paired}")
            }
        };
        if d.get("push_failing").and_then(|x| x.as_bool()) == Some(true) {
            last.push_str(tr!(" · the relay no longer has its notifications", " · el relay ya no tiene sus notificaciones"));
        }
        lines.push(format!("{}  {}  ({last})", d["id"].as_str().unwrap_or(""), d["name"].as_str().unwrap_or("")));
    }
    let expired = v.get("expired").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    if !expired.is_empty() {
        lines.push(tr!("Unpaired on their own (open the app on the device, or run alondra mac connect on it, to pair it again):", "Desvinculados solos (abre la app en el dispositivo, o ejecuta alondra mac connect en él, para volver a vincularlo):").to_string());
    }
    for e in expired {
        let when = ago(e["expired_at_ms"].as_i64().unwrap_or(now), now);
        let why = match e["reason"].as_str() {
            Some("relay_forgot") => tr!("the relay no longer has its notifications", "el relay ya no tiene sus notificaciones").to_string(),
            _ => {
                let days = e["days"].as_f64().unwrap_or(0.0);
                trf!("unused for {days} days", "sin uso en {days} días")
            }
        };
        let name = e["name"].as_str().unwrap_or("");
        lines.push(trf!("  {name} — unpaired on its own {when}: {why}", "  {name} — desvinculado solo {when}: {why}"));
    }
    lines
}

// mac

/// A sandbox never reads the real Alondra account: its Macs and its relay are the sandbox's.
fn reads_account() -> bool {
    std::env::var("ALONDRA_SANDBOX").as_deref() != Ok("1")
}

/// How this terminal reaches a Mac in `mac list`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Via {
    ThisMac,
    LocalNetwork,
    Relay,
    /// In the account but not connected from this terminal yet.
    Account,
}

impl Via {
    fn id(self) -> &'static str {
        match self {
            Via::ThisMac => "this_mac",
            Via::LocalNetwork => "local_network",
            Via::Relay => "relay",
            Via::Account => "account",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Via::ThisMac => tr!("this Mac", "este Mac"),
            Via::LocalNetwork => tr!("local network", "red local"),
            Via::Relay => tr!("through the relay", "por el relay"),
            Via::Account => tr!("in your account, not connected: alondra mac connect", "en tu cuenta, sin conectar: alondra mac connect"),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct MacRow {
    pub id: String,
    pub name: Option<String>,
    pub via: Via,
}

/// This Mac first, then the Macs this terminal paired with, then the account's other Macs. A
/// Mac shows once, by the first way that reaches it.
pub fn mac_rows(this: Option<(String, String)>, direct: Vec<(String, String)>, relay: Vec<String>, account: Vec<String>) -> Vec<MacRow> {
    let mut rows: Vec<MacRow> = Vec::new();
    let mut add = |id: String, name: Option<String>, via: Via| {
        if !rows.iter().any(|row| row.id == id) {
            rows.push(MacRow { id, name, via });
        }
    };
    if let Some((id, name)) = this {
        add(id, Some(name), Via::ThisMac);
    }
    direct.into_iter().for_each(|(id, name)| add(id, Some(name), Via::LocalNetwork));
    relay.into_iter().for_each(|id| add(id, None, Via::Relay));
    account.into_iter().for_each(|id| add(id, None, Via::Account));
    rows
}

fn this_mac_id() -> Option<String> {
    let text = std::fs::read_to_string(client::config().home.join("service.toml")).ok()?;
    toml::from_str::<toml::Table>(&text).ok()?.get("mac_id")?.as_str().map(str::to_owned)
}

pub fn mac_list(json: bool) -> Result<()> {
    let this = this_mac_id().map(|id| (id, client::computer_name()));
    let direct = crate::direct_client::load()?.into_iter().map(|mac| (mac.mac_id, mac.name)).collect();
    let relay = crate::service_client::paired()?;
    let account = if reads_account() && crate::account::has_session() {
        crate::service_client::machines().unwrap_or_else(|error| {
            eprintln!("{}", trf!("Your account's Macs are not listed: {error:#}", "No se listan los Macs de tu cuenta: {error:#}"));
            vec![]
        })
    } else {
        vec![]
    };
    let rows = mac_rows(this, direct, relay, account);
    if json {
        let macs: Vec<Value> = rows.iter().map(|row| json!({"id": row.id, "name": row.name, "via": row.via.id(), "connected": row.via != Via::Account})).collect();
        return print_json(&json!({"schema": "alondra.mac/1", "ok": true, "macs": macs}));
    }
    if rows.is_empty() {
        println!("{}", tr!("No Macs yet. Connect to one with alondra mac connect.", "Aún no hay Macs. Conéctate a uno con alondra mac connect."));
    }
    for row in rows {
        let via = row.via.text();
        match row.name {
            Some(name) => println!("{name}  {}  ({via})", row.id),
            None => println!("{}  ({via})", row.id),
        }
    }
    Ok(())
}

pub fn mac_connect(mac: Option<&str>, host: Option<&str>, repair: bool) -> Result<()> {
    match mac {
        Some(mac) => crate::service_client::connect(mac, repair),
        None => crate::direct_client::connect(host, repair),
    }
}

pub fn mac_forget(mac: &str) -> Result<()> {
    let mut gone = crate::direct_client::forget(&client::config().home, mac)?;
    if crate::service_client::forget(mac)? {
        gone.push(mac.to_string());
    }
    if gone.is_empty() {
        bail!(trf!("This terminal has no Mac called {mac}; see them with alondra mac list", "Este terminal no tiene ningún Mac llamado {mac}; míralos con alondra mac list"));
    }
    println!("{}", trf!("Forgot {mac}. To use it again: alondra mac connect", "{mac} olvidado. Para volver a usarlo: alondra mac connect"));
    Ok(())
}

pub fn mac_reset_identity(no_input: bool) -> Result<()> {
    if !crate::doctor::can_ask(no_input) {
        return Err(NeedsPerson(tr!(
            "Resetting this Mac's identity needs you to type its name: run alondra mac reset-identity in a terminal",
            "Restablecer la identidad de este Mac necesita que escribas su nombre: ejecuta alondra mac reset-identity en una terminal"
        ).into()).into());
    }
    crate::setup::reset_identity(|| crate::doctor::run(crate::doctor::Options { fix: true, ..Default::default() }))
}

// account

pub fn account_status(json: bool) -> Result<()> {
    let who = crate::account::signed_in_as()?;
    if json {
        return print_json(&json!({"schema": "alondra.account/1", "ok": true, "signed_in": who.is_some(), "account": who}));
    }
    match who {
        Some(name) => println!("{}", trf!("Signed in as {name}.", "Sesión iniciada como {name}.")),
        None => println!("{}", tr!("Not signed in. Sign in with alondra account login.", "Sin sesión. Inicia sesión con alondra account login.")),
    }
    Ok(())
}

pub fn account_login(no_input: bool) -> Result<()> {
    if !crate::doctor::can_ask(no_input) {
        return Err(NeedsPerson(tr!("Signing in opens your browser: run alondra account login in a terminal", "Iniciar sesión abre el navegador: ejecuta alondra account login en una terminal").into()).into());
    }
    crate::account::login()
}

// relay

pub fn relay_status(json: bool) -> Result<()> {
    let on = crate::service_setup::has_relay(&client::config().home)?;
    let account = if on && reads_account() && crate::account::has_session() { crate::account::signed_in_as().ok().flatten() } else { None };
    if json {
        return print_json(&json!({"schema": "alondra.relay/1", "ok": true, "on": on, "account": account}));
    }
    println!("{}", match (on, account) {
        (true, Some(account)) => trf!("Notifications and access away from home: on ({account})", "Notificaciones y acceso fuera de casa: activados ({account})"),
        (true, None) => tr!("Notifications and access away from home: on", "Notificaciones y acceso fuera de casa: activados").into(),
        (false, _) => tr!("Local network only — turn on notifications with alondra relay on", "Solo red local: activa las notificaciones con alondra relay on").into(),
    });
    Ok(())
}

pub fn relay_on(no_input: bool) -> Result<()> {
    if crate::service_setup::has_relay(&client::config().home)? {
        println!("{}", tr!("Notifications and access away from home are already on.", "Las notificaciones y el acceso fuera de casa ya están activados."));
        return Ok(());
    }
    if !crate::doctor::can_ask(no_input) {
        return Err(NeedsPerson(tr!("Turning on the relay signs in in your browser: run alondra relay on in a terminal", "Activar el relay inicia sesión en el navegador: ejecuta alondra relay on en una terminal").into()).into());
    }
    crate::doctor::system::Real::new().enable_relay()?;
    println!("{}", tr!("Notifications and access away from home: on", "Notificaciones y acceso fuera de casa: activados"));
    Ok(())
}

pub fn relay_off(no_input: bool) -> Result<()> {
    let home = &client::config().home;
    if !crate::service_setup::has_relay(home)? {
        println!("{}", tr!("The relay is already off: local network only.", "El relay ya está desactivado: solo red local."));
        return Ok(());
    }
    let warning = tr!(
        "Devices that reach this Mac only through the relay lose access until alondra relay on.",
        "Los dispositivos que llegan a este Mac solo por el relay pierden el acceso hasta alondra relay on."
    );
    if !crate::doctor::can_ask(no_input) {
        return Err(NeedsPerson(trf!("{warning} Run alondra relay off in a terminal to confirm it.", "{warning} Ejecuta alondra relay off en una terminal para confirmarlo.")).into());
    }
    let confirmed = cliclack::confirm(trf!("{warning} Turn it off?", "{warning} ¿Lo desactivo?")).initial_value(false).interact()?;
    if !confirmed {
        println!("{}", tr!("Nothing changed.", "No ha cambiado nada."));
        return Ok(());
    }
    if crate::service_setup::remove_relay(home)? {
        crate::doctor::system::Real::new().restart_if_running()?;
    }
    println!("{}", tr!("Local network only — turn notifications back on with alondra relay on", "Solo red local: vuelve a activar las notificaciones con alondra relay on"));
    Ok(())
}

// config

/// The keys `config list` shows, in order, with their value now.
fn config_rows() -> Result<Vec<(&'static str, String)>> {
    let c = alondra_core::config::Config::load().map_err(|e| anyhow::anyhow!(e))?;
    let projects = std::fs::read_to_string(alondra_core::options::path_in(&c.home)).ok()
        .and_then(|text| alondra_core::options::OptionsFile::parse(&text).ok())
        .map(|file| file.projects_root(&alondra_core::config::home_dir(&|k| std::env::var(k).ok())).display().to_string())
        .unwrap_or_default();
    Ok(vec![
        ("home", c.home.display().to_string()),
        ("name", c.name.clone()),
        ("projects_root", projects),
        ("port", c.port.to_string()),
        ("listen", c.listen.to_string()),
        ("public_url", c.public_url.clone().unwrap_or_default()),
        ("legacy_bridge", c.legacy_bridge.to_string()),
        ("wake", c.wake.to_string()),
        ("retention_hours", c.retention_hours.to_string()),
        ("service_label", c.service_label.clone()),
        ("resume_dead_nodes", c.resume_dead_nodes.to_string()),
        ("close_free_nodes_after", c.close_free_nodes_after.map(|d| format!("{}s", d.as_secs())).unwrap_or_else(|| "off".into())),
        ("compact_idle_nodes_at", c.compact_idle_nodes_at.map(|p| format!("{p}%")).unwrap_or_else(|| "off".into())),
        ("compact_orchestrator_at", c.compact_orchestrator_at.map(|p| format!("{p}%")).unwrap_or_else(|| "off".into())),
        ("device_expiry_days", c.device_expiry_days.map(|d| d.to_string()).unwrap_or_else(|| "off".into())),
        ("auto_update", c.auto_update.to_string()),
        ("max_working_nodes", node_limit(&c.home)),
        ("report.repo", c.report_repo.clone().unwrap_or_default()),
    ])
}

/// What `options.toml` says; the daemon turns `auto` into a number from this Mac's size.
fn node_limit(home: &std::path::Path) -> String {
    let text = std::fs::read_to_string(alondra_core::options::path_in(home)).unwrap_or_default();
    match alondra_core::options::OptionsFile::parse(&text).map(|file| file.max_working_nodes) {
        Ok(alondra_core::options::NodeLimit::Fixed(count)) => count.to_string(),
        Ok(alondra_core::options::NodeLimit::Auto) => alondra_core::options::AUTO.into(),
        Err(_) => {
            let file = alondra_core::options::FILE_NAME;
            trf!("unreadable: {file} has an error", "ilegible: {file} tiene un error")
        }
    }
}

pub fn config_list(json: bool) -> Result<()> {
    let rows = config_rows()?;
    if json {
        let settings: serde_json::Map<String, Value> = rows.into_iter().map(|(k, v)| (k.to_string(), Value::String(v))).collect();
        return print_json(&json!({"schema": "alondra.config/1", "ok": true, "settings": settings}));
    }
    rows.iter().for_each(|(k, v)| println!("{k} = {v}"));
    Ok(())
}

pub fn config_get(key: &str, json: bool) -> Result<()> {
    let rows = config_rows()?;
    let Some((_, value)) = rows.iter().find(|(k, _)| *k == key) else {
        bail!(trf!("unknown setting: {key}", "ajuste desconocido: {key}"));
    };
    if json {
        return print_json(&json!({"schema": "alondra.config/1", "ok": true, "key": key, "value": value}));
    }
    println!("{value}");
    Ok(())
}

/// The keys `config set` and `config unset` take, for their messages.
pub fn settable() -> Vec<&'static str> {
    let mut keys: Vec<&str> = alondra_core::config::SETTABLE.iter().map(|(k, _)| *k).collect();
    keys.extend(["projects_root", "max_working_nodes"]);
    keys
}

/// What a key takes, for the message when a value is refused.
fn expected(key: &str) -> &'static str {
    match key {
        "name" => tr!("your name, as the nodes address you", "tu nombre, como te llaman los nodos"),
        "public_url" => tr!("a URL that starts with http:// or https://", "una URL que empiece por http:// o https://"),
        "listen" => tr!("an IP address, such as 127.0.0.1 or 0.0.0.0", "una dirección IP, como 127.0.0.1 o 0.0.0.0"),
        "port" => tr!("a port number from 1 to 65535", "un número de puerto del 1 al 65535"),
        "wake" => tr!("true or false", "true o false"),
        "retention_hours" => tr!("a number of hours above 0", "un número de horas mayor que 0"),
        "service_label" => tr!("a launchd label such as dev.alondra.alondrad", "una etiqueta de launchd como dev.alondra.alondrad"),
        "report.repo" => tr!("a GitHub repository as owner/repo", "un repositorio de GitHub como dueño/repo"),
        _ => "",
    }
}

pub fn config_set(key: &str, value: Option<&str>) -> Result<()> {
    match (key, value) {
        ("max_working_nodes", Some(value)) => return crate::cli::set_node_limit(value),
        ("max_working_nodes", None) => return crate::cli::set_node_limit(alondra_core::options::AUTO),
        ("projects_root", Some(value)) => return set_projects_root(value),
        ("projects_root", None) => bail!(tr!("projects_root always has a folder: choose another with alondra config set projects_root <folder>", "projects_root siempre tiene una carpeta: elige otra con alondra config set projects_root <carpeta>")),
        _ => {}
    }
    if !alondra_core::config::SETTABLE.iter().any(|(k, _)| *k == key) {
        let keys = settable().join(", ");
        bail!(trf!("{key} cannot be changed here; settable: {keys}", "{key} no se cambia aquí; se puede cambiar: {keys}"));
    }
    let restart = match daemon().put("/settings/config", &json!({"key": key, "value": value})) {
        Ok(answer) => answer.get("restart").and_then(Value::as_bool).unwrap_or(false),
        Err(error) if client::delivery(&error) == client::Delivery::Unsent => {
            set_in_file(key, value)?;
            false
        }
        Err(error) => return Err(refused(key, error)),
    };
    if restart {
        let sys = crate::doctor::system::Real::new();
        if key == "service_label" { sys.relabel_service()? } else { sys.restart_if_running()? }
    }
    match value {
        Some(value) => println!("{key} = {value}"),
        None => println!("{}", trf!("{key}: back to its default", "{key}: vuelve a su valor por defecto")),
    }
    Ok(())
}

/// With the daemon stopped nobody else writes `config.toml`; it reads the file when it starts.
fn set_in_file(key: &str, value: Option<&str>) -> Result<()> {
    use alondra_core::config::SetError;
    match alondra_core::config::set_key(&client::config().home, key, value) {
        Ok(()) => Ok(()),
        Err(SetError::Invalid) => bail!(invalid(key)),
        Err(SetError::UnknownKey) => bail!(trf!("{key} cannot be changed here", "{key} no se cambia aquí")),
        Err(SetError::File(why)) => bail!(trf!("config.toml could not be changed: {why}", "no se pudo cambiar config.toml: {why}")),
    }
}

fn invalid(key: &str) -> String {
    let wants = expected(key);
    trf!("{key} takes {wants}", "{key} admite {wants}")
}

fn refused(key: &str, error: anyhow::Error) -> anyhow::Error {
    let invalid_value = error.chain().any(|cause| cause.downcast_ref::<client::Answered>().is_some_and(|a| a.status == 422));
    if invalid_value { anyhow::anyhow!(invalid(key)) } else { error }
}

fn set_projects_root(value: &str) -> Result<()> {
    let answer = daemon().put("/settings", &json!({"projects_root": value, "lang": if crate::i18n::lang() == crate::i18n::Lang::Es { "es" } else { "en" }}))
        .context(tr!("Alondra is not running on this Mac: start it with alondra doctor --fix and try again", "Alondra no está en marcha en este Mac: arráncalo con alondra doctor --fix y vuelve a probar"))?;
    let root = answer.get("projects_root").and_then(Value::as_str).unwrap_or(value);
    println!("projects_root = {root}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_list_shows_this_mac_first_and_each_mac_once() {
        let rows = mac_rows(
            Some(("mac-home".into(), "Mac Studio".into())),
            vec![("mac-lan".into(), "Mac mini".into())],
            vec!["mac-relay".into(), "mac-lan".into()],
            vec!["mac-home".into(), "mac-relay".into(), "mac-work".into()],
        );
        let shown: Vec<(&str, Via)> = rows.iter().map(|row| (row.id.as_str(), row.via)).collect();
        assert_eq!(shown, [("mac-home", Via::ThisMac), ("mac-lan", Via::LocalNetwork), ("mac-relay", Via::Relay), ("mac-work", Via::Account)]);
        assert!(mac_rows(None, vec![], vec![], vec![]).is_empty());
    }

    #[test]
    fn config_set_takes_setups_keys_and_the_node_limit() {
        let keys = settable();
        for key in ["name", "projects_root", "public_url", "listen", "port", "wake", "retention_hours", "service_label", "report.repo", "max_working_nodes"] {
            assert!(keys.contains(&key), "{key}");
        }
        assert_eq!(keys.len(), 10);
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        assert_eq!(invalid("port"), "port takes a port number from 1 to 65535");
    }

    #[test]
    fn the_device_list_says_when_each_device_was_last_used_and_which_went_on_their_own() {
        const DAY: i64 = 86_400_000;
        const NOW: i64 = 100 * DAY;
        let listing = json!({
            "devices": [
                {"id": "phone-a", "name": "iPhone", "created_at": 0, "last_seen": NOW - 5 * 60_000, "push_failing": false},
                {"id": "client-b", "name": "Terminal", "created_at": NOW - 3 * DAY, "last_seen": null, "push_failing": true}
            ],
            "expired": [
                {"id": "phone-c", "name": "Old iPhone", "reason": "unused", "days": 30.0, "expired_at_ms": NOW - 3 * DAY},
                {"id": "phone-d", "name": "iPad", "reason": "relay_forgot", "days": null, "expired_at_ms": NOW - 2 * 3_600_000}
            ]
        });
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        assert_eq!(device_lines(&listing, NOW), [
            "phone-a  iPhone  (used 5 min ago)",
            "client-b  Terminal  (never used, paired 3 days ago · the relay no longer has its notifications)",
            "Unpaired on their own (open the app on the device, or run alondra mac connect on it, to pair it again):",
            "  Old iPhone — unpaired on its own 3 days ago: unused for 30 days",
            "  iPad — unpaired on its own 2 h ago: the relay no longer has its notifications",
        ]);
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        assert_eq!(device_lines(&listing, NOW), [
            "phone-a  iPhone  (usado hace 5 min)",
            "client-b  Terminal  (sin uso, vinculado hace 3 días · el relay ya no tiene sus notificaciones)",
            "Desvinculados solos (abre la app en el dispositivo, o ejecuta alondra mac connect en él, para volver a vincularlo):",
            "  Old iPhone — desvinculado solo hace 3 días: sin uso en 30 días",
            "  iPad — desvinculado solo hace 2 h: el relay ya no tiene sus notificaciones",
        ]);
        let pending = json!({"pending": [{"id": "req-1", "name": "iPhone", "code": "482 913"}]});
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        assert_eq!(pending_lines(&pending), ["Waiting: “iPhone” wants to use Alondra. Code 482 913. Approve: alondra device approve req-1"]);
    }
}
