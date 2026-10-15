use super::*;
use crate::fleet_gate::{Hold, Key, Limit, SPACING, Tuning, Waiting};
use crate::machine_state::NoRoom;
use crate::node_push::testing::Node;
use crate::node_reports::{Envelope, Report};
use alondra_core::{Machine, MemoryCompressor, MemoryPressure};
use alondra_core::domain::{API_RETRIES, Actor, AgentId, ApiRetry, Command, CommandEnvelope, NodePhase, OUTAGE_PROBE, PROTOCOL_VERSION, Pause, PauseReason, TaskId, TaskPriority, TaskState};
use std::path::PathBuf;
use std::sync::Arc;

const MIN: Millis = 60_000;
const SAMPLE: Millis = 10_000;

#[test]
fn a_pause_by_the_user_never_returns_by_itself_and_a_declared_wait_waits_out_its_grace() {
    let paused = |reason| Paused { task: "t".into(), reason, since: 0, cut: None, turn_since: None, cleanup: Cleanup::Done, released: false };
    assert!(!paused(Reason::ByUser).returns(true, false, 10 * 60 * MIN));
    assert!(paused(Reason::Memory).returns(true, false, 0));
    assert!(!paused(Reason::Usage).returns(false, false, 0));
    let declared = paused(Reason::Declared { reason: "build queue".into() });
    assert!(!declared.returns(true, false, DECLARED_GRACE - 1));
    assert!(declared.returns(true, false, DECLARED_GRACE));
    assert!(Paused { released: true, ..declared }.returns(true, false, 0), "\"I've fixed it\" lets it go at once");
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

struct NoLaunch;

impl crate::orchestrate::Launcher for NoLaunch {
    async fn run(&self, _: &str, _: &[String]) -> Result<String, crate::orchestrate::Refusal> {
        unreachable!("nothing is launched")
    }

    async fn stop(&self, _: &str) {}

    async fn respawn(&self, _: &str) -> Result<(), String> {
        unreachable!("nothing is respawned")
    }
}

/// A fleet of nodes on one Mac, each connected through a mod that acts like the real one: an
/// interrupt ends its turn, a message starts a new one.
struct Fleet {
    home: Home,
    domain: Arc<DomainRuntime>,
    nodes: Vec<(String, Node, usize)>,
    /// Every pause and every resume, in order, by task.
    log: Vec<(Millis, &'static str, String)>,
    max_working: usize,
    notices: usize,
}

impl Fleet {
    fn new(tasks: &[(&str, TaskPriority)], limit: Limit, started: Millis) -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        domain.live.gate.tune(Tuning { spacing: SPACING, limit: Some(limit) });
        run(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        run(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
        let mut nodes = vec![];
        for (index, (id, priority)) in tasks.iter().enumerate() {
            let agent = format!("agent-{id}");
            run(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
                task_id: (*id).into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
                dependencies: Default::default(), stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
            });
            run(&domain, user(), Command::SetPriority { task_id: (*id).into(), priority: *priority });
            run(&domain, Actor::System, Command::RegisterAgent { agent_id: AgentId(agent.clone()), name: agent.clone(), at: 0 });
            run(&domain, Actor::System, Command::MoveNode { agent_id: AgentId(agent.clone()), event: alondra_core::domain::NodeEvent::TurnStarted, at: 0 });
            run(&domain, user(), Command::AssignTask { task_id: (*id).into(), agent_id: AgentId(agent.clone()), model: None });
            domain.report(Envelope { session_id: agent.clone(), report: Report::TurnStart }, started + index as Millis * MIN);
            nodes.push((agent.clone(), Node::connect(&domain, &agent), 0));
        }
        crate::machine_watch::pace(&domain, &Machine { at: started, memory_free_percent: Some(50), cpus: 11, load: [5.0; 3], ..Machine::default() });
        Self { home: Home(home), domain, nodes, log: vec![], max_working: 0, notices: 0 }
    }

    fn working(&self, now: Millis) -> usize {
        crate::fleet_gate::working_tasks(&self.domain.state().unwrap()).len()
    }

    /// The node's turn ends, as its mod and the Stop hook say.
    fn end_turn(&self, task: &str, now: Millis) {
        self.domain.report(Envelope { session_id: format!("agent-{task}"), report: Report::TurnComplete { answer: String::new(), aborted: false } }, now);
        crate::domain_agent_lifecycle::node_event_at(&self.domain, &alondra_core::domain::AgentId(format!("agent-{task}")), alondra_core::domain::NodeEvent::TurnEnded, now).unwrap();
    }

    /// The node's build died for lack of memory and it said so with `wait_for_machine`.
    fn waits_for_machine(&self, task: &str, now: Millis) {
        let session = format!("agent-{task}");
        self.domain.live.gate.hold_back(&self.domain, &session, Reason::Declared { reason: "bazel was killed for lack of memory".into() }, now);
        self.end_turn(task, now);
    }

    fn orchestrator_inbox(&self) -> usize {
        self.domain.store.lock().unwrap().pending_messages().unwrap().iter().filter(|message| message.recipient_kind == "orchestrator").count()
    }

    /// One sample of the Mac and one look of the gate, then what the mods do with what they got.
    async fn tick(&mut self, free: u32, now: Millis) {
        self.tick_with(Machine { at: now, memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() }).await;
    }

    async fn tick_with(&mut self, machine: Machine) {
        let now = machine.at;
        let before = self.orchestrator_inbox();
        crate::machine_watch::pace(&self.domain, &machine);
        self.notices += self.orchestrator_inbox() - before;
        crate::fleet_gate::pass_at(&self.domain, &self.home.0, &NoLaunch, now).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        for (session, node, seen) in &mut self.nodes {
            let lines: Vec<serde_json::Value> = node.lines.lock().unwrap().clone();
            for line in &lines[*seen..] {
                let task = session.trim_start_matches("agent-").to_owned();
                if line["interrupt"] == true {
                    self.domain.report(Envelope { session_id: session.clone(), report: Report::TurnComplete { answer: String::new(), aborted: true } }, now);
                    self.log.push((now, "pause", task));
                } else if line["text"].is_string() {
                    self.domain.report(Envelope { session_id: session.clone(), report: Report::TurnStart }, now);
                    self.log.push((now, "resume", task));
                }
            }
            *seen = lines.len();
        }
        self.max_working = self.max_working.max(self.working(now));
    }

    async fn hold(&mut self, free: u32, from: Millis, span: Millis) -> Millis {
        let mut now = from;
        while now < from + span {
            self.tick(free, now).await;
            now += SAMPLE;
        }
        now
    }

    fn events(&self, kind: &str) -> Vec<(Millis, String)> {
        self.log.iter().filter(|(_, what, _)| *what == kind).map(|(at, _, task)| (*at, task.clone())).collect()
    }

    /// The node holds a pause for memory an older daemon made, as the upgrade finds it.
    fn paused_for_memory_before(&self, task: &str, now: Millis) {
        let pause = Pause { reason: PauseReason::Memory, since: now, cut: Some("bazel build //...".into()), turn_since: Some(now), cleanup: alondra_core::domain::PauseCleanup::Done, released: false };
        assert!(crate::domain_agent_lifecycle::node_event_at(&self.domain, &AgentId(format!("agent-{task}")), alondra_core::domain::NodeEvent::Paused { pause }, now).unwrap());
        self.end_turn(task, now);
    }
}

fn tasks(events: &[(Millis, String)]) -> Vec<&str> {
    events.iter().map(|(_, task)| task.as_str()).collect()
}

/// The Mac building with `margin_gib` left before macOS's warning, under `pressure`.
fn building(now: Millis, margin_gib: f64, pressure: MemoryPressure) -> Machine {
    const PAGE: u64 = 16_384;
    const WEIGHED: u64 = 900_000;
    let compressor = ((WEIGHED as f64 - margin_gib * ((1 << 30) / PAGE) as f64) / 2.0) as u64;
    let counters = MemoryCompressor { page_size: PAGE, uncompressed_pages: WEIGHED - compressor, compressor_pages: compressor, ..MemoryCompressor::default() };
    Machine { at: now, memory_free_percent: Some(30), memory_pressure: Some(pressure), compressor: Some(counters), cpus: 11, load: [20.0; 3], ..Machine::default() }
}

/// Asks the gate to start a task without a node yet, created on the first ask.
fn ask_new(fleet: &Fleet, now: Millis) -> Result<(), Hold> {
    if !fleet.domain.state().unwrap().tasks.contains_key(&TaskId::from("t-new")) {
        run(&fleet.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
            task_id: "t-new".into(), request_id: "r".into(), title: "Task t-new".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
    }
    let waiting = Waiting { key: Key::Task("t-new".into()), priority: TaskPriority::High, resuming: false, since: now };
    fleet.domain.live.gate.ask(&fleet.domain, waiting, now)
}

/// 13 October: Alondra paused working nodes whenever the Mac reached its limit, and each cut build
/// ran again later, so the fleet worked one task at a time all day. An hour at critical pressure
/// with three nodes building now pauses none of them; memory only holds what has not started yet.
#[tokio::test]
async fn an_hour_at_critical_pressure_pauses_no_working_node_and_only_holds_new_starts() {
    use TaskPriority::{High, Low, Normal};
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", High), ("t2", Normal), ("t3", Low)], Limit::Fixed(6), base - 10 * MIN);
    let mut now = base;
    while now < base + 60 * MIN {
        fleet.tick_with(building(now, -2.0, MemoryPressure::Critical)).await;
        now += SAMPLE;
    }
    assert!(fleet.events("pause").is_empty(), "{:?}", fleet.log);
    assert_eq!(fleet.working(now), 3);
    for task in ["t1", "t2", "t3"] {
        assert_eq!(phase(&fleet, task), NodePhase::Working, "{task}");
    }
    assert!(fleet.domain.live.gate.pauses(&fleet.domain).paused.is_empty());
    assert!(matches!(ask_new(&fleet, now), Err(Hold::Memory { why: NoRoom::Pressure(MemoryPressure::Critical), .. })), "a new start waits for memory");
    assert_eq!(fleet.notices, 0, "the orchestrator has nothing to do about memory");
}

/// A build that brings macOS to warning: the nodes at work go on unpaused, no new node starts
/// while it lasts, and one does a minute after macOS is back to normal with margin.
#[tokio::test]
async fn a_build_that_brings_macos_to_warning_holds_new_starts_while_the_running_nodes_keep_working() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], Limit::Fixed(6), base - 10 * MIN);
    let mut now = base;
    while now < base + 10 * MIN {
        let (margin, pressure) = if now - base < 2 * MIN { (0.5, MemoryPressure::Normal) } else { (-1.5, MemoryPressure::Warning) };
        fleet.tick_with(building(now, margin, pressure)).await;
        now += SAMPLE;
    }
    assert!(fleet.events("pause").is_empty(), "{:?}", fleet.log);
    for task in ["t1", "t2", "t3"] {
        assert_eq!(phase(&fleet, task), NodePhase::Working, "{task}");
    }
    assert!(matches!(ask_new(&fleet, now), Err(Hold::Memory { why: NoRoom::Pressure(MemoryPressure::Warning), .. })));
    let cleared = now;
    while now < cleared + MIN {
        fleet.tick_with(building(now, 3.0, MemoryPressure::Normal)).await;
        now += SAMPLE;
    }
    assert_eq!(ask_new(&fleet, now), Ok(()), "a minute of room after the build");
    assert!(fleet.events("pause").is_empty());
}

