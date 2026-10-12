use super::*;
use crate::machine_watch::{Action, step_on};
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
fn the_learned_limit_drops_below_what_hit_the_limit_and_climbs_one_every_5_fine_minutes() {
    let auto = Limit::Auto { max: 6 };
    let mut learned = Learned::default();
    assert_eq!(learned.limit(auto), 6);
    learned.at_limit(6);
    assert_eq!(learned.limit(auto), 5);
    learned.learn(auto, Health::Fine, true, 0);
    learned.learn(auto, Health::Fine, true, 4 * MIN);
    assert_eq!(learned.limit(auto), 5);
    learned.learn(auto, Health::Tight, true, 4 * MIN + SAMPLE);
    learned.learn(auto, Health::Fine, true, 5 * MIN);
    learned.learn(auto, Health::Fine, true, 9 * MIN);
    assert_eq!(learned.limit(auto), 5, "the climb starts again after the Mac was not fine");
    learned.learn(auto, Health::Fine, false, 10 * MIN);
    learned.learn(auto, Health::Fine, true, 11 * MIN);
    learned.learn(auto, Health::Fine, true, 16 * MIN);
    assert_eq!(learned.limit(auto), 6);
    assert_eq!(learned.cap, None, "back at the automatic limit it follows it again");
    learned.at_limit(1);
    assert_eq!(learned.limit(auto), 1, "always at least one");
}

#[test]
fn a_number_the_user_chose_is_the_limit_whatever_the_gate_learned() {
    let mut learned = Learned::default();
    learned.at_limit(3);
    assert_eq!(learned.limit(Limit::Auto { max: 4 }), 2);
    assert_eq!(learned.limit(Limit::Fixed(4)), 4);
    learned.learn(Limit::Fixed(4), Health::Fine, true, 0);
    learned.learn(Limit::Fixed(4), Health::Fine, true, 60 * MIN);
    assert_eq!(learned.cap, Some(2), "a fixed number learns nothing");
}

/// The Mac's memory on 9 October 2026 from 17:35, as `machine-samples/2026-10-09.csv` recorded it:
/// minutes since 17:35, the health from then on, and the nodes the gate counted as working.
const OCTOBER_9: &[(Millis, Health, usize)] = &[
    (0, Health::Tight, 4), (1, Health::AtLimit, 4), (2, Health::Tight, 3),
    (57, Health::Fine, 2), (59, Health::Tight, 3), (61, Health::AtLimit, 3), (62, Health::Tight, 3),
    (66, Health::Fine, 3), (74, Health::Tight, 3), (81, Health::Fine, 3), (82, Health::Tight, 3),
    (91, Health::Fine, 3), (96, Health::Tight, 3), (101, Health::Fine, 3), (105, Health::Tight, 3),
    (122, Health::Fine, 3), (123, Health::Tight, 2), (150, Health::Fine, 2), (158, Health::Tight, 2),
    (165, Health::Fine, 2), (182, Health::Fine, 2),
];

/// The learned limit every minute of `OCTOBER_9`, with tasks queued the whole time.
fn october_9(setting: Limit) -> Vec<(Millis, u32)> {
    let mut learned = Learned::default();
    let mut health = Health::Fine;
    let mut limits = vec![];
    for minute in 0..=OCTOBER_9.last().unwrap().0 {
        if let Some(&(_, next, working)) = OCTOBER_9.iter().find(|(at, ..)| *at == minute) {
            if next == Health::AtLimit && health != Health::AtLimit {
                learned.at_limit(working);
            }
            health = next;
        }
        for sample in 0..6 {
            learned.learn(setting, health, true, minute * MIN + sample * SAMPLE);
        }
        limits.push((minute, learned.limit(setting)));
    }
    limits
}

fn clock(minute: Millis) -> String {
    let at = 17 * 60 + 35 + minute;
    format!("{:02}:{:02}", at / 60, at % 60)
}

/// When the limit changed: every minute it differs from the minute before.
fn changes(limits: &[(Millis, u32)]) -> Vec<(String, u32)> {
    limits.windows(2).filter(|pair| pair[0].1 != pair[1].1).map(|pair| (clock(pair[1].0), pair[1].1)).collect()
}

#[test]
fn in_automatic_the_limit_october_9_lowered_to_3_and_2_climbs_back_by_itself() {
    let limits = october_9(Limit::Auto { max: 4 });
    assert_eq!(changes(&limits), [
        ("17:36".into(), 3), ("18:36".into(), 2), ("18:46".into(), 3), ("20:10".into(), 4),
    ], "it drops at each AtLimit and climbs one step every 5 minutes the Mac stays fine");
}

