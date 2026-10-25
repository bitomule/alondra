//! `alondra --version` never depends on the daemon: with nothing listening, or with a daemon that
//! never answers, it still answers with exit 0 after one attempt bounded by its own short limit.

#[path = "../../test_support.rs"]
mod test_support;

use std::io::ErrorKind;
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Sixty times the CLI's 500 ms limit on `GET /daemon`: only a hang reaches it, a loaded Mac does not.
const HANG_GUARD: Duration = Duration::from_secs(30);

fn run_against(port: u16, args: &[&str]) -> Output {
    let root = test_support::Directory::new();
    std::fs::create_dir_all(root.join("home")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .args(args)
        .env_clear()
        .env("HOME", &*root)
        .env("ALONDRA_HOME", root.join("home"))
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_LANG", "en")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let hang_guard = Instant::now() + HANG_GUARD;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= hang_guard {
            let _ = child.kill();
            let _ = child.wait();
            panic!("hang guard: alondra {args:?} still running after {HANG_GUARD:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

fn run_without_daemon(args: &[&str]) -> Output {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    run_against(port, args)
}

fn connections_waiting(listener: &TcpListener) -> usize {
    listener.set_nonblocking(true).unwrap();
    let mut count = 0;
    loop {
        match listener.accept() {
            Ok(_) => count += 1,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return count,
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn version_json_with_the_daemon_down_is_one_document_and_exit_zero() {
    let output = run_without_daemon(&["--version", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {output:?}"));
    assert_eq!(doc["schema"], "alondra.version/1");
    assert!(doc["cli"]["version"].as_str().is_some_and(|v| !v.is_empty()), "{doc}");
    assert!(doc["cli"]["path"].as_str().is_some_and(|p| p.ends_with("alondra")), "{doc}");
    assert_eq!(doc["service"], serde_json::json!({"reachable": false}));
    assert_eq!(doc["same_version"], false);
}

#[test]
fn version_json_asks_a_daemon_that_never_answers_once_and_gives_up_by_itself() {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let output = run_against(silent.local_addr().unwrap().port(), &["--version", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {output:?}"));
    assert_eq!(doc["service"], serde_json::json!({"reachable": false}));
    assert_eq!(connections_waiting(&silent), 1);
}

#[test]
fn short_version_with_the_daemon_down_says_the_service_is_not_running() {
    let output = run_without_daemon(&["-V"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("alondra ") && text.trim_end().ends_with("· service not running"), "{text}");
}

#[test]
fn json_alone_is_a_usage_error() {
    let output = run_without_daemon(&["--json"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
