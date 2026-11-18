//! Carries a scenario out: assembles the daemon over fake edges on a virtual clock, runs the
//! steps, and reads the checkpoints.

use alondra_engine_sdk::sdk_owner::SdkOwners as _;
use crate::checkpoint::{self, Files};
use crate::claude_peer::{Activity, Seen};
use crate::error::ScenarioError;
use crate::format::{Checkpoint, Expect, GithubChange, Scenario, Step, parse_duration};
use crate::http::{PHONE_SECRET, Phone};
use alondra_agent_runtime::owned_host::{HostLauncher, HostSpawn, Launcher};
use crate::claude_host::{ApiError, ClaudeEdge};
use alondra_domain_rules::{
    Actor, CardSpec, Command, CommandEnvelope, DeliveryTarget, LaunchPlan, MergeMethod, NodeEnd, PROTOCOL_VERSION, StackDependency,
};
use alondra_views::action_id::ActionId;
use alondra_daemon_engine::github_world::GithubWorld;
use alondra_daemon_engine::machine::MachineSampler;
use alondrad::assemble::{self, Assembled, Backend, GithubSeams, Pushes, Seams};
use alondrad::push_transport::{RecordedPushes, SentPush};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the daemon must stay unchanged, in virtual seconds, for it to count as resting. The
/// scenario daemon polls every couple of seconds, so this outlasts every loop's period.
const QUIET: u64 = 30;
/// Where the pushes of the scenario's phone go: the push binding the app registers, 32 bytes.
const PUSH_DESTINATION: &str = "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ";
const REPOSITORY_HOST: &str = "github.com";

pub struct Outcome {
    /// Every checkpoint passed, in order, with the files it read.
    pub checkpoints: Vec<(String, Files)>,
}

/// The Mac of a scenario: always rested, with room for everything. It counts the samples the
/// daemon takes of it.
#[derive(Clone, Default)]
struct CalmMac(Arc<std::sync::atomic::AtomicU64>);
impl MachineSampler for CalmMac {
    fn sample(&self) -> alondra_core::Machine {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        alondra_core::Machine {
            at: alondra_core::now_millis(),
            memory_total: 16 << 30,
            memory_available: 12 << 30,
            memory_free_percent: Some(75),
            memory_pressure: None,
            compressor: None,
            load: [0.5; 3],
            cpus: 8,
            cpu_usage: 5.0,
            disk_total: 1 << 40,
            disk_available: 1 << 39,
            disk_purgeable: 0,
            unreliable: false,
        }
    }
}

/// Starts no process: a scenario that reached for one has left the world it controls.
struct RefusingLauncher(Arc<Mutex<Vec<String>>>);
impl HostLauncher for RefusingLauncher {
    fn spawn(&self, spawn: HostSpawn<'_>) -> std::io::Result<std::process::Child> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("process {}", spawn.executable.display()));
        Err(std::io::Error::other("a scenario starts no process"))
    }
}

pub fn run(scenario: &Scenario, accepted: Option<&Path>) -> Result<Outcome, ScenarioError> {
    run_with(scenario, accepted, |_| ())
}

/// Runs the scenario as far as it goes: the checkpoints it passed, and the failure that stopped it.
pub fn run_partial(scenario: &Scenario) -> (Outcome, Option<ScenarioError>) {
    match run_inner(scenario, None, |_| ()) {
        Ok((outcome, failure)) => (outcome, failure),
        Err(error) => (Outcome { checkpoints: Vec::new() }, Some(error)),
    }
}

/// `run`, with `probe` handed the launcher the daemon starts processes through, once it is
/// assembled: how a test of the harness makes something reach for a process.
pub fn run_with(scenario: &Scenario, accepted: Option<&Path>, probe: impl FnOnce(&Launcher)) -> Result<Outcome, ScenarioError> {
    match run_inner(scenario, accepted, probe)? {
        (outcome, None) => Ok(outcome),
        (_, Some(failure)) => Err(failure),
    }
}

fn run_inner(
    scenario: &Scenario,
    accepted: Option<&Path>,
    probe: impl FnOnce(&Launcher),
) -> Result<(Outcome, Option<ScenarioError>), ScenarioError> {
    if std::env::var("TZ").as_deref() != Ok("UTC") {
        return Err(ScenarioError::Invalid("scenarios run with TZ=UTC: the views show local time, which differs from Mac to Mac".into()));
    }
    let runtime = alondra_core::clock::VirtualRuntime::new(EPOCH_MILLIS).map_err(|error| ScenarioError::Invalid(error.to_string()))?;
    runtime.block_on(execute(scenario, accepted, probe))
}

/// 2026-11-15 09:00 UTC: every scenario starts at the same instant.
const EPOCH_MILLIS: i64 = 1_794_733_200_000;

/// The stand-in for the provider's host: what it saw, and how much it is still moving.
struct Peer {
    seen: Arc<Mutex<Seen>>,
    activity: Activity,
    hosts: usize,
}

/// Only a hang ends a wait for the hosts: a run whose hosts stop short of their transcripts fails
/// naming what is left, instead of waiting for the test's own limit.
const WATCHDOG: Duration = Duration::from_secs(120);

/// Real milliseconds a wait for the Claude hosts may last: a daemon that never comes to rest ends
/// the run with what kept moving, not a hang.
const PATIENCE_MS: u64 = 20_000;
/// How often the daemon samples the Mac and the accounts' usage for the gauges the apps show: by
/// then a scenario has started.
const MACHINE_SAMPLE: Duration = Duration::from_secs(10);
/// The 2 ms polls with no frame between the daemon and the hosts, and no commit of the daemon, that
/// make a moment of calm.
const CALM_POLLS: u64 = 25;
/// The polls that outlast the longest wait of a host's owner between two looks at the inputs it
/// holds (250 ms, while a turn runs).
const OWNER_WAKE_POLLS: u64 = 150;

struct Context<'a> {
    peer: Option<Peer>,
    scenario: &'a Scenario,
    accepted: Option<&'a Path>,
    config: alondra_core::config::Config,
    /// The provider of the Claude nodes, when the case runs any.
    claude: Option<ClaudeEdge>,
    mac: CalmMac,
    daemon: Option<Assembled>,
    github: Arc<GithubWorld>,
    pushes: RecordedPushes,
    leaks: Arc<Mutex<Vec<String>>>,
    phone: Phone,
    heads: BTreeMap<u64, String>,
    /// The capability the daemon issued to each task's node, by task.
    capabilities: BTreeMap<String, String>,
    /// The inputs the user's messages became, by task, in the order the user wrote them.
    told: BTreeMap<String, std::collections::VecDeque<(String, String)>>,
    request: Option<String>,
    commands: u64,
    spent: Duration,
    budget: Duration,
    outcome: Outcome,
}