/// Nodes an older daemon paused for memory return on the first look after the upgrade and come
/// back a start apart, like any node: their pause was memory's, and memory no longer pauses.
#[tokio::test]
async fn nodes_an_older_daemon_paused_for_memory_come_back_through_the_gate_a_start_apart() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], Limit::Fixed(6), base - 10 * MIN);
    fleet.paused_for_memory_before("t2", base);
    fleet.paused_for_memory_before("t3", base);
    let now = fleet.hold(50, base, 5 * MIN).await;
    let resumes = fleet.events("resume");
    assert_eq!(tasks(&resumes), ["t2", "t3"], "{:?}", fleet.log);
    assert_eq!(resumes[0].0, base, "on the first look");
    assert_eq!(resumes[1].0 - resumes[0].0, SPACING, "{resumes:?}");
    let told = fleet.nodes[1].1.lines.lock().unwrap().iter().filter_map(|line| line["text"].as_str().map(str::to_owned)).collect::<Vec<_>>();
    assert!(told.iter().any(|text| text.contains("The Mac has room again: carry on with your task")), "{told:?}");
    assert_eq!(fleet.working(now), 3);
    assert!(fleet.events("pause").is_empty());
}

/// D3: the user's Resume goes through at once, even over the limit, and holds the next slot; a
/// pause by the user never comes back by itself.
#[tokio::test]
async fn the_users_resume_goes_through_over_the_limit_and_their_pause_never_returns_by_itself() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], Limit::Fixed(2), base - 10 * MIN);
    fleet.domain.live.gate.pause_by_user(&fleet.domain, "agent-t3", base).unwrap();
    let now = fleet.hold(50, base, 20 * MIN).await;
    assert_eq!(tasks(&fleet.events("pause")), ["t3"]);
    assert!(fleet.events("resume").is_empty(), "a pause by the user waits for the user");
    let pacing = fleet.domain.live.gate.live(&fleet.domain, now).unwrap();
    assert_eq!(pacing.paused[0].reason, alondra_core::views::LivePauseReason::User);

    assert!(fleet.domain.live.gate.resume_now(&fleet.domain, "agent-t3", now));
    fleet.tick(50, now).await;
    assert_eq!(tasks(&fleet.events("resume")), ["t3"]);
    assert_eq!(fleet.working(now), 3, "over the limit of 2");
    let waiting = Waiting { key: Key::Task("t-new".into()), priority: Normal, resuming: false, since: now };
    assert!(fleet.domain.live.gate.ask(&fleet.domain, waiting, now + 5 * MIN).is_err(), "it counts toward the limit");
}

