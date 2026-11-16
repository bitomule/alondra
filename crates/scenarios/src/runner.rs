//! Carries a scenario out: assembles the daemon over fake edges on a virtual clock, runs the
//! steps, and reads the checkpoints.

use alondra_daemon_engine::sdk_owner::SdkOwners as _;
use crate::checkpoint::{self, Files};
use crate::claude_peer::{Activity, Seen};
use crate::error::ScenarioError;
use crate::format::{Checkpoint, Expect, GithubChange, Scenario, Step, parse_duration};
use crate::http::Phone;
use alondra_agent_runtime::owned_host::{HostLauncher, HostSpawn, Launcher};
use alondra_core::domain::{
    Actor, CardSpec, Command, CommandEnvelope, DeliveryTarget, LaunchPlan, MergeMethod, NodeEnd, PROTOCOL_VERSION,
    RuntimeObservation, SdkApiFailure, StackDependency,
};
use alondra_core::views::action_id::ActionId;
use alondra_daemon_engine::github_world::GithubWorld;
use alondra_daemon_engine::machine::MachineSampler;
use alondrad::assemble::{self, Assembled, Backend, GithubSeams, Pushes, Seams};
use alondrad::push_transport::RecordedPushes;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the daemon must stay unchanged, in virtual seconds, for it to count as resting. The
/// scenario daemon polls every couple of seconds, so this outlasts every loop's period.
const QUIET: u64 = 30;
const REPOSITORY_HOST: &str = "github.com";

pub struct Outcome {
    /// Every checkpoint passed, in order, with the files it read.
    pub checkpoints: Vec<(String, Files)>,
}

/// The Mac of a scenario: always rested, with room for everything.
struct CalmMac;
impl MachineSampler for CalmMac {
    fn sample(&self) -> alondra_core::Machine {
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
    /// The real milliseconds the runner may still wait for a host that has lines left to play.
    patience: Arc<std::sync::atomic::AtomicU64>,
}

/// Real time a scenario may spend, in all, waiting for the threads of its hosts: a transcript that
/// is never played to its end ends the run with its failure, not a hang.
const PATIENCE_MS: u64 = 20_000;


/// The SDK host of a task as the scenario plays it: what its provider process reports to the
/// daemon, through the commands the daemon records the host's observations with.
struct Host {
    generation: u64,
    turn: Option<String>,
    turns: u64,
}

struct Context<'a> {
    peer: Option<Peer>,
    scenario: &'a Scenario,
    accepted: Option<&'a Path>,
    config: alondra_core::config::Config,
    outage: Option<(SdkApiFailure, Vec<String>)>,
    hosts: BTreeMap<String, Host>,
    daemon: Option<Assembled>,
    github: Arc<GithubWorld>,
    pushes: RecordedPushes,
    leaks: Arc<Mutex<Vec<String>>>,
    phone: Phone,
    heads: BTreeMap<u64, String>,
    request: Option<String>,
    commands: u64,
    spent: Duration,
    budget: Duration,
    outcome: Outcome,
}

async fn execute(scenario: &Scenario, accepted: Option<&Path>, probe: impl FnOnce(&Launcher)) -> Result<(Outcome, Option<ScenarioError>), ScenarioError> {
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
    let (launcher, backend, peer) = if codex.is_empty() {
        (Launcher::new(RefusingLauncher(leaks.clone())), Backend::Given(None), None)
    } else {
        let case = scenario.case.as_deref().ok_or_else(|| ScenarioError::Invalid("a step that starts a provider reads the transcripts of its case".into()))?;
        let peer = crate::codex_peer::launcher(case, &codex)?;
        let (seen, activity) = (peer.seen.clone(), peer.activity.clone());
        let launcher = Launcher::new(peer);
        let backend = codex_backend(&config.home, launcher.clone()).map_err(|e| invalid(&e))?;
        (launcher, Backend::Given(Some(backend)), Some(Peer { seen, activity, hosts: codex.len(), patience: Arc::new(std::sync::atomic::AtomicU64::new(PATIENCE_MS)) }))
    };
    probe(&launcher);
    let seams = seams_of(&github, &pushes, launcher, backend);
    let daemon = assemble::start(&config, seams).await.map_err(|e| invalid(&format!("{e:#}")))?;
    let phone = Phone::pair(&daemon, std::net::SocketAddr::new(config.listen, config.port)).map_err(|e| invalid(&e))?;
    let mut context = Context {
        peer,
        scenario,
        accepted,
        config,
        outage: None,
        hosts: BTreeMap::new(),
        daemon: Some(daemon),
        github,
        pushes,
        leaks,
        phone,
        heads: BTreeMap::new(),
        request: None,
        commands: 0,
        spent: Duration::ZERO,
        budget,
        outcome: Outcome { checkpoints: Vec::new() },
    };
    let result = steps(&mut context).await;
    if let Some(daemon) = &context.daemon {
        daemon.tasks.abort();
        let domain = daemon.domain.clone();
        tokio::task::spawn_blocking(move || domain.stop_sdk_owners()).await.ok();
    }
    Ok((context.outcome, result.err()))
}

