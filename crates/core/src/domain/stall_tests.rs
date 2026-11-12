//! A node that stops with nothing explaining its next step is Alondra's to fix, never the user's:
//! its first three stops in a row go back to it at once, the next one is a case the orchestrator
//! settles with a closed outcome, and past the limit the only outcome is a task closed with a result.
use super::lifecycle_tests::{assert_aggregate_rules, deliver, exec, last, must, node, sdk_turn, user, world};
use alondra_domain::EMPTY_TURN_WAKES;
use super::*;
use proptest::prelude::*;
use std::collections::BTreeSet;

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn lifecycle(s: &DomainState) -> std::sync::Arc<alondra_domain::TaskLifecycle> {
    s.lifecycle_of(&"t".into()).unwrap()
}
fn end_empty(s: &mut DomainState, turn: &str) -> CommandOutcome {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: turn.into(), failure: None, failed: None })
}
/// Starts `turn` with what the node was told last, the wake or the orchestrator's instruction.
fn start(s: &mut DomainState, turn: &str) {
    let input = lifecycle(s).pending_inputs().first().map(|input| input.as_str().to_owned()).expect("something to start the turn with");
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: turn.into(), input: input.into() },
    ] });
}
fn open_case(s: &DomainState) -> Option<InteractionId> {
    s.interactions.values().find(|i| is_stall_case(i) && s.open(i)).map(|i| i.id.clone())
}
fn settle(s: &mut DomainState, actor: Actor, outcome: StallOutcome) -> Result<CommandOutcome, DomainError> {
    let case = open_case(s).expect("an open case");
    exec(s, actor, Command::SettleStall { case_id: case, task_id: "t".into(), outcome })
}
fn instruct(text: &str) -> StallOutcome {
    StallOutcome::Instruct { text: text.into() }
}
fn user_cards(s: &DomainState) -> BTreeSet<InteractionId> {
    s.interactions.values().filter(|i| i.recipient == Recipient::User).map(|i| i.id.clone()).collect()
}
/// The node of task `t` stopped and was handed back its turn `EMPTY_TURN_WAKES` times in a row,
/// and stopped again: the first case is open.
fn first_case() -> DomainState {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    end_empty(&mut s, "turn-1");
    assert_eq!(stops_until_a_case(&mut s), u32::from(EMPTY_TURN_WAKES));
    s
}
/// The node of task `t` starts with what it was told and stops, again and again, until a case
/// opens: how many of those stops were handed back to it.
fn stops_until_a_case(s: &mut DomainState) -> u32 {
    let mut handed_back = 0;
    while open_case(s).is_none() {
        assert!(matches!(last(s, "t"), LifecycleRecord::TurnEnded { wake: Some(_), .. } | LifecycleRecord::Answered { .. }), "{:?}", last(s, "t"));
        handed_back += u32::from(matches!(last(s, "t"), LifecycleRecord::TurnEnded { .. }));
        let turn = format!("turn-at-{}", s.revision);
        start(s, &turn);
        end_empty(s, &turn);
    }
    handed_back
}

/// An empty turn goes back to its node at once, with what is missing, three times in a row; only
/// the fourth goes to the orchestrator.
#[test]
fn an_empty_turn_is_handed_back_three_times_and_then_goes_to_the_orchestrator() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    for n in 1..=EMPTY_TURN_WAKES {
        let turn = format!("turn-{n}");
        if n > 1 {
            start(&mut s, &turn);
        }
        end_empty(&mut s, &turn);
        let LifecycleRecord::TurnEnded { wake: Some(wake), text: Some(text), .. } = last(&s, "t") else { panic!("empty turn {n} goes back to the node: {:?}", last(&s, "t")) };
        assert_eq!(lifecycle(&s).pending_inputs().iter().map(|input| input.as_str()).collect::<Vec<_>>(), [wake.as_str()], "at once");
        assert!(text.contains("present_result"), "it says what is missing: {text}");
        if n > 1 {
            assert!(text.contains(&format!("reminder {n} of {EMPTY_TURN_WAKES}")), "{text}");
        }
        assert!(open_case(&s).is_none());
    }
    start(&mut s, "turn-4");
    end_empty(&mut s, "turn-4");
    assert!(matches!(last(&s, "t"), LifecycleRecord::TurnEnded { wake: None, stalled: true, .. }), "{:?}", last(&s, "t"));
    let case = open_case(&s).expect("the fourth empty turn in a row is the orchestrator's case");
    assert_eq!(s.interactions[&case].recipient, Recipient::Orchestrator);
    assert_eq!(lifecycle(&s).card_owner(&case), Some(alondra_domain::Owner::Orchestrator));
    assert!(user_cards(&s).is_empty());
}

