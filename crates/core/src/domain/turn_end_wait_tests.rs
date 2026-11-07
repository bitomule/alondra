//! A turn that ends while its node waits on something Alondra recorded (the orchestrator's answer
//! to its message, a suggestion, a question to the user, another task) is not empty: its lifecycle
//! records that wait, no card says it stopped, nobody hears of it, every client says what the task
//! waits for, and the fact that ends the wait reaches the node and moves the task on. A turn with
//! nothing of that kind wakes the node once, and the next one is a case for the orchestrator,
//! never a card for the user.
use super::lifecycle_tests::{assert_aggregate_rules, must, node, sdk_turn, task, user, woken, world};
use super::*;
use crate::views::text::Lang;
use crate::views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const PLAN: &str = "# Rename the queue\n\n- [ ] Phase 1: the task unit\n- [ ] Phases 2-7: one task each\n";

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
/// On the phone, the Mac and the terminal the task says `waits`, nothing is for the user, and no
/// card says it stopped.
fn shows_waiting(s: &DomainState, waits: &str) {
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        assert_ne!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou, "its history never says it needed the user");
        let detail = render(s, "task:t", client);
        assert!(detail.contains(waits), "{client:?}: the task says {waits}: {detail}");
        for said in [&detail, &render(s, "tasks", client)] {
            assert!(!said.contains("Te necesita") && !said.contains("sin entregar ni preguntar"), "{client:?}: {said}");
        }
    }
}
/// Starts the turn the last wake opened, `turn-2`, and returns the case its empty end opens.
fn woken_again(s: &mut DomainState) -> String {
    let LifecycleRecord::TurnEnded { wake: Some(input), .. } = super::lifecycle_tests::last(s, "t") else { panic!("woken") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    format!("{STALLED_PREFIX}t-1-turn-2")
}
/// The orchestrator, never the user, hears of the case `case`, and nothing waits for the user.
fn goes_to_the_orchestrator(s: &DomainState, outcome: &CommandOutcome, case: &InteractionId) {
    assert!(outcome.effects.iter().any(|e| matches!(e, DomainEffect::NotifyOrchestrator { interaction_id: Some(id), .. } if id == case)), "the orchestrator hears of it");
    assert!(!outcome.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "the user hears of nothing");
    assert!(is_stall_case(&s.interactions[case]));
    assert!(pending(s).is_empty(), "nothing waits for the user: {:?}", pending(s));
    assert_ne!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
}
fn approve_plan(s: &mut DomainState) {
    must(s, Actor::System, Command::RecordDecisionRequest {
        task_id: "t".into(), generation: 1, epoch: 3, hold: Hold::Plan, tool: Some("ExitPlanMode".into()), input: json!({}), mcp_server: None,
        prompt: DecisionPrompt::Card {
            body: PLAN.into(), options: Hold::Plan.options(),
            card: CardSpec::new(CardKind::Review, "Rename the queue").content(ContentKind::Plan, PLAN),
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
fn lifecycle(s: &DomainState) -> alondra_domain::TaskLifecycle {
    replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap()
}
/// The turn ended on what the node waits for: its lifecycle records that wait and no card exists.
fn awaits(s: &DomainState, card: &InteractionId, awaits: alondra_domain::Await) {
    assert!(!s.interactions.contains_key(card), "no card says it stopped");
    assert_eq!(lifecycle(s).awaits(), Some(&awaits));
}
/// The turn ended empty: the node is woken with what happened, and no card exists yet.
fn wakes(s: &DomainState, outcome: &CommandOutcome, card: &InteractionId) {
    assert!(!notified(outcome, card) && !s.interactions.contains_key(card), "nobody hears of it yet");
    let (LifecycleRecord::TurnEnded { wake: Some(_), text: Some(text), .. } | LifecycleRecord::AwaitEnded { wake: Some(_), text: Some(text), .. }) = super::lifecycle_tests::last(s, "t")
        else { panic!("the node is woken: {:?}", super::lifecycle_tests::last(s, "t")) };
    assert_eq!(text, WAKE_INSTRUCTION);
    assert_eq!(lifecycle(s).activity(), alondra_domain::ActivityView::Starting);
}
/// What ended the wait reached the node: nothing holds the task and the node goes on.
fn resumes(s: &mut DomainState, card: &InteractionId) {
    must(s, Actor::System, Command::TellNode { task_id: "t".into(), text: "The orchestrator answered.".into(), from_user: false, answers: None });
    assert!(!s.interactions.contains_key(card), "no card ever said it stopped");
    let lifecycle = lifecycle(s);
    assert_eq!(lifecycle.awaits(), None, "what the node is told ends the wait");
    assert!(lifecycle.cards().is_empty(), "nothing holds the task: {:?}", lifecycle.cards());
    assert_eq!(lifecycle.activity(), alondra_domain::ActivityView::Starting, "its next turn starts with what it was told");
    assert_aggregate_rules(s);
}

/// 21 October: the user approved a seven-phase plan, its node told the
/// orchestrator, without blocking, to create one task per phase 2-7, and ended its turn. The user got
/// «paró sin entregar ni preguntar» while the node waited for the orchestrator.
#[test]
fn the_21_october_turn_that_ended_on_a_message_to_the_orchestrator_waits_for_it() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    approve_plan(&mut s);
    ask_orchestrator(&mut s, "t-rename-queue-1", false);
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card), "the user hears of nothing");
    awaits(&s, &card, alondra_domain::Await::Orchestrator("t-rename-queue-1".into()));
    assert!(pending(&s).is_empty(), "nothing waits for the user: {:?}", pending(&s));
    shows_waiting(&s, "Trabajando");
    assert_aggregate_rules(&s);

    let answered = answer_as_orchestrator(&mut s, "t-rename-queue-1");
    assert!(told(&answered), "the answer goes to the node");
    assert!(pending(&s).is_empty(), "the answer is on its way: still nothing for the user");
    resumes(&mut s, &card);
}

#[test]
fn a_turn_that_ends_on_a_blocking_question_to_the_orchestrator_waits_for_it() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    ask_orchestrator(&mut s, "q", true);
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card));
    awaits(&s, &card, alondra_domain::Await::Orchestrator("q".into()));
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    shows_waiting(&s, "Trabajando");
    answer_as_orchestrator(&mut s, "q");
    resumes(&mut s, &card);
}

