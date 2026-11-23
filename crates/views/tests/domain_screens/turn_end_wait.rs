//! A turn that ends while its node waits on something Alondra recorded (the orchestrator's answer
//! to its message, a question to the user, another task) is not empty: its lifecycle
//! records that wait, no card says it stopped, nobody hears of it, every client says what the task
//! waits for, and the fact that ends the wait reaches the node and moves the task on. A turn with
//! nothing of that kind wakes the node once, and the next one is a case for the orchestrator,
use alondra_domain_rules::*;
use alondra_views as views;
use crate::inbox_items;
use alondra_core::text::Lang;
use alondra_views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

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
    shows_waiting(&s, "Alondra prepara su siguiente paso");
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
    shows_waiting(&s, "Alondra prepara su siguiente paso");
    answer_as_orchestrator(&mut s, "q");
    resumes(&mut s, &card);
}

#[test]
fn a_turn_that_ends_while_the_task_depends_on_another_waits_for_it_and_goes_on_when_it_is_done() {
    let mut s = world();
    task(&mut s, "d", Some("b"));
    must(&mut s, node("b"), Command::StartTask { task_id: "d".into() });
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    must(&mut s, orchestrator(), Command::SetDependencies { task_id: "t".into(), dependencies: BTreeSet::from(["d".into()]), stack: None, reason: None });
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

/// `wait_for_machine`: the gate holds the node paused before its turn ends, and the turn end is
/// that wait. The gate's word that it goes on ends the card.
#[test]
fn a_turn_that_ends_while_the_node_waits_for_the_machine_asks_nobody() {
    let mut s = world();
    let card = InteractionId::from(sdk_turn(&mut s, "t", "a").as_str());
    let pause = Pause { reason: PauseReason::Declared { reason: "Bazel needs the disk".into() }, since: 5, cut: None, turn_since: None,
        cleanup: PauseCleanup::Done, released: false };
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Paused { pause }});
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
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Resumed});
    let lifecycle = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert!(lifecycle.cards().is_empty(), "nothing holds the task: {:?}", lifecycle.cards());
    assert_eq!(lifecycle.activity(), alondra_domain::ActivityView::Starting, "its next turn starts with what it was told");
    assert_aggregate_rules(&s);
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
    must(&mut s, orchestrator(), Command::SetDependencies { task_id: "t".into(), dependencies: BTreeSet::from(["d".into()]), stack: None, reason: None });
    answer_as_orchestrator(&mut s, "t-85fa-ask-depend-1");
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "The orchestrator answered: wait for that task.".into(), from_user: false, answers: None });
    assert!(!s.interactions.contains_key(&first), "no card ever said it stopped");
    let LifecycleRecord::Told { input, .. } = last(&s, "t") else { panic!("told") };
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
    shows_waiting(&s, "Alondra prepara su siguiente paso");
    assert!(matches!(last(&s, "t"), LifecycleRecord::TurnEnded { wake: None, .. }), "the second end names no wake: nothing is sent");
    assert_aggregate_rules(&s);
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
    assert!(!alondra_views::push_content(&read, &case, Lang::Es).is_ok_and(|content| content.alert.is_some()), "no alert for the user");
    let need = s.interactions[&InteractionId::from(case.as_str())].card.effective_title();
    assert!(need.starts_with("“simpool: un") && need.chars().count() <= CardSpec::TITLED_NEED_MAX, "{need}");
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
    inbox_items(s).into_iter().filter(|item| item.pending)
        .filter_map(|item| item.id.strip_prefix("interaction:").map(str::to_owned)).collect()
}

