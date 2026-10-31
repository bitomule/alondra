//! Every request the user made hears how it ended from Alondra, once: its answer when it needed no
//! task, «Done» when its tasks are over. Nobody else's requests get a card, and the orchestrator
//! cannot send a second one.
use super::*;
use crate::request_outcome::{DoneCard, TasksCard};
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
        plan_items: None, continues: None, task_id: id.into(), request_id: request.into(), title: format!("Task {id}"), agent_id: None,
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
    assert_eq!((card.card.effective_title(), card.card.why.as_str()), ("Already in review", "Yes, since Monday: PR #1159 merged it."));
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
    assert_eq!(card.card.effective_title(), "Already on main");
    assert_eq!(card.card.content.as_ref().map(|c| (c.kind, c.body.as_str())), Some((ContentKind::Report, answer.trim())));
}

#[test]
fn an_answer_without_a_headline_uses_request_metadata_and_keeps_the_body() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    close(&mut s, "r", Some("Main is green\nThe last run passed at 9:00."), None);
    let card = the_card(&s, "r");
    assert_eq!((card.card.effective_title(), card.card.why.as_str()), ("Claude accounts", ""));
    assert_eq!(card.card.content.as_ref().unwrap().body, "Main is green\nThe last run passed at 9:00.");
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
    assert_eq!((card.author.clone(), card.card.effective_title()), (Author::System, "Done"));
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
    assert_eq!((card.card.effective_title(), card.card.why.as_str()), ("Done in part: 1 of 2 tasks was cancelled", "All its work is finished."));

    let mut s = state();
    asked(&mut s, user(), "r", false);
    task(&mut s, "design", "r");
    cancel(&mut s, "design");
    close(&mut s, "r", Some("Not needed after all"), None);
    let card = the_card(&s, "r");
    assert_eq!((card.card.effective_title(), card.card.why.as_str()), ("Not done: its task was cancelled", "Not needed after all"));
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
    s.interactions.insert("old".into(), Interaction { system_card: None,
        attachments: vec![], id: "old".into(), scope, author: Author::Orchestrator { generation: 0 }, recipient: Recipient::User,
        body: "Yes, with R2".into(), blocking: false, subject: None,
        payload: InteractionPayload::Information { read_by: BTreeSet::new() }, revision: s.revision, order: s.revision,
        withdrawn: false, card: CardSpec::new(CardKind::Know, "Images are encrypted with R2"), clarification: None, snoozed_until: None, resurfaced_at_revision: None, shortened: false,
    });
    close(&mut s, "r", Some("Yes, with R2"), None);
    assert_eq!(the_card(&s, "r").id.0, "old");
}

