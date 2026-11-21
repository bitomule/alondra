//! The hosts the engine's tests start: the pinned Node host with the official SDK and a fake native
//! Claude, in a folder each test owns and a reaper empties when the test ends, however it ends.
use crate::sdk_backend::{self as backend, SdkBackend};
use crate::test_support::Directory;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub fn runfile(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} names a runfile")));
    std::fs::canonicalize(path).unwrap()
}

/// Reads until the test process that started it closes the pipe. It leaves alone only after `done`:
/// a dying process closes its files before it stops being this one's parent, so nothing else tells
/// a test that ended from one that was cut. Then it kills every process still holding a file under
/// the folder, a host with its whole group, waits for each to exit, and removes the folder.
const REAPER: &str = r#"
import os, select, shutil, signal, subprocess, sys
root, owner = sys.argv[1], int(sys.argv[2])
if sys.stdin.buffer.read() == b'done':
    sys.exit(0)
listed = subprocess.run(['/usr/sbin/lsof', '-t', '+D', root], capture_output=True, text=True).stdout.split()
exits, watched = select.kqueue(), 0
for pid in {int(pid) for pid in listed} - {owner}:
    try:
        leads = os.getpgid(pid) == pid
    except ProcessLookupError:
        continue
    members = subprocess.run(['/usr/bin/pgrep', '-g', str(pid)], capture_output=True, text=True).stdout.split() if leads else [pid]
    for member in members:
        try:
            exits.control([select.kevent(int(member), select.KQ_FILTER_PROC, select.KQ_EV_ADD | select.KQ_EV_ONESHOT, select.KQ_NOTE_EXIT)], 0)
            watched += 1
        except ProcessLookupError:
            pass
    try:
        (os.killpg if leads else os.kill)(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
while watched:
    watched -= len(exits.control(None, watched))
shutil.rmtree(os.path.dirname(root), ignore_errors=True)
"#;

/// The folder of a test's SDK hosts: the test owns every host started under it. Dropping it kills
/// each host left with its whole process group, and fails the test that left one running; if the
/// test process dies without unwinding (a timeout, a kill), its reaper kills them instead.
pub struct HostRoot {
    reaper: std::process::Child,
    directory: Directory,
}

impl HostRoot {
    pub fn new() -> Self {
        use std::os::unix::process::CommandExt;
        let directory = Directory::for_unix_socket();
        let reaper = std::process::Command::new("/usr/bin/python3")
            .args(["-I", "-c", REAPER])
            .arg(&*directory)
            .arg(std::process::id().to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("start the hosts' reaper");
        Self { reaper, directory }
    }

    /// Kills the process group of every host still running here and waits for each to end: the
    /// hosts it killed.
    ///
    /// The tests of a binary run side by side in one process, and a child it forks to start a host
    /// of another test holds every descriptor of this process, the lock of a host here included,
    /// until it runs its own program. That child is not a host of this folder: once it has run its
    /// program, only a process that opened the lock itself still holds it.
    pub fn kill_hosts(&self) -> Vec<String> {
        use std::os::unix::fs::MetadataExt;
        let Ok(folders) = std::fs::read_dir(self.directory.join("h")) else {
            return Vec::new();
        };
        let me = std::process::id() as i32;
        let program = executable(me);
        let mut killed = Vec::new();
        for folder in folders {
            let folder = folder.unwrap().path();
            let Ok(lock) = std::fs::metadata(folder.join("host.lock")) else {
                continue;
            };
            let holds = |pid: i32| alondra_process_group::holders(lock.dev(), lock.ino()).unwrap().iter().any(|holder| holder.pid == pid);
            for holder in alondra_process_group::holders(lock.dev(), lock.ino()).unwrap() {
                if holder.pid == me || holder.zombie || alondra_process_group::group(holder.pid).unwrap() != Some(holder.pid) {
                    continue;
                }
                if (program.is_some() && executable(holder.pid) == program) || !holds(holder.pid) {
                    continue;
                }
                let held = holding(holder.pid, &folder.join("host.lock"));
                let mut exits = alondra_process_group::Exits::new().unwrap();
                for member in alondra_process_group::members(holder.pid).unwrap().iter().filter(|member| !member.zombie) {
                    exits.watch(member).unwrap();
                }
                assert_eq!(unsafe { libc::killpg(holder.pid, libc::SIGKILL) }, 0, "kill the group of {holder}");
                exits.wait().unwrap();
                killed.push(format!("{} ({holder}; {held})", folder.file_name().unwrap().to_string_lossy()));
            }
        }
        killed
    }
}

/// The program `pid` runs, which is still this process's own while a child it forked has not run
/// another.
fn executable(pid: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer holds exactly the bytes it says and outlives the call, which writes at
    // most that many and returns how many it wrote.
    let written = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    let written = usize::try_from(written).ok().filter(|written| *written > 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&path[..written])))
}

