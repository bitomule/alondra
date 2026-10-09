use super::*;
use crate::machine_watch::{Action, pacing_notice, step_on};
use crate::node_reports::{Envelope, Report};
use alondra_core::Machine;
use alondra_core::domain::{AgentId, Command, CommandEnvelope, PROTOCOL_VERSION};

const MIN: Millis = 60_000;
const SAMPLE: Millis = 10_000;

fn waiting(key: &str, priority: TaskPriority, resuming: bool, since: Millis) -> Waiting {
    Waiting { key: Key::Task(key.into()), priority, resuming, since }
}

fn keys(queue: &[Waiting]) -> Vec<String> {
    queue.iter().map(|waiting| match &waiting.key {
        Key::Task(task) => task.0.clone(),
        Key::Run(run) | Key::Resume(run) => run.clone(),
    }).collect()
}

#[test]
fn the_queue_goes_by_priority_then_resumed_work_then_age() {
    let queue = ordered(vec![
        waiting("low-old", TaskPriority::Low, false, 0),
        waiting("normal-new", TaskPriority::Normal, false, 30),
        waiting("normal-old", TaskPriority::Normal, false, 10),
        waiting("normal-resumed", TaskPriority::Normal, true, 40),
        waiting("high-new", TaskPriority::High, false, 50),
    ]);
    assert_eq!(keys(&queue), ["high-new", "normal-resumed", "normal-old", "normal-new", "low-old"]);
}

fn fine() -> MacState {
    let mut mac = MacState::default();
    mac.step(&Machine { memory_free_percent: Some(50), cpus: 11, load: [5.0; 3], ..Machine::default() }, 0);
    mac
}

#[test]
fn memory_holds_every_start_and_the_cpu_only_new_ones() {
    let new = waiting("new", TaskPriority::Normal, false, 0);
    let resumed = waiting("resumed", TaskPriority::Normal, true, 0);
    assert_eq!(decide(&new, 0, 6, &fine(), None, SPACING, 0), Ok(()));

    let mut tight = fine();
    tight.step(&Machine { memory_free_percent: Some(3), cpus: 11, ..Machine::default() }, SAMPLE);
    assert!(matches!(decide(&resumed, 0, 6, &tight, None, SPACING, SAMPLE), Err(Hold::Memory { health: Health::AtLimit, free: Some(3) })));

    let mut busy = fine();
    (busy.cpu_busy, busy.load) = (true, 52.0);
    assert!(matches!(decide(&new, 0, 6, &busy, None, SPACING, 0), Err(Hold::Cpu { cpus: 11, .. })));
    assert_eq!(decide(&resumed, 0, 6, &busy, None, SPACING, 0), Ok(()), "the CPU only holds new starts");
}

#[test]
fn one_start_a_minute_and_never_past_the_limit() {
    let new = waiting("new", TaskPriority::Normal, false, 0);
    assert_eq!(decide(&new, 6, 6, &fine(), None, SPACING, 0), Err(Hold::Slots { working: 6, limit: 6 }));
    assert_eq!(decide(&new, 2, 6, &fine(), Some(0), SPACING, 59_000), Err(Hold::Spacing { until: SPACING }));
    assert_eq!(decide(&new, 2, 6, &fine(), Some(0), SPACING, SPACING), Ok(()));
}

#[test]
fn the_learned_limit_drops_below_what_hit_the_limit_and_climbs_one_every_15_minutes() {
    let mut learned = Learned::default();
    assert_eq!(learned.limit(6), 6);
    learned.at_limit(6);
    assert_eq!(learned.limit(6), 5);
    learned.learn(6, Health::Fine, true, 0);
    learned.learn(6, Health::Fine, true, 14 * MIN);
    assert_eq!(learned.limit(6), 5);
    learned.learn(6, Health::Tight, true, 14 * MIN + SAMPLE);
    learned.learn(6, Health::Fine, true, 15 * MIN);
    learned.learn(6, Health::Fine, true, 29 * MIN);
    assert_eq!(learned.limit(6), 5, "the climb starts again after the Mac was not fine");
    learned.learn(6, Health::Fine, false, 30 * MIN);
    learned.learn(6, Health::Fine, true, 31 * MIN);
    learned.learn(6, Health::Fine, true, 46 * MIN);
    assert_eq!(learned.limit(6), 6);
    assert_eq!(learned.cap, None, "back at the configured limit it follows it again");
    learned.at_limit(1);
    assert_eq!(learned.limit(6), 1, "always at least one");
}

