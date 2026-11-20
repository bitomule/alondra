//! The hosts of a test's agents, with no process behind them: each folder holds, in memory, the
//! host that runs there, and a test says what a host does on its connections. They speak no
//! provider's protocol and pass the contract the real hosts pass (`contract::agent_host`).
use alondra_engine_ports::Registry;
use alondra_engine_ports::agent_host::{
    AdoptedEndpoint, AgentHost, DiagnosticCode, EndedHost, HostFolder, HostLaunch, HostStop, LaunchIdentity, LaunchedHost, ProbeOutput, ProbeRun, Retained, StopHandle,
};
use alondra_process_group::Member;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// No kernel gives a process an id this high, so a number of these hosts never names a real one.
const FIRST_PROCESS: u32 = 1_000_000_000;
static NEXT_PROCESS: AtomicU32 = AtomicU32::new(FIRST_PROCESS);
const BOOTSTRAP_LIMIT: usize = 65_536;
const FORCED_STOP_GRACE: Duration = Duration::from_secs(5);

/// What a host does, as a test plays it.
pub trait Serve: Send + Sync {
    /// A connection to `host` opened; `stream` is the host's end of it. It runs on the thread that
    /// connects: a host that answers does it on a thread of its own.
    fn connect(&self, host: &Served, stream: UnixStream);

    /// `host` was asked to stop. It ends as a process that signal ends, unless a test plays a host
    /// that goes on.
    fn stop(&self, host: &Served, stop: Stop) {
        host.end(stop.status());
    }
}

/// A host that accepts its connections and says nothing on them.
struct Silent;
impl Serve for Silent {
    fn connect(&self, host: &Served, stream: UnixStream) {
        host.hold(stream);
    }
}

/// What was asked of a host to make it stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Request,
    AfterInterrupt,
    Kill,
}
impl Stop {
    /// The exit of a process the signal of this stop ended.
    pub fn status(self) -> ExitStatus {
        ExitStatus::from_raw(match self {
            Self::Request => libc::SIGTERM,
            Self::AfterInterrupt => libc::SIGINT,
            Self::Kill => libc::SIGKILL,
        })
    }
}

/// A command a provider asked to run to its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probed {
    pub program: OsString,
    pub arguments: Vec<OsString>,
    pub input: Vec<u8>,
}

type Probe = dyn Fn(&ProbeRun) -> io::Result<ProbeOutput> + Send + Sync;

struct Host {
    process: u32,
    folder: HostFolder,
    bootstrap: Vec<u8>,
    life: Mutex<Life>,
    ended: Condvar,
}
#[derive(Default)]
struct Life {
    exit: Option<ExitStatus>,
    claimed: bool,
    footprint: u64,
    held: Vec<UnixStream>,
}
impl Host {
    fn life(&self) -> MutexGuard<'_, Life> {
        self.life.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn exit(&self) -> Option<ExitStatus> {
        self.life().exit
    }
    fn alive(&self) -> bool {
        self.exit().is_none()
    }
    fn end(&self, status: ExitStatus) {
        let closed = {
            let mut life = self.life();
            if life.exit.is_some() {
                return;
            }
            life.exit = Some(status);
            std::mem::take(&mut life.held)
        };
        drop(closed);
        self.ended.notify_all();
    }
    fn wait(&self) -> ExitStatus {
        let mut life = self.life();
        loop {
            match life.exit {
                Some(status) => return status,
                None => life = self.ended.wait(life).unwrap_or_else(PoisonError::into_inner),
            }
        }
    }
    fn ended_within(&self, timeout: Duration) -> bool {
        let (life, _) = self.ended.wait_timeout_while(self.life(), timeout, |life| life.exit.is_none()).unwrap_or_else(PoisonError::into_inner);
        life.exit.is_some()
    }
    fn member(&self) -> Member {
        Member { pid: self.group(), started: (u64::from(self.process), 0), zombie: false }
    }
    fn group(&self) -> i32 {
        i32::try_from(self.process).expect("a doubled process id fits a process group number")
    }
}

