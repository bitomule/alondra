//! The card rules R1–R22 of the card redesign, one test each, named as on the design board.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}
fn agent() -> Actor {
    Actor::Agent { agent_id: "a".into() }
}
fn agent_b() -> Actor {
    Actor::Agent { agent_id: "b".into() }
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
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
/// The rejection's code and field; the state must not move.
fn refused(s: &mut DomainState, a: Actor, c: Command) -> (String, Option<String>) {
    let before = s.clone();
    let error = exec(s, a, c).unwrap_err();
    assert_eq!(*s, before, "a rejected command changed state");
    if let Some(field) = &error.subject {
        assert!(error.message.starts_with(&format!("{field}: ")), "the message names its field: {}", error.message);
    }
    (error.code, error.subject)
}
fn task(s: &mut DomainState, id: &str, agent: &str) {
    ok(s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: Some(agent.into()),
        review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
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
    ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Task t".into(), agent_id: None,
        review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
    });
    ok(&mut s, Actor::System, Command::AdmitExecution { provider: crate::accounts::Provider::Claude, task_id: "t".into(), agent_id: "a".into(),
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
fn inform(id: &str, scope: Scope, body: &str) -> Command {
    Command::PublishInformation {
        attachments: vec![], interaction_id: id.into(), scope, recipient: Recipient::User, body: body.into(),
        card: CardSpec::new(CardKind::Know, "Read the release notes"),
    }
}

fn card_reply(request: &str, card: &str, body: &str) -> Command {
    serde_json::from_value(serde_json::json!({
        "kind": "create_card_reply", "request_id": request, "interaction_id": card,
        "body": body, "auto_merge": false,
    })).unwrap()
}

#[test]
fn a_card_reply_keeps_the_card_and_every_task_of_its_request() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    task(&mut s, "t-2", "b");
    let scope = Scope::Orchestrator { request_id: Some("r".into()) };
    ok(&mut s, Actor::System, inform("weekly-notice", scope.clone(), "The weekly report is ready."));
    let title = s.interactions[&"weekly-notice".into()].card.effective_title().to_owned();
    let body = "What did it do?\nI have not seen anything.";
    let outcome = ok(&mut s, user(), card_reply("follow-up", "weekly-notice", body));
    let request = &s.requests[&"follow-up".into()];
    assert_eq!(request.body, body);
    assert!(request.message);
    assert_eq!(request.origin, RequestOrigin::User);
    assert_eq!(serde_json::to_value(request).unwrap()["reply_to"], serde_json::json!({
        "interaction_id": "weekly-notice", "title": title, "scope": scope,
        "request_id": "r", "task_ids": ["t", "t-2"],
    }));
    assert!(outcome.effects.iter().any(|effect| matches!(effect,
        DomainEffect::NotifyOrchestrator { request_id: Some(id), .. } if id.0 == "follow-up")));
}

#[test]
fn a_card_reply_can_name_one_task_or_no_task() {
    for scope in [scope("t"), Scope::Orchestrator { request_id: None }] {
        let mut s = fixture();
        ok(&mut s, Actor::System, inform("notice", scope.clone(), "An update"));
        ok(&mut s, user(), card_reply("follow-up", "notice", "Explain the update"));
        let reference = serde_json::to_value(&s.requests[&"follow-up".into()]).unwrap()["reply_to"].clone();
        assert_eq!(reference["scope"], serde_json::to_value(&scope).unwrap());
        match scope {
            Scope::Task { .. } => {
                assert_eq!(reference["request_id"], "r");
                assert_eq!(reference["task_ids"], serde_json::json!(["t"]));
            }
            Scope::Orchestrator { .. } => {
                assert!(reference["request_id"].is_null());
                assert_eq!(reference["task_ids"], serde_json::json!([]));
            }
        }
    }
}

#[test]
fn a_card_reply_does_not_acquire_tasks_added_after_the_user_sent_it() {
    let mut s = fixture();
    ok(&mut s, Actor::System, inform("notice", Scope::Orchestrator { request_id: Some("r".into()) }, "An update"));
    ok(&mut s, user(), card_reply("follow-up", "notice", "Explain"));
    register_agent(&mut s, "b");
    task(&mut s, "t-2", "b");
    let reference = serde_json::to_value(&s.requests[&"follow-up".into()]).unwrap()["reply_to"].clone();
    assert_eq!(reference["task_ids"], serde_json::json!(["t"]));
}

#[test]
fn a_card_reply_cannot_supply_a_forged_reference() {
    let command = serde_json::json!({
        "kind": "create_card_reply", "request_id": "follow-up", "interaction_id": "notice",
        "body": "Explain", "reply_to": {"interaction_id": "other", "task_ids": ["other-task"]},
    });
    assert!(serde_json::from_value::<Command>(command).is_err());
}

#[test]
fn a_card_reply_rejects_missing_or_withdrawn_cards_and_cards_not_for_the_user() {
    let mut s = fixture();
    ok(&mut s, Actor::System, inform("notice", scope("t"), "An update"));
    s.interactions.get_mut(&"notice".into()).unwrap().withdrawn = true;
    let mut hidden = inform("hidden", scope("t"), "An internal update");
    if let Command::PublishInformation { recipient, .. } = &mut hidden { *recipient = Recipient::Orchestrator; }
    ok(&mut s, agent(), hidden);
    for card in ["missing", "notice", "hidden"] {
        let (code, field) = refused(&mut s, user(), card_reply("follow-up", card, "Explain"));
        assert_eq!(code, "invalid_input");
        assert_eq!(field.as_deref(), Some("interaction_id"));
    }
}

#[test]
fn only_the_user_can_create_a_card_reply_and_a_refusal_creates_no_message() {
    let mut s = fixture();
    ok(&mut s, Actor::System, inform("notice", scope("t"), "An update"));
    for actor in [agent(), orchestrator(), Actor::System] {
        assert_eq!(refused(&mut s, actor, card_reply("follow-up", "notice", "Explain")).0, "forbidden");
    }
    assert_eq!(refused(&mut s, user(), card_reply("follow-up", "notice", "  ")).0, "invalid_input");
    ok(&mut s, user(), card_reply("follow-up", "notice", "Explain"));
    refused(&mut s, user(), card_reply("follow-up", "notice", "Explain again"));
}

#[test]
fn old_requests_and_general_messages_have_no_card_reply_reference() {
    let old: Request = serde_json::from_value(serde_json::json!({
        "id": "old-request", "body": "Original request", "closed": false,
    })).unwrap();
    assert!(serde_json::to_value(old).unwrap()["reply_to"].is_null());
    let mut s = fixture();
    ok(&mut s, user(), Command::CreateRequest {
        request_id: "general".into(), body: "A general message".into(),
        auto_merge: false, maintenance: false, message: true,
    });
    assert!(serde_json::to_value(&s.requests[&"general".into()]).unwrap()["reply_to"].is_null());
}
fn answer(id: &str) -> Command {
    Command::AnswerQuestion { attachments: vec![], interaction_id: id.into(), option_id: "a".into(), inputs: BTreeMap::new() }
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
fn register_pr(s: &mut DomainState) {
    ok(s, agent(), Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
        number: 7, head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}
fn pr_target() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }]
}
fn open_reviews(s: &DomainState) -> usize {
    s.interactions.values().filter(|i| i.card.kind == CardKind::Review && s.open(i)).count()
}

#[test]
fn only_the_assigned_node_sends_cards() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    task(&mut s, "t2", "b");
    assert_eq!(refused(&mut s, agent_b(), ask("q", "t", false, decide("Pick a name"))),
        ("not_assigned".into(), Some("scope.task_id".into())));
    ok(&mut s, agent(), ask("q", "t", false, decide("Pick a name")));
}

#[test]
fn a_closed_task_accepts_only_a_closing_notice() {
    let mut s = fixture();
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    ok(&mut s, agent(), ask("winding-down", "t", false, decide("Keep or drop the half-done work")));
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    assert_eq!(refused(&mut s, agent(), ask("q", "t", false, decide("Pick a name"))),
        ("task_not_active".into(), Some("scope.task_id".into())));
    assert_eq!(refused(&mut s, agent(), inform("closing", scope("t"), "I stopped: the task was cancelled")),
        ("forbidden".into(), Some("card.kind".into())));
    ok(&mut s, Actor::System, inform("closing", scope("t"), "The task was cancelled"));
    assert_eq!(s.interactions[&"closing".into()].card.kind, CardKind::Know);
}

#[test]
fn a_node_without_a_task_only_informs() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    assert_eq!(refused(&mut s, agent_b(), ask("q", "t", false, decide("Pick a name"))),
        ("no_task".into(), Some("scope".into())));
    assert_eq!(refused(&mut s, agent_b(), inform("hello", Scope::Orchestrator { request_id: None }, "I am free")),
        ("forbidden".into(), Some("card.kind".into())));
    let mut to_orchestrator = inform("hello", Scope::Orchestrator { request_id: None }, "I am free");
    if let Command::PublishInformation { recipient, .. } = &mut to_orchestrator {
        *recipient = Recipient::Orchestrator;
    }
    ok(&mut s, agent_b(), to_orchestrator);
    assert_eq!(s.interactions[&"hello".into()].author, Author::Agent { agent_id: "b".into() });
}

#[test]
fn one_blocking_ask_per_node() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q1", "t", true, decide("Pick a name")));
    assert_eq!(refused(&mut s, agent(), ask("q2", "t", true, decide("Pick a colour"))),
        ("ask_already_open".into(), Some("blocking".into())));
    ok(&mut s, agent(), ask("q3", "t", false, decide("Pick a colour")));
    let mut replacing = decide("Pick a name and a colour");
    replacing.replaces = Some("q1".into());
    ok(&mut s, agent(), ask("q4", "t", true, replacing));
    assert!(s.interactions[&"q1".into()].withdrawn);
}

#[test]
fn replacing_withdraws_the_old_card_atomically() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q1", "t", false, decide("Pick a name")));
    let mut replacing = decide("Pick a shorter name");
    replacing.replaces = Some("q1".into());
    let out = ok(&mut s, agent(), ask("q2", "t", false, replacing.clone()));
    assert!(s.interactions[&"q1".into()].withdrawn, "the old card is withdrawn in the same step");
    assert!(s.open(&s.interactions[&"q2".into()]));
    assert_eq!(s.interactions[&"q1".into()].revision, out.revision);
    ok(&mut s, user(), answer("q2"));
    replacing.replaces = Some("q2".into());
    assert_eq!(refused(&mut s, agent(), ask("q3", "t", false, replacing)),
        ("already_answered".into(), Some("card.replaces".into())));
}

#[test]
fn after_clarify_the_node_must_replace_the_card() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q1", "t", true, decide("Pick a name")));
    let out = ok(&mut s, user(), Command::RequestClarification {
        interaction_id: "q1".into(), reason: ClarificationReason::Unclear, text: Some("Which product?".into()),
    });
    assert!(matches!(&out.effects[..], [DomainEffect::NotifyAgent { message, .. }] if message.contains("Which product?") && message.contains("card.replaces")));
    assert_eq!(refused(&mut s, agent(), ask("q2", "t", false, decide("Pick a colour"))),
        ("clarification_pending".into(), Some("card.replaces".into())));
    let mut replacing = decide("Pick a name for the Mac app");
    replacing.replaces = Some("q1".into());
    ok(&mut s, agent(), ask("q3", "t", true, replacing));
    ok(&mut s, agent(), ask("q4", "t", false, decide("Pick a colour")));
}

