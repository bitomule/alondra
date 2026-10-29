//! `alondra doctor --json` in each of its forms prints exactly one JSON document on stdout, with
//! the daemon down and nothing installed: progress and questions never reach stdout.

#[path = "../../test_support.rs"]
mod test_support;

use std::net::TcpListener;
use std::process::{Command, Output};

use serde_json::Value;

fn alondra(args: &[&str], config: Option<&str>) -> (Output, test_support::Directory) {
    let root = test_support::Directory::new();
    std::fs::create_dir_all(root.join("home")).unwrap();
    if let Some(text) = config {
        std::fs::write(root.join("home/config.toml"), text).unwrap();
    }
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let binary = std::fs::canonicalize(env!("CARGO_BIN_EXE_alondra")).unwrap();
    let output = Command::new(binary)
        .args(args)
        .current_dir(&*root)
        .env_clear()
        .env("HOME", &*root)
        .env("ALONDRA_HOME", root.join("home"))
        .env("ALONDRA_PORT", port.to_string())
        .env("ALONDRA_LANG", "en")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    (output, root)
}

fn doctor(args: &[&str]) -> (Output, Value) {
    let (output, root) = alondra(&[&["doctor"], args].concat(), None);
    let projects = format!("projects folder: {} (cwd)", root.display());
    if args.contains(&"--fix") {
        assert!(String::from_utf8_lossy(&output.stderr).contains(&projects), "{output:?}");
    }
    let doc = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {output:?}"));
    (output, doc)
}

fn check<'a>(doc: &'a Value, id: &str) -> &'a Value {
    doc["checks"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("no {id}: {doc}"))
}

#[test]
fn the_internal_update_report_keeps_the_compact_output_and_doctor_exit_code() {
    let (output, _root) = alondra(&["doctor", "--after-update"], Some("unknown_setting = true\n"));
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(said.contains("unknown field `unknown_setting`"), "{said}");
    assert!(said.starts_with('\n'), "{said}");
    assert!(!said.contains("alondra.doctor/1"), "{said}");
}

#[test]
fn doctor_json_keeps_todays_shape_and_exits_4_with_the_daemon_down() {
    let (output, doc) = doctor(&["--json"]);
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert_eq!(doc["schema"], "alondra.doctor/1");
    assert_eq!(doc["ok"], false);
    for c in doc["checks"].as_array().unwrap() {
        for key in ["check", "id", "status", "detail", "fix", "fixable"] {
            assert!(c.get(key).is_some(), "{key} missing from {c}");
        }
    }
    assert_eq!(check(&doc, "alondrad")["status"], "fail");
    assert_eq!(check(&doc, "projects")["fixable"], true, "no projects folder yet: {doc}");
    assert!(doc["next_step"].is_string(), "{doc}");
}

#[test]
fn doctor_fix_json_without_input_reports_on_stdout_and_progress_on_stderr() {
    let (output, doc) = doctor(&["--fix", "--json", "--no-input"]);
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert_eq!(doc["schema"], "alondra.doctor/1");
    assert_eq!(check(&doc, "identity")["status"], "pass", "the service identity is created on its own: {doc}");
    let progress = String::from_utf8_lossy(&output.stderr);
    assert!(progress.contains("✗ Background service:"), "{progress}");
    assert_eq!(check(&doc, "projects")["status"], "pass", "without a terminal the cwd becomes the projects folder: {doc}");
}

#[test]
fn doctor_test_push_json_without_a_relay_exits_3_with_the_next_step() {
    let (output, doc) = doctor(&["--test-push", "--json"]);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert_eq!(doc["schema"], "alondra.doctor/1");
    assert_eq!(doc["test_push"]["sent"], false);
    assert_eq!(doc["next_step"], "alondra relay on");
}

#[test]
fn alondra_without_a_terminal_on_a_mac_never_set_up_exits_3_and_says_how() {
    let (output, root) = alondra(&[], None);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("Alondra is not set up on this Mac. Run `alondra` in a terminal, or `alondra doctor --fix --no-input`."), "{said}");
    assert_eq!(std::fs::read_dir(root.join("home")).unwrap().count(), 0, "nothing was written");
}

#[test]
fn alondra_without_a_terminal_and_the_daemon_down_exits_4() {
    let (output, _root) = alondra(&[], Some("name = \"MacBook Pro\"\n"));
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("alondra doctor --fix"), "{output:?}");
}
