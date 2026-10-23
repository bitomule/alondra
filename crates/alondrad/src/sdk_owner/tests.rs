//! The vertical slice through the real owner: the daemon's store, commands and `own`, the pinned
//! Node host with the official SDK, and a fake native Claude that answers each prompt with a
//! result. Each step waits on the host's next event, never on a clock.
use super::*;
use crate::test_support::Directory;
use alondra_core::accounts::{AccountId, AccountState, AccountStatus, Provider};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const TASK: &str = "sdk-slice";

fn runfile(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} names a runfile")));
    std::fs::canonicalize(path).unwrap()
}

/// Waits for the end of the test process that started it, then kills the process group of every
/// host still holding a file under the folder and removes the folder. Dropping a `HostRoot` ends
/// it first, while that process lives: then it does nothing.
const REAPER: &str = r#"
import os, shutil, signal, subprocess, sys
root, owner = sys.argv[1], int(sys.argv[2])
sys.stdin.buffer.read()
if os.getppid() == owner:
    sys.exit(0)
listed = subprocess.run(['/usr/sbin/lsof', '-t', '+D', root], capture_output=True, text=True).stdout.split()
for pid in {int(pid) for pid in listed}:
    try:
        if os.getpgid(pid) == pid:
            os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
shutil.rmtree(os.path.dirname(root), ignore_errors=True)
"#;

/// The folder of a test's SDK hosts: the test owns every host started under it. Dropping it kills
/// each host left with its whole process group, and fails the test that left one running; if the
/// test process dies without unwinding (a timeout, a kill), its reaper kills them instead.
pub(crate) struct HostRoot {
    reaper: std::process::Child,
    directory: Directory,
}

impl HostRoot {
    pub(crate) fn new() -> Self {
        Self::owned_by(std::process::id())
    }

    fn owned_by(owner: u32) -> Self {
        use std::os::unix::process::CommandExt;
        let directory = Directory::for_unix_socket();
        let reaper = std::process::Command::new("/usr/bin/python3")
            .args(["-I", "-c", REAPER])
            .arg(&*directory)
            .arg(owner.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("start the hosts' reaper");
        Self { reaper, directory }
    }

    /// Kills the process group of every host still running here and waits for each to end: the
    /// hosts it killed.
    fn kill_hosts(&self) -> Vec<String> {
        use std::os::unix::fs::MetadataExt;
        let Ok(folders) = std::fs::read_dir(self.directory.join("h")) else {
            return Vec::new();
        };
        let me = std::process::id() as i32;
        let mut killed = Vec::new();
        for folder in folders {
            let folder = folder.unwrap().path();
            let Ok(lock) = std::fs::metadata(folder.join("host.lock")) else {
                continue;
            };
            for holder in alondra_process_group::holders(lock.dev(), lock.ino()).unwrap() {
                if holder.pid == me || holder.zombie || alondra_process_group::group(holder.pid).unwrap() != Some(holder.pid) {
                    continue;
                }
                let mut exits = alondra_process_group::Exits::new().unwrap();
                for member in alondra_process_group::members(holder.pid).unwrap().iter().filter(|member| !member.zombie) {
                    exits.watch(member).unwrap();
                }
                assert_eq!(unsafe { libc::killpg(holder.pid, libc::SIGKILL) }, 0, "kill the group of {holder}");
                exits.wait().unwrap();
                killed.push(format!("{} ({holder})", folder.file_name().unwrap().to_string_lossy()));
            }
        }
        killed
    }
}

impl std::ops::Deref for HostRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.directory
    }
}

impl AsRef<Path> for HostRoot {
    fn as_ref(&self) -> &Path {
        &self.directory
    }
}

impl HostRoot {
    fn end_reaper(&mut self) {
        drop(self.reaper.stdin.take());
        self.reaper.wait().unwrap();
    }
}

impl Drop for HostRoot {
    fn drop(&mut self) {
        let left = self.kill_hosts();
        self.end_reaper();
        if !left.is_empty() && !std::thread::panicking() {
            panic!("the test left SDK hosts running: {}", left.join(", "));
        }
    }
}

/// The pinned Node host with the official SDK and a fake native Claude that records what it is
/// launched with in the returned capture, its hosts' folders under `state`.
pub(crate) fn fixture_backend(home: &Path, state: &HostRoot) -> (SdkBackend, PathBuf) {
    let native = home.join("native");
    std::fs::copy(runfile("FIXTURE_NATIVE"), &native).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let capture = home.join("native-capture.json");
    let environment = BTreeMap::from([("HOME".to_owned(), home.to_str().unwrap().to_owned()), ("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
    let mut native_environment = environment.clone();
    native_environment.insert("FIXTURE_CAPTURE".into(), capture.to_str().unwrap().into());
    let (node, host) = (runfile("FIXTURE_NODE"), runfile("FIXTURE_HOST"));
    let backend = SdkBackend {
        node_sha256: backend::digest(&node).unwrap(),
        host_sha256: backend::digest(&host).unwrap(),
        native_sha256: backend::digest(&native).unwrap(),
        node,
        host,
        native,
        host_environment: environment,
        native_environment,
        confinement: None,
        state_root: state.join("h"),
        io_timeout_seconds: 30,
        installed_native: false,
    };
    (backend, capture)
}

/// A domain whose launches start through the fixture backend: what the native Claude was started
/// with, the prompts it got, and every host of the domain stopped when this is dropped. Declare it
/// before the directories it lives in, so its hosts stop first.
pub(crate) struct SdkLaunches {
    domain: Arc<DomainRuntime>,
    pub(crate) capture: PathBuf,
    state_root: PathBuf,
    _state: HostRoot,
}

impl SdkLaunches {
    pub(crate) fn enable(domain: &Arc<DomainRuntime>, home: &Path) -> Self {
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
    pub(crate) fn flag(&self, flag: &str) -> Option<String> {
        let args = self.args();
        args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1)).cloned()
            .or_else(|| args.iter().find_map(|arg| arg.strip_prefix(&format!("{flag}=")).map(str::to_owned)))
    }

    /// Every prompt the native Claudes got, in order.
    pub(crate) fn prompts(&self) -> Vec<String> {
        std::fs::read_to_string(format!("{}.prompts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    /// Each native Claude the hosts started: its session, the one it resumed and its folder.
    pub(crate) fn starts(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(format!("{}.starts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    /// What the launch of `agent`'s execution kept beside its host.
    pub(crate) fn options(&self, agent: &str) -> LaunchOptions {
        LaunchOptions::load(&self.state_root.join(agent)).unwrap()
    }

    /// Waits on the domain until `task`'s turn has ended with `turns` turn-end cards.
    pub(crate) async fn until_turns_ended(&self, task: &str, turns: usize) {
        let mut changed = self.domain.subscribe();
        let ended = |domain: &DomainRuntime| domain.read().state().interactions.keys()
            .filter(|card| card.0.starts_with(&format!("{TURN_ENDED_PREFIX}{task}-"))).count() >= turns;
        while !ended(&self.domain) {
            changed.changed().await.unwrap();
        }
    }

    /// Waits on the domain until `task`'s first turn ended with Claude's answer.
    pub(crate) async fn until_answered(&self, task: &str) {
        self.until_turns_ended(task, 1).await;
    }
}

impl Drop for SdkLaunches {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
    }
}

const HOLDING_NATIVE: &str = r#"#!/usr/bin/python3
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
    pub(crate) fn refuse_starts(&self, refused: bool) {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.state_root).unwrap();
        std::fs::set_permissions(&self.state_root, std::fs::Permissions::from_mode(if refused { 0o500 } else { 0o700 })).unwrap();
    }
}

struct Slice {
    domain: Arc<DomainRuntime>,
    database: PathBuf,
    capture: PathBuf,
    worktree: PathBuf,
    plugin: PathBuf,
    backend: SdkBackend,
    /// Every raw host event the daemon committed for the task, by cursor, as it was committed.
    kept: Arc<Mutex<BTreeMap<u64, Vec<u8>>>>,
    _home: Directory,
    _state: HostRoot,
}

impl Drop for Slice {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
    }
}

/// Adds to `kept` the task's raw host events the domain still holds the bytes of.
fn keep(database: &Path, kept: &Mutex<BTreeMap<u64, Vec<u8>>>) {
    let observer = Connection::open(database).unwrap();
    let mut statement = observer.prepare("SELECT cursor,bytes FROM provider_observations WHERE task_id=?1 AND length(bytes)>1").unwrap();
    let held = statement.query_map([TASK], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))).unwrap().map(Result::unwrap);
    let mut kept = kept.lock().unwrap();
    for (cursor, bytes) in held {
        kept.entry(cursor as u64).or_insert(bytes);
    }
}

fn user() -> Actor {
    Actor::User { user_id: "slice-user".into() }
}

fn envelope(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command }
}

impl Slice {
    fn new() -> Self {
        Self::with_hosts(HostRoot::new())
    }

    fn with_hosts(state: HostRoot) -> Self {
        let home = Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let worktree = home.join("worktree");
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        let plugin = home.join("plugin");
        std::fs::DirBuilder::new().mode(0o700).create(&plugin).unwrap();
        let (backend, capture) = fixture_backend(&home, &state);
        let database = home.join("domain.sqlite");
        let domain = Arc::new(DomainRuntime::open_at(&home, &database).unwrap());
        let slice = Self { domain, database, capture, worktree, plugin, backend, kept: Arc::default(), _home: home, _state: state };
        slice.on_acknowledge(|| {});
        slice.run(&Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest {
            request_id: "sdk-slice-request".into(),
            body: "Write the release notes".into(),
            auto_merge: false,
            maintenance: false,
            message: false,
        });
        slice.run(&Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "sdk-slice-request".into(), title: "Release notes".into() });
        slice.run(&Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
            plan_items: None,
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

    fn run(&self, actor: &Actor, id: &str, command: Command) {
        self.try_run(actor, id, command).unwrap();
    }

    fn try_run(&self, actor: &Actor, id: &str, command: Command) -> Result<CommandOutcome, StoreError> {
        let state = self.domain.state().unwrap();
        self.domain.execute_with(actor, &envelope(&state, id, command), None, None, || Ok(()))
    }

    fn launch(&self) -> Driver {
        self.launch_told("Write the release notes", capabilities())
    }

    fn launch_with(&self, prompt: &str) -> Driver {
        self.launch_told(prompt, capabilities())
    }

    /// Launches the task told `prompt`, on a binding that permits `capabilities`.
    fn launch_told(&self, prompt: &str, capabilities: BTreeSet<Capability>) -> Driver {
        self.launch_with_backend(&self.backend, prompt, capabilities)
    }

    fn launch_with_backend(&self, backend: &SdkBackend, prompt: &str, capabilities: BTreeSet<Capability>) -> Driver {
        start_bound(Arc::clone(&self.domain), backend, Launch {
            task: TASK.into(),
            name: "Release notes".into(),
            directory: self.worktree.to_str().unwrap().into(),
            prompt: prompt.into(),
            plugin: self.plugin.clone(),
            options: LaunchOptions::default(),
            model: None,
            account: AccountId::default_account(),
        }, capabilities, None)
        .unwrap()
    }

    /// Launches a turn that the fake Claude holds open, and reads the host until it runs.
    fn launch_working(&self) -> Driver {
        let mut driver = self.launch_told("consume-and-hold", capabilities());
        while Self::lifecycle(&self.domain).activity() != ActivityView::Working {
            driver.observe().unwrap();
        }
        driver
    }

    fn lifecycle(domain: &DomainRuntime) -> alondra_domain::TaskLifecycle {
        let read = domain.read();
        replay(&TASK.into(), &read.state().lifecycle_journal[&TaskId::from(TASK)]).unwrap()
    }

    /// Every control request the fake Claude received, as it got it.
    fn controls(&self) -> Vec<serde_json::Value> {
        let path = PathBuf::from(format!("{}.controls", self.capture.display()));
        std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    /// The task's controls in the store: kind, argument and status, in order.
    fn control_rows(&self) -> Vec<(String, Option<String>, String)> {
        let observer = self.observer();
        let mut statement = observer.prepare("SELECT kind,argument,status FROM lifecycle_controls WHERE task_id=?1 ORDER BY id").unwrap();
        statement.query_map([TASK], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().map(Result::unwrap).collect()
    }

    fn journal_events(&self) -> i64 {
        self.observer().query_row("SELECT count(*) FROM lifecycle_events WHERE task_id=?1", [TASK], |row| row.get(0)).unwrap()
    }

    /// Runs `hook` after each commit of a host event, once the event is kept. The domain keeps an
    /// event's bytes only until the next one is normalized, so they are kept here first.
    fn on_acknowledge(&self, mut hook: impl FnMut() + Send + 'static) {
        let (database, kept) = (self.database.clone(), Arc::clone(&self.kept));
        self.domain.on_acknowledge(move || {
            keep(&database, &kept);
            hook();
        });
    }

    /// The raw host events committed for the task that carry `text`.
    fn observations_with(&self, text: &str) -> usize {
        keep(&self.database, &self.kept);
        self.kept.lock().unwrap().values().filter(|bytes| String::from_utf8_lossy(bytes).contains(text)).count()
    }

    /// Reads the host until Claude's turn has its result, whatever the lifecycle then waits for.
    fn read_turn_result(&self, driver: &mut Driver) {
        while self.observations_with("\"type\":\"result\"") == 0 {
            driver.observe().unwrap();
        }
    }

    /// Reads the host until the task's lifecycle no longer waits for it.
    fn settle(&self, driver: &mut Driver) {
        while driver.waits_for_host() {
            driver.observe().unwrap();
        }
    }

    fn prompts(&self) -> Vec<String> {
        let path = PathBuf::from(format!("{}.prompts", self.capture.display()));
        std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    fn observer(&self) -> Connection {
        Connection::open(&self.database).unwrap()
    }

    /// Every turn-end card of the task, in the read the clients get, with whether it is answered.
    fn read_cards(domain: &DomainRuntime) -> Vec<(String, bool)> {
        let read = domain.read();
        read.state()
            .interactions
            .values()
            .filter(|card| card.id.0.starts_with(&format!("{TURN_ENDED_PREFIX}{TASK}-")))
            .map(|card| (card.id.0.clone(), matches!(&card.payload, InteractionPayload::Question { answer: Some(_), .. })))
            .collect()
    }

    /// What the turn-end card of the task asks the user.
    fn turn_end_need(domain: &DomainRuntime) -> String {
        let read = domain.read();
        let state = read.state();
        let card = state.interactions.values().find(|card| card.id.0.starts_with(&format!("{TURN_ENDED_PREFIX}{TASK}-"))).expect("a turn-end card");
        card.card.need.replace(&state.tasks[&TaskId::from(TASK)].title, "<title>")
    }

    fn stored_cards(&self) -> i64 {
        self.observer()
            .query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1", [format!("{TURN_ENDED_PREFIX}{TASK}-%")], |row| row.get(0))
            .unwrap()
    }
}

fn committed_revision(database: &Path) -> u64 {
    Connection::open(database).unwrap().query_row("SELECT revision FROM domain_meta WHERE singleton=1", [], |row| row.get::<_, i64>(0)).unwrap() as u64
}

/// At every acknowledgement, what readers see is the revision SQL committed.
fn watch_acknowledgements(slice: &Slice) -> Arc<Mutex<Vec<(u64, u64)>>> {
    let seen = Arc::new(Mutex::new(vec![]));
    let (recorded, database, domain) = (Arc::clone(&seen), slice.database.clone(), Arc::downgrade(&slice.domain));
    slice.on_acknowledge(move || {
        let (published, committed) = (domain.upgrade().unwrap().read().state().revision, committed_revision(&database));
        assert_eq!(published, committed, "readers see the committed revision when its observation is acknowledged");
        recorded.lock().unwrap().push((published, committed));
    });
    seen
}

// A1: a task launched through the SDK backend runs its first turn on the real host; the end of
// that turn commits one card with the raw observation, and every acknowledgement leaves only
// once readers see the commit it seals.
fn host_left_running(slice: &Slice) -> Vec<alondra_process_group::Member> {
    let driver = slice.launch();
    let processes = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    drop(driver);
    assert!(processes.iter().all(|process| alondra_process_group::alive(process).unwrap()), "a dropped owner leaves its host running");
    processes
}

fn survivors(processes: &[alondra_process_group::Member]) -> Vec<String> {
    processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect()
}

#[test]
fn a_test_that_leaves_a_host_running_fails_and_the_host_ends_with_it() {
    let slice = Slice::new();
    let processes = host_left_running(&slice);
    let failure = catch_unwind(AssertUnwindSafe(|| drop(slice))).unwrap_err();
    let message = failure.downcast_ref::<String>().unwrap();
    assert!(message.starts_with("the test left SDK hosts running: sdk-"), "{message}");
    assert_eq!(survivors(&processes), Vec::<String>::new(), "the host's group ended with the test");
}

#[test]
fn the_hosts_of_a_test_process_that_dies_without_unwinding_end_and_their_folder_goes() {
    let mut owner = std::process::Command::new("/bin/sleep").arg("600").stdin(std::process::Stdio::null()).spawn().unwrap();
    let mut slice = Slice::with_hosts(HostRoot::owned_by(owner.id()));
    let processes = host_left_running(&slice);
    owner.kill().unwrap();
    owner.wait().unwrap();
    let root = slice._state.to_path_buf();
    slice._state.end_reaper();
    assert_eq!(survivors(&processes), Vec::<String>::new(), "the reaper killed the host's group");
    assert!(!root.parent().unwrap().exists(), "the reaper removed the hosts' folder");
}

#[test]
fn a_task_launched_through_the_sdk_ends_its_turn_with_one_card_readers_see_before_the_host_is_acknowledged() {
    let slice = Slice::new();
    let acknowledged = watch_acknowledgements(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts(), ["Write the release notes"], "the host's Claude got the launch prompt");
    let cards = Slice::read_cards(&slice.domain);
    assert_eq!(cards.len(), 1, "one turn-end card: {cards:?}");
    assert!(!cards[0].1, "the card waits for the user");
    assert_eq!(slice.stored_cards(), 1);
    let acknowledged = acknowledged.lock().unwrap().clone();
    assert!(!acknowledged.is_empty(), "the host's observations were acknowledged");
    for (published, committed) in acknowledged {
        assert_eq!(published, committed, "readers see the committed revision when its observation is acknowledged");
    }
    let raw: i64 = slice.observer().query_row("SELECT count(*) FROM provider_observations WHERE task_id=?1", [TASK], |row| row.get(0)).unwrap();
    assert!(raw >= 2, "the raw observations are kept: {raw}");
    let offered = node_offers(&slice.domain, driver.agent());
    for claude_bg in ["attach", "interrupt:", "close:"] {
        assert!(!offered.iter().any(|offer| offer.starts_with(claude_bg)), "an SDK node offers no {claude_bg}: {offered:?}");
    }
    driver.stop().unwrap();
}

/// What the TUI's detail of the task `agent` works on offers, given the live node the daemon gathers for it.
fn node_offers(domain: &DomainRuntime, agent: &AgentId) -> Vec<String> {
    use alondra_core::views::{Client, Item, RenderContext, Transport, build, live::{LiveNode, LiveSnapshot}};
    let read = domain.read();
    let live = LiveSnapshot { nodes: vec![LiveNode { agent_id: agent.0.clone(), short_id: Some(agent.0.chars().take(8).collect()), ..Default::default() }], ..Default::default() };
    let view = build(&format!("task:{TASK}"), RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: Client::Tui, lang: alondra_core::views::text::Lang::En,
        transport: Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap();
    let mut offers = vec![];
    for section in &view.sections {
        if section.id == "attach" {
            offers.push("attach".to_owned());
        }
        for item in &section.items {
            if let Item::Actions(bar) = item {
                offers.extend(bar.actions.iter().map(|action| action.id.clone()));
            }
        }
    }
    offers
}

// A2: the user answers the card from a client, through the normal command path. The answer
// commits with its dispatch prepared, the owner claims and sends it, and the host's Claude gets
// what it is told; its next turn ends with a card of its own.
#[test]
fn carrying_on_from_a_client_commits_and_reaches_the_sdk() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let (card, _) = Slice::read_cards(&slice.domain).pop().unwrap();
    let state = slice.domain.state().unwrap();
    let carry_on = Command::AnswerQuestion {
        interaction_id: card.as_str().into(),
        option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Add the tests first".into()))]),
        attachments: vec![],
    };
    let outcome = slice.domain.execute_with(&user(), &envelope(&state, "client-carry-on", carry_on), None, None, || Ok(()));
    assert!(outcome.as_ref().is_ok_and(|outcome| outcome.changed), "the client's answer is confirmed: {outcome:?}");
    assert_eq!(driver.pending_permits(), 1, "its dispatch reached the owner");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].starts_with("Go on with your task") && prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    let mut cards = Slice::read_cards(&slice.domain);
    cards.sort();
    assert_eq!(cards.len(), 2, "{cards:?}");
    assert_eq!(cards.iter().filter(|(_, answered)| *answered).count(), 1, "the first card is answered, the second waits: {cards:?}");
    let (second, _) = cards.iter().find(|(_, answered)| !answered).unwrap().clone();
    let state = slice.domain.state().unwrap();
    let written = Command::AnswerQuestion {
        interaction_id: second.as_str().into(),
        option_id: String::new(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Write the changelog too".into()))]),
        attachments: vec![],
    };
    let outcome = slice.domain.execute_with(&user(), &envelope(&state, "client-writes", written), None, None, || Ok(()));
    assert!(outcome.as_ref().is_ok_and(|outcome| outcome.changed), "what the user writes on the card is confirmed: {outcome:?}");
    assert_eq!(driver.pending_permits(), 1);
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts().last().map(String::as_str), Some("Write the changelog too"), "it reaches the host's Claude as written");
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Also update the changelog"})).unwrap();
    let told = tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "David", driver.agent(), message, true))
        .unwrap();
    assert_eq!((told["delivery_state"].as_str(), told["reaches"].as_str()), (Some("dispatching"), Some("now")), "the resting node starts on it: {told}");
    assert_eq!(driver.pending_permits(), 1, "what the user told the resting node left for its host");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let last = slice.prompts().last().cloned().unwrap();
    assert!(last.starts_with("[From David · ") && last.ends_with("Also update the changelog"), "it reaches the host's Claude as a turn of its own, saying who sent it: {last}");
    driver.stop().unwrap();
}

// A tool result of 5 MiB, twice in its SDK message, is kept shrunk with the evidence of what it
// was: its turn ends with its card and the user's answer reaches the same Claude.
#[test]
fn a_huge_tool_result_is_kept_shrunk_and_the_node_goes_on() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("huge-tool-result");
    slice.settle(&mut driver);
    assert_eq!(slice.observations_with("\"code\":\"truncated_sdk_message\""), 1, "the tool result is kept, shrunk");
    assert_eq!(Slice::read_cards(&slice.domain).len(), 1, "its turn ended with its card");
    carry_on(&slice.domain, "Summarize the log");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert!(prompts.len() == 2 && prompts[1].ends_with("Summarize the log"), "the next message reaches the same Claude: {prompts:?}");
    assert_eq!(Slice::read_cards(&slice.domain).len(), 2);
    driver.stop().unwrap();
}

// A message goes into the running turn when the provider can, and starts a turn when the
// turn just closed. Here what someone tells the node while its turn runs reaches the same native
// turn: the host takes it into its streaming input and Claude consumes it in that turn, which then
// ends with one card.
#[test]
fn a_message_mid_turn_reaches_claude_inside_the_turn() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let turns = |domain: &DomainRuntime| {
        let read = domain.read();
        read.state().lifecycle_journal[&TaskId::from(TASK)].iter()
            .filter(|record| matches!(record, alondra_core::domain::LifecycleRecord::TurnStarted { .. })).count()
    };
    assert_eq!(turns(&slice.domain), 1);
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Rebase on main before you push"})).unwrap();
    tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "David", driver.agent(), message, true))
        .unwrap();
    assert_eq!(driver.pending_permits(), 1, "it leaves while the turn runs, without waiting for the turn to end");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert!(prompts.last().is_some_and(|last| last.ends_with("Rebase on main before you push")), "{prompts:?}");
    assert_eq!(turns(&slice.domain), 1, "Claude took it inside the turn that ran");
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty(), "nothing is left waiting");
    assert_eq!(Slice::read_cards(&slice.domain).len(), 1, "the turn ends with one card");
    driver.stop().unwrap();
}

