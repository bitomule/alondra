//! A node of the Agent SDK is what its task's lifecycle says: every reader (the orchestrator's
//! `node` and `tasks_in_progress`, the screens and the gate) counts its task the same, its phase
//! never moves, and a pause the gate holds it in lives in its task's lifecycle.
use super::lifecycle_cache_tests::{run, step, three_tasks, Step};
use super::lifecycle_tests::{assert_aggregate_rules, exec, must, task, user, world};
use super::*;
use alondra_domain::{Capacity, LiveState};
use proptest::prelude::*;

const SDK: &str = "s-task";
const NODE: &str = "c";

#[test]
fn only_a_closed_and_reopened_task_can_choose_a_new_native_provider_and_account() {
    use crate::accounts::{AccountId, AccountKey, Provider};

    let mut s = world();
    task(&mut s, "planned", None);
    assert_eq!(s.pending_reopening(&"planned".into()), None);
    assert_eq!(s.pending_reopening(&"missing".into()), None);

    let mut s = three_tasks();
    let id = TaskId::from(SDK);
    assert_eq!(s.pending_reopening(&id), None, "an active execution cannot use reopening's exception");
    must(&mut s, user(), Command::CancelTask { task_id: id.clone() });
    assert_eq!(s.pending_reopening(&id), None);
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: id.clone(), end: NodeEnd::Closed, agent_id: Some(NODE.into()) });
    assert_eq!(s.pending_reopening(&id), None, "a closed task has not yet been reopened");
    must(&mut s, user(), Command::ReopenTask { task_id: id.clone() });
    let reopened = s.lifecycle_journal[&id].iter().rev().find_map(|record| match record {
        LifecycleRecord::Reopened { input } => Some(input.as_str()),
        _ => None,
    }).unwrap();
    assert_eq!(s.pending_reopening(&id), Some(reopened));
    assert!(s.tasks[&id].agent_id.is_none());
    assert_aggregate_rules(&s);

    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "fresh".into(), name: "Fresh".into(), at: 1 });
    let account = AccountId::parse("work").unwrap();
    let admit = |provider, account, instance: &str| Command::AdmitExecution {
        provider, task_id: id.clone(), agent_id: "fresh".into(), conversation: "fresh-codex-conversation".into(),
        instance: instance.into(), host: "sdk-host".into(), model: None, account, leaves_limit: None,
    };
    must(&mut s, Actor::System, admit(Provider::Codex, Some(account.clone()), "first"));
    assert_eq!(s.tasks[&id].account_key(), AccountKey::new(Provider::Codex, account.clone()));
    assert_eq!(s.tasks[&id].agent_id, Some("fresh".into()));
    assert_eq!(s.pending_reopening(&id), None, "the first admission consumes the reopening");
    for (provider, account) in [(Provider::Claude, Some(account)), (Provider::Codex, None)] {
        let revision = s.revision;
        let journal = s.lifecycle_journal.clone();
        assert_eq!(exec(&mut s, Actor::System, admit(provider, account, "rejected")).unwrap_err().code, "active_account_immutable");
        assert_eq!(s.revision, revision);
        assert_eq!(s.lifecycle_journal, journal);
        assert_eq!(s.pending_reopening(&id), None);
    }
    assert_aggregate_rules(&s);
    must(&mut s, Actor::System, Command::ReleaseTask { task_id: id.clone(), reason: "The host ended".into() });
    assert_eq!(s.pending_reopening(&id), None, "releasing the new execution cannot reuse the old reopening");
    assert_eq!(exec(&mut s, Actor::System, admit(Provider::Claude, None, "after-release")).unwrap_err().code, "active_account_immutable");
    assert_aggregate_rules(&s);
}

