//! A scenario as it is written: one `scenario.toml` per case, a world, steps and the checkpoints
//! the steps pass through.

use crate::error::ScenarioError;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    /// The pull request that fixed what the scenario reproduces; empty for a scenario of behaviour.
    #[serde(default)]
    pub fixes: Option<String>,
    /// The most virtual time the scenario may spend, as `30s`, `5m` or `4h`.
    pub max_virtual: String,
    #[serde(default)]
    pub world: World,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct World {
    /// The clients whose views a checkpoint reads.
    #[serde(default = "World::default_clients")]
    pub clients: Vec<String>,
    #[serde(default = "World::default_langs")]
    pub langs: Vec<String>,
    /// `owner/name` of the repository whose pull requests the scenario opens.
    #[serde(default = "World::default_repository")]
    pub repository: String,
}

impl World {
    fn default_clients() -> Vec<String> {
        vec!["ios".into(), "tui".into()]
    }
    fn default_langs() -> Vec<String> {
        vec!["es".into(), "en".into()]
    }
    fn default_repository() -> String {
        "demo/app".into()
    }
}

impl Default for World {
    fn default() -> Self {
        Self { clients: Self::default_clients(), langs: Self::default_langs(), repository: Self::default_repository() }
    }
}

/// A task of a request that may stand on a pull request another task delivered.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackOn {
    pub upstream: String,
    pub presentation: String,
    pub pr: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// The user asks for something: a request and the first task of it.
    Request { text: String, task: String, title: String },
    /// A further task of the request, optionally stacked on a pull request of an earlier one.
    Task { id: String, title: String, #[serde(default)] stack_on: Option<StackOn> },
    /// The node of `task` registers a pull request it opened.
    RegisterPr { task: String, number: u64 },
    /// The node of `task` presents the pull requests as its result.
    Present { task: String, id: String, prs: Vec<u64>, body: String },
    /// The node of `task` tells the pull request is ready for others to stack on.
    StackReady { task: String, presentation: String, pr: u64 },
    /// Something happens on GitHub.
    Github(GithubChange),
    /// The user presses `action` on the card of a pull request, as the app does it from the view.
    Press { pr: u64, action: String },
    /// Virtual time passes.
    Advance { by: String },
    /// The daemon runs until nothing changes for a while.
    Rest { max: String },
    Checkpoint(Checkpoint),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum GithubChange {
    OpenPr { number: u64, head: String, base: String },
    MergeExternally { number: u64 },
    CloseExternally { number: u64 },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub name: String,
    /// The views read: `home`, `tasks`, `task:<id>`.
    #[serde(default)]
    pub screens: Vec<String>,
    /// Whether the pushes sent since the last checkpoint are compared.
    #[serde(default)]
    pub pushes: bool,
    #[serde(default)]
    pub expect: Vec<Expect>,
}

/// What the case itself asserts: the thing the bug broke, in words a reader checks at a glance.
#[derive(Debug, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expect {
    /// The calls Alondra made to GitHub that change something, in order, as `gh` is typed.
    GhCalls { calls: Vec<String> },
    PrMergedInto { pr: u64, branch: String },
    TaskState { task: String, state: String },
}

pub fn parse_duration(text: &str) -> Result<Duration, ScenarioError> {
    let invalid = || ScenarioError::Invalid(format!("`{text}` is not a duration like 30s, 5m or 4h"));
    let split = text.find(|c: char| !c.is_ascii_digit()).ok_or_else(invalid)?;
    let amount: u64 = text[..split].parse().map_err(|_| invalid())?;
    match &text[split..] {
        "ms" => Ok(Duration::from_millis(amount)),
        "s" => Ok(Duration::from_secs(amount)),
        "m" => Ok(Duration::from_secs(amount * 60)),
        "h" => Ok(Duration::from_secs(amount * 3600)),
        _ => Err(invalid()),
    }
}

impl Scenario {
    pub fn parse(text: &str) -> Result<Self, ScenarioError> {
        let scenario: Scenario = toml::from_str(text).map_err(|error| ScenarioError::Invalid(error.to_string()))?;
        parse_duration(&scenario.max_virtual)?;
        for step in &scenario.steps {
            match step {
                Step::Advance { by } => drop(parse_duration(by)?),
                Step::Rest { max } => drop(parse_duration(max)?),
                _ => {}
            }
        }
        Ok(scenario)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn what_is_not_a_duration_or_a_known_field_is_refused_before_anything_runs() {
        assert!(matches!(Scenario::parse("name = \"x\"\nmax_virtual = \"soon\""), Err(ScenarioError::Invalid(_))));
        assert!(matches!(Scenario::parse("name = \"x\"\nmax_virtual = \"1h\"\nsurprise = 1"), Err(ScenarioError::Invalid(_))));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
    }
}
