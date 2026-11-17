//! The edge answers Claude's command line and the host's probes from the world, per account, and
//! nothing else.

use super::*;

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