/// Starts the turn the last wake opened, `turn-2`, and returns the case its empty end opens.
fn woken_again(s: &mut DomainState) -> String {
    let LifecycleRecord::TurnEnded { wake: Some(input), .. } = last(s, "t") else { panic!("woken") };
    let input = rewoken(s, "t", "turn-woken", input);
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
    let (LifecycleRecord::TurnEnded { wake: Some(_), text: Some(text), .. } | LifecycleRecord::AwaitEnded { wake: Some(_), text: Some(text), .. }) = last(s, "t")
        else { panic!("the node is woken: {:?}", last(s, "t")) };
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

fn declare_task_wait(s: &mut DomainState, tasks: &[&str]) -> CommandOutcome {
    must(s, node("a"), Command::WaitForTasks {
        task_id: "t".into(), tasks: tasks.iter().map(|id| TaskId::from(*id)).collect(),
    })
}

fn cancel_wait_target(s: &mut DomainState, target: &str) -> CommandOutcome {
    let cancelled = must(s, user(), Command::CancelTask { task_id: target.into(), reason: None });
    if s.tasks[&TaskId::from(target)].state.terminal() {
        return cancelled;
    }
    assert!(!told(&cancelled), "cancellation has not finished its cleanup yet");
    must(s, Actor::System, Command::RecordTaskCleanup { task_id: target.into() })
}

pub(super) fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

pub(super) fn node(id: &str) -> Actor {
    Actor::Agent { agent_id: id.into() }
}

pub(super) fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

pub(super) fn must(s: &mut DomainState, actor: Actor, command: Command) -> CommandOutcome {
    let shown = format!("{command:?}");
    match exec(s, actor, command) {
        Ok(outcome) => outcome,
        Err(error) => panic!("{shown} was refused: {error}"),
    }
}

pub(super) fn task(s: &mut DomainState, id: &str, agent: Option<&str>) {
    must(s, orchestrator(), Command::CreateTask { priority: Default::default(),
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: agent.map(Into::into), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")), plan_items: None, continues: None,
    });
}

/// The texts the store keeps for a journal's inputs.
fn writes_texts(id: &TaskId, records: &Journal) -> BTreeMap<alondra_domain::InputId, String> {
    journal_writes(id, records, 0).unwrap().into_iter()
        .filter_map(|write| Some((write.input?, write.text?)))
        .collect()
}

/// After every accepted command, the DTO of every task agrees with the replay of its journal,
/// running the derivations again moves nothing (only decisions do), and the events the store keeps
/// for each journal give back exactly its records.
pub(super) fn assert_aggregate_rules(s: &DomainState) {
    s.validate_journal().unwrap_or_else(|error| panic!("{error}"));
    let mut again = s.clone();
    again.derive_states();
    for (id, task) in &s.tasks {
        assert_eq!(&again.tasks[id], task, "derive_states moved the task {id} without a decision");
    }
    for card in s.interactions.values() {
        assert!(!(card.withdrawn && DomainState::answered_question(card)), "the card {} is answered and withdrawn, which the store refuses", card.id);
    }
    for (id, records) in &s.lifecycle_journal {
        let writes = journal_writes(id, records, 0).unwrap();
        let initial = writes[0].input.clone().expect("the journal is created with its initial input");
        let events: Vec<_> = writes.into_iter().filter_map(|write| write.event).collect();
        let texts = writes_texts(id, records);
        assert_eq!(&records_from_events(id, &initial, &events, &texts).unwrap(), records, "the store's events of {id} do not give back its records");
    }
    for (review, held) in &s.plan_reviews {
        for task in held.open() {
            let lifecycle = replay(task, &s.lifecycle_journal[task]).unwrap();
            assert!(lifecycle.plan_reviews().iter().any(|(id, _)| *id == review), "the lifecycle of {task} does not hold it for the review {review}");
        }
    }
}

/// Ends `turn` of the SDK task `id` with nothing that explains its next step, which wakes its
/// node, and starts `next`, the turn that wake opens: ending that one too opens the case for the
/// orchestrator whose id this returns.
pub(super) fn woken(s: &mut DomainState, id: &str, turn: &str, next: &str) -> String {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: id.into(), generation: 1, turn: turn.into(), failure: None, failed: None });
    let LifecycleRecord::TurnEnded { wake: Some(input), text: Some(text), .. } = last(s, id) else { panic!("the node is woken: {:?}", last(s, id)) };
    assert_eq!(text, WAKE_INSTRUCTION);
    let input = rewoken(s, id, &format!("{turn}-woken"), input);
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: next.into(), input },
    ] });
    format!("{STALLED_PREFIX}{id}-1-{next}")
}

/// The node of the SDK task `id`, woken once with `input`, starts and ends empty the turns of
/// its other wakes, `{prefix}-2` and on: this returns the input of its last wake.
pub(super) fn rewoken(s: &mut DomainState, id: &str, prefix: &str, mut input: String) -> String {
    for n in 2..=alondra_domain::EMPTY_TURN_WAKES {
        let turn = format!("{prefix}-{n}");
        must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
            RuntimeObservation::TurnStarted { turn: turn.clone(), input: input.clone() },
        ] });
        must(s, Actor::System, Command::RecordTurnBoundary { task_id: id.into(), generation: 1, turn, failure: None, failed: None });
        let LifecycleRecord::TurnEnded { wake: Some(next), .. } = last(s, id) else { panic!("wake {n} in a row: {:?}", last(s, id)) };
        input = next;
    }
    input
}

fn held(s: &DomainState) -> Vec<(String, ClaudeHold)> {
    s.claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect()
}

pub(super) fn last(s: &DomainState, task: &str) -> LifecycleRecord {
    s.lifecycle_journal[&TaskId::from(task)].last().unwrap().clone()
}

pub(super) fn world() -> DomainState {
    let mut s = DomainState::new("epoch");
    for agent in ["a", "b"] {
        must(&mut s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    }
    must(&mut s, user(), Command::CreateRequest {
        request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
    });
    must(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    s
}

/// An SDK task whose observed turn started: admitted, resource available, the planned input taken.
pub(super) fn sdk_turn(s: &mut DomainState, id: &str, node: &str) -> String {
    sdk_turn_named(s, id, node, "turn-1")
}

fn sdk_turn_named(s: &mut DomainState, id: &str, node: &str, turn: &str) -> String {
    sdk_turn_on(s, id, node, turn, None)
}

/// An SDK task whose observed turn started on `account`.
fn sdk_turn_on(s: &mut DomainState, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    sdk_turn_on_provider(s,alondra_core::accounts::Provider::Claude,id,node,turn,account)
}

fn sdk_turn_on_provider(s: &mut DomainState, provider: alondra_core::accounts::Provider, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    task(s, id, None);
    must(s, Actor::System, Command::AdmitExecution { provider,
        task_id: id.into(), agent_id: node.into(), conversation: format!("conversation-{id}"), instance: format!("instance-{id}"), host: "sdk-host".into(), model: None,
        account: account.map(|account| alondra_core::accounts::AccountId::parse(account).unwrap()), leaves_limit: None,
    });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(id)][0].clone() else { panic!("planned first") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
    format!("{TURN_ENDED_PREFIX}{id}-1-{turn}")
}