/// An answer the orchestrator gave before the turn ended already reached the node: it explains
/// nothing about a later turn end, which is empty.
#[test]
fn an_orchestrator_answer_given_before_the_turn_ended_explains_nothing() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    ask_orchestrator(&mut s, "q", false);
    answer_as_orchestrator(&mut s, "q");
    let ended = end_turn(&mut s, "turn-1");
    wakes(&s, &ended, &card);
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
    awaits(&s, &card, alondra_domain::Await::Orchestrator("p".into()));
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    assert_eq!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::Working(alondra_domain::Worker::Alondra(alondra_domain::Step::Answering))));
    shows_waiting(&s, "Trabajando");
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
    awaits(&s, &card, alondra_domain::Await::Tasks(alondra_domain::Several::new(["d".into()]).unwrap()));
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

/// The node's message was taken back unanswered: nothing explains its next step any more, so the
/// turn is empty after all and the node is woken; the next empty turn goes to the orchestrator.
#[test]
fn a_message_to_the_orchestrator_withdrawn_unanswered_leaves_the_turn_empty() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    ask_orchestrator(&mut s, "q", false);
    end_turn(&mut s, "turn-1");
    let withdrawn = must(&mut s, node("a"), Command::Withdraw { interaction_id: "q".into() });
    wakes(&s, &withdrawn, &card);
    assert_aggregate_rules(&s);
    let LifecycleRecord::AwaitEnded { wake: Some(input), .. } = super::lifecycle_tests::last(&s, "t") else { panic!("woken") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    let second = InteractionId::from(format!("{STALLED_PREFIX}t-1-turn-2").as_str());
    let ended = end_turn(&mut s, "turn-2");
    goes_to_the_orchestrator(&s, &ended, &second);
}

