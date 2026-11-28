//! What the disk gate reserves for each task, when a task stops reserving, and how the gate says
//! who holds the disk.
use super::tests::{GB, Home, ask, enqueue, run, start, world};
use super::*;
use crate::domain_runtime::DomainRuntime;
use alondra_core::Machine;
use alondra_domain_model::{Actor, Command};
use alondra_engine_ports::disk::Space;

const SAMPLE: Millis = 10_000;

fn key(task: &str) -> Key {
    Key::Task(task.into())
}

/// A watch whose projects kept these peaks, in GB.
fn watch_with(projects: &[(&str, &[f64])]) -> crate::machine_watch::MachineWatch {
    let watch = crate::machine_watch::MachineWatch::default();
    for (project, peaks) in projects {
        let peaks: Vec<u64> = peaks.iter().map(|peak| (peak * GB as f64) as u64).collect();
        watch.seed_peaks(project, &peaks);
    }
    watch.measure_disk(Space { free: 100 * GB, purgeable: 0, total: 500 * GB });
    watch
}

/// What the gate reserves for a task of `project` it admits.
fn reserves(watch: &crate::machine_watch::MachineWatch, task: &str, project: &str) -> u64 {
    watch.reserve_admission(key(task), Some(project.into()));
    watch.disk_holders(&BTreeMap::new(), &|_| false, 0).iter().find(|holder| holder.key == key(task)).map_or(0, |holder| holder.bytes)
}

// spec: disk-gate.reservation/average-of-its-project
#[test]
fn a_project_measured_three_times_reserves_the_average_of_its_peaks() {
    assert_eq!(reserves(&watch_with(&[("app", &[1.0, 2.0, 6.0])]), "t", "app"), 3 * GB);
}

// spec: disk-gate.reservation/never-under-one-gigabyte
#[test]
fn the_average_is_never_under_one_gigabyte() {
    assert_eq!(reserves(&watch_with(&[("app", &[0.1, 0.2, 0.3])]), "t", "app"), GB);
}

// spec: disk-gate.reservation/project-measured-too-few-times
#[test]
fn a_project_measured_too_few_times_reserves_the_average_of_the_measured_ones_or_its_own_if_larger() {
    let watch = watch_with(&[("app", &[1.0, 2.0, 6.0]), ("web", &[2.0, 2.0, 2.0]), ("new", &[0.5]), ("big", &[9.0])]);
    assert_eq!(reserves(&watch, "t-new", "new"), 2_500_000_000);
    assert_eq!(reserves(&watch, "t-big", "big"), 9 * GB);
}

// spec: disk-gate.reservation/nothing-measured-yet
#[test]
fn with_no_project_measured_three_times_the_gate_reserves_8_gb() {
    assert_eq!(reserves(&watch_with(&[("web", &[2.0, 2.0])]), "t", "app"), 8 * GB);
}

// spec: disk-gate.reservation/one-estimate-per-task
#[test]
fn a_task_admitted_three_times_between_two_readings_reserves_one_estimate() {
    let watch = watch_with(&[("app", &[1.0, 2.0, 6.0])]);
    for _start_resume_and_handover in 0..3 {
        watch.reserve_admission(key("A"), Some("app".into()));
    }
    assert_eq!(watch.disk_reserved(&BTreeMap::new()), 3 * GB);
}

fn create(domain: &DomainRuntime, id: &str, title: &str, project: &str) {
    let mut launch = alondra_domain_model::LaunchPlan::new("node", "/tmp");
    launch.project = Some(project.into());
    run(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: id.into(), request_id: "r".into(), title: title.into(), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(launch),
    });
}

// spec: disk-gate.release/ended-task-releases
#[test]
fn a_task_that_ends_stops_reserving_at_the_gates_next_decision() {
    let (_home, domain) = world();
    for id in ["A", "B"] {
        create(&domain, id, id, "app");
    }
    let open = domain.state().unwrap();
    let ends: [(&str, fn(&mut alondra_domain_model::Task)); 3] = [
        ("cancelled", |task| task.state = TaskState::Cancelled),
        ("done", |task| task.state = TaskState::Done),
        ("closing after its delivery was accepted", |task| task.state = TaskState::Closing),
    ];
    for (ending, end) in ends {
        let watch = watch_with(&[("app", &[1.0, 2.0, 6.0])]);
        for id in ["A", "B"] {
            watch.reserve_admission(key(id), Some("app".into()));
        }
        assert_eq!(watch.disk_reserved(&BTreeMap::new()), 6 * GB);
        let mut state = open.as_ref().clone();
        end(state.tasks.get_mut(&TaskId::from("A")).unwrap());
        let holders = watch.disk_holders(&BTreeMap::new(), &|task| releases_disk(&state, task), 0);
        assert_eq!(holders, [alondra_engine_ports::admission::DiskHolder { key: key("B"), bytes: 3 * GB }], "A {ending}");
        assert_eq!(watch.disk_reserved(&BTreeMap::new()), 3 * GB, "A {ending}: released without a new reading");
    }
    assert!(!releases_disk(&open, &"A".into()), "a task at work keeps its reservation");
}

/// A sample of the Mac with its own disk at `usable` bytes, `free` of them free right now.
fn disk_sample(domain: &DomainRuntime, usable: u64, free: u64, now: Millis) {
    crate::machine_watch::pace(domain, &Machine { at: now, memory_total: 16 << 30, memory_available: 12 << 30, memory_free_percent: Some(50), cpus: 11, load: [5.0; 3],
        disk_total: 500 * GB, disk_available: free, disk_purgeable: usable - free, ..Machine::default() });
    domain.machine_watch().measure_disk(Space { free, purgeable: usable - free, total: 500 * GB });
}

