use super::*;
use crate::fleet_gate::{Key, Limit, Tuning, Waiting};
use crate::node_push::testing::Node;
use crate::node_reports::{Envelope, Report};
use alondra_core::Machine;
use alondra_core::domain::{API_RETRIES, Actor, AgentId, ApiRetry, Command, CommandEnvelope, NodePhase, OUTAGE_PROBE, PROTOCOL_VERSION, Pause, PauseReason, TaskId, TaskState};
use std::path::PathBuf;
use std::sync::Arc;

const MIN: Millis = 60_000;
const SAMPLE: Millis = 10_000;

fn candidate(session: &str, priority: TaskPriority, turn_since: Millis, waits_on_user: bool) -> Candidate {
    Candidate { session: session.into(), task: session.into(), priority, turn_since, waits_on_user, cuts: 0 }
}

#[test]
fn the_lowest_priority_and_then_the_latest_turn_is_paused_never_one_waiting_on_the_user_nor_the_last() {
    let working = [
        candidate("high", TaskPriority::High, 50, false),
        candidate("normal-old", TaskPriority::Normal, 10, false),
        candidate("normal-new", TaskPriority::Normal, 40, false),
        candidate("low-asks", TaskPriority::Low, 90, true),
    ];
    assert_eq!(choose(&working, false).map(|node| node.session.as_str()), Some("normal-new"), "the Low one waits on the user");
    assert_eq!(choose(&working[..1], false), None, "the last node working is never paused");
    let asking = [candidate("a", TaskPriority::Normal, 0, true), candidate("b", TaskPriority::Normal, 1, true)];
    assert_eq!(choose(&asking, false), None);
}

/// A node brought back after a cut reruns what was cut, so it goes after the nodes cut fewer
/// times; one cut `SPARED_AFTER` times is let finish, unless macOS reports critical pressure.
#[test]
fn the_fewest_cuts_go_first_and_a_node_cut_twice_is_let_finish_unless_pressure_is_critical() {
    let cut = |session, turn_since, cuts| Candidate { cuts, ..candidate(session, TaskPriority::Normal, turn_since, false) };
    let working = [cut("old", 10, 0), cut("rerun", 90, 1)];
    assert_eq!(choose(&working, false).map(|node| node.session.as_str()), Some("old"), "the latest turn is the one rerunning a cut");
    let working = [cut("old", 10, 0), cut("rerun", 90, 1), candidate("low", TaskPriority::Low, 5, false)];
    assert_eq!(choose(&working, false).map(|node| node.session.as_str()), Some("low"), "priority still comes first");
    let working = [Candidate { priority: TaskPriority::High, ..cut("high", 10, 0) }, cut("rerun", 90, SPARED_AFTER)];
    assert_eq!(choose(&working, false), None, "cut twice and the one due: nobody is paused and it finishes");
    assert_eq!(choose(&working, true).map(|node| node.session.as_str()), Some("rerun"), "critical pressure cuts it all the same");
}

#[test]
fn the_step_doubles_up_to_30_minutes_when_a_resume_saturates_the_mac_and_comes_back_after_30_calm_minutes() {
    let mut step = Step::default();
    assert!(!step.saturated(0), "no resume to blame");
    assert_eq!(step.step, 3 * MIN);
    let mut now = MIN;
    for expected in [6, 12, 24, 30, 30] {
        step.resumed(now);
        assert!(step.saturated(now + 9 * MIN));
        assert_eq!(step.step, expected * MIN);
        now += 10 * MIN;
    }
    step.resumed(now);
    assert!(!step.saturated(now + 11 * MIN), "saturation 11 minutes after a resume is not the resume's");
    step.calm(now + 40 * MIN);
    assert_eq!(step.step, 30 * MIN, "only 29 minutes calm");
    step.calm(now + 41 * MIN);
    assert_eq!(step.step, 3 * MIN);
    assert_eq!(step.next_at(), Some(now + 3 * MIN));
}

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
    assert!(text.ends_with("carry on with your task and run again what was cut."), "{text}");
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
        domain.live.gate.tune(Tuning { spacing: crate::fleet_gate::SPACING, limit: Some(limit) });
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

    /// One sample of the Mac and one look of the gate, then what the mods do with what they got.
    fn orchestrator_inbox(&self) -> usize {
        self.domain.store.lock().unwrap().pending_messages().unwrap().iter().filter(|message| message.recipient_kind == "orchestrator").count()
    }

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
}

