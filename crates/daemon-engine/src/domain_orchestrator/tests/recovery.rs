use super::testing::{Fixture, fixture};
use super::*;
use alondra_core::domain::LaunchPlan;
use std::sync::atomic::{AtomicBool, Ordering};

fn request(f: &Fixture, id: &str) {
    f.run(
        Actor::User {
            user_id: "u".into(),
        },
        id,
        Command::CreateRequest {
            request_id: id.into(),
            body: format!("Create the task for {id}"),
            auto_merge: false,
            maintenance: false,
            message: false,
        },
    );
}

fn task_on_next_spawn(f: &Fixture, requests: &[&str]) -> (faults::SpawnHook, Arc<AtomicBool>) {
    task_then_on_next_spawn(f, requests, || {})
}

fn task_then_on_next_spawn(f: &Fixture, requests: &[&str], after: impl FnOnce() + Send + 'static) -> (faults::SpawnHook, Arc<AtomicBool>) {
    let domain = Arc::clone(&f.executor.domain);
    let home = f.executor.home.clone();
    let requests: Vec<String> = requests.iter().map(|id| (*id).to_owned()).collect();
    let called = Arc::new(AtomicBool::new(false));
    let done = Arc::clone(&called);
    let hook = faults::on_spawn(&f.executor.home, move |bearer| {
        for id in requests {
            let state = domain.read();
            if !state
                .index()
                .tasks_of_request(&id.as_str().into())
                .is_empty()
            {
                continue;
            }
            let envelope = CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.state().data_epoch.clone(),
                command_id: format!("task-for-{id}").into(),
                expected_revision: 0,
                command: Command::CreateTask {
                    plan_items: None, continues: None,
                    task_id: format!("task-{id}").into(),
                    request_id: id.as_str().into(),
                    title: format!("Task for {id}"),
                    agent_id: None,
                    review_prs: false,
                    dependencies: Default::default(),
                    stack: None,
                    launch: Some(LaunchPlan::new("node", home.display().to_string())),
                },
            };
            drop(state);
            domain.execute_bearer(bearer, &envelope).unwrap();
        }
        after();
        done.store(true, Ordering::SeqCst);
    });
    (hook, called)
}

#[tokio::test]
async fn a_write_failure_claiming_a_batch_and_writing_its_runtime_recovers_without_a_daemon_restart() {
    a_write_failure_recovers_without_a_daemon_restart(&[("claim", 1), ("claim_commit", 1), ("runtime_write", 1), ("runtime_write", 2), ("runtime_write", 3), ("runtime_rename", 1), ("runtime_rename", 2), ("runtime_rename", 3)]).await;
}

#[tokio::test]
async fn a_write_failure_its_first_capability_recovers_without_a_daemon_restart() {
    a_write_failure_recovers_without_a_daemon_restart(&[("capability", 1), ("capability_create", 1), ("capability_write", 1), ("capability_sync", 1), ("capability_rename", 1), ("capability_directory_sync", 1)]).await;
}

#[tokio::test]
async fn a_write_failure_its_second_capability_recovers_without_a_daemon_restart() {
    a_write_failure_recovers_without_a_daemon_restart(&[("capability", 2), ("capability_create", 2), ("capability_write", 2), ("capability_sync", 2), ("capability_rename", 2), ("capability_directory_sync", 2)]).await;
}

#[tokio::test]
async fn a_write_failure_its_owner_recovers_without_a_daemon_restart() {
    a_write_failure_recovers_without_a_daemon_restart(&[("owner_create", 1), ("owner_write", 1), ("owner_sync", 1), ("owner_rename", 1), ("owner_directory_sync", 1)]).await;
}

#[tokio::test]
async fn a_write_failure_reporting_and_finishing_a_batch_recovers_without_a_daemon_restart() {
    a_write_failure_recovers_without_a_daemon_restart(&[("report", 1), ("report", 3), ("report_commit", 1), ("ack", 1), ("ack", 3), ("delivery_finish", 1), ("delivery_finish", 2), ("delivery_commit", 1), ("finish_batch", 1), ("finish_batch_commit", 1)]).await;
}

