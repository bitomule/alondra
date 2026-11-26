//! The edge answers Claude's command line and the host's probes from the world, per account, and
//! nothing else.

use alondra_agent_runtime::owned_host::{HostLauncher, ProbeRun};
use alondra_scenarios::claude_host::{ClaudeEdge, assistant_errors};
use serde_json::{Value, json};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command as Process;
use std::sync::{Arc, Mutex};

fn edge() -> (tempfile::TempDir, ClaudeEdge, Arc<Mutex<Vec<String>>>) {
    let home = tempfile::Builder::new().prefix("ce-").tempdir().unwrap();
    let leaks = Arc::new(Mutex::new(Vec::new()));
    let edge = ClaudeEdge::new(home.path(), leaks.clone());
    (home, edge, leaks)
}

fn status(edge: &ClaudeEdge, folder: Option<&Path>) -> Value {
    let mut command = Process::new("/usr/local/bin/claude");
    command.args(["auth", "status", "--json"]);
    if let Some(folder) = folder {
        command.env("CLAUDE_CONFIG_DIR", folder);
    }
    let output = edge.run(ProbeRun { command, input: Vec::new(), timeout: None }).unwrap();
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn an_expired_sign_in_is_seen_only_on_its_own_account() {
    let (home, edge, leaks) = edge();
    let work = alondra_core::accounts::folders(home.path()).join("work");
    edge.set_signed_in("work", false);
    assert_eq!(status(&edge, Some(&work))["loggedIn"], json!(false));
    assert_eq!(status(&edge, None)["loggedIn"], json!(true), "the default account is still signed in");
    edge.set_signed_in("work", true);
    assert_eq!(status(&edge, Some(&work))["loggedIn"], json!(true));
    assert!(leaks.lock().unwrap().is_empty());
}

#[test]
fn a_run_the_edge_does_not_answer_is_a_leak() {
    let (_home, edge, leaks) = edge();
    let command = Process::new("/usr/bin/git");
    assert!(edge.run(ProbeRun { command, input: Vec::new(), timeout: None }).is_err());
    assert_eq!(leaks.lock().unwrap().len(), 1, "{:?}", leaks.lock().unwrap());
}

#[test]
fn the_errors_a_scenario_names_are_the_sdks_own() {
    let errors = assistant_errors();
    for error in ["authentication_failed", "rate_limit", "server_error", "overloaded"] {
        assert!(errors.iter().any(|known| known == error), "{error} is in sdk-errors.json");
    }
    assert!(!errors.iter().any(|known| known == "outage"));
}

/// The daemon kills a host's group right after writing it a frame the relay had not passed on yet:
/// the frame goes with the process and is never handled, so it stops counting once the process
/// exited, and the wait for calm does not wait for it to its patience.
#[test]
fn a_frame_lost_with_a_host_s_process_stops_counting_once_the_process_exits() {
    struct Owned(std::process::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let runtime = alondra_core::clock::VirtualRuntime::new(1_700_000_000_000).unwrap();
    runtime.block_on(async {
        let (home, edge, _) = edge();
        let mut process = Owned(Process::new("/bin/sleep").arg("600").process_group(0).spawn().unwrap());
        let pid = process.0.id();
        let bootstrap = json!({
            "host_instance_id": "host-1",
            "control_secret": "01".repeat(32),
            "fence": {"profile_id": "default", "account_id": "default", "data_epoch": "epoch", "host_id": "host"},
            "execution": {"task_id": "t1", "agent_id": "sdk-t1", "conversation_id": "conversation", "process_instance_id": "process", "generation": 1},
            "manifest": {"host_release": "0.0.0", "host_sha256": "a".repeat(64), "executable_sha256": "b".repeat(64), "sdk_version": "sdk"},
            "native": {"session_id": "session"},
        });
        let _host = edge.start_host(&serde_json::to_vec(&bootstrap).unwrap(), pid, home.path().join("b")).unwrap();
        alondra_core::clock::frame_sent("host-1");
        process.0.kill().unwrap();
        process.0.wait().unwrap();
        let rested = alondra_core::clock::spawn_blocking_unwatched(|| alondra_core::clock::until_idle(std::time::Duration::from_secs(20), || true)).await.unwrap();
        assert!(rested, "{} frames still counted for a host whose process exited", alondra_core::clock::frames_in_transit());
    });
}
