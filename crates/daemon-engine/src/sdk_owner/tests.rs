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

pub const TASK: &str = "sdk-slice";

fn runfile(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} names a runfile")));
    std::fs::canonicalize(path).unwrap()
}

/// Reads until the test process that started it closes the pipe. It leaves alone only after `done`:
/// a dying process closes its files before it stops being this one's parent, so nothing else tells
/// a test that ended from one that was cut. Then it kills every process still holding a file under
/// the folder, a host with its whole group, waits for each to exit, and removes the folder.
const REAPER: &str = r#"
import os, select, shutil, signal, subprocess, sys
root, owner = sys.argv[1], int(sys.argv[2])
if sys.stdin.buffer.read() == b'done':
    sys.exit(0)
listed = subprocess.run(['/usr/sbin/lsof', '-t', '+D', root], capture_output=True, text=True).stdout.split()
exits, watched = select.kqueue(), 0
for pid in {int(pid) for pid in listed} - {owner}:
    try:
        leads = os.getpgid(pid) == pid
    except ProcessLookupError:
        continue
    members = subprocess.run(['/usr/bin/pgrep', '-g', str(pid)], capture_output=True, text=True).stdout.split() if leads else [pid]
    for member in members:
        try:
            exits.control([select.kevent(int(member), select.KQ_FILTER_PROC, select.KQ_EV_ADD | select.KQ_EV_ONESHOT, select.KQ_NOTE_EXIT)], 0)
            watched += 1
        except ProcessLookupError:
            pass
    try:
        (os.killpg if leads else os.kill)(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
while watched:
    watched -= len(exits.control(None, watched))
shutil.rmtree(os.path.dirname(root), ignore_errors=True)
"#;

/// The folder of a test's SDK hosts: the test owns every host started under it. Dropping it kills
/// each host left with its whole process group, and fails the test that left one running; if the
/// test process dies without unwinding (a timeout, a kill), its reaper kills them instead.
pub struct HostRoot {
    reaper: std::process::Child,
    directory: Directory,
}

impl HostRoot {
    pub fn new() -> Self {
        use std::os::unix::process::CommandExt;
        let directory = Directory::for_unix_socket();
        let reaper = std::process::Command::new("/usr/bin/python3")
            .args(["-I", "-c", REAPER])
            .arg(&*directory)
            .arg(std::process::id().to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
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
        use std::io::Write;
        if let Some(mut pipe) = self.reaper.stdin.take() {
            let _ = pipe.write_all(b"done");
        }
        self.reaper.wait().unwrap();
    }

    /// What a dying test process does first: its pipe closes while it is still the reaper's parent.
    fn close_reaper_pipe(&mut self) {
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
            provider: alondra_core::accounts::Provider::Claude, origin: None,
    };
    (backend, capture)
}

/// A domain whose launches start through the fixture backend: what the native Claude was started
/// with, the prompts it got, and every host of the domain stopped when this is dropped. Declare it
/// before the directories it lives in, so its hosts stop first.
pub struct SdkLaunches {
    domain: Arc<DomainRuntime>,
    pub(crate) capture: PathBuf,
    state_root: PathBuf,
    _state: HostRoot,
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
    pub(crate) fn flag(&self, flag: &str) -> Option<String> {
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
    pub(crate) fn starts(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(format!("{}.starts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    /// What the launch of `agent`'s execution kept beside its host.
    pub(crate) fn options(&self, agent: &str) -> LaunchOptions {
        LaunchOptions::load(&self.state_root.join(agent)).unwrap()
    }

    /// Waits on the domain until `task`'s turns have ended on `turns` cards it rested on.
    pub(crate) async fn until_turns_ended(&self, task: &str, turns: usize) {
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
fn advance(driver: &mut Driver) {
    if driver.pending_permits() > 0 {
        driver.step(&|| false).unwrap();
    } else {
        driver.observe().unwrap();
    }
}

/// Whether `card` is one an ended turn of `task` rests on: the case the orchestrator settles when
/// the node stopped, or the user's card when the turn was interrupted or failed.
pub(crate) fn rests_on(task: &str, card: &InteractionId) -> bool {
    card.0.starts_with(&format!("{STALLED_PREFIX}{task}-")) || card.0.starts_with(&format!("{TURN_ENDED_PREFIX}{task}-"))
}

pub(crate) fn not_wakes(prompts: impl Iterator<Item = String>) -> Vec<String> {
    prompts.filter(|prompt| prompt != alondra_core::domain::WAKE_INSTRUCTION).collect()
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

pub struct Slice {
    pub domain: Arc<DomainRuntime>,
    database: PathBuf,
    capture: PathBuf,
    pub worktree: PathBuf,
    pub plugin: PathBuf,
    pub backend: SdkBackend,
    /// Every raw host event the daemon committed for the task, by cursor, as it was committed.
    pub kept: Arc<Mutex<BTreeMap<u64, Vec<u8>>>>,
    /// Before `_home`: the hosts write in the HOME they get, so they end before it is removed.
    _state: HostRoot,
    pub _home: Directory,
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

pub fn user() -> Actor {
    Actor::User { user_id: "slice-user".into() }
}

fn envelope(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command }
}

impl Slice {
    pub fn new() -> Self {
        Self::with_hosts(HostRoot::new())
    }

    fn with_hosts(state: HostRoot) -> Self { Self::with_database_name(state,"domain.sqlite") }

    pub fn with_database_name(state: HostRoot,name:&str)->Self {
        let home = Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let worktree = home.join("worktree");
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        let plugin = home.join("plugin");
        std::fs::DirBuilder::new().mode(0o700).create(&plugin).unwrap();
        let (backend, capture) = fixture_backend(&home, &state);
        let database = home.join(name);
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

    fn launch(&self) -> Driver {
        self.launch_told("Write the release notes", capabilities())
    }

    pub fn launch_with(&self, prompt: &str) -> Driver {
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
        }, capabilities, None, None)
        .unwrap()
    }

    /// Launches a turn that the fake Claude holds open, and reads the host until it runs.
    pub fn launch_working(&self) -> Driver {
        let mut driver = self.launch_told("consume-and-hold", capabilities());
        while Self::lifecycle(&self.domain).activity() != ActivityView::Working {
            driver.observe().unwrap();
        }
        driver
    }

    pub fn lifecycle(domain: &DomainRuntime) -> alondra_domain::TaskLifecycle {
        let read = domain.read();
        replay(&TASK.into(), &read.state().lifecycle_journal[&TaskId::from(TASK)]).unwrap()
    }

    /// Every control request the fake Claude received, as it got it.
    pub fn controls(&self) -> Vec<serde_json::Value> {
        let path = PathBuf::from(format!("{}.controls", self.capture.display()));
        std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    /// The task's controls in the store: kind, argument and status, in order.
    pub fn control_rows(&self) -> Vec<(String, Option<String>, String)> {
        let observer = self.observer();
        let mut statement = observer.prepare("SELECT kind,argument,status FROM lifecycle_controls WHERE task_id=?1 ORDER BY id").unwrap();
        statement.query_map([TASK], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().map(Result::unwrap).collect()
    }

    fn journal_events(&self) -> i64 {
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
            driver.observe().unwrap();
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
        let path = PathBuf::from(format!("{}.prompts", self.capture.display()));
        not_wakes(std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()))
    }

    fn observer(&self) -> Connection {
        Connection::open(&self.database).unwrap()
    }

    /// Every card an ended turn of the task rested on, in the read the clients get, with whether
    /// it is settled.
    pub fn read_cards(domain: &DomainRuntime) -> Vec<(String, bool)> {
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

    fn stored_cards(&self) -> i64 {
        stored_turn_ends(&self.database)
    }
}

/// The cards ended turns of the task rested on, committed in SQL.
fn stored_turn_ends(database: &Path) -> i64 {
    Connection::open(database).unwrap()
        .query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1 OR id LIKE ?2",
            [format!("{STALLED_PREFIX}{TASK}-%"), format!("{TURN_ENDED_PREFIX}{TASK}-%")], |row| row.get(0))
        .unwrap()
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
fn a_reaper_whose_pipe_closes_while_its_parent_still_lives_ends_the_hosts_and_their_folder() {
    let mut slice = Slice::new();
    let processes = host_left_running(&slice);
    let root = slice._state.to_path_buf();
    slice._state.close_reaper_pipe();
    assert_eq!(survivors(&processes), Vec::<String>::new(), "the reaper killed the host's group");
    assert!(!root.parent().unwrap().exists(), "the reaper removed the hosts' folder");
}

/// The test process `a_test_process_killed_mid_test_leaves_no_host_and_no_folder` kills. It also
/// stands for a test that hangs: left alone, it runs until Bazel's timeout cuts it.
#[test]
#[ignore = "started by a_test_process_killed_mid_test_leaves_no_host_and_no_folder"]
fn a_test_process_holding_a_host() {
    let slice = Slice::new();
    let driver = slice.launch();
    println!("holding {} {}", driver.worker.host_group().unwrap(), slice._state.display());
    loop {
        std::thread::park();
    }
}

#[test]
fn a_test_process_killed_mid_test_leaves_no_host_and_no_folder() {
    use std::io::{BufRead, Read};
    let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "--nocapture", "sdk_owner::tests::a_test_process_holding_a_host"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut errors = owner.stderr.take().unwrap();
    let holding = std::io::BufReader::new(owner.stdout.take().unwrap()).lines().map(Result::unwrap)
        .find_map(|line| line.strip_prefix("holding ").map(str::to_owned)).expect("the test process launched its host");
    let (group, root) = holding.split_once(' ').unwrap();
    let processes = alondra_process_group::members(group.parse().unwrap()).unwrap();
    assert!(!processes.is_empty(), "the host runs");
    owner.kill().unwrap();
    owner.wait().unwrap();
    // The reaper inherits this pipe: it closes when the reaper has finished.
    let mut said = String::new();
    errors.read_to_string(&mut said).unwrap();
    assert_eq!(survivors(&processes), Vec::<String>::new(), "the reaper killed the host's group: {said}");
    assert!(!Path::new(root).parent().unwrap().exists(), "the reaper removed the hosts' folder: {said}");
}

#[test]
fn a_task_launched_through_the_sdk_ends_its_turn_with_one_card_readers_see_before_the_host_is_acknowledged() {
    let slice = Slice::new();
    let acknowledged = watch_acknowledgements(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts(), ["Write the release notes"], "the host's Claude got the launch prompt");
    let cards = Slice::read_cards(&slice.domain);
    assert_eq!(cards.len(), 1, "one card: {cards:?}");
    assert!(cards[0].0.starts_with(STALLED_PREFIX) && !cards[0].1, "the woken node stopped again: its case waits for the orchestrator, never the user: {cards:?}");
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
pub fn node_offers(domain: &DomainRuntime, agent: &AgentId) -> Vec<String> {
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

// A2: the node stopped twice, so its case goes to the orchestrator, never the user. The
// orchestrator's settlement commits with its dispatch prepared, the owner claims and sends it, and
// the host's Claude gets the instruction as written; its next stop is a case of its own at once.
#[test]
fn the_orchestrators_instruction_on_a_stopped_node_commits_and_reaches_the_sdk() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let (case, _) = Slice::read_cards(&slice.domain).pop().unwrap();
    let state = slice.domain.state().unwrap();
    let by_user = Command::AnswerQuestion {
        interaction_id: case.as_str().into(),
        option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Add the tests first".into()))]),
        attachments: vec![],
    };
    let refused = slice.domain.execute_with(&user(), &envelope(&state, "client-carry-on", by_user), None, None, || Ok(()));
    assert!(matches!(&refused, Err(StoreError::Domain(error)) if error.code == "forbidden"), "the user cannot settle the case: {refused:?}");
    let settle = Command::SettleStall { case_id: case.as_str().into(), task_id: TASK.into(), outcome: StallOutcome::Instruct { text: "Add the tests first".into() } };
    let orchestrator = Actor::Orchestrator { generation: state.orchestrator_generation };
    let outcome = slice.domain.execute_with(&orchestrator, &envelope(&state, "orchestrator-instructs", settle), None, None, || Ok(()));
    assert!(outcome.as_ref().is_ok_and(|outcome| outcome.changed), "the orchestrator's settlement is confirmed: {outcome:?}");
    assert_eq!(driver.pending_permits(), 1, "its dispatch reached the owner");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert_eq!(prompts, ["Write the release notes", "Add the tests first"], "it reaches the host's Claude as written");
    let mut cards = Slice::read_cards(&slice.domain);
    cards.sort();
    assert_eq!(cards.len(), 2, "{cards:?}");
    assert_eq!(cards.iter().filter(|(_, settled)| *settled).count(), 1, "the first case is settled, the second waits: {cards:?}");
    instruct(&slice.domain, "Write the changelog too");
    assert_eq!(driver.pending_permits(), 1);
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts().last().map(String::as_str), Some("Write the changelog too"), "it reaches the host's Claude as written");
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Also update the changelog"})).unwrap();
    let told = tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "Alice", driver.agent(), message, true))
        .unwrap();
    assert_eq!((told["delivery_state"].as_str(), told["reaches"].as_str()), (Some("dispatching"), Some("now")), "the resting node starts on it: {told}");
    assert_eq!(driver.pending_permits(), 1, "what the user told the resting node left for its host");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let last = slice.prompts().last().cloned().unwrap();
    assert!(last.starts_with("[From Alice · ") && last.ends_with("Also update the changelog"), "it reaches the host's Claude as a turn of its own, saying who sent it: {last}");
    driver.stop().unwrap();
}

// A tool result of 5 MiB, twice in its SDK message, is kept shrunk with the evidence of what it
// was: its turn ends with its case and the orchestrator's instruction reaches the same Claude.
#[test]
fn a_huge_tool_result_is_kept_shrunk_and_the_node_goes_on() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("huge-tool-result");
    slice.settle(&mut driver);
    assert_eq!(slice.observations_with("\"code\":\"truncated_sdk_message\""), 1, "the tool result is kept, shrunk");
    assert_eq!(Slice::read_cards(&slice.domain).len(), 1, "its turn ended with its card");
    instruct(&slice.domain, "Summarize the log");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert!(prompts.len() == 2 && prompts[1].ends_with("Summarize the log"), "the next message reaches the same Claude: {prompts:?}");
    assert_eq!(Slice::read_cards(&slice.domain).len(), 2);
    driver.stop().unwrap();
}

// A message goes into the running turn when the provider can take it, and starts a turn when the
// turn just closed. What someone tells the node while its turn runs reaches the same native
// turn: the host takes it into its streaming input and Claude consumes it in that turn, which then
// ends with one card.
#[test]
fn a_message_mid_turn_reaches_claude_inside_the_turn() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let turns = |domain: &DomainRuntime| {
        let read = domain.read();
        let journal = &read.state().lifecycle_journal[&TaskId::from(TASK)];
        let wakes: Vec<&String> = journal.iter().filter_map(|record| match record {
            alondra_core::domain::LifecycleRecord::TurnEnded { wake: Some(wake), .. } => Some(wake),
            _ => None,
        }).collect();
        journal.iter().filter(|record| matches!(record, alondra_core::domain::LifecycleRecord::TurnStarted { input, .. } if !wakes.contains(&input))).count()
    };
    assert_eq!(turns(&slice.domain), 1);
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Rebase on main before you push"})).unwrap();
    tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "Alice", driver.agent(), message, true))
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



// A3: the writer stops between the commit of the turn end and its publication. A runtime opened
// on the same database gets the host's unacknowledged event again: the exact replay publishes
// the committed card once and acknowledges it, with no second card.
#[test]
fn a_crash_between_commit_and_publication_reconciles_without_a_second_card() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    let database = slice.database.clone();
    slice.domain.before_owner_publish(move || {
        if stored_turn_ends(&database) > 0 {
            panic!("the daemon stopped after the commit");
        }
    });
    let crash = loop {
        if let Err(crash) = catch_unwind(AssertUnwindSafe(|| advance(&mut driver))) {
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
    while driver.pending_permits() == 0 && driver.waits_for_host() {
        let (_, work) = measured(&slice.domain, || driver.observe().unwrap());
        eprintln!("observation: {work:?}");
        observations.push(work);
    }
    let turn_end = observations.last().unwrap();
    assert_eq!(turn_end.index_builds, 1, "the turn end publishes the wake it sends once");
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
    let state = slice.domain.state().unwrap();
    assert_eq!(state.agents[&agent].phase, NodePhase::ThroughSdk, "no event moves its phase");
    let live = alondra_core::domain::TaskActivity::Live(state.sdk_live(&agent).expect("its task is open")).key();
    let shown = crate::domain_orchestrator_reads::node(&slice.domain, &agent.0).unwrap()["node"].clone();
    assert_eq!((shown["alive"].as_bool(), shown["state"].as_str(), shown["sdk_host"].as_bool()), (Some(true), Some(live), Some(true)), "{shown}");
    assert_eq!(shown["task"]["activity"].as_str(), Some(live), "the node's state is its task's");
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
pub struct NoClaudeSessions;
impl crate::node_close::Sessions for NoClaudeSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> crate::node_close::Answer<'a, Result<crate::node_close::Ran, String>> {
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
            from_user: false, answers: None,
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
    slice.run(&Actor::System, "end-turn", Command::TellNode { task_id: TASK.into(), text: "[From Alondra · 09:00] Thanks".into(), from_user: false, answers: None });
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

/// The orchestrator settles the case the task rests on: its node gets `instruction`.
fn instruct(domain: &DomainRuntime, instruction: &str) {
    let (case, _) = Slice::read_cards(domain).into_iter().find(|(card, settled)| !settled && card.starts_with(STALLED_PREFIX)).expect("an open case");
    let state = domain.state().unwrap();
    let settle = Command::SettleStall { case_id: case.as_str().into(), task_id: TASK.into(), outcome: StallOutcome::Instruct { text: instruction.into() } };
    let orchestrator = Actor::Orchestrator { generation: state.orchestrator_generation };
    assert!(domain.execute_with(&orchestrator, &envelope(&state, &format!("settle-{case}"), settle), None, None, || Ok(())).unwrap().changed);
}

// B1: the daemon crashes with the host still running. A fresh daemon on the same database adopts
// that live host with the next owner generation, and the orchestrator's instruction reaches the same
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
    instruct(&restarted, "Add the tests first");
    adopted.step(&|| false).unwrap();
    slice.settle(&mut adopted);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "the same Claude got the launch and the instruction, no restart: {prompts:?}");
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
pub fn strand_host(slice: &Slice, driver: Driver) -> (String, Vec<alondra_process_group::Member>) {
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let processes = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let folder = slice.backend.state_root.join(&driver.agent().0);
    drop(driver);
    std::fs::remove_file(folder.join("s")).unwrap();
    assert!(processes.iter().all(|process| alondra_process_group::alive(process).unwrap()), "the host still runs");
    (session, processes)
}

/// The stranded host is gone with its whole group, and a host that resumes the same Claude
/// session took the execution: what the orchestrator says next reaches that session.
fn assert_replaced(slice: &Slice, domain: &Arc<DomainRuntime>, mut driver: Driver, session: &str, stranded: &[alondra_process_group::Member]) {
    let left: Vec<String> = stranded.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "the stranded host's processes survived: {left:?}");
    instruct(domain, "Add the tests first");
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
    let driver = driver.unwrap_or_else(|Failed { why, .. }| panic!("the execution was left as it was: {why}"));
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
pub fn until_own_work(slice: &Slice, driver: &mut Driver) {
    while lifecycle(&slice.domain).activity() != ActivityView::OwnWork {
        driver.observe().unwrap();
    }
    assert!(background_commands(slice).into_iter().all(running), "the background command runs");
}

/// The host that replaced the stranded one ended the background command with it, the resumed
/// session is told that command ended and will not report back, and it goes on: its turn ends.
pub fn assert_told_the_work_ended(slice: &Slice, domain: &Arc<DomainRuntime>, mut driver: Driver, session: &str) {
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

/// Ends what a test's background commands left, on success and on failure alike.
struct BackgroundCommands(Vec<alondra_process_group::Member>);

impl BackgroundCommands {
    fn of(slice: &Slice) -> Self {
        Self(background_commands(slice).into_iter().filter_map(|pid| alondra_process_group::process(pid).unwrap()).collect())
    }
    fn running(&self) -> Vec<String> {
        survivors(&self.0)
    }
}

impl Drop for BackgroundCommands {
    fn drop(&mut self) {
        alondra_process_group::end_tree(&self.0, std::time::Duration::from_secs(5)).unwrap();
    }
}

/// A node with a background command in a group of its own, a shell with a child of its own, under
/// an owner that runs by itself, as a daemon's are.
fn node_with_a_background_group(slice: &Slice) -> (AgentId, String, BackgroundCommands, Vec<alondra_process_group::Member>) {
    slice.domain.enable_sdk(slice.backend.clone());
    slice.domain.live.gate.ignore_mac();
    slice.domain.live.gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(6)) });
    let mut driver = slice.launch_with("background-group");
    until_own_work(slice, &mut driver);
    let commands = BackgroundCommands::of(slice);
    assert_eq!(commands.running().len(), 2, "the shell and its child run");
    let host = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let agent = driver.agent().clone();
    let session = slice.starts().last().unwrap()["session"].as_str().unwrap().to_owned();
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    (agent, session, commands, host)
}

fn assert_stopped_whole(slice: &Slice, agent: &AgentId, commands: &BackgroundCommands, host: &[alondra_process_group::Member]) {
    assert_eq!(commands.running(), Vec::<String>::new(), "the background command and its child ended");
    assert_eq!(survivors(host), Vec::<String>::new(), "the host and its Claude ended");
    assert_eq!(slice.starts().len(), 1, "no Claude runs: nothing can wake a turn");
    assert_ne!(lifecycle(&slice.domain).activity(), ActivityView::OwnWork, "nothing waits for the work that ended");
    assert!(slice.domain.owns_sdk_host(agent), "a host waits with no Claude for what the pause holds");
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
}

// 5 November, t-flaky-daemon-engine-under-load: paused at 00:00:37 with a stress script running in
// the background. The script's end woke Claude, a turn no input of Alondra's opens, and it ran the
// script again, three times, until its process ended 13 minutes later. Of 161 pauses by the user
// in the journals, 60 went on like that. A pause stops the host, so nothing is left to wake.
#[tokio::test(flavor = "multi_thread")]
async fn a_pause_by_the_user_stops_the_host_its_background_commands_and_every_turn_until_it_resumes() {
    let slice = Slice::new();
    let (agent, session, commands, host) = node_with_a_background_group(&slice);
    let gate = &slice.domain.live.gate;
    gate.pause_by_user(&slice.domain, &agent.0, alondra_core::now_millis()).unwrap();
    slice.domain.join_retakes();
    assert_stopped_whole(&slice, &agent, &commands, &host);
    assert!(slice.domain.state().unwrap().host_ended_since_pause(&TASK.into()));
    crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    slice.domain.join_retakes();
    assert!(gate.paused(&slice.domain, &agent.0).is_some(), "only the user ends it");
    assert_eq!(slice.starts().len(), 1, "a later look at the pauses starts nothing");

    assert!(gate.resume_now(&slice.domain, &agent.0, alondra_core::now_millis()));
    crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    until_last_prompt_ends_with(&slice.domain, &slice.capture, "run again what was cut.").await;
    slice.domain.stop_sdk_owners();
    assert_eq!(slice.starts().last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session goes on: {:?}", slice.starts());
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "no card asks the user");
}

// Only a pause by the user cuts what a node runs. A build that uses a lot of the disk or the Mac
// would only run again, and the usage limit stops Claude's turns, never its commands.
#[tokio::test(flavor = "multi_thread")]
async fn no_pause_but_the_users_kills_what_the_node_runs() {
    use crate::pauses::Reason;
    for reason in [Reason::Disk, Reason::Declared { reason: "Waiting for a full machine".into() }, Reason::Usage] {
        let slice = Slice::new();
        let (agent, _, commands, host) = node_with_a_background_group(&slice);
        slice.domain.live.gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(0)) });
        assert!(slice.domain.live.gate.hold_back(&slice.domain, &agent.0, reason.clone(), alondra_core::now_millis()));
        crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
        slice.domain.join_retakes();
        assert_eq!(commands.running().len(), 2, "{reason:?}: the background command and its child go on");
        assert_eq!(survivors(&host).len(), host.len(), "{reason:?}: the host and its Claude go on");
        assert_eq!(slice.starts().len(), 1, "{reason:?}: the same Claude goes on");
        slice.domain.stop_sdk_owners();
    }
}

// A daemon that restarts with a node paused by the user adopts its live host: the gate stops it at
// its next look.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_a_restarted_daemon_adopts_for_a_paused_node_stops_at_the_gates_next_look() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("background-group");
    until_own_work(&slice, &mut driver);
    let commands = BackgroundCommands::of(&slice);
    let host = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    let agent = driver.agent().clone();
    assert!(slice.domain.live.gate.hold_back(&slice.domain, &agent.0, crate::pauses::Reason::ByUser, alondra_core::now_millis()));
    drop(driver);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    restarted.live.gate.ignore_mac();
    adopt_all(&restarted, &slice.backend);
    assert_eq!(commands.running().len(), 2, "the adopted host still runs them");
    crate::admission::pass_at(&restarted, &slice._home, alondra_core::now_millis()).await.unwrap();
    restarted.join_retakes();
    assert_eq!(commands.running(), Vec::<String>::new(), "the background command and its child ended");
    assert_eq!(survivors(&host), Vec::<String>::new());
    restarted.stop_sdk_owners();
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
    let driver = driver.unwrap_or_else(|Failed { why, .. }| panic!("the execution was left as it was: {why}"));
    assert_told_the_work_ended(&slice, &restarted, driver, &session);
    assert_eq!(Slice::read_cards(&restarted).iter().filter(|(_, answered)| !answered).count(), 1, "the task goes on: its turn ended on a card");
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
        if stored_turn_ends(&database) > 0 {
            panic!("the daemon stopped after the commit");
        }
    });
    let crash = loop {
        if let Err(crash) = catch_unwind(AssertUnwindSafe(|| advance(&mut driver))) {
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

// B3: the orchestrator's instruction is claimed and recorded, then the daemon dies before the host receives it.
// A fresh daemon adopts and reconciles the unsent dispatch by re-sending its exact bytes: the host
// offers it to the same Claude exactly once.
#[test]
fn a_send_the_host_never_got_is_sent_once_by_the_adopting_owner() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    instruct(&slice.domain, "Add the tests first");
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

// B5: the store issued the permit of the orchestrator's instruction and the daemon died before claiming it:
// the permit lived only in that daemon. The adopting owner gets it again and the answer reaches
// the same Claude once.
#[test]
fn an_answer_whose_permit_the_dead_daemon_never_claimed_is_sent_by_the_adopting_owner() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    instruct(&slice.domain, "Add the tests first");
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

// B4: the host offered the orchestrator's instruction but the daemon crashed before recording its receipt. A
// fresh daemon adopts and reconciles it from the retained receipt: it is handed, and the host is
// not asked to offer it a second time.
#[test]
fn a_send_the_host_offered_is_not_offered_again_when_its_receipt_was_lost() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    instruct(&slice.domain, "Add the tests first");
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
        crate::domain_messages::tell_sdk_node(&slice.domain, &TASK.into(), "Alice", &format!("Next step {turn}"), &format!("tell-{turn}"), true).unwrap();
        driver.step(&|| false).unwrap();
        slice.settle(&mut driver);
    }
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 261, "every input reached the same Claude");
    assert!(prompts[260].ends_with("Next step 260"), "{}", prompts[260]);
    assert_eq!(Slice::read_cards(&slice.domain).len(), 261, "every stop after its wake ended on its own case");
    driver.stop().unwrap();
}

