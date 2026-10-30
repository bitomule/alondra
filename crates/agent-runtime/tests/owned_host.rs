use alondra_agent_runtime::owned_host::{AdoptedHost, OwnedHost, VerifiedArtifact};
use alondra_process_group::Member;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
#[path = "support/mod.rs"]
mod support;
fn artifact() -> VerifiedArtifact {
    let path = support::fixture();
    let hash = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
    VerifiedArtifact::verify(&path, &path, &hash, &hash).unwrap()
}
fn message(path: &std::path::Path, request: &[u8]) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(request)?;
    let mut reply = String::new();
    BufReader::new(stream).read_to_string(&mut reply)?;
    Ok(reply)
}
struct Running<'a> {
    host: Option<OwnedHost>,
    directory: &'a support::Directory,
    descendant: bool,
}
impl<'a> Running<'a> {
    fn new(directory: &'a support::Directory) -> Self {
        let host = OwnedHost::launch(
            support::owner(directory, 0),
            artifact(),
            directory,
            &support::environment(directory),
            |_, _| Ok(b"private-fixture-bootstrap".to_vec()),
        )
        .unwrap();
        let mut value = Self {
            host: Some(host),
            directory,
            descendant: false,
        };
        drop(
            value
                .host
                .as_mut()
                .unwrap()
                .wait_for_socket(Instant::now() + Duration::from_secs(10))
                .unwrap(),
        );
        value
    }
    fn stop_descendant(&mut self) -> std::io::Result<()> {
        if self.descendant {
            message(&self.directory.join("d"), b"stop\n")?;
            let completion = OpenOptions::new()
                .read(true)
                .write(true)
                .open(self.directory.join("descendant.done"))?;
            completion.lock()?;
            drop(completion);
            self.descendant = false;
        }
        Ok(())
    }
}
impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = self.stop_descendant();
        if let Some(mut host) = self.host.take() {
            let _ = host.request_stop();
            let _ = host.wait();
        }
    }
}
#[test]
fn retained_bootstrap_and_live_lock_never_authorize_a_replacement_host() {
    let directory = support::directory();
    let mut running = Running::new(&directory);
    let bytes = fs::read(directory.join("host.lock")).unwrap();
    let inode = fs::metadata(directory.join("host.lock")).unwrap().ino();
    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("host.lock"))
        .unwrap();
    assert!(
        contender.try_lock().is_err(),
        "successful handoff dropped all parent copies but the actual child owns the lock"
    );
    drop(contender);
    let identity = message(&directory.join("s"), b"identity\n").unwrap();
    let fields = identity
        .trim()
        .split(':')
        .map(|v| v.parse::<i32>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        fields[0], fields[1],
        "actual host is its own process-group leader"
    );
    assert_ne!(fields[1], rustix::process::getpgrp().as_raw_pid());
    message(&directory.join("s"), b"exit\n").unwrap();
    assert!(running.host.as_mut().unwrap().wait().unwrap().success());
    let owner = support::owner(&directory, 1);
    let refused = OwnedHost::launch(
        owner,
        artifact(),
        &directory,
        &support::environment(&directory),
        |_, _| panic!("retained bytes must be refused before building a new identity"),
    );
    assert!(refused.is_err());
    assert_eq!(fs::read(directory.join("host.lock")).unwrap(), bytes);
    assert_eq!(
        fs::metadata(directory.join("host.lock")).unwrap().ino(),
        inode
    );
    let owner = support::owner(&directory, 2);
    let (adopted, retained) =
        AdoptedHost::adopt(owner, &directory, |_, _, material| {
            Ok(material.to_vec())
        })
        .unwrap();
    assert_eq!(retained, bytes);
    assert!(
        adopted
            .connect(Instant::now() + Duration::from_secs(10))
            .is_err()
    );
    assert_eq!(fs::read(directory.join("host.lock")).unwrap(), bytes);
}
#[test]
fn failed_spawn_clears_only_fresh_material_and_releases_all_lock_copies() {
    let directory = support::directory();
    let executable = directory.join("not-executable");
    fs::write(&executable, b"not executable").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o600)).unwrap();
    let hash = format!("{:x}", Sha256::digest(fs::read(&executable).unwrap()));
    let verified = VerifiedArtifact::verify(&executable, &executable, &hash, &hash).unwrap();
    let built = std::cell::Cell::new(false);
    let result = OwnedHost::launch(
        support::owner(&directory, 0),
        verified,
        &directory,
        &support::environment(&directory),
        |_, _| {
            built.set(true);
            Ok(b"private fresh material".to_vec())
        },
    );
    assert!(
        built.get(),
        "verified artifact reached bootstrap preparation"
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("nonexecutable fixture unexpectedly spawned"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    assert_eq!(fs::read(directory.join("host.lock")).unwrap(), b"");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("host.lock"))
        .unwrap();
    assert!(file.try_lock().is_ok());
    drop(file);
    assert!(!directory.join("s").exists());
}
fn identity(directory: &support::Directory) -> (i32, i32) {
    let line = message(&directory.join("s"), b"identity\n").unwrap();
    let (pid, group) = line.trim().split_once(':').unwrap();
    (pid.parse().unwrap(), group.parse().unwrap())
}
fn spawn_descendant(running: &mut Running) -> Member {
    running.descendant = true;
    let pid = message(&running.directory.join("s"), b"descendant\n")
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    assert_eq!(
        message(&running.directory.join("d"), b"ping\n").unwrap(),
        "alive:false:false\n"
    );
    let (_, group) = identity(running.directory);
    alondra_process_group::members(group)
        .unwrap()
        .into_iter()
        .find(|member| member.pid == pid)
        .expect("the descendant runs in the host's process group")
}
fn assert_ended(running: &mut Running, descendant: &Member) {
    assert!(
        !alondra_process_group::alive(descendant).unwrap(),
        "descendant-fixture {descendant} survived the leader"
    );
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(running.directory.join("descendant.done"))
        .unwrap()
        .lock()
        .unwrap();
    running.descendant = false;
    assert!(UnixStream::connect(running.directory.join("d")).is_err());
}
#[test]
fn wait_ends_what_the_host_left_in_its_group_before_reaping_it() {
    let directory = support::directory();
    let mut running = Running::new(&directory);
    let descendant = spawn_descendant(&mut running);
    message(&directory.join("s"), b"exit\n").unwrap();
    let status = running.host.as_mut().unwrap().wait().unwrap();
    assert!(status.success());
    assert_eq!(
        running.host.as_mut().unwrap().observed_exit().unwrap(),
        Some(status)
    );
    assert_ended(&mut running, &descendant);
}
#[test]
fn a_host_killed_before_its_own_cleanup_still_has_its_group_ended() {
    let directory = support::directory();
    let mut running = Running::new(&directory);
    let descendant = spawn_descendant(&mut running);
    let (leader, _) = identity(&directory);
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(leader).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    let status = running.host.as_mut().unwrap().wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert_ended(&mut running, &descendant);
}
#[test]
fn owned_stop_is_live_then_exited_idempotent_and_never_signals_after_reaping() {
    let directory = support::directory();
    let mut running = Running::new(&directory);
    assert!(
        running
            .host
            .as_mut()
            .unwrap()
            .observed_exit()
            .unwrap()
            .is_none()
    );
    running.host.as_mut().unwrap().request_stop().unwrap();
    let status = running.host.as_mut().unwrap().wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGTERM));
    running.host.as_mut().unwrap().request_stop().unwrap();
    running.host.as_mut().unwrap().request_stop().unwrap();
    assert_eq!(
        running.host.as_mut().unwrap().observed_exit().unwrap(),
        Some(status)
    );
}
struct Reaped {
    queue: std::os::fd::OwnedFd,
    pid: rustix::process::Pid,
}
impl Reaped {
    fn watch(pid: u32) -> Self {
        use std::os::fd::{AsRawFd, FromRawFd};
        // kqueue returns a fresh descriptor, owned by this fixture until its child was reaped.
        let descriptor = unsafe { libc::kqueue() };
        assert!(descriptor >= 0);
        let queue = unsafe { std::os::fd::OwnedFd::from_raw_fd(descriptor) };
        let mut change: libc::kevent = unsafe { std::mem::zeroed() };
        change.ident = pid as libc::uintptr_t;
        change.filter = libc::EVFILT_PROC;
        change.flags = libc::EV_ADD | libc::EV_ONESHOT;
        change.fflags = libc::NOTE_REAP;
        // The fixture subscribes while its own child is alive, before any stop or handle drop.
        assert_eq!(unsafe { libc::kevent(queue.as_raw_fd(), &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) }, 0);
        Self { queue, pid: rustix::process::Pid::from_raw(pid.try_into().unwrap()).unwrap() }
    }
    fn wait(self) {
        use std::os::fd::AsRawFd;
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let timeout = libc::timespec { tv_sec: 10, tv_nsec: 0 };
        // NOTE_REAP synchronizes with the runtime's wait, rather than racing its exit monitor.
        assert_eq!(unsafe { libc::kevent(self.queue.as_raw_fd(), std::ptr::null(), 0, &mut event, 1, &timeout) }, 1, "the runtime did not reap its child");
        assert_eq!(event.flags & libc::EV_ERROR, 0);
        assert_ne!(event.fflags & libc::NOTE_REAP, 0);
        assert_eq!(rustix::process::waitid(rustix::process::WaitId::Pid(self.pid), rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOWAIT | rustix::process::WaitIdOptions::NOHANG).unwrap_err(), rustix::io::Errno::CHILD);
    }
}
#[test]
fn a_dropped_host_is_reaped_and_its_group_ends_after_sigterm_or_sigkill() {
    for kill in [false, true] {
        let directory = support::directory();
        let mut running = Running::new(&directory);
        let descendant = spawn_descendant(&mut running);
        let host = running.host.take().unwrap();
        let reaped = Reaped::watch(host.process_id());
        let stopper = host.stopper();
        let process = stopper.process().unwrap().expect("the launched host is live");
        drop(host);
        assert!(alondra_process_group::alive(&process).unwrap(), "dropping the handle preserves live work");
        assert!(alondra_process_group::alive(&descendant).unwrap());
        if kill { stopper.kill().unwrap(); } else { stopper.request().unwrap(); }
        reaped.wait();
        assert_ended(&mut running, &descendant);
        assert!(stopper.process().unwrap().is_none());
        assert!(stopper.process_group().is_none());
        stopper.request().unwrap();
        stopper.kill().unwrap();
    }
}
#[test]
fn a_live_host_dropped_then_adopted_is_reaped_when_its_new_owner_stops_it() {
    let directory = support::directory();
    let mut running = Running::new(&directory);
    let host = running.host.take().unwrap();
    let reaped = Reaped::watch(host.process_id());
    let original = host.stopper();
    let process = original.process().unwrap().unwrap();
    drop(host);
    let (mut adopted, _) = AdoptedHost::adopt(support::owner(&directory, 1), &directory, |_, _, material| Ok(material.to_vec())).unwrap();
    drop(adopted.connect(Instant::now() + Duration::from_secs(10)).unwrap());
    let current = adopted.stopper();
    assert_eq!(current.process().unwrap(), Some(process));
    assert_eq!(message(&directory.join("s"), b"identity\n").unwrap(), format!("{0}:{0}\n", process.pid));
    adopted.stop().unwrap();
    reaped.wait();
    assert!(current.process().unwrap().is_none());
    assert!(original.process().unwrap().is_none());
    assert!(original.process_group().is_none());
}
struct Daemon<'a> {
    child: Child,
    directory: &'a support::Directory,
}
impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Some(mut stdin) = self.child.stdin.take() {
                let _ = stdin.write_all(b"stop\n");
            }
            let _ = self.child.wait();
        }
        let _ = message(&self.directory.join("s"), b"exit\n");
        if let Ok(lock) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.directory.join("host.lock"))
        {
            let _ = lock.lock();
        }
    }
}
struct ResumeParent(rustix::process::Pid);
impl Drop for ResumeParent {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
    }
}
#[test]
fn an_adopted_host_is_dead_while_its_stopped_parent_still_holds_it_unreaped() {
    let directory = support::directory();
    fs::write(directory.join(".alondra-process-fixture"), b"alondra-process-fixture:owned-host\n").unwrap();
    let child = Command::new(support::fixture())
        .env_clear()
        .envs(support::environment(&directory))
        .env("ALONDRA_FIXTURE_MODE", "daemon")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut daemon = Daemon { child, directory: &directory };
    let mut ready = String::new();
    BufReader::new(daemon.child.stdout.take().unwrap()).read_line(&mut ready).unwrap();
    assert_eq!(ready.trim().split(':').count(), 3);
    let (mut adopted, _) = AdoptedHost::adopt(support::owner(&directory, 1), &directory, |_, _, material| Ok(material.to_vec())).unwrap();
    drop(adopted.connect(Instant::now() + Duration::from_secs(10)).unwrap());
    let stopper = adopted.stopper();
    let host = stopper.process().unwrap().unwrap();
    let parent = rustix::process::Pid::from_child(&daemon.child);
    assert_eq!(alondra_process_group::parent(host.pid).unwrap().unwrap().pid, parent.as_raw_nonzero().get());
    let resume = ResumeParent(parent);
    rustix::process::kill_process(parent, rustix::process::Signal::STOP).unwrap();
    let stopped = rustix::process::waitid(rustix::process::WaitId::Pid(parent), rustix::process::WaitIdOptions::STOPPED).unwrap().unwrap();
    assert!(stopped.stopped(), "the fixture parent cannot reap while its threads are stopped");
    let mut exited = alondra_process_group::Exits::new().unwrap();
    exited.watch(&host).unwrap();
    stopper.kill().unwrap();
    exited.wait().unwrap();
    assert_unreaped_zombie(host.pid);
    assert!(stopper.process().unwrap().is_none());
    adopted.stop().unwrap();
    assert_unreaped_zombie(host.pid);
    drop(resume);
    drop(daemon);
    assert_eq!(rustix::process::test_kill_process(rustix::process::Pid::from_raw(host.pid).unwrap()), Err(rustix::io::Errno::SRCH), "the resumed parent reaped its own child");
}
fn assert_unreaped_zombie(pid: i32) {
    let status = Command::new("/bin/ps").env_clear().args(["-p", &pid.to_string(), "-o", "stat="]).output().unwrap();
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    assert!(String::from_utf8_lossy(&status.stdout).trim().starts_with('Z'), "the stopped parent still holds an unreaped zombie: {}", String::from_utf8_lossy(&status.stdout));
    assert!(rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok(), "kill(pid,0) sees the zombie as existing");
}
#[test]
fn forced_adopted_cleanup_ends_descendants_with_a_live_or_exited_host() {
    for kill_host_first in [false, true] {
        let directory = support::directory();
        fs::write(directory.join(".alondra-process-fixture"), b"alondra-process-fixture:owned-host\n").unwrap();
        let child = Command::new(support::fixture())
            .env_clear()
            .envs(support::environment(&directory))
            .env("ALONDRA_FIXTURE_MODE", "daemon")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut daemon = Daemon { child, directory: &directory };
        let mut ready = String::new();
        BufReader::new(daemon.child.stdout.take().unwrap()).read_line(&mut ready).unwrap();
        assert_eq!(ready.trim().split(':').count(), 3);
        let mut running = Running { host: None, directory: &directory, descendant: false };
        let descendant = spawn_descendant(&mut running);
        let (mut adopted, _) = AdoptedHost::adopt(support::owner(&directory, 1), &directory, |_, _, material| Ok(material.to_vec())).unwrap();
        drop(adopted.connect(Instant::now() + Duration::from_secs(10)).unwrap());
        let stopper = adopted.stopper();
        let host = stopper.process().unwrap().unwrap();
        let parent = rustix::process::Pid::from_child(&daemon.child);
        let resume = ResumeParent(parent);
        let resume_host = ResumeParent(rustix::process::Pid::from_raw(host.pid).unwrap());
        rustix::process::kill_process(parent, rustix::process::Signal::STOP).unwrap();
        assert!(rustix::process::waitid(rustix::process::WaitId::Pid(parent), rustix::process::WaitIdOptions::STOPPED).unwrap().unwrap().stopped());
        if kill_host_first {
            let mut exited = alondra_process_group::Exits::new().unwrap();
            exited.watch(&host).unwrap();
            stopper.kill().unwrap();
            exited.wait().unwrap();
            assert!(alondra_process_group::alive(&descendant).unwrap(), "the command outlived the host and its parent's reaper is stopped");
            assert!(adopted.stop_with_anchor(None, true).is_err(), "a dead host's numeric group is never enough to authorize cleanup");
            assert!(alondra_process_group::alive(&descendant).unwrap());
        } else {
            rustix::process::kill_process(rustix::process::Pid::from_raw(host.pid).unwrap(), rustix::process::Signal::STOP).unwrap();
        }
        adopted.stop_with_anchor(kill_host_first.then_some(descendant), true).unwrap();
        assert!(!alondra_process_group::alive(&host).unwrap());
        assert_ended(&mut running, &descendant);
        assert!(stopper.process().unwrap().is_none());
        drop(resume_host);
        drop(resume);
        drop(daemon);
    }
}
#[test]
fn actual_host_survives_only_its_owned_fake_daemon_group_death() {
    let directory = support::directory();
    fs::write(
        directory.join(".alondra-process-fixture"),
        b"alondra-process-fixture:owned-host\n",
    )
    .unwrap();
    let child = Command::new(support::fixture())
        .env_clear()
        .envs(support::environment(&directory))
        .env("ALONDRA_FIXTURE_MODE", "daemon")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut daemon = Daemon {
        child,
        directory: &directory,
    };
    let mut line = String::new();
    BufReader::new(daemon.child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let fields = line
        .trim()
        .split(':')
        .map(|v| v.parse::<i32>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), 3);
    assert_eq!(
        fields[0], fields[1],
        "host leader must be detached before any group signal"
    );
    assert_eq!(fields[2], i32::try_from(daemon.child.id()).unwrap());
    assert_ne!(fields[1], fields[2]);
    assert_ne!(fields[2], rustix::process::getpgrp().as_raw_pid());
    let bootstrap = fs::read(directory.join("host.lock")).unwrap();
    let inode = fs::metadata(directory.join("host.lock")).unwrap().ino();
    rustix::process::kill_process_group(
        rustix::process::Pid::from_child(&daemon.child),
        rustix::process::Signal::TERM,
    )
    .unwrap();
    daemon.child.wait().unwrap();
    assert_eq!(fs::read(directory.join("host.lock")).unwrap(), bootstrap);
    assert_eq!(
        fs::metadata(directory.join("host.lock")).unwrap().ino(),
        inode
    );
    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("host.lock"))
        .unwrap();
    assert!(
        contender.try_lock().is_err(),
        "surviving host retains the same kernel lock"
    );
    drop(contender);
    let (adopted, retained) = AdoptedHost::adopt(
        support::owner(&directory, 1),
        &directory,
        |_, _, material| Ok(material.to_vec()),
    )
    .unwrap();
    assert_eq!(retained, bootstrap);
    let mut stream = adopted
        .connect(Instant::now() + Duration::from_secs(10))
        .unwrap();
    stream.write_all(b"identity\n").unwrap();
    let mut adopted_identity = String::new();
    BufReader::new(stream)
        .read_to_string(&mut adopted_identity)
        .unwrap();
    assert_eq!(adopted_identity, format!("{}:{}\n", fields[0], fields[1]));
    assert_eq!(
        message(&directory.join("s"), b"identity\n").unwrap(),
        format!("{}:{}\n", fields[0], fields[1]),
        "remaining owned host survives the dead daemon's isolated process group"
    );
}
#[test]
fn only_a_lock_no_process_holds_makes_an_ended_host_and_retiring_it_frees_its_folder() {
    use alondra_agent_runtime::owned_host::{EndedHost, Retained};
    let directory = support::directory();
    let state = directory.join("host");
    let archive = directory.join("host.ended");
    assert!(matches!(EndedHost::claim(&state).unwrap(), Retained::Absent), "no folder, no host");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let mut environment = support::environment(&directory);
    environment.insert("ALONDRA_FIXTURE_STATE".into(), state.to_str().unwrap().into());
    let mut host = OwnedHost::launch(support::owner(&directory, 0), artifact(), &state, &environment, |_, _| {
        Ok(b"retained-bootstrap".to_vec())
    })
    .unwrap();
    drop(host.wait_for_socket(Instant::now() + Duration::from_secs(10)).unwrap());
    assert!(matches!(EndedHost::claim(&state).unwrap(), Retained::Held), "the live host holds its lock");
    message(&state.join("s"), b"exit\n").unwrap();
    assert!(host.wait().unwrap().success());
    let Retained::Ended(ended) = EndedHost::claim(&state).unwrap() else { panic!("the host ended, so its lock is free") };
    assert_eq!(ended.bootstrap(), b"retained-bootstrap");
    assert!(matches!(EndedHost::claim(&state).unwrap(), Retained::Held), "the claim keeps the lock until it is retired");
    ended.retire(&archive).unwrap();
    assert_eq!(fs::read(archive.join("host.lock")).unwrap(), b"retained-bootstrap", "the ended host is kept as evidence");
    assert!(matches!(EndedHost::claim(&state).unwrap(), Retained::Absent), "its path is free for the next host");
    let lock = OpenOptions::new().read(true).write(true).open(archive.join("host.lock")).unwrap();
    assert!(lock.try_lock().is_ok(), "retiring released the lock");
}
#[test]
fn a_live_host_no_owner_can_adopt_is_ended_with_its_group_by_the_lock_it_holds() {
    use alondra_agent_runtime::owned_host::{EndedHost, Retained};
    let directory = support::directory();
    fs::write(
        directory.join(".alondra-process-fixture"),
        b"alondra-process-fixture:owned-host\n",
    )
    .unwrap();
    let child = Command::new(support::fixture())
        .env_clear()
        .envs(support::environment(&directory))
        .env("ALONDRA_FIXTURE_MODE", "daemon")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut daemon = Daemon {
        child,
        directory: &directory,
    };
    let mut line = String::new();
    BufReader::new(daemon.child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    rustix::process::kill_process_group(
        rustix::process::Pid::from_child(&daemon.child),
        rustix::process::Signal::TERM,
    )
    .unwrap();
    daemon.child.wait().unwrap();
    let (leader, group) = identity(&directory);
    let host = alondra_process_group::process(leader).unwrap().expect("the host outlived its daemon");
    let mut running = Running {
        host: None,
        directory: &directory,
        descendant: false,
    };
    running.descendant = true;
    let pid = message(&directory.join("s"), b"descendant\n").unwrap().trim().parse::<i32>().unwrap();
    let descendant = alondra_process_group::members(group)
        .unwrap()
        .into_iter()
        .find(|member| member.pid == pid)
        .expect("the descendant runs in the host's process group");
    assert_eq!(group, leader, "the host leads its own group");
    fs::remove_file(directory.join("s")).unwrap();
    let (adopted, _) = AdoptedHost::adopt(support::owner(&directory, 1), &directory, |_, _, material| Ok(material.to_vec())).unwrap();
    assert!(
        adopted.connect(Instant::now() + Duration::from_secs(10)).is_err(),
        "with its owner's socket gone no owner reaches the host"
    );
    assert!(matches!(EndedHost::claim(&directory).unwrap(), Retained::Held), "yet it still holds its lock");
    let Retained::Ended(ended) = EndedHost::end_holders(&directory).unwrap() else {
        panic!("ending the holders frees the lock")
    };
    assert_eq!(ended.bootstrap(), fs::read(directory.join("host.lock")).unwrap());
    assert!(!alondra_process_group::alive(&host).unwrap(), "the host exited");
    assert_ended(&mut running, &descendant);
    assert!(
        alondra_process_group::members(group).unwrap().iter().all(|member| member.zombie),
        "nothing of its group is left"
    );
}
