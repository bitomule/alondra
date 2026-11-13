use super::*;
use crate::machine_watch::step_on;
use crate::node_reports::{Envelope, Report};
use alondra_core::Machine;
use alondra_core::domain::{Actor, AgentId, Command, CommandEnvelope, RuntimeObservation, PROTOCOL_VERSION};

const MIN: Millis = 60_000;
const SAMPLE: Millis = 10_000;
const GIB: i64 = 1 << 30;

#[test]
fn startup_admission_waits_for_the_first_memory_measurement() {
    for key in [Key::Task("queued".into()), Key::Resume("paused".into())] {
        let head = Waiting { resuming: matches!(&key, Key::Resume(_)), key, priority: TaskPriority::Normal, since: 0 };
        let mut mac = MacState::default();
        assert!(decide(&head, 0, 6, &mac, 0, None, 0, 0).is_err(), "no admission before the first measurement: {:?}", head.key);
        mac.step(&Machine::default(), SAMPLE);
        assert!(decide(&head, 0, 6, &mac, 0, None, 0, SAMPLE).is_err(), "an empty sample is not a memory measurement");
        mac.step(&Machine { memory_total: 16 << 30, memory_available: 8 << 30, memory_free_percent: Some(50), ..Machine::default() }, 2 * SAMPLE);
        assert_eq!(decide(&head, 0, 6, &mac, 0, None, 0, 2 * SAMPLE), Ok(()), "the first healthy measurement is sufficient");
    }
}

#[test]
fn startup_admission_keeps_starts_and_resumes_queued_until_measured() {
    for resuming in [false, true] {
        let (_home, domain) = unmeasured_world();
        let now = alondra_core::now_millis();
        let key = if resuming {
            paused_by_user(&domain, "queued", TaskPriority::Normal, now);
            Key::Resume("agent-queued".into())
        } else {
            create(&domain, "queued");
            Key::Task("queued".into())
        };
        let gate = &domain.live.gate;
        let waiting = Waiting { key, priority: TaskPriority::Normal, resuming, since: now };
        assert!(gate.ask(&domain, waiting.clone(), now).is_err(), "the fresh gate must not admit {waiting:?}");
        assert_eq!(gate.snapshot(&domain, now).unwrap().in_flight, 0);
        crate::machine_watch::pace(&domain, &Machine { at: now + SAMPLE, ..Machine::default() });
        assert!(gate.ask(&domain, waiting.clone(), now + SAMPLE).is_err(), "an empty sample cannot open the gate");
        crate::machine_watch::pace(&domain, &Machine { at: now + 2 * SAMPLE, memory_total: 16 << 30, memory_available: 8 << 30,
            memory_free_percent: Some(50), disk_total: 500 << 30, disk_available: 100 << 30, ..Machine::default() });
        domain.live.machine_watch.measure_disk(crate::disk::Space { total: 500 << 30, free: 100 << 30, purgeable: 0 });
        assert_eq!(gate.ask(&domain, waiting, now + 2 * SAMPLE), Ok(()));
        assert_eq!(gate.snapshot(&domain, now + 2 * SAMPLE).unwrap().in_flight, 1);
    }
}

#[test]
fn startup_admission_explicit_memory_bypass_keeps_disk_and_slot_checks() {
    let (_home, domain) = unmeasured_world();
    create(&domain, "t1");
    create(&domain, "t2");
    let gate = &domain.live.gate;
    gate.ignore_mac();
    domain.live.machine_watch.ignore_peaks();
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(1)) });
    let now = alondra_core::now_millis();
    assert!(matches!(ask(&domain, "t1", now), Err(Hold::Disk { .. })));
    domain.live.machine_watch.measure_disk(crate::disk::Space { total: 500 << 30, free: 100 << 30, purgeable: 0 });
    assert_eq!(ask(&domain, "t1", now), Ok(()));
    assert!(matches!(ask(&domain, "t2", now), Err(Hold::Slots { .. })));
}

#[test]
fn startup_admission_granted_work_keeps_disk_reserves_until_new_bytes() {
    const GB: u64 = 1_000_000_000;
    for resuming in [false, true] {
        let (_home, domain) = world();
        let gate = &domain.live.gate;
        gate.ignore_mac();
        let watch = &domain.live.machine_watch;
        let disk = |free| crate::disk::Space { total: 500 * GB, free, purgeable: 0 };
        watch.measure_disk(disk(36 * GB));
        let base = alondra_core::now_millis();
        for (index, id) in ["t1", "t2", "t3"].into_iter().enumerate() {
            let now = base + index as Millis * SAMPLE;
            if resuming { paused_by_user(&domain, id, TaskPriority::Normal, now); }
            else { create(&domain, id); }
            let key = if resuming { Key::Resume(format!("agent-{id}")) } else { Key::Task(id.into()) };
            let waiting = Waiting { key, priority: TaskPriority::Normal, resuming, since: now };
            assert_eq!(gate.ask(&domain, waiting.clone(), now), Ok(()));
            let reserved = watch.disk_reserved(&BTreeMap::new());
            assert_eq!(gate.ask(&domain, waiting, now), Ok(()));
            assert_eq!(watch.disk_reserved(&BTreeMap::new()), reserved, "reading the same grant again creates no charge");
            if resuming { goes_on(&domain, id, now); }
            else { start(&domain, id, now); }
            finish(&domain, id, now + 1);
            crate::machine_watch::pace(&domain, &Machine { at: now + 2, ..Machine::default() });
        }
        create(&domain, "t4");
        assert!(matches!(ask(&domain, "t4", base + 4 * SAMPLE), Err(Hold::Disk { needed, .. }) if needed == 37 * GB), "completed grants cannot disappear between failed measurements");
        assert_eq!(watch.disk_reserved(&BTreeMap::new()), 24 * GB, "a rejected head creates no additional reservation");
        watch.measure_disk(disk(4 * GB));
        assert!(matches!(ask(&domain, "t4", base + 5 * SAMPLE), Err(Hold::Disk { .. })), "alone, it still needs the disk's line");
        watch.measure_disk(disk(36 * GB));
        assert_eq!(ask(&domain, "t4", base + 6 * SAMPLE), Ok(()));
    }
}

#[test]
fn startup_admission_missing_memory_cannot_cancel_pending_reserves() {
    let (_home, domain) = world();
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(4)) });
    for id in ["t1", "t2"] { create(&domain, id); }
    let base = alondra_core::now_millis();
    let watch = &domain.live.machine_watch;
    watch.measure_nodes(base - MIN, vec![GIB as u64; 30]);
    watch.measure_disk(crate::disk::Space { total: 500 << 30, free: 100 << 30, purgeable: 0 });
    let sample = |at| Machine { at, memory_total: 18 << 30, memory_available: 5 << 30,
        memory_free_percent: Some(60), ..Machine::default() };
    crate::machine_watch::pace(&domain, &sample(base));
    assert_eq!(ask(&domain, "t1", base), Ok(()));
    start(&domain, "t1", base);
    assert!(matches!(ask(&domain, "t2", base), Err(Hold::Memory { why: NoRoom::Available { .. }, .. })));

    let footprints = BTreeMap::from([("agent-t1".into(), GIB as u64)]);
    watch.measure_node_memory(base + SAMPLE, footprints.clone());
    crate::machine_watch::pace(&domain, &Machine { at: base + 2 * SAMPLE, ..Machine::default() });
    assert_eq!(domain.live.gate.snapshot(&domain, base + 2 * SAMPLE).unwrap().mac.observed_in_flight, 0,
        "a missing system budget cannot include a newer node footprint");
    assert!(matches!(ask(&domain, "t2", base + 2 * SAMPLE), Err(Hold::Memory { why: NoRoom::Available { .. }, .. })));

    watch.measure_node_memory(base + 2 * SAMPLE, footprints);
    crate::machine_watch::pace(&domain, &sample(base + 3 * SAMPLE));
    assert_eq!(ask(&domain, "t2", base + 3 * SAMPLE), Ok(()), "a valid new system budget can include the footprint");
}

fn waiting(key: &str, priority: TaskPriority, resuming: bool, since: Millis) -> Waiting {
    Waiting { key: Key::Task(key.into()), priority, resuming, since }
}