// The Mac goes down on a host whose journal was compacted: the launch turn wrote more than the
// journal keeps once acknowledged, then the host offered the orchestrator's instruction and everything died
// before the daemon recorded the receipt. The next daemon reads the compacted journal from disk:
// the answer was handed, so it goes to the resumed session flagged as maybe received already, never
// as an answer the host never got.
#[test]
fn a_compacted_journal_proves_to_the_next_daemon_that_an_answer_was_handed() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("bulky:6");
    slice.settle(&mut driver);
    instruct(&slice.domain, "Add the tests first stall-before-starting");
    assert!(driver.claim_record_and_offer_without_receipt(), "the host offered the answer");
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    let journal = restarted.read().state().lifecycle_journal[&TaskId::from(TASK)].clone();
    assert!(journal.iter().any(|record| matches!(record, LifecycleRecord::DoubtResent { .. })),
        "the ended host's journal proves the answer was handed: it goes again flagged as maybe received, never as one the host never got: {journal:?}");
    slice.settle(&mut resumed);
    let prompts = slice.prompts();
    assert!(prompts.last().unwrap().ends_with("Add the tests first stall-before-starting"), "the resumed session took the answer: {prompts:?}");
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

// 25 Oct, t-codex-support-design: the usage limit held the task, a subagent handed its report
// back and Claude's own turn hit the limit again. The domain refused that second wait, so the end
// of the turn went unrecorded and the owner stopped. Both ends are recorded and the task waits for
// the limit once.
#[test]
fn a_turn_claude_opens_by_itself_on_the_usage_limit_that_holds_its_task_keeps_the_owner_going() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("Write the release notes rate-limited-twice");
    let ended = |slice: &Slice| slice.domain.read().state().lifecycle_journal[&TaskId::from(TASK)].iter()
        .filter(|record| matches!(record, LifecycleRecord::TurnEnded { .. })).count();
    while ended(&slice) < 2 {
        driver.observe().unwrap();
    }
    assert!(!driver.host_to_replace(), "the owner keeps its host");
    let held: Vec<(String, ClaudeHold)> = slice.domain.read().state().claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect();
    assert_eq!(held, [(TASK.to_owned(), ClaudeHold::Usage)], "one wait for the limit");
    assert_eq!(slice.domain.read().state().live_state(&TASK.into()), Some(alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::Usage)));
    driver.stop().unwrap();
}