fn seams_of(github: &Arc<GithubWorld>, pushes: &RecordedPushes, launcher: Launcher, backend: Backend) -> Seams {
    Seams {
        github: GithubSeams::of(github.clone()),
        push: Pushes::new(pushes.clone()),
        machine: Arc::new(CalmMac),
        launcher,
        backend,
        world: assemble::World::Isolated,
    }
}

async fn steps(context: &mut Context<'_>) -> Result<(), ScenarioError> {
    for (index, step) in context.scenario.steps.iter().enumerate() {
        context.step(index, step).await?;
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

/// The backend of a daemon whose Codex nodes start through `launcher`: three files that stand where
/// the host and the native executable stand, named by their digests as a real configuration names them.
fn codex_backend(home: &Path, launcher: Launcher) -> Result<alondra_daemon_engine::sdk_owner::backend::SdkBackend, String> {
    use alondra_daemon_engine::sdk_owner::backend::{SdkBackend, digest};
    let file = |name: &str| -> Result<(std::path::PathBuf, String), String> {
        let path = home.join(name);
        std::fs::write(&path, name).map_err(|e| e.to_string())?;
        let sha = digest(&path).map_err(|e| format!("{e:#}"))?;
        Ok((path, sha))
    };
    let ((node, node_sha256), (native, native_sha256)) = (file("codex-host")?, file("codex-native")?);
    let environment = BTreeMap::from([("HOME".to_owned(), home.display().to_string()), ("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
    Ok(SdkBackend {
        provider: alondra_core::accounts::Provider::Codex,
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
        launcher,
    })
}

fn agent_of(task: &str) -> String {
    format!("agent-{task}")
}

fn pr_id(number: u64) -> String {
    format!("pr{number}")
}

impl Context<'_> {
    fn daemon(&self) -> &Assembled {
        self.daemon.as_ref().expect("a daemon is assembled between steps")
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
                self.command(index, Actor::User { user_id: "ana".into() }, Command::CreateRequest {
                    request_id: "request".into(), body: text.clone(), auto_merge: false, maintenance: false, message: false,
                })?;
                self.command(index, orchestrator(), Command::TitleRequest { request_id: "request".into(), title: title.clone() })?;
                self.request = Some("request".into());
                self.create_task(index, task, title, None)?;
            }
            Step::Task { id, title, stack_on } => {
                self.command(index, Actor::System, Command::RegisterAgent { agent_id: agent_of(id).into(), name: id.clone(), at: 0 })?;
                let stack = match stack_on {
                    Some(on) => Some(StackDependency {
                        upstream_task: on.upstream.clone().into(),
                        presentation_id: on.presentation.clone().into(),
                        pr_id: pr_id(on.pr).into(),
                        head: self.head(index, on.pr)?,
                    }),
                    None => None,
                };
                self.create_task(index, id, title, stack)?;
            }
            Step::RegisterPr { task, number } => {
                let head = self.head(index, *number)?;
                self.command(index, Actor::Agent { agent_id: agent_of(task).into() }, Command::RegisterPr {
                    task_id: task.clone().into(),
                    pr_id: pr_id(*number).into(),
                    host: REPOSITORY_HOST.into(),
                    repository: self.scenario.world.repository.clone(),
                    number: *number,
                    head,
                    base: self.github.base_of(*number),
                    allowed_methods: vec![MergeMethod::Squash, MergeMethod::Merge],
                })?;
            }
            Step::Present { task, id, prs, body } => {
                let mut targets = Vec::new();
                for number in prs {
                    targets.push(DeliveryTarget::PullRequest { pr_id: pr_id(*number).into(), head: self.head(index, *number)? });
                }
                self.command(index, Actor::Agent { agent_id: agent_of(task).into() }, Command::PresentResult {
                    checklist: Vec::new(),
                    attachments: Vec::new(),
                    task_id: task.clone().into(),
                    interaction_id: id.clone().into(),
                    body: body.clone(),
                    targets,
                    is_final: true,
                    nothing_to_review: false,
                    card: CardSpec::delivery("Review the result", &[], true),
                })?;
            }
            Step::StackReady { task, presentation, pr } => {
                let head = self.head(index, *pr)?;
                self.command(index, Actor::Agent { agent_id: agent_of(task).into() }, Command::PublishStackReadiness {
                    task_id: task.clone().into(),
                    presentation_id: presentation.clone().into(),
                    pr_id: pr_id(*pr).into(),
                    head,
                    ready: true,
                })?;
            }
            Step::CodexNode { task, .. } => self.start_codex_node(index, task).await?,
            Step::Github(change) => match change {
                GithubChange::OpenPr { number, head, base } => {
                    let commit = self.github.open_pr(*number, head, base);
                    self.heads.insert(*number, commit);
                }
                GithubChange::MergeExternally { number } => self.github.merge_externally(*number),
                GithubChange::CloseExternally { number } => self.github.close_externally(*number),
            },
            Step::Press { presentation, action, text, screen } => self.press(index, presentation, action, text.as_deref(), screen.as_deref().unwrap_or("home")).await?,
            Step::Answer { card, option, text } => self.answer(index, card, option, text.as_deref()).await?,
            Step::Node { task } => self.start_node(index, task)?,
            Step::TurnEnd { task } => {
                let failure = self.failure_for(task);
                self.end_turn(index, task, failure)?;
            }
            Step::ApiError { task, error, status } => {
                self.end_turn(index, task, Some(SdkApiFailure { error: error.clone(), status: *status, resets_at: None }))?;
            }
            Step::ProviderDown { error, status, tasks } => {
                self.outage = Some((SdkApiFailure { error: error.clone(), status: *status, resets_at: None }, tasks.clone()));
            }
            Step::LoginExpired => {
                self.outage = Some((SdkApiFailure { error: "authentication_failed".into(), status: Some(401), resets_at: None }, Vec::new()));
            }
            Step::ProviderUp | Step::LoginRestored => self.outage = None,
            Step::HostExit { task, resumes } => self.host_exit(index, task, *resumes)?,
            Step::Tell { task, text } => {
                self.command(index, Actor::System, Command::TellNode { task_id: task.clone().into(), text: text.clone(), from_user: true, answers: None })?;
            }
            Step::ReadMessage { task } => {
                let input = self.pending_input(index, task)?.ok_or_else(|| Self::failed(index, format!("{task} has no message waiting")))?;
                let generation = self.hosts.get(task).map_or(1, |host| host.generation);
                self.command(index, Actor::System, Command::RecordRuntimeFacts { task_id: task.clone().into(), generation, facts: vec![RuntimeObservation::InputConsumed { input }] })?;
            }
            Step::NodeEnd { task } => {
                self.command(index, Actor::System, Command::RecordNodeEnd { task_id: task.clone().into(), end: NodeEnd::Closed, agent_id: Some(agent_of(task).into()) })?;
                self.hosts.remove(task);
            }
            Step::Restart => self.restart(index).await?,
            Step::Update => {
                let running: Vec<String> = self.hosts.keys().cloned().collect();
                self.restart(index).await?;
                for task in running {
                    self.host_exit(index, &task, true)?;
                }
            }
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

    /// The host's thread and the daemon's owner of it run in real time, beside the virtual clock:
    /// waits, in the runtime's blocking pool (which keeps the clock from jumping), until the frames
    /// between them stop for a moment, so the next step reads what the daemon did with them.
    async fn settle_peer(&self) {
        let Some(peer) = &self.peer else { return };
        let (activity, seen, patience, hosts) = (peer.activity.clone(), peer.seen.clone(), peer.patience.clone(), peer.hosts);
        tokio::task::spawn_blocking(move || {
            let played = || {
                let seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                seen.remaining.len() == hosts && seen.unplayed() == 0
            };
            let (mut last, mut calm) = (activity.count(), 0);
            loop {
                std::thread::sleep(Duration::from_millis(2));
                let now = activity.count();
                if now != last {
                    (last, calm) = (now, 0);
                    continue;
                }
                calm += 1;
                if calm < 4 {
                    continue;
                }
                let left = patience.load(std::sync::atomic::Ordering::SeqCst);
                if played() || left == 0 {
                    return;
                }
                patience.store(left.saturating_sub(2), std::sync::atomic::Ordering::SeqCst);
            }
        })
        .await
        .ok();
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
        let options = alondra_daemon_engine::sdk_owner::LaunchOptions {
            provider: Provider::Codex, model: Some("demo-model".into()), effort: Some("high".into()), selection: Some(selection),
            ..Default::default()
        };
        alondra_daemon_engine::sdk_owner::launch(&self.daemon().domain, alondra_daemon_engine::sdk_owner::Launch {
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

    fn failure_for(&self, task: &str) -> Option<SdkApiFailure> {
        let (failure, tasks) = self.outage.as_ref()?;
        (tasks.is_empty() || tasks.iter().any(|named| named == task)).then(|| failure.clone())
    }

    /// The pending input the task's node has not taken yet, if any.
    fn pending_input(&self, index: usize, task: &str) -> Result<Option<String>, ScenarioError> {
        let state = self.daemon().domain.state().map_err(|e| Self::failed(index, e))?;
        let lifecycle = state.lifecycle_of(&task.to_owned().into()).map_err(|e| Self::failed(index, e))?;
        Ok(lifecycle.pending_inputs().first().map(|input| input.as_str().to_owned()))
    }

    fn start_node(&mut self, index: usize, task: &str) -> Result<(), ScenarioError> {
        alondra_daemon_engine::domain_store::admit_through_sdk(&self.daemon().domain, &task.to_owned().into(), &agent_of(task));
        let input = self
            .pending_input(index, task)?
            .ok_or_else(|| Self::failed(index, format!("the admitted task {task} has no input to take")))?;
        self.hosts.insert(task.to_owned(), Host { generation: 1, turn: None, turns: 0 });
        self.take_input(index, task, input, true)
    }

    fn take_input(&mut self, index: usize, task: &str, input: String, first: bool) -> Result<(), ScenarioError> {
        let host = self.hosts.get_mut(task).ok_or_else(|| Self::failed(index, format!("{task} has no node: start it with `node`")))?;
        host.turns += 1;
        let turn = format!("turn-{}", host.turns);
        host.turn = Some(turn.clone());
        let generation = host.generation;
        let mut facts = Vec::new();
        if first {
            facts.push(RuntimeObservation::ResourceAvailable);
        }
        facts.push(RuntimeObservation::TurnStarted { turn, input });
        self.command(index, Actor::System, Command::RecordRuntimeFacts { task_id: task.to_owned().into(), generation, facts })
    }

    fn end_turn(&mut self, index: usize, task: &str, failure: Option<SdkApiFailure>) -> Result<(), ScenarioError> {
        let host = self.hosts.get_mut(task).ok_or_else(|| Self::failed(index, format!("{task} has no node: start it with `node`")))?;
        let turn = host.turn.take().ok_or_else(|| Self::failed(index, format!("{task} has no turn running")))?;
        let generation = host.generation;
        if failure.is_none() {
            self.command(index, Actor::System, Command::RecordRuntimeFacts { task_id: task.to_owned().into(), generation, facts: vec![RuntimeObservation::ModelAnswered] })?;
        }
        self.command(index, Actor::System, Command::RecordTurnBoundary { task_id: task.to_owned().into(), generation, turn, failure, failed: None })
    }

    fn host_exit(&mut self, index: usize, task: &str, resumes: bool) -> Result<(), ScenarioError> {
        let host = self.hosts.get_mut(task).ok_or_else(|| Self::failed(index, format!("{task} has no node: start it with `node`")))?;
        host.turn = None;
        let generation = host.generation;
        self.command(index, Actor::System, Command::RecordHostEnded { task_id: task.to_owned().into(), generation, resumes, by_alondra: false, ended_work: Vec::new() })
    }

    /// What the nodes do between steps: one with an input waiting takes it as a turn, and while the
    /// provider is down that turn ends at once on the provider's error.
    fn play_nodes(&mut self, index: usize) -> Result<(), ScenarioError> {
        let tasks: Vec<String> = self.hosts.iter().filter(|(_, host)| host.turn.is_none()).map(|(task, _)| task.clone()).collect();
        for task in tasks {
            let Some(input) = self.pending_input(index, &task)? else { continue };
            self.take_input(index, &task, input, false)?;
            if let Some(failure) = self.failure_for(&task) {
                self.end_turn(index, &task, Some(failure))?;
            }
        }
        Ok(())
    }

    /// Stops the daemon and starts it again over the same home, with the same edges.
    async fn restart(&mut self, index: usize) -> Result<(), ScenarioError> {
        if let Some(old) = self.daemon.take() {
            old.tasks.abort();
            drop(old);
        }
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        let launcher = Launcher::new(RefusingLauncher(self.leaks.clone()));
        let seams = seams_of(&self.github, &self.pushes, launcher, Backend::Given(None));
        let daemon = assemble::start(&self.config, seams).await.map_err(|e| Self::failed(index, format!("{e:#}")))?;
        self.daemon = Some(daemon);
        self.pass(Duration::from_secs(1), index).await
    }

    /// Whether the scenario starts a node of the task on the provider, which then admits it.
    fn runs_on_a_provider(&self, task: &str) -> bool {
        self.scenario.steps.iter().any(|step| matches!(step, Step::Node { task: named } if named == task))
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
        self.settle_peer().await;
        self.play_nodes(index)
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
        let path = format!("/views/{key}?protocol={}&client={client}&lang={lang}", alondra_core::views::VIEWS_PROTOCOL);
        let response = self.phone.get(&path).await.map_err(|e| ScenarioError::Invalid(e.to_string()))?;
        if response.status != 200 {
            return Err(ScenarioError::ActionRefused { action: format!("GET {path}"), code: response.status, body: response.body });
        }
        Ok(response.json())
    }

    async fn press(&mut self, index: usize, presentation: &str, verb: &str, typed: Option<&str>, screen: &str) -> Result<(), ScenarioError> {
        let (client, lang) = ("ios", "en");
        let view = self.view(screen, client, lang).await?;
        let found = find_action(&view, presentation, verb);
        let Some(action) = found else {
            return Err(ScenarioError::ActionMissing { presentation: presentation.to_owned(), action: verb.to_owned(), view: checkpoint::render(&view) });
        };
        self.send_action(index, &view, &action, verb, typed, client, lang).await
    }

    /// The user presses the option of a card, as the app does it from the view of the home.
    async fn answer(&mut self, index: usize, card: &str, option: &str, typed: Option<&str>) -> Result<(), ScenarioError> {
        let (client, lang) = ("ios", "en");
        let view = self.view("home", client, lang).await?;
        let Some(action) = find_option(&view, card, option) else {
            return Err(ScenarioError::ActionMissing { presentation: card.to_owned(), action: option.to_owned(), view: checkpoint::render(&view) });
        };
        self.send_action(index, &view, &action, option, typed, client, lang).await
    }

    async fn send_action(&mut self, index: usize, view: &Value, action: &Value, verb: &str, typed: Option<&str>, client: &str, lang: &str) -> Result<(), ScenarioError> {
        self.commands += 1;
        let body = json!({
            "protocol": alondra_core::views::VIEWS_PROTOCOL,
            "command_id": format!("00000000-0000-4000-8000-{:012x}", self.commands),
            "data_epoch": view["data_epoch"],
            "client": client, "lang": lang,
            "action_id": action["id"],
            "option_id": action.get("option_id").cloned().unwrap_or(Value::Null),
            "input": typed.map_or_else(|| json!({}), |typed| json!({"text": typed})),
        });
        let response = self.phone.post("/actions", &body).await.map_err(|e| Self::failed(index, e))?;
        if response.status >= 300 {
            return Err(ScenarioError::ActionRefused { action: verb.to_owned(), code: response.status, body: response.body });
        }
        Ok(())
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
        if checkpoint.pushes {
            let sent: Vec<Value> = self
                .pushes
                .take()
                .into_iter()
                .map(|push| json!({"device": push.device, "delivery": push.delivery, "with_sound": push.with_sound, "payload": push.payload}))
                .collect();
            files.insert("pushes.json".into(), checkpoint::render(&Value::Array(sent)));
        }
        for expect in &checkpoint.expect {
            self.expect(&checkpoint.name, expect).await?;
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

    async fn expect(&self, checkpoint: &str, expect: &Expect) -> Result<(), ScenarioError> {
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
            Expect::ViewShows { screen, text: wanted, present } => {
                let rendered = checkpoint::render(&self.view(screen, "ios", "en").await?);
                if rendered.contains(wanted.as_str()) != *present {
                    let verb = if *present { "shows" } else { "does not show" };
                    return Err(failed(format!("{screen} {verb} {wanted:?}"), rendered));
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
