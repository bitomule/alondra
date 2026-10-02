//! The two commands the node mod runs: `event` hands alondrad one report of its session, and
//! `connect` holds the session's connection to alondrad open for as long as both live. The open
//! connection is what tells alondrad the node is reachable; this process sleeps in `read` and
//! copies whatever alondrad writes down it to its standard output.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};

const MAX_REPORT_BYTES: u64 = 1024 * 1024;
const CLOSED_BY_DAEMON: i32 = 3;

#[derive(Subcommand)]
pub enum Cmd {
    /// Hands alondrad the report on standard input
    Event,
    /// Opens the session's connection to alondrad and holds it until either side ends
    Connect,
}

pub fn run(command: Cmd) -> Result<()> {
    let input = stdin_json()?;
    let socket = crate::client::config().home.join("hooks.sock");
    match command {
        Cmd::Event => event(&socket, input),
        Cmd::Connect => {
            let mut stream = open(&socket, json!({"node_connect": input}))?;
            println!("connected");
            std::io::stdout().flush()?;
            std::io::copy(&mut stream, &mut std::io::stdout())?;
            std::process::exit(CLOSED_BY_DAEMON);
        }
    }
}

fn stdin_json() -> Result<Value> {
    let mut text = String::new();
    std::io::stdin().take(MAX_REPORT_BYTES).read_to_string(&mut text)?;
    let value: Value = serde_json::from_str(&text).context("the report is not JSON")?;
    if !value.is_object() {
        bail!("the report is not a JSON object");
    }
    Ok(value)
}

fn event(socket: &Path, report: Value) -> Result<()> {
    let mut stream = open(socket, json!({"node_event": report}))?;
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest)?;
    Ok(())
}

/// Sends one frame and reads the daemon's status; what follows the status is left on the stream.
fn open(socket: &Path, frame: Value) -> Result<UnixStream> {
    let mut stream = UnixStream::connect(socket).with_context(|| format!("cannot reach alondrad at {}", socket.display()))?;
    let body = serde_json::to_vec(&frame)?;
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(&body)?;
    let mut status = [0u8; 2];
    stream.read_exact(&mut status).context("alondrad closed the connection without an answer")?;
    let status = u16::from_be_bytes(status);
    if status >= 400 {
        let mut reason = String::new();
        let _ = stream.read_to_string(&mut reason);
        bail!("alondrad refused the report ({status}): {reason}");
    }
    Ok(stream)
}
