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

/// Nodes on one Mac, each connected through a mod that acts like the real one: an
/// interrupt ends its turn, a message starts a new one.
struct Bench {
    home: Home,
    domain: Arc<DomainRuntime>,
    nodes: Vec<(String, Node, usize)>,
    /// Every pause and every resume, in order, by task.
    log: Vec<(Millis, &'static str, String)>,
    max_working: usize,
    notices: usize,
}

impl Bench {
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
            run(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
                task_id: (*id).into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
                dependencies: Default::default(), stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
            });
            if !priority.is_normal() {
                run(&domain, user(), Command::SetPriority { task_id: (*id).into(), priority: *priority });
            }
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
        crate::admission::working_tasks(&self.domain.state().unwrap()).len()
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
        crate::admission::pass_at(&self.domain, &self.home.0, now).await.unwrap();
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
fn ask_new(bench: &Bench, now: Millis) -> Result<(), Hold> {
    if !bench.domain.state().unwrap().tasks.contains_key(&TaskId::from("t-new")) {
        run(&bench.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
            task_id: "t-new".into(), request_id: "r".into(), title: "Task t-new".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
    }
    let waiting = Waiting { key: Key::Task("t-new".into()), priority: TaskPriority::High, resuming: false, since: now };
    bench.domain.live.gate.ask(&bench.domain, waiting, now)
}

/// 13 October: Alondra paused working nodes whenever the Mac reached its limit, and each cut build
/// ran again later, so the Mac worked one task at a time all day. An hour at critical pressure
/// with three nodes building now pauses none of them; memory only holds what has not started yet.
#[tokio::test]
async fn an_hour_at_critical_pressure_pauses_no_working_node_and_only_holds_new_starts() {
    use TaskPriority::{High, Low, Normal};
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", High), ("t2", Normal), ("t3", Low)], Limit::Fixed(6), base - 10 * MIN);
    let mut now = base;
    while now < base + 60 * MIN {
        bench.tick_with(building(now, -2.0, MemoryPressure::Critical)).await;
        now += SAMPLE;
    }
    assert!(bench.events("pause").is_empty(), "{:?}", bench.log);
    assert_eq!(bench.working(now), 3);
    for task in ["t1", "t2", "t3"] {
        assert_eq!(phase(&bench, task), NodePhase::Working, "{task}");
    }
    assert!(bench.domain.live.gate.pauses(&bench.domain).paused.is_empty());
    assert!(matches!(ask_new(&bench, now), Err(Hold::Memory { why: NoRoom::Pressure(MemoryPressure::Critical), .. })), "a new start waits for memory");
    assert_eq!(bench.notices, 0, "the orchestrator has nothing to do about memory");
}

/// A build that brings macOS to critical pressure: the nodes at work go on unpaused, no new node
/// starts while it lasts, and one does a minute after macOS is back to normal.
#[tokio::test]
async fn a_build_that_brings_macos_to_critical_pressure_holds_new_starts_while_the_running_nodes_keep_working() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], Limit::Fixed(6), base - 10 * MIN);
    let mut now = base;
    while now < base + 10 * MIN {
        let (margin, pressure) = if now - base < 2 * MIN { (0.5, MemoryPressure::Normal) } else { (-1.5, MemoryPressure::Critical) };
        bench.tick_with(building(now, margin, pressure)).await;
        now += SAMPLE;
    }
    assert!(bench.events("pause").is_empty(), "{:?}", bench.log);
    for task in ["t1", "t2", "t3"] {
        assert_eq!(phase(&bench, task), NodePhase::Working, "{task}");
    }
    assert!(matches!(ask_new(&bench, now), Err(Hold::Memory { why: NoRoom::Pressure(MemoryPressure::Critical), .. })));
    let cleared = now;
    while now < cleared + MIN {
        bench.tick_with(building(now, 3.0, MemoryPressure::Normal)).await;
        now += SAMPLE;
    }
    assert_eq!(ask_new(&bench, now), Ok(()), "a minute of room after the build");
    assert!(bench.events("pause").is_empty());
}