/// The tasks the node waited on are no longer its dependencies, and nobody told it: the turn is
/// empty after all, and the node is woken.
#[test]
fn a_dependency_taken_away_while_the_node_waits_for_it_leaves_the_turn_empty() {
    let mut s = world();
    task(&mut s, "d", None);
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    must(&mut s, orchestrator(), Command::SetDependencies { task_id: "t".into(), dependencies: BTreeSet::from(["d".into()]), stack: None });
    end_turn(&mut s, "turn-1");
    let cleared = must(&mut s, orchestrator(), Command::SetDependencies { task_id: "t".into(), dependencies: BTreeSet::new(), stack: None });
    wakes(&s, &cleared, &card);
    assert_aggregate_rules(&s);
}

/// 24 October: “Informe: qué aprender del IDE de BB” started two subagents in the background and
/// ended turns saying it waited for them. While a task_started of its own has no task_notification
/// yet, its turn is not empty: no card and no wake. Once the last one ended and the turn that took
/// it up ends with nothing more, the turn is empty and the node is woken.
#[test]
fn the_24_october_turn_that_ended_with_its_subagents_running_waits_for_them() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let facts = |s: &mut DomainState, facts: Vec<RuntimeObservation>| {
        must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts });
    };
    let no_card = |s: &DomainState, turn: &str| {
        assert!(!s.interactions.contains_key(&InteractionId::from(format!("{TURN_ENDED_PREFIX}t-1-{turn}").as_str())), "{turn}: no card");
        assert!(matches!(super::lifecycle_tests::last(s, "t"), LifecycleRecord::TurnEnded { wake: None, .. }), "{turn}: no wake");
        assert_eq!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::Working(alondra_domain::Worker::OwnWork)), "{turn}: its subagents work");
    };
    facts(&mut s, vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:plugins".into() },
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:ide".into() },
    ]);
    end_turn(&mut s, "turn-1");
    no_card(&s, "turn-1");
    facts(&mut s, vec![
        RuntimeObservation::WorkEnded { work: "claude-task:ide".into(), continuation: "claude-notification:ide".into() },
        RuntimeObservation::TurnResumed { turn: "turn-2".into(), continuation: "claude-notification:ide".into() },
    ]);
    end_turn(&mut s, "turn-2");
    no_card(&s, "turn-2");
    facts(&mut s, vec![
        RuntimeObservation::WorkEnded { work: "claude-task:plugins".into(), continuation: "claude-notification:plugins".into() },
        RuntimeObservation::TurnResumed { turn: "turn-3".into(), continuation: "claude-notification:plugins".into() },
    ]);
    let ended = end_turn(&mut s, "turn-3");
    wakes(&s, &ended, &InteractionId::from(format!("{TURN_ENDED_PREFIX}t-1-turn-3").as_str()));
    assert_aggregate_rules(&s);
}

/// 24 October: “Parcel: el permiso de la app de GitHub” asked the user to do something outside
/// Alondra and ended its turn; the user also got a card saying it stopped without asking. A request
/// to the user is a question too: the turn waits for it.
#[test]
fn the_24_october_turn_that_ended_on_a_request_to_the_user_waits_for_it() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    must(&mut s, node("a"), Command::RequestAction {
        attachments: vec![], interaction_id: "t-parcel-pr-perm-remove-1".into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User,
        body: "Remove the pull request permission from the GitHub app".into(), blocking: true,
        options: vec![QuestionOption { id: "done".into(), label: "Done".into(), description: None }],
        card: CardSpec { verify: Some("I read the app's permissions".into()), ..CardSpec::new(CardKind::Do, "Remove the pull request permission").why("The app must not open pull requests") },
    });
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card), "the user hears of no stop");
    awaits(&s, &card, alondra_domain::Await::User("t-parcel-pr-perm-remove-1".into()));
    assert_eq!(pending(&s), ["t-parcel-pr-perm-remove-1"], "only the request waits for him");
    assert_eq!(s.live_state(&"t".into()), alondra_domain::Several::new(["t-parcel-pr-perm-remove-1".into()]).map(alondra_domain::LiveState::WaitingUser));
    assert_aggregate_rules(&s);
}

