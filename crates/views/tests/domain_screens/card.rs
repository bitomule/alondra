//! The card rules R1–R22 of the card redesign, one test each, named as on the design board.
use alondra_domain_model::*;
use alondra_views as views;
use crate::inbox_items;
use std::collections::BTreeSet;

#[test]
fn a_suggestion_goes_to_the_orchestrator_and_never_to_the_users_inbox() {
    let mut s = fixture();
    let out = ok(&mut s, agent(), suggest("p"));
    assert!(out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyOrchestrator { interaction_id: Some(i), .. } if i.0 == "p")));
    assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "{:?}", out.effects);
    assert!(inbox_items(&s).iter().all(|item| item.id != "interaction:p"));
    assert_eq!(alondra_views::inbox_badge(&ReadIndex::build(&s)), 0);
}

#[test]
fn snooze_persists_and_old_cards_still_decode() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q", "t", true, decide("Choose the name")));
    let mut old = serde_json::to_value(&s.interactions[&InteractionId::from("q")]).unwrap();
    old.as_object_mut().unwrap().remove("snoozed_until");
    old.as_object_mut().unwrap().remove("resurfaced_at_revision");
    let old: Interaction = serde_json::from_value(old).unwrap();
    assert_eq!(old.snoozed_until, None);
    assert_eq!(old.resurfaced_at_revision, None);
    ok(&mut s, user(), snooze("q", 200));
    let saved = serde_json::to_vec(&s.interactions[&InteractionId::from("q")]).unwrap();
    let restored: Interaction = serde_json::from_slice(&saved).unwrap();
    assert_eq!(restored.snoozed_until, Some(200));
    assert_eq!(restored, s.interactions[&InteractionId::from("q")]);
    let index = ReadIndex::build(&s);
    assert_eq!(index.snoozed().len(), 1);
    assert_eq!(index.snoozed()[0].until, 200);
    assert_eq!(index.next_snoozed_until(), Some(200));
    assert!(index.waiting().is_empty());
    assert_eq!(index.counts().pending, 0);
    assert_eq!(alondra_views::inbox_badge(&index), 0);
    assert!(index.task(&"t".into()).unwrap().waits_for_user);
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config { cases: 128, failure_persistence: None, ..Default::default() })]
    #[test]
    fn snooze_never_stops_a_task_waiting_for_the_user(
        times in proptest::collection::vec(101i64..i64::MAX, 1..20),
        kind in 0u8..3,
    ) {
        let mut s = sdk_fixture();
        match kind {
            0 => { ok(&mut s, agent(), ask("q", "t", true, decide("Choose the name"))); }
            1 => { ok(&mut s, agent(), Command::RequestAction { attachments: vec![], interaction_id: "q".into(), scope: scope("t"),
                recipient: Recipient::User, body: "Try the build".into(), blocking: true, options: two(),
                card: CardSpec::new(CardKind::Test, "Try the build").why("The release needs testing") }); }
            _ => {
                ok(&mut s, agent(), present("q", text(), true, review()));
                ok(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
            }
        }
        let attention = s.attention(&s.tasks[&TaskId::from("t")]);
        let live = s.live_state(&"t".into());
        let task_state = s.tasks[&TaskId::from("t")].state;
        for until in times {
            let snoozed = ok(&mut s, user(), snooze("q", until));
            proptest::prop_assert_eq!(notify_count(&snoozed, "q"), 0);
            let index = ReadIndex::build(&s);
            proptest::prop_assert!(index.waiting().is_empty());
            proptest::prop_assert_eq!(index.activity().inbox, 0);
            let current = &index.task(&"t".into()).unwrap().attention;
            match (&attention, current) {
                (Attention::Act(before), Attention::Act(after)) => proptest::prop_assert_eq!(&before.interaction, &after.interaction),
                _ => proptest::prop_assert_eq!(&attention, current),
            }
            proptest::prop_assert_eq!(index.task(&"t".into()).unwrap().live.as_ref(), live.as_ref());
            proptest::prop_assert_eq!(s.tasks[&TaskId::from("t")].state, task_state);
            proptest::prop_assert!(index.task(&"t".into()).unwrap().waits_for_user);
            let live_view = alondra_views::LiveSnapshot::default();
            let view = alondra_views::build("tasks", alondra_views::RenderContext {
                domain: &s, index: &index, live: &live_view, client: alondra_views::Client::Tui, lang: alondra_core::text::Lang::En,
                transport: alondra_views::Transport::Loopback, cursor: "", rendered_at: "",
            }).unwrap();
            proptest::prop_assert!(serde_json::to_string(&view).unwrap().contains("\"id\":\"block:for-you\""));
            ok(&mut s, user(), Command::UnsnoozeCard { interaction_id: "q".into() });
        }
    }
}

