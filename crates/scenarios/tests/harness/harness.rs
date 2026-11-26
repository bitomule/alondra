//! The harness tests itself: it sees a failure and names it, it runs a scenario twice to the same
//! bytes, and nothing it runs reaches a real service.

use crate::test_support;
use alondra_scenarios::{Outcome, Scenario, ScenarioError, checkpoint, run, run_with};

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

#[test]
fn the_relay_socket_is_private_from_the_moment_it_is_bound() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    use std::process::{Child, Command, Stdio};

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let directory = crate::test_support::Directory::for_unix_socket();
    std::fs::create_dir_all(&*directory).unwrap();
    let socket = directory.join("s");
    let mut command = Command::new(std::env::var_os("SCENARIO_HOST_RELAY").unwrap());
    command.arg(&socket).arg(directory.join("b")).arg(directory.join("i")).env("SCENARIO_TEST_BOUND", "1").stdin(Stdio::piped()).stdout(Stdio::piped());
    // SAFETY: only the forked test child changes its umask before exec.
    unsafe { command.pre_exec(|| { libc::umask(0o022); Ok(()) }); }
    let mut child = OwnedChild(command.spawn().unwrap());
    let mut line = String::new();
    BufReader::new(child.0.stdout.take().unwrap()).read_line(&mut line).unwrap();
    assert_eq!(line, "bound\n");
    assert_eq!(std::fs::symlink_metadata(socket).unwrap().permissions().mode() & 0o777, 0o600);
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
    if let Err(error) = run(&scenario(SMALL), Some(case.path())) {
        panic!("the recorded checkpoint differs: {error}");
    }
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
fn a_process_run_through_the_launcher_is_a_leak() {
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
        assert!(launcher.get().spawn(spawn).is_err(), "a host starts only from a daemon's bootstrap");
        let run = alondra_agent_runtime::owned_host::ProbeRun { command: std::process::Command::new("/usr/bin/true"), input: Vec::new(), timeout: None };
        assert!(launcher.get().run(run).is_err());
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
    let raw = files.get("pushes.json").unwrap_or_else(|| panic!("pushes.json is missing from checkpoint: {files:#?}"));
    let pushes: serde_json::Value = serde_json::from_str(raw).unwrap_or_else(|error| panic!("invalid pushes.json: {error}; contents: {raw}"));
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
fn the_start_problem_budget_counts_reads_and_rejects_an_empty_fixture() {
    let budget = "\n[[step.expect]]\ncheck = \"start_problem_reads\"\nmax_passes = 0\n";
    let two_tasks = DELIVERED.replace("do = \"node\"", "do = \"task\"\nid = \"t2\"\ntitle = \"Another export\"\n\n[[step]]\ndo = \"node\"");
    match run(&scenario(&format!("{two_tasks}{budget}")), None) {
        Err(ScenarioError::ExpectFailed { expected, actual, .. }) => {
            assert_eq!(expected, "at most 0 card reads per start-problem pass");
            assert!(actual.contains("tasks=2"), "{actual}");
        }
        other => panic!("expected a failed work budget, found {:?}", other.map(|_| ())),
    }
    match run(&scenario(&format!("{SMALL}{budget}")), None) {
        Err(ScenarioError::ExpectFailed { expected, .. }) => {
            assert_eq!(expected, "multiple open tasks and nonempty card history");
        }
        other => panic!("expected an insufficient fixture, found {:?}", other.map(|_| ())),
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

/// Virtual time moves only when a step moves it: reading the views of a checkpoint over HTTP lets
/// no daemon timer jump the clock, so a checkpoint keeps the exact instant of what happened. The
/// scenario starts once the daemon checked its accounts and sampled the Mac after it, 10 s in.
#[test]
fn a_checkpoint_keeps_the_instants_the_steps_set() {
    let text = format!("{SMALL}
[[step]]
do = \"advance\"
by = \"25m\"

[[step]]
do = \"task\"
id = \"t2\"
title = \"Add the summary\"

[[step]]
do = \"checkpoint\"
name = \"later\"
screens = [\"task:t2\"]
");
    let outcome = run(&scenario(&text), None).expect("the scenario runs");
    let read = |checkpoint: usize, file: &str| outcome.checkpoints[checkpoint].1[file].clone();
    assert!(read(0, "task-t1.ios.en.json").contains("\"2026-11-15T09:00:10.000+00:00\""), "{}", read(0, "task-t1.ios.en.json"));
    assert!(read(1, "task-t2.ios.en.json").contains("\"2026-11-15T09:25:10.000+00:00\""), "{}", read(1, "task-t2.ios.en.json"));
}

/// A host takes real time to exit once the daemon stops it, while virtual time waits: the daemon
/// closes the host of a task that rests after its turn, and whatever that closing takes in real
/// time, it lands at the instant that started it, so what comes after keeps its instant.
#[test]
fn a_host_slow_to_exit_moves_no_instant() {
    let text = r#"
name = "a resting node whose host closes"
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
do = "login_expired"
account = "default"

[[step]]
do = "turn_end"
task = "t1"

[[step]]
do = "rest"
max = "10m"

[[step]]
do = "task"
id = "t2"
title = "Add the summary"

[[step]]
do = "checkpoint"
name = "after-the-rest"
screens = ["task:t2"]
"#;
    let at_once = run(&scenario(text), None).expect("the scenario runs");
    let slowly = alondra_scenarios::runner::run_with_slow_host_exits(&scenario(text), std::time::Duration::from_millis(400)).expect("the scenario runs");
    assert_eq!(at_once.checkpoints, slowly.checkpoints);
}

/// The commands and probes the daemon runs around a host take real time too, which virtual time
/// never waits for: a probe that takes a second leaves every instant where a probe that takes none
/// leaves it.
#[test]
fn a_slow_probe_moves_no_instant() {
    let text = r#"
name = "a resting node whose host closes"
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
do = "login_expired"
account = "default"

[[step]]
do = "turn_end"
task = "t1"

[[step]]
do = "rest"
max = "10m"

[[step]]
do = "task"
id = "t2"
title = "Add the summary"

[[step]]
do = "checkpoint"
name = "after-the-rest"
screens = ["task:t2"]
"#;
    let at_once = run(&scenario(text), None).expect("the scenario runs");
    let slowly = alondra_scenarios::runner::run_with_slow_probes(&scenario(text), std::time::Duration::from_millis(1500)).expect("the scenario runs");
    assert_eq!(at_once.checkpoints, slowly.checkpoints);
}

/// A blocking job of the daemon that a loaded Mac schedules late: virtual time never waits for a
/// job nobody told it about, so a job that starts after the step that woke it reads the clock, and
/// commits, at the instant of a later step. The daemon's jobs count as work from the moment they are
/// submitted, so the instants stay where a job that starts at once leaves them.
#[test]
fn a_probe_that_starts_late_moves_no_instant() {
    let text = r#"
name = "a resting node whose host closes"
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
do = "login_expired"
account = "default"

[[step]]
do = "turn_end"
task = "t1"

[[step]]
do = "rest"
max = "10m"

[[step]]
do = "task"
id = "t2"
title = "Add the summary"

[[step]]
do = "checkpoint"
name = "after-the-rest"
screens = ["task:t2"]
"#;
    let at_once = run(&scenario(text), None).expect("the scenario runs");
    let late = alondra_scenarios::runner::run_with_late_probes(&scenario(text), std::time::Duration::from_millis(1500)).expect("the scenario runs");
    assert_eq!(at_once.checkpoints, late.checkpoints);
}

/// 15 November: on a loaded Mac a release run failed «starting the SDK host» for a host that did
/// start, only later than the daemon's I/O timeout in real time. A host that opens its socket later
/// than that timeout still starts its node, and the scenario reads the same as with a host that
/// listens at once.
#[test]
fn a_host_slow_to_start_still_starts_its_node() {
    let text = r#"
name = "a node whose host starts late"
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
do = "checkpoint"
name = "started"
screens = ["task:t1"]
"#;
    let at_once = run(&scenario(text), None).expect("the scenario runs");
    let late = alondra_scenarios::runner::run_with_slow_host_starts(&scenario(text), std::time::Duration::from_millis(1_500), 1).expect("a host that starts late starts its node");
    assert_eq!(at_once.checkpoints, late.checkpoints);
}
