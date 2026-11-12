use super::*;
use crate::admission::{Hold, Key, Limit, SPACING, Tuning, Waiting};
use crate::machine_state::NoRoom;
use crate::node_push::testing::Node;
use crate::node_reports::{Envelope, Report};
use alondra_core::{Machine, MemoryCompressor, MemoryPressure};
use alondra_core::domain::{API_RETRIES, Actor, AgentId, ApiRetry, Command, CommandEnvelope, NodePhase, OUTAGE_PROBE, PROTOCOL_VERSION, Pause, PauseReason, TaskId, TaskPriority};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::Arc;

const MIN: Millis = 60_000;
const SAMPLE: Millis = 10_000;

#[test]
fn a_pause_by_the_user_never_returns_by_itself_and_a_declared_wait_returns_without_waiting_out_any_time() {
    let paused = |reason| Paused { task: "t".into(), reason, since: 0, cut: None, turn_since: None, cleanup: Cleanup::Done, released: false };
    assert!(!paused(Reason::ByUser).returns(true, false, 10 * 60 * MIN));
    assert!(paused(Reason::Memory).returns(true, false, 0));
    assert!(!paused(Reason::Usage).returns(false, false, 0));
    let declared = paused(Reason::Declared { reason: "build queue".into() });
    assert!(declared.returns(true, false, 0), "the gate's measures alone say when it has room");
    let retry = alondra_core::domain::ApiRetry { error: "server_error".into(), detail: None, attempt: 1, at: 30_000, outage: false };
    let failed = paused(Reason::ApiError(retry));
    assert!(!failed.returns(true, false, 29_999), "an API error waits for its retry");
    assert!(failed.returns(true, false, 30_000));
    assert!(failed.returns(true, true, 0), "the model answering another node lets it go at once");
}

#[test]
fn the_resume_message_says_when_and_what_was_cut() {
    let paused = Paused { task: "t".into(), reason: Reason::Memory, since: 0, cut: Some("bazel test //crates/...".into()), turn_since: None, cleanup: Cleanup::Done, released: false };
    let text = resume_text(&paused, 0);
    assert!(text.starts_with("Alondra stopped you at "), "{text}");
    assert!(text.contains("because the Mac ran out of memory, in the middle of `bazel test //crates/...`."), "{text}");
    assert!(text.ends_with("The Mac has room again: carry on with your task and run again what was cut."), "{text}");
}

struct Home(crate::test_support::Directory);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: crate::auth::random_hex(8).into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

const SDK_TASK: &str = "t-sdk-paused";

/// Drives an owner until it has nothing to send and its host owes nothing.
fn settle(driver: &mut crate::sdk_owner::Driver) {
    while driver.pending_permits() > 0 || driver.waits_for_host() {
        if driver.pending_permits() > 0 {
            driver.step(&|| false).unwrap();
        } else {
            driver.observe().unwrap();
        }
    }
}

/// A task of the Agent SDK whose turn runs, on the pinned host with the fake native Claude. Its
/// owner is driven here step by step, so nothing reaches Claude unless a step hands it over.
struct SdkNode {
    driver: crate::sdk_owner::Driver,
    domain: Arc<DomainRuntime>,
    capture: PathBuf,
    backend: crate::sdk_owner::SdkBackend,
    home: crate::test_support::Directory,
    _state: crate::sdk_owner::tests::HostRoot,
}

impl SdkNode {
    /// A node whose first turn the fake Claude holds open.
    fn working() -> Self {
        let mut node = Self::told("consume-and-hold");
        while node.lifecycle().activity() != alondra_domain::ActivityView::Working {
            node.driver.observe().unwrap();
        }
        node
    }

    /// A node whose first turn ended: it rests until it is told something.
    fn resting() -> Self {
        let mut node = Self::told("Write the notes");
        settle(&mut node.driver);
        node
    }

