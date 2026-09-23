#[macro_use]
mod i18n;
mod attach;
mod client;
mod inbox;
mod mcp;
mod tui;

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
    #[command(about = tr!("Text summary: open questions, fleet and machine", "Resumen en texto: preguntas abiertas, flota y máquina"))]
    Status,
    #[command(about = tr!("Answer a question by its number", "Contesta una pregunta por su número"))]
    Answer { id: u64, text: Vec<String> },
    #[command(about = tr!("Send a message to a node (name, short id or session_id)", "Manda un mensaje a un nodo (nombre, id corto o session_id)"))]
    Send { node: String, text: Vec<String> },
    #[command(about = tr!("Open a node's terminal (← on an empty line returns)", "Abre el terminal de un nodo (← con la línea vacía vuelve)"))]
    Attach { node: String },
    #[command(about = tr!(
        "Pair an iPhone or iPad: shows a QR code with the public URL and a one-time code",
        "Vincula un iPhone o iPad: enseña un QR con la URL pública y un código de un solo uso"
    ))]
    Pair {
        #[arg(long, env = "ALONDRA_PUBLIC_URL", help = tr!(
            "Public URL the device uses to reach the Mac (e.g. https://alondra.yourdomain.com)",
            "URL pública por la que el dispositivo llega al Mac (p. ej. https://alondra.tudominio.com)"
        ))]
        url: String,
    },
    #[command(about = tr!("Paired devices; --revoke <id> removes one", "Dispositivos vinculados; con --revoke <id> quita uno"))]
    Devices {
        #[arg(long, help = tr!("Id of the device to unpair", "Id del dispositivo que se desvincula"))]
        revoke: Option<String>,
    },
    #[command(about = tr!("Check that alondrad responds", "Comprueba que alondrad responde"))]
    Doctor,
    /// Servidor MCP para los nodos (lo arranca el plugin)
    #[command(hide = true)]
    Mcp {
        #[arg(long, env = "ALONDRA_ROLE", default_value = "node")]
        role: String,
    },
    /// Hook de Stop: espera mensajes para esta sesión (lo arranca el plugin)
    #[command(hide = true)]
    Inbox,
}

fn main() -> Result<()> {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    match cli.command {
        None => tui::run(),
        Some(Cmd::Status) => status(),
        Some(Cmd::Answer { id, text }) => {
            Client::new(Duration::from_secs(10)).post(&format!("/v1/questions/{id}/answer"), &json!({"text": text.join(" "), "by": "David", "device": "cli"}))?;
            println!("{}", trf!("Answered #{id}", "Contestada #{id}"));
            Ok(())
        }
        Some(Cmd::Send { node, text }) => {
            let r = Client::new(Duration::from_secs(10)).post(&format!("/v1/nodes/{}/messages", mcp::urlencode(&node)), &json!({"text": text.join(" "), "from": "david"}))?;
            let msg = r.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("{}", trf!("Message {msg} is in {node}'s inbox", "Mensaje {msg} en la bandeja de {node}"));
            Ok(())
        }
        Some(Cmd::Attach { node }) => {
            let v: serde_json::Value = Client::new(Duration::from_secs(5)).get(&format!("/v1/nodes/{}", mcp::urlencode(&node)))?;
            let short = v["node"]["short_id"].as_str().unwrap_or(&node).to_string();
            attach::attach(&short)
        }
        Some(Cmd::Pair { url }) => pair(&url),
        Some(Cmd::Devices { revoke }) => devices(revoke),
        Some(Cmd::Doctor) => {
            let v: serde_json::Value = Client::new(Duration::from_secs(5)).get("/v1/health")?;
            println!("{v}");
            Ok(())
        }
        Some(Cmd::Mcp { role }) => mcp::serve(if role == "orchestrator" { mcp::Role::Orchestrator } else { mcp::Role::Node }),
        Some(Cmd::Inbox) => std::process::exit(inbox::run()?),
    }
}

fn status() -> Result<()> {
    let o = Client::new(Duration::from_secs(10)).overview()?;
    let m = &o.machine;
    let mb = m.memory_available / 1_048_576;
    let mem = m.memory_free_percent.map(|p| trf!("{p}% free", "{p}% libre")).unwrap_or_else(|| trf!("{mb} MB available", "{mb} MB disponibles"));
    let (l1, l5, l15) = (m.load[0], m.load[1], m.load[2]);
    let shaky = if m.unreliable { tr!(" (unreliable readings)", " (medidas poco fiables)") } else { "" };
    println!("{}", trf!("Machine: memory {mem} · load {l1:.1} {l5:.1} {l15:.1}{shaky}", "Máquina: memoria {mem} · carga {l1:.1} {l5:.1} {l15:.1}{shaky}"));
    let waiting = o.open_questions.len();
    println!("\n{}", trf!("Waiting for you ({waiting}):", "Te esperan ({waiting}):"));
    for q in &o.open_questions {
        println!("  #{:<4} {}  · {}", q.id, q.title, q.node_name.clone().unwrap_or_default());
    }
    let fleet = o.nodes.len();
    println!("\n{}", trf!("Fleet ({fleet}):", "Flota ({fleet}):"));
    for n in &o.nodes {
        let mins = (now_millis() - n.state_since) / 60_000;
        println!("  {:<24} {:<34} {:>5}m", i18n::state(n.state), n.display_name(), mins);
    }
    Ok(())
}

fn pair(url: &str) -> Result<()> {
    let client = Client::new(Duration::from_secs(10));
    let r = client.post("/v1/pairing", &json!({}))?;
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
        let path = format!("/v1/devices/{}", mcp::urlencode(&id));
        client.delete(&path)?;
        println!("{}", trf!("Device {id} unpaired.", "Dispositivo {id} desvinculado."));
        return Ok(());
    }
    let v: serde_json::Value = client.get("/v1/devices")?;
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