/// Nodes an older daemon paused for memory return on the first look after the upgrade and come
/// back a start apart, like any node: their pause was memory's, and memory no longer pauses.
#[tokio::test]
async fn nodes_an_older_daemon_paused_for_memory_come_back_through_the_gate_a_start_apart() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], Limit::Fixed(6), base - 10 * MIN);
    bench.paused_for_memory_before("t2", base);
    bench.paused_for_memory_before("t3", base);
    let now = bench.hold(50, base, 5 * MIN).await;
    let resumes = bench.events("resume");
    assert_eq!(tasks(&resumes), ["t2", "t3"], "{:?}", bench.log);
    assert_eq!(resumes[0].0, base, "on the first look");
    assert_eq!(resumes[1].0 - resumes[0].0, SPACING, "{resumes:?}");
    let told = bench.nodes[1].1.lines.lock().unwrap().iter().filter_map(|line| line["text"].as_str().map(str::to_owned)).collect::<Vec<_>>();
    assert!(told.iter().any(|text| text.contains("The Mac has room again: carry on with your task")), "{told:?}");
    assert_eq!(bench.working(now), 3);
    assert!(bench.events("pause").is_empty());
}

/// The user's Resume counts toward the limit like a start: with every slot taken the node waits in
/// the queue, and goes on once a slot is free; a pause by the user never comes back by itself.
#[tokio::test]
async fn the_users_resume_waits_for_a_slot_and_their_pause_never_returns_by_itself() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], Limit::Fixed(2), base - 10 * MIN);
    bench.domain.live.gate.pause_by_user(&bench.domain, "agent-t3", base).unwrap();
    let now = bench.hold(50, base, 20 * MIN).await;
    assert_eq!(tasks(&bench.events("pause")), ["t3"]);
    assert!(bench.events("resume").is_empty(), "a pause by the user waits for the user");
    let pacing = bench.domain.live.gate.live(&bench.domain, now).unwrap();
    assert_eq!(pacing.paused[0].reason, alondra_core::views::LivePauseReason::User);

    assert!(bench.domain.live.gate.resume_now(&bench.domain, "agent-t3", now));
    let now = bench.hold(50, now, 5 * MIN).await;
    assert!(bench.events("resume").is_empty(), "t1 and t2 hold the limit of 2");
    assert_eq!(bench.working(now), 2);
    let pacing = bench.domain.live.gate.live(&bench.domain, now).unwrap();
    assert!(pacing.paused.is_empty(), "it no longer waits for the user");
    assert_eq!(pacing.queued.iter().map(|queued| queued.task_id.as_str()).collect::<Vec<_>>(), ["t3"]);

    bench.end_turn("t1", now);
    let now = bench.hold(50, now, 2 * MIN).await;
    assert_eq!(tasks(&bench.events("resume")), ["t3"]);
    assert_eq!(bench.working(now), 2, "it took the slot t1 left");
    let waiting = Waiting { key: Key::Task("t-new".into()), priority: Normal, resuming: false, since: now };
    assert!(bench.domain.live.gate.ask(&bench.domain, waiting, now + 5 * MIN).is_err(), "it counts toward the limit");
}

/// The orchestrator cannot message a paused node: that would start it again past the limit.
#[tokio::test]
async fn the_orchestrator_is_told_a_paused_node_resumes_by_itself() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let bench = Bench::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    bench.paused_for_memory_before("t2", base);
    let body = serde_json::from_value(serde_json::json!({"node": "agent-t2", "text": "carry on"})).unwrap();
    let refused = crate::domain_orchestrator_tools::send(&bench.domain, &bench.home.0, &Actor::Orchestrator { generation: 1 }, body).await.unwrap_err();
    assert_eq!(refused.code, "node_paused");
    assert!(refused.message.contains("Alondra paused it for the machine and resumes it by itself"), "{}", refused.message);
}

