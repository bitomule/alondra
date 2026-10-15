use alondra_store::OwnerGrant;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

const HINT: &[u8] = b"Alondra host: socket listening";
const BOOTSTRAP_LIMIT: usize = 65_536;
const LINE_LIMIT: usize = 4096;
const TAIL_LIMIT: usize = 16_384;
const CONNECT_LIMIT: usize = 8;

#[derive(Clone, Debug)]
pub struct VerifiedArtifact {
    executable: PathBuf,
    artifact: PathBuf,
    executable_sha256: String,
    artifact_sha256: String,
}
impl VerifiedArtifact {
    pub fn verify(
        executable: &Path,
        artifact: &Path,
        executable_sha256: &str,
        artifact_sha256: &str,
    ) -> io::Result<Self> {
        for digest in [executable_sha256, artifact_sha256] {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
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
    fn revalidate(&self) -> io::Result<()> {
        if file_hash(&self.executable)? != self.executable_sha256
            || file_hash(&self.artifact)? != self.artifact_sha256
        {
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
#[derive(Debug)]
pub struct LaunchIdentity {
    state_dir: PathBuf,
    ownership_path: PathBuf,
    socket_path: PathBuf,
    artifact: VerifiedArtifact,
}
impl LaunchIdentity {
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }
    pub fn ownership_path(&self) -> &Path {
        &self.ownership_path
    }
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
    pub fn artifact(&self) -> &VerifiedArtifact {
        &self.artifact
    }
}
#[derive(Clone, Copy)]
enum Wake {
    Hint,
    Closed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DiagnosticCode {
    OutputDiscarded,
    ReadinessHint,
    PipeClosed,
    ReaderFailed,
}
struct DiagnosticDrain {
    cancel: UnixStream,
    thread: Option<JoinHandle<()>>,
}
impl DiagnosticDrain {
    fn stop(&mut self) -> bool {
        let _ = self.cancel.shutdown(std::net::Shutdown::Both);
        self.thread
            .take()
            .is_some_and(|thread| thread.join().is_err())
    }
}
impl Drop for DiagnosticDrain {
    fn drop(&mut self) {
        self.stop();
    }
}
/// No query or native mission is authorized by this handle. The private adapter must
/// authenticate the endpoint and consume active dispatch authority before requesting work.
pub struct OwnedHost {
    owner: OwnerGrant,
    identity: LaunchIdentity,
    child: Child,
    wake: mpsc::Receiver<Wake>,
    tail: Arc<Mutex<VecDeque<DiagnosticCode>>>,
    drains: Vec<DiagnosticDrain>,
    lock_identity: (u64, u64),
}
impl OwnedHost {
    /// The builder belongs to the concrete private adapter and validates its bootstrap
    /// semantics against this issued grant and verified manifest. It must perform no I/O.
    pub fn launch(
        owner: OwnerGrant,
        artifact: VerifiedArtifact,
        state_dir: &Path,
        environment: &BTreeMap<String, String>,
        build: impl FnOnce(&OwnerGrant, &LaunchIdentity) -> io::Result<Vec<u8>>,
    ) -> io::Result<Self> {
        private_directory(state_dir)?;
        let state_dir = state_dir.canonicalize()?;
        let identity = LaunchIdentity {
            ownership_path: state_dir.join("host.lock"),
            socket_path: state_dir.join("s"),
            state_dir,
            artifact,
        };
        use std::os::unix::ffi::OsStrExt;
        if identity.socket_path.as_os_str().as_bytes().len() > 103 {
            return Err(refused("owned Unix endpoint path is too long"));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&identity.ownership_path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(refused("private lock file refused"));
        }
        file.try_lock()
            .map_err(|error| io::Error::other(format!("owned host lock unavailable: {error}")))?;
        let metadata = file.metadata()?;
        let named = fs::symlink_metadata(&identity.ownership_path)?;
        if named.is_symlink() || (named.dev(), named.ino()) != (metadata.dev(), metadata.ino()) {
            return Err(refused("owned lock inode changed before handoff"));
        }
        if metadata.len() != 0 {
            return Err(refused("retained host bootstrap requires reconciliation"));
        }
        let rollback = file.try_clone()?;
        let prepared = (|| -> io::Result<Self> {
            let bootstrap = build(&owner, &identity)?;
            if bootstrap.is_empty() || bootstrap.len() > BOOTSTRAP_LIMIT {
                return Err(refused("private bootstrap capacity"));
            }
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&bootstrap)?;
            file.sync_all()?;
            file.seek(SeekFrom::Start(0))?;
            identity.artifact.revalidate()?;
            let lock_identity = (metadata.dev(), metadata.ino());
            let (sender, wake) = mpsc::sync_channel(CONNECT_LIMIT);
            let tail = Arc::new(Mutex::new(VecDeque::with_capacity(TAIL_LIMIT)));
            let (stdout_sender, stdout_drain) = prepare_drain(Some(sender), tail.clone())?;
            let (stderr_sender, stderr_drain) = match prepare_drain(None, tail.clone()) {
                Ok(value) => value,
                Err(error) => {
                    drop(stdout_sender);
                    drop(stdout_drain);
                    return Err(error);
                }
            };
            let spawned = {
                let mut command = Command::new(identity.artifact.executable());
                command
                    .arg(identity.artifact.artifact())
                    .current_dir(identity.state_dir())
                    .env_clear()
                    .envs(environment)
                    .stdin(Stdio::from(file))
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .process_group(0);
                command.spawn()
            };
            let mut child = match spawned {
                Ok(child) => child,
                Err(error) => {
                    drop(stdout_sender);
                    drop(stderr_sender);
                    drop(stdout_drain);
                    drop(stderr_drain);
                    return Err(error);
                }
            };
            // All fallible reader-thread creation finished before spawn. Command's source
            // descriptor dropped without unlock; the child retains the inherited lock.
            let stdout = child.stdout.take().expect("piped child stdout");
            let stderr = child.stderr.take().expect("piped child stderr");
            stdout_sender
                .send(Box::new(stdout))
                .expect("prepared stdout reader");
            stderr_sender
                .send(Box::new(stderr))
                .expect("prepared stderr reader");
            let drains = vec![stdout_drain, stderr_drain];
            Ok(Self {
                owner,
                identity,
                child,
                wake,
                tail,
                drains,
                lock_identity,
            })
        })();
        if prepared.is_err() {
            rollback
                .set_len(0)
                .and_then(|_| rollback.sync_all())
                .map_err(|_| io::Error::other("fresh host bootstrap cleanup unresolved"))?;
        }
        drop(rollback);
        prepared
    }
    pub fn owner(&self) -> &OwnerGrant {
        &self.owner
    }
    pub fn identity(&self) -> &LaunchIdentity {
        &self.identity
    }
    pub fn replace_owner(&mut self, owner: OwnerGrant) -> io::Result<()> {
        if owner.scope() != self.owner.scope()
            || owner.execution() != self.owner.execution()
            || owner.binding() != self.owner.binding()
            || owner.generation() <= self.owner.generation()
        {
            return Err(refused("owned host generation mismatch"));
        }
        self.owner = owner;
        Ok(())
    }
    /// Returns an unauthenticated connection. Hints, file ownership and Hello fields
    /// never replace the concrete adapter's full expected-transcript mutual proof.
    /// An error retains this host; elapsed time never proves that its work ended.
    pub fn wait_for_socket(&mut self, deadline: Instant) -> io::Result<UnixStream> {
        let mut attempts = AttemptBudget::new();
        match self.connect(deadline) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.kind() == io::ErrorKind::ConnectionRefused
                    || error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    format!("owned host exited before authentication: {status}"),
                ));
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned startup observation unresolved",
                    )
                })?;
            match self.wake.recv_timeout(remaining) {
                Ok(Wake::Hint) if attempts.on_hint() => match self.connect(deadline) {
                    Ok(stream) => return Ok(stream),
                    Err(error)
                        if error.kind() == io::ErrorKind::NotFound
                            || error.kind() == io::ErrorKind::ConnectionRefused
                            || error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error),
                },
                Ok(Wake::Hint) => {
                    return Err(io::Error::other(
                        "owned readiness attempt budget exhausted; host retained",
                    ));
                }
                Ok(Wake::Closed) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "owned diagnostic pipe closed before authentication; host retained",
                    ));
                }
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned startup observation unresolved; host retained",
                    ));
                }
            }
        }
    }
    fn connect(&self, deadline: Instant) -> io::Result<UnixStream> {
        connect_identity(&self.identity, self.lock_identity, deadline)
    }
    pub fn observed_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
    /// Requests graceful shutdown of this unreaped child. This is fixture cleanup, not
    /// current-owner control after adoption; production control requires authenticated fencing.
    /// Only actual exit can be observed by wait.
    pub fn request_stop(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        match rustix::process::kill_process(
            rustix::process::Pid::from_child(&self.child),
            rustix::process::Signal::TERM,
        ) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn diagnostic_tail(&self) -> Vec<DiagnosticCode> {
        self.tail
            .lock()
            .expect("diagnostic tail poisoned")
            .iter()
            .copied()
            .collect()
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        for drain in &mut self.drains {
            if drain.stop() {
                push_code(&self.tail, DiagnosticCode::ReaderFailed);
            }
        }
        Ok(status)
    }
}
/// An existing endpoint's retained private material, without an invented child handle.
/// It conveys no authenticated peer state until the concrete adapter verifies mutual proof.
pub struct AdoptedHost {
    owner: OwnerGrant,
    identity: LaunchIdentity,
    lock_identity: (u64, u64),
}
impl AdoptedHost {
    pub fn adopt<T>(
        owner: OwnerGrant,
        artifact: VerifiedArtifact,
        state_dir: &Path,
        inspect: impl FnOnce(&OwnerGrant, &LaunchIdentity, &[u8]) -> io::Result<T>,
    ) -> io::Result<(Self, T)> {
        private_directory(state_dir)?;
        let identity = LaunchIdentity {
            state_dir: state_dir.to_path_buf(),
            ownership_path: state_dir.join("host.lock"),
            socket_path: state_dir.join("s"),
            artifact,
        };
        identity.artifact.revalidate()?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(identity.ownership_path())?;
        let before = file.metadata()?;
        if !before.is_file()
            || before.nlink() != 1
            || before.uid() != rustix::process::geteuid().as_raw()
            || before.mode() & 0o777 != 0o600
            || before.len() == 0
            || before.len() > BOOTSTRAP_LIMIT as u64
        {
            return Err(refused("retained private host material refused"));
        }
        let mut bytes = vec![
            0;
            usize::try_from(before.len())
                .map_err(|_| refused("retained material capacity"))?
        ];
        file.read_exact(&mut bytes)?;
        let after = file.metadata()?;
        let named = fs::symlink_metadata(identity.ownership_path())?;
        if (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        ) || named.is_symlink()
            || (named.dev(), named.ino()) != (before.dev(), before.ino())
        {
            return Err(refused("retained private host material changed"));
        }
        let value = inspect(&owner, &identity, &bytes)?;
        let lock_identity = (before.dev(), before.ino());
        Ok((
            Self {
                owner,
                identity,
                lock_identity,
            },
            value,
        ))
    }
    pub fn owner(&self) -> &OwnerGrant {
        &self.owner
    }
    pub fn identity(&self) -> &LaunchIdentity {
        &self.identity
    }
    pub fn replace_owner(&mut self, owner: OwnerGrant) -> io::Result<()> {
        if owner.scope() != self.owner.scope()
            || owner.execution() != self.owner.execution()
            || owner.binding() != self.owner.binding()
            || owner.generation() <= self.owner.generation()
        {
            return Err(refused("adopted host generation mismatch"));
        }
        self.owner = owner;
        Ok(())
    }
    /// Does not acquire the launch lock, rotate material, spawn or authenticate the peer.
    pub fn connect(&self, deadline: Instant) -> io::Result<UnixStream> {
        connect_identity(&self.identity, self.lock_identity, deadline)
    }
}
fn connect_identity(
    identity: &LaunchIdentity,
    expected_lock: (u64, u64),
    deadline: Instant,
) -> io::Result<UnixStream> {
    private_directory(identity.state_dir())?;
    let lock = fs::symlink_metadata(identity.ownership_path())?;
    if !lock.is_file()
        || lock.is_symlink()
        || lock.nlink() != 1
        || lock.uid() != rustix::process::geteuid().as_raw()
        || lock.mode() & 0o777 != 0o600
        || (lock.dev(), lock.ino()) != expected_lock
    {
        return Err(refused("owned lock inode or permissions changed"));
    }
    let socket = fs::symlink_metadata(identity.socket_path())?;
    if !socket.file_type().is_socket()
        || socket.uid() != rustix::process::geteuid().as_raw()
        || socket.mode() & 0o777 != 0o600
    {
        return Err(refused("private Unix endpoint refused"));
    }
    connect_deadline(identity.socket_path(), deadline)
}
pub(super) fn connect_socket() -> io::Result<std::os::fd::OwnedFd> {
    use rustix::net::{AddressFamily, SocketType, socket};
    let socket = socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
    rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC)?;
    rustix::io::ioctl_fionbio(&socket, true)?;
    Ok(socket)
}
pub(super) fn connect_deadline(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::net::sockopt::socket_error;
    use rustix::net::{SocketAddrUnix, connect, getpeername};
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let socket = connect_socket()?;
    let address = SocketAddrUnix::new(path)?;
    match connect(&socket, &address) {
        Ok(()) => {}
        Err(rustix::io::Errno::INPROGRESS | rustix::io::Errno::ALREADY) => loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(io::ErrorKind::TimedOut)?;
            let timeout = Timespec::try_from(remaining).map_err(|_| io::ErrorKind::InvalidInput)?;
            let mut fds = [PollFd::new(&socket, PollFlags::OUT)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) => return Err(io::ErrorKind::TimedOut.into()),
                Ok(_) => {
                    socket_error(&socket)??;
                    break;
                }
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
        },
        Err(error) => return Err(error.into()),
    }
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    getpeername(&socket)?.ok_or(io::ErrorKind::NotConnected)?;
    let stream = UnixStream::from(socket);
    stream.set_nonblocking(false)?;
    Ok(stream)
}
impl Drop for OwnedHost {
    fn drop(&mut self) {
        for drain in &mut self.drains {
            drain.stop();
        }
    }
}
fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn private_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.is_symlink()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o700
        || path.canonicalize()? != path
    {
        return Err(refused("private canonical host directory refused"));
    }
    Ok(())
}
fn file_hash(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
struct WakeDecoder {
    line: [u8; LINE_LIMIT],
    length: usize,
    overflow: bool,
}
impl WakeDecoder {
    fn new() -> Self {
        Self {
            line: [0; LINE_LIMIT],
            length: 0,
            overflow: false,
        }
    }
    fn feed(&mut self, bytes: &[u8]) -> usize {
        let mut hints = 0;
        for byte in bytes {
            if *byte == b'\n' {
                if !self.overflow && &self.line[..self.length] == HINT {
                    hints += 1;
                }
                self.length = 0;
                self.overflow = false;
            } else if self.length < LINE_LIMIT && !self.overflow {
                self.line[self.length] = *byte;
                self.length += 1;
            } else {
                self.overflow = true;
            }
        }
        hints
    }
}
trait DiagnosticReader: Read + AsFd {}
impl<T: Read + AsFd> DiagnosticReader for T {}
type Reader = Box<dyn DiagnosticReader + Send>;
fn prepare_drain(
    sender: Option<mpsc::SyncSender<Wake>>,
    tail: Arc<Mutex<VecDeque<DiagnosticCode>>>,
) -> io::Result<(mpsc::SyncSender<Reader>, DiagnosticDrain)> {
    let (reader_sender, reader_receiver) = mpsc::sync_channel::<Reader>(1);
    let (cancel, stop) = UnixStream::pair()?;
    let handle = thread::Builder::new()
        .name("owned-host-diagnostics".into())
        .spawn(move || {
            let Ok(mut reader) = reader_receiver.recv() else {
                return;
            };

            let mut decoder = WakeDecoder::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let cancelled = {
                    use rustix::event::{PollFd, PollFlags, poll};
                    let mut fds = [
                        PollFd::from_borrowed_fd(reader.as_fd(), PollFlags::IN),
                        PollFd::new(&stop, PollFlags::IN),
                    ];
                    let outcome = loop {
                        match poll(&mut fds, None) {
                            Err(rustix::io::Errno::INTR) => continue,
                            value => break value,
                        }
                    };
                    outcome.is_err() || !fds[1].revents().is_empty()
                };
                if cancelled {
                    break;
                }
                let count = match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        push_code(&tail, DiagnosticCode::ReaderFailed);
                        break;
                    }
                    Ok(count) => count,
                };
                push_code(&tail, DiagnosticCode::OutputDiscarded);
                if let Some(sender) = &sender {
                    for _ in 0..decoder.feed(&buffer[..count]) {
                        push_code(&tail, DiagnosticCode::ReadinessHint);
                        let _ = sender.try_send(Wake::Hint);
                    }
                }
            }
            push_code(&tail, DiagnosticCode::PipeClosed);
            if let Some(sender) = sender {
                let _ = sender.try_send(Wake::Closed);
            }
        })?;
    Ok((
        reader_sender,
        DiagnosticDrain {
            cancel,
            thread: Some(handle),
        },
    ))
}
fn push_code(tail: &Arc<Mutex<VecDeque<DiagnosticCode>>>, code: DiagnosticCode) {
    let mut tail = tail.lock().expect("diagnostic code tail poisoned");
    if tail.len() == TAIL_LIMIT {
        tail.pop_front();
    }
    tail.push_back(code);
}
struct AttemptBudget {
    used: usize,
}
impl AttemptBudget {
    fn new() -> Self {
        Self { used: 1 }
    }
    fn on_hint(&mut self) -> bool {
        if self.used == CONNECT_LIMIT {
            return false;
        }
        self.used += 1;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_hint_requires_exact_complete_line_and_oversized_suffix_never_matches() {
        let mut decoder = WakeDecoder::new();
        assert_eq!(decoder.feed(&HINT[..10]), 0);
        assert_eq!(decoder.feed(&HINT[10..]), 0);
        assert_eq!(decoder.feed(b"\n"), 1);
        assert_eq!(decoder.feed(&vec![b'x'; LINE_LIMIT + 100]), 0);
        assert_eq!(decoder.feed(HINT), 0);
        assert_eq!(decoder.feed(b"\n"), 0);
        assert_eq!(decoder.feed(HINT), 0);
        assert_eq!(decoder.feed(b"\n"), 1);
    }
    #[test]
    fn initial_connect_counts_toward_exact_eight_attempt_budget() {
        let mut budget = AttemptBudget::new();
        for _ in 1..CONNECT_LIMIT {
            assert!(budget.on_hint());
        }
        assert!(!budget.on_hint());
        assert!(!budget.on_hint());
        assert_eq!(budget.used, 8);
    }
}
