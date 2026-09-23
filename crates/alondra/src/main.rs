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
#[command(name = "alondra", about = "La colmena desde la terminal. Sin argumentos abre la TUI.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Resumen en texto: preguntas abiertas, flota y máquina
    Status,
    /// Contesta una pregunta por su número
    Answer { id: u64, text: Vec<String> },
    /// Manda un mensaje a un nodo (nombre, id corto o session_id)
    Send { node: String, text: Vec<String> },
    /// Abre el terminal de un nodo (← con la línea vacía vuelve)
    Attach { node: String },
    /// Comprueba que alondrad responde
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
            println!("Contestada #{id}");
            Ok(())
        }
        Some(Cmd::Send { node, text }) => {
            let r = Client::new(Duration::from_secs(10)).post(&format!("/v1/nodes/{}/messages", mcp::urlencode(&node)), &json!({"text": text.join(" "), "from": "david"}))?;
            println!("Mensaje {} en la bandeja de {node}", r.get("id").and_then(|v| v.as_u64()).unwrap_or(0));
            Ok(())
        }
        Some(Cmd::Attach { node }) => {
            let v: serde_json::Value = Client::new(Duration::from_secs(5)).get(&format!("/v1/nodes/{}", mcp::urlencode(&node)))?;
            let short = v["node"]["short_id"].as_str().unwrap_or(&node).to_string();
            attach::attach(&short)
        }
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
    println!(
        "Máquina: memoria {} · carga {:.1} {:.1} {:.1}{}",
        m.memory_free_percent.map(|p| format!("{p}% libre")).unwrap_or_else(|| format!("{} MB disponibles", m.memory_available / 1_048_576)),
        m.load[0],
        m.load[1],
        m.load[2],
        if m.unreliable { " (medidas poco fiables)" } else { "" }
    );
    println!("\nTe esperan ({}):", o.open_questions.len());
    for q in &o.open_questions {
        println!("  #{:<4} {}  · {}", q.id, q.title, q.node_name.clone().unwrap_or_default());
    }
    println!("\nFlota ({}):", o.nodes.len());
    for n in &o.nodes {
        let mins = (now_millis() - n.state_since) / 60_000;
        println!("  {:<20} {:<34} {:>5}m", n.state.label(), n.display_name(), mins);
    }
    Ok(())
}