// The node asks the user during its turn and the turn ends: the card that asks how it goes on
// waits out of sight behind the node's question, the user's answer reaches Claude as its next
// input, and that card goes with it.
#[test]
fn a_turn_that_ends_with_the_nodes_question_open_shows_only_that_question_and_the_answer_reaches_claude() {
    use alondra_core::domain::{CardKind, CardSpec, QuestionOption, Recipient, Scope};
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let options = ["Short", "Long"].map(|label| QuestionOption { id: label.to_lowercase(), label: label.into(), description: None }).to_vec();
    slice.run(&Actor::Agent { agent_id: driver.agent().clone() }, "ask", Command::AskQuestion {
        attachments: vec![], interaction_id: "q-name".into(), scope: Scope::Task { task_id: TASK.into() }, recipient: Recipient::User,
        body: "Pick the name".into(), blocking: true, options, card: CardSpec::derived(CardKind::Decide, "Pick the name"),
    });
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Keep it short"})).unwrap();
    tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "David", driver.agent(), message, true))
        .unwrap();
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let inbox = |domain: &DomainRuntime| domain.read().index().waiting().iter().map(|item| item.id.clone()).collect::<Vec<_>>();
    assert_eq!(inbox(&slice.domain), ["interaction:q-name"], "only the node's question waits for the user");
    let behind = open_card(&slice.domain, TURN_ENDED_PREFIX).expect("the lifecycle still requires its card").id;

    answer(&slice.domain, &"q-name".into(), "short");
    assert_eq!(crate::domain_delivery::tell_sdk_nodes(&slice.domain), 1, "the answer goes to the node");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert!(prompts.last().is_some_and(|last| last.contains("Short.")), "{prompts:?}");
    assert!(slice.domain.read().state().interactions[&behind].withdrawn, "the card behind the question went with the answer");
    driver.stop().unwrap();
}

// A3: the writer stops between the commit of the turn end and its publication. A runtime opened
// on the same database gets the host's unacknowledged event again: the exact replay publishes
// the committed card once and acknowledges it, with no second card.
#[test]
fn a_crash_between_commit_and_publication_reconciles_without_a_second_card() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    let database = slice.database.clone();
    slice.domain.before_owner_publish(move || {
        let cards: i64 = Connection::open(&database)
            .unwrap()
            .query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1", [format!("{TURN_ENDED_PREFIX}{TASK}-%")], |row| row.get(0))
            .unwrap();
        if cards > 0 {
            panic!("the daemon stopped after the commit");
        }
    });
    let crash = loop {
        if let Err(crash) = catch_unwind(AssertUnwindSafe(|| driver.observe().unwrap())) {
            break crash;
        }
    };
    assert_eq!(crash.downcast_ref::<&str>(), Some(&"the daemon stopped after the commit"));
    assert_eq!(slice.stored_cards(), 1, "the turn end committed its card");
    assert!(Slice::read_cards(&slice.domain).is_empty(), "nothing of it was published");
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let acknowledged = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&acknowledged);
    restarted.on_acknowledge(move || *counter.lock().unwrap() += 1);
    driver.rebind(Arc::clone(&restarted));
    assert_eq!(driver.observe().unwrap(), alondra_store::ObservationAcceptance::Duplicate,
        "the host's unacknowledged turn end comes again, and the store knows it already");
    assert_eq!(*acknowledged.lock().unwrap(), 1, "the replay acknowledges the event once");
    assert_eq!(Slice::read_cards(&restarted).len(), 1, "readers see the one card");
    assert_eq!(slice.stored_cards(), 1, "the replay adds no second card");
    assert!(!driver.waits_for_host(), "the turn is over");
    driver.stop().unwrap();
}

// A5: deterministic counts of the slice's work, never its time: per step, how many lifecycle
// transactions it ran, how many aggregates the store rebuilt from their journals, how many journal
// events it decoded, how many writing statements the daemon issued and how many read indexes
// publication built.
#[derive(Debug, PartialEq, Eq)]
struct Work {
    transactions: u64,
    aggregate_loads: u64,
    event_decodes: u64,
    writes: u64,
    index_builds: u64,
}

fn measured<T>(domain: &DomainRuntime, step: impl FnOnce() -> T) -> (T, Work) {
    use std::sync::atomic::Ordering;
    let (store, (writes, _), builds) = (alondra_store::measure::counts(), crate::domain_store::statement_counts(), domain.index_builds.load(Ordering::SeqCst));
    let value = step();
    let done = alondra_store::measure::counts() - store;
    let (written, _) = crate::domain_store::statement_counts();
    (value, Work {
        transactions: done.transactions,
        aggregate_loads: done.aggregate_loads,
        event_decodes: done.event_decodes,
        writes: written - writes,
        index_builds: domain.index_builds.load(Ordering::SeqCst) - builds,
    })
}

#[test]
fn the_slice_counts_its_aggregate_loads_decodes_and_index_builds() {
    let slice = Slice::new();
    let (mut driver, launch) = measured(&slice.domain, || slice.launch());
    eprintln!("launch: {launch:?}");
    let mut observations = vec![];
    while driver.waits_for_host() {
        let (_, work) = measured(&slice.domain, || driver.observe().unwrap());
        eprintln!("observation: {work:?}");
        observations.push(work);
    }
    let turn_end = observations.last().unwrap();
    assert_eq!(turn_end.index_builds, 1, "the turn end publishes its card once");
    let (traces, facts): (Vec<&Work>, Vec<&Work>) = observations.iter().partition(|work| work.writes == 0);
    assert_eq!(traces.len(), 5, "the native trace of the first prompt, queued and started, its title before init and two system messages no SDK release declares: {observations:?}");
    assert!(traces.iter().all(|work| work.index_builds == 0), "an observation that changes nothing for readers builds no index: {observations:?}");
    assert!(facts.iter().all(|work| work.index_builds == 1), "each observation with facts builds one index: {observations:?}");
    assert!(launch.aggregate_loads <= launch.transactions, "the launch rebuilds its one task at most once per transaction: {launch:?}");
    assert!(observations.iter().all(|work| work.aggregate_loads <= work.transactions),
        "an observation rebuilds its one task at most once per transaction: {observations:?}");
    let journal: i64 = rusqlite::Connection::open(&slice.database).unwrap()
        .query_row("SELECT count(*) FROM lifecycle_events WHERE task_id=?1", [TASK], |row| row.get(0)).unwrap();
    assert!(turn_end.event_decodes <= (turn_end.transactions + turn_end.index_builds) * journal as u64,
        "the turn end decodes the task's {journal} journal events at most once per transaction and once per publication: {turn_end:?}");
    assert!(turn_end.writes <= 16, "the turn end's writes grew past what this slice measured, with each of its two commits writing its command's body beside its audit row: {turn_end:?}");
    let state = slice.domain.state().unwrap();
    let retitle = envelope(&state, "retitle", Command::TitleRequest { request_id: "sdk-slice-request".into(), title: "Notes for the release".into() });
    let (_, first) = measured(&slice.domain, || slice.domain.execute_with(&user(), &retitle, None, None, || Ok(())).unwrap());
    let (_, replay) = measured(&slice.domain, || slice.domain.execute_with(&user(), &retitle, None, None, || Ok(())).unwrap());
    eprintln!("command: {first:?}, exact replay: {replay:?}");
    assert_eq!(first.index_builds, 1);
    assert_eq!((replay.aggregate_loads, replay.event_decodes, replay.writes, replay.index_builds), (0, 0, 0, 0), "an exact replay rebuilds nothing: {replay:?}");
    driver.stop().unwrap();
}

#[test]
fn stopping_the_owners_leaves_no_process_of_their_hosts() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let group = driver.worker.host_group().expect("the driver has its host");
    let processes = alondra_process_group::members(group).unwrap();
    assert!(processes.len() >= 2, "the host and its native Claude share the host's group: {processes:?}");
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    assert_eq!(slice.domain.node_groups(), [group], "the gate measures the node by its host's group");
    assert!(alondra_process_group::footprint(group).unwrap() > 0);
    slice.domain.stop_sdk_owners();
    assert!(slice.domain.node_groups().is_empty(), "a stopped node costs nothing");
    let left: Vec<String> =
        processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "SDK host processes survived the daemon shutdown: {left:?}");
}

#[test]
fn the_log_names_each_claude_session_once_and_every_turn_end() {
    let init = |session: &str| serde_json::json!({"type": "system", "subtype": "init", "session_id": session, "model": "claude-opus-5-5"});
    let mut session = None;
    assert_eq!(claude_said(&init("db594c1c"), &mut session).as_deref(), Some("the SDK host's Claude runs session db594c1c with claude-opus-5-5"));
    assert_eq!(claude_said(&init("db594c1c"), &mut session), None, "the init of each turn of the same session is not logged again");
    let ended = serde_json::json!({"type": "result", "subtype": "success", "is_error": false, "terminal_reason": "completed"});
    assert_eq!(claude_said(&ended, &mut session).as_deref(), Some("a turn of the SDK host's Claude ended (completed)"));
    let failed = serde_json::json!({"type": "result", "subtype": "success", "is_error": true, "terminal_reason": "api_error"});
    assert_eq!(claude_said(&failed, &mut session).as_deref(), Some("a turn of the SDK host's Claude failed (success, api_error)"));
    assert_eq!(claude_said(&serde_json::json!({"type": "assistant"}), &mut session), None, "what Claude says inside a turn is not logged");
    assert_eq!(claude_said(&init("f4bc77bd"), &mut session).as_deref(), Some("the SDK host's Claude runs session f4bc77bd with claude-opus-5-5"));
}

#[test]
fn the_orchestrators_node_and_tasks_in_progress_show_an_sdk_node_as_the_domain_holds_it() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    let phase = slice.domain.state().unwrap().agents[&agent].phase.key();
    let shown = crate::domain_orchestrator_reads::node(&slice.domain, &agent.0).unwrap()["node"].clone();
    assert_eq!((shown["alive"].as_bool(), shown["state"].as_str(), shown["sdk_host"].as_bool()), (Some(true), Some(phase), Some(true)), "{shown}");
    assert_eq!(shown["name"], "Release notes");
    assert_eq!(shown["task"]["id"], TASK);
    assert!(shown["last_message"].as_str().is_some_and(|said| !said.is_empty()), "what its Claude said last: {shown}");
    let in_progress = crate::domain_orchestrator_reads::tasks_in_progress(&slice.domain, serde_json::Value::Null, serde_json::Value::Null).unwrap();
    let listed: Vec<&str> = in_progress["nodes"].as_array().unwrap().iter().filter_map(|node| node["session_id"].as_str()).collect();
    assert_eq!(listed, [agent.0.as_str()], "tasks_in_progress lists the SDK node");
    slice.domain.stop_sdk_owners();
    let shown = crate::domain_orchestrator_reads::node(&slice.domain, &agent.0).unwrap()["node"].clone();
    assert_eq!(shown["sdk_host"].as_bool(), Some(false), "no host runs it once its owner stopped: {shown}");
}

/// The closer reaches an SDK node only through its owner: `claude rm` and the session listing are
/// never asked about one.
struct NoClaudeSessions;
impl crate::node_close::Sessions for NoClaudeSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> crate::node_close::Answer<'a, Result<String, String>> {
        panic!("the closer ran claude {args:?} for an SDK node")
    }
    fn leftovers<'a>(&'a self, session_id: &'a str) -> crate::node_close::Answer<'a, Option<String>> {
        panic!("the closer looked for leftovers of {session_id}, an SDK node")
    }
}

#[tokio::test]
async fn cancelling_an_sdk_task_with_background_work_stops_its_host_and_ends_the_task() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("background");
    while !slice.domain.state().unwrap().lifecycle_journal[&TASK.into()].iter().any(|record| matches!(record, LifecycleRecord::WorkStarted { .. })) {
        driver.observe().unwrap();
    }
    let group = driver.worker.host_group().unwrap();
    let processes = alondra_process_group::members(group).unwrap();
    assert!(processes.len() >= 2, "the host and its native Claude share the host's group: {processes:?}");
    let agent = driver.agent().clone();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    slice.run(&user(), "cancel", Command::CancelTask { task_id: TASK.into() });
    let task = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].clone();
    assert!(task.ending(), "the cancelled task waits for the receipts of its node's end");
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Cancelled, Some(NodeEnd::Closed)), "the cancelled task with background work ended");
    assert!(!slice.domain.owns_sdk_host(&agent));
    let left: Vec<String> =
        processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "SDK host processes survived the cancellation: {left:?}");
}

// 20 October: a task was cancelled while its node was mid-turn; Alondra never managed to close the
// node and asked the user again and again to "Close it now". The node of a cancelled task is
// Alondra's to close: its turn is interrupted, its host stopped, and the user hears nothing.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_an_sdk_task_mid_turn_interrupts_the_turn_stops_its_host_and_asks_nobody() {
    let slice = Slice::new();
    let driver = slice.launch_working();
    let group = driver.worker.host_group().unwrap();
    let processes = alondra_process_group::members(group).unwrap();
    let agent = driver.agent().clone();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    slice.run(&user(), "cancel", Command::CancelTask { task_id: TASK.into() });
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "Claude's turn was interrupted before its host closed");
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Cancelled, Some(NodeEnd::Closed)));
    assert!(!slice.domain.owns_sdk_host(&agent));
    let left: Vec<String> =
        processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "SDK host processes survived the cancellation: {left:?}");
    let cards: Vec<&str> = state.interactions.keys().map(|id| id.0.as_str()).filter(|id| id.starts_with("problem-close-")).collect();
    assert!(cards.is_empty(), "the user is never asked to close it: {cards:?}");
}

// The node delivers, its host dies with nothing left in its worktrees, and the user accepts the
// result: the task closes without the session, which is gone, with its node's end as the fact.
#[tokio::test]
async fn accepting_a_task_whose_host_died_closes_it() {
    let slice = Slice::new();
    let driver = slice.launch_working();
    let agent = driver.agent().clone();
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    kill_host_and_daemon(driver);
    slice.domain.enable_sdk(slice.backend.clone());
    slice.run(&user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets });
    assert!(!slice.domain.owns_sdk_host(&agent), "no host of this daemon is left for the node");
    let timing = crate::task_closer::Timing::new(Some(std::time::Duration::ZERO));
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, alondra_core::now_millis()).await.unwrap();
    assert_eq!(lifecycle(&slice.domain).activity(), ActivityView::Resting, "the end of the host its journal kept is committed: nothing runs");
    assert!(slice.domain.owns_sdk_host(&agent), "a host that resumes the session took the execution");
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, alondra_core::now_millis()).await.unwrap();
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Done, Some(NodeEnd::Closed)), "the accepted task closes although its session had ended");
    assert!(!slice.domain.owns_sdk_host(&agent), "and the host that took over is stopped");
}

// 21 October: the host of a closing task died and no host could take its session over. Each pass
// told the node why, that message woke the closer again and kept the node's lifecycle at work: 1563
// messages in two hours, and a daemon start that replayed them took seconds. A failed take-over waits
// for the next attempt and tells nothing to a node nothing runs.
#[tokio::test]
async fn a_session_no_host_can_take_over_is_tried_once_per_retry_and_its_node_is_told_nothing() {
    let slice = Slice::new();
    let driver = slice.launch_working();
    let agent = driver.agent().clone();
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    kill_host_and_daemon(driver);
    slice.run(&user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets });
    let journal = || slice.domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].len();
    let failures = || slice.domain.store.lock().unwrap().task_closings().unwrap().iter().map(|row| row.failures).sum::<u32>();
    let told = journal();
    let timing = crate::task_closer::Timing::new(Some(std::time::Duration::ZERO));
    let now = alondra_core::now_millis();
    for pass in 0..3 {
        crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, now + pass).await.unwrap();
    }
    assert_eq!(failures(), 1, "one take-over until the retry is due");
    assert_eq!(journal(), told, "the node nothing runs is told nothing");
    let retry = i64::try_from(timing.retry.as_millis()).unwrap();
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, now + retry).await.unwrap();
    assert_eq!(failures(), 2, "the take-over is tried again once the retry is due");
    assert_eq!(journal(), told);
    assert!(!slice.domain.owns_sdk_host(&agent));
}

// 21 October: the host of a closing task was left with no owner. Its node confirmed its cleanup and
// gave its worktree up, and still the closer took the execution over: a new host resumed the Claude
// session in the worktree being deleted, and every notice queued for it became one more turn. A task
// that owes nothing more ends its host where it is and its session is never resumed.
#[tokio::test(flavor = "multi_thread")]
async fn a_closing_task_that_owes_nothing_ends_its_unowned_host_without_resuming_its_session() {
    let slice = Slice::new();
    let driver = slice.launch_working();
    let agent = driver.agent().clone();
    let node = Actor::Agent { agent_id: agent.clone() };
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&node, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    slice.run(&user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets });
    slice.run(&node, "cleaned", Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });
    for notice in 0..3 {
        slice.run(&Actor::System, &format!("notice-{notice}"), Command::TellNode {
            task_id: TASK.into(), text: format!("[From Alondra · 03:5{notice}] Alondra could not close this session yet"),
        });
    }
    let (_, stranded) = strand_host(&slice, driver);
    std::fs::remove_dir_all(&slice.worktree).unwrap();
    slice.domain.enable_sdk(slice.backend.clone());
    let prompts = slice.prompts();
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    assert_eq!(slice.starts().len(), 1, "no host resumed the session: {:?}", slice.starts());
    assert_eq!(slice.prompts(), prompts, "no notice reached Claude");
    let left: Vec<String> = stranded.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "the stranded host's processes survived the close: {left:?}");
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Done, Some(NodeEnd::Gone)));
    assert!(!slice.domain.owns_sdk_host(&agent));
    assert!(slice.domain.store.lock().unwrap().task_closings().unwrap().is_empty(), "the close finished with no failure");
    let cards: Vec<&str> = state.interactions.keys().map(|id| id.0.as_str()).filter(|id| id.starts_with("problem-close-")).collect();
    assert!(cards.is_empty(), "the user is never asked: {cards:?}");
}