/// What an hour of memory swinging between `low` and `high` every 3 minutes does, sampled every
/// 10 s: the notices the orchestrator gets, the cards the user gets, and how many starts the gate
/// lets through, once the Mac entered pressure, with a task waiting the whole time.
#[derive(Debug, Default, PartialEq)]
struct Hour {
    /// How many times the Mac went from fine to short of memory.
    pressured: usize,
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
        if let Some((Health::Fine, _)) = mac.step(&machine, now) {
            hour.pressured += 1;
        }
        let (next, actions) = step_on(incident, &machine, None, &[], false, now);
        incident = next;
        for action in actions {
            match action {
                Action::Raise { .. } => hour.cards += 1,
                Action::Withdraw(_) => hour.withdrawn += 1,
                _ => {}
            }
        }
        if hour.pressured > 0 && decide(&head, 0, 6, &mac, last_admission, SPACING, now).is_ok() {
            hour.admitted += 1;
            last_admission = Some(now);
        }
        now += SAMPLE;
    }
    hour
}

#[test]
fn memory_swinging_between_9_and_16_percent_for_an_hour_resumes_nothing_and_enters_the_pressure_once() {
    let hour = swing(9, 16);
    assert_eq!(hour.pressured, 1, "only entering the pressure: {hour:?}");
    assert_eq!((hour.cards, hour.withdrawn, hour.admitted), (0, 0, 0), "memory never puts a card in front of the user: {hour:?}");
}

#[test]
fn memory_swinging_between_9_and_22_percent_never_stays_fine_long_enough_to_count_as_recovered() {
    let hour = swing(9, 22);
    assert_eq!(hour.pressured, 1, "only entering the pressure: {hour:?}");
    assert_eq!((hour.cards, hour.withdrawn, hour.admitted), (0, 0, 0), "memory never puts a card in front of the user: {hour:?}");
}

#[test]
fn memory_back_above_20_percent_for_5_minutes_recovers_once_and_lets_the_queue_go() {
    let mut mac = MacState::default();
    let mut recovered = 0;
    let mut now = 0;
    for free in std::iter::repeat_n(8, 6).chain(std::iter::repeat_n(30, 32)) {
        if let Some((_, Health::Fine)) = mac.step(&Machine { memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() }, now) {
            recovered += 1;
        }
        now += SAMPLE;
    }
    assert_eq!(mac.health, Health::Fine);
    assert_eq!(recovered, 1);
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
    run(domain, Actor::System, Command::RegisterAgent { agent_id: AgentId(agent.clone()), name: agent.clone(), at: 0 });
    run(domain, Actor::System, Command::MoveNode { agent_id: AgentId(agent.clone()), event: alondra_core::domain::NodeEvent::TurnStarted, at: 0 });
    run(domain, Actor::User { user_id: "u".into() }, Command::AssignTask { task_id: id.into(), agent_id: AgentId(agent.clone()), model: None });
    domain.report(Envelope { session_id: agent, report: Report::TurnStart }, now);
}