// 25-26 Oct: the usage limit came back at 01:00 and the daemon woke only the nodes its hooks saw
// stop. Usage coming back probes Claude with a task of the Agent SDK the limit holds too, whatever
// its card says of the reset, and Claude answering it lets the task go on.
#[test]
fn usage_coming_back_lets_a_task_of_the_agent_sdk_the_limit_held_go_on() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("Write the release notes rate-limited");
    slice.settle(&mut driver);
    let held: Vec<(String, ClaudeHold)> = slice.domain.read().state().claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect();
    assert_eq!(held, [(TASK.to_owned(), ClaudeHold::Usage)]);
    std::fs::write(format!("{}.recovered", slice.capture.display()), "").unwrap();
    crate::usage_pause::wake_nodes(&slice.domain);
    assert_eq!(driver.pending_permits(), 1, "the probe's dispatch reached the owner");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert!(prompts.last().is_some_and(|prompt| prompt.starts_with("Your usage limit has renewed")), "{prompts:?}");
    assert!(slice.domain.read().state().claude_held().is_empty(), "Claude answered: nothing waits for the limit");
    driver.stop().unwrap();
}

// Each launch made a provider binding of its own while the store keeps one active binding per
// host: every task after the first was refused with BindingRevoked.
#[test]
fn a_second_task_starts_through_the_same_sdk_host() {
    let slice = Slice::new();
    slice.run(&Actor::Orchestrator { generation: 0 }, "second-task", Command::CreateTask {
        plan_items: None, continues: None,
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
            provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: id.into(), state, native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0, profile: None,
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
    let Err(Failed { why: refused, .. }) = slice.launch_on("sdk-on-other", "other") else { panic!("an account the user never added runs nothing") };
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
    let options = LaunchOptions { provider: alondra_core::accounts::Provider::Claude, selection: None, model: Some("fixture-model".into()), effort: Some("high".into()), disallowed_tools: vec!["WebFetch".into()] };
    let mut driver = start_bound(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: TASK.into(), name: "Release notes".into(), directory: slice.worktree.to_str().unwrap().into(),
        prompt: "Write the release notes".into(), plugin: slice.plugin.clone(), options,
        model: None, account: AccountId::default_account(),
    }, capabilities(), None, None).unwrap();
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
    instruct(&restarted, "Add the tests first");
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

// A session is replaced by interrupting the old turn, detaching the old session and starting the
// new one, and the old runtime is interrupted only for the exact committed replacement.
// «Replace» stops the node's host through its owner and starts the task on a new execution: its
// first generation's open request is withdrawn with it, and an answer to that card is refused.
#[test]
fn native_recovery_preserves_every_queued_user_input_and_stages_the_whole_new_generation_before_offer() {
    let slice=Slice::new();let mut driver=slice.launch_working();let original_agent=driver.agent().clone();
    let original_account=slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].account_key();
    let native=native_recovery(&slice.domain,&slice.backend,&TASK.into(),Provider::Claude,&original_account.account,slice.worktree.to_str().unwrap()).unwrap().unwrap().session;
    for (id,text) in [("queued-first","First exact queued instruction"),("queued-second","Second exact queued instruction")]{
        slice.run(&Actor::System,id,Command::TellNode{task_id:TASK.into(),text:text.into(), from_user: false, answers: None});
    }
    driver.stop().unwrap();drop(driver);
    end_unowned_host(Arc::clone(&slice.domain),&slice.backend,TASK.into()).unwrap();
    crate::node_revival::user_closed(&slice.domain,&original_agent.0).unwrap();
    let pending:Vec<_>=lifecycle(&slice.domain).pending_inputs().into_iter().cloned().collect();
    assert_eq!(pending.len(),2,"the two existing instructions authorize continuation and survive release without an invented third input");
    let expected:Vec<String>=pending.iter().map(|input|slice.observer().query_row(
        "SELECT body FROM lifecycle_inputs WHERE task_id=?1 AND input_id=?2",rusqlite::params![TASK,input.as_str()],|row|row.get(0)).unwrap()).collect();
    assert_eq!(&expected[..2],["First exact queued instruction","Second exact queued instruction"]);
    let mut replacement=start(Arc::clone(&slice.domain),&slice.backend,Launch{
        task:TASK.into(),name:"Recovered release notes".into(),directory:slice.worktree.to_str().unwrap().into(),
        prompt:"This launch summary must not replace any queued user input".into(),plugin:slice.plugin.clone(),
        options:LaunchOptions::default(),model:None,account:original_account.account.clone(),
    }).unwrap();
    let stamp=lifecycle(&slice.domain).execution().unwrap().clone();
    let rows:Vec<(String,String,String,i64)>=slice.observer().prepare(
        "SELECT d.input_id,d.kind,d.status,(SELECT count(*) FROM provider_input_metadata m WHERE m.task_id=d.task_id AND m.input_id=d.input_id AND m.conversation_id=?3) FROM lifecycle_dispatches d WHERE d.task_id=?1 AND d.generation=?2 ORDER BY d.id"
    ).unwrap().query_map(rusqlite::params![TASK,i64::try_from(stamp.generation()).unwrap(),stamp.conversation().as_str()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(rows.len(),pending.len(),"every inherited pending input has a committed new-generation intent");
    for (at,(id,kind,status,metadata)) in rows.iter().enumerate(){
        assert_eq!(id,pending[at].as_str());assert_eq!(kind,if at==0{"launch"}else{"input"});assert_eq!(*metadata,1);
        if at>0{assert_eq!(status,"prepared","later inputs remain unoffered when start returns");}
    }
    slice.settle(&mut replacement);
    assert_eq!(lifecycle(&slice.domain).resource(),Some(alondra_domain::ResourceEvidence::Available),"the current generation has positively resumed its native session");
    assert!(lifecycle(&slice.domain).pending_inputs().is_empty(),"both real queued inputs have been consumed before checking the native witness");
    let delivered:Vec<String>=slice.prompts().into_iter().skip(1).collect();
    assert_eq!(delivered,expected,"every original payload reaches the same native session exactly once and in order");
    assert_eq!(slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].account_key(),original_account);
    let capture:serde_json::Value=serde_json::from_slice(&std::fs::read(&slice.capture).unwrap()).unwrap();
    let args:Vec<&str>=capture["args"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    assert!(args.windows(2).any(|pair|pair==["--resume",native.as_str()]) || args.iter().any(|arg|*arg==format!("--resume={native}")),"{args:?}");
    let starts:Vec<serde_json::Value>=std::fs::read_to_string(format!("{}.starts",slice.capture.display())).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
    let resumed=starts.last().unwrap();assert_eq!(resumed["resume"].as_str(),Some(native.as_str()));assert_eq!(resumed["cwd"].as_str(),Some(slice.worktree.to_str().unwrap()));assert!(resumed["history"].as_u64().unwrap()>0);
    replacement.stop().unwrap();
}

#[test]
fn replacement_requires_positive_original_native_session_evidence_before_stopping_its_host() {
    let slice=Slice::new();let mut driver=slice.launch();let original=driver.agent().clone();
    assert!(require_recoverable_session(&slice.domain,&slice.backend,&TASK.into(),Provider::Claude,&AccountId::default_account(),slice.worktree.to_str().unwrap()).is_err(), "an admitted UUID alone is not a native session witness");
    assert_eq!(slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.as_ref(),Some(&original));
    slice.settle(&mut driver);
    require_recoverable_session(&slice.domain,&slice.backend,&TASK.into(),Provider::Claude,&AccountId::default_account(),slice.worktree.to_str().unwrap()).unwrap();
    driver.stop().unwrap();
}

#[test]
fn replacing_an_sdk_node_starts_a_new_execution_and_its_old_cards_are_refused() {
    let slice = Slice::new();
    let (driver, card) = slice.asked("decide:bash make test");
    let old = driver.agent().clone();
    let native=native_recovery(&slice.domain,&slice.backend,&TASK.into(),Provider::Claude,&AccountId::default_account(),slice.worktree.to_str().unwrap()).unwrap().unwrap().session;
    assert!(require_recoverable_session(&slice.domain,&slice.backend,&TASK.into(),Provider::Claude,&AccountId::parse("another").unwrap(),slice.worktree.to_str().unwrap()).is_err(), "a different account is refused while the original host and card remain live");
    assert!(open_card(&slice.domain,"decision-").is_some());
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
    let capture:serde_json::Value=serde_json::from_slice(&std::fs::read(&slice.capture).unwrap()).unwrap();
    let args:Vec<&str>=capture["args"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    assert!(args.windows(2).any(|pair|pair==["--resume",native.as_str()]) || args.iter().any(|arg|*arg==format!("--resume={native}")), "replacement must resume the exact original native conversation: {args:?}");
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
    let native=native_recovery(&slice.domain,&slice.backend,&TASK.into(),Provider::Claude,&AccountId::default_account(),slice.worktree.to_str().unwrap()).unwrap().unwrap().session;
    assert!(require_recoverable_session(&slice.domain,&slice.backend,&TASK.into(),Provider::Claude,&AccountId::parse("another").unwrap(),slice.worktree.to_str().unwrap()).is_err(), "a different account is refused while the original host and card remain live");
    assert!(open_card(&slice.domain,"decision-").is_some());
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    let body = serde_json::from_value(serde_json::json!({"task": TASK, "replace": true, "model": "sonnet", "effort": "high", "reason": "Release notes are short prose"})).unwrap();
    let launched = crate::domain_orchestrator_tools::launch(&slice.domain, &slice._home, body, |_| None).await;
    assert!(launched.is_ok(), "{launched:?}");
    let state=slice.domain.state().unwrap();let stamp=state.lifecycle_of(&TASK.into()).unwrap().execution().unwrap().clone();
    let metadata=slice.domain.store.lock().unwrap().conversation_metadata(&TASK.into(),stamp.conversation().as_str()).unwrap().unwrap();
    assert_eq!(alondra_provider_claude::conversation_session(&metadata).as_deref(),Some(native.as_str()));

    assert!(open_card(&slice.domain, "decision-").is_none(), "the stopped host's request is closed with its card");
    assert!(!slice.domain.owns_sdk_host(&old), "the old host is stopped");
    let new = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().expect("the task has a node");
    assert_ne!(new, old);
    assert_eq!(lifecycle(&slice.domain).execution().map(|stamp| stamp.generation()), Some(2), "a new generation runs it");
    assert!(slice.answer(&card, Hold::ALLOW, None).is_err(), "the old generation's card takes no answer");
    slice.domain.stop_sdk_host(&new).unwrap();
}

// A cancelled SDK task is closed whatever its subagent left in its worktree: the worktrees with
// nothing unsaved are deleted, the one with unsaved files waits for the user's answer on a card.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_sdk_task_with_unsaved_files_in_a_subagents_worktree_is_closed_and_the_user_decides_on_them() {
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
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].state, TaskState::Cancelled);
    assert!(!slice.domain.owns_sdk_host(&agent), "the host is closed: the node has nothing to settle");
    assert!(subagent.join("draft.txt").exists(), "nothing with unsaved files is deleted unasked");
    assert!(!own.exists(), "the worktree with nothing unsaved is deleted");
    let cards: Vec<_> = state.interactions.values().filter(|i| i.system_card == Some(alondra_core::domain::SystemCard::DiscardWorktrees)).collect();
    assert_eq!(cards.len(), 1);
    let card = alondra_core::discard_worktrees::DiscardWorktrees::from_body(&cards[0].body).unwrap();
    assert_eq!((card.tasks[0].files, card.tasks[0].paths.clone()), (1, vec![subagent.to_str().unwrap().to_owned()]));
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
    kill_host(&driver);
    drop(driver);
}

/// Kills the driver's whole host group and waits until each of its processes exited.
pub(crate) fn kill_host(driver: &Driver) {
    let group = driver.worker.host_group().unwrap();
    let mut exits = alondra_process_group::Exits::new().unwrap();
    for member in alondra_process_group::members(group).unwrap() {
        exits.watch(&member).unwrap();
    }
    assert!(std::process::Command::new("/bin/kill").args(["-KILL", "--", &format!("-{group}")]).status().unwrap().success());
    exits.wait().unwrap();
}

impl Slice {
    /// Each native Claude the hosts started: its session, the session it resumed and how many
    /// prompts that session's transcript already held.
    pub fn starts(&self) -> Vec<serde_json::Value> {
        let path = PathBuf::from(format!("{}.starts", self.capture.display()));
        std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    pub fn restart(&self) -> Arc<DomainRuntime> {
        Arc::new(DomainRuntime::open_at(&self._home, &self.database).unwrap())
    }

    fn take_over(&self, domain: &Arc<DomainRuntime>) -> Driver {
        adopt(Arc::clone(domain), &self.backend, TASK.into(), Some(&self.plugin)).unwrap()
    }

    /// Reads the host until its turn is running.
    pub fn until_working(&self, driver: &mut Driver) {
        while lifecycle(&self.domain).activity() != ActivityView::Working {
            driver.observe().unwrap();
        }
    }
}

pub fn lifecycle(domain: &DomainRuntime) -> alondra_domain::TaskLifecycle {
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

// R1: the Mac shuts down after the first turn's case: host, Claude and daemon are gone. The daemon
// that comes back finds no process holding the host's lock, launches a fresh host for the same
// execution with the next owner generation, and the orchestrator's instruction resumes the same native session with its
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
    assert_eq!(slice.starts().len(), 1, "no Claude starts before the orchestrator settles the case");
    let node = restarted.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    assert!(!crate::node_revival::needed(&restarted, &node.0), "no claude respawn, and no card about it, for a node of the SDK");
    instruct(&restarted, "Add the tests first");
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
    assert_eq!(starts[1]["history"], 2, "with the transcript of the first turn and of the one its wake opened");
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

// A session whose event stream fails is released, and a new one opens on the next command. Here
// the host dies while this daemon runs (its Node crashed mid-turn): its owner
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

/// A node cut by a restart whose resumed session works on in a long turn, with a message handed to
/// it that it has not read yet: the state of t-corrupt-journal-detail-codex on 2026-10-27 at 17:42.
fn resumed_with_an_unread_message(slice: &Slice) -> (Arc<DomainRuntime>, Driver) {
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    kill_host_and_daemon(driver);
    std::fs::write(format!("{}.hold", slice.capture.display()), "").unwrap();
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    resumed.step(&|| false).unwrap();
    while lifecycle(&restarted).activity() != ActivityView::Working {
        resumed.observe().unwrap();
    }
    let state = restarted.state().unwrap();
    let tell = Command::TellNode { task_id: TASK.into(), text: "[From the orchestrator · 17:42] Keep the journal's old rows".into(), from_user: false, answers: None };
    restarted.execute_fixture(&Actor::System, envelope(&state, "orchestrator-message", tell)).unwrap();
    assert_eq!(resumed.pending_permits(), 1, "the message goes to the host while the turn runs");
    resumed.step(&|| false).unwrap();
    while !slice.prompts().last().is_some_and(|prompt| prompt.ends_with("Keep the journal's old rows")) {
        resumed.observe().unwrap();
    }
    assert_eq!(lifecycle(&restarted).pending_inputs().len(), 1, "it is not read yet");
    (restarted, resumed)
}

/// Nobody is asked about the message: it goes again, flagged, to the session the new host resumes.
fn assert_message_goes_again_by_itself(domain: &DomainRuntime) {
    assert!(open_card(domain, HANDOFF_DOUBT_PREFIX).is_none(), "no card asks whether to send the message again");
    assert!(open_card(domain, SESSION_ENDED_PREFIX).is_none(), "no card asks how the task goes on");
    let read = domain.read();
    let journal = &read.state().lifecycle_journal[&TaskId::from(TASK)];
    assert!(journal.iter().any(|record| matches!(record, LifecycleRecord::DoubtResent { text: Some(text), .. }
        if text.starts_with("Your session was cut off when its host stopped") && text.ends_with("Keep the journal's old rows"))), "the message is sent again");
}

// 2026-10-27, t-corrupt-journal-detail-codex: after a restart its session worked on for 8 minutes,
// then its owner stopped on an error of Alondra's own. The resumed session had not finished a turn,
// so its end was taken for a session that keeps closing, and the user got «closed before confirming
// it read your message: send it again?». An end Alondra caused is no such sign: it resumes again.
#[test]
fn a_message_cut_when_the_owner_of_a_resumed_session_fails_goes_again_without_asking() {
    let slice = Slice::new();
    let (restarted, mut resumed) = resumed_with_an_unread_message(&slice);
    resumed.fail_next_step = true;
    assert!(resumed.step(&|| false).is_err(), "the step failed");
    assert!(resumed.host_to_replace());
    let mut next = resumed.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    assert_message_goes_again_by_itself(&restarted);
    next.stop().unwrap();
}

// The same message when the daemon stops (a restart, an update) while the resumed session works.
#[test]
fn a_message_cut_when_the_daemon_stops_a_resumed_session_goes_again_without_asking() {
    let slice = Slice::new();
    let (restarted, resumed) = resumed_with_an_unread_message(&slice);
    let mut owner = Owner::spawn(resumed);
    owner.stop().unwrap();
    drop(restarted);
    let again = slice.restart();
    let mut next = slice.take_over(&again);
    assert_message_goes_again_by_itself(&again);
    next.stop().unwrap();
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

// Refusing a turn-end reaction must not discard the actual end or replace its running host.
#[test]
fn refused_turn_end_reactions_keep_the_same_host_and_record_its_next_turn() {
    let slice = Slice::new();
    let acknowledged = watch_acknowledgements(&slice);
    let mut driver = slice.launch();
    let execution = lifecycle(&slice.domain).execution().cloned().unwrap();
    let group = driver.worker.host_group().unwrap();
    let mut native = None;
    for round in 0..2 {
        let before = slice.domain.read().state().lifecycle_journal[&TaskId::from(TASK)].iter()
            .filter(|record| matches!(record, LifecycleRecord::TurnEnded { .. })).count();
        if round > 0 {
            tell(&slice, &driver, "Write the next release note in the same session");
        }
        crate::domain_store::refuse_next(crate::domain_store::Refuse::TurnEnd);
        slice.settle(&mut driver);
        let state = slice.domain.state().unwrap();
        let journal = &state.lifecycle_journal[&TaskId::from(TASK)];
        assert!(journal.iter().filter(|record| matches!(record, LifecycleRecord::TurnEnded { .. })).count() > before,
            "the actual observed end remains recorded after its reaction was refused");
        assert!(lifecycle(&slice.domain).turn().is_none(), "no ended turn remains open");
        assert_eq!(lifecycle(&slice.domain).execution(), Some(&execution));
        assert_eq!(driver.worker.host_group(), Some(group));
        assert!(!driver.host_to_replace());
        assert!(!journal.iter().any(|record| matches!(record, LifecycleRecord::HostEnded { .. })));
        assert!(open_card(&slice.domain, HOST_LOST_PREFIX).is_none());
        assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none());
        let starts = slice.starts();
        assert_eq!(starts.len(), 1, "the native process was never replaced");
        let current = (starts[0]["session"].clone(), starts[0]["pid"].clone());
        if let Some(previous) = &native { assert_eq!(&current, previous); }
        native = Some(current);
        let ended = journal.iter().rev().find_map(|record| match record {
            LifecycleRecord::TurnEnded { turn, .. } => Some(turn.clone()), _ => None,
        }).unwrap();
        let before_replay = state.lifecycle_journal.clone();
        slice.run(&Actor::System, &format!("end-evidence-replay-{round}"), Command::RecordRuntimeFacts {
            task_id: TASK.into(), generation: execution.generation(), facts: vec![RuntimeObservation::TurnEnded { turn: ended, failure: None, failed: None }],
        });
        assert_eq!(slice.domain.read().state().lifecycle_journal, before_replay, "the same ended turn is evidence, not another transition");
    }
    assert!(!acknowledged.lock().unwrap().is_empty(), "the host was acknowledged after publication");
    assert!(slice.prompts().iter().any(|prompt| prompt.contains("Write the next release note in the same session")),
        "a second real input reached the same native process and ended");
    driver.stop().unwrap();
}


// A subagent hands its report back after the turn ended: Claude starts a turn by itself while the
// task rests. Its owner reads it as it comes: the case that asked the orchestrator how the task
// goes on is withdrawn and the node works, with nothing sent to it.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_claude_starts_while_its_task_rests_is_read_as_it_comes() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("own-turn-later");
    slice.settle(&mut driver);
    let card = open_card(&slice.domain, STALLED_PREFIX).expect("the turn ended on its case").id;
    let owner = Owner::spawn(driver);
    let mut changed = slice.domain.subscribe();
    std::fs::write(format!("{}.own-turn", slice.capture.display()), "").unwrap();
    let reader = Arc::clone(&slice.domain);
    changed.wait_for(|_| reader.read().state().interactions[&card].withdrawn).await.unwrap();
    assert_eq!(lifecycle(&slice.domain).activity(), ActivityView::Working, "the node works");
    assert_eq!(slice.prompts().len(), 1, "nothing was sent to it");
    drop(owner);
}


// Production, 25 October 07:03: the disk filled up and the daemon restarted with 953 MB free.
// Taking over two SDK executions could not write the node's capability (No space left on device)
// and each was given up with a card, though nothing about the task had failed. A takeover that
// finds the disk full now waits with no card; a sample of the machine that still measures it full
// takes nothing over, and the first that measures space again takes it over with the same session.
#[tokio::test(flavor = "multi_thread")]
async fn a_takeover_the_full_disk_refused_waits_for_space_and_then_resumes_the_same_session() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    restarted.full_disk.store(true, std::sync::atomic::Ordering::SeqCst);
    adopt_all(&restarted, &slice.backend);
    let task = TaskId::from(TASK);
    let agent = restarted.state().unwrap().tasks[&task].agent_id.clone().unwrap();
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none(), "a full disk is not the task's failure: no card");
    assert_ne!(restarted.state().unwrap().tasks[&task].state, TaskState::NeedsYou);
    assert!(!restarted.owns_sdk_host(&agent));
    assert!(restarted.waiting_for_space.lock().unwrap().contains(&task), "the takeover waits for space");
    restarted.full_disk.store(false, std::sync::atomic::Ordering::SeqCst);
    let disk = |free: u64| alondra_core::Machine { disk_total: 500_000_000_000, disk_available: free, ..alondra_core::Machine::default() };
    crate::machine_watch::look(&restarted, disk(900_000_000));
    restarted.join_retakes();
    assert!(!restarted.owns_sdk_host(&agent), "a sample that measures the disk full takes nothing over");
    assert!(restarted.waiting_for_space.lock().unwrap().contains(&task));
    loop {
        crate::machine_watch::look(&restarted, disk(400_000_000_000));
        if !restarted.live.machine_watch.disk_low() {
            break;
        }
    }
    restarted.join_retakes();
    assert!(restarted.owns_sdk_host(&agent), "the sample that measured space took the execution over");
    assert!(restarted.waiting_for_space.lock().unwrap().is_empty());
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none());
    let mut changed = restarted.subscribe();
    let capture = slice.capture.clone();
    changed.wait_for(|_| prompts_at(&capture).last().is_some_and(|prompt| prompt.starts_with("Your session was cut off when its host stopped"))).await.unwrap();
    restarted.stop_sdk_owners();
    let starts = slice.starts();
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
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
    crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "Alice", driver.agent(), message, true).await.unwrap();
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

