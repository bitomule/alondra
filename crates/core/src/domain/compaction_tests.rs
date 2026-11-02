//! The cards of a done or cancelled Task keep what is still read of them and let go of the rest,
//! and a Task reopened after that works as before.
use super::*;
use std::collections::BTreeSet;

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}
fn agent() -> Actor {
    Actor::Agent { agent_id: "a".into() }
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    };
    s.execute(&a, &envelope)
}
fn ok(s: &mut DomainState, a: Actor, c: Command) -> CommandOutcome {
    exec(s, a, c).unwrap()
}
fn task(id: &str, dependencies: &[&str]) -> Command {
    Command::CreateTask {
        plan_items: None, task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None,
        review_prs: false, dependencies: dependencies.iter().map(|d| TaskId::from(*d)).collect::<BTreeSet<_>>(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp/project")),
    }
}
fn long(head: &str) -> String {
    format!("{head}\n\n{}", "What the node measured, line after line of it. ".repeat(200))
}
fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "The report".into() }]
}
fn compact(s: &mut DomainState, tasks: &[&str]) -> Result<CommandOutcome, DomainError> {
    exec(s, Actor::System, Command::CompactEndedTasks { task_ids: tasks.iter().map(|t| TaskId::from(*t)).collect() })
}
fn card<'s>(s: &'s DomainState, id: &str) -> &'s Interaction {
    &s.interactions[&InteractionId::from(id)]
}

fn working() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node".into(), at: 0 });
    ok(&mut s, orchestrator(), Command::CreateRequest { request_id: "r".into(), body: "Make it smaller".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Smaller store".into() });
    ok(&mut s, orchestrator(), task("t", &[]));
    ok(&mut s, orchestrator(), Command::AssignTask { task_id: "t".into(), agent_id: "a".into(), model: None });
    let scope = Scope::Task { task_id: "t".into() };
    ok(&mut s, agent(), Command::AskQuestion {
        attachments: vec![], interaction_id: "ask".into(), scope: scope.clone(), recipient: Recipient::User, body: long("Which table goes first?"),
        blocking: true,
        options: vec![
            QuestionOption { id: "a".into(), label: "Cards".into(), description: Some(long("The cards weigh most")) },
            QuestionOption { id: "b".into(), label: "Inputs".into(), description: Some("They are small".into()) },
        ],
        card: CardSpec::new(CardKind::Decide, "Choose the table to compact first").why("The cards weigh most").content(ContentKind::Options, long("Both, measured")),
    });
    ok(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "ask".into(), option_id: "a".into(), inputs: Default::default() });
    for id in ["read", "unread"] {
        ok(&mut s, orchestrator(), Command::PublishInformation {
            attachments: vec![], interaction_id: id.into(), scope: scope.clone(), recipient: Recipient::User, body: long(&format!("What I found, {id}")),
            card: CardSpec::new(CardKind::Know, format!("Read what the copy measured, {id}")).content(ContentKind::Report, long("The numbers")),
        });
    }
    ok(&mut s, user(), Command::MarkRead { interaction_id: "read".into() });
    let mut delivery = CardSpec::delivery("Review the smaller store", &text(), true);
    delivery.content = Some(CardContent { kind: ContentKind::Report, summary: Some("Smaller".into()), body: long("# Report") });
    ok(&mut s, agent(), Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: "t".into(), interaction_id: "final".into(), body: long("The store is smaller"),
        targets: text(), is_final: true, nothing_to_review: false, card: delivery,
    });
    s
}
fn done() -> DomainState {
    let mut s = working();
    ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: text() });
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
    s
}

/// What is still read of a card once its task ended.
fn read(i: &Interaction) -> (String, String, String, Option<String>, Option<String>, String, Vec<(String, String)>, Option<Answer>) {
    let options = match &i.payload {
        InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => {
            options.iter().map(|o| (o.id.clone(), o.label.clone())).collect()
        }
        _ => vec![],
    };
    let answer = match &i.payload {
        InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. } => answer.clone(),
        _ => None,
    };
    (body_title(&i.body), i.card.need.clone(), i.card.why.clone(), i.card.plan.clone(), i.card.kept_at.clone(),
        format!("{:?}", i.card.content.as_ref().map(|c| (c.kind, c.summary.clone()))), options, answer)
}

