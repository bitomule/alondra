//! The SDK owners as their tests here and in the crates above drive them: a slice of the daemon with
//! one task on a real host, and what they read of it.
//! The vertical slice through the real owner: the daemon's store, commands and `own`, the pinned
//! Node host with the official SDK, and a fake native Claude that answers each prompt with a
//! result. Each step waits on the host's next event, never on a clock.
use alondra_daemon_engine_model::LaunchOptionsFile as _;
use alondra_daemon_engine::gate::GateOf as _;
use alondra_engine_sdk::sdk_owner::SdkOwners as _;
use alondra_daemon_engine::contexts::ContextsOf as _;
use alondra_engine_sdk::sdk_owner::*;
use alondra_engine_sdk::domain_runtime::DomainRuntime;
use alondra_engine_sdk::domain_store::StoreError;
use alondra_daemon_engine_model::LaunchPlan;
use alondra_domain::ActivityView;
use alondra_domain_rules::*;
use alondra_provider_contract::Capability;
use alondra_engine_admission::AdmissionOf as _;
use crate::test_support::Directory;
use alondra_core::accounts::{AccountId, AccountKey, AccountState, AccountStatus, Provider};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use alondra_daemon_engine::host_fixtures::{HostRoot, fixture_backend, runfile};


pub const TASK: &str = "sdk-slice";

/// A domain whose launches start through the fixture backend: what the native Claude was started
/// with, the prompts it got, and every host of the domain stopped when this is dropped. Declare it
/// before the directories it lives in, so its hosts stop first.
pub struct SdkLaunches {
    pub domain: Arc<DomainRuntime>,
    pub capture: PathBuf,
    pub state_root: PathBuf,
    pub _state: HostRoot,
}

impl SdkLaunches {
    pub fn enable(domain: &Arc<DomainRuntime>, home: &Path) -> Self {
        let state = HostRoot::new();
        let (backend, capture) = fixture_backend(home, &state);
        let state_root = backend.state_root.clone();
        domain.enable_sdk(backend);
        Self { domain: Arc::clone(domain), capture, state_root, _state: state }
    }