// R4: the first turn ended, the orchestrator instructed the node and the host offered that to a Claude whose
// session then ended before it showed it read it; the Mac went down before the receipt was kept.
// Nobody is asked: the resumed session gets the same words once more, flagged as possibly received
// already, and its transcript tells Claude whether it was.
#[test]
fn a_doubtful_answer_goes_again_to_the_resumed_session_flagged_unasked() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    instruct(&slice.domain, "Add the tests first unread");
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

// R5: the orchestrator's instruction was claimed and recorded, and the Mac went down before the host got it. The ended
// host's journal shows it never took the command, so it goes once to the resumed session, unasked.
#[test]
fn an_answer_the_ended_host_never_took_goes_to_the_resumed_session_unasked() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    instruct(&slice.domain, "Add the tests first");
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
// Every turn whose result is not a success ends as failed, with its code. Claude ended this one
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
    assert_eq!(card.card.effective_title(), "Claude didn't get to read your message");
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

// A prompt Claude completes without starting fails its turn, as Claude does with one its
// transcript already holds. The message leaves the queue and «Discard» lets it go: the
// orchestrator gets the case of how the task goes on and Claude gets nothing more.
#[test]
fn a_message_claude_completes_without_starting_it_can_be_discarded() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    instruct(&slice.domain, "complete-unstarted");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let card = open_card(&slice.domain, DROPPED_PREFIX).expect("the dropped message asks the user");
    assert!(card.card.content.as_ref().is_some_and(|content| content.body.ends_with("complete-unstarted")), "{:?}", card.card.content);
    assert!(!driver.waits_for_host(), "the owner no longer waits for it");
    answer(&slice.domain, &card.id, DISCARD);
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty());
    assert_eq!(driver.pending_permits(), 0, "nothing is sent");
    assert!(Slice::read_cards(&slice.domain).iter().any(|(card, settled)| !settled && card.starts_with(STALLED_PREFIX)), "the orchestrator settles how it goes on");
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

