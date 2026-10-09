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
    for item in s.projection().inbox.iter().filter(|item| item.pending) {
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
    ok(s, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: Some(agent.into()),
        review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
    });
}
fn register_agent(s: &mut DomainState, id: &str) {
    ok(s, Actor::System, Command::RegisterAgent { agent_id: id.into(), name: format!("Instance {id}") });
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
    QuestionOption { id: id.into(), label: label.into(), description: None, inputs: vec![] }
}
fn two() -> Vec<QuestionOption> {
    vec![option("a", "Atlas"), option("b", "Borealis")]
}
fn decide(need: &str) -> CardSpec {
    CardSpec::new(CardKind::Decide, need).why("The name goes on the release")
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
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    ok(&mut s, agent(), ask("winding-down", "t", false, decide("Keep or drop the half-done work")));
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    assert_eq!(refused(&mut s, agent(), ask("q", "t", false, decide("Pick a name"))),
        ("task_not_active".into(), Some("scope.task_id".into())));
    assert_eq!(refused(&mut s, agent(), inform("closing", scope("t"), "I stopped: the task was cancelled")),
        ("forbidden".into(), Some("card.kind".into())));
    ok(&mut s, orchestrator(), inform("closing", scope("t"), "The task was cancelled"));
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
    ok(&mut s, orchestrator(), inform("i1", scope("t"), "Tests pass"));
    let out = ok(&mut s, orchestrator(), inform("i2", scope("t"), "Tests pass"));
    assert!(!out.changed);
    assert_eq!(out.created_ids, vec!["i1".to_owned()], "the existing notice is returned");
    assert!(!s.interactions.contains_key(&"i2".into()));
    ok(&mut s, user(), Command::MarkRead { interaction_id: "i1".into() });
    ok(&mut s, orchestrator(), inform("i3", scope("t"), "Tests pass"));
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
        body: format!("# {title}\n\n1. Do it"), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, title).content(ContentKind::Plan, format!("# {title}\n\n1. Do it")),
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
    register_pr(&mut s);
    let mut progress = CardSpec::new(CardKind::Know, "Read the progress");
    progress.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
    ok(&mut s, agent(), present("d1", text(), false, progress));
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
    assert_eq!(field(&mut s, decide("")), ("invalid_input".into(), Some("card.need".into())));
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
    let starred = vec![option("a", "Atlas (Recommended)"), option("b", "Borealis (Recommended)")];
    assert_eq!(refused(&mut s, agent(), with(starred, None)).1, Some("card.recommended".into()));
    ok(&mut s, agent(), with(two(), Some("a")));
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
        request_id: "q-r".into(), answer: Some("Yes, since Monday".into()), withdrawn: false, title: Some("Review status".into()),
    });
    let told: Vec<&Interaction> = s.interactions.values()
        .filter(|i| i.scope == Scope::Orchestrator { request_id: Some("q-r".into()) })
        .collect();
    assert_eq!(told.len(), 1);
    assert_eq!((told[0].card.kind, told[0].card.need.as_str(), told[0].body.as_str()), (CardKind::Know, "Review status", "Yes, since Monday"));
    assert!(out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if interaction_id == &told[0].id)));
    ok(&mut s, user(), Command::CreateRequest { request_id: "told".into(), body: "Encrypt images?".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, orchestrator(), inform("told-answer", Scope::Orchestrator { request_id: Some("told".into()) }, "Yes, with R2"));
    ok(&mut s, orchestrator(), Command::CloseRequest {
        request_id: "told".into(), answer: Some("Yes, with R2".into()), withdrawn: false, title: None,
    });
    assert_eq!(s.interactions.values().filter(|i| i.scope == Scope::Orchestrator { request_id: Some("told".into()) }).count(), 1,
        "an answer already sent is not sent again");
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
fn settle(id: &str, option_id: &str, note: &str) -> Command {
    Command::AnswerQuestion {
        attachments: vec![], interaction_id: id.into(), option_id: option_id.into(),
        inputs: BTreeMap::from([("note".to_owned(), InputValue::Text(note.into()))]),
    }
}

#[test]
fn a_suggestion_goes_to_the_orchestrator_and_never_to_the_users_inbox() {
    let mut s = fixture();
    let out = ok(&mut s, agent(), suggest("p"));
    assert!(out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyOrchestrator { interaction_id: Some(i), .. } if i.0 == "p")));
    assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "{:?}", out.effects);
    assert!(s.projection().inbox.iter().all(|item| item.id != "interaction:p"));
    assert_eq!(crate::views::inbox_badge(&s), 0);
}