#[test]
fn a_repeated_notice_is_not_shown_twice() {
    let mut s = fixture();
    ok(&mut s, Actor::System, inform("i1", scope("t"), "Tests pass"));
    let out = ok(&mut s, Actor::System, inform("i2", scope("t"), "Tests pass"));
    assert!(!out.changed);
    assert_eq!(out.created_ids, vec!["i1".to_owned()], "the existing notice is returned");
    assert!(!s.interactions.contains_key(&"i2".into()));
    ok(&mut s, user(), Command::MarkRead { interaction_id: "i1".into() });
    ok(&mut s, Actor::System, inform("i3", scope("t"), "Tests pass"));
    assert!(s.interactions.contains_key(&"i3".into()), "once read, the same words are news again");
}

#[test]
fn an_answered_question_is_final() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q", "t", true, decide("Pick a name")));
    ok(&mut s, user(), answer("q"));
    assert_eq!(refused(&mut s, user(), answer("q")).0, "question_already_answered");
    assert_eq!(refused(&mut s, agent(), Command::Withdraw { interaction_id: "q".into() }).0, "question_already_answered");
}

#[test]
fn one_live_review_per_task() {
    let mut s = fixture();
    ok(&mut s, agent(), present("d1", text(), true, review()));
    ok(&mut s, agent(), present("d2", text(), true, review()));
    assert!(s.interactions[&"d1".into()].withdrawn);
    assert_eq!(open_reviews(&s), 1);
}

#[test]
fn no_final_delivery_while_a_blocking_ask_is_open() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q", "t", true, decide("Pick a name")));
    assert_eq!(refused(&mut s, agent(), present("d", text(), true, review())),
        ("blocking_open".into(), Some("is_final".into())));
    ok(&mut s, agent(), present("progress", text(), false, CardSpec::new(CardKind::Know, "Read the progress")));
    ok(&mut s, agent(), Command::Withdraw { interaction_id: "q".into() });
    ok(&mut s, agent(), present("d", text(), true, review()));
}

#[test]
fn a_review_targets_the_current_head_of_its_own_open_pr() {
    let mut s = fixture();
    register_pr(&mut s);
    let stale = vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head0".into() }];
    assert_eq!(refused(&mut s, agent(), present("d", stale, true, review())),
        ("pr_mismatch".into(), Some("targets.head".into())));
    let unknown = vec![DeliveryTarget::PullRequest { pr_id: "other".into(), head: "head1".into() }];
    assert_eq!(refused(&mut s, agent(), present("d", unknown, true, review())),
        ("pr_mismatch".into(), Some("targets.pr_id".into())));
    ok(&mut s, agent(), present("d", pr_target(), true, review()));
    assert_eq!(s.interactions[&"d".into()].card.kind, CardKind::Review);
}

#[test]
fn a_pr_presented_without_a_head_shows_the_commit_it_is_at_when_the_delivery_commits() {
    let mut s = fixture();
    register_pr(&mut s);
    let omitted: Vec<DeliveryTarget> = serde_json::from_value(serde_json::json!([{"kind":"pull_request","pr_id":"pr"}])).unwrap();
    ok(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head2".into(), base: "main".into(), state: PrState::Open, merged_head: None,
    });
    ok(&mut s, agent(), present("d", omitted, true, review()));
    let InteractionPayload::Delivery { targets, .. } = &s.interactions[&"d".into()].payload else { panic!("a delivery") };
    assert_eq!(targets, &vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head2".into() }]);
}

#[test]
fn a_head_the_pr_is_not_at_is_refused_naming_the_commit_it_is_at() {
    let mut s = fixture();
    register_pr(&mut s);
    let stale = vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head0".into() }];
    let error = exec(&mut s, agent(), present("d", stale, true, review())).unwrap_err();
    assert_eq!(error.message, "targets.head: PR #7 is at head1, not head0. If head0 is your latest commit, push it and present again; \
        to present the commit the PR is at, give head head1 or leave head out");
}

#[test]
fn a_merged_pr_is_reported_not_reviewed() {
    let mut s = fixture();
    register_pr(&mut s);
    ok(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
    ok(&mut s, agent(), present("d", pr_target(), true, review()));
    assert_eq!(s.interactions[&"d".into()].card.kind, CardKind::Know);
}

#[test]
fn a_new_plan_replaces_the_open_one() {
    let mut s = fixture();
    let plan = |id: &str, title: &str| Command::HoldHook {
        interaction_id: id.into(), task_id: "t".into(), agent_id: "a".into(), hold: Hold::Plan,
        body: format!("# {title}\n\n- [ ] Do it"), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, title).content(ContentKind::Plan, format!("# {title}\n\n- [ ] Do it")),
    };
    ok(&mut s, Actor::System, plan("p1", "Ship the cards"));
    ok(&mut s, Actor::System, plan("p2", "Ship the cards in two steps"));
    assert!(s.interactions[&"p1".into()].withdrawn);
    assert!(s.open(&s.interactions[&"p2".into()]));
}

#[test]
fn blocks_points_at_an_open_target_of_the_task() {
    let mut s = fixture();
    register_pr(&mut s);
    let mut blocking = decide("Pick a name");
    blocking.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
    ok(&mut s, agent(), ask("q1", "t", false, blocking.clone()));
    blocking.blocks = vec![CardTarget::PullRequest { pr_id: "missing".into() }];
    assert_eq!(refused(&mut s, agent(), ask("q2", "t", false, blocking.clone())),
        ("unknown_target".into(), Some("card.blocks".into())));
    register_agent(&mut s, "b");
    task(&mut s, "t2", "b");
    ok(&mut s, agent_b(), Command::PresentResult {
        checklist: Vec::new(),
        attachments: vec![], task_id: "t2".into(), interaction_id: "elsewhere".into(), body: "Done".into(),
        targets: text(), is_final: false, nothing_to_review: false, card: CardSpec::new(CardKind::Know, "Read the result"),
    });
    blocking.blocks = vec![CardTarget::Delivery { interaction_id: "elsewhere".into() }];
    assert_eq!(refused(&mut s, agent(), ask("q3", "t", false, blocking)),
        ("unknown_target".into(), Some("card.blocks".into())), "a Task the card's Task does not depend on");
}

#[test]
fn merge_waits_for_what_blocks_it() {
    let mut s = fixture();
    register_pr(&mut s);
    ok(&mut s, agent(), present("d", pr_target(), true, review()));
    let mut blocking = decide("Pick a name");
    blocking.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
    ok(&mut s, agent(), ask("q", "t", false, blocking));
    assert_eq!(s.blocked_by(&"d".into()), vec![InteractionId::from("q")]);
    let authorize = || Command::AuthorizeMerge {
        authorization_id: "auth".into(), presentation_id: "d".into(), pr_id: "pr".into(),
        expected_head: "head1".into(), method: MergeMethod::Squash,
    };
    assert_eq!(refused(&mut s, user(), authorize()), ("blocked".into(), Some("presentation_id".into())));
    assert_eq!(refused(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: pr_target() }).0, "blocked");
    ok(&mut s, user(), answer("q"));
    assert!(s.blocked_by(&"d".into()).is_empty());
    ok(&mut s, user(), authorize());
}

#[test]
fn blocks_never_form_a_cycle() {
    let mut s = fixture();
    ok(&mut s, agent(), present("d1", text(), false, CardSpec::new(CardKind::Know, "Read the progress")));
    register_pr(&mut s);
    s.interactions.get_mut(&"d1".into()).unwrap().card.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
    let mut back = review();
    back.blocks = vec![CardTarget::Delivery { interaction_id: "d1".into() }];
    assert_eq!(refused(&mut s, agent(), present("d2", pr_target(), true, back)),
        ("dependency_cycle".into(), Some("card.blocks".into())));
    ok(&mut s, agent(), present("d2", pr_target(), true, review()));
}

#[test]
fn each_kind_requires_its_fields() {
    let mut s = fixture();
    let field = |s: &mut DomainState, card: CardSpec| refused(s, agent(), ask("q", "t", false, card));
    assert_eq!(field(&mut s, decide("").titled("Release design")), ("invalid_input".into(), Some("card.need".into())));
    assert_eq!(field(&mut s, decide(&"n".repeat(91))), ("invalid_input".into(), Some("card.need".into())));
    assert_eq!(field(&mut s, decide("Pick a name").why("w".repeat(241))), ("invalid_input".into(), Some("card.why".into())));
    assert_eq!(field(&mut s, CardSpec::new(CardKind::Decide, "Pick a name")), ("invalid_input".into(), Some("card.why".into())));
    assert_eq!(field(&mut s, CardSpec::new(CardKind::Know, "Read this")), ("invalid_input".into(), Some("card.kind".into())));
    let mut summary = decide("Pick a name").content(ContentKind::Options, "Atlas or Borealis");
    summary.content.as_mut().unwrap().summary = Some("s".repeat(61));
    assert_eq!(field(&mut s, summary), ("invalid_input".into(), Some("card.content.summary".into())));
    let action = |card: CardSpec| Command::RequestAction {
        attachments: vec![], interaction_id: "act".into(), scope: scope("t"), recipient: Recipient::User,
        body: "Log in".into(), blocking: false, options: vec![], card,
    };
    let login = CardSpec::new(CardKind::Do, "Log in to App Store Connect").why("The upload needs a session");
    assert_eq!(refused(&mut s, agent(), action(login.clone())), ("invalid_input".into(), Some("card.verify".into())));
    let mut checked = login;
    checked.verify = Some("I run asc whoami".into());
    ok(&mut s, agent(), action(checked));
    assert_eq!(refused(&mut s, agent(), present("d", text(), true, CardSpec::new(CardKind::Review, "Review it"))),
        ("invalid_input".into(), Some("card.content".into())));
    ok(&mut s, agent(), ask("q", "t", false, decide(&"n".repeat(90))));
}

#[test]
fn options_are_few_short_and_one_recommended() {
    let mut s = fixture();
    let with = |options: Vec<QuestionOption>, recommended: Option<&str>| {
        let mut card = decide("Pick a name");
        card.recommended = recommended.map(Into::into);
        Command::AskQuestion {
            attachments: vec![], interaction_id: "q".into(), scope: scope("t"), recipient: Recipient::User,
            body: "Which name?".into(), blocking: false, options, card,
        }
    };
    assert_eq!(refused(&mut s, agent(), with(vec![option("a", "Atlas")], None)).1, Some("options".into()));
    let nine = (0..9).map(|n| option(&format!("o{n}"), &format!("Name {n}"))).collect();
    assert_eq!(refused(&mut s, agent(), with(nine, None)).1, Some("options".into()));
    assert_eq!(refused(&mut s, agent(), with(vec![option("a", "Atlas"), option("b", "atlas")], None)).1, Some("options.label".into()));
    assert_eq!(refused(&mut s, agent(), with(vec![option("a", &"x".repeat(41)), option("b", "B")], None)).1, Some("options.label".into()));
    assert_eq!(refused(&mut s, agent(), with(two(), Some("c"))).1, Some("card.recommended".into()));
    ok(&mut s, agent(), with(two(), Some("a")));
}

/// A label's text never decides a card: two labels that end in "(Recommended)" are words, and the
/// recommendation is `card.recommended`.
#[test]
fn a_label_that_reads_recommended_is_only_a_label() {
    let mut s = fixture();
    let mut card = decide("Pick a name");
    card.recommended = Some("a".into());
    ok(&mut s, agent(), Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: scope("t"), recipient: Recipient::User, body: "Which name?".into(),
        blocking: false, options: vec![option("a", "Atlas (Recommended)"), option("b", "Borealis (Recommended)")], card,
    });
}

