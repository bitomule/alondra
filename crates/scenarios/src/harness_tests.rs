//! The harness tests itself: it sees a failure and names it, it runs a scenario twice to the same
//! bytes, and nothing it runs reaches a real service.

use crate::{Outcome, Scenario, ScenarioError, checkpoint, run, run_with};

const SMALL: &str = r#"
name = "a request and its task"
max_virtual = "1h"

[[step]]
do = "request"
text = "Export the report"
task = "t1"
title = "Add the export"

[[step]]
do = "checkpoint"
name = "asked"
screens = ["home", "task:t1"]
pushes = true
"#;

fn scenario(text: &str) -> Scenario {
    Scenario::parse(text).expect("the scenario is valid")
}

fn recorded() -> Outcome {
    run(&scenario(SMALL), None).expect("the scenario runs")
}

#[test]
fn a_checkpoint_reads_each_screen_by_client_and_language() {
    let outcome = recorded();
    let (name, files) = &outcome.checkpoints[0];
    assert_eq!(name, "asked");
    let names: Vec<&str> = files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "home.ios.en.json", "home.ios.es.json", "home.tui.en.json", "home.tui.es.json", "pushes.json",
            "task-t1.ios.en.json", "task-t1.ios.es.json", "task-t1.tui.en.json", "task-t1.tui.es.json",
        ]
    );
}

#[test]
fn the_same_scenario_twice_reads_the_same_bytes() {
    let (first, second) = (recorded(), recorded());
    assert_eq!(first.checkpoints, second.checkpoints);
}