    fn told(prompt: &str) -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let state = crate::sdk_owner::tests::HostRoot::new();
        let worktree = home.join("worktree");
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        let (backend, capture) = crate::sdk_owner::tests::fixture_backend(&home, &state);
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        domain.enable_sdk(backend.clone());
        run(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateRequest {
            request_id: "r".into(), body: "Write the release notes".into(), auto_merge: false, maintenance: false, message: false });
        run(&domain, Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r".into(), title: "Release notes".into() });
        run(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
            task_id: SDK_TASK.into(), request_id: "r".into(), title: "Write the release notes".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", worktree.to_str().unwrap())) });
        let driver = crate::sdk_owner::start(Arc::clone(&domain), &backend, crate::sdk_owner::Launch {
            task: SDK_TASK.into(), name: "Release notes".into(), directory: worktree.to_str().unwrap().into(),
            prompt: prompt.into(), plugin: home.join("plugin"), options: Default::default(),
            model: None, account: alondra_core::accounts::AccountId::default_account(),
        }).unwrap();
        domain.driven_by_hand.lock().unwrap().insert(driver.agent().clone());
        domain.live.gate.ignore_mac();
        domain.live.machine_watch.measure_disk(crate::disk::Space { total: 500_000_000_000, free: 100_000_000_000, purgeable: 0 });
        Self { driver, domain, capture, backend, home, _state: state }
    }

    /// One more node of the same host root whose first turn the fake Claude holds open.
    fn beside(&self, task: &str) -> crate::sdk_owner::Driver {
        let mut driver = self.beside_told(task, "consume-and-hold");
        while self.activity(task) != alondra_domain::ActivityView::Working {
            driver.observe().unwrap();
        }
        driver
    }

    /// One more node of the same host root, resting after its first turn.
    fn beside_resting(&self, task: &str) -> crate::sdk_owner::Driver {
        let mut driver = self.beside_told(task, "Write the notes");
        settle(&mut driver);
        driver
    }

    fn beside_told(&self, task: &str, prompt: &str) -> crate::sdk_owner::Driver {
        let worktree = self.home.join(task);
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        run(&self.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
            task_id: task.into(), request_id: "r".into(), title: format!("Task {task}"), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", worktree.to_str().unwrap())) });
        let driver = crate::sdk_owner::start(Arc::clone(&self.domain), &self.backend, crate::sdk_owner::Launch {
            task: task.into(), name: format!("Task {task}"), directory: worktree.to_str().unwrap().into(),
            prompt: prompt.into(), plugin: self.home.join("plugin"), options: Default::default(),
            model: None, account: alondra_core::accounts::AccountId::default_account(),
        }).unwrap();
        self.domain.driven_by_hand.lock().unwrap().insert(driver.agent().clone());
        driver
    }

    fn tell(&self, task: &str, text: &str) {
        crate::domain_messages::tell_sdk_node(&self.domain, &task.into(), crate::node_push::ALONDRA, text, &format!("tell-{task}"), false).unwrap();
    }

    fn set_limit(&self, limit: u32) {
        self.domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(limit)) });
    }

    fn gate(&self) -> crate::admission::Snapshot {
        self.domain.live.gate.snapshot(&self.domain, alondra_core::now_millis()).unwrap()
    }

    /// The tasks whose next turn waits for a slot, in the gate's order.
    fn waiting_turns(&self) -> Vec<String> {
        self.gate().queue.into_iter().filter_map(|waiting| match waiting.key { Key::Turn(task) => Some(task.0), _ => None }).collect()
    }

    fn cards(&self) -> Vec<alondra_core::domain::InteractionId> {
        self.domain.state().unwrap().interactions.keys().cloned().collect()
    }

    fn activity(&self, task: &str) -> alondra_domain::ActivityView {
        self.domain.state().unwrap().lifecycle_of(&task.into()).unwrap().activity()
    }

    fn lifecycle(&self) -> alondra_domain::TaskLifecycle {
        let read = self.domain.read();
        alondra_core::domain::replay(&SDK_TASK.into(), &read.state().lifecycle_journal[&TaskId::from(SDK_TASK)]).unwrap()
    }

    fn agent(&self) -> String {
        self.driver.agent().0.clone()
    }

    /// The prompts Claude got, besides the wake Alondra sends after a turn that ended empty.
    fn prompts(&self) -> Vec<String> {
        crate::sdk_owner::tests::not_wakes(std::fs::read_to_string(format!("{}.prompts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()))
    }

    fn pending_inputs(&self) -> usize {
        self.lifecycle().pending_inputs().len()
    }

    /// Steps the owner as its thread does, until `done` holds: each step sends what may be sent,
    /// or reads what the host has.
    fn step_until(&mut self, done: impl Fn(&Self) -> bool) {
        while !done(self) {
            self.driver.step(&|| true).unwrap();
        }
    }

    /// A few steps of the owner: each sends whatever it may send.
    fn step(&mut self) {
        for _ in 0..4 {
            self.driver.step(&|| true).unwrap();
        }
    }

    /// How many of the task's inputs the owner claimed to hand to its host.
    fn dispatched_inputs(&self) -> i64 {
        rusqlite::Connection::open(crate::domain_database_migration::active_path(&self.home)).unwrap()
            .query_row("SELECT count(*) FROM lifecycle_dispatches WHERE task_id=?1 AND kind='input' AND status NOT IN ('prepared','revoked')", [SDK_TASK], |row| row.get(0)).unwrap()
    }

    /// Steps the owner until Claude got a prompt that says `text`.
    fn until_told(&mut self, text: &str) {
        self.step_until(|node| node.prompts().iter().any(|prompt| prompt.contains(text)));
    }

    fn turn_ended(&self) -> bool {
        self.domain.read().state().lifecycle_journal[&TaskId::from(SDK_TASK)].iter()
            .any(|record| matches!(record, alondra_core::domain::LifecycleRecord::TurnEnded { .. }))
    }

    /// One look of the gate, which lets the node go on once it has a slot.
    async fn gate_look(&self) {
        crate::admission::pass_at(&self.domain, &self.home, alondra_core::now_millis()).await.unwrap();
    }

    /// The user pauses the node from its task; its turn is interrupted and ends.
    fn pause(&mut self) {
        self.domain.live.gate.pause_by_user(&self.domain, &self.agent(), alondra_core::now_millis()).unwrap();
        self.step_until(Self::turn_ended);
        let state = self.domain.state().unwrap();
        assert!(matches!(state.node_pause(&AgentId(self.agent())), Some(Pause { reason: PauseReason::ByUser, .. })), "{:?}", state.node_pause(&AgentId(self.agent())));
        assert_eq!(state.node_phase(&AgentId(self.agent())), Some(&NodePhase::ThroughSdk), "its task's lifecycle holds the pause, not its phase");
    }

    /// Each native Claude the hosts started, with the session it runs and the one it resumed.
    fn starts(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(format!("{}.starts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    /// The node's host dies while it is paused and no owner of the daemon is left to replace it.
    fn lose_host(&mut self) {
        std::fs::DirBuilder::new().mode(0o700).create(self.home.join("plugin")).unwrap();
        crate::sdk_owner::tests::kill_host(&self.driver);
        self.domain.driven_by_hand.lock().unwrap().remove(self.driver.agent());
        self.driver.let_go();
        assert!(!self.domain.owns_sdk_host(&AgentId(self.agent())));
    }

    fn deliver(&self) {
        run(&self.domain, Actor::Agent { agent_id: self.driver.agent().clone() }, Command::PresentResult {
            attachments: vec![], task_id: SDK_TASK.into(), interaction_id: "result".into(), body: "Done".into(), is_final: true, nothing_to_review: false,
            targets: vec![alondra_core::domain::DeliveryTarget::Text { text: "Done".into() }], checklist: vec![],
            card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
    }

    /// The user sends the delivery back with changes.
    fn get_it_back(&self) {
        run(&self.domain, user(), Command::SubmitFeedback { merge_failure: None, attachments: vec![], presentation_id: "result".into(),
            targets: vec![alondra_core::domain::DeliveryTarget::Text { text: "Done".into() }], body: "Pass the MCP variables too".into(),
            instruction: alondra_core::domain::Instruction::Changes });
    }

    fn host_lost_card(&self) -> Option<alondra_core::domain::Interaction> {
        crate::sdk_owner::tests::open_card(&self.domain, alondra_core::domain::HOST_LOST_PREFIX)
    }
}

impl Drop for SdkNode {
    fn drop(&mut self) {
        let _ = self.driver.stop();
    }
}

#[tokio::test]
async fn a_paused_sdk_node_receives_nothing_until_it_is_resumed_and_then_gets_the_resume_text_and_what_it_was_told() {
    let mut node = SdkNode::working();
    node.pause();
    crate::domain_messages::tell_sdk_node(&node.domain, &SDK_TASK.into(), crate::node_push::ALONDRA, "Use the short changelog format", "tell-while-paused", false).unwrap();
    node.step();
    assert_eq!(node.dispatched_inputs(), 0, "the owner hands nothing to the host while the node is paused");
    assert_eq!(node.pending_inputs(), 2, "the resume text and what it was told wait for the end of the pause");
    assert_eq!(node.prompts(), ["consume-and-hold"]);

    assert!(node.domain.live.gate.resume_now(&node.domain, &node.agent(), alondra_core::now_millis()));
    node.gate_look().await;
    node.until_told("Use the short changelog format");
    let resumed = node.prompts().iter().filter(|prompt| prompt.contains("resumed you")).count();
    assert_eq!(resumed, 1, "{:?}", node.prompts());
    assert!(node.domain.live.gate.paused(&node.domain, &node.agent()).is_none());
    assert_eq!(node.domain.state().unwrap().node_pause(&AgentId(node.agent())), None);
}

/// Pausing interrupts the turn with what the node goes on with already told: no card asks the user
/// how it goes on, its task says it is paused, and resuming it is the user's one step.
#[tokio::test]
async fn pausing_an_sdk_node_raises_no_card_and_its_task_shows_it_paused() {
    let mut node = SdkNode::working();
    node.pause();
    node.step();
    let state = node.domain.state().unwrap();
    let cards: Vec<_> = state.interactions.keys()
        .filter(|id| [alondra_core::domain::TURN_ENDED_PREFIX, alondra_core::domain::STALLED_PREFIX].iter().any(|prefix| id.0.starts_with(prefix)))
        .collect();
    assert!(cards.is_empty(), "a pause asks nothing: {cards:?}");
    let read = node.domain.read();
    let live = alondra_core::views::live::LiveSnapshot::default();
    let tasks = alondra_core::views::build("tasks", alondra_core::views::RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: alondra_core::views::Client::Tui,
        lang: alondra_core::views::text::Lang::En, transport: alondra_core::views::Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap();
    let tasks = serde_json::to_string(&tasks).unwrap();
    assert!(tasks.contains("\"Paused\""), "{tasks}");
    assert!(read.index().waiting().is_empty(), "the pause waits in nobody's inbox: {:?}", read.index().waiting());
    let screen = serde_json::to_string(&alondra_core::views::build(&format!("task:{SDK_TASK}"), alondra_core::views::RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: alondra_core::views::Client::Tui,
        lang: alondra_core::views::text::Lang::En, transport: alondra_core::views::Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap()).unwrap();
    assert!(screen.contains("Paused") && !screen.contains("Resume “"), "its screen says it is paused and shows no card: {screen}");
    drop(read);

    assert!(node.domain.live.gate.resume_now(&node.domain, &node.agent(), alondra_core::now_millis()));
    node.gate_look().await;
    node.until_told("resumed you");
    assert_eq!(node.prompts().len(), 2, "{:?}", node.prompts());
}

/// The user's own message to a node they paused resumes it like their Resume: it waits in the
/// queue while every slot is taken, nothing reaches Claude meanwhile, and the gate lets it go on.
#[tokio::test]
async fn the_users_own_message_to_a_paused_sdk_node_waits_for_a_slot_and_then_reaches_claude() {
    let mut node = SdkNode::working();
    node.pause();
    node.domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(0)) });
    let body = serde_json::from_value(serde_json::json!({"text": "Use the short changelog format"})).unwrap();
    crate::domain_messages::deliver(&node.domain, &node.home, &user(), "The user", &AgentId(node.agent()), body, true).await.unwrap();
    node.gate_look().await;
    node.step();
    assert!(node.domain.live.gate.paused(&node.domain, &node.agent()).is_some(), "no slot: it still waits");
    assert_eq!(node.dispatched_inputs(), 0, "nothing reaches the host before the gate lets it go on");
    let pacing = node.domain.live.gate.live(&node.domain, alondra_core::now_millis()).unwrap();
    assert_eq!(pacing.queued.iter().map(|queued| queued.task_id.as_str()).collect::<Vec<_>>(), [SDK_TASK], "it waits in the queue");
    let read = node.domain.read();
    let live = alondra_core::views::live::LiveSnapshot { pacing: Some(pacing), ..Default::default() };
    let shown = |key: &str| serde_json::to_string(&alondra_core::views::build(key, alondra_core::views::RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: alondra_core::views::Client::Tui,
        lang: alondra_core::views::text::Lang::En, transport: alondra_core::views::Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap()).unwrap();
    let tasks = shown("tasks");
    assert!(tasks.contains("Starts when there is room") && !tasks.contains("\"text\":\"Paused") && !tasks.contains("\"resume:"),
        "its task shows it queued, with no Resume: {tasks}");
    let screen = shown(&format!("task:{SDK_TASK}"));
    assert!(screen.contains("1st of 1 · 0 of 0 slots taken"), "its own screen says its place: {screen}");
    drop(read);

    node.domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(1)) });
    node.gate_look().await;
    assert!(node.domain.live.gate.paused(&node.domain, &node.agent()).is_none(), "a slot is free: it goes on");
    node.until_told("Use the short changelog format");
    let resumed = node.prompts().iter().filter(|prompt| prompt.contains("resumed you")).count();
    assert!(resumed <= 1, "{:?}", node.prompts());
}

/// The user paused the node and its host died with no owner left to replace it, as when a daemon's
/// start puts a host the pause's interrupt then closes. Resuming it launches a host that resumes the
/// same native session, and what the node goes on with reaches it.
#[tokio::test(flavor = "multi_thread")]
async fn resuming_a_paused_sdk_node_whose_host_died_resumes_its_session_in_a_new_host() {
    let mut node = SdkNode::working();
    let session = node.starts()[0]["session"].as_str().unwrap().to_owned();
    node.pause();
    node.lose_host();

    let mut changed = node.domain.subscribe();
    assert!(node.domain.live.gate.resume_now(&node.domain, &node.agent(), alondra_core::now_millis()));
    node.gate_look().await;
    node.domain.join_retakes();
    assert!(node.domain.owns_sdk_host(&AgentId(node.agent())), "a new host took the task over");
    let capture = node.capture.clone();
    let told = move || crate::sdk_owner::tests::not_wakes(std::fs::read_to_string(format!("{}.prompts", capture.display())).unwrap_or_default()
        .lines().map(|line| serde_json::from_str(line).unwrap())).iter().any(|prompt: &String| prompt.contains("resumed you"));
    changed.wait_for(|_| told()).await.unwrap();
    node.domain.stop_sdk_owners();
    let starts = node.starts();
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
    assert!(node.host_lost_card().is_none());
}

/// While the node is paused, a host that could not be replaced puts no card the pause would refuse.
/// Its resume tries again; when no host can start, the task gets the card to try again or cancel.
#[tokio::test(flavor = "multi_thread")]
async fn resuming_a_paused_sdk_node_no_host_can_take_over_gives_its_task_a_card_to_try_again() {
    let mut node = SdkNode::working();
    node.pause();
    node.lose_host();
    crate::sdk_owner::lost(&node.domain, &SDK_TASK.into(), "its new host stopped before it could take the task's next message");
    assert!(node.host_lost_card().is_none(), "the pause holds the task: the card waits for its resume");

    use std::os::unix::fs::PermissionsExt;
    let state_root = node.domain.sdk().unwrap().0.state_root.clone();
    let refuse = |refused: bool| std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(if refused { 0o500 } else { 0o700 })).unwrap();
    refuse(true);
    assert!(node.domain.live.gate.resume_now(&node.domain, &node.agent(), alondra_core::now_millis()));
    node.gate_look().await;
    node.domain.join_retakes();
    refuse(false);
    let card = node.host_lost_card().expect("the task has a card to try again");
    let alondra_core::domain::InteractionPayload::Question { options, .. } = &card.payload else { panic!("{card:?}") };
    assert!(options.iter().any(|option| option.id == alondra_core::domain::RETAKE), "{options:?}");
    assert_eq!(node.domain.state().unwrap().tasks[&TaskId::from(SDK_TASK)].state, alondra_core::domain::TaskState::NeedsYou);
}

/// 4 Nov, t-oss-9-export: the disk fell under its floor while the node built, and the gate held its
/// next turn. The turn it ran ended with nothing told; once the disk had room the gate ended the
/// pause with nothing new, the lifecycle refused that every minute, and the node never came back.
/// Now its pause ends by telling it to carry on.
#[tokio::test]
async fn a_disk_hold_the_node_rested_through_ends_once_there_is_room_by_telling_it_to_carry_on() {
    const GB: u64 = 1_000_000_000;
    let mut node = SdkNode::working();
    let domain = Arc::clone(&node.domain);
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    assert!(domain.live.gate.hold_back(&domain, &node.agent(), Reason::Disk, alondra_core::now_millis()));
    interrupt_sdk_node(&domain, &node.agent());
    node.step_until(SdkNode::turn_ended);
    assert_eq!(node.lifecycle().activity(), alondra_domain::ActivityView::Resting);
    assert_eq!(node.pending_inputs(), 0, "nothing waits for it");

    domain.live.machine_watch.measure_disk(crate::disk::Space { free: 60 * GB, purgeable: 0, total: 500 * GB });
    node.gate_look().await;
    assert!(domain.live.gate.paused(&domain, &node.agent()).is_none(), "the pause ended");
    assert_eq!(node.pending_inputs(), 1, "recovery queues the carry-on instruction before the owner runs");
    node.until_told("held your next turn while the disk was nearly full");
    assert!(domain.read().index().waiting().is_empty(), "nobody is asked anything");
}

/// 5 Nov, t-env-allowlist-nodes: the user sent its delivery back and the node worked on it when the
/// disk fell under its floor. Its host died, a new one took the same session, and once the disk had
/// room the gate ended the pause with nothing to go on with: the task read as running for 2 h 40 min
/// with no turn and no wait. The end of the pause now tells the session to go on.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_dies_under_a_disk_hold_of_a_delivery_sent_back_goes_on_once_there_is_room() {
    const GB: u64 = 1_000_000_000;
    let mut node = SdkNode::working();
    let session = node.starts()[0]["session"].as_str().unwrap().to_owned();
    let domain = Arc::clone(&node.domain);
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    node.deliver();
    node.get_it_back();
    assert!(domain.live.gate.hold_back(&domain, &node.agent(), Reason::Disk, alondra_core::now_millis()));

    node.lose_host();
    crate::sdk_owner::retake(&domain, SDK_TASK.into());
    domain.join_retakes();
    assert!(domain.owns_sdk_host(&AgentId(node.agent())), "a new host took the task over");
    assert_eq!(node.lifecycle().activity(), alondra_domain::ActivityView::Resting);
    assert_eq!(node.pending_inputs(), 0, "nothing waits for it while the disk has no room");

    domain.live.machine_watch.measure_disk(crate::disk::Space { free: 60 * GB, purgeable: 0, total: 500 * GB });
    let mut changed = domain.subscribe();
    node.gate_look().await;
    assert!(domain.live.gate.paused(&domain, &node.agent()).is_none(), "the pause ended");
    let capture = node.capture.clone();
    let told = move || crate::sdk_owner::tests::not_wakes(std::fs::read_to_string(format!("{}.prompts", capture.display())).unwrap_or_default()
        .lines().map(|line| serde_json::from_str(line).unwrap())).iter().any(|prompt: &String| prompt.contains("held your next turn"));
    tokio::time::timeout(std::time::Duration::from_secs(60), changed.wait_for(|_| told())).await.expect("nothing told the session to go on").unwrap();
    domain.stop_sdk_owners();
    assert_eq!(node.starts().last().unwrap()["resume"].as_str(), Some(session.as_str()), "the same session goes on");
    assert!(domain.read().index().waiting().is_empty(), "nobody is asked anything: {:?}", domain.read().index().waiting());
}

/// An intentional interruption is not a stranded task for the gate to wake automatically.
#[tokio::test]
async fn an_interrupted_task_waits_for_instruction_instead_of_being_woken_by_the_gate() {
    let mut node = SdkNode::working();
    let domain = Arc::clone(&node.domain);
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    node.deliver();
    interrupt_sdk_node(&domain, &node.agent());
    node.step_until(SdkNode::turn_ended);
    node.get_it_back();
    assert!(domain.state().unwrap().stranded_tasks().is_empty());
    assert_eq!(node.lifecycle().awaits(), Some(&alondra_domain::Await::Instruction));

    let now = alondra_core::now_millis();
    let look = |at| crate::admission::pass_at(&domain, &node.home, at);
    look(now).await.unwrap();
    look(now + crate::admission::STRANDED_AFTER - 1).await.unwrap();
    assert_eq!(node.pending_inputs(), 0, "not yet");

    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(0)) });
    look(now + crate::admission::STRANDED_AFTER).await.unwrap();
    assert_eq!(node.pending_inputs(), 0, "the Mac has no slot");

    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    look(now + crate::admission::STRANDED_AFTER + 1).await.unwrap();
    assert_eq!(node.pending_inputs(), 0, "capacity recovery does not undo an intentional interruption");
    run(&domain, Actor::System, Command::TellNode { task_id: SDK_TASK.into(), text: "Continue the release notes".into(), from_user: true, answers: None });
    assert_eq!(node.pending_inputs(), 1);
    node.until_told("Continue the release notes");
    assert!(domain.read().index().waiting().is_empty(), "nobody is asked anything: {:?}", domain.read().index().waiting());
}