    /// The arguments the last native Claude was started with.
    pub(crate) fn args(&self) -> Vec<String> {
        let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&self.capture).unwrap()).unwrap();
        serde_json::from_value(capture["args"].clone()).unwrap()
    }

    /// The value the last native Claude got for `flag`.
    pub fn flag(&self, flag: &str) -> Option<String> {
        let args = self.args();
        args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1)).cloned()
            .or_else(|| args.iter().find_map(|arg| arg.strip_prefix(&format!("{flag}=")).map(str::to_owned)))
    }

    /// Every prompt the native Claudes got, in order, besides the wake Alondra sends after a turn
    /// that ended empty.
    pub(crate) fn prompts(&self) -> Vec<String> {
        not_wakes(std::fs::read_to_string(format!("{}.prompts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()))
    }

    /// Each native Claude the hosts started: its session, the one it resumed and its folder.
    pub fn starts(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(format!("{}.starts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    /// What the launch of `agent`'s execution kept beside its host.
    pub(crate) fn options(&self, agent: &str) -> LaunchOptions {
        LaunchOptions::load(&self.state_root.join(agent)).unwrap()
    }

    /// Waits on the domain until `task`'s turns have ended on `turns` cards it rested on.
    pub async fn until_turns_ended(&self, task: &str, turns: usize) {
        let mut changed = self.domain.subscribe();
        let ended = |domain: &DomainRuntime| domain.read().state().interactions.keys()
            .filter(|card| rests_on(task, card)).count() >= turns;
        while !ended(&self.domain) {
            changed.changed().await.unwrap();
        }
    }

    /// Waits on the domain until `task`'s first turn ended with Claude's answer.
    pub async fn until_answered(&self, task: &str) {
        self.until_turns_ended(task, 1).await;
    }
}

/// One move of the owner: send what the store issued, else read the host.
pub fn advance(driver: &mut Driver) {
    if driver.pending_permits() > 0 {
        driver.step(&|| false).unwrap();
    } else {
        driver.observe_written().unwrap();
    }
}

/// Whether `card` is one an ended turn of `task` rests on: the case the orchestrator settles when
/// the node stopped, or the user's card when the turn was interrupted or failed.
pub(crate) fn rests_on(task: &str, card: &InteractionId) -> bool {
    card.0.starts_with(&format!("{STALLED_PREFIX}{task}-")) || card.0.starts_with(&format!("{TURN_ENDED_PREFIX}{task}-"))
}

pub fn not_wakes(prompts: impl Iterator<Item = String>) -> Vec<String> {
    prompts.filter(|prompt| !prompt.starts_with(alondra_domain_rules::WAKE_INSTRUCTION)).collect()
}

impl Drop for SdkLaunches {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
    }
}

pub const HOLDING_NATIVE: &str = r#"#!/usr/bin/python3
import json, os, subprocess, sys
fake = subprocess.Popen([os.path.join(os.path.dirname(os.path.abspath(__file__)), 'native-fake')] + sys.argv[1:], stdin=subprocess.PIPE, close_fds=False)
for raw in sys.stdin:
    if json.loads(raw).get('type') == 'user':
        continue
    fake.stdin.write(raw.encode())
    fake.stdin.flush()
fake.stdin.close()
sys.exit(fake.wait())
"#;

impl SdkLaunches {
    /// `enable`, with a native Claude that keeps every prompt from the fake one: each node starts
    /// and never begins a turn, so it holds its slot until its task ends.
    pub(crate) fn enable_holding(domain: &Arc<DomainRuntime>, home: &Path) -> Self {
        let state = HostRoot::new();
        let (mut backend, capture) = fixture_backend(home, &state);
        let fake = home.join("native-fake");
        std::fs::rename(&backend.native, &fake).unwrap();
        std::fs::write(&backend.native, HOLDING_NATIVE).unwrap();
        std::fs::set_permissions(&backend.native, std::fs::Permissions::from_mode(0o700)).unwrap();
        backend.native_sha256 = backend::digest(&backend.native).unwrap();
        let state_root = backend.state_root.clone();
        domain.enable_sdk(backend);
        Self { domain: Arc::clone(domain), capture, state_root, _state: state }
    }
}

impl SdkLaunches {
    /// While `refused`, no new host gets the folder it needs, so every start fails before the
    /// domain registers a node; hosts that run already go on.
    pub fn refuse_starts(&self, refused: bool) {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.state_root).unwrap();
        std::fs::set_permissions(&self.state_root, std::fs::Permissions::from_mode(if refused { 0o500 } else { 0o700 })).unwrap();
    }
}

/// What plays the slice's hosts and the native Claude behind each: real processes (`ProcessHosts`),
/// or a model in the test's own process. The crate that compiles these fixtures names its own in
/// `crate::engine_tests::SliceHosts`.
pub trait Hosts: Default {
    /// The backend launches start, its native Claude kept under `home`.
    fn backend(&mut self, home: &Path) -> SdkBackend;
    /// Gives `domain`, which the slice just opened, these hosts and the ones it left running.
    fn serve(&self, domain: &DomainRuntime);
    /// Each native Claude the hosts started, as the fake native Claude records it.
    fn starts(&self) -> Vec<serde_json::Value>;
    /// Every prompt the native Claudes got, in order, the wakes included.
    fn prompts(&self) -> Vec<String>;
    /// Every control request the native Claudes received, as they got it.
    fn controls(&self) -> Vec<serde_json::Value>;
    /// What the native Claudes got for each decision they asked for, in order.
    fn decisions(&self) -> Vec<serde_json::Value>;
    /// The driver's host dies with every process it started, and is gone once this returns.
    fn kill(&self, driver: &Driver);
}

/// The pinned Node host with the official SDK and the fake native Claude, which records what it gets
/// in files beside `capture`.
pub struct ProcessHosts {
    pub root: HostRoot,
    pub capture: PathBuf,
}

impl Default for ProcessHosts {
    fn default() -> Self {
        Self::from(HostRoot::new())
    }
}

impl From<HostRoot> for ProcessHosts {
    fn from(root: HostRoot) -> Self {
        Self { root, capture: PathBuf::new() }
    }
}

impl ProcessHosts {
    fn recorded(&self, suffix: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(format!("{}.{suffix}", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }
}

impl Hosts for ProcessHosts {
    fn backend(&mut self, home: &Path) -> SdkBackend {
        let (backend, capture) = fixture_backend(home, &self.root);
        self.capture = capture;
        backend
    }

    fn serve(&self, _: &DomainRuntime) {}

    fn starts(&self) -> Vec<serde_json::Value> {
        self.recorded("starts")
    }

    fn prompts(&self) -> Vec<String> {
        self.recorded("prompts").into_iter().map(|prompt| serde_json::from_value(prompt).unwrap()).collect()
    }

    fn controls(&self) -> Vec<serde_json::Value> {
        self.recorded("controls")
    }

    fn decisions(&self) -> Vec<serde_json::Value> {
        self.recorded("decisions")
    }

    fn kill(&self, driver: &Driver) {
        kill_host(driver);
    }
}

/// Kills the driver's whole host group and waits until each of its processes exited.
pub fn kill_host(driver: &Driver) {
    let group = driver.worker.host_group().unwrap();
    let mut exits = alondra_process_group::Exits::new().unwrap();
    for member in alondra_process_group::members(group).unwrap() {
        exits.watch(&member).unwrap();
    }
    assert!(std::process::Command::new("/bin/kill").args(["-KILL", "--", &format!("-{group}")]).status().unwrap().success());
    exits.wait().unwrap();
}

pub struct Slice<H: Hosts = crate::engine_tests::SliceHosts> {
    pub domain: Arc<DomainRuntime>,
    pub database: PathBuf,
    pub worktree: PathBuf,
    pub plugin: PathBuf,
    pub backend: SdkBackend,
    /// Every raw host event the daemon committed for the task, by cursor, as it was committed.
    pub kept: Arc<Mutex<BTreeMap<u64, Vec<u8>>>>,
    /// Before `_home`: the hosts write in the HOME they get, so they end before it is removed.
    pub hosts: H,
    pub _home: Directory,
}

impl<H: Hosts> Drop for Slice<H> {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
    }
}

/// Adds to `kept` the task's raw host events the domain still holds the bytes of.
pub fn keep(database: &Path, kept: &Mutex<BTreeMap<u64, Vec<u8>>>) {
    let observer = Connection::open(database).unwrap();
    let mut statement = observer.prepare("SELECT cursor,bytes FROM provider_observations WHERE task_id=?1 AND length(bytes)>1").unwrap();
    let held = statement.query_map([TASK], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))).unwrap().map(Result::unwrap);
    let mut kept = kept.lock().unwrap();
    for (cursor, bytes) in held {
        kept.entry(cursor as u64).or_insert(bytes);
    }
}

pub fn user() -> Actor {
    Actor::User { user_id: "slice-user".into() }
}

pub fn envelope(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command }
}

pub fn measure_healthy_machine(domain: &DomainRuntime) {
    alondra_engine_admission::measure_machine(&*domain, &alondra_core::Machine { memory_total: 16 << 30, memory_available: 12 << 30,
        memory_free_percent: Some(75), cpus: 11, disk_total: 500_000_000_000, disk_available: 100_000_000_000, ..Default::default() }, alondra_engine_ports::disk::Space { total: 500_000_000_000, free: 100_000_000_000, purgeable: 0 });
}

impl Slice {
    pub fn new() -> Self {
        Self::with_hosts(Default::default())
    }

    pub fn lifecycle(domain: &dyn alondra_engine_ports::Runtime) -> alondra_domain::TaskLifecycle {
        let read = domain.read();
        replay(&TASK.into(), &read.state().lifecycle_journal[&TaskId::from(TASK)]).unwrap()
    }

    /// Every card an ended turn of the task rested on, in the read the clients get, with whether
    /// it is settled.
    pub fn read_cards(domain: &dyn alondra_engine_ports::Runtime) -> Vec<(String, bool)> {
        let read = domain.read();
        read.state()
            .interactions
            .values()
            .filter(|card| rests_on(TASK, &card.id))
            .map(|card| (card.id.0.clone(), card.withdrawn || matches!(&card.payload, InteractionPayload::Question { answer: Some(_), .. })))
            .collect()
    }

    /// What the turn-end card of the task asks the user.
    pub fn turn_end_need(domain: &DomainRuntime) -> String {
        let read = domain.read();
        let state = read.state();
        let card = state.interactions.values().find(|card| card.id.0.starts_with(&format!("{TURN_ENDED_PREFIX}{TASK}-"))).expect("a turn-end card");
        card.card.effective_title().replace(&state.tasks[&TaskId::from(TASK)].title, "<title>")
    }

    pub fn decision_cards(domain: &DomainRuntime) -> Vec<DecisionCard> {
        let read = domain.read();
        read.state()
            .interactions
            .values()
            .filter(|card| card.id.0.starts_with(DECISION_CARDS))
            .map(|card| DecisionCard {
                id: card.id.0.clone(),
                answered: matches!(&card.payload, InteractionPayload::Question { answer: Some(_), .. }),
                withdrawn: card.withdrawn,
            })
            .collect()
    }

    pub fn open_decision_cards(domain: &DomainRuntime) -> Vec<String> {
        Self::decision_cards(domain).into_iter().filter(|card| !card.answered && !card.withdrawn).map(|card| card.id).collect()
    }

    /// The cards of the task that are not how a turn ended, open or not.
    pub fn other_cards(domain: &DomainRuntime) -> Vec<String> {
        domain.read().state().interactions.keys()
            .filter(|card| card.0.contains(TASK) && !rests_on(TASK, card))
            .map(|card| card.0.clone())
            .collect()
    }
}

impl<H: Hosts> Slice<H> {
    pub fn with_hosts(hosts: H) -> Self { Self::with_database_name(hosts,"domain.sqlite") }

    pub fn with_database_name(hosts: H,name:&str)->Self { Self::with_request_body(hosts,name,"Write the release notes") }

    pub fn with_request_body(mut hosts: H,name:&str,request:&str)->Self {
        let home = Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let worktree = home.join("worktree");
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        let plugin = home.join("plugin");
        std::fs::DirBuilder::new().mode(0o700).create(&plugin).unwrap();
        let backend = hosts.backend(&home);
        let database = home.join(name);
        let domain = Arc::new(super::open_at(&home, &database).unwrap());
        hosts.serve(&domain);
        measure_healthy_machine(&domain);
        let slice = Self { domain, database, worktree, plugin, backend, kept: Arc::default(), _home: home, hosts };
        slice.on_acknowledge(|| {});
        slice.run(&Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest {
            request_id: "sdk-slice-request".into(),
            body: request.into(),
            auto_merge: false,
            maintenance: false,
            message: false,
        });
        slice.run(&Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "sdk-slice-request".into(), title: "Release notes".into() });
        slice.run(&Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
            plan_items: None, continues: None,
            task_id: TASK.into(),
            request_id: "sdk-slice-request".into(),
            title: "Write the release notes".into(),
            agent_id: None,
            review_prs: false,
            dependencies: BTreeSet::new(),
            stack: None,
            launch: Some(LaunchPlan::new("node", slice.worktree.to_str().unwrap())),
        });
        slice
    }

    pub fn run(&self, actor: &Actor, id: &str, command: Command) {
        self.try_run(actor, id, command).unwrap();
    }

    pub fn try_run(&self, actor: &Actor, id: &str, command: Command) -> Result<CommandOutcome, StoreError> {
        let state = self.domain.state().unwrap();
        self.domain.execute_fixture(actor, envelope(&state, id, command))
    }

    pub fn launch(&self) -> Driver {
        self.launch_told("Write the release notes", capabilities())
    }

    pub fn launch_with(&self, prompt: &str) -> Driver {
        self.launch_told(prompt, capabilities())
    }

    /// Launches the task told `prompt`, on a binding that permits `capabilities`.
    pub fn launch_told(&self, prompt: &str, capabilities: BTreeSet<Capability>) -> Driver {
        self.launch_with_backend(&self.backend, prompt, capabilities)
    }

    pub fn launch_with_backend(&self, backend: &SdkBackend, prompt: &str, capabilities: BTreeSet<Capability>) -> Driver {
        start_bound(self.domain.clone(), backend, Launch {
            task: TASK.into(),
            name: "Release notes".into(),
            directory: self.worktree.to_str().unwrap().into(),
            prompt: prompt.into(),
            plugin: self.plugin.clone(),
            options: LaunchOptions::default(),
            model: None,
            account: AccountId::default_account(),
        }, capabilities, None, None)
        .unwrap()
    }

    /// Launches a turn that the fake Claude holds open, and reads the host until it runs.
    pub fn launch_working(&self) -> Driver {
        let mut driver = self.launch_told("consume-and-hold", capabilities());
        while Slice::lifecycle(&*self.domain).activity() != ActivityView::Working {
            driver.observe_written().unwrap();
        }
        driver
    }


    /// Every control request the fake Claude received, as it got it.
    pub fn controls(&self) -> Vec<serde_json::Value> {
        self.hosts.controls()
    }

    /// The task's controls in the store: kind, argument and status, in order.
    pub fn control_rows(&self) -> Vec<(String, Option<String>, String)> {
        let observer = self.observer();
        let mut statement = observer.prepare("SELECT kind,argument,status FROM lifecycle_controls WHERE task_id=?1 ORDER BY id").unwrap();
        statement.query_map([TASK], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().map(Result::unwrap).collect()
    }

    pub fn journal_events(&self) -> i64 {
        self.observer().query_row("SELECT count(*) FROM lifecycle_events WHERE task_id=?1", [TASK], |row| row.get(0)).unwrap()
    }

    /// Runs `hook` after each commit of a host event, once the event is kept. The domain keeps an
    /// event's bytes only until the next one is normalized, so they are kept here first.
    pub fn on_acknowledge(&self, mut hook: impl FnMut() + Send + 'static) {
        let (database, kept) = (self.database.clone(), Arc::clone(&self.kept));
        self.domain.on_acknowledge(move || {
            keep(&database, &kept);
            hook();
        });
    }

    /// The raw host events committed for the task that carry `text`.
    pub fn observations_with(&self, text: &str) -> usize {
        keep(&self.database, &self.kept);
        self.kept.lock().unwrap().values().filter(|bytes| String::from_utf8_lossy(bytes).contains(text)).count()
    }

    /// Reads the host until Claude's turn has its result, whatever the lifecycle then waits for.
    pub fn read_turn_result(&self, driver: &mut Driver) {
        while self.observations_with("\"type\":\"result\"") == 0 {
            driver.observe_written().unwrap();
        }
    }

    /// Reads the host until the task's lifecycle no longer waits for it, sending what the store
    /// issues meanwhile: the wake after an empty turn reaches Claude, whose next turn ends too.
    pub fn settle(&self, driver: &mut Driver) {
        while driver.pending_permits() > 0 || driver.waits_for_host() {
            advance(driver);
        }
    }

    /// The prompts Claude got, besides the wake Alondra sends after a turn that ended empty.
    pub fn prompts(&self) -> Vec<String> {
        not_wakes(self.hosts.prompts().into_iter())
    }

    pub fn observer(&self) -> Connection {
        Connection::open(&self.database).unwrap()
    }



    pub fn stored_cards(&self) -> i64 {
        stored_turn_ends(&self.database)
    }
}

