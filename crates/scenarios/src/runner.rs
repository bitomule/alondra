//! Carries a scenario out: assembles the daemon over fake edges on a virtual clock, runs the
//! steps, and reads the checkpoints.

use crate::checkpoint::{self, Files};
use crate::error::ScenarioError;
use crate::format::{Checkpoint, Expect, GithubChange, Scenario, Step, parse_duration};
use crate::http::Phone;
use alondra_agent_runtime::owned_host::{HostLauncher, HostSpawn, Launcher};
use alondra_core::domain::{
    Actor, CardSpec, Command, CommandEnvelope, DeliveryTarget, LaunchPlan, MergeMethod, PROTOCOL_VERSION, StackDependency,
};
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

/// `run`, with `probe` handed the launcher the daemon starts processes through, once it is
/// assembled: how a test of the harness makes something reach for a process.
pub fn run_with(scenario: &Scenario, accepted: Option<&Path>, probe: impl FnOnce(&Launcher)) -> Result<Outcome, ScenarioError> {
    if std::env::var("TZ").as_deref() != Ok("UTC") {
        return Err(ScenarioError::Invalid("scenarios run with TZ=UTC: the views show local time, which differs from Mac to Mac".into()));
    }
    let runtime = alondra_core::clock::VirtualRuntime::new(EPOCH_MILLIS).map_err(|error| ScenarioError::Invalid(error.to_string()))?;
    runtime.block_on(execute(scenario, accepted, probe))
}

/// 2026-11-15 09:00 UTC: every scenario starts at the same instant.
const EPOCH_MILLIS: i64 = 1_794_733_200_000;

struct Context<'a> {
    scenario: &'a Scenario,
    accepted: Option<&'a Path>,
    daemon: Assembled,
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

async fn execute(scenario: &Scenario, accepted: Option<&Path>, probe: impl FnOnce(&Launcher)) -> Result<Outcome, ScenarioError> {
    let budget = parse_duration(&scenario.max_virtual)?;
    let home = tempfile::Builder::new().prefix("sc-").tempdir().map_err(|error| ScenarioError::Invalid(error.to_string()))?;
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
    let launcher = Launcher::new(RefusingLauncher(leaks.clone()));
    probe(&launcher);
    let seams = Seams {
        github: GithubSeams::of(github.clone()),
        push: Pushes::new(pushes.clone()),
        machine: Arc::new(CalmMac),
        launcher,
        backend: Backend::Given(None),
        world: assemble::World::Isolated,
    };
    let daemon = assemble::start(&config, seams).await.map_err(|e| invalid(&format!("{e:#}")))?;
    let phone = Phone::pair(&daemon, std::net::SocketAddr::new(config.listen, config.port)).map_err(|e| invalid(&e))?;
    let mut context = Context {
        scenario,
        accepted,
        daemon,
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
    context.daemon.tasks.abort();
    result.map(|()| context.outcome)
}

async fn steps(context: &mut Context<'_>) -> Result<(), ScenarioError> {
    for (index, step) in context.scenario.steps.iter().enumerate() {
        context.step(index, step).await?;
        if let Some(what) = context.leaks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).first() {
            return Err(ScenarioError::Leak(what.clone()));
        }
    }
    Ok(())
}

fn agent_of(task: &str) -> String {
    format!("agent-{task}")
}

fn pr_id(number: u64) -> String {
    format!("pr{number}")
}