async fn a_write_failure_recovers_without_a_daemon_restart(points: &[(&'static str, usize)]) {
    for &(point, occurrence) in points {
        let f = fixture();
        request(&f, "first");
        let (first_hook, _) = task_on_next_spawn(&f, &["first"]);
        let fault = faults::Injection::at(&f.executor.home, point, occurrence);
        let _ = f.executor.tick().await;
        assert!(
            fault.hits() > 0,
            "{point}/{occurrence} did not fail a write"
        );
        drop(fault);
        drop(first_hook);
        request(&f, "second");
        let (_hook, called) = task_on_next_spawn(&f, &["first", "second"]);
        let (_hold, stop) = tokio::sync::watch::channel(false);
        for _ in 0..3 {
            let now = load(&f.executor.home)
                .unwrap()
                .retry_at
                .unwrap_or_else(now_millis);
            f.executor
                .tick_with_clock(stop.clone(), Some(now))
                .await
                .unwrap();
            if called.load(Ordering::SeqCst) {
                break;
            }
        }
        assert!(
            called.load(Ordering::SeqCst),
            "{point}/{occurrence}: writes recovered but no batch processed the pending requests"
        );
        let prompts = f.prompts();
        let prompt = prompts.last().unwrap();
        assert!(
            prompt.contains("request second:"),
            "{point}/{occurrence}: the native provider did not see the pending request"
        );
        let state = f.domain().read();
        assert_eq!(state.state().tasks.len(), 2, "{point}/{occurrence}");
        assert!(
            f.attempts()
                .iter()
                .all(|state| !matches!(state.as_str(), "dispatching" | "uncertain")),
            "{point}/{occurrence}: {:?}",
            f.attempts()
        );
        assert!(
            load(&f.executor.home).unwrap().active_run.is_none(),
            "{point}/{occurrence}"
        );
        assert!(
            f.cards(PROBLEM_PREFIX).is_empty() && f.cards(STUCK_PROBLEM_PREFIX).is_empty(),
            "{point}/{occurrence}"
        );
        assert!(
            !f.executor
                .home
                .join("runtime/orchestrator/owner.json")
                .exists()
        );
        println!("recovered {point}/{occurrence}: 2 requests -> 2 tasks, no restart, no Problem");
    }
}

#[tokio::test]
async fn repeated_recovery_write_failures_keep_the_claim_until_space_returns() {
    let f = fixture();
    request(&f, "pending");
    let fault = faults::Injection::at(&f.executor.home, "runtime_write", 1);
    assert!(f.executor.tick().await.is_err());
    drop(fault);
    let fault = faults::Injection::at(&f.executor.home, "reconcile", 1);
    for _ in 0..3 {
        assert!(f.executor.tick().await.is_err());
    }
    assert_eq!(fault.hits(), 3);
    assert!(f.starts().is_empty());
    drop(fault);
    let (_hook, called) = task_on_next_spawn(&f, &["pending"]);
    f.tick().await;
    assert!(called.load(Ordering::SeqCst));
    assert_eq!(f.domain().read().state().tasks.len(), 1);
}

#[tokio::test]
async fn non_actionable_confirmed_batch_interruption_continues_without_repeating_its_task() {
    let f = fixture();
    request(&f, "pending");
    let (_hook, called) = task_on_next_spawn(&f, &["pending"]);
    f.mark_once("fixture:tool-then-exit");
    f.tick().await;
    assert!(called.load(Ordering::SeqCst));
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_none());
    assert_eq!(runtime.batch_handoffs.len(), 1);
    let run = &runtime.batch_handoffs[0].run_id;
    assert_eq!(f.domain().store.lock().unwrap().batch_effects(run).unwrap().len(), 1);
    assert!(f.cards(PROBLEM_PREFIX).is_empty());
    assert_eq!(f.domain().read().state().tasks.len(), 1);
    let (_hold, stop) = tokio::sync::watch::channel(false);
    f.executor.tick_with_clock(stop.clone(), Some(runtime.retry_at.unwrap() - 1)).await.unwrap();
    assert_eq!(f.starts().len(), 1, "a stopped native turn uses retry backoff");
    f.executor.tick_with_clock(stop, runtime.retry_at).await.unwrap();
    assert_eq!(f.starts().len(), 2);
    assert!(f.prompts().last().unwrap().contains("CONFIRMED BATCH HANDOFF"));
    assert_eq!(f.domain().read().state().tasks.len(), 1, "the committed task is not created twice");
    assert_eq!(f.domain().store.lock().unwrap().batch_effects(run).unwrap().len(), 1);
    assert!(load(&f.executor.home).unwrap().batch_handoffs.is_empty());
    assert!(f.cards(PROBLEM_PREFIX).is_empty());
}

#[tokio::test]
async fn non_actionable_confirmed_batch_recovers_a_failed_runtime_write_after_claim_release() {
    let f = fixture();
    request(&f, "pending");
    let fault = Arc::new(std::sync::Mutex::new(None));
    let armed = Arc::clone(&fault);
    let home = f.executor.home.clone();
    let (_hook, _) = task_then_on_next_spawn(&f, &["pending"], move || {
        *armed.lock().unwrap() = Some(faults::Injection::at(&home, "runtime_write", 2));
    });
    f.mark_once("fixture:tool-then-exit");
    assert!(f.executor.tick().await.is_err());
    let fault = fault.lock().unwrap().take().unwrap();
    assert_eq!(fault.hits(), 1);
    drop(fault);
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_some(), "the final runtime write did not commit");
    assert_eq!(runtime.batch_handoffs.len(), 1);
    assert_eq!(f.attempts(), ["retryable"], "the claims were already released");
    f.executor.repair_incomplete().await.unwrap();
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_none());
    assert_eq!(runtime.batch_handoffs.len(), 1);
    f.tick().await;
    assert!(f.prompts().last().unwrap().contains("CONFIRMED BATCH HANDOFF"));
    assert_eq!(f.domain().read().state().tasks.len(), 1);
    assert!(load(&f.executor.home).unwrap().batch_handoffs.is_empty());
    assert!(f.cards(PROBLEM_PREFIX).is_empty());
}

