#[macro_use]
mod i18n;
mod attach;
mod account;
mod cli;
mod client;
mod claude_auth;
mod doctor;
mod delivery;
mod mcp;
mod ops;
mod service_setup;
mod service_pair;
mod service_client;
mod client_pair;
mod runtime;
mod setup;
mod node;
mod setup_projects;
mod state;
mod tui;
mod update;

use std::time::Duration;

use alondra_core::now_millis;
use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;

use crate::client::Client;

#[derive(Parser)]
#[command(name = "alondra", about = tr!("The hive from the terminal. With no arguments it opens the TUI.", "La colmena desde la terminal. Sin argumentos abre la TUI."))]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(about = "Inspect or explicitly resolve uncertain deliveries; Local by default")]
    Delivery(delivery::Options),
    #[command(about = "List Macs enrolled in your service account")]
    Machines,
    #[command(about = "Pair or repair this Mac's access to a remote Mac; the TUI stays Local")]
    Connect { mac: String, #[arg(long, help = "Request new approval on the destination; keep current trust until approval succeeds")] repair: bool },
    #[command(about = tr!("Sign in to the Alondra service in your browser", "Inicia sesión en el servicio Alondra desde el navegador"))]
    Login,
    #[command(about = tr!("Show the signed-in Alondra service account", "Muestra la cuenta conectada al servicio Alondra"))]
    Account,
    #[command(about = tr!("Sign out of the Alondra service on this Mac", "Cierra la sesión del servicio Alondra en este Mac"))]
    Logout,
    #[command(about = tr!("Text summary: open questions, fleet and machine", "Resumen en texto: preguntas abiertas, flota y máquina"))]
    Status,
    #[command(about = "Stop the active orchestrator execution; inspect partial effects before retrying")]
    OrchestratorCancel { run_id: String },
    #[command(about = "Continue without replaying the interrupted batch after checking its effects")]
    OrchestratorAcknowledge { run_id: String, #[arg(long, required = true)] confirm: bool },
    #[command(about = "Retry an ambiguous orchestrator run after checking its previous effects")]
    OrchestratorRetry { run_id: String, #[arg(long, required = true, help = "Confirm that replay may repeat actions from this execution")] confirm: bool },
    #[command(about = tr!("Answer a question by its id; the text goes to the option that takes it", "Contesta una pregunta por su id; el texto va a la opción que lo admite"))]
    Answer {
        id: String,
        #[arg(long, help = tr!("Option number or id", "Número o id de la opción"))]
        option: Option<String>,
        text: Vec<String>,
    },
    #[command(about = tr!("Send a message to a Task's node (task id), a node (name, short id or session_id) or the orchestrator", "Manda un mensaje al nodo de una tarea (id de tarea), a un nodo (nombre, id corto o session_id) o al orquestador"))]
    Send { node: String, text: Vec<String> },
    #[command(about = tr!("Cancel tasks by id", "Cancela tareas por su id"))]
    Cancel {
        #[arg(required = true)]
        tasks: Vec<String>,
        #[arg(long, help = tr!("Why, shown with each task", "Por qué, se muestra en cada tarea"))]
        reason: Option<String>,
    },
    #[command(about = tr!("Close a node's session (name, short id or session_id)", "Cierra la sesión de un nodo (nombre, id corto o session_id)"))]
    Close {
        node: String,
        #[arg(long, help = tr!("Close even with unpushed work or open questions", "Cierra aunque tenga trabajo sin subir o preguntas abiertas"))]
        force: bool,
    },
    #[command(about = tr!("Open a node's terminal (← on an empty line, Ctrl+Z or /exit returns)", "Abre el terminal de un nodo (← con la línea vacía, Ctrl+Z o /exit vuelven)"))]
    Attach { node: String },
    #[command(about = tr!(
        "Pair an iPhone or iPad: shows a QR code with the public URL and a one-time code",
        "Vincula un iPhone o iPad: enseña un QR con la URL pública y un código de un solo uso"
    ))]
    Pair {
        #[arg(long, help = tr!(
            "Public URL the device uses to reach the Mac (default: public_url from config.toml)",
            "URL pública por la que el dispositivo llega al Mac (por defecto: public_url de config.toml)"
        ))]
        url: Option<String>,
    },
    #[command(about = tr!("Paired devices; --revoke <id> removes one", "Dispositivos vinculados; con --revoke <id> quita uno"))]
    Devices {
        #[arg(long, help = tr!("Id of the device to unpair", "Id del dispositivo que se desvincula"))]
        revoke: Option<String>,
    },
    #[command(about = tr!(
        "Check the whole installation: daemon, service, plugin, notifications, tmux",
        "Comprueba toda la instalación: servidor, servicio, plugin, notificaciones, tmux"
    ))]
    Doctor {
        #[arg(long, help = tr!("Machine-readable output", "Salida para máquinas"))]
        json: bool,
    },
    #[command(about = tr!(
        "Install the newest release with Homebrew and wait until the daemon runs it",
        "Instala la última versión con Homebrew y espera a que el servidor la ejecute"
    ))]
    Update {
        #[arg(long, help = tr!("Only say whether a newer release exists", "Solo dice si hay una versión más nueva"))]
        check: bool,
    },
    #[command(about = tr!("Audit or safely rebuild the local event-derived state", "Audita o reconstruye de forma segura el estado local derivado de eventos"))]
    State {
        #[command(subcommand)]
        command: StateCmd,
    },
    #[command(about = tr!(
        "Set up or repair Alondra: account login, Mac enrollment, daemon, Claude Code plugin and pairing",
        "Configura o repara Alondra: login, alta del Mac, daemon, plugin de Claude Code y vinculación"
    ))]
    Setup(Box<setup::Args>),
    #[command(about = tr!("Show the effective settings (environment > config.toml > default)", "Enseña los ajustes en vigor (entorno > config.toml > por defecto)"))]
    Config {
        #[arg(long, help = tr!("Print a single value", "Imprime un solo valor"))]
        get: Option<String>,
    },
    /// Node MCP server, started by the node plugin.
    #[command(hide = true)]
    McpNode,
    /// Orchestrator MCP server, started by its separate plugin.
    #[command(hide = true)]
    McpOrchestrator,
    /// What the node mod runs to report a session to alondrad and to hold its connection open.
    #[command(hide = true)]
    Node {
        #[command(subcommand)]
        command: node::Cmd,
    },
    /// Retired: sessions started with an older plugin still call it from their Stop hook
    #[command(hide = true)]
    Inbox,
    /// Local Claude Code hook transport; the daemon remains the sole state writer.
    #[command(hide = true)]
    Hook,
    /// Prints the raw bytes and the parsed key of every key pressed, to debug terminal input
    #[command(hide = true)]
    Keys,
}