/// Two nodes that declared a wait no gauge measures are the nodes' state, not a card: with the
/// Mac fine they come back one at a time, a start apart, without waiting out any minutes.
#[tokio::test]
async fn two_nodes_waiting_for_the_machine_with_the_mac_fine_come_back_one_at_a_time_at_once() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    bench.waits_for_machine("t1", base);
    bench.waits_for_machine("t2", base + SAMPLE);
    bench.hold(50, base, 12 * MIN).await;
    let resumes = bench.events("resume");
    assert_eq!(tasks(&resumes), ["t1", "t2"]);
    assert!(resumes[0].0 < base + MIN, "the measures say there is room: no grace to wait out {resumes:?}");
    assert!(resumes[1].0 - resumes[0].0 >= SPACING, "{resumes:?}");
    let cards = bench.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(alondra_core::machine_incident::MACHINE_PREFIX)).count();
    assert_eq!(cards, 0);
    assert!(bench.events("pause").is_empty(), "a node that waits by itself is never interrupted");
}

/// A node waits for the machine while the Mac has no room: hours pass with nothing new measured,
/// and nothing brings it back, reminds anyone or raises a card; the first measurements with room
/// bring it back.
#[tokio::test]
async fn a_declared_wait_without_room_stays_for_hours_and_returns_once_the_measures_say_there_is_room() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", Normal)], Limit::Fixed(6), base - 10 * MIN);
    bench.waits_for_machine("t1", base);
    let mut now = base;
    for _ in 0..36 {
        bench.tick(3, now).await;
        now += 10 * MIN;
    }
    assert!(bench.events("resume").is_empty(), "six hours without room: it still waits");
    let cards = bench.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(alondra_core::machine_incident::MACHINE_PREFIX)).count();
    assert_eq!((cards, bench.notices), (0, 0));
    bench.hold(50, now, 3 * MIN).await;
    assert_eq!(tasks(&bench.events("resume")), ["t1"]);
}

/// A node that declared its wait is left alone: no interrupt and nothing of its own stopped, even
/// with the Mac at its limit; only the user's pause interrupts and cleans up.
#[tokio::test]
async fn a_declared_wait_is_never_interrupted_nor_cleaned_up_and_shows_the_nodes_reason() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", Normal)], Limit::Fixed(6), base - 10 * MIN);
    bench.domain.live.gate.hold_back(&bench.domain, "agent-t1", Reason::Declared { reason: "cola de compilación".into() }, base);
    bench.hold(3, base, 2 * MIN).await;
    assert!(bench.events("pause").is_empty());
    let paused = bench.domain.live.gate.paused(&bench.domain, "agent-t1").unwrap();
    assert_eq!(paused.cleanup, Cleanup::Done, "nothing of its own is stopped");
    let pacing = bench.domain.live.gate.live(&bench.domain, base + 2 * MIN).unwrap();
    assert_eq!(pacing.paused[0].waits_for.as_deref(), Some("cola de compilación"));
}

/// When the usage limit resets the nodes it stopped come back through the queue, not all at once.
#[tokio::test]
async fn the_usage_limit_reset_brings_nodes_back_one_at_a_time() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], Limit::Fixed(6), base - 10 * MIN);
    for task in ["t1", "t2", "t3"] {
        bench.domain.live.gate.hold_back(&bench.domain, &format!("agent-{task}"), Reason::Usage, base);
        bench.end_turn(task, base);
    }
    bench.hold(50, base, 10 * MIN).await;
    let resumes = bench.events("resume");
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
    let mut bench = Bench::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    bench.domain.live.gate.pause_by_user(&bench.domain, "agent-t2", base).unwrap();
    bench.tick(50, base).await;
    assert_eq!(tasks(&bench.events("pause")), ["t2"]);

    bench.domain.report(Envelope { session_id: "agent-t2".into(), report: Report::TurnStart }, base + 12_000);
    bench.tick(50, base + 20_000).await;
    assert_eq!(tasks(&bench.events("pause")), ["t2", "t2"], "its new turn is cut");
    assert_eq!(bench.domain.live.gate.paused(&bench.domain, "agent-t2").map(|paused| paused.reason), Some(Reason::ByUser));

    bench.hold(50, base + 30_000, 20 * MIN).await;
    assert_eq!(tasks(&bench.events("pause")), ["t2", "t2"], "a turn already cut is not cut again");
    assert!(bench.events("resume").is_empty(), "only the user resumes it");
    assert!(bench.domain.live.gate.paused_by_user(&bench.domain, "agent-t2"));
}

