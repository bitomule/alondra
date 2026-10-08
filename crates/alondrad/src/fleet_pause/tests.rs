use super::*;
use crate::fleet_gate::{Key, Tuning, Waiting};
use crate::node_push::testing::Node;
use crate::node_reports::{Envelope, Report};
use alondra_core::Machine;
use alondra_core::domain::{Actor, AgentId, Command, CommandEnvelope, PROTOCOL_VERSION};
use std::path::PathBuf;
use std::sync::Arc;

const MIN: Millis = 60_000;
const SAMPLE: Millis = 10_000;

fn candidate(session: &str, priority: TaskPriority, turn_since: Millis, waits_on_user: bool) -> Candidate {
    Candidate { session: session.into(), task: session.into(), priority, turn_since, waits_on_user }
}

#[test]
fn the_lowest_priority_and_then_the_latest_turn_is_paused_never_one_waiting_on_the_user_nor_the_last() {
    let working = [
        candidate("high", TaskPriority::High, 50, false),
        candidate("normal-old", TaskPriority::Normal, 10, false),
        candidate("normal-new", TaskPriority::Normal, 40, false),
        candidate("low-asks", TaskPriority::Low, 90, true),
    ];
    assert_eq!(choose(&working).map(|node| node.session.as_str()), Some("normal-new"), "the Low one waits on the user");
    assert_eq!(choose(&working[..1]), None, "the last node working is never paused");
    let asking = [candidate("a", TaskPriority::Normal, 0, true), candidate("b", TaskPriority::Normal, 1, true)];
    assert_eq!(choose(&asking), None);
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
    assert!(!paused(Reason::ByUser).returns(true, 10 * 60 * MIN));
    assert!(paused(Reason::Memory).returns(true, 0));
    assert!(!paused(Reason::Usage).returns(false, 0));
    let declared = paused(Reason::Declared { reason: "build queue".into() });
    assert!(!declared.returns(true, DECLARED_GRACE - 1));
    assert!(declared.returns(true, DECLARED_GRACE));
    assert!(Paused { released: true, ..declared }.returns(true, 0), "\"I've fixed it\" lets it go at once");
}

#[test]
fn the_resume_message_says_when_and_what_was_cut() {
    let paused = Paused { task: "t".into(), reason: Reason::Memory, since: 0, cut: Some("bazel test //crates/...".into()), turn_since: None, cleanup: Cleanup::Done, released: false };
    let text = resume_text(&paused, 0);
    assert!(text.starts_with("Alondra stopped you at "), "{text}");
    assert!(text.contains("because the Mac ran out of memory, in the middle of `bazel test //crates/...`."), "{text}");
    assert!(text.ends_with("carry on with your task and run again what was cut."), "{text}");
}

