//! The daemon↔host transport every host model speaks, as the persistent host speaks it: the hello
//! and challenge, the daemon's proof and the host's, the journal of events the daemon has not
//! acknowledged, and one receipt per command, sent again for a copy of it. The model that compiles
//! this names its protocol crate `crate::protocol`; what a host does with each command is the
//! model's (`Model::command`).

use crate::protocol::{
    Attach, Attached, Body, Challenge, Command, Execution, Fence, Frame, FramedReader, FramedWriter, Hello, PROTOCOL_VERSION, command_digest, host_proof,
    verify_owner_proof,
};
use alondra_engine_doubles::agent_host::Served;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::ops::{Deref, DerefMut};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// The exit of a process SIGKILL ended.
const KILLED: i32 = 9;

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The process a host runs as: whether it still runs, and how it ends.
pub trait Process: Send + Sync {
    fn alive(&self) -> bool;
    /// The process ends: by itself once its work is closed, or killed with whatever it started.
    fn end(&self, closed: bool);
}

/// What the daemon launched a host with.
#[derive(Deserialize)]
pub struct Bootstrap {
    pub host_instance_id: String,
    control_secret: String,
    pub fence: Fence,
    pub execution: Execution,
    pub manifest: Manifest,
    pub native: Native,
    #[serde(default)]
    pub state_dir: PathBuf,
    #[serde(default)]
    pub worker: Option<Worker>,
    #[serde(default)]
    pub journal_base: Option<u64>,
    #[serde(default)]
    pub opens_on_control: bool,
}

#[derive(Deserialize)]
pub struct Manifest {
    pub host_release: String,
    pub host_sha256: String,
    pub executable_sha256: String,
    pub sdk_version: String,
}

#[derive(Deserialize)]
pub struct Native {
    pub session_id: String,
    #[serde(default)]
    pub executable: PathBuf,
    #[serde(default)]
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub resume: Option<Value>,
}

#[derive(Default, Deserialize)]
pub struct Worker {
    #[serde(default)]
    pub plugins: Vec<PathBuf>,
    #[serde(default)]
    pub settings: serde_json::Map<String, Value>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
}

impl Bootstrap {
    /// The bootstrap a host was launched with, and the secret its proofs are made with.
    pub fn read(bytes: &[u8]) -> io::Result<(Self, [u8; 32])> {
        let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
        let bootstrap: Self = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
        let secret = decode_secret(&bootstrap.control_secret).ok_or_else(|| invalid("the control secret is not 32 bytes of hex".into()))?;
        Ok((bootstrap, secret))
    }

    /// The task this host runs; none for an orchestrator batch.
    pub fn task(&self) -> Option<String> {
        match &self.execution {
            Execution::Task(execution) => Some(execution.task_id.to_string()),
            Execution::Orchestrator(_) => None,
        }
    }

    /// Whether the native session continues one an earlier host ran.
    pub fn resumes(&self) -> bool {
        self.native.resume.as_ref().is_some_and(|resume| !resume.is_null() && *resume != Value::Bool(false))
    }
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

/// A host's side of the wire: the lane to the attached daemon, every event it has not acknowledged
/// and the receipt of every command.
#[derive(Default)]
pub struct Wire {
    lane: Option<FramedWriter<UnixStream>>,
    /// The host's end of every connection, closed when the host ends.
    connections: Vec<UnixStream>,
    journal: VecDeque<(u64, Frame)>,
    pub cursor: u64,
    pub acknowledged: u64,
    receipts: BTreeMap<String, Frame>,
    /// The host closed: it ends once the daemon acknowledged its last event.
    pub closing: bool,
    pub ended: bool,
}

impl Wire {
    fn at(base: u64) -> Self {
        Self { cursor: base, acknowledged: base, ..Self::default() }
    }

    fn send(&mut self, frame: &Frame) {
        if let Some(lane) = self.lane.as_mut()
            && lane.write(frame).is_err()
        {
            self.lane = None;
        }
    }

    /// Whether a daemon is attached and has acknowledged every event.
    pub fn committed(&self) -> bool {
        self.lane.is_some() && self.acknowledged >= self.cursor
    }