#[derive(Subcommand)]
enum StateCmd {
    #[command(about = tr!("Replay and validate the local database", "Reproduce y valida la base de datos local"))]
    Audit,
    #[command(about = tr!("Back up the database and rebuild from a complete event log; stop alondrad first", "Crea una copia y reconstruye desde un historial completo; detén alondrad antes"))]
    Repair,
}

fn main() -> Result<()> {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    match cli.command {
        Some(Cmd::Delivery(options)) => delivery::run(options),
        Some(Cmd::Machines) => service_client::list(),
        Some(Cmd::Connect { mac, repair }) => service_client::connect(&mac, repair),
        None => tui::run(),
        Some(Cmd::Login) => account::login(),
        Some(Cmd::Account) => account::status(),
        Some(Cmd::Logout) => account::logout(),
        Some(Cmd::Status) => status(),
        Some(Cmd::OrchestratorCancel { run_id }) => cli::recover(ops::Recovery::Cancel, &run_id),
        Some(Cmd::OrchestratorAcknowledge { run_id, confirm: _ }) => cli::recover(ops::Recovery::Acknowledge, &run_id),
        Some(Cmd::OrchestratorRetry { run_id, confirm: _ }) => cli::recover(ops::Recovery::Retry, &run_id),
        Some(Cmd::Answer { id, option, text }) => cli::answer_cmd(&id, option.as_deref(), &text.join(" ")),
        Some(Cmd::Send { node, text }) => cli::send(&node, &text.join(" ")),
        Some(Cmd::Cancel { tasks, reason }) => cli::cancel(&tasks, reason.as_deref()),
        Some(Cmd::Close { node, force }) => cli::close(&node, force),
        Some(Cmd::Attach { node }) => cli::attach(&node),
        Some(Cmd::Pair { url: None }) if client::config().home.join("service.toml").exists() => service_pair::run(&client::config().home),
        Some(Cmd::Pair { url }) => match url.or_else(|| setup::pairing_url(client::config())) {
            Some(url) => pair(&url),
            None => anyhow::bail!(tr!(
                "No public URL: pass --url or set public_url with `alondra setup --public-url …`",
                "No hay URL pública: pasa --url o ponla con `alondra setup --public-url …`"
            )),
        },
        Some(Cmd::Devices { revoke }) => devices(revoke),
        Some(Cmd::Doctor { json }) => doctor::run(json),
        Some(Cmd::Update { check }) => update::run(check),
        Some(Cmd::State { command }) => {
            let home = client::config().home.clone();
            match command {
                StateCmd::Audit => {
                    let (seq, tasks) = state::audit(&home)?;
                    println!("State valid through event {seq}; {tasks} tasks.");
                }
                StateCmd::Repair => println!("{}", state::repair_from_events(&home)?),
            }
            Ok(())
        }
        Some(Cmd::Setup(args)) => setup::run(*args),
        Some(Cmd::Config { get }) => show_config(get.as_deref()),
        Some(Cmd::McpNode) => mcp::serve_node(),
        Some(Cmd::McpOrchestrator) => mcp::serve_orchestrator(),
        Some(Cmd::Hook) => hook(),
        Some(Cmd::Node { command }) => node::run(command),
        Some(Cmd::Inbox) => Ok(()),
        Some(Cmd::Keys) => keys_probe(),
    }
}