/// The orchestrator cannot message a paused node: that would start it again past the limit.
#[tokio::test]
async fn the_orchestrator_is_told_a_paused_node_resumes_by_itself() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let fleet = Fleet::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    fleet.paused_for_memory_before("t2", base);
    let body = serde_json::from_value(serde_json::json!({"node": "agent-t2", "text": "carry on"})).unwrap();
    let refused = crate::domain_orchestrator_tools::send(&fleet.domain, &fleet.home.0, &Actor::Orchestrator { generation: 1 }, body).await.unwrap_err();
    assert_eq!(refused.code, "node_paused");
    assert!(refused.message.contains("Alondra paused it for the machine and resumes it by itself"), "{}", refused.message);
}

/// Two nodes that declared a wait no gauge measures are the nodes' state, not a card: they wait
/// out the grace with the Mac fine and come back one at a time, a start apart.
#[tokio::test]
async fn two_nodes_waiting_for_the_machine_with_the_mac_fine_come_back_one_at_a_time_after_the_grace() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    fleet.waits_for_machine("t1", base);
    fleet.waits_for_machine("t2", base + SAMPLE);
    fleet.hold(50, base, 12 * MIN).await;
    let resumes = fleet.events("resume");
    assert_eq!(tasks(&resumes), ["t1", "t2"]);
    assert!(resumes[0].0 >= base + DECLARED_GRACE, "{resumes:?}");
    assert!(resumes[1].0 - resumes[0].0 >= SPACING, "{resumes:?}");
    let cards = fleet.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(alondra_core::machine_incident::MACHINE_PREFIX)).count();
    assert_eq!(cards, 0);
    assert!(fleet.events("pause").is_empty(), "a node that waits by itself is never interrupted");
}

