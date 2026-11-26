//! The daemon↔host transport every host model speaks, as the persistent host speaks it: the hello
//! and challenge, the daemon's proof and the host's, the journal of events the daemon has not
//! acknowledged, and one receipt per command, sent again for a copy of it. The host keeps that
//! journal in `<state_dir>/outbox` as the persistent host does, so the daemon that comes after it
//! reads what it left. The model that compiles this names its protocol crate `crate::protocol`;
//! what a host does with each command is the model's (`Model::command`).

use crate::protocol::{
    Attach, Attached, Body, Challenge, Command, Execution, Fence, Frame, FramedReader, FramedWriter, Hello, PROTOCOL_VERSION, command_digest, encode,
    host_proof, verify_owner_proof,
};
use alondra_engine_doubles::agent_host::Served;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{self, Write as _};
use std::ops::{Deref, DerefMut};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
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
    /// The host executable the daemon launched.
    #[serde(default)]
    pub artifact_path: PathBuf,
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

/// The journal the persistent host keeps in `<state_dir>/outbox` (`agent-host/journal.ts`): each
/// command and acknowledgement as the daemon sent it, each grant, receipt and event as the host
/// sends it, every one framed as on the wire and appended before the host acts on it or sends it.
/// A write that fails leaves `uncertain` beside it, and the host keeps and sends nothing more.
struct Outbox {
    file: File,
    folder: PathBuf,
    /// The commands it holds: a copy the daemon sends again is not kept twice.
    commands: BTreeSet<String>,
    uncertain: bool,
}

impl Outbox {
    /// The journal in `folder`, created when the host starts; none for a host with no folder.
    fn open(folder: &Path) -> Option<Self> {
        if folder.as_os_str().is_empty() {
            return None;
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(folder.join("outbox")).ok()?;
        Some(Self { file, folder: folder.to_path_buf(), commands: BTreeSet::new(), uncertain: false })
    }

    fn append(&mut self, framed: &[u8]) -> Result<(), String> {
        if self.uncertain {
            return Err("the host's journal is uncertain: it keeps nothing more".into());
        }
        match self.file.write_all(framed) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.uncertain = true;
                let _ = std::fs::write(self.folder.join("uncertain"), "Technical evidence storage uncertain\n");
                Err(format!("the host could not journal a frame: {error}"))
            }
        }
    }
}