pub fn subtypes(controls: &[serde_json::Value]) -> Vec<&str> {
    controls.iter().map(|control| control["subtype"].as_str().unwrap()).collect()
}





// Claude Code tracks a foreground command that runs for seconds as its own work, which ends
// inside the turn that waits for it: that turn goes on with it and ends on one card.
#[test]
fn a_command_claude_code_tracks_as_work_ends_inside_its_turn_and_the_turn_ends_with_one_card() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("foreground-command");
    slice.read_turn_result(&mut driver);
    assert_eq!(slice.observations_with("\"subtype\":\"task_notification\""), 1, "the command's work ended");
    slice.settle(&mut driver);
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "nothing waits for the command any more");
    let cards = Slice::read_cards(&slice.domain);
    assert_eq!(cards.len(), 1, "the turn ended with one card: {cards:?}");
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




#[test]
fn ending_a_host_rejects_controls_whose_preparation_left_no_command() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let agent = driver.agent().clone();
    for command_id in ["first-interrupt", "second-interrupt"] {
        slice.run(&user(), command_id, Command::InterruptTurn { agent_id: agent.clone() });
        driver.sealed();
        let permit = driver.controls.pop_front().unwrap();
        assert!(driver.domain.own(driver.scope.clone(), |owner| owner.claim_control(permit)).unwrap().0);
        // The native connection can disappear here, before prepare_control records bytes.
        let _abandoned = driver.sealed().control_attempts.pop().unwrap();
    }
    assert_eq!(slice.control_rows().into_iter().map(|row| row.2).collect::<Vec<_>>(), ["claimed", "claimed"]);
    assert!(slice.controls().is_empty());
    drop(driver);
    let mut driver = adopt(Arc::clone(&slice.domain), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(slice.control_rows().into_iter().map(|row| row.2).collect::<Vec<_>>(), ["claimed", "claimed"], "adopting a live host does not discard its requests");
    driver.stop().unwrap();
    drop(driver);
    end_unowned_host(Arc::clone(&slice.domain), &slice.backend, TASK.into()).unwrap();
    assert_eq!(slice.control_rows().into_iter().map(|row| row.2).collect::<Vec<_>>(), ["rejected", "rejected"]);
    assert!(slice.controls().is_empty(), "the ended host never received either control");
}

// 3 November: «Live Activity 3» was cancelled after its host ended. Its owner had claimed an input
// and lost the host before it recorded the command, and the node was told how to settle its
// worktrees after the cancel. Every close failed: the lifecycle refused the host's end, and the
// claimed input with no command held every change of the execution. The user got a card that
// neither «Try again» nor «Leave it open» could settle.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_task_whose_ended_host_left_a_claim_with_no_command_closes_and_asks_nobody() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    slice.run(&Actor::System, "told-before", Command::TellNode { task_id: TASK.into(), text: "Add the changelog".into(), from_user: false, answers: None });
    driver.sealed();
    let permit = driver.permits.pop_front().expect("the input waits for the host");
    driver.domain.own(driver.scope.clone(), |owner| owner.claim(permit)).unwrap();
    // The host can disappear here, before record_command records the bytes it would get.
    let _abandoned = driver.sealed().attempts.pop().unwrap();
    driver.stop().unwrap();
    drop(driver);
    slice.run(&user(), "cancel", Command::CancelTask { task_id: TASK.into() });
    slice.run(&Actor::System, "told-after", Command::TellNode { task_id: TASK.into(), text: "Settle your worktrees".into(), from_user: false, answers: None });
    slice.domain.enable_sdk(slice.backend.clone());
    close_pass(&slice).await;
    let task = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].clone();
    assert_eq!(task.state, TaskState::Cancelled);
    let statuses: Vec<String> = slice.observer().prepare("SELECT status FROM lifecycle_dispatches WHERE task_id=?1 AND kind='input' ORDER BY id").unwrap()
        .query_map([TASK], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(statuses[0], "rejected", "the ended host never received the claimed input: {statuses:?}");
    for prefix in [alondra_core::close_problem::CLOSE_PROBLEM_PREFIX, SESSION_ENDED_PREFIX, HANDOFF_DOUBT_PREFIX, HOST_LOST_PREFIX] {
        assert!(open_card(&slice.domain, prefix).is_none(), "no card about {prefix}");
    }
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
    assert_eq!(slice.options(&card), [Hold::ALLOW, Hold::FOR_TASK, Hold::DENY], "no «allow always» outside a repo");
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
    assert_eq!(slice.options(&card), [Hold::ALLOW, Hold::FOR_TASK, Hold::ALWAYS, Hold::DENY]);
    slice.answer(&card, Hold::ALWAYS, None).unwrap();
    let settings = std::fs::canonicalize(&slice.worktree).unwrap().join(crate::allow_always::SETTINGS);
    assert_eq!(crate::allow_always::allowed(&settings).unwrap(), ["Bash(make build *)"]);
    let rules = slice.domain.store.lock().unwrap().allowed_rules().unwrap();
    assert_eq!(rules.iter().map(|rule| (rule.rule.as_str(), rule.state)).collect::<Vec<_>>(), [("make build *", crate::allow_always::RuleState::Active)]);
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice), serde_json::json!({"behavior": "allow", "toolUseID": "toolu_fixture_1", "updatedInput": {"command": "make build"}}));
    driver.stop().unwrap();
}

/// Tells the node to ask for `command` run from `cwd` and runs the owner until Claude has its
/// answer or the user is asked: the card, or `None` when nobody was asked.
fn asks_again(slice: &Slice, driver: &mut Driver, cwd: &str, command: &str) -> Option<String> {
    let answered = slice.decisions().len();
    slice.run(&Actor::System, &format!("tell-{answered}"), Command::TellNode {
        task_id: TASK.into(), text: format!("[From Alice · 09:00] decide:bash@{cwd} {command}"),
        from_user: false, answers: None,
    });
    driver.step(&|| false).unwrap();
    slice.until(driver, |slice| slice.decisions().len() > answered || !Slice::open_decision_cards(&slice.domain).is_empty());
    let card = Slice::open_decision_cards(&slice.domain).pop();
    if card.is_none() {
        let last = slice.decisions().pop().unwrap();
        assert_eq!((&last["response"]["behavior"], &last["response"]["updatedInput"]["command"]), (&serde_json::json!("allow"), &serde_json::json!(command)), "{last}");
        slice.rest(driver);
    }
    card
}

fn allowed_in_task(slice: &Slice) -> Vec<(String, u32)> {
    slice.domain.state().unwrap().tasks[&TaskId(TASK.into())].allowed.iter().map(|(card, grant)| (card.0.clone(), grant.uses)).collect()
}