#[test]
fn only_alondra_raises_a_problem() {
    let mut s = fixture();
    let problem = |id: &str| Command::RaiseProblem {
        attachments: vec![], interaction_id: id.into(), scope: scope("t"), body: "The launch failed".into(),
        options: vec![option("retry", "Retry"), option("cancel", "Cancel the task")],
        card: CardSpec::new(CardKind::Problem, "Decide what to do with the failed launch").why("The task has no node"),
    };
    assert_eq!(refused(&mut s, agent(), problem("x")).0, "forbidden");
    assert_eq!(refused(&mut s, user(), problem("x")).0, "forbidden");
    let disguised = CardSpec::new(CardKind::Problem, "Fix the build").why("It fails");
    assert_eq!(refused(&mut s, agent(), ask("x", "t", false, disguised)), ("forbidden".into(), Some("card.kind".into())));
    ok(&mut s, Actor::System, problem("p1"));
    assert_eq!(s.interactions[&"p1".into()].author, Author::System);
    ok(&mut s, orchestrator(), problem("p2"));
}

#[test]
fn closing_with_an_answer_tells_the_user() {
    let mut s = fixture();
    ok(&mut s, user(), Command::CreateRequest { request_id: "q-r".into(), body: "Is it in review?".into(), auto_merge: false, maintenance: false, message: false });
    let out = ok(&mut s, orchestrator(), Command::CloseRequest {
        request_id: "q-r".into(), answer: Some("Yes, since Monday".into()), withdrawn: false, title: Some("Review status".into()), headline: None, folded_into: None });
    let told: Vec<&Interaction> = s.interactions.values()
        .filter(|i| i.scope == Scope::Orchestrator { request_id: Some("q-r".into()) })
        .collect();
    assert_eq!(told.len(), 1);
    assert_eq!((told[0].card.kind, told[0].card.effective_title(), told[0].body.as_str()), (CardKind::Know, "Review status", "Yes, since Monday"));
    assert!(out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if interaction_id == &told[0].id)));
}

fn answer_cards(s: &DomainState, request: &str) -> usize {
    s.interactions.values()
        .filter(|i| i.recipient == Recipient::User && i.scope == Scope::Orchestrator { request_id: Some(request.into()) })
        .count()
}

/// 22 October: «¿a qué viene este card?» for a request a node had raised.
#[test]
fn closing_a_request_a_node_raised_tells_the_user_nothing() {
    let mut s = fixture();
    ok(&mut s, agent(), suggest("p"));
    ok(&mut s, orchestrator(), settle("p", new_task("t-readme", "r-readme")));
    assert_eq!(s.requests[&"r-readme".into()].origin, RequestOrigin::Node { agent_id: "a".into() });
    ok(&mut s, orchestrator(), Command::CancelTask { task_id: "t-readme".into(), reason: None });
    let out = ok(&mut s, orchestrator(), Command::CloseRequest {
        request_id: "r-readme".into(), answer: Some("Fixed by PR #1153".into()), withdrawn: false, title: None, headline: None, folded_into: None });
    assert_eq!(answer_cards(&s, "r-readme"), 0);
    assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "{:?}", out.effects);
    assert_eq!(s.requests[&"r-readme".into()].answer.as_deref(), Some("Fixed by PR #1153"));
}

#[test]
fn closing_a_request_the_orchestrator_raised_tells_the_user_nothing() {
    let mut s = fixture();
    ok(&mut s, orchestrator(), Command::CreateRequest { request_id: "req-measure".into(), body: "Measure the SDK usage".into(), auto_merge: false, maintenance: false, message: false });
    assert_eq!(s.requests[&"req-measure".into()].origin, RequestOrigin::Orchestrator);
    let out = ok(&mut s, orchestrator(), Command::CloseRequest {
        request_id: "req-measure".into(), answer: Some("make e2e-sdk passes again".into()), withdrawn: false, title: Some("SDK e2e".into()), headline: None, folded_into: None });
    assert_eq!(answer_cards(&s, "req-measure"), 0);
    assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "{:?}", out.effects);
    assert_eq!(s.requests[&"req-measure".into()].answer.as_deref(), Some("make e2e-sdk passes again"));
}

#[test]
fn closing_a_request_the_user_made_tells_the_user_its_answer() {
    let mut s = fixture();
    ok(&mut s, user(), Command::CreateRequest { request_id: "mine".into(), body: "Is main green?".into(), auto_merge: false, maintenance: false, message: false });
    assert_eq!(s.requests[&"mine".into()].origin, RequestOrigin::User);
    ok(&mut s, orchestrator(), Command::CloseRequest {
        request_id: "mine".into(), answer: Some("Yes".into()), withdrawn: false, title: None, headline: None, folded_into: None });
    assert_eq!(answer_cards(&s, "mine"), 1);
}

#[test]
fn a_request_stored_before_its_origin_reads_as_the_users() {
    let mut stored = serde_json::to_value(Request::new("old".into(), "Is main green?".into())).unwrap();
    stored.as_object_mut().unwrap().remove("origin");
    let request: Request = serde_json::from_value(stored).unwrap();
    assert_eq!(request.origin, RequestOrigin::User);
}

#[test]
fn withdraw_question_is_still_accepted_as_withdraw() {
    let command: Command = serde_json::from_value(serde_json::json!({"kind": "withdraw_question", "interaction_id": "q"})).unwrap();
    assert_eq!(command, Command::Withdraw { interaction_id: "q".into() });
}

#[test]
fn a_card_written_before_cards_is_read_from_its_body() {
    let mut command = serde_json::json!({"kind": "ask_question", "body": "Which name?\n\nThe release needs one.",
        "options": [{"id": "a", "label": "Atlas", "inputs": []}, {"id": "b", "label": "Borealis", "inputs": []}]})
        .as_object().unwrap().clone();
    derive_wire_card(&mut command);
    let card: CardSpec = serde_json::from_value(command["card"].clone()).unwrap();
    assert_eq!((card.kind, card.need.as_str(), card.why.as_str()), (CardKind::Decide, "Which name?", "The release needs one."));
    let mut said = serde_json::json!({"kind": "publish_information", "body": "Long text", "need": "Read the summary"})
        .as_object().unwrap().clone();
    derive_wire_card(&mut said);
    assert!(!said.contains_key("need"));
    assert_eq!(said["card"]["need"], "Read the summary");
}

fn suggest(id: &str) -> Command {
    Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: scope("t"), recipient: Recipient::Orchestrator,
        body: "The README still names the old flag".into(), blocking: false,
        card: CardSpec::new(CardKind::Propose, "Update the README's old flag").why("It is not part of this task"),
        options: suggestion_options(),
    }
}
fn settle(id: &str, decision: SuggestionDecision) -> Command {
    Command::SettleSuggestion { interaction_id: id.into(), decision }
}
fn discard(reason: &str) -> SuggestionDecision {
    SuggestionDecision::Discard { reason: reason.into() }
}
fn current(note: &str) -> SuggestionDecision {
    SuggestionDecision::CurrentTask { note: note.into() }
}

#[test]
fn a_suggestion_goes_to_the_orchestrator_and_never_to_the_users_inbox() {
    let mut s = fixture();
    let out = ok(&mut s, agent(), suggest("p"));
    assert!(out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyOrchestrator { interaction_id: Some(i), .. } if i.0 == "p")));
    assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "{:?}", out.effects);
    assert!(inbox_items(&s).iter().all(|item| item.id != "interaction:p"));
    assert_eq!(crate::views::inbox_badge(&ReadIndex::build(&s)), 0);
}

#[test]
fn only_a_node_suggests_and_only_to_the_orchestrator_with_its_answers() {
    let mut s = fixture();
    let Command::AskQuestion { card, .. } = suggest("p") else { unreachable!() };
    let to_user = Command::AskQuestion {
        attachments: vec![], interaction_id: "u".into(), scope: scope("t"), recipient: Recipient::User,
        body: "Update the README".into(), blocking: false, card: card.clone(), options: suggestion_options(),
    };
    assert_eq!(refused(&mut s, agent(), to_user.clone()), ("card_contract".into(), Some("recipient".into())));
    assert_eq!(refused(&mut s, orchestrator(), to_user), ("card_contract".into(), Some("author".into())));
    let mut own_answers = suggest("o");
    if let Command::AskQuestion { options, .. } = &mut own_answers {
        *options = vec![option("do_it", "Do it"), option("not_now", "Not now")];
    }
    assert_eq!(refused(&mut s, agent(), own_answers), ("invalid_input".into(), Some("options".into())));
    let mut blocking = suggest("b");
    if let Command::AskQuestion { blocking: b, .. } = &mut blocking {
        *b = true;
    }
    assert_eq!(refused(&mut s, agent(), blocking), ("invalid_input".into(), Some("blocking".into())));
}

#[test]
fn a_task_keeps_at_most_five_suggestions_waiting() {
    let mut s = fixture();
    for at in 1..=OPEN_SUGGESTIONS_MAX {
        ok(&mut s, agent(), suggest(&format!("p{at}")));
    }
    assert_eq!(refused(&mut s, agent(), suggest("p6")), ("too_many_suggestions".into(), Some("scope.task_id".into())));
    ok(&mut s, orchestrator(), settle("p1", discard("Task 12 already covers it")));
    ok(&mut s, agent(), suggest("p6"));
}

#[test]
fn the_orchestrators_answer_and_note_reach_the_node() {
    let mut s = fixture();
    ok(&mut s, agent(), suggest("p"));
    assert_eq!(refused(&mut s, user(), settle("p", current("Step 3"))).0, "forbidden");
    assert_eq!(refused(&mut s, agent(), settle("p", current("Step 3"))).0, "forbidden");
    let out = ok(&mut s, orchestrator(), settle("p", current("Step 3 of your plan covers it")));
    assert!(out.effects.iter().any(|e| matches!(e,
        DomainEffect::NotifyAgent { agent_id, interaction_id: Some(i), .. } if agent_id.0 == "a" && i.0 == "p")), "{:?}", out.effects);
    let InteractionPayload::Question { answer: Some(answer), .. } = &s.interactions[&"p".into()].payload else { panic!("unanswered") };
    assert_eq!((answer.option_id.as_str(), &answer.inputs[ANSWER_TEXT]), ("yours", &InputValue::Text("Step 3 of your plan covers it".into())));
    assert_eq!(answer.decision, Some(current("Step 3 of your plan covers it")));
}

/// A suggestion nobody settled stayed open after its task ended: the sweep skipped Propose cards,
/// so it kept showing in the orchestrator's pending list and in its author's open cards.
#[test]
fn a_task_that_ends_takes_the_suggestions_nobody_settled_with_it() {
    for cancelled in [false, true] {
        let mut s = fixture();
        ok(&mut s, agent(), suggest("waiting"));
        ok(&mut s, agent(), suggest("settled"));
        ok(&mut s, orchestrator(), settle("settled", discard("Task 12 already covers it")));
        if cancelled {
            ok(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None});
        } else {
            ok(&mut s, agent(), present("final", text(), true, review()));
            ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: text() });
        }
        ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
        assert!(s.tasks[&TaskId::from("t")].state.terminal());
        let waiting = &s.interactions[&"waiting".into()];
        assert!(waiting.withdrawn && !s.open(waiting), "cancelled={cancelled}: the suggestion leaves with its task");
        assert!(matches!(&waiting.payload, InteractionPayload::Question { answer: None, .. }), "the suggestion stays as it was asked");
        assert!(s.open_cards(&agent(), Some(&"t".into())).iter().all(|card| card.id.0 != "waiting"));
        let settled = &s.interactions[&"settled".into()];
        assert!(!settled.withdrawn && matches!(&settled.payload, InteractionPayload::Question { answer: Some(_), .. }));
        assert!(!s.interactions.keys().any(|id| id.0.starts_with(ENDED_UNANSWERED_PREFIX)), "the user is told nothing about a suggestion");
    }
}