    /// Journals an observation under the next cursor and sends it on the lane, if one is open.
    pub fn event(&mut self, bootstrap: &Bootstrap, observation: Value) -> Result<(), String> {
        self.cursor += 1;
        let frame = json!({"protocol_version": PROTOCOL_VERSION, "body": {"type": "event", "fence": bootstrap.fence, "execution": bootstrap.execution,
            "cursor": self.cursor, "observation": observation}});
        let frame: Frame = serde_json::from_value(frame).map_err(|error| format!("the host wrote an event that is not a frame: {error}"))?;
        self.journal.push_back((self.cursor, frame.clone()));
        self.send(&frame);
        Ok(())
    }

    /// Sends again the receipt of a command the host answered already: a copy gets the same one.
    pub fn answered_before(&mut self, command: &Command) -> bool {
        let Some(receipt) = self.receipts.get(&command.command_id.to_string()).cloned() else { return false };
        self.send(&receipt);
        true
    }

    /// Keeps and sends the receipt that closes `command` with `outcome`.
    pub fn receipt(&mut self, bootstrap: &Bootstrap, command: &Command, outcome: Value) -> Result<(), String> {
        let digest = command_digest(command).map_err(|error| format!("a command the daemon sent has no digest: {error}"))?;
        let id = command.command_id.to_string();
        let receipt = json!({"protocol_version": PROTOCOL_VERSION, "body": {"type": "receipt", "fence": bootstrap.fence, "execution": bootstrap.execution,
            "command_id": id, "command_sha256": digest, "outcome": outcome}});
        let receipt: Frame = serde_json::from_value(receipt).map_err(|error| format!("the host wrote a receipt that is not a frame: {error}"))?;
        self.receipts.insert(id, receipt.clone());
        self.send(&receipt);
        Ok(())
    }
}

/// What a host holds under its lock: its wire, and what the model plays in it.
pub struct HostState<P> {
    pub wire: Wire,
    pub play: P,
}

impl<P> Deref for HostState<P> {
    type Target = P;
    fn deref(&self) -> &P {
        &self.play
    }
}

impl<P> DerefMut for HostState<P> {
    fn deref_mut(&mut self) -> &mut P {
        &mut self.play
    }
}

/// Who a host is, as the model reads it from what the host was launched with.
pub struct Identity {
    pub task: String,
    pub account: String,
    pub session: String,
    pub capability: String,
    pub resumes: bool,
    pub process_id: u32,
}

/// One host the daemon started: its process, its identity, and what it holds.
pub struct Host<P> {
    pub task: String,
    pub account: String,
    pub session: String,
    /// The node's capability, which the daemon gave its native agent in the environment.
    pub capability: String,
    /// The session this host resumes, when it continues one.
    pub resumes: bool,
    /// The number the host's process goes by.
    pub process_id: u32,
    /// No daemon attached to this host yet, counted in the model's hosts starting.
    unattached: AtomicBool,
    starting: Arc<AtomicU64>,
    process: Box<dyn Process>,
    pub bootstrap: Bootstrap,
    secret: [u8; 32],
    capabilities: &'static [&'static str],
    pub state: Mutex<HostState<P>>,
}