struct Home(PathBuf);
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
    fn new(tasks: &[(&str, TaskPriority)], limit: u32, started: Millis) -> Self {
        let home = std::env::temp_dir().join(format!("alondra-pause-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        domain.live.gate.tune(Tuning { spacing: crate::fleet_gate::SPACING, limit: Some(limit) });
        run(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false });
        run(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
        let mut nodes = vec![];
        for (index, (id, priority)) in tasks.iter().enumerate() {
            let agent = format!("agent-{id}");
            run(&domain, user(), Command::CreateTask {
                task_id: (*id).into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
                dependencies: Default::default(), stack: None,
            });
            run(&domain, user(), Command::SetPriority { task_id: (*id).into(), priority: *priority });
            run(&domain, Actor::System, Command::RegisterAgent { agent_id: AgentId(agent.clone()), name: agent.clone() });
            run(&domain, Actor::System, Command::ObserveAgent { agent_id: AgentId(agent.clone()), connected: true });
            run(&domain, user(), Command::AssignTask { task_id: (*id).into(), agent_id: AgentId(agent.clone()), model: None });
            domain.live.mods.apply(Envelope { session_id: agent.clone(), report: Report::TurnStart }, started + index as Millis * MIN);
            nodes.push((agent.clone(), Node::connect(&domain, &agent), 0));
        }
        Self { home: Home(home), domain, nodes, log: vec![], max_working: 0, notices: 0 }
    }

    fn working(&self, now: Millis) -> usize {
        crate::fleet_gate::busy_tasks(&self.domain, &self.domain.state().unwrap(), now).len()
    }

    fn end_turn(&self, task: &str, now: Millis) {
        self.domain.live.mods.apply(Envelope { session_id: format!("agent-{task}"), report: Report::TurnComplete { answer: String::new(), aborted: false } }, now);
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
        let before = self.orchestrator_inbox();
        crate::machine_watch::pace(&self.domain, &Machine { at: now, memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() });
        self.notices += self.orchestrator_inbox() - before;
        crate::fleet_gate::pass_at(&self.domain, &self.home.0, &NoLaunch, now).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        for (session, node, seen) in &mut self.nodes {
            let lines: Vec<serde_json::Value> = node.lines.lock().unwrap().clone();
            for line in &lines[*seen..] {
                let task = session.trim_start_matches("agent-").to_owned();
                if line["interrupt"] == true {
                    self.domain.live.mods.apply(Envelope { session_id: session.clone(), report: Report::TurnComplete { answer: String::new(), aborted: true } }, now);
                    self.log.push((now, "pause", task));
                } else if line["text"].is_string() {
                    self.domain.live.mods.apply(Envelope { session_id: session.clone(), report: Report::TurnStart }, now);
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
    let mut fleet = Fleet::new(&[("t1", High), ("t2", High), ("t3", Normal), ("t4", Normal), ("t5", Normal), ("t6", Normal), ("t7", Low), ("t8", Low)], 6, base - 60 * MIN);
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
    let paused: Vec<String> = fleet.domain.live.gate.pauses().paused.into_values().map(|paused| paused.task.0).collect();
    assert_eq!(paused, ["t7"]);
    assert_eq!(fleet.events("pause").len(), 5, "no flapping: nothing was paused again");
    assert_eq!(fleet.notices, 2, "the orchestrator heard memory run out and come back, once each");

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
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], 6, base - 10 * MIN);
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
    assert_eq!(fleet.notices, 1, "only entering the pressure");
}

/// A resume followed by the Mac short of memory again doubles the step between resumes and lowers
/// the limit below what saturated it; after 30 calm minutes the step is 3 minutes again.
#[tokio::test]
async fn a_resume_that_saturates_the_mac_again_doubles_the_step_and_lowers_the_limit() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal), ("t4", Normal)], 6, base - 10 * MIN);
    let mut now = fleet.hold(3, base, 3 * MIN + SAMPLE).await;
    assert_eq!(fleet.events("pause").len(), 3);
    while fleet.events("resume").is_empty() {
        fleet.tick(30, now).await;
        now += SAMPLE;
    }
    assert_eq!(fleet.working(now), 2);
    let now = fleet.hold(12, now, MIN).await;
    assert_eq!(fleet.domain.live.gate.pauses().step.step, 6 * MIN, "the Mac went short of memory within 10 minutes of the resume");
    let snapshot = fleet.domain.live.gate.snapshot(&fleet.domain, now).unwrap();
    assert_eq!(snapshot.limit, 1, "the learned limit dropped below the 2 nodes that saturated it");
    assert_eq!(fleet.events("pause").len(), 3, "short of memory but not at its limit: nobody is paused");
    let now = fleet.hold(30, now, 31 * MIN).await;
    assert_eq!(fleet.domain.live.gate.pauses().step.step, STEP, "30 calm minutes set the step back");
    let _ = now;
}

/// D3: the user's Resume goes through at once, even over the limit, and holds the next slot; a
/// pause by the user never comes back by itself.
#[tokio::test]
async fn the_users_resume_goes_through_over_the_limit_and_their_pause_never_returns_by_itself() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], 2, base - 10 * MIN);
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
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal)], 6, base - 10 * MIN);
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
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal)], 6, base - 10 * MIN);
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
    let mut fleet = Fleet::new(&[("t1", Normal)], 6, base - 10 * MIN);
    fleet.domain.live.gate.hold_back(&fleet.domain, "agent-t1", Reason::Declared { reason: "cola de compilación".into() }, base);
    fleet.hold(3, base, 2 * MIN).await;
    assert!(fleet.events("pause").is_empty());
    let paused = fleet.domain.live.gate.paused("agent-t1").unwrap();
    assert_eq!(paused.cleanup, Cleanup::Done, "nothing of its own is stopped");
    let pacing = fleet.domain.live.gate.live(&fleet.domain, base + 2 * MIN).unwrap();
    assert_eq!(pacing.paused[0].waits_for.as_deref(), Some("cola de compilación"));
}

/// When the usage limit resets the nodes it stopped come back through the queue, not all at once.
#[tokio::test]
async fn the_usage_limit_reset_brings_nodes_back_one_at_a_time() {
    use TaskPriority::Normal;
    let base = alondra_core::now_millis();
    let mut fleet = Fleet::new(&[("t1", Normal), ("t2", Normal), ("t3", Normal)], 6, base - 10 * MIN);
    for task in ["t1", "t2", "t3"] {
        fleet.end_turn(task, base);
        fleet.domain.live.gate.hold_back(&fleet.domain, &format!("agent-{task}"), Reason::Usage, base);
    }
    fleet.hold(50, base, 10 * MIN).await;
    let resumes = fleet.events("resume");
    assert_eq!(resumes.len(), 3);
    assert!(resumes.windows(2).all(|pair| pair[1].0 - pair[0].0 >= STEP), "{resumes:?}");
}