// A worktree of the task holds work that is not saved: its node is told once which, not again on
// every retry while nothing changes, and the node's new confirmation closes it at once instead of
// after the retry's pause.
#[tokio::test(flavor = "multi_thread")]
async fn an_sdk_node_is_told_once_about_its_unsaved_worktree_and_its_confirmation_closes_it_at_once() {
    let slice = Slice::new();
    let driver = slice.launch_working();
    let agent = driver.agent().clone();
    let node = Actor::Agent { agent_id: agent.clone() };
    let root = Directory::new();
    let dirty = crate::worktree::testing::task_worktree(&root, "sdk-slice");
    slice.run(&Actor::System, "worktree", Command::RecordTaskWorktree {
        task_id: TASK.into(), path: dirty.to_str().unwrap().into(), origin: alondra_core::domain::WorktreeOrigin::Subagent, branch: None,
    });
    std::fs::write(dirty.join("draft.txt"), "x").unwrap();
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&node, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    slice.run(&user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets });
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    slice.run(&node, "cleaned", Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });
    let told = || slice.domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].iter()
        .filter(|record| matches!(record, LifecycleRecord::Told { text: Some(text), .. } if text.contains(dirty.to_str().unwrap()))).count();
    let timing = crate::task_closer::Timing::new(None);
    let retry = i64::try_from(timing.retry.as_millis()).unwrap();
    let now = alondra_core::now_millis();
    for attempt in 0..2 {
        crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, now + attempt * retry).await.unwrap();
    }
    let failures = slice.domain.store.lock().unwrap().task_closing(&TASK.into()).unwrap().unwrap().failures;
    assert_eq!((failures, told()), (2, 1), "tried after each pause, told once");
    std::fs::remove_file(dirty.join("draft.txt")).unwrap();
    slice.run(&node, "cleaned-again", Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, now + retry + 1).await.unwrap();
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Done, Some(NodeEnd::Closed)), "the confirmation closes it before the pause is over");
    assert!(!slice.domain.owns_sdk_host(&agent));
}

// The node delivers inside its turn and the user accepts: Alondra's request to clean up, a message
// of its outbox, is what the node's Claude is told next. A node of `claude --bg` takes it through its
// mod; nothing took it for a node of the SDK.
#[tokio::test(flavor = "multi_thread")]
async fn alondras_cleanup_request_reaches_the_sdk_node_as_its_next_input() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let agent = driver.agent().clone();
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    slice.run(&user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets });
    let pending: Vec<_> = slice.domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
        .filter(|message| message.recipient_kind == "agent" && message.agent_id.as_deref() == Some(agent.0.as_str())).collect();
    assert_eq!(pending.len(), 1, "Alondra asks the node to clean up: {pending:?}");
    let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
    let push = crate::push::Push::open(&slice._home).unwrap();
    crate::domain_delivery::deliver_from_runtime(&slice.domain, &slice._home, &push, None, &client, pending[0].clone(), std::time::Duration::ZERO).await.unwrap();
    assert!(slice.domain.store.lock().unwrap().pending_messages().unwrap().iter().all(|message| message.id != pending[0].id), "the message is handed");
    assert_eq!(driver.pending_permits(), 1, "it leaves for the node's host");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let last = slice.prompts().last().cloned().unwrap();
    assert!(last.starts_with("[From Alondra · ") && last.contains("is finished and is closing"), "{last}");
    driver.stop().unwrap();
}

// The node pushes to its PR after it delivered: the GitHub observer takes the delivery back, and
// the input that hand-back gives the task is what the node's Claude is told, once. Before, that
// input had no text, the store refused the observer's commit for the dispatch it lacked, and the
// daemon died on it at every start.
#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_behind_its_pr_goes_back_to_the_sdk_node_with_what_changed() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let node = Actor::Agent { agent_id: driver.agent().clone() };
    slice.run(&node, "register", Command::RegisterPr {
        task_id: TASK.into(), pr_id: "pr-notes".into(), host: "github.example.invalid".into(), repository: "synthetic/notes".into(),
        number: 7, head: "head-a".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    slice.run(&node, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: vec![DeliveryTarget::PullRequest { pr_id: "pr-notes".into(), head: "head-a".into() }],
        is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    slice.run(&Actor::System, "end-turn", Command::TellNode { task_id: TASK.into(), text: "[From Alondra · 09:00] Thanks".into() });
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    slice.run(&Actor::System, "observe", Command::ObservePrState {
        pr_id: "pr-notes".into(), head: "head-b".into(), base: "main".into(), state: PrState::Open, merged_head: None,
    });
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].final_presentation, None, "the delivery behind the PR was taken back");
    assert!(state.interactions[&InteractionId::from("final")].withdrawn);
    assert!(slice.domain.store.lock().unwrap().pending_messages().unwrap().iter().all(|message| message.recipient_kind != "agent"),
        "the node is told through its input, not a second message");
    let restarted = DomainRuntime::open_at(&slice._home, &slice.database).unwrap();
    assert_eq!(restarted.state().unwrap().tasks[&TaskId::from(TASK)].final_presentation, None, "a daemon start reads what was committed");
    assert_eq!(driver.pending_permits(), 1, "the input leaves for the node's host");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let told: Vec<String> = slice.prompts().into_iter().filter(|prompt| prompt.contains("Pull request #7 has commits")).collect();
    assert_eq!(told, vec![delivery_withdrawn_behind_pr(7)]);
    driver.stop().unwrap();
}

// The node was asked to clean up and its turn with that request ended: its host stays until the
// node says it cleaned up. Before, the closer stopped it as soon as the task rested, while the node
// was still answering the request.
#[tokio::test(flavor = "multi_thread")]
async fn an_sdk_node_asked_to_clean_up_is_closed_only_once_it_says_it_did() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let agent = driver.agent().clone();
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    slice.run(&user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets });
    let pending = slice.domain.store.lock().unwrap().pending_messages().unwrap().into_iter().find(|message| message.recipient_kind == "agent").unwrap();
    let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
    let push = crate::push::Push::open(&slice._home).unwrap();
    crate::domain_delivery::deliver_from_runtime(&slice.domain, &slice._home, &push, None, &client, pending, std::time::Duration::ZERO).await.unwrap();
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(lifecycle(&slice.domain).activity(), ActivityView::Resting, "the turn with the request ended");
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    let timing = crate::task_closer::Timing::new(None);
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, alondra_core::now_millis()).await.unwrap();
    assert!(slice.domain.owns_sdk_host(&agent), "the host stays until the node says it cleaned up");
    assert_eq!(slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].state, TaskState::Closing);
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "cleaned", Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, alondra_core::now_millis()).await.unwrap();
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Done, Some(NodeEnd::Closed)));
    assert!(!slice.domain.owns_sdk_host(&agent));
}

#[tokio::test]
async fn cancelling_an_sdk_task_that_waits_on_its_card_stops_its_host_and_ends_the_task() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let group = driver.worker.host_group().unwrap();
    let processes = alondra_process_group::members(group).unwrap();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    let state = slice.domain.state().unwrap();
    let cancelled = slice.domain.execute_with(&user(), &envelope(&state, "client-cancel", Command::CancelTask { task_id: TASK.into() }), None, None, || Ok(()));
    assert!(cancelled.is_ok(), "the user's cancellation commits: {cancelled:?}");
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].state, TaskState::Cancelled);
    let left: Vec<String> =
        processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "SDK host processes survived the cancellation: {left:?}");
}

/// The owner generation currently fencing the slice's execution, read straight from SQL.
fn owner_generation(database: &Path) -> i64 {
    Connection::open(database).unwrap().query_row("SELECT owner_generation FROM provider_execution_owners", [], |row| row.get(0)).unwrap()
}

fn carry_on(domain: &DomainRuntime, note: &str) {
    let (card, _) = Slice::read_cards(domain).into_iter().find(|(_, answered)| !answered).expect("an open turn-end card");
    let state = domain.state().unwrap();
    let answer = Command::AnswerQuestion {
        interaction_id: card.as_str().into(),
        option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(note.into()))]),
        attachments: vec![],
    };
    assert!(domain.execute_with(&user(), &envelope(&state, &format!("client-carry-on-{}", card.as_str()), answer), None, None, || Ok(())).unwrap().changed);
}

// B1: the daemon crashes with the host still running. A fresh daemon on the same database adopts
// that live host with the next owner generation, and «Carry on» from a client reaches the same
// native Claude, which starts only once; its next turn ends with a card of its own.
#[test]
fn a_daemon_restart_adopts_the_live_host_and_the_same_claude_answers_the_next_turn() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts(), ["Write the release notes"], "the launch prompt reached the host once");
    assert_eq!(owner_generation(&slice.database), 1, "the launch owned the execution as generation 1");
    let group = driver.worker.host_group().unwrap();
    let processes = alondra_process_group::members(group).unwrap();
    // The daemon dies: drop its driver without stopping the host, which keeps running.
    drop(driver);
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(owner_generation(&slice.database), 2, "adoption reserved the next owner generation");
    carry_on(&restarted, "Add the tests first");
    adopted.step(&|| false).unwrap();
    slice.settle(&mut adopted);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "the same Claude got the launch and the carry-on, no restart: {prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    assert_eq!(Slice::read_cards(&restarted).len(), 2, "the adopted turn ended with its own card");
    restarted.sdk_owners.lock().unwrap().push(Owner::spawn(adopted));
    assert_eq!(restarted.node_groups(), [group], "the adopted host is measured as the node it runs");
    restarted.stop_sdk_owners();
    let left: Vec<String> =
        processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "the adopted host's processes survived its stop: {left:?}");
}

/// The owner of the task's host goes without stopping it, as the owner of 0.3.0 did when a step
/// failed, and the host's owner socket goes too: the host lives on, holds its lock, and no owner
/// can ever reach it again.
fn strand_host(slice: &Slice, driver: Driver) -> (String, Vec<alondra_process_group::Member>) {
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let processes = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let folder = slice.backend.state_root.join(&driver.agent().0);
    drop(driver);
    std::fs::remove_file(folder.join("s")).unwrap();
    assert!(processes.iter().all(|process| alondra_process_group::alive(process).unwrap()), "the host still runs");
    (session, processes)
}

/// The stranded host is gone with its whole group, and a host that resumes the same Claude
/// session took the execution: what the user says next reaches that session.
fn assert_replaced(slice: &Slice, domain: &Arc<DomainRuntime>, mut driver: Driver, session: &str, stranded: &[alondra_process_group::Member]) {
    let left: Vec<String> = stranded.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "the stranded host's processes survived: {left:?}");
    carry_on(domain, "Add the tests first");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session), "the same session is resumed: {starts:?}");
    assert!(slice.prompts()[1].ends_with("Add the tests first"), "{:?}", slice.prompts());
    driver.stop().unwrap();
}

// Production, 0.3.1: at the daemon's start five SDK hosts still held their lock, so they were
// found live, but their owner's socket was gone and each adoption failed as Disconnected; every
// execution was left as it was, for good. A live host that refuses its adoption is now ended by
// the lock it holds, waited out until it exits, and replaced like a host that ended.
#[test]
fn at_start_a_live_host_that_refuses_its_adoption_is_ended_and_replaced_by_one_that_resumes_the_session() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    let (session, stranded) = strand_host(&slice, driver);
    let restarted = slice.restart();
    let mut taken = take_all(&restarted, &slice.backend);
    let (task, driver) = taken.pop().expect("the daemon's start takes the execution");
    assert_eq!(task, TaskId::from(TASK));
    let driver = driver.unwrap_or_else(|Failed(why)| panic!("the execution was left as it was: {why}"));
    assert_eq!(driver.agent(), &agent, "the same node goes on");
    assert_replaced(&slice, &restarted, driver, &session, &stranded);
}

// The same stranded host, met by a daemon that is running: taking the execution over, as the
// move to the SDK or the closer of a task does, ends it and replaces it the same way.
#[test]
fn a_running_daemon_ends_a_live_host_that_refuses_its_adoption_and_resumes_the_session() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let (session, stranded) = strand_host(&slice, driver);
    let replaced = slice.take_over(&slice.domain);
    assert_replaced(&slice, &slice.domain, replaced, &session, &stranded);
}

/// The background commands the fake Claude started, by process id.
fn background_commands(slice: &Slice) -> Vec<libc::pid_t> {
    std::fs::read_to_string(format!("{}.background", slice.capture.display())).unwrap_or_default().lines().map(|line| line.parse().unwrap()).collect()
}

fn running(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Reads the host until the task's lifecycle waits only on the background work Claude left running.
fn until_own_work(slice: &Slice, driver: &mut Driver) {
    while lifecycle(&slice.domain).activity() != ActivityView::OwnWork {
        driver.observe().unwrap();
    }
    assert!(background_commands(slice).into_iter().all(running), "the background command runs");
}

/// The host that replaced the stranded one ended the background command with it, the resumed
/// session is told that command ended and will not report back, and it goes on: its turn ends.
fn assert_told_the_work_ended(slice: &Slice, domain: &Arc<DomainRuntime>, mut driver: Driver, session: &str) {
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
    assert_eq!(lifecycle(domain).activity(), ActivityView::Resting, "its turn ended: nothing waits for a notification");
    driver.stop().unwrap();
}

// t-parcel-open-issues-widget, 22 October: the session ended its turn on «I go on when it ends»
// with `bazel test` running in the background, and the 0.3.5 daemon ended its 0.3.4 host, which
// it would not adopt, and the command with it. A host's end is a fact: the resumed session is told
// the command ended and will not report back, instead of waiting for a notification no process
// will send.
#[test]
fn background_work_its_replaced_host_ended_is_reported_to_the_resumed_session_which_goes_on() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("background-shell");
    until_own_work(&slice, &mut driver);
    let (session, _) = strand_host(&slice, driver);
    let restarted = slice.restart();
    let (_, driver) = take_all(&restarted, &slice.backend).pop().expect("the daemon's start takes the execution");
    let driver = driver.unwrap_or_else(|Failed(why)| panic!("the execution was left as it was: {why}"));
    assert_told_the_work_ended(&slice, &restarted, driver, &session);
    assert_eq!(Slice::read_cards(&restarted).iter().filter(|(_, answered)| !answered).count(), 1, "the task goes on: its turn ended on a card");
}

// The same, with the task delivered: the user replied to the result and the node's turn on the
// reply left the command running. A delivery under review asked nothing of a resumed session, so
// nothing ever woke it: the task read «Answering you» for 12 hours.
#[test]
fn background_work_for_the_users_reply_that_its_replaced_host_ended_is_reported_to_the_resumed_session() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let node = Actor::Agent { agent_id: driver.agent().clone() };
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&node, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Wrap up"})).unwrap();
    let told = tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "David", driver.agent(), message, true))
        .unwrap();
    assert_eq!(told["reaches"], "turn", "Claude reads it inside the turn that runs: {told}");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!((lifecycle(&slice.domain).phase(), lifecycle(&slice.domain).activity()), (alondra_domain::TaskPhase::Delivered, ActivityView::Resting));
    slice.run(&user(), "reply", Command::SubmitFeedback { attachments: vec![], presentation_id: "final".into(), targets,
        body: "background-shell".into(), instruction: alondra_core::domain::Instruction::Reply });
    assert_eq!(crate::domain_delivery::tell_sdk_nodes(&slice.domain), 1, "the reply is told to the node");
    driver.step(&|| false).unwrap();
    until_own_work(&slice, &mut driver);
    let (session, _) = strand_host(&slice, driver);
    let restarted = slice.restart();
    let (_, driver) = take_all(&restarted, &slice.backend).pop().expect("the daemon's start takes the execution");
    let driver = driver.unwrap_or_else(|Failed(why)| panic!("the execution was left as it was: {why}"));
    assert_eq!(lifecycle(&restarted).phase(), alondra_domain::TaskPhase::Delivered, "the delivery stays the one the user reviews");
    assert_told_the_work_ended(&slice, &restarted, driver, &session);
}

// B2: the writer crashes between the turn-end commit and its publication. A fresh daemon adopts the
// live host, which re-sends the unacknowledged event; the adopting owner sees the store already has
// it (Duplicate), acknowledges it once and publishes exactly one card.
#[test]
fn an_unacknowledged_turn_end_comes_back_to_the_adopting_owner_once() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    let database = slice.database.clone();
    slice.domain.before_owner_publish(move || {
        let cards: i64 = Connection::open(&database).unwrap()
            .query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1", [format!("{TURN_ENDED_PREFIX}{TASK}-%")], |row| row.get(0)).unwrap();
        if cards > 0 {
            panic!("the daemon stopped after the commit");
        }
    });
    let crash = loop {
        if let Err(crash) = catch_unwind(AssertUnwindSafe(|| driver.observe().unwrap())) {
            break crash;
        }
    };
    assert_eq!(crash.downcast_ref::<&str>(), Some(&"the daemon stopped after the commit"));
    assert_eq!(slice.stored_cards(), 1, "the turn end committed its card");
    drop(driver);
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let acknowledged = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&acknowledged);
    restarted.on_acknowledge(move || *counter.lock().unwrap() += 1);
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(adopted.observe().unwrap(), alondra_store::ObservationAcceptance::Duplicate, "the host re-sends the unacknowledged turn end and the store knows it");
    assert_eq!(*acknowledged.lock().unwrap(), 1, "the replay acknowledges the event once");
    assert_eq!(Slice::read_cards(&restarted).len(), 1, "readers see the one card");
    assert_eq!(slice.stored_cards(), 1, "the replay adds no second card");
    adopted.stop().unwrap();
}

// B3: a client's answer is claimed and recorded, then the daemon dies before the host receives it.
// A fresh daemon adopts and reconciles the unsent dispatch by re-sending its exact bytes: the host
// offers it to the same Claude exactly once.
#[test]
fn a_send_the_host_never_got_is_sent_once_by_the_adopting_owner() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first");
    assert!(driver.claim_and_record_without_sending(), "the owner claimed and recorded the dispatch");
    drop(driver);
    assert_eq!(slice.prompts().len(), 1, "the host's Claude never got the answer");
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    slice.settle(&mut adopted);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "reconciliation sent the answer to the same Claude exactly once: {prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    adopted.stop().unwrap();
}

// B5: the store issued the permit of a client's answer and the daemon died before claiming it:
// the permit lived only in that daemon. The adopting owner gets it again and the answer reaches
// the same Claude once.
#[test]
fn an_answer_whose_permit_the_dead_daemon_never_claimed_is_sent_by_the_adopting_owner() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first");
    assert_eq!(driver.pending_permits(), 1, "the store issued the answer's permit to this daemon");
    drop(driver);
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(adopted.pending_permits(), 1, "the adopting owner holds the permit again");
    adopted.step(&|| false).unwrap();
    slice.settle(&mut adopted);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "the answer reached the same Claude once: {prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    adopted.stop().unwrap();
}

// B4: the host offered a client's answer but the daemon crashed before recording its receipt. A
// fresh daemon adopts and reconciles it from the retained receipt: it is handed, and the host is
// not asked to offer it a second time.
#[test]
fn a_send_the_host_offered_is_not_offered_again_when_its_receipt_was_lost() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first");
    assert!(driver.claim_record_and_offer_without_receipt(), "the host offered the answer");
    drop(driver);
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    slice.settle(&mut adopted);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "the answer reached the host once; reconciliation did not offer it again: {prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    assert_eq!(Slice::read_cards(&restarted).len(), 2, "the reconciled turn ended with its own card");
    adopted.stop().unwrap();
}

// 21 Oct: an SDK node stopped within hours. Its host's journal had a 16 MiB lifetime limit and
// took 256 commands at most, and each daemon connection tracked 256 commands for good: a long node
// crossed them and its host stopped serving. Now a turn writes over 20 MiB and the node then takes
// more than 256 inputs, through the same host.
#[test]
fn a_long_node_keeps_its_host_past_tens_of_mib_and_hundreds_of_inputs() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("bulky:24");
    slice.settle(&mut driver);
    assert_eq!(slice.observations_with(&"x".repeat(900_000)), 24, "every large answer was committed");
    for turn in 1..=260 {
        carry_on(&slice.domain, &format!("Next step {turn}"));
        driver.step(&|| false).unwrap();
        slice.settle(&mut driver);
    }
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 261, "every input reached the same Claude");
    assert!(prompts[260].ends_with("Next step 260"), "{}", prompts[260]);
    assert_eq!(Slice::read_cards(&slice.domain).len(), 261, "every turn ended with its own card");
    driver.stop().unwrap();
}

// The Mac goes down on a host whose journal was compacted: the launch turn wrote more than the
// journal keeps once acknowledged, then the host offered a client's answer and everything died
// before the daemon recorded the receipt. The next daemon reads the compacted journal from disk:
// the answer was handed, so it goes to the resumed session flagged as maybe received already, never
// as an answer the host never got.
#[test]
fn a_compacted_journal_proves_to_the_next_daemon_that_an_answer_was_handed() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("bulky:6");
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first");
    assert!(driver.claim_record_and_offer_without_receipt(), "the host offered the answer");
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    let journal = restarted.read().state().lifecycle_journal[&TaskId::from(TASK)].clone();
    assert!(journal.iter().any(|record| matches!(record, LifecycleRecord::DoubtResent { .. })),
        "the ended host's journal proves the answer was handed: it goes again flagged as maybe received, never as one the host never got: {journal:?}");
    assert_eq!(slice.prompts().len(), 2);
    resumed.stop().unwrap();
}