/// Each end of a turn is one record in the journal, whatever the task is doing when it ends, and
/// it names a wake only when the lifecycle sends it: nobody tries one end and then records another.
#[test]
fn a_turn_end_is_recorded_once_whatever_the_state() {
    let ends = |s: &DomainState| s.lifecycle_journal[&TaskId::from("t")].iter().filter(|record| matches!(record, LifecycleRecord::TurnEnded { .. })).count();
    let mut delivered = world();
    sdk_turn(&mut delivered, "t", "a");
    deliver(&mut delivered, "t", "a", "result");
    let mut own_work = world();
    sdk_turn(&mut own_work, "t", "a");
    must(&mut own_work, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "tests".into() },
    ] });
    let mut paused = world();
    sdk_turn(&mut paused, "t", "a");
    must(&mut paused, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Paused { pause: Pause {
        reason: PauseReason::Disk, since: 0, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false,
    } }, at: 0 });
    for (state, mut s) in [("delivered", delivered), ("own work", own_work), ("paused", paused)] {
        let before = ends(&s);
        end_empty(&mut s, "turn-1");
        assert_eq!(ends(&s), before + 1, "{state}: one end recorded");
        let Some(LifecycleRecord::TurnEnded { wake, .. }) = s.lifecycle_journal[&TaskId::from("t")].iter().rev().find(|record| matches!(record, LifecycleRecord::TurnEnded { .. })) else { unreachable!() };
        assert!(wake.as_ref().is_none_or(|wake| lifecycle(&s).pending_inputs().iter().any(|pending| pending.as_str() == wake)), "{state}: the end names a wake only when it reaches the node: {wake:?}");
    }
}

#[test]
fn the_instruction_reaches_the_node_and_its_next_stops_go_back_to_it_before_the_next_case() {
    let mut s = first_case();
    let case = open_case(&s).expect("the second stop is a case");
    assert!(s.interactions[&case].body.contains("case 1 of 3"), "{}", s.interactions[&case].body);
    settle(&mut s, orchestrator(), instruct("Present PR #12 with present_result")).unwrap();
    assert!(matches!(last(&s, "t"), LifecycleRecord::Answered { text: Some(text), .. } if text == "Present PR #12 with present_result"),
        "the node gets the instruction as it was written: {:?}", last(&s, "t"));
    assert!(open_case(&s).is_none());
    start(&mut s, "turn-5");
    end_empty(&mut s, "turn-5");
    assert!(matches!(last(&s, "t"), LifecycleRecord::TurnEnded { wake: Some(_), .. }), "the instruction starts the count again: {:?}", last(&s, "t"));
    assert!(open_case(&s).is_none());
    assert_eq!(stops_until_a_case(&mut s), u32::from(EMPTY_TURN_WAKES));
    let next = open_case(&s).expect("the stop after the wakes is a case");
    assert!(s.interactions[&next].body.contains("case 2 of 3"), "{}", s.interactions[&next].body);
    assert!(user_cards(&s).is_empty());
    assert_aggregate_rules(&s);
}

#[test]
fn after_three_instructions_the_only_outcome_is_to_close_the_task_with_its_result() {
    let mut s = first_case();
    for _ in 0..STALL_ATTEMPTS {
        settle(&mut s, orchestrator(), instruct("Commit and push what is left")).unwrap();
        stops_until_a_case(&mut s);
    }
    let case = open_case(&s).unwrap();
    assert!(matches!(&s.interactions[&case].payload, InteractionPayload::Question { options, .. } if options.iter().map(|o| o.id.as_str()).eq([STALL_CLOSE])));
    let refused = settle(&mut s, orchestrator(), instruct("Try once more")).unwrap_err();
    assert_eq!(refused.code, "stall_limit");
    settle(&mut s, orchestrator(), StallOutcome::Close { result: "Half the fix is on branch fix/queue; the tests were never written.".into() }).unwrap();
    let t = &s.tasks[&TaskId::from("t")];
    assert_eq!(t.stall_closing, Some(StallClosing::Settled { result: "Half the fix is on branch fix/queue; the tests were never written.".into() }));
    assert!(t.over(), "the task ends");
    assert!(user_cards(&s).is_empty(), "nobody asked the user");
    assert_aggregate_rules(&s);
}

