use super::*;
use crate::machine_watch::step_on;
use crate::node_reports::{Envelope, Report};
use alondra_core::Machine;
use alondra_core::domain::{Actor, AgentId, Command, CommandEnvelope, PROTOCOL_VERSION};

const MIN: Millis = 60_000;
const SAMPLE: Millis = 10_000;
const GIB: i64 = 1 << 30;

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
    assert_eq!(decide(&new, 0, 6, &fine(), 0, None, SPACING, MIN), Ok(()));

    let mut short = fine();
    short.step(&Machine { memory_free_percent: Some(3), cpus: 11, ..Machine::default() }, SAMPLE);
    assert_eq!(decide(&resumed, 0, 6, &short, 0, None, SPACING, MIN), Err(Hold::Memory { why: NoRoom::Free { free: 3 }, free: Some(3) }));

    let mut busy = fine();
    (busy.cpu_busy, busy.load) = (true, 52.0);
    assert!(matches!(decide(&new, 0, 6, &busy, 0, None, SPACING, MIN), Err(Hold::Cpu { cpus: 11, .. })));
    assert_eq!(decide(&resumed, 0, 6, &busy, 0, None, SPACING, MIN), Ok(()), "the CPU only holds new starts");
}

#[test]
fn one_start_a_minute_and_never_past_the_limit() {
    let new = waiting("new", TaskPriority::Normal, false, 0);
    assert_eq!(decide(&new, 6, 6, &fine(), 0, None, SPACING, MIN), Err(Hold::Slots { working: 6, limit: 6 }));
    assert_eq!(decide(&new, 2, 6, &fine(), 0, Some(MIN), SPACING, MIN + 59_000), Err(Hold::Spacing { until: MIN + SPACING }));
    assert_eq!(decide(&new, 2, 6, &fine(), 0, Some(MIN), SPACING, MIN + SPACING), Ok(()));
}

/// An hour of memory swinging between 9 % and 22 % free every 3 minutes, sampled every 10 s, with
/// a task waiting the whole time: no card for the user, and a start only once every sample of the
/// last minute had 20 % free.
#[test]
fn memory_swinging_for_an_hour_puts_no_card_in_front_of_the_user_and_admits_only_after_a_minute_of_room() {
    const SWING: Millis = 3 * MIN;
    let mut mac = MacState::default();
    let mut incident = None;
    let mut actions = vec![];
    let mut admitted = vec![];
    let head = waiting("queued", TaskPriority::Normal, false, 0);
    let mut last_admission = None;
    let mut now = 0;
    while now < 60 * MIN {
        let free = if (now / SWING) % 2 == 0 { 9 } else { 22 };
        let machine = Machine { at: now, memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() };
        mac.step(&machine, now);
        let (next, taken) = step_on(incident, &machine, None, &[], false, now);
        incident = next;
        actions.extend(taken);
        if decide(&head, 0, 6, &mac, 0, last_admission, SPACING, now).is_ok() {
            admitted.push(now);
            last_admission = Some(now);
        }
        now += SAMPLE;
    }
    assert!(actions.is_empty(), "memory never puts a card in front of the user: {actions:?}");
    assert_eq!(admitted.len(), 30, "three starts a minute apart in each of the ten swings up: {admitted:?}");
    assert!(admitted.iter().all(|at| (at / SWING) % 2 == 1 && at % SWING >= ROOM_SINCE_SWING), "{admitted:?}");
}