fn tasks(events: &[(Millis, String)]) -> Vec<&str> {
    events.iter().map(|(_, task)| task.as_str()).collect()
}

/// The case of the 8 nodes left stopped for hours: memory ran out, some builds were cut and their
/// nodes said they wait for the machine, the rest kept working. Alondra pauses one node a minute,
/// the lowest priority first, never the last one; once the Mac has been fine for 5 minutes it
/// brings them back one every 3 minutes, by priority and never past the limit, with nobody
/// stepping in, and the slot a node leaves goes to the next paused one.
#[tokio::test]
async fn eight_nodes_stopped_by_memory_come_back_by_themselves_by_priority_within_the_limit() {
    use TaskPriority::{High, Low, Normal};
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", High), ("t2", High), ("t3", Normal), ("t4", Normal), ("t5", Normal), ("t6", Normal), ("t7", Low), ("t8", Low)], Limit::Fixed(6), base - 60 * MIN);
    fleet.tick(3, base).await;
    fleet.waits_for_machine("t2", base);
    fleet.waits_for_machine("t5", base);
    let now = fleet.hold(3, base + SAMPLE, 6 * MIN).await;
    assert_eq!(tasks(&fleet.events("pause")), ["t8", "t7", "t6", "t4", "t3"], "Low first, then the latest turn; t1 is the last one working and keeps going");
    let pauses = fleet.events("pause");
    assert!(pauses.windows(2).all(|pair| pair[1].0 - pair[0].0 >= PAUSE_EVERY), "one a minute: {pauses:?}");
    assert_eq!(fleet.working(now), 1);

    let recovered = now;
    fleet.max_working = 0;
    let now = fleet.hold(30, recovered, 25 * MIN).await;
    let resumes = fleet.events("resume");
    assert_eq!(tasks(&resumes), ["t2", "t5", "t6", "t4", "t3"], "High first, then Normal by when it stopped; the Low ones wait for a slot");
    assert!(resumes[0].0 >= recovered + 5 * MIN, "nothing comes back before the Mac has been fine for 5 minutes: {resumes:?}");
    assert!(resumes.windows(2).all(|pair| pair[1].0 - pair[0].0 >= STEP), "one every 3 minutes: {resumes:?}");
    assert_eq!(fleet.working(now), 6);

    fleet.end_turn("t1", now);
    let now = fleet.hold(30, now, 5 * MIN).await;
    assert_eq!(tasks(&fleet.events("resume"))[5..], ["t8"], "the slot t1 left goes to the Low node paused first");
    assert_eq!(fleet.working(now), 6);
    assert_eq!(fleet.max_working, 6, "never past the limit");
    let paused: Vec<String> = fleet.domain.live.gate.pauses(&fleet.domain).paused.into_values().map(|paused| paused.task.0).collect();
    assert_eq!(paused, ["t7"]);
    assert_eq!(fleet.events("pause").len(), 5, "no flapping: nothing was paused again");
    assert_eq!(fleet.notices, 0, "the orchestrator has nothing to do about memory");

    let pacing = fleet.domain.live.gate.live(&fleet.domain, now).unwrap();
    assert_eq!(pacing.paused.len(), 1);
    assert_eq!(pacing.paused[0].reason, alondra_core::views::LivePauseReason::Memory);
}

/// Memory swinging between 9 % and 22 % every 3 minutes for an hour, with nodes paused: none
/// comes back, because the Mac is never fine for 5 minutes in a row, and nothing else is paused
/// while it is not at its limit.
#[tokio::test]
async fn memory_swinging_for_an_hour_resumes_no_paused_node() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], Limit::Fixed(6), base - 10 * MIN);
    let mut now = fleet.hold(3, base, 2 * MIN).await;
    assert_eq!(fleet.events("pause").len(), 2);
    let until = now + 60 * MIN;
    while now < until {
        let free = if ((now - base) / (3 * MIN)) % 2 == 0 { 9 } else { 22 };
        fleet.tick(free, now).await;
        now += SAMPLE;
    }
    assert!(fleet.events("resume").is_empty(), "{:?}", fleet.log);
    assert_eq!(fleet.events("pause").len(), 2, "{:?}", fleet.log);
    assert_eq!(fleet.notices, 0, "the orchestrator has nothing to do about memory");
}