/// A node that declared its wait is left alone: no interrupt and nothing of its own stopped, even
/// with the Mac at its limit; only the user's pause interrupts and cleans up.
#[tokio::test]
async fn a_declared_wait_is_never_interrupted_nor_cleaned_up_and_shows_the_nodes_reason() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal)], Limit::Fixed(6), base - 10 * MIN);
    fleet.domain.live.gate.hold_back(&fleet.domain, "agent-t1", Reason::Declared { reason: "cola de compilación".into() }, base);
    fleet.hold(3, base, 2 * MIN).await;
    assert!(fleet.events("pause").is_empty());
    let paused = fleet.domain.live.gate.paused(&fleet.domain, "agent-t1").unwrap();
    assert_eq!(paused.cleanup, Cleanup::Done, "nothing of its own is stopped");
    let pacing = fleet.domain.live.gate.live(&fleet.domain, base + 2 * MIN).unwrap();
    assert_eq!(pacing.paused[0].waits_for.as_deref(), Some("cola de compilación"));
}

/// When the usage limit resets the nodes it stopped come back through the queue, not all at once.
#[tokio::test]
async fn the_usage_limit_reset_brings_nodes_back_one_at_a_time() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], Limit::Fixed(6), base - 10 * MIN);
    for task in ["t1", "t2", "t3"] {
        fleet.domain.live.gate.hold_back(&fleet.domain, &format!("agent-{task}"), Reason::Usage, base);
        fleet.end_turn(task, base);
    }
    fleet.hold(50, base, 10 * MIN).await;
    let resumes = fleet.events("resume");
    assert_eq!(resumes.len(), 3);
    assert!(resumes.windows(2).all(|pair| pair[1].0 - pair[0].0 >= SPACING), "{resumes:?}");
}

