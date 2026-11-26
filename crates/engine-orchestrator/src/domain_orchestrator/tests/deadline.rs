//! What a task waits for from the orchestrator has no deadline: a stopped node's case is reminded
//! when a turn of the orchestrator ends handled with it unsettled, and settled by Alondra when the
//! turn that read the reminder ends too. A busy or failing orchestrator ends no such turn, however
//! long the clock runs.
use super::testing::fixture;
use super::*;
use alondra_domain_model::{
    LaunchPlan, LifecycleRecord, Recipient, TaskId, RuntimeObservation, STALL_ATTEMPTS, StallClosing, StallOutcome, UNSETTLED_INSTRUCT, is_stall_case,
};
use alondra_domain_model::orchestrator_cards::STALL_NOTICE_PREFIX;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

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
    as_actor(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(),
        plan_items: None, continues: None, task_id: task(), request_id: "r".into(), title: "Task".into(), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    crate::domain_store::admit_through_sdk(domain, &task(), "a");
    system(domain, Command::RecordRuntimeFacts { task_id: task(), generation: 1, facts: vec![RuntimeObservation::ResourceAvailable] });
    stop_until_a_case(domain)
}

fn reminded(domain: &DomainRuntime, case: &InteractionId) -> bool {
    domain.state().unwrap().interactions.contains_key(&InteractionId::from(format!("{STALL_NOTICE_PREFIX}{case}-1").as_str()))
}

/// The node has `text` pending as the answer to `case`.
fn instructed(domain: &DomainRuntime, case: &InteractionId, text: &str) -> bool {
    let state = domain.state().unwrap();
    let answered = state.lifecycle_journal[&task()].iter().any(|record| matches!(record,
        LifecycleRecord::Answered { card, text: Some(said), .. } if card == case && said == text));
    answered && !state.lifecycle_of(&task()).unwrap().pending_inputs().is_empty()
}

/// A batch of the orchestrator read every message waiting for it and ended with its result: what
/// it owes is settled.
fn turn_ends(domain: &DomainRuntime) {
    {
        let mut store = domain.store.lock().unwrap();
        for message in store.pending_messages().unwrap().into_iter().filter(|message| message.recipient_kind == "orchestrator") {
            if let Some(claim) = store.claim_message(message.id).unwrap() {
                store.finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Handed, None).unwrap();
            }
        }
    }
    crate::orchestrator_inbox::settle(domain);
}

fn user_cards(domain: &DomainRuntime) -> Vec<InteractionId> {
    domain.state().unwrap().interactions.values().filter(|i| i.recipient == Recipient::User).map(|i| i.id.clone()).collect()
}

/// What the orchestrator is told and what Alondra does at `at` on the clock.
async fn tick_at(executor: &Executor, at: i64) {
    let (_hold, stop) = tokio::sync::watch::channel(false);
    let _ = executor.tick_with_clock(stop, Some(at)).await;
}

/// The orchestrator keeps failing a batch with messages always waiting for it.
fn busy(f: &super::testing::Fixture, at: i64) {
    f.executor.domain.tell_orchestrator(&format!("busy-{at}"), None, format!("Work that arrived at {at}")).unwrap();
    let mut runtime = load(&f.executor.home).unwrap();
    runtime.retry_at = Some(i64::MAX);
    runtime.last_error = Some("the batch failed".into());
    save(&f.executor.home, &runtime).unwrap();
}

/// The orchestrator is free again: its next tick runs a batch.
fn free(f: &super::testing::Fixture) {
    let mut runtime = load(&f.executor.home).unwrap();
    runtime.retry_at = None;
    runtime.last_error = None;
    save(&f.executor.home, &runtime).unwrap();
}

#[tokio::test]
async fn a_stall_case_waits_through_a_busy_orchestrator_and_is_settled_after_two_ended_turns() {
    let f = fixture();
    let domain = &f.executor.domain;
    let case = stalled(domain);
    for at in [0, 10 * MINUTE, 21 * MINUTE, HOUR, 24 * HOUR] {
        busy(&f, at);
        tick_at(&f.executor, at).await;
        assert!(!reminded(domain, &case), "a failing orchestrator ended no turn: no reminder at {at}");
    }
    assert!(!instructed(domain, &case, UNSETTLED_INSTRUCT), "a day on the clock settled nothing");
    free(&f);
    f.tick().await;
    assert!(reminded(domain, &case), "the turn that ended with the case unsettled is reminded at once");
    assert!(!instructed(domain, &case, UNSETTLED_INSTRUCT), "the reminder comes first");
    f.tick().await;
    assert!(instructed(domain, &case, UNSETTLED_INSTRUCT), "the node has the default instruction: {:?}", domain.state().unwrap().lifecycle_journal[&task()].last());
    let about_the_stop: Vec<_> = user_cards(domain).into_iter().filter(|card| !card.0.starts_with("request-tasks-")).collect();
    assert!(about_the_stop.is_empty(), "no card asks the user about the stop: {about_the_stop:?}");
}

#[test]
fn after_three_cases_the_task_closes_with_its_result() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = crate::wiring::open(&home).unwrap();
    let mut case = stalled(&domain);
    for n in 1..=STALL_ATTEMPTS {
        as_actor(&domain, Actor::Orchestrator { generation: 0 }, Command::SettleStall {
            case_id: case.clone(), task_id: task(), outcome: StallOutcome::Instruct { text: format!("Instruction {n}") },
        });
        case = stop_until_a_case(&domain);
    }
    assert_eq!(domain.state().unwrap().tasks[&task()].stalls, STALL_ATTEMPTS + 1);
    for at in [0, 10 * MINUTE, 21 * MINUTE, 24 * HOUR] {
        crate::orchestrator_inbox::settle_overdue(&domain, at);
    }
    assert!(domain.state().unwrap().tasks[&task()].stall_closing.is_none(), "the clock closes nothing");
    turn_ends(&domain);
    turn_ends(&domain);
    let state = domain.state().unwrap();
    assert_eq!(state.tasks[&task()].stall_closing, Some(StallClosing::Unsettled), "closed with what it got done");
    assert!(state.tasks[&task()].cancelled(), "{:?}", state.tasks[&task()].state);
    assert!(user_cards(&domain).is_empty());
}
