//! The Claude host of a scenario: it stands where the SDK host stands, on the host's socket, and
//! speaks the daemon's wire protocol with the real codec and the real proofs. Nothing of the
//! daemon's side of the protocol is replaced: the normalizer, the correlation of commands and the
//! adoption of hosts read what this peer writes as they read the real host's.
//!
//! What the peer says comes from a transcript: after each frame the daemon is expected to send,
//! the frames the host answers with. A frame the daemon sends that the transcript did not expect
//! is a mismatch, reported with both frames.

use alondra_agent_protocol::{
    Attach, Attached, Body, Challenge, Command, Execution, Fence, Frame, FramedReader, FramedWriter, Hello, PROTOCOL_VERSION,
    command_digest, host_proof, verify_owner_proof,
};
use alondra_agent_runtime::owned_host::{HostLauncher, HostSpawn};
use alondra_scenarios_transcript::{Line, Transcript};
use serde::Deserialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command as Process, Stdio};
use std::sync::{Arc, Mutex};

/// The capabilities the peer announces: every action the protocol carries.
const CAPABILITIES: &[&str] = &[
    "streaming_input", "exact_decisions", "interrupt", "force_stop", "set_model", "set_permission_mode", "mcp_status",
    "mcp_reconnect", "mcp_sign_in", "mcp_clear_auth", "plan_supersession",
];

/// What the daemon wrote into the host's bootstrap file, as far as the peer needs it.
#[derive(Deserialize)]
struct Bootstrap {
    socket_path: PathBuf,
    host_instance_id: String,
    control_secret: String,
    fence: Fence,
    execution: Execution,
    manifest: Manifest,
}

#[derive(Deserialize)]
struct Manifest {
    host_release: String,
    host_sha256: String,
    executable_sha256: String,
    sdk_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PeerError {
    /// The daemon sent a frame the transcript did not expect next.
    Mismatch { expected: Option<Value>, received: Value },
    Protocol(String),
}

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mismatch { expected, received } => write!(f, "expected {expected:?}, the daemon sent {received}"),
            Self::Protocol(why) => write!(f, "protocol: {why}"),
        }
    }
}

/// What a peer saw and answered, shared with the runner.
#[derive(Default)]
pub struct Seen {
    /// The commands the daemon sent, as JSON.
    pub commands: Vec<Value>,
    pub acknowledged: Vec<u64>,
    /// The first thing that went wrong, if anything did.
    pub failure: Option<PeerError>,
}

enum Step {
    Expect(Value),
    Emit(Value),
    Exit,
}

fn steps(transcript: &Transcript) -> VecDeque<Step> {
    transcript
        .lines
        .iter()
        .filter_map(|line| match line {
            Line::ExpectOutbound { frame } => Some(Step::Expect(frame.clone())),
            Line::EmitInbound { frame } => Some(Step::Emit(frame.clone())),
            Line::RuntimeExit { .. } => Some(Step::Exit),
            Line::TranscriptStart(_) => None,
        })
        .collect()
}

/// A frame of the daemon as it is compared: without what the daemon assigns on each run.
fn comparable(mut frame: Value) -> Value {
    if let Some(body) = frame.get_mut("body").and_then(Value::as_object_mut) {
        for key in ["command_id", "deadline_ms", "fence", "execution"] {
            body.remove(key);
        }
        if let Some(action) = body.get_mut("action").and_then(Value::as_object_mut) {
            action.remove("cwd");
        }
    }
    frame
}

struct Session {
    bootstrap: Bootstrap,
    secret: [u8; 32],
    cursor: u64,
    last: Option<Command>,
}

impl Session {
    /// The frame of the transcript with what the host knows filled in: its fence and execution, the
    /// command a receipt answers and the next cursor of an event.
    fn fill(&mut self, frame: &Value) -> Result<Frame, PeerError> {
        let protocol = |why: String| PeerError::Protocol(why);
        let mut frame = frame.clone();
        let body = frame.get_mut("body").and_then(Value::as_object_mut).ok_or_else(|| protocol("an inbound frame has no body".into()))?;
        body.insert("fence".into(), serde_json::to_value(&self.bootstrap.fence).map_err(|e| protocol(e.to_string()))?);
        body.insert("execution".into(), serde_json::to_value(&self.bootstrap.execution).map_err(|e| protocol(e.to_string()))?);
        match body.get("type").and_then(Value::as_str) {
            Some("receipt") => {
                let command = self.last.as_ref().ok_or_else(|| protocol("a receipt before any command".into()))?;
                body.insert("command_id".into(), Value::String(command.command_id.to_string()));
                body.insert("command_sha256".into(), Value::String(command_digest(command).map_err(|e| protocol(e.to_string()))?));
            }
            Some("event") => {
                self.cursor += 1;
                body.insert("cursor".into(), self.cursor.into());
            }
            _ => {}
        }
        serde_json::from_value(frame).map_err(|e| protocol(format!("an inbound frame of the transcript is not a frame: {e}")))
    }
}