/// A node an older daemon paused for memory that starts a turn by itself while memory has no room
/// is stopped again, as the node machine keeps it paused, and stays so until the gate resumes it.
#[tokio::test]
async fn a_node_paused_for_memory_that_starts_a_turn_by_itself_is_stopped_again_and_stays_paused() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    bench.paused_for_memory_before("t2", base);
    bench.tick(3, base).await;
    bench.domain.report(Envelope { session_id: "agent-t2".into(), report: Report::TurnStart }, base + 12_000);
    bench.tick(3, base + 20_000).await;
    assert_eq!(tasks(&bench.events("pause")), ["t2"], "its new turn is cut");
    assert_eq!(bench.domain.live.gate.paused(&bench.domain, "agent-t2").map(|paused| paused.reason), Some(Reason::Memory));
    assert_eq!(phase(&bench, "t2").key(), "paused");
    assert!(bench.events("resume").is_empty());
}

/// The loop of 13 October: memory ran out with two nodes building, Alondra paused the newer one
/// and stopped its build, which then ran again and again. Two builds that bring macOS to warning,
/// even critical, now both run to the end without a cut.
#[tokio::test]
async fn two_builds_that_exhaust_memory_both_finish_without_a_cut() {
    use TaskPriority::Normal;
    const BUILD: Millis = 8 * MIN;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("long", Normal), ("rerun", Normal)], Limit::Fixed(6), base - 10 * MIN);
    let mut now = base;
    while now < base + BUILD {
        let pressure = if now - base < BUILD / 2 { alondra_core::MemoryPressure::Warning } else { alondra_core::MemoryPressure::Critical };
        bench.tick_with(Machine { at: now, memory_free_percent: Some(4), memory_pressure: Some(pressure), cpus: 11, load: [5.0; 3], ..Machine::default() }).await;
        now += SAMPLE;
    }
    assert!(bench.events("pause").is_empty(), "{:?}", bench.log);
    assert_eq!(phase(&bench, "long"), NodePhase::Working);
    assert_eq!(phase(&bench, "rerun"), NodePhase::Working);
}

/// The user's own message ends their pause, since it is what the node goes on with; the
/// orchestrator cannot write to it at all.
#[tokio::test]
async fn the_users_message_ends_their_pause_and_the_orchestrators_does_not() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    bench.domain.live.gate.pause_by_user(&bench.domain, "agent-t2", base).unwrap();
    bench.tick(50, base).await;

    let body = serde_json::from_value(serde_json::json!({"node": "agent-t2", "text": "carry on"})).unwrap();
    let refused = crate::domain_orchestrator_tools::send(&bench.domain, &bench.home.0, &Actor::Orchestrator { generation: 1 }, body).await.unwrap_err();
    assert_eq!(refused.code, "node_paused");
    assert!(bench.domain.live.gate.paused_by_user(&bench.domain, "agent-t2"));

    let body = serde_json::from_value(serde_json::json!({"text": "Use the new API"})).unwrap();
    crate::domain_messages::deliver(&bench.domain, &bench.home.0, &user(), "The user", &AgentId("agent-t2".into()), body, true).await.unwrap();
    assert!(bench.domain.live.gate.paused(&bench.domain, "agent-t2").is_none());
}

/// What Claude Code's `StopFailure` hook carries, handed to the daemon as its hook path does.
fn api_error(bench: &Bench, task: &str, error: &str, now: Millis) {
    let session = format!("agent-{task}");
    let payload = serde_json::json!({"session_id": session, "hook_event_name": "StopFailure", "error_type": error,
        "error_details": "API Error: Connection lost mid-response. The response above may be incomplete."});
    bench.domain.live.observe(&payload);
    crate::api_failures::turn_failed(&bench.domain, &AgentId(session), &payload, now).unwrap();
}

fn phase(bench: &Bench, task: &str) -> alondra_core::domain::NodePhase {
    bench.domain.state().unwrap().node_phase(&AgentId(format!("agent-{task}"))).unwrap().clone()
}