#[test]
fn an_ended_tasks_cards_keep_what_is_read_and_let_go_of_the_rest() {
    let mut s = done();
    let before = s.clone();
    assert_eq!(s.compactable_tasks(10), [TaskId::from("t")]);
    let outcome = compact(&mut s, &["t"]).unwrap();
    assert!(outcome.changed);
    assert_eq!(s.tasks[&"t".into()].revision, before.tasks[&"t".into()].revision, "the buttons a screen shows of the task stay valid");
    for (id, was) in &before.interactions {
        assert_eq!(read(&s.interactions[id]), read(was), "{id} reads the same");
        assert_eq!((s.interactions[id].revision, s.interactions[id].order), (was.revision, was.order), "{id} keeps its time and place");
    }
    assert_eq!(result_excerpt(&card(&s, "final").body), result_excerpt(&card(&before, "final").body), "a task that builds on it reads the same result");
    assert!(card(&s, "final").body.chars().count() <= RESULT_LIMIT + 12);
    assert!(card(&before, "final").body.chars().count() > RESULT_LIMIT + 12);
    assert!(card(&s, "final").shortened && !card(&before, "final").shortened, "the cut result says it was cut");
    assert_eq!(card(&s, "ask").body, "Which table goes first?");
    assert_eq!(card(&s, "read").body, "What I found, read");
    for id in ["final", "ask", "read"] {
        assert_eq!(card(&s, id).card.content.as_ref().map(|c| c.body.as_str()), Some(""), "{id} lets go of its content");
    }
    let InteractionPayload::Question { options, .. } = &card(&s, "ask").payload else { panic!("question") };
    assert!(options.iter().all(|o| o.description.is_none()));
    assert_eq!(card(&s, "unread"), card(&before, "unread"), "a notice nobody read still waits for the user whole");
    assert!(s.compactable_tasks(10).is_empty());
    assert!(!compact(&mut s, &["t"]).unwrap().changed, "compacting twice changes nothing");
}

#[test]
fn only_alondra_compacts_and_only_ended_tasks() {
    let mut s = working();
    assert!(s.compactable_tasks(10).is_empty(), "an open task is never compactable");
    let before = s.clone();
    assert_eq!(compact(&mut s, &["t"]).unwrap_err().code, "conflict");
    assert_eq!(s, before);
    let mut s = done();
    let before = s.clone();
    for actor in [user(), agent(), orchestrator()] {
        assert_eq!(exec(&mut s, actor, Command::CompactEndedTasks { task_ids: vec!["t".into()] }).unwrap_err().code, "forbidden");
    }
    assert_eq!(compact(&mut s, &[]).unwrap_err().code, "invalid_input");
    assert_eq!(s, before);
}

#[test]
fn a_cancelled_tasks_cards_are_compacted_too() {
    let mut s = working();
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Cancelled);
    assert_eq!(s.compactable_tasks(10), [TaskId::from("t")]);
    compact(&mut s, &["t"]).unwrap();
    assert_eq!(card(&s, "ask").body, "Which table goes first?");
}

#[test]
fn a_reopened_task_after_compaction_waits_on_no_old_card_and_delivers_again() {
    let mut s = done();
    ok(&mut s, orchestrator(), task("follow", &["t"]));
    compact(&mut s, &["t"]).unwrap();
    let compacted = s.clone();
    ok(&mut s, user(), Command::ReopenTask { task_id: "t".into() });
    assert!(!s.tasks[&"t".into()].state.terminal());
    assert!(s.compactable_tasks(10).is_empty(), "a reopened task is not compacted while it runs");
    assert!(inbox_items(&s).iter().filter(|item| item.pending).all(|item| !["ask", "read", "final"].contains(&item.id.as_str())),
        "no compacted card waits for the user again");
    ok(&mut s, orchestrator(), Command::AssignTask { task_id: "t".into(), agent_id: "a".into(), model: None });
    ok(&mut s, agent(), Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: "t".into(), interaction_id: "again".into(), body: long("Smaller again"),
        targets: text(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review it again", &text(), true),
    });
    ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "again".into(), targets: text() });
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
    assert_eq!(s.compactable_tasks(10), [TaskId::from("t")]);
    compact(&mut s, &["t"]).unwrap();
    assert_eq!(result_excerpt(&card(&s, "again").body), result_excerpt(&long("Smaller again")));
    for id in ["ask", "read", "unread"] {
        assert_eq!(card(&s, id).body, card(&compacted, id).body, "{id} was already compacted");
    }
}

#[test]
fn a_plan_another_task_implements_is_kept_whole() {
    const PLAN: &str = "# Smaller\n\n## Phase 1\n- [ ] Cards\n\n## Phase 2\nDepends on: phase 1\n- [ ] Inputs\n";
    let mut s = working();
    ok(&mut s, Actor::System, Command::HoldHook {
        interaction_id: "plan".into(), task_id: "t".into(), agent_id: "a".into(), hold: Hold::Plan,
        body: PLAN.into(), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, "Smaller").content(ContentKind::Plan, PLAN),
    });
    ok(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "plan".into(), option_id: Hold::APPROVE.into(), inputs: Default::default() });
    ok(&mut s, orchestrator(), Command::CreateTask {
        plan_items: Some(PlanItems { from_task: "t".into(), items: vec!["2".into()] }), task_id: "phase".into(), request_id: "r".into(),
        title: "Phase 2".into(), agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp/project")),
    });
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    let before = card(&s, "plan").clone();
    compact(&mut s, &["t"]).unwrap();
    assert_eq!(card(&s, "plan"), &before, "the phase still reads the plan it implements");
    assert_eq!(card(&s, "ask").body, "Which table goes first?");
}