/// The cards ended turns of the task rested on, committed in SQL.
pub fn stored_turn_ends(database: &Path) -> i64 {
    Connection::open(database).unwrap()
        .query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1 OR id LIKE ?2",
            [format!("{STALLED_PREFIX}{TASK}-%"), format!("{TURN_ENDED_PREFIX}{TASK}-%")], |row| row.get(0))
        .unwrap()
}

pub fn committed_revision(database: &Path) -> u64 {
    Connection::open(database).unwrap().query_row("SELECT revision FROM domain_meta WHERE singleton=1", [], |row| row.get::<_, i64>(0)).unwrap() as u64
}

/// At every acknowledgement, what readers see is the revision SQL committed.
pub fn watch_acknowledgements<H: Hosts>(slice: &Slice<H>) -> Arc<Mutex<Vec<(u64, u64)>>> {
    let seen = Arc::new(Mutex::new(vec![]));
    let (recorded, database, domain) = (Arc::clone(&seen), slice.database.clone(), Arc::downgrade(&slice.domain));
    slice.on_acknowledge(move || {
        let (published, committed) = (domain.upgrade().unwrap().read().state().revision, committed_revision(&database));
        assert_eq!(published, committed, "readers see the committed revision when its observation is acknowledged");
        recorded.lock().unwrap().push((published, committed));
    });
    seen
}