// D3b: «Permitir en esta tarea» on a reset inside the task's worktree: the same command from the
// same folder runs again without a card, and nothing else does: another target, another folder,
// another branch or a path outside the worktree asks again. Revoked, it asks again; once the task
// ends, nothing it allowed is left.
#[test]
fn a_command_allowed_in_the_task_runs_again_only_as_the_same_command_from_the_same_folder() {
    let slice = Slice::new();
    let worktree = slice.worktree.to_str().unwrap().to_owned();
    let reset = "git reset --hard origin/main";
    let (mut driver, card) = slice.asked(&format!("decide:bash@{worktree} {reset}"));
    assert_eq!(slice.options(&card), [Hold::ALLOW, Hold::FOR_TASK, Hold::DENY], "no «allow always» outside a repo, «allow in this task» inside the worktree");
    slice.answer(&card, Hold::FOR_TASK, None).unwrap();
    slice.rest(&mut driver);
    assert_eq!(only_response(&slice)["behavior"], "allow");
    assert_eq!(allowed_in_task(&slice), [(card.clone(), 0)]);
    assert_eq!(asks_again(&slice, &mut driver, &worktree, reset), None, "the same command from the same folder is not asked again");
    assert_eq!(allowed_in_task(&slice), [(card.clone(), 1)]);
    let observer = slice.observer();
    let stored: (String, i64) = observer.query_row("SELECT card,uses FROM task_permission_grants WHERE task_id=?1", [TASK], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(stored, (card.clone(), 1), "what the user allowed is kept with the task");
    for (cwd, command, why) in [
        (worktree.clone(), "git reset --hard HEAD~1", "another target"),
        (format!("{worktree}/crates"), reset, "another folder"),
    ] {
        let card = asks_again(&slice, &mut driver, &cwd, command).unwrap_or_else(|| panic!("{why} asks again"));
        slice.answer(&card, Hold::DENY, None).unwrap();
        slice.rest(&mut driver);
    }
    let branch = asks_again(&slice, &mut driver, &worktree, "git branch -D scratch-a").unwrap();
    slice.answer(&branch, Hold::FOR_TASK, None).unwrap();
    slice.rest(&mut driver);
    let other = asks_again(&slice, &mut driver, &worktree, "git branch -D scratch-b").expect("another branch asks again");
    slice.answer(&other, Hold::DENY, None).unwrap();
    slice.rest(&mut driver);
    let outside = asks_again(&slice, &mut driver, &worktree, "cp build/app /Users/someone/Desktop/app").expect("outside the worktree it asks");
    assert_eq!(slice.options(&outside), [Hold::ALLOW, Hold::DENY], "and cannot be allowed in the task");
    let state = slice.domain.state().unwrap();
    assert_eq!(state.interactions[&InteractionId(outside.clone())].card.allow_for_task,
        Some(alondra_core::allow_for_task::AllowForTask::Refused { why: alondra_core::allow_for_task::NotForTask::Outside, part: "/Users/someone/Desktop/app".into() }));
    slice.answer(&outside, Hold::DENY, None).unwrap();
    slice.rest(&mut driver);
    slice.run(&user(), "revoke", Command::RevokeTaskGrant { task_id: TASK.into(), card: card.as_str().into() });
    let revoked = asks_again(&slice, &mut driver, &worktree, reset).expect("revoked, it asks again");
    slice.answer(&revoked, Hold::ALLOW, None).unwrap();
    slice.rest(&mut driver);
    assert_eq!(allowed_in_task(&slice), [(branch, 0)]);
    driver.stop().unwrap();
    slice.run(&user(), "cancel", Command::CancelTask { task_id: TASK.into() });
    assert!(allowed_in_task(&slice).is_empty(), "nothing stays allowed once the task ends");
    let left: i64 = slice.observer().query_row("SELECT count(*) FROM task_permission_grants", [], |row| row.get(0)).unwrap();
    assert_eq!(left, 0);
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
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "Alice", driver.agent(), message, true))
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
    instruct(&slice.domain, "Ask it again");
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


/// The task is cancelled from its turn-end card, which stays answered with that choice.
pub fn assert_cancelled_from_its_card(slice: &Slice, reply: &serde_json::Value) {
    assert_eq!(reply["outcome"], "applied", "{reply}");
    let state = slice.domain.state().unwrap();
    assert!(state.tasks[&TaskId::from(TASK)].cancelled(), "the task is cancelled");
    let cards: Vec<_> = state.interactions.values()
        .filter(|card| card.id.0.starts_with(&format!("{TURN_ENDED_PREFIX}{TASK}-")))
        .map(|card| (matches!(&card.payload, InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == alondra_core::domain::CANCEL_TASK), card.withdrawn))
        .collect();
    assert!(cards.contains(&(true, false)), "the card keeps the user's choice: {cards:?}");
}

/// The stopped node's case is the orchestrator's, never a card of the user's: the orchestrator
/// closes the task from it with its result, which the case keeps.
fn assert_the_orchestrator_closes_it_from_its_case(slice: &Slice) {
    let (case, _) = Slice::read_cards(&slice.domain).into_iter().find(|(card, settled)| !settled && card.starts_with(STALLED_PREFIX)).expect("an open case");
    let state = slice.domain.state().unwrap();
    let open_for_user = |state: &DomainState| state.interactions.values()
        .filter(|card| card.recipient == alondra_core::domain::Recipient::User && card.scope == alondra_core::domain::Scope::Task { task_id: TASK.into() } && state.open(card))
        .count();
    assert_eq!(open_for_user(&state), 0, "no card asks the user how it goes on");
    let result = "The release notes were never written.";
    let close = Command::SettleStall { case_id: case.as_str().into(), task_id: TASK.into(), outcome: StallOutcome::Close { result: result.into() } };
    let orchestrator = Actor::Orchestrator { generation: state.orchestrator_generation };
    assert!(slice.domain.execute_with(&orchestrator, &envelope(&state, "orchestrator-closes", close), None, None, || Ok(())).unwrap().changed);
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert!(task.over(), "the task ends");
    assert_eq!(task.stall_closing, Some(alondra_core::domain::StallClosing::Settled { result: result.into() }));
    assert!(matches!(&state.interactions[&case.as_str().into()].payload, InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == STALL_CLOSE),
        "the case keeps the orchestrator's outcome");
    assert_eq!(open_for_user(&state), 0, "nobody asked the user");
}

#[test]
fn the_orchestrator_closes_a_stopped_sdk_task_from_its_case() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    assert_the_orchestrator_closes_it_from_its_case(&slice);
    driver.stop().unwrap();
}


#[test]
fn the_orchestrator_closes_a_task_stopped_after_an_approved_plan_from_its_case() {
    let slice = Slice::new();
    let (mut driver, card) = slice.asked("decide:plan");
    slice.answer(&card, Hold::APPROVE, None).unwrap();
    slice.rest(&mut driver);
    assert_eq!(slice.control_rows()[0].2, "handed");
    assert_the_orchestrator_closes_it_from_its_case(&slice);
    driver.stop().unwrap();
}