async fn execute(scenario: &Scenario, accepted: Option<&Path>, probe: impl FnOnce(&Launcher)) -> Result<(Outcome, Option<ScenarioError>), ScenarioError> {
    // Tokio jumps a paused clock to its next timer whenever the runtime waits, as it does on a view
    // read over HTTP, unless a blocking task runs: one runs for the whole scenario, so virtual time
    // moves only when a step moves it.
    let (thaw, frozen) = std::sync::mpsc::channel::<()>();
    let freeze = tokio::task::spawn_blocking(move || frozen.recv().ok());
    let budget = parse_duration(&scenario.max_virtual)?;
    let home = tempfile::Builder::new().prefix("sc-").tempdir_in(short_root()).map_err(|error| ScenarioError::Invalid(error.to_string()))?;
    let invalid = |error: &dyn std::fmt::Display| ScenarioError::Invalid(error.to_string());
    let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|listener| listener.local_addr()).map_err(|e| invalid(&e))?.port();
    let mut config = alondra_core::config::Config::resolve(
        home.path().canonicalize().map_err(|e| invalid(&e))?.join("home"),
        &alondra_core::config::FileConfig::default(),
        |_| None,
        || Some("Ana".to_owned()),
    );
    std::fs::create_dir_all(&config.home).map_err(|e| invalid(&e))?;
    config.port = port;
    config.listen = [127, 0, 0, 1].into();
    config.wake = false;
    config.auto_update = false;
    config.fast_timing = true;
    config.sandbox = false;
    config.private_names = Vec::new();

    let github = Arc::new(GithubWorld::new());
    let pushes = RecordedPushes::default();
    let leaks = Arc::new(Mutex::new(Vec::new()));
    let codex = codex_transcripts(scenario);
    write_accounts(&config.home, &scenario.world.accounts).map_err(|e| invalid(&e))?;
    let mut claude = None;
    let (launcher, backend, peer) = if codex.is_empty() {
        let edge = ClaudeEdge::new(&config.home, leaks.clone());
        let launcher = Launcher::new(edge.clone());
        let backend = claude_backend(&config.home, &scenario.world.release).map_err(|e| invalid(&e))?;
        claude = Some(edge);
        (launcher, Backend::Given(Some(backend)), None)
    } else {
        let case = scenario.case.as_deref().ok_or_else(|| ScenarioError::Invalid("a step that starts a provider reads the transcripts of its case".into()))?;
        let peer = crate::codex_peer::launcher(case, &codex)?;
        let (seen, activity) = (peer.seen.clone(), peer.activity.clone());
        let launcher = Launcher::new(peer);
        let backend = codex_backend(&config.home).map_err(|e| invalid(&e))?;
        (launcher, Backend::Given(Some(backend)), Some(Peer { seen, activity, hosts: codex.len() }))
    };
    probe(&launcher);
    let mac = CalmMac::default();
    let seams = seams_of(&github, &pushes, &mac, launcher, backend);
    alondrad::scenario_phone::prepare(&config.home).map_err(|e| invalid(&format!("{e:#}")))?;
    let domain = alondra_engine_sdk::engine_tests::open(&config.home).map_err(|e| invalid(&format!("{e:#}")))?;
    let daemon = assemble::start_over(&config, domain, seams).await.map_err(|e| invalid(&format!("{e:#}")))?;
    let phone = Phone::pair(&daemon, std::net::SocketAddr::new(config.listen, config.port)).map_err(|e| invalid(&e))?;
    let destination = alondrad::scenario_phone::Phone::new(phone.device(), PHONE_SECRET).map_err(|e| invalid(&format!("{e:#}")))?;
    daemon.register_phone_push(&destination, PUSH_DESTINATION).await.map_err(|e| invalid(&format!("{e:#}")))?;
    let mut context = Context {
        peer,
        scenario,
        accepted,
        config,
        claude,
        mac,
        daemon: Some(daemon),
        github,
        pushes,
        leaks,
        phone,
        heads: BTreeMap::new(),
        capabilities: BTreeMap::new(),
        told: BTreeMap::new(),
        request: None,
        commands: 0,
        spent: Duration::ZERO,
        budget,
        outcome: Outcome { checkpoints: Vec::new() },
    };
    let result = match context.accounts_checked().await {
        Ok(()) => steps(&mut context).await,
        Err(error) => Err(error),
    };
    drop(thaw);
    freeze.await.ok();
    if let Some(daemon) = &context.daemon {
        daemon.tasks.abort();
        let domain = daemon.domain.clone();
        tokio::task::spawn_blocking(move || domain.stop_sdk_owners()).await.ok();
    }
    if let Some(edge) = &context.claude {
        edge.end_all();
    }
    Ok((context.outcome, result.err()))
}

fn seams_of(github: &Arc<GithubWorld>, pushes: &RecordedPushes, mac: &CalmMac, launcher: Launcher, backend: Backend) -> Seams {
    Seams {
        github: GithubSeams::of(github.clone()),
        push: Pushes::new(pushes.clone()),
        machine: Arc::new(mac.clone()),
        launcher,
        backend,
        world: assemble::World::Isolated,
    }
}

async fn steps(context: &mut Context<'_>) -> Result<(), ScenarioError> {
    for (index, step) in context.scenario.steps.iter().enumerate() {
        context.step(index, step).await?;
        if context.daemon.is_some() {
            context.settle().await?;
        }
        context.peer_failure()?;
        if let Some(what) = context.leaks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).first() {
            return Err(ScenarioError::Leak(what.clone()));
        }
    }
    Ok(())
}

