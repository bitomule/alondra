//! The lifecycle the domain keeps for each task is always the one a full replay of its journal
//! gives: after every command, accepted or refused, whatever the order of starts, deliveries,
//! reopenings, cancellations and the facts of an SDK execution.
use super::lifecycle_tests::{exec, node, sdk_turn, task, user, world};
use super::*;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;

const TASKS: [&str; 2] = ["a-task", "b-task"];
const SDK: &str = "s-task";

#[derive(Clone, Debug)]
pub(super) enum Step {
    Start(usize),
    Ask(usize),
    Answer(usize),
    Deliver(usize),
    Accept(usize),
    Complete(usize),
    Reopen(usize),
    Cancel(usize),
    NodeEnded(usize),
    Tell,
    TakeInput,
    TurnEnded,
    Stale,
}

pub(super) fn step() -> impl Strategy<Value = Step> {
    let any_task = 0..TASKS.len() + 1;
    prop_oneof![
        any_task.clone().prop_map(Step::Start),
        any_task.clone().prop_map(Step::Ask),
        any_task.clone().prop_map(Step::Answer),
        any_task.clone().prop_map(Step::Deliver),
        any_task.clone().prop_map(Step::Accept),
        any_task.clone().prop_map(Step::Complete),
        any_task.clone().prop_map(Step::Reopen),
        any_task.clone().prop_map(Step::Cancel),
        any_task.prop_map(Step::NodeEnded),
        Just(Step::Tell),
        Just(Step::TakeInput),
        Just(Step::TurnEnded),
        Just(Step::Stale),
    ]
}

/// Two `claude --bg` tasks with their nodes and one task whose SDK execution runs a turn.
pub(super) fn three_tasks() -> DomainState {
    let mut s = world();
    task(&mut s, TASKS[0], Some("a"));
    task(&mut s, TASKS[1], Some("b"));
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "c".into(), name: "c".into(), at: 0 });
    sdk_turn(&mut s, SDK, "c");
    s
}

fn must(s: &mut DomainState, actor: Actor, command: Command) {
    super::lifecycle_tests::must(s, actor, command);
}

fn of(at: usize) -> (&'static str, &'static str) {
    [(TASKS[0], "a"), (TASKS[1], "b"), (SDK, "c")][at]
}

fn delivery(task: &str, n: usize) -> InteractionId {
    format!("delivery-{task}-{n}").into()
}

fn targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "Done".into() }]
}

fn last_delivery(s: &DomainState, task: &str) -> Option<InteractionId> {
    s.interactions.values().filter(|i| i.id.0.starts_with(&format!("delivery-{task}-"))).max_by_key(|i| i.order).map(|i| i.id.clone())
}

pub(super) fn run(s: &mut DomainState, step: &Step, n: usize) -> Result<CommandOutcome, DomainError> {
    let options = || vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }];
    match *step {
        Step::Start(at) => exec(s, node(of(at).1), Command::StartTask { task_id: of(at).0.into() }),
        Step::Ask(at) => exec(s, node(of(at).1), Command::AskQuestion {
            attachments: vec![], interaction_id: format!("question-{n}").into(), scope: Scope::Task { task_id: of(at).0.into() },
            recipient: Recipient::User, body: "Which way?".into(), blocking: true, options: options(),
            card: CardSpec::question("Choose the way", &options()),
        }),
        Step::Answer(at) => {
            let open = s.interactions.values()
                .filter(|i| i.scope == Scope::Task { task_id: of(at).0.into() } && i.id.0.starts_with("question-") && s.open(i))
                .map(|i| i.id.clone()).next().unwrap_or_else(|| "question-none".into());
            exec(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: open, option_id: "yes".into(), inputs: BTreeMap::new() })
        }
        Step::Deliver(at) => exec(s, node(of(at).1), Command::PresentResult {
            attachments: vec![], task_id: of(at).0.into(), interaction_id: delivery(of(at).0, n), body: "Done".into(), is_final: true,
            nothing_to_review: false, targets: targets(), checklist: vec![], card: CardSpec::delivery("Review the result", &[], true),
        }),
        Step::Accept(at) => {
            let presentation_id = last_delivery(s, of(at).0).unwrap_or_else(|| delivery(of(at).0, n));
            exec(s, user(), Command::AcceptDelivery { presentation_id, targets: targets() })
        }
        Step::Complete(at) => {
            let presentation_id = last_delivery(s, of(at).0).unwrap_or_else(|| delivery(of(at).0, n));
            exec(s, Actor::System, Command::CompleteTask { task_id: of(at).0.into(), presentation_id })
        }
        Step::Reopen(at) => exec(s, user(), Command::ReopenTask { task_id: of(at).0.into() }),
        Step::Cancel(at) => exec(s, user(), Command::CancelTask { task_id: of(at).0.into() }),
        Step::NodeEnded(at) => exec(s, Actor::System, Command::RecordNodeEnd { task_id: of(at).0.into(), end: NodeEnd::Closed, agent_id: Some(of(at).1.into()) }),
        Step::Tell => exec(s, Actor::System, Command::TellNode { task_id: SDK.into(), text: format!("Notice {n}"), from_user: false, answers: None }),
        Step::TakeInput => {
            let lifecycle = s.lifecycle_of(&SDK.into())?;
            let input = lifecycle.pending_inputs().first().map_or_else(|| "input:none".into(), |input| input.as_str().to_owned());
            exec(s, Actor::System, Command::RecordRuntimeFacts {
                task_id: SDK.into(), generation: lifecycle.generation(),
                facts: vec![RuntimeObservation::TurnStarted { turn: format!("turn-{n}"), input }],
            })
        }
        Step::TurnEnded => {
            let turn = s.lifecycle_journal[&TaskId::from(SDK)].iter().rev().find_map(|record| match record {
                LifecycleRecord::TurnStarted { turn, .. } => Some(turn.clone()),
                _ => None,
            }).unwrap_or_else(|| "turn-none".into());
            let generation = s.lifecycle_of(&SDK.into())?.generation();
            exec(s, Actor::System, Command::RecordTurnBoundary { task_id: SDK.into(), generation, turn, failure: None, failed: None })
        }
        Step::Stale => s.execute(&Actor::System, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("stale-{n}").into(),
            expected_revision: s.revision.saturating_sub(1), command: Command::TellNode { task_id: SDK.into(), text: "Late".into(), from_user: false, answers: None },
        }),
    }
}

