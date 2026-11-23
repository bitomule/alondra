//! The card protocol rules of the state audit's group G6 (F-CARDS-2 to F-CARDS-5), each test
//! the exact sequence of its finding in `docs/architecture/state-machines.md`.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}
fn agent(id: &str) -> Actor {
    Actor::Agent { agent_id: id.into() }
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&a, &CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    })
}
fn ok(s: &mut DomainState, a: Actor, c: Command) -> CommandOutcome {
    exec(s, a, c).unwrap()
}
fn refused(s: &mut DomainState, a: Actor, c: Command) -> DomainError {
    let before = s.clone();
    let error = exec(s, a, c).unwrap_err();
    assert_eq!(*s, before, "a refused command changed state");
    error
}
fn task(s: &mut DomainState, id: &str, agent: &str) {
    ok(s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: format!("Instance {agent}"), at: 0 });
    ok(s, orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: Some(agent.into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::LaunchPlan::new("node", "/tmp")),
    });
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    s.requests.get_mut(&"r".into()).unwrap().title = Some("Build it".into());
    task(&mut s, "t", "a");
    s
}
fn option(id: &str, label: &str) -> QuestionOption {
    QuestionOption { id: id.into(), label: label.into(), description: None }
}
fn decide(need: &str) -> CardSpec {
    CardSpec::new(CardKind::Decide, need).needing(need).why("The name goes on the release")
}
fn ask(id: &str, to: Recipient, card: CardSpec) -> Command {
    Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() }, recipient: to,
        body: "Which name?".into(), blocking: false, options: vec![option("a", "Atlas"), option("b", "Borealis")], card,
    }
}
fn gq(need: &str) -> GroupQuestion {
    GroupQuestion {
        title: Some("Release choices".into()), need: need.into(), why: "The release needs it".into(), recommended: None,
        options: vec![option("o0", "Yes"), option("o1", "No")],
    }
}
fn asks(group: &str) -> Command {
    Command::AskQuestions {
        interaction_id: group.into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User, blocking: true,
        why: "Three things for the release".into(), questions: vec![gq("Ship the Mac app?"), gq("Ship it today?"), gq("Ship to beta?")], language: None,
    }
}
fn answer(card: &str) -> Command {
    Command::AnswerQuestion { attachments: vec![], interaction_id: card.into(), option_id: "o0".into(), inputs: BTreeMap::new() }
}
fn text(words: &str) -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: words.into() }]
}
fn present(task: &str, id: &str, targets: Vec<DeliveryTarget>, is_final: bool, replaces: Option<&str>) -> Command {
    let mut card = CardSpec::delivery("Result ready", &targets, is_final);
    card.replaces = replaces.map(Into::into);
    Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: task.into(), interaction_id: id.into(), body: "Result ready".into(),
        targets, is_final, nothing_to_review: false, card,
    }
}
fn handed_back(s: &DomainState, task: &str, delivery: &str) -> bool {
    s.lifecycle_journal[&TaskId::from(task)].iter().any(|record| matches!(record,
        LifecycleRecord::HandedBack { delivery: d, .. } if d.0 == delivery))
}
/// The delivery left the user's queue and the task is in progress again, as `Withdraw` leaves it.
fn back_in_progress(s: &DomainState, task: &str, delivery: &str) {
    let t = &s.tasks[&TaskId::from(task)];
    assert_eq!(t.final_presentation, None, "no final delivery stands for the task");
    assert!(s.interactions[&InteractionId::from(delivery)].withdrawn);
    assert!(handed_back(s, task, delivery), "the lifecycle took the delivery back");
    assert_ne!(t.state, TaskState::ForYourReview, "the task no longer waits on a card the user cannot see");
    s.validate().unwrap();
}