/// An approved plan the node did nothing with leaves nothing pending: the orchestrator settles it.
#[test]
fn a_turn_that_ends_right_after_its_plan_is_approved_with_nothing_pending_goes_to_the_orchestrator() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    approve_plan(&mut s);
    let ended = end_turn(&mut s, "turn-1");
    wakes(&s, &ended, &card);
    let second = InteractionId::from(woken_again(&mut s).as_str());
    let ended = end_turn(&mut s, "turn-2");
    goes_to_the_orchestrator(&s, &ended, &second);
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
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Your PR merged: publish the release.".into(), from_user: false, answers: None });
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
    assert_eq!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::Machine)),
        "its task's lifecycle holds the gate's pause");
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        assert!(!render(&s, "tasks", client).contains("sin entregar ni preguntar"), "{client:?}");
    }
    assert!(!s.interactions.contains_key(&card), "the turn ends on the pause its task's lifecycle holds: no card says it stopped");
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "The Mac is fine again.".into(), from_user: false, answers: None });
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Resumed, at: 6 });
    let lifecycle = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert!(lifecycle.cards().is_empty(), "nothing holds the task: {:?}", lifecycle.cards());
    assert_eq!(lifecycle.activity(), alondra_domain::ActivityView::Starting, "its next turn starts with what it was told");
    assert_aggregate_rules(&s);
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

/// 22 October: “simpool: un simulador reutilizado no arrastra el estado del teclado” asked the user
/// whether to change simpool and ended its turn. The turn end is that question: no card says it
/// stopped, and the user's answer reaching the node ends the card the lifecycle keeps.
#[test]
fn the_22_october_turn_that_ended_on_a_question_to_the_user_waits_for_the_answer() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    ask_user(&mut s, "t-simpool-slot-keyboard-state-ask-1");
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card), "the user hears of no stop");
    awaits(&s, &card, alondra_domain::Await::User("t-simpool-slot-keyboard-state-ask-1".into()));
    assert_eq!(pending(&s), ["t-simpool-slot-keyboard-state-ask-1"], "only his question waits for him");
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for said in [render(&s, "task:t", client), render(&s, "tasks", client)] {
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
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "The orchestrator answered: wait for that task.".into(), from_user: false, answers: None });
    assert!(!s.interactions.contains_key(&first), "no card ever said it stopped");
    let LifecycleRecord::Told { input, .. } = super::lifecycle_tests::last(&s, "t") else { panic!("told") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    let card = InteractionId::from(format!("{TURN_ENDED_PREFIX}t-1-turn-2").as_str());
    let ended = end_turn(&mut s, "turn-2");
    assert!(!notified(&ended, &card), "the user hears of no stop");
    awaits(&s, &card, alondra_domain::Await::Tasks(alondra_domain::Several::new(["d".into()]).unwrap()));
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    shows_waiting(&s, "Task d");
    assert_aggregate_rules(&s);
}

/// A plan or a permission Claude asks for is the decision the task waits on: the turn that ends
/// behind it creates no card at all.
#[test]
fn a_turn_that_ends_behind_an_open_plan_or_permission_creates_no_card() {
    for (hold, tool, body, spec) in [
        (Hold::Plan, "ExitPlanMode", PLAN, CardSpec::new(CardKind::Review, "Rename the queue").content(ContentKind::Plan, PLAN)),
        (Hold::Permission, "Bash", "Run make", CardSpec::new(CardKind::Authorize, "Run make")),
    ] {
        let mut s = world();
        let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
        let asked = decision(&mut s, hold, tool, body, spec);
        let ended = end_turn(&mut s, "turn-1");
        assert!(!notified(&ended, &card) && !s.interactions.contains_key(&card), "{hold:?}: no card at all");
        assert!(s.open(&s.interactions[&asked]), "{hold:?}: the decision waits for the user");
        assert!(pending(&s).iter().all(|id| *id == asked.0), "{hold:?}: nothing else does: {:?}", pending(&s));
        assert_aggregate_rules(&s);
    }
}

