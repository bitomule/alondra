use alondra_domain::*;
use alondra_foundation::TaskId;
use proptest::prelude::*;

mod support;
use support::*;

fn history_start(started: bool) -> TaskLifecycle {
    if started { running() } else { TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap() }
}

/// An answer the user or a policy gave that has not reached its turn yet: it owes its handing
/// over, whatever the execution is doing.
fn answer_on_its_way(task: &TaskLifecycle) -> bool {
    ["first", "second", "policy", "asked"]
        .into_iter()
        .any(|id| task.callback(&CallbackId::new(id).unwrap()) == Some(CallbackView::Answered))
}

fn something_runs(task: &TaskLifecycle) -> bool {
    matches!(task.activity(), ActivityView::Starting | ActivityView::Working | ActivityView::OwnWork | ActivityView::Continuation)
}

/// A delivery under review that the user told something while no execution runs: an execution
/// has to start to take it.
fn told_with_no_execution(task: &TaskLifecycle) -> bool {
    task.phase() == TaskPhase::Delivered
        && !task.pending_inputs().is_empty()
        && task.resource().is_none_or(|resource| resource == ResourceEvidence::Released)
        && !task.cards().into_iter().any(|(card, _)| task.card_owner(card) == Some(Owner::User))
}

fn owed(obligation: Option<Obligation>) -> Option<(Party, Owed)> {
    obligation.map(|obligation| (obligation.party, obligation.owed))
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0xa10d_a),
        ..ProptestConfig::default()
    })]
    #[test]
    fn every_live_task_owes_its_next_step_to_one_party(started in any::<bool>(), actions in prop::collection::vec(0u8..75, 1..160)) {
        let mut task = history_start(started);
        for action in actions {
            let Some(event) = arbitrary_event(&task, action) else { continue };
            let _ = task.apply(task.revision(), event);
            let obligation = task.obligation();
            if matches!(task.phase(), TaskPhase::Completed | TaskPhase::Cancelled) {
                prop_assert_eq!(obligation, None, "an ended task owes nothing: {:?}", task);
                continue;
            }
            let Some(obligation) = obligation else {
                return Err(TestCaseError::fail(format!("nobody owes the next step of a live task: {task:?}")));
            };
            let answer = answer_on_its_way(&task);
            match task.live() {
                Some(LiveState::WaitingUser(_)) if told_with_no_execution(&task) => prop_assert_eq!(owed(Some(obligation)), Some((Party::Alondra, Owed::Start)),
                    "a delivery told something with no execution to take it owes a start: {:?}", task),
                Some(LiveState::WaitingUser(_)) if !something_runs(&task) && !answer => prop_assert_eq!(obligation.party, Party::User,
                    "a task that waits on the user with nothing running owes the user: {:?}", task),
                Some(LiveState::Working(Worker::Turn)) if task.phase() != TaskPhase::Finishing && !answer => prop_assert_eq!(obligation.owed, Owed::Turn,
                    "a running turn is owed by the provider: {:?}", task),
                Some(LiveState::WaitingCapacity(capacity)) => prop_assert_eq!(obligation.owed, Owed::Room(capacity),
                    "a task held for capacity owes room: {:?}", task),
                _ => {}
            }
        }
    }

    #[test]
    fn an_obligation_keeps_its_start_until_what_is_owed_changes(started in any::<bool>(), actions in prop::collection::vec(0u8..75, 1..160)) {
        let mut task = history_start(started);
        for action in actions {
            let Some(event) = arbitrary_event(&task, action) else { continue };
            let before = task.obligation();
            if task.apply(task.revision(), event).is_err() {
                continue;
            }
            let after = task.obligation();
            match (before, after) {
                (Some(before), Some(after)) if owed(Some(before)) == owed(Some(after)) => prop_assert_eq!(after.since, before.since,
                    "the same step stays owed since it began: {:?}", task),
                (_, Some(after)) => prop_assert_eq!(after.since, task.revision(), "a new step is owed since now: {:?}", task),
                (_, None) => {}
            }
        }
    }

    #[test]
    fn a_rejected_event_leaves_the_obligation_untouched(started in any::<bool>(), actions in prop::collection::vec(0u8..75, 1..160)) {
        let mut task = history_start(started);
        for action in actions {
            let Some(event) = arbitrary_event(&task, action) else { continue };
            let before = task.obligation();
            if task.apply(task.revision(), event).is_err() {
                prop_assert_eq!(task.obligation(), before, "a refusal changes nothing: {:?}", task);
            }
        }
    }
}

/// The execution of a delivery under review was released with what the user told it still to
/// take: a new execution has to start for it, and nothing waits on the user.
#[test]
fn a_delivery_whose_execution_left_with_an_input_pending_owes_a_start_not_the_user() {
    let mut task = running();
    apply(&mut task, Event::Present { delivery: "result".into() });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None, stalled: false });
    assert_eq!(owed(task.obligation()), Some((Party::User, Owed::User)), "the review is the user's while nothing was told");
    apply(&mut task, Event::ExecutionReleased { input: input("changes") });
    assert_eq!(task.phase(), TaskPhase::Delivered);
    assert_eq!(task.pending_inputs(), vec![&input("changes")]);
    assert_eq!(owed(task.obligation()), Some((Party::Alondra, Owed::Start)));
}