/// The transcripts of every host the scenario's `codex_node` steps start, in order.
fn codex_transcripts(scenario: &Scenario) -> Vec<String> {
    scenario
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::CodexNode { transcripts, .. } => Some(transcripts.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn runs_on_codex(scenario: &Scenario, task: &str) -> bool {
    scenario.steps.iter().any(|step| matches!(step, Step::CodexNode { task: named, .. } if named == task))
}

/// A folder short enough for a host's socket to live deep inside it.
fn short_root() -> std::path::PathBuf {
    let temporary = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).and_then(|path| path.canonicalize().ok());
    match temporary {
        Some(root) if root.as_os_str().len() <= 32 => root,
        _ => std::path::PathBuf::from("/tmp"),
    }
}

fn codex_backend(home: &Path) -> Result<alondra_engine_sdk::sdk_owner::backend::SdkBackend, String> {
    sdk_backend(home, alondra_core::accounts::Provider::Codex, "codex", &alondra_engine_sdk::sdk_owner::backend::this_release())
}

/// The backend of a daemon whose Claude nodes run `release`.
fn claude_backend(home: &Path, release: &str) -> Result<alondra_engine_sdk::sdk_owner::backend::SdkBackend, String> {
    sdk_backend(home, alondra_core::accounts::Provider::Claude, "claude", release)
}

/// The backend of a daemon: three files that stand where the
/// host and the native executable stand, named by their digests as a real configuration names them.
/// Each release installs its own host, so the files of two releases differ.
fn sdk_backend(
    home: &Path,
    provider: alondra_core::accounts::Provider,
    name: &str,
    release: &str,
) -> Result<alondra_engine_sdk::sdk_owner::backend::SdkBackend, String> {
    use alondra_engine_sdk::sdk_owner::backend::{SdkBackend, digest};
    let file = |kind: &str| -> Result<(std::path::PathBuf, String), String> {
        let path = home.join(format!("{name}-{kind}-{release}"));
        std::fs::write(&path, format!("{name} {kind} {release}")).map_err(|e| e.to_string())?;
        let sha = digest(&path).map_err(|e| format!("{e:#}"))?;
        Ok((path, sha))
    };
    let ((node, node_sha256), (native, native_sha256)) = (file("host")?, file("native")?);
    let environment = BTreeMap::from([("HOME".to_owned(), home.display().to_string()), ("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
    Ok(SdkBackend {
        provider,
        origin: None,
        host: node.clone(),
        host_sha256: node_sha256.clone(),
        node,
        node_sha256,
        native,
        native_sha256,
        host_environment: environment.clone(),
        native_environment: environment,
        confinement: None,
        state_root: home.join("sdk"),
        io_timeout_seconds: 30,
        installed_native: false,
        release: release.to_owned(),
    })
}

/// The accounts of the scenario's world, as the user set them up in Settings: each but the
/// default one has its own folder for Claude's configuration.
fn write_accounts(home: &Path, accounts: &[String]) -> Result<(), String> {
    use alondra_core::accounts::{AccountId, AccountsConfig, Provider, ProviderAccount};
    let mut settings = AccountsConfig::fresh();
    settings.connect(Provider::Claude);
    for id in accounts {
        let id = AccountId::parse(id)?;
        if let Some(folder) = alondra_core::accounts::folder(home, &id) {
            std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        }
        let mut name = id.as_str().to_owned();
        name[..1].make_ascii_uppercase();
        settings.claude.push(ProviderAccount { id, name, enabled: true, billing: Default::default() });
    }
    std::fs::write(alondra_core::options::path_in(home), settings.to_toml()).map_err(|e| e.to_string())
}

fn agent_of(task: &str) -> String {
    format!("agent-{task}")
}


impl Context<'_> {
    fn daemon(&self) -> &Assembled {
        self.daemon.as_ref().expect("a daemon is assembled between steps")
    }

    /// The id the daemon gives the scenario's PR `number`: its host, repository and number.
    fn pr_id(&self, number: u64) -> alondra_domain_rules::PrId {
        alondra_domain_rules::hosted_pr_id(REPOSITORY_HOST, &self.scenario.world.repository, number)
    }

    /// The subject a case names a card by: `pr7` stands for the card of the scenario's PR 7.
    fn subject(&self, named: &str) -> String {
        match named.strip_prefix("pr").and_then(|number| number.parse().ok()) {
            Some(number) => self.pr_id(number).0,
            None => named.to_owned(),
        }
    }

    fn failed(index: usize, reason: impl std::fmt::Display) -> ScenarioError {
        ScenarioError::Step { index, reason: reason.to_string() }
    }

    fn head(&self, index: usize, number: u64) -> Result<String, ScenarioError> {
        self.heads.get(&number).cloned().ok_or_else(|| Self::failed(index, format!("PR {number} was not opened on GitHub before")))
    }

    /// Runs a command as `actor`; a revision that moved under it is read again.
    fn command(&mut self, index: usize, actor: Actor, command: Command) -> Result<(), ScenarioError> {
        self.commands += 1;
        let id = format!("scenario-{}", self.commands);
        let mut last = String::new();
        for _ in 0..8 {
            let state = self.daemon().domain.state().map_err(|e| Self::failed(index, e))?;
            let envelope = CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
                command_id: id.clone().into(),
                expected_revision: state.revision,
                command: command.clone(),
            };
            match self.daemon().domain.execute(&actor, &envelope, None, || Ok(())) {
                Ok(_) => return Ok(()),
                Err(error) => last = error.to_string(),
            }
        }
        Err(Self::failed(index, format!("the domain refused {command:?}: {last}")))
    }

    async fn step(&mut self, index: usize, step: &Step) -> Result<(), ScenarioError> {
        let orchestrator = || Actor::Orchestrator { generation: 0 };
        match step {
            Step::Request { text, task, title } => {
                self.command(index, Actor::System, Command::RegisterAgent { agent_id: agent_of(task).into(), name: task.clone(), at: 0 })?;
                let request = self.new_request(index, text).await?;
                self.command(index, orchestrator(), Command::TitleRequest { request_id: request.clone().into(), title: title.clone() })?;
                self.request = Some(request);
                self.create_task(index, task, title, None)?;
            }
            Step::Task { id, title, stack_on } => {
                self.command(index, Actor::System, Command::RegisterAgent { agent_id: agent_of(id).into(), name: id.clone(), at: 0 })?;
                let stack = match stack_on {
                    Some(on) => Some(StackDependency {
                        upstream_task: on.upstream.clone().into(),
                        presentation_id: on.presentation.clone().into(),
                        pr_id: self.pr_id(on.pr),
                        head: self.head(index, on.pr)?,
                    }),
                    None => None,
                };
                self.create_task(index, id, title, stack)?;
            }
            Step::RegisterPr { task, number } => {
                let head = self.head(index, *number)?;
                self.node_command(index, task, &format!("/tasks/{task}/pull-requests"), None, Command::RegisterPr {
                    task_id: task.clone().into(),
                    pr_id: self.pr_id(*number),
                    host: REPOSITORY_HOST.into(),
                    repository: self.scenario.world.repository.clone(),
                    number: *number,
                    head,
                    base: self.github.base_of(*number),
                    allowed_methods: vec![MergeMethod::Squash, MergeMethod::Merge],
                }).await?;
            }
            Step::Present { task, id, prs, body, refused } => {
                let mut targets = Vec::new();
                for number in prs {
                    targets.push(DeliveryTarget::PullRequest { pr_id: self.pr_id(*number), head: self.head(index, *number)? });
                }
                self.node_command(index, task, &format!("/tasks/{task}/results"), refused.as_deref(), Command::PresentResult {
                    checklist: Vec::new(),
                    attachments: Vec::new(),
                    task_id: task.clone().into(),
                    interaction_id: id.clone().into(),
                    body: body.clone(),
                    targets,
                    is_final: true,
                    nothing_to_review: false,
                    card: CardSpec::delivery("Review the result", &[], true),
                }).await?;
            }
            Step::StackReady { task, presentation, pr } => {
                let head = self.head(index, *pr)?;
                self.node_command(index, task, &format!("/tasks/{task}/stack-readiness"), None, Command::PublishStackReadiness {
                    task_id: task.clone().into(),
                    presentation_id: presentation.clone().into(),
                    pr_id: self.pr_id(*pr),
                    head,
                    ready: true,
                }).await?;
            }
            Step::CodexNode { task, .. } => self.start_codex_node(index, task).await?,
            Step::Github(change) => match change {
                GithubChange::OpenPr { number, head, base, draft } => {
                    let commit = self.github.open_pr(*number, head, base);
                    self.github.set_draft(*number, *draft);
                    self.heads.insert(*number, commit);
                }
                GithubChange::MarkReady { number } => self.github.set_draft(*number, false),
                GithubChange::MergeExternally { number } => self.github.merge_externally(*number),
                GithubChange::CloseExternally { number } => self.github.close_externally(*number),
            },
            Step::Press { presentation, action, text, screen } => self.press(index, presentation, action, text.as_deref(), screen.as_deref().unwrap_or("home")).await?,
            Step::Answer { card, option, text } => self.answer(index, card, option, text.as_deref()).await?,
            Step::Node { task, account } => self.start_claude_node(index, task, account).await?,
            Step::TurnEnd { task } => {
                self.edge(index)?.end_turn(task, None).map_err(|why| Self::failed(index, why))?;
                self.settle().await?;
            }
            Step::ApiError { task, error, status } => {
                let error = api_error(index, error, *status)?;
                self.edge(index)?.end_turn(task, Some(error)).map_err(|why| Self::failed(index, why))?;
                self.settle().await?;
            }
            Step::ProviderDown { error, status, tasks } => {
                let error = api_error(index, error, *status)?;
                self.edge(index)?.set_outage(Some((error, tasks.clone())));
            }
            Step::ProviderUp => self.edge(index)?.set_outage(None),
            Step::LoginExpired { account } => self.edge(index)?.set_signed_in(account, false),
            Step::LoginRestored { account } => self.edge(index)?.set_signed_in(account, true),
            Step::HostExit { task } => {
                self.edge(index)?.kill(task);
                self.settle().await?;
            }
            Step::PressTask { task, action } => self.press_task(index, task, action).await?,
            Step::NodeLimit { count } => {
                self.daemon().limit_nodes(*count);
                self.settle().await?;
            }
            Step::Tell { task, text } => self.tell(index, task, text).await?,
            Step::ReadMessage { task } => {
                let text = self.message_waiting(index, task)?;
                self.edge(index)?.read(task, &text).map_err(|why| Self::failed(index, why))?;
                self.settle().await?;
            }
            Step::CleanupDone { task } => {
                self.node_command(index, task, &format!("/tasks/{task}/cleanup"), None, Command::ConfirmCleanup { task_id: task.clone().into(), discard: Vec::new() }).await?;
            }
            Step::NodeEnd { task } => {
                self.command(index, Actor::System, Command::RecordNodeEnd { task_id: task.clone().into(), end: NodeEnd::Closed, agent_id: Some(agent_of(task).into()) })?;
            }
            Step::Restart { lose } => self.restart(index, lose, None).await?,
            Step::Update => self.restart(index, &[], Some(alondra_engine_sdk::sdk_owner::backend::this_release())).await?,
            Step::HostCanStart { task } => self.edge(index)?.set_startable(task, true),
            Step::Advance { by } => self.pass(parse_duration(by)?, index).await?,
            Step::Rest { max } => self.rest(parse_duration(max)?, index).await?,
            Step::Checkpoint(checkpoint) => self.checkpoint(index, checkpoint).await?,
        }
        Ok(())
    }

    /// The first thing that went wrong between the daemon and the provider's host, if anything did.
    fn peer_failure(&self) -> Result<(), ScenarioError> {
        let Some(peer) = &self.peer else { return Ok(()) };
        match peer.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).failure.clone() {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }

    /// The Claude hosts answer the daemon in real time, beside the virtual clock: waits, in the
    /// runtime's blocking pool (which keeps the clock where it is), until no takeover of a host is
    /// under way, the daemon is attached to every host that runs and acknowledged what each said,
    /// and neither a frame nor a commit moved for a moment, so the next step reads what the daemon
    /// did with them. Past the patience the run ends, naming what kept moving.
    async fn settle(&self) -> Result<(), ScenarioError> {
        if let Some(edge) = &self.claude {
            let edge = edge.clone();
            let domain = self.daemon.as_ref().map(|daemon| daemon.domain.clone());
            let watched = edge.clone();
            let retaking = {
                let domain = domain.clone();
                move || domain.as_ref().is_some_and(|domain| domain.retaking())
            };
            // While a turn runs, the owner of its host looks for new inputs only between reads of
            // the host, which wait up to `OWNER_WAKE`: an input the daemon holds for a host that
            // was not offered it yet may still go out once that wait ends.
            let undelivered = move || {
                let Some(state) = domain.as_ref().and_then(|domain| domain.state().ok()) else { return false };
                watched.turns().into_iter().any(|(task, offered)| {
                    state.lifecycle_of(&task.into()).is_ok_and(|lifecycle| lifecycle.pending_inputs().len() > offered)
                })
            };
            let revision = {
                let domain = self.daemon.as_ref().map(|daemon| daemon.domain.clone());
                move || domain.as_ref().map_or(0, |domain| domain.read().state().revision)
            };
            let unsettled = tokio::task::spawn_blocking(move || {
                let (mut last, mut calm, mut patience) = ((edge.activity(), revision()), 0, PATIENCE_MS);
                loop {
                    std::thread::sleep(Duration::from_millis(2));
                    let now = (edge.activity(), revision());
                    if now != last {
                        (last, calm) = (now, 0);
                    } else {
                        calm += 1;
                    }
                    let enough = if undelivered() { OWNER_WAKE_POLLS } else { CALM_POLLS };
                    let (committed, retaking) = (edge.committed(), retaking());
                    if calm >= enough && committed && !retaking {
                        return None;
                    }
                    patience = patience.saturating_sub(2);
                    if patience == 0 {
                        let mut moving = Vec::new();
                        if calm < enough {
                            moving.push("frames or commits");
                        }
                        if !committed {
                            moving.push("a host the daemon did not attach to or acknowledge");
                        }
                        if retaking {
                            moving.push("a takeover of a host");
                        }
                        return Some(moving.join(", "));
                    }
                }
            })
            .await
            .unwrap_or_else(|error| Some(error.to_string()));
            if let Some(what) = unsettled {
                return Err(ScenarioError::Unsettled(format!("{what}, after {} s", PATIENCE_MS / 1000)));
            }
        }
        self.settle_peer().await
    }

    /// The hosts and the daemon's owners of them answer each other at once, without virtual time
    /// passing: once the first host started, the runner waits, on each exchange of frames, until
    /// every host of the scenario played its transcript and the daemon committed what each said, so
    /// the next step reads what the daemon did with them. The wait is in the runtime's blocking
    /// pool, which keeps the clock where it is.
    async fn settle_peer(&self) -> Result<(), ScenarioError> {
        let Some(peer) = &self.peer else { return Ok(()) };
        let (activity, seen, hosts) = (peer.activity.clone(), peer.seen.clone(), peer.hosts);
        let settled = tokio::task::spawn_blocking(move || {
            let reached = || {
                let seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                seen.remaining.is_empty() || seen.failure.is_some() || seen.played(hosts)
            };
            activity.wait_until(reached, WATCHDOG)
        })
        .await
        .unwrap_or(false);
        if settled {
            return Ok(());
        }
        let seen = peer.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Err(ScenarioError::HostsStalled { started: seen.remaining.len(), expected: hosts, remaining: seen.remaining.clone() })
    }

    /// The task's node starts on Codex as a launch in production starts it: the daemon picks the
    /// account and model, writes the mission and starts the host it supervises.
    async fn start_codex_node(&mut self, index: usize, task: &str) -> Result<(), ScenarioError> {
        use alondra_core::accounts::{AccountId, AccountKey, AccountState, AccountStatus, Provider};
        use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, ModelKey, ModelProfile, ModelSelection, TaskSelection};
        // The daemon's first look at its accounts finishes before the scenario says which exist.
        for _ in 0..5 {
            self.pass(Duration::from_secs(1), index).await?;
        }
        let mut settings = alondra_core::accounts::AccountsConfig::fresh();
        settings.connect(Provider::Codex);
        std::fs::write(alondra_core::options::path_in(&self.config.home), settings.to_toml()).map_err(|e| Self::failed(index, e))?;
        let state = self.daemon().domain.state().map_err(|e| Self::failed(index, e))?;
        let mut accounts = state.accounts.clone();
        accounts.push(AccountStatus {
            provider: Provider::Codex, id: AccountId::default_account(), name: "Codex".into(), state: AccountState::SignedIn,
            native_identity: Some("demo-workspace".into()), email: Some("ana@example.com".into()), plan: None, billing: Default::default(),
            sign_ins: 0, profile: None,
        });
        self.command(index, Actor::System, Command::RecordAccounts { accounts, reserve: state.account_reserve })?;
        let key = AccountKey { provider: Provider::Codex, account: AccountId::default_account() };
        self.command(index, Actor::System, Command::RecordModelCatalog { catalog: AccountModelCatalog {
            account: key.clone(), revision: "demo-catalog".into(), measured_at: 1, native_default: Some("demo-model".into()),
            models: vec![CatalogModel {
                model: "demo-model".into(), display_name: "Demo model".into(), reasoning_efforts: vec!["high".into()],
                default_reasoning_effort: Some("high".into()), context_window: None, input_modalities: vec!["text".into()],
                is_default: true, hidden: false,
            }],
        } })?;
        alondra_daemon_engine::accounts::finish_initial_check(&self.daemon().domain);
        let model = ModelKey { provider: Provider::Codex, model: "demo-model".into() };
        let mut selection = TaskSelection::automatic(ModelProfile::General);
        selection.request_model(Some(model.clone()));
        selection.effective = Some(ModelSelection { model, reasoning_effort: Some("high".into()), catalog_revision: "demo-catalog".into() });
        selection.account = Some(key);
        selection.native_tool_denials = Some(Vec::new());
        let home = &self.config.home;
        let (directory, plugin) = (home.join("work"), home.join("plugin"));
        std::fs::create_dir_all(plugin.join("skills/alondra")).and_then(|()| std::fs::create_dir_all(&directory)).map_err(|e| Self::failed(index, e))?;
        std::fs::write(plugin.join("skills/alondra/SKILL.md"), "You are an Alondra task node. Follow the task request and report the result through Alondra.")
            .map_err(|e| Self::failed(index, e))?;
        let title = state.tasks.get(&task.to_owned().into()).map(|task| task.title.clone()).unwrap_or_default();
        let options = alondra_engine_sdk::sdk_owner::LaunchOptions {
            provider: Provider::Codex, model: Some("demo-model".into()), effort: Some("high".into()), selection: Some(selection),
            ..Default::default()
        };
        alondra_engine_sdk::sdk_owner::launch(&self.daemon().domain, alondra_engine_sdk::sdk_owner::Launch {
            task: task.to_owned().into(), name: task.to_owned(), directory: directory.display().to_string(), prompt: title, plugin, options,
            model: None, account: AccountId::default_account(),
        })
        .await
        .map_err(|error| Self::failed(index, format!("the node did not start: {}", error.message)))?;
        Ok(())
    }

    fn create_task(&mut self, index: usize, task: &str, title: &str, stack: Option<StackDependency>) -> Result<(), ScenarioError> {
        let request = self.request.clone().ok_or_else(|| Self::failed(index, "a task needs a request before it"))?;
        self.command(index, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
            task_id: task.to_owned().into(),
            request_id: request.into(),
            title: title.to_owned(),
            agent_id: (!runs_on_codex(self.scenario, task) && !self.runs_on_a_provider(task)).then(|| agent_of(task).into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack,
            launch: Some(LaunchPlan::new("node", "/work")),
            plan_items: None,
            continues: None,
        })
    }

    fn pending_inputs(&self, index: usize, task: &str) -> Result<Vec<String>, ScenarioError> {
        let state = self.daemon().domain.state().map_err(|e| Self::failed(index, e))?;
        let lifecycle = state.lifecycle_of(&task.to_owned().into()).map_err(|e| Self::failed(index, e))?;
        Ok(lifecycle.pending_inputs().into_iter().map(|input| input.as_str().to_owned()).collect())
    }

    /// The daemon's first look at the sign-in of the Claude accounts goes through the edge as it
    /// starts: the scenario begins once it is done and the machine was sampled after it, which is
    /// when the apps' gauges show those accounts.
    async fn accounts_checked(&mut self) -> Result<(), ScenarioError> {
        use alondra_core::accounts::{AccountState, Provider};
        if self.claude.is_none() {
            return Ok(());
        }
        let domain = self.daemon().domain.clone();
        let checked = tokio::task::spawn_blocking(move || {
            let done = || {
                alondra_daemon_engine::accounts::require_initial_check(&domain).is_ok()
                    && domain.read().state().accounts.iter().any(|account| account.provider == Provider::Claude && account.state != AccountState::Unchecked)
            };
            for _ in 0..PATIENCE_MS / 2 {
                if done() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            done()
        })
        .await
        .unwrap_or(false);
        if !checked {
            return Err(ScenarioError::Invalid("the daemon never finished its first look at the Claude accounts".into()));
        }
        // The daemon samples the Mac as it starts, and again a sampling period later: that second
        // sample comes after the accounts were checked, whenever the first one came.
        while self.mac.0.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            if self.spent > MACHINE_SAMPLE * 2 {
                return Err(ScenarioError::Invalid("the daemon did not sample the Mac a second time".into()));
            }
            self.pass(Duration::from_secs(1), 0).await?;
        }
        Ok(())
    }

    fn edge(&self, index: usize) -> Result<&ClaudeEdge, ScenarioError> {
        self.claude.as_ref().ok_or_else(|| Self::failed(index, "a case with Codex nodes plays no Claude"))
    }

    /// The task's node starts on Claude as a launch in production starts it, on `account`: the
    /// daemon picks the model, writes the mission and starts the host it supervises, after the
    /// accounts were checked through the same edge.
    async fn start_claude_node(&mut self, index: usize, task: &str, account: &str) -> Result<(), ScenarioError> {
        use alondra_core::accounts::{AccountId, Provider};
        for _ in 0..30 {
            if alondra_daemon_engine::accounts::require_initial_check(&self.daemon().domain).is_ok() {
                break;
            }
            self.pass(Duration::from_secs(1), index).await?;
        }
        let account = AccountId::parse(account).map_err(|why| Self::failed(index, why))?;
        let home = &self.config.home;
        let (directory, plugin) = (home.join("work"), home.join("plugin"));
        std::fs::create_dir_all(plugin.join("skills/alondra")).and_then(|()| std::fs::create_dir_all(&directory)).map_err(|e| Self::failed(index, e))?;
        std::fs::write(plugin.join("skills/alondra/SKILL.md"), "You are an Alondra task node. Follow the task request and report the result through Alondra.")
            .map_err(|e| Self::failed(index, e))?;
        let state = self.daemon().domain.state().map_err(|e| Self::failed(index, e))?;
        let title = state.tasks.get(&task.to_owned().into()).map(|task| task.title.clone()).unwrap_or_default();
        let options = alondra_engine_sdk::sdk_owner::LaunchOptions { provider: Provider::Claude, ..Default::default() };
        alondra_engine_sdk::sdk_owner::launch(&self.daemon().domain, alondra_engine_sdk::sdk_owner::Launch {
            task: task.to_owned().into(), name: task.to_owned(), directory: directory.display().to_string(), prompt: title, plugin, options,
            model: None, account,
        })
        .await
        .map_err(|error| Self::failed(index, format!("the node did not start: {}", error.message)))?;
        self.settle().await?;
        if self.edge(index)?.running(task).is_none() {
            return Err(Self::failed(index, format!("the daemon started no host for {task}")));
        }
        Ok(())
    }

    /// The daemon stops as a crash stops it: its loops end and its owners let go of their hosts
    /// without stopping them, which run on. A daemon starts over the same home, of `release` when
    /// an update installs one (its hosts are files of their own), and takes the hosts over. The
    /// hosts of `lose` die with the first daemon, and no new one can start for their tasks.
    async fn restart(&mut self, index: usize, lose: &[String], release: Option<String>) -> Result<(), ScenarioError> {
        if let Some(old) = self.daemon.take() {
            old.tasks.abort();
            let domain = old.domain.clone();
            drop(old);
            tokio::task::spawn_blocking(move || domain.crash_sdk_owners()).await.ok();
        }
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        for task in lose {
            let edge = self.edge(index)?;
            edge.kill(task);
            edge.set_startable(task, false);
        }
        let backend = match (&self.claude, self.peer.is_some()) {
            (Some(edge), _) => {
                let release = release.unwrap_or_else(|| self.scenario.world.release.clone());
                let launcher = Launcher::new(edge.clone());
                Some(claude_backend(&self.config.home, &release).map(|backend| (launcher, backend)).map_err(|e| Self::failed(index, e))?)
            }
            (None, _) => None,
        };
        let (launcher, backend) = match backend {
            Some((launcher, backend)) => (launcher, Backend::Given(Some(backend))),
            None => (Launcher::new(RefusingLauncher(self.leaks.clone())), Backend::Given(None)),
        };
        let seams = seams_of(&self.github, &self.pushes, &self.mac, launcher, backend);
        let domain = alondra_engine_sdk::engine_tests::open(&self.config.home).map_err(|e| Self::failed(index, format!("{e:#}")))?;
        let daemon = assemble::start_over(&self.config, domain, seams).await.map_err(|e| Self::failed(index, format!("{e:#}")))?;
        self.daemon = Some(daemon);
        self.pass(Duration::from_secs(1), index).await
    }

    /// Whether the scenario starts a node of the task on the provider, which then admits it.
    fn runs_on_a_provider(&self, task: &str) -> bool {
        self.scenario.steps.iter().any(|step| matches!(step, Step::Node { task: named, .. } if named == task))
    }

    /// Lets virtual time pass, with the daemon's loops running through it.
    async fn pass(&mut self, by: Duration, index: usize) -> Result<(), ScenarioError> {
        self.spent += by;
        if self.spent > self.budget {
            return Err(ScenarioError::NoRest { at_step: index, spent_secs: self.spent.as_secs() });
        }
        alondra_core::clock::advance(by).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        self.settle().await?;
        self.peer_failure()?;
        match self.claude.as_ref().and_then(ClaudeEdge::trouble) {
            Some(trouble) => Err(Self::failed(index, format!("the Claude host: {}", trouble.0))),
            None => Ok(()),
        }
    }

    async fn rest(&mut self, max: Duration, index: usize) -> Result<(), ScenarioError> {
        let mut waited = Duration::ZERO;
        let mut quiet = 0;
        let mut revision = self.daemon().domain.state().map_err(|e| Self::failed(index, e))?.revision;
        while quiet < QUIET {
            if waited >= max {
                return Err(ScenarioError::NoRest { at_step: index, spent_secs: self.spent.as_secs() });
            }
            self.pass(Duration::from_secs(1), index).await?;
            waited += Duration::from_secs(1);
            let now = self.daemon().domain.state().map_err(|e| Self::failed(index, e))?.revision;
            if now == revision {
                quiet += 1;
            } else {
                revision = now;
                quiet = 0;
            }
        }
        Ok(())
    }

    async fn view(&self, screen: &str, client: &str, lang: &str) -> Result<Value, ScenarioError> {
        let key = screen.replace(':', "%3A");
        let path = format!("/views/{key}?protocol={}&client={client}&lang={lang}", alondra_views::VIEWS_PROTOCOL);
        let response = self.phone.get(&path).await.map_err(|e| ScenarioError::Invalid(e.to_string()))?;
        if response.status != 200 {
            return Err(ScenarioError::ActionRefused { action: format!("GET {path}"), code: response.status, body: response.body });
        }
        Ok(response.json())
    }

    async fn press(&mut self, index: usize, presentation: &str, verb: &str, typed: Option<&str>, screen: &str) -> Result<(), ScenarioError> {
        let (client, lang) = ("ios", "en");
        let view = self.view(screen, client, lang).await?;
        let found = find_action(&view, &self.subject(presentation), verb);
        let Some(action) = found else {
            return Err(ScenarioError::ActionMissing { presentation: presentation.to_owned(), action: verb.to_owned(), view: checkpoint::render(&view) });
        };
        self.send_action(index, &view, &action, verb, typed_input(typed), client, lang).await.map(drop)
    }

    /// The user presses `verb` on the task's screen, whether the action names the task or its node.
    async fn press_task(&mut self, index: usize, task: &str, verb: &str) -> Result<(), ScenarioError> {
        let (client, lang) = ("ios", "en");
        let screen = format!("task:{task}");
        let view = self.view(&screen, client, lang).await?;
        let state = self.daemon().domain.state().map_err(|e| Self::failed(index, e))?;
        let node = state.tasks.get(&task.to_owned().into()).and_then(|task| task.agent_id.clone()).map(|agent| agent.0);
        let found = std::iter::once(task.to_owned()).chain(node).find_map(|subject| find_action(&view, &subject, verb));
        let Some(action) = found else {
            return Err(ScenarioError::ActionMissing { presentation: screen, action: verb.to_owned(), view: checkpoint::render(&view) });
        };
        self.send_action(index, &view, &action, verb, json!({}), client, lang).await?;
        self.settle().await
    }

    /// The user writes a request on the app's new request screen; the daemon names it by the
    /// command that sent it.
    async fn new_request(&mut self, index: usize, text: &str) -> Result<String, ScenarioError> {
        let (client, lang) = ("ios", "en");
        let view = self.view("new_request", client, lang).await?;
        let Some(action) = find_action(&view, "view", "new_request") else {
            return Err(ScenarioError::ActionMissing { presentation: "new_request".into(), action: "new_request".into(), view: checkpoint::render(&view) });
        };
        self.send_action(index, &view, &action, "new_request", json!({"fields": {"text": text}}), client, lang).await
    }

    /// The user writes to the task's node from the task's screen, with the composer the app shows
    /// there: to the node that runs it, or kept on the task for the node that starts it.
    async fn tell(&mut self, index: usize, task: &str, text: &str) -> Result<(), ScenarioError> {
        let (client, lang) = ("ios", "en");
        let screen = format!("task:{task}");
        let view = self.view(&screen, client, lang).await?;
        let Some(action) = find_message(&view, task) else {
            return Err(ScenarioError::ActionMissing { presentation: screen, action: "msg".into(), view: checkpoint::render(&view) });
        };
        let before = self.pending_inputs(index, task)?;
        self.send_action(index, &view, &action, "msg", json!({"text": text}), client, lang).await?;
        let input = self
            .pending_inputs(index, task)?
            .into_iter()
            .find(|input| !before.contains(input))
            .ok_or_else(|| Self::failed(index, format!("the message to {task} left no input for its node")))?;
        self.told.entry(task.to_owned()).or_default().push_back((input, text.to_owned()));
        Ok(())
    }

    /// The text of the oldest message the user wrote to `task` that its node has not read: the
    /// input it became must still wait for the node, whatever happened since it was written.
    fn message_waiting(&mut self, index: usize, task: &str) -> Result<String, ScenarioError> {
        let (input, text) = self
            .told
            .get_mut(task)
            .and_then(std::collections::VecDeque::pop_front)
            .ok_or_else(|| Self::failed(index, format!("nobody wrote to {task}")))?;
        if !self.pending_inputs(index, task)?.contains(&input) {
            return Err(Self::failed(index, format!("the message the user wrote to {task} no longer waits for its node")));
        }
        Ok(text)
    }

    /// The capability the daemon issues to the node of `task` when it binds its session: once,
    /// kept by the node for as long as it runs, across restarts of the daemon.
    fn capability(&mut self, index: usize, task: &str) -> Result<String, ScenarioError> {
        if let Some(host) = self.claude.as_ref().and_then(|edge| edge.running(task)) {
            return Ok(host.capability.clone());
        }
        if let Some(capability) = self.capabilities.get(task) {
            return Ok(capability.clone());
        }
        let capability = self.daemon().domain.bind_agent(&agent_of(task)).map_err(|e| Self::failed(index, e))?;
        let capability = capability.expose_for_child_environment().to_owned();
        self.capabilities.insert(task.to_owned(), capability.clone());
        Ok(capability)
    }

    /// A tool of the node of `task`, called as `alondra mcp-node` calls it: the command's fields at
    /// the tool's route, with the session's capability. With `refused`, the daemon must turn it
    /// down with that code.
    async fn node_command(&mut self, index: usize, task: &str, path: &str, refused: Option<&str>, command: Command) -> Result<(), ScenarioError> {
        let capability = self.capability(index, task)?;
        self.commands += 1;
        let state = self.daemon().domain.state().map_err(|e| Self::failed(index, e))?;
        let mut body = serde_json::to_value(&command).map_err(|e| Self::failed(index, e))?;
        let fields = body.as_object_mut().ok_or_else(|| Self::failed(index, "a command is an object"))?;
        fields.remove("kind");
        fields.insert("data_epoch".into(), json!(state.data_epoch));
        fields.insert("command_id".into(), json!(format!("node-{}", self.commands)));
        fields.insert("expected_revision".into(), json!(0));
        let addr = std::net::SocketAddr::new(self.config.listen, self.config.port);
        let response = crate::http::node_post(addr, &capability, path, &body).await.map_err(|e| Self::failed(index, e))?;
        let code = (response.status >= 300).then(|| response.json()["code"].as_str().unwrap_or_default().to_owned());
        match (refused, code) {
            (None, None) => Ok(()),
            (Some(wanted), Some(code)) if wanted == code => Ok(()),
            (Some(wanted), None) => Err(Self::failed(index, format!("the daemon accepted {path}; the case expects it refused with {wanted}"))),
            (_, Some(_)) => Err(ScenarioError::ActionRefused { action: format!("POST {path}"), code: response.status, body: response.body }),
        }
    }

    /// The user presses the option of a card, as the app does it from the view of the home.
    async fn answer(&mut self, index: usize, card: &str, option: &str, typed: Option<&str>) -> Result<(), ScenarioError> {
        let (client, lang) = ("ios", "en");
        let view = self.view("home", client, lang).await?;
        let Some(action) = find_option(&view, card, option) else {
            return Err(ScenarioError::ActionMissing { presentation: card.to_owned(), action: option.to_owned(), view: checkpoint::render(&view) });
        };
        self.send_action(index, &view, &action, option, typed_input(typed), client, lang).await.map(drop)
    }

    /// Sends the action as the app does and answers the id of the command that carried it.
    async fn send_action(&mut self, index: usize, view: &Value, action: &Value, verb: &str, input: Value, client: &str, lang: &str) -> Result<String, ScenarioError> {
        self.commands += 1;
        let command_id = format!("00000000-0000-4000-8000-{:012x}", self.commands);
        let body = json!({
            "protocol": alondra_views::VIEWS_PROTOCOL,
            "command_id": command_id,
            "data_epoch": view["data_epoch"],
            "client": client, "lang": lang,
            "action_id": action["id"],
            "option_id": action.get("option_id").cloned().unwrap_or(Value::Null),
            "input": input,
        });
        let response = self.phone.post("/actions", &body).await.map_err(|e| Self::failed(index, e))?;
        if response.status >= 300 || response.json()["outcome"] != "applied" {
            return Err(ScenarioError::ActionRefused { action: verb.to_owned(), code: response.status, body: response.body });
        }
        Ok(command_id)
    }

    async fn checkpoint(&mut self, _index: usize, checkpoint: &Checkpoint) -> Result<(), ScenarioError> {
        let mut files = Files::new();
        let world = &self.scenario.world;
        for screen in &checkpoint.screens {
            for client in &world.clients {
                for lang in &world.langs {
                    let mut view = self.view(screen, client, lang).await?;
                    checkpoint::normalize(&mut view);
                    files.insert(format!("{}.{client}.{lang}.json", screen.replace(':', "-")), checkpoint::render(&view));
                }
            }
        }
        let sent = checkpoint.pushes.then(|| self.pushes.take());
        if let Some(sent) = &sent {
            let mut rendered = Value::Array(
                settled(sent)
                    .into_iter()
                    .map(|push| {
                        let device = if push.device == self.phone.device() { "<phone>" } else { push.device.as_str() };
                        json!({"device": device, "delivery": push.delivery, "with_sound": push.with_sound, "payload": push.payload})
                    })
                    .collect(),
            );
            checkpoint::normalize(&mut rendered);
            files.insert("pushes.json".into(), checkpoint::render(&rendered));
        }
        for expect in &checkpoint.expect {
            self.expect(&checkpoint.name, expect, sent.as_deref()).await?;
        }
        if let Some(case) = self.accepted {
            let differences = checkpoint::compare(&checkpoint::accepted(case, &checkpoint.name), &files);
            if !differences.is_empty() {
                return Err(ScenarioError::CheckpointDiff { checkpoint: checkpoint.name.clone(), differences });
            }
        }
        self.outcome.checkpoints.push((checkpoint.name.clone(), files));
        Ok(())
    }

    async fn expect(&self, checkpoint: &str, expect: &Expect, sent: Option<&[SentPush]>) -> Result<(), ScenarioError> {
        let failed = |expected: String, actual: String| ScenarioError::ExpectFailed { checkpoint: checkpoint.to_owned(), expected, actual };
        match expect {
            Expect::GhCalls { calls } => {
                let actual = self.github.gh_calls();
                if &actual != calls {
                    return Err(failed(format!("{calls:?}"), format!("{actual:?}")));
                }
            }
            Expect::PrMergedInto { pr, branch } => {
                let actual = self.github.merged_into(*pr);
                if actual.as_deref() != Some(branch.as_str()) {
                    return Err(failed(format!("PR {pr} merged into {branch}"), format!("{actual:?}")));
                }
            }
            Expect::TranscriptsPlayed => {
                let peer = self.peer.as_ref().ok_or_else(|| ScenarioError::Invalid("no step of this case starts a provider".into()))?;
                let seen = peer.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if seen.remaining.len() != peer.hosts || seen.unplayed() != 0 {
                    return Err(failed(
                        format!("{} hosts that played their transcripts to the end", peer.hosts),
                        format!("{} hosts started, {} lines left to play: {:?}", seen.remaining.len(), seen.unplayed(), seen.remaining),
                    ));
                }
            }
            Expect::NoPushes { delivery } => {
                let sent = sent.ok_or_else(|| ScenarioError::Invalid("a check on pushes needs `pushes = true` on its checkpoint".into()))?;
                let unwanted: Vec<&SentPush> = sent.iter().filter(|push| delivery.as_deref().is_none_or(|kind| push.delivery == kind)).collect();
                if !unwanted.is_empty() {
                    let kind = delivery.as_deref().unwrap_or("any");
                    return Err(failed(format!("no {kind} push"), format!("{unwanted:?}")));
                }
            }
            Expect::Badge { count } => {
                let sent = sent.ok_or_else(|| ScenarioError::Invalid("a check on pushes needs `pushes = true` on its checkpoint".into()))?;
                let actual = sent.iter().rev().find(|push| push.delivery == "badge").and_then(|push| push.payload.pointer("/aps/badge").and_then(Value::as_u64));
                if actual != Some(*count) {
                    return Err(failed(format!("the icon badge set to {count}"), format!("{actual:?}")));
                }
            }
            Expect::ViewShows { screen, text: wanted, present } => {
                let rendered = checkpoint::render(&self.view(screen, "ios", "en").await?);
                if rendered.contains(wanted.as_str()) != *present {
                    let verb = if *present { "shows" } else { "does not show" };
                    return Err(failed(format!("{screen} {verb} {wanted:?}"), rendered));
                }
            }
            Expect::Hosts { task, started, running, sessions } => {
                let edge = self.claude.as_ref().ok_or_else(|| ScenarioError::Invalid("a case with Codex nodes plays no Claude host".into()))?;
                let hosts = edge.hosts_of(task);
                let alive = hosts.iter().filter(|host| host.alive()).count();
                let held: BTreeSet<&str> = hosts.iter().map(|host| host.session.as_str()).collect();
                if (hosts.len(), alive, held.len()) != (*started, *running, *sessions) {
                    return Err(failed(
                        format!("{started} hosts started for {task}, {running} running, {sessions} sessions"),
                        format!("{} started, {alive} running, {} sessions", hosts.len(), held.len()),
                    ));
                }
            }
            Expect::TaskState { task, state } => {
                let domain_state = self.daemon().domain.state().map_err(|e| ScenarioError::Invalid(e.to_string()))?;
                let actual = domain_state.tasks.get(&task.clone().into()).map(|task| task.state);
                if actual != Some(*state) {
                    return Err(failed(format!("task {task} {state:?}"), format!("{actual:?}")));
                }
            }
        }
        Ok(())
    }
}

/// The pushes a phone is left with: every alert in the order sent, then the last retraction and
/// the last badge, which carry the whole queue. Whether a burst of cards leaving the queue settles
/// into one of those pushes or two, and which of the two goes last, depends on how the daemon's
/// threads race the paused clock; what the phone ends with does not.
fn settled(sent: &[SentPush]) -> Vec<&SentPush> {
    let last_of = |delivery: &str| sent.iter().rev().find(|push| push.delivery == delivery);
    sent.iter()
        .filter(|push| !matches!(push.delivery, "badge" | "background"))
        .chain(last_of("background"))
        .chain(last_of("badge"))
        .collect()
}

/// The action `verb` of the card of `presentation`, as the view serves it: the action whose id
/// reads as that verb on that presentation.
fn find_action(view: &Value, presentation: &str, verb: &str) -> Option<Value> {
    fn walk<'a>(value: &'a Value, found: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                if map.get("id").and_then(Value::as_str).is_some() {
                    found.push(value);
                }
                map.values().for_each(|inner| walk(inner, found));
            }
            Value::Array(items) => items.iter().for_each(|inner| walk(inner, found)),
            _ => {}
        }
    }
    let mut candidates = Vec::new();
    walk(view, &mut candidates);
    candidates
        .into_iter()
        .find(|candidate| {
            candidate["id"]
                .as_str()
                .and_then(|id| ActionId::parse(id).ok())
                .is_some_and(|id| id.verb.as_str() == verb && id.subject == presentation)
        })
        .cloned()
}