#[test]
fn snooze_keeps_the_card_and_its_question_in_task_screens() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q", "t", true, decide("Choose the name")));
    ok(&mut s, user(), snooze("q", 200));
    let index = ReadIndex::build(&s);
    assert!(index.waiting().is_empty());
    assert_eq!(alondra_views::inbox_badge(&index), 0);
    let live = alondra_views::LiveSnapshot::default();
    for client in [alondra_views::Client::Tui, alondra_views::Client::Ios, alondra_views::Client::Mac] {
        for lang in [alondra_core::text::Lang::En, alondra_core::text::Lang::Es] {
            let context = alondra_views::RenderContext {
                domain: &s, index: &index, live: &live, client, lang,
                transport: alondra_views::Transport::Loopback, cursor: "", rendered_at: "",
            };
            let detail = alondra_views::build("task:t", context).unwrap();
            let waiting = detail.sections.iter().find(|section| section.id == "waiting")
                .expect("the task still offers its snoozed question");
            assert!(waiting.items.iter().any(|item| matches!(item,
                alondra_views::Item::Card(card) if card.id == "q")));
            let list = alondra_views::build("tasks", alondra_views::RenderContext {
                domain: &s, index: &index, live: &live, client, lang,
                transport: alondra_views::Transport::Loopback, cursor: "", rendered_at: "",
            }).unwrap();
            assert!(serde_json::to_string(&list).unwrap().contains("Choose the name"));
        }
    }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn agent() -> Actor {
    Actor::Agent { agent_id: "a".into() }
}

/// Every card in Waiting for You has something the user can do now and nothing holding it back,
/// whatever state a test reached (the property of the cards design, checked after every command).
fn assert_cards_usable(s: &DomainState) {
    for item in inbox_items(&s).iter().filter(|item| item.pending) {
        assert!(item.blocked_by.is_empty(), "{} is listed while held back", item.id);
        assert!(item.available_actions.iter().any(|action| action.enabled), "{} is listed with nothing the user can do", item.id);
    }
}

fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    s.seed_mergeable_for_presented(&c);
    let outcome = s.execute(&a, &CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    });
    assert_cards_usable(s);
    outcome
}

fn ok(s: &mut DomainState, a: Actor, c: Command) -> CommandOutcome {
    exec(s, a, c).unwrap()
}

fn task(s: &mut DomainState, id: &str, agent: &str) {
    ok(s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: Some(agent.into()),
        review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_domain_model::LaunchPlan::new("node", "/tmp")),
    });
}

fn register_agent(s: &mut DomainState, id: &str) {
    ok(s, Actor::System, Command::RegisterAgent { agent_id: id.into(), name: format!("Instance {id}"), at: 0 });
}

/// The same task, its node running a turn through the Agent SDK.
fn sdk_fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    register_agent(&mut s, "a");
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    s.requests.get_mut(&"r".into()).unwrap().title = Some("Build it".into());
    ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Task t".into(), agent_id: None,
        review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_domain_model::LaunchPlan::new("node", "/tmp")),
    });
    ok(&mut s, Actor::System, Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude, task_id: "t".into(), agent_id: "a".into(),
        conversation: "conversation".into(), instance: "instance".into(), host: "sdk-host".into(), model: None, account: None, leaves_limit: None });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from("t")][0].clone() else { panic!("planned first") };
    ok(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: "turn-1".into(), input },
    ] });
    s
}

fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    register_agent(&mut s, "a");
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    s.requests.get_mut(&"r".into()).unwrap().title = Some("Build it".into());
    task(&mut s, "t", "a");
    s
}

fn scope(task: &str) -> Scope {
    Scope::Task { task_id: task.into() }
}

fn option(id: &str, label: &str) -> QuestionOption {
    QuestionOption { id: id.into(), label: label.into(), description: None }
}

fn two() -> Vec<QuestionOption> {
    vec![option("a", "Atlas"), option("b", "Borealis")]
}

fn decide(need: &str) -> CardSpec {
    CardSpec::new(CardKind::Decide, need).needing(need).why("The name goes on the release")
}

fn ask(id: &str, task: &str, blocking: bool, card: CardSpec) -> Command {
    Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: scope(task), recipient: Recipient::User,
        body: "Which name?".into(), blocking, options: two(), card,
    }
}

fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "The result".into() }]
}

fn review() -> CardSpec {
    CardSpec::new(CardKind::Review, "Review the release").content(ContentKind::Report, "Everything shipped")
}

fn present(id: &str, targets: Vec<DeliveryTarget>, is_final: bool, card: CardSpec) -> Command {
    Command::PresentResult {
        checklist: Vec::new(),
        attachments: vec![], task_id: "t".into(), interaction_id: id.into(), body: "Result ready".into(),
        targets, is_final, nothing_to_review: false, card,
    }
}

fn suggest(id: &str) -> Command {
    Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: scope("t"), recipient: Recipient::Orchestrator,
        body: "The README still names the old flag".into(), blocking: false,
        card: CardSpec::new(CardKind::Propose, "Update the README's old flag").why("It is not part of this task"),
        options: suggestion_options(),
    }
}

fn current(note: &str) -> SuggestionDecision {
    SuggestionDecision::CurrentTask { note: note.into() }
}

fn nothing_to_review(id: &str, targets: Vec<DeliveryTarget>, is_final: bool) -> Command {
    match present(id, targets, is_final, review()) {
        Command::PresentResult { attachments, task_id, interaction_id, body, targets, is_final, card, .. } =>
            Command::PresentResult { checklist: Vec::new(), attachments, task_id, interaction_id, body, targets, is_final, nothing_to_review: true, card },
        _ => unreachable!(),
    }
}

fn snooze(id: &str, until: i64) -> Command {
    Command::SnoozeCard { interaction_id: id.into(), until, now: 100 }
}

fn notify_count(outcome: &CommandOutcome, id: &str) -> usize {
    outcome.effects.iter().filter(|effect| matches!(effect,
        DomainEffect::NotifyUser { interaction_id, .. } if interaction_id.0 == id)).count()
}