#[test]
fn only_a_node_suggests_and_only_to_the_orchestrator_with_its_answers() {
    let mut s = fixture();
    let Command::AskQuestion { card, .. } = suggest("p") else { unreachable!() };
    let to_user = Command::AskQuestion {
        attachments: vec![], interaction_id: "u".into(), scope: scope("t"), recipient: Recipient::User,
        body: "Update the README".into(), blocking: false, card: card.clone(), options: suggestion_options(),
    };
    assert_eq!(refused(&mut s, agent(), to_user.clone()), ("forbidden".into(), Some("card.kind".into())));
    assert_eq!(refused(&mut s, orchestrator(), to_user), ("forbidden".into(), Some("card.kind".into())));
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
    ok(&mut s, orchestrator(), settle("p1", "discarded", "Task 12 already covers it"));
    ok(&mut s, agent(), suggest("p6"));
}

#[test]
fn the_orchestrators_answer_and_note_reach_the_node() {
    let mut s = fixture();
    ok(&mut s, agent(), suggest("p"));
    assert_eq!(refused(&mut s, user(), settle("p", "yours", "")).0, "forbidden");
    let out = ok(&mut s, orchestrator(), settle("p", "yours", "Step 3 of your plan covers it"));
    assert!(out.effects.iter().any(|e| matches!(e,
        DomainEffect::NotifyAgent { agent_id, interaction_id: Some(i), .. } if agent_id.0 == "a" && i.0 == "p")), "{:?}", out.effects);
    let InteractionPayload::Question { answer: Some(answer), .. } = &s.interactions[&"p".into()].payload else { panic!("unanswered") };
    assert_eq!(answer.inputs["note"], InputValue::Text("Step 3 of your plan covers it".into()));
}