/// Who `pid` is and how it holds `lock`, so a host left running says which process it was: its
/// command, the process that started it, and the descriptors that keep the lock open.
fn holding(pid: i32, lock: &Path) -> String {
    let table = alondra_process_group::table().unwrap_or_default();
    let command = |pid: i32| table.iter().find(|entry| entry.pid == pid).map_or_else(|| "?".to_owned(), |entry| entry.argv.join(" ").chars().take(200).collect());
    let parent = table.iter().find(|entry| entry.pid == pid).map(|entry| entry.parent);
    let descriptors = std::process::Command::new("/usr/sbin/lsof").args(["-nP", "-a", "-F", "fn", "-p", &pid.to_string()]).arg("--").arg(lock).output()
        .map(|output| String::from_utf8_lossy(&output.stdout).lines().filter_map(|line| line.strip_prefix('f')).collect::<Vec<_>>().join(","))
        .unwrap_or_default();
    format!("command: {}; parent: {}; holds host.lock as descriptor {}",
        command(pid), parent.map_or_else(|| "?".to_owned(), |parent| format!("{parent} {}", command(parent))), if descriptors.is_empty() { "?" } else { &descriptors })
}

impl std::ops::Deref for HostRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.directory
    }
}

impl AsRef<Path> for HostRoot {
    fn as_ref(&self) -> &Path {
        &self.directory
    }
}

impl HostRoot {
    fn end_reaper(&mut self) {
        use std::io::Write;
        if let Some(mut pipe) = self.reaper.stdin.take() {
            let _ = pipe.write_all(b"done");
        }
        self.reaper.wait().unwrap();
    }

    /// What a dying test process does first: its pipe closes while it is still the reaper's parent.
    pub fn close_reaper_pipe(&mut self) {
        drop(self.reaper.stdin.take());
        self.reaper.wait().unwrap();
    }
}

impl Drop for HostRoot {
    fn drop(&mut self) {
        let left = self.kill_hosts();
        self.end_reaper();
        if !left.is_empty() && !std::thread::panicking() {
            panic!("the test left SDK hosts running: {}", left.join(", "));
        }
    }
}

/// The pinned Node host with the official SDK and a fake native Claude that records what it is
/// launched with in the returned capture, its hosts' folders under `state`.
pub fn fixture_backend(home: &Path, state: &HostRoot) -> (SdkBackend, PathBuf) {
    let native = home.join("native");
    std::fs::copy(runfile("FIXTURE_NATIVE"), &native).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let capture = home.join("native-capture.json");
    let environment = crate::test_support::fixture_environment(home);
    let mut native_environment = environment.clone();
    native_environment.insert("FIXTURE_CAPTURE".into(), capture.to_str().unwrap().into());
    let (node, host) = (runfile("FIXTURE_NODE"), runfile("FIXTURE_HOST"));
    let backend = SdkBackend {
        node_sha256: backend::digest(&node).unwrap(),
        host_sha256: backend::digest(&host).unwrap(),
        native_sha256: backend::digest(&native).unwrap(),
        node,
        host,
        native,
        host_environment: environment,
        native_environment,
        confinement: None,
        state_root: state.join("h"),
        io_timeout_seconds: 30,
        installed_native: false, release: crate::sdk_backend::this_release(),
            provider: alondra_core::accounts::Provider::Claude, origin: None,
    };
    (backend, capture)
}


#[cfg(test)]
mod tests {
    use super::*;

    // 21 November: a test that ended while another test of its binary started a host took the child
    // forked for that host, which held the lock of a host of the first test until it ran its own
    // program, for a host it left running: it killed it, and the other test waited for a host that
    // was gone. A child of this process that still runs this program is no host of a test's folder.
    #[test]
    fn a_child_forked_for_another_tests_host_is_no_host_of_this_folder() {
        let root = HostRoot::new();
        let folder = root.join("h").join("sdk-left");
        std::fs::create_dir_all(&folder).unwrap();
        let lock = std::fs::File::create(folder.join("host.lock")).unwrap();
        let (mut ready, written) = std::os::unix::net::UnixStream::pair().unwrap();
        // SAFETY: the child only calls functions that are safe after a fork in a process with
        // threads: it leads a group of its own, says so, and waits for the signal that ends it.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork: {}", std::io::Error::last_os_error());
        if child == 0 {
            unsafe {
                libc::setpgid(0, 0);
                libc::write(std::os::fd::AsRawFd::as_raw_fd(&written), b"x".as_ptr().cast(), 1);
                loop {
                    libc::pause();
                }
            }
        }
        drop(written);
        let mut byte = [0u8; 1];
        std::io::Read::read_exact(&mut ready, &mut byte).unwrap();
        let killed = root.kill_hosts();
        unsafe { libc::kill(child, libc::SIGKILL) };
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        drop(lock);
        assert_eq!(killed, Vec::<String>::new(), "the fork holds the lock only as a copy of this process");
    }
}
