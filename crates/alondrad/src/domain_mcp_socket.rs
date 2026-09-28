//! A private, process-bound handoff to the orchestrator MCP server. The Claude
//! process receives only a public protocol marker; the bearer stays in the
//! daemon until a directly descended MCP process proves its kernel identity.
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::domain_runtime::DomainRuntime;

const SOCKET: &str = "domain-mcp.sock";
const MAX_ANCESTORS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    parent: u32,
    uid: u32,
    birth_seconds: u64,
    birth_micros: u64,
}

#[cfg(target_os = "macos")]
fn process_identity(pid: u32) -> Result<ProcessIdentity> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let read = unsafe {
        libc::proc_pidinfo(pid as i32, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size)
    };
    ensure!(read == size, "process identity unavailable");
    let info = unsafe { info.assume_init() };
    ensure!(info.pbi_pid == pid, "process identity changed");
    Ok(ProcessIdentity {
        pid, parent: info.pbi_ppid, uid: info.pbi_uid,
        birth_seconds: info.pbi_start_tvsec,
        birth_micros: info.pbi_start_tvusec,
    })
}

#[cfg(not(target_os = "macos"))]
fn process_identity(_pid: u32) -> Result<ProcessIdentity> {
    bail!("process-bound MCP credentials require macOS")
}

#[cfg(target_os = "macos")]
fn peer_pid(stream: &UnixStream) -> Result<u32> {
    let mut pid: libc::pid_t = 0;
    let mut size = std::mem::size_of_val(&pid) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(), &mut size)
    };
    ensure!(result == 0 && size as usize == std::mem::size_of_val(&pid) && pid > 0,
        "MCP peer PID unavailable");
    Ok(pid as u32)
}

#[cfg(not(target_os = "macos"))]
fn peer_pid(_stream: &UnixStream) -> Result<u32> { bail!("MCP peer PID requires macOS") }

// The bearer is never Debug, serialized, persisted, or copied into Claude's
// environment. A single execution can register one Claude parent at a time.
struct Lease {
    claude: ProcessIdentity,
    generation: u64,
    bearer: String,
}

#[derive(Default)]
pub(crate) struct Leases(Mutex<BTreeMap<String, Lease>>);

impl Leases {
    pub(crate) fn register(&self, execution: &str, generation: u64, claude_pid: u32, bearer: &str) -> Result<()> {
        ensure!(!execution.is_empty() && bearer.len() == 64, "invalid MCP registration");
        let claude = process_identity(claude_pid)?;
        ensure!(claude.uid == unsafe { libc::geteuid() }, "Claude owner differs from daemon");
        let mut leases = self.0.lock().expect("MCP leases poisoned");
        ensure!(!leases.contains_key(execution), "duplicate MCP execution");
        leases.insert(execution.to_owned(), Lease { claude, generation, bearer: bearer.to_owned() });
        Ok(())
    }

    pub(crate) fn revoke(&self, execution: &str) {
        self.0.lock().expect("MCP leases poisoned").remove(execution);
    }

    fn bearer_for(&self, peer: u32, generation: u64) -> Result<String> {
        let leases = self.0.lock().expect("MCP leases poisoned");
        for lease in leases.values() {
            if lease.generation == generation && descended_from(peer, lease.claude, process_identity)? {
                return Ok(lease.bearer.clone());
            }
        }
        bail!("MCP process is outside the active orchestrator execution")
    }
}

fn descended_from(
    peer: u32,
    ancestor: ProcessIdentity,
    mut lookup: impl FnMut(u32) -> Result<ProcessIdentity>,
) -> Result<bool> {
    let mut pid = peer;
    let mut child_birth: Option<(u64, u64)> = None;
    for depth in 0..=MAX_ANCESTORS {
        let current = lookup(pid)?;
        if current.uid != ancestor.uid { return Ok(false); }
        let birth = (current.birth_seconds, current.birth_micros);
        if child_birth.is_some_and(|later| birth > later) { return Ok(false); }
        if current.pid == ancestor.pid {
            return Ok(depth > 0 && current == ancestor);
        }
        if current.parent == 0 || current.parent == pid { return Ok(false); }
        // A descendant cannot predate its parent; this also catches some
        // recycled PIDs before the registered parent is reached.
        child_birth = Some(birth);
        pid = current.parent;
    }
    Ok(false)
}

