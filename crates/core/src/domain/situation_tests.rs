//! Every task shows one of seven situations, decided once: «needs you» only for a card the inbox
//! lists, «working» only for a turn or work of its own, and a slot only for work or a step to it.
use super::lifecycle_tests::{exec, node, user};
use super::sdk_node_tests::{act, perform, Act, NODE, SDK};
use super::lifecycle_cache_tests::three_tasks;
use super::*;
use alondra_domain::{Await, FinishIntent, Owner, Party, TaskPhase, WaitReason};
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Move {
    Sdk(Act),
    /// The SDK node asks two questions together, as one group.
    AskGrouped,
    /// The user snoozes the first open card of the SDK task.
    Snooze,
    /// Its execution is released and the gate admits a new one, which starts.
    Relaunch,
    /// The SDK node asks the orchestrator something and goes on with its turn.
    AskOrchestrator,
}

fn moves() -> impl Strategy<Value = Move> {
    prop_oneof![
        8 => act().prop_map(Move::Sdk),
        1 => Just(Move::AskGrouped),
        1 => Just(Move::Snooze),
        1 => Just(Move::Relaunch),
        1 => Just(Move::AskOrchestrator),
    ]
}

fn apply(s: &mut DomainState, step: &Move, n: usize) {
    match step {
        Move::Sdk(act) => perform(s, act, n),
        Move::AskGrouped => {
            let options = vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }];
            for index in 1..=2 {
                let mut card = CardSpec::question("Choose the way", &options);
                card.group = Some(QuestionGroup { id: format!("group-{n}"), index, total: 2 });
                let _ = exec(s, node(NODE), Command::AskQuestion {
                    attachments: vec![], interaction_id: format!("grouped-{n}-{index}").into(), scope: Scope::Task { task_id: SDK.into() },
                    recipient: Recipient::User, body: "Which way?".into(), blocking: true, options: options.clone(), card,
                });
            }
        }
        Move::AskOrchestrator => {
            let options = vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }];
            let _ = exec(s, node(NODE), Command::AskQuestion {
                attachments: vec![], interaction_id: format!("to-orchestrator-{n}").into(), scope: Scope::Task { task_id: SDK.into() },
                recipient: Recipient::Orchestrator, body: "Is the other task done?".into(), blocking: false, options: options.clone(),
                card: CardSpec::question("Is the other task done?", &options),
            });
        }
        Move::Relaunch => {
            let _ = exec(s, Actor::System, Command::ReleaseTask { task_id: SDK.into(), reason: "The host ended".into() });
            let _ = exec(s, Actor::System, Command::AdmitExecution {
                provider: crate::accounts::Provider::Claude, task_id: SDK.into(), agent_id: NODE.into(), conversation: format!("conversation-{SDK}"),
                instance: format!("relaunch-{n}"), host: "sdk-host".into(), model: None, account: None, leaves_limit: None,
            });
        }
        Move::Snooze => {
            let Some(card) = s.interactions.values()
                .find(|i| i.scope == Scope::Task { task_id: SDK.into() } && i.recipient == Recipient::User && s.open(i))
                .map(|i| i.id.clone()) else { return };
            let _ = exec(s, user(), Command::SnoozeCard { interaction_id: card, until: 1_000_000 + n as i64, now: 1_000 + n as i64 });
        }
    }
}

fn histories() -> impl Strategy<Value = Vec<Move>> {
    prop::collection::vec(moves(), 1..60)
}

/// Runs `check` on the state after each move of `history`.
fn each_state(history: &[Move], mut check: impl FnMut(&DomainState, &ReadIndex) -> Result<(), String>) -> Result<(), TestCaseError> {
    let mut s = three_tasks();
    for (n, step) in history.iter().enumerate() {
        apply(&mut s, step, n);
        let index = ReadIndex::build(&s);
        check(&s, &index).map_err(|why| TestCaseError::fail(format!("{why} after {step:?} in {history:?}")))?;
    }
    Ok(())
}

fn situation(index: &ReadIndex, task: &TaskId) -> Situation {
    index.task(task).expect("every task has a reading").situation
}

fn listed(s: &DomainState, index: &ReadIndex, card: &InteractionId) -> bool {
    let shown = |id: &InteractionId| index.waiting_card(id).is_some() || index.snoozed_card(id).is_some();
    shown(card) || s.interactions.get(card).and_then(|i| i.card.group.as_ref()).is_some_and(|group| {
        s.interactions.values().filter(|i| i.card.group.as_ref().is_some_and(|other| other.id == group.id)).any(|i| shown(&i.id))
    })
}