#[tokio::test]
async fn non_actionable_read_only_batch_interruptions_back_off_and_only_persistent_failure_asks() {
    let f = fixture();
    f.tell("read-only", "Inspect the current state");
    f.mark("fixture:read-then-exit");
    let (_hold, stop) = tokio::sync::watch::channel(false);
    for failure in 1..=STUCK_AFTER + 1 {
        let next = load(&f.executor.home).unwrap().retry_at;
        f.executor.tick_with_clock(stop.clone(), next).await.unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert_eq!(runtime.consecutive_failures, failure);
        assert!(runtime.active_run.is_none() && runtime.retry_at.is_some());
        assert!(f.cards(PROBLEM_PREFIX).is_empty());
        assert_eq!(f.cards(STUCK_PROBLEM_PREFIX).len(), usize::from(failure >= STUCK_AFTER));
        let starts = f.starts().len();
        f.executor.tick_with_clock(stop.clone(), Some(runtime.retry_at.unwrap() - 1)).await.unwrap();
        assert_eq!(f.starts().len(), starts, "no immediate retry loop");
    }
    f.clear_marker();
    let next = load(&f.executor.home).unwrap().retry_at;
    f.executor.tick_with_clock(stop, next).await.unwrap();
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.consecutive_failures, 0);
    assert!(runtime.stuck_problem.is_none() && runtime.batch_handoffs.is_empty());
    assert!(f.domain().read().state().interactions.values()
        .filter(|card| card.id.0.starts_with(STUCK_PROBLEM_PREFIX)).all(|card| card.withdrawn));
}

#[tokio::test]
async fn confirmed_effects_survive_failed_completion_without_repeating_the_task() {
    let f = fixture();
    request(&f, "pending");
    let (_hook, called) = task_on_next_spawn(&f, &["pending"]);
    let fault = faults::Injection::at(&f.executor.home, "report", 1);
    f.executor.tick().await.unwrap();
    assert!(called.load(Ordering::SeqCst));
    drop(fault);
    f.tick().await;
    assert_eq!(f.domain().read().state().tasks.len(), 1);
    assert!(
        f.prompts()
            .last()
            .unwrap()
            .contains("CONFIRMED BATCH HANDOFF")
    );
    assert!(load(&f.executor.home).unwrap().active_run.is_none());
    assert!(f.cards(PROBLEM_PREFIX).is_empty());
}

