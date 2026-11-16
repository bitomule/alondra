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
    /// The folder of the case, which the transcripts its steps name are read from.
    #[serde(skip)]
    pub case: Option<std::path::PathBuf>,
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

impl Step {
    fn server_error() -> String {
        "server_error".into()
    }
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
    /// The user asks for something from the app's new request screen, and the orchestrator makes
    /// the first task of it.
    Request { text: String, task: String, title: String },
    /// A further task of the request, optionally stacked on a pull request of an earlier one.
    Task { id: String, title: String, #[serde(default)] stack_on: Option<StackOn> },
    /// The node of `task` registers a pull request it opened.
    RegisterPr { task: String, number: u64 },
    /// The node of `task` presents the pull requests as its result. With `refused`, the daemon must
    /// turn the delivery down with that code, and nothing is presented.
    Present { task: String, id: String, prs: Vec<u64>, body: String, #[serde(default)] refused: Option<String> },
    /// The node of `task` tells the pull request is ready for others to stack on.
    StackReady { task: String, presentation: String, pr: u64 },
    /// The task starts on Codex: the daemon launches its node as it does in production, and the
    /// host it starts plays the first of `transcripts`. A host that ends, and the one that takes
    /// its place, each play the next.
    CodexNode { task: String, transcripts: Vec<String> },
    /// Something happens on GitHub.
    Github(GithubChange),
    /// The user presses `action` (a verb of the action ids: `merge`, `accept`, `changes`...) on the
    /// card of the presentation, as the app does it from the view.
    Press { presentation: String, action: String, #[serde(default)] text: Option<String>, #[serde(default)] screen: Option<String> },
    /// The node of `task` starts on its provider: its SDK host is admitted and takes the planned
    /// input as a turn.
    Node { task: String },
    /// The turn of the node of `task` ends. While the provider is down it ends on the error the
    /// provider answers with.
    TurnEnd { task: String },
    /// The turn of the node of `task` ends on this API error, whatever the provider's state.
    ApiError { task: String, error: String, #[serde(default)] status: Option<u16> },
    /// The provider answers every turn with this error, until `provider_up`; only the turns of
    /// `tasks` when it names any, and the others' turns get answers. A node with an input waiting
    /// takes it and its turn ends on the error.
    ProviderDown { #[serde(default = "Step::server_error")] error: String, #[serde(default)] status: Option<u16>, #[serde(default)] tasks: Vec<String> },
    ProviderUp,
    /// The sign-in of the provider expired: every turn ends on `authentication_failed`.
    LoginExpired,
    /// The user signed in again: the provider answers.
    LoginRestored,
    /// The SDK host of `task` ended. `resumes`: another host resumes the same session.
    HostExit { task: String, #[serde(default)] resumes: bool },
    /// The user writes to the node of `task` from the task's screen of the app.
    Tell { task: String, text: String },
    /// The node of `task` reads the oldest message the user wrote to it that it has not read, in
    /// the turn it has running: the input that message became must still be waiting.
    ReadMessage { task: String },
    /// The node of `task` says it cleaned up after its task, as `cleanup_done` does.
    CleanupDone { task: String },
    /// The node of `task` ended and Alondra recorded it.
    NodeEnd { task: String },
    /// The daemon stops and starts again over the same home, as a restart does.
    Restart,
    /// The daemon is replaced by a newer release: every SDK host of the older one is refused, ends,
    /// and one that resumes its session takes its place.
    Update,
    /// The user presses the option `option` of the card the daemon shows for `task` whose id starts
    /// with `card`, as the app does it from the view.
    Answer { card: String, option: String, #[serde(default)] text: Option<String> },
    /// Virtual time passes.
    Advance { by: String },
    /// The daemon runs until nothing changes for a while.
    Rest { max: String },
    Checkpoint(Checkpoint),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum GithubChange {
    OpenPr { number: u64, head: String, base: String, #[serde(default)] draft: bool },
    /// The PR leaves draft, as `gh pr ready` does.
    MarkReady { number: u64 },
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
    TaskState { task: String, state: alondra_core::domain::TaskState },
    /// Every transcript of the case was played to its end: the daemon sent what each expected and
    /// received everything it said.
    TranscriptsPlayed,
    /// The pushes sent since the last checkpoint (the checkpoint sets `pushes`) include none, or
    /// none of that `delivery` (`alert`, `background` or `badge`).
    NoPushes { #[serde(default)] delivery: Option<String> },
    /// The last icon badge pushed since the last checkpoint (the checkpoint sets `pushes`) is `count`.
    Badge { count: u64 },
    /// The view `screen` as the iOS app reads it in English shows (or, with `present = false`, does
    /// not show) this text.
    ViewShows { screen: String, text: String, #[serde(default = "Expect::shown")] present: bool },
}

impl Expect {
    fn shown() -> bool {
        true
    }
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
resumes = true

[[step]]
do = "restart"

[[step]]
do = "update"
"#,
        )
        .unwrap();
        assert!(matches!(&scenario.steps[0], Step::ProviderDown { error, status: Some(529), tasks } if error == "server_error" && tasks.is_empty()));
        assert!(matches!(scenario.steps[3], Step::HostExit { resumes: true, .. }));
        assert!(matches!(scenario.steps[5], Step::Update));
    }

    #[test]
    fn what_is_not_a_duration_or_a_known_field_is_refused_before_anything_runs() {
        assert!(matches!(Scenario::parse("name = \"x\"\nmax_virtual = \"soon\""), Err(ScenarioError::Invalid(_))));
        assert!(matches!(Scenario::parse("name = \"x\"\nmax_virtual = \"1h\"\nsurprise = 1"), Err(ScenarioError::Invalid(_))));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
    }
}