#[test]
fn reopening_with_the_original_node_kept_cannot_change_its_native_session_account() {
    use crate::accounts::Provider;

    let mut s = three_tasks();
    let id = TaskId::from(SDK);
    must(&mut s, user(), Command::CancelTask { task_id: id.clone() });
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: id.clone(), end: NodeEnd::KeptByUser, agent_id: Some(NODE.into()) });
    must(&mut s, user(), Command::ReopenTask { task_id: id.clone() });
    assert_eq!(s.pending_reopening(&id), None);
    assert_eq!(s.tasks[&id].agent_id, Some(NODE.into()));
    assert!(s.tasks[&id].started);
    assert_eq!(exec(&mut s, Actor::System, Command::AdmitExecution {
        provider: Provider::Codex, task_id: id, agent_id: NODE.into(), conversation: "other-conversation".into(),
        instance: "other-instance".into(), host: "sdk-host".into(), model: None, account: None, leaves_limit: None,
    }).unwrap_err().code, "active_account_immutable");
    assert_aggregate_rules(&s);
}

#[derive(Clone, Debug)]
enum Act {
    Task(Step),
    Pause { by_user: bool },
    Resume,
    Interrupt,
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        4 => step().prop_map(Act::Task),
        1 => any::<bool>().prop_map(|by_user| Act::Pause { by_user }),
        1 => Just(Act::Resume),
        1 => Just(Act::Interrupt),
    ]
}

fn pause(by_user: bool, since: Millis) -> Pause {
    let reason = if by_user { PauseReason::ByUser } else { PauseReason::Declared { reason: "The build queue is full".into() } };
    Pause { reason, since, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false }
}

/// What the gate does: it pauses the node, and resumes it by telling it how it goes on first.
fn perform(s: &mut DomainState, act: &Act, n: usize) {
    let at = 1_000 + n as Millis;
    let _ = match act {
        Act::Task(step) => run(s, step, n).map(drop),
        Act::Pause { by_user } => exec(s, Actor::System, Command::MoveNode { agent_id: NODE.into(), event: NodeEvent::Paused { pause: pause(*by_user, at) }, at }).map(drop),
        Act::Resume => {
            let _ = exec(s, Actor::System, Command::TellNode { task_id: SDK.into(), text: format!("Carry on {n}"), from_user: false, answers: None });
            exec(s, Actor::System, Command::MoveNode { agent_id: NODE.into(), event: NodeEvent::Resumed, at }).map(drop)
        }
        Act::Interrupt => exec(s, Actor::System, Command::InterruptTurn { agent_id: NODE.into() }).map(drop),
    };
}