#[tokio::test]
async fn a_failed_handoff_cleanup_does_not_run_completed_goals_again() {
    let f = fixture();
    request(&f, "pending");
    let (_hook, called) = task_on_next_spawn(&f, &["pending"]);
    let fault = faults::Injection::at(&f.executor.home, "report", 1);
    f.executor.tick().await.unwrap();
    assert!(called.load(Ordering::SeqCst));
    drop(fault);
    f.executor.repair_incomplete().await.unwrap();
    assert_eq!(load(&f.executor.home).unwrap().batch_handoffs.len(), 1);
    let fault = faults::Injection::at(&f.executor.home, "finish_batch", 2);
    assert!(f.executor.tick().await.is_err());
    let starts = f.starts().len();
    drop(fault);
    f.tick().await;
    assert_eq!(
        f.starts().len(),
        starts,
        "the continuation already completed"
    );
    let runtime = load(&f.executor.home).unwrap();
    assert!(
        runtime.batch_handoffs.is_empty()
            && runtime.pending_resolutions.is_empty()
            && runtime.active_run.is_none()
    );
    assert_eq!(f.domain().read().state().tasks.len(), 1);
}

#[tokio::test]
async fn an_already_uncertain_claim_can_be_acknowledged_without_resurrecting_its_problem() {
    let f = fixture();
    request(&f, "pending");
    let epoch = f.domain().read().state().data_epoch.clone();
    let selection =
        crate::orchestrator_provider::selection(f.domain(), &f.executor.home, now_millis())
            .unwrap();
    let ids: Vec<i64> = f
        .executor
        .pending()
        .unwrap()
        .iter()
        .map(|message| message.id)
        .collect();
    let (claims, owner) = f
        .domain()
        .store
        .lock()
        .unwrap()
        .claim_selected_orchestrator_batch(
            &ids,
            "uncertain",
            "uncertain-session",
            epoch,
            Some(&selection),
        )
        .unwrap();
    drop(owner);
    {
        let mut store = f.domain().store.lock().unwrap();
        store
            .begin_tool_operation(
                "uncertain",
                "call",
                "launch",
                &serde_json::json!({"task":"unknown"}),
            )
            .unwrap();
        for claim in &claims {
            store
                .finish_message(
                    claim.attempt_id,
                    claim.version,
                    State::Uncertain,
                    Some("interrupted"),
                )
                .unwrap();
        }
    }
    f.executor.repair_incomplete().await.unwrap();
    assert_eq!(
        load(&f.executor.home).unwrap().failure,
        Some(Failure::Interrupted)
    );
    f.executor.acknowledge("uncertain").unwrap();
    f.tick().await;
    f.tick().await;
    assert!(load(&f.executor.home).unwrap().active_run.is_none());
    assert!(!f.attempts().iter().any(|state| state == "uncertain"));
    let state = f.domain().read();
    assert!(
        state
            .state()
            .interactions
            .values()
            .filter(|card| card.id.0.starts_with(PROBLEM_PREFIX))
            .all(|card| card.withdrawn)
    );
}

#[tokio::test]
async fn explicit_cancellation_wins_over_watchdog_restart_and_failed_finalization() {
    for (effects, failed_save) in [(false, false), (true, false), (true, true)] {
        let f = fixture();
        request(&f, "cancelled");
        let _hook = effects.then(|| task_on_next_spawn(&f, &["cancelled"]));
        f.mark_once("fixture:quiet-hold");
        let (restart, stop) = tokio::sync::watch::channel(false);
        let during = async {
            f.held().await;
            let run = load(&f.executor.home).unwrap().active_run.unwrap();
            f.executor.cancel(&run.id).unwrap();
            let fault =
                failed_save.then(|| faults::Injection::at(&f.executor.home, "runtime_write", 1));
            restart.send_replace(true);
            f.executor.interrupt_host().unwrap();
            fault
        };
        let (result, fault) = tokio::join!(f.executor.tick_with_stop(stop), during);
        if let Some(fault) = &fault {
            assert!(result.is_err());
            assert!(fault.hits() > 0);
        } else {
            result.unwrap();
        }
        drop(fault);
        let starts = f.starts().len();
        f.executor.repair_incomplete().await.unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert!(
            runtime.batch_handoffs.is_empty(),
            "a user cancellation must not become an automatic continuation"
        );
        if effects {
            assert_eq!(runtime.failure, Some(Failure::Cancelled));
            assert!(runtime.active_run.is_some());
            f.tick().await;
            assert_eq!(
                f.starts().len(),
                starts,
                "cancelled effects require the user's decision"
            );
            assert_eq!(f.domain().read().state().tasks.len(), 1);
        } else {
            assert!(runtime.active_run.is_none());
            assert!(f.attempts().iter().all(|state| state == "discarded"));
        }
    }
}