// 19 Oct: Claude's sign-in expired and nothing told the user. Through the SDK, the native
// Claude's 401 ends the turn with `authentication_failed`: the task waits on its card asking the
// user to sign in, committed and published before the host is acknowledged. Once the user signs
// in and says so, the task goes on and Claude's answer leaves no card open.
#[test]
fn a_signed_out_turn_waits_on_the_sign_in_card_until_claude_answers_again() {
    let slice = Slice::new();
    let acknowledged = watch_acknowledgements(&slice);
    let mut driver = slice.launch_with("Write the release notes signed-out");
    slice.settle(&mut driver);
    assert_eq!(Slice::read_cards(&slice.domain), [], "no card that tells the task to go on");
    let held: Vec<(String, ClaudeHold)> = slice.domain.read().state().claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect();
    assert_eq!(held, [(TASK.to_owned(), ClaudeHold::SignedOut)]);
    let listed = |domain: &DomainRuntime| -> Vec<InteractionId> {
        domain.read().index().waiting().iter().map(|item| { let InboxSubject::Interaction { interaction_id } = &item.subject; interaction_id.clone() })
            .filter(|id| id.0.starts_with(SIGNED_OUT_PREFIX)).collect()
    };
    let card = listed(&slice.domain).pop().expect("Waiting for You lists the sign-in card");
    let stored: i64 = slice.observer().query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1 AND task_id=?2", [format!("{SIGNED_OUT_PREFIX}%"), TASK.to_owned()], |row| row.get(0)).unwrap();
    assert_eq!(stored, 1, "its card, committed");
    assert!(!acknowledged.lock().unwrap().is_empty(), "the host's observations were acknowledged after their commit");

    std::fs::write(format!("{}.recovered", slice.capture.display()), "").unwrap();
    let state = slice.domain.state().unwrap();
    let signed_in = Command::AnswerQuestion { interaction_id: card, option_id: SIGNED_IN.into(), inputs: BTreeMap::new(), attachments: vec![] };
    slice.domain.execute_with(&user(), &envelope(&state, "client-signed-in", signed_in), None, None, || Ok(())).unwrap();
    assert_eq!(driver.pending_permits(), 1, "the probe's dispatch reached the owner");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].starts_with("Claude answers again"), "{}", prompts[1]);
    let read = slice.domain.read();
    assert!(read.state().claude_held().is_empty(), "Claude answered: nothing waits for it");
    assert_eq!(listed(&slice.domain), [], "no sign-in card is listed");
    driver.stop().unwrap();
}

// The API kept failing past Claude Code's retries: the task waits with no announcement, and the
// daemon's scheduled probe, run as the daemon runs it, sends exactly that task on once Claude
// answers again.
#[test]
fn an_overloaded_api_holds_the_task_until_the_scheduled_probe_finds_claude_answering() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("Write the release notes overloaded");
    slice.settle(&mut driver);
    let held: Vec<(String, ClaudeHold)> = slice.domain.read().state().claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect();
    assert_eq!(held, [(TASK.to_owned(), ClaudeHold::Outage)]);
    assert_eq!(Slice::read_cards(&slice.domain), [], "no card that tells the task to go on");
    std::fs::write(format!("{}.recovered", slice.capture.display()), "").unwrap();
    crate::claude_access::probe(&slice.domain, ClaudeHold::Outage, None);
    assert_eq!(driver.pending_permits(), 1, "the probe's dispatch reached the owner");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].starts_with("Claude answers again"), "{}", prompts[1]);
    assert!(slice.domain.read().state().claude_held().is_empty(), "Claude answered: nothing waits for it");
    driver.stop().unwrap();
}

// Each launch made a provider binding of its own while the store keeps one active binding per
// host: every task after the first was refused with BindingRevoked.
#[test]
fn a_second_task_starts_through_the_same_sdk_host() {
    let slice = Slice::new();
    slice.run(&Actor::Orchestrator { generation: 0 }, "second-task", Command::CreateTask {
        plan_items: None,
        task_id: "sdk-slice-second".into(),
        request_id: "sdk-slice-request".into(),
        title: "Write the upgrade guide".into(),
        agent_id: None,
        review_prs: false,
        dependencies: BTreeSet::new(),
        stack: None,
        launch: Some(LaunchPlan::new("node", slice.worktree.to_str().unwrap())),
    });
    let mut first = slice.launch();
    slice.settle(&mut first);
    let mut second = start(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: "sdk-slice-second".into(),
        name: "Upgrade guide".into(),
        directory: slice.worktree.to_str().unwrap().into(),
        prompt: "Write the upgrade guide".into(),
        plugin: slice.plugin.clone(),
        options: LaunchOptions::default(),
        model: None,
        account: AccountId::default_account(),
    })
    .expect("the second task starts too");
    slice.settle(&mut second);
    assert_eq!(slice.prompts(), ["Write the release notes", "Write the upgrade guide"]);
    first.stop().unwrap();
    second.stop().unwrap();
}

impl Slice {
    /// A second task of the request, ready to launch in the slice's worktree.
    fn task(&self, task: &str) {
        self.run(&Actor::Orchestrator { generation: 0 }, &format!("task-{task}"), Command::CreateTask {
            plan_items: None,
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

    fn launch_on(&self, task: &str, account: &str) -> Result<Driver, Failed> {
        start(Arc::clone(&self.domain), &self.backend, Launch {
            task: task.into(), name: format!("Work on {task}"), directory: self.worktree.to_str().unwrap().into(),
            prompt: format!("Work on {task}"), plugin: self.plugin.clone(), options: LaunchOptions::default(), model: None,
            account: AccountId::parse(account).unwrap(),
        })
    }

    /// The account and generation of the credential the task's execution is bound to.
    fn credential(&self, task: &str) -> (String, u64) {
        let observer = Connection::open(&self.database).unwrap();
        observer.query_row(
            "SELECT b.credential, b.credential_generation FROM lifecycle_execution_bindings e JOIN provider_bindings b ON b.id=e.binding_id WHERE e.task_id=?1 ORDER BY e.generation DESC LIMIT 1",
            [task], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)),
        ).unwrap()
    }

    fn accounts_seen(&self, id: &str, work: AccountState) {
        let accounts = [("default", AccountState::SignedIn), ("work", work)].map(|(id, state)| AccountStatus {
            provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: id.into(), state, email: None, plan: None, billing: Default::default(), sign_ins: 0,
        }).to_vec();
        self.run(&Actor::System, id, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
    }
}

fn sha256(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

// An execution on an account the user added runs Claude with that account's folder as its
// configuration, is bound to the account as its credential, and the task keeps its account. The
// default account's execution has no `CLAUDE_CONFIG_DIR`; an account the user never registered
// gets no Claude at all. Signing an account in again binds its next executions to a new generation.
#[test]
fn an_execution_runs_on_its_accounts_folder_and_is_bound_to_that_account() {
    let slice = Slice::new();
    std::fs::write(alondra_core::options::path_in(&slice._home), "[[accounts.claude]]\nid = \"work\"\nname = \"Work\"\n").unwrap();
    let folder = slice._home.join("accounts/work");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&folder).unwrap();
    slice.accounts_seen("seen-1", AccountState::SignedIn);
    slice.task("sdk-on-work");
    let mut on_work = slice.launch_on("sdk-on-work", "work").expect("a registered account launches");
    slice.settle(&mut on_work);
    let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&slice.capture).unwrap()).unwrap();
    assert_eq!(capture["env_value_sha256"]["CLAUDE_CONFIG_DIR"].as_str(), Some(sha256(folder.to_str().unwrap()).as_str()), "its own folder");
    assert_eq!(slice.credential("sdk-on-work"), ("work".into(), 1));
    assert_eq!(slice.domain.read().state().tasks[&TaskId::from("sdk-on-work")].account.as_ref().map(AccountId::as_str), Some("work"));
    on_work.stop().unwrap();

    let mut on_default = slice.launch();
    slice.settle(&mut on_default);
    let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&slice.capture).unwrap()).unwrap();
    let keys: Vec<&str> = capture["env_keys"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    assert!(!keys.contains(&"CLAUDE_CONFIG_DIR"), "the user's default profile: {keys:?}");
    assert_eq!(slice.credential(TASK), ("default".into(), 1));
    assert_eq!(slice.domain.read().state().tasks[&TaskId::from(TASK)].account, None, "the provider's own sign-in");
    on_default.stop().unwrap();

    slice.task("sdk-on-other");
    std::fs::DirBuilder::new().mode(0o700).create(slice._home.join("accounts/other")).unwrap();
    let Err(Failed(refused)) = slice.launch_on("sdk-on-other", "other") else { panic!("an account the user never added runs nothing") };
    assert!(refused.contains("not one of the accounts"), "{refused}");

    slice.accounts_seen("seen-2", AccountState::SignedOut);
    slice.accounts_seen("seen-3", AccountState::SignedIn);
    slice.task("sdk-on-work-again");
    let mut again = slice.launch_on("sdk-on-work-again", "work").unwrap();
    slice.settle(&mut again);
    assert_eq!(slice.credential("sdk-on-work-again"), ("work".into(), 2), "signed in again: the credential's next generation");
    assert_eq!(slice.credential("sdk-on-work"), ("work".into(), 1), "the earlier execution keeps the binding it was admitted under");
    again.stop().unwrap();
}

// `claude --bg` gets the model, effort and denied tools of its launch as flags. Through the SDK the
// host gives them to its Claude, and a host that resumes the session after the first ended gives
// them again: they are kept beside the execution.
#[test]
fn the_launchs_model_effort_and_denied_tools_reach_claude_and_survive_a_resumed_session() {
    let slice = Slice::new();
    let options = LaunchOptions { model: Some("fixture-model".into()), effort: Some("high".into()), disallowed_tools: vec!["WebFetch".into()] };
    let mut driver = start_bound(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: TASK.into(), name: "Release notes".into(), directory: slice.worktree.to_str().unwrap().into(),
        prompt: "Write the release notes".into(), plugin: slice.plugin.clone(), options,
        model: None, account: AccountId::default_account(),
    }, capabilities(), None).unwrap();
    slice.settle(&mut driver);
    let launched = |slice: &Slice| -> Vec<String> {
        let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&slice.capture).unwrap()).unwrap();
        capture["args"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect()
    };
    let has = |args: &[String], flag: &str, value: &str| {
        args.windows(2).any(|pair| pair[0] == flag && pair[1].contains(value)) || args.iter().any(|arg| arg.strip_prefix(&format!("{flag}=")).is_some_and(|v| v.contains(value)))
    };
    let args = launched(&slice);
    assert!(has(&args, "--model", "fixture-model"), "{args:?}");
    assert!(has(&args, "--effort", "high"), "{args:?}");
    assert!(has(&args, "--disallowedTools", "WebFetch"), "{args:?}");
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    carry_on(&restarted, "Add the tests first");
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let args = launched(&slice);
    assert!(args.iter().any(|arg| arg.starts_with("--resume")), "the resumed session: {args:?}");
    assert!(has(&args, "--model", "fixture-model") && has(&args, "--effort", "high") && has(&args, "--disallowedTools", "WebFetch"), "{args:?}");
    assert!(has(&args, "--settings", &format!("Edit(/{}/**)", slice.worktree.display())), "the resumed session still edits its own worktree without asking: {args:?}");
    resumed.stop().unwrap();
}

// A node's Claude edits its own worktree without a permission card, by the path it was given and
// by its real path behind a symlink; a path a rule cannot spell literally gets no rule.
#[test]
fn a_node_may_edit_only_inside_its_own_worktree_without_asking() {
    let scratch = Directory::new();
    let real = scratch.join("real-worktree");
    std::fs::create_dir_all(&real).unwrap();
    let real = real.canonicalize().unwrap();
    assert_eq!(super::own_worktree_edits(&real), vec![format!("Edit(/{}/**)", real.display())]);
    let link = scratch.join("linked-worktree");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(super::own_worktree_edits(&link), vec![format!("Edit(/{}/**)", link.display()), format!("Edit(/{}/**)", real.display())]);
    assert!(super::own_worktree_edits(std::path::Path::new("/tmp/odd[name]")).is_empty());
    assert!(super::own_worktree_edits(std::path::Path::new("relative/worktree")).is_empty());
}

// Replacing a session interrupts the old turn, detaches the old session and starts the
// new one, and interrupts the old runtime only for the exact committed replacement. Here
// «Replace» stops the node's host through its owner and starts the task on a new execution: its
// first generation's open request is withdrawn with it, and an answer to that card is refused.
#[test]
fn replacing_an_sdk_node_starts_a_new_execution_and_its_old_cards_are_refused() {
    let slice = Slice::new();
    let (driver, card) = slice.asked("decide:bash make test");
    let old = driver.agent().clone();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    slice.domain.stop_sdk_host(&old).unwrap();
    end_unowned_host(Arc::clone(&slice.domain), &slice.backend, TASK.into()).unwrap();
    assert!(open_card(&slice.domain, "decision-").is_none(), "the stopped host's request is closed with its card");
    let mut replacement = start(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: TASK.into(), name: "Release notes".into(), directory: slice.worktree.to_str().unwrap().into(),
        prompt: "Write the release notes".into(), plugin: slice.plugin.clone(), options: LaunchOptions::default(),
        model: None, account: AccountId::default_account(),
    }).unwrap();
    slice.settle(&mut replacement);
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].agent_id.as_ref(), Some(replacement.agent()), "the task is the new node's");
    assert_ne!(replacement.agent(), &old);
    assert_eq!(lifecycle(&slice.domain).execution().map(|stamp| stamp.generation()), Some(2), "a new generation runs it");
    assert!(slice.answer(&card, Hold::ALLOW, None).is_err(), "the old generation's card takes no answer");
    replacement.stop().unwrap();
}

// The orchestrator's replace goes through the launch every source uses: the owner stops the old
// host, its open request is closed with its card, and a new generation runs the task.
#[tokio::test(flavor = "multi_thread")]
async fn the_orchestrators_replace_stops_the_sdk_node_and_closes_its_request_before_a_new_generation_runs() {
    let slice = Slice::new();
    slice.domain.enable_sdk(slice.backend.clone());
    let (driver, card) = slice.asked("decide:bash make test");
    let old = driver.agent().clone();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    let body = serde_json::from_value(serde_json::json!({"task": TASK, "replace": true, "model": "sonnet", "effort": "high", "reason": "Release notes are short prose"})).unwrap();
    let launched = crate::domain_orchestrator_tools::launch(&slice.domain, &slice._home, body, |_| None).await;
    assert!(launched.is_ok(), "{launched:?}");
    assert!(open_card(&slice.domain, "decision-").is_none(), "the stopped host's request is closed with its card");
    assert!(!slice.domain.owns_sdk_host(&old), "the old host is stopped");
    let new = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().expect("the task has a node");
    assert_ne!(new, old);
    assert_eq!(lifecycle(&slice.domain).execution().map(|stamp| stamp.generation()), Some(2), "a new generation runs it");
    assert!(slice.answer(&card, Hold::ALLOW, None).is_err(), "the old generation's card takes no answer");
    slice.domain.stop_sdk_host(&new).unwrap();
}

// A cancelled SDK task whose subagent left work in its worktree keeps its host: the node is told
// which worktree holds what, and the task ends only once the node gave it up.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_sdk_task_with_work_left_in_a_subagents_worktree_keeps_its_host_until_the_node_settles_it() {
    let slice = Slice::new();
    let root = Directory::new();
    let own = crate::worktree::testing::task_worktree(&root, TASK);
    slice.domain.set_task_worktree(&TaskId::from(TASK), own.to_str().unwrap()).unwrap();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    let repo = root.join("repo");
    let subagent = repo.join(".claude/worktrees/agent-a0000000000000001");
    let added = std::process::Command::new("git").current_dir(&repo)
        .args(["worktree", "add", "-q", "-b", "worktree-agent-a0000000000000001", subagent.to_str().unwrap()]).status().unwrap();
    assert!(added.success());
    slice.run(&Actor::System, "subagent", Command::RecordTaskWorktree {
        task_id: TASK.into(), path: subagent.to_str().unwrap().into(), origin: alondra_core::domain::WorktreeOrigin::Subagent, branch: None,
    });
    std::fs::write(subagent.join("draft.txt"), "x").unwrap();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    slice.run(&user(), "cancel", Command::CancelTask { task_id: TASK.into() });
    let timing = crate::task_closer::Timing::new(None);
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, alondra_core::now_millis()).await.unwrap();
    assert!(slice.domain.owns_sdk_host(&agent), "the host stays so the node can settle its worktree");
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].state, TaskState::Closing);
    let told = state.lifecycle_journal[&TaskId::from(TASK)].iter().rev()
        .find_map(|record| match record { alondra_core::domain::LifecycleRecord::Told { text, .. } => text.clone(), _ => None })
        .expect("the node is told what holds the close");
    assert!(told.contains(subagent.to_str().unwrap()) && told.contains("discard"), "{told}");
    assert!(subagent.join("draft.txt").exists(), "nothing with work is deleted");
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "give-up", Command::ConfirmCleanup { task_id: TASK.into(), discard: vec![subagent.to_str().unwrap().into()] });
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, alondra_core::now_millis() + 10 * 60_000).await.unwrap();
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].state, TaskState::Cancelled);
    assert!(!subagent.exists() && !own.exists(), "both worktrees are deleted once nothing in them is lost");
    assert!(!slice.domain.owns_sdk_host(&agent));
}

