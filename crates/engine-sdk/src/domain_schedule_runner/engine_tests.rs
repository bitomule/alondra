//! The scheduler's tests that read the engine's own usage pause, disk watch and admission gate, so
//! they open the whole engine.
use super::fixture::Fixture;
use super::*;
use alondra_engine_admission::AdmissionOf as _;
use alondra_store_sqlite::AsSqlite as _;
use crate::live::LiveOf as _;
use crate::model_fixture::SdkLaunches;
use alondra_domain_rules::InteractionId;


#[tokio::test(flavor = "multi_thread")]
async fn a_due_schedule_and_a_pending_launch_wait_out_the_usage_limit_and_each_launch_once_after_it() {
    let fixture = Fixture::new();
    let _sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
    fixture.claim("manual-1");
    let now = alondra_core::now_millis();
    fixture.domain.live().usage_pause.exhaust(Some(now + 3_600_000), now);
    let runs = || fixture.domain.state().unwrap().schedule.runs.len();
    run_cycle(&*fixture.domain, &fixture.home, false).await.unwrap();
    let mut due = fixture.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
    due.enabled = true;
    due.next_run = Some(now);
    due.revision += 1;
    execute_system(&*fixture.domain, Command::UpdateSchedule { schedule: due }).unwrap();

    run_cycle(&*fixture.domain, &fixture.home, true).await.unwrap();
    run_cycle(&*fixture.domain, &fixture.home, true).await.unwrap();
    assert_eq!(fixture.launches(), 0, "paused: Claude is not called");
    assert_eq!(runs(), 1, "the due schedule starts no run: it is deferred, not failed");
    let state = fixture.domain.state().unwrap();
    let manual = &state.schedule.runs[&ScheduleRunId::from("manual-1")];
    assert_eq!(manual.launch.as_ref().unwrap().state, ScheduleLaunchState::Pending, "the claimed launch waits too");
    assert!(state.schedule.runs.values().all(|run| run.state != ScheduleRunState::Failed), "nothing is recorded as failed");
    assert_eq!(state.schedule.schedules[&ScheduleId::from("schedule-1")].next_run, Some(now), "still due");

    fixture.domain.live().usage_pause.resume(alondra_core::now_millis());
    run_cycle(&*fixture.domain, &fixture.home, true).await.unwrap();
    assert_eq!(fixture.launches(), 2, "back: the deferred launch and the due schedule go, one each");
    assert_eq!(runs(), 2);
    run_cycle(&*fixture.domain, &fixture.home, true).await.unwrap();
    assert_eq!(fixture.launches(), 2, "and never again");
    assert_eq!(runs(), 2);
    let state = fixture.domain.state().unwrap();
    assert!(state.schedule.runs.values().all(|run| run.launch.as_ref().is_some_and(|launch| launch.state == ScheduleLaunchState::Handed)));
}


#[tokio::test(flavor = "multi_thread")]
async fn a_due_schedule_and_a_pending_launch_wait_while_the_disk_is_nearly_full() {
    let fixture = Fixture::new();
    let _sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
    fixture.claim("manual-1");
    let now = alondra_core::now_millis();
    fixture.domain.machine_watch().set_disk_low(true);
    run_cycle(&*fixture.domain, &fixture.home, false).await.unwrap();
    let mut due = fixture.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
    due.enabled = true;
    due.next_run = Some(now);
    due.revision += 1;
    execute_system(&*fixture.domain, Command::UpdateSchedule { schedule: due }).unwrap();

    run_cycle(&*fixture.domain, &fixture.home, true).await.unwrap();
    run_cycle(&*fixture.domain, &fixture.home, true).await.unwrap();
    assert_eq!(fixture.launches(), 0, "no node starts on a nearly full disk");
    let state = fixture.domain.state().unwrap();
    assert_eq!(state.schedule.runs[&ScheduleRunId::from("manual-1")].launch.as_ref().unwrap().state, ScheduleLaunchState::Pending);
    assert!(state.schedule.runs.values().all(|run| run.state != ScheduleRunState::Failed), "nothing is recorded as failed");

    fixture.domain.machine_watch().set_disk_low(false);
    run_cycle(&*fixture.domain, &fixture.home, true).await.unwrap();
    assert_eq!(fixture.launches(), 2, "space again: the deferred launch and the due schedule go");
}


/// Puts a run back as a daemon before the fix left it: still waiting to launch after its Task
/// was cancelled.
pub fn leave_launch_waiting(domain: &dyn Runtime, run: &str) {
    let store = domain.store().lock().unwrap();
    let conn = store.sqlite().conn_for_test();
    let triggers: Vec<(String, String)> = conn
        .prepare("SELECT name, sql FROM sqlite_master WHERE type='trigger' AND name IN ('schedule_run_result_transition','schedule_launch_transition')")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(triggers.len(), 2);
    for (name, _) in &triggers {
        conn.execute_batch(&format!("DROP TRIGGER {name}")).unwrap();
    }
    conn.execute(
        "UPDATE schedule_runs SET state='sent', error=NULL, launch=json_set(launch,'$.state','pending','$.finished_at',NULL,'$.error',NULL) WHERE id=?1",
        [run],
    )
    .unwrap();
    for (_, sql) in &triggers {
        conn.execute_batch(sql).unwrap();
    }
}

#[tokio::test]
async fn a_launch_left_waiting_by_a_cancelled_task_never_passes_the_gate_and_ends_on_the_next_start() {
    let fixture = Fixture::new();
    fixture.claim("stuck");
    finish_claim(&*fixture.domain, &fixture.home, &"stuck".into()).unwrap();
    let task = fixture.domain.state().unwrap().schedule.runs[&ScheduleRunId::from("stuck")].task_id.clone().unwrap();
    execute_system(&*fixture.domain, Command::CancelTask { task_id: task.clone(), reason: None }).unwrap();
    leave_launch_waiting(&*fixture.domain, "stuck");

    let restarted = Fixture::open(&fixture.home);
    let state = restarted.state().unwrap();
    let run = &state.schedule.runs[&ScheduleRunId::from("stuck")];
    assert_eq!((run.state, run.launch.as_ref().unwrap().state), (ScheduleRunState::Sent, ScheduleLaunchState::Pending));
    assert_eq!(state.tasks[&task].state, TaskState::Cancelled);
    let waiting = alondra_engine_ports::admission::Waiting {
        key: alondra_engine_ports::admission::Key::Run("stuck".into()),
        priority: alondra_domain_rules::TaskPriority::High,
        resuming: false,
        since: 0,
    };
    assert!(restarted.gate().ask(waiting, alondra_core::now_millis()).is_err(), "the gate never lets a launch of a cancelled task through");

    run_cycle(&restarted, &fixture.home, false).await.unwrap();
    let state = restarted.state().unwrap();
    let run = &state.schedule.runs[&ScheduleRunId::from("stuck")];
    assert_eq!(run.state, ScheduleRunState::Failed);
    assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Failed);
    assert_eq!(run.error.as_deref(), Some(alondra_domain_rules::SCHEDULED_TASK_CANCELLED));
    assert!(!state.interactions.contains_key(&InteractionId::from("problem-launch-stuck")), "no Problem on a cancelled Task");
    let revision = state.revision;
    run_cycle(&restarted, &fixture.home, false).await.unwrap();
    assert_eq!(restarted.state().unwrap().revision, revision, "it is settled once");
}