/// 5 November, 20:54: ten nodes the disk held, six of them still in the turn they ran, and 19 GB free
/// in the container. Those turns reserved 3 GB each, so nothing came back, not even before a new
/// High task. A held node reserves nothing: they come back one by one, first, while their builds fit.
#[tokio::test]
async fn ten_nodes_the_disk_held_come_back_one_by_one_on_19_gb_free_before_a_new_high_task() {
    const GB: u64 = 1_000_000_000;
    let node = SdkNode::working();
    let domain = Arc::clone(&node.domain);
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(12)) });
    let watch = &domain.live.machine_watch;
    watch.seed_peaks("app", &[3 * GB; 3]);
    let tasks: Vec<String> = (2..=10).map(|index| format!("t-sdk-{index}")).collect();
    let mut drivers: Vec<(String, crate::sdk_owner::Driver)> = tasks.iter().map(|task| (task.clone(), node.beside(task))).collect();
    let sessions: Vec<String> = std::iter::once(node.agent()).chain(drivers.iter().map(|(_, driver)| driver.agent().0.clone())).collect();
    let now = alondra_core::now_millis();
    for session in &sessions {
        assert!(gate.hold_back(&domain, session, Reason::Disk, now));
    }
    watch.measure_disk(crate::disk::Space { free: 19 * GB, purgeable: 0, total: 500 * GB });
    node.gate_look().await;
    assert_eq!(crate::admission::working_tasks(&domain.state().unwrap()).len(), 0, "their turns go on, but they take no slot while the disk holds them");

    let mut end_turn = |index: usize| {
        let (task, driver) = &mut drivers[index];
        interrupt_sdk_node(&domain, &driver.agent().0);
        while node.activity(task) != alondra_domain::ActivityView::Resting {
            driver.step(&|| true).unwrap();
        }
    };
    (0..4).for_each(&mut end_turn);
    run(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "t-high".into(), request_id: "r".into(), title: "Task t-high".into(), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")) });
    let high = Waiting { key: Key::Task("t-high".into()), priority: TaskPriority::High, resuming: false, since: now };
    run(&domain, user(), Command::SetPriority { task_id: "t-high".into(), priority: TaskPriority::High });
    assert!(gate.ask(&domain, high, alondra_core::now_millis()).is_err(), "the nodes the disk held go first");

    let back = || sessions.iter().filter(|session| gate.paused(&domain, session).is_none()).count();
    assert_eq!(back(), 0);
    for expected in 1..=4 {
        node.gate_look().await;
        assert_eq!(back(), expected, "one more each look: its 3 GB and the 5 GB line fit beside those already back");
    }
    end_turn(4);
    node.gate_look().await;
    assert_eq!(back(), 4, "the four back reserve 12 GB: one more needs 20 GB");
    let state = domain.state().unwrap();
    let at_work = crate::admission::working_tasks(&state);
    assert_eq!(at_work.len(), 4, "the five turns still held take no slot: only the four back do");
    assert_eq!(watch.disk_reserved(&crate::admission::reserving_tasks(&state, at_work.iter())), 12 * GB, "only the four back reserve");
    assert!(domain.read().index().waiting().is_empty(), "nobody is asked anything");
    for (_, driver) in &mut drivers {
        let _ = driver.stop();
    }
}