/// The action of the card whose subject starts with `card` that carries the option `option`.
fn find_option(view: &Value, card: &str, option: &str) -> Option<Value> {
    fn walk<'a>(value: &'a Value, found: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                if map.get("id").and_then(Value::as_str).is_some() {
                    found.push(value);
                }
                map.values().for_each(|inner| walk(inner, found));
            }
            Value::Array(items) => items.iter().for_each(|inner| walk(inner, found)),
            _ => {}
        }
    }
    let mut candidates = Vec::new();
    walk(view, &mut candidates);
    candidates
        .into_iter()
        .find(|candidate| {
            candidate.get("option_id").and_then(Value::as_str) == Some(option)
                && candidate["id"].as_str().and_then(|id| ActionId::parse(id).ok()).is_some_and(|id| id.subject.starts_with(card))
        })
        .cloned()
}

/// An error the API answers with, as the SDK names it: only one of the SDK's own.
fn api_error(index: usize, error: &str, status: Option<u16>) -> Result<ApiError, ScenarioError> {
    if !crate::claude_host::assistant_errors().iter().any(|known| known == error) {
        return Err(Context::failed(index, format!("`{error}` is no error the SDK reports (sdk-errors.json)")));
    }
    Ok(ApiError { error: error.to_owned(), status })
}

fn typed_input(typed: Option<&str>) -> Value {
    typed.map_or_else(|| json!({}), |typed| json!({"text": typed}))
}

/// The composer of the task's screen: a message to the node that runs the task, or one kept on the
/// task when no node runs it.
fn find_message(view: &Value, task: &str) -> Option<Value> {
    fn walk<'a>(value: &'a Value, found: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                if map.get("id").and_then(Value::as_str).is_some() {
                    found.push(value);
                }
                map.values().for_each(|inner| walk(inner, found));
            }
            Value::Array(items) => items.iter().for_each(|inner| walk(inner, found)),
            _ => {}
        }
    }
    let mut candidates = Vec::new();
    walk(view, &mut candidates);
    candidates
        .into_iter()
        .find(|candidate| {
            candidate["id"].as_str().and_then(|id| ActionId::parse(id).ok()).is_some_and(|id| match id.verb {
                alondra_views::action_id::Verb::Msg => id.extra.as_deref() == Some(task),
                alondra_views::action_id::Verb::TaskMsg => id.subject == task,
                _ => false,
            })
        })
        .cloned()
}
