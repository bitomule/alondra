//! What the clients show of a card follows its type's contract: a result asks to review it, the
//! button that closes the task says so, and a card that is only read never counts as waiting.
use super::*;
use crate::domain::{
    Actor, CardKind, CardSpec, Command, CommandEnvelope, ContentKind, DeliveryTarget, DomainState, LaunchPlan, inbox_items,
};
use std::collections::BTreeSet;

fn ok(s: &mut DomainState, a: Actor, c: Command) {
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    };
    s.execute(&a, &envelope).unwrap();
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node A".into(), at: 0 });
    ok(&mut s, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Build it".into(), agent_id: Some("a".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    s
}
fn present(s: &mut DomainState, need: &str) {
    let card = CardSpec { need: need.into(), ..CardSpec::new(CardKind::Review, "The new tagline, merged in the web repository").content(ContentKind::Report, "Merged.") };
    ok(s, Actor::Agent { agent_id: "a".into() }, Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "result".into(),
        body: "Merged.".into(), targets: vec![DeliveryTarget::Text { text: "Merged".into() }],
        is_final: true, nothing_to_review: false, card,
    });
}
fn context<'a>(domain: &'a DomainState, index: &'a crate::domain::ReadIndex, live: &'a LiveSnapshot, client: Client) -> RenderContext<'a> {
    RenderContext { domain, index, live, client, lang: Lang::Es, transport: Transport::Loopback, cursor: "cursor-1", rendered_at: "" }
}
fn card_of(domain: &DomainState, id: &str) -> Card {
    let index = crate::domain::ReadIndex::build(domain);
    let live = LiveSnapshot::default();
    let item = inbox_items(domain).into_iter().find(|item| item.id == format!("interaction:{id}")).expect("the card is in the inbox");
    card::from_item(&item, &context(domain, &index, &live, Client::Ios), true).unwrap()
}

/// Case 6 of the audit: «Dar la tarea por hecha» with a «Visto» button.
#[test]
fn the_button_that_closes_the_task_says_it_does() {
    let mut s = fixture();
    present(&mut s, "Mark the task as done");
    let card = card_of(&s, "result");
    let labels: Vec<&str> = card.actions.iter().map(|action| action.label.as_str()).collect();
    assert!(labels.contains(&"Dar por hecha"), "{labels:?}");
    assert!(!labels.contains(&"Visto"), "{labels:?}");
}

/// Case 1 of the audit, as stored before results had their type's need: the card asks to review
/// it, never for the decision its author wrote and no button takes.
#[test]
fn a_result_stored_with_a_decision_in_its_need_asks_to_review_it() {
    let mut s = fixture();
    present(&mut s, "Review it");
    s.interactions.get_mut(&"result".into()).unwrap().card.need = "Decide whether you post the app in developer forums".into();
    assert_eq!(card_of(&s, "result").need.as_deref(), Some("Revisa el resultado"));
}

/// Case 3 of the audit: seven «He creado la tarea…» cards counted on the icon and in «Te espera».
#[test]
fn a_card_that_is_only_read_is_listed_apart_and_never_counts_on_the_icon() {
    let mut s = fixture();
    ok(&mut s, Actor::System, Command::AnnounceRequestTasks);
    let index = crate::domain::ReadIndex::build(&s);
    assert_eq!(index.waiting().len(), 1, "the notice is still in the inbox to read");
    assert_eq!(inbox_badge(&index), 0);
    let live = LiveSnapshot::default();
    let tui = build("home", context(&s, &index, &live, Client::Tui)).unwrap();
    let sections: Vec<(&str, Option<u32>)> = tui.sections.iter().map(|section| (section.id.as_str(), section.count)).collect();
    assert_eq!(sections[0], ("to_read", Some(1)), "{sections:?}");
    assert!(sections.iter().all(|(id, _)| *id != "waiting"), "{sections:?}");
    let ios = build("home", context(&s, &index, &live, Client::Ios)).unwrap();
    let queue = &ios.sections[0];
    assert_eq!((queue.count, queue.card_ids.len(), queue.card_waits.as_slice()), (Some(0), 1, ["read".to_owned()].as_slice()));
    assert_eq!(ios.status.badge, 0);

    present(&mut s, "Review it");
    let index = crate::domain::ReadIndex::build(&s);
    assert_eq!(inbox_badge(&index), 1, "the result asks for something");
}
