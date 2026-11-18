//! The port through which the engine and the providers start, adopt, stop and reclaim the local
//! processes their agents run in, and run a provider's command line to its end. The adapter that
//! owns those processes implements it; nobody above it sees how.
use alondra_process_group::Member;
use std::collections::BTreeMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub trait AgentHost: Send + Sync {
    /// Starts a host in `launch.state_dir` with the bootstrap `build` writes for it. `build` is
    /// called at most once.
    fn launch(&self, launch: HostLaunch<'_>, build: &mut dyn FnMut(&LaunchIdentity) -> io::Result<Vec<u8>>) -> io::Result<Box<dyn LaunchedHost>>;
    /// Reads the bootstrap of the host running in `state_dir` and hands it to `inspect`, called at
    /// most once, before reaching the host.
    fn adopt(&self, state_dir: &Path, inspect: &mut dyn FnMut(&HostFolder, &[u8]) -> io::Result<()>) -> io::Result<Box<dyn AdoptedEndpoint>>;
    /// What is left in the folder where a host ran: nothing, a live host, or the bootstrap of one
    /// that ended, with its folder locked.
    fn claim(&self, state_dir: &Path) -> io::Result<Retained>;
    /// Ends whatever still holds the lock of `state_dir`, then claims it.
    fn end_holders(&self, state_dir: &Path) -> io::Result<Retained>;
    /// Runs a provider's command line, or its host in a probe mode, to its end.
    fn run(&self, run: ProbeRun) -> io::Result<ProbeOutput>;
}

/// The registry's `AgentHost`, or an error when nothing provides it.
pub fn of(registry: &crate::Registry) -> io::Result<Arc<dyn AgentHost>> {
    registry.port::<dyn AgentHost>().ok_or_else(|| io::Error::other("nothing starts the hosts of agents: no AgentHost is wired"))
}

/// What a launch needs besides its bootstrap.
pub struct HostLaunch<'a> {
    pub artifact: VerifiedArtifact,
    pub state_dir: &'a Path,
    pub environment: &'a BTreeMap<String, String>,
    /// The Seatbelt profile the host and everything it starts run under, if any.
    pub confinement: Option<&'a Path>,
}

/// A host this daemon started and holds as its child.
pub trait LaunchedHost: Send {
    fn identity(&self) -> &LaunchIdentity;
    /// Returns an unauthenticated connection; an error retains the host.
    fn wait_for_socket(&mut self, deadline: Instant) -> io::Result<UnixStream>;
    fn observed_exit(&mut self) -> io::Result<Option<ExitStatus>>;
    /// It leads its own process group, named by this id.
    fn process_id(&self) -> u32;
    fn stopper(&self) -> HostStop;
    fn diagnostic_tail(&self) -> Vec<DiagnosticCode>;
    /// Waits for the host and every process left in its group to exit, then reaps it.
    fn wait(&mut self) -> io::Result<ExitStatus>;
}

/// A host an earlier daemon started, reached at its endpoint.
pub trait AdoptedEndpoint: Send {
    fn identity(&self) -> &HostFolder;
    /// Also records the process at the endpoint, which a later stop is addressed to.
    fn connect(&self, deadline: Instant) -> io::Result<UnixStream>;
    fn stopper(&self) -> HostStop;
    fn stop_with_anchor(&mut self, anchor: Option<Member>, force: bool) -> io::Result<()>;
    fn stop(&mut self) -> io::Result<()>;
}

/// Asks one host to stop, from any thread, while it is still the process it was.
pub trait StopHandle: Send + Sync {
    fn process(&self) -> io::Result<Option<Member>>;
    fn kill(&self) -> io::Result<()>;
    fn request(&self) -> io::Result<()>;
    fn request_graceful_stop(&self, timeout: Duration) -> io::Result<bool>;
    fn request_after_interrupt(&self) -> io::Result<()>;
    fn process_group(&self) -> Option<i32>;
}