/// The cards a task's lifecycle waits on the user for: its own cards but a pause, which reads
/// paused, the question its last turn ended on, and its delivery while that waits for a review the
/// user has not given yet.
fn cards_waited_on(s: &DomainState, task: &TaskId, lifecycle: &alondra_domain::TaskLifecycle) -> Vec<InteractionId> {
    let mut cards: Vec<InteractionId> = lifecycle.cards().into_iter()
        .filter(|(card, reason)| *reason != WaitReason::Pause && lifecycle.card_owner(card) == Some(Owner::User))
        .map(|(card, _)| card.clone())
        .collect();
    if let Some(Await::User(card)) = lifecycle.awaits() {
        cards.push(card.clone());
    }
    if cards.is_empty() && lifecycle.phase() == TaskPhase::Delivered && s.task_handed_back(task).is_none() {
        cards.extend(lifecycle.delivery().cloned());
    }
    cards
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: None, rng_seed: proptest::test_runner::RngSeed::Fixed(9), ..ProptestConfig::default() })]

    #[test]
    fn every_open_task_is_in_one_of_seven_situations(history in histories()) {
        each_state(&history, |s, index| {
            for task in s.tasks.values() {
                let shown = situation(index, &task.id);
                let cancelling = s.lifecycle_of(&task.id).is_ok_and(|lifecycle| lifecycle.finishing_intent() == Some(FinishIntent::Cancel));
                if !task.state.terminal() && (shown == Situation::Finished || (shown == Situation::Cancelled && !cancelling)) {
                    return Err(format!("the open task {} ({:?}) reads {shown:?}", task.id, task.state));
                }
            }
            Ok(())
        })?;
    }

    #[test]
    fn needs_you_means_an_open_card_of_the_user(history in histories()) {
        each_state(&history, |s, index| {
            for task in s.tasks.values() {
                let read = index.task(&task.id).unwrap();
                let open_card = read.waits_for_user && !read.waits_for_review;
                if (read.situation == Situation::NeedsYou) != open_card {
                    return Err(format!("{} reads {:?} with cards {:?} (waits for the user {}, only the review {})",
                        task.id, read.situation, read.waiting, read.waits_for_user, read.waits_for_review));
                }
            }
            Ok(())
        })?;
    }

    #[test]
    fn every_card_a_task_waits_on_is_listed_in_the_inbox(history in histories()) {
        each_state(&history, |s, index| {
            for task in s.tasks.values().filter(|task| !task.archived) {
                let Ok(lifecycle) = s.lifecycle_of(&task.id) else { continue };
                if lifecycle.obligation().is_none_or(|obligation| obligation.party != Party::User) {
                    continue;
                }
                if let Some(card) = cards_waited_on(s, &task.id, &lifecycle).into_iter().find(|card| !listed(s, index, card)) {
                    return Err(format!("{} waits on card {card} that the inbox does not list", task.id));
                }
                if let Some(card) = &index.task(&task.id).unwrap().unlisted {
                    return Err(format!("{} reads card {card} as unlisted", task.id));
                }
            }
            Ok(())
        })?;
    }

    #[test]
    fn working_means_a_turn_or_work_of_its_own(history in histories()) {
        each_state(&history, |s, index| {
            let lifecycle = s.lifecycle_of(&SDK.into()).map_err(|error| error.message)?;
            match situation(index, &SDK.into()) {
                Situation::Working(Doing::Turn | Doing::AfterMerge) if lifecycle.turn().is_none() =>
                    Err(format!("it reads working on a turn with none running ({:?})", lifecycle.activity())),
                Situation::Working(Doing::OwnWork) if lifecycle.activity() != alondra_domain::ActivityView::OwnWork =>
                    Err(format!("it reads working in the background while it is {:?}", lifecycle.activity())),
                _ => Ok(()),
            }
        })?;
    }

    #[test]
    fn the_header_the_live_activity_and_the_slots_count_the_same_tasks(history in histories()) {
        each_state(&history, |s, index| {
            let working = s.tasks.values().filter(|task| matches!(situation(index, &task.id), Situation::Working(_))).count() as u32;
            if index.activity().working != working {
                return Err(format!("the header counts {} working, the tasks say {working}", index.activity().working));
            }
            let for_you = s.tasks.values().filter(|task| match situation(index, &task.id) {
                Situation::NeedsYou => true,
                Situation::Delivered(crate::domain::Delivery::ForReview) => index.task(&task.id).is_some_and(|reading| reading.for_you()),
                Situation::Working(_) | Situation::Waiting(_) | Situation::Paused | Situation::Delivered(_) | Situation::Finished | Situation::Cancelled => false,
            }).count() as u32;
            if index.activity().waiting != for_you {
                return Err(format!("the header counts {} waiting, {for_you} tasks need the user or have their delivery in front of them", index.activity().waiting));
            }
            Ok(())
        })?;
    }

    #[test]
    fn a_node_paused_for_disk_takes_no_slot_and_does_not_work(history in histories()) {
        let mut moves = history;
        moves.push(Move::Sdk(Act::DiskPause));
        each_state(&moves, |s, _| {
            let Some(pause) = s.node_pause(&NODE.into()) else { return Ok(()) };
            if s.slots().contains(&NODE.into()) {
                return Err(format!("it holds a slot while paused for {:?}", pause.reason));
            }
            Ok(())
        })?;
    }

    #[test]
    fn a_task_that_holds_a_slot_works_or_is_a_step_from_it(history in histories()) {
        each_state(&history, |s, index| {
            let through_sdk = |task: &Task| task.agent_id.as_ref().is_some_and(|agent| s.through_sdk(agent));
            for task in s.tasks.values().filter(|task| !task.state.terminal() && through_sdk(task) && s.task_holds_slot(&task.id)) {
                let shown = situation(index, &task.id);
                if !matches!(shown, Situation::Working(_) | Situation::Waiting(WaitsFor::Step(AlondraStep::Starting | AlondraStep::Delivering | AlondraStep::Waking))) {
                    return Err(format!("{} holds a slot and reads {shown:?}", task.id));
                }
            }
            Ok(())
        })?;
    }

    #[test]
    fn the_header_the_live_activity_and_the_gate_count_the_same_tasks(history in histories()) {
        each_state(&history, |s, index| {
            for task in s.tasks.values().filter(|task| !task.state.terminal() && task.state != TaskState::Closing && task.wrap_up.asked_at.is_none()) {
                let Some(agent) = task.agent_id.as_ref().filter(|agent| s.through_sdk(agent) && s.node_pause(agent).is_none()) else { continue };
                let shown = situation(index, &task.id);
                let takes = matches!(shown, Situation::Working(_) | Situation::Waiting(WaitsFor::Step(AlondraStep::Starting | AlondraStep::Delivering | AlondraStep::Waking)))
                    && s.lifecycle_of(&task.id).is_ok_and(|lifecycle| lifecycle.holds_slot());
                let counted = index.holds_slot(agent);
                if takes != counted {
                    return Err(format!("{} reads {shown:?} and its execution {} a slot, but the gate {} it", task.id,
                        if takes { "holds" } else { "holds no" }, if counted { "counts" } else { "does not count" }));
                }
            }
            Ok(())
        })?;
    }
}

