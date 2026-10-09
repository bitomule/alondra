//! `alondra --version` never depends on the daemon: with nothing listening it still answers,
//! at once and with exit 0.

#[path = "../../test_support.rs"]
mod test_support;

use std::net::TcpListener;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn run_without_daemon(args: &[&str]) -> (Output, Duration) {
    let root = test_support::Directory::new();
    std::fs::create_dir_all(root.join("home")).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_alondra"))
        .args(args)
        .env_clear()
        .env("HOME", &*root)
        .env("ALONDRA_HOME", root.join("home"))
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_LANG", "en")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    (output, started.elapsed())
}

#[test]
fn version_json_with_the_daemon_down_is_one_document_and_exit_zero() {
    let (output, took) = run_without_daemon(&["--version", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {output:?}"));
    assert_eq!(doc["schema"], "alondra.version/1");
    assert!(doc["cli"]["version"].as_str().is_some_and(|v| !v.is_empty()), "{doc}");
    assert!(doc["cli"]["path"].as_str().is_some_and(|p| p.ends_with("alondra")), "{doc}");
    assert_eq!(doc["service"], serde_json::json!({"reachable": false}));
    assert_eq!(doc["same_version"], false);
    assert!(took < Duration::from_secs(2), "took {took:?}");
}

#[test]
fn short_version_with_the_daemon_down_says_the_service_is_not_running() {
    let (output, _) = run_without_daemon(&["-V"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("alondra ") && text.trim_end().ends_with("· service not running"), "{text}");
}

#[test]
fn json_alone_is_a_usage_error() {
    let (output, _) = run_without_daemon(&["--json"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