/// What an hour of memory swinging between `low` and `high` every 3 minutes does, sampled every
/// 10 s: the notices the orchestrator gets, the cards the user gets, and how many starts the gate
/// lets through, once the Mac entered pressure, with a task waiting the whole time.
#[derive(Debug, Default, PartialEq)]
struct Hour {
    notices: Vec<String>,
    cards: usize,
    withdrawn: usize,
    admitted: usize,
}

fn swing(low: u32, high: u32) -> Hour {
    let mut mac = MacState::default();
    let mut incident = None;
    let mut hour = Hour::default();
    let head = waiting("queued", TaskPriority::Normal, false, 0);
    let mut last_admission = None;
    let mut now = 0;
    while now < 60 * MIN {
        let free = if (now / (3 * MIN)) % 2 == 0 { low } else { high };
        let machine = Machine { at: now, memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() };
        if let Some((from, to)) = mac.step(&machine, now) {
            hour.notices.extend(pacing_notice(from, to, mac.free_percent, mac.pressure));
        }
        let (next, actions) = step_on(incident, &machine, mac.health, None, &[], false, now);
        incident = next;
        for action in actions {
            match action {
                Action::TellOrchestrator(text) => hour.notices.push(text),
                Action::Raise { .. } => hour.cards += 1,
                Action::Withdraw(_) => hour.withdrawn += 1,
                _ => {}
            }
        }
        if !hour.notices.is_empty() && decide(&head, 0, 6, &mac, last_admission, SPACING, now).is_ok() {
            hour.admitted += 1;
            last_admission = Some(now);
        }
        now += SAMPLE;
    }
    hour
}

#[test]
fn memory_swinging_between_9_and_16_percent_for_an_hour_resumes_nothing_and_tells_once() {
    let hour = swing(9, 16);
    assert_eq!(hour.notices.len(), 1, "only entering the pressure: {:?}", hour.notices);
    assert!(hour.notices[0].starts_with("The Mac is short of memory (9% free)"), "{:?}", hour.notices);
    assert_eq!((hour.cards, hour.withdrawn, hour.admitted), (1, 0, 0), "{hour:?}");
}

#[test]
fn memory_swinging_between_9_and_22_percent_never_stays_fine_long_enough_to_count_as_recovered() {
    let hour = swing(9, 22);
    assert_eq!(hour.notices.len(), 1, "only entering the pressure: {:?}", hour.notices);
    assert_eq!((hour.cards, hour.withdrawn, hour.admitted), (1, 0, 0), "{hour:?}");
}

#[test]
fn memory_back_above_20_percent_for_5_minutes_recovers_once_and_lets_the_queue_go() {
    let mut mac = MacState::default();
    let mut notices = vec![];
    let mut now = 0;
    for free in std::iter::repeat_n(8, 6).chain(std::iter::repeat_n(30, 32)) {
        if let Some((from, to)) = mac.step(&Machine { memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() }, now) {
            notices.extend(pacing_notice(from, to, mac.free_percent, mac.pressure));
        }
        now += SAMPLE;
    }
    assert_eq!(mac.health, Health::Fine);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[1], "The Mac has memory again (30% free): Alondra resumes the paused nodes and starts the queued launches one by one.");
}