/// The 7 of 6 working of 11 November: a node of the Agent SDK whose turn the disk's floor held was
/// counted by the gate while its task read paused, so the gate and the header disagreed.
#[tokio::test]
async fn a_node_paused_for_disk_takes_no_slot_in_the_gate_or_the_header() {
    let node = SdkNode::working();
    let domain = Arc::clone(&node.domain);
    assert_eq!(crate::admission::working_tasks(&domain.state().unwrap()).len(), 1, "its turn works");
    assert!(domain.live.gate.hold_back(&domain, &node.agent(), Reason::Disk, alondra_core::now_millis()));
    node.gate_look().await;
    let state = domain.state().unwrap();
    assert!(crate::admission::working_tasks(&state).is_empty(), "the disk holds it: no slot");
    assert_eq!(state.slots().len(), 0);
    assert_eq!(domain.read().index().activity().working, 0, "the header counts none either");
}

/// The disk fell under its floor while two nodes built. The heavier one's turn goes on uncut and
/// takes no slot; once it ends, what the node is told waits, with no card, shown in the
/// queue for the disk; once the disk has room it gets it, and is never told it was held. The
/// lighter one goes on.
#[tokio::test]
async fn under_the_disks_floor_the_heavier_nodes_next_turn_waits_for_room_without_cutting_the_one_it_runs() {
    const GB: u64 = 1_000_000_000;
    let mut node = SdkNode::working();
    let domain = Arc::clone(&node.domain);
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    run(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "t-light".into(), request_id: "r".into(), title: "Fix a typo".into(), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")) });
    run(&domain, Actor::System, Command::RegisterAgent { agent_id: AgentId("agent-light".into()), name: "agent-light".into(), at: 0 });
    crate::domain_store::admit_through_sdk(&domain, &"t-light".into(), "agent-light");
    let input = domain.state().unwrap().lifecycle_of(&"t-light".into()).unwrap().pending_inputs()[0].as_str().to_owned();
    run(&domain, Actor::System, Command::RecordRuntimeFacts { task_id: "t-light".into(), generation: 1, facts: vec![
        alondra_core::domain::RuntimeObservation::ResourceAvailable,
        alondra_core::domain::RuntimeObservation::TurnStarted { turn: "turn-light".into(), input },
    ] });
    let now = alondra_core::now_millis();
    let working = crate::admission::working_projects(&domain.state().unwrap());
    assert_eq!(working.keys().map(|task| task.0.as_str()).collect::<Vec<_>>(), ["t-light", SDK_TASK]);
    let watch = &domain.live.machine_watch;
    let heavy: std::collections::BTreeMap<TaskId, Option<String>> = [(TaskId::from(SDK_TASK), None)].into();
    watch.measure_peaks(14 * GB, &heavy, now);
    watch.measure_peaks(2 * GB, &heavy, now + SAMPLE);
    watch.measure_peaks(2 * GB, &working, now + 2 * SAMPLE);
    watch.measure_disk(crate::disk::Space { free: 2 * GB, purgeable: 0, total: 500 * GB });
    assert!(watch.under_floor());

    crate::admission::hold_for_disk(&domain, now + 2 * SAMPLE);
    let held = domain.live.gate.paused(&domain, &node.agent()).expect("the heavier node is held");
    assert_eq!(held.reason, Reason::Disk);
    assert!(domain.live.gate.paused(&domain, "agent-light").is_none(), "the lightest goes on");
    node.step();
    assert_eq!(node.lifecycle().activity(), alondra_domain::ActivityView::Working, "the turn it runs is not cut");
    assert_eq!(node.prompts(), ["consume-and-hold"]);
    assert!(!crate::admission::working_tasks(&domain.state().unwrap()).contains(&TaskId::from(SDK_TASK)), "the disk holds it: its turn runs but it takes no slot");

    interrupt_sdk_node(&domain, &node.agent());
    node.step_until(SdkNode::turn_ended);
    crate::domain_messages::tell_sdk_node(&domain, &SDK_TASK.into(), crate::node_push::ALONDRA, "Use the short changelog format", "tell-while-held", false).unwrap();
    node.gate_look().await;
    node.step();
    assert!(!crate::admission::working_tasks(&domain.state().unwrap()).contains(&TaskId::from(SDK_TASK)), "its turn ended: it holds no slot");
    assert!(domain.live.gate.paused(&domain, &node.agent()).is_some(), "the disk has no room: it still waits");
    assert_eq!(node.dispatched_inputs(), 0, "what it is told waits for room");
    let pacing = domain.live.gate.live(&domain, alondra_core::now_millis()).unwrap();
    assert!(matches!(pacing.queued.as_slice(), [queued] if queued.task_id == SDK_TASK && matches!(queued.hold, alondra_core::views::LiveHold::Disk { .. })),
        "it waits in the queue for the disk: {:?}", pacing.queued);
    assert!(domain.read().index().waiting().is_empty(), "nobody is asked anything: {:?}", domain.read().index().waiting());
    assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(alondra_core::machine_incident::MACHINE_PREFIX)));

    watch.measure_disk(crate::disk::Space { free: 60 * GB, purgeable: 0, total: 500 * GB });
    node.gate_look().await;
    node.until_told("Use the short changelog format");
    assert!(node.prompts().iter().all(|prompt| !prompt.contains("held your next turn")), "{:?}", node.prompts());
    assert!(domain.live.gate.paused(&domain, &node.agent()).is_none());
}