fn api_cards(bench: &Bench) -> Vec<alondra_core::domain::Interaction> {
    let state = bench.domain.state().unwrap();
    state.interactions.values().filter(|card| state.open(card) && card.body.contains("api_failed")).cloned().collect()
}

fn answer(bench: &Bench, card: &alondra_core::domain::Interaction, option: &str) {
    run(&bench.domain, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.id.clone(), option_id: option.into(), inputs: Default::default() });
    crate::api_failures::settle_cards(&bench.domain).unwrap();
}

/// The case of 9 October: "API Error: Connection lost mid-response" ended a node's turn and it
/// sat there until someone wrote to it. Now it waits 30 s without a slot and Alondra sends it back.
#[tokio::test]
async fn a_turn_an_api_error_cut_is_sent_back_by_itself_and_its_node_works_again() {
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    api_error(&bench, "t1", "server_error", base);
    assert!(matches!(phase(&bench, "t1"), NodePhase::Paused(Pause { reason: PauseReason::ApiError(ApiRetry { attempt: 1, outage: false, .. }), .. })));
    assert_eq!(bench.working(base), 0, "a node waiting out an API error holds no slot");
    let pacing = bench.domain.live.gate.live(&bench.domain, base).unwrap();
    assert_eq!(pacing.paused[0].reason, alondra_core::views::LivePauseReason::ApiError);
    assert_eq!(pacing.paused[0].retry.as_ref().map(|retry| retry.attempt), Some(1));
    bench.hold(50, base, 30_000 - 1).await;
    assert!(bench.events("resume").is_empty(), "not before its retry");
    bench.tick(50, base + 30_000).await;
    assert_eq!(tasks(&bench.events("resume")), ["t1"]);
    assert_eq!(phase(&bench, "t1"), NodePhase::Working);
    let line = bench.nodes[0].1.lines.lock().unwrap().last().cloned().unwrap();
    let text = line["text"].as_str().unwrap();
    assert!(text.contains("Claude API error (API Error: Connection lost mid-response.") && text.contains("Carry on where you left off"), "{text}");
    assert!(api_cards(&bench).is_empty());
}

/// Every node failing at once is the API down: they wait for it with no card and no new start,
/// one of them probing it, and all come back once the model answers again.
#[tokio::test]
async fn while_the_api_is_down_for_every_node_they_wait_without_a_card_and_come_back_with_it() {
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", TaskPriority::Normal), ("t2", TaskPriority::Normal), ("t3", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    api_error(&bench, "t1", "overloaded", base);
    api_error(&bench, "t2", "overloaded", base + 1_000);
    api_error(&bench, "t3", "overloaded", base + 2_000);
    assert!(bench.domain.live.api_watch.down(base + 2_000), "no new node starts while it is down");
    for task in ["t2", "t3"] {
        assert!(matches!(phase(&bench, task), NodePhase::Paused(Pause { reason: PauseReason::ApiError(ApiRetry { outage: true, .. }), .. })), "{task}");
    }
    let mut now = base + 2_000;
    for _ in 0..30 {
        now = bench.hold(50, now, OUTAGE_PROBE).await;
        let back: Vec<String> = bench.events("resume").into_iter().map(|(_, task)| task).collect();
        let last = back.last().cloned();
        if let Some(task) = last.filter(|task| phase(&bench, task) == NodePhase::Working) {
            api_error(&bench, &task, "overloaded", now);
        }
        assert!(bench.working(now) <= 1, "one node at a time probes the API");
    }
    assert!(api_cards(&bench).is_empty(), "an outage of an hour raises no card");
    assert!(bench.events("resume").len() >= 25, "it kept probing: {:?}", bench.events("resume"));

    let probe = bench.events("resume").last().unwrap().1.clone();
    bench.domain.live.api_watch.answered(&format!("agent-{probe}"), now + 1);
    let now = bench.hold(50, now + 2, 5 * MIN).await;
    assert_eq!(bench.working(now), 3, "the model answered: everyone came back");
    assert!(!bench.domain.live.api_watch.down(now));
}