/// A host as the test that plays it sees it.
#[derive(Clone)]
pub struct Served(Arc<Host>);
impl Served {
    pub fn bootstrap(&self) -> &[u8] {
        &self.0.bootstrap
    }
    pub fn state_dir(&self) -> &Path {
        self.0.folder.state_dir()
    }
    pub fn process_id(&self) -> u32 {
        self.0.process
    }
    pub fn alive(&self) -> bool {
        self.0.alive()
    }
    /// The host ends by itself with `status`, and its connections close.
    pub fn end(&self, status: ExitStatus) {
        self.0.end(status);
    }
    /// Waits until the host ends, however it ends, and returns how.
    pub fn wait(&self) -> ExitStatus {
        self.0.wait()
    }
    /// Keeps the host's end of a connection open until the host ends.
    pub fn hold(&self, stream: UnixStream) {
        let mut life = self.0.life();
        if life.exit.is_none() {
            life.held.push(stream);
        }
    }
}

#[derive(Default)]
struct Folder {
    host: Option<Arc<Host>>,
    launching: bool,
    launches: usize,
    connections: usize,
    stops: Vec<Stop>,
}

struct Shared {
    folders: Mutex<BTreeMap<PathBuf, Folder>>,
    serve: Mutex<Arc<dyn Serve>>,
    probe: Mutex<Option<Arc<Probe>>>,
    probed: Mutex<Vec<Probed>>,
}
impl Shared {
    fn folders(&self) -> MutexGuard<'_, BTreeMap<PathBuf, Folder>> {
        self.folders.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn serve(&self) -> Arc<dyn Serve> {
        self.serve.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The host whose bootstrap `state_dir` still holds. One whose folder went, retired or
    /// deleted, is forgotten, and ends if it ran.
    fn host(&self, state_dir: &Path) -> Option<Arc<Host>> {
        let mut folders = self.folders();
        let folder = folders.get_mut(state_dir)?;
        let host = folder.host.clone()?;
        if std::fs::symlink_metadata(host.folder.ownership_path()).is_ok() {
            return Some(host);
        }
        folder.host = None;
        drop(folders);
        host.end(Stop::Kill.status());
        None
    }

    fn connect(&self, host: &Arc<Host>, deadline: Instant, ended: io::ErrorKind) -> io::Result<UnixStream> {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if !host.alive() {
            return Err(io::Error::new(ended, "the doubled host ended"));
        }
        let (ours, theirs) = UnixStream::pair()?;
        if let Some(folder) = self.folders().get_mut(host.folder.state_dir()) {
            folder.connections += 1;
        }
        self.serve().connect(&Served(host.clone()), theirs);
        Ok(ours)
    }

    fn ask(&self, host: &Arc<Host>, stop: Stop) {
        if !host.alive() {
            return;
        }
        if let Some(folder) = self.folders().get_mut(host.folder.state_dir()) {
            folder.stops.push(stop);
        }
        match stop {
            Stop::Kill => host.end(stop.status()),
            Stop::Request | Stop::AfterInterrupt => self.serve().stop(&Served(host.clone()), stop),
        }
    }
}

/// The doubled hosts. They outlive the runtime they were given to: a test that restarts its daemon
/// gives the next runtime the same ones (`provide`), and finds there the hosts the one before left.
pub struct InProcessHosts(Arc<Shared>);

impl Default for InProcessHosts {
    fn default() -> Self {
        Self(Arc::new(Shared { folders: Mutex::default(), serve: Mutex::new(Arc::new(Silent)), probe: Mutex::default(), probed: Mutex::default() }))
    }
}

/// Gives `registry` new doubled hosts, both as the port and as themselves for the test to set.
pub fn wire(registry: &Registry) -> Arc<InProcessHosts> {
    let hosts = Arc::new(InProcessHosts::default());
    hosts.provide(registry);
    hosts
}

/// The doubled hosts `registry` was given.
pub fn of(registry: &Registry) -> Arc<InProcessHosts> {
    registry.port::<InProcessHosts>().expect("the test's runtime has the doubled hosts")
}

impl InProcessHosts {
    /// Gives `registry` these hosts, with whatever runs in them.
    pub fn provide(self: &Arc<Self>, registry: &Registry) {
        registry.provide::<dyn AgentHost>(self.clone());
        registry.provide::<InProcessHosts>(self.clone());
    }