// A5: deterministic counts of the slice's work, never its time: per step, how many lifecycle
// transactions it ran, how many aggregates the store rebuilt from their journals, how many journal
// events it decoded, how many writing statements the daemon issued and how many read indexes
// publication built.
#[derive(Debug, PartialEq, Eq)]
pub struct Work {
    pub transactions: u64,
    pub aggregate_loads: u64,
    pub event_decodes: u64,
    pub writes: u64,
    pub index_builds: u64,
}

/// The closer reaches an SDK node only through its owner: `claude rm` and the session listing are
/// never asked about one.
pub struct NoClaudeSessions;
impl alondra_daemon_engine::node_close::Sessions for NoClaudeSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> alondra_daemon_engine::node_close::Answer<'a, Result<alondra_daemon_engine::node_close::Ran, String>> {
        panic!("the closer ran claude {args:?} for an SDK node")
    }
    fn leftovers<'a>(&'a self, session_id: &'a str) -> alondra_daemon_engine::node_close::Answer<'a, Option<String>> {
        panic!("the closer looked for leftovers of {session_id}, an SDK node")
    }
}

/// The owner generation currently fencing the slice's execution, read straight from SQL.
pub fn owner_generation(database: &Path) -> i64 {
    Connection::open(database).unwrap().query_row("SELECT owner_generation FROM provider_execution_owners", [], |row| row.get(0)).unwrap()
}

