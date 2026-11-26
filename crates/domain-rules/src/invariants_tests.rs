//! The invariants each name the way a state breaks them.
use alondra_domain_model::invariants::violations;
use super::lifecycle_cache_tests::{run, three_tasks, Step};
use super::sdk_node_tests::SDK;
use super::*;

fn exec(s: &mut DomainState, a: Actor, c: Command) {
    s.execute(&a, &CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    })
    .unwrap();
}

fn with_a_task() -> DomainState {
    let mut s = DomainState::new("epoch");
    exec(&mut s, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    exec(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Instance a".into(), at: 0 });
    exec(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Task t".into(), agent_id: Some("a".into()),
        review_prs: false, dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    s
}

fn ask(s: &mut DomainState) {
    exec(s, Actor::Agent { agent_id: "a".into() }, Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User,
        body: "Which name?".into(), blocking: true,
        options: vec![QuestionOption { id: "a".into(), label: "Atlas".into(), description: None }, QuestionOption { id: "b".into(), label: "Borealis".into(), description: None }],
        card: CardSpec::new(CardKind::Decide, "Choose a name").needing("Choose a name").why("The name goes on the release"),
    });
}

#[test]
fn a_state_the_domain_built_breaks_none() {
    let mut s = with_a_task();
    ask(&mut s);
    assert_eq!(violations(&s), Vec::<String>::new());
}

#[test]
fn a_live_task_nobody_moves_is_named() {
    let mut s = with_a_task();
    s.lifecycle_journal.remove(&"t".into());
    assert_eq!(violations(&s), vec!["task t is NotStarted and nobody owes its next step".to_owned()]);
}

#[test]
fn an_ended_task_whose_lifecycle_still_owes_and_its_open_question_are_named() {
    let mut s = with_a_task();
    ask(&mut s);
    s.tasks.get_mut(&"t".into()).unwrap().state = TaskState::Cancelled;
    let found = violations(&s);
    assert!(found.iter().any(|why| why.starts_with("task t ended as Cancelled but its lifecycle still owes")), "{found:?}");
    assert!(found.contains(&"card q still asks for an answer for task t, which ended".to_owned()), "{found:?}");
}

#[test]
fn an_answer_given_again_changes_nothing() {
    let mut s = with_a_task();
    ask(&mut s);
    let answer = |option: &str| Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(), option_id: option.into(), inputs: Default::default() };
    exec(&mut s, Actor::User { user_id: "u".into() }, answer("a"));
    let answered = s.clone();
    for option in ["a", "b"] {
        let again = s.execute(&Actor::User { user_id: "u".into() }, &CommandEnvelope {
            protocol_version: 2, data_epoch: s.data_epoch.clone(), command_id: format!("again-{option}").into(),
            expected_revision: s.revision, command: answer(option),
        });
        assert!(again.is_err(), "answering {option} again was accepted");
        assert_eq!(s, answered);
    }
}

#[test]
fn a_delivered_task_whose_delivery_card_is_gone_is_named() {
    let mut s = three_tasks();
    run(&mut s, &Step::Deliver(2), 1).unwrap();
    run(&mut s, &Step::TurnEnded, 2).unwrap();
    let task = TaskId::from(SDK);
    assert_eq!(s.obligation_of(&task).map(|obligation| obligation.party), Some(alondra_domain::Party::User), "the delivery waits on the user");
    assert_eq!(violations(&s), Vec::<String>::new());
    let delivery = s.tasks[&task].final_presentation.clone().expect("a delivery");
    s.interactions.get_mut(&delivery).unwrap().withdrawn = true;
    let found = violations(&s);
    assert!(found.contains(&format!("task {SDK} stands delivered on card {delivery}, which is gone")), "{found:?}");
    assert!(found.contains(&format!("task {SDK} waits on the user and no card asks them anything")), "{found:?}");
}