fn node_pause_card(domain: &DomainRuntime) -> Option<alondra_core::domain::InteractionId> {
    let state = domain.state().unwrap();
    state.interactions.values().find(|card| state.node_pause_card(&card.id) && state.open(card)).map(|card| card.id.clone())
}

fn answer_resume(domain: &DomainRuntime, card: &alondra_core::domain::InteractionId) -> Result<(), String> {
    let state = domain.state().unwrap();
    domain.execute(&user(), &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: crate::auth::random_hex(8).into(),
        expected_revision: state.revision,
        command: Command::AnswerQuestion { interaction_id: card.clone(), option_id: "resume".into(), inputs: Default::default(), attachments: vec![] },
    }, None, || Ok(())).map(drop).map_err(|error| format!("{error:?}"))
}

/// 6 Nov, t-account-identity-per-profile: the disk fell under its floor while the node's turn ran,
/// and 30 s later its host's connection broke and its owner stopped. Nothing took the task over
/// and nothing recorded that its host had ended: it read as running, with no node, for 44 minutes.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_dies_under_a_disk_hold_while_its_turn_runs_never_leaves_its_task_running_without_a_node() {
    const GB: u64 = 1_000_000_000;
    let mut node = SdkNode::working();
    let domain = Arc::clone(&node.domain);
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    domain.live.machine_watch.measure_disk(crate::disk::Space { free: GB, purgeable: 0, total: 500 * GB });
    assert!(domain.live.gate.hold_back(&domain, &node.agent(), Reason::Disk, alondra_core::now_millis()));
    node.lose_host();
    node.gate_look().await;
    domain.join_retakes();
    assert!(domain.live.gate.paused(&domain, &node.agent()).is_some(), "the disk still has no room");
    let agent = AgentId(node.agent());
    assert!(domain.owns_sdk_host(&agent) || node.lifecycle().activity() != alondra_domain::ActivityView::Working,
        "the task reads as running with no node: {:?}", node.lifecycle().activity());
    domain.stop_sdk_owners();
}

