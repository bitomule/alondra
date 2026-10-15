#![forbid(unsafe_code)]
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, Stdio};
#[path = "../support/mod.rs"]
mod support;
fn bind(path: &std::path::Path) -> UnixListener {
    let listener = UnixListener::bind(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    listener
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("descendant") {
        descendant();
        return;
    }
    let path = std::path::PathBuf::from(std::env::var_os("ALONDRA_FIXTURE_STATE").unwrap());
    if std::env::var("ALONDRA_FIXTURE_MODE").as_deref() == Ok("daemon") {
        daemon(&path);
        return;
    }
    let listener = bind(&path.join("s"));
    println!("Alondra host: socket listening");
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut command = String::new();
        if BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut command)
            .unwrap()
            == 0
        {
            continue;
        }
        match command.trim() {
            "identity" => {
                writeln!(
                    stream,
                    "{}:{}",
                    std::process::id(),
                    rustix::process::getpgrp().as_raw_pid()
                )
                .unwrap();
            }
            "descendant" => {
                let (mut parent, child) = UnixStream::pair().unwrap();
                let descendant = Command::new(std::env::current_exe().unwrap())
                    .arg("descendant")
                    .env_clear()
                    .env("ALONDRA_FIXTURE_STATE", &path)
                    .stdin(Stdio::from(std::os::fd::OwnedFd::from(child)))
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap();
                let mut ready = [0];
                parent.read_exact(&mut ready).unwrap();
                assert_eq!(ready, [b'R']);
                // The parent fixture owns the descendant control socket until explicit stop.
                writeln!(stream, "{}", descendant.id()).unwrap();
            }
            "exit" => return,
            _ => {
                writeln!(stream, "ok").unwrap();
            }
        }
    }
}
fn descendant() {
    let path = std::path::PathBuf::from(std::env::var_os("ALONDRA_FIXTURE_STATE").unwrap());
    let completion = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.join("descendant.done"))
        .unwrap();
    completion.lock().unwrap();
    let listener = bind(&path.join("d"));
    let mut ready = UnixStream::from(std::io::stdin().as_fd().try_clone_to_owned().unwrap());
    ready.write_all(b"R").unwrap();
    drop(ready);
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        if request.trim() == "stop" {
            stream.write_all(b"stopped\n").unwrap();
            return;
        }
        // An EPIPE after the host cancels diagnostics must not terminate this fixture work.
        let stdout_broken = writeln!(std::io::stdout(), "descendant diagnostic")
            .is_err_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe);
        let stderr_broken = writeln!(std::io::stderr(), "descendant diagnostic")
            .is_err_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe);
        writeln!(stream, "alive:{stdout_broken}:{stderr_broken}").unwrap();
    }
}
fn daemon(path: &std::path::Path) {
    use alondra_agent_runtime::owned_host::{OwnedHost, VerifiedArtifact};
    use sha2::{Digest, Sha256};
    let directory = support::Directory::for_process_fixture(path, "owned-host").unwrap();
    let executable = std::env::current_exe().unwrap();
    let hash = format!("{:x}", Sha256::digest(fs::read(&executable).unwrap()));
    let artifact = VerifiedArtifact::verify(&executable, &executable, &hash, &hash).unwrap();
    let mut host = OwnedHost::launch(
        support::owner(&directory, 0),
        artifact,
        &directory,
        &support::environment(&directory),
        |_, _| Ok(b"private-fixture-bootstrap".to_vec()),
    )
    .unwrap();
    let mut stream = host
        .wait_for_socket(std::time::Instant::now() + std::time::Duration::from_secs(10))
        .unwrap();
    stream.write_all(b"identity\n").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    println!("{}:{}", line.trim(), rustix::process::getpgrp().as_raw_pid());
    std::io::stdout().flush().unwrap();
    let mut exit = String::new();
    std::io::stdin().read_line(&mut exit).unwrap();
    host.request_stop().unwrap();
    host.wait().unwrap();
}