/// The owner of the task's host goes without stopping it, as the owner of 0.3.0 did when a step
/// failed, and the host's owner socket goes too: the host lives on, holds its lock, and no owner
/// can ever reach it again.
pub fn strand_host(slice: &Slice<ProcessHosts>, driver: Driver) -> (String, Vec<alondra_process_group::Member>) {
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let processes = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let folder = slice.backend.state_root.join(&driver.agent().0);
    drop(driver);
    std::fs::remove_file(folder.join("s")).unwrap();
    assert!(processes.iter().all(|process| alondra_process_group::alive(process).unwrap()), "the host still runs");
    (session, processes)
}

/// The background commands the fake Claude started, by process id.
pub fn background_commands(slice: &Slice<ProcessHosts>) -> Vec<libc::pid_t> {
    std::fs::read_to_string(format!("{}.background", slice.hosts.capture.display())).unwrap_or_default().lines().map(|line| line.parse().unwrap()).collect()
}

pub fn running(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Reads the host until the task's lifecycle waits only on the background work Claude left running.
pub fn until_own_work(slice: &Slice<ProcessHosts>, driver: &mut Driver) {
    while lifecycle(&*slice.domain).activity() != ActivityView::OwnWork {
        driver.observe_written().unwrap();
    }
    assert!(background_commands(slice).into_iter().all(running), "the background command runs");
}

/// The host that replaced the stranded one ended the background command with it, the resumed
/// session is told that command ended and will not report back, and it goes on: its turn ends.
pub fn assert_told_the_work_ended(slice: &Slice<ProcessHosts>, domain: &Arc<DomainRuntime>, mut driver: Driver, session: &str) {
    assert!(background_commands(slice).into_iter().all(|pid| !running(pid)), "the background command ended with its host");
    assert!(open_card(domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
    assert_eq!(driver.pending_permits(), 1, "the resumed session is told to go on");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let starts = slice.starts();
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session), "the same session is resumed: {starts:?}");
    let told = slice.prompts().last().unwrap().clone();
    assert!(told.starts_with("Your session was cut off when its host stopped"), "{told}");
    assert!(told.contains("This background work ended with that host and will not report back:\n- bfixture1 (bazel test //app:snapshots)\n"), "{told}");
    assert_eq!(lifecycle(&**domain).activity(), ActivityView::Resting, "its turn ended: nothing waits for a notification");
    driver.stop().unwrap();
}

impl<H: Hosts> Slice<H> {
    /// A second task of the request, ready to launch in the slice's worktree.
    pub fn task(&self, task: &str) {
        self.run(&Actor::Orchestrator { generation: 0 }, &format!("task-{task}"), Command::CreateTask {
            plan_items: None, continues: None,
            task_id: task.into(),
            request_id: "sdk-slice-request".into(),
            title: format!("Work on {task}"),
            agent_id: None,
            review_prs: false,
            dependencies: BTreeSet::new(),
            stack: None,
            launch: Some(LaunchPlan::new("node", self.worktree.to_str().unwrap())),
        });
    }

    pub fn launch_on(&self, task: &str, account: &str) -> Result<Driver, Failed> {
        start(self.domain.clone(), &self.backend, Launch {
            task: task.into(), name: format!("Work on {task}"), directory: self.worktree.to_str().unwrap().into(),
            prompt: format!("Work on {task}"), plugin: self.plugin.clone(), options: LaunchOptions::default(), model: None,
            account: AccountId::parse(account).unwrap(),
        })
    }

    /// The account and generation of the credential the task's execution is bound to.
    pub fn credential(&self, task: &str) -> (String, u64) {
        let observer = Connection::open(&self.database).unwrap();
        observer.query_row(
            "SELECT b.credential, b.credential_generation FROM lifecycle_execution_bindings e JOIN provider_bindings b ON b.id=e.binding_id WHERE e.task_id=?1 ORDER BY e.generation DESC LIMIT 1",
            [task], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)),
        ).unwrap()
    }

    pub fn accounts_seen(&self, id: &str, work: AccountState) {
        let accounts = [("default", AccountState::SignedIn), ("work", work)].map(|(id, state)| AccountStatus {
            provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: id.into(), state, native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0, profile: None,
        }).to_vec();
        self.run(&Actor::System, id, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
    }
}