fn show_config(get: Option<&str>) -> Result<()> {
    let c = alondra_core::config::Config::load().map_err(|e| anyhow::anyhow!(e))?;
    let rows = [
        ("home", c.home.display().to_string()),
        ("name", c.name.clone()),
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
        ("archived_tasks_days", c.archived_tasks_days.to_string()),
        ("auto_update", c.auto_update.to_string()),
    ];
    match get {
        Some(key) => match rows.iter().find(|(k, _)| *k == key) {
            Some((_, v)) => println!("{v}"),
            None => anyhow::bail!(trf!("unknown setting: {key}", "ajuste desconocido: {key}")),
        },
        None => rows.iter().for_each(|(k, v)| println!("{k} = {v}")),
    }
    Ok(())
}

fn hook() -> Result<()> {
    use std::io::{Read, Write};

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let mut payload: serde_json::Value = serde_json::from_slice(&input)?;
    if std::env::var("ALONDRA_LAUNCHED").is_ok_and(|s| !s.is_empty())
        && payload.get("hook_event_name").and_then(serde_json::Value::as_str) == Some("PreToolUse")
        && payload.get("tool_name").and_then(serde_json::Value::as_str) == Some("CronCreate")
        && !short_session_poll(&payload)
    {
        let reason = if std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION").is_ok() {
            "This session is managed by Alondra. Use the alondra schedule_create tool for a durable task visible in Alondra. CronCreate only lives inside this Claude session and expires after seven days."
        } else {
            "This session is managed by Alondra. CronCreate only lives inside this Claude session and expires after seven days. If this work should run on a schedule, ask the orchestrator for a durable Alondra schedule: Alondra's ask tool with recipient kind orchestrator."
        };
        let output = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}});
        println!("{output}");
        return Ok(());
    }
    if payload.get("hook_event_name").and_then(serde_json::Value::as_str) == Some("MessageDisplay")
        && payload.get("final") != Some(&serde_json::Value::Bool(true))
    {
        std::io::stdout().write_all(b"{}\n")?;
        return Ok(());
    }
    compact_hook_payload(&mut payload);
    if let Ok(run) = std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION") {
        payload["_alondra_run"] = json!(run);
    }
    let event = payload.get("hook_event_name").and_then(serde_json::Value::as_str).map(str::to_string);
    let critical = matches!(event.as_deref(), Some("SessionStart" | "PreToolUse" | "PermissionRequest" | "Elicitation"));
    let launched = std::env::var("ALONDRA_LAUNCHED").is_ok_and(|s| !s.is_empty());
    let body = serde_json::to_vec(&json!({"launched": launched, "payload": payload}))?;
    match send_hook(&body) {
        HookReply::Answered => Ok(()),
        HookReply::Refused(reason) if critical => {
            eprintln!("Alondra cannot validate this action: {reason}");
            std::process::exit(2);
        }
        HookReply::Refused(reason) => Err(anyhow::anyhow!(reason)),
        HookReply::Unreachable(error) => {
            eprintln!("Alondra did not answer; continuing without it: {error:#}");
            Ok(())
        }
    }
}