/// The night three paused nodes went back to work: the pause cut each turn, then Alondra stopped
/// what their shells left running, and the end of those background commands started a new turn.
/// A node the user paused is stopped again whenever it starts a turn by itself, and stays paused.
#[tokio::test]
async fn a_node_the_user_paused_that_starts_a_turn_by_itself_is_stopped_again_and_stays_paused() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    fleet.domain.live.gate.pause_by_user(&fleet.domain, "agent-t2", base).unwrap();
    fleet.tick(50, base).await;
    assert_eq!(tasks(&fleet.events("pause")), ["t2"]);

    fleet.domain.report(Envelope { session_id: "agent-t2".into(), report: Report::TurnStart }, base + 12_000);
    fleet.tick(50, base + 20_000).await;
    assert_eq!(tasks(&fleet.events("pause")), ["t2", "t2"], "its new turn is cut");
    assert_eq!(fleet.domain.live.gate.paused(&fleet.domain, "agent-t2").map(|paused| paused.reason), Some(Reason::ByUser));

    fleet.hold(50, base + 30_000, 20 * MIN).await;
    assert_eq!(tasks(&fleet.events("pause")), ["t2", "t2"], "a turn already cut is not cut again");
    assert!(fleet.events("resume").is_empty(), "only the user resumes it");
    assert!(fleet.domain.live.gate.paused_by_user(&fleet.domain, "agent-t2"));
}

/// A node an older daemon paused for memory that starts a turn by itself while memory has no room
/// is stopped again, as the node machine keeps it paused, and stays so until the gate resumes it.
#[tokio::test]
async fn a_node_paused_for_memory_that_starts_a_turn_by_itself_is_stopped_again_and_stays_paused() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    fleet.paused_for_memory_before("t2", base);
    fleet.tick(3, base).await;
    fleet.domain.report(Envelope { session_id: "agent-t2".into(), report: Report::TurnStart }, base + 12_000);
    fleet.tick(3, base + 20_000).await;
    assert_eq!(tasks(&fleet.events("pause")), ["t2"], "its new turn is cut");
    assert_eq!(fleet.domain.live.gate.paused(&fleet.domain, "agent-t2").map(|paused| paused.reason), Some(Reason::Memory));
    assert_eq!(phase(&fleet, "t2").key(), "paused");
    assert!(fleet.events("resume").is_empty());
}

