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
    /// The Claude accounts set up in Settings, by id, each signed in: `default` is Claude's own.
    #[serde(default = "World::default_accounts")]
    pub accounts: Vec<String>,
    /// The release the daemon runs until an `update` installs this one.
    #[serde(default = "alondra_engine_sdk::sdk_owner::backend::this_release")]
    pub release: String,
    /// The context window, in tokens, Claude's catalog gives its model; none by default, as it
    /// gives none today.
    #[serde(default)]
    pub context_window: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeFile {
    pub path: String,
    pub text: String,
}

/// A file of a spec proposal: the whole new text, or none to remove it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedFile {
    pub path: String,
    #[serde(default)]
    pub text: Option<String>,
    /// A mockup: a file of the task's worktree Alondra reads and keeps.
    #[serde(default)]
    pub source: Option<String>,
    /// Instead of text, a synthetic PNG of this width and height.
    #[serde(default)]
    pub png: Option<[u32; 2]>,
}

impl ProposedFile {
    /// What the file holds: its text, or its synthetic PNG.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        self.png.map(synthetic_png).or_else(|| self.text.as_ref().map(|text| text.clone().into_bytes()))
    }
}

/// The signature and header of a PNG of that size: all a mockup or a capture is read by.
pub fn synthetic_png([width, height]: [u32; 2]) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0]);
    bytes
}