/// `payload` framed as the wire and the journal frame it: its length in four big-endian bytes,
/// then the payload.
fn framed(payload: &[u8]) -> Result<Vec<u8>, String> {
    let length = u32::try_from(payload.len()).map_err(|_| "a frame longer than the wire carries".to_owned())?;
    let mut bytes = Vec::with_capacity(4 + payload.len());
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

/// A host's side of the wire: the lane to the attached daemon, every event it has not acknowledged
/// and the receipt of every command.
#[derive(Default)]
pub struct Wire {
    lane: Option<FramedWriter<UnixStream>>,
    /// The connection `lane` belongs to, so the end of an older one never closes a newer one's.
    lane_connection: u64,
    connections_served: u64,
    /// The host's end of every connection, closed when the host ends.
    connections: Vec<UnixStream>,
    outbox: Option<Outbox>,
    journal: VecDeque<(u64, Frame)>,
    pub cursor: u64,
    pub acknowledged: u64,
    receipts: BTreeMap<String, Frame>,
    /// The host closed: it ends once the daemon acknowledged its last event.
    pub closing: bool,
    pub ended: bool,
}

impl Wire {
    fn at(base: u64, folder: &Path) -> Self {
        Self { cursor: base, acknowledged: base, outbox: Outbox::open(folder), ..Self::default() }
    }

    /// The host cuts its connections and stays up, as the persistent host destroys its owner's
    /// socket when a command fails with its outcome unknown.
    pub fn cut(&mut self) {
        self.lane = None;
        for connection in self.connections.drain(..) {
            let _ = connection.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Journals `frame` as the host sends it, before it goes on the lane.
    fn keep(&mut self, frame: &Frame) -> Result<(), String> {
        let bytes = encode(frame).map_err(|error| format!("the host made a frame it cannot send: {error}"))?;
        self.keep_framed(&bytes)
    }

    /// Journals what the daemon sent, by its exact bytes, before the host acts on it.
    fn keep_received(&mut self, payload: &[u8]) -> Result<(), String> {
        self.keep_framed(&framed(payload)?)
    }

    /// Journals a command the host never answered, before it acts on it: once, however many copies
    /// of it come.
    fn keep_command(&mut self, command: &Command, payload: &[u8]) -> Result<(), String> {
        if self.outbox.as_mut().is_some_and(|outbox| !outbox.commands.insert(command.command_id.to_string())) {
            return Ok(());
        }
        self.keep_received(payload)
    }

    /// A journal that cannot keep a frame closes the lane, as the persistent host closes its
    /// owner's.
    fn keep_framed(&mut self, bytes: &[u8]) -> Result<(), String> {
        let Some(outbox) = self.outbox.as_mut() else { return Ok(()) };
        let kept = outbox.append(bytes);
        if kept.is_err() {
            self.lane = None;
        }
        kept
    }

    fn send(&mut self, frame: &Frame) {
        if let Some(lane) = self.lane.as_mut()
            && lane.write(frame).is_err()
        {
            self.lane = None;
        }
    }

    /// Makes `writer` the lane, in place of any older connection's: the daemon that adopts a host
    /// attaches while the one it replaces may still be closing its own connection. Returns what
    /// `detach` takes to close this lane and no other.
    fn attach(&mut self, writer: FramedWriter<UnixStream>) -> u64 {
        self.connections_served += 1;
        self.lane_connection = self.connections_served;
        self.lane = Some(writer);
        self.lane_connection
    }

    /// The connection `connection` ended: its lane closes unless a newer connection took its place.
    fn detach(&mut self, connection: u64) {
        if self.lane_connection == connection {
            self.lane = None;
        }
    }

    /// Whether a daemon is attached and has acknowledged every event.
    pub fn committed(&self) -> bool {
        self.lane.is_some() && self.acknowledged >= self.cursor
    }

    /// Journals an observation under the next cursor and sends it on the lane, if one is open.
    pub fn event(&mut self, bootstrap: &Bootstrap, observation: Value) -> Result<(), String> {
        let cursor = self.cursor + 1;
        let frame = json!({"protocol_version": PROTOCOL_VERSION, "body": {"type": "event", "fence": bootstrap.fence, "execution": bootstrap.execution,
            "cursor": cursor, "observation": observation}});
        let frame: Frame = serde_json::from_value(frame).map_err(|error| format!("the host wrote an event that is not a frame: {error}"))?;
        self.keep(&frame)?;
        self.cursor = cursor;
        self.journal.push_back((cursor, frame.clone()));
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
        self.keep(&receipt)?;
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
    /// The host's process is stopped, as SIGSTOP stops it.
    frozen: AtomicBool,
    starting: Arc<AtomicU64>,
    process: Box<dyn Process>,
    /// The clock the daemon counts the frames it writes to this host on.
    clock: alondra_core::clock::Clock,
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
        let wire = Wire::at(bootstrap.journal_base.unwrap_or_default(), &bootstrap.state_dir);
        Self {
            task,
            account,
            session,
            capability,
            resumes,
            process_id,
            unattached: AtomicBool::new(true),
            frozen: AtomicBool::new(false),
            starting,
            process,
            clock: alondra_core::clock::Clock::current(),
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

    /// The host's process stops where it is, as SIGSTOP stops it: from now on it reads, answers and
    /// writes nothing, and only a kill ends it.
    pub fn freeze(&self) {
        self.frozen.store(true, Ordering::SeqCst);
    }

    pub fn frozen(&self) -> bool {
        self.frozen.load(Ordering::SeqCst)
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
        state.wire.outbox = None;
        let connections = std::mem::take(&mut state.wire.connections);
        drop(state);
        self.clock.frames_dropped(&self.bootstrap.host_instance_id);
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
        let attached = Frame::new(Body::Attached(Attached {
            fence: attach.fence,
            execution: attach.execution,
            host_instance_id: attach.host_instance_id,
            owner_generation: attach.owner_generation,
            proof,
        }));
        lock(&self.state).wire.keep(&attached)?;
        writer.write(&attached).map_err(wire)
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
/// frame it sends, until it goes. It returns once the connection closes. A frozen host leaves the
/// connection open and unread.
pub fn serve<M: Model>(model: &M, host: &Arc<Host<M::Play>>, stream: UnixStream) {
    let mut reading = Reading { host, to_the_end: false };
    if host.frozen() {
        return;
    }
    let Ok(clone) = stream.try_clone() else { return };
    let (mut reader, mut writer) = (FramedReader::new(stream), FramedWriter::new(clone));
    match host.handshake(&mut reader, &mut writer) {
        Ok(()) => {
            host.attached();
            host.clock.frame_handled(&host.bootstrap.host_instance_id);
        }
        Err(why) if why.is_empty() => return,
        Err(why) => return model.troubled(format!("the attach of the host of {}: {why}", host.task)),
    }
    let connection;
    {
        let mut state = lock(&host.state);
        let wire = &mut state.wire;
        let unacknowledged: Vec<Frame> = wire.journal.iter().filter(|(cursor, _)| *cursor > wire.acknowledged).map(|(_, frame)| frame.clone()).collect();
        connection = wire.attach(writer);
        for frame in &unacknowledged {
            wire.send(frame);
        }
    }
    model.tick();
    loop {
        let received = match reader.read_received() {
            Ok(Some(received)) => received,
            Ok(None) => {
                reading.to_the_end = true;
                break;
            }
            Err(_) => break,
        };
        let _handled = Handled(host);
        model.tick();
        if host.frozen() {
            continue;
        }
        let mut state = lock(&host.state);
        if state.wire.ended {
            return;
        }
        let (frame, payload, _) = received.into_parts();
        match frame.body {
            Body::Acknowledge(acknowledge) => {
                if let Err(why) = state.wire.keep_received(&payload) {
                    model.troubled(format!("the host of {}: {why}", host.task));
                    break;
                }
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
                if let Err(why) = state.wire.keep_command(&command, &payload) {
                    model.troubled(format!("the host of {}: {why}", host.task));
                    break;
                }
                model.command(host, &mut state, command);
            }
            other => model.troubled(format!("the daemon sent the host of {} a frame it never sends: {other:?}", host.task)),
        }
    }
    lock(&host.state).wire.detach(connection);
    model.tick();
}

/// A frame the daemon wrote to the host, read: it is handled once what it says is done, its answer
/// written, at the end of the turn of the loop that read it.
struct Handled<'a, P>(&'a Host<P>);

impl<P> Drop for Handled<'_, P> {
    fn drop(&mut self) {
        self.0.clock.frame_handled(&self.0.bootstrap.host_instance_id);
    }
}

/// One connection the host serves: when it stops reading before the daemon closed it, what the
/// daemon wrote after is never handled.
struct Reading<'a, P> {
    host: &'a Host<P>,
    to_the_end: bool,
}

impl<P> Drop for Reading<'_, P> {
    fn drop(&mut self) {
        if !self.to_the_end {
            self.host.clock.frames_dropped(&self.host.bootstrap.host_instance_id);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::owner_proof;
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    struct Ticks(Mutex<mpsc::Sender<()>>);

    impl Model for Ticks {
        type Play = ();
        fn tick(&self) {
            let _ = lock(&self.0).send(());
        }
        fn troubled(&self, why: String) {
            panic!("{why}");
        }
        fn command(&self, _: &Host<()>, _: &mut HostState<()>, _: Command) {}
    }

    struct Running;

    impl Process for Running {
        fn alive(&self) -> bool {
            true
        }
        fn end(&self, _: bool) {}
    }

    const SECRET: &str = "0101010101010101010101010101010101010101010101010101010101010101";

    fn host() -> Arc<Host<()>> {
        let bootstrap = json!({
            "host_instance_id": "host-1",
            "control_secret": SECRET,
            "fence": {"profile_id": "default", "account_id": "default", "data_epoch": "epoch", "host_id": "host"},
            "execution": {"task_id": "t", "agent_id": "sdk-t", "conversation_id": "conversation", "process_instance_id": "process", "generation": 1},
            "manifest": {"host_release": "0.0.0", "host_sha256": "a".repeat(64), "executable_sha256": "b".repeat(64), "sdk_version": "sdk"},
            "native": {"session_id": "session"},
        });
        let identity = Identity { task: "t".into(), account: "default".into(), session: "session".into(), capability: String::new(), resumes: false, process_id: 1 };
        let bootstrap = Bootstrap::read(&serde_json::to_vec(&bootstrap).unwrap()).unwrap();
        Arc::new(Host::new(identity, bootstrap, &[], Box::new(Running), Arc::default(), ()))
    }

    /// A daemon's connection to `host`, attached, and the thread that serves it.
    struct Daemon {
        stream: UnixStream,
        served: JoinHandle<()>,
    }

    fn attach(host: &Arc<Host<()>>, ticks: &Arc<Ticks>, moved: &mpsc::Receiver<()>) -> Daemon {
        let (stream, host_end) = UnixStream::pair().unwrap();
        let (served_host, model) = (host.clone(), ticks.clone());
        let served = std::thread::spawn(move || serve(&*model, &served_host, host_end));
        let mut reader = FramedReader::new(stream.try_clone().unwrap());
        let mut writer = FramedWriter::new(stream.try_clone().unwrap());
        let Some(Body::Hello(_)) = reader.read().unwrap().map(|frame| frame.body) else { panic!("the host speaks first") };
        let Some(Body::Challenge(challenge)) = reader.read().unwrap().map(|frame| frame.body) else { panic!("then it challenges") };
        let mut attach = Attach {
            fence: challenge.fence,
            execution: challenge.execution,
            host_instance_id: challenge.host_instance_id,
            nonce: challenge.nonce,
            owner_generation: 1,
            host_release: "0.0.0".into(),
            host_sha256: "a".repeat(64),
            executable_sha256: "b".repeat(64),
            sdk_version: "sdk".into(),
            proof: String::new(),
        };
        attach.proof = owner_proof(&decode_secret(SECRET).unwrap(), &attach).unwrap();
        writer.write(&Frame::new(Body::Attach(attach))).unwrap();
        let Some(Body::Attached(_)) = reader.read().unwrap().map(|frame| frame.body) else { panic!("the host attaches the daemon") };
        moved.recv().unwrap();
        Daemon { stream, served }
    }

    impl Daemon {
        fn goes(self) {
            self.stream.shutdown(std::net::Shutdown::Both).unwrap();
            self.served.join().unwrap();
        }
    }

    fn lane_open(host: &Host<()>) -> bool {
        lock(&host.state).wire.lane.is_some()
    }

    #[test]
    fn the_daemon_that_goes_leaves_no_lane() {
        let (sender, moved) = mpsc::channel();
        let (host, ticks) = (host(), Arc::new(Ticks(Mutex::new(sender))));
        let daemon = attach(&host, &ticks, &moved);
        assert!(lane_open(&host));
        daemon.goes();
        assert!(!lane_open(&host));
    }

    /// The daemon that adopts a host attaches while the one it replaces is still closing its
    /// connection: whichever thread of the host sees its connection end last, the new daemon keeps
    /// its lane, and its commands get their receipts.
    #[test]
    fn a_daemon_that_took_over_keeps_its_lane_when_the_one_it_replaced_goes_after_it_attached() {
        let (sender, moved) = mpsc::channel();
        let (host, ticks) = (host(), Arc::new(Ticks(Mutex::new(sender))));
        let replaced = attach(&host, &ticks, &moved);
        let adopting = attach(&host, &ticks, &moved);
        replaced.goes();
        assert!(lane_open(&host), "the connection that ended is not the one the lane belongs to");
        adopting.goes();
        assert!(!lane_open(&host));
    }
}