impl Context<'_> {
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
            let state = self.daemon.domain.state().map_err(|e| Self::failed(index, e))?;
            let envelope = CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
                command_id: id.clone().into(),
                expected_revision: state.revision,
                command: command.clone(),
            };
            match self.daemon.domain.execute(&actor, &envelope, None, || Ok(())) {
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
            Step::Github(change) => match change {
                GithubChange::OpenPr { number, head, base } => {
                    let commit = self.github.open_pr(*number, head, base);
                    self.heads.insert(*number, commit);
                }
                GithubChange::MergeExternally { number } => self.github.merge_externally(*number),
                GithubChange::CloseExternally { number } => self.github.close_externally(*number),
            },
            Step::Press { pr, action } => self.press(index, *pr, action).await?,
            Step::Advance { by } => self.pass(parse_duration(by)?, index).await?,
            Step::Rest { max } => self.rest(parse_duration(max)?, index).await?,
            Step::Checkpoint(checkpoint) => self.checkpoint(index, checkpoint).await?,
        }
        Ok(())
    }

    fn create_task(&mut self, index: usize, task: &str, title: &str, stack: Option<StackDependency>) -> Result<(), ScenarioError> {
        let request = self.request.clone().ok_or_else(|| Self::failed(index, "a task needs a request before it"))?;
        self.command(index, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
            task_id: task.to_owned().into(),
            request_id: request.into(),
            title: title.to_owned(),
            agent_id: Some(agent_of(task).into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack,
            launch: Some(LaunchPlan::new("node", "/work")),
            plan_items: None,
            continues: None,
        })
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
        Ok(())
    }

    async fn rest(&mut self, max: Duration, index: usize) -> Result<(), ScenarioError> {
        let mut waited = Duration::ZERO;
        let mut quiet = 0;
        let mut revision = self.daemon.domain.state().map_err(|e| Self::failed(index, e))?.revision;
        while quiet < QUIET {
            if waited >= max {
                return Err(ScenarioError::NoRest { at_step: index, spent_secs: self.spent.as_secs() });
            }
            self.pass(Duration::from_secs(1), index).await?;
            waited += Duration::from_secs(1);
            let now = self.daemon.domain.state().map_err(|e| Self::failed(index, e))?.revision;
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

    async fn press(&mut self, index: usize, pr: u64, verb: &str) -> Result<(), ScenarioError> {
        let (client, lang) = ("ios", "en");
        let view = self.view("home", client, lang).await?;
        let found = find_action(&view, pr, verb);
        let Some(action) = found else {
            return Err(ScenarioError::ActionMissing { pr, action: verb.to_owned(), view: checkpoint::render(&view) });
        };
        self.commands += 1;
        let body = json!({
            "protocol": alondra_core::views::VIEWS_PROTOCOL,
            "command_id": format!("00000000-0000-4000-8000-{:012x}", self.commands),
            "data_epoch": view["data_epoch"],
            "client": client, "lang": lang,
            "action_id": action["id"],
            "option_id": action.get("option_id").cloned().unwrap_or(Value::Null),
            "input": {},
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
            self.expect(&checkpoint.name, expect)?;
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

    fn expect(&self, checkpoint: &str, expect: &Expect) -> Result<(), ScenarioError> {
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
            Expect::TaskState { task, state } => {
                let domain_state = self.daemon.domain.state().map_err(|e| ScenarioError::Invalid(e.to_string()))?;
                let tasks = &domain_state.tasks;
                let actual = tasks.get(&task.clone().into()).map(|task| format!("{:?}", task.state).to_lowercase());
                if actual.as_deref() != Some(state.to_lowercase().as_str()) {
                    return Err(failed(format!("task {task} {state}"), format!("{actual:?}")));
                }
            }
        }
        Ok(())
    }
}

/// The action `verb` of the card of pull request `pr`, as the view serves it.
fn find_action(view: &Value, pr: u64, verb: &str) -> Option<Value> {
    let marker = format!("PR #{pr}");
    let prefix = format!("{verb}:");
    fn walk<'a>(value: &'a Value, found: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                if map.get("type").and_then(Value::as_str) == Some("card") {
                    found.push(value);
                }
                map.values().for_each(|inner| walk(inner, found));
            }
            Value::Array(items) => items.iter().for_each(|inner| walk(inner, found)),
            _ => {}
        }
    }
    let mut cards = Vec::new();
    walk(view, &mut cards);
    cards
        .into_iter()
        .filter(|card| card["eyebrow"]["text"].as_str().is_some_and(|text| text.contains(&marker)))
        .find_map(|card| {
            let mut candidates: Vec<&Value> = Vec::new();
            for key in ["actions", "more"] {
                candidates.extend(card[key].as_array().into_iter().flatten());
            }
            candidates.extend(card.get("reply").filter(|reply| reply.is_object()));
            candidates.into_iter().find(|action| action["id"].as_str().is_some_and(|id| id.starts_with(&prefix))).cloned()
        })
}
