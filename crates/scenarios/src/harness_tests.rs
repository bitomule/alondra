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
    let text = SMALL.replace("[[step]]\ndo = \"checkpoint\"", "[[step]]\ndo = \"press\"\npr = 7\naction = \"merge\"\n\n[[step]]\ndo = \"checkpoint\"");
    match run(&scenario(&text), None) {
        Err(ScenarioError::ActionMissing { pr: 7, action, view }) => {
            assert_eq!(action, "merge");
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