fn keys(queue: &[Waiting]) -> Vec<String> {
    queue.iter().map(|waiting| match &waiting.key {
        Key::Task(task) | Key::Turn(task) => task.0.clone(),
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

fn turn(task: &str, priority: TaskPriority, since: Millis) -> Waiting {
    Waiting { key: Key::Turn(task.into()), priority, resuming: true, since }
}

#[test]
fn free_slots_go_to_the_first_turns_of_the_queue_by_priority_then_age() {
    let queue = ordered(vec![
        turn("low-oldest", TaskPriority::Low, 0),
        turn("normal-new", TaskPriority::Normal, 30),
        turn("normal-old", TaskPriority::Normal, 10),
        turn("high-newest", TaskPriority::High, 50),
    ]);
    assert_eq!(turns_to_grant(&queue, 2, false), [TaskId::from("high-newest"), TaskId::from("normal-old")]);
    assert!(turns_to_grant(&queue, 0, false).is_empty());
}

#[test]
fn a_start_ahead_of_a_turn_keeps_its_slot_only_if_nothing_but_the_spacing_holds_it() {
    let queue = ordered(vec![waiting("start", TaskPriority::High, false, 0), turn("turn", TaskPriority::Normal, 10)]);
    assert!(turns_to_grant(&queue, 1, true).is_empty(), "the slot is the start's, a minute away at most");
    assert_eq!(turns_to_grant(&queue, 2, true), [TaskId::from("turn")]);
    assert_eq!(turns_to_grant(&queue, 1, false), [TaskId::from("turn")], "a start the Mac holds cannot use the slot");
    let behind = ordered(vec![waiting("start", TaskPriority::Low, false, 0), turn("turn", TaskPriority::Normal, 10)]);
    assert_eq!(turns_to_grant(&behind, 1, true), [TaskId::from("turn")], "a turn ahead of the start goes first");
}

#[test]
fn the_age_of_a_waiting_turn_survives_a_restart_and_an_older_file_has_none() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(FILE), r#"{"last_admission":7,"pauses":{"paused":{},"owed":{},"held":{},"resumed":{}}}"#).unwrap();
    assert!(Admission::persisted_in(&home).lock().saved.turn_waits.is_empty(), "a file of an older daemon still reads");
    let gate = Admission::persisted_in(&home);
    gate.lock().saved.turn_waits.insert("t".into(), 1_000);
    gate.save(&gate.lock());
    assert_eq!(Admission::persisted_in(&home).lock().saved.turn_waits.get(&TaskId::from("t")), Some(&1_000));
}

fn fine() -> MacState {
    let mut mac = MacState::default();
    mac.step(&Machine { memory_total: 16 << 30, memory_available: 12 << 30, memory_free_percent: Some(50), cpus: 11, load: [5.0; 3], ..Machine::default() }, 0);
    mac
}

#[test]
fn memory_holds_every_start_and_every_resume_and_the_cpu_holds_none() {
    let new = waiting("new", TaskPriority::Normal, false, 0);
    let resumed = waiting("resumed", TaskPriority::Normal, true, 0);
    assert_eq!(decide(&new, 0, 6, &fine(), 0, None, SPACING, MIN), Ok(()));

    let mut short = fine();
    short.step(&Machine { memory_total: 16 << 30, memory_available: 12 << 30, memory_free_percent: Some(3), cpus: 11, ..Machine::default() }, SAMPLE);
    assert!(matches!(decide(&resumed, 0, 6, &short, 0, None, SPACING, MIN), Err(Hold::Memory { why: NoRoom::Free { free: 3, .. }, free: Some(3) })));

    let mut busy = fine();
    busy.step(&Machine { memory_total: 16 << 30, memory_available: 12 << 30, load: [39.0; 3], cpus: 11, memory_free_percent: Some(80), ..Machine::default() }, MIN - SAMPLE);
    busy.step(&Machine { memory_total: 16 << 30, memory_available: 12 << 30, load: [39.0; 3], cpus: 11, memory_free_percent: Some(80), ..Machine::default() }, MIN);
    assert_eq!((busy.load, busy.cpus), (39.0, 11), "the load is read");
    assert_eq!(decide(&new, 0, 6, &busy, 0, None, SPACING, MIN), Ok(()), "a load of 39 on 11 cores holds no start");
    assert_eq!(decide(&resumed, 0, 6, &busy, 0, None, SPACING, MIN), Ok(()), "nor a resume");
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
    mac.node_cost = Some(128 << 20);
    let mut incident = None;
    let mut actions = vec![];
    let mut admitted = vec![];
    let head = waiting("queued", TaskPriority::Normal, false, 0);
    let mut last_admission = None;
    let mut now = 0;
    while now < 60 * MIN {
        let free = if (now / SWING) % 2 == 0 { 9 } else { 22 };
        let machine = Machine { at: now, memory_total: 16 << 30, memory_available: 12 << 30,
            memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() };
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
    let world = unmeasured_world();
    healthy(&world.1);
    world
}

fn healthy(domain: &DomainRuntime) {
    crate::machine_watch::pace(domain, &Machine { at: 1, memory_total: 16 << 30, memory_available: 12 << 30,
        memory_free_percent: Some(75), cpus: 11, ..Machine::default() });
    domain.live.machine_watch.measure_disk(crate::disk::Space { total: 500 << 30, free: 100 << 30, purgeable: 0 });
}

fn unmeasured_world() -> (Home, Arc<DomainRuntime>) {
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
    run(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
}

/// The admitted task's node starts through the Agent SDK and takes its first turn.
fn start(domain: &DomainRuntime, id: &str, now: Millis) {
    let agent = format!("agent-{id}");
    run(domain, Actor::System, Command::RegisterAgent { agent_id: AgentId(agent.clone()), name: agent.clone(), at: 0 });
    crate::domain_store::admit_through_sdk(domain, &id.into(), &agent);
    run(domain, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: format!("turn-{id}-{now}"), input: next_input(domain, id) },
    ] });
    domain.report(Envelope { session_id: agent, report: Report::TurnStart }, now);
}

/// The input the task's node goes on with next.
fn next_input(domain: &DomainRuntime, id: &str) -> String {
    let state = domain.state().unwrap();
    let lifecycle = state.lifecycle_of(&id.into()).unwrap();
    lifecycle.pending_inputs().first().map(|input| input.as_str().to_owned()).expect("an input waits for the node")
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

    run(&domain, Actor::User { user_id: "u".into() }, Command::CancelTask { task_id: "t1".into(), reason: None });
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

/// The work Mac from 12:19 to 14:20 on 28 October: macOS at warning with 36–51 % free and 1–3
/// nodes working, while the gate held the next task every 10 minutes. The warning now only spaces
/// starts 3 minutes apart: the next task starts, and the one after it 3 minutes later.
#[test]
fn macos_warning_with_36_to_51_percent_free_and_up_to_3_nodes_working_admits_a_start_every_3_minutes() {
    let (_home, domain) = world();
    domain.live.gate.tune(Tuning { spacing: SPACING, limit: Some(Limit::Auto { max: 11 }) });
    for id in ["t1", "t2", "t3", "t4", "t5"] {
        create(&domain, id);
    }
    let base = alondra_core::now_millis();
    let warned = |from: Millis, to: Millis| {
        let mut now = from;
        for free in (36..=51).chain((36..=51).rev()).cycle() {
            if now >= to {
                break;
            }
            crate::machine_watch::pace(&domain, &Machine { at: now, memory_free_percent: Some(free), memory_total: 16 * GIB as u64, memory_available: 8 << 30,
                memory_pressure: Some(alondra_core::MemoryPressure::Warning), cpus: 11, load: [6.0; 3], ..Machine::default() });
            now += SAMPLE;
        }
    };
    let mut now = base;
    for (working, id) in ["t1", "t2", "t3"].into_iter().enumerate() {
        warned(now, now + 2 * MIN);
        now += 2 * MIN;
        if working > 0 {
            assert!(matches!(ask(&domain, id, now), Err(Hold::Spacing { .. })), "{id}: under macOS's warning, 3 minutes between starts");
            warned(now, now + MIN);
            now += MIN;
        }
        assert_eq!(domain.live.gate.snapshot(&domain, now).unwrap().working, working);
        assert_eq!(ask(&domain, id, now), Ok(()), "{id} starts with {working} node(s) working");
        start(&domain, id, now);
    }
    warned(now, now + 3 * MIN);
    now += 3 * MIN;
    let snapshot = domain.live.gate.snapshot(&domain, now).unwrap();
    assert_eq!((snapshot.mac.room(0, now), snapshot.working), (Ok(()), 3));
    assert!(!domain.live.gate.live(&domain, now).unwrap().tight, "the Mac is not short of memory");
    assert_eq!(ask(&domain, "t4", now), Ok(()), "a fourth starts with 3 working");
}

/// A rehearsal's daemon (`ALONDRA_IGNORE_MAC_LOAD=1`) shares the Mac with the user's nodes and
/// builds: their memory pressure and load never hold its starts, which the same samples hold for
/// a daemon that reads the Mac.
#[test]
fn a_daemon_that_ignores_the_mac_starts_a_task_under_critical_memory_pressure_and_a_busy_cpu() {
    let base = alondra_core::now_millis();
    let ask = |ignores: bool| {
        let (_home, domain) = world();
        if ignores {
            domain.live.gate.ignore_mac();
        }
        create(&domain, "t-fresh");
        let mut now = base;
        while now <= base + 2 * MIN {
            let machine = Machine { at: now, memory_total: 16 << 30, memory_available: 12 << 30, memory_free_percent: Some(31), memory_pressure: Some(alondra_core::MemoryPressure::Critical),
                cpus: 11, load: [60.0; 3], ..Machine::default() };
            crate::machine_watch::pace(&domain, &machine);
            now += SAMPLE;
        }
        domain.live.gate.ask(&domain, waiting("t-fresh", TaskPriority::Normal, false, base), now)
    };
    assert!(matches!(ask(false), Err(Hold::Memory { why: NoRoom::Pressure(alondra_core::MemoryPressure::Critical), .. })), "the live daemon waits for room");
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
        "the admission gate holds 4 waiting (next t-x, High): 3 of 3 working (t-a, t-b, 1 starting); waiting on the user, not counted: t-c");
    let short = Hold::Memory { why: NoRoom::Free { free: 22, needed: 27, floor: 20, nodes: 3, cost: 410_000_000 }, free: Some(22) };
    assert_eq!(hold_line(1, "t-x", TaskPriority::Low, &short, &[], &[], 2).unwrap(),
        "the admission gate holds 1 waiting (next t-x, Low): memory has no room for one more node: 22% free, 27% needed to keep 20% free with 3 node(s) at 0.38 GiB each (22% free); 2 node(s) paused");
    let swapping = Hold::Memory { why: NoRoom::Swapping { swapped: 300 << 20, free: 31, needed: 33 }, free: Some(31) };
    assert_eq!(hold_line(1, "t-x", TaskPriority::Low, &swapping, &[], &[], 0).unwrap(),
        "the admission gate holds 1 waiting (next t-x, Low): memory has no room for one more node: macOS swapped out 300 MiB in the last minute with 31% free, 33% needed while it swaps (31% free)");
    assert_eq!(hold_line(1, "t-x", TaskPriority::Normal, &Hold::Spacing { until: 5 }, &[], &[], 0), None, "the minute between starts is not news");

    let mut told = Told::default();
    let tight = |free| Cause::of(&Hold::Memory { why: NoRoom::Free { free, needed: 20, floor: 20, nodes: 1, cost: 1 }, free: Some(free) });
    assert!(told.due(tight(12), 0));
    for (second, free) in (10..).step_by(10).zip([11, 9, 13, 8, 12]) {
        assert!(!told.due(tight(free), second * 1_000), "free memory moving from sample to sample is the same cause");
    }
    assert!(!told.due(tight(7), 9 * MIN), "the same cause within 10 minutes");
    assert!(told.due(tight(7), 10 * MIN), "said again after 10 minutes");
    assert!(told.due(Cause::of(&Hold::Slots { working: 3, limit: 3 }), 10 * MIN + 2), "a new cause at once");
    assert!(!told.due(Cause::of(&Hold::Slots { working: 4, limit: 4 }), 10 * MIN + 3), "a different count is the same cause");
    assert!(!told.due(None, 11 * MIN));
    assert!(told.due(Cause::of(&Hold::Slots { working: 4, limit: 4 }), 11 * MIN + 1), "after the queue moved, the next wait is told");
}

fn sample(domain: &DomainRuntime, free: u32, from: Millis, to: Millis) {
    let mut now = from;
    while now < to {
        crate::machine_watch::pace(domain, &Machine { at: now, memory_total: 16 << 30, memory_available: 12 << 30, memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() });
        now += SAMPLE;
    }
}

/// The node's turn ends, as its mod and the Stop hook say.
/// The node's turn ends with nothing for anyone: what it goes on with waits for it.
fn end_turn(domain: &DomainRuntime, id: &str) {
    let turn = domain.state().unwrap().lifecycle_of(&id.into()).unwrap().turn().expect("a turn runs").as_str().to_owned();
    run(domain, Actor::System, Command::RecordTurnBoundary { task_id: id.into(), generation: 1, turn, failure: None, failed: None });
}

/// The node's turn ends on a question for the user: it rests on the card and takes no slot. A node
/// with no turn running already rests.
fn finish(domain: &DomainRuntime, id: &str, now: Millis) {
    let Some(turn) = domain.state().unwrap().lifecycle_of(&id.into()).unwrap().turn().map(|turn| turn.as_str().to_owned()) else { return };
    let options = vec![alondra_core::domain::QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }];
    run(domain, Actor::Agent { agent_id: AgentId(format!("agent-{id}")) }, Command::AskQuestion {
        attachments: vec![], interaction_id: format!("q-{id}-{now}").into(), scope: alondra_core::domain::Scope::Task { task_id: id.into() },
        recipient: alondra_core::domain::Recipient::User, body: "Ship it?".into(), blocking: false,
        card: alondra_core::domain::CardSpec::question("Ship it?", &options), options,
    });
    run(domain, Actor::System, Command::RecordTurnBoundary { task_id: id.into(), generation: 1, turn, failure: None, failed: None });
    domain.report(Envelope { session_id: format!("agent-{id}"), report: Report::TurnComplete { answer: "Done".into(), aborted: false } }, now);
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

/// An 18 GiB Mac with `free` % of its memory free, sampled every 10 s from `from` until `to`.
fn free_of_18_gib(domain: &DomainRuntime, free: u32, from: Millis, to: Millis) {
    let mut now = from;
    while now < to {
        crate::machine_watch::pace(domain, &Machine { at: now, memory_free_percent: Some(free), memory_total: 18 * GIB as u64, memory_available: 18 * GIB as u64 * u64::from(free) / 100,
            memory_pressure: Some(alondra_core::MemoryPressure::Normal), cpus: 11, load: [5.0; 3], ..Machine::default() });
        now += SAMPLE;
    }
}

/// Nodes admitted in the last 3 minutes do not show their memory yet, so each reserves what a node
/// measured. With nodes of 0.41 GB, as on 22 October, 32 % free on 18 GiB takes five starts in a row
/// and keeps 20 % free, where 1 GiB a node took two.
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
    free_of_18_gib(&domain, 32, base - MIN, base + SAMPLE);
    for id in ["t1", "t2", "t3", "t4", "t5"] {
        assert_eq!(ask(&domain, id, base), Ok(()), "{id}");
        start(&domain, id, base);
    }
    let held = Err(Hold::Memory { why: NoRoom::Free { free: 32, needed: 33, floor: 20, nodes: 6, cost }, free: Some(32) });
    assert_eq!(ask(&domain, "t6", base), held);
    free_of_18_gib(&domain, 32, base + SAMPLE, base + 3 * MIN);
    assert_eq!(ask(&domain, "t6", base + 3 * MIN - 1), held);
    assert_eq!(ask(&domain, "t6", base + 3 * MIN), Ok(()), "the five are no longer in flight");
}

