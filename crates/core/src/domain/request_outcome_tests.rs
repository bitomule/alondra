//! Every request the user made hears how it ended from Alondra, once: its answer when it needed no
//! task, «Done» when its tasks are over. Nobody else's requests get a card, and the orchestrator
//! cannot send a second one.
use super::*;
use crate::request_outcome::DoneCard;
use std::collections::BTreeSet;

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
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
fn state() -> DomainState {
    let mut s = DomainState::new("epoch");
    s
}
fn asked(s: &mut DomainState, actor: Actor, id: &str, maintenance: bool) {
    ok(s, actor, Command::CreateRequest { request_id: id.into(), body: "Several Claude accounts".into(), auto_merge: false, maintenance, message: false });
    ok(s, orchestrator(), Command::TitleRequest { request_id: id.into(), title: "Claude accounts".into() });
}
fn task(s: &mut DomainState, id: &str, request: &str) {
    ok(s, orchestrator(), Command::CreateTask {
        plan_items: None, task_id: id.into(), request_id: request.into(), title: format!("Task {id}"), agent_id: None,
        review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp/project")),
    });
}
fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "The design".into() }]
}
fn done(s: &mut DomainState, task_id: &str, pr: Option<u64>) {
    let node = format!("node-{task_id}");
    ok(s, Actor::System, Command::RegisterAgent { agent_id: node.as_str().into(), name: node.clone(), at: 0 });
    ok(s, orchestrator(), Command::AssignTask { task_id: task_id.into(), agent_id: node.as_str().into(), model: None });
    let id = format!("final-{task_id}");
    ok(s, Actor::Agent { agent_id: node.as_str().into() }, Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: task_id.into(), interaction_id: id.as_str().into(), body: "The design".into(),
        targets: text(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the design", &text(), true),
    });
    ok(s, user(), Command::AcceptDelivery { presentation_id: id.as_str().into(), targets: text() });
    ok(s, Actor::System, Command::RecordNodeEnd { task_id: task_id.into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(s.tasks[&task_id.into()].state, TaskState::Done);
    if let Some(number) = pr {
        merged(s, task_id, number);
    }
}
/// The PR the task's node merged, as Alondra records it once GitHub says so.
fn merged(s: &mut DomainState, task_id: &str, number: u64) {
    let id = PrId::from(format!("pr-{number}"));
    s.pull_requests.insert(id.clone(), PullRequest {
        id: id.clone(), task_id: task_id.into(), host: "github.com".into(), repository: "owner/repo".into(), number,
        head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
        allowed_methods: vec![MergeMethod::Squash], review: None,
    });
    s.tasks.get_mut(&task_id.into()).unwrap().pr_id = Some(id);
}
fn cancel(s: &mut DomainState, task_id: &str) {
    ok(s, user(), Command::CancelTask { task_id: task_id.into() });
}
fn close(s: &mut DomainState, request: &str, answer: Option<&str>, headline: Option<&str>) -> CommandOutcome {
    ok(s, orchestrator(), Command::CloseRequest {
        request_id: request.into(), answer: answer.map(String::from), withdrawn: false, title: None, headline: headline.map(String::from),
    })
}
fn cards(s: &DomainState, request: &str) -> Vec<Interaction> {
    s.interactions.values()
        .filter(|i| i.recipient == Recipient::User && i.scope == Scope::Orchestrator { request_id: Some(request.into()) })
        .cloned()
        .collect()
}
fn the_card(s: &DomainState, request: &str) -> Interaction {
    let cards = cards(s, request);
    assert_eq!(cards.len(), 1, "{cards:?}");
    cards.into_iter().next().unwrap()
}
fn notified(outcome: &CommandOutcome, card: &Interaction) -> bool {
    outcome.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == card.id))
}
fn inform(id: &str, scope: Scope) -> Command {
    Command::PublishInformation {
        attachments: vec![], interaction_id: id.into(), scope, recipient: Recipient::User, body: "Yes, with R2".into(),
        card: CardSpec::new(CardKind::Know, "Images are encrypted with R2"),
    }
}

#[test]
fn an_answer_without_tasks_reaches_the_user_under_its_headline() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    let out = close(&mut s, "r", Some("Yes, since Monday: PR #1159 merged it."), Some("Already in review"));
    let card = the_card(&s, "r");
    assert_eq!((card.author.clone(), card.card.kind), (Author::System, CardKind::Know));
    assert_eq!((card.card.need.as_str(), card.card.why.as_str()), ("Already in review", "Yes, since Monday: PR #1159 merged it."));
    assert!(matches!(card.payload, InteractionPayload::Information { ref read_by } if read_by.is_empty()));
    assert!(notified(&out, &card));
}

#[test]
fn a_long_answer_keeps_its_headline_and_goes_whole_in_the_detail() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    let answer = format!("It is on main.\n\n{}", "Every step of the release is covered. ".repeat(10));
    close(&mut s, "r", Some(&answer), Some("Already on main"));
    let card = the_card(&s, "r");
    assert_eq!(card.card.need, "Already on main");
    assert_eq!(card.card.content.as_ref().map(|c| (c.kind, c.body.as_str())), Some((ContentKind::Report, answer.trim())));
}