fn new_task(task_id: &str, request_id: &str) -> SuggestionDecision {
    SuggestionDecision::NewTask {
        task_id: task_id.into(), request_id: request_id.into(), title: "Update the README flag".into(),
        launch: crate::domain::LaunchPlan::new("readme", "/tmp"), dependencies: BTreeSet::new(), note: None,
    }
}

/// 16 October: the orchestrator answered «it goes as a separate task» and no task ever existed.
#[test]
fn a_suggestion_that_becomes_a_task_creates_it_in_the_same_command() {
    let mut s = fixture();
    ok(&mut s, agent(), suggest("p"));
    let out = ok(&mut s, orchestrator(), settle("p", new_task("t-readme", "r-readme")));
    let created = &s.tasks[&"t-readme".into()];
    assert_eq!((created.request_id.0.as_str(), created.title.as_str()), ("r-readme", "Update the README flag"));
    assert_eq!(created.revision, s.revision, "the task and the answer are one commit");
    let request = &s.requests[&"r-readme".into()];
    assert_eq!(request.title.as_deref(), Some("Update the README flag"));
    assert!(request.body.contains("The README still names the old flag"), "{}", request.body);
    let InteractionPayload::Question { answer: Some(answer), .. } = &s.interactions[&"p".into()].payload else { panic!("unanswered") };
    assert_eq!((answer.option_id.as_str(), answer.decision.clone()), ("new_task", Some(new_task("t-readme", "r-readme"))));
    assert!(out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyAgent { interaction_id: Some(i), .. } if i.0 == "p")));
}

#[test]
fn a_suggestion_settles_once_and_never_by_words_alone() {
    let mut s = fixture();
    ok(&mut s, agent(), suggest("p"));
    let words = Command::AnswerQuestion {
        attachments: vec![], interaction_id: "p".into(), option_id: String::new(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("It goes as a separate task".into()))]),
    };
    assert_eq!(refused(&mut s, orchestrator(), words), ("suggestion_needs_decision".into(), Some("interaction_id".into())));
    for option in ["new_task", "yours", "discarded"] {
        let picked = Command::AnswerQuestion { attachments: vec![], interaction_id: "p".into(), option_id: option.into(), inputs: BTreeMap::new() };
        assert_eq!(refused(&mut s, orchestrator(), picked).0, "suggestion_needs_decision");
    }
    assert_eq!(refused(&mut s, orchestrator(), settle("p", discard("  "))).0, "invalid_input");
    assert_eq!(refused(&mut s, orchestrator(), settle("p", current(""))).0, "invalid_input");
    ok(&mut s, orchestrator(), settle("p", discard("Task 12 already covers it")));
    assert_eq!(refused(&mut s, orchestrator(), settle("p", new_task("t-2", "r-2"))).0, "question_already_answered");
    assert!(!s.tasks.contains_key(&"t-2".into()));
}

#[test]
fn a_new_task_that_cannot_be_created_leaves_the_suggestion_open() {
    let mut s = fixture();
    ok(&mut s, agent(), suggest("p"));
    let taken = refused(&mut s, orchestrator(), settle("p", new_task("t", "r-new")));
    assert_eq!(taken.0, "invalid_input", "{taken:?}");
    assert_eq!(refused(&mut s, orchestrator(), settle("p", new_task("t-new", "r"))).0, "invalid_input");
    let SuggestionDecision::NewTask { task_id, request_id, launch, dependencies, note, .. } = new_task("t-new", "r-new") else { unreachable!() };
    let long = SuggestionDecision::NewTask { task_id, request_id, title: "x".repeat(super::rules::REQUEST_TITLE_LIMIT + 1), launch, dependencies, note };
    assert_eq!(refused(&mut s, orchestrator(), settle("p", long)).0, "invalid_input");
    let InteractionPayload::Question { answer, .. } = &s.interactions[&"p".into()].payload else { unreachable!() };
    assert!(answer.is_none());
}

#[test]
fn a_task_that_ended_cannot_take_its_suggestion_in() {
    let mut s = fixture();
    ok(&mut s, agent(), suggest("p"));
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    assert_eq!(refused(&mut s, orchestrator(), settle("p", current("Step 3"))).0, "task_not_active");
    ok(&mut s, orchestrator(), settle("p", new_task("t-readme", "r-readme")));
}

#[test]
fn only_a_suggestion_is_settled() {
    let mut s = fixture();
    ok(&mut s, agent(), Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: scope("t"), recipient: Recipient::Orchestrator,
        body: "Which branch?".into(), blocking: true, card: decide("Pick the branch"), options: two(),
    });
    assert_eq!(refused(&mut s, orchestrator(), settle("q", discard("No"))).0, "invalid_action_for_kind");
}

#[test]
fn a_suggestion_holds_back_neither_the_delivery_nor_the_end_of_its_task() {
    let mut s = fixture();
    ok(&mut s, agent(), suggest("p"));
    assert!(!s.unanswered(&"t".into()));
    ok(&mut s, agent(), present("review", text(), true, review()));
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    ok(&mut s, orchestrator(), settle("p", discard("Task 12 already covers it")));
}

#[test]
fn no_pending_card_is_left_with_nothing_the_user_can_do_once_its_task_ends() {
    let mut s = fixture();
    let with_verify = |kind: CardKind, need: &str| {
        let mut card = CardSpec::new(kind, need).why("Because");
        card.verify = Some("I check it".into());
        card
    };
    ok(&mut s, agent(), present("review", text(), true, review()));
    ok(&mut s, agent(), ask("decide", "t", false, decide("Pick a name")));
    ok(&mut s, agent(), ask("test", "t", false, CardSpec::new(CardKind::Test, "Try the build").why("Because")));
    ok(&mut s, agent(), ask("provide", "t", false, CardSpec::new(CardKind::Provide, "Send the key").why("Because")));
    ok(&mut s, agent(), Command::RequestAction {
        attachments: vec![], interaction_id: "do".into(), scope: scope("t"), recipient: Recipient::User,
        body: "Log in".into(), blocking: false, options: two(), card: with_verify(CardKind::Do, "Log in"),
    });
    ok(&mut s, Actor::System, inform("know", scope("t"), "Heads up"));
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    let items = inbox_items(&s);
    assert!(items.iter().any(|item| item.pending), "the notice remains");
    for item in items.iter().filter(|item| item.pending) {
        assert!(
            item.available_actions.iter().any(|action| action.enabled),
            "{} is shown as pending with every action disabled: {:?}",
            item.id, item.available_actions.iter().map(|action| action.disabled_reason.clone()).collect::<Vec<_>>()
        );
    }
    let mut pending: Vec<&str> = items.iter().filter(|item| item.pending).map(|item| item.id.as_str()).collect();
    pending.sort();
    assert_eq!(pending, ["interaction:know"],
        "a card nobody can answer leaves the inbox in silence: {pending:?}");
    for card in ["decide", "test", "provide", "do"] {
        assert!(s.interactions[&card.into()].withdrawn, "{card} is withdrawn, not left open");
    }
}

// F2: the inbox is one item per interaction, in the order the user should meet them.

fn selected_inbox(s: &DomainState) -> Vec<InboxItem> {
    inbox_items(s)
}

#[test]
fn inbox_selection_keeps_user_cards_current_final_delivery_and_action_guards() {
    let mut s = fixture();
    ok(&mut s, Actor::System, inform("z-notice", scope("t"), "First notice"));
    let notice_order = s.revision;
    let mut private = inform("private", scope("t"), "For the node");
    if let Command::PublishInformation { recipient, .. } = &mut private {
        *recipient = Recipient::Agent { agent_id: "a".into() };
    }
    ok(&mut s, orchestrator(), private);
    ok(&mut s, agent(), present("partial", text(), false, review()));
    ok(&mut s, agent(), present("final", text(), true, review()));
    let final_order = s.revision;
    ok(&mut s, orchestrator(), inform("a-notice", Scope::Orchestrator { request_id: None }, "Second notice"));
    let revision = s.revision;
    let before = s.clone();
    let inbox = selected_inbox(&s);
    assert_eq!(inbox.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        ["interaction:final", "interaction:z-notice", "interaction:a-notice"]);
    assert_eq!(inbox[0].order, final_order);
    assert_eq!(inbox[1].order, notice_order);
    assert_eq!(inbox[0].body.as_deref(), Some("Result ready"));
    assert_eq!(inbox[1].body.as_deref(), Some("First notice"));
    assert_eq!(inbox[0].task_id, Some("t".into()));
    assert_eq!(inbox[0].request_id, Some("r".into()));
    assert_eq!(inbox[0].agent_id, Some("a".into()));
    assert!(inbox[0].pending);
    assert!(inbox[0].available_actions.iter().any(|a| a.kind == "accept_delivery" && a.enabled
        && a.presentation_id == Some("final".into()) && a.expected_revision == revision));
    assert!(inbox[1].available_actions.iter().any(|a| a.kind == "mark_read" && a.enabled
        && a.expected_revision == revision));
    assert_eq!(s, before, "reading the inbox changes nothing");
    ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: text() });
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    ok(&mut s, user(), Command::ArchiveTask { task_id: "t".into(), archived: true });
    assert_eq!(selected_inbox(&s).iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["interaction:a-notice"]);
    ok(&mut s, user(), Command::ArchiveTask { task_id: "t".into(), archived: false });
    assert_eq!(inbox_ids(&s), ["final", "z-notice", "a-notice"]);
    assert!(!selected_inbox(&s)[0].pending);
}

#[test]
fn inbox_equal_order_uses_id_after_rank_and_excludes_other_recipients() {
    let mut s = fixture();
    for id in ["z", "a"] {
        ok(&mut s, Actor::System, inform(id, scope("t"), id));
    }
    let mut private = inform("orchestrator", scope("t"), "For the orchestrator");
    if let Command::PublishInformation { recipient, .. } = &mut private {
        *recipient = Recipient::Orchestrator;
    }
    ok(&mut s, agent(), private);
    for id in ["z", "a"] {
        s.interactions.get_mut(&InteractionId::from(id)).unwrap().order = 4;
    }
    s.validate().unwrap();
    assert_eq!(inbox_ids(&s), ["a", "z"]);
    assert_eq!(selected_inbox(&s).iter().map(|i| i.order).collect::<Vec<_>>(), [4, 4]);
}

fn ordered_inbox(s: &DomainState) -> Vec<(String, Revision)> {
    s.inbox_orders()
}