/// One node failing while another gets answers is not an outage: after 30 s, 2 min and 10 min
/// a card asks the user, and "try again now" sends the node back at once.
#[tokio::test]
async fn a_node_the_api_keeps_failing_while_others_work_asks_the_user_after_its_retries() {
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", TaskPriority::Normal), ("t2", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    let mut now = base;
    for attempt in 1..=API_RETRIES.len() {
        api_error(&bench, "t1", "server_error", now);
        bench.domain.live.api_watch.answered("agent-t2", now + 1);
        let wait = API_RETRIES[attempt - 1];
        now = bench.hold(50, now + 1, wait + 1).await;
        assert_eq!(bench.events("resume").len(), attempt, "attempt {attempt} came back after {wait} ms");
        assert!(api_cards(&bench).is_empty());
    }
    api_error(&bench, "t1", "server_error", now);
    let cards = api_cards(&bench);
    assert_eq!(cards.len(), 1, "the fourth failure in a row asks the user");
    assert_eq!(phase(&bench, "t1"), NodePhase::WaitingUser { in_turn: false }, "the user holds it now, not a slot");
    let es = alondra_core::start_problem::card("Task t1", &serde_json::from_str(&cards[0].body).unwrap(), None, alondra_core::views::Lang::Es);
    assert_eq!(es.effective_title(), "Los errores de la API de Claude cortan una y otra vez «Task t1»");
    let options: Vec<&str> = match &cards[0].payload {
        alondra_core::domain::InteractionPayload::Question { options, .. } => options.iter().map(|option| option.id.as_str()).collect(),
        _ => panic!("a question"),
    };
    assert_eq!(options, [alondra_core::domain::START_RETRY, alondra_core::domain::START_CANCEL]);

    let resumes = bench.events("resume").len();
    answer(&bench, &cards[0], alondra_core::domain::START_RETRY);
    bench.tick(50, now + SAMPLE).await;
    assert_eq!(bench.events("resume").len(), resumes + 1, "\"try again now\" sends it back at once");
    assert_eq!(phase(&bench, "t1"), NodePhase::Working);

    api_error(&bench, "t1", "server_error", now + MIN);
    assert!(matches!(phase(&bench, "t1"), NodePhase::Paused(Pause { reason: PauseReason::ApiError(ApiRetry { attempt: 1, .. }), .. })),
        "after the user's retry Alondra counts again from the first wait");
}