impl Step {
    fn server_error() -> String {
        "server_error".into()
    }
    fn default_account() -> String {
        World::default_account()
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
    fn default_accounts() -> Vec<String> {
        vec![Self::default_account()]
    }
    fn default_account() -> String {
        alondra_core::accounts::AccountId::DEFAULT.into()
    }
}

impl Default for World {
    fn default() -> Self {
        Self {
            clients: Self::default_clients(),
            langs: Self::default_langs(),
            repository: Self::default_repository(),
            accounts: Self::default_accounts(),
            release: alondra_engine_sdk::sdk_owner::backend::this_release(),
        }
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
    /// A further task of the request, optionally stacked on a pull request of an earlier one,
    /// waiting for the earlier tasks of `after`, or carrying on the task `continues` names.
    Task { id: String, title: String, #[serde(default)] stack_on: Option<StackOn>, #[serde(default)] after: Vec<String>, #[serde(default)] continues: Option<String> },
    /// The orchestrator cancels `task`, as `cancel_task` does.
    OrchestratorCancel { task: String },
    /// The orchestrator gives `task` a node of its own again, as a launch does. With `refused`, the
    /// daemon must turn it down with that code.
    OrchestratorAssign { task: String, #[serde(default)] refused: Option<String> },
    /// The node of `task` registers a pull request it opened, by the scenario's repository or by
    /// `repository` when it names it at another address.
    RegisterPr { task: String, number: u64, #[serde(default)] repository: Option<String> },
    /// The node of `task` presents the pull requests as its result, and `text` when it says what it
    /// delivers in words. With `refused`, the daemon must turn the delivery down with that code, and
    /// nothing is presented.
    Present { task: String, id: String, #[serde(default)] prs: Vec<u64>, #[serde(default)] text: Option<String>, body: String, #[serde(default)] refused: Option<String>,
        /// Point 1 of the approved plan, marked done with each of these scenarios as its evidence.
        #[serde(default)] scenarios: Vec<String>,
        /// Point 1 of the approved plan, marked done with each of these links as its evidence.
        #[serde(default)] links: Vec<String> },
    /// The node of `task` asks the user `body` on the card `id`, to answer in writing, and waits
    /// on it: the turn that ends next ends on that question.
    Ask { task: String, id: String, title: String, body: String },
    /// The node of `task` tells the pull request is ready for others to stack on.
    StackReady { task: String, presentation: String, pr: u64 },
    /// The task starts on Codex: the daemon launches its node as it does in production, and the
    /// host it starts plays the first of `transcripts`. A host that ends, and the one that takes
    /// its place, each play the next.
    CodexNode { task: String, transcripts: Vec<String> },
    /// The task starts on Antigravity, as `CodexNode` starts it on Codex: each host the daemon starts
    /// plays the next of `transcripts`, the frames the Antigravity host passes on from `agy acp`.
    AntigravityNode { task: String, transcripts: Vec<String> },
    /// Something happens on GitHub.
    Github(GithubChange),
    /// The user presses `action` (a verb of the action ids: `merge`, `accept`, `changes`...) on the
    /// card of the presentation, as the app does it from the view.
    Press { presentation: String, action: String, #[serde(default)] text: Option<String>, #[serde(default)] screen: Option<String> },
    /// The node of `task` starts on Claude, on `account`, as a launch in production starts it: the
    /// daemon starts its SDK host, which takes the task's first input as a turn. With `model`, the
    /// task runs that model of the account's catalog, as a launch that resolved its selection. With
    /// `stages`, the orchestrator chose stages for the task in its launch plan: the node starts as
    /// its author, which plans it, and a new node implements the plan the user approves.
    Node {
        task: String, #[serde(default = "Step::default_account")] account: String, #[serde(default)] model: Option<String>,
        #[serde(default)] stages: bool,
    },
    /// The conversation of the node of `task` holds `tokens` from now on, and its hosts' answers say
    /// what each request read from the prompt cache and wrote to it.
    Conversation { task: String, tokens: u64 },
    /// The turn of the node of `task` ends: on the error the API answers while it fails for the
    /// node's account, with Claude's answer otherwise. A node with an input waiting takes it next.
    TurnEnd { task: String },
    /// Claude Code loses the response it was reading in the turn of the node of `task`, as when the
    /// Mac sleeps mid-response: it reports the API error and goes on with the same turn.
    LostResponse { task: String },
    /// The turn of the node of `task` ends on this error of the SDK's (`sdk-errors.json`), whatever
    /// the API does for others.
    ApiError { task: String, error: String, #[serde(default)] status: Option<u16> },
    /// Claude's API fails every turn with this error, for every account, until `provider_up`; only
    /// the turns of `tasks` when it names any. A turn that starts meanwhile ends on it at once.
    /// `detail` is Claude Code's finer code of the error (`connection_failed`: this Mac cannot
    /// reach the API) and `said` what Claude Code writes of it.
    ProviderDown {
        #[serde(default = "Step::server_error")] error: String, #[serde(default)] status: Option<u16>, #[serde(default)] tasks: Vec<String>,
        #[serde(default)] detail: Option<String>, #[serde(default)] said: Option<String>,
    },
    ProviderUp,
    /// The sign-in of `account` expired: Claude says it is signed out, and every turn on it ends on
    /// `authentication_failed`. Other accounts go on.
    LoginExpired { #[serde(default = "Step::default_account")] account: String },
    /// The user signed in to `account` again.
    LoginRestored { #[serde(default = "Step::default_account")] account: String },
    /// The process of the SDK host of `task` dies.
    HostExit { task: String },
    /// The user presses `action` (`pause`, `resume`...) that the task's screen offers for `task` or
    /// its node.
    PressTask { task: String, action: String },
    /// The user sets how many nodes work at once.
    NodeLimit { count: u32 },
    /// The user writes to the node of `task` from the task's screen of the app.
    Tell { task: String, text: String },
    /// The node of `task` reads the oldest message the user wrote to it that it has not read, in
    /// the turn it has running: the input that message became must still be waiting.
    ReadMessage { task: String },
    /// The node of `task` says it cleaned up after its task, as `cleanup_done` does.
    CleanupDone { task: String },
    /// The node of `task` ended and Alondra recorded it.
    NodeEnd { task: String },
    /// The daemon goes as a crash takes it and starts again over the same home: the SDK hosts
    /// run on, and the new daemon takes them over. The hosts of the tasks of `lose` die with it,
    /// and no new host can start for those tasks until `host_can_start`. Those of them also in
    /// `full_disk` die as a full disk left one: their journal ends with their last event twice.
    Restart { #[serde(default)] lose: Vec<String>, #[serde(default)] full_disk: Vec<String> },
    /// The daemon is replaced by this release, which installs its own host: the hosts the older
    /// release started run on, and the new daemon takes them over or refuses them.
    Update,
    /// A new host can start for `task` again.
    HostCanStart { task: String },
    /// The user presses the option `option` of the card the daemon shows for `task` whose id starts
    /// with `card`, as the app does it from the view.
    Answer { card: String, option: String, #[serde(default)] text: Option<String> },
    /// The orchestrator tells the user something, as `publish_information` does: a card `card`
    /// with this title and body, under no request.
    Inform { card: String, title: String, body: String },
    /// The orchestrator asks the user, as `request_action` does, to do `title` for `task`: a card
    /// `card` scoped to that task. With `refused`, the daemon must turn it down with that code, and
    /// nothing is shown.
    OrchestratorAction { task: String, card: String, title: String, #[serde(default)] refused: Option<String> },
    /// The orchestrator closes the case of the stopped node of `task` with `result`, as
    /// `settle_stall` does with `close`.
    SettleStall { task: String, result: String },
    /// The user writes `text` in the reply field of the card `card` of the home, as the app does.
    Reply { card: String, text: String },
    /// Files of the daemon's home are written as another process of the Mac writes them, each
    /// whole at once: every `path` is relative to the home and stays inside it.
    WriteHome { #[serde(rename = "file")] files: Vec<HomeFile> },
    /// The worktree Alondra makes for `task`: a linked worktree of a new repository, in its
    /// `.claude/worktrees`, holding `files`, and `outside_link`, a link to a folder outside it.
    /// With `from`, the node of `task` enters the worktree of that other task instead.
    /// With `base`, those files are committed on the repository's `main` before the worktree
    /// branches from it.
    Worktree { task: String, #[serde(default)] files: Vec<String>, #[serde(default)] outside_link: Option<String>, #[serde(default)] from: Option<String>,
        #[serde(default)] base: Vec<HomeFile>,
        /// Files written into the worktree that no commit holds, such as the image a mockup is
        /// proposed from.
        #[serde(default)] images: Vec<ProposedFile> },
    /// The node of `task` proposes the specs of its plan, as `propose_specs` does. With `refused`,
    /// the daemon must turn it down with that code.
    ProposeSpecs { task: String, level: String, files: Vec<ProposedFile>, #[serde(default)] refused: Option<String> },
    /// The user opens the editor of the proposed specs of `task`, picks `item` (the title of a spec,
    /// scenario or decision it lists to edit) and saves its form with `fields` over what it shows.
    EditSpec { task: String, item: String, fields: serde_json::Map<String, serde_json::Value> },
    /// The node of `task` asks for a change of its approved specs, as `request_spec_change` does.
    RequestSpecChange { task: String, reason: String },
    /// The implementer of `task` says its work is ready to validate, as `ready_for_validation`
    /// does; with `refused` the daemon must turn it down with that code.
    ReadyForValidation { task: String, #[serde(default)] refused: Option<String> },
    /// The validator of `task` records what one check found, as `record_validation` does.
    RecordValidation { task: String, subject: String, check: String, verdict: String, #[serde(default)] evidence: Vec<String>, refused: Option<String> },
    /// The validator of `task` ends its round without delivering, as `end_validation` does.
    EndValidation { task: String, findings: String },
    /// The node of `task` writes, for everything the spec engine lists as not validated in its
    /// worktree, the ledger entry `alondra spec run` and `alondra spec record` would write with
    /// verdict met, and commits it with `message`.
    SpecLedger { task: String, message: String },
    /// The node of `task` writes `files` in its worktree and commits them, with `message`; a file
    /// without text is removed.
    Commit { task: String, files: Vec<ProposedFile>, message: String },
    /// The node of `task` pushes the commit its worktree is on to PR `number`: the PR's head on
    /// GitHub becomes that commit.
    Push { task: String, number: u64 },
    /// The worktree of `task` moves to the revision `to` without moving any PR.
    Checkout { task: String, to: String },
    /// The node of `task` asks permission to run `command` in Bash from `cwd`, a folder of the
    /// task's worktree (its top when absent), as Claude Code asks; `request` names the request.
    Permission { task: String, request: String, command: String, #[serde(default)] cwd: Option<String> },
    /// The node of `task` asks to leave plan mode with `plan`, as Claude Code's `ExitPlanMode` asks;
    /// `request` names the request. Its turn goes on until a `turn_end`.
    Plan { task: String, request: String, plan: String },
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
    /// The repository moves to `to`: its pull requests keep their numbers, and the old name
    /// redirects to the new one.
    MoveRepository { to: String },
    /// Someone creates the repository `repository` again, under a name that held a moved or deleted
    /// one, and opens a pull request `number` there: the old name no longer redirects, and the
    /// numbers start again.
    ReuseName { repository: String, number: u64, head: String, base: String },
    /// The base asks for a review the PR has not got: GitHub blocks its merge.
    RequireReview { number: u64 },
    /// The review arrives: GitHub lets the PR merge.
    ApproveReview { number: u64 },
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
    /// Whether the gauges of the screens read the accounts as the settled daemon holds them. The
    /// sample of the Mac takes the accounts when it is published, and a usage probe that woke at
    /// the same instant of the clock commits before or after that, as the threads run: a
    /// checkpoint that follows a probe sets this to read what the daemon holds, not what won.
    #[serde(default)]
    pub settled_accounts: bool,
    #[serde(default)]
    pub expect: Vec<Expect>,
}

/// What the case itself asserts: the thing the bug broke, in words a reader checks at a glance.
#[derive(Debug, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expect {
    /// Two TUI windows sharing a paired identity keep their signed long polls open.
    SharedTuiFeed { max_requests: u64, max_verifications: usize },
    /// The calls Alondra made to GitHub that change something, in order, as `gh` is typed.
    GhCalls { calls: Vec<String> },
    PrMergedInto { pr: u64, branch: String },
    /// Pull request `pr` of `repository` is `state` (`open`, `closed` or `merged`) on GitHub.
    PrStateIn { repository: String, pr: u64, state: alondra_domain_model::PrState },
    /// The searches of open PRs and reads of their branches the daemon made, in order.
    SweepReads { reads: Vec<String> },
    TaskState { task: String, state: alondra_domain_model::TaskState },
    /// Pull request `pr` of the scenario's repository belongs to `task`.
    PrOwner { pr: u64, task: String },
    /// Each start-problem decision pass reads at most this many times the card population.
    StartProblemReads { max_passes: u64 },
    /// Building the committed read index scans card history at most this many times.
    ReadIndexCards { max_passes: u64 },
    /// Task list and detail rendering reads at most this many cards from domain history.
    TaskPresentationReads { max_reads: u64 },
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
    /// What the daemon answered the permission `request` of the node of `task`: `allow`, `deny`,
    /// or `none` while it waits on the user's card.
    Answered { task: String, request: String, answer: String },
    /// The file `path` of the worktree of `task` holds `text`, or, without `text`, does not exist.
    WorktreeFile { task: String, path: String, #[serde(default)] text: Option<String>, #[serde(default)] png: Option<[u32; 2]> },
    /// How many SDK hosts the daemon started for `task`, how many of them run now, and how many
    /// Claude sessions they held between them.
    Hosts { task: String, started: usize, running: usize, sessions: usize },
    /// The first request of the last host started for `task` wrote at most `max` tokens to the
    /// prompt cache.
    CacheWritten { task: String, max: u64 },
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