#[test]
fn inbox_multiple_blockers_keep_server_order_before_and_after_an_answer() {
    let mut s = fixture();
    register_pr(&mut s);
    ok(&mut s, agent(), present("d", pr_target(), true, review()));
    let delivery_order = s.revision;
    let mut revisions = vec![];
    for id in ["q1", "q2"] {
        let mut card = decide("Keep the filter?");
        card.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
        ok(&mut s, agent(), ask(id, "t", false, card));
        revisions.push(s.revision);
    }
    let before = s.clone();
    assert_eq!(ordered_inbox(&s), vec![
        ("interaction:q1".into(), revisions[0]), ("interaction:q2".into(), revisions[1]),
        ("interaction:d".into(), delivery_order),
    ]);
    let d = selected_inbox(&s).into_iter().find(|i| i.id == "interaction:d").unwrap();
    assert_eq!(d.blocked_by, vec![InteractionId::from("q1"), InteractionId::from("q2")]);
    assert!(d.available_actions.iter().filter(|a| a.kind == "accept_delivery" || a.kind == "authorize_merge").all(|a| !a.enabled));
    let index = ReadIndex::build(&s);
    let inbox = serde_json::json!({"inbox": inbox_items(&s), "counts": index.counts()});
    assert_eq!(s, before, "reading the order and the inbox changes nothing");
    let expected: serde_json::Value = serde_json::from_str(include_str!("../../fixtures/inbox_order.json")).unwrap();
    assert_eq!(inbox, expected);
    ok(&mut s, user(), answer("q1"));
    assert_eq!(ordered_inbox(&s), vec![
        ("interaction:q2".into(), revisions[1]), ("interaction:d".into(), delivery_order),
        ("interaction:q1".into(), revisions[0]),
    ]);
}

fn inbox_ids(s: &DomainState) -> Vec<String> {
    ordered_inbox(s).iter().map(|(id, _)| id.trim_start_matches("interaction:").to_owned()).collect()
}
fn register_pr_of(s: &mut DomainState, actor: Actor, task: &str, pr: &str, number: u64) {
    ok(s, actor, Command::RegisterPr {
        task_id: task.into(), pr_id: pr.into(), host: "github.com".into(), repository: "owner/repo".into(),
        number, head: format!("head-{pr}"), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}
fn present_pr(s: &mut DomainState, actor: Actor, task: &str, id: &str, pr: &str) {
    ok(s, actor, Command::PresentResult {
        checklist: Vec::new(),
        attachments: vec![], task_id: task.into(), interaction_id: id.into(), body: "Result ready".into(),
        targets: vec![DeliveryTarget::PullRequest { pr_id: pr.into(), head: format!("head-{pr}") }],
        is_final: true, nothing_to_review: false, card: review(),
    });
}

#[test]
fn the_inbox_has_one_item_per_interaction_and_never_groups_by_pr_or_document() {
    let mut s = fixture();
    register_pr(&mut s);
    ok(&mut s, agent(), present("d", pr_target(), true, review()));
    let mut blocking = decide("Pick a name");
    blocking.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
    ok(&mut s, agent(), ask("q", "t", true, blocking));
    ok(&mut s, Actor::System, inform("n", scope("t"), "Heads up"));
    assert_eq!(inbox_ids(&s), ["q", "d", "n"], "a question, the delivery of the PR, a notice: three cards");
    assert_eq!(ReadIndex::build(&s).counts().roots, 3);
    for item in &inbox_items(&s) {
        assert!(item.id.starts_with("interaction:"), "{}: no PR or document root", item.id);
        assert!(matches!(item.subject, InboxSubject::Interaction { .. }));
        assert!(!matches!(item.kind.as_str(), "pull_request" | "document_review"));
    }
}

#[test]
fn the_inbox_runs_problem_then_blocking_asks_then_review_provide_know_oldest_first() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    task(&mut s, "t2", "b");
    ok(&mut s, Actor::System, inform("know-old", scope("t"), "The first notice"));
    ok(&mut s, agent(), ask("provide", "t", false,
        CardSpec::new(CardKind::Provide, "Send a screenshot of the bug").why("I cannot reproduce it")));
    ok(&mut s, Actor::System, inform("know-new", scope("t"), "The second notice"));
    register_agent(&mut s, "c");
    task(&mut s, "t3", "c");
    ok(&mut s, Actor::Agent { agent_id: "c".into() }, Command::PresentResult {
        checklist: Vec::new(),
        attachments: vec![], task_id: "t3".into(), interaction_id: "review".into(), body: "Done".into(),
        targets: text(), is_final: true, nothing_to_review: false, card: review(),
    });
    ok(&mut s, Actor::System, Command::HoldHook {
        interaction_id: "permission".into(), task_id: "t".into(), agent_id: "a".into(), hold: Hold::Permission,
        body: "Bash: rm -rf build".into(), options: Hold::Permission.options(),
        card: CardSpec::new(CardKind::Authorize, "Allow cleaning the build").content(ContentKind::Command, "rm -rf build"),
    });
    ok(&mut s, agent(), ask("decide", "t", true, decide("Pick a name")));
    let mut login = CardSpec::new(CardKind::Do, "Log in to App Store Connect").why("The upload needs a session");
    login.verify = Some("I run asc whoami".into());
    ok(&mut s, agent_b(), Command::RequestAction {
        attachments: vec![], interaction_id: "do".into(), scope: scope("t2"), recipient: Recipient::User,
        body: "Log in".into(), blocking: true, options: vec![], card: login,
    });
    ok(&mut s, Actor::System, Command::RaiseProblem {
        attachments: vec![], interaction_id: "problem".into(), scope: scope("t"), body: "The launch failed".into(),
        options: vec![option("retry", "Retry"), option("cancel", "Cancel the task")],
        card: CardSpec::new(CardKind::Problem, "Decide what to do with the failed launch").why("The task has no node"),
    });
    assert_eq!(
        inbox_ids(&s),
        ["problem", "permission", "decide", "do", "review", "provide", "know-old", "know-new"],
        "problem, what holds a node up (oldest first), review, provide, notices (oldest first)"
    );
}

#[test]
fn a_card_that_blocks_sits_right_above_what_it_blocks() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    task(&mut s, "t2", "b");
    ok(&mut s, agent_b(), Command::PresentResult {
        checklist: Vec::new(),
        attachments: vec![], task_id: "t2".into(), interaction_id: "elsewhere".into(), body: "Done".into(),
        targets: text(), is_final: true, nothing_to_review: false, card: review(),
    });
    register_pr(&mut s);
    ok(&mut s, agent(), present("d", pr_target(), true, review()));
    let mut asks = decide("Keep the filter?");
    asks.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
    ok(&mut s, agent(), ask("q", "t", false, asks));
    assert_eq!(inbox_ids(&s), ["elsewhere", "q", "d"], "the question comes out above the PR it holds back, not after it");
    let inbox = inbox_items(&s);
    let item = |id: &str| inbox.iter().find(|i| i.id == format!("interaction:{id}")).unwrap();
    assert_eq!(item("d").blocked_by, vec![InteractionId::from("q")]);
    assert_eq!(item("q").blocks, vec![InteractionId::from("d")]);
    ok(&mut s, user(), answer("q"));
    assert!(inbox_items(&s).iter().all(|i| i.blocked_by.is_empty()), "answered, it holds nothing back");
}

#[test]
fn a_stacked_pr_is_blocked_by_the_delivery_it_sits_on() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    task(&mut s, "t2", "b");
    register_pr_of(&mut s, agent_b(), "t2", "pr2", 8);
    present_pr(&mut s, agent_b(), "t2", "above", "pr2");
    register_pr_of(&mut s, agent(), "t", "pr", 7);
    present_pr(&mut s, agent(), "t", "below", "pr");
    s.tasks.get_mut(&TaskId::from("t2")).unwrap().stack = Some(StackDependency {
        upstream_task: "t".into(), presentation_id: "below".into(), pr_id: "pr".into(), head: "head-pr".into(),
    });
    let inbox = inbox_items(&s);
    let item = |id: &str| inbox.iter().find(|i| i.id == format!("interaction:{id}")).unwrap();
    assert_eq!(item("above").blocked_by, vec![InteractionId::from("below")]);
    assert!(item("below").blocked_by.is_empty());
    assert_eq!(inbox_ids(&s), ["below", "above"], "the PR below comes first even though the one above is older");
}

fn notice(id: &str, need: &str, replaces: Option<&str>) -> Command {
    let mut card = CardSpec::new(CardKind::Know, need);
    card.replaces = replaces.map(Into::into);
    Command::PublishInformation {
        attachments: vec![], interaction_id: id.into(), scope: scope("t"), recipient: Recipient::User, body: "body".into(), card,
    }
}

#[test]
fn a_correction_replaces_the_earlier_notice() {
    let mut s = fixture();
    ok(&mut s, Actor::System, notice("n1", "Build is green", None));
    ok(&mut s, Actor::System, notice("n2", "Correction: build is red", Some("n1")));
    assert!(s.interactions[&"n1".into()].withdrawn);
    assert!(s.open(&s.interactions[&"n2".into()]));
}

fn waiting_ids(s: &DomainState) -> Vec<String> {
    inbox_items(&s).iter().filter(|item| item.pending).map(|item| item.id.trim_start_matches("interaction:").to_owned()).collect()
}
fn gq(need: &str, labels: &[&str]) -> GroupQuestion {
    GroupQuestion {
        title: Some("Release choices".into()),
        need: need.into(), why: "The release needs it".into(), recommended: None,
        options: labels.iter().enumerate().map(|(n, label)| option(&format!("o{n}"), label)).collect(),
    }
}
fn asks(questions: Vec<GroupQuestion>) -> Command {
    Command::AskQuestions {
        interaction_id: "g".into(), scope: scope("t"), recipient: Recipient::User, blocking: true,
        why: "Three things for the release".into(), questions, language: None,
    }
}
fn three() -> Vec<GroupQuestion> {
    vec![gq("Which name?", &["Atlas", "Borealis"]), gq("Ship it today?", &["Yes", "No"]), gq("Which channel?", &["Beta", "Stable"])]
}
fn answer_card(card: &str, option: &str) -> Command {
    Command::AnswerQuestion { attachments: vec![], interaction_id: card.into(), option_id: option.into(), inputs: BTreeMap::new() }
}
fn told_the_node(outcome: &CommandOutcome) -> usize {
    outcome.effects.iter().filter(|effect| matches!(effect, DomainEffect::NotifyAgent { .. })).count()
}

#[test]
fn several_questions_are_one_card_each_in_order_and_linked_as_a_group() {
    let mut s = fixture();
    let outcome = ok(&mut s, agent(), asks(three()));
    assert_eq!(outcome.created_ids, ["g-q1", "g-q2", "g-q3"]);
    for (at, need) in ["Which name?", "Ship it today?", "Which channel?"].into_iter().enumerate() {
        let card = &s.interactions[&format!("g-q{}", at + 1).as_str().into()].card;
        assert_eq!((card.kind, card.need.as_str()), (CardKind::Decide, need));
        assert_eq!(card.group, Some(QuestionGroup { id: "g".into(), index: at as u8 + 1, total: 3 }));
    }
    assert_eq!(waiting_ids(&s), ["g-q1", "g-q2", "g-q3"], "adjacent and in question order");
}

#[test]
fn a_group_gives_every_question_the_language_its_author_declared() {
    let mut s = fixture();
    let mut command = asks(three());
    if let Command::AskQuestions { language, .. } = &mut command {
        *language = Some(alondra_foundation::Lang::Es);
    }
    ok(&mut s, agent(), command);
    for card in ["g-q1", "g-q2", "g-q3"] {
        assert_eq!(s.interactions[&card.into()].card.language, Some(alondra_foundation::Lang::Es));
    }
}

