//! A `claude --bg` node's move through the real owner: the daemon's store and commands, the
//! pinned Node host with the official SDK, and the fake native Claude, which records every start
//! with the session it resumed and its folder. The node's `claude --bg` process is a real child
//! process, named by its identity; Claude Code's `claude stop` is the double here. Every wait is
//! on a fact the domain publishes, never on a clock.
use super::*;
use crate::domain_store::StoreError;
use crate::test_support::Directory;
use std::collections::BTreeSet;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const TASK: &str = "t-release-notes";
const SESSION: &str = "11111111-2222-4333-8444-555555555555";
const CARD: &str = "q-format";

/// The node's `claude --bg` process: a real process this test owns and ends. `claude stop` ends
/// it, unless it is told to answer some stops while the process goes on, as Claude Code can answer
/// before its process exited.
struct Bg {
    child: Mutex<Option<std::process::Child>>,
    process: HandoverProcess,
    early_answers: AtomicUsize,
    stops: AtomicUsize,
}

impl Bg {
    fn spawn() -> Arc<Self> {
        let child = std::process::Command::new("/bin/sleep").arg("600").stdin(std::process::Stdio::null()).spawn().unwrap();
        let member = alondra_process_group::process(i32::try_from(child.id()).unwrap()).unwrap().unwrap();
        Arc::new(Self { child: Mutex::new(Some(child)), process: identity(member), early_answers: AtomicUsize::new(0), stops: AtomicUsize::new(0) })
    }

    fn running(&self) -> bool {
        alondra_process_group::alive(&member(&self.process)).unwrap()
    }

    fn end(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            child.wait().unwrap();
        }
    }
}

impl Drop for Bg {
    fn drop(&mut self) {
        self.end();
    }
}

impl BgSessions for Bg {
    fn seen(&self, _: &DomainRuntime, session: &str) -> Option<HandoverProcess> {
        (session == SESSION && self.running()).then_some(self.process)
    }

    fn listed<'a>(&'a self, session: &'a str) -> Answer<'a, Result<Option<HandoverProcess>, String>> {
        Box::pin(async move { Ok((session == SESSION && self.running()).then_some(self.process)) })
    }

    fn stop<'a>(&'a self, _: &'a str) -> Answer<'a, Result<(), String>> {
        Box::pin(async move {
            self.stops.fetch_add(1, Ordering::SeqCst);
            if self.early_answers.load(Ordering::SeqCst) > 0 {
                self.early_answers.fetch_sub(1, Ordering::SeqCst);
            } else {
                self.end();
            }
            Ok(())
        })
    }

    fn alive(&self, process: &HandoverProcess) -> bool {
        alondra_process_group::alive(&member(process)).unwrap()
    }
}

struct World {
    domain: Arc<DomainRuntime>,
    bg: Arc<Bg>,
    backend: crate::sdk_owner::SdkBackend,
    capture: PathBuf,
    worktree: PathBuf,
    database: PathBuf,
    home: Directory,
    _state: Directory,
}

fn envelope(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command }
}

fn run(domain: &DomainRuntime, actor: &Actor, id: &str, command: Command) -> Result<CommandOutcome, StoreError> {
    let state = domain.state().unwrap();
    domain.execute_with(actor, &envelope(&state, id, command), None, None, || Ok(()))
}

fn user() -> Actor {
    Actor::User { user_id: "david".into() }
}

fn node() -> Actor {
    Actor::Agent { agent_id: SESSION.into() }
}

fn event(domain: &DomainRuntime, happened: NodeEvent) {
    crate::domain_agent_lifecycle::node_event(domain, &AgentId(SESSION.into()), happened).unwrap();
}