#[test]
fn eight_starts_with_a_limit_of_6_leave_2_queued_in_order_until_a_node_finishes() {
    let (_home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: SPACING, limit: Some(Limit::Fixed(6)) });
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

    finish(&domain, "t1", now);
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
    domain.live.gate.tune(Tuning { spacing: SPACING, limit: Some(Limit::Auto { max: 6 }) });
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
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(1)) });
    let now = alondra_core::now_millis();
    for id in ["t1", "t2", "t3"] {
        create(&domain, id);
    }
    start(&domain, "t1", now);
    domain.report(Envelope { session_id: "agent-t1".into(), report: Report::ToolStart {
        tool: "ExitPlanMode".into(), tool_use_id: Some("plan-call".into()), input: serde_json::json!({"plan": "- [ ] Fix it"}) } }, now);
    let t2 = Waiting { key: Key::Task("t2".into()), priority: TaskPriority::Normal, resuming: false, since: now };
    assert!(matches!(gate.ask(&domain, t2.clone(), now), Err(Hold::Slots { working: 1, limit: 1 })), "t1 is at work");

    let card: crate::domain_holds::Card = serde_json::from_value(serde_json::json!({
        "session_id": "agent-t1", "id": "plan-call", "kind": "plan", "tool": "ExitPlanMode", "input": {"plan": "- [ ] Fix it"}})).unwrap();
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
fn the_log_says_why_the_queue_waits_and_repeats_it_only_when_its_cause_changes_or_every_10_minutes() {
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
    let tight = |free| Cause::of(&Hold::Memory { health: Health::Tight, free: Some(free) });
    assert!(told.due(tight(12), 0));
    for (second, free) in (10..).step_by(10).zip([11, 9, 13, 8, 12]) {
        assert!(!told.due(tight(free), second * 1_000), "free memory moving from sample to sample is the same cause");
    }
    assert!(!told.due(tight(7), 9 * MIN), "the same cause within 10 minutes");
    assert!(told.due(tight(7), 10 * MIN), "said again after 10 minutes");
    assert!(told.due(Cause::of(&Hold::Memory { health: Health::AtLimit, free: Some(7) }), 10 * MIN + 1), "Tight to AtLimit is news at once");
    assert!(told.due(Cause::of(&Hold::Cpu { load: 52.4, cpus: 11 }), 10 * MIN + 2), "a new cause at once");
    assert!(!told.due(Cause::of(&Hold::Cpu { load: 61.0, cpus: 11 }), 10 * MIN + 3), "a different load is the same cause");
    assert!(!told.due(None, 11 * MIN));
    assert!(told.due(Cause::of(&Hold::Cpu { load: 61.0, cpus: 11 }), 11 * MIN + 1), "after the queue moved, the next wait is told");
}

fn sample(domain: &DomainRuntime, free: u32, from: Millis, to: Millis) {
    let mut now = from;
    while now < to {
        crate::machine_watch::pace(domain, &Machine { at: now, memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() });
        now += SAMPLE;
    }
}

/// The node's turn ends, as its mod and the Stop hook say.
fn finish(domain: &DomainRuntime, id: &str, now: Millis) {
    domain.report(Envelope { session_id: format!("agent-{id}"), report: Report::TurnComplete { answer: "Done".into(), aborted: false } }, now);
    crate::domain_agent_lifecycle::node_event_at(domain, &AgentId(format!("agent-{id}")), alondra_core::domain::NodeEvent::TurnEnded, now).unwrap();
}

fn ask(domain: &DomainRuntime, id: &str, now: Millis) -> Result<(), Hold> {
    domain.live.gate.ask(domain, Waiting { key: Key::Task(id.into()), priority: TaskPriority::Normal, resuming: false, since: 0 }, now)
}

#[test]
fn with_a_fixed_4_an_afternoon_of_memory_pressure_ends_with_4_working_once_the_mac_is_fine() {
    let (_home, domain) = world();
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(4)) });
    for id in ["t1", "t2", "t3", "t4", "t5", "t6"] {
        create(&domain, id);
    }
    let base = alondra_core::now_millis();
    for id in ["t1", "t2", "t3", "t4"] {
        assert_eq!(ask(&domain, id, base), Ok(()));
        start(&domain, id, base);
    }
    sample(&domain, 3, base, base + MIN);
    finish(&domain, "t4", base + MIN);
    sample(&domain, 50, base + MIN, base + 10 * MIN);
    sample(&domain, 3, base + 10 * MIN, base + 11 * MIN);
    finish(&domain, "t3", base + 11 * MIN);
    let now = base + 20 * MIN;
    sample(&domain, 50, base + 11 * MIN, now);
    let snapshot = domain.live.gate.snapshot(&domain, now).unwrap();
    assert_eq!((snapshot.mac.health, snapshot.working, snapshot.limit), (Health::Fine, 2, 4),
        "reaching its limit with 4 and then 3 working never lowers the number the user chose");
    for id in ["t5", "t6"] {
        assert_eq!(ask(&domain, id, now), Ok(()), "{id} takes a free slot of the 4");
        start(&domain, id, now);
    }
    assert_eq!(domain.live.gate.snapshot(&domain, now).unwrap().working, 4);
}

#[test]
fn in_automatic_the_same_afternoon_lowers_the_limit_and_5_fine_minutes_raise_it_again() {
    let (_home, domain) = world();
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Auto { max: 4 }) });
    for id in ["t1", "t2", "t3", "t4"] {
        create(&domain, id);
    }
    let base = alondra_core::now_millis();
    for id in ["t1", "t2", "t3", "t4"] {
        assert_eq!(ask(&domain, id, base), Ok(()));
        start(&domain, id, base);
    }
    sample(&domain, 3, base, base + MIN);
    assert_eq!(domain.live.gate.snapshot(&domain, base + MIN).unwrap().limit, 3);
    domain.live.gate.forget_learned();
    assert_eq!(domain.live.gate.snapshot(&domain, base + MIN).unwrap().limit, 4, "choosing a limit forgets what the gate learned");
}