#[test]
fn a_card_stored_before_the_language_field_decodes_without_one_and_keeps_its_bytes() {
    let old = r#"{"kind":"decide","title":"Release design","need":"Choose a design","why":"The release needs one"}"#;
    let card: CardSpec = serde_json::from_str(old).unwrap();
    assert_eq!(card.language, None);
    assert!(!serde_json::to_string(&card).unwrap().contains("language"));
    let declared: CardSpec = serde_json::from_str(&old.replace("}", r#","language":"es"}"#)).unwrap();
    assert_eq!(declared.language, Some(alondra_foundation::Lang::Es));
}

#[test]
fn a_group_is_two_to_four_questions_that_each_stand_on_their_own() {
    let mut s = fixture();
    assert_eq!(refused(&mut s, agent(), asks(vec![gq("Which name?", &["Atlas", "Borealis"])])).1, Some("questions".into()));
    let five = (0..5).map(|n| gq(&format!("Question {n}?"), &["Yes", "No"])).collect();
    assert_eq!(refused(&mut s, agent(), asks(five)).1, Some("questions".into()));
    assert_eq!(refused(&mut s, agent(), asks(vec![gq("Which name?", &["Atlas", "Borealis"]), gq("", &["Yes", "No"])])).1, Some("card.need".into()));
    assert_eq!(refused(&mut s, agent(), asks(vec![gq("Which name?", &["Atlas", "Borealis"]), gq("Ship it?", &["Yes"])])).1, Some("options".into()));
    let mut wrong = gq("Ship it?", &["Yes", "No"]);
    wrong.recommended = Some("o9".into());
    assert_eq!(refused(&mut s, agent(), asks(vec![gq("Which name?", &["Atlas", "Borealis"]), wrong])).1, Some("card.recommended".into()));
}

#[test]
fn a_group_is_one_blocking_ask_of_its_node() {
    let mut s = fixture();
    ok(&mut s, agent(), asks(three()));
    let mut another = asks(three());
    if let Command::AskQuestions { interaction_id, .. } = &mut another {
        *interaction_id = "h".into();
    }
    assert_eq!(refused(&mut s, agent(), another).0, "ask_already_open");
}

#[test]
fn the_node_is_told_only_when_the_last_question_is_answered() {
    let mut s = fixture();
    ok(&mut s, agent(), asks(three()));
    let first = ok(&mut s, user(), answer_card("g-q1", "o1"));
    assert_eq!(told_the_node(&first), 0, "one answer of three is not yet the answer");
    let second = ok(&mut s, user(), answer_card("g-q3", "o0"));
    assert_eq!(told_the_node(&second), 0, "nor is two, whichever they are");
    let last = ok(&mut s, user(), answer_card("g-q2", "o0"));
    assert_eq!(told_the_node(&last), 1, "the last one tells it, once");
}

#[test]
fn an_answer_can_be_taken_back_until_the_node_has_them_all() {
    let mut s = fixture();
    ok(&mut s, agent(), asks(three()));
    ok(&mut s, user(), answer_card("g-q1", "o1"));
    ok(&mut s, user(), answer_card("g-q2", "o0"));
    assert_eq!(waiting_ids(&s), ["g-q3"], "answered questions leave the stack");
    ok(&mut s, user(), Command::ReopenAnswer { interaction_id: "g-q1".into() });
    assert_eq!(waiting_ids(&s), ["g-q1", "g-q3"], "the reopened card is back in its place");
    let again = ok(&mut s, user(), answer_card("g-q1", "o0"));
    assert_eq!(told_the_node(&again), 0);
    let last = ok(&mut s, user(), answer_card("g-q3", "o1"));
    assert_eq!(told_the_node(&last), 1);
    let refusal = refused(&mut s, user(), Command::ReopenAnswer { interaction_id: "g-q1".into() });
    assert_eq!(refusal.0, "question_already_answered", "the node has them: they are final");
    assert_eq!(refused(&mut s, agent(), Command::ReopenAnswer { interaction_id: "g-q1".into() }).0, "forbidden", "only the user takes an answer back");
}

#[test]
fn only_an_answered_question_of_a_group_can_be_reopened() {
    let mut s = fixture();
    ok(&mut s, agent(), asks(three()));
    assert_eq!(refused(&mut s, user(), Command::ReopenAnswer { interaction_id: "g-q1".into() }).0, "invalid_action_for_kind");
    ok(&mut s, agent(), ask("alone", "t", false, decide("Pick a name")));
    ok(&mut s, user(), answer("alone"));
    assert_eq!(refused(&mut s, user(), Command::ReopenAnswer { interaction_id: "alone".into() }).0, "invalid_action_for_kind");
}

#[test]
fn withdrawing_one_question_withdraws_the_group_and_discards_the_answers() {
    let mut s = fixture();
    ok(&mut s, agent(), asks(three()));
    ok(&mut s, user(), answer_card("g-q1", "o1"));
    ok(&mut s, agent(), Command::Withdraw { interaction_id: "g-q2".into() });
    assert!(["g-q1", "g-q2", "g-q3"].iter().all(|id| s.interactions[&(*id).into()].withdrawn), "all three go together, answered or not");
    assert!(waiting_ids(&s).is_empty());
    let told = exec(&mut s, user(), answer_card("g-q3", "o0"));
    assert!(told.is_err(), "a withdrawn question cannot be answered");
}

#[test]
fn a_finished_group_cannot_be_withdrawn() {
    let mut s = fixture();
    ok(&mut s, agent(), asks(three()));
    for id in ["g-q1", "g-q2", "g-q3"] {
        ok(&mut s, user(), answer_card(id, "o0"));
    }
    assert_eq!(refused(&mut s, agent(), Command::Withdraw { interaction_id: "g-q1".into() }).0, "question_already_answered");
}

#[test]
fn a_recommended_mark_in_a_label_is_taken_off_it() {
    assert_eq!(without_recommended_suffix("LAN and Tailscale (Recommended)").as_deref(), Some("LAN and Tailscale"));
    assert_eq!(without_recommended_suffix("Atlas (recommended)  ").as_deref(), Some("Atlas"));
    assert_eq!(without_recommended_suffix("Atlas"), None);
    assert_eq!(without_recommended_suffix("(Recommended)"), None, "a label cannot be only the mark");
    assert_eq!(without_recommended_suffix("é(Recommended"), None);
}

#[test]
fn only_alondra_wakes_the_orchestrator_with_information() {
    let mut s = fixture();
    let to_orchestrator = |id: &str, scope: Scope| {
        let mut command = inform(id, scope, "Machine at its limit");
        if let Command::PublishInformation { recipient, .. } = &mut command {
            *recipient = Recipient::Orchestrator;
        }
        command
    };
    let from_node = ok(&mut s, agent(), to_orchestrator("progress", scope("t")));
    assert!(!from_node.effects.iter().any(|e| matches!(e, DomainEffect::NotifyOrchestrator { .. })), "a node's progress does not wake it");
    let from_alondra = ok(&mut s, Actor::System, to_orchestrator("machine", Scope::Orchestrator { request_id: None }));
    assert!(matches!(&from_alondra.effects[..], [DomainEffect::NotifyOrchestrator { interaction_id: Some(id), request_id: None }] if id.0 == "machine"));
}

fn held_plan(id: &str, agent: &str, title: &str) -> Command {
    Command::HoldHook {
        interaction_id: id.into(), task_id: "t".into(), agent_id: agent.into(), hold: Hold::Plan,
        body: format!("# {title}\n\n- [ ] Do it"), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, title).content(ContentKind::Plan, format!("# {title}\n\n- [ ] Do it")),
    }
}
fn waiting_for_user(s: &DomainState) -> Vec<String> {
    inbox_items(&s).iter().filter(|item| item.pending).map(|item| item.id.clone()).collect()
}

#[test]
fn a_plan_sent_back_with_changes_leaves_only_the_new_plan_waiting() {
    let mut s = fixture();
    ok(&mut s, Actor::System, held_plan("p1", "a", "Weekly cycle plan"));
    let inputs = BTreeMap::from([("text".to_owned(), InputValue::Text("Drop the plates idea".into()))]);
    ok(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "p1".into(), option_id: Hold::REQUEST_CHANGES.into(), inputs });
    ok(&mut s, Actor::System, held_plan("p2", "a", "Weekly cycle plan"));
    assert_eq!(waiting_for_user(&s), vec!["interaction:p2".to_owned()]);
}

#[test]
fn a_plan_from_the_task_s_next_node_replaces_the_one_its_old_node_left_open() {
    let mut s = fixture();
    ok(&mut s, Actor::System, held_plan("p1", "a", "Weekly cycle plan"));
    register_agent(&mut s, "b");
    ok(&mut s, user(), Command::AssignTask { task_id: "t".into(), agent_id: "b".into(), model: None });
    ok(&mut s, Actor::System, held_plan("p2", "b", "Weekly cycle plan"));
    assert!(s.interactions[&"p1".into()].withdrawn);
    assert_eq!(waiting_for_user(&s), vec!["interaction:p2".to_owned()]);
}

#[test]
fn a_plan_on_another_task_is_left_alone() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    task(&mut s, "t2", "b");
    ok(&mut s, Actor::System, held_plan("p1", "a", "Weekly cycle plan"));
    let mut other = held_plan("p2", "b", "Another plan");
    if let Command::HoldHook { task_id, .. } = &mut other {
        *task_id = "t2".into();
    }
    ok(&mut s, Actor::System, other);
    assert!(s.open(&s.interactions[&"p1".into()]));
    assert!(s.open(&s.interactions[&"p2".into()]));
}

#[test]
fn a_new_final_result_retires_the_one_waiting() {
    let mut s = fixture();
    ok(&mut s, agent(), present("d1", text(), true, review()));
    ok(&mut s, agent(), present("d2", text(), true, review()));
    assert!(s.interactions[&"d1".into()].withdrawn);
    assert_eq!(waiting_for_user(&s), vec!["interaction:d2".to_owned()]);
}

fn problem(id: &str, cause: Option<&str>) -> Command {
    let card = CardSpec::new(CardKind::Problem, "Alondra can't merge PR #7").why("GitHub keeps failing");
    Command::RaiseProblem {
        attachments: vec![], interaction_id: id.into(), scope: scope("t"), body: "Last error: 502".into(), options: vec![],
        card: CardSpec { cause: cause.map(str::to_owned), ..card },
    }
}

#[test]
fn a_problem_with_the_same_cause_replaces_the_open_one() {
    let mut s = fixture();
    ok(&mut s, Actor::System, problem("m1", Some("merge:pr")));
    ok(&mut s, Actor::System, problem("m2", Some("merge:pr")));
    assert!(s.interactions[&"m1".into()].withdrawn);
    assert_eq!(waiting_for_user(&s), vec!["interaction:m2".to_owned()]);
}

#[test]
fn problems_with_other_causes_or_none_stay_side_by_side() {
    let mut s = fixture();
    ok(&mut s, Actor::System, problem("m1", Some("merge:pr")));
    ok(&mut s, Actor::System, problem("c1", Some("checks:pr")));
    ok(&mut s, Actor::System, problem("x1", None));
    ok(&mut s, Actor::System, problem("x2", None));
    assert_eq!(waiting_for_user(&s).len(), 4);
}

#[test]
fn only_a_problem_names_a_cause() {
    let mut s = fixture();
    let mut card = decide("Pick a name");
    card.cause = Some("naming".into());
    assert_eq!(refused(&mut s, agent(), ask("q", "t", false, card)), ("invalid_input".into(), Some("card.cause".into())));
    assert_eq!(refused(&mut s, Actor::System, problem("m", Some(" "))), ("invalid_input".into(), Some("card.cause".into())));
}