#[test]
fn a_suggestion_holds_back_neither_the_delivery_nor_the_end_of_its_task() {
    let mut s = fixture();
    ok(&mut s, agent(), suggest("p"));
    assert!(!s.unanswered(&"t".into()));
    ok(&mut s, agent(), present("review", text(), true, review()));
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    ok(&mut s, orchestrator(), settle("p", "new_task", "Created as its own task"));
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
    ok(&mut s, orchestrator(), inform("know", scope("t"), "Heads up"));
    ok(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    let items = s.projection().inbox;
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
    s.inbox_projection()
}

#[test]
fn inbox_selection_keeps_user_cards_current_final_delivery_and_action_guards() {
    let mut s = fixture();
    ok(&mut s, orchestrator(), inform("z-notice", scope("t"), "First notice"));
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
    assert_eq!(s, before, "projection is read-only");
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
        ok(&mut s, orchestrator(), inform(id, scope("t"), id));
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

fn inbox_ids(s: &DomainState) -> Vec<String> {
    selected_inbox(s).iter().map(|item| item.id.trim_start_matches("interaction:").to_owned()).collect()
}
fn register_pr_of(s: &mut DomainState, actor: Actor, task: &str, pr: &str, number: u64) {
    ok(s, actor, Command::RegisterPr {
        task_id: task.into(), pr_id: pr.into(), host: "github.com".into(), repository: "owner/repo".into(),
        number, head: format!("head-{pr}"), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}
fn present_pr(s: &mut DomainState, actor: Actor, task: &str, id: &str, pr: &str) {
    ok(s, actor, Command::PresentResult {
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
    ok(&mut s, orchestrator(), inform("n", scope("t"), "Heads up"));
    let projection = s.projection();
    assert_eq!(inbox_ids(&s), ["q", "d", "n"], "a question, the delivery of the PR, a notice: three cards");
    assert_eq!(projection.counts.roots, 3);
    for item in &projection.inbox {
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
    ok(&mut s, orchestrator(), inform("know-old", scope("t"), "The first notice"));
    ok(&mut s, agent(), ask("provide", "t", false,
        CardSpec::new(CardKind::Provide, "Send a screenshot of the bug").why("I cannot reproduce it")));
    ok(&mut s, orchestrator(), inform("know-new", scope("t"), "The second notice"));
    register_agent(&mut s, "c");
    task(&mut s, "t3", "c");
    ok(&mut s, Actor::Agent { agent_id: "c".into() }, Command::PresentResult {
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
        attachments: vec![], task_id: "t2".into(), interaction_id: "elsewhere".into(), body: "Done".into(),
        targets: text(), is_final: true, nothing_to_review: false, card: review(),
    });
    register_pr(&mut s);
    ok(&mut s, agent(), present("d", pr_target(), true, review()));
    let mut asks = decide("Keep the filter?");
    asks.blocks = vec![CardTarget::PullRequest { pr_id: "pr".into() }];
    ok(&mut s, agent(), ask("q", "t", false, asks));
    assert_eq!(inbox_ids(&s), ["elsewhere", "q", "d"], "the question comes out above the PR it holds back, not after it");
    let projection = s.projection();
    let item = |id: &str| projection.inbox.iter().find(|i| i.id == format!("interaction:{id}")).unwrap();
    assert_eq!(item("d").blocked_by, vec![InteractionId::from("q")]);
    assert_eq!(item("q").blocks, vec![InteractionId::from("d")]);
    ok(&mut s, user(), answer("q"));
    assert!(s.projection().inbox.iter().all(|i| i.blocked_by.is_empty()), "answered, it holds nothing back");
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
    let projection = s.projection();
    let item = |id: &str| projection.inbox.iter().find(|i| i.id == format!("interaction:{id}")).unwrap();
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
    ok(&mut s, orchestrator(), notice("n1", "Build is green", None));
    ok(&mut s, orchestrator(), notice("n2", "Correction: build is red", Some("n1")));
    assert!(s.interactions[&"n1".into()].withdrawn);
    assert!(s.open(&s.interactions[&"n2".into()]));
}

fn waiting_ids(s: &DomainState) -> Vec<String> {
    s.projection().inbox.iter().filter(|item| item.pending).map(|item| item.id.trim_start_matches("interaction:").to_owned()).collect()
}
fn gq(need: &str, labels: &[&str]) -> GroupQuestion {
    GroupQuestion {
        need: need.into(), why: "The release needs it".into(), recommended: None,
        options: labels.iter().enumerate().map(|(n, label)| option(&format!("o{n}"), label)).collect(),
    }
}
fn asks(questions: Vec<GroupQuestion>) -> Command {
    Command::AskQuestions {
        interaction_id: "g".into(), scope: scope("t"), recipient: Recipient::User, blocking: true,
        why: "Three things for the release".into(), questions,
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
        body: format!("# {title}\n\n1. Do it"), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, title).content(ContentKind::Plan, format!("# {title}\n\n1. Do it")),
    }
}
fn waiting_for_user(s: &DomainState) -> Vec<String> {
    s.projection().inbox.iter().filter(|item| item.pending).map(|item| item.id.clone()).collect()
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
    s.tasks.get_mut(&"t".into()).unwrap().agent_id = Some("b".into());
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
            Command::PresentResult { attachments, task_id, interaction_id, body, targets, is_final, nothing_to_review: true, card },
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