/// The loop of 13 October: memory ran out with two nodes building, Alondra paused the newer one
/// and stopped its build, which then ran again and again. Two builds that bring macOS to warning,
/// even critical, now both run to the end without a cut.
#[tokio::test]
async fn two_builds_that_exhaust_memory_both_finish_without_a_cut() {
    use TaskPriority::Normal;
    const BUILD: Millis = 8 * MIN;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("long", Normal), ("rerun", Normal)], Limit::Fixed(6), base - 10 * MIN);
    let mut now = base;
    while now < base + BUILD {
        let pressure = if now - base < BUILD / 2 { alondra_core::MemoryPressure::Warning } else { alondra_core::MemoryPressure::Critical };
        fleet.tick_with(Machine { at: now, memory_free_percent: Some(4), memory_pressure: Some(pressure), cpus: 11, load: [5.0; 3], ..Machine::default() }).await;
        now += SAMPLE;
    }
    assert!(fleet.events("pause").is_empty(), "{:?}", fleet.log);
    assert_eq!(phase(&fleet, "long"), NodePhase::Working);
    assert_eq!(phase(&fleet, "rerun"), NodePhase::Working);
}

/// The user's own message ends their pause, since it is what the node goes on with; the
/// orchestrator cannot write to it at all.
#[tokio::test]
async fn the_users_message_ends_their_pause_and_the_orchestrators_does_not() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    fleet.domain.live.gate.pause_by_user(&fleet.domain, "agent-t2", base).unwrap();
    fleet.tick(50, base).await;

    let body = serde_json::from_value(serde_json::json!({"node": "agent-t2", "text": "carry on"})).unwrap();
    let refused = crate::domain_orchestrator_tools::send(&fleet.domain, &fleet.home.0, &Actor::Orchestrator { generation: 1 }, body).await.unwrap_err();
    assert_eq!(refused.code, "node_paused");
    assert!(fleet.domain.live.gate.paused_by_user(&fleet.domain, "agent-t2"));

    let body = serde_json::from_value(serde_json::json!({"text": "Use the new API"})).unwrap();
    crate::domain_messages::deliver(&fleet.domain, &fleet.home.0, &user(), "The user", &AgentId("agent-t2".into()), body, true).await.unwrap();
    assert!(fleet.domain.live.gate.paused(&fleet.domain, "agent-t2").is_none());
}

/// What Claude Code's `StopFailure` hook carries, handed to the daemon as its hook path does.
fn api_error(fleet: &Fleet, task: &str, error: &str, now: Millis) {
    let session = format!("agent-{task}");
    let payload = serde_json::json!({"session_id": session, "hook_event_name": "StopFailure", "error_type": error,
        "error_details": "API Error: Connection lost mid-response. The response above may be incomplete."});
    fleet.domain.live.observe(&payload);
    crate::api_failures::turn_failed(&fleet.domain, &AgentId(session), &payload, now).unwrap();
}

fn phase(fleet: &Fleet, task: &str) -> alondra_core::domain::NodePhase {
    fleet.domain.state().unwrap().node_phase(&AgentId(format!("agent-{task}"))).unwrap().clone()
}

fn api_cards(fleet: &Fleet) -> Vec<alondra_core::domain::Interaction> {
    let state = fleet.domain.state().unwrap();
    state.interactions.values().filter(|card| state.open(card) && card.body.contains("api_failed")).cloned().collect()
}

fn answer(fleet: &Fleet, card: &alondra_core::domain::Interaction, option: &str) {
    run(&fleet.domain, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.id.clone(), option_id: option.into(), inputs: Default::default() });
    crate::api_failures::settle_cards(&fleet.domain).unwrap();
}