pub fn sha256(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

impl<H: Hosts> Slice<H> {
    /// The driver's host dies with every process it started.
    pub fn kill_host(&self, driver: &Driver) {
        self.hosts.kill(driver);
    }

    /// The Mac goes down: the driver's host dies, its native Claude included, and the daemon with
    /// them.
    pub fn kill_host_and_daemon(&self, driver: Driver) {
        self.hosts.kill(&driver);
        drop(driver);
    }

    /// Each native Claude the hosts started: its session, the session it resumed and how many
    /// prompts that session's transcript already held.
    pub fn starts(&self) -> Vec<serde_json::Value> {
        self.hosts.starts()
    }

    pub fn restart(&self) -> Arc<DomainRuntime> {
        let domain = Arc::new(super::open_at(&self._home, &self.database).unwrap());
        self.hosts.serve(&domain);
        measure_healthy_machine(&domain);
        domain
    }

    pub fn take_over(&self, domain: &Arc<DomainRuntime>) -> Driver {
        adopt(domain.clone(), &self.backend, TASK.into(), Some(&self.plugin)).unwrap()
    }

    /// Reads the host until its turn is running.
    pub fn until_working(&self, driver: &mut Driver) {
        while lifecycle(&*self.domain).activity() != ActivityView::Working {
            driver.observe_written().unwrap();
        }
    }
}

pub fn lifecycle(domain: &dyn alondra_engine_ports::Runtime) -> alondra_domain::TaskLifecycle {
    let read = domain.read();
    replay(&TASK.into(), &read.state().lifecycle_journal[&TaskId::from(TASK)]).unwrap()
}

/// The open card of the task whose id starts with `prefix`.
pub fn open_card(domain: &DomainRuntime, prefix: &str) -> Option<Interaction> {
    domain.read().state().interactions.values()
        .find(|card| card.id.0.starts_with(prefix) && !card.withdrawn && matches!(&card.payload, InteractionPayload::Question { answer: None, .. }))
        .cloned()
}

pub fn answer(domain: &DomainRuntime, card: &InteractionId, option: &str) {
    let state = domain.state().unwrap();
    let answer = Command::AnswerQuestion { interaction_id: card.clone(), option_id: option.into(), inputs: BTreeMap::new(), attachments: vec![] };
    assert!(domain.execute_with(&user(), &envelope(&state, &format!("client-{option}"), answer), None, None, || Ok(())).unwrap().changed);
}

pub fn system() -> Actor {
    Actor::System
}

pub fn subtypes(controls: &[serde_json::Value]) -> Vec<&str> {
    controls.iter().map(|control| control["subtype"].as_str().unwrap()).collect()
}

pub const DECISION_CARDS: &str = "decision-sdk-slice-";

/// A task's card that asks the user for a decision Claude waits on.
#[derive(Debug, PartialEq)]
pub struct DecisionCard {
    pub id: String,
    pub answered: bool,
    pub withdrawn: bool,
}

impl<H: Hosts> Slice<H> {
    /// What the fake Claude got for each decision it asked for, in order.
    pub fn decisions(&self) -> Vec<serde_json::Value> {
        self.hosts.decisions()
    }



    pub fn options(&self, card: &str) -> Vec<String> {
        let state = self.domain.state().unwrap();
        let InteractionPayload::Question { options, .. } = &state.interactions[&InteractionId(card.into())].payload else { panic!("{card} is no question") };
        options.iter().map(|option| option.id.clone()).collect()
    }

    /// Runs the owner one step at a time until `done`; it never waits for a host that has nothing
    /// the task waits for.
    pub fn until(&self, driver: &mut Driver, done: impl Fn(&Self) -> bool) {
        while !done(self) {
            assert!(driver.pending_controls() > 0 || driver.pending_permits() > 0 || driver.waits_for_host(),
                "nothing is left to run and it is not done: {:?}", Slice::lifecycle(&*self.domain).activity());
            driver.step(&|| false).unwrap();
        }
    }

    /// Launches the task asking Claude for `scenario` and runs it until the user is asked.
    pub fn asked(&self, scenario: &str) -> (Driver, String) {
        let mut driver = self.launch_with(scenario);
        self.until(&mut driver, |slice| !Slice::open_decision_cards(&slice.domain).is_empty());
        let card = Slice::open_decision_cards(&self.domain).remove(0);
        (driver, card)
    }

    pub fn answer(&self, card: &str, option: &str, note: Option<&str>) -> Result<CommandOutcome, StoreError> {
        let inputs = note.map(|note| BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(note.into()))])).unwrap_or_default();
        self.try_run(&user(), &format!("answer-{card}"), Command::AnswerQuestion {
            interaction_id: card.into(),
            option_id: option.into(),
            inputs,
            attachments: vec![],
        })
    }

    /// Runs the owner until nothing is left to send and the task, as its owner's daemon reads it,
    /// rests.
    pub fn rest(&self, driver: &mut Driver) {
        while driver.pending_controls() > 0 || driver.pending_permits() > 0 || driver.waits_for_host() {
            driver.step(&|| false).unwrap();
        }
        assert_eq!(Slice::lifecycle(&**driver.domain()).activity(), ActivityView::Resting);
    }

    pub fn decision_requests(&self) -> Vec<(i64, String)> {
        let observer = self.observer();
        let mut statement = observer.prepare("SELECT epoch,kind FROM lifecycle_decision_requests WHERE task_id=?1 ORDER BY epoch").unwrap();
        statement.query_map([TASK], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect()
    }
}