/// The user asked for changes after the node's host ended: the lifecycle still holds the delivery
/// for review, but the card left the inbox, so the task waits for Alondra to hand the changes over.
#[test]
fn a_delivery_sent_back_with_its_host_gone_waits_for_alondra_not_the_user() {
    use super::lifecycle_cache_tests::Step as TaskStep;
    let mut s = three_tasks();
    for (n, act) in [Act::Task(TaskStep::Deliver(2)), Act::HostEnded, Act::SendBack].iter().enumerate() {
        perform(&mut s, act, n);
    }
    let index = ReadIndex::build(&s);
    let read = index.task(&SDK.into()).unwrap();
    assert_eq!(read.situation, Situation::Waiting(WaitsFor::Step(AlondraStep::Delivering)));
    assert_eq!(read.unlisted, None);
}

/// A delivery the user sent back with changes, whose node took the changes and then asked the
/// orchestrator something it must have answered before it goes on, is not «delivering»: the node
/// holds the changes and waits for that answer.
#[test]
fn a_node_that_took_the_changes_and_asks_the_orchestrator_is_not_delivering() {
    use super::lifecycle_cache_tests::{run, Step};
    let mut s = three_tasks();
    let task = TaskId::from(SDK);
    let asked = |s: &mut DomainState| exec(s, node(NODE), Command::AskQuestion {
        attachments: vec![], interaction_id: "to-orchestrator".into(), scope: Scope::Task { task_id: SDK.into() },
        recipient: Recipient::Orchestrator, body: "Split the follow-up?".into(), blocking: true,
        options: vec![QuestionOption { id: "split".into(), label: "Split it".into(), description: None },
            QuestionOption { id: "keep".into(), label: "Keep one task".into(), description: None }],
        card: CardSpec::derived(CardKind::Decide, "Decide whether to split the follow-up"),
    }).unwrap();
    run(&mut s, &Step::Deliver(2), 0).unwrap();
    apply(&mut s, &Move::Sdk(Act::SendBack), 1);
    run(&mut s, &Step::TakeInput, 2).unwrap();
    asked(&mut s);
    run(&mut s, &Step::TurnEnded, 3).unwrap();
    let index = ReadIndex::build(&s);
    assert_eq!(s.task_handed_back(&task), Some(Instruction::Changes));
    assert_eq!(situation(&index, &task), Situation::Waiting(WaitsFor::Step(AlondraStep::Answering)), "the node already took the changes and waits for the orchestrator");

#[test]
fn a_paused_task_the_gate_queues_waits_for_a_slot_and_nothing_else_changes() {
    assert_eq!(Situation::Paused.queued_at_gate(true), Situation::Waiting(WaitsFor::Slot));
    assert_eq!(Situation::Paused.queued_at_gate(false), Situation::Paused);
    for other in [Situation::NeedsYou, Situation::Finished, Situation::Cancelled, Situation::Working(Doing::Turn),
        Situation::Waiting(WaitsFor::Disk), Situation::Delivered(Delivery::ForReview)] {
        assert_eq!(other.queued_at_gate(true), other);
    }
}