#[test]
fn open_cards_lists_what_the_reader_may_replace() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    task(&mut s, "t2", "b");
    ok(&mut s, agent(), ask("q1", "t", false, decide("Pick a name")));
    ok(&mut s, agent(), ask("q2", "t", false, decide("Pick a color")));
    ok(&mut s, user(), answer("q2"));
    ok(&mut s, agent_b(), ask("other", "t2", false, decide("Pick a name")));
    ok(&mut s, orchestrator(), ask("o1", "t", false, decide("Pick an icon")));
    let ids = |actor: Actor, task: Option<&str>| -> Vec<String> {
        s.open_cards(&actor, task.map(TaskId::from).as_ref()).iter().map(|i| i.id.0.clone()).collect()
    };
    assert_eq!(ids(agent(), Some("t")), vec!["q1"]);
    assert_eq!(ids(agent_b(), Some("t")), Vec::<String>::new());
    assert_eq!(ids(orchestrator(), None), vec!["o1"]);
    assert_eq!(ids(user(), Some("t")), Vec::<String>::new());
}

fn nothing_to_review(id: &str, targets: Vec<DeliveryTarget>, is_final: bool) -> Command {
    match present(id, targets, is_final, review()) {
        Command::PresentResult { attachments, task_id, interaction_id, body, targets, is_final, card, .. } =>
            Command::PresentResult { checklist: Vec::new(), attachments, task_id, interaction_id, body, targets, is_final, nothing_to_review: true, card },
        _ => unreachable!(),
    }
}

#[test]
fn a_final_delivery_with_nothing_to_review_closes_the_task_without_a_card() {
    let mut s = fixture();
    ok(&mut s, agent(), nothing_to_review("empty", text(), true));
    assert_eq!(open_reviews(&s), 0, "nothing to review puts no card in front of the user");
    assert!(s.interactions.values().all(|i| !s.open(i)));
    assert!(matches!(s.tasks[&TaskId::from("t")].state, TaskState::Closing | TaskState::Done), "the task ends as if accepted");
}

#[test]
fn nothing_to_review_is_refused_for_a_notice_a_pr_or_a_document() {
    let mut s = fixture();
    assert_eq!(refused(&mut s, agent(), nothing_to_review("notice", text(), false)).1.as_deref(), Some("nothing_to_review"));
    register_pr(&mut s);
    assert_eq!(refused(&mut s, agent(), nothing_to_review("pr", pr_target(), true)).1.as_deref(), Some("nothing_to_review"));
}

// Frozen outgoing-list scan: expected membership never calls the optimized target predicate.
fn expanded_blocker_scan(state: &DomainState, delivery: &InteractionId) -> Vec<InteractionId> {
    let mut blockers: Vec<&Interaction> = state.interactions.values().filter(|item| {
        if !state.open(item) { return false; }
        let mut outgoing = vec![];
        for target in &item.card.blocks {
            for candidate in state.interactions.values().filter(|candidate| state.open(candidate)) {
                let InteractionPayload::Delivery { targets, .. } = &candidate.payload else { continue };
                let hit = match target {
                    CardTarget::Delivery { interaction_id } => interaction_id == &candidate.id,
                    CardTarget::PullRequest { pr_id } => targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { pr_id: known, .. } if known == pr_id)),
                    CardTarget::Document { document_id } => targets.iter().any(|target| matches!(target, DeliveryTarget::DocumentRevision { document_id: known, .. } if known == document_id)),
                };
                if hit && candidate.id != item.id { outgoing.push(candidate.id.clone()); }
            }
        }
        outgoing.contains(delivery)
    }).collect();
    blockers.sort_by_key(|item| item.order);
    blockers.into_iter().map(|item| item.id.clone()).collect()
}

#[test]
fn delivery_blocker_queries_match_all_target_kinds_once_and_keep_equal_order_ties() {
    let mut s = fixture();
    register_pr(&mut s);
    ok(&mut s, agent(), Command::CreateDocument {
        task_id: "t".into(), document_id: "doc".into(), title: "Synthetic document".into(), revision_id: "revision".into(),
        content_digest: "a".repeat(64), content_reference: "library-file:synthetic/document.txt".into(), role: "implementation".into(),
    });
    let targets = vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() },
        DeliveryTarget::DocumentRevision { document_id: "doc".into(), revision_id: "revision".into() }];
    ok(&mut s, agent(), present("d", targets, true, review()));
    for id in ["q-z", "q-a"] {
        let mut card = decide("Choose the release name");
        card.blocks = vec![CardTarget::Document { document_id: "doc".into() },
            CardTarget::PullRequest { pr_id: "pr".into() }, CardTarget::Delivery { interaction_id: "d".into() }];
        ok(&mut s, agent(), ask(id, "t", false, card));
    }
    for id in ["q-z", "q-a"] { s.interactions.get_mut(&InteractionId::from(id)).unwrap().order = s.revision; }
    s.validate().unwrap();
    assert_eq!(s.blocked_by(&"d".into()), [InteractionId::from("q-a"), InteractionId::from("q-z")]);
    assert_eq!(s.blocked_deliveries(&s.interactions[&"q-z".into()]), [InteractionId::from("d"), InteractionId::from("d"), InteractionId::from("d")]);
    for id in ["d", "q-z", "q-a", "missing"] {
        assert_eq!(s.blocked_by(&id.into()), expanded_blocker_scan(&s, &id.into()), "query {id}");
    }
    ok(&mut s, user(), answer("q-a"));
    assert_eq!(s.blocked_by(&"d".into()), [InteractionId::from("q-z")]);
    assert_eq!(s.blocked_by(&"d".into()), expanded_blocker_scan(&s, &"d".into()));
}

#[test]
fn delivery_blocker_queries_exclude_self_and_closed_query_targets() {
    let mut s = fixture();
    register_pr(&mut s);
    let mut card = review();
    card.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
    ok(&mut s, agent(), present("d", pr_target(), true, card));
    assert!(s.blocked_by(&"d".into()).is_empty());
    assert!(s.blocked_deliveries(&s.interactions[&"d".into()]).is_empty());
    let mut card = decide("Choose the release name");
    card.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
    ok(&mut s, agent(), ask("q", "t", false, card));
    assert_eq!(s.blocked_by(&"d".into()), [InteractionId::from("q")]);
    assert_eq!(s.blocked_by(&"d".into()), expanded_blocker_scan(&s, &"d".into()));
    ok(&mut s, agent(), Command::Withdraw { interaction_id: "d".into() });
    assert!(s.open(&s.interactions[&"q".into()]));
    assert!(!s.open(&s.interactions[&"d".into()]));
    assert!(s.blocked_by(&"d".into()).is_empty());
    assert_eq!(s.blocked_by(&"d".into()), expanded_blocker_scan(&s, &"d".into()));
}

/// Words never decide a card. Stopping for the subscription's usage is still never a node's call,
/// but the rule lives in the skill and the tool's description: a card that reads like it, or a
/// question about another quota, reaches its reader.
#[test]
fn a_card_is_never_refused_for_the_words_it_uses() {
    let mut s = fixture();
    let to_orchestrator = |id: &str, card: CardSpec, options: Vec<QuestionOption>| Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: scope("t"), recipient: Recipient::Orchestrator,
        body: "Usage".into(), blocking: false, options, card,
    };
    ok(&mut s, agent(), ask("u1", "t", false, decide("¿Paro el despliegue hasta que se reinicie la cuota de Firebase?")));
    ok(&mut s, agent(), to_orchestrator("o1",
        CardSpec::new(CardKind::Decide, "Should I stop for the weekly usage?").why("Weekly usage is at 92%"),
        vec![option("hold", "Hold until the reset"), option("go", "Keep going")]));
    let mut until_reset = CardSpec::new(CardKind::Do, "Tell me when the API quota resets so I can stop waiting").why("The quota is at 99%");
    until_reset.verify = Some("I read the quota".into());
    ok(&mut s, agent(), Command::RequestAction {
        attachments: vec![], interaction_id: "d1".into(), scope: scope("t"), recipient: Recipient::User, body: "Quota".into(),
        blocking: false, options: vec![], card: until_reset,
    });
}

#[test]
fn a_secret_card_is_answered_only_with_where_its_value_was_written() {
    let mut s = fixture();
    let destination = crate::secret_destination::SecretDestination { file: ".env".into(), variable: "STRIPE_KEY".into() };
    let card = CardSpec { secret_destination: Some(destination.clone()), ..CardSpec::new(CardKind::Provide, "Send the Stripe key").why("The build needs it") };
    let mut outside = card.clone();
    outside.secret_destination = Some(crate::secret_destination::SecretDestination { file: "../.env".into(), variable: "STRIPE_KEY".into() });
    assert_eq!(refused(&mut s, agent(), ask("outside", "t", false, outside)), ("invalid_input".into(), Some("card.secret_destination".into())));
    let decide_with_secret = CardSpec { secret_destination: Some(destination.clone()), ..decide("Pick a name") };
    assert_eq!(refused(&mut s, agent(), ask("decide", "t", false, decide_with_secret)).1, Some("card.secret_destination".into()));
    let skip = vec![option("skip", "I can't provide it")];
    let ask_secret = |id: &str| Command::AskQuestion { attachments: vec![], interaction_id: id.into(), scope: scope("t"), recipient: Recipient::User,
        body: "Send the Stripe key".into(), blocking: false, options: skip.clone(), card: card.clone() };
    ok(&mut s, agent(), ask_secret("secret"));
    let typed = |text: &str| Command::AnswerQuestion { attachments: vec![], interaction_id: "secret".into(), option_id: String::new(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(text.into()))]) };
    assert_eq!(refused(&mut s, user(), typed("sk_live_123")), ("invalid_input".into(), Some("inputs".into())));
    ok(&mut s, user(), typed(&destination.written()));
    let InteractionPayload::Question { answer: Some(answer), .. } = &s.interactions[&"secret".into()].payload else { panic!("unanswered") };
    assert_eq!(answer.inputs[ANSWER_TEXT], InputValue::Text("STRIPE_KEY written to .env".into()));
    ok(&mut s, agent(), ask_secret("again"));
    ok(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "again".into(), option_id: "skip".into(), inputs: BTreeMap::new() });
}

/// A form is what an MCP server asks through a held hook: a node's own card never carries one.
#[test]
fn only_a_held_request_for_input_carries_a_form() {
    let mut s = fixture();
    let form = crate::requested_form::RequestedForm::from_schema(&serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}})).unwrap();
    let card = CardSpec { form: Some(form), ..CardSpec::new(CardKind::Provide, "Send the project name").why("The server needs it") };
    assert_eq!(refused(&mut s, agent(), ask("form", "t", false, card)), ("invalid_input".into(), Some("card.form".into())));
}

fn snooze(id: &str, until: i64) -> Command {
    Command::SnoozeCard { interaction_id: id.into(), until, now: 100 }
}
fn notify_count(outcome: &CommandOutcome, id: &str) -> usize {
    outcome.effects.iter().filter(|effect| matches!(effect,
        DomainEffect::NotifyUser { interaction_id, .. } if interaction_id.0 == id)).count()
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
    assert_eq!(crate::views::inbox_badge(&index), 0);
    assert!(index.task(&"t".into()).unwrap().waits_for_user);
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
}