#[tokio::test]
async fn a_claude_denial_cannot_be_replayed_after_losing_both_final_writes_and_restarting() {
    let f = fixture();
    request(&f, "denied");
    f.mark_once("fixture:denied");
    rusqlite::Connection::open(crate::domain_database_migration::active_path(&f.executor.home)).unwrap()
        .execute_batch("CREATE TRIGGER reject_terminal_report BEFORE INSERT ON orchestrator_batch_reports WHEN json_extract(NEW.report,'$.kind')='result' BEGIN SELECT RAISE(ABORT,'injected terminal report failure'); END;").unwrap();
    let fault = faults::Injection::at(&f.executor.home, "runtime_write", 3);
    assert!(f.executor.tick().await.is_err());
    assert!(fault.hits() > 0);
    drop(fault);
    let restarted = Executor {
        domain: Arc::new(DomainRuntime::open(&f.executor.home).unwrap()),
        home: f.executor.home.clone(),
        claude_home: f.executor.claude_home.clone(),
    };
    restarted.repair_incomplete().await.unwrap();
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.failure, Some(Failure::Interrupted));
    assert!(runtime.active_run.is_some() && runtime.batch_handoffs.is_empty());
    restarted.tick().await.unwrap();
    assert_eq!(
        f.starts().len(),
        1,
        "missing terminal evidence cannot erase a possible refusal"
    );
    assert!(restarted.pending().unwrap().is_empty());
}

async fn finish_batch_after_tool_call(
    f: &Fixture,
    kind: &'static str,
    answer: Result<serde_json::Value, DomainError>,
) {
    f.mark_once("fixture:quiet-hold");
    let during = async {
        f.held().await;
        let bearer = f.starts().into_iter().next().unwrap();
        let outcome = crate::domain_runtime::batch_operations::execute(
            Arc::clone(&f.executor.domain),
            &bearer,
            Some("abcd"),
            kind,
            serde_json::json!({"task": "task"}),
            async move { answer },
        )
        .await;
        f.release();
        outcome
    };
    let ((), outcome) = tokio::join!(f.tick(), during);
    drop(outcome);
}

#[tokio::test]
async fn a_tool_call_the_domain_refused_does_not_stop_a_batch_that_finishes() {
    let f = fixture();
    request(&f, "refused");
    let refusal = DomainError::new(
        "conflict",
        "the task or its launch plan changed; read it again before updating",
    );
    finish_batch_after_tool_call(&f, "set_launch_plan", Err(refusal)).await;
    let runtime = load(&f.executor.home).unwrap();
    assert!(
        runtime.active_run.is_none() && runtime.failure.is_none() && runtime.last_error.is_none(),
        "{runtime:?}"
    );
    assert!(
        f.cards(PROBLEM_PREFIX).is_empty(),
        "a refusal changed nothing, so there is nothing for the user to decide"
    );
    assert_eq!(f.attempts(), ["handed"]);
}

#[tokio::test]
async fn a_tool_call_whose_outcome_was_lost_still_stops_the_batch() {
    let f = fixture();
    request(&f, "lost");
    let lost = DomainError::new(
        "operation_uncertain",
        "The operation executor stopped; reconcile its effects before continuing",
    );
    finish_batch_after_tool_call(&f, "send", Err(lost)).await;
    assert_eq!(f.cards(PROBLEM_PREFIX).len(), 1);
    assert_eq!(f.attempts(), ["uncertain"]);
}