#[test]
fn a_view_that_changed_is_a_checkpoint_diff_naming_the_screen_client_and_language() {
    let outcome = recorded();
    let case = tempfile::tempdir().unwrap();
    let (name, files) = &outcome.checkpoints[0];
    checkpoint::write(case.path(), name, files).unwrap();
    assert!(run(&scenario(SMALL), Some(case.path())).is_ok());
    let file = case.path().join("checkpoints/asked/home.ios.es.json");
    let tampered = std::fs::read_to_string(&file).unwrap().replacen("\"view\": \"home\"", "\"view\": \"changed\"", 1);
    assert!(tampered.contains("changed"), "the accepted file has no view key to change");
    std::fs::write(&file, tampered).unwrap();
    match run(&scenario(SMALL), Some(case.path())) {
        Err(ScenarioError::CheckpointDiff { checkpoint, differences }) => {
            assert_eq!(checkpoint, "asked");
            assert_eq!(differences.keys().map(String::as_str).collect::<Vec<_>>(), ["home.ios.es.json"]);
        }
        other => panic!("expected a checkpoint diff, found {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_checkpoint_nobody_accepted_is_a_diff() {
    let case = tempfile::tempdir().unwrap();
    assert!(matches!(run(&scenario(SMALL), Some(case.path())), Err(ScenarioError::CheckpointDiff { .. })));
}

#[test]
fn an_assertion_of_the_case_that_fails_is_reported_with_what_it_expected() {
    let text = format!("{SMALL}\n[[step.expect]]\ncheck = \"gh_calls\"\ncalls = [\"merge 9\"]\n");
    match run(&scenario(&text), None) {
        Err(ScenarioError::ExpectFailed { checkpoint, expected, actual }) => {
            assert_eq!((checkpoint.as_str(), expected.as_str(), actual.as_str()), ("asked", "[\"merge 9\"]", "[]"));
        }
        other => panic!("expected a failed assertion, found {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_daemon_that_does_not_settle_inside_the_time_given_is_no_rest() {
    let text = SMALL.replace("[[step]]\ndo = \"checkpoint\"", "[[step]]\ndo = \"rest\"\nmax = \"5s\"\n\n[[step]]\ndo = \"checkpoint\"");
    assert!(matches!(run(&scenario(&text), None), Err(ScenarioError::NoRest { .. })));
}

#[test]
fn a_press_on_a_card_that_is_not_there_names_the_view_it_looked_in() {
    let text = SMALL.replace("[[step]]\ndo = \"checkpoint\"", "[[step]]\ndo = \"press\"\npresentation = \"d1\"\naction = \"merge\"\n\n[[step]]\ndo = \"checkpoint\"");
    match run(&scenario(&text), None) {
        Err(ScenarioError::ActionMissing { presentation, action, view }) => {
            assert_eq!((presentation.as_str(), action.as_str()), ("d1", "merge"));
            assert!(view.contains("\"view\": \"home\""), "{view}");
        }
        other => panic!("expected a missing action, found {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_scenario_that_is_not_valid_never_starts() {
    assert!(matches!(Scenario::parse("name = \"x\"\nmax_virtual = \"1h\"\n[[step]]\ndo = \"fly\""), Err(ScenarioError::Invalid(_))));
}

#[test]
fn a_process_started_through_the_launcher_is_a_leak() {
    let result = run_with(&scenario(SMALL), None, |launcher| {
        let state = tempfile::tempdir().unwrap();
        let bootstrap = tempfile::tempfile().unwrap();
        let spawn = alondra_agent_runtime::owned_host::HostSpawn {
            executable: std::path::Path::new("/usr/bin/true"),
            artifact: std::path::Path::new("/usr/bin/true"),
            state_dir: state.path(),
            environment: &Default::default(),
            confinement: None,
            stdin: bootstrap,
        };
        assert!(launcher.get().spawn(spawn).is_err());
    });
    match result {
        Err(ScenarioError::Leak(what)) => assert_eq!(what, "process /usr/bin/true"),
        other => panic!("expected a leak, found {:?}", other.map(|_| ())),
    }
}

const DELIVERED: &str = r#"
name = "a delivery waits for the user"
max_virtual = "1h"

[[step]]
do = "request"
text = "Export the report"
task = "t1"
title = "Add the export"

[[step]]
do = "node"
task = "t1"

[[step]]
do = "github"
event = "open_pr"
number = 7
head = "export"
base = "main"

[[step]]
do = "register_pr"
task = "t1"
number = 7

[[step]]
do = "present"
task = "t1"
id = "d1"
prs = [7]
body = "The export is ready"

[[step]]
do = "rest"
max = "1m"

[[step]]
do = "checkpoint"
name = "delivered"
screens = ["home"]
"#;

fn delivered(checkpoint_lines: &str) -> Result<Outcome, ScenarioError> {
    run(&scenario(&format!("{DELIVERED}{checkpoint_lines}")), None)
}

#[test]
fn the_pushes_a_phone_received_are_in_the_checkpoint_that_asks_for_them() {
    let outcome = delivered("pushes = true\n").expect("the scenario runs");
    let (_, files) = &outcome.checkpoints[0];
    let pushes: serde_json::Value = serde_json::from_str(&files["pushes.json"]).unwrap();
    let alerts: Vec<&serde_json::Value> = pushes.as_array().unwrap().iter().filter(|push| push["delivery"] == "alert").collect();
    assert_eq!(alerts.len(), 1, "{pushes}");
    assert_eq!(alerts[0]["device"], "<phone>");
    assert_eq!(alerts[0]["payload"]["alondra"]["card"], "d1");
}

#[test]
fn an_alert_the_phone_received_fails_a_check_that_there_was_none() {
    let lines = "pushes = true\n\n[[step.expect]]\ncheck = \"no_pushes\"\ndelivery = \"alert\"\n";
    match delivered(lines) {
        Err(ScenarioError::ExpectFailed { checkpoint, expected, actual }) => {
            assert_eq!(checkpoint, "delivered");
            assert_eq!(expected, "no alert push");
            assert!(actual.contains("alert"), "{actual}");
        }
        other => panic!("expected a failed assertion, found {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_badge_the_phone_never_got_fails_a_check_on_it() {
    let lines = "pushes = true\n\n[[step.expect]]\ncheck = \"badge\"\ncount = 0\n";
    match delivered(lines) {
        Err(ScenarioError::ExpectFailed { expected, actual, .. }) => {
            assert_eq!((expected.as_str(), actual.as_str()), ("the icon badge set to 0", "None"));
        }
        other => panic!("expected a failed assertion, found {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_check_on_pushes_needs_the_checkpoint_to_read_them() {
    let lines = "\n[[step.expect]]\ncheck = \"no_pushes\"\n";
    assert!(matches!(delivered(lines), Err(ScenarioError::Invalid(_))));
}