// The task's native Claude loads every settings source of the user's default profile, with no
// strict MCP that would hide the user's servers, and Alondra's node plugin, launch settings and a
// capability issued to its node on top: its Alondra MCP server reaches the daemon as that node.
#[test]
fn the_tasks_claude_loads_the_users_profile_and_alondras_plugin_settings_and_capability() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&slice.capture).unwrap()).unwrap();
    let args: Vec<&str> = capture["args"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    let after = |flag: &str| args.iter().position(|arg| *arg == flag).and_then(|at| args.get(at + 1)).copied()
        .or_else(|| args.iter().find_map(|arg| arg.strip_prefix(&format!("{flag}="))));
    assert_eq!(after("--setting-sources"), Some("user,project,local"), "{args:?}");
    assert!(!args.contains(&"--strict-mcp-config"), "the user's MCP servers load: {args:?}");
    assert_eq!(after("--plugin-dir"), slice.plugin.to_str(), "{args:?}");
    let settings: serde_json::Value = serde_json::from_str(after("--settings").expect("Alondra's launch settings")).unwrap();
    let own = super::own_worktree_edits(&slice.worktree);
    assert!(own.contains(&format!("Edit(/{}/**)", slice.worktree.display())), "{own:?}");
    assert_eq!(settings, crate::plugins::launch_settings_with(Some(serde_json::json!({"allow": own}))),
        "the only permission Alondra adds is editing inside the node's own worktree");
    let environment: Vec<&str> = capture["env_keys"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    for alondra in ["ALONDRA_DOMAIN_CAPABILITY", "ALONDRA_DOMAIN_PROTOCOL", "ALONDRA_SESSION_ID", "ALONDRA_HOME", "ALONDRA_SDK_HOST"] {
        assert!(environment.contains(&alondra), "{alondra} reaches the node plugin's MCP server: {environment:?}");
    }
    for profile in ["CLAUDE_CONFIG_DIR", "CLAUDE_HOME"] {
        assert!(!environment.contains(&profile), "the user's default profile: {environment:?}");
    }
    let agent = driver.agent().0.clone();
    let session = capture["env_value_sha256"]["ALONDRA_SESSION_ID"].as_str().unwrap();
    use sha2::Digest;
    let expected: String = sha2::Sha256::digest(agent.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(session, expected, "the node the capability was issued to");
    let one: String = sha2::Sha256::digest(b"1").iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(capture["env_value_sha256"]["CLAUDE_CODE_ENABLE_TODO_TOOLS"].as_str(), Some(one.as_str()),
        "the node has Claude Code's task list, which shows as its steps: {environment:?}");
    driver.stop().unwrap();
}

/// The Mac goes down: every process of the host's group is killed, its native Claude included, and
/// the daemon with them.
fn kill_host_and_daemon(driver: Driver) {
    let group = driver.worker.host_group().unwrap();
    let mut exits = alondra_process_group::Exits::new().unwrap();
    for member in alondra_process_group::members(group).unwrap() {
        exits.watch(&member).unwrap();
    }
    assert!(std::process::Command::new("/bin/kill").args(["-KILL", "--", &format!("-{group}")]).status().unwrap().success());
    exits.wait().unwrap();
    drop(driver);
}

impl Slice {
    /// Each native Claude the hosts started: its session, the session it resumed and how many
    /// prompts that session's transcript already held.
    fn starts(&self) -> Vec<serde_json::Value> {
        let path = PathBuf::from(format!("{}.starts", self.capture.display()));
        std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    fn restart(&self) -> Arc<DomainRuntime> {
        Arc::new(DomainRuntime::open_at(&self._home, &self.database).unwrap())
    }

    fn take_over(&self, domain: &Arc<DomainRuntime>) -> Driver {
        adopt(Arc::clone(domain), &self.backend, TASK.into(), Some(&self.plugin)).unwrap()
    }

    /// Reads the host until its turn is running.
    fn until_working(&self, driver: &mut Driver) {
        while lifecycle(&self.domain).activity() != ActivityView::Working {
            driver.observe().unwrap();
        }
    }
}

fn lifecycle(domain: &DomainRuntime) -> alondra_domain::TaskLifecycle {
    let read = domain.read();
    replay(&TASK.into(), &read.state().lifecycle_journal[&TaskId::from(TASK)]).unwrap()
}

/// The open card of the task whose id starts with `prefix`.
fn open_card(domain: &DomainRuntime, prefix: &str) -> Option<Interaction> {
    domain.read().state().interactions.values()
        .find(|card| card.id.0.starts_with(prefix) && !card.withdrawn && matches!(&card.payload, InteractionPayload::Question { answer: None, .. }))
        .cloned()
}

fn answer(domain: &DomainRuntime, card: &InteractionId, option: &str) {
    let state = domain.state().unwrap();
    let answer = Command::AnswerQuestion { interaction_id: card.clone(), option_id: option.into(), inputs: BTreeMap::new(), attachments: vec![] };
    assert!(domain.execute_with(&user(), &envelope(&state, &format!("client-{option}"), answer), None, None, || Ok(())).unwrap().changed);
}

// R1: the Mac shuts down after the first turn's card: host, Claude and daemon are gone. The daemon
// that comes back finds no process holding the host's lock, launches a fresh host for the same
// execution with the next owner generation, and «Carry on» resumes the same native session with its
// transcript: one more Claude starts, with --resume of the stored session.
#[test]
fn a_dead_host_is_replaced_by_one_that_resumes_the_same_claude_session() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    assert_eq!(owner_generation(&slice.database), 2, "the replacement owns the execution with the next generation");
    assert_eq!(slice.starts().len(), 1, "no Claude starts before the user answers");
    let node = restarted.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    assert!(!crate::node_revival::needed(&restarted, &node.0), "no claude respawn, and no card about it, for a node of the SDK");
    carry_on(&restarted, "Add the tests first");
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
    assert_eq!(starts[1]["history"], 1, "with the transcript of the first turn");
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    assert_eq!(Slice::read_cards(&restarted).len(), 2, "the resumed turn ended with its own card");
    resumed.stop().unwrap();
}

// R2: the Mac shuts down mid-turn. The ended host's turn ended with it, and a host's end is not
// the user's decision: the resumed session is told to go on by itself, with no card. Before, the
// task held on «was interrupted when its session closed» until someone pressed «Carry on».
#[test]
fn a_turn_cut_by_the_end_of_its_host_goes_on_by_itself_in_the_resumed_session() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    assert_goes_on_by_itself(&slice, &restarted, &mut resumed, &session);
    resumed.stop().unwrap();
}

/// No card asks how the cut task goes on: the session the new host resumed is told to go on, and
/// it does.
fn assert_goes_on_by_itself(slice: &Slice, domain: &DomainRuntime, resumed: &mut Driver, session: &str) {
    assert!(open_card(domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
    assert_eq!(resumed.pending_permits(), 1, "the resumed session is told to go on");
    resumed.step(&|| false).unwrap();
    slice.settle(resumed);
    let starts = slice.starts();
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session), "the same session is resumed: {starts:?}");
    assert!(slice.prompts().last().unwrap().starts_with("Your session was cut off when its host stopped"), "{:?}", slice.prompts());
}

// A session whose event stream fails ends as `runtime_error`, and a new one opens on the
// next command. Here the host dies while this daemon runs (its Node crashed mid-turn): its owner
// puts a host that resumes the same Claude session in its place, and the task goes on in it by
// itself instead of staying «working». A replacement that ends before it was handed anything is not
// replaced again: it would only end again.
#[test]
fn a_host_that_dies_under_the_daemon_is_replaced_by_one_that_resumes_the_session() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let group = driver.worker.host_group().unwrap();
    let mut exits = alondra_process_group::Exits::new().unwrap();
    for member in alondra_process_group::members(group).unwrap() {
        exits.watch(&member).unwrap();
    }
    assert!(std::process::Command::new("/bin/kill").args(["-KILL", "--", &format!("-{group}")]).status().unwrap().success());
    exits.wait().unwrap();
    while driver.observe().is_ok() {}
    assert!(driver.host_to_replace(), "the owner saw its host close");
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    assert_eq!(owner_generation(&slice.database), 2, "the replacement owns the execution with the next generation");
    resumed.host_closed = true;
    assert!(!resumed.host_to_replace(), "a replacement handed nothing yet is not replaced again");
    resumed.host_closed = false;
    assert_goes_on_by_itself(&slice, &slice.domain, &mut resumed, &session);
    resumed.stop().unwrap();
}

// The native Claude crashes alone mid-turn: its host lives on with nothing to run. Its owner ends
// it as a host that ended with its Claude, and a host that resumes the same session takes its
// place, and the task goes on in that session by itself. Before, the owner refused
// the Claude's exit from a live host and stopped, leaving the task «working».
#[test]
fn a_claude_that_crashes_under_its_live_host_is_resumed_by_a_new_host() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let (session, native) = (slice.starts()[0]["session"].as_str().unwrap().to_owned(), slice.starts()[0]["pid"].to_string());
    assert!(std::process::Command::new("/bin/kill").args(["-KILL", &native]).status().unwrap().success());
    while driver.observe().is_ok() {}
    assert!(!driver.worker.host_exited(), "the host outlived its Claude");
    assert!(driver.host_to_replace(), "the owner takes the Claude's exit as the end of its host");
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    assert_goes_on_by_itself(&slice, &slice.domain, &mut resumed, &session);
    assert_eq!(slice.starts().len(), 2, "{:?}", slice.starts());
    resumed.stop().unwrap();
}

// Production, 0.3.0: a step of the owner failed («provider observation refused») and the owner
// went, leaving its host running with nobody to read it, for good. An owner whose step fails now
// stops its host, waits for its group to end, and puts a host that resumes the same session in its
// place, where the task goes on by itself.
#[test]
fn an_owner_whose_step_fails_stops_its_host_and_resumes_the_session_in_a_new_one() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let processes = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    driver.fail_next_step = true;
    assert!(driver.step(&|| false).is_err(), "the step failed");
    assert!(driver.host_to_replace(), "a failed owner replaces its host");
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    let left: Vec<String> = processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "the failed owner's host survived: {left:?}");
    assert_goes_on_by_itself(&slice, &slice.domain, &mut resumed, &session);
    resumed.stop().unwrap();
}

// A replacement whose owner fails before it was handed anything is not replaced again: it would only
// end again. Its owner still stops it, and the task gets a card that says what happened and offers
// to try again or cancel; it is never left as it is.
#[tokio::test(flavor = "multi_thread")]
async fn a_replacement_whose_owner_fails_is_stopped_and_its_task_gets_a_card_to_try_again() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    let processes = alondra_process_group::members(resumed.worker.host_group().unwrap()).unwrap();
    resumed.fail_next_step = true;
    let mut changed = restarted.subscribe();
    let owner = Owner::spawn(resumed);
    let reader = Arc::clone(&restarted);
    changed.wait_for(|_| open_card(&reader, HOST_LOST_PREFIX).is_some()).await.unwrap();
    assert!(owner.ended(), "the owner owns nothing any more");
    let agent = owner.agent().clone();
    restarted.add_sdk_owner(owner);
    assert!(!restarted.owns_sdk_host(&agent));
    let left: Vec<String> = processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "the replacement's host survived its owner: {left:?}");
    let card = open_card(&restarted, HOST_LOST_PREFIX).unwrap();
    assert_eq!(card.card.content.as_ref().map(|content| content.body.as_str()), Some("Its new host stopped before it could take the task's next message"));
}

// The daemon's start could not take the execution over (here no new host can get its folder): the
// task gets a card with that reason instead of being left as it is. Once that is fixed, «Try
// again» takes it over and the same session goes on.
#[tokio::test(flavor = "multi_thread")]
async fn an_execution_the_start_could_not_take_over_gets_a_card_and_try_again_resumes_it() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let (session_host, _) = strand_host(&slice, driver);
    assert_eq!(session_host, session);
    let refuse = |refused: bool| std::fs::set_permissions(&slice.backend.state_root, std::fs::Permissions::from_mode(if refused { 0o500 } else { 0o700 })).unwrap();
    refuse(true);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    adopt_all(&restarted, &slice.backend);
    let card = open_card(&restarted, HOST_LOST_PREFIX).expect("the task has a card");
    assert_eq!(card.card.content.as_ref().map(|content| content.body.as_str()), Some("Its new host could not start (Disconnected)"));
    assert_eq!(restarted.state().unwrap().tasks[&TaskId::from(TASK)].state, TaskState::NeedsYou);
    refuse(false);
    answer(&restarted, &card.id, RETAKE);
    restarted.join_retakes();
    let agent = restarted.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    assert!(restarted.owns_sdk_host(&agent), "the execution was taken over again");
    let mut changed = restarted.subscribe();
    let capture = slice.capture.clone();
    changed.wait_for(|_| prompts_at(&capture).last().is_some_and(|prompt| prompt.starts_with("Your session was cut off when its host stopped"))).await.unwrap();
    restarted.stop_sdk_owners();
    let starts = slice.starts();
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
}

// Alondra cannot record the end of Claude's turn, so the task's record no longer follows its
// Claude: only that turn fails. The owner stops that host, a new one resumes the same session and
// the task goes on by itself, with no card to try again.
#[test]
fn a_turn_end_alondra_cannot_record_fails_only_that_turn_and_a_new_host_resumes_the_session() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    crate::domain_store::refuse_next(crate::domain_store::Refuse::TurnEnd);
    let failed = loop {
        if let Err(Failed(why)) = driver.observe() {
            break why;
        }
    };
    assert!(failed.starts_with("Alondra could not record what its Claude did (the end of its turn"), "{failed}");
    assert!(driver.host_to_replace(), "its host is replaced, not lost");
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    assert!(open_card(&slice.domain, HOST_LOST_PREFIX).is_none(), "no card to try again");
    assert_goes_on_by_itself(&slice, &slice.domain, &mut resumed, &session);
    assert_eq!(Slice::read_cards(&slice.domain).iter().filter(|(_, answered)| !answered).count(), 1, "the resumed turn ends with its one card");
    resumed.stop().unwrap();
}

// The same happens again in the session Alondra resumed, before any of its turns ended: resuming
// once more would only end the same way, so one card asks the user how it goes on.
#[test]
fn a_turn_end_alondra_cannot_record_again_after_resuming_asks_the_user_once() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    let unrecorded = |driver: &mut Driver| loop {
        if let Err(Failed(why)) = driver.observe() {
            break why;
        }
    };
    crate::domain_store::refuse_next(crate::domain_store::Refuse::TurnEnd);
    unrecorded(&mut driver);
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    resumed.step(&|| false).unwrap();
    crate::domain_store::refuse_next(crate::domain_store::Refuse::TurnEnd);
    unrecorded(&mut resumed);
    assert!(resumed.host_to_replace());
    let mut again = resumed.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    assert!(open_card(&slice.domain, HOST_LOST_PREFIX).is_none(), "no card to try again");
    let card = open_card(&slice.domain, SESSION_ENDED_PREFIX).expect("one card asks how it goes on");
    let InteractionPayload::Question { options, .. } = &card.payload else { unreachable!() };
    assert_eq!(options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(), [CARRY_ON, CANCEL_TASK]);
    let open = slice.domain.read().state().interactions.values()
        .filter(|card| card.scope == alondra_core::domain::Scope::Task { task_id: TASK.into() } && !card.withdrawn && matches!(&card.payload, InteractionPayload::Question { answer: None, .. }))
        .count();
    assert_eq!(open, 1, "only that card waits on the user");
    again.stop().unwrap();
}

// A subagent hands its report back after the turn ended: Claude starts a turn by itself while the
// task rests. Its owner reads it as it comes: the card that asked how the task goes on is withdrawn
// and the node works, with nothing sent to it.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_claude_starts_while_its_task_rests_is_read_as_it_comes() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("own-turn-later");
    slice.settle(&mut driver);
    let card = open_card(&slice.domain, TURN_ENDED_PREFIX).expect("the turn ended with its card").id;
    let owner = Owner::spawn(driver);
    let mut changed = slice.domain.subscribe();
    std::fs::write(format!("{}.own-turn", slice.capture.display()), "").unwrap();
    let reader = Arc::clone(&slice.domain);
    changed.wait_for(|_| reader.read().state().interactions[&card].withdrawn).await.unwrap();
    assert_eq!(lifecycle(&slice.domain).activity(), ActivityView::Working, "the node works");
    assert_eq!(slice.prompts().len(), 1, "nothing was sent to it");
    drop(owner);
}

// The domain refuses to record that Claude accepted an interrupt: the owner goes on, and the turn
// Claude ends still ends with one card.
#[test]
fn an_accepted_interrupt_alondra_cannot_record_leaves_the_owner_going() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    press_interrupt(&slice, driver.agent());
    crate::domain_store::refuse_next(crate::domain_store::Refuse::InterruptAccepted);
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "Claude got the interrupt");
    slice.settle(&mut driver);
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "the interrupted turn ended");
    assert_eq!(Slice::read_cards(&slice.domain).len(), 1, "with one card");
    driver.stop().unwrap();
}

// The task of 2026-10-21 as its «Try again» card left it: its host ended with Claude's request to
// leave plan mode with 41 points in its journal, unread, asked from a turn Claude opened by itself
// while what the user told it waited. «Try again» reads that journal: the plan is refused with
// why, and a host that resumes the same session takes the task.
#[tokio::test(flavor = "multi_thread")]
async fn try_again_on_a_task_whose_ended_host_left_a_refused_plan_resumes_its_session() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("own-turn-plan");
    slice.settle(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Submit your plan"})).unwrap();
    crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "David", driver.agent(), message, true).await.unwrap();
    driver.step(&|| false).unwrap();
    while !driver.worker.poll().unwrap().is_some_and(|batch| String::from_utf8_lossy(&batch.evidence).contains("decision_requested")) {
        driver.observe().unwrap();
    }
    kill_host_and_daemon(driver);
    let refuse = |refused: bool| std::fs::set_permissions(&slice.backend.state_root, std::fs::Permissions::from_mode(if refused { 0o500 } else { 0o700 })).unwrap();
    refuse(true);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    adopt_all(&restarted, &slice.backend);
    let card = open_card(&restarted, HOST_LOST_PREFIX).expect("the task has the card to try again");
    refuse(false);
    answer(&restarted, &card.id, RETAKE);
    restarted.join_retakes();
    let agent = restarted.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    assert!(restarted.owns_sdk_host(&agent), "the execution was taken over again: {:?}", open_card(&restarted, HOST_LOST_PREFIX).and_then(|card| card.card.content));
    let mut changed = restarted.subscribe();
    let capture = slice.capture.clone();
    changed.wait_for(|_| prompts_at(&capture).last().is_some_and(|prompt| prompt.starts_with("Your session was cut off when its host stopped"))).await.unwrap();
    restarted.stop_sdk_owners();
    let starts = slice.starts();
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
    let refused = restarted.read().state().lifecycle_journal[&TaskId::from(TASK)].iter().any(|record| matches!(record,
        alondra_core::domain::LifecycleRecord::DecidedByPolicy { decision: alondra_core::domain::DecisionRecord::Deny { message }, .. } if message.contains("at most 40 points")));
    assert!(refused, "the plan the ended host left was refused for its length");
}

fn prompts_at(capture: &Path) -> Vec<String> {
    let path = PathBuf::from(format!("{}.prompts", capture.display()));
    std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// The descriptor of this process that is connected to the socket at `path`.
fn connected_to(path: &Path) -> libc::c_int {
    use std::os::unix::ffi::OsStrExt;
    (0..4096)
        .find(|fd| {
            // SAFETY: getpeername writes at most `length` bytes into the zeroed address it is given.
            let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
            let mut length = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
            let named = unsafe { libc::getpeername(*fd, (&raw mut address).cast(), &mut length) } == 0;
            let peer: Vec<u8> = address.sun_path.iter().take_while(|byte| **byte != 0).map(|byte| *byte as u8).collect();
            named && i32::from(address.sun_family) == libc::AF_UNIX && peer == path.as_os_str().as_bytes()
        })
        .expect("a connection to the host")
}

// The host and its Claude die together, and the owner's read of the host fails before the host's
// exit can be observed. The connection failing is the fact the owner acts on: the replacement
// waits for the host's exit and reads its lock, and a host that resumes the session takes its place.
// Before, the owner took a host whose exit it could not see yet as alive, detached, and the task
// stayed «working» until the daemon restarted. Here the host closes its end and lives on, which
// orders the two facts with no clock: the connection fails first, the host ends only when stopped.
#[test]
fn a_host_whose_connection_fails_before_its_exit_is_seen_is_replaced() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let socket = connected_to(&slice.backend.state_root.join(&driver.agent().0).join("s"));
    // SAFETY: four bytes from a live array to a descriptor the driver keeps open.
    assert_eq!(unsafe { libc::write(socket, [0u8; 4].as_ptr().cast(), 4) }, 4, "a frame of no length, which the host refuses by closing");
    while driver.observe().is_ok() {}
    assert!(!driver.worker.host_exited(), "the read failed while the host's exit was not observable");
    assert!(driver.host_to_replace(), "the failed connection ends the host for its owner");
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    assert_goes_on_by_itself(&slice, &slice.domain, &mut resumed, &session);
    resumed.stop().unwrap();
}

// R3: a normal shutdown stops the host (5d) mid-turn: what the stopping host wrote after the last
// acknowledgement (its Claude's exit, the end of its SDK iterator) is committed as evidence, and
// the task goes on by itself in the session the next daemon resumes.
#[test]
fn a_host_stopped_mid_turn_by_a_normal_shutdown_leaves_evidence_the_next_daemon_commits() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let mut owner = Owner::spawn(driver);
    owner.stop().unwrap();
    drop(owner);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    assert_goes_on_by_itself(&slice, &restarted, &mut resumed, &session);
    resumed.stop().unwrap();
}

// R4: the first turn ended, the user answered and the host offered the answer to a Claude whose
// session then ended before it showed it read it; the Mac went down before the receipt was kept.
// Nobody is asked: the resumed session gets the same words once more, flagged as possibly received
// already, and its transcript tells Claude whether it was.
#[test]
fn a_doubtful_answer_goes_again_to_the_resumed_session_flagged_unasked() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first unread");
    assert!(driver.claim_record_and_offer_without_receipt(), "the host offered the answer");
    while slice.prompts().len() < 2 {
        driver.observe().unwrap();
    }
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    assert!(open_card(&restarted, HANDOFF_DOUBT_PREFIX).is_none() && open_card(&restarted, SESSION_ENDED_PREFIX).is_none(), "nobody is asked");
    assert_eq!(resumed.pending_permits(), 1, "the answer goes again");
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 3, "{prompts:?}");
    assert!(prompts[2].starts_with("Your session was cut off when its host stopped") && prompts[2].ends_with(&prompts[1]), "{prompts:?}");
    assert!(slice.starts()[1]["resume"].is_string(), "to the resumed session");
    assert_eq!(Slice::read_cards(&restarted).len(), 2, "its turn ended with its own card");
    resumed.stop().unwrap();
}

// R5: the answer was claimed and recorded, and the Mac went down before the host got it. The ended
// host's journal shows it never took the command, so it goes once to the resumed session, unasked.
#[test]
fn an_answer_the_ended_host_never_took_goes_to_the_resumed_session_unasked() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first");
    assert!(driver.claim_and_record_without_sending(), "the owner claimed and recorded the dispatch");
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    assert!(open_card(&restarted, HANDOFF_DOUBT_PREFIX).is_none(), "nothing to ask: the journal proves it never left");
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    assert!(slice.starts()[1]["resume"].is_string());
    resumed.stop().unwrap();
}

// The daemon dies and the host's state folder goes with it, as when a sandbox or a test fixture is
// removed: no owner can adopt that host any more, so it ends its whole group, Claude included.
// A turn whose result is not a success ends as failed, with its code. Claude ended this one
// at its turn limit: the task waits for no one, neither the API nor the host, and its card names
// the error.
#[test]
fn a_turn_claude_ends_as_failed_asks_how_to_go_on_and_names_its_error() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("max-turns");
    slice.settle(&mut driver);
    assert_eq!(Slice::turn_end_need(&slice.domain), "Claude's turn ended with an error (`error_max_turns`): tell it how to go on");
    assert!(slice.domain.read().state().claude_held().is_empty(), "nothing waits for the API");
    assert!(!driver.waits_for_host());
    driver.stop().unwrap();
}