#[test]
fn an_admitted_nodes_observed_memory_replaces_its_reserve_once_and_only_while_fresh() {
    let (_home, domain) = world();
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(4)) });
    create(&domain, "t1");
    create(&domain, "t2");
    let base = alondra_core::now_millis();
    domain.live.machine_watch.measure_nodes(base - MIN, vec![GIB as u64; 30]);
    let sample = |at| Machine { at, memory_total: 18 << 30, memory_available: 5 << 30,
        memory_free_percent: Some(60), ..Machine::default() };
    crate::machine_watch::pace(&domain, &sample(base));
    assert_eq!(ask(&domain, "t1", base), Ok(()));
    start(&domain, "t1", base);
    assert!(matches!(ask(&domain, "t2", base), Err(Hold::Memory { why: NoRoom::Available { .. }, .. })));

    domain.live.machine_watch.measure_node_memory(base, BTreeMap::from([("agent-t1".into(), GIB as u64)]));
    crate::machine_watch::pace(&domain, &sample(base + SAMPLE));
    let snapshot = domain.live.gate.snapshot(&domain, base + SAMPLE).unwrap();
    assert_eq!(snapshot.mac.observed_in_flight, GIB as u64);
    assert_eq!(ask(&domain, "t2", base + SAMPLE), Ok(()));

    domain.live.gate.lock().admitted.push((Key::Resume("agent-t1".into()), base));
    let snapshot = domain.live.gate.snapshot(&domain, base + SAMPLE).unwrap();
    assert_eq!(snapshot.mac.observed_in_flight, GIB as u64, "two references never credit the same node twice");
    let snapshot = domain.live.gate.snapshot(&domain, base + MIN).unwrap();
    assert_eq!(snapshot.mac.observed_in_flight, 0, "stale footprints cannot cancel a pending reserve");
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