impl<P> Host<P> {
    /// A host that announces `capabilities`, counted in `starting` until a daemon attaches.
    pub fn new(identity: Identity, (bootstrap, secret): (Bootstrap, [u8; 32]), capabilities: &'static [&'static str], process: Box<dyn Process>, starting: Arc<AtomicU64>, play: P) -> Self {
        starting.fetch_add(1, Ordering::SeqCst);
        let Identity { task, account, session, capability, resumes, process_id } = identity;
        let wire = Wire::at(bootstrap.journal_base.unwrap_or_default());
        Self {
            task,
            account,
            session,
            capability,
            resumes,
            process_id,
            unattached: AtomicBool::new(true),
            starting,
            process,
            bootstrap,
            secret,
            capabilities,
            state: Mutex::new(HostState { wire, play }),
        }
    }

    /// The first daemon attached to this host, or it ended before one did.
    fn attached(&self) {
        if self.unattached.swap(false, Ordering::SeqCst) {
            self.starting.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub fn alive(&self) -> bool {
        !lock(&self.state).wire.ended && self.process.alive()
    }

    /// Whether the host is past what a daemon must still read: ended, or attached with nothing
    /// unacknowledged.
    pub fn settled(&self) -> bool {
        let state = lock(&self.state);
        state.wire.ended || !self.process.alive() || state.wire.committed()
    }

    /// The host's process ends, its connections with it: by itself once it closed, or killed.
    pub fn end(&self, closed: bool) {
        let mut state = lock(&self.state);
        if std::mem::replace(&mut state.wire.ended, true) {
            return;
        }
        self.attached();
        state.wire.lane = None;
        let connections = std::mem::take(&mut state.wire.connections);
        drop(state);
        self.process.end(closed);
        for connection in connections {
            let _ = connection.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Keeps the host's end of a connection, closed when the host ends; false once it ended.
    pub fn hold(&self, stream: &UnixStream) -> bool {
        let Ok(connection) = stream.try_clone() else { return true };
        let mut state = lock(&self.state);
        if state.wire.ended {
            return false;
        }
        state.wire.connections.push(connection);
        true
    }

    fn handshake(&self, reader: &mut FramedReader<UnixStream>, writer: &mut FramedWriter<UnixStream>) -> Result<(), String> {
        let b = &self.bootstrap;
        let wire = |error: crate::protocol::Error| error.to_string();
        writer
            .write(&Frame::new(Body::Hello(Hello {
                protocol_versions: vec![PROTOCOL_VERSION],
                capabilities: self.capabilities.iter().map(|c| (*c).to_owned()).collect(),
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
            None => return Err(String::new()),
            other => return Err(format!("expected the daemon's attach, found {other:?}")),
        };
        verify_owner_proof(&self.secret, &attach).map_err(|e| format!("the daemon's proof: {e}"))?;
        let proof = host_proof(&self.secret, &attach).map_err(|e| e.to_string())?;
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
}

/// What a model does with the hosts it plays.
pub trait Model {
    type Play;
    /// Something moved on a host's wire.
    fn tick(&self);
    /// Something went wrong that the test should hear of.
    fn troubled(&self, why: String);
    /// The host answers `command`, which it never answered before.
    fn command(&self, host: &Host<Self::Play>, state: &mut HostState<Self::Play>, command: Command);
}

/// Serves one daemon's connection to `host`: the attach, what it had not acknowledged, then every
/// frame it sends, until it goes. It returns once the connection closes.
pub fn serve<M: Model>(model: &M, host: &Arc<Host<M::Play>>, stream: UnixStream) {
    let Ok(clone) = stream.try_clone() else { return };
    let (mut reader, mut writer) = (FramedReader::new(stream), FramedWriter::new(clone));
    match host.handshake(&mut reader, &mut writer) {
        Ok(()) => host.attached(),
        Err(why) if why.is_empty() => return,
        Err(why) => return model.troubled(format!("the attach of the host of {}: {why}", host.task)),
    }
    {
        let mut state = lock(&host.state);
        let wire = &mut state.wire;
        let unacknowledged: Vec<Frame> = wire.journal.iter().filter(|(cursor, _)| *cursor > wire.acknowledged).map(|(_, frame)| frame.clone()).collect();
        wire.lane = Some(writer);
        for frame in &unacknowledged {
            wire.send(frame);
        }
    }
    model.tick();
    while let Ok(Some(frame)) = reader.read() {
        model.tick();
        let mut state = lock(&host.state);
        if state.wire.ended {
            return;
        }
        match frame.body {
            Body::Acknowledge(acknowledge) => {
                let wire = &mut state.wire;
                wire.acknowledged = wire.acknowledged.max(acknowledge.cursor);
                let acknowledged = wire.acknowledged;
                wire.journal.retain(|(cursor, _)| *cursor > acknowledged);
                if wire.closing && wire.acknowledged >= wire.cursor {
                    drop(state);
                    host.end(true);
                    return;
                }
            }
            Body::Command(command) => {
                if state.wire.answered_before(&command) {
                    continue;
                }
                model.command(host, &mut state, command);
            }
            other => model.troubled(format!("the daemon sent the host of {} a frame it never sends: {other:?}", host.task)),
        }
    }
    lock(&host.state).wire.lane = None;
    model.tick();
}

/// A doubled host of `engine-doubles` as the process of a host of a model.
pub struct Doubled(pub Served);
impl Process for Doubled {
    fn alive(&self) -> bool {
        self.0.alive()
    }
    fn end(&self, closed: bool) {
        self.0.end(ExitStatus::from_raw(if closed { 0 } else { KILLED }));
    }
}

/// `host` ends when its doubled process does, however that ends.
pub fn ends_with<P: Send + 'static>(served: &Served, host: &Arc<Host<P>>) {
    let (watched, ended) = (served.clone(), host.clone());
    std::thread::spawn(move || {
        watched.wait();
        ended.end(false);
    });
}
