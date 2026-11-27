use alondra_scenarios::format::{GithubChange, Scenario, Step, parse_duration};
use alondra_scenarios::ScenarioError;
use std::time::Duration;

#[test]
fn a_scenario_reads_its_steps_in_order() {
    let scenario = Scenario::parse(
        r#"
name = "demo"
max_virtual = "1h"

[[step]]
do = "request"
text = "Do it"
task = "t1"
title = "Do it"

[[step]]
do = "github"
event = "open_pr"
number = 7
head = "feature"
base = "main"

[[step]]
do = "checkpoint"
name = "after"
screens = ["home"]
"#,
    )
    .unwrap();
    assert_eq!(scenario.steps.len(), 3);
    assert!(matches!(scenario.steps[1], Step::Github(GithubChange::OpenPr { number: 7, .. })));
}

#[test]
fn the_provider_restart_and_update_steps_are_read() {
    let scenario = Scenario::parse(
        r#"
name = "demo"
max_virtual = "1h"

[[step]]
do = "provider_down"
status = 529

[[step]]
do = "api_error"
task = "t1"
error = "rate_limit"
status = 429

[[step]]
do = "login_expired"

[[step]]
do = "host_exit"
task = "t1"

[[step]]
do = "restart"
lose = ["t1"]
full_disk = ["t1"]

[[step]]
do = "update"
"#,
    )
    .unwrap();
    assert!(matches!(&scenario.steps[0], Step::ProviderDown { error, status: Some(529), tasks, .. } if error == "server_error" && tasks.is_empty()));
    assert!(matches!(&scenario.steps[2], Step::LoginExpired { account } if account == "default"));
    assert!(matches!(&scenario.steps[4], Step::Restart { lose, full_disk } if lose == &["t1"] && full_disk == &["t1"]));
    assert!(matches!(scenario.steps[5], Step::Update));
}

#[test]
fn a_staged_node_and_its_plan_are_read() {
    let scenario = Scenario::parse(
        r#"
name = "demo"
max_virtual = "1h"

[[step]]
do = "node"
task = "t1"
stages = true

[[step]]
do = "plan"
task = "t1"
request = "author-plan"
plan = "- [ ] Do it"
"#,
    )
    .unwrap();
    assert!(matches!(&scenario.steps[0], Step::Node { stages: true, model: None, .. }));
    assert!(matches!(&scenario.steps[1], Step::Plan { request, plan, .. } if request == "author-plan" && plan == "- [ ] Do it"));
}

#[test]
fn what_is_not_a_duration_or_a_known_field_is_refused_before_anything_runs() {
    assert!(matches!(Scenario::parse("name = \"x\"\nmax_virtual = \"soon\""), Err(ScenarioError::Invalid(_))));
    assert!(matches!(Scenario::parse("name = \"x\"\nmax_virtual = \"1h\"\nsurprise = 1"), Err(ScenarioError::Invalid(_))));
    assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
}
