//! A turn that ends while its node waits on something Alondra recorded (the orchestrator's answer
//! to its message, a suggestion, another task) is that wait: the card that asks the user how the
//! task goes on stays out of sight, nobody hears of it, every client says what the task waits
//! for, and the fact that ends the wait reaches the node and ends the card with it.
use super::lifecycle_tests::{assert_aggregate_rules, must, node, sdk_turn, task, user, world};
use super::*;
use crate::views::text::Lang;
use crate::views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const PLAN: &str = "# Remove the Fleet\n\n- [ ] Phase 1: the task unit\n- [ ] Phases 2-7: one task each\n";

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn end_turn(s: &mut DomainState, turn: &str) -> CommandOutcome {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: turn.into(), failure: None, failed: None })
}
fn notified(outcome: &CommandOutcome, card: &InteractionId) -> bool {
    outcome.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if interaction_id == card))
}
fn told(outcome: &CommandOutcome) -> bool {
    outcome.effects.iter().any(|e| matches!(e, DomainEffect::NotifyAgent { agent_id, .. } if agent_id.0 == "a"))
}
fn pending(s: &DomainState) -> Vec<String> {
    super::read_index::inbox_items(s).into_iter().filter(|item| item.pending)
        .filter_map(|item| item.id.strip_prefix("interaction:").map(str::to_owned)).collect()
}
fn render(s: &DomainState, screen: &str, client: Client) -> String {
    let index = ReadIndex::build(s);
    let live = LiveSnapshot::default();
    let view = build(screen, RenderContext { domain: s, index: &index, live: &live, client, lang: Lang::Es,
        transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
    serde_json::to_string(&view).unwrap()
}
/// On the phone, the Mac and the terminal the task and its node say `waits`, nothing is for the
/// user, and no card says it stopped.
fn shows_waiting(s: &DomainState, waits: &str) {
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        assert_ne!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou, "its history never says it needed the user");
        let detail = render(s, "task:t", client);
        let fleet = render(s, "fleet", client);
        assert!(detail.contains(waits), "{client:?}: the task says {waits}: {detail}");
        assert!(fleet.contains(waits), "{client:?}: its node says {waits}: {fleet}");
        for said in [&detail, &render(s, "tasks", client), &fleet] {
            assert!(!said.contains("Te necesita") && !said.contains("sin entregar ni preguntar"), "{client:?}: {said}");
        }
    }
}
fn approve_plan(s: &mut DomainState) {
    must(s, Actor::System, Command::RecordDecisionRequest {
        task_id: "t".into(), generation: 1, epoch: 3, hold: Hold::Plan, tool: Some("ExitPlanMode".into()), input: json!({}), mcp_server: None,
        prompt: DecisionPrompt::Card {
            body: PLAN.into(), options: Hold::Plan.options(),
            card: CardSpec::new(CardKind::Review, "Remove the Fleet").content(ContentKind::Plan, PLAN),
        },
    });
    let card = decision_card_id(&"t".into(), 1, 3);
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: Hold::APPROVE.into(), inputs: BTreeMap::new() });
    must(s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 3 });
}
fn ask_orchestrator(s: &mut DomainState, id: &str, blocking: bool) {
    must(s, node("a"), Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::Orchestrator,
        body: "The plan is approved: create one task per phase 2-7.".into(), blocking,
        options: vec![
            QuestionOption { id: "create".into(), label: "Create them".into(), description: None },
            QuestionOption { id: "decline".into(), label: "Do not create them".into(), description: None },
        ],
        card: CardSpec::new(CardKind::Decide, "Create one task per phase 2-7").why("The user approved a plan in phases"),
    });
}
fn answer_as_orchestrator(s: &mut DomainState, id: &str) -> CommandOutcome {
    must(s, orchestrator(), Command::AnswerQuestion { attachments: vec![], interaction_id: id.into(), option_id: "create".into(), inputs: BTreeMap::new() })
}
/// What ended the wait reached the node: the card is gone and the node goes on.
fn resumes(s: &mut DomainState, card: &InteractionId) {
    must(s, Actor::System, Command::TellNode { task_id: "t".into(), text: "The orchestrator answered.".into() });
    assert!(s.interactions[card].withdrawn, "what the node is told ends the card");
    assert!(!pending(s).contains(&card.0));
    let lifecycle = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert!(lifecycle.cards().is_empty(), "nothing holds the task: {:?}", lifecycle.cards());
    assert_eq!(lifecycle.activity(), alondra_domain::ActivityView::Starting, "its next turn starts with what it was told");
    assert_aggregate_rules(s);
}