impl World {
    /// A task whose `claude --bg` node works in its worktree, its session's transcript on disk.
    fn new() -> Self {
        let home = Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let state = Directory::for_unix_socket();
        let worktree = home.join("worktree");
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(home.join(crate::plugins::NODE)).unwrap();
        let (backend, capture) = crate::sdk_owner::tests::fixture_backend(&home, &state);
        std::fs::write(format!("{}.transcript-{SESSION}", capture.display()), "the claude --bg session's first prompt\n").unwrap();
        let database = home.join("domain.sqlite");
        let bg = Bg::spawn();
        let domain = Self::boot(&home, &database, &backend, &bg);
        let world = Self { domain, bg, backend, capture, worktree, database, home, _state: state };
        let orchestrator = Actor::Orchestrator { generation: 0 };
        run(&world.domain, &orchestrator, "request", Command::CreateRequest { request_id: "r".into(), body: "Write the release notes".into(), auto_merge: false, maintenance: false, message: false }).unwrap();
        run(&world.domain, &orchestrator, "title", Command::TitleRequest { request_id: "r".into(), title: "Release notes".into() }).unwrap();
        run(&world.domain, &orchestrator, "task", Command::CreateTask {
            plan_items: None, task_id: TASK.into(), request_id: "r".into(), title: "Write the release notes".into(), agent_id: None, review_prs: false,
            dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", world.worktree.to_str().unwrap())),
        }).unwrap();
        run(&world.domain, &Actor::System, "register", Command::RegisterAgent { agent_id: SESSION.into(), name: "release-notes".into(), at: 0 }).unwrap();
        run(&world.domain, &user(), "assign", Command::AssignTask { task_id: TASK.into(), agent_id: SESSION.into(), model: None }).unwrap();
        run(&world.domain, &node(), "start", Command::StartTask { task_id: TASK.into() }).unwrap();
        world.domain.connect_node(serde_json::from_value(serde_json::json!({"session_id": SESSION, "version": "2.1.300", "cwd": world.worktree})).unwrap(), 1);
        event(&world.domain, NodeEvent::TurnStarted);
        world
    }

    fn boot(home: &Path, database: &Path, backend: &crate::sdk_owner::SdkBackend, bg: &Arc<Bg>) -> Arc<DomainRuntime> {
        let domain = Arc::new(DomainRuntime::open_at(home, database).unwrap());
        domain.enable_sdk(backend.clone());
        domain.handovers.use_sessions(Arc::clone(bg) as Arc<dyn BgSessions>);
        domain
    }

    /// The node asks the user which format to use and its turn ends: it rests on its card.
    fn rest(&self) {
        run(&self.domain, &node(), "ask", Command::AskQuestion {
            attachments: vec![], interaction_id: CARD.into(), scope: Scope::Task { task_id: TASK.into() }, recipient: Recipient::User,
            body: "Which changelog format?".into(), blocking: false,
            options: vec![QuestionOption { id: "keep".into(), label: "Keep a Changelog".into(), description: None },
                QuestionOption { id: "plain".into(), label: "Plain list".into(), description: None }],
            card: CardSpec::new(CardKind::Decide, "Choose the changelog format").why("The release notes follow it"),
        }).unwrap();
        event(&self.domain, NodeEvent::TurnEnded);
        assert_eq!(self.domain.state().unwrap().node_phase(&AgentId(SESSION.into())), Some(&NodePhase::WaitingUser { in_turn: false }));
    }

    fn answer(&self) {
        run(&self.domain, &user(), "answer", Command::AnswerQuestion { attachments: vec![], interaction_id: CARD.into(), option_id: "plain".into(), inputs: Default::default() }).unwrap();
        assert_eq!(self.waiting_messages(), 1, "the answer waits in the outbox for the node");
    }

    fn waiting_messages(&self) -> usize {
        self.domain.store.lock().unwrap().pending_messages().unwrap().iter().filter(|message| message.agent_id.as_deref() == Some(SESSION)).count()
    }

    /// A daemon that died: what it ran is gone, a new one opens the same database and, as at
    /// startup, takes over the SDK executions before it goes on with the moves.
    async fn restart(&mut self) {
        self.domain.stop_sdk_owners();
        self.domain = Self::boot(&self.home, &self.database, &self.backend, &self.bg);
        let (domain, backend) = (Arc::clone(&self.domain), self.backend.clone());
        tokio::task::spawn_blocking(move || crate::sdk_owner::adopt_all(&domain, &backend)).await.unwrap();
    }

    fn step(&self) -> Option<HandoverStep> {
        handover_of(&self.domain, &AgentId(SESSION.into())).map(|handover| handover.step)
    }