/// The last sample at 9 % is 10 s before the swing up, so the minute after it ends 50 s in.
const ROOM_SINCE_SWING: Millis = 50_000;

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
    run(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None,
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

/// The learned limit is gone: two minutes of macOS warning with 6 nodes working hold new starts,
/// and the automatic limit stays what the hardware takes.
#[test]
fn a_macos_warning_with_31_percent_free_and_6_nodes_working_holds_new_starts_and_keeps_the_automatic_limit() {
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
    assert_eq!(snapshot.mac.room(0, now), Err(NoRoom::Pressure(alondra_core::MemoryPressure::Warning)));
    assert_eq!((snapshot.working, snapshot.limit), (6, 6));
    assert!(domain.live.gate.live(&domain, now).unwrap().tight, "no room for new starts");
}

/// A rehearsal's daemon (`ALONDRA_IGNORE_MAC_LOAD=1`) shares the Mac with the user's fleet and
/// builds: their memory pressure and load never hold its starts, which the same samples hold for
/// a daemon that reads the Mac.
#[test]
fn a_daemon_that_ignores_the_mac_starts_a_task_under_macos_memory_warning_and_a_busy_cpu() {
    let base = alondra_core::now_millis();
    let ask = |ignores: bool| {
        let (_home, domain) = world();
        if ignores {
            domain.live.gate.ignore_mac();
        }
        create(&domain, "t-fresh");
        let mut now = base;
        while now <= base + 2 * MIN {
            let machine = Machine { at: now, memory_free_percent: Some(31), memory_pressure: Some(alondra_core::MemoryPressure::Warning),
                cpus: 11, load: [60.0; 3], ..Machine::default() };
            crate::machine_watch::pace(&domain, &machine);
            now += SAMPLE;
        }
        domain.live.gate.ask(&domain, waiting("t-fresh", TaskPriority::Normal, false, base), now)
    };
    assert!(matches!(ask(false), Err(Hold::Memory { why: NoRoom::Pressure(alondra_core::MemoryPressure::Warning), .. })), "the live daemon waits for room");
    assert_eq!(ask(true), Ok(()), "the rehearsal's daemon starts it");
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
    let margin = Hold::Memory { why: NoRoom::Margin { margin: GIB / 2, needed: 1_230_000_000, nodes: 3, cost: 410_000_000 }, free: Some(42) };
    assert_eq!(hold_line(1, "t-x", TaskPriority::Low, &margin, &[], &[], 2).unwrap(),
        "the fleet gate holds 1 waiting (next t-x, Low): memory has no room for one more node: 0.5 GiB left before macOS's memory warning, 1.1 GiB needed for 3 node(s) at 0.38 GiB each (42% free); 2 node(s) paused");
    assert_eq!(hold_line(1, "t-x", TaskPriority::Normal, &Hold::Cpu { load: 52.4, cpus: 11 }, &[], &[], 0).unwrap(),
        "the fleet gate holds 1 waiting (next t-x, Normal): the CPU is overloaded (load 52 on 11 cores), which holds new starts");
    assert_eq!(hold_line(1, "t-x", TaskPriority::Normal, &Hold::Spacing { until: 5 }, &[], &[], 0), None, "the minute between starts is not news");

    let mut told = Told::default();
    let tight = |free| Cause::of(&Hold::Memory { why: NoRoom::Free { free }, free: Some(free) });
    assert!(told.due(tight(12), 0));
    for (second, free) in (10..).step_by(10).zip([11, 9, 13, 8, 12]) {
        assert!(!told.due(tight(free), second * 1_000), "free memory moving from sample to sample is the same cause");
    }
    assert!(!told.due(tight(7), 9 * MIN), "the same cause within 10 minutes");
    assert!(told.due(tight(7), 10 * MIN), "said again after 10 minutes");
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
    assert_eq!((snapshot.mac.room(0, now), snapshot.working, snapshot.limit), (Ok(()), 2, 4),
        "memory short with 4 and then 3 working never lowers the number the user chose");
    for id in ["t5", "t6"] {
        assert_eq!(ask(&domain, id, now), Ok(()), "{id} takes a free slot of the 4");
        start(&domain, id, now);
    }
    assert_eq!(domain.live.gate.snapshot(&domain, now).unwrap().working, 4);
}

/// The Mac with `margin_gib` left before macOS's warning, sampled every 10 s from `from` until `to`.
fn margin(domain: &DomainRuntime, margin_gib: f64, from: Millis, to: Millis) {
    const PAGE: u64 = 16_384;
    const WEIGHED: u64 = 900_000;
    let compressor = ((WEIGHED as f64 - margin_gib * ((1 << 30) / PAGE) as f64) / 2.0) as u64;
    let counters = alondra_core::MemoryCompressor { page_size: PAGE, uncompressed_pages: WEIGHED - compressor, compressor_pages: compressor, ..Default::default() };
    let mut now = from;
    while now < to {
        crate::machine_watch::pace(domain, &Machine { at: now, memory_free_percent: Some(40), memory_pressure: Some(alondra_core::MemoryPressure::Normal),
            compressor: Some(counters), cpus: 11, load: [5.0; 3], ..Machine::default() });
        now += SAMPLE;
    }
}

/// Nodes admitted in the last 3 minutes do not show their memory yet, so each reserves what a node
/// measured. With nodes of 0.41 GB, as on 22 October, 2 GiB to macOS's warning takes five starts in
/// a row, where 1 GiB a node took one.
#[test]
fn each_node_admitted_in_the_last_3_minutes_reserves_what_a_node_measured() {
    let (_home, domain) = world();
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(10)) });
    for id in ["t1", "t2", "t3", "t4", "t5", "t6"] {
        create(&domain, id);
    }
    let base = alondra_core::now_millis();
    let cost: i64 = 410_000_000;
    domain.live.machine_watch.measure_nodes(base - MIN, vec![cost as u64; 30]);
    margin(&domain, 2.0, base - MIN, base + SAMPLE);
    for id in ["t1", "t2", "t3", "t4", "t5"] {
        assert_eq!(ask(&domain, id, base), Ok(()), "{id}");
        start(&domain, id, base);
    }
    let held = Err(Hold::Memory { why: NoRoom::Margin { margin: 2 * GIB, needed: 6 * cost, nodes: 6, cost }, free: Some(40) });
    assert_eq!(ask(&domain, "t6", base), held);
    margin(&domain, 2.0, base + SAMPLE, base + 3 * MIN);
    assert_eq!(ask(&domain, "t6", base + 3 * MIN - 1), held);
    assert_eq!(ask(&domain, "t6", base + 3 * MIN), Ok(()), "the five are no longer in flight");
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
    let resume = Waiting { key: Key::Resume("agent-t1".into()), priority: TaskPriority::Normal, resuming: true, since: now + MIN };
    assert_eq!(snapshot.queue.first().map(|head| &head.key), Some(&resume.key),
        "the measures say there is room: it is first to come back, ahead of new starts, without waiting out any minutes");
    assert_eq!(gate.ask(&domain, resume, now + MIN), Ok(()));
    let snapshot = gate.snapshot(&domain, now + MIN).unwrap();
    assert_eq!((snapshot.working, snapshot.working_tasks.into_iter().collect::<Vec<_>>()), (1, vec![TaskId::from("t1")]),
        "back from its wait it takes its slot again");
    assert_eq!(ask(&domain, "t2", now + MIN), Err(Hold::Slots { working: 1, limit: 1 }));
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