enum HookReply {
    Answered,
    Refused(String),
    Unreachable(anyhow::Error),
}

/// A reply already printed stands even if its acknowledgement is lost: the daemon records
/// that delivery as uncertain. A 5xx, like no answer at all, lets Claude continue; a 4xx is
/// a deliberate refusal.
fn send_hook(body: &[u8]) -> HookReply {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let path = client::config().home.join("hooks.sock");
    let exchange = (|| -> Result<Vec<u8>> {
        let mut stream = UnixStream::connect(&path)
            .map_err(|e| anyhow::anyhow!("cannot send hook to {}: {e}", path.display()))?;
        anyhow::ensure!(body.len() <= u32::MAX as usize, "hook input is too large");
        stream.write_all(&(body.len() as u32).to_be_bytes())?;
        stream.write_all(body)?;
        let response = read_reply(&mut stream)?;
        anyhow::ensure!(response.len() >= 2, "daemon closed the hook connection without a reply");
        if u16::from_be_bytes([response[0], response[1]]) < 400 {
            let mut stdout = std::io::stdout();
            if stdout.write_all(&response[2..]).and_then(|()| stdout.flush()).is_ok() {
                let _ = stream.write_all(&[6]);
            }
        }
        Ok(response)
    })();
    let response = match exchange {
        Ok(response) => response,
        Err(error) => return HookReply::Unreachable(error),
    };
    let detail = String::from_utf8_lossy(&response[2..]);
    match u16::from_be_bytes([response[0], response[1]]) {
        status @ 500.. => HookReply::Unreachable(anyhow::anyhow!("daemon failed ({status}): {detail}")),
        status @ 400..=499 => HookReply::Refused(format!("daemon rejected hook ({status}): {detail}")),
        _ => HookReply::Answered,
    }
}

/// A successful reply is one JSON value: once it is complete the hook acknowledges it without
/// waiting for the daemon to close the connection. A refusal's text runs until the close.
fn read_reply(stream: &mut impl std::io::Read) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(response);
        }
        response.extend_from_slice(&chunk[..read]);
        if response.len() > 2 && u16::from_be_bytes([response[0], response[1]]) < 400
            && serde_json::from_slice::<serde::de::IgnoredAny>(&response[2..]).is_ok()
        {
            return Ok(response);
        }
    }
}

/// Alondra schedules start at one-hour intervals. Keep shorter session polling in Claude.
fn short_session_poll(payload: &serde_json::Value) -> bool {
    let cron = payload.pointer("/tool_input/cron").and_then(serde_json::Value::as_str)
        .or_else(|| payload.pointer("/tool_input/cron_expression").and_then(serde_json::Value::as_str));
    let Some(cron) = cron else { return false; };
    let parts: Vec<_> = cron.split_whitespace().collect();
    if parts.len() != 5 { return false; }
    // Any minute field with more than one value can fire more than once an hour,
    // including lists and ranges on specific weekdays. Alondra cannot express it.
    !parts[0].parse::<u8>().is_ok_and(|minute| minute <= 59)
}

#[cfg(test)]
mod schedule_hook_tests {
    use super::*;

    #[test]
    fn short_polls_stay_in_claude_while_daily_work_uses_alondra() {
        assert!(short_session_poll(&json!({"tool_input": {"cron": "*/5 * * * *"}})));
        assert!(short_session_poll(&json!({"tool_input": {"cron": "* * * * *"}})));
        assert!(short_session_poll(&json!({"tool_input": {"cron": "0,30 9-17 * * 1-5"}})));
        assert!(!short_session_poll(&json!({"tool_input": {"cron": "0 9 * * 1"}})));
        assert!(!short_session_poll(&json!({"tool_input": {"cron": "0 * * * *"}})));
    }
}