#[test]
fn a_number_the_user_set_caps_the_automatic_limit_the_memory_gives() {
    let limits = NodeLimits::of(11, 18 << 30, 520 << 20);
    assert_eq!(limits.auto, 22);
    let tuning = Tuning { spacing: 0, limit: None };
    assert_eq!(limit_setting(NodeLimit::Fixed(8), limits, tuning).count(), 8, "the user's 8 holds with room for 22");
    assert_eq!(limit_setting(NodeLimit::Auto, limits, tuning).count(), 22, "unset, the memory decides");
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
        crate::machine_watch::pace(&domain, &Machine { at: now, memory_total: 16 << 30, memory_available: 12 << 30, memory_free_percent: Some(free), cpus: 11, load: [5.0; 3], ..Machine::default() });
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

#[test]
fn a_task_cancelled_after_the_gate_let_it_through_is_not_let_through_again_and_leaves_its_slot() {
    let (_home, domain) = world();
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(1)) });
    create(&domain, "t1");
    create(&domain, "t2");
    let now = alondra_core::now_millis();
    assert_eq!(ask(&domain, "t1", now), Ok(()));
    run(&domain, Actor::User { user_id: "u".into() }, Command::CancelTask { task_id: "t1".into(), reason: None });
    assert!(ask(&domain, "t1", now).is_err(), "a cancelled task is never let through");
    assert_eq!(ask(&domain, "t2", now), Ok(()), "its slot goes to the next");
}

fn home_with_previous_gate_file() -> crate::test_support::Directory {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let mut saved = Saved::default();
    saved.pauses.held.insert("t-held".into(), crate::pauses::Held { body: serde_json::json!({}), origin: "orchestrator".into(), since: 1 });
    std::fs::write(home.join(PREVIOUS_FILE), serde_json::to_vec(&saved).unwrap()).unwrap();
    std::fs::write(home.join("fleet-gate.next"), "{").unwrap();
    home
}

#[test]
fn a_home_with_the_previous_gate_file_starts_with_its_pauses_under_the_new_name() {
    let home = home_with_previous_gate_file();

    crate::home_renames::apply(&home).unwrap();

    assert!(!home.join(PREVIOUS_FILE).exists() && !home.join("fleet-gate.next").exists());
    assert!(home.join(FILE).exists());
    let domain = crate::domain_runtime::DomainRuntime::open(&home).unwrap();
    assert!(held_by_user(&domain, &"t-held".into()), "the task the user paused in the queue stays paused");
}

#[test]
fn the_previous_gate_file_is_read_when_it_could_not_move() {
    let home = home_with_previous_gate_file();

    let gate = Admission::persisted_in(&home);

    assert!(gate.lock().saved.pauses.held.contains_key(&TaskId::from("t-held")));
}

/// What Settings offers is what the gate applies: automatic, from the node cost the Mac measures,
/// and the most this Mac takes, also for a number over it in `options.toml`.
#[test]
fn settings_offer_the_automatic_limit_the_gate_applies_and_the_most_it_takes() {
    let (cores, memory) = (11, 16 << 30);
    let most = NodeLimits::of(cores, memory, 0).most;
    let over_4_gb = memory.saturating_sub(4 << 30);
    for (cost, auto) in [(over_4_gb / 2, 2), (128 << 20, NodeLimits::of(cores, memory, 128 << 20).auto)] {
        let (home, domain) = world();
        domain.live.gate.tune(Tuning { spacing: 0, limit: None });
        let now = alondra_core::now_millis();
        domain.live.machine_watch.measure_nodes(now - MIN, vec![cost; 30]);
        let pacing = domain.live.gate.live(&domain, now).unwrap();
        assert_eq!((pacing.fixed, pacing.auto, pacing.limit, pacing.most), (None, auto, auto, most), "{cost} bytes a node");

        let state = domain.state().unwrap();
        let index = alondra_core::domain::ReadIndex::build(&state);
        let live = alondra_core::views::LiveSnapshot { pacing: Some(pacing), ..Default::default() };
        let context = alondra_core::views::RenderContext { domain: &state, index: &index, live: &live, client: alondra_core::views::Client::Ios,
            lang: alondra_core::views::Lang::En, transport: alondra_core::views::Transport::Loopback, cursor: "c", rendered_at: "" };
        let view = alondra_core::views::build("machine", context).unwrap();
        let menu = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
            alondra_core::views::Item::Row(row) => row.actions.iter().find(|action| action.id.starts_with("set_limit:")).cloned(),
            _ => None,
        }).expect("Settings offer the limit");
        let ids: Vec<&str> = menu.options.iter().map(|option| option.id.as_str()).collect();
        let offered: Vec<String> = std::iter::once("auto".to_string()).chain((1..=most).map(|count| count.to_string())).collect();
        assert_eq!(ids, offered);
        assert_eq!(menu.options[0].label, format!("Automatic ({auto}) · now"));

        std::fs::write(alondra_core::options::path_in(&home.0), format!("max_working_nodes = {}\n", most + 5)).unwrap();
        let pacing = domain.live.gate.live(&domain, now).unwrap();
        assert_eq!((pacing.fixed, pacing.limit), (Some(most + 5), most), "a number over the most works as the most");
    }
}

#[test]
fn a_full_disk_neither_breaks_the_gate_nor_fills_the_log() {
    let (home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    gate.ignore_mac();
    for id in ["t1", "t2", "t3", "t4"] {
        create(&domain, id);
    }
    let now = alondra_core::now_millis();
    let disk = crate::save::full_disk::fill();
    for id in ["t1", "t2", "t3"] {
        assert_eq!(ask(&domain, id, now), Ok(()), "{id}");
    }
    assert_eq!(gate.snapshot(&domain, now).unwrap().in_flight, 3);
    let logged = |what: &str| disk.lines().iter().filter(|line| line.contains(what)).count();
    assert_eq!(logged("could not save"), 1, "{:?}", disk.lines());

    disk.free();
    assert_eq!(ask(&domain, "t4", now), Ok(()));
    let file = home.0.join(FILE);
    assert!(file.exists());
    assert_eq!(logged(&format!("saved {} again", file.display())), 1, "{:?}", disk.lines());
    assert_eq!(logged("could not save"), 1);
}

#[test]
fn a_thread_that_panicked_holding_the_gate_does_not_stop_it() {
    let (_home, domain) = world();
    domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    domain.live.gate.ignore_mac();
    create(&domain, "t1");
    domain.live.gate.poison();
    let now = alondra_core::now_millis();
    assert_eq!(ask(&domain, "t1", now), Ok(()));
    assert_eq!(domain.live.gate.live(&domain, now).unwrap().working, 1);
}

#[test]
fn another_providers_capacity_does_not_admit_a_usage_paused_node() {
    use alondra_core::accounts::{AccountId, AccountKey, AccountState, AccountStatus, Provider};
    use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
    let (_home, domain) = world();
    domain.live.gate.ignore_mac();
    create(&domain, "t1");
    start(&domain, "t1", 1_000);
    let status = |provider| AccountStatus { provider, id: AccountId::default_account(), name: "Personal".into(), state: AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0, profile: None };
    run(&domain, Actor::System, Command::RecordAccounts { accounts: vec![status(Provider::Claude), status(Provider::Codex)], reserve: alondra_core::usage::default_reserve() });
    let record = |provider, windows| crate::usage_probe::record_for_provider(&domain, &AccountKey::new(provider, AccountId::default_account()),
        UsageReading { windows, plan: None, complete: true }, 1_000);
    record(Provider::Claude, vec![WindowReading { kind: WindowKind::Session, used: Share::FULL, resets_at: Some(9_000), exhausted: true }]);
    record(Provider::Codex, vec![
        WindowReading { kind: WindowKind::Other("codex:account:primary:300".into()), used: Share::of(0.2), resets_at: Some(9_000), exhausted: false },
        WindowReading { kind: WindowKind::Other("codex:account:secondary:10080".into()), used: Share::of(0.2), resets_at: Some(9_000), exhausted: false },
    ]);
    domain.live.usage_pause.resume(2_000);
    assert!(domain.live.gate.hold_back(&domain, "agent-t1", Reason::Usage, 2_000));
    assert!(!domain.live.gate.snapshot(&domain, 2_000).unwrap().queue.iter().any(|waiting| waiting.key == Key::Resume("agent-t1".into())),
        "global Codex capacity cannot release this Claude account");
    assert!(domain.live.gate.snapshot(&domain, 9_000).unwrap().queue.iter().any(|waiting| waiting.key == Key::Resume("agent-t1".into())),
        "its own declared reset admits the same node");
}

/// 4 Nov: the gate resumed t-oss-9-export every minute at the head of the queue, the node machine
/// refused it every time, and the ten tasks behind it never started. A resume the node machine
/// refused stays out of the queue until its task's lifecycle moves, and the next one goes ahead.
#[test]
fn a_resume_the_node_machine_refused_lets_the_queue_go_on_until_its_task_moves() {
    let (_home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(1)) });
    gate.ignore_mac();
    let base = alondra_core::now_millis();
    paused_by_user(&domain, "t1", TaskPriority::High, base);
    create(&domain, "t2");
    assert!(gate.resume_now(&domain, "agent-t1", base));
    let queue = |now| gate.snapshot(&domain, now).unwrap().queue.into_iter().map(|waiting| waiting.key).collect::<Vec<_>>();
    assert_eq!(queue(base).first(), Some(&Key::Resume("agent-t1".into())), "the resumed node comes first");

    let revision = lifecycle_revision(&domain.state().unwrap(), "agent-t1").unwrap();
    gate.lock().refused.insert("agent-t1".into(), revision);
    assert!(!queue(base + MIN).contains(&Key::Resume("agent-t1".into())), "a refused resume is not tried again as it is");
    assert_eq!(ask(&domain, "t2", base + MIN), Ok(()), "the next one in the queue goes ahead");
    assert!(still_paused(&domain, "t1"));

    gate.lock().refused.insert("agent-t1".into(), revision - 1);
    assert!(queue(base + 2 * MIN).contains(&Key::Resume("agent-t1".into())), "once its task's lifecycle moved it is tried again");
    gate.lock().refused.insert("agent-t1".into(), revision);
    assert!(gate.resume_now(&domain, "agent-t1", base + 2 * MIN));
    assert!(queue(base + 2 * MIN).contains(&Key::Resume("agent-t1".into())), "and when the user resumes it again");
}