#[derive(Clone)]
pub struct HostStop(Arc<dyn StopHandle>);
impl HostStop {
    pub fn new(handle: Arc<dyn StopHandle>) -> Self {
        Self(handle)
    }
    /// The bound host while it is still the same live process, with its kernel birth identity.
    pub fn process(&self) -> io::Result<Option<Member>> {
        self.0.process()
    }
    pub fn kill(&self) -> io::Result<()> {
        self.0.kill()
    }
    pub fn request(&self) -> io::Result<()> {
        self.0.request()
    }
    pub fn request_graceful_stop(&self, timeout: Duration) -> io::Result<bool> {
        self.0.request_graceful_stop(timeout)
    }
    pub fn request_after_interrupt(&self) -> io::Result<()> {
        self.0.request_after_interrupt()
    }
    /// `None` once the host is gone or before an adopted host was reached.
    pub fn process_group(&self) -> Option<i32> {
        self.0.process_group()
    }
}

/// A launched host with the authority that owns it: the store's grant for a task, or another
/// owner its own authority issued.
pub struct OwnedHost<O> {
    owner: O,
    host: Box<dyn LaunchedHost>,
}
impl<O> OwnedHost<O> {
    pub fn launch(
        hosts: &dyn AgentHost,
        owner: O,
        launch: HostLaunch<'_>,
        build: impl FnOnce(&O, &LaunchIdentity) -> io::Result<Vec<u8>>,
    ) -> io::Result<Self> {
        let mut build = Some(build);
        let host = hosts.launch(launch, &mut |identity| build.take().ok_or_else(|| io::Error::other("a host bootstrap is built once"))?(&owner, identity))?;
        Ok(Self { owner, host })
    }
    pub fn owner(&self) -> &O {
        &self.owner
    }
    pub fn identity(&self) -> &LaunchIdentity {
        self.host.identity()
    }
    pub fn wait_for_socket(&mut self, deadline: Instant) -> io::Result<UnixStream> {
        self.host.wait_for_socket(deadline)
    }
    pub fn observed_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        self.host.observed_exit()
    }
    pub fn process_id(&self) -> u32 {
        self.host.process_id()
    }
    pub fn request_stop(&mut self) -> io::Result<()> {
        self.stopper().request()
    }
    /// The process group this host led. Use `stopper` for control protected against PID reuse.
    pub fn process_group(&self) -> i32 {
        i32::try_from(self.host.process_id()).expect("a process id fits a process group number")
    }
    pub fn stopper(&self) -> HostStop {
        self.host.stopper()
    }
    pub fn diagnostic_tail(&self) -> Vec<DiagnosticCode> {
        self.host.diagnostic_tail()
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.host.wait()
    }
}

/// An adopted host with the authority that owns it.
pub struct AdoptedHost<O> {
    owner: O,
    host: Box<dyn AdoptedEndpoint>,
}
impl<O> AdoptedHost<O> {
    /// Nothing of the host's release is read from disk: only its bootstrap says what it runs.
    pub fn adopt<T>(
        hosts: &dyn AgentHost,
        owner: O,
        state_dir: &Path,
        inspect: impl FnOnce(&O, &HostFolder, &[u8]) -> io::Result<T>,
    ) -> io::Result<(Self, T)> {
        let mut inspect = Some(inspect);
        let mut value = None;
        let host = hosts.adopt(state_dir, &mut |folder, bytes| {
            let inspect = inspect.take().ok_or_else(|| io::Error::other("a host bootstrap is inspected once"))?;
            value = Some(inspect(&owner, folder, bytes)?);
            Ok(())
        })?;
        let value = value.ok_or_else(|| io::Error::other("the adopted host's bootstrap was not inspected"))?;
        Ok((Self { owner, host }, value))
    }
    pub fn owner(&self) -> &O {
        &self.owner
    }
    pub fn identity(&self) -> &HostFolder {
        self.host.identity()
    }
    pub fn connect(&self, deadline: Instant) -> io::Result<UnixStream> {
        self.host.connect(deadline)
    }
    pub fn stopper(&self) -> HostStop {
        self.host.stopper()
    }
    pub fn stop_with_anchor(&mut self, anchor: Option<Member>, force: bool) -> io::Result<()> {
        self.host.stop_with_anchor(anchor, force)
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.host.stop()
    }
}

/// What a daemon finds in the folder where a host of its execution ran. The kernel releases a
/// host's lock when its process ends, a reboot included, so taking the lock is the fact that
/// the host ended.
pub enum Retained {
    /// No host ever wrote its bootstrap here.
    Absent,
    /// A process still holds the host's lock: the host is alive.
    Held,
    /// No process holds the lock: the host ended. The claim keeps the lock until it is retired.
    Ended(EndedHost),
}