fn kept(s: &DomainState) -> Vec<(TaskId, Arc<alondra_domain::TaskLifecycle>)> {
    s.lifecycle_journal.keys().map(|task| (task.clone(), s.lifecycle_of(task).unwrap())).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_kept_lifecycle_of_every_task_is_its_journals_replay(steps in prop::collection::vec(step(), 1..40)) {
        let mut s = three_tasks();
        prop_assert_eq!(s.kept_lifecycles_match_journals(), Ok(()));
        for (n, step) in steps.iter().enumerate() {
            let before = kept(&s);
            let outcome = run(&mut s, step, n);
            prop_assert_eq!(s.kept_lifecycles_match_journals(), Ok(()), "after {:?} in {:?}", step, steps);
            if outcome.is_err() {
                let after = kept(&s);
                prop_assert!(before.len() == after.len() && before.iter().zip(&after).all(|((a, x), (b, y))| a == b && Arc::ptr_eq(x, y)),
                    "the refused {:?} left every kept lifecycle as it was", step);
            }
        }
    }
}

#[test]
fn a_state_read_without_its_kept_lifecycles_reads_its_journals() {
    let mut s = three_tasks();
    must(&mut s, Actor::System, Command::TellNode { task_id: SDK.into(), text: "Notice".into(), from_user: false, answers: None });
    let read: DomainState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert!(read.kept_lifecycles_match_journals().is_err(), "nothing is restored from the DTO");
    for (task, lifecycle) in kept(&s) {
        assert_eq!(*read.lifecycle_of(&task).unwrap(), *lifecycle, "{task} is replayed from its journal");
    }
}

#[test]
fn a_journal_moved_by_hand_is_replayed_rather_than_read_from_what_was_kept() {
    let mut s = three_tasks();
    let task = TaskId::from(SDK);
    let kept = s.lifecycle_of(&task).unwrap();
    s.lifecycle_journal.get_mut(&task).unwrap().push(LifecycleRecord::Told { input: "input:by-hand".into(), text: Some("By hand".into()), answers: None });
    let read = s.lifecycle_of(&task).unwrap();
    assert_eq!(read.revision(), kept.revision() + 1);
    assert_eq!(*read, replay(&task, &s.lifecycle_journal[&task]).unwrap());
}

/// A turn that ends unheard leaves its card under the final delivery the node presents next. Once
/// the user accepted it, the node asks something; the user's feedback takes the acceptance back, so
/// the question withdraws the delivery and the node is handed it back with the feedback as its next
/// input, which also tells it how the task goes on.
#[test]
fn feedback_hands_back_an_accepted_delivery_a_question_followed_while_its_turn_ended_unheard() {
    let mut s = three_tasks();
    for (n, step) in [Step::Ask(2), Step::TurnEnded, Step::Answer(2), Step::Deliver(2), Step::Accept(2), Step::Ask(2)].iter().enumerate() {
        run(&mut s, step, n).unwrap_or_else(|error| panic!("{step:?} was refused: {error}"));
    }
    assert!(s.lifecycle_of(&SDK.into()).unwrap().awaits().is_some(), "the delivery stands over the wait of the turn that ended on the question");
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: "feedback".into(), expected_revision: s.revision,
        command: Command::SubmitFeedback { merge_failure: None,
            attachments: vec![], presentation_id: delivery(SDK, 3), targets: targets(), body: "Change it".into(), instruction: Instruction::Changes,
        },
    };
    assert_eq!(s.guard(&user(), &envelope), Ok(()));
    s.execute(&user(), &envelope).unwrap();
    assert!(s.interactions[&delivery(SDK, 3)].withdrawn, "the question withdraws the delivery");
    assert_eq!(s.lifecycle_of(&SDK.into()).unwrap().phase(), alondra_domain::TaskPhase::InProgress);
    assert_eq!(s.lifecycle_of(&SDK.into()).unwrap().awaits(), None, "the handed back delivery is what the turn waited for");
    assert_eq!(s.kept_lifecycles_match_journals(), Ok(()));
}