/// Starts the node of task `id` and then the user pauses it at `at`: its turn is cut and it holds
/// no slot.
fn paused_by_user(domain: &DomainRuntime, id: &str, priority: TaskPriority, at: Millis) {
    create(domain, id);
    if !priority.is_normal() {
        run(domain, Actor::User { user_id: "u".into() }, Command::SetPriority { task_id: id.into(), priority });
    }
    start(domain, id, at);
    domain.live.gate.pause_by_user(domain, &format!("agent-{id}"), at).unwrap();
    domain.report(Envelope { session_id: format!("agent-{id}"), report: Report::TurnComplete { answer: String::new(), aborted: true } }, at);
}

/// A node the gate let back takes its turn with what it was told, if anything waits for it.
fn goes_on(domain: &DomainRuntime, id: &str, now: Millis) {
    let waiting = domain.state().unwrap().lifecycle_of(&id.into()).unwrap().pending_inputs().first().map(|input| input.as_str().to_owned());
    if let Some(input) = waiting {
        run(domain, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
            RuntimeObservation::TurnStarted { turn: format!("turn-{id}-{now}"), input },
        ] });
    }
    domain.report(Envelope { session_id: format!("agent-{id}"), report: Report::TurnStart }, now);
}

fn still_paused(domain: &DomainRuntime, id: &str) -> bool {
    domain.live.gate.paused(domain, &format!("agent-{id}")).is_some()
}

/// One look of the gate at `now`, outside an async test.
fn look(domain: &DomainRuntime, home: &Path, now: Millis) {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
        .block_on(pass_at(domain, home, now)).unwrap();
}

fn priority() -> impl proptest::strategy::Strategy<Value = TaskPriority> {
    use proptest::prelude::*;
    prop_oneof![Just(TaskPriority::Low), Just(TaskPriority::Normal), Just(TaskPriority::High)]
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig { cases: 12, ..proptest::prelude::ProptestConfig::default() })]

    /// 27 October: with the limit at 6 the user resumed 19 paused nodes and every one went on at
    /// once. However many nodes the user resumes together, and whenever the working ones end
    /// their turns, never more than the limit work, by the gate's count or by the nodes out of
    /// their pause; they come back by priority, then by the age of their pause, and all of them
    /// come back in the end.
    #[test]
    fn resuming_many_paused_nodes_never_puts_more_than_the_limit_to_work(
        nodes in proptest::collection::vec(priority(), 1..=16),
        limit in 1u32..=6,
        ends in proptest::collection::vec(proptest::prelude::any::<bool>(), 0..64),
    ) {
        let (home, domain) = world();
        let gate = &domain.live.gate;
        gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(limit)) });
        gate.ignore_mac();
        let base = alondra_core::now_millis();
        let ids: Vec<String> = (0..nodes.len()).map(|index| format!("t{index:02}")).collect();
        for (index, (id, priority)) in ids.iter().zip(&nodes).enumerate() {
            paused_by_user(&domain, id, *priority, base + index as Millis);
        }
        let resumed_at = base + MIN;
        for id in &ids {
            let session = format!("agent-{id}");
            proptest::prop_assert!(gate.resume_now(&domain, &session, resumed_at), "{}", session);
        }
        let mut expected: Vec<(TaskPriority, &String)> = nodes.iter().copied().zip(&ids).collect();
        expected.sort();
        let expected: Vec<&String> = expected.into_iter().map(|(_, id)| id).collect();

        let mut back: Vec<&String> = vec![];
        let mut working: Vec<&String> = vec![];
        let mut step = 0;
        while back.len() < ids.len() {
            proptest::prop_assert!(step < 400, "{} of {} came back", back.len(), ids.len());
            let now = resumed_at + step as Millis * SAMPLE;
            domain.live.machine_watch.measure_disk(crate::disk::Space { total: 500 << 30, free: 100 << 30, purgeable: 0 });
            look(&domain, &home.0, now);
            let gone_on: Vec<&String> = ids.iter().filter(|id| !back.contains(id) && !still_paused(&domain, id)).collect();
            for id in gone_on {
                goes_on(&domain, id, now);
                back.push(id);
                working.push(id);
            }
            let counted = gate.snapshot(&domain, now).unwrap().working;
            proptest::prop_assert!(counted <= limit as usize, "the gate counts {} working over a limit of {}", counted, limit);
            proptest::prop_assert!(working.len() <= limit as usize, "{} nodes out of their pause over a limit of {}: {:?}", working.len(), limit, working);
            if ends.get(step).copied().unwrap_or(true) && !working.is_empty() {
                finish(&domain, working.remove(0), now);
            }
            step += 1;
        }
        proptest::prop_assert_eq!(back, expected);
    }
}