/// With nothing open, the node is woken with what happened; when its next turn ends the same
/// way, the stop is Alondra's to fix: the orchestrator gets the case and the user hears of nothing.
#[test]
fn a_turn_that_ends_with_nothing_open_wakes_the_node_and_the_next_goes_to_the_orchestrator() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    let ended = end_turn(&mut s, "turn-1");
    wakes(&s, &ended, &card);
    assert!(pending(&s).is_empty(), "{:?}", pending(&s));
    let second = InteractionId::from(woken_again(&mut s).as_str());
    let ended = end_turn(&mut s, "turn-2");
    goes_to_the_orchestrator(&s, &ended, &second);
    shows_waiting(&s, "Alondra lo está reconduciendo");
    assert!(matches!(super::lifecycle_tests::last(&s, "t"), LifecycleRecord::TurnEnded { wake: None, .. }), "the second end names no wake: nothing is sent");
    assert_aggregate_rules(&s);
}

/// 26 Oct, t-alpha-report-1-local: the node's tests ran in the background and the turn that read
/// their end never reached Alondra; in that turn it registered its PR. The usage limit paused
/// it, «sigue» resumed it, and its turn ended with no `present_result`. Nothing woke it, nobody
/// was asked, and it held its slot at work for three hours.
#[test]
fn the_26_october_turn_that_ended_with_its_pr_registered_and_unpresented_wakes_the_node_to_present_it() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:tests".into() },
    ] });
    end_turn(&mut s, "turn-1");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkEnded { work: "claude-task:tests".into(), continuation: "claude-notification:tests".into() },
    ] });
    must(&mut s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1265,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    let pause = Pause { reason: PauseReason::Usage, since: 5, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Paused { pause }, at: 5 });
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "sigue".into(), from_user: false, answers: None });
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Resumed, at: 6 });
    let input = lifecycle(&s).pending_inputs().first().map(|input| input.as_str().to_owned()).expect("«sigue» waits for the node");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input },
    ] });
    let ended = end_turn(&mut s, "turn-2");
    let second = InteractionId::from(format!("{STALLED_PREFIX}t-1-turn-2").as_str());
    assert!(!notified(&ended, &second) && !s.interactions.contains_key(&second) && !s.interactions.contains_key(&card), "nobody is asked yet");
    let LifecycleRecord::TurnEnded { wake: Some(_), text: Some(text), .. } = super::lifecycle_tests::last(&s, "t") else {
        panic!("the empty turn wakes the node: {:?}", super::lifecycle_tests::last(&s, "t"));
    };
    assert_eq!(text, EmptyTurn::UnpresentedPr { number: 1265 }.wake_text());
    assert!(text.contains("#1265") && text.contains("present_result"), "{text}");
    assert_eq!(s.empty_turn(&"t".into()), EmptyTurn::UnpresentedPr { number: 1265 });
    assert_eq!(lifecycle(&s).activity(), alondra_domain::ActivityView::Starting);
    assert_aggregate_rules(&s);
}

/// A turn recorded before turns woke the node, read back from its journal, still asks the user at once.
#[test]
fn a_turn_end_recorded_before_wakes_existed_reads_back_as_the_card() {
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    let journal = s.lifecycle_journal.get_mut(&TaskId::from("t")).unwrap();
    let old: LifecycleRecord = serde_json::from_value(json!({"kind": "turn_ended", "turn": "turn-1", "unheard_card": card})).unwrap();
    journal.push(old);
    let lifecycle = lifecycle(&s);
    assert_eq!(lifecycle.live(), alondra_domain::Several::new([InteractionId::from(card.as_str())]).map(alondra_domain::LiveState::WaitingUser));
    assert!(lifecycle.pending_inputs().len() == 0, "nothing wakes the node");
}