/// A waiting task raised to High goes ahead of an older Normal one at once, before it asks the
/// gate again: the queue reads priorities from the domain, not from when each task last asked.
#[test]
fn a_waiting_task_raised_to_high_goes_ahead_before_it_asks_again() {
    let (_home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(1)) });
    for id in ["busy", "older", "raised"] {
        create(&domain, id);
    }
    let base = alondra_core::now_millis();
    assert_eq!(ask(&domain, "busy", base), Ok(()));
    start(&domain, "busy", base);
    let normal = |id: &str, since| Waiting { key: Key::Task(id.into()), priority: TaskPriority::Normal, resuming: false, since };
    assert!(gate.ask(&domain, normal("older", base + 1), base).is_err());
    assert!(gate.ask(&domain, normal("raised", base + 2), base).is_err());
    assert_eq!(keys(&gate.snapshot(&domain, base).unwrap().queue), ["older", "raised"]);

    run(&domain, Actor::Orchestrator { generation: 0 }, Command::SetPriority { task_id: "raised".into(), priority: TaskPriority::High });
    let snapshot = gate.snapshot(&domain, base).unwrap();
    assert_eq!(keys(&snapshot.queue), ["raised", "older"]);
    assert_eq!(snapshot.queue[0].priority, TaskPriority::High);

    let later = base + 10 * MIN;
    finish(&domain, "busy", later);
    assert!(gate.ask(&domain, normal("older", base + 1), later).is_err(), "the High task is first even though it has not asked since");
    let raised = Waiting { priority: TaskPriority::High, ..normal("raised", base + 2) };
    assert_eq!(gate.ask(&domain, raised, later), Ok(()));
    let reopened = DomainRuntime::open(domain.home()).unwrap();
    assert_eq!(reopened.state().unwrap().tasks[&TaskId::from("raised")].priority, TaskPriority::High, "kept in the store");
}

/// Setting the priority a task already has changes nothing, so it is refused with why instead of
/// being acknowledged as if it had been recorded.
#[test]
fn setting_the_priority_a_task_already_has_is_refused() {
    let (_home, domain) = world();
    create(&domain, "work");
    run(&domain, Actor::Orchestrator { generation: 0 }, Command::SetPriority { task_id: "work".into(), priority: TaskPriority::High });
    let state = domain.state().unwrap();
    let again = domain.execute(&Actor::Orchestrator { generation: 0 }, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: crate::auth::random_hex(8).into(),
        expected_revision: state.revision,
        command: Command::SetPriority { task_id: "work".into(), priority: TaskPriority::High },
    }, None, || Ok(()));
    let Err(crate::domain_store::StoreError::Domain(error)) = again else { panic!("a priority that changes nothing is refused") };
    assert_eq!(error.code, "unchanged");
    assert!(error.message.contains("already high"), "{}", error.message);
    assert_eq!(domain.state().unwrap().revision, state.revision);
}

#[test]
fn a_started_node_already_waiting_on_the_user_leaves_its_slot_although_it_was_admitted_a_moment_ago() {
    let (_home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(1)) });
    let now = alondra_core::now_millis();
    for id in ["t1", "t2"] {
        create(&domain, id);
    }
    let t1 = Waiting { key: Key::Task("t1".into()), priority: TaskPriority::Normal, resuming: false, since: now };
    assert_eq!(gate.ask(&domain, t1, now), Ok(()));
    start(&domain, "t1", now);
    run(&domain, Actor::Agent { agent_id: "agent-t1".into() }, Command::AskQuestion {
        attachments: vec![], interaction_id: "question".into(), scope: alondra_core::domain::Scope::Task { task_id: "t1".into() },
        recipient: alondra_core::domain::Recipient::User, body: "Which format?".into(), blocking: true,
        options: ["plain", "markdown"].map(|id| alondra_core::domain::QuestionOption { id: id.into(), label: id.into(), description: None }).to_vec(),
        card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Decide, "Choose the format"),
    });
    assert_eq!(gate.snapshot(&domain, now).unwrap().working, 0, "t1's node waits on the user, a second after its admission");
    let t2 = Waiting { key: Key::Task("t2".into()), priority: TaskPriority::Normal, resuming: false, since: now };
    assert_eq!(gate.ask(&domain, t2, now), Ok(()), "its slot goes to the queue");
}
