//! The launcher launchd runs must be invisible to alondrad: same argv[0], same environment, its
//! exit status and the signals launchd sends.
#[path = "../../test_support.rs"]
mod test_support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LAUNCHER: &str = env!("CARGO_BIN_EXE_alondrad-launcher");

struct Scripts(test_support::Directory);

impl Scripts {
    fn new() -> Self {
        let root = test_support::Directory::new();
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

fn wait(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("launcher did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_child_sees_its_own_path_as_argv0_the_arguments_and_the_environment() {
    let scripts = Scripts::new();
    let child = scripts.script("daemon", r#"printf '%s|%s|%s' "$0" "$*" "$XPC_SERVICE_NAME""#);
    let out = Command::new(LAUNCHER).arg(&child).args(["--flag", "value"])
        .env("XPC_SERVICE_NAME", "com.example.alondra").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{}|--flag value|com.example.alondra", child.display()));
}

#[test]
fn the_launcher_exits_with_the_childs_status_so_launchd_relaunches_on_75() {
    let scripts = Scripts::new();
    for code in [0, 3, 75] {
        let child = scripts.script(&format!("exit-{code}"), &format!("exit {code}"));
        let status = Command::new(LAUNCHER).arg(&child).status().unwrap();
        assert_eq!(status.code(), Some(code));
    }
}

#[test]
fn sigterm_to_the_launcher_reaches_the_child() {
    let scripts = Scripts::new();
    let child = scripts.script("daemon", "trap 'echo stopping; exit 7' TERM\necho ready\nwhile :; do sleep 0.05; done");
    let mut launcher = Command::new(LAUNCHER).arg(&child).stdout(Stdio::piped()).spawn().unwrap();
    let mut lines = BufReader::new(launcher.stdout.take().unwrap()).lines();
    assert_eq!(lines.next().unwrap().unwrap(), "ready");
    let killed = Command::new("kill").args(["-TERM", &launcher.id().to_string()]).status().unwrap();
    assert!(killed.success());
    assert_eq!(wait(&mut launcher).code(), Some(7));
    assert_eq!(lines.next().unwrap().unwrap(), "stopping");
}

#[test]
fn a_missing_program_fails_instead_of_looping_silently() {
    let scripts = Scripts::new();
    let out = Command::new(LAUNCHER).arg(scripts.0.join("missing")).output().unwrap();
    assert_eq!(out.status.code(), Some(69));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot start"));
    let out = Command::new(LAUNCHER).output().unwrap();
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn version_names_the_launcher_and_its_own_version() {
    let out = Command::new(LAUNCHER).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "alondrad-launcher 1");
}