/// 21 October: David approved the plan of “Remove the Fleet's task unit”, its node told the
/// orchestrator, without blocking, to create one task per phase 2-7, and ended its turn. David got
/// «paró sin entregar ni preguntar» while the node waited for the orchestrator.
#[test]
fn the_21_october_turn_that_ended_on_a_message_to_the_orchestrator_waits_for_it() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    approve_plan(&mut s);
    ask_orchestrator(&mut s, "t-remove-fleet-phases-1", false);
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card), "David hears of nothing");
    assert!(pending(&s).is_empty(), "nothing waits for David: {:?}", pending(&s));
    shows_waiting(&s, "Esperando al orquestador");
    assert_aggregate_rules(&s);

    let answered = answer_as_orchestrator(&mut s, "t-remove-fleet-phases-1");
    assert!(told(&answered), "the answer goes to the node");
    assert!(pending(&s).is_empty(), "the answer is on its way: still nothing for David");
    resumes(&mut s, &card);
}

#[test]
fn a_turn_that_ends_on_a_blocking_question_to_the_orchestrator_waits_for_it() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    ask_orchestrator(&mut s, "q", true);
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card));
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    shows_waiting(&s, "Esperando al orquestador");
    answer_as_orchestrator(&mut s, "q");
    resumes(&mut s, &card);
}

/// An answer the orchestrator gave before the turn ended already reached the node: it explains
/// nothing about a later turn end.
#[test]
fn an_orchestrator_answer_given_before_the_turn_ended_explains_nothing() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    ask_orchestrator(&mut s, "q", false);
    answer_as_orchestrator(&mut s, "q");
    let ended = end_turn(&mut s, "turn-1");
    assert!(notified(&ended, &card), "nothing is pending: the user hears of it");
    assert!(pending(&s).contains(&card.0));
}

#[test]
fn a_turn_that_ends_with_a_suggestion_pending_waits_for_the_orchestrator() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    must(&mut s, node("a"), Command::AskQuestion {
        attachments: vec![], interaction_id: "p".into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::Orchestrator,
        body: "The README still names the old flag".into(), blocking: false,
        card: CardSpec::new(CardKind::Propose, "Update the README's old flag").why("It is not part of this task"),
        options: suggestion_options(),
    });
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card));
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    shows_waiting(&s, "Esperando al orquestador");
    let settled = must(&mut s, orchestrator(), Command::SettleSuggestion {
        interaction_id: "p".into(), decision: SuggestionDecision::CurrentTask { note: "Fix it here".into() },
    });
    assert!(told(&settled));
    assert!(pending(&s).is_empty(), "the decision is on its way");
    resumes(&mut s, &card);
}

#[test]
fn a_turn_that_ends_while_the_task_depends_on_another_waits_for_it_and_goes_on_when_it_is_done() {
    let mut s = world();
    task(&mut s, "d", Some("b"));
    must(&mut s, node("b"), Command::StartTask { task_id: "d".into() });
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    must(&mut s, orchestrator(), Command::SetDependencies { task_id: "t".into(), dependencies: BTreeSet::from(["d".into()]), stack: None });
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card));
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    shows_waiting(&s, "Task d");
    assert_aggregate_rules(&s);

    must(&mut s, node("b"), Command::PresentResult {
        attachments: vec![], task_id: "d".into(), interaction_id: "d1".into(), body: "Done".into(), is_final: true, nothing_to_review: false,
        targets: vec![DeliveryTarget::Text { text: "Done".into() }], checklist: vec![], card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "d1".into(), targets: vec![DeliveryTarget::Text { text: "Done".into() }] });
    must(&mut s, node("b"), Command::ConfirmCleanup { task_id: "d".into(), discard: Vec::new() });
    let done = must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "d".into(), end: NodeEnd::Closed, agent_id: Some("b".into()) });
    assert_eq!(s.tasks[&TaskId::from("d")].state, TaskState::Done);
    assert!(told(&done), "the node hears its dependency is done");
    assert!(pending(&s).iter().all(|id| *id != card.0), "{:?}", pending(&s));
    resumes(&mut s, &card);
}

/// The node took back what it waited for: nothing explains its next step, so the card shows.
#[test]
fn a_message_to_the_orchestrator_the_node_withdraws_leaves_the_card_in_sight() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    ask_orchestrator(&mut s, "q", false);
    end_turn(&mut s, "turn-1");
    must(&mut s, node("a"), Command::Withdraw { interaction_id: "q".into() });
    assert!(pending(&s).contains(&card.0), "{:?}", pending(&s));
    assert_aggregate_rules(&s);
}

