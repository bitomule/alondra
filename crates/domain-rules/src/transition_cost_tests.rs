//! A command reads the cards in proportion to how many there are, never once per task: the rules
//! that ask every task whether a card holds it find its cards in one pass over the interactions,
//! and those cards answer what scanning every interaction answers.
use super::lifecycle_cache_tests::{run, step, three_tasks};
use super::lifecycle_tests::{must, node, task, user, world};
use super::*;
use proptest::prelude::*;

const DELIVERIES: usize = 3;

fn deliver(task: &str, n: usize) -> Command {
    Command::PresentResult {
        attachments: vec![], task_id: task.into(), interaction_id: format!("delivery-{task}-{n}").into(), body: "Done".into(),
        is_final: true, nothing_to_review: false, targets: vec![DeliveryTarget::Text { text: "Done".into() }], checklist: vec![],
        card: CardSpec::delivery("Review the result", &[], true),
    }
}

/// `tasks` started tasks whose nodes delivered `DELIVERIES` times each.
fn delivered(tasks: usize) -> DomainState {
    let mut s = world();
    for t in 0..tasks {
        let (id, agent) = (format!("task-{t}"), format!("node-{t}"));
        must(&mut s, Actor::System, Command::RegisterAgent { agent_id: agent.as_str().into(), name: agent.clone(), at: 0 });
        task(&mut s, &id, Some(&agent));
        must(&mut s, node(&agent), Command::StartTask { task_id: id.as_str().into() });
        for n in 0..DELIVERIES {
            must(&mut s, node(&agent), deliver(&id, n));
        }
    }
    s
}

/// Five delivered tasks, and `ended` more that are done and left no card.
fn ended(ended: usize) -> DomainState {
    let mut s = delivered(5);
    let template = s.tasks[&TaskId::from("task-4")].clone();
    for t in 0..ended {
        let id = TaskId::from(format!("ended-{t}").as_str());
        s.tasks.insert(id.clone(), Task { id, state: TaskState::Done, agent_id: None, final_presentation: None, ..template.clone() });
    }
    s
}

/// The cards one more delivery of the first task reads among `tasks` delivered tasks.
fn cards_read_by_a_delivery(tasks: usize) -> u64 {
    cards_read_by_a_delivery_in(&delivered(tasks))
}

fn cards_read_by_a_delivery_in(s: &DomainState) -> u64 {
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: "measured".into(),
        expected_revision: s.revision, command: deliver("task-0", DELIVERIES),
    };
    let before = measure::cards_read();
    s.preview(&node("node-0"), &envelope).unwrap();
    measure::cards_read() - before
}

#[test]
fn a_command_reads_the_cards_once_per_rule_not_once_per_task() {
    let (few, many) = (cards_read_by_a_delivery(5), cards_read_by_a_delivery(20));
    assert!(many <= 4 * few, "with four times the tasks and cards a delivery read {many} cards, against {few}: more than four times as many");
}

// Most tasks of a real domain have ended: a rule that looks for what each ended task left open
// reads every card once, not once per ended task.
#[test]
fn ended_tasks_do_not_multiply_the_cards_a_command_reads() {
    let (few, many) = (cards_read_by_a_delivery_in(&ended(5)), cards_read_by_a_delivery_in(&ended(80)));
    assert!(many <= 2 * few, "with sixteen times the ended tasks and the same cards a delivery read {many} cards, against {few}");
}

#[test]
fn started_tasks_do_not_scan_start_problem_cards_on_each_observation() {
    let state = delivered(20);
    let before = measure::cards_read();
    for _ in 0..4 {
        assert!(state.react(&Default::default()).is_empty());
    }
    assert_eq!(measure::cards_read() - before, 0,
        "observing already started tasks needs no start-problem history");
}

#[test]
fn waiting_tasks_share_one_start_problem_history_scan() {
    let mut state = delivered(20);
    for task in state.tasks.values_mut() {
        task.state = TaskState::NotStarted;
        task.agent_id = None;
        task.started = false;
    }
    let before = measure::cards_read();
    assert!(state.react(&Default::default()).is_empty());
    let reads = measure::cards_read() - before;
    assert_eq!(reads, state.interactions.len() as u64, "all waiting tasks share one history scan");
}

fn task_cards_agree_with_the_scans(s: &DomainState) -> Result<(), TestCaseError> {
    let cards = TaskCards::new(s);
    for t in s.tasks.keys() {
        prop_assert_eq!(cards.blockers(t), s.blockers(t), "blockers of {}", t);
        prop_assert_eq!(cards.unanswered(t), s.unanswered(t), "unanswered of {}", t);
        prop_assert_eq!(cards.asked_aside(t), s.interactions.values().any(|i| s.asked_aside(i, t)), "asked aside in {}", t);
        prop_assert_eq!(cards.first_blocker(t), Holds::first_blocker(s, t), "first blocker of {}", t);
    }
    for i in s.interactions.keys() {
        prop_assert_eq!(cards.first_card(i), Holds::first_card(s, i), "first card in front of {}", i);
    }
    let nodes = NodeTasks::new(s);
    for agent in s.agents.keys() {
        prop_assert_eq!(nodes.current(agent).map(|t| &t.id), s.node_task(agent).map(|t| &t.id), "task of node {}", agent);
    }
    let waiting: Vec<&TaskId> = s.tasks_waiting_for_turn().into_iter().map(|t| &t.id).collect();
    let one_by_one: Vec<&TaskId> = s.tasks.values().filter(|t| !t.over()
        && t.agent_id.as_ref().filter(|agent| Some(*agent) != s.orchestrator_session.as_ref()).is_some_and(|agent| s.through_sdk(agent)
            && !s.task_facts(agent).closing && s.node_pause(agent).is_none_or(|pause| pause.released))
        && s.lifecycle_of(&t.id).is_ok_and(|lifecycle| lifecycle.waits_for_slot())).map(|t| &t.id).collect();
    prop_assert_eq!(waiting, one_by_one, "tasks waiting for a turn");
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_cards_found_in_one_pass_answer_what_scanning_every_interaction_answers(steps in prop::collection::vec(step(), 1..40)) {
        let mut s = three_tasks();
        task_cards_agree_with_the_scans(&s)?;
        for (n, step) in steps.iter().enumerate() {
            let _ = run(&mut s, step, n);
            task_cards_agree_with_the_scans(&s)?;
        }
    }
}