/// The case of 9 October: "API Error: Connection lost mid-response" ended a node's turn and it
/// sat there until someone wrote to it. Now it waits 30 s without a slot and Alondra sends it back.
#[tokio::test]
async fn a_turn_an_api_error_cut_is_sent_back_by_itself_and_its_node_works_again() {
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    api_error(&fleet, "t1", "server_error", base);
    assert!(matches!(phase(&fleet, "t1"), NodePhase::Paused(Pause { reason: PauseReason::ApiError(ApiRetry { attempt: 1, outage: false, .. }), .. })));
    assert_eq!(fleet.working(base), 0, "a node waiting out an API error holds no slot");
    let pacing = fleet.domain.live.gate.live(&fleet.domain, base).unwrap();
    assert_eq!(pacing.paused[0].reason, alondra_core::views::LivePauseReason::ApiError);
    assert_eq!(pacing.paused[0].retry.as_ref().map(|retry| retry.attempt), Some(1));
    fleet.hold(50, base, 30_000 - 1).await;
    assert!(fleet.events("resume").is_empty(), "not before its retry");
    fleet.tick(50, base + 30_000).await;
    assert_eq!(tasks(&fleet.events("resume")), ["t1"]);
    assert_eq!(phase(&fleet, "t1"), NodePhase::Working);
    let line = fleet.nodes[0].1.lines.lock().unwrap().last().cloned().unwrap();
    let text = line["text"].as_str().unwrap();
    assert!(text.contains("Claude API error (API Error: Connection lost mid-response.") && text.contains("Carry on where you left off"), "{text}");
    assert!(api_cards(&fleet).is_empty());
}

/// Every node failing at once is the API down: they wait for it with no card and no new start,
/// one of them probing it, and all come back once the model answers again.
#[tokio::test]
async fn while_the_api_is_down_for_every_node_they_wait_without_a_card_and_come_back_with_it() {
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", TaskPriority::Normal), ("t2", TaskPriority::Normal), ("t3", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    api_error(&fleet, "t1", "overloaded", base);
    api_error(&fleet, "t2", "overloaded", base + 1_000);
    api_error(&fleet, "t3", "overloaded", base + 2_000);
    assert!(fleet.domain.live.api_watch.down(base + 2_000), "no new node starts while it is down");
    for task in ["t2", "t3"] {
        assert!(matches!(phase(&fleet, task), NodePhase::Paused(Pause { reason: PauseReason::ApiError(ApiRetry { outage: true, .. }), .. })), "{task}");
    }
    let mut now = base + 2_000;
    for _ in 0..30 {
        now = fleet.hold(50, now, OUTAGE_PROBE).await;
        let back: Vec<String> = fleet.events("resume").into_iter().map(|(_, task)| task).collect();
        let last = back.last().cloned();
        if let Some(task) = last.filter(|task| phase(&fleet, task) == NodePhase::Working) {
            api_error(&fleet, &task, "overloaded", now);
        }
        assert!(fleet.working(now) <= 1, "one node at a time probes the API");
    }
    assert!(api_cards(&fleet).is_empty(), "an outage of an hour raises no card");
    assert!(fleet.events("resume").len() >= 25, "it kept probing: {:?}", fleet.events("resume"));

    let probe = fleet.events("resume").last().unwrap().1.clone();
    fleet.domain.live.api_watch.answered(&format!("agent-{probe}"), now + 1);
    let now = fleet.hold(50, now + 2, 5 * MIN).await;
    assert_eq!(fleet.working(now), 3, "the model answered: everyone came back");
    assert!(!fleet.domain.live.api_watch.down(now));
}