/// Claude's batch hook includes complete tool responses; Alondra needs only the fact that a
/// batch finished. Assistant display deltas are also deliberately kept out of the event log.
fn compact_hook_payload(payload: &mut serde_json::Value) {
    let event = payload.get("hook_event_name").and_then(serde_json::Value::as_str).map(str::to_string);
    match event.as_deref() {
        Some("PostToolBatch") => {
            let count = payload.get("tool_calls").and_then(serde_json::Value::as_array).map(Vec::len);
            if let Some(map) = payload.as_object_mut() {
                map.remove("tool_calls");
                if let Some(count) = count {
                    map.insert("tool_calls_count".into(), count.into());
                }
            }
        }
        Some("PostToolUse") => {
            // PR detection reads only the last stdout line. Complete tool output can be
            // megabytes and is never otherwise persisted by the daemon.
            if let Some(map) = payload.as_object_mut() {
                if let Some(response) = map.remove("tool_response") {
                    let stdout = response.get("stdout").and_then(serde_json::Value::as_str)
                        .or_else(|| response.as_str());
                    if let Some(last) = stdout.and_then(|s| s.lines().map(str::trim).rfind(|s| !s.is_empty())) {
                        map.insert("tool_response".into(), json!({"stdout": last}));
                    }
                }
            }
        }
        Some("PostToolUseFailure") => { if let Some(map) = payload.as_object_mut() { map.remove("tool_response"); } }
        Some("MessageDisplay") => { if let Some(map) = payload.as_object_mut() { map.remove("delta"); } }
        Some("InstructionsLoaded") => { if let Some(map) = payload.as_object_mut() { map.remove("content"); } }
        _ => {}
    }
}

fn status() -> Result<()> {
    let client = Client::for_user(Duration::from_secs(10));
    let home = client.views("home", None)?;
    println!("{}", home.status.summary);
    for (key, fallback) in [
        ("home", tr!("Waiting for You", "Te espera")),
        ("tasks", tr!("Tasks", "Tareas")),
        ("fleet", tr!("Fleet", "Flota")),
    ] {
        let view = if key == "home" { home.clone() } else { client.views(key, None)? };
        println!("\n{fallback}:");
        for section in &view.sections {
            if key != "home" {
                if let Some(title) = &section.title { println!("  {title}:"); }
            }
            for item in &section.items {
                match item {
                    alondra_core::views::Item::Card(card) => {
                        let summary = card.body.as_ref().map(|body| body.text.lines().next().unwrap_or_default()).unwrap_or(&card.title);
                        println!("  {:<16} {}", card.eyebrow.as_ref().map_or("item", |label| label.text.as_str()), summary);
                    }
                    alondra_core::views::Item::Row(row) => println!("  {:<24} {}", row.badges.first().map_or("", |badge| badge.text.as_str()), row.title),
                    alondra_core::views::Item::Markdown(markdown) => println!("  {}", markdown.text.lines().next().unwrap_or_default()),
                    _ => {}
                }
            }
        }
    }
    if !home.status.gauges.is_empty() {
        println!("\n{}:", tr!("Machine", "Máquina"));
        for gauge in &home.status.gauges { println!("  {}: {}", gauge.label, gauge.text); }
    }
    Ok(())
}

fn pair(url: &str) -> Result<()> {
    let client = Client::new(Duration::from_secs(10));
    let r = client.post("/pairing", &json!({}))?;
    let code = r.get("code").and_then(|c| c.as_str()).unwrap_or_default();
    let payload = json!({"url": url.trim_end_matches('/'), "code": code}).to_string();
    let qr = qrcode::QrCode::new(payload.as_bytes())?;
    let art = qr.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build();
    println!("{art}");
    println!("{}", tr!("Scan it from Alondra on the iPhone (Settings → Pair with Mac).", "Escanéalo desde Alondra en el iPhone (Ajustes → Vincular con el Mac)."));
    println!("{}", trf!("Or by hand: URL {url}  ·  code {code}  ·  expires in 10 minutes and works once.", "O a mano: URL {url}  ·  código {code}  ·  caduca en 10 minutos y sirve una vez."));
    Ok(())
}

fn devices(revoke: Option<String>) -> Result<()> {
    let client = Client::new(Duration::from_secs(10));
    if let Some(id) = revoke {
        let path = format!("/devices/{}", mcp::urlencode(&id));
        client.delete(&path)?;
        println!("{}", trf!("Device {id} unpaired.", "Dispositivo {id} desvinculado."));
        return Ok(());
    }
    let v: serde_json::Value = client.get("/devices")?;
    let list = v.get("devices").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    if list.is_empty() {
        println!("{}", tr!("No paired devices. Use: alondra pair --url <public URL>", "No hay dispositivos vinculados. Usa: alondra pair --url <URL pública>"));
    }
    for d in list {
        let last = d.get("last_seen").and_then(|x| x.as_i64()).map(|t| {
                let mins = (now_millis() - t) / 60_000;
                trf!("seen {mins} min ago", "visto hace {mins} min")
            })
            .unwrap_or_else(|| tr!("never used", "sin uso").into());
        println!("{}  {}  ({last})", d["id"].as_str().unwrap_or(""), d["name"].as_str().unwrap_or(""));
    }
    Ok(())
}