fn planned(s: &mut DomainState, id: &str, request: &str, after: &[&str]) {
    ok(s, orchestrator(), Command::CreateTask {
        plan_items: None, continues: None, task_id: id.into(), request_id: request.into(), title: format!("Task {id}"), agent_id: None,
        review_prs: false, dependencies: after.iter().map(|d| TaskId::from(*d)).collect(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp/project")),
    });
}
fn announce(s: &mut DomainState) -> CommandOutcome {
    ok(s, Actor::System, Command::AnnounceRequestTasks)
}
fn tasks_cards(s: &DomainState, request: &str) -> Vec<(Interaction, TasksCard)> {
    let mut cards: Vec<_> = cards(s, request).into_iter()
        .filter(|i| i.id.0.starts_with(crate::request_outcome::TASKS_PREFIX))
        .map(|i| { let card = TasksCard::from_body(&i.body).unwrap(); (i, card) })
        .collect();
    cards.sort_by_key(|(i, _)| i.order);
    cards
}
fn ids(card: &TasksCard) -> Vec<&str> {
    card.tasks.iter().map(|t| t.id.as_str()).collect()
}

#[test]
fn the_tasks_a_turn_made_reach_the_user_on_one_card_in_the_order_they_run() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    planned(&mut s, "ship", "r", &[]);
    planned(&mut s, "design", "r", &[]);
    planned(&mut s, "docs", "r", &["design"]);
    planned(&mut s, "build", "r", &["design"]);
    ok(&mut s, orchestrator(), Command::SetDependencies { task_id: "ship".into(), dependencies: ["build".into(), "docs".into()].into(), stack: None });
    ok(&mut s, orchestrator(), Command::SetPriority { task_id: "build".into(), priority: TaskPriority::High });
    ok(&mut s, orchestrator(), Command::SetPriority { task_id: "docs".into(), priority: TaskPriority::Low });
    let out = announce(&mut s);
    let [(interaction, card)] = tasks_cards(&s, "r").try_into().unwrap();
    assert_eq!(interaction.id.0, TasksCard::id("r", 1));
    assert_eq!((interaction.author.clone(), interaction.card.kind), (Author::System, CardKind::Know));
    assert_eq!(ids(&card), ["design", "build", "docs", "ship"]);
    assert_eq!(card.tasks[1].priority, TaskPriority::High);
    assert_eq!(interaction.card.effective_title(), "I created 4 tasks");
    assert_eq!(interaction.card.why, "First “Task design”; then “Task build” and “Task docs”, in parallel; then “Task ship”.");
    assert_eq!(card.task_line(3, crate::views::Lang::Es), ("4.".into(), "Task ship · después de 2 y 3".into()));
    assert_eq!(card.task_line(1, crate::views::Lang::En), ("2.".into(), "Task build · High priority · after 1".into()));
    assert!(notified(&out, &interaction));
}

#[test]
fn announcing_again_tells_nothing_new() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    planned(&mut s, "design", "r", &[]);
    announce(&mut s);
    assert!(s.unannounced_tasks().is_empty());
    let before = tasks_cards(&s, "r");
    let out = announce(&mut s);
    assert_eq!(tasks_cards(&s, "r"), before);
    assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "{:?}", out.effects);
    assert_eq!(before[0].0.card.effective_title(), "I created the task “Task design”");
}

#[test]
fn a_second_batch_names_only_its_new_tasks() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    planned(&mut s, "design", "r", &[]);
    announce(&mut s);
    planned(&mut s, "build", "r", &["design"]);
    planned(&mut s, "docs", "r", &[]);
    announce(&mut s);
    let cards = tasks_cards(&s, "r");
    assert_eq!(cards.len(), 2);
    let (interaction, card) = &cards[1];
    assert_eq!(interaction.id.0, TasksCard::id("r", 2));
    assert!(card.more);
    assert_eq!(ids(card), ["build", "docs"]);
    assert_eq!(interaction.card.effective_title(), "I created 2 more tasks");
    assert_eq!(card.task_line(0, crate::views::Lang::Es).1, "Task build · después de «Task design»");
    assert_eq!(card.text(crate::views::Lang::Es).0, "He creado 2 tareas más");
}

#[test]
fn schedules_nodes_messages_and_maintenance_get_no_tasks_card() {
    let mut s = state();
    asked(&mut s, orchestrator(), "own", false);
    asked(&mut s, user(), "maintenance", true);
    asked(&mut s, user(), "scheduled", false);
    s.requests.get_mut(&RequestId::from("scheduled")).unwrap().origin = RequestOrigin::Schedule;
    ok(&mut s, user(), Command::CreateRequest { request_id: "message".into(), body: "From Claude".into(), auto_merge: false, maintenance: false, message: true });
    ok(&mut s, orchestrator(), Command::TitleRequest { request_id: "message".into(), title: "From Claude".into() });
    s.requests.get_mut(&RequestId::from("own")).unwrap().origin = RequestOrigin::Node { agent_id: "n".into() };
    for request in ["own", "maintenance", "scheduled", "message"] {
        planned(&mut s, &format!("{request}-task"), request, &[]);
    }
    assert!(s.unannounced_tasks().is_empty());
    let out = announce(&mut s);
    assert!(s.interactions.is_empty(), "{:?}", s.interactions.keys().collect::<Vec<_>>());
    assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })));
}

