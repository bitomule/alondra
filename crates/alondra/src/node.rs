//! The commands the node mod runs: `event` hands alondrad one report of its session, `connect`
//! holds the session's connection to alondrad open for as long as both live, `ack` tells alondrad
//! which of the messages it wrote down that connection the session has taken, and `hold` puts one
//! card to the user and prints the answer. The open connection is what tells alondrad the node is
//! reachable; `connect` sleeps in `read` and copies whatever alondrad writes down it, one JSON line
//! per message or interrupt, to its
//! standard output.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};

const MAX_REPORT_BYTES: u64 = 1024 * 1024;
const CLOSED_BY_DAEMON: i32 = 3;
const REDIAL: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

#[derive(Subcommand)]
pub enum Cmd {
    /// Hands alondrad the report on standard input
    Event,
    /// Opens the session's connection to alondrad and holds it until either side ends
    Connect,
    /// Tells alondrad the session took these message ids; the session id is on standard input
    Ack { ids: Vec<String> },
    /// Puts the card on standard input to the user and prints the answer, waiting across daemon restarts
    Hold,
}

pub fn run(command: Cmd) -> Result<()> {
    let input = stdin_json()?;
    let socket = crate::client::config().home.join("hooks.sock");
    match command {
        Cmd::Event => event(&socket, input),
        Cmd::Ack { ids } => {
            let mut frame = input;
            frame["ids"] = json!(ids);
            event_frame(&socket, json!({"node_ack": frame}))
        }
        Cmd::Hold => hold(&socket, input),
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
    event_frame(socket, json!({"node_event": report}))
}

fn event_frame(socket: &Path, frame: Value) -> Result<()> {
    let mut stream = open(socket, frame)?;
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest)?;
    Ok(())
}

/// A daemon that is down or restarting is dialled again every second and sent the same card,
/// which it recognises by its id: a card posted before the restart is still waiting when it is back.
fn hold(socket: &Path, card: Value) -> Result<()> {
    loop {
        match open(socket, json!({"node_hold": card})) {
            Ok(mut stream) => {
                let mut answer = String::new();
                if stream.read_to_string(&mut answer).is_ok() && !answer.is_empty() {
                    println!("{answer}");
                    return Ok(());
                }
            }
            Err(error) if error.is::<Refused>() => return Err(error),
            Err(_) => {}
        }
        std::thread::sleep(REDIAL);
    }
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
        return Err(Refused(format!("alondrad refused the report ({status}): {reason}")).into());
    }
    Ok(stream)
}