/// One node failing while another gets answers is not an outage: after 30 s, 2 min and 10 min
/// a card asks the user, and "try again now" sends the node back at once.
#[tokio::test]
async fn a_node_the_api_keeps_failing_while_others_work_asks_the_user_after_its_retries() {
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", TaskPriority::Normal), ("t2", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    let mut now = base;
    for attempt in 1..=API_RETRIES.len() {
        api_error(&fleet, "t1", "server_error", now);
        fleet.domain.live.api_watch.answered("agent-t2", now + 1);
        let wait = API_RETRIES[attempt - 1];
        now = fleet.hold(50, now + 1, wait + 1).await;
        assert_eq!(fleet.events("resume").len(), attempt, "attempt {attempt} came back after {wait} ms");
        assert!(api_cards(&fleet).is_empty());
    }
    api_error(&fleet, "t1", "server_error", now);
    let cards = api_cards(&fleet);
    assert_eq!(cards.len(), 1, "the fourth failure in a row asks the user");
    assert_eq!(phase(&fleet, "t1"), NodePhase::WaitingUser { in_turn: false }, "the user holds it now, not a slot");
    let es = alondra_core::start_problem::card("Task t1", &serde_json::from_str(&cards[0].body).unwrap(), None, alondra_core::views::Lang::Es);
    assert_eq!(es.need, "Los errores de la API de Claude cortan una y otra vez «Task t1»");
    let options: Vec<&str> = match &cards[0].payload {
        alondra_core::domain::InteractionPayload::Question { options, .. } => options.iter().map(|option| option.id.as_str()).collect(),
        _ => panic!("a question"),
    };
    assert_eq!(options, [alondra_core::domain::START_RETRY, alondra_core::domain::START_CANCEL]);

    let resumes = fleet.events("resume").len();
    answer(&fleet, &cards[0], alondra_core::domain::START_RETRY);
    fleet.tick(50, now + SAMPLE).await;
    assert_eq!(fleet.events("resume").len(), resumes + 1, "\"try again now\" sends it back at once");
    assert_eq!(phase(&fleet, "t1"), NodePhase::Working);

    api_error(&fleet, "t1", "server_error", now + MIN);
    assert!(matches!(phase(&fleet, "t1"), NodePhase::Paused(Pause { reason: PauseReason::ApiError(ApiRetry { attempt: 1, .. }), .. })),
        "after the user's retry Alondra counts again from the first wait");
}

#[tokio::test]
async fn cancelling_on_the_card_closes_the_task_and_an_error_retrying_cannot_fix_asks_at_once() {
    let base = alondra_core::now_millis();
    let fleet = Fleet::new(&[("t1", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    api_error(&fleet, "t1", "authentication_failed", base);
    let cards = api_cards(&fleet);
    assert_eq!(cards.len(), 1, "signing in again is the user's to do: no retry");
    let es = alondra_core::start_problem::card("Task t1", &serde_json::from_str(&cards[0].body).unwrap(), None, alondra_core::views::Lang::Es);
    assert!(es.why.contains("inicia sesión"), "{es:?}");
    answer(&fleet, &cards[0], alondra_core::domain::START_CANCEL);
    let state = fleet.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from("t1")].state, TaskState::Cancelled);
    assert!(matches!(phase(&fleet, "t1"), NodePhase::Closing | NodePhase::Closed));
}

/// The usage limit waits for its reset, without a slot and without a card.
#[tokio::test]
async fn a_usage_limit_pauses_the_node_until_it_resets() {
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    api_error(&fleet, "t1", "rate_limit", base);
    assert!(matches!(phase(&fleet, "t1"), NodePhase::Paused(Pause { reason: PauseReason::Usage, .. })));
    assert_eq!(fleet.working(base), 0);
    fleet.hold(50, base, 10 * MIN).await;
    assert!(fleet.events("resume").is_empty(), "the limit has not reset");
    assert!(api_cards(&fleet).is_empty());
}

/// Only a turn an API error cut is sent back: a turn that ended normally, or one that waits on
/// the user, never is.
#[tokio::test]
async fn a_normal_end_or_a_wait_on_the_user_is_never_sent_back() {
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", TaskPriority::Normal), ("t2", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    fleet.end_turn("t1", base);
    run(&fleet.domain, Actor::Agent { agent_id: AgentId("agent-t2".into()) }, Command::AskQuestion {
        attachments: vec![], interaction_id: "q-t2".into(), scope: alondra_core::domain::Scope::Task { task_id: "t2".into() },
        recipient: alondra_core::domain::Recipient::User, body: "Which name?".into(), blocking: false,
        options: vec![
            alondra_core::domain::QuestionOption { id: "a".into(), label: "Atlas".into(), description: None },
            alondra_core::domain::QuestionOption { id: "b".into(), label: "Borealis".into(), description: None },
        ],
        card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Choose the release name").why("The name goes on the release"),
    });
    api_error(&fleet, "t2", "server_error", base + 1_000);
    assert_eq!(phase(&fleet, "t2"), NodePhase::WaitingUser { in_turn: false });
    fleet.hold(50, base + 2_000, 15 * MIN).await;
    assert!(fleet.events("resume").is_empty(), "{:?}", fleet.events("resume"));
    assert!(api_cards(&fleet).is_empty());
}
