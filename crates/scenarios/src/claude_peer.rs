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
use alondra_scenarios_transcript::frames::FrameComparator;
use alondra_scenarios_transcript::{Line, Transcript};
use serde::Deserialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Where the events of a host that replaces another continue: after the last the daemon kept.
    #[serde(default)]
    journal_base: u64,
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
    /// The daemon closed its end of the connection, as it does with a host it replaces.
    Closed,
}

fn wire(error: alondra_agent_protocol::Error) -> PeerError {
    match error {
        alondra_agent_protocol::Error::Io(_) => PeerError::Closed,
        other => PeerError::Protocol(other.to_string()),
    }
}

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mismatch { expected, received } => write!(f, "expected {expected:?}, the daemon sent {received}"),
            Self::Protocol(why) => write!(f, "protocol: {why}"),
            Self::Closed => f.write_str("the daemon closed the connection"),
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
    /// What each host started so far still has to play of its transcript, by the order it started in.
    pub remaining: Vec<usize>,
}

impl Seen {
    pub fn unplayed(&self) -> usize {
        self.remaining.iter().sum()
    }
}

/// How many frames the peers have exchanged with the daemon: the runner waits for it to stop
/// moving before it reads what the daemon did with them.
#[derive(Clone, Default)]
pub struct Activity(Arc<AtomicU64>);

impl Activity {
    pub fn count(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn tick(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
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

/// Whether the frame the daemon sent is the one the transcript expected. The prompt of an offer is
/// the mission the daemon wrote around the task, so the transcript names the part of it that
/// matters and the offer has to contain it.
fn is_expected(ids: &mut FrameComparator, expected: &Value, received: &Value) -> bool {
    let prompt = |frame: &Value| frame.pointer("/body/action/prompt").and_then(Value::as_str).map(str::to_owned);
    let (mut expected, mut received) = (expected.clone(), received.clone());
    if let (Some(wanted), Some(sent)) = (prompt(&expected), prompt(&received)) {
        if !sent.contains(&wanted) {
            return false;
        }
        for frame in [&mut expected, &mut received] {
            if let Some(action) = frame.pointer_mut("/body/action").and_then(Value::as_object_mut) {
                action.remove("prompt");
            }
        }
    }
    ids.matches(&expected, &received)
}

struct Session {
    /// The last cursor the daemon acknowledged, and the cursor the host ends after: it ends once
    /// the daemon has committed what the host said before it.
    acknowledged: u64,
    ends_after: Option<u64>,
    app_server: bool,
    capabilities: &'static [&'static str],
    host: usize,
    activity: Activity,
    /// The process group of the process the daemon supervises as the host.
    group: i32,
    bootstrap: Bootstrap,
    secret: [u8; 32],
    cursor: u64,
    last: Option<Command>,
    /// The ids the daemon assigned to the recorded ones, shared by every host of the scenario.
    ids: Arc<Mutex<FrameComparator>>,
}

impl Session {
    /// The host's process, or only its app-server, ends. Whether the connection goes on.
    fn end(&mut self) -> bool {
        self.activity.tick();
        if self.app_server {
            // Only the app-server dies: its host lives on, and cannot say so.
            let _ = Process::new("/usr/bin/pkill").args(["-KILL", "-P", &self.group.to_string()]).status();
            true
        } else {
            unsafe { libc::killpg(self.group, libc::SIGKILL) };
            false
        }
    }

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
                if let Some(observation) = body.get_mut("observation") {
                    self.ids.lock().unwrap_or_else(std::sync::PoisonError::into_inner).assign(observation);
                }
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
            capabilities: session.capabilities.iter().map(|c| (*c).to_owned()).collect(),
            fence: b.fence.clone(),
            host_release: b.manifest.host_release.clone(),
            host_sha256: b.manifest.host_sha256.clone(),
            sdk_version: b.manifest.sdk_version.clone(),
        })))
        .map_err(wire)?;
    writer
        .write(&Frame::new(Body::Challenge(Challenge {
            fence: b.fence.clone(),
            execution: b.execution.clone(),
            host_instance_id: b.host_instance_id.clone(),
            nonce: "a".repeat(64),
            executable_sha256: b.manifest.executable_sha256.clone(),
        })))
        .map_err(wire)?;
    let attach: Attach = match reader.read().map_err(wire)?.map(|frame| frame.body) {
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
        .map_err(wire)
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
                writer.write(&frame).map_err(wire)?;
                session.activity.tick();
            }
            _ if session.acknowledged < session.cursor => {
                session.ends_after = Some(session.cursor);
                return Ok(true);
            }
            _ => return Ok(session.end()),
        }
    }
    Ok(true)
}

fn note(seen: &Mutex<Seen>, host: usize, remaining: usize) {
    seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remaining[host] = remaining;
}