/// The task says on every client that it closed without a delivery, with what got done.
#[test]
fn a_task_closed_for_its_stops_says_so_with_its_result_on_every_client() {
    use crate::views::text::Lang;
    use crate::views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
    let render = |s: &DomainState, client: Client, lang: Lang| {
        let index = ReadIndex::build(s);
        let live = LiveSnapshot::default();
        serde_json::to_string(&build("task:t", RenderContext { domain: s, index: &index, live: &live, client, lang,
            transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap()).unwrap()
    };
    let mut s = first_case();
    settle(&mut s, orchestrator(), StallOutcome::Close { result: "Half the fix is on branch fix/queue.".into() }).unwrap();
    let mut unsettled = first_case();
    settle(&mut unsettled, Actor::System, StallOutcome::Unsettled).unwrap();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let en = render(&s, client, Lang::En);
        assert!(en.contains("Cancelled") && en.contains("Half the fix is on branch fix/queue."), "{client:?}: {en}");
        let es = render(&s, client, Lang::Es);
        assert!(es.contains("Cancelada") && es.contains("Half the fix is on branch fix/queue."), "{client:?}: {es}");
        let es = render(&unsettled, client, Lang::Es);
        assert!(es.contains("Cancelada") && es.contains("Alondra la cerró"), "{client:?}: {es}");
    }
}

#[test]
fn a_delivery_or_the_users_word_starts_the_count_again() {
    let mut s = first_case();
    settle(&mut s, orchestrator(), instruct("Present it")).unwrap();
    stops_until_a_case(&mut s);
    assert_eq!(s.tasks[&TaskId::from("t")].stalls, 2);
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Focus on the tests".into(), from_user: true, answers: None });
    assert_eq!(s.tasks[&TaskId::from("t")].stalls, 0, "the user's word is the task moving on");
    assert!(open_case(&s).is_none(), "and it answers the open case");
    start(&mut s, "turn-after-word");
    end_empty(&mut s, "turn-after-word");
    assert!(open_case(&s).is_none(), "the first stop since then wakes the node again");
    assert!(matches!(last(&s, "t"), LifecycleRecord::TurnEnded { wake: Some(_), .. }));
}

#[test]
fn the_users_word_while_a_case_is_open_withdraws_it() {
    let mut s = first_case();
    let case = open_case(&s).unwrap();
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Go on with the tests".into(), from_user: true, answers: None });
    assert!(s.interactions[&case].withdrawn, "the orchestrator never settles what no longer holds");
    assert_eq!(settle_closed(&mut s, &case).code, "stall_settled");
}
fn settle_closed(s: &mut DomainState, case: &InteractionId) -> DomainError {
    exec(s, orchestrator(), Command::SettleStall { case_id: case.clone(), task_id: "t".into(), outcome: StallOutcome::Close { result: "Done".into() } }).unwrap_err()
}

#[test]
fn only_the_orchestrator_or_alondra_settles_a_case_and_only_alondra_leaves_one_unsettled() {
    let mut s = first_case();
    assert_eq!(settle(&mut s, user(), instruct("Go on")).unwrap_err().code, "forbidden");
    assert_eq!(settle(&mut s, node("a"), StallOutcome::Close { result: "Done".into() }).unwrap_err().code, "forbidden");
    assert_eq!(settle(&mut s, orchestrator(), StallOutcome::Unsettled).unwrap_err().code, "invalid_input");
    assert_eq!(settle(&mut s, orchestrator(), instruct("  ")).unwrap_err().code, "invalid_input");
    let case = open_case(&s).unwrap();
    assert_eq!(exec(&mut s, orchestrator(), Command::AnswerQuestion {
        attachments: vec![], interaction_id: case, option_id: STALL_INSTRUCT.into(), inputs: Default::default(),
    }).unwrap_err().code, "stall_needs_outcome");
    settle(&mut s, Actor::System, StallOutcome::Unsettled).unwrap();
    assert_eq!(s.tasks[&TaskId::from("t")].stall_closing, Some(StallClosing::Unsettled));
    assert!(s.tasks[&TaskId::from("t")].over());
}

#[test]
fn a_dropped_input_the_user_lets_go_goes_to_the_orchestrator_when_nothing_else_is_left() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    end_empty(&mut s, "turn-1");
    let LifecycleRecord::TurnEnded { wake: Some(wake), .. } = last(&s, "t") else { panic!("woken") };
    must(&mut s, Actor::System, Command::RecordDroppedInput { task_id: "t".into(), generation: 1, input: wake, text: WAKE_INSTRUCTION.into() });
    let dropped = s.interactions.keys().find(|id| id.0.starts_with(DROPPED_PREFIX)).unwrap().clone();
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: dropped, option_id: DISCARD.into(), inputs: Default::default() });
    assert!(open_case(&s).is_some(), "letting it go leaves nothing to go on with: the orchestrator settles it");
    assert_eq!(user_cards(&s).iter().filter(|id| s.open(&s.interactions[*id])).count(), 0, "no card asks the user how it goes on");
}