/// A resume followed by the Mac short of memory again doubles the step between resumes and lowers
/// the limit below what saturated it; after 30 calm minutes the step is 3 minutes again.
#[tokio::test]
async fn a_resume_that_saturates_the_mac_again_doubles_the_step_and_lowers_the_limit() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal), ("t4", Normal)], Limit::Auto { max: 6 }, base - 10 * MIN);
    let mut now = fleet.hold(3, base, 3 * MIN + SAMPLE).await;
    assert_eq!(fleet.events("pause").len(), 3);
    while fleet.events("resume").is_empty() {
        fleet.tick(30, now).await;
        now += SAMPLE;
    }
    assert_eq!(fleet.working(now), 2);
    let now = fleet.hold(12, now, MIN).await;
    assert_eq!(fleet.domain.live.gate.pauses(&fleet.domain).step.step, 6 * MIN, "the Mac went short of memory within 10 minutes of the resume");
    let snapshot = fleet.domain.live.gate.snapshot(&fleet.domain, now).unwrap();
    assert_eq!(snapshot.limit, 1, "the learned limit dropped below the 2 nodes that saturated it");
    assert_eq!(fleet.events("pause").len(), 3, "short of memory but not at its limit: nobody is paused");
    let now = fleet.hold(30, now, 31 * MIN).await;
    assert_eq!(fleet.domain.live.gate.pauses(&fleet.domain).step.step, STEP, "30 calm minutes set the step back");
    let _ = now;
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
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    fleet.tick(3, base).await;
    assert_eq!(tasks(&fleet.events("pause")), ["t2"]);
    let body = serde_json::from_value(serde_json::json!({"node": "agent-t2", "text": "carry on"})).unwrap();
    let refused = crate::domain_orchestrator_tools::send(&fleet.domain, &fleet.home.0, &Actor::Orchestrator { generation: 1 }, body).await.unwrap_err();
    assert_eq!(refused.code, "node_paused");
    assert!(refused.message.contains("Alondra paused it for the machine and resumes it by itself"), "{}", refused.message);
}

/// Two nodes that declared a wait no gauge measures are the nodes' state, not a card: they wait
/// out the grace with the Mac fine and come back one at a time, a step apart.
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
    assert!(resumes[1].0 - resumes[0].0 >= STEP, "{resumes:?}");
    let cards = fleet.domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(alondra_core::machine_incident::MACHINE_PREFIX)).count();
    assert_eq!(cards, 0);
    assert!(fleet.events("pause").is_empty(), "a node that waits by itself is never interrupted");
}

/// A node that declared its wait is left alone: no interrupt and nothing of its own stopped, even
/// with the Mac at its limit; only memory pauses and the user's interrupt and clean up.
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
        fleet.end_turn(task, base);
        fleet.domain.live.gate.hold_back(&fleet.domain, &format!("agent-{task}"), Reason::Usage, base);
    }
    fleet.hold(50, base, 10 * MIN).await;
    let resumes = fleet.events("resume");
    assert_eq!(resumes.len(), 3);
    assert!(resumes.windows(2).all(|pair| pair[1].0 - pair[0].0 >= STEP), "{resumes:?}");
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

/// A node Alondra paused for memory that starts a turn by itself (Alondra stopped its background
/// build and Claude Code told it so) is stopped again and stays paused until Alondra resumes it.
#[tokio::test]
async fn a_node_paused_for_memory_that_starts_a_turn_by_itself_is_stopped_again_and_stays_paused() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal)], Limit::Fixed(6), base - 10 * MIN);
    fleet.tick(3, base).await;
    assert_eq!(tasks(&fleet.events("pause")), ["t2"]);
    fleet.domain.report(Envelope { session_id: "agent-t2".into(), report: Report::TurnStart }, base + 12_000);
    fleet.tick(50, base + 20_000).await;
    assert_eq!(tasks(&fleet.events("pause")), ["t2", "t2"], "its new turn is cut");
    assert_eq!(fleet.domain.live.gate.paused(&fleet.domain, "agent-t2").map(|paused| paused.reason), Some(Reason::Memory));
    assert_eq!(phase(&fleet, "t2").key(), "paused");
    assert!(fleet.events("resume").is_empty());
}