#[test]
fn snooze_returns_only_due_cards_and_return_now_is_immediate_and_idempotent() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("old", "t", false, decide("Choose the first name")));
    ok(&mut s, agent(), ask("q", "t", false, decide("Choose the second name")));
    ok(&mut s, agent(), ask("later", "t", false, decide("Choose the third name")));
    ok(&mut s, user(), snooze("q", 200));
    ok(&mut s, user(), snooze("later", 300));
    let early = ok(&mut s, Actor::System, Command::ResurfaceDueCards { now: 199 });
    assert!(!early.changed);
    assert!(early.effects.is_empty());
    let returned = ok(&mut s, Actor::System, Command::ResurfaceDueCards { now: 200 });
    assert_eq!(notify_count(&returned, "q"), 1);
    assert_eq!(notify_count(&returned, "later"), 0);
    let q = &s.interactions[&InteractionId::from("q")];
    assert_eq!(q.snoozed_until, None);
    assert_eq!(q.resurfaced_at_revision, Some(returned.revision));
    assert_eq!(ReadIndex::build(&s).waiting()[0].id, "interaction:q");
    assert!(!ok(&mut s, Actor::System, Command::ResurfaceDueCards { now: 200 }).changed);
    let returned_now = ok(&mut s, user(), Command::UnsnoozeCard { interaction_id: "later".into() });
    assert_eq!(notify_count(&returned_now, "later"), 1);
    assert_eq!(ReadIndex::build(&s).waiting()[0].id, "interaction:later");
    assert_eq!(ReadIndex::build(&s).next_snoozed_until(), None);
    assert_eq!(refused(&mut s, user(), Command::UnsnoozeCard { interaction_id: "later".into() }).0, "not_snoozed");
}

#[test]
fn snooze_new_agent_card_wakes_only_its_task_and_replacement_retires_the_old_card() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    task(&mut s, "other", "b");
    ok(&mut s, agent(), ask("q", "t", false, decide("Choose the name")));
    ok(&mut s, agent_b(), ask("other-q", "other", false, decide("Choose the other name")));
    ok(&mut s, user(), snooze("q", 200));
    ok(&mut s, user(), snooze("other-q", 200));
    let new_card = ok(&mut s, agent(), ask("new", "t", false, decide("Choose the release")));
    assert_eq!(s.interactions[&InteractionId::from("q")].snoozed_until, None);
    assert_eq!(notify_count(&new_card, "q"), 1);
    assert_eq!(s.interactions[&InteractionId::from("other-q")].snoozed_until, Some(200));
    ok(&mut s, user(), snooze("q", 200));
    let mut replacement = decide("Choose the updated name");
    replacement.replaces = Some("q".into());
    let replaced = ok(&mut s, agent(), ask("replacement", "t", false, replacement));
    assert!(s.interactions[&InteractionId::from("q")].withdrawn);
    assert_eq!(s.interactions[&InteractionId::from("q")].snoozed_until, None);
    assert_eq!(notify_count(&replaced, "q"), 0);
    assert_eq!(notify_count(&replaced, "replacement"), 1);
    assert_eq!(ReadIndex::build(&s).waiting()[0].id, "interaction:replacement");
}

#[test]
fn snooze_rejects_nonfuture_closed_hidden_and_nonuser_cards_without_changing_state() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q", "t", false, decide("Choose the name")));
    assert_eq!(refused(&mut s, user(), snooze("q", 100)).0, "invalid_input");
    assert_eq!(refused(&mut s, user(), snooze("q", 99)).0, "invalid_input");
    assert_eq!(refused(&mut s, agent(), snooze("q", 200)).0, "forbidden");
    assert_eq!(refused(&mut s, user(), Command::ResurfaceDueCards { now: 200 }).0, "forbidden");
    assert_eq!(refused(&mut s, user(), Command::UnsnoozeCard { interaction_id: "q".into() }).0, "not_snoozed");
    ok(&mut s, user(), snooze("q", 200));
    assert_eq!(refused(&mut s, user(), snooze("q", 300)).0, "not_waiting");
    ok(&mut s, user(), answer("q"));
    assert_eq!(s.interactions[&InteractionId::from("q")].snoozed_until, None);
    assert_eq!(refused(&mut s, user(), snooze("q", 200)).0, "not_waiting");
    let mut to_orchestrator = ask("private", "t", false, decide("Choose the owner"));
    if let Command::AskQuestion { recipient, .. } = &mut to_orchestrator { *recipient = Recipient::Orchestrator; }
    ok(&mut s, agent(), to_orchestrator);
    assert_eq!(refused(&mut s, user(), snooze("private", 200)).0, "not_waiting");
}

#[test]
fn snooze_cancellation_withdraws_the_card_without_resurfacing_it() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q", "t", true, decide("Choose the name")));
    ok(&mut s, user(), snooze("q", 200));
    let ended = ok(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    assert!(s.interactions[&InteractionId::from("q")].withdrawn);
    assert_eq!(s.interactions[&InteractionId::from("q")].snoozed_until, None);
    assert_eq!(notify_count(&ended, "q"), 0);
    assert!(!ok(&mut s, Actor::System, Command::ResurfaceDueCards { now: 300 }).changed);
    assert!(ReadIndex::build(&s).snoozed().is_empty());
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
            let live_view = crate::views::LiveSnapshot::default();
            let view = crate::views::build("tasks", crate::views::RenderContext {
                domain: &s, index: &index, live: &live_view, client: crate::views::Client::Tui, lang: crate::views::Lang::En,
                transport: crate::views::Transport::Loopback, cursor: "", rendered_at: "",
            }).unwrap();
            proptest::prop_assert!(serde_json::to_string(&view).unwrap().contains("\"id\":\"block:for-you\""));
            ok(&mut s, user(), Command::UnsnoozeCard { interaction_id: "q".into() });
        }
    }
}

#[test]
fn snooze_completion_clears_a_snoozed_review_without_resurfacing_it() {
    let mut s = fixture();
    ok(&mut s, agent(), present("review", text(), true, review()));
    ok(&mut s, user(), snooze("review", 200));
    let accepted = ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "review".into(), targets: text() });
    assert_eq!(s.interactions[&InteractionId::from("review")].snoozed_until, None);
    assert_eq!(notify_count(&accepted, "review"), 0);
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Done);
    assert!(ReadIndex::build(&s).snoozed().is_empty());
}

#[test]
fn snooze_a_new_final_result_replaces_the_snoozed_review_at_the_top() {
    let mut s = fixture();
    register_agent(&mut s, "b");
    task(&mut s, "other", "b");
    ok(&mut s, agent_b(), ask("other-q", "other", true, decide("Choose the other name")));
    ok(&mut s, agent(), present("d1", text(), true, review()));
    ok(&mut s, user(), snooze("d1", 200));
    let replaced = ok(&mut s, agent(), present("d2", text(), true, review()));
    assert!(s.interactions[&InteractionId::from("d1")].withdrawn);
    assert_eq!(s.interactions[&InteractionId::from("d1")].snoozed_until, None);
    assert_eq!(ReadIndex::build(&s).waiting()[0].id, "interaction:d2");
    assert_eq!(notify_count(&replaced, "d2"), 1);
    assert_eq!(notify_count(&replaced, "d1"), 0);
}

#[test]
fn snooze_reading_information_clears_its_return_time_without_waking_a_question() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q", "t", true, decide("Choose the name")));
    ok(&mut s, user(), snooze("q", 200));
    ok(&mut s, Actor::System, inform("notice", scope("t"), "The build is ready"));
    assert_eq!(s.interactions[&InteractionId::from("q")].snoozed_until, Some(200));
    ok(&mut s, user(), snooze("notice", 300));
    let read = ok(&mut s, user(), Command::MarkRead { interaction_id: "notice".into() });
    assert_eq!(s.interactions[&InteractionId::from("notice")].snoozed_until, None);
    assert_eq!(notify_count(&read, "notice"), 0);
    assert_eq!(ReadIndex::build(&s).next_snoozed_until(), Some(200));
}

#[test]
fn snooze_cancelling_a_review_retires_it_and_clears_its_return_time() {
    let mut s = fixture();
    ok(&mut s, agent(), present("review", text(), true, review()));
    ok(&mut s, user(), snooze("review", 200));
    let cancelled = ok(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    assert!(s.interactions[&InteractionId::from("review")].withdrawn);
    assert_eq!(s.interactions[&InteractionId::from("review")].snoozed_until, None);
    assert!(ReadIndex::build(&s).snoozed().is_empty());
    assert_eq!(notify_count(&cancelled, "review"), 0);
    assert!(!ok(&mut s, Actor::System, Command::ResurfaceDueCards { now: 300 }).changed);
}

#[test]
fn snooze_keeps_the_card_and_its_question_in_task_screens() {
    let mut s = fixture();
    ok(&mut s, agent(), ask("q", "t", true, decide("Choose the name")));
    ok(&mut s, user(), snooze("q", 200));
    let index = ReadIndex::build(&s);
    assert!(index.waiting().is_empty());
    assert_eq!(crate::views::inbox_badge(&index), 0);
    let live = crate::views::LiveSnapshot::default();
    for client in [crate::views::Client::Tui, crate::views::Client::Ios, crate::views::Client::Mac] {
        for lang in [crate::views::Lang::En, crate::views::Lang::Es] {
            let context = crate::views::RenderContext {
                domain: &s, index: &index, live: &live, client, lang,
                transport: crate::views::Transport::Loopback, cursor: "", rendered_at: "",
            };
            let detail = crate::views::build("task:t", context).unwrap();
            let waiting = detail.sections.iter().find(|section| section.id == "waiting")
                .expect("the task still offers its snoozed question");
            assert!(waiting.items.iter().any(|item| matches!(item,
                crate::views::Item::Card(card) if card.id == "q")));
            let list = crate::views::build("tasks", crate::views::RenderContext {
                domain: &s, index: &index, live: &live, client, lang,
                transport: crate::views::Transport::Loopback, cursor: "", rendered_at: "",
            }).unwrap();
            assert!(serde_json::to_string(&list).unwrap().contains("Choose the name"));
        }
    }
}

#[test]
fn card_title_contract_validates_structure_and_preserves_bodies() {
    let mut s = fixture();
    let mut missing = decide("Choose a design");
    missing.title = None;
    assert_eq!(refused(&mut s, agent(), ask("q", "t", false, missing)).1, Some("card.title".into()));
    for title in ["", " ", "First\nSecond", "First\rSecond", &"x".repeat(CardSpec::TITLE_MAX + 1)] {
        let error = refused(&mut s, agent(), ask("q", "t", false, decide("Choose a design").titled(title)));
        assert_eq!(error, ("invalid_input".into(), Some("card.title".into())));
    }
    let empty = decide("Choose a design").titled("Release design").content(ContentKind::Options, " ");
    assert_eq!(refused(&mut s, agent(), ask("q", "t", false, empty)).1, Some("card.content.body".into()));
    for need in ["Choose a design\n", "Choose\ra design"] {
        assert_eq!(refused(&mut s, agent(), ask("q", "t", false, decide(need).titled("Release design"))).1, Some("card.need".into()));
    }
    let body = "# Release design\n\nAtlas has larger text.";
    ok(&mut s, agent(), ask("q", "t", false,
        decide("Choose a design").titled("Release design").content(ContentKind::Options, body)));
    let card = &s.interactions[&"q".into()].card;
    assert_eq!(card.title.as_deref(), Some("Release design"));
    assert_eq!(card.content.as_ref().unwrap().body, body);
}

#[test]
fn card_title_contract_reads_old_cards_without_changing_content() {
    let body = "# Old subject\n\nKeep this text.";
    let card: CardSpec = serde_json::from_value(serde_json::json!({
        "kind":"review","need":"Old subject","content":{"kind":"report","body":body}
    })).unwrap();
    assert_eq!(card.title, None);
    assert_eq!(card.effective_title(), "Old subject");
    assert_eq!(card.content.unwrap().body, body);
}