/// The case of a task with a long title tells the orchestrator what it is about, and no
/// notification reaches the iPhone for it.
#[test]
fn the_stop_of_a_task_with_a_long_title_reaches_no_phone() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    s.tasks.get_mut(&TaskId::from("t")).unwrap().title = "simpool: un simulador reutilizado no arrastra el estado del teclado".into();
    let case = woken(&mut s, "t", "turn-1", "turn-2");
    end_turn(&mut s, "turn-2");
    let read = ReadSnapshot::build(std::sync::Arc::new(s.clone()));
    assert!(!crate::views::push_content(&read, &case, Lang::Es).is_ok_and(|content| content.alert.is_some()), "no alert for the user");
    let need = s.interactions[&InteractionId::from(case.as_str())].card.effective_title();
    assert!(need.starts_with("“simpool: un") && need.chars().count() <= CardSpec::TITLED_NEED_MAX, "{need}");
}

fn declare_task_wait(s: &mut DomainState, tasks: &[&str]) -> CommandOutcome {
    must(s, node("a"), Command::WaitForTasks {
        task_id: "t".into(), tasks: tasks.iter().map(|id| TaskId::from(*id)).collect(),
    })
}

fn cancel_wait_target(s: &mut DomainState, target: &str) -> CommandOutcome {
    let cancelled = must(s, user(), Command::CancelTask { task_id: target.into() });
    if s.tasks[&TaskId::from(target)].state.terminal() {
        return cancelled;
    }
    assert!(!told(&cancelled), "cancellation has not finished its cleanup yet");
    must(s, Actor::System, Command::RecordTaskCleanup { task_id: target.into() })
}

#[test]
fn a_node_declares_a_task_wait_without_a_question_or_review_card() {
    let mut s = world();
    task(&mut s, "d", Some("b"));
    task(&mut s, "e", None);
    must(&mut s, node("b"), Command::StartTask { task_id: "d".into() });
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    let before = s.interactions.len();
    let declared = declare_task_wait(&mut s, &["d", "e"]);
    assert_eq!(s.interactions.len(), before);
    assert!(!told(&declared));
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::WaitingDependency);
    let ended = end_turn(&mut s, "turn-1");
    assert!(!notified(&ended, &card));
    awaits(&s, &card, alondra_domain::Await::Tasks(alondra_domain::Several::new(["d".into(), "e".into()]).unwrap()));
    assert!(pending(&s).is_empty());
    shows_waiting(&s, "Task d");
    assert_aggregate_rules(&s);

    let cancelled = cancel_wait_target(&mut s, "e");
    assert!(!told(&cancelled), "the other task is still open");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::WaitingDependency);
    must(&mut s, node("b"), Command::PresentResult {
        attachments: vec![], task_id: "d".into(), interaction_id: "d1".into(), body: "The audit found no issue".into(), is_final: true, nothing_to_review: false,
        targets: vec![DeliveryTarget::Text { text: "The audit found no issue".into() }], checklist: vec![], card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "d1".into(), targets: vec![DeliveryTarget::Text { text: "The audit found no issue".into() }] });
    must(&mut s, node("b"), Command::ConfirmCleanup { task_id: "d".into(), discard: Vec::new() });
    let done = must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "d".into(), end: NodeEnd::Closed, agent_id: Some("b".into()) });
    assert!(told(&done));
    let messages: Vec<_> = done.effects.iter().filter_map(|e| match e {
        DomainEffect::NotifyAgent { agent_id, message, .. } if agent_id.0 == "a" => Some(message), _ => None,
    }).collect();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("The audit found no issue") && messages[0].contains("Task e"));
    assert!(pending(&s).is_empty());
    resumes(&mut s, &card);
}

#[test]
fn cancellation_ends_a_declared_wait_and_tells_the_node_once() {
    let mut s = world();
    task(&mut s, "d", Some("b"));
    must(&mut s, node("b"), Command::StartTask { task_id: "d".into() });
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    declare_task_wait(&mut s, &["d"]);
    end_turn(&mut s, "turn-1");
    let cancelled = must(&mut s, user(), Command::CancelTask { task_id: "d".into() });
    assert!(!told(&cancelled));
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::WaitingDependency);
    let ended = must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "d".into(), end: NodeEnd::Closed, agent_id: Some("b".into()) });
    assert!(told(&ended));
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert!(pending(&s).is_empty());
    let again = must(&mut s, Actor::System, Command::ReviewWaits);
    assert!(!told(&again));
    resumes(&mut s, &card);
}