// Claude answered the prompt with a result that never delivered it. The owner stops waiting for
// it, the user sees the whole message, and «Send it again» gives Claude the same words.
#[test]
fn a_prompt_claude_never_delivers_asks_whether_to_send_it_again_and_goes_again_on_the_users_word() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("failed-delivery");
    slice.settle(&mut driver);
    let card = open_card(&slice.domain, DROPPED_PREFIX).expect("the dropped prompt asks the user");
    assert_eq!(card.card.need, "Claude didn't get to read your message");
    assert_eq!(card.card.content.as_ref().map(|content| content.body.as_str()), Some("failed-delivery"));
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty(), "nothing waits for Claude to read it");
    assert!(!driver.waits_for_host(), "the owner no longer waits for it");
    assert_eq!(Slice::read_cards(&slice.domain), [], "no other card");

    let state = slice.domain.state().unwrap();
    let again = Command::AnswerQuestion { interaction_id: card.id.clone(), option_id: SEND_AGAIN.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Write the release notes".into()))]), attachments: vec![] };
    slice.domain.execute_with(&user(), &envelope(&state, "client-send-again", again), None, None, || Ok(())).unwrap();
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert_eq!(prompts, ["failed-delivery", "failed-delivery\n\nWrite the release notes"]);
    assert_eq!(Slice::read_cards(&slice.domain).len(), 1, "its turn ended with its own card");
    driver.stop().unwrap();
}

// The turn of a prompt Claude completes without starting fails, as Claude does with one its
// transcript already holds. Here the message leaves the queue and «Discard» lets it go: the task
// asks how to go on and Claude gets nothing more.
#[test]
fn a_message_claude_completes_without_starting_it_can_be_discarded() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "complete-unstarted");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let card = open_card(&slice.domain, DROPPED_PREFIX).expect("the dropped message asks the user");
    assert!(card.card.content.as_ref().is_some_and(|content| content.body.ends_with("complete-unstarted")), "{:?}", card.card.content);
    assert!(!driver.waits_for_host(), "the owner no longer waits for it");
    answer(&slice.domain, &card.id, DISCARD);
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty());
    assert_eq!(driver.pending_permits(), 0, "nothing is sent");
    assert!(Slice::read_cards(&slice.domain).iter().any(|(_, answered)| !answered), "the task asks how to go on");
    assert_eq!(slice.prompts().len(), 2);
    driver.stop().unwrap();
}

#[test]
fn a_host_whose_state_folder_is_removed_ends_its_group() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let group = driver.worker.host_group().expect("the driver has its host");
    let processes = alondra_process_group::members(group).unwrap();
    assert!(processes.len() >= 2, "the host and its native Claude share the host's group: {processes:?}");
    drop(driver);
    std::fs::remove_dir_all(&slice.backend.state_root).unwrap();
    let mut exits = alondra_process_group::Exits::new().unwrap();
    for process in &processes {
        exits.watch(process).unwrap();
    }
    exits.wait().unwrap();
    let left: Vec<String> = alondra_process_group::members(group)
        .unwrap()
        .iter()
        .filter(|member| !member.zombie)
        .map(ToString::to_string)
        .collect();
    assert!(left.is_empty(), "the unadoptable host's group survived: {left:?}");
}

fn system() -> Actor {
    Actor::System
}

fn subtypes(controls: &[serde_json::Value]) -> Vec<&str> {
    controls.iter().map(|control| control["subtype"].as_str().unwrap()).collect()
}

/// The user presses «Interrupt» on the task of `agent`, through the action path the clients use.
fn press_interrupt(slice: &Slice, agent: &AgentId) -> serde_json::Value {
    press(slice, alondra_core::views::action_id::Verb::Interrupt, agent)
}

/// The user presses the node's `verb` button on the task of `agent`, through the action path the
/// clients use.
fn press(slice: &Slice, verb: alondra_core::views::action_id::Verb, agent: &AgentId) -> serde_json::Value {
    let state = slice.domain.state().unwrap();
    let action = alondra_core::views::action_id::ActionId::new(verb, &agent.0, state.revision, None).unwrap();
    let request = crate::views::actions::ActionRequest {
        protocol: alondra_core::views::VIEWS_PROTOCOL,
        command_id: format!("0b6f9a52-3c1e-4d7a-9f20-{:012x}", state.revision),
        data_epoch: state.data_epoch.0.clone(),
        client: "ios".into(),
        lang: "en".into(),
        action_id: action.to_string(),
        option_id: None,
        input: Default::default(),
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
        crate::views::actions::execute_action(&slice.domain, &client, &slice._home, "test-device", request, None, || Ok(())).await.unwrap()
    })
}

// C1: the user presses Interrupt on the task of a working SDK node. The control commits with the
// domain's decision, the owner sends it, the host hands it to Claude, and the interrupted turn
// ends with one card.
#[test]
fn interrupting_a_working_sdk_node_from_its_task_reaches_claude_and_its_turn_ends_with_one_card() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let offered = node_offers(&slice.domain, driver.agent());
    assert!(offered.iter().any(|offer| offer.starts_with("interrupt:")), "a working SDK node offers Interrupt: {offered:?}");
    for claude_bg in ["attach", "close:"] {
        assert!(!offered.iter().any(|offer| offer.starts_with(claude_bg)), "an SDK node offers no {claude_bg}: {offered:?}");
    }
    let reply = press_interrupt(&slice, driver.agent());
    assert_eq!(reply["outcome"], "applied", "{reply}");
    let turn = Slice::lifecycle(&slice.domain).turn().unwrap().as_str().to_owned();
    assert_eq!(slice.control_rows(), [("interrupt".to_owned(), Some(turn), "prepared".to_owned())], "the interrupt committed with the decision");
    assert!(slice.controls().is_empty(), "nothing reached Claude before the owner sends it");
    assert_eq!(driver.pending_controls(), 1, "its permit reached the owner");
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "Claude got the interrupt");
    assert_eq!(slice.control_rows()[0].2, "handed");
    slice.settle(&mut driver);
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "the interrupted turn ended");
    let cards = Slice::read_cards(&slice.domain);
    assert_eq!(cards.len(), 1, "the turn ended with one card: {cards:?}");
    assert_eq!(Slice::turn_end_need(&slice.domain), "You interrupted “<title>”: tell it how to go on", "the card says the user stopped the turn");
    assert!(!node_offers(&slice.domain, driver.agent()).iter().any(|offer| offer.starts_with("interrupt:")), "nothing runs: no Interrupt");
    let again = slice.try_run(&user(), "interrupt-again", Command::InterruptTurn { agent_id: driver.agent().clone() }).unwrap_err();
    let StoreError::Domain(refusal) = again else { panic!("{again:?}") };
    assert_eq!(alondra_core::views::refusal_text(&refusal.why(), alondra_core::views::text::Lang::En), "This node isn't running anything to interrupt.");
    driver.stop().unwrap();
}

// Claude accepts the interrupt but its turn goes on, and it ignores SIGTERM. Once the interrupt is
// accepted, its task offers «Force stop» instead of «Interrupt»; pressing it ends Claude's process,
// which ends the turn as the user's interrupt, and «Carry on» reaches the same session in a new
// host. Before, the node stayed «working» with nothing that could stop it.
#[test]
fn a_turn_claude_keeps_running_after_its_interrupt_is_force_stopped_and_resumed() {
    use alondra_core::views::action_id::Verb;
    let slice = Slice::new();
    let mut driver = slice.launch_with("ignore-interrupt");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let refused = slice.try_run(&user(), "force-too-soon", Command::ForceStopTurn { agent_id: driver.agent().clone() }).unwrap_err();
    let StoreError::Domain(refused) = refused else { panic!("{refused:?}") };
    assert_eq!(refused.refusal, Some(alondra_core::domain::Refusal::InterruptNotAccepted), "a turn is interrupted before it is forced");
    assert_eq!(press_interrupt(&slice, driver.agent())["outcome"], "applied");
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "Claude got the interrupt");
    assert_eq!(Slice::lifecycle(&slice.domain).turn_stop(), Some(alondra_domain::TurnStop::InterruptAccepted), "its receipt is the fact that Claude accepted it");
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Working, "and the turn goes on");
    let offered = node_offers(&slice.domain, driver.agent());
    assert!(offered.iter().any(|offer| offer.starts_with("force_stop:")), "the stop button is now Force stop: {offered:?}");
    assert!(!offered.iter().any(|offer| offer.starts_with("interrupt:")), "in place of Interrupt: {offered:?}");

    assert_eq!(press(&slice, Verb::ForceStop, driver.agent())["outcome"], "applied");
    let turn = Slice::lifecycle(&slice.domain).turn().unwrap().as_str().to_owned();
    assert_eq!(slice.control_rows().last().unwrap(), &("force_stop".to_owned(), Some(turn), "prepared".to_owned()));
    let offered = node_offers(&slice.domain, driver.agent());
    assert!(!offered.iter().any(|offer| offer.starts_with("force_stop:") || offer.starts_with("interrupt:")), "it is forced once, and nothing else offers to stop it: {offered:?}");
    let native = slice.starts()[0]["pid"].as_i64().unwrap();
    driver.step(&|| false).unwrap();
    assert_eq!(slice.control_rows().last().unwrap().2, "handed", "the host answered once Claude exited");
    // SAFETY: signal 0 only asks whether the process exists.
    assert_eq!(unsafe { libc::kill(native as libc::pid_t, 0) }, -1, "Claude's process is gone");
    while driver.observe().is_ok() {}
    assert!(driver.host_to_replace(), "Claude's exit ends its host for the owner");
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "the forced turn ended");
    let card = open_card(&slice.domain, SESSION_ENDED_PREFIX).expect("the stopped task asks the user");
    let title = slice.domain.read().state().tasks[&TaskId::from(TASK)].title.clone();
    assert_eq!(card.card.need, format!("You interrupted “{title}”: tell it how to go on"), "it ended as the user's interrupt");
    answer(&slice.domain, &card.id, CARRY_ON);
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
    resumed.stop().unwrap();
}

// Claude Code tracks a foreground command that runs for seconds as its own work, which ends
// inside the turn that waits for it: that turn goes on with it and ends on one card.
#[test]
fn a_command_claude_code_tracks_as_work_ends_inside_its_turn_and_the_turn_ends_with_one_card() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("foreground-command");
    slice.read_turn_result(&mut driver);
    assert_eq!(slice.observations_with("\"subtype\":\"task_notification\""), 1, "the command's work ended");
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "nothing waits for the command any more");
    let cards = Slice::read_cards(&slice.domain);
    assert_eq!(cards.len(), 1, "the turn ended with one card: {cards:?}");
    assert!(!node_offers(&slice.domain, driver.agent()).iter().any(|offer| offer.starts_with("interrupt:")), "nothing runs: no Interrupt");
    driver.stop().unwrap();
}

// The user interrupts from the task while such a command runs: Claude Code stops it, and the turn
// that waited for it ends with one card.
#[test]
fn interrupting_a_command_claude_code_tracks_as_work_ends_its_turn_with_one_card() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("foreground-command-hold");
    while slice.observations_with("\"subtype\":\"task_started\"") == 0 {
        driver.observe().unwrap();
    }
    assert!(node_offers(&slice.domain, driver.agent()).iter().any(|offer| offer.starts_with("interrupt:")), "the running command offers Interrupt");
    let reply = press_interrupt(&slice, driver.agent());
    assert_eq!(reply["outcome"], "applied", "{reply}");
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "Claude got the interrupt");
    slice.read_turn_result(&mut driver);
    assert_eq!(slice.observations_with("\"status\":\"stopped\""), 1, "Claude Code stopped the command");
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "the interrupted turn ended");
    let cards = Slice::read_cards(&slice.domain);
    assert_eq!(cards.len(), 1, "the turn ended with one card: {cards:?}");
    assert_eq!(Slice::turn_end_need(&slice.domain), "You interrupted “<title>”: tell it how to go on", "the card says the user stopped the turn");
    assert!(!node_offers(&slice.domain, driver.agent()).iter().any(|offer| offer.starts_with("interrupt:")), "nothing runs: no Interrupt");
    driver.stop().unwrap();
}

// C2: Alondra sets the model and the permission mode and reads the MCP status. Each control reaches
// Claude with the host's receipt, and the MCP status comes back as kept host evidence.
#[test]
fn model_permission_mode_and_mcp_status_controls_reach_claude_and_the_status_is_kept() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    slice.run(&system(), "set-model", Command::SetModel { task_id: TASK.into(), model: "fixture-model".into() });
    driver.step(&|| false).unwrap();
    slice.run(&system(), "set-mode", Command::SetPermissionMode { task_id: TASK.into(), mode: PermissionModeRecord::DontAsk });
    driver.step(&|| false).unwrap();
    slice.run(&system(), "mcp-status", Command::ReadMcpStatus { task_id: TASK.into() });
    driver.step(&|| false).unwrap();
    assert!(driver.waits_for_host(), "the owner reads the host until the status it asked for is kept");
    slice.settle(&mut driver);
    let controls = slice.controls();
    assert_eq!(subtypes(&controls), ["set_model", "set_permission_mode", "mcp_status"]);
    assert_eq!((controls[0]["model"].as_str(), controls[1]["mode"].as_str()), (Some("fixture-model"), Some("dontAsk")));
    let statuses: Vec<String> = slice.control_rows().into_iter().map(|(_, _, status)| status).collect();
    assert_eq!(statuses, ["handed", "handed", "handed"]);
    assert_eq!(slice.observations_with("control_result"), 1, "the MCP status is kept as the host's evidence");
    assert_eq!(slice.observations_with("\"status\":\"connected\""), 1, "with the servers Claude listed");
    assert_eq!(Slice::read_cards(&slice.domain).len(), 1, "no control adds a card");
    driver.stop().unwrap();
}

// C3: refusal without effect. The execution's binding does not permit setting the model: the
// command is refused with the capability error, and nothing of it is kept or sent.
#[test]
fn a_control_the_binding_does_not_permit_is_refused_with_no_effect() {
    let slice = Slice::new();
    let mut without = capabilities();
    without.remove(&Capability::SetModel);
    let mut driver = slice.launch_told("Write the release notes", without);
    slice.settle(&mut driver);
    let (revision, events) = (slice.domain.state().unwrap().revision, slice.journal_events());
    let refused = slice.try_run(&system(), "set-model", Command::SetModel { task_id: TASK.into(), model: "fixture-model".into() }).unwrap_err();
    let StoreError::Storage(error) = &refused else { panic!("{refused:?}") };
    assert!(matches!(error.downcast_ref::<alondra_store::Error>(), Some(alondra_store::Error::UnsupportedCapability)), "{error:#}");
    assert_eq!(slice.domain.state().unwrap().revision, revision, "the domain did not change");
    assert_eq!(slice.journal_events(), events, "no journal event");
    assert!(slice.control_rows().is_empty(), "no control row");
    assert_eq!(driver.pending_controls(), 0, "no permit");
    assert!(slice.controls().is_empty(), "the host got nothing");
    driver.stop().unwrap();
}

// C4: refusal by the host. Claude does not offer the model: the host refuses before touching it,
// the control is recorded rejected, and Claude never got a request to change its model.
#[test]
fn a_model_claude_does_not_offer_is_rejected_by_the_host_and_never_reaches_claude() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    slice.run(&system(), "set-model", Command::SetModel { task_id: TASK.into(), model: "unlisted-model".into() });
    driver.step(&|| false).unwrap();
    assert_eq!(slice.control_rows(), [("set_model".to_owned(), Some("unlisted-model".to_owned()), "rejected".to_owned())]);
    assert!(slice.controls().is_empty(), "Claude got no request: {:?}", slice.controls());
    assert!(!driver.waits_for_host());
    driver.stop().unwrap();
}

// C5: Alondra compacts a resting SDK task that waits on its card: `/compact` reaches Claude, the
// compaction turn runs and ends, and the task rests again on the same single card.
#[test]
fn compacting_a_resting_task_runs_a_compaction_turn_and_keeps_its_one_card() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let before = Slice::read_cards(&slice.domain);
    assert_eq!(before.len(), 1);
    slice.run(&system(), "compact", Command::CompactConversation { task_id: TASK.into() });
    assert_eq!(driver.pending_permits(), 1, "the compaction is an input the owner sends");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts(), ["Write the release notes", "/compact"]);
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting);
    assert_eq!(Slice::read_cards(&slice.domain), before, "the same card waits, unanswered, and no other");
    assert_eq!(slice.stored_cards(), 1);
    driver.stop().unwrap();
}

// A large conversation resting on its card compacts once, in the last minutes of the prompt cache
// its last request wrote, and keeps that card; before then nothing happens.
#[test]
fn a_large_conversation_resting_on_its_card_compacts_once_before_its_cache_expires() {
    use crate::idle_compaction::{Timing, compact_due};
    let slice = Slice::new();
    let mut driver = slice.launch_with("large-context");
    slice.settle(&mut driver);
    let before = Slice::read_cards(&slice.domain);
    assert_eq!(before.len(), 1);
    let readings = slice.domain.contexts.readings();
    assert_eq!(readings.len(), 1, "the answer's usage was measured");
    let reading = readings[0].1;
    assert_eq!(reading.tokens, 250_403);
    let opens = reading.expires_at - Timing::LIVE.lead;
    assert_eq!(compact_due(&slice.domain, opens - 1, &Timing::LIVE), Some(opens), "too early, it looks again when the window opens");
    assert_eq!(driver.pending_permits(), 0);
    compact_due(&slice.domain, opens, &Timing::LIVE);
    assert_eq!(driver.pending_permits(), 1, "the compaction is an input the owner sends");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts(), ["large-context", "/compact"]);
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting);
    assert_eq!(Slice::read_cards(&slice.domain), before, "the same card waits, unanswered, and no other");
    compact_due(&slice.domain, opens + 1, &Timing::LIVE);
    assert_eq!(driver.pending_permits(), 0, "one rest compacts once");
    driver.stop().unwrap();
}

/// The context the live view shows for the SDK task's node, launched told `prompt`.
fn shown_context(prompt: &str) -> (Option<u32>, Option<u64>) {
    let slice = Slice::new();
    let mut driver = slice.launch_with(prompt);
    slice.settle(&mut driver);
    let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
    let state = slice.domain.state().unwrap();
    let live = tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::views::live::gather(&client, &state, alondra_core::views::Lang::En, &[], false, false));
    let node = live.nodes.iter().find(|node| node.agent_id == driver.agent().0).expect("the task's node is live");
    let shown = (node.context_percent, node.context_window);
    driver.stop().unwrap();
    shown
}

// The node of an SDK task shows how much of its model's window its last answer used: 250 403
// tokens of the million Claude said its model has.
#[test]
fn an_sdk_node_shows_the_context_its_last_answer_used_of_its_models_window() {
    assert_eq!(shown_context("large-context"), (Some(25), Some(1_000_000)));
}

// Without a window Claude gave for the conversation's own model, the node shows no percent rather
// than one against a window nobody said.
#[test]
fn an_sdk_node_without_its_models_window_shows_no_context() {
    assert_eq!(shown_context("large-context-unsized"), (None, None), "Claude sized only another model");
    assert_eq!(shown_context("Write the release notes"), (None, None), "Claude sized no model");
}

// C6: the daemon dies after recording a read of the MCP status and before the host gets it. A
// fresh daemon adopts the host and reconciles the control once: Claude answers it a single time
// and its status is kept.
#[test]
fn a_control_the_host_never_got_is_sent_once_by_the_adopting_owner() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    slice.run(&system(), "mcp-status", Command::ReadMcpStatus { task_id: TASK.into() });
    assert!(driver.claim_and_record_control_without_sending(), "the owner claimed and recorded the control");
    drop(driver);
    assert!(slice.controls().is_empty(), "the host never got it");
    assert_eq!(slice.control_rows()[0].2, "claimed");
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(slice.control_rows()[0].2, "handed", "reconciled with the host's answer");
    slice.settle(&mut adopted);
    assert_eq!(subtypes(&slice.controls()), ["mcp_status"], "Claude got it exactly once");
    assert_eq!(slice.observations_with("control_result"), 1, "its status is kept");
    adopted.stop().unwrap();
}

// C7: an interrupt whose turn ended before the owner claimed it is revoked by the claim and never
// reaches Claude.
#[test]
fn an_interrupt_whose_turn_already_ended_is_revoked_and_never_sent() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let agent = driver.agent().clone();
    slice.run(&user(), "interrupt-first", Command::InterruptTurn { agent_id: agent.clone() });
    slice.run(&user(), "interrupt-second", Command::InterruptTurn { agent_id: agent });
    let statuses = |slice: &Slice| slice.control_rows().into_iter().map(|(_, _, status)| status).collect::<Vec<String>>();
    assert_eq!(statuses(&slice), ["prepared", "prepared"]);
    driver.step(&|| false).unwrap();
    assert_eq!(statuses(&slice), ["handed", "prepared"]);
    slice.settle(&mut driver);
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "the first interrupt ended the turn");
    while driver.pending_controls() > 0 {
        driver.step(&|| false).unwrap();
    }
    assert_eq!(statuses(&slice), ["handed", "revoked"]);
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "only the first reached Claude");
    assert_eq!(Slice::read_cards(&slice.domain).len(), 1);
    driver.stop().unwrap();
}

// C8: a quiet turn never holds back a control. With nothing from the host, a step returns to look
// for what to send instead of blocking on the host, and the interrupt that comes next is sent.
#[test]
fn a_step_on_a_quiet_turn_returns_to_send_the_interrupt_that_comes_next() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    assert!(driver.waits_for_host(), "the turn runs");
    driver.step(&|| false).unwrap();
    slice.run(&user(), "interrupt", Command::InterruptTurn { agent_id: driver.agent().clone() });
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "the interrupt went out on the next step");
    slice.settle(&mut driver);
    assert_eq!(Slice::read_cards(&slice.domain).len(), 1);
    driver.stop().unwrap();
}