    /// What every host does from now on.
    pub fn set_serve(&self, serve: impl Serve + 'static) {
        *self.0.serve.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(serve);
    }

    /// What a provider's command line answers from now on. It is never run.
    pub fn set_probe(&self, probe: impl Fn(&ProbeRun) -> io::Result<ProbeOutput> + Send + Sync + 'static) {
        *self.0.probe.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(probe));
    }

    /// The memory the live host in `state_dir` holds from now on.
    pub fn set_footprint(&self, state_dir: &Path, bytes: u64) {
        if let Some(host) = self.0.host(state_dir) {
            host.life().footprint = bytes;
        }
    }

    /// The live host in `state_dir`, to end it or read what it was launched with.
    pub fn served(&self, state_dir: &Path) -> Option<Served> {
        self.0.host(state_dir).filter(|host| host.alive()).map(Served)
    }

    /// The folders where a host runs.
    pub fn live(&self) -> Vec<PathBuf> {
        self.0.folders().iter().filter(|(_, folder)| folder.host.as_ref().is_some_and(|host| host.alive())).map(|(path, _)| path.clone()).collect()
    }

    /// How many hosts were launched in `state_dir`.
    pub fn launches(&self, state_dir: &Path) -> usize {
        self.0.folders().get(state_dir).map_or(0, |folder| folder.launches)
    }

    /// How many connections the hosts of `state_dir` took.
    pub fn connections(&self, state_dir: &Path) -> usize {
        self.0.folders().get(state_dir).map_or(0, |folder| folder.connections)
    }

    /// What was asked of the hosts of `state_dir` to make them stop, in order.
    pub fn stops(&self, state_dir: &Path) -> Vec<Stop> {
        self.0.folders().get(state_dir).map_or_else(Vec::new, |folder| folder.stops.clone())
    }