#[test]
fn replacing_one_question_of_a_group_is_refused_and_the_group_is_replaced_whole() {
    let mut s = fixture();
    ok(&mut s, agent("a"), asks("g"));
    let mut again = decide("Ship only the Mac app?");
    again.replaces = Some("g-q1".into());
    let refusal = refused(&mut s, agent("a"), ask("again", Recipient::User, again));
    assert_eq!((refusal.code.as_str(), refusal.subject.as_deref()), ("question_in_group", Some("card.replaces")));
    assert!(refusal.message.contains("withdraw"), "the refusal says how to replace the group: {}", refusal.message);

    ok(&mut s, agent("a"), Command::Withdraw { interaction_id: "g-q1".into() });
    ok(&mut s, agent("a"), asks("h"));
    let told = |outcome: &CommandOutcome| outcome.effects.iter().filter(|effect| matches!(effect, DomainEffect::NotifyAgent { .. })).count();
    assert_eq!(told(&ok(&mut s, user(), answer("h-q1"))) + told(&ok(&mut s, user(), answer("h-q2"))), 0);
    assert_eq!(told(&ok(&mut s, user(), answer("h-q3"))), 1, "the new group's answers reach the node together");
}

#[test]
fn a_progress_note_that_replaces_the_final_delivery_hands_it_back() {
    let mut s = fixture();
    ok(&mut s, agent("a"), present("t", "d1", text("Report"), true, None));
    ok(&mut s, user(), Command::RequestClarification {
        interaction_id: "d1".into(), reason: ClarificationReason::Unclear, text: Some("What changed?".into()),
    });
    ok(&mut s, agent("a"), present("t", "note", text("The parser now keeps comments"), false, Some("d1")));
    back_in_progress(&s, "t", "d1");
}

#[test]
fn a_question_to_the_orchestrator_that_replaces_the_final_delivery_hands_it_back() {
    let mut s = fixture();
    ok(&mut s, agent("a"), present("t", "d1", text("Report"), true, None));
    ok(&mut s, agent("a"), ask("which", Recipient::Orchestrator, {
        let mut card = decide("Pick the release name");
        card.replaces = Some("d1".into());
        card
    }));
    back_in_progress(&s, "t", "d1");
}

#[test]
fn a_withdrawn_delivery_cannot_be_accepted() {
    let mut s = fixture();
    ok(&mut s, agent("a"), present("t", "d1", text("Report"), true, None));
    s.interactions.get_mut(&InteractionId::from("d1")).unwrap().withdrawn = true;
    let refusal = refused(&mut s, user(), Command::AcceptDelivery { presentation_id: "d1".into(), targets: text("Report") });
    assert_eq!(refusal.code, "acceptance_stale");
}

fn document(s: &mut DomainState) {
    ok(s, agent("a"), Command::CreateDocument {
        task_id: "t".into(), document_id: "doc".into(), title: "Design".into(), revision_id: "v1".into(),
        content_digest: "a".repeat(64), content_reference: "files/design".into(), role: "result".into(),
    });
}
fn revise(task: &str) -> Command {
    Command::ReviseDocument {
        task_id: task.into(), document_id: "doc".into(), revision_id: "v2".into(),
        content_digest: "b".repeat(64), content_reference: "files/design-2".into(),
    }
}
fn revision_one() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::DocumentRevision { document_id: "doc".into(), revision_id: "v1".into() }]
}
fn told(outcome: &CommandOutcome, agent: &str) -> Vec<String> {
    outcome.effects.iter().filter_map(|effect| match effect {
        DomainEffect::NotifyAgent { agent_id, message, .. } if agent_id.0 == agent => Some(message.clone()),
        _ => None,
    }).collect()
}

#[test]
fn a_new_revision_of_a_shared_document_hands_back_the_other_tasks_delivery_and_tells_its_node() {
    let mut s = fixture();
    task(&mut s, "t2", "b");
    document(&mut s);
    ok(&mut s, user(), Command::LinkTaskDocument { task_id: "t2".into(), document_id: "doc".into(), role: "shared".into() });
    ok(&mut s, agent("b"), present("t2", "d2", revision_one(), true, None));
    let revised = ok(&mut s, agent("a"), revise("t"));
    back_in_progress(&s, "t2", "d2");
    assert!(told(&revised, "b").iter().any(|message| message.contains("Design") && message.contains("present")),
        "the other task's node learns why its delivery came back: {:?}", revised.effects);
}

#[test]
fn a_node_that_revises_its_delivered_document_gets_its_delivery_back() {
    let mut s = fixture();
    document(&mut s);
    ok(&mut s, agent("a"), present("t", "d1", revision_one(), true, None));
    ok(&mut s, agent("a"), revise("t"));
    back_in_progress(&s, "t", "d1");
    let current = vec![DeliveryTarget::DocumentRevision { document_id: "doc".into(), revision_id: "v2".into() }];
    ok(&mut s, agent("a"), present("t", "d2", current.clone(), true, None));
    ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "d2".into(), targets: current });
}