fn serve(stream: UnixStream, queue: &mut VecDeque<Step>, session: &mut Session, seen: &Mutex<Seen>) -> Result<(), PeerError> {
    let protocol = |why: String| PeerError::Protocol(why);
    let mut writer = FramedWriter::new(stream.try_clone().map_err(|e| protocol(e.to_string()))?);
    let mut reader = FramedReader::new(stream);
    handshake(&mut reader, &mut writer, session)?;
    let alive = emit(queue, session, &mut writer)?;
    note(seen, session.host, queue.len());
    if !alive {
        return Ok(());
    }
    while let Some((frame, _)) = reader.read_with_digest().map_err(wire)? {
        session.activity.tick();
        match frame.body {
            Body::Acknowledge(acknowledge) => {
                seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).acknowledged.push(acknowledge.cursor);
                session.acknowledged = session.acknowledged.max(acknowledge.cursor);
                if session.ends_after.is_some_and(|cursor| session.acknowledged >= cursor) {
                    session.ends_after = None;
                    if !session.end() {
                        return Ok(());
                    }
                }
            }
            Body::Command(command) => {
                let received = serde_json::to_value(Frame::new(Body::Command(command.clone()))).map_err(|e| protocol(e.to_string()))?;
                seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).commands.push(received.clone());
                let expected = match queue.front() {
                    Some(Step::Expect(expected)) => Some(expected.clone()),
                    _ => None,
                };
                let matched = expected.as_ref().is_some_and(|expected| is_expected(&mut session.ids.lock().unwrap_or_else(std::sync::PoisonError::into_inner), expected, &received));
                if !matched {
                    let mismatch = PeerError::Mismatch { expected, received };
                    seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).failure.get_or_insert(mismatch.clone());
                    return Err(mismatch);
                }
                queue.pop_front();
                session.last = Some(command);
                let alive = emit(queue, session, &mut writer)?;
                note(seen, session.host, queue.len());
                if !alive {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Starts a stand-in for the provider's host wherever the daemon would start one: a host that
/// speaks the protocol on the host's socket as the transcript says, in a thread of this process.
/// Each host the daemon starts plays the next transcript: a host that ended and the one that
/// replaces it are two.
pub struct PeerLauncher {
    app_server: bool,
    capabilities: &'static [&'static str],
    transcripts: Mutex<VecDeque<Transcript>>,
    ids: Arc<Mutex<FrameComparator>>,
    pub seen: Arc<Mutex<Seen>>,
    pub activity: Activity,
    pub hosts: Hosts,
}

/// The processes of every host a peer started, to end when the run that started them ends.
#[derive(Clone, Default)]
pub struct Hosts(Arc<Mutex<Vec<alondra_process_group::Member>>>);

impl Hosts {
    pub fn members(&self) -> Vec<alondra_process_group::Member> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn started(&self, group: i32) -> std::io::Result<()> {
        let members = alondra_process_group::members(group)?;
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend(members);
        Ok(())
    }

    /// Ends every process the hosts started that still runs; a PID another process took by now
    /// is left alone.
    pub fn end(&self) -> std::io::Result<()> {
        alondra_process_group::end_tree(&self.members(), std::time::Duration::from_secs(5))
    }
}

impl PeerLauncher {
    pub fn new(transcript: Transcript) -> Self {
        Self::sequence(vec![transcript])
    }

    pub fn sequence(transcripts: Vec<Transcript>) -> Self {
        Self::declaring(CAPABILITIES, transcripts)
    }

    /// Each host's process has one direct child, standing where the Codex app-server stands: the
    /// daemon takes the host for lost when that child is gone.
    pub fn with_app_server(mut self) -> Self {
        self.app_server = true;
        self
    }

    /// A peer whose hosts announce `capabilities` in their hello: what the provider's real host announces.
    pub fn declaring(capabilities: &'static [&'static str], transcripts: Vec<Transcript>) -> Self {
        Self { app_server: false, capabilities, transcripts: Mutex::new(transcripts.into()), ids: Arc::default(), seen: Arc::default(), activity: Activity::default(), hosts: Hosts::default() }
    }
}

impl HostLauncher for PeerLauncher {
    fn spawn(&self, mut spawn: HostSpawn<'_>) -> std::io::Result<Child> {
        let invalid = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_owned());
        let mut text = String::new();
        spawn.stdin.read_to_string(&mut text)?;
        let bootstrap: Bootstrap = serde_json::from_str(&text).map_err(|e| invalid(&e.to_string()))?;
        let secret = decode_secret(&bootstrap.control_secret).ok_or_else(|| invalid("the control secret is not 32 bytes of hex"))?;
        let transcript = self
            .transcripts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .ok_or_else(|| invalid("the daemon started a host the scenario has no transcript for"))?;
        let listener = UnixListener::bind(&bootstrap.socket_path)?;
        std::fs::set_permissions(&bootstrap.socket_path, std::fs::Permissions::from_mode(0o600))?;
        // The process the daemon supervises: it does nothing but exist until it is stopped.
        let mut process = if self.app_server {
            let mut shell = Process::new("/bin/sh");
            shell.args(["-c", "/bin/sleep 86400 & echo started; wait"]);
            shell
        } else {
            let mut sleep = Process::new("/bin/sleep");
            sleep.arg("86400");
            sleep
        };
        let mut child = process.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).spawn()?;
        if self.app_server {
            // The host is not up until its app-server is: the daemon looks for that child at once.
            let mut stdout = std::io::BufReader::new(child.stdout.take().ok_or_else(|| invalid("the host has no output"))?);
            std::io::BufRead::read_line(&mut stdout, &mut String::new())?;
            child.stdout = Some(stdout.into_inner());
        }
        if let Err(error) = self.hosts.started(child.id() as i32) {
            unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) };
            child.wait()?;
            return Err(error);
        }
        let seen = self.seen.clone();
        let mut queue = steps(&transcript);
        let host = {
            let mut seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            seen.remaining.push(queue.len());
            seen.remaining.len() - 1
        };
        let mut session = Session { acknowledged: bootstrap.journal_base, ends_after: None, app_server: self.app_server, capabilities: self.capabilities, host, activity: self.activity.clone(), group: child.id() as i32, cursor: bootstrap.journal_base, bootstrap, secret, last: None, ids: self.ids.clone() };
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                match serve(stream, &mut queue, &mut session, &seen) {
                    Err(PeerError::Closed) | Ok(()) => {}
                    Err(error) => {
                        seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).failure.get_or_insert(error);
                        break;
                    }
                }
            }
        });
        Ok(child)
    }
}

#[cfg(test)]
#[path = "claude_peer_tests.rs"]
mod tests;