    /// The command lines asked to run, in order.
    pub fn probed(&self) -> Vec<Probed> {
        self.0.probed.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl AgentHost for InProcessHosts {
    fn launch(&self, launch: HostLaunch<'_>, build: &mut dyn FnMut(&LaunchIdentity) -> io::Result<Vec<u8>>) -> io::Result<Box<dyn LaunchedHost>> {
        private_directory(launch.state_dir)?;
        let retained = self.0.host(launch.state_dir);
        {
            let mut folders = self.0.folders();
            let folder = folders.entry(launch.state_dir.to_path_buf()).or_default();
            let held = folder.launching || retained.as_ref().is_some_and(|host| { let life = host.life(); life.exit.is_none() || life.claimed });
            if held {
                return Err(io::Error::other("the doubled host's folder is held"));
            }
            if retained.is_some() {
                return Err(refused("retained host bootstrap requires reconciliation"));
            }
            folder.launching = true;
        }
        let identity = LaunchIdentity::new(HostFolder::at(launch.state_dir.to_path_buf()), launch.artifact);
        let built = build(&identity).and_then(|bootstrap| retain(identity.folder(), bootstrap));
        let mut folders = self.0.folders();
        let folder = folders.entry(launch.state_dir.to_path_buf()).or_default();
        folder.launching = false;
        let bootstrap = built?;
        let host = Arc::new(Host {
            process: NEXT_PROCESS.fetch_add(1, Ordering::Relaxed),
            folder: HostFolder::at(launch.state_dir.to_path_buf()),
            bootstrap,
            life: Mutex::default(),
            ended: Condvar::new(),
        });
        folder.host = Some(host.clone());
        folder.launches += 1;
        Ok(Box::new(Launched { shared: self.0.clone(), identity, host }))
    }

    fn adopt(&self, state_dir: &Path, inspect: &mut dyn FnMut(&HostFolder, &[u8]) -> io::Result<()>) -> io::Result<Box<dyn AdoptedEndpoint>> {
        private_directory(state_dir)?;
        let host = self.0.host(state_dir).ok_or(io::ErrorKind::NotFound)?;
        let folder = HostFolder::at(state_dir.to_path_buf());
        inspect(&folder, &host.bootstrap)?;
        Ok(Box::new(Adopted { shared: self.0.clone(), folder, host, reached: Arc::default() }))
    }

    fn claim(&self, state_dir: &Path) -> io::Result<Retained> {
        match std::fs::symlink_metadata(state_dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Retained::Absent),
            Err(error) => return Err(error),
            Ok(_) => private_directory(state_dir)?,
        }
        let Some(host) = self.0.host(state_dir) else { return Ok(Retained::Absent) };
        let mut life = host.life();
        if life.exit.is_none() || life.claimed {
            return Ok(Retained::Held);
        }
        life.claimed = true;
        drop(life);
        Ok(Retained::Ended(EndedHost::new(state_dir.to_path_buf(), host.bootstrap.clone(), Box::new(Claim(host)))))
    }

    fn end_holders(&self, state_dir: &Path) -> io::Result<Retained> {
        private_directory(state_dir)?;
        let host = self.0.host(state_dir).ok_or(io::ErrorKind::NotFound)?;
        self.0.ask(&host, Stop::Request);
        host.wait();
        match self.claim(state_dir)? {
            Retained::Held => Err(io::Error::other(format!("the host lock of {} is still held after its holder ended", state_dir.display()))),
            claimed => Ok(claimed),
        }
    }

    fn run(&self, run: ProbeRun) -> io::Result<ProbeOutput> {
        self.0.probed.lock().unwrap_or_else(PoisonError::into_inner).push(Probed {
            program: run.command.get_program().to_owned(),
            arguments: run.command.get_args().map(ToOwned::to_owned).collect(),
            input: run.input.clone(),
        });
        let probe = self.0.probe.lock().unwrap_or_else(PoisonError::into_inner).clone();
        match probe {
            Some(probe) => probe(&run),
            None => Err(io::Error::new(io::ErrorKind::Unsupported, "the test set no answer for a provider's command line")),
        }
    }
}

/// Writes the bootstrap where a host keeps it, so the folder holds what the real one's holds when
/// it is read or retired.
fn retain(folder: &HostFolder, bootstrap: Vec<u8>) -> io::Result<Vec<u8>> {
    if bootstrap.is_empty() || bootstrap.len() > BOOTSTRAP_LIMIT {
        return Err(refused("private bootstrap capacity"));
    }
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(folder.ownership_path())?;
    io::Write::write_all(&mut file, &bootstrap)?;
    Ok(bootstrap)
}

/// The claim of an ended host's folder, released when the claim is retired or dropped.
struct Claim(Arc<Host>);
impl Drop for Claim {
    fn drop(&mut self) {
        self.0.life().claimed = false;
    }
}

struct Launched {
    shared: Arc<Shared>,
    identity: LaunchIdentity,
    host: Arc<Host>,
}
impl LaunchedHost for Launched {
    fn identity(&self) -> &LaunchIdentity {
        &self.identity
    }
    fn wait_for_socket(&mut self, deadline: Instant) -> io::Result<UnixStream> {
        self.shared.connect(&self.host, deadline, io::ErrorKind::ConnectionAborted)
    }
    fn observed_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(self.host.exit())
    }
    fn process_id(&self) -> u32 {
        self.host.process
    }
    fn stopper(&self) -> HostStop {
        HostStop::new(Arc::new(Stopper { shared: self.shared.clone(), bound: Bound::Launched(self.host.clone()) }))
    }
    fn diagnostic_tail(&self) -> Vec<DiagnosticCode> {
        Vec::new()
    }
    fn wait(&mut self) -> io::Result<ExitStatus> {
        Ok(self.host.wait())
    }
}

