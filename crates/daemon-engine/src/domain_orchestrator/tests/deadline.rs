//! What a task waits for from the orchestrator has a deadline: a stopped node's case is reminded
//! after 10 minutes and settled by Alondra after 20, however busy or failing the orchestrator is,
//! and never while the usage limit holds it.
use super::testing::fixture;
use super::*;
use alondra_core::domain::{
    LaunchPlan, LifecycleRecord, Recipient, TaskId, RuntimeObservation, STALL_ATTEMPTS, StallClosing, StallOutcome, UNSETTLED_INSTRUCT, is_stall_case,
};
use alondra_core::orchestrator_cards::STALL_NOTICE_PREFIX;

const MINUTE: i64 = 60_000;

fn system(domain: &DomainRuntime, command: Command) {
    let state = domain.state().unwrap();
    let shown = format!("{command:?}");
    domain.execute_as_system(CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("c-{}", state.revision).into(),
        expected_revision: state.revision, command,
    }).unwrap_or_else(|error| panic!("{shown} was refused: {error:?}"));
}

fn as_actor(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("c-{}", state.revision).into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

fn task() -> TaskId {
    "t".into()
}

fn open_case(domain: &DomainRuntime) -> Option<InteractionId> {
    let state = domain.state().unwrap();
    state.interactions.values().find(|i| is_stall_case(i) && state.open(i)).map(|i| i.id.clone())
}

/// The node of task `t` starts each turn with what it was told last and stops with nothing,
/// until a case about it is open.
fn stop_until_a_case(domain: &DomainRuntime) -> InteractionId {
    for n in 0.. {
        if let Some(case) = open_case(domain) {
            return case;
        }
        let state = domain.state().unwrap();
        let turn = format!("turn-{}-{n}", state.revision);
        let input = state.lifecycle_of(&task()).unwrap().pending_inputs().first().map(|input| input.as_str().to_owned()).expect("something to start the turn with");
        system(domain, Command::RecordRuntimeFacts { task_id: task(), generation: 1, facts: vec![RuntimeObservation::TurnStarted { turn: turn.clone(), input }] });
        system(domain, Command::RecordTurnBoundary { task_id: task(), generation: 1, turn, failure: None, failed: None });
    }
    unreachable!()
}

/// Task `t`, whose node runs a turn through the SDK and stops until its first case is open.
fn stalled(domain: &DomainRuntime) -> InteractionId {
    system(domain, Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
    as_actor(domain, Actor::User { user_id: "u".into() }, Command::CreateRequest {
        request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
    });
    as_actor(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        plan_items: None, continues: None, task_id: task(), request_id: "r".into(), title: "Task".into(), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    crate::domain_store::admit_through_sdk(domain, &task(), "a");
    system(domain, Command::RecordRuntimeFacts { task_id: task(), generation: 1, facts: vec![RuntimeObservation::ResourceAvailable] });
    stop_until_a_case(domain)
}

fn reminded(domain: &DomainRuntime, case: &InteractionId) -> bool {
    domain.state().unwrap().interactions.contains_key(&InteractionId::from(format!("{STALL_NOTICE_PREFIX}{case}").as_str()))
}

/// The node has `text` pending as the answer to `case`.
fn instructed(domain: &DomainRuntime, case: &InteractionId, text: &str) -> bool {
    let state = domain.state().unwrap();
    let answered = state.lifecycle_journal[&task()].iter().any(|record| matches!(record,
        LifecycleRecord::Answered { card, text: Some(said), .. } if card == case && said == text));
    answered && !state.lifecycle_of(&task()).unwrap().pending_inputs().is_empty()
}

fn user_cards(domain: &DomainRuntime) -> Vec<InteractionId> {
    domain.state().unwrap().interactions.values().filter(|i| i.recipient == Recipient::User).map(|i| i.id.clone()).collect()
}

/// What the orchestrator is told and what Alondra does as the minutes pass.
async fn tick_at(executor: &Executor, minutes: i64) {
    let (_hold, stop) = tokio::sync::watch::channel(false);
    let _ = executor.tick_with_clock(stop, Some(minutes * MINUTE)).await;
}

/// The orchestrator keeps failing a batch with messages always waiting for it.
fn busy(f: &super::testing::Fixture, minutes: i64) {
    f.executor.domain.tell_orchestrator(&format!("busy-{minutes}"), None, format!("Work that arrived at minute {minutes}")).unwrap();
    let mut runtime = load(&f.executor.home).unwrap();
    runtime.retry_at = Some(i64::MAX);
    save(&f.executor.home, &runtime).unwrap();
}

#[tokio::test]
async fn a_stall_case_is_settled_by_time_while_the_orchestrator_is_busy() {
    let f = fixture();
    let domain = &f.executor.domain;
    let case = stalled(domain);
    for minutes in [0, 5, 10, 15, 21] {
        busy(&f, minutes);
        tick_at(&f.executor, minutes).await;
        assert_eq!(reminded(domain, &case), minutes >= 10, "reminded at 10 minutes, not before (minute {minutes})");
    }
    assert!(instructed(domain, &case, UNSETTLED_INSTRUCT), "the node has the default instruction: {:?}", domain.state().unwrap().lifecycle_journal[&task()].last());
    assert!(user_cards(domain).is_empty(), "no card for the user");
}

#[tokio::test]
async fn a_last_error_does_not_stop_the_fallback() {
    let f = fixture();
    let domain = &f.executor.domain;
    let case = stalled(domain);
    for minutes in [0, 10, 21] {
        domain.store.lock().unwrap().discard_expired_orchestrator_messages(i64::MAX).unwrap();
        let mut runtime = load(&f.executor.home).unwrap();
        runtime.last_error = Some("the batch failed".into());
        save(&f.executor.home, &runtime).unwrap();
        tick_at(&f.executor, minutes).await;
    }
    assert!(instructed(domain, &case, UNSETTLED_INSTRUCT));
}

#[test]
fn after_three_cases_the_task_closes_with_its_result() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    let mut case = stalled(&domain);
    for n in 1..=STALL_ATTEMPTS {
        as_actor(&domain, Actor::Orchestrator { generation: 0 }, Command::SettleStall {
            case_id: case.clone(), task_id: task(), outcome: StallOutcome::Instruct { text: format!("Instruction {n}") },
        });
        case = stop_until_a_case(&domain);
    }
    assert_eq!(domain.state().unwrap().tasks[&task()].stalls, STALL_ATTEMPTS + 1);
    for minutes in [0, 10, 21] {
        crate::orchestrator_inbox::settle_overdue(&domain, minutes * MINUTE);
    }
    let state = domain.state().unwrap();
    assert_eq!(state.tasks[&task()].stall_closing, Some(StallClosing::Unsettled), "closed with what it got done");
    assert!(state.tasks[&task()].cancelled(), "{:?}", state.tasks[&task()].state);
    assert!(user_cards(&domain).is_empty());
}

#[test]
fn no_fallback_runs_while_the_usage_limit_holds_the_orchestrator() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    let case = stalled(&domain);
    domain.live.usage_pause.exhaust(Some(2 * 60 * MINUTE), 0);
    for minutes in (0..=60).step_by(5) {
        crate::orchestrator_inbox::settle_overdue(&domain, minutes * MINUTE);
    }
    assert!(!reminded(&domain, &case), "an hour held by the usage limit counts for nothing");
    assert!(domain.state().unwrap().open(&domain.state().unwrap().interactions[&case]));
    domain.live.usage_pause.resume(60 * MINUTE);
    for minutes in [61, 71] {
        crate::orchestrator_inbox::settle_overdue(&domain, minutes * MINUTE);
    }
    assert!(reminded(&domain, &case), "the clock runs again once the limit is over");
}