#[test]
fn a_node_waiting_for_the_machine_leaves_its_slot_and_comes_back_through_the_gate() {
    let (_home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(1)) });
    create(&domain, "t1");
    create(&domain, "t2");
    let now = alondra_core::now_millis();
    assert_eq!(ask(&domain, "t1", now), Ok(()));
    start(&domain, "t1", now);
    assert_eq!(ask(&domain, "t2", now), Err(Hold::Slots { working: 1, limit: 1 }));

    assert!(gate.hold_back(&domain, "agent-t1", Reason::Declared { reason: "The build ran out of memory".into() }, now + MIN));
    let snapshot = gate.snapshot(&domain, now + MIN).unwrap();
    assert_eq!(snapshot.working, 0, "its mod still reports the turn that called wait_for_machine, yet it waits");
    assert_eq!(ask(&domain, "t2", now + MIN), Ok(()), "its slot goes to the queue");
    start(&domain, "t2", now + MIN);

    gate.release_declared(&domain);
    let resume = Waiting { key: Key::Resume("agent-t1".into()), priority: TaskPriority::Normal, resuming: true, since: now + MIN };
    assert_eq!(gate.ask(&domain, resume.clone(), now + 2 * MIN), Err(Hold::Slots { working: 1, limit: 1 }), "it waits for a slot like any start");
    finish(&domain, "t2", now + 5 * MIN);
    assert_eq!(gate.ask(&domain, resume, now + 5 * MIN), Ok(()));
    let snapshot = gate.snapshot(&domain, now + 5 * MIN).unwrap();
    assert_eq!((snapshot.working, snapshot.working_tasks.into_iter().collect::<Vec<_>>()), (1, vec![TaskId::from("t1")]),
        "back from its wait it takes its slot again");
}

/// 9 October: 59 notices to the orchestrator, one per swing of the Mac's memory, and none ever read.
#[test]
fn memory_coming_and_going_for_an_hour_tells_the_orchestrator_nothing() {
    let (_home, domain) = world();
    let base = alondra_core::now_millis();
    create(&domain, "t1");
    start(&domain, "t1", base);
    let mut now = base;
    while now < base + 60 * MIN {
        let free = if ((now - base) / (6 * MIN)) % 2 == 0 { 8 } else { 40 };
        crate::machine_watch::pace(&domain, &Machine { at: now, memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() });
        now += SAMPLE;
    }
    let state = domain.state().unwrap();
    let told: Vec<_> = state.interactions.values().filter(|i| i.recipient == alondra_core::domain::Recipient::Orchestrator).map(|i| i.id.0.clone()).collect();
    assert!(told.is_empty(), "{told:?}");
}

/// 0.2.64 hung on its first start after an upgrade from 0.2.63 with paused nodes: reading the
/// start facts held the domain store while the gate handed the old pauses to the node machine,
/// which needs that store too.
#[test]
fn reading_start_facts_hands_pauses_an_older_daemon_saved_without_holding_the_store() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(FILE), serde_json::json!({
        "learned": {"cap": null, "since": null}, "last_admission": null,
        "pauses": {
            "step": {"step": 180_000, "last_resume": null, "saturated_at": null}, "last_pause": 1, "owed": {}, "held": {},
            "paused": {"agent-t1": {"task": "t1", "reason": {"kind": "memory"}, "since": 1, "cleanup": "done", "released": false}},
        },
    }).to_string()).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    run(&domain, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    create(&domain, "t1");
    run(&domain, Actor::System, Command::RegisterAgent { agent_id: AgentId("agent-t1".into()), name: "agent-t1".into(), at: 0 });
    run(&domain, Actor::User { user_id: "u".into() }, Command::AssignTask { task_id: "t1".into(), agent_id: AgentId("agent-t1".into()), model: None });
    assert!(!domain.live.gate.lock().legacy.is_empty(), "the old pause is still waiting to be handed over");
    let (sent, received) = std::sync::mpsc::channel();
    let reading = Arc::clone(&domain);
    std::thread::spawn(move || {
        let state = reading.state().unwrap();
        sent.send(crate::start_failures::facts(&reading, &state).is_ok()).ok();
    });
    assert_eq!(received.recv_timeout(std::time::Duration::from_secs(5)), Ok(true), "reading the start facts deadlocked");
}
