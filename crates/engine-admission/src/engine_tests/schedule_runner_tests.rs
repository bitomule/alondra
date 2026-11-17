//! Scheduled runs at the gate: they wait while the disk is nearly full, and a launch of a cancelled
//! task never passes.
use super::*;
use alondra_daemon_engine::domain_schedule_runner::*;
use alondra_daemon_engine::domain_schedule_runner::tests::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_due_schedule_and_a_pending_launch_wait_while_the_disk_is_nearly_full() {
    wired();
    let fixture = Fixture::new();
    let _sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
    fixture.claim("manual-1");
    let now = alondra_core::now_millis();
    fixture.domain.machine_watch().set_disk_low(true);
    run_cycle(&fixture.domain, &fixture.home, false).await.unwrap();
    let mut due = fixture.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
    due.enabled = true;
    due.next_run = Some(now);
    due.revision += 1;
    execute_system(&fixture.domain, Command::UpdateSchedule { schedule: due }).unwrap();

    run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
    run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
    assert_eq!(fixture.launches(), 0, "no node starts on a nearly full disk");
    let state = fixture.domain.state().unwrap();
    assert_eq!(state.schedule.runs[&ScheduleRunId::from("manual-1")].launch.as_ref().unwrap().state, ScheduleLaunchState::Pending);
    assert!(state.schedule.runs.values().all(|run| run.state != ScheduleRunState::Failed), "nothing is recorded as failed");

    fixture.domain.machine_watch().set_disk_low(false);
    run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
    assert_eq!(fixture.launches(), 2, "space again: the deferred launch and the due schedule go");
}

#[tokio::test]
async fn a_launch_left_waiting_by_a_cancelled_task_never_passes_the_gate_and_ends_on_the_next_start() {
    wired();
    let fixture = Fixture::new();
    fixture.claim("stuck");
    finish_claim(&fixture.domain, &fixture.home, &"stuck".into()).unwrap();
    let task = fixture.domain.state().unwrap().schedule.runs[&ScheduleRunId::from("stuck")].task_id.clone().unwrap();
    execute_system(&fixture.domain, Command::CancelTask { task_id: task.clone(), reason: None }).unwrap();
    leave_launch_waiting(&fixture.domain, "stuck");

    let restarted = Fixture::open(&fixture.home);
    let state = restarted.state().unwrap();
    let run = &state.schedule.runs[&ScheduleRunId::from("stuck")];
    assert_eq!((run.state, run.launch.as_ref().unwrap().state), (ScheduleRunState::Sent, ScheduleLaunchState::Pending));
    assert_eq!(state.tasks[&task].state, TaskState::Cancelled);
    let waiting = crate::ports::admission::Waiting {
        key: crate::ports::admission::Key::Run("stuck".into()),
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