/// 28 November, 10:23: a high priority task that only had to run one command waited with 25.6 GB
/// usable, because the gate counted the admissions of a task that kept handing over between its
/// stages, and kept counting them once it was cancelled.
// spec: disk-gate.release/held-command-task-starts
#[test]
fn a_task_that_only_runs_a_command_is_not_held_by_the_reservations_of_a_cancelled_task() {
    let (_home, domain): (Home, _) = world();
    let gate = &domain.admission();
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    gate.ignore_mac();
    domain.machine_watch().seed_peaks("app", &[GB, 2 * GB, 6 * GB]);
    let (usable, free) = (25_600_000_000, 21_800_000_000);
    let base = alondra_core::now_millis();
    disk_sample(&domain, usable, free, base);
    for id in ["A", "B", "C", "D"] {
        create(&domain, id, id, "app");
    }
    for id in ["A", "B"] {
        assert_eq!(ask(&domain, id, base), Ok(()), "{id} starts");
        start(&domain, id, base);
    }
    disk_sample(&domain, usable, free, base + SAMPLE);
    assert_eq!(domain.machine_watch().disk_reserved(&working_projects(&*domain, &domain.state().unwrap())), 6 * GB, "A and B reserve 3 GB each");
    for admission in 0..3 {
        assert_eq!(ask(&domain, "C", base + SAMPLE + admission * IN_FLIGHT), Ok(()), "C admitted, time {admission}");
    }
    run(&domain, Actor::User { user_id: "u".into() }, Command::CancelTask { task_id: "C".into(), reason: None });
    run(&domain, Actor::User { user_id: "u".into() }, Command::SetPriority { task_id: "D".into(), priority: TaskPriority::High });
    let now = base + SAMPLE + 3 * IN_FLIGHT;
    enqueue(&domain, "D", now);
    let snapshot = gate.snapshot(&*domain, now).unwrap();
    assert_eq!(snapshot.disk_needed, 14 * GB, "6 GB for A and B, 3 GB for D and the 5 GB line");
    assert_eq!(snapshot.hold, None, "D starts");
    let waiting = Waiting { key: key("D"), priority: TaskPriority::High, resuming: false, since: now };
    assert_eq!(gate.ask(&*domain, waiting, now), Ok(()));
}

/// Alpha, Beta, Gamma and Delta work and reserve 4, 3, 2 and 1 GB; the disk has 12 GB usable and
/// Echo waits alone in the queue to start, needing 18 GB.
fn four_reserve_and_echo_waits() -> (Home, Arc<DomainRuntime>, Millis) {
    let (home, domain) = world();
    let gate = &domain.admission();
    gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(6)) });
    gate.ignore_mac();
    let tasks = [("alpha", "Alpha", "p4", 4), ("beta", "Beta", "p3", 3), ("gamma", "Gamma", "p2", 2), ("delta", "Delta", "p1", 1)];
    for (_, _, project, gb) in tasks {
        domain.machine_watch().seed_peaks(project, &[gb * GB; 3]);
    }
    let base = alondra_core::now_millis();
    disk_sample(&domain, 12 * GB, 12 * GB, base);
    domain.machine_watch().measure_disk(Space { free: 100 * GB, purgeable: 0, total: 500 * GB });
    for (id, title, project, _) in tasks {
        create(&domain, id, title, project);
        assert_eq!(ask(&domain, id, base), Ok(()), "{id} starts");
        start(&domain, id, base);
    }
    disk_sample(&domain, 12 * GB, 12 * GB, base + SAMPLE);
    create(&domain, "echo", "Echo", "p3");
    enqueue(&domain, "echo", base + SAMPLE);
    (home, domain, base + SAMPLE)
}

// spec: disk-gate.waiting-reason/orchestrator-gets-every-one
#[test]
fn the_orchestrator_reads_every_task_that_holds_the_disk() {
    let (_home, domain, now) = four_reserve_and_echo_waits();
    assert_eq!(queued_text(&*domain, &"echo".into(), now),
        "It waits in the queue, 1 of 1: the disk has no room for one more build (12.0 GB free of 18.0 GB needed; reserved by “Alpha” 4.0 GB, “Beta” 3.0 GB, “Gamma” 2.0 GB and “Delta” 1.0 GB). Alondra starts it by itself when its turn comes; you do not need to launch it again.");
}

// spec: disk-gate.waiting-reason/log-gets-every-one
#[test]
fn the_gates_log_line_names_every_task_that_holds_the_disk() {
    let (_home, domain, now) = four_reserve_and_echo_waits();
    let snapshot = domain.admission().snapshot(&*domain, now).unwrap();
    let hold = snapshot.hold.expect("the disk holds echo");
    let line = hold_line(&domain.state().unwrap(), snapshot.queue.len(), "echo", TaskPriority::Normal, &hold, &[], &[], 0).unwrap();
    assert_eq!(line, "the admission gate holds 1 waiting (next echo, Normal): the disk has no room for one more build: 12.0 GB free of 18.0 GB needed, reserved by alpha 4.00 GB, beta 3.00 GB, gamma 2.00 GB, delta 1.00 GB");
}

const BOUNDARY: &str = include_str!("../../../specs/disk-gate/boundary.toml");

// spec: disk-gate.boundary/depends-on-no-component
#[test]
fn the_disk_gate_depends_on_no_other_component() {
    let listed: Vec<&str> = BOUNDARY.lines().filter_map(|line| line.strip_prefix("depends_on")).map(|rest| rest.trim_start().trim_start_matches('=').trim()).collect();
    assert_eq!(listed, ["[]"]);
}
