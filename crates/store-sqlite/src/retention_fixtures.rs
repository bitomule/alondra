//! A store with one done task whose cards are worth compacting, for the tests of retention here
//! and of the engine's commands that race with it.
use crate::DomainStore;
use crate::tests::{Fixture, seeded};
use alondra_domain_rules::*;

pub fn envelope(store: &mut DomainStore, id: &str, command: Command) -> CommandEnvelope {
    let state = store.load().unwrap();
    CommandEnvelope { protocol_version: 2, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command }
}

pub fn agent() -> Actor {
    Actor::Agent { agent_id: "agent-1".into() }
}

pub fn long(head: &str) -> String {
    format!("{head}\n\n{}", "What the node measured, line after line of it. ".repeat(300))
}

pub fn ended_task(fixture: &Fixture) -> DomainStore {
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let user = Actor::User { user_id: "user-1".into() };
    let ask = Command::AskQuestion {
        attachments: vec![], interaction_id: "ask-1".into(), scope: Scope::Task { task_id: "task-1".into() }, recipient: Recipient::User,
        body: long("Which table goes first?"), blocking: true,
        options: vec![
            QuestionOption { id: "cards".into(), label: "Cards".into(), description: Some(long("They weigh most")) },
            QuestionOption { id: "inputs".into(), label: "Inputs".into(), description: None },
        ],
        card: CardSpec::new(CardKind::Decide, "Choose the table to compact first").why("The cards weigh most").content(ContentKind::Options, long("Measured")),
    };
    let answer = Command::AnswerQuestion { attachments: vec![], interaction_id: "ask-1".into(), option_id: "cards".into(), inputs: Default::default() };
    let targets = vec![DeliveryTarget::Text { text: "The answer".into() }];
    let mut card = CardSpec::delivery("Review the result", &targets, true);
    card.content = Some(CardContent { kind: ContentKind::Report, summary: None, body: long("# Report") });
    let present = Command::PresentResult {
        checklist: vec![], task_id: "task-1".into(), interaction_id: "delivery-1".into(), body: long("Finished the answer"),
        targets: targets.clone(), is_final: true, nothing_to_review: false, attachments: vec![], card,
    };
    let accept = Command::AcceptDelivery { presentation_id: "delivery-1".into(), targets };
    let end = Command::RecordNodeEnd { task_id: "task-1".into(), end: NodeEnd::Closed, agent_id: None };
    let mut steps = vec![("ask".to_owned(), agent(), ask), ("answer".into(), user.clone(), answer)];
    for n in 0..60 {
        steps.push((format!("notice-{n}"), Actor::Orchestrator { generation: 0 }, Command::PublishInformation {
            attachments: vec![], interaction_id: format!("notice-{n}").into(), scope: Scope::Task { task_id: "task-1".into() }, recipient: Recipient::User,
            body: format!("Notice {n}\n\n{}", "A few lines the node measured. ".repeat(40)), card: CardSpec::new(CardKind::Know, format!("Read notice {n}")),
        }));
        steps.push((format!("read-{n}"), user.clone(), Command::MarkRead { interaction_id: format!("notice-{n}").into() }));
    }
    steps.extend([("present".into(), agent(), present), ("accept".into(), user, accept), ("end".into(), Actor::System, end)]);
    for (id, actor, command) in steps {
        let e = envelope(&mut store, &id, command);
        store.execute_authenticated(&actor, &e, None).unwrap();
    }
    assert_eq!(store.load().unwrap().tasks[&TaskId::from("task-1")].state, TaskState::Done);
    store
}