/// The task is cancelled from its turn-end card, which stays answered with that choice.
pub fn assert_cancelled_from_its_card<H: Hosts>(slice: &Slice<H>, reply: &serde_json::Value) {
    assert_eq!(reply["outcome"], "applied", "{reply}");
    let state = slice.domain.state().unwrap();
    assert!(state.tasks[&TaskId::from(TASK)].cancelled(), "the task is cancelled");
    let cards: Vec<_> = state.interactions.values()
        .filter(|card| card.id.0.starts_with(&format!("{TURN_ENDED_PREFIX}{TASK}-")))
        .map(|card| (matches!(&card.payload, InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == alondra_domain_rules::CANCEL_TASK), card.withdrawn))
        .collect();
    assert!(cards.contains(&(true, false)), "the card keeps the user's choice: {cards:?}");
}

impl<H: Hosts> Slice<H> {
    /// Launches task `task` of the request told `prompt`, and reads its host until `done`.
    pub fn launch_until(&self, task: &str, prompt: &str, done: impl Fn(&DomainState, &AgentId) -> bool) -> Driver {
        let mut driver = start_bound(self.domain.clone(), &self.backend, Launch {
            task: task.into(), name: format!("Work on {task}"), directory: self.worktree.to_str().unwrap().into(), prompt: prompt.into(),
            plugin: self.plugin.clone(), options: LaunchOptions::default(), model: None, account: AccountId::default_account(),
        }, capabilities(), None, None).unwrap();
        while !done(&self.domain.state().unwrap(), driver.agent()) {
            assert!(driver.pending_controls() > 0 || driver.pending_permits() > 0 || driver.waits_for_host(),
                "{task} has nothing left to run: {:?}", self.domain.state().unwrap().node_phase(driver.agent()));
            driver.step(&|| false).unwrap();
        }
        driver
    }
}