#[test]
fn a_case_survives_a_restart_and_is_settled_after_it() {
    let mut s = first_case();
    let mut read: DomainState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(open_case(&read), open_case(&s));
    assert_eq!(read.tasks[&TaskId::from("t")].stalls, 1, "the count is kept");
    settle(&mut read, orchestrator(), instruct("Present it")).unwrap();
    settle(&mut s, orchestrator(), instruct("Present it")).unwrap();
    assert_eq!(read.lifecycle_journal, s.lifecycle_journal);
}

/// What the end of a turn, which named `unheard`, left task `t` with: a closed result, an input
/// on its way to the node, or a case someone can settle. Never at rest with nothing.
pub(super) fn after_the_end(s: &DomainState, unheard: &InteractionId) -> Result<(), String> {
    let t = &s.tasks[&TaskId::from("t")];
    let l = lifecycle(s);
    if t.over() || l.execution().is_none() || !l.pending_inputs().is_empty() {
        return Ok(());
    }
    if l.cards().iter().any(|(card, _)| *card == unheard) {
        return match s.interactions.get(unheard) {
            Some(i) if s.open(i) => Ok(()),
            _ => Err(format!("it waits on {unheard}, which nobody can settle: {l:?}")),
        };
    }
    s.turn_result(&t.id, &l, unheard).map(drop).map_err(|empty| format!("its turn ended empty ({empty:?}) with nothing on its way: {l:?}"))
}