/// An approved plan the node did nothing with leaves nothing pending: the user is asked.
#[test]
fn a_turn_that_ends_right_after_its_plan_is_approved_with_nothing_pending_asks_the_user() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    approve_plan(&mut s);
    let ended = end_turn(&mut s, "turn-1");
    assert!(notified(&ended, &card));
    assert!(pending(&s).contains(&card.0));
}

/// Work of its own (a subagent, a background shell, a Monitor) is what the task does next: the
/// turn ends with no card, and the turn that work wakes is the node's own.
#[test]
fn a_turn_that_ends_behind_work_of_its_own_asks_nobody() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:monitor".into() },
    ] });
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card) && !s.interactions.contains_key(&card), "no card at all");
    assert_eq!(replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().activity(), alondra_domain::ActivityView::OwnWork);
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkEnded { work: "claude-task:monitor".into(), continuation: "claude-notification:monitor".into() },
        RuntimeObservation::TurnResumed { turn: "turn-2".into(), continuation: "claude-notification:monitor".into() },
    ] });
    assert_eq!(replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().activity(), alondra_domain::ActivityView::Working);
    assert!(pending(&s).is_empty());
    assert_aggregate_rules(&s);
}

#[test]
fn a_turn_that_ends_after_its_delivery_asks_nobody() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    must(&mut s, node("a"), Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: "r".into(), body: "Done".into(), is_final: true, nothing_to_review: false,
        targets: vec![DeliveryTarget::Text { text: "Done".into() }], checklist: vec![], card: CardSpec::delivery("Review the result", &[], true),
    });
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card) && !s.interactions.contains_key(&card));
    assert!(pending(&s).iter().all(|id| *id != card.0));
}

/// A node of the SDK that declared work after its PR merged does that work in turns of its own
/// once the PR merged: they are observed, and ending one asks nobody.
#[test]
fn a_turn_of_the_work_after_the_merge_ends_without_a_card() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    let targets = || vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }];
    must(&mut s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    must(&mut s, node("a"), Command::DeclarePostMergeWork { task_id: "t".into(), reason: Some("publish the release".into()), waits_for: vec![] });
    must(&mut s, node("a"), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: targets() });
    must(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card) && !s.interactions.contains_key(&card));
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Your PR merged: publish the release.".into() });
    let LifecycleRecord::Told { input, .. } = super::lifecycle_tests::last(&s, "t") else { panic!("told") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    let ended = end_turn(&mut s, "turn-2");
    let card = InteractionId::from(format!("{TURN_ENDED_PREFIX}t-1-turn-2").as_str());
    assert!(!notified(&ended, &card) && !s.interactions.contains_key(&card));
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    assert_aggregate_rules(&s);
}

/// `wait_for_machine`: the gate holds the node paused before its turn ends, and the turn end is
/// that wait. The gate's word that it goes on ends the card.
#[test]
fn a_turn_that_ends_while_the_node_waits_for_the_machine_asks_nobody() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    let pause = Pause { reason: PauseReason::Declared { reason: "Bazel needs the disk".into() }, since: 5, cut: None, turn_since: None,
        cleanup: PauseCleanup::Done, released: false };
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Paused { pause }, at: 5 });
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card));
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        assert!(!render(&s, "tasks", client).contains("sin entregar ni preguntar"), "{client:?}");
    }
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Resumed, at: 6 });
    resumes(&mut s, &card);
}

fn ask_user(s: &mut DomainState, id: &str) {
    must(s, node("a"), Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User,
        body: "Keyboard keys do not break the captures: close without changes in simpool?".into(), blocking: true,
        options: vec![
            QuestionOption { id: "close".into(), label: "Close without changes".into(), description: None },
            QuestionOption { id: "reset".into(), label: "Reset the keyboard".into(), description: None },
        ],
        card: CardSpec::new(CardKind::Decide, "Choose what simpool does with the keyboard").why("The captures pass with the keys set"),
    });
}
fn decision(s: &mut DomainState, hold: Hold, tool: &str, body: &str, card: CardSpec) -> InteractionId {
    must(s, Actor::System, Command::RecordDecisionRequest {
        task_id: "t".into(), generation: 1, epoch: 3, hold, tool: Some(tool.into()), input: json!({"command": "make"}), mcp_server: None,
        prompt: DecisionPrompt::Card { body: body.into(), options: hold.options(), card },
    });
    decision_card_id(&"t".into(), 1, 3)
}