impl Slice {
    /// Launches task `task` of the request told `prompt`, and reads its host until `done`.
    fn launch_until(&self, task: &str, prompt: &str, done: impl Fn(&DomainState, &AgentId) -> bool) -> Driver {
        let mut driver = start_bound(Arc::clone(&self.domain), &self.backend, Launch {
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
        assert!(state.node_waits_on_user(driver.agent()), "the log lists it as waiting on the user");
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

    /// A daemon of this release that took over a node running a command in the background on a
    /// host of the release before, as an upgrade leaves it: the daemon, its driver, the node's
    /// Claude session and the old host's processes.
    fn upgraded_during_a_background_command(&self) -> (Arc<DomainRuntime>, Driver, String, Vec<alondra_process_group::Member>) {
        let before = self.release_before();
        let mut driver = self.launch_with_backend(&before, "background-command", capabilities());
        while lifecycle(&self.domain).activity() != ActivityView::OwnWork {
            driver.observe().unwrap();
        }
        let command = self.background_command();
        let session = self.starts()[0]["session"].as_str().unwrap().to_owned();
        let old_host = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
        // The daemon upgrades: it exits and leaves its hosts running for the next one.
        drop(driver);
        let upgraded = self.restart();
        upgraded.enable_sdk(self.backend.clone());
        let (_, adopted) = take_all(&upgraded, &self.backend).pop().expect("the start takes the execution");
        let adopted = adopted.unwrap_or_else(|Failed { why, .. }| panic!("the execution was not taken over: {why}"));
        assert!(alondra_process_group::alive(&command).unwrap(), "the background command survived the upgrade");
        assert!(adopted.worker.runs_another_release(), "the node goes on on the host of the release before");
        assert_eq!(self.starts().len(), 1, "no Claude started again");
        (upgraded, adopted, session, old_host)
    }

    /// What the orchestrator said after the move reached the same session on a host of this release, the
    /// host of the release before is gone, and nothing was asked.
    fn assert_moved_to_this_release(&self, upgraded: &DomainRuntime, session: &str, old_host: &[alondra_process_group::Member]) {
        let starts = self.starts();
        assert_eq!(starts.len(), 2, "one more Claude started, on this release's host: {starts:?}");
        assert_eq!(starts[1]["resume"].as_str(), Some(session), "it resumed the same session");
        let prompts = self.prompts();
        assert_eq!(prompts.len(), 2, "the new host's Claude got only what the orchestrator said, nothing about a cut: {prompts:?}");
        assert!(prompts[1].ends_with("Add the tests first") && !prompts[1].contains("cut off"), "nothing about a cut: {prompts:?}");
        let left: Vec<String> = old_host.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
        assert!(left.is_empty(), "the host of the release before is gone: {left:?}");
        assert_eq!(Self::other_cards(upgraded), Vec::<String>::new(), "moving the node asked the user nothing");
    }

    /// The cards of the task that are not how a turn ended, open or not.
    fn other_cards(domain: &DomainRuntime) -> Vec<String> {
        domain.read().state().interactions.keys()
            .filter(|card| card.0.contains(TASK) && !rests_on(TASK, card))
            .map(|card| card.0.clone())
            .collect()
    }
}

// Production, 2026-10-22 20:46: daemon 0.3.5 refused every live host 0.3.4 had launched, because
// its executable was not its own, and ended each with its process group: a background bazel test
// died with exit 143. A daemon of the next release adopts the host of the release before as it
// runs, so the command Claude left running goes on; once the command ended and Claude took up its
// notification, the node rests, and a host of this release that resumes the same session takes the
// old one's place, with no card. What the orchestrator says next reaches that session.
#[tokio::test(flavor = "multi_thread")]
async fn an_upgrade_keeps_the_background_command_of_a_host_of_the_release_before_and_moves_the_node_once_it_rests() {
    let slice = Slice::new();
    let (upgraded, adopted, session, old_host) = slice.upgraded_during_a_background_command();
    let mut changed = upgraded.subscribe();
    let owner = Owner::spawn(adopted);
    slice.release_background_command();
    let reader = Arc::clone(&upgraded);
    let gone = |old: &[alondra_process_group::Member]| old.iter().all(|process| !alondra_process_group::alive(process).unwrap());
    changed.wait_for(|_| lifecycle(&reader).activity() == ActivityView::Resting && gone(&old_host)).await.unwrap();
    let rested = Slice::read_cards(&upgraded).len();
    instruct(&upgraded, "Add the tests first");
    changed.wait_for(|_| Slice::read_cards(&reader).len() > rested).await.unwrap();
    slice.assert_moved_to_this_release(&upgraded, &session, &old_host);
    upgraded.add_sdk_owner(owner);
    upgraded.stop_sdk_owners();
}

// Production can answer the node in the moment between its owner seeing it rest on the host of
// the release before and the owner's next step. The answer is not handed to that host: a host of
// this release takes its place first, and the answer reaches the same session there. The owner is
// stepped by hand up to the step that finds the node at rest; the answer comes before the next.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_that_comes_as_the_node_rests_on_a_host_of_the_release_before_reaches_this_release() {
    let mut slice = Slice::new();
    let completion_gate = slice._home.join("wake-completion");
    let path = std::ffi::CString::new(completion_gate.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    slice.backend.native_environment.insert("FIXTURE_COMPLETION_GATE".into(), completion_gate.to_str().unwrap().into());
    let before = slice.release_before();
    let mut driver = slice.launch_with_backend(&before, "Write the release notes", capabilities());
    slice.settle(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let old_host = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    drop(driver);
    let upgraded = slice.restart();
    upgraded.enable_sdk(slice.backend.clone());
    let mut adopted = slice.take_over(&upgraded);
    assert!(adopted.worker.runs_another_release(), "the node goes on on the host of the release before");
    while !adopted.moves_to_this_release {
        adopted.step(&|| false).unwrap();
    }
    assert_eq!(lifecycle(&upgraded).activity(), ActivityView::Resting, "the node rests on the host of the release before");
    adopted.moves_to_this_release = false;
    let rested = Slice::read_cards(&upgraded).len();
    instruct(&upgraded, "Add the tests first");
    let instruction = lifecycle(&upgraded).pending_inputs().last().unwrap().as_str().to_owned();
    let dispatch: i64 = slice.observer().query_row(
        "SELECT id FROM lifecycle_dispatches WHERE task_id=?1 AND input_id=?2 ORDER BY id DESC LIMIT 1",
        rusqlite::params![TASK, instruction], |row| row.get(0),
    ).unwrap();
    // The result already made the node rest; its native completion frame arrives afterward.
    std::fs::write(&completion_gate, b"release\n").unwrap();
    assert!(adopted.worker.readable(std::time::Duration::from_secs(slice.backend.io_timeout_seconds)).unwrap());
    adopted.step(&|| false).unwrap();
    let status: String = slice.observer().query_row(
        "SELECT status FROM lifecycle_dispatches WHERE id=?1", [dispatch], |row| row.get(0),
    ).unwrap();
    assert_eq!(status, "prepared", "the late completion must be read before the new instruction is handed to the old host");
    assert!(lifecycle(&upgraded).pending_inputs().iter().any(|input| input.as_str() == instruction), "the new instruction remains pending");
    assert_eq!(slice.prompts(), ["Write the release notes"], "the old host receives no new instruction");
    let mut changed = upgraded.subscribe();
    let owner = Owner::spawn(adopted);
    let reader = Arc::clone(&upgraded);
    tokio::time::timeout(std::time::Duration::from_secs(60), changed.wait_for(|_| Slice::read_cards(&reader).len() > rested))
        .await
        .expect("the answer's turn did not end within 60 s on a host of this release")
        .unwrap();
    slice.assert_moved_to_this_release(&upgraded, &session, &old_host);
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
    instruct(&started, "Add the tests first");
    taken.step(&|| false).unwrap();
    slice.settle(&mut taken);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(!prompts[1].contains("cut off"), "its first prompt is what the orchestrator said, nothing about a cut: {prompts:?}");
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

#[path = "codex_tests.rs"]
pub mod codex_tests;

#[test]
fn fixture_current_revision_keeps_actor_fences_and_client_staleness() {
    let slice = Slice::new();
    let title = || Command::TitleRequest { request_id: "sdk-slice-request".into(), title: "Forbidden writer".into() };
    let forbidden = slice.try_run(&Actor::Agent { agent_id: "unassigned".into() }, "wrong-actor", title()).unwrap_err();
    assert!(matches!(forbidden, StoreError::Domain(error) if error.code == "forbidden"));
    let stale = slice.try_run(&Actor::Orchestrator { generation: 1 }, "wrong-generation", title()).unwrap_err();
    assert!(matches!(stale, StoreError::Domain(error) if error.code == "stale_generation"));
    let state = slice.domain.state().unwrap();
    let mut request = envelope(&state, "stale-client", title());
    request.expected_revision = 0;
    let stale = slice.domain.execute_with(&user(), &request, None, None, || Ok(())).unwrap_err();
    assert!(matches!(stale, StoreError::Domain(error) if error.code == "stale_revision"));
}

async fn tell_as_user(domain: &DomainRuntime, home: &Path, agent: &AgentId, text: &str) -> serde_json::Value {
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": text})).unwrap();
    crate::domain_messages::deliver(domain, home, &user(), "Alice", agent, message, true).await.unwrap()
}

/// Waits until the last prompt Claude got ends with `text`, or fails once a minute passed: a
/// message that never arrives is a failure, never a hang.
async fn until_last_prompt_ends_with(domain: &DomainRuntime, capture: &Path, text: &str) {
    let mut changed = domain.subscribe();
    let arrived = changed.wait_for(|_| prompts_at(capture).last().is_some_and(|prompt| prompt.ends_with(text)));
    tokio::time::timeout(std::time::Duration::from_secs(60), arrived).await
        .unwrap_or_else(|_| panic!("the message never reached Claude: {:?}", prompts_at(capture))).unwrap();
}

// 30 October: a node read «Answering you» doing nothing, and what the user wrote to it never reached
// it. Its execution had no owner in the daemon: its host's owner was gone and nothing took it over,
// so the message waited for an owner that never came. Writing to it now takes it over: a host that
// resumes the same Claude session takes the message.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_to_a_task_whose_execution_no_owner_holds_takes_it_over_and_reaches_its_session() {
    let slice = Slice::new();
    slice.domain.enable_sdk(slice.backend.clone());
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let agent = driver.agent().clone();
    let (session, _) = strand_host(&slice, driver);
    assert!(!slice.domain.owns_sdk_host(&agent));
    tell_as_user(&slice.domain, &slice._home, &agent, "Are you there?").await;
    until_last_prompt_ends_with(&slice.domain, &slice.capture, "Are you there?").await;
    assert!(slice.domain.owns_sdk_host(&agent), "the execution was taken over");
    slice.domain.stop_sdk_owners();
    assert_eq!(slice.starts().last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {:?}", slice.starts());
}

// The task's card says Alondra could not resume its session, with «Try again» and «Cancel». The user
// writes to the node instead: the message was held behind that card and nothing resumed. Writing is
// their «Try again»: the card leaves and a host that resumes the session gets what they wrote.
#[tokio::test(flavor = "multi_thread")]
async fn the_users_message_to_a_task_whose_host_was_lost_is_its_try_again() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let (session, _) = strand_host(&slice, driver);
    let refuse = |refused: bool| std::fs::set_permissions(&slice.backend.state_root, std::fs::Permissions::from_mode(if refused { 0o500 } else { 0o700 })).unwrap();
    refuse(true);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    adopt_all(&restarted, &slice.backend);
    let card = open_card(&restarted, HOST_LOST_PREFIX).expect("the task has the card to try again");
    refuse(false);
    let agent = restarted.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    let told = tell_as_user(&restarted, &slice._home, &agent, "Are you there?").await;
    assert_ne!(told["reaches"], "pause", "nothing holds the message: {told}");
    assert!(open_card(&restarted, &card.id.0).is_none(), "the card left the user's inbox");
    until_last_prompt_ends_with(&restarted, &slice.capture, "Are you there?").await;
    let prompts = prompts_at(&slice.capture);
    assert!(prompts.last().unwrap().starts_with("Your session was cut off when its host stopped"), "{prompts:?}");
    restarted.stop_sdk_owners();
    assert_eq!(slice.starts().last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {:?}", slice.starts());
}

// The thread that owns a node's host panics: it held nothing any more but still counted as the
// owner, so nothing could take the execution over until the daemon restarted. It no longer counts,
// and the user's message takes the execution over.
#[tokio::test(flavor = "multi_thread")]
async fn an_owner_whose_thread_panicked_owns_nothing_and_a_message_takes_its_execution_over() {
    let slice = Slice::new();
    slice.domain.enable_sdk(slice.backend.clone());
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let agent = driver.agent().clone();
    driver.panic_next_step = true;
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    while !slice.domain.sdk_owners.lock().unwrap().iter().all(|owner| owner.thread.as_ref().is_some_and(std::thread::JoinHandle::is_finished)) {
        std::thread::yield_now();
    }
    assert!(!slice.domain.owns_sdk_host(&agent), "an owner whose thread is gone owns nothing");
    tell_as_user(&slice.domain, &slice._home, &agent, "Are you there?").await;
    until_last_prompt_ends_with(&slice.domain, &slice.capture, "Are you there?").await;
    slice.domain.stop_sdk_owners();
}


/// The previous host admitted a conversation but never supplied positive native-session evidence.
fn reopen_without_native_witness(slice: &Slice) -> (AgentId, String) {
    let mut original = slice.launch();
    let old_node = original.agent().clone();
    let refusal = require_recoverable_session(&slice.domain, &slice.backend, &TASK.into(), Provider::Claude,
        &AccountId::default_account(), slice.worktree.to_str().unwrap()).unwrap_err();
    assert!(refusal.to_string().contains("The original native session cannot be proven"), "{refusal}");
    slice.run(&user(), "cancel-original", Command::CancelTask { task_id: TASK.into() });
    original.stop().unwrap();
    drop(original);
    slice.run(&Actor::System, "close-original", Command::RecordNodeEnd {
        task_id: TASK.into(), end: NodeEnd::Closed, agent_id: Some(old_node.clone()),
    });
    slice.run(&user(), "reopen-original", Command::ReopenTask { task_id: TASK.into() });
    let input = slice.domain.state().unwrap().pending_reopening(&TASK.into()).unwrap().to_owned();
    (old_node, input)
}

fn prepared_reopened_launch(slice: &Slice, prompt: &str) -> Launch {
    Launch {
        task: TASK.into(), name: "Reopened release notes".into(), directory: slice.worktree.display().to_string(),
        prompt: prompt.into(), plugin: slice.plugin.clone(), options: LaunchOptions::default(), model: None,
        account: AccountId::default_account(),
    }
}

#[test]
fn reopening_unproven_claude_history_starts_a_new_node_and_clean_session_with_the_mission() {
    let slice = Slice::new();
    let (old_node, input) = reopen_without_native_witness(&slice);
    let before = slice.starts().len();
    let old_execution = slice.domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].iter().rev()
        .find_map(|record| match record { LifecycleRecord::Admitted { execution: Some(execution), .. } => Some(execution.clone()), _ => None }).unwrap();
    let reopening = prepare_reopening(&slice.domain, &slice.backend, &TASK.into(), Provider::Claude,
        &AccountId::default_account(), &slice.worktree, input, true).unwrap();
    let mission = "Write the complete release notes, keeping the approved release scope and requested corrections.";
    let mut replacement = start_bound(Arc::clone(&slice.domain), &slice.backend,
        prepared_reopened_launch(&slice, mission), capabilities(), None, Some(reopening)).unwrap();
    slice.settle(&mut replacement);
    let state = slice.domain.state().unwrap();
    let execution = lifecycle(&slice.domain).execution().unwrap().clone();
    assert_ne!(replacement.agent(), &old_node);
    assert_eq!(state.tasks[&TaskId::from(TASK)].agent_id.as_ref(), Some(replacement.agent()));
    assert_ne!(execution.conversation().as_str(), old_execution.conversation);
    assert_eq!(execution.generation(), 2, "the reopened task admits its second execution");
    assert!(state.pending_reopening(&TASK.into()).is_none(), "the first admission consumes the reopening");
    let starts = slice.starts();
    assert_eq!(starts.len(), before + 1, "the same admission starts exactly one new native process: {starts:?}");
    let fresh = starts.last().unwrap();
    assert!(fresh["resume"].is_null(), "unproven history cannot be resumed: {fresh}");
    assert_eq!(fresh["history"].as_u64(), Some(0), "the new native session starts clean");
    assert_eq!(slice.prompts().iter().filter(|prompt| prompt.as_str() == mission).count(), 1, "the new node receives its mission exactly once");
    replacement.stop().unwrap();
}

#[test]
fn a_cancellation_after_reopening_preparation_prevents_any_new_execution_admission() {
    stale_reopening_does_not_admit(false);
}

#[test]
fn another_reopening_after_preparation_cannot_admit_a_node_for_the_previous_reopening() {
    stale_reopening_does_not_admit(true);
}

fn stale_reopening_does_not_admit(reopen_again: bool) {
    let slice = Slice::new();
    let (_, input) = reopen_without_native_witness(&slice);
    let reopening = prepare_reopening(&slice.domain, &slice.backend, &TASK.into(), Provider::Claude,
        &AccountId::default_account(), &slice.worktree, input.clone(), true).unwrap();
    if reopen_again {
        slice.run(&Actor::Orchestrator { generation: 0 }, "keep-request-open", Command::CreateTask {
            plan_items: None, continues: None, task_id: "remaining-release-work".into(), request_id: "sdk-slice-request".into(),
            title: "Check the release checklist".into(), agent_id: None, review_prs: false,
            dependencies: BTreeSet::new(), stack: None,
            launch: Some(LaunchPlan::new("remaining-release-work", slice.worktree.to_str().unwrap())),
        });
    }
    slice.run(&user(), "cancel-after-preparation", Command::CancelTask { task_id: TASK.into() });
    if reopen_again {
        slice.run(&Actor::System, "cleanup-after-preparation", Command::RecordTaskCleanup { task_id: TASK.into() });
        assert_eq!(slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].state, TaskState::Cancelled);
        assert!(!slice.domain.state().unwrap().requests[&alondra_core::domain::RequestId::from("sdk-slice-request")].closed);
        slice.run(&user(), "reopen-after-preparation", Command::ReopenTask { task_id: TASK.into() });
        assert_ne!(slice.domain.state().unwrap().pending_reopening(&TASK.into()), Some(input.as_str()));
    }
    let before_records = slice.domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].clone();
    let before_starts = slice.starts();
    let result = start_bound(Arc::clone(&slice.domain), &slice.backend,
        prepared_reopened_launch(&slice, "This stale reopening must not reach Claude"), capabilities(), None, Some(reopening));
    let failure = match result {
        Ok(mut unexpected) => {
            unexpected.stop().unwrap();
            panic!("the stale reopening admitted a new execution");
        }
        Err(failure) => failure,
    };
    assert!(failure.to_string().contains("reopening changed before admission"), "{failure}");
    let state = slice.domain.state().unwrap();
    assert_eq!(state.lifecycle_journal[&TaskId::from(TASK)], before_records, "revalidation must happen before staging an admission or input");
    assert!(state.tasks[&TaskId::from(TASK)].agent_id.is_none());
    assert_eq!(slice.starts(), before_starts, "no native process starts for the stale reopening");
}

#[test]
fn reopening_a_session_whose_native_history_disappeared_starts_clean_without_a_resume_retry() {
    let slice = Slice::new();
    let mut original = slice.launch();
    slice.settle(&mut original);
    let old_node = original.agent().clone();
    let native = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    require_recoverable_session(&slice.domain, &slice.backend, &TASK.into(), Provider::Claude,
        &AccountId::default_account(), slice.worktree.to_str().unwrap()).unwrap();
    slice.run(&user(), "cancel-history-original", Command::CancelTask { task_id: TASK.into() });
    original.stop().unwrap();
    drop(original);
    slice.run(&system(), "close-history-original", Command::RecordNodeEnd {
        task_id: TASK.into(), end: NodeEnd::Closed, agent_id: Some(old_node.clone()),
    });
    let project: String = slice.worktree.to_string_lossy().chars().map(|character| if character.is_ascii_alphanumeric() { character } else { '-' }).collect();
    let witness = slice._home.join(".claude/projects").join(project).join(format!("{native}.jsonl"));
    assert!(witness.is_file(), "the native fixture supplied its real-shaped history witness");
    std::fs::remove_file(witness).unwrap();
    std::fs::remove_file(format!("{}.transcript-{native}", slice.capture.display())).unwrap();
    slice.run(&user(), "reopen-missing-history", Command::ReopenTask { task_id: TASK.into() });
    let input = slice.domain.state().unwrap().pending_reopening(&TASK.into()).unwrap().to_owned();
    let reopening = prepare_reopening(&slice.domain, &slice.backend, &TASK.into(), Provider::Claude,
        &AccountId::default_account(), &slice.worktree, input, true).unwrap();
    assert!(reopening.recovery.is_none(), "a retained session UUID cannot prove a missing native transcript");
    let mission = "Publish the retained release scope after the previous native history disappeared.";
    let mut replacement = start_bound(Arc::clone(&slice.domain), &slice.backend,
        prepared_reopened_launch(&slice, mission), capabilities(), None, Some(reopening)).unwrap();
    slice.settle(&mut replacement);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "the reopened task starts exactly one new native process");
    assert_ne!(replacement.agent(), &old_node);
    assert_ne!(starts[1]["session"].as_str(), Some(native.as_str()));
    assert!(starts[1]["resume"].is_null(), "missing history is never offered to a failing resume");
    assert_eq!(starts[1]["history"].as_u64(), Some(0));
    assert_eq!(slice.prompts().iter().filter(|prompt| prompt.as_str() == mission).count(), 1);
    assert!(slice.domain.store.lock().unwrap().start_retries().unwrap().is_empty());
    replacement.stop().unwrap();
}

#[test]
fn a_failed_reopening_workspace_cannot_replace_the_original_admissions_native_directory() {
    let slice = Slice::new();
    let mut original = slice.launch();
    slice.settle(&mut original);
    let old_node = original.agent().clone();
    let native = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    slice.run(&user(), "cancel-directory-original", Command::CancelTask { task_id: TASK.into() });
    original.stop().unwrap();
    drop(original);
    slice.run(&system(), "close-directory-original", Command::RecordNodeEnd {
        task_id: TASK.into(), end: NodeEnd::Closed, agent_id: Some(old_node.clone()),
    });
    slice.run(&user(), "reopen-directory-original", Command::ReopenTask { task_id: TASK.into() });
    let failed_workspace = slice._home.join("failed-reopening-workspace");
    std::fs::create_dir(&failed_workspace).unwrap();
    slice.domain.set_task_worktree(&TASK.into(), failed_workspace.to_str().unwrap()).unwrap();
    assert_eq!(original_directory(&slice.domain, &TASK.into()).unwrap().as_deref(), slice.worktree.to_str(),
        "only the admitted launch placement identifies the original native session's directory");
    require_recoverable_session(&slice.domain, &slice.backend, &TASK.into(), Provider::Claude,
        &AccountId::default_account(), slice.worktree.to_str().unwrap()).unwrap();
    assert!(require_recoverable_session(&slice.domain, &slice.backend, &TASK.into(), Provider::Claude,
        &AccountId::default_account(), failed_workspace.to_str().unwrap()).is_err());
    let input = slice.domain.state().unwrap().pending_reopening(&TASK.into()).unwrap().to_owned();
    let reopening = prepare_reopening(&slice.domain, &slice.backend, &TASK.into(), Provider::Claude,
        &AccountId::default_account(), &slice.worktree, input, true).unwrap();
    assert!(reopening.recovery.is_some());
    let mut replacement = start_bound(Arc::clone(&slice.domain), &slice.backend,
        prepared_reopened_launch(&slice, "Continue in the original admission's directory"), capabilities(), None, Some(reopening)).unwrap();
    slice.settle(&mut replacement);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1]["resume"].as_str(), Some(native.as_str()));
    assert_eq!(starts[1]["cwd"].as_str(), slice.worktree.to_str());
    assert!(starts[1]["history"].as_u64().unwrap() > 0);
    assert_ne!(replacement.agent(), &old_node);
    replacement.stop().unwrap();
}