fn handshake(
    reader: &mut FramedReader<UnixStream>,
    writer: &mut FramedWriter<UnixStream>,
    session: &Session,
) -> Result<(), PeerError> {
    let protocol = |why: String| PeerError::Protocol(why);
    let b = &session.bootstrap;
    writer
        .write(&Frame::new(Body::Hello(Hello {
            protocol_versions: vec![PROTOCOL_VERSION],
            capabilities: CAPABILITIES.iter().map(|c| (*c).to_owned()).collect(),
            fence: b.fence.clone(),
            host_release: b.manifest.host_release.clone(),
            host_sha256: b.manifest.host_sha256.clone(),
            sdk_version: b.manifest.sdk_version.clone(),
        })))
        .map_err(|e| protocol(e.to_string()))?;
    writer
        .write(&Frame::new(Body::Challenge(Challenge {
            fence: b.fence.clone(),
            execution: b.execution.clone(),
            host_instance_id: b.host_instance_id.clone(),
            nonce: "a".repeat(64),
            executable_sha256: b.manifest.executable_sha256.clone(),
        })))
        .map_err(|e| protocol(e.to_string()))?;
    let attach: Attach = match reader.read().map_err(|e| protocol(e.to_string()))?.map(|frame| frame.body) {
        Some(Body::Attach(attach)) => attach,
        other => return Err(protocol(format!("expected the daemon's attach, found {other:?}"))),
    };
    verify_owner_proof(&session.secret, &attach).map_err(|e| protocol(format!("the daemon's proof: {e}")))?;
    let proof = host_proof(&session.secret, &attach).map_err(|e| protocol(e.to_string()))?;
    writer
        .write(&Frame::new(Body::Attached(Attached {
            fence: attach.fence,
            execution: attach.execution,
            host_instance_id: attach.host_instance_id,
            owner_generation: attach.owner_generation,
            proof,
        })))
        .map_err(|e| protocol(e.to_string()))
}

fn decode_secret(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut secret = [0u8; 32];
    for (index, byte) in secret.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(secret)
}

/// Sends the frames the transcript has next, until it expects something from the daemon again.
/// `false` when the host ends: the connection closes.
fn emit(queue: &mut VecDeque<Step>, session: &mut Session, writer: &mut FramedWriter<UnixStream>) -> Result<bool, PeerError> {
    while let Some(Step::Emit(_) | Step::Exit) = queue.front() {
        match queue.pop_front() {
            Some(Step::Emit(frame)) => {
                let frame = session.fill(&frame)?;
                writer.write(&frame).map_err(|e| PeerError::Protocol(e.to_string()))?;
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn serve(stream: UnixStream, queue: &mut VecDeque<Step>, session: &mut Session, seen: &Mutex<Seen>) -> Result<(), PeerError> {
    let protocol = |why: String| PeerError::Protocol(why);
    let mut writer = FramedWriter::new(stream.try_clone().map_err(|e| protocol(e.to_string()))?);
    let mut reader = FramedReader::new(stream);
    handshake(&mut reader, &mut writer, session)?;
    if !emit(queue, session, &mut writer)? {
        return Ok(());
    }
    while let Some((frame, _)) = reader.read_with_digest().map_err(|e| protocol(e.to_string()))? {
        match frame.body {
            Body::Acknowledge(acknowledge) => seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).acknowledged.push(acknowledge.cursor),
            Body::Command(command) => {
                let received = serde_json::to_value(Frame::new(Body::Command(command.clone()))).map_err(|e| protocol(e.to_string()))?;
                seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).commands.push(received.clone());
                let expected = match queue.front() {
                    Some(Step::Expect(expected)) => Some(expected.clone()),
                    _ => None,
                };
                if expected.as_ref().map(|expected| comparable(expected.clone())) != Some(comparable(received.clone())) {
                    let mismatch = PeerError::Mismatch { expected, received };
                    seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).failure.get_or_insert(mismatch.clone());
                    return Err(mismatch);
                }
                queue.pop_front();
                session.last = Some(command);
                if !emit(queue, session, &mut writer)? {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Starts a stand-in for the SDK host wherever the daemon would start one: a host that speaks the
/// protocol on the host's socket as the transcript says, in a thread of this process.
pub struct PeerLauncher {
    transcript: Mutex<Option<Transcript>>,
    pub seen: Arc<Mutex<Seen>>,
}

impl PeerLauncher {
    pub fn new(transcript: Transcript) -> Self {
        Self { transcript: Mutex::new(Some(transcript)), seen: Arc::default() }
    }
}

impl HostLauncher for PeerLauncher {
    fn spawn(&self, mut spawn: HostSpawn<'_>) -> std::io::Result<Child> {
        let invalid = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_owned());
        let mut text = String::new();
        spawn.stdin.read_to_string(&mut text)?;
        let bootstrap: Bootstrap = serde_json::from_str(&text).map_err(|e| invalid(&e.to_string()))?;
        let secret = decode_secret(&bootstrap.control_secret).ok_or_else(|| invalid("the control secret is not 32 bytes of hex"))?;
        let transcript = self.transcript.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take().ok_or_else(|| invalid("this peer already ran"))?;
        let listener = UnixListener::bind(&bootstrap.socket_path)?;
        std::fs::set_permissions(&bootstrap.socket_path, std::fs::Permissions::from_mode(0o600))?;
        let seen = self.seen.clone();
        let mut queue = steps(&transcript);
        let mut session = Session { bootstrap, secret, cursor: 0, last: None };
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                if let Err(error) = serve(stream, &mut queue, &mut session, &seen) {
                    seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).failure.get_or_insert(error);
                    return;
                }
            }
        });
        // The process the daemon supervises: it does nothing but exist until it is stopped.
        Process::new("/bin/sleep").arg("86400").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).spawn()
    }
}

#[cfg(test)]
#[path = "claude_peer_tests.rs"]
mod tests;