#[test]
fn already_ended_tasks_do_not_hold_a_declared_wait() {
    for cancelled in [false, true] {
        let mut s = world();
        task(&mut s, "d", Some("b"));
        must(&mut s, node("b"), Command::StartTask { task_id: "d".into() });
        if cancelled {
            must(&mut s, user(), Command::CancelTask { task_id: "d".into() });
        } else {
            super::lifecycle_tests::deliver(&mut s, "d", "b", "d1");
            must(&mut s, user(), Command::AcceptDelivery {
                presentation_id: "d1".into(), targets: vec![DeliveryTarget::Text { text: "Done".into() }],
            });
        }
        if !cancelled {
            must(&mut s, node("b"), Command::ConfirmCleanup { task_id: "d".into(), discard: vec![] });
        }
        must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "d".into(), end: NodeEnd::Closed, agent_id: Some("b".into()) });
        sdk_turn(&mut s, "t", "a");
        let declared = declare_task_wait(&mut s, &["d"]);
        assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
        assert!(!told(&declared));
        assert!(s.unmet_dependency_ids(&s.tasks[&TaskId::from("t")]).is_empty());
    }
}

#[test]
fn a_declared_task_wait_survives_a_daemon_restart_and_ends_durably_once() {
    let mut s = world();
    task(&mut s, "d", None);
    sdk_turn(&mut s, "t", "a");
    declare_task_wait(&mut s, &["d"]);
    end_turn(&mut s, "turn-1");
    let saved = serde_json::to_vec(&s).unwrap();
    let mut loaded: DomainState = serde_json::from_slice(&saved).unwrap();
    loaded.derive_loaded_states();
    assert_eq!(loaded.tasks[&TaskId::from("t")].state, TaskState::WaitingDependency);
    assert!(loaded.ended_waits().is_empty());
    // A completed dependency observed when opening the domain must still be saved and announced.
    loaded.tasks.get_mut(&TaskId::from("d")).unwrap().state = TaskState::Done;
    loaded.derive_loaded_states();
    assert_eq!(loaded.ended_waits(), vec![TaskId::from("t")]);
    let resumed = must(&mut loaded, Actor::System, Command::ReviewWaits);
    assert!(told(&resumed));
    let mut reopened: DomainState = serde_json::from_slice(&serde_json::to_vec(&loaded).unwrap()).unwrap();
    reopened.derive_loaded_states();
    assert!(reopened.ended_waits().is_empty());
    assert!(!told(&must(&mut reopened, Actor::System, Command::ReviewWaits)));
}

#[test]
fn only_the_assigned_node_declares_a_nonempty_acyclic_task_wait() {
    let mut s = world();
    task(&mut s, "d", Some("b"));
    sdk_turn(&mut s, "t", "a");
    for actor in [user(), orchestrator(), node("b")] {
        super::lifecycle_tests::refused_unchanged(&mut s, actor, Command::WaitForTasks { task_id: "t".into(), tasks: BTreeSet::from(["d".into()]) });
    }
    for tasks in [BTreeSet::new(), BTreeSet::from(["missing".into()]), BTreeSet::from(["t".into()])] {
        super::lifecycle_tests::refused_unchanged(&mut s, node("a"), Command::WaitForTasks { task_id: "t".into(), tasks });
    }
    declare_task_wait(&mut s, &["d"]);
    super::lifecycle_tests::refused_unchanged(&mut s, node("b"), Command::WaitForTasks { task_id: "d".into(), tasks: BTreeSet::from(["t".into()]) });
}