const PLAN: &str = "# Index the library\n\n- [ ] Store feature prints\n- [ ] Index changes in the background\n";
fn approved_plan() -> DomainState {
    let mut s = fixture();
    let targets = text("Report");
    let mut card = CardSpec::delivery("Viable: index once and follow changes", &targets, true);
    card.plan = Some(PLAN.into());
    ok(&mut s, agent("a"), Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: "t".into(), interaction_id: "report".into(),
        body: "Viable: index once and follow changes".into(), targets, is_final: true, nothing_to_review: false, card,
    });
    ok(&mut s, user(), Command::DecideProposedPlan { presentation_id: "report".into(), targets: text("Report"), choice: PlanChoice::Implement });
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "implement-report".into(), title: "Index the library".into() });
    ok(&mut s, orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: "build".into(), request_id: "implement-report".into(), title: "Build the index".into(), agent_id: None,
        review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(crate::LaunchPlan::new("node", "/tmp")),
    });
    assert!(!s.tasks[&TaskId::from("t")].state.terminal(), "the research node still cleans up");
    s
}
fn feedback(instruction: Instruction, body: &str) -> Command {
    Command::SubmitFeedback {
        attachments: vec![], presentation_id: "report".into(), targets: text("Report"), body: body.into(), instruction, merge_failure: None,
    }
}
fn accepted(s: &DomainState) -> bool {
    matches!(&s.interactions[&InteractionId::from("report")].payload, InteractionPayload::Delivery { acceptance: Some(a), .. } if a.valid)
}
/// The approved plan stays approved and its node is not sent back to rewrite it.
fn plan_untouched(before: &DomainState, s: &DomainState, outcome: &CommandOutcome) {
    assert!(accepted(s), "the approved plan stays approved");
    assert!(told(outcome, "a").is_empty(), "the research node is not sent back to work: {:?}", outcome.effects);
    let research = TaskId::from("t");
    assert_eq!(s.tasks[&research].wrap_up, before.tasks[&research].wrap_up, "the node goes on winding down as before");
    assert_eq!(s.lifecycle_journal.get(&research), before.lifecycle_journal.get(&research), "nothing is handed back to the research node");
    assert_eq!(s.request_plans.len(), 1, "one build of the plan");
    s.validate().unwrap();
}
fn new_request<'a>(s: &'a DomainState, outcome: &CommandOutcome) -> &'a Request {
    let id = outcome.created_ids.iter().find(|id| s.requests.contains_key(&RequestId::from(id.as_str())))
        .unwrap_or_else(|| panic!("the feedback became a request: {:?}", outcome.created_ids));
    let request = &s.requests[&RequestId::from(id.as_str())];
    assert!(outcome.effects.contains(&DomainEffect::NotifyOrchestrator { interaction_id: None, request_id: Some(request.id.clone()) }),
        "the orchestrator gets it: {:?}", outcome.effects);
    let reply_to = request.reply_to.as_ref().expect("it names the approved plan");
    assert_eq!((reply_to.interaction_id.0.as_str(), reply_to.request_id.as_ref().map(|r| r.0.as_str())), ("report", Some("implement-report")),
        "it points at the build in progress");
    assert_eq!(reply_to.task_ids, [TaskId::from("build")]);
    request
}

#[test]
fn requesting_changes_on_an_approved_plan_becomes_a_new_request() {
    let mut s = approved_plan();
    let before = s.clone();
    let outcome = ok(&mut s, user(), feedback(Instruction::Changes, "Index photos only"));
    plan_untouched(&before, &s, &outcome);
    let request = new_request(&s, &outcome);
    assert_eq!((request.body.as_str(), request.message), ("Index photos only", false));
}

#[test]
fn replying_on_an_approved_plan_reaches_the_orchestrator_as_a_note_on_the_build() {
    let mut s = approved_plan();
    for (instruction, body) in [(Instruction::Reply, "Start with the photos"), (Instruction::Question, "Does it cover videos?")] {
        let before = s.clone();
        let outcome = ok(&mut s, user(), feedback(instruction, body));
        plan_untouched(&before, &s, &outcome);
        let request = new_request(&s, &outcome);
        assert_eq!((request.body.as_str(), request.message), (body, true));
    }
}