const DECISION_CARDS: &str = "decision-sdk-slice-";

/// A task's card that asks the user for a decision Claude waits on.
#[derive(Debug, PartialEq)]
struct DecisionCard {
    id: String,
    answered: bool,
    withdrawn: bool,
}

impl Slice {
    /// What the fake Claude got for each decision it asked for, in order.
    fn decisions(&self) -> Vec<serde_json::Value> {
        let path = PathBuf::from(format!("{}.decisions", self.capture.display()));
        std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    fn decision_cards(domain: &DomainRuntime) -> Vec<DecisionCard> {
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

    fn open_decision_cards(domain: &DomainRuntime) -> Vec<String> {
        Self::decision_cards(domain).into_iter().filter(|card| !card.answered && !card.withdrawn).map(|card| card.id).collect()
    }

    fn options(&self, card: &str) -> Vec<String> {
        let state = self.domain.state().unwrap();
        let InteractionPayload::Question { options, .. } = &state.interactions[&InteractionId(card.into())].payload else { panic!("{card} is no question") };
        options.iter().map(|option| option.id.clone()).collect()
    }

    /// Runs the owner one step at a time until `done`; it never waits for a host that has nothing
    /// the task waits for.
    fn until(&self, driver: &mut Driver, done: impl Fn(&Slice) -> bool) {
        while !done(self) {
            assert!(driver.pending_controls() > 0 || driver.pending_permits() > 0 || driver.waits_for_host(),
                "nothing is left to run and it is not done: {:?}", Slice::lifecycle(&self.domain).activity());
            driver.step(&|| false).unwrap();
        }
    }

    /// Launches the task asking Claude for `scenario` and runs it until the user is asked.
    fn asked(&self, scenario: &str) -> (Driver, String) {
        let mut driver = self.launch_with(scenario);
        self.until(&mut driver, |slice| !Slice::open_decision_cards(&slice.domain).is_empty());
        let card = Slice::open_decision_cards(&self.domain).remove(0);
        (driver, card)
    }

    fn answer(&self, card: &str, option: &str, note: Option<&str>) -> Result<CommandOutcome, StoreError> {
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
    fn rest(&self, driver: &mut Driver) {
        while driver.pending_controls() > 0 || driver.pending_permits() > 0 || driver.waits_for_host() {
            driver.step(&|| false).unwrap();
        }
        assert_eq!(Slice::lifecycle(&driver.domain).activity(), ActivityView::Resting);
    }

    fn decision_requests(&self) -> Vec<(i64, String)> {
        let observer = self.observer();
        let mut statement = observer.prepare("SELECT epoch,kind FROM lifecycle_decision_requests WHERE task_id=?1 ORDER BY epoch").unwrap();
        statement.query_map([TASK], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect()
    }
}

/// Claude got an answer, or the user is asked for one.
fn decided_or_asked(slice: &Slice) -> bool {
    !slice.decisions().is_empty() || !Slice::decision_cards(&slice.domain).is_empty()
}

/// The response the SDK gave Claude for its one decision, as the fake Claude received it.
fn only_response(slice: &Slice) -> serde_json::Value {
    let decisions = slice.decisions();
    assert_eq!(decisions.len(), 1, "Claude got one answer: {decisions:?}");
    assert_eq!(decisions[0]["cancelled"], false, "{decisions:?}");
    decisions[0]["response"].clone()
}

// D1: Claude asks to run a command. Its card and kept request commit with the host's event, and
// readers see them before the host is acknowledged; the user allows it, the command Claude asked
// for is what runs, the request settles and no decision card is left waiting.
#[test]
fn a_permission_card_commits_before_the_ack_and_allowing_it_runs_the_command_claude_asked_for() {
    let slice = Slice::new();
    let seen = Arc::new(Mutex::new(vec![]));
    let (recorded, database, domain) = (Arc::clone(&seen), slice.database.clone(), Arc::downgrade(&slice.domain));
    slice.on_acknowledge(move || {
        let observer = Connection::open(&database).unwrap();
        let count = |sql: &str| observer.query_row(sql, [TASK], |row| row.get::<_, i64>(0)).unwrap();
        let asked = count("SELECT count(*) FROM provider_observations WHERE task_id=?1 AND CAST(bytes AS TEXT) LIKE '%decision_requested%'");
        let kept = count("SELECT count(*) FROM lifecycle_decision_requests WHERE task_id=?1");
        let visible = !Slice::decision_cards(&domain.upgrade().unwrap()).is_empty();
        recorded.lock().unwrap().push((asked, kept, visible));
    });
    let (mut driver, card) = slice.asked("decide:bash make test");
    let acknowledged = seen.lock().unwrap().clone();
    assert!(acknowledged.iter().any(|(asked, _, _)| *asked == 1), "the request was acknowledged: {acknowledged:?}");
    for (asked, kept, visible) in &acknowledged {
        if *asked == 1 {
            assert!(*kept == 1 && *visible, "acknowledged before its request was kept and its card published: {acknowledged:?}");
        }
    }
    assert_eq!(slice.options(&card), [Hold::ALLOW, Hold::DENY], "no «allow always» outside a repo");
    assert!(slice.decisions().is_empty(), "Claude waits for the user");
    assert!(slice.answer(&card, Hold::ALLOW, None).unwrap().changed);
    assert_eq!(slice.control_rows()[0].0, "respond", "the answer commits its response");
    slice.rest(&mut driver);
    let response = only_response(&slice);
    assert_eq!(response, serde_json::json!({"behavior": "allow", "toolUseID": "toolu_fixture_1", "updatedInput": {"command": "make test"}}), "the command Claude asked for runs");
    assert_eq!(slice.control_rows()[0].2, "handed");
    assert!(Slice::open_decision_cards(&slice.domain).is_empty());
    assert_eq!(slice.decision_requests().into_iter().map(|(_, kind)| kind).collect::<Vec<_>>(), ["permission"]);
    let cards = Slice::read_cards(&slice.domain);
    assert_eq!(cards.len(), 1, "the turn ended with its one card: {cards:?}");
    driver.stop().unwrap();
}

// D2: the user denies the command with a note: Claude is denied with the user's words.
#[test]
fn a_denied_command_reaches_claude_with_the_users_words() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:bash rm -rf build");
    slice.answer(&card, Hold::DENY, Some("Keep the build folder")).unwrap();
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice), serde_json::json!({"behavior": "deny", "toolUseID": "toolu_fixture_1", "message": "The user denied it: Keep the build folder"}));
    assert!(Slice::open_decision_cards(&slice.domain).is_empty());
    driver.stop().unwrap();
}

/// Makes the task's worktree a repo of its own, whose settings can keep «allow always».
fn repo(slice: &Slice) {
    let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&slice.worktree).args(args).output().unwrap().status.success(), "{args:?}");
    git(&["init", "-q", "-b", "main"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "first"]);
}

// D3: «Permitir siempre» on a command of a repo: Claude runs it, and the rule is written into the
// repo's local settings and remembered as active.
#[test]
fn allowing_a_command_always_writes_its_rule_for_the_repo_and_runs_it() {
    let slice = Slice::new();
    repo(&slice);
    let (mut driver, card) = slice.asked("decide:bash make build");
    assert_eq!(slice.options(&card), [Hold::ALLOW, Hold::ALWAYS, Hold::DENY]);
    slice.answer(&card, Hold::ALWAYS, None).unwrap();
    let settings = std::fs::canonicalize(&slice.worktree).unwrap().join(crate::allow_always::SETTINGS);
    assert_eq!(crate::allow_always::allowed(&settings).unwrap(), ["Bash(make build *)"]);
    let rules = slice.domain.store.lock().unwrap().allowed_rules().unwrap();
    assert_eq!(rules.iter().map(|rule| (rule.rule.as_str(), rule.state)).collect::<Vec<_>>(), [("make build *", crate::allow_always::RuleState::Active)]);
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice), serde_json::json!({"behavior": "allow", "toolUseID": "toolu_fixture_1", "updatedInput": {"command": "make build"}}));
    driver.stop().unwrap();
}

// D4: Claude asks one question: the option the user chose reaches Claude under the question's text.
#[test]
fn a_question_is_answered_to_claude_by_its_text() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:question");
    assert_eq!(slice.options(&card), ["o0", "o1"]);
    slice.answer(&card, "o1", None).unwrap();
    slice.rest(&mut driver);
    let response = only_response(&slice);
    assert_eq!(response["behavior"], "allow");
    assert_eq!(response["updatedInput"]["answers"], serde_json::json!({"Which fixture color?": "Blue"}));
    assert_eq!(response["updatedInput"]["questions"].as_array().unwrap().len(), 1);
    driver.stop().unwrap();
}

// D5: Claude asks two questions together: a card each, answered in either order, and only once
// both are answered do the answers reach Claude, each under its question's text.
#[test]
fn two_questions_reach_claude_together_once_the_last_is_answered() {
    for order in [[0, 1], [1, 0]] {
        let slice = Slice::new();
        let (mut driver, _) = slice.asked("decide:questions");
        let mut open = Slice::open_decision_cards(&slice.domain);
        open.sort();
        assert_eq!(open.len(), 2, "{open:?}");
        assert!(open[0].ends_with("-q1") && open[1].ends_with("-q2"), "{open:?}");
        let option = |at: usize| ["o0", "o1"][at];
        slice.answer(&open[order[0]], option(order[0]), None).unwrap_or_else(|error| panic!("answering {} first: {error:?}", open[order[0]]));
        assert!(slice.control_rows().is_empty(), "nothing reaches Claude with one question still open");
        slice.answer(&open[order[1]], option(order[1]), None).unwrap();
        slice.rest(&mut driver);
        let response = only_response(&slice);
        assert_eq!(response["updatedInput"]["answers"], serde_json::json!({"Which fixture color?": "Red", "Which fixture sizes?": "Large"}), "{order:?}");
        driver.stop().unwrap();
    }
}

// D6: Claude Code asks to leave plan mode with the plan file Claude wrote; its Review card shows
// that plan, and once it is approved Claude goes on with exactly what it asked with, in auto mode.
#[test]
fn an_approved_plan_lets_claude_go_on_with_it() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:plan");
    assert_eq!(slice.options(&card), [Hold::APPROVE, Hold::REQUEST_CHANGES]);
    let shown = slice.domain.state().unwrap().interactions[&InteractionId(card.clone())].shown_content().map(|content| content.body.clone());
    assert!(shown.as_deref().is_some_and(|plan| plan.contains("- [ ] Change the fixture")), "the card shows the plan: {shown:?}");
    slice.answer(&card, Hold::APPROVE, None).unwrap();
    slice.rest(&mut driver);
    let plan_file = format!("{}.plan.md", slice.capture.display());
    assert_eq!(only_response(&slice), serde_json::json!({"behavior": "allow", "toolUseID": "toolu_fixture_1",
        "updatedInput": {"plan": "# Fixture plan\n\n- [ ] Change the fixture\n- [ ] Test the fixture", "planFilePath": plan_file},
        "updatedPermissions": [{"type": "setMode", "mode": "auto", "destination": "session"}]}));
    driver.stop().unwrap();
}

// D16: Claude asks to leave plan mode before it wrote its plan file, so Claude Code sends no plan:
// Claude is told to write it first, and the user gets no card with nothing to review.
#[test]
fn a_plan_claude_never_wrote_sends_it_back_to_write_one_without_a_card() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("decide:unwritten-plan");
    slice.until(&mut driver, decided_or_asked);
    assert!(Slice::decision_cards(&slice.domain).is_empty(), "no card");
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice), serde_json::json!({"behavior": "deny", "toolUseID": "toolu_fixture_1", "message": alondra_core::domain::UNWRITTEN_PLAN}));
    assert!(Slice::decision_cards(&slice.domain).is_empty(), "no card");
    driver.stop().unwrap();
}

/// Tells the node `text` as the user does from a client.
fn tell(slice: &Slice, driver: &Driver, text: &str) {
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": text})).unwrap();
    tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "David", driver.agent(), message, true))
        .unwrap();
}

// The node of 2026-10-21: its turn ended, a subagent's report started a turn of Claude's own, and
// while what the user told it waited behind that turn Claude asked to leave plan mode with 41
// points. Alondra refuses the plan for its length and Claude gets why as the tool's result; it
// shortens the plan, the user approves it on its card, and what the user told it reaches Claude.
#[test]
fn a_plan_refused_in_a_turn_claude_started_by_itself_reaches_claude_and_its_shorter_plan_is_reviewed() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("own-turn-plan");
    slice.settle(&mut driver);
    tell(&slice, &driver, "Submit your plan");
    slice.until(&mut driver, |slice| !Slice::open_decision_cards(&slice.domain).is_empty());
    let decisions = slice.decisions();
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0]["response"]["behavior"], "deny", "{decisions:?}");
    assert!(decisions[0]["response"]["message"].as_str().is_some_and(|message| message.contains("at most 40 points")), "{decisions:?}");
    let card = Slice::open_decision_cards(&slice.domain).remove(0);
    let shown = slice.domain.state().unwrap().interactions[&InteractionId(card.clone())].shown_content().map(|content| content.body.clone());
    assert!(shown.as_deref().is_some_and(|plan| plan.contains("- [ ] Change the fixture")), "the card shows the shorter plan: {shown:?}");
    slice.answer(&card, Hold::APPROVE, None).unwrap();
    slice.rest(&mut driver);
    assert_eq!(slice.decisions()[1]["response"]["behavior"], "allow");
    assert!(slice.prompts().last().is_some_and(|prompt| prompt.ends_with("Submit your plan")), "{:?}", slice.prompts());
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty(), "nothing is left waiting");
    driver.stop().unwrap();
}

// Alondra cannot record Claude's request as a card: Claude is denied it with why, as a decision
// of Alondra's own, and goes on; the node keeps its host and nobody is asked.
#[test]
fn a_request_alondra_cannot_ask_is_denied_to_claude_with_why_and_the_node_goes_on() {
    let slice = Slice::new();
    crate::domain_store::refuse_next(crate::domain_store::Refuse::DecisionRequest);
    let mut driver = slice.launch_with("decide:question");
    slice.until(&mut driver, decided_or_asked);
    slice.rest(&mut driver);
    let response = only_response(&slice);
    assert_eq!(response["behavior"], "deny", "{response}");
    let message = response["message"].as_str().unwrap_or_default();
    assert_eq!(message, format!("{}: the test refuses it", alondra_core::domain::UNASKABLE));
    assert!(Slice::decision_cards(&slice.domain).is_empty(), "no card");
    assert_eq!(Slice::read_cards(&slice.domain).len(), 1, "its turn ended with its one card");
    carry_on(&slice.domain, "Ask it again");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert!(slice.prompts().last().is_some_and(|prompt| prompt.ends_with("Ask it again")), "the same Claude goes on");
    driver.stop().unwrap();
}

// D7: the user asks for changes to the plan: Claude is told so with the user's words.
#[test]
fn changes_requested_to_a_plan_reach_claude_as_its_denial() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:plan");
    slice.answer(&card, Hold::REQUEST_CHANGES, Some("Test first")).unwrap();
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice), serde_json::json!({"behavior": "deny", "toolUseID": "toolu_fixture_1", "message": "The user requests changes to the plan: Test first"}));
    driver.stop().unwrap();
}

// D8: an MCP server asks for input: the user's accepted answer reaches it as its form's values.
#[test]
fn an_accepted_request_for_input_reaches_its_server_as_the_forms_values() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:elicitation");
    assert_eq!(slice.options(&card), [Hold::ACCEPT, Hold::DECLINE]);
    slice.answer(&card, Hold::ACCEPT, Some("Ada")).unwrap();
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice), serde_json::json!({"action": "accept", "content": {"name": "Ada"}}));
    driver.stop().unwrap();
}

// D9: the user declines an MCP server's request for input: the server is told it was declined.
#[test]
fn a_declined_request_for_input_reaches_its_server_declined() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:elicitation");
    slice.answer(&card, Hold::DECLINE, None).unwrap();
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice), serde_json::json!({"action": "decline"}));
    driver.stop().unwrap();
}

// D10: Claude calls Alondra's own tool, served by the Alondra plugin: it runs without a card.
#[test]
fn alondras_own_tool_runs_without_asking_the_user() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("decide:alondra");
    slice.until(&mut driver, decided_or_asked);
    assert!(Slice::decision_cards(&slice.domain).is_empty(), "no card");
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice), serde_json::json!({"behavior": "allow", "toolUseID": "toolu_fixture_1", "updatedInput": {"need": "Fixture need"}}));
    assert!(Slice::decision_cards(&slice.domain).is_empty(), "no card");
    driver.stop().unwrap();
}

// D11: a tool that only takes the name of Alondra's, from a server the user configured, is asked.
#[test]
fn a_tool_that_only_takes_alondras_name_is_put_to_the_user() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:imitation");
    assert_eq!(slice.options(&card), [Hold::ALLOW, Hold::DENY]);
    assert!(slice.decisions().is_empty(), "nothing was allowed by its name");
    slice.answer(&card, Hold::DENY, None).unwrap();
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice)["behavior"], "deny");
    driver.stop().unwrap();
}

// D12: what nodes may never do is denied by Alondra's own rule even when it reaches Claude's
// permission callback: Claude is told why, and the user is not asked.
#[test]
fn a_node_running_a_user_only_command_is_denied_without_a_card() {
    let slice = Slice::new();
    let mut driver = slice.launch_with(&format!("decide:bash {} 55 yes", ["alondra", "answer"].join(" ")));
    slice.until(&mut driver, decided_or_asked);
    assert!(Slice::decision_cards(&slice.domain).is_empty(), "no card");
    slice.rest(&mut driver);
    let response = only_response(&slice);
    assert_eq!(response["behavior"], "deny", "{response}");
    assert!(response["message"].as_str().unwrap().starts_with("Nodes cannot run"), "{response}");
    assert!(Slice::decision_cards(&slice.domain).is_empty(), "no card");
    driver.stop().unwrap();
}

// D13: the user interrupts while Claude waits on its card: the request is cancelled, Claude is
// denied, the card stops waiting and an answer to it is refused; the task waits on one card that
// says how it goes on.
#[test]
fn an_interrupt_cancels_the_decision_withdraws_its_card_and_refuses_a_late_answer() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:bash make test");
    slice.run(&user(), "interrupt", Command::InterruptTurn { agent_id: driver.agent().clone() });
    slice.rest(&mut driver);
    let decisions = slice.decisions();
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!((decisions[0]["cancelled"].as_bool(), decisions[0]["response"]["behavior"].as_str()), (Some(true), Some("deny")));
    assert_eq!(Slice::decision_cards(&slice.domain), [DecisionCard { id: card.clone(), answered: false, withdrawn: true }]);
    let late = slice.answer(&card, Hold::ALLOW, None).unwrap_err();
    assert!(matches!(late, StoreError::Domain(_)), "{late:?}");
    assert!(slice.control_rows().iter().all(|(kind, _, _)| kind != "respond"), "nothing was staged for Claude");
    let state = slice.domain.state().unwrap();
    let waiting: Vec<&str> = state.interactions.values()
        .filter(|card| matches!(&card.scope, Scope::Task { task_id } if task_id.0 == TASK) && !card.withdrawn
            && matches!(&card.payload, InteractionPayload::Question { answer: None, .. }))
        .map(|card| card.id.0.as_str())
        .collect();
    assert_eq!(waiting.len(), 1, "one card says how the task goes on: {waiting:?}");
    driver.stop().unwrap();
}

// D14: the daemon dies after the user's answer committed and its response was recorded, before the
// host got it. The adopting owner sends it once: Claude gets the answer a single time.
#[test]
fn an_answer_the_host_never_got_is_sent_once_by_the_adopting_owner() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:bash make test");
    slice.answer(&card, Hold::ALLOW, None).unwrap();
    assert!(driver.claim_and_record_control_without_sending(), "the owner recorded the response");
    drop(driver);
    assert!(slice.decisions().is_empty(), "the host never got it");
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(slice.control_rows()[0].2, "handed", "reconciled with the host's answer");
    slice.rest(&mut adopted);
    assert_eq!(only_response(&slice), serde_json::json!({"behavior": "allow", "toolUseID": "toolu_fixture_1", "updatedInput": {"command": "make test"}}));
    assert!(Slice::open_decision_cards(&restarted).is_empty());
    adopted.stop().unwrap();
}