#[test]
fn a_completed_task_wait_stays_closed_when_a_named_task_is_reopened() {
    let mut s = world();
    task(&mut s, "d", None);
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    declare_task_wait(&mut s, &["d"]);
    end_turn(&mut s, "turn-1");
    let cancelled = cancel_wait_target(&mut s, "d");
    assert!(told(&cancelled));
    resumes(&mut s, &card);
    must(&mut s, user(), Command::ReopenTask { task_id: "d".into() });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert!(s.tasks[&TaskId::from("t")].ended_task_waits.contains_key(&TaskId::from("d")));
    assert!(s.unmet_dependency_ids(&s.tasks[&TaskId::from("t")]).is_empty());
}

#[test]
fn tasks_saved_before_task_waits_decode_without_a_wait() {
    let mut s = world();
    task(&mut s, "d", None);
    let mut value = serde_json::to_value(&s.tasks[&TaskId::from("d")]).unwrap();
    value.as_object_mut().unwrap().remove("waiting_for_tasks");
    value.as_object_mut().unwrap().remove("ended_task_waits");
    let task: Task = serde_json::from_value(value).unwrap();
    assert!(task.waiting_for_tasks.is_empty());
    assert!(task.ended_task_waits.is_empty());
}

#[test]
fn each_ended_target_stays_satisfied_while_other_targets_are_still_open() {
    let mut s = world();
    task(&mut s, "d", None);
    task(&mut s, "e", None);
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    declare_task_wait(&mut s, &["d", "e"]);
    end_turn(&mut s, "turn-1");
    assert!(!told(&cancel_wait_target(&mut s, "d")));
    must(&mut s, user(), Command::ReopenTask { task_id: "d".into() });
    let ended = cancel_wait_target(&mut s, "e");
    assert!(told(&ended));
    assert!(ended.effects.iter().any(|effect| matches!(effect,
        DomainEffect::NotifyAgent { agent_id, message, .. } if agent_id.0 == "a" && message.contains("It was cancelled"))));
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert!(pending(&s).is_empty());
    resumes(&mut s, &card);
}

#[test]
fn waiting_on_an_ended_target_creates_no_false_dependency_cycle() {
    let mut s = world();
    task(&mut s, "d", None);
    task(&mut s, "e", None);
    sdk_turn(&mut s, "t", "a");
    must(&mut s, orchestrator(), Command::SetDependencies {
        task_id: "d".into(), dependencies: BTreeSet::from(["t".into()]), stack: None,
    });
    cancel_wait_target(&mut s, "d");
    declare_task_wait(&mut s, &["d", "e"]);
    assert_eq!(s.unmet_dependency_ids(&s.tasks[&TaskId::from("t")]), vec![&TaskId::from("e")]);
}

#[test]
fn cancelling_the_waiting_task_never_resumes_it_when_its_targets_end() {
    let mut s = world();
    task(&mut s, "d", None);
    sdk_turn(&mut s, "t", "a");
    declare_task_wait(&mut s, &["d"]);
    end_turn(&mut s, "turn-1");
    must(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    let ended = cancel_wait_target(&mut s, "d");
    assert!(!told(&ended));
    assert!(s.tasks[&TaskId::from("t")].cancelled());
}

#[test]
fn a_task_working_on_requested_changes_resumes_after_its_declared_wait() {
    let mut s = world();
    task(&mut s, "d", None);
    super::lifecycle_tests::delivered_through_the_sdk(&mut s, Some(Instruction::Changes));
    declare_task_wait(&mut s, &["d"]);
    let card = InteractionId::from(format!("{TURN_ENDED_PREFIX}t-1-turn-2").as_str());
    end_turn(&mut s, "turn-2");
    assert!(pending(&s).is_empty());
    let ended = cancel_wait_target(&mut s, "d");
    assert!(told(&ended));
    resumes(&mut s, &card);
}

#[test]
fn a_final_result_under_review_is_not_silently_turned_into_a_task_wait() {
    let mut s = world();
    task(&mut s, "d", None);
    super::lifecycle_tests::delivered_through_the_sdk(&mut s, None);
    assert_eq!(super::lifecycle_tests::refused_unchanged(&mut s, node("a"), Command::WaitForTasks {
        task_id: "t".into(), tasks: BTreeSet::from(["d".into()]),
    }), "conflict");
}