/// Production, 2026-11-08 (t-agents-md-review-public-audit, t-security-audit-local-daemon and
/// t-usage-limit-no-cards): each read as working with no host, and every gate pass tried to take it
/// over and failed on the same observation of its ended host, for good. Here the host ends its turn
/// and dies before its owner reads that end, which its adapter then refuses: the observation is
/// discarded, the first pass takes the task over, and the next has nothing to take.
#[tokio::test(flavor = "multi_thread")]
async fn a_working_task_whose_ended_host_left_an_observation_no_owner_can_settle_leaves_the_loop() {
    let mut node = SdkNode::working();
    let domain = Arc::clone(&node.domain);
    run(&domain, Actor::System, Command::TellNode { task_id: SDK_TASK.into(), text: "Carry on".into(), from_user: false, answers: None });
    node.driver.step(&|| false).unwrap();
    let outbox = node.backend.state_root.join(node.agent()).join("outbox");
    while !String::from_utf8_lossy(&std::fs::read(&outbox).unwrap()).contains("\"type\":\"result\"") {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    node.lose_host();
    crate::domain_store::refuse_next(crate::domain_store::Refuse::Observation);
    crate::sdk_owner::take_working_unowned(&domain, &domain.state().unwrap(), alondra_core::now_millis());
    domain.join_retakes();
    let agent = AgentId(node.agent());
    assert!(domain.owns_sdk_host(&agent), "the first pass takes the task over");
    crate::sdk_owner::take_working_unowned(&domain, &domain.state().unwrap(), alondra_core::now_millis() + 2 * MIN);
    domain.join_retakes();
    assert!(domain.owns_sdk_host(&agent), "and the task stays owned");
    domain.stop_sdk_owners();
}

/// 2026-11-10: tasks read «Esperando a que Claude empiece» for hours, holding slots of the gate,
/// with no host to start anything. An execution that waits for a turn to start, or for a
/// notification to open one, is taken over when no owner holds it, as one at work is.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_waiting_for_its_turn_to_start_with_no_host_of_its_own_is_taken_over() {
    let mut node = SdkNode::resting();
    let domain = Arc::clone(&node.domain);
    node.tell(SDK_TASK, "Carry on");
    assert_eq!(node.lifecycle().activity(), alondra_domain::ActivityView::Starting);
    node.lose_host();
    crate::sdk_owner::take_working_unowned(&domain, &domain.state().unwrap(), alondra_core::now_millis());
    domain.join_retakes();
    assert!(domain.owns_sdk_host(&AgentId(node.agent())), "a host takes the waiting turn: {:?}", node.lifecycle().activity());
    domain.stop_sdk_owners();
}

/// 0.3.31, a task «delivering» with no host of its own: Alondra took it over every minute for
/// hours, and nothing about the task changed. Here every new host ends before it takes the input
/// it is handed. The first look takes the task over; its hosts end until the task waits on the
/// user's card that says so, and no later look takes it over again.
#[tokio::test(flavor = "multi_thread")]
async fn a_delivering_task_whose_hosts_end_before_taking_its_input_is_never_taken_over_again_unchanged() {
    let mut node = SdkNode::resting();
    let domain = Arc::clone(&node.domain);
    node.tell(SDK_TASK, "Carry on");
    assert_eq!(node.lifecycle().activity(), alondra_domain::ActivityView::Starting);
    node.lose_host();
    std::fs::write(&node.backend.native, ENDS_ON_ITS_FIRST_PROMPT).unwrap();
    let agent = AgentId(node.agent());
    let journal = || domain.read().state().lifecycle_journal[&TaskId::from(SDK_TASK)].iter().cloned().collect::<Vec<_>>();
    let mut looks = vec![];
    for look in 0..4 {
        let before = journal().len();
        crate::sdk_owner::take_working_unowned(&domain, &domain.state().unwrap(), alondra_core::now_millis() + look * 2 * MIN);
        domain.join_retakes();
        while domain.owns_sdk_host(&agent) || domain.in_hand.lock().unwrap().contains_key(&TaskId::from(SDK_TASK)) {
            std::thread::yield_now();
        }
        looks.push(journal()[before..].to_vec());
    }
    assert_eq!(looks[0].iter().filter(|record| matches!(record, alondra_core::domain::LifecycleRecord::TakenOver)).count(), 1, "the first look takes the task over once: {looks:#?}");
    assert!(looks[1..].iter().all(Vec::is_empty), "no later look takes over a task that waits on its owner: {looks:#?}");
    assert!(node.host_lost_card().is_some(), "the user is told its hosts end before they take anything: {looks:#?}");
    domain.stop_sdk_owners();
}