/// Swaps the slice's native Claude for one that never gets a prompt, so its session never opens,
/// and returns where the working one waits to be put back.
fn hold_prompts(slice: &Slice) -> PathBuf {
    let fake = slice._home.join("native-fake");
    std::fs::copy(&slice.backend.native, &fake).unwrap();
    std::fs::write(&slice.backend.native, HOLDING_NATIVE).unwrap();
    fake
}

/// Tells the node `text` as the user does, through the daemon `domain`.
fn tell_through(domain: &DomainRuntime, slice: &Slice, driver: &Driver, text: &str) {
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": text})).unwrap();
    tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(domain, &slice._home, &user(), "Alice", driver.agent(), message, true))
        .unwrap();
}

/// Sends `signal` to every process of the host's group.
fn signal_host(driver: &Driver, signal: &str) {
    let group = driver.worker.host_group().unwrap();
    assert!(std::process::Command::new("/bin/kill").args([signal, "--", &format!("-{group}")]).status().unwrap().success());
}

/// The mission and then the user's message reached Claude, each once, and nobody was asked.
fn mission_then_message(slice: &Slice, domain: &DomainRuntime) {
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[0].ends_with("Write the release notes") && prompts[0].matches("Your session was cut off").count() == 1, "{prompts:?}");
    assert!(prompts[1].starts_with("[From Alice · ") && prompts[1].ends_with("Also update the changelog"), "{prompts:?}");
    assert_eq!(lifecycle(domain).resource(), Some(alondra_domain::ResourceEvidence::Available));
    assert!(domain.state().unwrap().tasks[&TaskId::from(TASK)].started);
    for prefix in [HANDOFF_DOUBT_PREFIX, HOST_LOST_PREFIX, SESSION_ENDED_PREFIX] {
        assert!(open_card(domain, prefix).is_none(), "no card {prefix}");
    }
}

// The daemon went down with a task whose Claude never opened its session. The host that takes it
// over gets its mission again and opens the session with it, and the message the user sent
// meanwhile follows.
#[test]
fn a_session_that_never_opened_gets_its_mission_and_the_queued_message_from_the_host_that_takes_over() {
    let slice = Slice::new();
    let fake = hold_prompts(&slice);
    let driver = slice.launch();
    assert_eq!(lifecycle(&slice.domain).resource(), Some(alondra_domain::ResourceEvidence::Reserved));
    kill_host_and_daemon(driver);
    std::fs::copy(&fake, &slice.backend.native).unwrap();
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    tell_through(&restarted, &slice, &resumed, "Also update the changelog");
    slice.settle(&mut resumed);
    mission_then_message(&slice, &restarted);
    resumed.stop().unwrap();
}

// A host took the mission but its Claude never opens the session. Once the deadline passes,
// another host takes its place and gets it again; the user's message waits and follows.
#[test]
fn a_host_whose_claude_never_opens_the_session_is_replaced_once_its_deadline_passes() {
    let slice = Slice::new();
    let fake = hold_prompts(&slice);
    let driver = slice.launch();
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    tell_through(&restarted, &slice, &resumed, "Also update the changelog");
    while resumed.pending_permits() > 0 {
        resumed.step(&|| false).unwrap();
    }
    assert!(resumed.waits_for_host(), "the host took the mission");
    let opening = Duration::from_secs(slice.backend.io_timeout_seconds);
    resumed.step_at(&|| false, Instant::now()).unwrap();
    assert!(!resumed.host_to_replace(), "nothing is replaced before the deadline");
    assert!(resumed.step_at(&|| false, Instant::now() + opening).is_err(), "past the deadline the host is stuck");
    assert!(resumed.host_to_replace());
    let mut recovered = resumed.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    std::fs::copy(&fake, &slice.backend.native).unwrap();
    slice.settle(&mut recovered);
    mission_then_message(&slice, &restarted);
    recovered.stop().unwrap();
}

// The task of the report: after the daemon restarted, the first input to the new host of a session
// that never opened went without a receipt (here the host is stopped and the handoff waits out its
// I/O timeout).
// The host is replaced by itself, however many ended before, and nobody is asked.
#[test]
fn a_takeover_whose_first_input_is_uncertain_relaunches_by_itself_without_a_card() {
    let slice = Slice::new();
    let fake = hold_prompts(&slice);
    let driver = slice.launch();
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    tell_through(&restarted, &slice, &resumed, "Also update the changelog");
    signal_host(&resumed, "-STOP");
    while resumed.pending_permits() > 0 {
        resumed.step(&|| false).unwrap();
    }
    assert!(resumed.step(&|| false).is_err(), "the input in doubt ends this host");
    resumed.relaunches = RELAUNCH_WAITS.len();
    assert!(resumed.host_to_replace(), "a session that never opened is relaunched however many hosts ended before");
    signal_host(&resumed, "-CONT");
    let mut recovered = resumed.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    std::fs::copy(&fake, &slice.backend.native).unwrap();
    slice.settle(&mut recovered);
    mission_then_message(&slice, &restarted);
    recovered.stop().unwrap();
}

// A session that never opened, whose host ended with no host to resume it: the card asks the user
// whether to send the mission again. «Send it again» reaches the Claude of the host that takes the
// execution over, though its session has not opened yet.
#[test]
fn send_again_on_the_card_of_a_mission_whose_session_never_opened_reaches_claude() {
    let slice = Slice::new();
    let fake = hold_prompts(&slice);
    let driver = slice.launch();
    kill_host_and_daemon(driver);
    std::fs::copy(&fake, &slice.backend.native).unwrap();
    let restarted = slice.restart();
    end_unowned_host(Arc::clone(&restarted), &slice.backend, TASK.into()).unwrap();
    let card = open_card(&restarted, HANDOFF_DOUBT_PREFIX).expect("the user is asked whether to send it again");
    assert_eq!(lifecycle(&restarted).resource(), Some(alondra_domain::ResourceEvidence::Reserved));
    answer(&restarted, &card.id, RESEND);
    let mut resumed = slice.take_over(&restarted);
    slice.settle(&mut resumed);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(prompts[0].ends_with("Write the release notes"), "{prompts:?}");
    assert_eq!(lifecycle(&restarted).resource(), Some(alondra_domain::ResourceEvidence::Available));
    resumed.stop().unwrap();
}

// 2026-11-03, t-deps-done-still-waiting: every host the daemon put in place took an input and lost
// it, because its Claude never started. Offering the input reset the count of replacements, so the
// task was resumed without end. After RELAUNCH_WAITS hosts in a row whose Claude never ran, the
// task holds on the card that says Alondra could not resume its session.
#[tokio::test(flavor = "multi_thread")]
async fn a_claude_that_never_starts_in_its_resumed_session_ends_on_the_lost_host_card_and_not_in_endless_hosts() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    kill_host_and_daemon(driver);
    std::fs::remove_file(format!("{}.transcript-{session}", slice.capture.display())).unwrap();
    let restarted = slice.restart();
    let resumed = slice.take_over(&restarted);
    let first_generation = u64::try_from(owner_generation(&slice.database)).unwrap();
    restarted.enable_sdk(slice.backend.clone());
    let mut changed = restarted.subscribe();
    let owner = Owner::spawn(resumed);
    let reader = Arc::clone(&restarted);
    let database = slice.database.clone();
    changed.wait_for(|_| {
        let generation = u64::try_from(owner_generation(&database)).unwrap();
        assert!(generation <= first_generation + RELAUNCH_WAITS.len() as u64, "hosts keep replacing each other: generation {generation}");
        open_card(&reader, HOST_LOST_PREFIX).is_some()
    }).await.unwrap();
    assert_eq!(u64::try_from(owner_generation(&slice.database)).unwrap(), first_generation + RELAUNCH_WAITS.len() as u64, "{RELAUNCH_WAITS:?} replacements, then the card");
    let card = open_card(&restarted, HOST_LOST_PREFIX).expect("the task holds on the card that says its session could not be resumed");
    assert!(card.body.contains("Claude did not start in its session"), "{}", card.body);
    assert!(open_card(&restarted, HANDOFF_DOUBT_PREFIX).is_none(), "no card asks about a message");
    drop(owner);
}