#[test]
fn a_resume_waits_while_the_disk_has_no_room_for_one_more_build() {
    const GB: u64 = 1_000_000_000;
    let (home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    gate.ignore_mac();
    let base = alondra_core::now_millis();
    paused_by_user(&domain, "t1", TaskPriority::Normal, base);
    let watch = &domain.live.machine_watch;
    watch.measure_disk(crate::disk::Space { free: 4 * GB, purgeable: 0, total: 500 * GB });
    assert!(gate.resume_now(&domain, "agent-t1", base + MIN));
    look(&domain, &home.0, base + MIN);
    assert!(still_paused(&domain, "t1"), "the disk is under its line");
    let queued = gate.live(&domain, base + MIN).unwrap().queued;
    assert_eq!(queued.iter().map(|queued| (queued.task_id.as_str(), &queued.hold)).collect::<Vec<_>>(),
        [("t1", &alondra_core::views::LiveHold::Disk { free: 4 * GB, needed: 5 * GB, volume: None })], "it waits in the queue, and says why");
    assert!(gate.live(&domain, base + MIN).unwrap().paused.is_empty(), "no longer shown as paused by the user");

    watch.measure_disk(crate::disk::Space { free: 4 * GB, purgeable: 11 * GB, total: 500 * GB });
    look(&domain, &home.0, base + MIN + SAMPLE);
    assert!(!still_paused(&domain, "t1"), "with room on the disk it goes on");
}

/// The daemon restarts into a new version while nodes the user resumed wait for a slot: they
/// still wait, and come back one at a time as slots free up.
#[test]
fn nodes_the_user_resumed_still_wait_for_a_slot_after_the_daemon_restarts() {
    let (home, domain) = world();
    let base = alondra_core::now_millis();
    for (index, id) in ["t1", "t2", "t3"].iter().enumerate() {
        paused_by_user(&domain, id, TaskPriority::Normal, base + index as Millis);
    }
    create(&domain, "t4");
    start(&domain, "t4", base);
    for id in ["t1", "t2", "t3"] {
        assert!(domain.live.gate.resume_now(&domain, &format!("agent-{id}"), base + MIN));
    }
    drop(domain);

    let domain = DomainRuntime::open(&home.0).unwrap();
    healthy(&domain);
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(1)) });
    gate.ignore_mac();
    let now = base + 2 * MIN;
    look(&domain, &home.0, now);
    assert!(["t1", "t2", "t3"].iter().all(|id| still_paused(&domain, id)), "t4 holds the one slot");
    assert_eq!(gate.live(&domain, now).unwrap().queued.iter().map(|queued| queued.task_id.as_str()).collect::<Vec<_>>(), ["t1", "t2", "t3"]);

    finish(&domain, "t4", now);
    look(&domain, &home.0, now + SAMPLE);
    look(&domain, &home.0, now + 2 * SAMPLE);
    assert_eq!(["t1", "t2", "t3"].map(|id| still_paused(&domain, id)), [false, true, true], "the oldest pause first, and one only");
    goes_on(&domain, "t1", now + 2 * SAMPLE);
    finish(&domain, "t1", now + 3 * SAMPLE);
    look(&domain, &home.0, now + 4 * SAMPLE);
    assert_eq!(["t1", "t2", "t3"].map(|id| still_paused(&domain, id)), [false, false, true]);
}

const GB: u64 = 1_000_000_000;

fn create_in(domain: &DomainRuntime, id: &str, project: &str) {
    let mut launch = alondra_core::domain::LaunchPlan::new("node", "/tmp");
    launch.project = Some(project.into());
    run(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(launch),
    });
}

/// A sample of the Mac at `now` whose own disk has `usable` bytes free, none of them purgeable.
fn disk_sample(domain: &DomainRuntime, usable: u64, now: Millis) {
    crate::machine_watch::pace(domain, &Machine { at: now, memory_total: 16 << 30, memory_available: 12 << 30, memory_free_percent: Some(50), cpus: 11, load: [5.0; 3],
        disk_total: 500 * GB, disk_available: usable, ..Machine::default() });
    domain.live.machine_watch.measure_disk(crate::disk::Space { free: usable, purgeable: 0, total: 500 * GB });
}

fn machine_cards(domain: &DomainRuntime) -> usize {
    domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(alondra_core::machine_incident::MACHINE_PREFIX)).count()
}

fn swap_sample(domain: &DomainRuntime, free: u64, used: u64, now: Millis) {
    swap_sample_with_total(domain, free, used, 30 * GB, now);
}

fn swap_sample_with_total(domain: &DomainRuntime, free: u64, used: u64, total: u64, now: Millis) {
    let compressor = alondra_core::MemoryCompressor { swap_used: used, swap_total: Some(total),
        ..alondra_core::MemoryCompressor::default() };
    crate::machine_watch::pace(domain, &Machine { at: now, memory_total: 64u64 << 30,
        memory_available: 32 << 30,
        memory_free_percent: Some(29), cpus: 11, load: [5.0; 3], compressor: Some(compressor),
        disk_total: 500 * GB, disk_available: free, ..Machine::default() });
    domain.live.machine_watch.measure_disk(crate::disk::Space { free, purgeable: 0, total: 500 * GB });
}

