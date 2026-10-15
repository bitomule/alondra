use alondra_agent_runtime::owned_host::{AdoptedHost, OwnedHost, VerifiedArtifact};
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
        AdoptedHost::adopt(owner, artifact(), &directory, |_, _, material| {
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
#[test]
fn observed_host_exit_and_wait_do_not_require_descendant_pipe_eof() {
    let directory = support::directory();
    let mut running = Running::new(&directory);
    running.descendant = true;
    let child = message(&directory.join("s"), b"descendant\n").unwrap();
    assert!(child.trim().parse::<u32>().unwrap() > 0);
    assert_eq!(
        message(&directory.join("d"), b"ping\n").unwrap(),
        "alive:false:false\n"
    );
    message(&directory.join("s"), b"exit\n").unwrap();
    let status = running.host.as_mut().unwrap().wait().unwrap();
    assert!(status.success());
    assert_eq!(
        running.host.as_mut().unwrap().observed_exit().unwrap(),
        Some(status)
    );
    assert_eq!(
        message(&directory.join("d"), b"ping\n").unwrap(),
        "alive:true:true\n",
        "both cancelled readers produced real EPIPE while fixture work remains alive"
    );
    running.stop_descendant().unwrap();
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
        artifact(),
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