/// The loop of 13 October: memory runs out with two nodes building, Alondra pauses the newer one
/// and stops its build; the build's end wakes the node, which runs the build again and is paused
/// again a minute later, over and over; once brought back it reruns the build, saturates the Mac
/// and, its turn being the newest, is the one paused again. Here the Mac's memory follows the
/// builds under way (one fits, two exhaust it with macOS warning, three would be critical) and a
/// node wakes 12 s after each cut, as Claude Code wakes a node whose background command died.
/// The node needing 8 uninterrupted minutes finishes, the other gets its turn back afterwards, the
/// Mac never reaches critical pressure and, once a node is paused, it stays out of memory's way.
#[tokio::test]
async fn a_node_whose_build_saturates_the_mac_finishes_it_instead_of_being_cut_again_and_again() {
    use TaskPriority::Normal;
    const BUILD: Millis = 8 * MIN;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("long", Normal), ("rerun", Normal)], Limit::Fixed(6), base - 10 * MIN);
    let mut progress: Millis = 0;
    let mut finished_at = None;
    let mut cut_builds = 0;
    let mut critical = 0;
    let mut exhausted: Millis = 0;
    let mut wakes: Vec<(Millis, &str)> = vec![];
    let mut now = base;
    while now < base + 90 * MIN {
        let building: Vec<&str> = ["long", "rerun"].into_iter().filter(|task| phase(&fleet, task) == NodePhase::Working).collect();
        let pressure = match building.len() {
            0 | 1 => alondra_core::MemoryPressure::Normal,
            2 => alondra_core::MemoryPressure::Warning,
            _ => alondra_core::MemoryPressure::Critical,
        };
        critical += usize::from(pressure == alondra_core::MemoryPressure::Critical);
        exhausted += if building.len() >= 2 { SAMPLE } else { 0 };
        let free = 70u32.saturating_sub(33 * building.len() as u32);
        let seen = fleet.log.len();
        fleet.tick_with(Machine { at: now, memory_free_percent: Some(free), memory_pressure: Some(pressure), cpus: 11, load: [5.0; 3], ..Machine::default() }).await;
        for (_, what, task) in &fleet.log[seen..] {
            if *what == "pause" && building.contains(&task.as_str()) {
                wakes.push((now + 12_000, if task == "long" { "long" } else { "rerun" }));
                if task == "rerun" {
                    cut_builds += 1;
                    progress = 0;
                }
            }
        }
        for (_, task) in wakes.iter().filter(|(at, _)| *at <= now) {
            fleet.domain.report(Envelope { session_id: format!("agent-{task}"), report: Report::TurnStart }, now);
        }
        wakes.retain(|(at, _)| *at > now);
        if finished_at.is_none() && phase(&fleet, "rerun") == NodePhase::Working {
            progress += SAMPLE;
            if progress >= BUILD {
                fleet.end_turn("rerun", now);
                finished_at = Some(now);
            }
        }
        now += SAMPLE;
    }
    let finished_at = finished_at.unwrap_or_else(|| panic!("the build never finished after {cut_builds} cuts: {:?}", fleet.log));
    assert!(cut_builds <= SPARED_AFTER as usize, "its build was cut {cut_builds} times: {:?}", fleet.log);
    assert!(finished_at < base + 30 * MIN, "it finished at minute {}", (finished_at - base) / MIN);
    assert_eq!(phase(&fleet, "long"), NodePhase::Working, "the other node is back at work once the Mac has room: {:?}", fleet.log);
    assert_eq!(critical, 0, "the Mac never reached critical pressure");
    assert!(exhausted <= 3 * MIN, "a pause relieves the Mac until Alondra resumes the node, yet two builds ran together for {} s: {:?}",
        exhausted / 1000, fleet.log);
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
            alondra_core::domain::QuestionOption { id: "a".into(), label: "Atlas".into(), description: None, inputs: vec![] },
            alondra_core::domain::QuestionOption { id: "b".into(), label: "Borealis".into(), description: None, inputs: vec![] },
        ],
        card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Choose the release name").why("The name goes on the release"),
    });
    api_error(&fleet, "t2", "server_error", base + 1_000);
    assert_eq!(phase(&fleet, "t2"), NodePhase::WaitingUser { in_turn: false });
    fleet.hold(50, base + 2_000, 15 * MIN).await;
    assert!(fleet.events("resume").is_empty(), "{:?}", fleet.events("resume"));
    assert!(api_cards(&fleet).is_empty());
}