/// A native Claude that ends as soon as it is handed a prompt, before it starts a turn.
const ENDS_ON_ITS_FIRST_PROMPT: &str = r#"#!/usr/bin/python3
import json, sys
for raw in sys.stdin:
    if json.loads(raw).get('type') == 'user':
        sys.exit(0)
"#;

/// 6 Nov: «Resume» on the card of a node the gate held for the disk or the machine answered «no
/// longer available» while the node was still paused. Every pause card's Resume is taken while
/// the card is open.
#[tokio::test(flavor = "multi_thread")]
async fn resume_on_the_card_of_a_paused_sdk_node_is_taken_while_the_card_is_open() {
    const GB: u64 = 1_000_000_000;
    for reason in [Reason::Disk, Reason::Declared { reason: "The build queue is full".into() }, Reason::ByUser] {
        let mut node = SdkNode::working();
        let domain = Arc::clone(&node.domain);
        domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(0)) });
        domain.live.machine_watch.measure_disk(crate::disk::Space { free: GB, purgeable: 0, total: 500 * GB });
        if reason == Reason::ByUser {
            node.pause();
        } else {
            assert!(domain.live.gate.hold_back(&domain, &node.agent(), reason.clone(), alondra_core::now_millis()));
            interrupt_sdk_node(&domain, &node.agent());
            node.step_until(SdkNode::turn_ended);
        }
        let card = node_pause_card(&domain).unwrap_or_else(|| panic!("{reason:?}: the pause has its card"));
        assert_eq!(answer_resume(&domain, &card), Ok(()), "{reason:?}: the card's Resume is refused while the node is paused");
        assert!(domain.live.gate.pauses(&domain).resumed.contains_key(&node.agent()), "{reason:?}: the user's resume waits for a slot");
    }
}

/// A turn at work always has an owner of this daemon.
fn owned_while_working(node: &SdkNode, case: &str) {
    let activity = node.lifecycle().activity();
    assert!(node.domain.owns_sdk_host(&AgentId(node.agent())) || activity != alondra_domain::ActivityView::Working,
        "{case}: the task reads as running with no node");
}

/// The running and paused states of a node of the Agent SDK, for each pause, with its host lost
/// during the pause or not: a turn at work always has an owner; while the pause's card is open the
/// gate holds the pause and the card's Resume is taken; once the pause ends the card goes.
#[tokio::test(flavor = "multi_thread")]
async fn every_pause_with_its_host_lost_or_not_keeps_a_working_task_owned_and_its_card_resumable() {
    const GB: u64 = 1_000_000_000;
    for reason in [Reason::ByUser, Reason::Disk, Reason::Declared { reason: "The build queue is full".into() }] {
        for host_lost in [false, true] {
            let case = format!("{reason:?}, host lost: {host_lost}");
            let mut node = SdkNode::working();
            let domain = Arc::clone(&node.domain);
            domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
            domain.live.machine_watch.measure_disk(crate::disk::Space { free: GB, purgeable: 0, total: 500 * GB });
            match &reason {
                Reason::ByUser => node.pause(),
                Reason::Disk => {
                    assert!(domain.live.gate.hold_back(&domain, &node.agent(), Reason::Disk, alondra_core::now_millis()));
                    if !host_lost {
                        interrupt_sdk_node(&domain, &node.agent());
                        node.step_until(SdkNode::turn_ended);
                    }
                }
                declared => {
                    assert!(domain.live.gate.hold_back(&domain, &node.agent(), declared.clone(), alondra_core::now_millis()));
                    interrupt_sdk_node(&domain, &node.agent());
                    node.step_until(SdkNode::turn_ended);
                }
            }
            owned_while_working(&node, &case);
            if host_lost {
                node.lose_host();
            }
            node.gate_look().await;
            domain.join_retakes();
            owned_while_working(&node, &case);
            let card = node_pause_card(&domain).unwrap_or_else(|| panic!("{case}: the pause has its card"));
            assert!(domain.live.gate.paused(&domain, &node.agent()).is_some(), "{case}: an open pause card is a pause the gate holds");
            assert_eq!(answer_resume(&domain, &card), Ok(()), "{case}: the card's Resume is taken while it is open");

            domain.live.machine_watch.measure_disk(crate::disk::Space { free: 60 * GB, purgeable: 0, total: 500 * GB });
            node.gate_look().await;
            domain.join_retakes();
            assert!(domain.live.gate.paused(&domain, &node.agent()).is_none(), "{case}: the pause ended");
            assert!(node_pause_card(&domain).is_none(), "{case}: the card of an ended pause is retired");
            owned_while_working(&node, &case);
            domain.stop_sdk_owners();
        }
    }
}

/// 6 Nov 07:50: pausing working nodes was refused with invalid_input. The Pause a working node of
/// the Agent SDK offers carries its revision and is taken; the same id written by hand without it
/// is no action at all.
#[tokio::test]
async fn the_pause_a_working_sdk_node_offers_is_taken_and_one_without_its_revision_is_no_action() {
    use alondra_core::views::action_id::{ActionId, Verb};
    let mut node = SdkNode::working();
    let agent = node.agent();
    let tasks = {
        let read = node.domain.read();
        let live = alondra_core::views::live::LiveSnapshot::default();
        serde_json::to_string(&alondra_core::views::build("tasks", alondra_core::views::RenderContext {
            domain: read.state(), index: read.index(), live: &live, client: alondra_core::views::Client::Tui,
            lang: alondra_core::views::text::Lang::En, transport: alondra_core::views::Transport::Loopback, cursor: "test", rendered_at: "",
        }).unwrap()).unwrap()
    };
    let start = tasks.find(&format!("\"pause:{agent}@")).unwrap_or_else(|| panic!("a working node offers Pause: {tasks}")) + 1;
    let offered = &tasks[start..start + tasks[start..].find('"').unwrap()];
    let action = ActionId::parse(offered).unwrap();
    assert_eq!((action.verb, action.subject.as_str()), (Verb::Pause, agent.as_str()));
    assert!(ActionId::parse(&format!("pause:{agent}")).is_err(), "an id without its revision is not one the apps send");

    node.domain.live.gate.pause_by_user(&node.domain, &action.subject, alondra_core::now_millis()).unwrap();
    node.step_until(SdkNode::turn_ended);
    let card = node_pause_card(&node.domain).expect("the pause has its card");
    assert_eq!(answer_resume(&node.domain, &card), Ok(()));
}

