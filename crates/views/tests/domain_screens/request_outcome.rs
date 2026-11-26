//! Every request the user made hears how it ended from Alondra, once: its answer when it needed no
//! task, «Done» when its tasks are over. Nobody else's requests get a card, and the orchestrator
//! cannot send a second one.
use alondra_domain_model::*;
use alondra_views as views;
use alondra_domain_model::request_outcome::DoneCard;
use std::collections::BTreeSet;
use alondra_domain_rules::Apply as _;

/// Case 9: retain the beta's version, build and destination when closing does not repeat them.
#[test]
fn client_text_contracts_done_carries_the_published_result() {
    let mut s = state();
    asked(&mut s, user(), "r", false);
    task(&mut s, "beta", "r");
    done(&mut s, "beta", None);
    let published = "Garden 2.4 (build 181) is available to internal testers in TestFlight.";
    let delivery = s.interactions.get_mut(&"final-beta".into()).unwrap();
    delivery.body = "The beta is ready".into();
    delivery.card.content.as_mut().unwrap().body = published.into();
    close(&mut s, "r", None, None);
    let card = the_card(&s, "r");
    let body = DoneCard::from_body(&card.body).unwrap();
    assert!(body.to_body().contains(published), "{}", body.to_body());
    let index = ReadIndex::build(&s);
    let live = alondra_views::LiveSnapshot::default();
    for client in [alondra_views::Client::Tui, alondra_views::Client::Ios, alondra_views::Client::Mac] {
        let view = alondra_views::build(&format!("card:{}", card.id), alondra_views::RenderContext {
            domain: &s, index: &index, live: &live, client, lang: alondra_core::text::Lang::Es,
            transport: alondra_views::Transport::Loopback, cursor: "cursor", rendered_at: "",
        }).unwrap();
        assert!(serde_json::to_string(&view).unwrap().contains(published), "{client:?}: {view:?}");
    }
}

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
    ok(s, orchestrator(), Command::CreateTask { priority: Default::default(),
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
        allowed_methods: vec![MergeMethod::Squash], review: None, aliases: Default::default(), address_read_at: 0, identity: Default::default(), tracking: Default::default(), missing: None, relocation_asked_at: None,
    });
    s.tasks.get_mut(&task_id.into()).unwrap().pr_id = Some(id);
}

fn close(s: &mut DomainState, request: &str, answer: Option<&str>, headline: Option<&str>) -> CommandOutcome {
    let outcome = if s.tasks.values().any(|task| task.request_id.0 == request) { alondra_domain_model::CloseOutcome::TasksEnded } else { alondra_domain_model::CloseOutcome::Answered };
    ok(s, orchestrator(), Command::CloseRequest { covered_by: vec![], outcome: Some(outcome),
        request_id: request.into(), answer: answer.map(String::from), withdrawn: false, title: None, headline: headline.map(String::from), folded_into: None })
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