pub(crate) async fn bind(home: &Path) -> Result<UnixListener> {
    let path = home.join(SOCKET);
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        ensure!(meta.file_type().is_socket(), "MCP path is not a socket");
        match std::os::unix::net::UnixStream::connect(&path) {
            Ok(_) => bail!("another daemon owns the MCP socket"),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => (),
            Err(error) => return Err(error).context("checking MCP socket"),
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub(crate) async fn serve(listener: UnixListener, domain: Arc<DomainRuntime>, leases: Arc<Leases>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let domain = domain.clone();
                let leases = leases.clone();
                tokio::spawn(async move { let _ = handle(stream, domain, leases).await; });
            }
            Err(error) => log!("MCP socket accept failed: {error}"),
        }
    }
}

async fn handle(mut stream: UnixStream, domain: Arc<DomainRuntime>, leases: Arc<Leases>) -> Result<()> {
    let peer = peer_pid(&stream)?;
    let mut version = [0u8; 1];
    tokio::time::timeout(std::time::Duration::from_secs(2), stream.read_exact(&mut version)).await??;
    ensure!(version == [2], "unsupported MCP handoff protocol");
    let generation = domain.state()?.orchestrator_generation;
    let bearer = leases.bearer_for(peer, generation)?;
    stream.write_all(bearer.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    #[test]
    fn kernel_reports_process_birth_for_a_live_child() {
        let mut child = std::process::Command::new("/bin/sleep").arg("1").spawn().unwrap();
        let identity = process_identity(child.id()).unwrap();
        assert_eq!(identity.pid, child.id());
        assert_eq!(identity.parent, std::process::id());
        let _ = child.kill();
        let _ = child.wait();
    }
    #[test]
    fn socket_child_helper() {
        let Some(path) = std::env::var_os("ALONDRA_MCP_TEST_SOCKET") else { return; };
        use std::io::{Read, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        stream.write_all(&[2]).unwrap();
        let mut decision = [0u8; 1];
        stream.read_exact(&mut decision).unwrap();
        let expected = std::env::var("ALONDRA_MCP_TEST_EXPECT").unwrap();
        assert_eq!(decision[0], if expected == "allow" { 1 } else { 0 });
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn socket_rejects_same_uid_sibling_and_accepts_registered_child() {
        let home = std::env::temp_dir().join(format!("alondra-mcp-{}", crate::auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let listener = bind(&home).await.unwrap();
        let leases = Leases::default();
        let mut unrelated = std::process::Command::new("/bin/sleep").arg("5").spawn().unwrap();
        leases.register("unrelated", 7, unrelated.id(), &"a".repeat(64)).unwrap();
        for expected in ["deny", "allow"] {
            if expected == "allow" {
                leases.revoke("unrelated");
                leases.register("parent", 7, std::process::id(), &"b".repeat(64)).unwrap();
            }
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "domain_mcp_socket::tests::socket_child_helper"])
                .env("ALONDRA_MCP_TEST_SOCKET", home.join(SOCKET))
                .env("ALONDRA_MCP_TEST_EXPECT", expected)
                .spawn().unwrap();
            let (mut stream, _) = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await.unwrap().unwrap();
            let mut version = [0u8; 1];
            stream.read_exact(&mut version).await.unwrap();
            assert_eq!(version, [2]);
            let peer = peer_pid(&stream).unwrap();
            assert_eq!(peer, child.id());
            let result = leases.bearer_for(peer, 7);
            assert_eq!(result.is_ok(), expected == "allow");
            stream.write_all(&[if result.is_ok() { 1 } else { 0 }]).await.unwrap();
            assert!(child.wait().unwrap().success());
        }
        let _ = unrelated.kill();
        let _ = unrelated.wait();
        std::fs::remove_file(home.join(SOCKET)).unwrap();
        std::fs::remove_dir(home).unwrap();
    }
    #[test]
    fn only_descendant_with_exact_parent_birth_is_accepted() {
        let parent = ProcessIdentity { pid: 10, parent: 1, uid: 501, birth_seconds: 100, birth_micros: 5 };
        let child = ProcessIdentity { pid: 11, parent: 10, uid: 501, birth_seconds: 101, birth_micros: 0 };
        let sibling = ProcessIdentity { pid: 12, parent: 1, uid: 501, birth_seconds: 101, birth_micros: 1 };
        let init = ProcessIdentity { pid: 1, parent: 0, uid: 501, birth_seconds: 1, birth_micros: 0 };
        let lookup = |pid| match pid { 1 => Ok(init), 10 => Ok(parent), 11 => Ok(child), 12 => Ok(sibling), _ => bail!("unknown PID") };
        assert!(descended_from(11, parent, lookup).unwrap());
        assert!(!descended_from(12, parent, lookup).unwrap());
        assert!(!descended_from(10, parent, lookup).unwrap());
        let recycled = ProcessIdentity { birth_seconds: 102, ..parent };
        assert!(!descended_from(11, recycled, lookup).unwrap());
    }
}