// D15: a response for a request Claude no longer waits on. The user interrupts, then answers the
// card; the interrupt reaches the host first and cancels the request there, and once the host has
// news the owner sends the recorded answer, which names the request's epoch: the host refuses it
// as stale, the response is recorded rejected and Claude got only the cancellation.
#[test]
fn a_response_to_a_request_claude_no_longer_waits_on_is_rejected_and_never_reaches_claude() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:bash make test");
    slice.run(&user(), "interrupt", Command::InterruptTurn { agent_id: driver.agent().clone() });
    slice.answer(&card, Hold::ALLOW, None).unwrap();
    assert_eq!(slice.control_rows().into_iter().map(|(kind, _, status)| (kind, status)).collect::<Vec<_>>(),
        [("interrupt".to_owned(), "prepared".to_owned()), ("respond".to_owned(), "prepared".to_owned())]);
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"]);
    while !driver.worker.readable(Duration::from_secs(1)).unwrap() {}
    let respond = |slice: &Slice| slice.control_rows().into_iter().find(|(kind, _, _)| kind == "respond").unwrap();
    while respond(&slice).2 == "prepared" {
        driver.step(&|| false).unwrap();
    }
    let respond = respond(&slice);
    assert_eq!(respond.2, "rejected", "the host refused the stale response");
    let epoch = slice.decision_requests()[0].0;
    let bytes: Vec<u8> = slice.observer().query_row("SELECT c.bytes FROM provider_control_commands c JOIN lifecycle_controls l ON l.id=c.control_id WHERE l.task_id=?1 AND l.kind='respond'", [TASK], |row| row.get(0)).unwrap();
    let recorded: serde_json::Value = serde_json::from_slice(&bytes[4..]).unwrap();
    assert_eq!(recorded.pointer("/body/action/epoch").and_then(serde_json::Value::as_i64), Some(epoch), "the response names its request's epoch: {recorded}");
    slice.rest(&mut driver);
    let decisions = slice.decisions();
    assert_eq!(decisions.len(), 1, "Claude got only the cancellation: {decisions:?}");
    assert_eq!((decisions[0]["cancelled"].as_bool(), decisions[0]["response"]["behavior"].as_str()), (Some(true), Some("deny")));
    driver.stop().unwrap();
}

/// The user cancels the task from its open turn-end card, through the action path the clients use.
fn cancel_from_turn_end_card(slice: &Slice) -> serde_json::Value {
    let (card, _) = Slice::read_cards(&slice.domain).into_iter().find(|(_, answered)| !answered).expect("an open turn-end card");
    let state = slice.domain.state().unwrap();
    let action = alondra_core::views::action_id::ActionId::new(alondra_core::views::action_id::Verb::Ans, &card, state.revision, None).unwrap();
    let request = crate::views::actions::ActionRequest {
        protocol: alondra_core::views::VIEWS_PROTOCOL,
        command_id: "5d2c8e41-7a93-4f06-b1e8-3c9a0d4f6e27".into(),
        data_epoch: state.data_epoch.0.clone(),
        client: "tui".into(),
        lang: "en".into(),
        action_id: action.to_string(),
        option_id: Some(alondra_core::domain::CANCEL_TASK.into()),
        input: Default::default(),
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
        crate::views::actions::execute_action(&slice.domain, &client, &slice._home, "test-device", request, None, || Ok(())).await
    }).unwrap_or_else(|error| panic!("the cancellation failed in the store: {error:#}"))
}

/// The task is cancelled from its turn-end card, which stays answered with that choice.
fn assert_cancelled_from_its_card(slice: &Slice, reply: &serde_json::Value) {
    assert_eq!(reply["outcome"], "applied", "{reply}");
    let state = slice.domain.state().unwrap();
    assert!(state.tasks[&TaskId::from(TASK)].cancelled(), "the task is cancelled");
    let cards: Vec<_> = state.interactions.values()
        .filter(|card| card.id.0.starts_with(&format!("{TURN_ENDED_PREFIX}{TASK}-")))
        .map(|card| (matches!(&card.payload, InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == alondra_core::domain::CANCEL_TASK), card.withdrawn))
        .collect();
    assert!(cards.contains(&(true, false)), "the card keeps the user's choice: {cards:?}");
}

#[test]
fn cancelling_from_the_turn_end_card_of_an_sdk_task_ends_the_task() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let reply = cancel_from_turn_end_card(&slice);
    assert_cancelled_from_its_card(&slice, &reply);
    driver.stop().unwrap();
}

#[test]
fn cancelling_from_the_turn_end_card_after_an_interrupt_ends_the_task() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    assert_eq!(press_interrupt(&slice, driver.agent())["outcome"], "applied");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(slice.control_rows()[0].2, "handed");
    let reply = cancel_from_turn_end_card(&slice);
    assert_cancelled_from_its_card(&slice, &reply);
    driver.stop().unwrap();
}

#[test]
fn cancelling_from_the_turn_end_card_after_an_approved_plan_ends_the_task() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:plan");
    slice.answer(&card, Hold::APPROVE, None).unwrap();
    slice.rest(&mut driver);
    assert_eq!(slice.control_rows()[0].2, "handed");
    let reply = cancel_from_turn_end_card(&slice);
    assert_cancelled_from_its_card(&slice, &reply);
    driver.stop().unwrap();
}

impl Slice {
    /// Launches task `task` of the request told `prompt`, and reads its host until `done`.
    fn launch_until(&self, task: &str, prompt: &str, done: impl Fn(&DomainState, &AgentId) -> bool) -> Driver {
        let mut driver = start_bound(Arc::clone(&self.domain), &self.backend, Launch {
            task: task.into(), name: format!("Work on {task}"), directory: self.worktree.to_str().unwrap().into(), prompt: prompt.into(),
            plugin: self.plugin.clone(), options: LaunchOptions::default(), model: None, account: AccountId::default_account(),
        }, capabilities(), None).unwrap();
        while !done(&self.domain.state().unwrap(), driver.agent()) {
            assert!(driver.pending_controls() > 0 || driver.pending_permits() > 0 || driver.waits_for_host(),
                "{task} has nothing left to run: {:?}", self.domain.state().unwrap().node_phase(driver.agent()));
            driver.step(&|| false).unwrap();
        }
        driver
    }
}

/// The nodes of 2026-10-22: five nodes of the Agent SDK wait inside their turn for the user to
/// approve a plan or answer a question, one works, and a High task waits in the queue with a limit
/// of 6. They hold no slot, so it starts, and the gate, the log's "waiting on the user" and the
/// apps' count agree on who works. A plan approved once every slot is taken again goes on at once
/// over the limit, as an answer never passes the gate; nothing more starts until the count is
/// back under it.
#[test]
fn nodes_waiting_on_the_user_inside_their_turn_leave_their_slots_to_the_queue() {
    use crate::admission::{Hold as Held, Key, Limit, Tuning, Waiting};
    let slice = Slice::new();
    let gate = &slice.domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    let task_of = |state: &DomainState, agent: &AgentId| state.tasks.values().find(|task| task.agent_id.as_ref() == Some(agent)).map(|task| task.id.clone());
    let activity = move |state: &DomainState, agent: &AgentId| task_of(state, agent)
        .and_then(|task| state.lifecycle_of(&task).ok()).map(|lifecycle| (lifecycle.activity(), !lifecycle.cards().is_empty()));
    let waits_on_user = move |state: &DomainState, agent: &AgentId| activity(state, agent) == Some((ActivityView::Working, true));
    let works = move |state: &DomainState, agent: &AgentId| activity(state, agent) == Some((ActivityView::Working, false));
    let mut drivers = vec![];
    for (task, prompt) in [("p1", "decide:plan"), ("p2", "decide:plan"), ("p3", "decide:plan"), ("q1", "decide:question"), ("q2", "decide:question")] {
        slice.task(task);
        drivers.push(slice.launch_until(task, prompt, waits_on_user));
    }
    slice.task("busy");
    drivers.push(slice.launch_until("busy", "consume-and-hold", works));
    let state = slice.domain.state().unwrap();
    for driver in &drivers[..5] {
        assert!(matches!(state.node_phase(driver.agent()), Some(NodePhase::WaitingUser { .. })), "the log lists it as waiting on the user");
    }
    assert_eq!(crate::admission::working_tasks(&state), BTreeSet::from([TaskId::from("busy")]), "only busy works");
    let now = alondra_core::now_millis();
    assert_eq!(gate.snapshot(&slice.domain, now).unwrap().working, 1);
    assert_eq!(gate.live(&slice.domain, now).unwrap().working, 1, "the apps show the gate's count");

    slice.task("high");
    slice.run(&user(), "priority-high", Command::SetPriority { task_id: "high".into(), priority: TaskPriority::High });
    let high = Waiting { key: Key::Task("high".into()), priority: TaskPriority::High, resuming: false, since: now };
    assert_eq!(gate.ask(&slice.domain, high, now), Ok(()), "the queued High task starts");
    drivers.push(slice.launch_until("high", "consume-and-hold", works));

    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(2)) });
    let plan = slice.domain.state().unwrap().lifecycle_of(&"p1".into()).unwrap().cards()[0].0.clone();
    slice.answer(&plan.0, Hold::APPROVE, None).unwrap();
    let state = slice.domain.state().unwrap();
    assert!(crate::admission::working_tasks(&state).contains(&TaskId::from("p1")), "approved, p1 goes on without waiting for a slot");
    slice.task("next");
    let next = Waiting { key: Key::Task("next".into()), priority: TaskPriority::High, resuming: false, since: now };
    assert_eq!(gate.ask(&slice.domain, next, now), Err(Held::Slots { working: 3, limit: 2 }), "nothing more starts while 3 of 2 work");
    let p1 = &mut drivers[0];
    while !slice.decisions().iter().any(|decision| decision["response"]["behavior"] == "allow") {
        p1.step(&|| false).unwrap();
    }
    for mut driver in drivers {
        driver.stop().unwrap();
    }
}

/// The owner of a node that just started, held on the first thing its host says until every
/// process of that host has exited: it is in the middle of reading its host when the host ends.
fn owner_reading_when_its_host_ends(slice: &Slice) -> (AgentId, std::sync::mpsc::Receiver<String>, Vec<alondra_process_group::Member>) {
    let driver = slice.launch_with("consume-and-hold");
    let agent = driver.agent().clone();
    let processes = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let mut exits = alondra_process_group::Exits::new().unwrap();
    for process in &processes {
        exits.watch(process).unwrap();
    }
    let (held, holding) = std::sync::mpsc::channel();
    let mut hold = Some((exits, held));
    slice.on_acknowledge(move || {
        if let Some((mut exits, held)) = hold.take() {
            held.send(()).unwrap();
            exits.wait().unwrap();
        }
    });
    let failures = slice.domain.owner_failures();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    holding.recv().unwrap();
    (agent, failures, processes)
}

/// Alondra ended the host itself: its owner reported no failure, no card asks the user, no host
/// took its place and none of its processes is left.
fn assert_ended_quietly(slice: &Slice, agent: &AgentId, failures: &std::sync::mpsc::Receiver<String>, processes: &[alondra_process_group::Member]) {
    assert_eq!(failures.try_iter().collect::<Vec<_>>(), Vec::<String>::new(), "the owner reported the end it caused as a failure");
    assert!(open_card(&slice.domain, HOST_LOST_PREFIX).is_none(), "no card asks to try again");
    assert_eq!(slice.starts().len(), 1, "no host resumed the session: {:?}", slice.starts());
    assert!(!slice.domain.owns_sdk_host(agent));
    let left: Vec<String> = processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "the host's processes survived: {left:?}");
}

fn deliver_and_accept(slice: &Slice, agent: &AgentId, targets: Vec<DeliveryTarget>) {
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    slice.run(&user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets });
}

async fn close_pass(slice: &Slice) {
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
}

// 22 October: the user accepted a task and Alondra closed its node; the node's owner was reading
// its host when the host ended and logged «the SDK owner of … stopped: the connection to the SDK
// host failed». Alondra ended that host itself: its owner stops as a closed execution, quietly.
#[tokio::test(flavor = "multi_thread")]
async fn closing_a_done_task_ends_its_host_with_no_failure_of_its_owner() {
    let slice = Slice::new();
    let (agent, failures, processes) = owner_reading_when_its_host_ends(&slice);
    deliver_and_accept(&slice, &agent, vec![DeliveryTarget::Text { text: "The notes".into() }]);
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "cleaned", Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });
    close_pass(&slice).await;
    let task = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].clone();
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Done, Some(NodeEnd::Closed)));
    assert_ended_quietly(&slice, &agent, &failures, &processes);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_task_ends_its_host_with_no_failure_of_its_owner() {
    let slice = Slice::new();
    let (agent, failures, processes) = owner_reading_when_its_host_ends(&slice);
    slice.run(&user(), "cancel", Command::CancelTask { task_id: TASK.into() });
    close_pass(&slice).await;
    let task = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].clone();
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Cancelled, Some(NodeEnd::Closed)));
    assert_ended_quietly(&slice, &agent, &failures, &processes);
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_task_whose_pr_merged_ends_its_host_with_no_failure_of_its_owner() {
    let slice = Slice::new();
    let (agent, failures, processes) = owner_reading_when_its_host_ends(&slice);
    let node = Actor::Agent { agent_id: agent.clone() };
    slice.run(&node, "register", Command::RegisterPr {
        task_id: TASK.into(), pr_id: "pr-notes".into(), host: "github.example.invalid".into(), repository: "synthetic/notes".into(),
        number: 7, head: "head-a".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    deliver_and_accept(&slice, &agent, vec![DeliveryTarget::PullRequest { pr_id: "pr-notes".into(), head: "head-a".into() }]);
    slice.run(&Actor::System, "merged", Command::ObservePrState {
        pr_id: "pr-notes".into(), head: "head-a".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head-a".into()),
    });
    slice.run(&node, "cleaned", Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });
    close_pass(&slice).await;
    let task = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].clone();
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Done, Some(NodeEnd::Closed)));
    assert_ended_quietly(&slice, &agent, &failures, &processes);
}

// The orchestrator's replace and a node's move to another host stop the old host through its owner.
#[test]
fn replacing_a_node_ends_its_host_with_no_failure_of_its_owner() {
    let slice = Slice::new();
    let (agent, failures, processes) = owner_reading_when_its_host_ends(&slice);
    slice.domain.stop_sdk_host(&agent).unwrap();
    assert_ended_quietly(&slice, &agent, &failures, &processes);
}

// A daemon that exits for its upgrade stops every host it owns.
#[test]
fn a_daemon_that_stops_ends_its_hosts_with_no_failure_of_their_owners() {
    let slice = Slice::new();
    let (agent, failures, processes) = owner_reading_when_its_host_ends(&slice);
    slice.domain.stop_sdk_owners();
    assert_ended_quietly(&slice, &agent, &failures, &processes);
}

// A host that dies by itself while its task is open is a failure: its owner says so and, with no
// host to take its place in this daemon, the task asks the user on its card.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_dies_while_its_task_is_open_is_a_failure_of_its_owner() {
    let slice = Slice::new();
    let driver = slice.launch_working();
    let group = driver.worker.host_group().unwrap();
    let failures = slice.domain.owner_failures();
    let mut changed = slice.domain.subscribe();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    assert!(std::process::Command::new("/bin/kill").args(["-KILL", "--", &format!("-{group}")]).status().unwrap().success());
    let reader = Arc::clone(&slice.domain);
    changed.wait_for(|_| open_card(&reader, HOST_LOST_PREFIX).is_some()).await.unwrap();
    let failure = failures.try_recv().unwrap();
    assert!(failure.starts_with("the SDK host closed") || failure.starts_with("the connection to the SDK host failed"), "{failure}");
}

impl Slice {
    /// The backend a daemon of the release before ran: the same host with other bytes, as the host
    /// of every release is, kept apart from this release's.
    fn release_before(&self) -> SdkBackend {
        use std::io::Write;
        let host = self._home.join("alondra-host-before");
        std::fs::copy(&self.backend.host, &host).unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::OpenOptions::new().append(true).open(&host).unwrap().write_all(b"\n// the release before\n").unwrap();
        let sha256 = backend::digest(&host).unwrap();
        assert_ne!(sha256, self.backend.host_sha256);
        SdkBackend { node: host.clone(), node_sha256: sha256.clone(), host, host_sha256: sha256, ..self.backend.clone() }
    }

    /// The command Claude left running in the background (`background-command`).
    fn background_command(&self) -> alondra_process_group::Member {
        let pid: i32 = std::fs::read_to_string(format!("{}.background", self.capture.display())).unwrap().parse().unwrap();
        alondra_process_group::members(alondra_process_group::group(pid).unwrap().unwrap()).unwrap()
            .into_iter().find(|member| member.pid == pid).expect("the background command runs")
    }

    /// Ends the command Claude left running in the background.
    fn release_background_command(&self) {
        std::fs::write(format!("{}.release", self.capture.display()), "done\n").unwrap();
    }

    /// The cards of the task that are not how a turn ended, open or not.
    fn other_cards(domain: &DomainRuntime) -> Vec<String> {
        domain.read().state().interactions.keys()
            .filter(|card| card.0.contains(TASK) && !card.0.starts_with(&format!("{TURN_ENDED_PREFIX}{TASK}-")))
            .map(|card| card.0.clone())
            .collect()
    }
}

// Production, 2026-10-22 20:46: daemon 0.3.5 refused every live host 0.3.4 had launched, because
// its executable was not its own, and ended each with its process group: a background bazel test
// died with exit 143. A daemon of the next release adopts the host of the release before as it
// runs, so the command Claude left running goes on; once the command ended and Claude took up its
// notification, the node rests, and a host of this release that resumes the same session takes the
// old one's place, with no card. What the user says next reaches that session.
#[tokio::test(flavor = "multi_thread")]
async fn an_upgrade_keeps_the_background_command_of_a_host_of_the_release_before_and_moves_the_node_once_it_rests() {
    let slice = Slice::new();
    let before = slice.release_before();
    let mut driver = slice.launch_with_backend(&before, "background-command", capabilities());
    while lifecycle(&slice.domain).activity() != ActivityView::OwnWork {
        driver.observe().unwrap();
    }
    let command = slice.background_command();
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let old_host = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    // The daemon upgrades: it exits and leaves its hosts running for the next one.
    drop(driver);
    let upgraded = slice.restart();
    upgraded.enable_sdk(slice.backend.clone());
    let (_, adopted) = take_all(&upgraded, &slice.backend).pop().expect("the start takes the execution");
    let adopted = adopted.unwrap_or_else(|Failed(why)| panic!("the execution was not taken over: {why}"));
    assert!(alondra_process_group::alive(&command).unwrap(), "the background command survived the upgrade");
    assert!(adopted.worker.runs_another_release(), "the node goes on on the host of the release before");
    assert_eq!(slice.starts().len(), 1, "no Claude started again");
    let mut changed = upgraded.subscribe();
    let owner = Owner::spawn(adopted);
    slice.release_background_command();
    let reader = Arc::clone(&upgraded);
    changed.wait_for(|_| lifecycle(&reader).activity() == ActivityView::Resting).await.unwrap();
    let rested = Slice::read_cards(&upgraded).len();
    carry_on(&upgraded, "Add the tests first");
    changed.wait_for(|_| Slice::read_cards(&reader).len() > rested).await.unwrap();
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "one more Claude started, on this release's host: {starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "it resumed the same session");
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "the new host's Claude got only what the user said, nothing about a cut: {prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first") && !prompts[1].contains("cut off"), "nothing about a cut: {prompts:?}");
    let left: Vec<String> = old_host.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "the host of the release before is gone: {left:?}");
    assert_eq!(Slice::other_cards(&upgraded), Vec::<String>::new(), "moving the node asked the user nothing");
    upgraded.add_sdk_owner(owner);
    upgraded.stop_sdk_owners();
}

// The same upgrade, with the daemon gone right after it stopped the host of the release before
// and before a host of this release took its place: the next start finds the host ended and puts
// one of this release that resumes the session in its place, and nothing is lost or asked.
#[test]
fn a_daemon_gone_while_it_moved_a_resting_node_to_this_release_leaves_it_to_the_next_start() {
    let slice = Slice::new();
    let before = slice.release_before();
    let mut driver = slice.launch_with_backend(&before, "Write the release notes", capabilities());
    slice.settle(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    drop(driver);
    let upgraded = slice.restart();
    let mut adopted = slice.take_over(&upgraded);
    assert!(adopted.worker.runs_another_release(), "the node goes on on the host of the release before");
    while !adopted.moves_to_this_release {
        adopted.step(&|| false).unwrap();
    }
    adopted.stop().unwrap();
    drop(adopted);
    let started = slice.restart();
    let mut taken = slice.take_over(&started);
    assert!(!taken.worker.runs_another_release(), "the next start launched a host of this release");
    carry_on(&started, "Add the tests first");
    taken.step(&|| false).unwrap();
    slice.settle(&mut taken);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(!prompts[1].contains("cut off"), "its first prompt is what the user said, nothing about a cut: {prompts:?}");
    assert_eq!(Slice::other_cards(&started), Vec::<String>::new(), "nothing was asked");
    taken.stop().unwrap();
}

// A live host this release cannot carry on with (here, one of a release older than the oldest it
// adopts) is ended and replaced by one that resumes the session, as every host was before.
#[test]
fn a_live_host_of_a_release_older_than_the_oldest_adoptable_is_replaced_as_before() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let processes = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let lock = slice.backend.state_root.join(&driver.agent().0).join("host.lock");
    drop(driver);
    let mut bootstrap: serde_json::Value = serde_json::from_slice(&std::fs::read(&lock).unwrap()).unwrap();
    bootstrap["manifest"]["host_release"] = "0.3.4".into();
    std::fs::write(&lock, serde_json::to_vec(&bootstrap).unwrap()).unwrap();
    let restarted = slice.restart();
    let replaced = slice.take_over(&restarted);
    assert!(!replaced.worker.runs_another_release());
    assert_replaced(&slice, &restarted, replaced, &session, &processes);
}