#[test]
fn an_answer_without_a_headline_opens_with_its_first_line() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    close(&mut s, "r", Some("Main is green\nThe last run passed at 9:00."), None);
    let card = the_card(&s, "r");
    assert_eq!((card.card.need.as_str(), card.card.why.as_str()), ("Main is green", "The last run passed at 9:00."));
}

#[test]
fn a_request_whose_tasks_are_done_tells_the_user_it_is_done() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    task(&mut s, "design", "r");
    task(&mut s, "build", "r");
    done(&mut s, "design", None);
    done(&mut s, "build", Some(1290));
    let out = close(&mut s, "r", Some("Both accounts work and the switch is in Settings."), None);
    let card = the_card(&s, "r");
    assert_eq!(card.id.0, DoneCard::id("r"));
    assert_eq!((card.author.clone(), card.card.need.as_str()), (Author::System, "Done"));
    assert_eq!(card.card.why, "Both accounts work and the switch is in Settings.");
    let body = DoneCard::from_body(&card.body).unwrap();
    assert_eq!(body.tasks.iter().map(|t| (t.id.as_str(), t.cancelled, t.pr)).collect::<Vec<_>>(),
        [("design", false, None), ("build", false, Some(1290))]);
    assert!(notified(&out, &card));
}

#[test]
fn a_request_with_cancelled_tasks_says_how_much_was_done() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    task(&mut s, "design", "r");
    task(&mut s, "build", "r");
    done(&mut s, "design", None);
    cancel(&mut s, "build");
    close(&mut s, "r", None, None);
    let card = the_card(&s, "r");
    assert_eq!((card.card.need.as_str(), card.card.why.as_str()), ("Done in part: 1 of 2 tasks was cancelled", "All its work is finished."));

    let mut s = state();
    asked(&mut s, user(), "r", false);
    task(&mut s, "design", "r");
    cancel(&mut s, "design");
    close(&mut s, "r", Some("Not needed after all"), None);
    let card = the_card(&s, "r");
    assert_eq!((card.card.need.as_str(), card.card.why.as_str()), ("Not done: its task was cancelled", "Not needed after all"));
}

#[test]
fn only_the_users_own_requests_hear_how_they_ended() {
    for (who, maintenance) in [(orchestrator(), false), (user(), true)] {
        let mut s = state();
        asked(&mut s, who, "r", maintenance);
        task(&mut s, "design", "r");
        done(&mut s, "design", None);
        let out = close(&mut s, "r", Some("Done"), None);
        assert!(cards(&s, "r").is_empty());
        assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "{:?}", out.effects);
    }
    let mut s = state();
    asked(&mut s, user(), "r", false);
    task(&mut s, "design", "r");
    cancel(&mut s, "design");
    ok(&mut s, user(), Command::CloseRequest { request_id: "r".into(), answer: None, withdrawn: true, title: None, headline: None });
    assert!(cards(&s, "r").is_empty(), "the user withdrew it: they know how it ended");
}

#[test]
fn the_orchestrator_cannot_send_its_own_card_about_a_users_request() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    task(&mut s, "design", "r");
    for scope in [Scope::Orchestrator { request_id: Some("r".into()) }, Scope::Task { task_id: "design".into() }] {
        let before = s.clone();
        let error = exec(&mut s, orchestrator(), inform("info", scope)).unwrap_err();
        assert_eq!((error.code.as_str(), error.subject.as_deref()), ("outcome_card", Some("scope")));
        assert_eq!(s, before);
    }
    asked(&mut s, orchestrator(), "mine", false);
    ok(&mut s, orchestrator(), inform("about-mine", Scope::Orchestrator { request_id: Some("mine".into()) }));
    ok(&mut s, orchestrator(), inform("general", Scope::Orchestrator { request_id: None }));
    ok(&mut s, Actor::System, inform("system", Scope::Task { task_id: "design".into() }));
}

#[test]
fn a_card_the_orchestrator_sent_before_the_upgrade_is_not_repeated() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    let scope = Scope::Orchestrator { request_id: Some("r".into()) };
    s.interactions.insert("old".into(), Interaction {
        attachments: vec![], id: "old".into(), scope, author: Author::Orchestrator { generation: 0 }, recipient: Recipient::User,
        body: "Yes, with R2".into(), blocking: false, subject: None,
        payload: InteractionPayload::Information { read_by: BTreeSet::new() }, revision: s.revision, order: s.revision,
        withdrawn: false, card: CardSpec::new(CardKind::Know, "Images are encrypted with R2"), clarification: None, snoozed_until: None, resurfaced_at_revision: None,
    });
    close(&mut s, "r", Some("Yes, with R2"), None);
    assert_eq!(the_card(&s, "r").id.0, "old");
}