struct Adopted {
    shared: Arc<Shared>,
    folder: HostFolder,
    host: Arc<Host>,
    reached: Arc<Mutex<Option<Arc<Host>>>>,
}
impl Adopted {
    fn reached(&self) -> Option<Arc<Host>> {
        self.reached.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}
impl AdoptedEndpoint for Adopted {
    fn identity(&self) -> &HostFolder {
        &self.folder
    }
    fn connect(&self, deadline: Instant) -> io::Result<UnixStream> {
        match self.shared.host(self.folder.state_dir()) {
            None => return Err(io::ErrorKind::NotFound.into()),
            Some(current) if !Arc::ptr_eq(&current, &self.host) => return Err(refused("another host took the folder")),
            Some(_) => {}
        }
        let stream = self.shared.connect(&self.host, deadline, io::ErrorKind::ConnectionRefused)?;
        *self.reached.lock().unwrap_or_else(PoisonError::into_inner) = Some(self.host.clone());
        Ok(stream)
    }
    fn stopper(&self) -> HostStop {
        HostStop::new(Arc::new(Stopper { shared: self.shared.clone(), bound: Bound::Adopted(self.reached.clone()) }))
    }
    fn stop_with_anchor(&mut self, _: Option<Member>, force: bool) -> io::Result<()> {
        if !force {
            return self.stop();
        }
        let Some(host) = self.reached() else { return Ok(()) };
        self.shared.ask(&host, Stop::Request);
        if !host.ended_within(FORCED_STOP_GRACE) {
            self.shared.ask(&host, Stop::Kill);
        }
        Ok(())
    }
    fn stop(&mut self) -> io::Result<()> {
        let Some(host) = self.reached() else { return Ok(()) };
        self.shared.ask(&host, Stop::Request);
        host.wait();
        Ok(())
    }
}

/// A launched host is bound from its launch; an adopted one once its endpoint was reached, as the
/// real one learns its process there.
enum Bound {
    Launched(Arc<Host>),
    Adopted(Arc<Mutex<Option<Arc<Host>>>>),
}
struct Stopper {
    shared: Arc<Shared>,
    bound: Bound,
}
impl Stopper {
    fn host(&self) -> Option<Arc<Host>> {
        match &self.bound {
            Bound::Launched(host) => Some(host.clone()),
            Bound::Adopted(reached) => reached.lock().unwrap_or_else(PoisonError::into_inner).clone(),
        }
    }
    fn running(&self) -> Option<Arc<Host>> {
        self.host().filter(|host| host.alive())
    }
    fn ask(&self, stop: Stop) {
        if let Some(host) = self.running() {
            self.shared.ask(&host, stop);
        }
    }
}
impl StopHandle for Stopper {
    fn process(&self) -> io::Result<Option<Member>> {
        Ok(self.running().map(|host| host.member()))
    }
    fn kill(&self) -> io::Result<()> {
        self.ask(Stop::Kill);
        Ok(())
    }
    fn request(&self) -> io::Result<()> {
        self.ask(Stop::Request);
        Ok(())
    }
    fn request_graceful_stop(&self, timeout: Duration) -> io::Result<bool> {
        let Some(host) = self.running() else { return Ok(true) };
        self.shared.ask(&host, Stop::Request);
        Ok(host.ended_within(timeout))
    }
    fn request_after_interrupt(&self) -> io::Result<()> {
        self.ask(Stop::AfterInterrupt);
        Ok(())
    }
    /// A launched host names its group until it ended; an adopted one keeps naming the process it
    /// reached, as the real one does.
    fn process_group(&self) -> Option<i32> {
        match &self.bound {
            Bound::Launched(_) => self.running().map(|host| host.group()),
            Bound::Adopted(_) => self.host().map(|host| host.group()),
        }
    }
    fn gone(&self) -> io::Result<bool> {
        Ok(self.host().is_some_and(|host| !host.alive()))
    }
    fn tree(&self) -> io::Result<Vec<Member>> {
        Ok(Vec::new())
    }
    fn footprint(&self) -> io::Result<Option<u64>> {
        Ok(self.process_group().and(self.host()).map(|host| { let life = host.life(); if life.exit.is_none() { life.footprint } else { 0 } }))
    }
}

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// The folder a host runs in is a private directory named by its real path, as the real hosts ask.
fn private_directory(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.mode() & 0o777 != 0o700 || path.canonicalize()? != path {
        return Err(refused("private canonical host directory refused"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::HostFixture;
    use crate::test_support::Directory;
    use alondra_engine_ports::agent_host::VerifiedArtifact;
    use std::io::{BufRead as _, BufReader, Write as _};

    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    /// A host that answers its process id and ends by itself when told to.
    struct Answering;
    impl Serve for Answering {
        fn connect(&self, host: &Served, mut stream: UnixStream) {
            let host = host.clone();
            std::thread::spawn(move || {
                let mut asked = String::new();
                if BufReader::new(stream.try_clone().unwrap()).read_line(&mut asked).is_err() {
                    return;
                }
                match asked.trim() {
                    "identity" => writeln!(stream, "{}", host.process_id()).unwrap(),
                    "exit" => host.end(ExitStatus::from_raw(0)),
                    _ => {}
                }
            });
        }
    }

    struct Fixture(Directory);
    impl Fixture {
        fn new() -> Self {
            let directory = Directory::new();
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("host"), b"").unwrap();
            Self(directory)
        }
    }
    impl HostFixture for Fixture {
        fn artifact(&self) -> VerifiedArtifact {
            let host = self.0.join("host");
            VerifiedArtifact::verify(&host, &host, EMPTY_SHA256, EMPTY_SHA256).unwrap()
        }
        fn environment(&self, _: &Path) -> BTreeMap<String, String> {
            BTreeMap::new()
        }
        fn process_id(&self, mut stream: UnixStream) -> u32 {
            stream.write_all(b"identity\n").unwrap();
            let mut answer = String::new();
            BufReader::new(stream).read_line(&mut answer).unwrap();
            answer.trim().parse().expect("the host answers its process id")
        }
        fn exit(&self, mut stream: UnixStream) {
            stream.write_all(b"exit\n").unwrap();
        }
    }

    fn private(directory: &Directory) -> PathBuf {
        use std::os::unix::fs::DirBuilderExt as _;
        let state = directory.join("state");
        std::fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
        state
    }

    fn launch(hosts: &dyn AgentHost, fixture: &Fixture, state: &Path) -> Box<dyn LaunchedHost> {
        let launch = HostLaunch { artifact: fixture.artifact(), state_dir: state, environment: &BTreeMap::new(), confinement: None };
        hosts.launch(launch, &mut |_| Ok(b"bootstrap".to_vec())).unwrap()
    }

    #[test]
    fn the_doubled_hosts_keep_the_contract_the_real_hosts_keep() {
        let hosts = InProcessHosts::default();
        hosts.set_serve(Answering);
        crate::contract::agent_host(&hosts, &Fixture::new());
        assert_eq!(hosts.live(), Vec::<PathBuf>::new(), "the contract leaves no host running");
    }

    #[test]
    fn a_doubled_host_never_has_the_number_of_a_process() {
        let fixture = Fixture::new();
        let state = private(&fixture.0);
        let hosts = InProcessHosts::default();
        let host = launch(&hosts, &fixture, &state);
        assert!(host.process_id() >= FIRST_PROCESS && i32::try_from(host.process_id()).is_ok());
        assert_eq!(alondra_process_group::process(host.process_id() as i32).unwrap(), None, "no process has it");
        assert_eq!(host.diagnostic_tail(), vec![]);
    }

    #[test]
    fn a_test_reads_what_ran_in_a_folder_and_what_was_asked_of_it() {
        let fixture = Fixture::new();
        let state = private(&fixture.0);
        let hosts = InProcessHosts::default();
        let mut host = launch(&hosts, &fixture, &state);
        assert_eq!(hosts.live(), [state.clone()]);
        assert_eq!(hosts.served(&state).unwrap().bootstrap(), b"bootstrap");
        let mut stream = host.wait_for_socket(Instant::now() + Duration::from_secs(10)).unwrap();
        stream.set_nonblocking(true).unwrap();
        assert_eq!(io::Read::read(&mut stream, &mut [0]).unwrap_err().kind(), io::ErrorKind::WouldBlock, "a host nobody plays keeps its connection open and says nothing");
        hosts.set_footprint(&state, 4096);
        let stop = host.stopper();
        assert_eq!(stop.footprint().unwrap(), Some(4096));
        stop.request_after_interrupt().unwrap();
        assert_eq!(host.wait().unwrap().signal(), Some(libc::SIGINT));
        assert_eq!(io::Read::read(&mut stream, &mut [0]).unwrap(), 0, "the connections of a host close when it ends");
        stop.kill().unwrap();
        assert_eq!((hosts.launches(&state), hosts.connections(&state), hosts.stops(&state)), (1, 1, vec![Stop::AfterInterrupt]));
        assert_eq!(hosts.live(), Vec::<PathBuf>::new());
        assert!(hosts.served(&state).is_none());
    }

    #[test]
    fn a_host_a_test_keeps_running_is_stopped_only_by_a_kill() {
        struct Stubborn;
        impl Serve for Stubborn {
            fn connect(&self, host: &Served, stream: UnixStream) {
                host.hold(stream);
            }
            fn stop(&self, _: &Served, _: Stop) {}
        }
        let fixture = Fixture::new();
        let state = private(&fixture.0);
        let hosts = InProcessHosts::default();
        hosts.set_serve(Stubborn);
        let mut host = launch(&hosts, &fixture, &state);
        let stop = host.stopper();
        assert!(!stop.request_graceful_stop(Duration::ZERO).unwrap(), "a host that goes on has not stopped");
        assert_eq!(host.observed_exit().unwrap(), None);
        stop.kill().unwrap();
        assert_eq!(host.wait().unwrap().signal(), Some(libc::SIGKILL));
        assert_eq!(hosts.stops(&state), [Stop::Request, Stop::Kill]);
    }

    #[test]
    fn the_hosts_of_one_runtime_are_there_for_the_next() {
        let fixture = Fixture::new();
        let state = private(&fixture.0);
        let before = Registry::default();
        let hosts = wire(&before);
        let host = launch(&*alondra_engine_ports::agent_host::of(&before).unwrap(), &fixture, &state);
        let process = host.process_id();
        drop((host, before));
        let after = Registry::default();
        hosts.provide(&after);
        assert!(Arc::ptr_eq(&of(&after), &hosts));
        let port = alondra_engine_ports::agent_host::of(&after).unwrap();
        assert!(matches!(port.claim(&state).unwrap(), Retained::Held));
        let adopted = port.adopt(&state, &mut |_, bootstrap| { assert_eq!(bootstrap, b"bootstrap"); Ok(()) }).unwrap();
        drop(adopted.connect(Instant::now() + Duration::from_secs(10)).unwrap());
        assert_eq!(adopted.stopper().process_group(), Some(process as i32));
    }

    #[test]
    fn a_providers_command_line_is_answered_by_the_test_and_never_run() {
        let hosts = InProcessHosts::default();
        let run = || {
            let mut command = std::process::Command::new("/usr/bin/false");
            command.arg("--version");
            ProbeRun { command, input: b"in".to_vec(), timeout: None }
        };
        assert_eq!(hosts.run(run()).unwrap_err().kind(), io::ErrorKind::Unsupported);
        hosts.set_probe(|run| Ok(ProbeOutput { success: true, stdout: run.input.clone(), stderr: vec![] }));
        assert_eq!(hosts.run(run()).unwrap(), ProbeOutput { success: true, stdout: b"in".to_vec(), stderr: vec![] });
        let asked = Probed { program: "/usr/bin/false".into(), arguments: vec!["--version".into()], input: b"in".to_vec() };
        assert_eq!(hosts.probed(), [asked.clone(), asked]);
    }
}