/// Every reader of the SDK task, against its lifecycle's `LiveState`.
fn agree(s: &DomainState) -> Result<(), String> {
    let agent = AgentId::from(NODE);
    let task = &s.tasks[&TaskId::from(SDK)];
    let phase = s.node_phase(&agent).cloned();
    if !matches!(phase, Some(NodePhase::ThroughSdk | NodePhase::Closed)) {
        return Err(format!("the phase of a node of the Agent SDK moved: {phase:?}"));
    }
    let live = s.live_state(&task.id);
    let open = !task.state.terminal() && task.state != TaskState::Closing && !task.archived;
    if open && task.agent_id.is_some() && s.task_activity(task) != live.clone().map(TaskActivity::Live) {
        return Err(format!("the task's activity {:?} is not its LiveState {live:?}", s.task_activity(task)));
    }
    let index = ReadIndex::build(s);
    if index.task(&task.id).map(|read| &read.live) != Some(&live) {
        return Err(format!("the screens read {:?}, the lifecycle says {live:?}", index.task(&task.id).map(|read| &read.live)));
    }
    let node = s.sdk_live(&agent);
    if task.agent_id.as_ref() == Some(&agent) && !task.over() && node != live {
        return Err(format!("the orchestrator's node reads {node:?}, the task says {live:?}"));
    }
    let slot = s.slots().iter().any(|a| a.id == agent);
    if slot != (task.agent_id.as_ref() == Some(&agent) && !task.state.terminal() && s.task_holds_slot(&task.id)) {
        return Err(format!("the gate counts a slot ({slot}) its task does not ({live:?})"));
    }
    let delivered_turn = matches!(&live, Some(LiveState::WaitingUser(cards)) if cards.iter().all(|card| task.final_presentation.as_ref() == Some(card)));
    if slot && !matches!(live, Some(LiveState::Working(_))) && !delivered_turn {
        return Err(format!("a node with a slot is {live:?}"));
    }
    if slot && s.node_waits_on_user(&agent) {
        return Err(format!("a node that waits on the user holds a slot ({live:?})"));
    }
    let pause = s.node_pause(&agent);
    if s.paused_nodes().contains_key(&agent) != pause.is_some() {
        return Err("the gate's pauses and the node's pause disagree".into());
    }
    if let Some(pause) = &pause {
        let held = match &pause.reason {
            PauseReason::ByUser => matches!(&live, Some(LiveState::WaitingUser(cards)) if cards.iter().any(|card| card.0.starts_with(NODE_PAUSE_PREFIX))),
            _ => matches!(live, Some(LiveState::WaitingCapacity(Capacity::Machine)) | Some(LiveState::WaitingUser(_))),
        };
        if !held || slot {
            return Err(format!("a paused node is {live:?} with a slot {slot}"));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn every_reader_counts_a_task_of_the_agent_sdk_as_its_lifecycle_says(acts in prop::collection::vec(act(), 1..60)) {
        let mut s = three_tasks();
        prop_assert_eq!(agree(&s), Ok(()));
        for (n, act) in acts.iter().enumerate() {
            perform(&mut s, act, n);
            prop_assert_eq!(agree(&s), Ok(()), "after {:?} in {:?}", act, acts);
        }
    }
}

/// A node delivers, is told something while its delivery waits for review (before or after the
/// turn that presented it ends), and pauses before it takes it up: it reads paused, never at
/// work, and holds no slot.
#[test]
fn a_node_paused_with_a_message_on_its_delivery_is_not_at_work() {
    let told_after_the_turn = [Act::Task(Step::Deliver(2)), Act::Task(Step::TurnEnded), Act::Task(Step::Tell)];
    let told_inside_the_turn = [Act::Task(Step::Deliver(2)), Act::Resume, Act::Task(Step::TurnEnded)];
    for (told, by_user) in [told_inside_the_turn, told_after_the_turn].into_iter().flat_map(|told| [false, true].map(|by_user| (told.clone(), by_user))) {
        let mut s = three_tasks();
        let acts = told.into_iter().chain([Act::Pause { by_user }]).collect::<Vec<_>>();
        for (n, act) in acts.iter().enumerate() {
            perform(&mut s, act, n);
            assert_eq!(agree(&s), Ok(()), "after {act:?} in {acts:?}");
        }
        assert!(s.node_pause(&NODE.into()).is_some(), "the node is paused after {acts:?}");
    }
}

/// The user pauses a node of the Agent SDK at work: its task's lifecycle holds it, not its phase,
/// and its own words resume it with what it was told.
#[test]
fn a_pause_of_a_node_of_the_agent_sdk_lives_in_its_task_s_lifecycle() {
    let mut s = three_tasks();
    let agent = AgentId::from(NODE);
    assert_eq!(s.node_phase(&agent), Some(&NodePhase::ThroughSdk));
    assert!(s.slots().iter().any(|a| a.id == agent), "its turn takes a slot");
    must(&mut s, Actor::System, Command::MoveNode { agent_id: NODE.into(), event: NodeEvent::Paused { pause: pause(true, 5) }, at: 5 });
    assert_eq!(s.node_phase(&agent), Some(&NodePhase::ThroughSdk), "no event moves its phase");
    assert_eq!(s.node_pause(&agent).map(|pause| (pause.reason, pause.since)), Some((PauseReason::ByUser, 5)));
    assert!(matches!(s.live_state(&SDK.into()), Some(LiveState::WaitingUser(cards)) if cards.first().0.starts_with(NODE_PAUSE_PREFIX)));
    assert!(!s.slots().iter().any(|a| a.id == agent), "a paused node takes no slot");
    run(&mut s, &Step::TurnEnded, 0).unwrap();
    assert!(matches!(s.live_state(&SDK.into()), Some(LiveState::WaitingUser(cards)) if cards.iter().all(|card| card.0.starts_with(NODE_PAUSE_PREFIX))),
        "its interrupted turn ends on the pause, with no card that asks how it goes on");
    assert_eq!(exec(&mut s, Actor::System, Command::MoveNode { agent_id: NODE.into(), event: NodeEvent::Resumed, at: 6 }).unwrap_err().code, "conflict",
        "the pause ends only once the node has something to go on with");
    must(&mut s, Actor::System, Command::TellNode { task_id: SDK.into(), text: "Go on".into(), from_user: false, answers: None });
    must(&mut s, Actor::System, Command::MoveNode { agent_id: NODE.into(), event: NodeEvent::Resumed, at: 7 });
    assert_eq!(s.node_pause(&agent), None);
    assert_eq!(s.live_state(&SDK.into()), Some(LiveState::Working(alondra_domain::Worker::Alondra(alondra_domain::Step::Delivering))));
    assert_eq!(s.node_phase(&agent), Some(&NodePhase::ThroughSdk));
}

/// A node's own wait for the machine is a wait on capacity, which the gate ends.
#[test]
fn a_node_of_the_agent_sdk_that_waits_for_the_machine_waits_on_capacity() {
    let mut s = three_tasks();
    must(&mut s, Actor::System, Command::MoveNode { agent_id: NODE.into(), event: NodeEvent::Paused { pause: pause(false, 5) }, at: 5 });
    assert_eq!(s.live_state(&SDK.into()), Some(LiveState::WaitingCapacity(Capacity::Machine)));
    assert!(matches!(s.node_pause(&NODE.into()).map(|pause| pause.reason), Some(PauseReason::Declared { .. })));
    assert_eq!(s.waiting_on(&SDK.into()), Some(RecordedWait::Machine), "its task's lifecycle explains what it waits for");
}

/// An older daemon kept a pause of a node of the Agent SDK in its phase. Opening the domain moves
/// it to the task's lifecycle: the gate reads the same pause, from the same moment, and counts the
/// same slots before and after, so the node neither loses its place nor takes a second one.
#[test]
fn an_older_daemon_s_pause_of_a_node_of_the_agent_sdk_moves_to_its_task_s_lifecycle_unchanged() {
    for by_user in [true, false] {
        let mut s = three_tasks();
        let agent = AgentId::from(NODE);
        s.agents.get_mut(&agent).unwrap().phase = NodePhase::Paused(pause(by_user, 42));
        let before = (s.paused_nodes().into_iter().map(|(agent, pause)| (agent, std::mem::discriminant(&pause.reason), pause.since)).collect::<Vec<_>>(),
            s.slots().iter().map(|a| a.id.clone()).collect::<Vec<_>>());
        assert_eq!(before.0.len(), 1, "the older daemon's pause");
        must(&mut s, Actor::System, Command::AdoptSdkNodes);
        let after = (s.paused_nodes().into_iter().map(|(agent, pause)| (agent, std::mem::discriminant(&pause.reason), pause.since)).collect::<Vec<_>>(),
            s.slots().iter().map(|a| a.id.clone()).collect::<Vec<_>>());
        assert_eq!(after, before, "the same pause and the same slots (by user: {by_user})");
        assert_eq!(s.node_phase(&agent), Some(&NodePhase::ThroughSdk));
        assert!(s.lifecycle_journal[&TaskId::from(SDK)].iter().any(|record| matches!(record, LifecycleRecord::Paused { card, .. } if card.0.starts_with(NODE_PAUSE_PREFIX))));
        assert_eq!(exec(&mut s, Actor::System, Command::AdoptSdkNodes).map(|outcome| outcome.changed), Ok(false), "once moved, nothing moves again");
    }
}

/// The phases an older daemon gave a node of the Agent SDK all become `ThroughSdk` at the first
/// commit, whatever command it is.
#[test]
fn any_commit_moves_an_older_phase_of_a_node_of_the_agent_sdk() {
    let mut s = three_tasks();
    let agent = AgentId::from(NODE);
    s.agents.get_mut(&agent).unwrap().phase = NodePhase::WaitingUser { in_turn: true };
    must(&mut s, Actor::System, Command::TellNode { task_id: SDK.into(), text: "Notice".into(), from_user: false, answers: None });
    assert_eq!(s.node_phase(&agent), Some(&NodePhase::ThroughSdk));
}