/// What can happen to a task whose node keeps stopping, in any order.
#[derive(Clone, Debug)]
enum Step {
    /// The running turn ends with nothing explaining what follows.
    EndEmpty,
    /// The node starts a turn with what it was told.
    Start,
    Instruct,
    Close,
    /// The orchestrator left the case unsettled after its reminder.
    Unsettled,
    /// The node's host ends, resumed on the same session or not.
    HostEnded { resumes: bool },
    /// The user changes the model of the running node.
    SetModel,
    /// The daemon restarts: the state is read back and every lifecycle replayed from its journal.
    Restart,
}
fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => Just(Step::EndEmpty),
        4 => Just(Step::Start),
        3 => Just(Step::Instruct),
        1 => Just(Step::Close),
        1 => Just(Step::Unsettled),
        1 => any::<bool>().prop_map(|resumes| Step::HostEnded { resumes }),
        1 => Just(Step::SetModel),
        1 => Just(Step::Restart),
    ]
}
fn run(s: &mut DomainState, step: &Step, turns: &mut u32) {
    let running = lifecycle(s).turn().is_some();
    match step {
        Step::EndEmpty if running => {
            let turn = lifecycle(s).turn().unwrap().as_str().to_owned();
            let _ = exec(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: lifecycle(s).execution().map_or(1, |e| e.generation()), turn, failure: None, failed: None });
        }
        Step::Start if !running && !lifecycle(s).pending_inputs().is_empty() && lifecycle(s).execution().is_some() => {
            *turns += 1;
            let input = lifecycle(s).pending_inputs()[0].as_str().to_owned();
            let generation = lifecycle(s).execution().unwrap().generation();
            let _ = exec(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation, facts: vec![
                RuntimeObservation::TurnStarted { turn: format!("turn-{turns}"), input: input.into() },
            ] });
        }
        Step::Instruct if open_case(s).is_some() => { let _ = settle(s, orchestrator(), instruct("Present what you have")); }
        Step::Close if open_case(s).is_some() => { let _ = settle(s, orchestrator(), StallOutcome::Close { result: "What got done".into() }); }
        Step::Unsettled if open_case(s).is_some() => { let _ = settle(s, Actor::System, StallOutcome::Unsettled); }
        Step::HostEnded { resumes } if lifecycle(s).execution().is_some() => {
            let generation = lifecycle(s).execution().unwrap().generation();
            let _ = exec(s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation, resumes: *resumes, ended_work: vec![], by_alondra: false });
        }
        Step::SetModel => { let _ = exec(s, user(), Command::SetModel { task_id: "t".into(), model: "claude-sonnet".into() }); }
        Step::Restart => *s = serde_json::from_str(&serde_json::to_string(s).unwrap()).unwrap(),
        _ => {}
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, failure_persistence: None, ..ProptestConfig::default() })]

    /// Every end of a turn that nothing explains, in any order with what the orchestrator, the
    /// host and the daemon do, leaves the task with a closed result, an input or a case.
    #[test]
    fn every_stop_leaves_the_task_with_a_closed_outcome_an_input_or_a_case(steps in prop::collection::vec(step(), 1..80)) {
        let mut s = world();
        sdk_turn(&mut s, "t", "a");
        let mut turns = 1;
        for step in &steps {
            let before = s.lifecycle_journal[&TaskId::from("t")].len();
            run(&mut s, step, &mut turns);
            let ended = s.lifecycle_journal[&TaskId::from("t")].iter().skip(before).find_map(|record| match record {
                LifecycleRecord::TurnEnded { unheard_card, stalled: true, .. } | LifecycleRecord::AwaitEnded { unheard_card, .. } => Some(unheard_card.clone()),
                _ => None,
            });
            if let Some(unheard) = ended {
                prop_assert!(after_the_end(&s, &unheard).is_ok(), "after {:?}: {}", step, after_the_end(&s, &unheard).unwrap_err());
            }
        }
    }

    /// No sequence of stops, with or without the orchestrator's instructions, host ends, model
    /// changes or restarts, puts a card in front of the user that asks how the task goes on. And
    /// a node that only keeps stopping always ends in a task closed with a result: driven on, it
    /// closes within four cases, each after at most three wakes.
    #[test]
    fn stops_never_reach_the_user_and_always_end_in_a_closed_task(steps in prop::collection::vec(step(), 1..60)) {
        let mut s = world();
        sdk_turn(&mut s, "t", "a");
        let mut turns = 1;
        let asked = |s: &DomainState| s.interactions.values()
            .filter(|i| i.recipient == Recipient::User && matches!(&i.scope, Scope::Task { task_id } if task_id.0 == "t"))
            .filter(|i| i.id.0.starts_with(TURN_ENDED_PREFIX) || i.id.0.starts_with(STALLED_PREFIX))
            .map(|i| i.id.clone()).collect::<Vec<_>>();
        for step in &steps {
            run(&mut s, step, &mut turns);
            prop_assert!(asked(&s).is_empty(), "after {:?} a card asks the user how the task goes on: {:?}", step, asked(&s));
        }
        let mut empty = 0;
        for _ in 0..120 {
            if s.tasks[&TaskId::from("t")].over() || lifecycle(&s).execution().is_none() { break }
            if open_case(&s).is_some() {
                let outcome = if s.tasks[&TaskId::from("t")].stalls > STALL_ATTEMPTS { StallOutcome::Close { result: "What got done".into() } } else { instruct("Present what you have") };
                settle(&mut s, orchestrator(), outcome).unwrap();
            } else if lifecycle(&s).turn().is_some() {
                empty += 1;
                run(&mut s, &Step::EndEmpty, &mut turns);
            } else if !lifecycle(&s).pending_inputs().is_empty() {
                run(&mut s, &Step::Start, &mut turns);
            } else {
                break;
            }
            prop_assert!(asked(&s).is_empty(), "driving it on asks the user: {:?}", asked(&s));
        }
        let t = &s.tasks[&TaskId::from("t")];
        let users_call = matches!(s.live_state(&t.id), Some(alondra_domain::LiveState::WaitingUser(_)));
        if !users_call && lifecycle(&s).execution().is_some() {
            prop_assert!(t.over(), "a node that keeps stopping ends its task: {:?}", lifecycle(&s));
            prop_assert!(t.stall_closing.is_some(), "it closed with a result: {:?}", t);
            prop_assert!(empty <= (u32::from(EMPTY_TURN_WAKES) + 1) * (STALL_ATTEMPTS + 1), "{} empty turns while driven on", empty);
        }
    }
}