fn keys_probe() -> anyhow::Result<()> {
    use std::io::Read;
    println!("Press keys; q quits. Shows the raw bytes the terminal sends.\r");
    crossterm::terminal::enable_raw_mode()?;
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 64];
    let result = loop {
        let n = match stdin.read(&mut buf) {
            Ok(n) => n,
            Err(e) => break Err(e.into()),
        };
        let bytes = &buf[..n];
        let shown: String = bytes.iter().map(|&b| if b == 0x1b { "ESC".to_string() } else if b.is_ascii_graphic() { (b as char).to_string() } else { format!("\\x{b:02x}") }).collect();
        print!("{shown}\r\n");
        if bytes == b"q" {
            break Ok(());
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    result
}


#[cfg(test)]
mod mcp_entrypoint_tests {
    use super::*;
    #[test]
    fn node_and_orchestrator_have_fixed_separate_entrypoints() {
        assert!(matches!(Cli::try_parse_from(["alondra", "mcp-node"]).unwrap().command, Some(Cmd::McpNode)));
        assert!(matches!(Cli::try_parse_from(["alondra", "mcp-orchestrator"]).unwrap().command, Some(Cmd::McpOrchestrator)));
        assert!(Cli::try_parse_from(["alondra", "mcp", "--role", "orchestrator"]).is_err());
        assert!(Cli::try_parse_from(["alondra", "mcp-node", "--role", "orchestrator"]).is_err());
    }

    #[test]
    fn cancel_takes_several_tasks_and_close_one_node() {
        let Some(Cmd::Cancel { tasks, reason }) = Cli::try_parse_from(["alondra", "cancel", "t-1", "t-2", "--reason", "done elsewhere"]).unwrap().command
            else { panic!("cancel did not parse") };
        assert_eq!((tasks, reason.as_deref()), (vec!["t-1".to_string(), "t-2".to_string()], Some("done elsewhere")));
        assert!(Cli::try_parse_from(["alondra", "cancel"]).is_err());
        assert!(matches!(Cli::try_parse_from(["alondra", "close", "node", "--force"]).unwrap().command, Some(Cmd::Close { force: true, .. })));
    }
}

#[cfg(test)]
mod destination_tests {
    use super::*;
    #[test]
    fn startup_does_not_accept_remote_destination_flags() {
        assert!(Cli::try_parse_from(["alondra"]).unwrap().command.is_none());
        assert!(Cli::try_parse_from(["alondra", "--mac", "mac-a"]).is_err());
        assert!(Cli::try_parse_from(["alondra", "--remote"]).is_err());
        let connect = Cli::try_parse_from(["alondra", "connect", "mac-a"]).unwrap();
        assert!(matches!(connect.command, Some(Cmd::Connect { mac, repair: false }) if mac == "mac-a"));
    }
}

#[cfg(test)]
mod delivery_cli_tests {
    use super::*;
    #[test]
    fn delivery_confirmation_and_destination_flags_are_scoped() {
        assert!(Cli::try_parse_from(["alondra","delivery","retry","7","--expected-version","2"]).is_err());
        assert!(Cli::try_parse_from(["alondra","delivery","retry","7","--expected-version","2","--terminal-checked"]).is_err());
        let cli=Cli::try_parse_from(["alondra","delivery","retry","7","--mac","mac-b","--expected-version","2","--terminal-checked","--confirm-duplicate-risk"]).unwrap();
        assert!(matches!(cli.command,Some(Cmd::Delivery(delivery::Options{mac:Some(mac),..})) if mac=="mac-b"));
        assert!(Cli::try_parse_from(["alondra","answer","7","--mac","mac-b"]).is_err());
        let cli=Cli::try_parse_from(["alondra","delivery","list"]).unwrap();
        assert!(matches!(cli.command,Some(Cmd::Delivery(delivery::Options{mac:None,..}))));
    }
}
