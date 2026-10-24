//! A command reads the cards in proportion to how many there are, never once per task: the rules
//! that ask every task whether a card holds it find its cards in one pass over the interactions,
//! and those cards answer what scanning every interaction answers.
use super::lifecycle_cache_tests::{run, step, three_tasks};
use super::lifecycle_tests::{must, node, task, world};
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

/// The cards one more delivery of the first task reads among `tasks` delivered tasks.
fn cards_read_by_a_delivery(tasks: usize) -> u64 {
    let s = delivered(tasks);
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
    let (few, many) = (cards_read_by_a_delivery(10), cards_read_by_a_delivery(40));
    assert!(many <= 4 * few, "with four times the tasks and cards a delivery read {many} cards, against {few}: more than four times as many");
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