/// 22 October: “simpool: un simulador reutilizado no arrastra el estado del teclado” asked David
/// whether to change simpool and ended its turn. The turn end is that question: no card says it
/// stopped, and David's answer reaching the node ends the card the lifecycle keeps.
#[test]
fn the_22_october_turn_that_ended_on_a_question_to_the_user_waits_for_the_answer() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    ask_user(&mut s, "t-simpool-slot-keyboard-state-ask-1");
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card), "David hears of no stop");
    assert_eq!(pending(&s), ["t-simpool-slot-keyboard-state-ask-1"], "only his question waits for him");
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for said in [render(&s, "task:t", client), render(&s, "tasks", client), render(&s, "fleet", client)] {
            assert!(!said.contains("sin entregar ni preguntar"), "{client:?}: {said}");
        }
    }
    assert_aggregate_rules(&s);

    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "t-simpool-slot-keyboard-state-ask-1".into(),
        option_id: "close".into(), inputs: BTreeMap::new() });
    assert!(pending(&s).is_empty(), "the answer is on its way: still no stop: {:?}", pending(&s));
    resumes(&mut s, &card);
}

/// 21 October: “MealScale · mejora semanal” asked the orchestrator whether to wait for the task
/// that fixes main's tests, the orchestrator made it depend on that task, and the node's next
/// turn, started with that answer, ended while the dependency had not even started.
#[test]
fn the_21_october_turn_that_ended_after_the_orchestrator_made_it_depend_on_a_task_waits_for_it() {
    let mut s = world();
    task(&mut s, "d", None);
    let first = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    ask_orchestrator(&mut s, "t-85fa-ask-depend-1", true);
    end_turn(&mut s, "turn-1");
    must(&mut s, orchestrator(), Command::SetDependencies { task_id: "t".into(), dependencies: BTreeSet::from(["d".into()]), stack: None });
    answer_as_orchestrator(&mut s, "t-85fa-ask-depend-1");
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "The orchestrator answered: wait for that task.".into() });
    assert!(s.interactions[&first].withdrawn);
    let LifecycleRecord::Told { input, .. } = super::lifecycle_tests::last(&s, "t") else { panic!("told") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    let card = InteractionId::from(format!("{TURN_ENDED_PREFIX}t-1-turn-2").as_str());
    let ended = end_turn(&mut s, "turn-2");
    assert!(!notified(&ended, &card), "David hears of no stop");
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    shows_waiting(&s, "Task d");
    assert_aggregate_rules(&s);
}

/// A plan or a permission Claude asks for is the decision the task waits on: the turn that ends
/// behind it creates no card at all.
#[test]
fn a_turn_that_ends_behind_an_open_plan_or_permission_creates_no_card() {
    for (hold, tool, body, spec) in [
        (Hold::Plan, "ExitPlanMode", PLAN, CardSpec::new(CardKind::Review, "Remove the Fleet").content(ContentKind::Plan, PLAN)),
        (Hold::Permission, "Bash", "Run make", CardSpec::new(CardKind::Authorize, "Run make")),
    ] {
        let mut s = world();
        let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
        let asked = decision(&mut s, hold, tool, body, spec);
        let ended = end_turn(&mut s, "turn-1");
        assert!(!notified(&ended, &card) && !s.interactions.contains_key(&card), "{hold:?}: no card at all");
        assert!(s.open(&s.interactions[&asked]), "{hold:?}: the decision waits for David");
        assert!(pending(&s).iter().all(|id| *id == asked.0), "{hold:?}: nothing else does: {:?}", pending(&s));
        assert_aggregate_rules(&s);
    }
}

/// With nothing open, the stop is the user's to decide: the card shows and David hears of it.
#[test]
fn a_turn_that_ends_with_nothing_open_asks_the_user() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    let ended = end_turn(&mut s, "turn-1");
    assert!(notified(&ended, &card));
    assert_eq!(pending(&s), [card.0.clone()]);
}

/// The card and its notification on the iPhone say all they ask in Spanish: the task's title is
/// what gets shortened, never the verb.
#[test]
fn the_stop_of_a_task_with_a_long_title_reads_whole_in_spanish_on_the_card_and_its_notification() {
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    s.tasks.get_mut(&TaskId::from("t")).unwrap().title = "simpool: un simulador reutilizado no arrastra el estado del teclado".into();
    end_turn(&mut s, "turn-1");
    let read = ReadSnapshot::build(std::sync::Arc::new(s.clone()));
    let alert = crate::views::push_content(&read, &card, Lang::Es).unwrap().alert.expect("an alert");
    let shown = [alert.title.clone(), render(&s, &format!("card:{card}"), Client::Ios)];
    for said in &shown {
        assert!(said.contains("«simpool: un simulador") && said.contains("…» paró sin entregar ni preguntar: dile cómo seguir"), "{said}");
        assert!(!said.contains("stopped"), "{said}");
    }
    assert!(alert.title.chars().count() <= CardSpec::TITLED_NEED_MAX, "{}", alert.title);
}