#[test]
fn growing_swap_holds_a_resume_in_the_queue_and_a_quiet_minute_releases_it() {
    let (home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    domain.live.machine_watch.seed_peaks("app", &[GB; 3]);
    create_in(&domain, "t1", "app");
    start(&domain, "t1", 1_000);
    gate.pause_by_user(&domain, "agent-t1", 1_000).unwrap();
    domain.report(Envelope { session_id: "agent-t1".into(), report: Report::TurnComplete { answer: String::new(), aborted: true } }, 1_000);
    for step in 0..=30 {
        swap_sample(&domain, 7_300_000_000, 20 * GB + step * 10_000_000, 10_000 + step as Millis * SAMPLE);
    }
    let now = 10_000 + 5 * MIN;
    assert!(gate.resume_now(&domain, "agent-t1", now));
    let logged = crate::save::full_disk::fill();
    logged.free();
    look(&domain, &home.0, now);
    assert!(still_paused(&domain, "t1"));
    assert!(matches!(gate.live(&domain, now).unwrap().queued[0].hold, alondra_core::views::LiveHold::Disk { .. }));
    let lines = logged.lines();
    let held = lines.iter().find(|line| line.contains("the admission gate holds")).expect("a disk hold is logged");
    assert!(held.contains("swap 20.30/30.00 GB, growth +0.300 GB over 300 s"), "{held}");
    assert!(held.contains("container free 7.30 GB, builds reserved 1.00 GB, free less builds 6.30 GB"), "{held}");
    look(&domain, &home.0, now + SAMPLE);
    assert_eq!(logged.lines().iter().filter(|line| line.contains("the admission gate holds")).count(), 1,
        "the new numbers do not repeat the hold every sample");
    for step in 1..=6 {
        swap_sample(&domain, 7_300_000_000, 20_300_000_000, now + step * SAMPLE);
    }
    look(&domain, &home.0, now + MIN);
    assert!(!still_paused(&domain, "t1"), "swap's old growth leaves no disk hysteresis behind");
    assert_eq!(machine_cards(&domain), 0);
}

#[test]
fn swap_does_not_spend_the_build_reserve_and_its_log_names_the_whole_budget() {
    let (_home, domain) = world();
    let watch = &domain.live.machine_watch;
    watch.seed_peaks("build", &[4 * GB; 3]);
    watch.seed_peaks("next", &[GB; 3]);
    create_in(&domain, "build", "build");
    swap_sample_with_total(&domain, 12 * GB, 20 * GB, 21 * GB, 10_000);
    start(&domain, "build", 10_000);
    swap_sample_with_total(&domain, 12 * GB, 20 * GB, 21 * GB, 20_000);
    for step in 1..=30 {
        swap_sample_with_total(&domain, 12 * GB - step * 100_000_000, 20 * GB + step * 100_000_000,
            21 * GB + step * 100_000_000, 20_000 + step as Millis * SAMPLE);
    }
    let working = working_projects(&domain, &domain.state().unwrap());
    assert_eq!(watch.disk_reserved(&working), 4 * GB, "the 3 GB lost to swap did not pay for a build");
    assert_eq!(watch.disk_used(&TaskId::from("build")), 0);
    let line = watch.disk_detail(&working, Some("next"), 20_000 + 5 * MIN).unwrap();
    assert_eq!(line, "swap 23.00/24.00 GB, growth +3.000 GB over 300 s, swap reserve 24.00 GB; container free 9.00 GB, builds reserved 5.00 GB, free less builds 4.00 GB");
    swap_sample_with_total(&domain, 9 * GB, 22 * GB, 24 * GB, 30_000 + 5 * MIN);
    assert_eq!(watch.disk_reserved(&working), 4 * GB, "freeing used swap slots without shrinking the files consumed no build space");
    swap_sample_with_total(&domain, 10 * GB, 22 * GB, 23 * GB, 40_000 + 5 * MIN);
    assert_eq!(watch.disk_reserved(&working), 4 * GB, "reclaiming a swapfile freed no build space");
}

#[test]
fn stable_and_shrinking_swap_leave_the_disk_gate_open_on_the_same_small_disk() {
    for change in [-10_000_000i64, 0, 10_000_000] {
        let (_home, domain) = world();
        domain.live.gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
        domain.live.machine_watch.seed_peaks("app", &[GB; 3]);
        create_in(&domain, "next", "app");
        for step in 0..=30 {
            let used = (20 * GB) as i64 + step * change;
            swap_sample(&domain, 7_300_000_000, used as u64, 10_000 + step * SAMPLE);
        }
        let result = ask(&domain, "next", 10_000 + 5 * MIN);
        if change > 0 {
            assert!(matches!(result, Err(Hold::Disk { .. })), "{result:?}");
        } else {
            assert_eq!(result, Ok(()), "change {change}");
        }
        assert_eq!(machine_cards(&domain), 0);
    }
}

// 30 October: builds took 2 GB, swap another 2.7 GB, with memory still 29% free.
#[test]
fn growing_swap_holds_starts_even_when_the_disk_budget_has_room() {
    for (extra_gb, roomy) in [(0, false), (0, true), (1, false), (1, true)] {
        let (_home, domain) = world();
        let gate = &domain.live.gate;
        gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
        let watch = &domain.live.machine_watch;
        watch.seed_peaks("build", &[2 * GB; 3]);
        watch.seed_peaks("next", &[GB; 3]);
        create_in(&domain, "build", "build");
        create_in(&domain, "next", "next");
        let extra = if roomy { (10 + extra_gb) * GB } else { extra_gb * GB };
        for step in 0..=240u64 {
            let now = 10_000 + step as Millis * SAMPLE;
            let free = 12 * GB + extra - 4_700_000_000 * step / 240;
            let used = 20_200_000_000 + 2_700_000_000 * step / 240;
            let counters = serde_json::from_value(serde_json::json!({
                "page_size": 16384, "uncompressed_pages": 3_000_000, "compressor_pages": 100_000,
                "stored_pages": 200_000, "swap_used": used, "swap_total": used + 600_000_000,
                "swapins": 0, "swapouts": 0,
            })).unwrap();
            crate::machine_watch::pace(&domain, &Machine { at: now, memory_total: 64u64 << 30,
                memory_available: 32 << 30,
                memory_free_percent: Some(29), cpus: 11, load: [5.0; 3], compressor: Some(counters),
                disk_total: 500 * GB, disk_available: free, ..Machine::default() });
            watch.measure_disk(crate::disk::Space { free, purgeable: 0, total: 500 * GB });
            if step == 0 {
                start(&domain, "build", now);
            }
        }
        let result = ask(&domain, "next", 10_000 + 40 * MIN);
        if roomy {
            assert!(matches!(result, Err(Hold::Memory { why: NoRoom::SwapGrowth { .. }, .. })), "{result:?}");
        } else {
            assert!(matches!(result, Err(Hold::Disk { .. })), "{:?}", result);
        }
        assert_eq!(machine_cards(&domain), 0);
    }
}

/// 27 October: six nodes building at once took the disk from 34 GB to nothing, each start having
/// found room for one build. A start now waits, with no card, while the nodes at work may still
/// take the room it needs, and goes once one of them ends and frees what it reserved.
#[test]
fn a_start_waits_while_the_nodes_at_work_may_still_take_the_disk_and_goes_once_one_ends() {
    let (_home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    gate.ignore_mac();
    domain.live.machine_watch.seed_peaks("app", &[6 * GB; 3]);
    let base = alondra_core::now_millis();
    disk_sample(&domain, 20 * GB, base);
    for id in ["t1", "t2", "t3"] {
        create_in(&domain, id, "app");
    }
    for (step, id) in ["t1", "t2"].into_iter().enumerate() {
        let now = base + (step as Millis + 1) * SAMPLE;
        assert_eq!(ask(&domain, id, now), Ok(()), "{id} fits");
        start(&domain, id, now);
        disk_sample(&domain, 20 * GB, now);
    }
    let now = base + 3 * SAMPLE;
    assert_eq!(ask(&domain, "t3", now), Err(Hold::Disk { free: 20 * GB, needed: 6 * GB + 6 * GB + 6 * GB + 5 * GB, volume: None }),
        "t1 and t2 may still take 6 GB each");
    disk_sample(&domain, 17 * GB, now + SAMPLE);
    assert_eq!(ask(&domain, "t3", now + SAMPLE), Err(Hold::Disk { free: 17 * GB, needed: 4_500_000_000 * 2 + 6 * GB + 5 * GB, volume: None }),
        "what they took is no longer free, nor reserved");
    finish(&domain, "t1", now + 2 * SAMPLE);
    disk_sample(&domain, 20 * GB, now + 2 * SAMPLE);
    assert_eq!(ask(&domain, "t3", now + 2 * SAMPLE), Ok(()), "t1 ended and its build was deleted: what it reserved is free again");
    assert_eq!(machine_cards(&domain), 0, "waiting for the disk asks the user nothing");
}

/// A node the disk paused goes back before a new task of any priority; one the memory paused keeps
/// its place by priority.
#[test]
fn a_node_the_disk_paused_goes_back_before_a_new_high_task() {
    let paused = |reason| Paused { task: "t".into(), reason, since: 0, cut: None, turn_since: None, cleanup: crate::pauses::Cleanup::Done, released: false };
    let mut pauses = Pauses::default();
    pauses.paused.insert("disk".into(), paused(Reason::Disk));
    pauses.paused.insert("memory".into(), paused(Reason::Memory));
    let resume = |session: &str, since| Waiting { key: Key::Resume(session.into()), priority: TaskPriority::Normal, resuming: true, since };
    let queue = ordered(vec![resume("disk", 2), resume("memory", 1), waiting("new", TaskPriority::High, false, 0)]);
    assert_eq!(keys(&disk_returns_first(queue, &pauses)), ["disk", "new", "memory"]);
}

/// The disk's floor held a node's next turn and the daemon restarted: the node still waits, shown
/// in the queue for the disk, and goes on by itself once the disk has room for its build.
#[test]
fn a_node_the_disks_floor_held_still_waits_after_a_restart_and_goes_on_once_there_is_room() {
    let (home, domain) = world();
    let base = alondra_core::now_millis();
    create_in(&domain, "t1", "app");
    start(&domain, "t1", base);
    assert!(domain.live.gate.hold_back(&domain, "agent-t1", Reason::Disk, base));
    finish(&domain, "t1", base + SAMPLE);
    drop(domain);

    let domain = DomainRuntime::open(&home.0).unwrap();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    gate.ignore_mac();
    let now = base + MIN;
    disk_sample(&domain, 4 * GB, now);
    look(&domain, &home.0, now);
    assert!(still_paused(&domain, "t1"), "4 GB are under the 5 GB line");
    let pacing = gate.live(&domain, now).unwrap();
    assert_eq!(pacing.queued.iter().map(|queued| (queued.task_id.as_str(), &queued.hold)).collect::<Vec<_>>(),
        [("t1", &alondra_core::views::LiveHold::Disk { free: 4 * GB, needed: 5 * GB, volume: None })], "it waits in the queue for the disk");
    assert!(pacing.paused.is_empty(), "not shown as paused");

    disk_sample(&domain, 16 * GB, now + SAMPLE);
    look(&domain, &home.0, now + SAMPLE);
    assert!(!still_paused(&domain, "t1"), "with room for its build it goes on");
    assert_eq!(machine_cards(&domain), 0);
}

/// Under the disk's floor the next turn of every node at work but the one that took least waits;
/// one already held stays as it is.
#[test]
fn under_the_floor_every_node_but_the_lightest_is_held() {
    use crate::disk_peaks::{AtWork, to_hold};
    let at_work = |task: &str, used: u64, can_wait: bool, held: bool| AtWork { task: task.into(), used, can_wait, held };
    assert_eq!(to_hold(&[at_work("t1", 9 * GB, true, false), at_work("t2", GB, true, false), at_work("t3", 4 * GB, true, false)]),
        [TaskId::from("t1"), TaskId::from("t3")]);
    assert_eq!(to_hold(&[at_work("t1", 9 * GB, true, false), at_work("t2", GB, false, false), at_work("t3", 4 * GB, false, false)]),
        [TaskId::from("t1")], "a node of claude --bg cannot wait");
    assert_eq!(to_hold(&[at_work("t1", 9 * GB, true, true), at_work("t2", GB, true, false)]), Vec::<TaskId>::new(),
        "the lightest still going is never held");
    assert_eq!(to_hold(&[at_work("t1", GB, true, false)]), Vec::<TaskId>::new(), "one node alone goes on");
}

fn build() -> impl proptest::strategy::Strategy<Value = (usize, u64, u64)> {
    use proptest::prelude::*;
    (0usize..3, 4u64..=8, 1u64..=5)
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig { cases: 24, ..proptest::prelude::ProptestConfig::default() })]

    /// Four to nine nodes of up to three projects ask to start together on a disk with 37 to 45 GB
    /// usable, room for any estimate up to the 30 GB a stretch is kept at, so every node can end; each
    /// one's build takes its true peak over a few samples, stays up to 3 more, and is deleted when
    /// its turn ends. While each project's estimate covers its true peak, however the builds overlap
    /// the disk never goes under its 5 GB line, no card is raised, and every node ends.
    #[test]
    fn nodes_whose_estimates_cover_their_builds_never_take_the_disk_under_its_line(
        builds in proptest::collection::vec(build(), 4..=9),
        extra in proptest::collection::vec(0u64..=4, 3),
        usable in 37u64..=45,
        stays in proptest::collection::vec(proptest::prelude::any::<bool>(), 1..64),
    ) {
        let (_home, domain) = world();
        let gate = &domain.live.gate;
        gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(12)) });
        gate.ignore_mac();
        let projects = ["app", "web", "docs"];
        for (index, project) in projects.iter().enumerate() {
            let truest = builds.iter().filter(|(of, _, _)| *of == index).map(|(_, peak, _)| *peak).max().unwrap_or(1);
            domain.live.machine_watch.seed_peaks(project, &[(truest + extra[index]) * GB; 3]);
        }
        let ids: Vec<String> = (0..builds.len()).map(|index| format!("t{index}")).collect();
        for (id, (project, _, _)) in ids.iter().zip(&builds) {
            create_in(&domain, id, projects[*project]);
        }
        let base = alondra_core::now_millis();
        let mut free = (usable * GB) as i64;
        disk_sample(&domain, free as u64, base);
        let mut took = vec![0u64; ids.len()];
        let mut at_peak = vec![0usize; ids.len()];
        let mut started = vec![false; ids.len()];
        let mut ended = vec![false; ids.len()];
        let mut step = 0usize;
        while ended.iter().any(|ended| !ended) {
            proptest::prop_assert!(step < 600, "{} of {} nodes ended", ended.iter().filter(|ended| **ended).count(), ids.len());
            let now = base + (step as Millis + 1) * SAMPLE;
            for (index, id) in ids.iter().enumerate() {
                if !started[index] && gate.ask(&domain, Waiting { key: Key::Task(id.as_str().into()), priority: TaskPriority::Normal, resuming: false, since: index as Millis }, now).is_ok() {
                    start(&domain, id, now);
                    started[index] = true;
                }
            }
            for (index, (_, peak, samples)) in builds.iter().enumerate() {
                if !started[index] || ended[index] {
                    continue;
                }
                let peak = peak * GB;
                if took[index] < peak {
                    let more = (peak / samples).max(1).min(peak - took[index]);
                    took[index] += more;
                    free -= more as i64;
                } else if !stays[step % stays.len()] && at_peak[index] < 3 {
                    at_peak[index] += 1;
                } else {
                    finish(&domain, &ids[index], now);
                    free += took[index] as i64;
                    ended[index] = true;
                }
            }
            proptest::prop_assert!(free >= crate::disk::LOW_DISK_BYTES as i64, "step {}: {} bytes left with {:?} taken", step, free, took);
            disk_sample(&domain, free as u64, now);
            step += 1;
        }
        proptest::prop_assert_eq!(machine_cards(&domain), 0);
    }
}