/// The folder of a host that ended, locked by this daemon, with the bootstrap it ran with.
pub struct EndedHost {
    state_dir: PathBuf,
    bootstrap: Vec<u8>,
    _lock: Box<dyn Send>,
}
impl EndedHost {
    /// `lock` keeps the folder claimed until the value is retired or dropped.
    pub fn new(state_dir: PathBuf, bootstrap: Vec<u8>, lock: Box<dyn Send>) -> Self {
        Self { state_dir, bootstrap, _lock: lock }
    }
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }
    pub fn bootstrap(&self) -> &[u8] {
        &self.bootstrap
    }
    /// Keeps the ended host's folder as evidence under `archive` and frees its path for the next
    /// host. The lock is released only once the rename is durable.
    pub fn retire(self, archive: &Path) -> io::Result<()> {
        if archive.parent() != self.state_dir.parent() || std::fs::symlink_metadata(archive).is_ok() {
            return Err(refused("ended host archive refused"));
        }
        std::fs::rename(&self.state_dir, archive)?;
        let parent = archive.parent().ok_or_else(|| refused("ended host archive has no parent"))?;
        std::fs::File::open(parent)?.sync_all()
    }
}

/// A host's executable and the artifact it runs, checked against the digests a release names.
#[derive(Clone, Debug)]
pub struct VerifiedArtifact {
    executable: PathBuf,
    artifact: PathBuf,
    executable_sha256: String,
    artifact_sha256: String,
}
impl VerifiedArtifact {
    pub fn verify(executable: &Path, artifact: &Path, executable_sha256: &str, artifact_sha256: &str) -> io::Result<Self> {
        for digest in [executable_sha256, artifact_sha256] {
            if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                return Err(refused("invalid artifact digest"));
            }
        }
        let value = Self {
            executable: executable.canonicalize()?,
            artifact: artifact.canonicalize()?,
            executable_sha256: executable_sha256.into(),
            artifact_sha256: artifact_sha256.into(),
        };
        value.revalidate()?;
        Ok(value)
    }
    /// Fails when either file no longer has the digest it was verified with.
    pub fn revalidate(&self) -> io::Result<()> {
        use alondra_foundation::artifact_digest::digest;
        if digest(&self.executable)? != self.executable_sha256 || digest(&self.artifact)? != self.artifact_sha256 {
            return Err(refused("owned artifact hash mismatch"));
        }
        Ok(())
    }
    pub fn executable(&self) -> &Path {
        &self.executable
    }
    pub fn artifact(&self) -> &Path {
        &self.artifact
    }
    pub fn executable_sha256(&self) -> &str {
        &self.executable_sha256
    }
    pub fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }
}

/// Where a host of one execution keeps its lock and its endpoint.
#[derive(Debug)]
pub struct HostFolder {
    state_dir: PathBuf,
    ownership_path: PathBuf,
    socket_path: PathBuf,
}
impl HostFolder {
    pub fn at(state_dir: PathBuf) -> Self {
        Self { ownership_path: state_dir.join("host.lock"), socket_path: state_dir.join("s"), state_dir }
    }
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }
    pub fn ownership_path(&self) -> &Path {
        &self.ownership_path
    }
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[derive(Debug)]
pub struct LaunchIdentity {
    folder: HostFolder,
    artifact: VerifiedArtifact,
}
impl LaunchIdentity {
    pub fn new(folder: HostFolder, artifact: VerifiedArtifact) -> Self {
        Self { folder, artifact }
    }
    pub fn folder(&self) -> &HostFolder {
        &self.folder
    }
    pub fn state_dir(&self) -> &Path {
        self.folder.state_dir()
    }
    pub fn ownership_path(&self) -> &Path {
        self.folder.ownership_path()
    }
    pub fn socket_path(&self) -> &Path {
        self.folder.socket_path()
    }
    pub fn artifact(&self) -> &VerifiedArtifact {
        &self.artifact
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DiagnosticCode {
    OutputDiscarded,
    ReadinessHint,
    PipeClosed,
    ReaderFailed,
    ReapFailed,
}

/// A command run to its end, with what it reads on its input and how long it may take.
pub struct ProbeRun {
    pub command: Command,
    pub input: Vec<u8>,
    /// `None` waits for as long as it runs.
    pub timeout: Option<Duration>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
#[path = "agent_host_tests.rs"]
mod tests;