impl<H: Hosts> Slice<H> {
    /// The backend a daemon of the release before ran: the same host with other bytes, as the host
    /// of every release is, kept apart from this release's.
    pub fn release_before(&self) -> SdkBackend {
        use std::io::Write;
        let host = self._home.join("alondra-host-before");
        std::fs::copy(&self.backend.host, &host).unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::OpenOptions::new().append(true).open(&host).unwrap().write_all(b"\n// the release before\n").unwrap();
        let sha256 = backend::digest(&host).unwrap();
        assert_ne!(sha256, self.backend.host_sha256);
        SdkBackend { node: host.clone(), node_sha256: sha256.clone(), host, host_sha256: sha256, ..self.backend.clone() }
    }

}

impl Slice<ProcessHosts> {
    /// The command Claude left running in the background (`background-command`).
    pub fn background_command(&self) -> alondra_process_group::Member {
        let pid: i32 = std::fs::read_to_string(format!("{}.background", self.hosts.capture.display())).unwrap().parse().unwrap();
        alondra_process_group::members(alondra_process_group::group(pid).unwrap().unwrap()).unwrap()
            .into_iter().find(|member| member.pid == pid).expect("the background command runs")
    }

    /// Claude starts a turn on the prompts it held with `stall-while-held`, from now on.
    pub fn let_claude_start(&self) {
        std::fs::write(format!("{}.start", self.hosts.capture.display()), "").unwrap();
    }

    /// Ends the command Claude left running in the background.
    pub fn release_background_command(&self) {
        std::fs::write(format!("{}.release", self.hosts.capture.display()), "done\n").unwrap();
    }

    /// A daemon of this release that took over a node running a command in the background on a
    /// host of the release before, as an upgrade leaves it: the daemon, its driver, the node's
    /// Claude session and the old host's processes.
    pub fn upgraded_during_a_background_command(&self) -> (Arc<DomainRuntime>, Driver, String, Vec<alondra_process_group::Member>) {
        let before = self.release_before();
        let mut driver = self.launch_with_backend(&before, "background-command", capabilities());
        while lifecycle(&*self.domain).activity() != ActivityView::OwnWork {
            driver.observe_written().unwrap();
        }
        let command = self.background_command();
        let session = self.starts()[0]["session"].as_str().unwrap().to_owned();
        let old_host = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
        // The daemon upgrades: it exits and leaves its hosts running for the next one.
        drop(driver);
        let upgraded = self.restart();
        upgraded.enable_sdk(self.backend.clone());
        let (_, adopted) = take_all(&(upgraded.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>), &self.backend).pop().expect("the start takes the execution");
        let adopted = adopted.unwrap_or_else(|Failed { why, .. }| panic!("the execution was not taken over: {why}"));
        assert!(alondra_process_group::alive(&command).unwrap(), "the background command survived the upgrade");
        assert!(adopted.worker.runs_another_release(), "the node goes on on the host of the release before");
        assert_eq!(self.starts().len(), 1, "no Claude started again");
        (upgraded, adopted, session, old_host)
    }

    /// What the orchestrator said after the move reached the same session on a host of this release, the
    /// host of the release before is gone, and nothing was asked.
    pub fn assert_moved_to_this_release(&self, upgraded: &DomainRuntime, session: &str, old_host: &[alondra_process_group::Member]) {
        let starts = self.starts();
        assert_eq!(starts.len(), 2, "one more Claude started, on this release's host: {starts:?}");
        assert_eq!(starts[1]["resume"].as_str(), Some(session), "it resumed the same session");
        let prompts = self.prompts();
        assert_eq!(prompts.len(), 2, "the new host's Claude got only what the orchestrator said, nothing about a cut: {prompts:?}");
        assert!(prompts[1].ends_with("Add the tests first") && !prompts[1].contains("cut off"), "nothing about a cut: {prompts:?}");
        let left: Vec<String> = old_host.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
        assert!(left.is_empty(), "the host of the release before is gone: {left:?}");
        assert_eq!(Slice::other_cards(upgraded), Vec::<String>::new(), "moving the node asked the user nothing");
    }

}

#[path = "codex_fixtures.rs"]
pub mod codex;
#[path = "untaken_fixtures.rs"]
pub mod untaken;