#[tokio::test]
async fn cancelling_on_the_card_closes_the_task_and_an_error_retrying_cannot_fix_asks_at_once() {
    let base = alondra_core::now_millis();
    let bench = Bench::new(&[("t1", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    api_error(&bench, "t1", "authentication_failed", base);
    let cards = api_cards(&bench);
    assert_eq!(cards.len(), 1, "signing in again is the user's to do: no retry");
    let es = alondra_core::start_problem::card("Task t1", &serde_json::from_str(&cards[0].body).unwrap(), None, alondra_core::views::Lang::Es);
    assert!(es.why.contains("inicia sesión"), "{es:?}");
    answer(&bench, &cards[0], alondra_core::domain::START_CANCEL);
    let state = bench.domain.state().unwrap();
    assert!(state.tasks[&TaskId::from("t1")].cancelled(), "{:?}", state.tasks[&TaskId::from("t1")].state);
    assert!(matches!(phase(&bench, "t1"), NodePhase::Closing | NodePhase::Closed));
}

/// The usage limit waits for its reset, without a slot and without a card.
#[tokio::test]
async fn a_usage_limit_pauses_the_node_until_it_resets() {
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    api_error(&bench, "t1", "rate_limit", base);
    assert!(matches!(phase(&bench, "t1"), NodePhase::Paused(Pause { reason: PauseReason::Usage, .. })));
    assert_eq!(bench.working(base), 0);
    bench.hold(50, base, 10 * MIN).await;
    assert!(bench.events("resume").is_empty(), "the limit has not reset");
    assert!(api_cards(&bench).is_empty());
}

/// Only a turn an API error cut is sent back: a turn that ended normally, or one that waits on
/// the user, never is.
#[tokio::test]
async fn a_normal_end_or_a_wait_on_the_user_is_never_sent_back() {
    let base = alondra_core::now_millis();
    let mut bench = Bench::new(&[("t1", TaskPriority::Normal), ("t2", TaskPriority::Normal)], Limit::Fixed(4), base - MIN);
    bench.end_turn("t1", base);
    run(&bench.domain, Actor::Agent { agent_id: AgentId("agent-t2".into()) }, Command::AskQuestion {
        attachments: vec![], interaction_id: "q-t2".into(), scope: alondra_core::domain::Scope::Task { task_id: "t2".into() },
        recipient: alondra_core::domain::Recipient::User, body: "Which name?".into(), blocking: false,
        options: vec![
            alondra_core::domain::QuestionOption { id: "a".into(), label: "Atlas".into(), description: None },
            alondra_core::domain::QuestionOption { id: "b".into(), label: "Borealis".into(), description: None },
        ],
        card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Choose the release name").why("The name goes on the release"),
    });
    api_error(&bench, "t2", "server_error", base + 1_000);
    assert_eq!(phase(&bench, "t2"), NodePhase::WaitingUser { in_turn: false });
    bench.hold(50, base + 2_000, 15 * MIN).await;
    assert!(bench.events("resume").is_empty(), "{:?}", bench.events("resume"));
    assert!(api_cards(&bench).is_empty());
}

const SDK_TASK: &str = "t-sdk-paused";

/// A task of the Agent SDK whose turn runs, on the pinned host with the fake native Claude. Its
/// owner is driven here step by step, so nothing reaches Claude unless a step hands it over.
struct SdkNode {
    driver: crate::sdk_owner::Driver,
    domain: Arc<DomainRuntime>,
    capture: PathBuf,
    home: crate::test_support::Directory,
    _state: crate::sdk_owner::tests::HostRoot,
}

impl SdkNode {
    /// A node whose first turn the fake Claude holds open.
    fn working() -> Self {
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
            prompt: "consume-and-hold".into(), plugin: home.join("plugin"), options: Default::default(),
            model: None, account: alondra_core::accounts::AccountId::default_account(),
        }).unwrap();
        domain.driven_by_hand.lock().unwrap().insert(driver.agent().clone());
        domain.live.gate.ignore_mac();
        let mut node = Self { driver, domain, capture, home, _state: state };
        while node.lifecycle().activity() != alondra_domain::ActivityView::Working {
            node.driver.observe().unwrap();
        }
        node
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
    let tasks = serde_json::to_string(&alondra_core::views::build("tasks", alondra_core::views::RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: alondra_core::views::Client::Tui,
        lang: alondra_core::views::text::Lang::En, transport: alondra_core::views::Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap()).unwrap();
    assert!(tasks.contains("Queued · 1st of 1") && tasks.contains("0 of 0 working") && !tasks.contains("\"text\":\"Paused") && !tasks.contains("\"resume:"),
        "its task shows it queued, with no Resume: {tasks}");
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
    node.until_told("held your next turn while the disk was nearly full");
    assert!(domain.read().index().waiting().is_empty(), "nobody is asked anything");
}

/// The disk fell under its floor while two nodes built. The heavier one's turn goes on uncut and
/// still counts as working; once it ends, what the node is told waits, with no card, shown in the
/// queue for the disk; once the disk has room it gets it, and is never told it was held. The
/// lighter one, of `claude --bg`, goes on.
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
    run(&domain, Actor::System, Command::MoveNode { agent_id: AgentId("agent-light".into()), event: alondra_core::domain::NodeEvent::TurnStarted, at: 0 });
    run(&domain, user(), Command::AssignTask { task_id: "t-light".into(), agent_id: AgentId("agent-light".into()), model: None });
    let now = alondra_core::now_millis();
    domain.report(Envelope { session_id: "agent-light".into(), report: Report::TurnStart }, now);
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
    assert!(crate::admission::working_tasks(&domain.state().unwrap()).contains(&TaskId::from(SDK_TASK)), "it counts as working until its turn ends");

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