/// 7 November: with 8 as the limit, 39 nodes worked, because only starts passed the gate. Ten nodes
/// told at once with two slots: two go on, eight wait with no card, and they enter as slots free.
#[tokio::test(flavor = "multi_thread")]
async fn ten_nodes_told_at_once_with_two_slots_never_work_more_than_two_and_the_rest_enter_as_slots_free() {
    let node = SdkNode::resting();
    let mut drivers: Vec<(String, crate::sdk_owner::Driver)> = (0..10).map(|index| {
        let task = format!("t-turn-{index}");
        let driver = node.beside_resting(&task);
        (task, driver)
    }).collect();
    let cards = node.cards();
    node.set_limit(2);
    for (task, _) in &drivers {
        run(&node.domain, Actor::System, Command::TellNode { task_id: task.as_str().into(), text: "consume-and-hold".into(), from_user: false, answers: None });
    }
    let now = alondra_core::now_millis();
    std::thread::scope(|scope| {
        for (task, _) in &drivers {
            let (domain, task) = (&node.domain, TaskId::from(task.as_str()));
            scope.spawn(move || domain.live.gate.may_take_turn(domain, &task, now));
        }
    });
    let gate = node.gate();
    assert_eq!((gate.working, node.waiting_turns().len()), (2, 8), "two have a slot and eight wait, however the asks interleaved");

    let held = |node: &SdkNode| std::fs::read_to_string(format!("{}.prompts", node.capture.display())).unwrap().matches("consume-and-hold").count();
    let mut rounds = 0;
    while held(&node) < 2 || rounds < 3 {
        for (_, driver) in &mut drivers {
            driver.step(&|| true).unwrap();
        }
        node.gate_look().await;
        assert!(node.gate().working <= 2, "{:?}", node.gate().working_tasks);
        rounds += 1;
    }
    assert_eq!((held(&node), node.waiting_turns().len()), (2, 8), "the two turns run and hold their slots; the rest wait");
    assert_eq!(node.cards().iter().filter(|card| !cards.contains(card)).count(), 0, "waiting for a slot asks the user nothing");

    node.set_limit(10);
    node.gate_look().await;
    while held(&node) < 10 {
        for (_, driver) in &mut drivers {
            driver.step(&|| true).unwrap();
        }
        node.gate_look().await;
    }
    assert!(node.waiting_turns().is_empty());
    for (_, driver) in &mut drivers {
        driver.stop().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slot_that_frees_goes_to_the_waiting_turn_of_highest_priority_then_to_the_oldest() {
    let node = SdkNode::resting();
    let mut drivers: Vec<crate::sdk_owner::Driver> = ["t-low", "t-old", "t-new", "t-high"].map(|task| node.beside_resting(task)).into_iter().collect();
    run(&node.domain, user(), Command::SetPriority { task_id: "t-low".into(), priority: TaskPriority::Low });
    run(&node.domain, user(), Command::SetPriority { task_id: "t-high".into(), priority: TaskPriority::High });
    node.set_limit(0);
    let now = alondra_core::now_millis();
    for (age, task) in ["t-low", "t-old", "t-new", "t-high"].into_iter().enumerate() {
        node.tell(task, "Go on");
        assert!(!node.domain.live.gate.may_take_turn(&node.domain, &task.into(), now + age as Millis));
    }
    assert_eq!(node.waiting_turns(), ["t-high", "t-old", "t-new", "t-low"]);
    node.set_limit(1);
    node.gate_look().await;
    assert_eq!(node.waiting_turns(), ["t-old", "t-new", "t-low"]);
    assert!(node.domain.live.gate.may_take_turn(&node.domain, &"t-high".into(), now + 10));
    assert!(!node.domain.live.gate.may_take_turn(&node.domain, &"t-old".into(), now + 10));
    for driver in &mut drivers {
        driver.stop().unwrap();
    }
}

/// A node that waits for a slot says so with its place in the queue, and nobody is asked anything.
#[tokio::test]
async fn a_turn_waiting_for_a_slot_reads_waiting_for_capacity_and_raises_no_card() {
    let mut node = SdkNode::resting();
    node.set_limit(0);
    let cards = node.cards();
    let delivered = node.dispatched_inputs();
    node.tell(SDK_TASK, "Go on");
    node.step();
    assert_eq!(node.dispatched_inputs(), delivered, "nothing more reached the host");
    assert_eq!(node.cards(), cards);
    assert_eq!(node.gate().working, 0, "an input held back takes no slot, so it never blocks what frees one");
    let read = node.domain.read();
    let live = alondra_core::views::live::LiveSnapshot { pacing: node.domain.live.gate.live(&node.domain, alondra_core::now_millis()), ..Default::default() };
    let tasks = alondra_core::views::build("tasks", alondra_core::views::RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: alondra_core::views::Client::Tui,
        lang: alondra_core::views::text::Lang::En, transport: alondra_core::views::Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap();
    let tasks = serde_json::to_string(&tasks).unwrap();
    assert!(tasks.contains("Waiting"), "{tasks}");
    drop(read);

    node.set_limit(1);
    node.gate_look().await;
    node.until_told("Go on");
}

/// A message to a resting node waits for a slot, and reaches Claude once one frees.
#[tokio::test]
async fn a_message_to_a_resting_node_waits_for_a_slot() {
    let mut node = SdkNode::resting();
    node.set_limit(0);
    let delivered = node.dispatched_inputs();

    node.tell(SDK_TASK, "A message for the node");
    node.step();
    assert_eq!((node.dispatched_inputs(), node.waiting_turns().len()), (delivered, 1), "a message waits");
    node.set_limit(1);
    node.gate_look().await;
    node.until_told("A message for the node");
}

/// A resume the gate let through reserved a slot, and the turn it starts takes that slot: it never
/// waits behind its own reservation.
#[tokio::test]
async fn a_node_back_from_a_pause_starts_its_turn_on_the_slot_of_its_resume() {
    let mut node = SdkNode::working();
    node.pause();
    node.set_limit(1);
    assert!(node.domain.live.gate.resume_now(&node.domain, &node.agent(), alondra_core::now_millis()));
    node.gate_look().await;
    for _ in 0..40 {
        node.step();
        if node.prompts().iter().any(|prompt| prompt.contains("resumed you")) {
            break;
        }
    }
    assert!(node.prompts().iter().any(|prompt| prompt.contains("resumed you")), "{:?}", node.waiting_turns());
    assert!(node.gate().working <= 1);
}