    fn starts(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(format!("{}.starts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    fn prompts(&self) -> Vec<String> {
        std::fs::read_to_string(format!("{}.prompts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    /// How the store left the launch of the task's current SDK execution.
    fn launch_status(&self) -> Option<String> {
        rusqlite::Connection::open(&self.database).unwrap()
            .query_row("SELECT status FROM lifecycle_dispatches WHERE task_id=?1 AND kind='launch' ORDER BY id DESC LIMIT 1", [TASK], |row| row.get(0))
            .ok()
    }

    fn sdk_node(&self) -> Option<AgentId> {
        self.domain.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().filter(|agent| agent.0 != SESSION)
    }

    /// How many live processes own the task: its `claude --bg` process and its SDK host.
    fn live_owners(&self) -> usize {
        usize::from(self.bg.running()) + usize::from(self.sdk_node().is_some_and(|agent| self.domain.owns_sdk_host(&agent)))
    }

    /// Waits on the domain until the SDK node's first turn ended with Claude's answer.
    async fn until_answered(&self) {
        let mut changed = self.domain.subscribe();
        while !self.domain.read().state().interactions.keys().any(|card| card.0.starts_with(&format!("{TURN_ENDED_PREFIX}{TASK}-"))) {
            changed.changed().await.unwrap();
        }
    }

    fn verdict(&self) -> Verdict {
        dry_run(&self.domain).unwrap().into_iter().find(|row| row.task.0 == TASK).unwrap().verdict
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
    }
}

/// What a finished move leaves: the same Claude session resumed once through the SDK, in the
/// node's folder, told the answer that moved it, and the old node closed.
async fn assert_moved(world: &World) {
    world.until_answered().await;
    assert_eq!(world.step(), Some(HandoverStep::Done));
    assert!(!world.bg.running(), "the claude --bg process is gone");
    let starts = world.starts();
    assert_eq!(starts.len(), 1, "the SDK starts Claude once: {starts:?}");
    assert_eq!(starts[0]["resume"].as_str(), Some(SESSION), "with --resume of the node's own session: {starts:?}");
    assert_eq!(starts[0]["history"], 1, "its transcript is the one the claude --bg session left");
    assert_eq!(starts[0]["cwd"].as_str().map(PathBuf::from), Some(world.worktree.canonicalize().unwrap()), "in the node's folder");
    let prompts = world.prompts();
    assert_eq!(prompts.len(), 1, "the answer reaches Claude once: {prompts:?}");
    assert!(prompts[0].contains("Plain list"), "the answer that moved the node is its first input: {}", prompts[0]);
    let agent = world.sdk_node().expect("the SDK node holds the task");
    assert!(world.domain.owns_sdk_host(&agent));
    let state = world.domain.state().unwrap();
    let lifecycle = replay(&TaskId::from(TASK), &state.lifecycle_journal[&TaskId::from(TASK)]).unwrap();
    assert_eq!(lifecycle.execution().map(|stamp| stamp.host().as_str().to_owned()).as_deref(), Some(crate::sdk_owner::HOST));
    assert_eq!(state.node_phase(&AgentId(SESSION.into())), Some(&NodePhase::Closed), "the claude --bg node is closed");
    assert_eq!(world.waiting_messages(), 0, "the answer it carried is handed");
    assert!(crate::domain_store::handover_copy_path(&world.database.canonicalize().unwrap()).exists(), "the database was copied before the first move");
    assert!(!crate::node_revival::needed(&world.domain, SESSION), "nothing brings the old node back");
}

// A node at rest moves with the next input for it: the user's answer to its card stops its
// claude --bg process, its exit is seen by the process's identity, and the SDK resumes its same
// session with the answer as the first input.
#[tokio::test]
async fn a_resting_node_moves_with_the_answer_to_its_card_and_resumes_the_same_claude_session() {
    let world = World::new();
    world.rest();
    assert_eq!(world.verdict(), Verdict::Passes(Reason::NextMessage));
    world.answer();
    assert_eq!(world.verdict(), Verdict::Passes(Reason::MessageWaiting));
    tick(&world.domain).await;
    assert_moved(&world).await;
    assert_eq!(world.bg.stops.load(Ordering::SeqCst), 1);
    assert_eq!(world.verdict(), Verdict::Stays(Reason::ThroughSdk));
}

// A daemon that dies right after any step of a move comes back and goes on from the step it
// recorded: the claude --bg process is stopped once, Claude starts once with the same session and
// the answer reaches it once.
#[tokio::test]
async fn a_crash_after_each_step_of_the_move_goes_on_from_that_step_and_repeats_nothing() {
    for step in [HandoverStep::Planned, HandoverStep::BgStopped, HandoverStep::SdkAdmitted] {
        let mut world = World::new();
        world.rest();
        world.answer();
        *world.domain.handovers.crash_after.lock().unwrap() = Some(step);
        tick(&world.domain).await;
        assert_eq!(world.step(), Some(step), "the move stopped at {step:?}");
        assert!(world.live_owners() <= 1, "two live owners after a crash at {step:?}");
        assert!(world.starts().is_empty(), "no SDK Claude ran before the crash at {step:?}");
        world.restart().await;
        tick(&world.domain).await;
        assert_eq!(world.launch_status().as_deref(), Some("handed"), "the launch goes out after the crash at {step:?}");
        assert_moved(&world).await;
        assert_eq!(world.bg.stops.load(Ordering::SeqCst), 1, "claude --bg is stopped once across the crash at {step:?}");
    }
}

// A node in a turn stays on claude --bg: what it is told reaches its session down its connection,
// and nothing stops it. Once it rests, the next input moves it.
#[tokio::test]
async fn a_node_in_a_turn_stays_on_claude_bg_until_it_rests() {
    let world = World::new();
    let connection = crate::node_push::testing::Node::connect(&world.domain, SESSION);
    assert!(matches!(tell(&world.domain, SESSION, "David", "Use the new endpoint", true), Ok(Told::Pushed(_))), "it is told through its connection, not moved");
    assert_eq!(world.verdict(), Verdict::Waits(Reason::InTurn));
    while connection.texts().is_empty() {
        tokio::task::yield_now().await;
    }
    assert_eq!(connection.texts(), ["Use the new endpoint"]);
    tick(&world.domain).await;
    assert_eq!(world.step(), None, "a working node has no move");
    assert!(world.bg.running() && world.bg.stops.load(Ordering::SeqCst) == 0, "nothing stopped it");
    world.rest();
    world.answer();
    tick(&world.domain).await;
    assert_moved(&world).await;
}

// `claude stop` can answer before the process exited: the SDK takes the session only once the
// process is seen gone by its identity, never while the claude --bg process still runs.
#[tokio::test]
async fn the_sdk_takes_the_session_only_once_the_claude_bg_process_has_exited() {
    let world = World::new();
    world.rest();
    world.answer();
    world.bg.early_answers.store(1, Ordering::SeqCst);
    tick(&world.domain).await;
    assert!(world.bg.running(), "claude stop answered, the process goes on");
    assert_eq!(world.step(), Some(HandoverStep::Planned), "the move waits for the process's exit");
    assert!(world.sdk_node().is_none() && world.starts().is_empty(), "no SDK node runs beside the claude --bg process");
    tick(&world.domain).await;
    assert_moved(&world).await;
    assert_eq!(world.bg.stops.load(Ordering::SeqCst), 2);
}

// A node brought back moves instead of being restarted: a paused node's resume opens its session
// through the SDK, and nothing calls claude respawn.
#[tokio::test]
async fn a_paused_node_moves_to_the_sdk_as_it_comes_back() {
    let world = World::new();
    event(&world.domain, NodeEvent::Paused { pause: Pause {
        reason: PauseReason::Usage, since: alondra_core::now_millis(), cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false,
    } });
    assert_eq!(world.verdict(), Verdict::Passes(Reason::BackFromPause));
    assert!(matches!(tell(&world.domain, SESSION, crate::node_push::ALONDRA, "Your usage is back: carry on with your task", true), Ok(Told::Moving)));
    assert_eq!(advance(&world.domain, &AgentId(SESSION.into())).await, Ok(HandoverStep::Done));
    world.until_answered().await;
    assert!(!world.bg.running());
    let starts = world.starts();
    assert_eq!(starts[0]["resume"].as_str(), Some(SESSION), "{starts:?}");
    assert!(world.prompts()[0].ends_with("Your usage is back: carry on with your task"), "{:?}", world.prompts());
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig { cases: 8, failure_persistence: None, ..proptest::prelude::ProptestConfig::default() })]

    /// Whatever step the daemon dies at and however often `claude stop` answers early, the task
    /// never has its claude --bg process and its SDK host alive at once, and the move ends.
    #[test]
    fn a_task_never_has_two_live_owners(crash in proptest::option::of(0..3usize), early in 0..3usize) {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut world = World::new();
            world.rest();
            world.answer();
            world.bg.early_answers.store(early, Ordering::SeqCst);
            *world.domain.handovers.crash_after.lock().unwrap() = crash.map(|at| [HandoverStep::Planned, HandoverStep::BgStopped, HandoverStep::SdkAdmitted][at]);
            for _ in 0..=early {
                tick(&world.domain).await;
                assert!(world.live_owners() <= 1, "two live owners");
            }
            if crash.is_some() {
                world.restart().await;
                for _ in 0..=early {
                    tick(&world.domain).await;
                    assert!(world.live_owners() <= 1, "two live owners after the restart");
                }
            }
            assert_moved(&world).await;
            assert!(world.live_owners() == 1, "the SDK host alone owns the task");
        });
    }
}