/// 6 November: 44 waited and 18 nodes stayed paused with 14.5 GB free while the disk's "needed"
/// climbed with every node (10, 15, 21 GB). The nodes paused or waiting reserve nothing: with the
/// limit at 8 and builds of 1 GB, the paused come back first, 8 work at a time, the disk never
/// holds them, and every one of the 44 gets to work.
#[test]
fn eighteen_paused_and_forty_four_waiting_with_14_5_gb_free_all_get_to_work() {
    const FREE: u64 = 14_500_000_000;
    let (home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(8)) });
    gate.ignore_mac();
    let watch = &domain.live.machine_watch;
    watch.seed_peaks("app", &[GB; 3]);
    let base = alondra_core::now_millis();
    disk_sample(&domain, FREE, base);
    let paused: Vec<String> = (0..18).map(|index| format!("p{index}")).collect();
    for id in &paused {
        create_in(&domain, id, "app");
        start(&domain, id, base);
        assert!(gate.hold_back(&domain, &format!("agent-{id}"), Reason::Disk, base));
        end_turn(&domain, id);
    }
    let fresh: Vec<String> = (0..26).map(|index| format!("q{index}")).collect();
    for id in &fresh {
        create_in(&domain, id, "app");
    }
    let mut started: BTreeMap<String, usize> = BTreeMap::new();
    let mut ended: BTreeSet<String> = BTreeSet::new();
    let mut order = vec![];
    for step in 1..=400usize {
        let now = base + step as Millis * SAMPLE;
        disk_sample(&domain, FREE, now);
        let state = domain.state().unwrap();
        let working = working_projects(&domain, &state);
        assert!(working.len() <= 8, "step {step}: {} working", working.len());
        assert!(watch.disk_reserved(&working) <= working.len() as u64 * GB,
            "step {step}: the reserve covers only the {} at work", working.len());
        let snapshot = gate.snapshot(&domain, now).unwrap();
        assert!(!matches!(snapshot.hold, Some(Hold::Disk { .. })), "step {step}: {:?} with {} working", snapshot.hold, working.len());
        look(&domain, &home.0, now);
        for id in &paused {
            if !started.contains_key(id) && !still_paused(&domain, id) {
                goes_on(&domain, id, now);
                started.insert(id.clone(), step);
                order.push(id.clone());
            }
        }
        for (index, id) in fresh.iter().enumerate() {
            if !started.contains_key(id) && gate.ask(&domain, Waiting { key: Key::Task(id.as_str().into()), priority: TaskPriority::Normal, resuming: false, since: base + index as Millis }, now).is_ok() {
                start(&domain, id, now);
                started.insert(id.clone(), step);
                order.push(id.clone());
            }
        }
        for (id, at) in &started {
            if !ended.contains(id) && step >= at + 3 {
                finish(&domain, id, now);
                ended.insert(id.clone());
            }
        }
        if ended.len() == 44 {
            break;
        }
    }
    assert_eq!(ended.len(), 44, "{} of 44 got to work and ended", ended.len());
    assert!(order[..18].iter().all(|id| id.starts_with('p')), "the paused come back before new work: {order:?}");
    assert_eq!(machine_cards(&domain), 0);
}

/// 6 November, 08:31: the next node's estimate was 7.85 GB, the disk had been short once and so
/// asked 2 GB more to recover, and 14.5 GB were free: 7.85 + 5 + 2 never fit, so with no node at
/// work nothing ever started again. With no other work reserving the disk, the next node needs
/// only the disk's line.
#[test]
fn with_no_other_node_at_work_the_next_one_comes_back_whatever_its_estimate() {
    let (home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(8)) });
    gate.ignore_mac();
    domain.live.machine_watch.seed_peaks("app", &[7_850_000_000; 3]);
    let base = alondra_core::now_millis();
    create_in(&domain, "t1", "app");
    start(&domain, "t1", base);
    assert!(gate.hold_back(&domain, "agent-t1", Reason::Disk, base));
    finish(&domain, "t1", base);
    disk_sample(&domain, 4 * GB, base + SAMPLE);
    look(&domain, &home.0, base + SAMPLE);
    assert!(still_paused(&domain, "t1"), "4 GB are under the disk's 5 GB line");
    disk_sample(&domain, 14_500_000_000, base + 2 * SAMPLE);
    look(&domain, &home.0, base + 2 * SAMPLE);
    assert!(!still_paused(&domain, "t1"), "14.5 GB free and nothing else at work: it comes back");
    assert_eq!(machine_cards(&domain), 0);
}

/// 11 November, 00:29: the log said "8 of 8 working" and named 14 tasks. What the gate counts and
/// what it names are one list.
#[test]
fn the_gate_names_as_many_tasks_as_it_counts() {
    let (_home, domain) = world();
    let gate = &domain.live.gate;
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    let now = alondra_core::now_millis();
    for id in ["t1", "t2", "t3", "t4"] {
        create(&domain, id);
    }
    for id in ["t1", "t2"] {
        assert_eq!(ask(&domain, id, now), Ok(()));
        start(&domain, id, now);
    }
    assert_eq!(ask(&domain, "t3", now), Ok(()), "admitted, its node not yet at work");
    let snapshot = gate.snapshot(&domain, now).unwrap();
    assert_eq!(snapshot.working, 3);
    assert_eq!(snapshot.working_tasks.len(), snapshot.working, "{:?}", snapshot.working_tasks);
    let state = domain.state().unwrap();
    let working: Vec<TaskId> = snapshot.working_tasks.iter().cloned().collect();
    let line = hold_line(1, "t4", TaskPriority::Normal, &Hold::Slots { working: snapshot.working, limit: 3 }, &working, &[], 0).unwrap();
    assert!(line.contains("3 of 3 working (t1, t2, t3)") && !line.contains("starting"), "{line}");
    assert_eq!(super::working_tasks(&domain, &state).len(), domain.read().index().slots().len());
}