struct Home(crate::test_support::Directory);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn world() -> (Home, Arc<DomainRuntime>) {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    run(&domain, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    run(&domain, Actor::User { user_id: "u".into() }, Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    (Home(home), domain)
}

fn run(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: crate::auth::random_hex(8).into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

fn create(domain: &DomainRuntime, id: &str) {
    run(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
}

/// The admitted task's node starts and takes its first turn.
fn start(domain: &DomainRuntime, id: &str, now: Millis) {
    let agent = format!("agent-{id}");
    run(domain, Actor::System, Command::RegisterAgent { agent_id: AgentId(agent.clone()), name: agent.clone() });
    run(domain, Actor::System, Command::ObserveAgent { agent_id: AgentId(agent.clone()), connected: true });
    run(domain, Actor::User { user_id: "u".into() }, Command::AssignTask { task_id: id.into(), agent_id: AgentId(agent.clone()), model: None });
    domain.live.mods.apply(Envelope { session_id: agent, report: Report::TurnStart }, now);
}

#[test]
fn eight_starts_with_a_limit_of_6_leave_2_queued_in_order_until_a_node_finishes() {
    let (_home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: SPACING, limit: Some(6) });
    let ids = ["t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8"];
    for id in ids {
        create(&domain, id);
    }
    run(&domain, Actor::User { user_id: "u".into() }, Command::SetPriority { task_id: "t7".into(), priority: TaskPriority::High });
    let base = alondra_core::now_millis();
    let mut started = vec![];
    for minute in 0..10 {
        let now = base + minute * MIN;
        for (index, id) in ids.iter().enumerate() {
            if started.contains(id) {
                continue;
            }
            let priority = domain.state().unwrap().tasks[&TaskId::from(*id)].priority;
            if gate.ask(&domain, Waiting { key: Key::Task((*id).into()), priority, resuming: false, since: base + index as Millis }, now).is_ok() {
                start(&domain, id, now);
                started.push(*id);
            }
        }
    }
    assert_eq!(started, ["t1", "t7", "t2", "t3", "t4", "t5"], "the first to ask, then the High one, then by age; one a minute");
    let now = base + 10 * MIN;
    let snapshot = gate.snapshot(&domain, now).unwrap();
    assert_eq!((snapshot.working, snapshot.limit), (6, 6));
    assert_eq!(keys(&snapshot.queue), ["t6", "t8"]);
    assert_eq!(snapshot.hold, Some(Hold::Slots { working: 6, limit: 6 }));
    let pacing = gate.live(&domain, now).unwrap();
    assert_eq!(pacing.queued.iter().map(|queued| (queued.task_id.as_str(), queued.position, queued.of)).collect::<Vec<_>>(),
        [("t6", 1, 2), ("t8", 2, 2)]);

    domain.live.mods.apply(Envelope { session_id: "agent-t1".into(), report: Report::TurnComplete { answer: "Done".into(), aborted: false } }, now);
    let t8 = Waiting { key: Key::Task("t8".into()), priority: TaskPriority::Normal, resuming: false, since: base + 7 };
    assert!(gate.ask(&domain, t8.clone(), now).is_err(), "t6 is older");
    let t6 = Waiting { key: Key::Task("t6".into()), priority: TaskPriority::Normal, resuming: false, since: base + 5 };
    assert_eq!(gate.ask(&domain, t6, now), Ok(()), "the slot t1 left goes to the oldest waiting");
    start(&domain, "t6", now);
    assert!(matches!(gate.ask(&domain, t8, now + MIN), Err(Hold::Slots { working: 6, limit: 6 })));
}

#[test]
fn a_launch_the_orchestrator_queued_is_kept_across_a_restart_and_leaves_when_its_task_is_cancelled() {
    let (home, domain) = world();
    create(&domain, "t1");
    let body: LaunchBody = serde_json::from_value(serde_json::json!({"task": "t1", "name": "app/t1", "cwd": "/tmp"})).unwrap();
    enqueue(&domain, &body, "orchestrator", 5).unwrap();
    let reopened = DomainRuntime::open(&home.0).unwrap();
    let queue = reopened.store.lock().unwrap().launch_queue().unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(LaunchBody::from_queue(&queue[0].body).unwrap().task_id(), &TaskId::from("t1"));
    let snapshot = reopened.live.gate.snapshot(&reopened, 10).unwrap();
    assert_eq!(keys(&snapshot.queue), ["t1"]);
    drop(reopened);

    run(&domain, Actor::User { user_id: "u".into() }, Command::CancelTask { task_id: "t1".into() });
    assert!(domain.live.gate.snapshot(&domain, 10).unwrap().queue.is_empty());
}

#[test]
fn a_review_follows_the_priority_of_the_task_it_reviews() {
    let (_home, domain) = world();
    create(&domain, "work");
    create(&domain, "review");
    run(&domain, Actor::User { user_id: "u".into() }, Command::SetPriority { task_id: "work".into(), priority: TaskPriority::Low });
    run(&domain, Actor::User { user_id: "u".into() }, Command::SetRelations {
        task_id: "review".into(), delivered_dependencies: ["work".into()].into(), blocks: ["work".into()].into() });
    assert_eq!(domain.state().unwrap().tasks[&TaskId::from("review")].priority, TaskPriority::Low);
    run(&domain, Actor::User { user_id: "u".into() }, Command::SetPriority { task_id: "work".into(), priority: TaskPriority::High });
    assert_eq!(domain.state().unwrap().tasks[&TaskId::from("review")].priority, TaskPriority::High);
    let reopened = DomainRuntime::open(domain.home()).unwrap();
    assert_eq!(reopened.state().unwrap().tasks[&TaskId::from("review")].priority, TaskPriority::High, "kept in the store");
}

#[test]
fn a_macos_warning_held_with_31_percent_free_and_6_nodes_working_lowers_the_limit_to_5() {
    let (_home, domain) = world();
    domain.live.gate.tune(Tuning { spacing: SPACING, limit: Some(6) });
    let base = alondra_core::now_millis();
    for id in ["t1", "t2", "t3", "t4", "t5", "t6"] {
        create(&domain, id);
        start(&domain, id, base);
    }
    let mut now = base;
    while now <= base + 2 * MIN {
        let machine = Machine { at: now, memory_free_percent: Some(31), memory_pressure: Some(alondra_core::MemoryPressure::Warning),
            cpus: 11, load: [14.0; 3], ..Machine::default() };
        crate::machine_watch::pace(&domain, &machine);
        now += SAMPLE;
    }
    let snapshot = domain.live.gate.snapshot(&domain, now).unwrap();
    assert_eq!(snapshot.mac.health, Health::AtLimit);
    assert_eq!((snapshot.working, snapshot.limit), (6, 5));
}

#[test]
fn a_node_waiting_for_the_user_to_approve_its_plan_leaves_its_slot_to_the_queue_until_the_user_answers() {
    let (_home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(1) });
    let now = alondra_core::now_millis();
    for id in ["t1", "t2", "t3"] {
        create(&domain, id);
    }
    start(&domain, "t1", now);
    domain.live.mods.apply(Envelope { session_id: "agent-t1".into(), report: Report::ToolStart {
        tool: "ExitPlanMode".into(), tool_use_id: Some("plan-call".into()), input: serde_json::json!({"plan": "Fix it"}) } }, now);
    let t2 = Waiting { key: Key::Task("t2".into()), priority: TaskPriority::Normal, resuming: false, since: now };
    assert!(matches!(gate.ask(&domain, t2.clone(), now), Err(Hold::Slots { working: 1, limit: 1 })), "t1 is at work");

    let card: crate::domain_holds::Card = serde_json::from_value(serde_json::json!({
        "session_id": "agent-t1", "id": "plan-call", "kind": "plan", "tool": "ExitPlanMode", "input": {"plan": "Fix it"}})).unwrap();
    let crate::domain_holds::Posted::Held(plan) = crate::domain_holds::post(&domain, &card, true).unwrap() else { panic!("the plan is held") };
    assert_eq!(gate.snapshot(&domain, now).unwrap().working, 0, "t1 waits for the user to approve its plan");
    assert_eq!(gate.ask(&domain, t2, now), Ok(()), "its slot goes to the queue");
    start(&domain, "t2", now);

    run(&domain, Actor::User { user_id: "u".into() }, Command::AnswerQuestion {
        attachments: vec![], interaction_id: plan, option_id: alondra_core::domain::Hold::APPROVE.into(), inputs: Default::default() });
    let t3 = Waiting { key: Key::Task("t3".into()), priority: TaskPriority::Normal, resuming: false, since: now };
    assert!(matches!(gate.ask(&domain, t3, now), Err(Hold::Slots { working: 2, limit: 1 })), "approved, t1 counts again");
}

#[test]
fn the_log_says_why_the_queue_waits_and_repeats_it_only_when_it_changes_or_every_10_minutes() {
    let working: Vec<TaskId> = vec!["t-a".into(), "t-b".into()];
    let on_user: Vec<TaskId> = vec!["t-c".into()];
    let slots = Hold::Slots { working: 3, limit: 3 };
    assert_eq!(hold_line(4, "t-x", TaskPriority::High, &slots, &working, &on_user, 0).unwrap(),
        "the fleet gate holds 4 waiting (next t-x, High): 3 of 3 working (t-a, t-b, 1 starting); waiting on the user, not counted: t-c");
    assert_eq!(hold_line(1, "t-x", TaskPriority::Low, &Hold::Memory { health: Health::Tight, free: Some(12) }, &[], &[], 2).unwrap(),
        "the fleet gate holds 1 waiting (next t-x, Low): the Mac's memory is Tight (12% free); 2 node(s) paused");
    assert_eq!(hold_line(1, "t-x", TaskPriority::Normal, &Hold::Cpu { load: 52.4, cpus: 11 }, &[], &[], 0).unwrap(),
        "the fleet gate holds 1 waiting (next t-x, Normal): the CPU is overloaded (load 52 on 11 cores), which holds new starts");
    assert_eq!(hold_line(1, "t-x", TaskPriority::Normal, &Hold::Spacing { until: 5 }, &[], &[], 0), None, "the minute between starts is not news");

    let mut told = Told::default();
    let line = || Some("slots".to_owned());
    assert!(told.due(line(), 0));
    assert!(!told.due(line(), 9 * MIN), "the same reason within 10 minutes");
    assert!(told.due(line(), 10 * MIN), "said again after 10 minutes");
    assert!(told.due(Some("memory".into()), 10 * MIN + 1), "a new reason at once");
    assert!(!told.due(None, 11 * MIN));
    assert!(told.due(Some("memory".into()), 11 * MIN + 1), "after the queue moved, the next wait is told");
}