#[test]
fn done_takes_the_place_of_an_unread_tasks_card() {
    for read in [false, true] {
        let mut s = state();
        asked(&mut s, user(), "r", false);
        planned(&mut s, "design", "r", &[]);
        announce(&mut s);
        let id = TasksCard::id("r", 1);
        if read {
            ok(&mut s, user(), Command::MarkRead { interaction_id: id.as_str().into() });
        }
        done(&mut s, "design", None);
        close(&mut s, "r", Some("The design is ready."), None);
        assert_eq!(s.interactions[&InteractionId::from(id.as_str())].withdrawn, !read);
        assert!(!s.interactions[&InteractionId::from(DoneCard::id("r").as_str())].withdrawn);
    }
}

#[test]
fn only_alondra_announces_tasks() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    planned(&mut s, "design", "r", &[]);
    for actor in [user(), orchestrator(), Actor::Agent { agent_id: "n".into() }] {
        let before = s.clone();
        assert_eq!(exec(&mut s, actor, Command::AnnounceRequestTasks).unwrap_err().code, "forbidden");
        assert_eq!(s, before);
    }
}

#[test]
fn tasks_the_daemon_finds_when_it_opens_count_as_told_without_a_card() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    planned(&mut s, "design", "r", &[]);
    let out = ok(&mut s, Actor::System, Command::AcknowledgeRequestTasks);
    assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "{:?}", out.effects);
    assert!(s.unannounced_tasks().is_empty());
    assert!(tasks_cards(&s, "r").iter().all(|(i, _)| i.withdrawn));
    planned(&mut s, "build", "r", &["design"]);
    announce(&mut s);
    let cards = tasks_cards(&s, "r");
    let (interaction, card) = cards.last().unwrap();
    assert!(!interaction.withdrawn);
    assert_eq!(ids(card), ["build"]);
    assert_eq!(interaction.card.effective_title(), "I created one more task: “Task build”");
    assert_eq!(interaction.card.why, "It starts once “Task design” is done.");
}

#[test]
fn card_title_contract_request_answer_fields_are_checked_before_closing() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    let mut cases = Vec::new();
    for headline in ["x".repeat(91), "First\nSecond".into(), "First\rSecond".into(), String::new(), " ".into()] {
        cases.push(("headline", Some("Complete answer.".to_owned()), None, Some(headline)));
    }
    cases.push(("answer", Some(" ".into()), None, Some("Release result".into())));
    cases.push(("title", Some("Complete answer.".into()), Some(" ".into()), None));
    for (field, answer, title, headline) in cases {
        let error = exec(&mut s, orchestrator(), Command::CloseRequest {
            request_id: "r".into(), answer, title, headline, withdrawn: false,
        }).unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert_eq!(error.subject.as_deref(), Some(field));
        assert!(!s.requests[&"r".into()].closed, "a malformed {field} must not close the request");
    }
    let body = "# Release result\n\nKeep this authored heading.";
    close(&mut s, "r", Some(body), Some("Release result"));
    let card = the_card(&s, "r");
    assert_eq!(card.card.title.as_deref(), Some("Release result"));
    assert_eq!(card.card.content.as_ref().unwrap().body, body);
}

#[test]
fn card_title_contract_request_subject_bounds_metadata_without_changing_the_answer() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    let metadata = format!("{}\nSecond line", "Request metadata ".repeat(20));
    let body = "# A different heading\n\nKeep this answer literally.";
    ok(&mut s, orchestrator(), Command::CloseRequest {
        request_id: "r".into(), answer: Some(body.into()), title: Some(metadata.clone()), headline: None, withdrawn: false,
    });
    let card = the_card(&s, "r");
    let title = card.card.title.as_deref().unwrap();
    assert!(title.starts_with("Request metadata Request metadata"));
    assert!(title.chars().count() <= CardSpec::TITLE_MAX);
    assert!(!title.contains(['\n', '\r']));
    assert_eq!(card.card.content.as_ref().unwrap().body, body);
    assert_eq!(s.requests[&"r".into()].title.as_deref(), Some(metadata.as_str()));
}
