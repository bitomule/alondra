use alondra_domain::*;
use alondra_foundation::{AgentId, InteractionId, TaskId};

fn input(value: &str) -> InputId {
    InputId::new(value).unwrap()
}
fn turn(value: &str) -> TurnId {
    TurnId::new(value).unwrap()
}
fn work(value: &str) -> WorkId {
    WorkId::new(value).unwrap()
}
fn continuation(value: &str) -> ContinuationId {
    ContinuationId::new(value).unwrap()
}
fn operation(value: &str) -> OperationId {
    OperationId::new(value).unwrap()
}
fn apply(task: &mut TaskLifecycle, event: Event) {
    task.apply(task.revision(), event).unwrap();
}
fn admit(task: &mut TaskLifecycle, instance: &str) {
    apply(
        task,
        Event::Admit {
            node: AgentId::from("node"),
            conversation: ConversationId::new("conversation").unwrap(),
            instance: InstanceId::new(instance).unwrap(),
            host: HostId::new("host").unwrap(),
        },
    );
}
fn fact(task: &mut TaskLifecycle, fact: RuntimeFact) {
    let stamp = task.execution().unwrap().clone();
    let cursor = task.cursor().unwrap() + 1;
    apply(
        task,
        Event::Runtime {
            stamp,
            cursor,
            fact,
        },
    );
}
fn running() -> TaskLifecycle {
    let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    admit(&mut task, "instance");
    fact(&mut task, RuntimeFact::ResourceAvailable);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("root"),
            trigger: Trigger::Input(input("initial")),
        },
    );
    task
}

#[test]
fn last_child_completion_waits_for_its_correlated_root_continuation() {
    let mut task = running();
    fact(
        &mut task,
        RuntimeFact::WorkStarted {
            turn: turn("root"),
            work: work("child"),
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("root"),
            unheard_card: "unused".into(),
            wake: None,
            awaits: None,
        },
    );
    assert_eq!(task.activity(), ActivityView::OwnWork);
    assert!(task.holds_slot());
    assert!(task.cards().is_empty());
    fact(
        &mut task,
        RuntimeFact::WorkEnded {
            work: work("child"),
            continuation: continuation("wake"),
        },
    );
    assert_eq!(task.activity(), ActivityView::Continuation);
    assert!(task.holds_slot());
    assert!(task.cards().is_empty());
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("resumed"),
            trigger: Trigger::OwnedContinuation(continuation("wake")),
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("resumed"),
            unheard_card: "unheard".into(),
            wake: None,
            awaits: None,
        },
    );
    assert_eq!(
        task.cards(),
        vec![(&InteractionId::from("unheard"), WaitReason::Unheard)]
    );
    assert!(!task.holds_slot());
}

#[test]
fn a_human_permission_and_owned_background_work_coexist_without_releasing_the_slot() {
    let mut task = running();
    fact(
        &mut task,
        RuntimeFact::WorkStarted {
            turn: turn("root"),
            work: work("child"),
        },
    );
    let callback = CallbackId::new("permission-callback").unwrap();
    fact(
        &mut task,
        RuntimeFact::Ask {
            card: "permission".into(),
            callback: callback.clone(),
            reason: WaitReason::Permission,
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("root"),
            unheard_card: "unused".into(),
            wake: None,
            awaits: None,
        },
    );
    assert_eq!(task.activity(), ActivityView::OwnWork);
    assert!(task.holds_slot());
    assert_eq!(
        task.cards(),
        vec![(&InteractionId::from("permission"), WaitReason::Permission)]
    );
    apply(
        &mut task,
        Event::Decide {
            card: "permission".into(),
            decision: Decision::Allow,
        },
    );
    assert!(task.holds_slot());
    fact(
        &mut task,
        RuntimeFact::CallbackSettled {
            callback,
            continuation: continuation("callback-wake"),
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("second"),
            trigger: Trigger::OwnedContinuation(continuation("callback-wake")),
        },
    );
    assert!(task.holds_slot());
    assert_eq!(task.activity(), ActivityView::Working);
}

#[test]
fn delivered_work_can_continue_and_acceptance_waits_for_the_exact_merge() {
    let mut task = running();
    apply(
        &mut task,
        Event::Present {
            delivery: "delivery".into(),
        },
    );
    assert_eq!(task.phase(), TaskPhase::Delivered);
    assert!(task.holds_slot());
    apply(
        &mut task,
        Event::Accept {
            delivery: "delivery".into(),
            merge: Some(operation("merge")),
        },
    );
    let before = format!("{task:?}");
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Finish {
                intent: FinishIntent::Complete,
                cleanup: operation("cleanup"),
                post_merge: None
            }
        ),
        Err(Refusal::InvalidState)
    );
    assert_eq!(format!("{task:?}"), before);
    assert_eq!(
        task.apply(
            task.revision(),
            Event::MergeObserved {
                operation: operation("wrong")
            }
        ),
        Err(Refusal::CorrelationMismatch)
    );
    apply(
        &mut task,
        Event::MergeObserved {
            operation: operation("merge"),
        },
    );
    apply(
        &mut task,
        Event::Finish {
            intent: FinishIntent::Complete,
            cleanup: operation("cleanup"),
            post_merge: Some(operation("post-merge")),
        },
    );
    assert_eq!(task.phase(), TaskPhase::Finishing);
    assert!(
        task.drain()
            .any(|item| *item == DrainObligation::PostMerge(operation("post-merge")))
    );
}

#[test]
fn cancellation_is_not_terminal_until_process_children_callbacks_and_cleanup_are_drained() {
    let mut task = running();
    fact(
        &mut task,
        RuntimeFact::WorkStarted {
            turn: turn("root"),
            work: work("child"),
        },
    );
    let callback = CallbackId::new("held").unwrap();
    fact(
        &mut task,
        RuntimeFact::Ask {
            card: "permission".into(),
            callback: callback.clone(),
            reason: WaitReason::Permission,
        },
    );
    apply(
        &mut task,
        Event::Finish {
            intent: FinishIntent::Cancel,
            cleanup: operation("cleanup"),
            post_merge: None,
        },
    );
    let obligations = task.drain().cloned().collect::<Vec<_>>();
    assert_eq!(obligations.len(), 4);
    assert_eq!(task.phase(), TaskPhase::Finishing);
    apply(
        &mut task,
        Event::DrainObserved {
            obligation: DrainObligation::Execution(InstanceId::new("instance").unwrap()),
        },
    );
    assert!(task.holds_slot());
    apply(
        &mut task,
        Event::DrainObserved {
            obligation: DrainObligation::Work(work("child")),
        },
    );
    assert!(!task.holds_slot());
    apply(
        &mut task,
        Event::DrainObserved {
            obligation: DrainObligation::Callback(callback),
        },
    );
    assert_eq!(task.phase(), TaskPhase::Finishing);
    apply(
        &mut task,
        Event::DrainObserved {
            obligation: DrainObligation::Cleanup(operation("cleanup")),
        },
    );
    assert_eq!(task.phase(), TaskPhase::Cancelled);
    assert!(task.execution().is_none());
    assert_eq!(task.activity(), ActivityView::None);
    assert!(!task.holds_slot());
}

#[test]
fn connection_loss_does_not_prove_exit_or_allow_a_replacement() {
    let mut task = running();
    fact(
        &mut task,
        RuntimeFact::ConnectionLost {
            problem_card: "recovery".into(),
        },
    );
    assert_eq!(task.resource(), Some(ResourceEvidence::ObservationPending));
    assert!(task.holds_slot());
    let before = format!("{task:?}");
    assert_eq!(
        task.apply(
            task.revision(),
            Event::RetireReleasedExecution {
                problem_card: "recovery".into(),
                input: input("retry")
            }
        ),
        Err(Refusal::InvalidState)
    );
    assert_eq!(format!("{task:?}"), before);
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    assert!(!task.holds_slot());
    let old = task.execution().unwrap().clone();
    apply(
        &mut task,
        Event::RetireReleasedExecution {
            problem_card: "recovery".into(),
            input: input("retry"),
        },
    );
    admit(&mut task, "second-instance");
    assert_eq!(task.execution().unwrap().generation(), old.generation() + 1);
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Runtime {
                stamp: old,
                cursor: 5,
                fact: RuntimeFact::Notification
            }
        ),
        Err(Refusal::StaleExecution)
    );
}

#[test]
fn an_already_exited_process_is_not_relaunched_to_finish_its_task() {
    let mut task = running();
    apply(
        &mut task,
        Event::Present {
            delivery: "delivery".into(),
        },
    );
    apply(
        &mut task,
        Event::Accept {
            delivery: "delivery".into(),
            merge: None,
        },
    );
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    apply(
        &mut task,
        Event::Finish {
            intent: FinishIntent::Complete,
            cleanup: operation("cleanup"),
            post_merge: None,
        },
    );
    assert_eq!(task.drain().count(), 1);
    apply(
        &mut task,
        Event::DrainObserved {
            obligation: DrainObligation::Cleanup(operation("cleanup")),
        },
    );
    assert_eq!(task.phase(), TaskPhase::Completed);
    assert!(task.execution().is_none());
}

#[test]
fn duplicate_gap_wrong_generation_and_wrong_revision_are_atomic_rejections() {
    let mut task = running();
    let stamp = task.execution().unwrap().clone();
    let before = format!("{task:?}");
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Runtime {
                stamp: stamp.clone(),
                cursor: 2,
                fact: RuntimeFact::Notification
            }
        ),
        Err(Refusal::DuplicateFact)
    );
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Runtime {
                stamp,
                cursor: 4,
                fact: RuntimeFact::Notification
            }
        ),
        Err(Refusal::CursorGap)
    );
    assert_eq!(
        task.apply(
            0,
            Event::Present {
                delivery: "delivery".into()
            }
        ),
        Err(Refusal::StaleRevision)
    );
    assert_eq!(format!("{task:?}"), before);
}

#[test]
fn reopen_requires_a_terminal_task_and_uses_a_new_execution_generation() {
    let mut task = running();
    let old = task.execution().unwrap().clone();
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Reopen {
                input: input("reopen")
            }
        ),
        Err(Refusal::InvalidState)
    );
    apply(
        &mut task,
        Event::Finish {
            intent: FinishIntent::Cancel,
            cleanup: operation("cleanup"),
            post_merge: None,
        },
    );
    let remaining = task.drain().cloned().collect::<Vec<_>>();
    for obligation in remaining {
        apply(&mut task, Event::DrainObserved { obligation });
    }
    apply(
        &mut task,
        Event::Reopen {
            input: input("reopen"),
        },
    );
    assert!(task.execution().is_none());
    admit(&mut task, "new-instance");
    assert!(task.execution().unwrap().generation() > old.generation());
}

#[test]
fn a_released_execution_queues_the_task_again_with_what_it_had_not_consumed() {
    let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    admit(&mut task, "instance");
    apply(
        &mut task,
        Event::ExecutionReleased {
            input: input("requeue"),
        },
    );
    assert_eq!(task.phase(), TaskPhase::InProgress);
    assert!(task.execution().is_none());
    assert_eq!(task.pending_inputs(), vec![&input("initial")], "the launch input was never consumed");
    assert!(!task.holds_slot());

    let mut task = running();
    apply(
        &mut task,
        Event::ExecutionReleased {
            input: input("requeue"),
        },
    );
    assert_eq!(task.pending_inputs(), vec![&input("requeue")]);
    admit(&mut task, "second");
    assert_eq!(task.execution().unwrap().generation(), 2);

    let mut task = running();
    apply(&mut task, Event::Present { delivery: "delivery".into() });
    apply(&mut task, Event::ExecutionReleased { input: input("requeue") });
    assert_eq!(task.pending_inputs(), vec![&input("requeue")], "a new execution would start with it");
    apply(&mut task, Event::Accept { delivery: "delivery".into(), merge: None });
    apply(&mut task, Event::Finish { intent: FinishIntent::Complete, cleanup: operation("cleanup"), post_merge: None });
    assert_eq!(task.drain().cloned().collect::<Vec<_>>(), vec![DrainObligation::Cleanup(operation("cleanup"))],
        "with no execution to take it, a continuation input does not hold back completion");
    let mut task = running();
    apply(&mut task, Event::Present { delivery: "delivery".into() });
    apply(&mut task, Event::HandBack { delivery: "delivery".into(), input: input("changes") });
    apply(&mut task, Event::Present { delivery: "again".into() });
    apply(&mut task, Event::Accept { delivery: "again".into(), merge: None });
    assert_eq!(
        task.apply(task.revision(), Event::Finish { intent: FinishIntent::Complete, cleanup: operation("cleanup"), post_merge: None }),
        Err(Refusal::InvalidState),
        "an execution that never took the user's changes cannot complete"
    );

    let mut task = running();
    fact(
        &mut task,
        RuntimeFact::WorkStarted {
            turn: turn("root"),
            work: work("background"),
        },
    );
    assert_eq!(
        task.apply(
            task.revision(),
            Event::ExecutionReleased {
                input: input("requeue")
            }
        ),
        Err(Refusal::InvalidState),
        "work it still owns cannot be released in silence"
    );
    assert_eq!(task.activity(), ActivityView::Working);

    let mut task = running();
    apply(
        &mut task,
        Event::Finish {
            intent: FinishIntent::Cancel,
            cleanup: operation("cleanup"),
            post_merge: None,
        },
    );
    assert_eq!(
        task.apply(
            task.revision(),
            Event::ExecutionReleased {
                input: input("requeue")
            }
        ),
        Err(Refusal::InvalidState),
        "a finishing task drains its execution instead"
    );
}

#[test]
fn empty_or_self_dependencies_and_empty_identities_are_rejected() {
    assert_eq!(InputId::new("  "), Err(IdentityError::EmptyIdentity));
    assert_eq!(
        TaskLifecycle::planned(TaskId::from(""), input("initial")),
        Err(Refusal::EmptyIdentity)
    );
    let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    assert_eq!(
        task.apply(
            task.revision(),
            Event::WaitForDependencies {
                dependencies: vec![]
            }
        ),
        Err(Refusal::EmptyObligation)
    );
    assert_eq!(
        task.apply(
            task.revision(),
            Event::WaitForDependencies {
                dependencies: vec![TaskId::from("task")]
            }
        ),
        Err(Refusal::SelfDependency)
    );
    apply(
        &mut task,
        Event::WaitForDependencies {
            dependencies: vec![TaskId::from("dependency")],
        },
    );
    assert!(task.execution().is_none());
    apply(
        &mut task,
        Event::DependenciesResolved {
            input: input("ready"),
        },
    );
    admit(&mut task, "instance");
}

#[test]
fn concurrent_callbacks_are_answered_independently_and_survive_a_delivery() {
    let mut task = running();
    fact(
        &mut task,
        RuntimeFact::WorkStarted {
            turn: turn("root"),
            work: work("child"),
        },
    );
    for id in ["first", "second"] {
        fact(
            &mut task,
            RuntimeFact::Ask {
                card: id.into(),
                callback: CallbackId::new(id).unwrap(),
                reason: WaitReason::Permission,
            },
        );
    }
    apply(
        &mut task,
        Event::Present {
            delivery: "result".into(),
        },
    );
    assert_eq!(task.cards().len(), 2);
    assert!(task.holds_slot());
    apply(
        &mut task,
        Event::Decide {
            card: "second".into(),
            decision: Decision::Allow,
        },
    );
    assert_eq!(
        task.cards(),
        vec![(&InteractionId::from("first"), WaitReason::Permission)]
    );
    fact(
        &mut task,
        RuntimeFact::CallbackSettled {
            callback: CallbackId::new("second").unwrap(),
            continuation: continuation("second-wake"),
        },
    );
    assert_eq!(task.delivery(), Some(&InteractionId::from("result")));
    assert!(task.holds_slot());
}

#[test]
fn completion_cannot_erase_an_unanswered_business_question() {
    let mut task = running();
    apply(
        &mut task,
        Event::Present {
            delivery: "result".into(),
        },
    );
    apply(
        &mut task,
        Event::Ask {
            card: "question".into(),
        },
    );
    apply(
        &mut task,
        Event::Accept {
            delivery: "result".into(),
            merge: None,
        },
    );
    let before = format!("{task:?}");
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Finish {
                intent: FinishIntent::Complete,
                cleanup: operation("cleanup"),
                post_merge: None
            }
        ),
        Err(Refusal::InvalidState)
    );
    assert_eq!(format!("{task:?}"), before);
    assert_eq!(
        task.cards(),
        vec![(&InteractionId::from("question"), WaitReason::Question)]
    );
}

#[test]
fn process_exit_preserves_permissions_and_adds_an_actionable_recovery_obligation() {
    let mut task = running();
    let callback = CallbackId::new("permission").unwrap();
    fact(
        &mut task,
        RuntimeFact::Ask {
            card: "permission".into(),
            callback: callback.clone(),
            reason: WaitReason::Permission,
        },
    );
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    assert_eq!(task.cards().len(), 2);
    assert_eq!(
        task.apply(
            task.revision(),
            Event::RetireReleasedExecution {
                problem_card: "recovery".into(),
                input: input("retry")
            }
        ),
        Err(Refusal::InvalidState)
    );
    fact(
        &mut task,
        RuntimeFact::CallbackAborted {
            callback,
            unheard_card: "unused".into(),
        },
    );
    assert_eq!(
        task.cards(),
        vec![(&InteractionId::from("recovery"), WaitReason::Recovery)]
    );
    apply(
        &mut task,
        Event::RetireReleasedExecution {
            problem_card: "recovery".into(),
            input: input("retry"),
        },
    );
    assert!(task.execution().is_none());
}

#[test]
fn aborting_the_last_callback_does_not_leave_an_open_task_silently_resting() {
    let mut task = running();
    let callback = CallbackId::new("permission").unwrap();
    fact(
        &mut task,
        RuntimeFact::Ask {
            card: "permission".into(),
            callback: callback.clone(),
            reason: WaitReason::Permission,
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("root"),
            unheard_card: "unused".into(),
            wake: None,
            awaits: None,
        },
    );
    fact(
        &mut task,
        RuntimeFact::CallbackAborted {
            callback,
            unheard_card: "unheard".into(),
        },
    );
    assert_eq!(
        task.cards(),
        vec![(&InteractionId::from("unheard"), WaitReason::Unheard)]
    );
    assert!(!task.holds_slot());
}

#[test]
fn implementation_and_changes_requested_stages_survive_queueing_and_recovery() {
    let mut task = running();
    apply(&mut task, Event::PlanApproved);
    assert_eq!(task.stage(), Some(WorkStage::Implement));
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    apply(
        &mut task,
        Event::RetireReleasedExecution {
            problem_card: "recovery".into(),
            input: input("retry"),
        },
    );
    admit(&mut task, "second");
    assert_eq!(task.stage(), Some(WorkStage::Implement));
    fact(&mut task, RuntimeFact::ResourceAvailable);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("second"),
            trigger: Trigger::Input(input("retry")),
        },
    );
    apply(
        &mut task,
        Event::Present {
            delivery: "result".into(),
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("second"),
            unheard_card: "unused".into(),
            wake: None,
            awaits: None,
        },
    );
    apply(
        &mut task,
        Event::HandBack {
            delivery: "result".into(),
            input: input("changes"),
        },
    );
    assert_eq!(task.stage(), Some(WorkStage::ChangesRequested));
}

#[test]
fn pause_blocks_new_admission_and_cannot_be_answered_as_an_ordinary_question() {
    let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    apply(
        &mut task,
        Event::Pause {
            card: "pause".into(),
            owner: Owner::User,
        },
    );
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Answer {
                card: "pause".into(),
                input: input("bypass")
            }
        ),
        Err(Refusal::InvalidState)
    );
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Admit {
                node: "node".into(),
                conversation: ConversationId::new("conversation").unwrap(),
                instance: InstanceId::new("instance").unwrap(),
                host: HostId::new("host").unwrap()
            }
        ),
        Err(Refusal::InvalidState)
    );
    apply(
        &mut task,
        Event::Resume {
            card: "pause".into(),
            input: input("resume"),
        },
    );
    admit(&mut task, "instance");
}

#[test]
fn a_turn_that_ends_with_nothing_running_waits_on_the_user_who_is_asked_how_it_goes_on() {
    let mut task = running();
    assert_eq!(task.live(), Some(LiveState::Working(Worker::Turn)));
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    assert_eq!(task.live(), Several::new([InteractionId::from("unheard")]).map(LiveState::WaitingUser));
}

fn ended(task: &mut TaskLifecycle, root: &str, wake: Option<&str>, awaits: Option<Await>) {
    fact(task, RuntimeFact::TurnEnded { turn: turn(root), unheard_card: InteractionId::from(format!("unheard-{root}")), wake: wake.map(input), awaits });
}
fn started(task: &mut TaskLifecycle, root: &str, by: &str) {
    fact(task, RuntimeFact::TurnStarted { turn: turn(root), trigger: Trigger::Input(input(by)) });
}

#[test]
fn the_first_empty_turn_wakes_the_node_and_the_second_asks_the_user() {
    let mut task = running();
    ended(&mut task, "root", Some("wake"), None);
    assert!(task.cards().is_empty(), "the first empty turn asks nobody");
    assert_eq!(task.pending_inputs(), vec![&input("wake")], "it wakes the node with what happened");
    assert_eq!(task.live(), Some(LiveState::Working(Worker::Alondra(Step::Delivering))));
    started(&mut task, "woken", "wake");
    let stamp = task.execution().unwrap().clone();
    let cursor = task.cursor().unwrap() + 1;
    let again = RuntimeFact::TurnEnded { turn: turn("woken"), unheard_card: "unheard-woken".into(), wake: Some(input("wake-again")), awaits: None };
    assert_eq!(task.apply(task.revision(), Event::Runtime { stamp, cursor, fact: again }), Err(Refusal::InvalidState), "a second wake in a row is never sent");
    ended(&mut task, "woken", None, None);
    assert!(task.pending_inputs().is_empty());
    assert_eq!(task.live(), Several::new([InteractionId::from("unheard-woken")]).map(LiveState::WaitingUser));
}

#[test]
fn any_new_fact_between_two_empty_turns_counts_from_zero_again() {
    let user_told = |task: &mut TaskLifecycle| {
        started(task, "woken", "wake");
        apply(task, Event::Tell { input: input("told") });
        fact(task, RuntimeFact::InputConsumed { input: input("told") });
    };
    let own_work = |task: &mut TaskLifecycle| {
        started(task, "woken", "wake");
        fact(task, RuntimeFact::WorkStarted { turn: turn("woken"), work: work("child") });
        fact(task, RuntimeFact::WorkEnded { work: work("child"), continuation: continuation("child-done") });
        fact(task, RuntimeFact::ContinuationConsumed { continuation: continuation("child-done") });
    };
    let asked = |task: &mut TaskLifecycle| {
        started(task, "woken", "wake");
        fact(task, RuntimeFact::Ask { card: "permission".into(), callback: CallbackId::new("permission").unwrap(), reason: WaitReason::Permission });
        apply(task, Event::Decide { card: "permission".into(), decision: Decision::Allow });
        fact(task, RuntimeFact::CallbackSettled { callback: CallbackId::new("permission").unwrap(), continuation: continuation("allowed") });
        fact(task, RuntimeFact::ContinuationConsumed { continuation: continuation("allowed") });
    };
    let cases: [(&str, &dyn Fn(&mut TaskLifecycle), &str); 3] = [("told", &user_told, "woken"), ("own work", &own_work, "woken"), ("a card", &asked, "woken")];
    for (between, step, root) in cases {
        let mut task = running();
        ended(&mut task, "root", Some("wake"), None);
        step(&mut task);
        ended(&mut task, root, Some("wake-again"), None);
        assert!(task.cards().is_empty(), "{between}: the user is not asked yet");
        assert!(task.pending_inputs().contains(&&input("wake-again")), "{between}: the node is woken again");
    }
}

#[test]
fn a_turn_recorded_before_wakes_existed_asks_the_user_at_once() {
    let mut task = running();
    ended(&mut task, "root", None, None);
    assert_eq!(task.live(), Several::new([InteractionId::from("unheard-root")]).map(LiveState::WaitingUser));
}

#[test]
fn a_wake_the_turn_has_no_use_for_is_refused() {
    let mut task = running();
    fact(&mut task, RuntimeFact::WorkStarted { turn: turn("root"), work: work("child") });
    let stamp = task.execution().unwrap().clone();
    let cursor = task.cursor().unwrap() + 1;
    let fact = RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: Some(input("wake")), awaits: None };
    assert_eq!(task.apply(task.revision(), Event::Runtime { stamp, cursor, fact }), Err(Refusal::InvalidState), "work of its own explains the turn");
}

#[test]
fn a_turn_that_ends_on_a_recorded_wait_is_that_wait_until_its_input_arrives() {
    for (awaits, live) in [
        (Await::User("question".into()), Several::new([InteractionId::from("question")]).map(LiveState::WaitingUser)),
        (Await::Orchestrator("message".into()), Some(LiveState::Working(Worker::Alondra(Step::Answering)))),
        (Await::Machine, Some(LiveState::WaitingCapacity(Capacity::Machine))),
        (Await::Tasks(Several::new([TaskId::from("dependency")]).unwrap()), Several::new([TaskId::from("dependency")]).map(LiveState::WaitingTask)),
    ] {
        let mut task = running();
        ended(&mut task, "root", None, Some(awaits.clone()));
        assert!(task.cards().is_empty(), "{awaits:?}: no card asks how it goes on");
        assert_eq!(task.live(), live, "{awaits:?}");
        assert!(!task.holds_slot(), "{awaits:?}: nothing runs");
        apply(&mut task, Event::Tell { input: input("answer") });
        assert_eq!(task.awaits(), None, "{awaits:?}: the input that ends the wait moves it on");
        assert_eq!(task.live(), Some(LiveState::Working(Worker::Alondra(Step::Delivering))), "{awaits:?}");
    }
    let mut task = running();
    let stamp = task.execution().unwrap().clone();
    let cursor = task.cursor().unwrap() + 1;
    let fact = RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: Some(Await::Tasks(Several::new([TaskId::from("task")]).unwrap())) };
    assert_eq!(task.apply(task.revision(), Event::Runtime { stamp, cursor, fact }), Err(Refusal::SelfDependency));
}

#[test]
fn a_pause_waits_on_who_it_names() {
    for (owner, live) in [
        (Owner::User, Several::new([InteractionId::from("pause")]).map(LiveState::WaitingUser)),
        (Owner::Capacity(Capacity::Machine), Some(LiveState::WaitingCapacity(Capacity::Machine))),
        (Owner::Capacity(Capacity::Usage), Some(LiveState::WaitingCapacity(Capacity::Usage))),
        (Owner::Capacity(Capacity::ClaudeApi), Some(LiveState::WaitingCapacity(Capacity::ClaudeApi))),
    ] {
        let mut task = running();
        apply(&mut task, Event::Pause { card: "pause".into(), owner });
        assert_eq!(task.live(), live, "{owner:?}");
    }
    let mut task = running();
    assert_eq!(task.apply(task.revision(), Event::Pause { card: "pause".into(), owner: Owner::Capacity(Capacity::Gate) }), Err(Refusal::InvalidState), "the gate holds a task only before it starts");
}

/// A pause the node took mid-turn holds the turn's end too: the turn ends on the pause, not on a
/// card that asks how the task goes on, and the pause ends without a new input only once what the
/// task goes on with already waits for its node.
#[test]
fn a_pause_ends_without_a_new_input_only_once_the_task_has_something_to_go_on_with() {
    let mut task = running();
    apply(&mut task, Event::Pause { card: "pause".into(), owner: Owner::Capacity(Capacity::Machine) });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    assert_eq!(task.live(), Some(LiveState::WaitingCapacity(Capacity::Machine)));
    assert!(!task.holds_slot(), "a resting execution on a pause takes no slot");
    assert_eq!(task.apply(task.revision(), Event::EndPause { card: "pause".into() }), Err(Refusal::InvalidState),
        "nothing tells the resting node how it goes on");
    apply(&mut task, Event::Tell { input: input("go-on") });
    assert_eq!(task.live(), Some(LiveState::WaitingCapacity(Capacity::Machine)), "what it was told waits for the pause to end");
    apply(&mut task, Event::EndPause { card: "pause".into() });
    assert_eq!(task.live(), Some(LiveState::Working(Worker::Alondra(Step::Delivering))));
    assert_eq!(task.apply(task.revision(), Event::EndPause { card: "pause".into() }), Err(Refusal::InvalidState), "no pause holds it any more");

    let mut task = running();
    apply(&mut task, Event::Pause { card: "pause".into(), owner: Owner::User });
    apply(&mut task, Event::Tell { input: input("resume") });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    assert_eq!(task.activity(), ActivityView::Resting, "what it was told inside its turn waits for the pause to end");
    apply(&mut task, Event::EndPause { card: "pause".into() });
    assert_eq!(task.live(), Some(LiveState::Working(Worker::Alondra(Step::Delivering))), "the pause's end hands it what it was told");
}

/// 26 Oct, t-alpha-report-1-local: the turn that read the notification of the node's background
/// work never reached Alondra, so its continuation still waited. The next turn, which «sigue»
/// started, ended with that continuation waiting again, nothing woke the node, and the task stayed
/// at work with nothing working for it for three hours.
#[test]
fn a_turn_takes_up_the_notifications_waiting_as_it_starts_and_its_empty_end_wakes_the_node() {
    let mut task = running();
    fact(&mut task, RuntimeFact::WorkStarted { turn: turn("root"), work: work("child") });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    fact(&mut task, RuntimeFact::WorkEnded { work: work("child"), continuation: continuation("wake") });
    assert_eq!(task.activity(), ActivityView::Continuation);
    apply(&mut task, Event::Tell { input: input("told") });
    fact(&mut task, RuntimeFact::TurnStarted { turn: turn("told"), trigger: Trigger::Input(input("told")) });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("told"), unheard_card: "unheard-told".into(), wake: Some(input("wake")), awaits: None });
    assert_eq!(task.pending_inputs(), vec![&input("wake")], "the empty turn wakes the node");
    assert_eq!(task.activity(), ActivityView::Starting);
    assert!(task.cards().is_empty(), "nobody is asked yet");
}

/// Codex reports reading a finished child's continuation after the turn that reads it has started;
/// that turn already took it, so the report is accepted, and one for a continuation nobody left is not.
#[test]
fn a_provider_reporting_the_continuation_its_turn_took_is_accepted_and_an_unknown_one_refused() {
    let mut task = running();
    fact(&mut task, RuntimeFact::WorkStarted { turn: turn("root"), work: work("child") });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    fact(&mut task, RuntimeFact::WorkEnded { work: work("child"), continuation: continuation("child-done") });
    apply(&mut task, Event::Tell { input: input("told") });
    fact(&mut task, RuntimeFact::TurnStarted { turn: turn("told"), trigger: Trigger::Input(input("told")) });
    fact(&mut task, RuntimeFact::ContinuationConsumed { continuation: continuation("child-done") });
    let stamp = task.execution().unwrap().clone();
    let cursor = task.cursor().unwrap() + 1;
    assert_eq!(
        task.apply(task.revision(), Event::Runtime { stamp, cursor, fact: RuntimeFact::ContinuationConsumed { continuation: continuation("child-done") } }),
        Err(Refusal::UnknownObligation),
        "a continuation is read once"
    );
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("told"), unheard_card: "unheard-told".into(), wake: None, awaits: None });
    assert_eq!(task.activity(), ActivityView::Resting);
}

/// A journal recorded before turns took up their notifications replays as it was recorded: a turn
/// started with two notifications waiting and ended without a wake leaves them waiting, and the
/// session ending then resumes the node (t-85fa650b… on 26 October; 0.3.8 refused its revision 60).
#[test]
fn a_turn_that_ends_without_a_wake_leaves_the_notifications_it_found_waiting() {
    let mut task = running();
    fact(&mut task, RuntimeFact::WorkStarted { turn: turn("root"), work: work("first") });
    fact(&mut task, RuntimeFact::WorkStarted { turn: turn("root"), work: work("second") });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    fact(&mut task, RuntimeFact::WorkEnded { work: work("first"), continuation: continuation("first-done") });
    fact(&mut task, RuntimeFact::WorkEnded { work: work("second"), continuation: continuation("second-done") });
    apply(&mut task, Event::Tell { input: input("told") });
    fact(&mut task, RuntimeFact::TurnStarted { turn: turn("told"), trigger: Trigger::Input(input("told")) });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("told"), unheard_card: "unheard-told".into(), wake: None, awaits: None });
    assert_eq!(task.activity(), ActivityView::Continuation);
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: Some(input("resumed")) });
    assert_eq!(task.pending_inputs(), vec![&input("resumed")]);
}

/// One step of an arbitrary history: an event the task may or may not accept.
fn arbitrary_event(task: &TaskLifecycle, action: u8) -> Option<Event> {
    let runtime = |fact| task.execution().cloned().map(|stamp| Event::Runtime { stamp, cursor: task.cursor().unwrap() + 1, fact });
    match action {
        0 => Some(Event::Admit { node: "node".into(), conversation: ConversationId::new("conversation").unwrap(), instance: InstanceId::new("instance").unwrap(), host: HostId::new("host").unwrap() }),
        1 => runtime(RuntimeFact::ResourceAvailable),
        2 => runtime(RuntimeFact::TurnStarted { turn: turn("root"), trigger: Trigger::Input(input("initial")) }),
        3 => runtime(RuntimeFact::WorkStarted { turn: turn("root"), work: work("child") }),
        4 => runtime(RuntimeFact::WorkEnded { work: work("child"), continuation: continuation("wake") }),
        5 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None }),
        6 => runtime(RuntimeFact::Ask { card: "first".into(), callback: CallbackId::new("first").unwrap(), reason: WaitReason::Permission }),
        7 => Some(Event::Decide { card: "first".into(), decision: Decision::Allow }),
        8 => Some(Event::Present { delivery: "result".into() }),
        9 => Some(Event::Accept { delivery: "result".into(), merge: None }),
        10 => Some(Event::Finish { intent: FinishIntent::Cancel, cleanup: operation("cleanup"), post_merge: None }),
        11 => task.drain().next().cloned().map(|obligation| Event::DrainObserved { obligation }),
        12 => Some(Event::Reopen { input: input("initial") }),
        13 => runtime(RuntimeFact::ResourceReleased { problem_card: "recovery".into() }),
        14 => runtime(RuntimeFact::ConnectionLost { problem_card: "recovery".into() }),
        15 => runtime(RuntimeFact::Ask { card: "second".into(), callback: CallbackId::new("second").unwrap(), reason: WaitReason::Permission }),
        16 => runtime(RuntimeFact::CallbackAborted { callback: CallbackId::new("first").unwrap(), unheard_card: "unheard".into() }),
        17 => runtime(RuntimeFact::CallbackSettled { callback: CallbackId::new("first").unwrap(), continuation: continuation("callback-wake") }),
        18 => Some(Event::PlanApproved),
        19 => Some(Event::WaitForDependencies { dependencies: vec![TaskId::from("dependency")] }),
        20 => Some(Event::DependenciesResolved { input: input("dependencies") }),
        21 => Some(Event::Pause { card: "pause".into(), owner: Owner::User }),
        39 => Some(Event::Pause { card: "pause".into(), owner: Owner::Capacity(Capacity::Usage) }),
        40 => Some(Event::Pause { card: "pause".into(), owner: Owner::Capacity(Capacity::ClaudeApi) }),
        41 => Some(Event::Accept { delivery: "result".into(), merge: Some(operation("merge")) }),
        42 => Some(Event::Finish { intent: FinishIntent::Complete, cleanup: operation("cleanup"), post_merge: Some(operation("after-merge")) }),
        43 => Some(Event::PostMergeDeclared { operation: operation("after-merge") }),
        44 => runtime(RuntimeFact::HostEnded { unheard_card: "ended".into(), resume: None }),
        45 => runtime(RuntimeFact::HostEnded { unheard_card: "ended".into(), resume: Some(input("resumed")) }),
        46 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: Some(input("wake")), awaits: None }),
        47 => runtime(RuntimeFact::TurnStarted { turn: turn("woken"), trigger: Trigger::Input(input("wake")) }),
        48 => runtime(RuntimeFact::TurnEnded { turn: turn("woken"), unheard_card: "unheard-woken".into(), wake: Some(input("wake-again")), awaits: None }),
        49 => runtime(RuntimeFact::TurnEnded { turn: turn("told"), unheard_card: "unheard-told".into(), wake: Some(input("wake")), awaits: None }),
        50 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: Some(Await::User("question".into())) }),
        51 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: Some(Await::Tasks(Several::new([TaskId::from("dependency")]).unwrap())) }),
        52 => runtime(RuntimeFact::TurnEnded { turn: turn("woken"), unheard_card: "unheard-woken".into(), wake: None, awaits: Some(Await::Machine) }),
        53 => runtime(RuntimeFact::TurnStarted { turn: turn("woken"), trigger: Trigger::Input(input("wake-again")) }),
        54 => Some(Event::Pause { card: "pause".into(), owner: Owner::Capacity(Capacity::Machine) }),
        55 => Some(Event::EndPause { card: "pause".into() }),
        56 => runtime(RuntimeFact::TurnEnded { turn: turn("told"), unheard_card: "unheard-told".into(), wake: None, awaits: None }),
        22 => Some(Event::Resume { card: "pause".into(), input: input("resume") }),
        23 => runtime(RuntimeFact::Notification),
        24 => Some(Event::RetireReleasedExecution { problem_card: "recovery".into(), input: input("retry") }),
        25 => Some(Event::RequestControl { control: operation("interrupt"), kind: Control::Interrupt { turn: task.turn().cloned() } }),
        26 => Some(Event::RequestControl { control: operation("model"), kind: Control::SetModel { model: "claude-sonnet".into() } }),
        27 => Some(Event::RequestControl { control: operation("mcp"), kind: Control::ReadMcpStatus }),
        28 => Some(Event::Compact { input: input("compact") }),
        29 => runtime(RuntimeFact::AnsweredByPolicy { callback: CallbackId::new("policy").unwrap(), decision: Decision::Deny { message: "Nodes cannot act as the user".into() } }),
        30 => Some(Event::Decide { card: "second".into(), decision: Decision::Deny { message: "No".into() } }),
        31 => Some(Event::MoveCard { from: "first".into(), to: "moved".into() }),
        32 => Some(Event::MoveCard { from: "moved".into(), to: "first".into() }),
        34 => task.turn().cloned().and_then(|turn| runtime(RuntimeFact::InterruptAccepted { turn })),
        35 => task.turn().cloned().map(|turn| Event::RequestControl { control: operation("force"), kind: Control::ForceStop { turn } }),
        36 => Some(Event::Tell { input: input("told") }),
        37 => runtime(RuntimeFact::TurnStarted { turn: turn("told"), trigger: Trigger::Input(input("told")) }),
        38 => runtime(RuntimeFact::TurnStarted { turn: turn("own"), trigger: Trigger::Unprompted }),
        _ => Some(Event::HandBack { delivery: "result".into(), input: input("changes") }),
    }
}

use proptest::prelude::*;
proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0xa10d_a),
        ..ProptestConfig::default()
    })]
    #[test]
    fn every_live_task_is_in_one_owned_state(started in any::<bool>(), actions in prop::collection::vec(0u8..56, 1..160)) {
        let mut task = if started { running() } else { TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap() };
        for action in actions {
            let Some(event) = arbitrary_event(&task, action) else { continue };
            let _ = task.apply(task.revision(), event);
            let live = task.live();
            if matches!(task.phase(), TaskPhase::Unstarted | TaskPhase::Completed | TaskPhase::Cancelled) {
                prop_assert_eq!(live, None, "only a live task has a state: {:?}", task);
                continue;
            }
            let Some(live) = live else {
                return Err(TestCaseError::fail(format!("a live task waits on nobody: {task:?}")));
            };
            let cards: Vec<(InteractionId, WaitReason)> = task.cards().into_iter().map(|(card, reason)| (card.clone(), reason)).collect();
            match live {
                LiveState::WaitingUser(waiting) => for card in waiting.iter() {
                    prop_assert!(task.delivery() == Some(card) || cards.iter().any(|(held, _)| held == card) || task.awaits() == Some(&Await::User(card.clone())),
                        "the user is asked by a card the task holds or the question its node asked: {:?} in {:?}", card, task);
                },
                LiveState::WaitingTask(tasks) => for dependency in tasks.iter() {
                    prop_assert!(task.dependencies().contains(&dependency) || matches!(task.awaits(), Some(Await::Tasks(awaited)) if awaited.iter().any(|t| t == dependency)),
                        "it waits on a task it depends on: {:?}", task);
                },
                LiveState::WaitingCapacity(Capacity::Gate) => prop_assert!(task.execution().is_none() && !task.pending_inputs().is_empty(),
                    "the gate holds a task that waits to start: {:?}", task),
                LiveState::WaitingCapacity(Capacity::Machine) => prop_assert!(task.awaits() == Some(&Await::Machine) || cards.iter().any(|(_, reason)| *reason == WaitReason::Pause),
                    "a node that waits for the machine said so, or the gate paused it: {:?}", task),
                LiveState::WaitingCapacity(_) => prop_assert!(cards.iter().any(|(_, reason)| *reason == WaitReason::Pause), "capacity holds it through a pause: {:?}", task),
                LiveState::Working(Worker::Turn) => prop_assert!(task.turn().is_some(), "a turn runs: {:?}", task),
                LiveState::Working(Worker::OwnWork) => prop_assert!(task.activity() == ActivityView::OwnWork || task.drain().any(|o| matches!(o, DrainObligation::Work(_))), "its own work runs: {:?}", task),
                LiveState::Working(Worker::AfterMerge) => prop_assert!(task.post_merge().is_some(), "it runs what it declared for after the merge: {:?}", task),
                LiveState::Working(Worker::Alondra(_)) => {}
            }
        }
    }

    /// No turn that ends empty is left without its wake or its card, and the node is never woken
    /// twice in a row: between two wakes there is a fact other than the wake's own turn.
    #[test]
    fn every_empty_turn_wakes_the_node_once_and_then_asks_the_user(started in any::<bool>(), actions in prop::collection::vec(prop_oneof![
        3 => prop::sample::select(vec![2u8, 3, 4, 6, 7, 17, 23, 36, 37, 38, 46, 47, 48, 49, 50, 52, 53]),
        1 => 0u8..56,
    ], 1..160)) {
        let mut task = if started { running() } else { TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap() };
        let mut woke = false;
        for action in actions {
            let Some(event) = arbitrary_event(&task, action) else { continue };
            let wake = match &event {
                Event::Runtime { fact: RuntimeFact::TurnEnded { wake: Some(wake), awaits: None, .. }, .. } => Some(wake.clone()),
                _ => None,
            };
            let quiet = matches!(&event, Event::Runtime { fact: RuntimeFact::TurnStarted { trigger: Trigger::Input(by), .. }, .. } if by.as_str().starts_with("wake"))
                || matches!(&event, Event::RequestControl { .. } | Event::Runtime { fact: RuntimeFact::Notification | RuntimeFact::InterruptAccepted { .. } | RuntimeFact::ResourceAvailable, .. });
            let executing = task.phase() == TaskPhase::InProgress && task.cards().is_empty() && task.dependencies().is_empty();
            if task.apply(task.revision(), event).is_err() { continue }
            let Some(wake) = wake else {
                if !quiet { woke = false; }
                continue;
            };
            let woken = task.pending_inputs().contains(&&wake);
            prop_assert!(woken, "an accepted end that names a wake sends it: {:?}", task);
            if woken {
                prop_assert!(!woke, "a node woken after an empty turn is not woken again before anything new: {:?}", task);
            }
            if executing && task.activity() == ActivityView::Resting {
                prop_assert!(!task.cards().is_empty(), "an empty turn nobody wakes asks the user: {:?}", task);
            }
            woke = woken;
        }
    }

    /// No history leaves a task in progress with no turn, no work of its node's own, no input on
    /// its way, no delivery and no wait someone owns. A continuation waiting when a turn starts is
    /// read by that turn; it stays the node's own only when the turn ends neither waking the node
    /// nor waiting on anything, as journals recorded before this rule did.
    #[test]
    fn no_task_in_progress_is_left_without_a_turn_work_a_delivery_or_an_owned_wait(started in any::<bool>(), actions in prop::collection::vec(prop_oneof![
        3 => prop::sample::select(vec![2u8, 3, 4, 5, 6, 7, 17, 36, 37, 38, 46, 47, 49, 56]),
        1 => 0u8..57,
    ], 1..160)) {
        let mut task = if started { running() } else { TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap() };
        let (mut fresh, mut callback_waits) = (false, false);
        for action in actions {
            let Some(event) = arbitrary_event(&task, action) else { continue };
            let observed = event.clone();
            let before = task.activity();
            if task.apply(task.revision(), event).is_err() { continue }
            match observed {
                Event::Runtime { fact: RuntimeFact::TurnStarted { .. }, .. } => fresh = callback_waits || matches!(before, ActivityView::Continuation | ActivityView::OwnWork) && fresh,
                Event::Runtime { fact: RuntimeFact::WorkEnded { .. }, .. } => fresh = true,
                Event::Decide { .. } | Event::Runtime { fact: RuntimeFact::CallbackSettled { .. } | RuntimeFact::AnsweredByPolicy { .. }, .. } => (fresh, callback_waits) = (true, true),
                _ => {}
            }
            if task.phase() != TaskPhase::InProgress { continue }
            let activity = task.activity();
            let owned = task.turn().is_some()
                || matches!(activity, ActivityView::OwnWork | ActivityView::Starting)
                || (activity == ActivityView::Continuation && fresh)
                || !task.pending_inputs().is_empty()
                || !task.cards().is_empty()
                || !task.dependencies().is_empty()
                || task.awaits().is_some();
            prop_assert!(owned, "nothing works for the task and nobody owns its wait: {:?}", task);
        }
    }

    #[test]
    fn arbitrary_histories_preserve_terminal_exclusion_and_atomic_rejections(started in any::<bool>(), actions in prop::collection::vec(0u8..56, 1..160)) {
        let mut task = if started { running() } else { TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap() };
        for action in actions {
            let event = arbitrary_event(&task, action);
            let Some(event) = event else { continue };
            let host_ended = matches!(event, Event::Runtime { fact: RuntimeFact::HostEnded { .. }, .. });
            let open = matches!(task.activity(), ActivityView::Working | ActivityView::OwnWork | ActivityView::Continuation);
            let users = task.phase() == TaskPhase::InProgress
                || matches!(event, Event::Runtime { fact: RuntimeFact::HostEnded { resume: Some(_), .. }, .. });
            let revision = task.revision();
            let control = matches!(event, Event::RequestControl { .. });
            let before = format!("{task:?}");
            let observed = (task.phase(), task.activity(), task.cursor(), task.turn().cloned(), task.cards().len(), task.pending_inputs().len(), task.holds_slot());
            let moved = matches!(event, Event::MoveCard { .. });
            if task.apply(task.revision(), event).is_err() { prop_assert_eq!(format!("{task:?}"), before); }
            else if moved { prop_assert_eq!((task.phase(), task.activity(), task.cursor(), task.turn().cloned(), task.cards().len(), task.pending_inputs().len(), task.holds_slot()), observed); }
            else if control { prop_assert_eq!((task.phase(), task.activity(), task.cursor(), task.turn().cloned(), task.cards().len(), task.pending_inputs().len(), task.holds_slot()), observed); }
            if matches!(task.phase(), TaskPhase::Completed | TaskPhase::Cancelled) {
                prop_assert!(task.execution().is_none());
                prop_assert!(!task.holds_slot());
                prop_assert_eq!(task.activity(), ActivityView::None);
                prop_assert_eq!(task.drain().count(), 0);
            }
            if task.phase() == TaskPhase::Finishing { prop_assert!(task.drain().count() > 0); }
            if task.activity() == ActivityView::OwnWork { prop_assert!(task.holds_slot()); }
            if host_ended && open && users && task.revision() != revision {
                prop_assert!(!task.pending_inputs().is_empty() || !task.cards().is_empty() || !task.dependencies().is_empty(),
                    "a session its host's end cut gets an input or a card when the user waits on it or a resumed session is told to go on: {:?}", task);
            }
            if task.phase() == TaskPhase::InProgress && task.activity() == ActivityView::Resting { prop_assert!(!task.cards().is_empty() || !task.dependencies().is_empty() || task.awaits().is_some()); }
            if let Some(stamp) = task.execution() { prop_assert_eq!(stamp.task(), task.task()); }
            prop_assert_eq!(task.turn_stop().is_some(), task.turn().is_some());
        }
    }
}

#[test]
fn abort_after_answer_replaces_the_last_callback_wake_with_an_actionable_card() {
    let mut task = running();
    let callback = CallbackId::new("permission").unwrap();
    fact(
        &mut task,
        RuntimeFact::Ask {
            card: "permission".into(),
            callback: callback.clone(),
            reason: WaitReason::Permission,
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("root"),
            unheard_card: "unused".into(),
            wake: None,
            awaits: None,
        },
    );
    apply(
        &mut task,
        Event::Decide {
            card: "permission".into(),
            decision: Decision::Allow,
        },
    );
    assert_eq!(task.activity(), ActivityView::Continuation);
    fact(
        &mut task,
        RuntimeFact::CallbackAborted {
            callback,
            unheard_card: "unheard".into(),
        },
    );
    assert_eq!(
        task.cards(),
        vec![(&InteractionId::from("unheard"), WaitReason::Unheard)]
    );
    assert_eq!(task.activity(), ActivityView::Resting);
    assert!(!task.holds_slot());
}

#[test]
fn callbacks_can_settle_in_reverse_answer_order_without_losing_either_wake() {
    let mut task = running();
    for id in ["first", "second"] {
        fact(
            &mut task,
            RuntimeFact::Ask {
                card: id.into(),
                callback: CallbackId::new(id).unwrap(),
                reason: WaitReason::Permission,
            },
        );
    }
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("root"),
            unheard_card: "unused".into(),
            wake: None,
            awaits: None,
        },
    );
    for id in ["first", "second"] {
        apply(
            &mut task,
            Event::Decide {
                card: id.into(),
                decision: Decision::Allow,
            },
        );
    }
    fact(
        &mut task,
        RuntimeFact::CallbackSettled {
            callback: CallbackId::new("second").unwrap(),
            continuation: continuation("second-wake"),
        },
    );
    fact(
        &mut task,
        RuntimeFact::CallbackAborted {
            callback: CallbackId::new("first").unwrap(),
            unheard_card: "unused".into(),
        },
    );
    assert!(task.cards().is_empty());
    assert_eq!(task.activity(), ActivityView::Continuation);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("resumed"),
            trigger: Trigger::OwnedContinuation(continuation("second-wake")),
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("resumed"),
            unheard_card: "unheard".into(),
            wake: None,
            awaits: None,
        },
    );
    assert_eq!(
        task.cards(),
        vec![(&InteractionId::from("unheard"), WaitReason::Unheard)]
    );
}

#[test]
fn a_permission_from_an_old_execution_cannot_attach_to_its_replacement() {
    let mut task = running();
    let old = task.execution().unwrap().clone();
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    apply(
        &mut task,
        Event::RetireReleasedExecution {
            problem_card: "recovery".into(),
            input: input("retry"),
        },
    );
    admit(&mut task, "replacement");
    fact(&mut task, RuntimeFact::ResourceAvailable);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("replacement"),
            trigger: Trigger::Input(input("retry")),
        },
    );
    fact(&mut task, RuntimeFact::Notification);
    let before = format!("{task:?}");
    let result = task.apply(
        task.revision(),
        Event::Runtime {
            stamp: old,
            cursor: 4,
            fact: RuntimeFact::Ask {
                card: "obsolete-permission".into(),
                callback: CallbackId::new("obsolete").unwrap(),
                reason: WaitReason::Permission,
            },
        },
    );
    assert_eq!(result, Err(Refusal::StaleExecution));
    assert_eq!(format!("{task:?}"), before);
    assert!(task.cards().is_empty());
}

#[test]
fn dependency_wait_survives_permission_exit_and_replacement_retirement() {
    let mut task = running();
    apply(
        &mut task,
        Event::WaitForDependencies {
            dependencies: vec!["upstream".into()],
        },
    );
    let callback = CallbackId::new("permission").unwrap();
    fact(
        &mut task,
        RuntimeFact::Ask {
            card: "permission".into(),
            callback: callback.clone(),
            reason: WaitReason::Permission,
        },
    );
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    assert_eq!(task.dependencies(), vec![&TaskId::from("upstream")]);
    fact(
        &mut task,
        RuntimeFact::CallbackAborted {
            callback,
            unheard_card: "unused".into(),
        },
    );
    apply(
        &mut task,
        Event::RetireReleasedExecution {
            problem_card: "recovery".into(),
            input: input("retry"),
        },
    );
    assert_eq!(task.dependencies(), vec![&TaskId::from("upstream")]);
    assert!(task.execution().is_none());
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Admit {
                node: "node".into(),
                conversation: ConversationId::new("conversation").unwrap(),
                instance: InstanceId::new("replacement").unwrap(),
                host: HostId::new("host").unwrap()
            }
        ),
        Err(Refusal::InvalidState)
    );
    apply(
        &mut task,
        Event::DependenciesResolved {
            input: input("resolved"),
        },
    );
    admit(&mut task, "replacement");
}

#[test]
fn a_blocked_delivery_keeps_its_work_and_cannot_be_accepted_before_unblocking() {
    let mut task = running();
    apply(
        &mut task,
        Event::WaitForDependencies {
            dependencies: vec!["upstream".into()],
        },
    );
    apply(
        &mut task,
        Event::Present {
            delivery: "result".into(),
        },
    );
    assert_eq!(task.dependencies(), vec![&TaskId::from("upstream")]);
    assert!(task.holds_slot());
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Accept {
                delivery: "result".into(),
                merge: None
            }
        ),
        Err(Refusal::InvalidState)
    );
    apply(
        &mut task,
        Event::DeliveryUnblocked {
            delivery: "result".into(),
        },
    );
    assert!(task.dependencies().is_empty());
    apply(
        &mut task,
        Event::Accept {
            delivery: "result".into(),
            merge: None,
        },
    );
}

#[test]
fn exit_does_not_resolve_an_accepted_external_effect_or_allow_replay() {
    let mut task = running();
    fact(
        &mut task,
        RuntimeFact::EffectAccepted {
            operation: operation("external"),
        },
    );
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    assert_eq!(
        task.apply(
            task.revision(),
            Event::RetireReleasedExecution {
                problem_card: "recovery".into(),
                input: input("retry")
            }
        ),
        Err(Refusal::InvalidState)
    );
    apply(
        &mut task,
        Event::Finish {
            intent: FinishIntent::Cancel,
            cleanup: operation("cleanup"),
            post_merge: None,
        },
    );
    apply(
        &mut task,
        Event::DrainObserved {
            obligation: DrainObligation::Cleanup(operation("cleanup")),
        },
    );
    assert_eq!(task.phase(), TaskPhase::Finishing);
    assert!(
        task.drain()
            .any(|item| *item == DrainObligation::ExternalEffect(operation("external")))
    );
    apply(
        &mut task,
        Event::DrainObserved {
            obligation: DrainObligation::ExternalEffect(operation("external")),
        },
    );
    assert_eq!(task.phase(), TaskPhase::Cancelled);
}

#[test]
fn distinct_connection_and_exit_cards_are_retired_together_without_stranding_recovery() {
    let mut task = running();
    fact(
        &mut task,
        RuntimeFact::ConnectionLost {
            problem_card: "connection".into(),
        },
    );
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "exit".into(),
        },
    );
    apply(
        &mut task,
        Event::RetireReleasedExecution {
            problem_card: "exit".into(),
            input: input("retry"),
        },
    );
    assert!(task.cards().is_empty());
    admit(&mut task, "replacement");
}

#[test]
fn repeated_pause_cannot_create_two_mutually_blocking_resume_cards() {
    let mut task = running();
    apply(
        &mut task,
        Event::Pause {
            card: "pause-a".into(),
            owner: Owner::User,
        },
    );
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Pause {
                card: "pause-b".into(),
                owner: Owner::Capacity(Capacity::Usage),
            }
        ),
        Err(Refusal::DuplicateObligation)
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("root"),
            unheard_card: "unused".into(),
            wake: None,
            awaits: None,
        },
    );
    apply(
        &mut task,
        Event::Resume {
            card: "pause-a".into(),
            input: input("resume"),
        },
    );
    assert!(task.cards().is_empty());
    assert_eq!(task.activity(), ActivityView::Starting);
}

#[test]
fn a_hand_back_while_work_runs_keeps_the_reply_until_its_consumption() {
    let mut task = running();
    fact(
        &mut task,
        RuntimeFact::WorkStarted {
            turn: turn("root"),
            work: work("child"),
        },
    );
    apply(
        &mut task,
        Event::Present {
            delivery: "result".into(),
        },
    );
    apply(
        &mut task,
        Event::HandBack {
            delivery: "result".into(),
            input: input("changes"),
        },
    );
    assert_eq!(task.stage(), Some(WorkStage::ChangesRequested));
    assert_eq!(task.pending_inputs(), vec![&input("changes")]);
    assert!(task.holds_slot());
    fact(
        &mut task,
        RuntimeFact::InputConsumed {
            input: input("changes"),
        },
    );
    assert!(task.pending_inputs().is_empty());
    assert!(task.holds_slot());
}

#[test]
fn answering_two_prelaunch_questions_preserves_initial_input_and_both_answers() {
    let mut task = TaskLifecycle::planned("task".into(), input("initial")).unwrap();
    for card in ["a", "b"] {
        apply(&mut task, Event::Ask { card: card.into() });
    }
    apply(
        &mut task,
        Event::Answer {
            card: "a".into(),
            input: input("answer-a"),
        },
    );
    apply(
        &mut task,
        Event::Answer {
            card: "b".into(),
            input: input("answer-b"),
        },
    );
    assert_eq!(
        task.pending_inputs(),
        vec![&input("initial"), &input("answer-a"), &input("answer-b")]
    );
    admit(&mut task, "instance");
    fact(&mut task, RuntimeFact::ResourceAvailable);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("root"),
            trigger: Trigger::Input(input("initial")),
        },
    );
    assert_eq!(
        task.pending_inputs(),
        vec![&input("answer-a"), &input("answer-b")]
    );
}

#[test]
fn dependency_resolution_and_multiple_answers_keep_their_input_order() {
    let mut task = TaskLifecycle::planned("task".into(), input("initial")).unwrap();
    apply(
        &mut task,
        Event::WaitForDependencies {
            dependencies: vec!["upstream".into()],
        },
    );
    apply(
        &mut task,
        Event::Ask {
            card: "question".into(),
        },
    );
    apply(
        &mut task,
        Event::DependenciesResolved {
            input: input("dependency-resolved"),
        },
    );
    assert!(task.dependencies().is_empty());
    assert_eq!(
        task.pending_inputs(),
        vec![&input("initial"), &input("dependency-resolved")]
    );
    apply(
        &mut task,
        Event::Answer {
            card: "question".into(),
            input: input("answer"),
        },
    );
    assert_eq!(
        task.pending_inputs(),
        vec![
            &input("initial"),
            &input("dependency-resolved"),
            &input("answer")
        ]
    );
}

#[test]
fn drain_evidence_updates_process_activity_and_slot_projections_together() {
    let mut task = running();
    apply(
        &mut task,
        Event::Finish {
            intent: FinishIntent::Cancel,
            cleanup: operation("cleanup"),
            post_merge: None,
        },
    );
    apply(
        &mut task,
        Event::DrainObserved {
            obligation: DrainObligation::Execution(InstanceId::new("instance").unwrap()),
        },
    );
    assert_eq!(task.phase(), TaskPhase::Finishing);
    assert_eq!(task.resource(), Some(ResourceEvidence::Released));
    assert_eq!(task.activity(), ActivityView::Resting);
    assert!(!task.holds_slot());
}

#[test]
fn retirement_preserves_its_input_behind_a_dependency_until_later_consumption() {
    let mut task = running();
    apply(
        &mut task,
        Event::WaitForDependencies {
            dependencies: vec!["upstream".into()],
        },
    );
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    apply(
        &mut task,
        Event::RetireReleasedExecution {
            problem_card: "recovery".into(),
            input: input("retry"),
        },
    );
    assert_eq!(task.pending_inputs(), vec![&input("retry")]);
    apply(
        &mut task,
        Event::DependenciesResolved {
            input: input("resolved"),
        },
    );
    assert_eq!(
        task.pending_inputs(),
        vec![&input("retry"), &input("resolved")]
    );
    admit(&mut task, "replacement");
    fact(&mut task, RuntimeFact::ResourceAvailable);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("replacement"),
            trigger: Trigger::Input(input("retry")),
        },
    );
    fact(
        &mut task,
        RuntimeFact::InputConsumed {
            input: input("resolved"),
        },
    );
    assert!(task.pending_inputs().is_empty());
}

#[test]
fn a_delivery_survives_exit_retirement_and_hand_back_without_losing_input() {
    let mut task = running();
    apply(
        &mut task,
        Event::Present {
            delivery: "result".into(),
        },
    );
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    apply(
        &mut task,
        Event::RetireReleasedExecution {
            problem_card: "recovery".into(),
            input: input("retry"),
        },
    );
    assert_eq!(task.delivery(), Some(&InteractionId::from("result")));
    assert_eq!(task.pending_inputs(), vec![&input("retry")]);
    apply(
        &mut task,
        Event::HandBack {
            delivery: "result".into(),
            input: input("changes"),
        },
    );
    assert_eq!(
        task.pending_inputs(),
        vec![&input("retry"), &input("changes")]
    );
    admit(&mut task, "replacement");
}

#[test]
fn callback_abort_promotes_a_waiting_business_input_instead_of_inventing_unheard() {
    let mut task = running();
    let callback = CallbackId::new("permission").unwrap();
    fact(
        &mut task,
        RuntimeFact::Ask {
            card: "permission".into(),
            callback: callback.clone(),
            reason: WaitReason::Permission,
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("root"),
            unheard_card: "unused".into(),
            wake: None,
            awaits: None,
        },
    );
    apply(
        &mut task,
        Event::Decide {
            card: "permission".into(),
            decision: Decision::Allow,
        },
    );
    apply(
        &mut task,
        Event::Ask {
            card: "question".into(),
        },
    );
    apply(
        &mut task,
        Event::Answer {
            card: "question".into(),
            input: input("business"),
        },
    );
    fact(
        &mut task,
        RuntimeFact::CallbackAborted {
            callback,
            unheard_card: "unused".into(),
        },
    );
    assert!(task.cards().is_empty());
    assert_eq!(task.activity(), ActivityView::Starting);
    assert_eq!(task.pending_inputs(), vec![&input("business")]);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("business"),
            trigger: Trigger::Input(input("business")),
        },
    );
    assert!(task.pending_inputs().is_empty());
}

#[test]
fn admission_reserves_a_slot_without_claiming_a_running_process() {
    let mut task = TaskLifecycle::planned("task".into(), input("initial")).unwrap();
    admit(&mut task, "instance");
    assert_eq!(task.resource(), Some(ResourceEvidence::Reserved));
    assert!(task.holds_slot());
    fact(&mut task, RuntimeFact::ResourceAvailable);
    assert_eq!(task.resource(), Some(ResourceEvidence::Available));
}

#[test]
fn a_delivered_question_can_be_answered_and_consumed_after_exit_recovery() {
    let mut task = running();
    apply(
        &mut task,
        Event::Ask {
            card: "question".into(),
        },
    );
    apply(
        &mut task,
        Event::Present {
            delivery: "result".into(),
        },
    );
    fact(
        &mut task,
        RuntimeFact::ResourceReleased {
            problem_card: "recovery".into(),
        },
    );
    apply(
        &mut task,
        Event::RetireReleasedExecution {
            problem_card: "recovery".into(),
            input: input("retry"),
        },
    );
    apply(
        &mut task,
        Event::Answer {
            card: "question".into(),
            input: input("answer"),
        },
    );
    assert_eq!(task.delivery(), Some(&InteractionId::from("result")));
    assert_eq!(
        task.pending_inputs(),
        vec![&input("retry"), &input("answer")]
    );
    admit(&mut task, "replacement");
    fact(&mut task, RuntimeFact::ResourceAvailable);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("replacement"),
            trigger: Trigger::Input(input("retry")),
        },
    );
    fact(
        &mut task,
        RuntimeFact::InputConsumed {
            input: input("answer"),
        },
    );
    assert_eq!(task.delivery(), Some(&InteractionId::from("result")));
    assert!(task.pending_inputs().is_empty());
}

#[test]
fn an_actual_dependency_is_a_continuation_after_an_empty_turn() {
    let mut task = TaskLifecycle::planned("task".into(), input("initial")).unwrap();
    admit(&mut task, "instance");
    apply(
        &mut task,
        Event::WaitForDependencies {
            dependencies: vec!["upstream".into()],
        },
    );
    fact(&mut task, RuntimeFact::ResourceAvailable);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("root"),
            trigger: Trigger::Input(input("initial")),
        },
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("root"),
            unheard_card: "unused".into(),
            wake: None,
            awaits: None,
        },
    );
    assert_eq!(task.activity(), ActivityView::Resting);
    assert_eq!(task.dependencies(), vec![&TaskId::from("upstream")]);
    assert!(!task.holds_slot());
    assert!(task.cards().is_empty());
    assert_eq!(task.resource(), Some(ResourceEvidence::Available));
    apply(
        &mut task,
        Event::DependenciesResolved {
            input: input("resolved"),
        },
    );
    assert_eq!(task.pending_inputs(), vec![&input("resolved")]);
    assert_eq!(task.activity(), ActivityView::Starting);
}

#[test]
fn exit_before_input_consumption_cannot_discard_the_initial_input_on_retirement() {
    for initialized in [false, true] {
        let mut task = TaskLifecycle::planned("task".into(), input("initial")).unwrap();
        admit(&mut task, "instance");
        if initialized {
            fact(&mut task, RuntimeFact::ResourceAvailable);
        }
        fact(
            &mut task,
            RuntimeFact::ResourceReleased {
                problem_card: "recovery".into(),
            },
        );
        let before = format!("{task:?}");
        assert_eq!(
            task.apply(
                task.revision(),
                Event::RetireReleasedExecution {
                    problem_card: "recovery".into(),
                    input: input("retry")
                }
            ),
            Err(Refusal::InvalidState)
        );
        assert_eq!(format!("{task:?}"), before);
        assert_eq!(task.pending_inputs(), vec![&input("initial")]);
        assert_eq!(
            task.cards(),
            vec![(&InteractionId::from("recovery"), WaitReason::Recovery)]
        );
    }
}

fn hold(task: &mut TaskLifecycle, review: &str) {
    apply(task, Event::HoldForPlanReview { review: review.into() });
}
fn settle(task: &mut TaskLifecycle, review: &str, outcome: PlanReviewOutcome) -> Result<(), Refusal> {
    let plan_revision = task.plan_revision();
    task.apply(task.revision(), Event::PlanReviewSettled { review: review.into(), plan_revision, outcome })
}
fn try_admit(task: &mut TaskLifecycle) -> Result<(), Refusal> {
    task.apply(task.revision(), Event::Admit {
        node: AgentId::from("node"), conversation: ConversationId::new("conversation").unwrap(),
        instance: InstanceId::new("instance").unwrap(), host: HostId::new("host").unwrap(),
    })
}

#[test]
fn a_held_task_is_not_admitted_and_the_end_of_its_wait_does_not_release_it() {
    let mut task = TaskLifecycle::planned(TaskId::from("phase-c"), input("initial")).unwrap();
    apply(&mut task, Event::WaitForDependencies { dependencies: vec![TaskId::from("phase-b")] });
    hold(&mut task, "decision-512");
    assert_eq!(task.plan_reviews(), [(&InteractionId::from("decision-512"), 0)]);
    apply(&mut task, Event::DependenciesResolved { input: input("dependencies") });
    assert_eq!(try_admit(&mut task), Err(Refusal::PlanReviewPending));
    settle(&mut task, "decision-512", PlanReviewOutcome::PlanChanged).unwrap();
    assert_eq!(task.plan_revision(), 1, "the new plan is the one it starts with");
    try_admit(&mut task).unwrap();
}

#[test]
fn only_a_settlement_of_that_review_at_the_current_plan_releases_it() {
    let mut task = TaskLifecycle::planned(TaskId::from("phase-d"), input("initial")).unwrap();
    hold(&mut task, "first");
    hold(&mut task, "second");
    assert_eq!(task.apply(task.revision(), Event::HoldForPlanReview { review: "first".into() }), Err(Refusal::DuplicateObligation));
    assert_eq!(settle(&mut task, "unknown", PlanReviewOutcome::Unchanged), Err(Refusal::UnknownObligation));
    settle(&mut task, "first", PlanReviewOutcome::PlanChanged).unwrap();
    let stale = task.apply(task.revision(), Event::PlanReviewSettled { review: "second".into(), plan_revision: 0, outcome: PlanReviewOutcome::Unchanged });
    assert_eq!(stale, Err(Refusal::StaleRevision), "a review of the old plan does not release a hold on the new one");
    assert_eq!(try_admit(&mut task), Err(Refusal::PlanReviewPending));
    settle(&mut task, "second", PlanReviewOutcome::Unchanged).unwrap();
    try_admit(&mut task).unwrap();
    assert_eq!(task.apply(task.revision(), Event::HoldForPlanReview { review: "late".into() }), Err(Refusal::InvalidState),
        "a task that started is not held: its node already works");
}

#[test]
fn a_hold_survives_cancelling_and_reopening_the_task() {
    let mut task = TaskLifecycle::planned(TaskId::from("phase-e"), input("initial")).unwrap();
    hold(&mut task, "decision-512");
    apply(&mut task, Event::Finish { intent: FinishIntent::Cancel, cleanup: operation("cleanup"), post_merge: None });
    let remaining: Vec<DrainObligation> = task.drain().cloned().collect();
    for obligation in remaining {
        apply(&mut task, Event::DrainObserved { obligation });
    }
    assert_eq!(task.phase(), TaskPhase::Cancelled);
    apply(&mut task, Event::Reopen { input: input("again") });
    assert_eq!(try_admit(&mut task), Err(Refusal::PlanReviewPending), "its plan still predates the decision");
    settle(&mut task, "decision-512", PlanReviewOutcome::KeptByUser).unwrap();
    try_admit(&mut task).unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x91a_4),
        ..ProptestConfig::default()
    })]
    #[test]
    fn no_history_admits_a_held_task_and_only_its_own_settlement_releases_it(actions in prop::collection::vec(0u8..12, 1..80)) {
        let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
        let reviews = ["first", "second"];
        for action in actions {
            let plan_revision = task.plan_revision();
            let event = match action {
                0 | 1 => Event::HoldForPlanReview { review: reviews[action as usize].into() },
                2 => Event::PlanReviewSettled { review: "first".into(), plan_revision, outcome: PlanReviewOutcome::Unchanged },
                3 => Event::PlanReviewSettled { review: "second".into(), plan_revision, outcome: PlanReviewOutcome::PlanChanged },
                4 => Event::PlanReviewSettled { review: "first".into(), plan_revision: plan_revision.wrapping_sub(1), outcome: PlanReviewOutcome::KeptByUser },
                5 => Event::Admit { node: "node".into(), conversation: ConversationId::new("conversation").unwrap(), instance: InstanceId::new("instance").unwrap(), host: HostId::new("host").unwrap() },
                6 => Event::WaitForDependencies { dependencies: vec![TaskId::from("dependency")] },
                7 => Event::DependenciesResolved { input: input("dependencies") },
                8 => Event::Finish { intent: FinishIntent::Cancel, cleanup: operation("cleanup"), post_merge: None },
                9 => match task.drain().next().cloned() { Some(obligation) => Event::DrainObserved { obligation }, None => continue },
                10 => Event::Reopen { input: input("again") },
                _ => Event::Pause { card: "pause".into(), owner: Owner::User },
            };
            let before = format!("{task:?}");
            let held: Vec<InteractionId> = task.plan_reviews().into_iter().map(|(review, _)| review.clone()).collect();
            let admitting = matches!(event, Event::Admit { .. });
            let settling = match &event { Event::PlanReviewSettled { review, .. } => Some(review.clone()), _ => None };
            match task.apply(task.revision(), event) {
                Err(_) => prop_assert_eq!(format!("{task:?}"), before),
                Ok(()) => {
                    prop_assert!(!(admitting && !held.is_empty()), "a held task was admitted");
                    let now: Vec<InteractionId> = task.plan_reviews().into_iter().map(|(review, _)| review.clone()).collect();
                    for released in held.iter().filter(|review| !now.contains(review)) {
                        prop_assert_eq!(Some(released), settling.as_ref(), "a hold went without its own settlement");
                    }
                }
            }
        }
    }
}

#[test]
fn work_after_the_merge_is_owed_from_its_declaration_and_only_a_fact_ends_it() {
    let mut task = running();
    apply(&mut task, Event::PostMergeDeclared { operation: operation("release") });
    assert_eq!(task.post_merge(), Some(&operation("release")));
    assert_eq!(
        task.apply(task.revision(), Event::PostMergeDeclared { operation: operation("again") }),
        Err(Refusal::DuplicateObligation)
    );
    apply(
        &mut task,
        Event::Finish {
            intent: FinishIntent::Cancel,
            cleanup: operation("cleanup"),
            post_merge: None,
        },
    );
    assert!(task.drain().any(|obligation| *obligation == DrainObligation::PostMerge(operation("release"))));
    assert_eq!(
        task.apply(
            task.revision(),
            Event::DrainObserved { obligation: DrainObligation::PostMerge(operation("release")) }
        ),
        Err(Refusal::DrainWithoutFact),
        "no drain ends work after the merge without the fact that ended it"
    );
    for obligation in [
        DrainObligation::Execution(InstanceId::new("instance").unwrap()),
        DrainObligation::Cleanup(operation("cleanup")),
    ] {
        apply(&mut task, Event::DrainObserved { obligation });
    }
    assert_eq!(task.phase(), TaskPhase::Finishing, "the ending still owes the work after the merge");
    apply(
        &mut task,
        Event::PostMergeEnded { operation: operation("release"), fact: PostMergeFact::UserDecision },
    );
    assert_eq!(task.phase(), TaskPhase::Cancelled);
    assert_eq!(task.post_merge(), None);
}

fn card(value: &str) -> InteractionId {
    InteractionId::from(value)
}
fn waiting_for_the_next_input() -> TaskLifecycle {
    let mut task = running();
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    apply(&mut task, Event::Answer { card: card("turn-card"), input: input("next") });
    assert_eq!(task.pending_inputs(), [&input("next")]);
    task
}

#[test]
fn a_host_that_ends_mid_turn_ends_its_turn_and_work_and_the_task_asks_how_to_go_on() {
    let mut task = running();
    fact(&mut task, RuntimeFact::WorkStarted { turn: turn("root"), work: work("background") });
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    assert_eq!(task.activity(), ActivityView::Resting);
    assert!(!task.holds_slot(), "nothing of the ended host runs any more");
    assert_eq!(task.cards(), [(&card("session-ended"), WaitReason::Unheard)]);
}

#[test]
fn a_host_that_ends_mid_turn_and_is_replaced_goes_on_with_its_resume_input_instead_of_a_card() {
    let mut task = running();
    fact(&mut task, RuntimeFact::WorkStarted { turn: turn("root"), work: work("background") });
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: Some(input("go-on")) });
    assert!(task.cards().is_empty(), "no card asks the user");
    assert_eq!(task.pending_inputs(), [&input("go-on")]);
    assert_eq!(task.activity(), ActivityView::Starting, "the resumed session is told to go on");
}

#[test]
fn a_resume_input_with_nothing_to_explain_is_refused() {
    let mut task = running();
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    let cursor = task.cursor().unwrap() + 1;
    let stamp = task.execution().unwrap().clone();
    assert_eq!(
        task.apply(task.revision(), Event::Runtime { stamp, cursor, fact: RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: Some(input("go-on")) } }),
        Err(Refusal::InvalidState),
        "the task already waits on its card: nothing to go on with"
    );
}

#[test]
fn a_doubtful_input_the_resumed_session_gets_again_is_settled_without_a_card() {
    let mut task = waiting_for_the_next_input();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut task, Event::ResendDoubtful { input: input("next"), resend: input("again") });
    assert_eq!(task.pending_inputs(), [&input("again")]);
    assert!(task.settled(&input("next")) && task.doubts().is_empty() && task.cards().is_empty());
}

#[test]
fn a_host_that_ends_while_its_task_waits_on_a_card_adds_no_second_card() {
    let mut task = running();
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    assert_eq!(task.cards(), [(&card("turn-card"), WaitReason::Unheard)]);
}

#[test]
fn a_doubtful_input_holds_the_task_until_the_user_sends_it_again_in_its_place() {
    let mut task = waiting_for_the_next_input();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    assert!(task.cards().is_empty(), "the input still explains the next step");
    apply(&mut task, Event::DoubtHandoff { card: card("doubt"), input: input("next") });
    assert_eq!(task.cards(), [(&card("doubt"), WaitReason::Doubt)]);
    assert_eq!(
        task.apply(task.revision(), Event::Answer { card: card("doubt"), input: input("other") }),
        Err(Refusal::InvalidState),
        "only settling answers a doubt"
    );
    apply(&mut task, Event::SettleHandoff { card: card("doubt"), resend: Some(input("again")), unheard_card: card("unheard") });
    assert_eq!(task.pending_inputs(), [&input("again")]);
    assert!(task.settled(&input("next")) && task.doubts().is_empty() && task.cards().is_empty());
    assert_eq!(task.activity(), ActivityView::Starting);
}

#[test]
fn a_doubtful_input_taken_as_received_leaves_the_queue_unconsumed_and_the_task_asks_how_to_go_on() {
    let mut task = waiting_for_the_next_input();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut task, Event::DoubtHandoff { card: card("doubt"), input: input("next") });
    apply(&mut task, Event::SettleHandoff { card: card("doubt"), resend: None, unheard_card: card("unheard") });
    assert!(task.pending_inputs().is_empty());
    assert!(task.settled(&input("next")));
    assert_eq!(task.cards(), [(&card("unheard"), WaitReason::Unheard)]);
}

#[test]
fn cancelling_settles_every_open_doubt() {
    let mut task = waiting_for_the_next_input();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut task, Event::DoubtHandoff { card: card("doubt"), input: input("next") });
    apply(&mut task, Event::Finish { intent: FinishIntent::Cancel, cleanup: operation("cleanup"), post_merge: None });
    assert!(task.settled(&input("next")) && task.doubts().is_empty());
}

#[test]
fn a_launch_input_can_be_sent_again_but_never_taken_as_received() {
    let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    admit(&mut task, "instance");
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut task, Event::DoubtHandoff { card: card("doubt"), input: input("initial") });
    assert_eq!(
        task.apply(task.revision(), Event::SettleHandoff { card: card("doubt"), resend: None, unheard_card: card("unheard") }),
        Err(Refusal::InvalidState)
    );
    apply(&mut task, Event::SettleHandoff { card: card("doubt"), resend: Some(input("again")), unheard_card: card("unheard") });
    assert_eq!(task.pending_inputs(), [&input("again")]);
}

#[test]
fn an_input_the_ended_host_never_took_is_sent_again_in_its_place_without_asking() {
    let mut task = waiting_for_the_next_input();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut task, Event::ResendUntaken { input: input("next"), resend: input("again") });
    assert_eq!(task.pending_inputs(), [&input("again")]);
    assert!(task.cards().is_empty());
    assert_eq!(
        task.apply(task.revision(), Event::ResendUntaken { input: input("missing"), resend: input("other") }),
        Err(Refusal::UnknownObligation)
    );
}

fn control(task: &mut TaskLifecycle, id: &str, kind: Control) -> Result<(), Refusal> {
    task.apply(task.revision(), Event::RequestControl { control: operation(id), kind })
}

#[test]
fn a_control_is_recorded_on_the_running_execution_and_changes_nothing_else() {
    let mut task = running();
    assert_eq!(task.turn(), Some(&turn("root")));
    let before = (task.phase(), task.activity(), task.cursor(), task.pending_inputs().len(), task.holds_slot());
    for (id, kind) in [
        ("interrupt", Control::Interrupt { turn: Some(turn("root")) }),
        ("model", Control::SetModel { model: "claude-opus-4-1".into() }),
        ("mode", Control::SetPermissionMode { mode: PermissionMode::Plan }),
        ("mcp", Control::ReadMcpStatus),
        ("reconnect", Control::ReconnectMcp { server: "linear".into() }),
        ("sign-in", Control::StartMcpSignIn { server: "claude.ai Gmail".into() }),
    ] {
        let revision = task.revision();
        control(&mut task, id, kind).unwrap();
        assert_eq!(task.revision(), revision + 1);
        assert_eq!((task.phase(), task.activity(), task.cursor(), task.pending_inputs().len(), task.holds_slot()), before);
        assert_eq!(task.turn(), Some(&turn("root")));
    }
}

#[test]
fn an_interrupt_names_exactly_what_runs_and_a_model_or_mcp_server_must_be_a_name() {
    let mut task = running();
    assert_eq!(control(&mut task, "other", Control::Interrupt { turn: Some(turn("other")) }), Err(Refusal::CorrelationMismatch));
    assert_eq!(control(&mut task, "none", Control::Interrupt { turn: None }), Err(Refusal::CorrelationMismatch));
    for model in ["", "   ", "line\nbreak", "tab\tbed", &"m".repeat(129)] {
        assert_eq!(control(&mut task, "model", Control::SetModel { model: model.into() }), Err(Refusal::InvalidModel), "{model:?}");
    }
    control(&mut task, "longest", Control::SetModel { model: "m".repeat(128) }).unwrap();
    for server in ["", "   ", "line\nbreak", &"s".repeat(257)] {
        assert_eq!(control(&mut task, "reconnect", Control::ReconnectMcp { server: server.into() }), Err(Refusal::InvalidMcpServer), "{server:?}");
        assert_eq!(control(&mut task, "sign-in", Control::StartMcpSignIn { server: server.into() }), Err(Refusal::InvalidMcpServer), "{server:?}");
    }
    fact(&mut task, RuntimeFact::WorkStarted { turn: turn("root"), work: work("child") });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unused".into(), wake: None, awaits: None });
    assert_eq!(task.activity(), ActivityView::OwnWork);
    assert_eq!(task.turn(), None);
    assert_eq!(control(&mut task, "ended", Control::Interrupt { turn: Some(turn("root")) }), Err(Refusal::CorrelationMismatch));
    control(&mut task, "own-work", Control::Interrupt { turn: None }).unwrap();
}

// What holds Claude while no root turn is open is interrupted without naming a turn: the wait for
// background work to wake it, or input it was handed and has not taken. Nothing else changes.
#[test]
fn an_execution_waiting_for_its_background_work_or_its_input_is_interrupted_without_a_turn() {
    let mut handed = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    admit(&mut handed, "instance");
    fact(&mut handed, RuntimeFact::ResourceAvailable);
    let mut waking = running();
    fact(&mut waking, RuntimeFact::WorkStarted { turn: turn("root"), work: work("child") });
    fact(&mut waking, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unused".into(), wake: None, awaits: None });
    fact(&mut waking, RuntimeFact::WorkEnded { work: work("child"), continuation: continuation("wake") });
    for (name, mut task, activity) in [("handed", handed, ActivityView::Starting), ("waking", waking, ActivityView::Continuation)] {
        assert_eq!(task.activity(), activity, "{name}");
        assert_eq!(control(&mut task, "named", Control::Interrupt { turn: Some(turn("root")) }), Err(Refusal::CorrelationMismatch), "{name}: no turn is open");
        let before = (task.phase(), task.activity(), task.cursor(), task.pending_inputs().len(), task.holds_slot());
        control(&mut task, "interrupt", Control::Interrupt { turn: None }).unwrap();
        assert_eq!((task.phase(), task.activity(), task.cursor(), task.pending_inputs().len(), task.holds_slot()), before, "{name}");
    }
}

#[test]
fn a_turn_is_forced_to_stop_only_after_claude_accepted_its_interrupt_and_only_once() {
    let mut task = running();
    assert_eq!(task.turn_stop(), Some(TurnStop::Running));
    assert_eq!(control(&mut task, "early", Control::ForceStop { turn: turn("root") }), Err(Refusal::InvalidState), "an interrupt comes first");
    assert_eq!(task.apply(task.revision(), Event::Runtime { stamp: task.execution().cloned().unwrap(), cursor: task.cursor().unwrap() + 1, fact: RuntimeFact::InterruptAccepted { turn: turn("other") } }),
        Err(Refusal::CorrelationMismatch));
    fact(&mut task, RuntimeFact::InterruptAccepted { turn: turn("root") });
    assert_eq!(task.turn_stop(), Some(TurnStop::InterruptAccepted));
    assert_eq!(task.activity(), ActivityView::Working, "the turn goes on");
    assert_eq!(control(&mut task, "other", Control::ForceStop { turn: turn("other") }), Err(Refusal::CorrelationMismatch));
    control(&mut task, "force", Control::ForceStop { turn: turn("root") }).unwrap();
    assert_eq!(task.turn_stop(), Some(TurnStop::ForceRequested));
    assert_eq!(control(&mut task, "again", Control::ForceStop { turn: turn("root") }), Err(Refusal::DuplicateObligation));
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: "stopped".into(), resume: None });
    assert_eq!(task.turn_stop(), None, "Claude's exit ends the turn");
    assert_eq!(task.cards().len(), 1);
}

#[test]
fn a_control_needs_an_available_execution_of_an_open_task() {
    let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    assert_eq!(control(&mut task, "unstarted", Control::ReadMcpStatus), Err(Refusal::InvalidState));
    admit(&mut task, "instance");
    assert_eq!(control(&mut task, "reserved", Control::ReadMcpStatus), Err(Refusal::InvalidState));
    let mut task = running();
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    assert_eq!(task.activity(), ActivityView::Resting);
    assert_eq!(control(&mut task, "resting", Control::Interrupt { turn: None }), Err(Refusal::InvalidState), "nothing runs to interrupt");
    control(&mut task, "resting-mode", Control::SetPermissionMode { mode: PermissionMode::DontAsk }).unwrap();
    fact(&mut task, RuntimeFact::ConnectionLost { problem_card: "recovery".into() });
    assert_eq!(control(&mut task, "lost", Control::ReadMcpStatus), Err(Refusal::InvalidState));
    let mut task = running();
    apply(&mut task, Event::Finish { intent: FinishIntent::Cancel, cleanup: operation("cleanup"), post_merge: None });
    let before = format!("{task:?}");
    assert_eq!(control(&mut task, "finishing", Control::Interrupt { turn: Some(turn("root")) }), Err(Refusal::InvalidState));
    assert_eq!(format!("{task:?}"), before);
}

#[test]
fn compacting_queues_its_input_on_a_resting_execution_and_keeps_every_card() {
    let mut task = running();
    assert_eq!(task.apply(task.revision(), Event::Compact { input: input("busy") }), Err(Refusal::InvalidState), "a turn runs");
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    assert_eq!(task.cards(), vec![(&InteractionId::from("unheard"), WaitReason::Unheard)]);
    apply(&mut task, Event::Compact { input: input("compact") });
    assert_eq!(task.pending_inputs(), vec![&input("compact")]);
    assert_eq!(task.cards(), vec![(&InteractionId::from("unheard"), WaitReason::Unheard)]);
    assert_eq!(task.apply(task.revision(), Event::Compact { input: input("again") }), Err(Refusal::InvalidState), "its input is already pending");
    fact(&mut task, RuntimeFact::TurnStarted { turn: turn("compaction"), trigger: Trigger::Input(input("compact")) });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("compaction"), unheard_card: "second".into(), wake: None, awaits: None });
    assert_eq!(task.activity(), ActivityView::Resting);
    assert_eq!(task.cards(), vec![(&InteractionId::from("unheard"), WaitReason::Unheard)], "no new card");
    assert!(task.pending_inputs().is_empty());

    let mut delivered = running();
    apply(&mut delivered, Event::Present { delivery: "delivery".into() });
    fact(&mut delivered, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unused".into(), wake: None, awaits: None });
    apply(&mut delivered, Event::Compact { input: input("compact") });
    assert_eq!(delivered.phase(), TaskPhase::Delivered);
    assert_eq!(delivered.delivery(), Some(&InteractionId::from("delivery")));

    let mut paused = running();
    fact(&mut paused, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    apply(&mut paused, Event::Pause { card: "pause".into(), owner: Owner::User });
    assert_eq!(paused.apply(paused.revision(), Event::Compact { input: input("compact") }), Err(Refusal::InvalidState));
    let mut unstarted = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    apply(&mut unstarted, Event::Ask { card: "question".into() });
    assert_eq!(unstarted.apply(unstarted.revision(), Event::Compact { input: input("compact") }), Err(Refusal::InvalidState), "no execution to compact");
}

fn asked(task: &mut TaskLifecycle, card: &str, epoch: u64, reason: WaitReason) -> CallbackId {
    let callback = decision_callback(task.generation(), epoch);
    fact(task, RuntimeFact::Ask { card: card.into(), callback: callback.clone(), reason });
    callback
}

#[test]
fn a_decision_callback_names_its_epoch_and_nothing_else() {
    assert_eq!(decision_epoch(3, &decision_callback(3, 42)), Some(42));
    assert_eq!(decision_epoch(4, &decision_callback(3, 42)), None);
    assert_eq!(decision_epoch(3, &CallbackId::new("decision-3-042").unwrap()), None);
    assert_eq!(decision_epoch(3, &CallbackId::new("decision-3-").unwrap()), None);
    assert_eq!(decision_epoch(3, &CallbackId::new("permission").unwrap()), None);
    assert_eq!(decision_continuation(3, 42).as_str(), decision_callback(3, 42).as_str());
}

/// Every reply to every kind of card: a callback is only decided, a plain card only answered,
/// and a decision answers only what its card asks.
#[test]
fn replies_to_cards_follow_one_table() {
    #[derive(Clone, Copy, Debug)]
    enum Held {
        Plain,
        Permission,
        Question,
    }
    let replies: Vec<(&str, Box<dyn Fn(&str) -> Event>)> = vec![
        ("answer", Box::new(|card: &str| Event::Answer { card: card.into(), input: input("reply") })),
        ("allow", Box::new(|card: &str| Event::Decide { card: card.into(), decision: Decision::Allow })),
        ("answers", Box::new(|card: &str| Event::Decide { card: card.into(), decision: Decision::Answers { answers: vec!["Yes".into()] } })),
        ("no answers", Box::new(|card: &str| Event::Decide { card: card.into(), decision: Decision::Answers { answers: vec![] } })),
        ("deny", Box::new(|card: &str| Event::Decide { card: card.into(), decision: Decision::Deny { message: "No".into() } })),
        ("blank deny", Box::new(|card: &str| Event::Decide { card: card.into(), decision: Decision::Deny { message: " ".into() } })),
        ("accept", Box::new(|card: &str| Event::Decide { card: card.into(), decision: Decision::Accept { text: String::new() } })),
        ("decline", Box::new(|card: &str| Event::Decide { card: card.into(), decision: Decision::Decline })),
    ];
    let expected = |held: Held, reply: &str| -> Result<(), Refusal> {
        match (held, reply) {
            (_, "no answers" | "blank deny") => Err(Refusal::InvalidDecision),
            (Held::Plain, "answer") => Ok(()),
            (Held::Plain, _) => Err(Refusal::InvalidState),
            (Held::Permission | Held::Question, "answer") => Err(Refusal::InvalidState),
            (Held::Permission, "allow" | "deny") => Ok(()),
            (Held::Permission, _) => Err(Refusal::InvalidDecision),
            (Held::Question, "answers" | "deny" | "accept" | "decline") => Ok(()),
            (Held::Question, _) => Err(Refusal::InvalidDecision),
        }
    };
    for held in [Held::Plain, Held::Permission, Held::Question] {
        for (name, reply) in &replies {
            let mut task = running();
            let callback = match held {
                Held::Plain => {
                    apply(&mut task, Event::Ask { card: "card".into() });
                    None
                }
                Held::Permission => Some(asked(&mut task, "card", 7, WaitReason::Permission)),
                Held::Question => Some(asked(&mut task, "card", 7, WaitReason::Question)),
            };
            let before = format!("{task:?}");
            let outcome = task.apply(task.revision(), reply("card"));
            assert_eq!(outcome, expected(held, name), "{name} on a {held:?} card");
            match outcome {
                Ok(()) => {
                    assert!(task.cards().is_empty(), "{name} on a {held:?} card leaves it");
                    if let Some(callback) = &callback {
                        assert_eq!(task.callback(callback), Some(CallbackView::Answered));
                        assert!(task.pending_inputs().is_empty(), "a decision queues no input");
                    }
                }
                Err(_) => assert_eq!(format!("{task:?}"), before, "{name} on a {held:?} card changed it"),
            }
        }
    }
}

#[test]
fn a_callback_awaiting_its_answer_moves_to_another_card_and_nothing_else_moves() {
    let mut task = running();
    let callback = asked(&mut task, "q1", 5, WaitReason::Question);
    apply(&mut task, Event::Ask { card: "plain".into() });
    let move_card = |from: &str, to: &str| Event::MoveCard { from: from.into(), to: to.into() };
    for (event, refusal) in [
        (move_card("absent", "q2"), Refusal::CorrelationMismatch),
        (move_card("q1", "q1"), Refusal::DuplicateObligation),
        (move_card("q1", "plain"), Refusal::DuplicateObligation),
        (move_card("q1", " "), Refusal::EmptyIdentity),
        (move_card("plain", "q2"), Refusal::InvalidState),
    ] {
        let before = format!("{task:?}");
        assert_eq!(task.apply(task.revision(), event.clone()), Err(refusal), "{event:?}");
        assert_eq!(format!("{task:?}"), before, "{event:?} changed the task");
    }
    apply(&mut task, move_card("q1", "q2"));
    assert_eq!(
        task.cards(),
        vec![(&InteractionId::from("plain"), WaitReason::Question), (&InteractionId::from("q2"), WaitReason::Question)]
    );
    assert_eq!(task.card_callback(&"q2".into()), Some(&callback));
    assert_eq!(task.card_callback(&"q1".into()), None);
    assert_eq!(task.callback(&callback), Some(CallbackView::AwaitingAnswer));
    let answers = Decision::Answers { answers: vec!["Yes".into(), "No".into()] };
    assert_eq!(
        task.apply(task.revision(), Event::Decide { card: "q1".into(), decision: answers.clone() }),
        Err(Refusal::CorrelationMismatch),
        "the card it moved from decides nothing"
    );
    apply(&mut task, Event::Decide { card: "q2".into(), decision: answers });
    assert_eq!(task.callback(&callback), Some(CallbackView::Answered));
    assert_eq!(task.apply(task.revision(), move_card("q2", "q3")), Err(Refusal::CorrelationMismatch), "a decided card moves nowhere");
}

#[test]
fn a_decided_callback_settles_inside_the_turn_that_asked_it() {
    let mut task = running();
    let callback = asked(&mut task, "decision", 5, WaitReason::Permission);
    assert_eq!(task.callback(&callback), Some(CallbackView::AwaitingAnswer));
    assert_eq!(task.card_callback(&"decision".into()), Some(&callback));
    assert_eq!(task.cards(), vec![(&InteractionId::from("decision"), WaitReason::Permission)]);
    apply(&mut task, Event::Decide { card: "decision".into(), decision: Decision::Allow });
    assert_eq!(task.callback(&callback), Some(CallbackView::Answered));
    assert_eq!(task.card_callback(&"decision".into()), None);
    assert_eq!(task.activity(), ActivityView::Working);
    assert_eq!(
        task.apply(task.revision(), Event::Decide { card: "decision".into(), decision: Decision::Allow }),
        Err(Refusal::InvalidState),
        "a decided card is decided once"
    );
    let continuation = decision_continuation(task.generation(), 5);
    fact(&mut task, RuntimeFact::CallbackSettled { callback: callback.clone(), continuation: continuation.clone() });
    fact(&mut task, RuntimeFact::ContinuationConsumed { continuation });
    assert_eq!(task.callback(&callback), None);
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    assert_eq!(task.activity(), ActivityView::Resting);
    assert_eq!(task.cards(), vec![(&InteractionId::from("unheard"), WaitReason::Unheard)]);
}

#[test]
fn an_aborted_callback_takes_its_card_and_refuses_a_late_decision() {
    let mut task = running();
    let callback = asked(&mut task, "decision", 5, WaitReason::Question);
    fact(&mut task, RuntimeFact::CallbackAborted { callback: callback.clone(), unheard_card: "cancelled".into() });
    assert!(task.cards().is_empty(), "the turn that asked still runs: nothing else is required");
    assert_eq!(task.callback(&callback), None);
    assert_eq!(
        task.apply(task.revision(), Event::Decide { card: "decision".into(), decision: Decision::Answers { answers: vec!["Yes".into()] } }),
        Err(Refusal::InvalidState)
    );
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    assert_eq!(task.cards(), vec![(&InteractionId::from("unheard"), WaitReason::Unheard)]);
}

#[test]
fn a_policy_decision_opens_its_callback_answered_without_a_card() {
    let mut task = running();
    let callback = decision_callback(task.generation(), 9);
    fact(&mut task, RuntimeFact::AnsweredByPolicy { callback: callback.clone(), decision: Decision::Allow });
    assert!(task.cards().is_empty());
    assert_eq!(task.callback(&callback), Some(CallbackView::Answered));
    assert_eq!(task.activity(), ActivityView::Working);
    let stamp = task.execution().unwrap().clone();
    let cursor = task.cursor().unwrap() + 1;
    for refused in [
        RuntimeFact::AnsweredByPolicy { callback: callback.clone(), decision: Decision::Allow },
        RuntimeFact::Ask { card: "again".into(), callback: callback.clone(), reason: WaitReason::Permission },
        RuntimeFact::AnsweredByPolicy { callback: decision_callback(1, 10), decision: Decision::Deny { message: "".into() } },
    ] {
        let before = format!("{task:?}");
        assert!(task.apply(task.revision(), Event::Runtime { stamp: stamp.clone(), cursor, fact: refused.clone() }).is_err(), "{refused:?}");
        assert_eq!(format!("{task:?}"), before);
    }
    let continuation = decision_continuation(task.generation(), 9);
    fact(&mut task, RuntimeFact::CallbackSettled { callback: callback.clone(), continuation: continuation.clone() });
    fact(&mut task, RuntimeFact::ContinuationConsumed { continuation });
    assert_eq!(task.callback(&callback), None);
    assert_eq!(task.activity(), ActivityView::Working);
}

/// A subagent hands its report back after the turn ended and Claude takes it up by itself: the
/// task sees it working, the card that asked how it goes on no longer holds, and a message that
/// waited joins that turn.
#[test]
fn a_turn_claude_opens_by_itself_takes_the_place_of_the_card_and_of_a_waiting_message() {
    let mut rested = running();
    fact(&mut rested, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    assert_eq!(rested.cards(), [(&card("turn-card"), WaitReason::Unheard)]);
    fact(&mut rested, RuntimeFact::TurnStarted { turn: turn("own"), trigger: Trigger::Unprompted });
    assert!(rested.cards().is_empty(), "Claude goes on by itself");
    assert_eq!(rested.activity(), ActivityView::Working);
    fact(&mut rested, RuntimeFact::TurnEnded { turn: turn("own"), unheard_card: card("own-card"), wake: None, awaits: None });
    assert_eq!(rested.cards(), [(&card("own-card"), WaitReason::Unheard)], "its end asks again how it goes on");

    let mut told = running();
    fact(&mut told, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    apply(&mut told, Event::Tell { input: input("told") });
    fact(&mut told, RuntimeFact::TurnStarted { turn: turn("own"), trigger: Trigger::Unprompted });
    assert_eq!((told.activity(), told.pending_inputs()), (ActivityView::Working, vec![&input("told")]));
    fact(&mut told, RuntimeFact::InputConsumed { input: input("told") });
    assert!(told.pending_inputs().is_empty(), "the message joins the turn Claude opened");

    let mut asking = running();
    fact(&mut asking, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    apply(&mut asking, Event::Ask { card: card("question") });
    fact(&mut asking, RuntimeFact::TurnStarted { turn: turn("own"), trigger: Trigger::Unprompted });
    assert_eq!(asking.cards(), [(&card("question"), WaitReason::Question)], "a question the node asked still waits on its answer");

    let mut working = running();
    let refused = working.apply(working.revision(), Event::Runtime {
        stamp: working.execution().unwrap().clone(), cursor: working.cursor().unwrap() + 1,
        fact: RuntimeFact::TurnStarted { turn: turn("own"), trigger: Trigger::Unprompted },
    });
    assert_eq!(refused, Err(Refusal::CorrelationMismatch), "a turn already runs");
}

/// Claude asks from a turn the task did not see start while what it was told waits for it: the
/// node of 2026-10-21, whose plan Alondra refused while a message waited behind its own turn.
#[test]
fn a_decision_asked_while_what_the_node_was_told_waits_settles_and_the_message_still_starts_its_turn() {
    let told = || {
        let mut task = running();
        fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
        apply(&mut task, Event::Tell { input: input("told") });
        assert_eq!(task.activity(), ActivityView::Starting);
        task
    };
    let refused = Decision::Deny { message: "your plan's checklist holds at most 40 points".into() };
    let mut policy = told();
    let callback = decision_callback(policy.generation(), 7);
    fact(&mut policy, RuntimeFact::AnsweredByPolicy { callback: callback.clone(), decision: refused });
    assert_eq!(policy.callback(&callback), Some(CallbackView::Answered));
    assert!(policy.cards().is_empty());
    let continuation = decision_continuation(policy.generation(), 7);
    fact(&mut policy, RuntimeFact::CallbackSettled { callback: callback.clone(), continuation });
    assert_eq!(policy.callback(&callback), None);
    assert_eq!((policy.activity(), policy.pending_inputs()), (ActivityView::Starting, vec![&input("told")]), "the message still waits for its turn");
    fact(&mut policy, RuntimeFact::TurnStarted { turn: turn("told"), trigger: Trigger::Input(input("told")) });
    assert_eq!(policy.activity(), ActivityView::Working);

    let mut asked = told();
    let callback = decision_callback(asked.generation(), 8);
    fact(&mut asked, RuntimeFact::Ask { card: card("plan"), callback: callback.clone(), reason: WaitReason::Permission });
    assert_eq!(asked.cards(), [(&card("plan"), WaitReason::Permission)]);
    apply(&mut asked, Event::Decide { card: card("plan"), decision: Decision::Allow });
    assert!(asked.cards().is_empty());
    let continuation = decision_continuation(asked.generation(), 8);
    fact(&mut asked, RuntimeFact::CallbackSettled { callback, continuation });
    fact(&mut asked, RuntimeFact::TurnStarted { turn: turn("told"), trigger: Trigger::Input(input("told")) });
    assert_eq!(asked.activity(), ActivityView::Working);

    let mut aborted = told();
    let callback = decision_callback(aborted.generation(), 9);
    fact(&mut aborted, RuntimeFact::Ask { card: card("plan"), callback: callback.clone(), reason: WaitReason::Permission });
    fact(&mut aborted, RuntimeFact::CallbackAborted { callback, unheard_card: card("unheard") });
    assert!(aborted.cards().is_empty(), "the message that waits explains what comes next");
    assert_eq!(aborted.pending_inputs(), [&input("told")]);
}

#[test]
fn a_policy_decision_needs_an_available_execution() {
    let mut task = running();
    fact(&mut task, RuntimeFact::ConnectionLost { problem_card: "recovery".into() });
    let stamp = task.execution().unwrap().clone();
    let cursor = task.cursor().unwrap() + 1;
    assert_eq!(
        task.apply(task.revision(), Event::Runtime { stamp, cursor, fact: RuntimeFact::AnsweredByPolicy {
            callback: decision_callback(1, 3), decision: Decision::Allow } }),
        Err(Refusal::InvalidState)
    );
}

#[test]
fn a_replacement_settles_the_card_an_ended_host_left_but_never_an_open_question() {
    let mut task = running();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut task, Event::ExecutionReleased { input: input("replace") });
    admit(&mut task, "replacement");
    assert!(task.cards().is_empty(), "the new execution is how the task goes on");
    assert_eq!(task.activity(), ActivityView::Starting);
    assert_eq!(task.generation(), 2);

    let mut asked = running();
    apply(&mut asked, Event::Ask { card: card("question") });
    fact(&mut asked, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    fact(&mut asked, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut asked, Event::ExecutionReleased { input: input("replace") });
    let admitted = asked.apply(asked.revision(), Event::Admit {
        node: AgentId::from("node"),
        conversation: ConversationId::new("conversation").unwrap(),
        instance: InstanceId::new("replacement").unwrap(),
        host: HostId::new("host").unwrap(),
    });
    assert_eq!(admitted, Err(Refusal::InvalidState), "a question the user has not answered still holds the task");
}

#[test]
fn what_someone_tells_the_node_joins_the_running_turn_or_waits_for_the_next_host() {
    let mut task = running();
    apply(&mut task, Event::Tell { input: input("told") });
    assert_eq!(task.pending_inputs(), [&input("told")], "it waits in the inbox of the turn that runs");
    assert_eq!(task.apply(task.revision(), Event::Tell { input: input("told") }), Err(Refusal::DuplicateObligation), "told once");
    fact(&mut task, RuntimeFact::InputConsumed { input: input("told") });
    assert!(task.pending_inputs().is_empty(), "and that same turn takes it");

    let mut ended = running();
    fact(&mut ended, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    fact(&mut ended, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut ended, Event::Tell { input: input("later") });
    assert_eq!(ended.pending_inputs(), [&input("later")], "it waits for the host that resumes the session");

    let mut cancelled = running();
    apply(&mut cancelled, Event::Finish { intent: FinishIntent::Cancel, cleanup: operation("cleanup"), post_merge: None });
    apply(&mut cancelled, Event::Tell { input: input("clean") });
    assert_eq!(cancelled.pending_inputs(), [&input("clean")], "a cancelled task's node can still be told to settle its worktrees");
}

#[test]
fn what_the_node_is_told_answers_the_card_that_asked_how_it_goes_on_but_never_a_question() {
    let mut rested = running();
    fact(&mut rested, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    assert_eq!(rested.cards(), [(&card("turn-card"), WaitReason::Unheard)]);
    apply(&mut rested, Event::Tell { input: input("deliver") });
    assert!(rested.cards().is_empty(), "being told how to go on is the answer the card waited for");
    assert_eq!(rested.pending_inputs(), [&input("deliver")]);
    assert_eq!(rested.activity(), ActivityView::Starting, "the node takes it in its next turn");

    let mut gone = running();
    fact(&mut gone, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    fact(&mut gone, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut gone, Event::Tell { input: input("later") });
    assert!(gone.cards().is_empty());
    assert_eq!(gone.pending_inputs(), [&input("later")], "it waits for the host that resumes the session");

    let mut asking = running();
    apply(&mut asking, Event::Ask { card: card("question") });
    apply(&mut asking, Event::Tell { input: input("aside") });
    assert_eq!(asking.cards(), [(&card("question"), WaitReason::Question)], "a question the node asked still waits on its answer");
    assert_eq!(asking.pending_inputs(), [&input("aside")]);
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x1a9b_7),
        ..ProptestConfig::default()
    })]
    /// What lets a replay look up only the inputs each event names: an input becomes pending only
    /// through an event that names it, and `is_pending` answers what `pending_inputs` lists.
    #[test]
    fn an_input_becomes_pending_only_through_the_event_that_names_it(actions in prop::collection::vec((0u8..28, 0u8..6), 1..200)) {
        let pool: Vec<InputId> = ["initial", "a", "b", "c", "d", "e"].iter().map(|name| input(name)).collect();
        let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
        for (action, pick) in actions {
            let named = pool[usize::from(pick)].clone();
            let runtime = |fact| task.execution().cloned().map(|stamp| Event::Runtime { stamp, cursor: task.cursor().unwrap() + 1, fact });
            let first = task.pending_inputs().first().map(|input| (*input).clone()).unwrap_or_else(|| named.clone());
            let event = match action {
                0 => Some(Event::Admit { node: "node".into(), conversation: ConversationId::new("conversation").unwrap(), instance: InstanceId::new("instance").unwrap(), host: HostId::new("host").unwrap() }),
                1 => runtime(RuntimeFact::ResourceAvailable),
                2 => runtime(RuntimeFact::TurnStarted { turn: turn("root"), trigger: Trigger::Input(first) }),
                3 => runtime(RuntimeFact::InputConsumed { input: first }),
                4 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None }),
                5..=8 => Some(Event::Tell { input: named }),
                9 => Some(Event::ExecutionReleased { input: named }),
                10 => runtime(RuntimeFact::HostEnded { unheard_card: "ended".into(), resume: None }),
                11 => Some(Event::DoubtHandoff { card: "doubt".into(), input: named }),
                12 => Some(Event::SettleHandoff { card: "doubt".into(), resend: Some(named), unheard_card: "settled".into() }),
                13 => Some(Event::SettleHandoff { card: "doubt".into(), resend: None, unheard_card: "settled".into() }),
                14 => Some(Event::ResendUntaken { input: first, resend: named }),
                15 => runtime(RuntimeFact::ResourceReleased { problem_card: "recovery".into() }),
                16 => Some(Event::RetireReleasedExecution { problem_card: "recovery".into(), input: named }),
                17 => Some(Event::WaitForDependencies { dependencies: vec![TaskId::from("dependency")] }),
                18 => Some(Event::DependenciesResolved { input: named }),
                19 => Some(Event::Pause { card: "pause".into(), owner: Owner::User }),
                20 => Some(Event::Resume { card: "pause".into(), input: named }),
                21 => Some(Event::Present { delivery: "result".into() }),
                22 => Some(Event::HandBack { delivery: "result".into(), input: named }),
                23 => Some(Event::Finish { intent: FinishIntent::Cancel, cleanup: operation("cleanup"), post_merge: None }),
                24 => Some(Event::Reopen { input: named }),
                25 => runtime(RuntimeFact::HostEnded { unheard_card: "ended".into(), resume: Some(named) }),
                26 => Some(Event::ResendDoubtful { input: first, resend: named }),
                _ => Some(Event::Compact { input: named }),
            };
            let Some(event) = event else { continue };
            let before: Vec<InputId> = task.pending_inputs().into_iter().cloned().collect();
            let debug = format!("{task:?}");
            let names: Vec<InputId> = event.inputs().into_iter().cloned().collect();
            if task.apply(task.revision(), event).is_err() {
                prop_assert_eq!(format!("{task:?}"), debug, "a refused event leaves no trace");
                continue;
            }
            for pending in task.pending_inputs() {
                prop_assert!(before.contains(pending) || names.contains(pending), "{:?} became pending through an event that does not name it", pending);
            }
            for candidate in &pool {
                prop_assert_eq!(task.is_pending(candidate), task.pending_inputs().contains(&candidate));
            }
        }
    }
}

#[test]
fn advancing_leaves_the_aggregate_it_started_from_as_it_was() {
    let planned = || TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    let admission = || Event::Admit {
        node: AgentId::from("node"),
        conversation: ConversationId::new("conversation").unwrap(),
        instance: InstanceId::new("instance").unwrap(),
        host: HostId::new("host").unwrap(),
    };
    let task = planned();
    let next = task.advanced(task.revision(), admission()).unwrap();
    assert_eq!(task, planned(), "the aggregate a reader still holds is unchanged");
    let mut applied = planned();
    admit(&mut applied, "instance");
    assert_eq!(next, applied, "advancing reaches what applying reaches");
    assert_eq!(task.advanced(task.revision() + 1, admission()), Err(Refusal::StaleRevision));
    assert!(next.advanced(next.revision(), admission()).is_err(), "a second admission of the same execution is refused");
}

/// A task whose node declared work after the merge, with its delivery accepted and merged and the
/// root turn still running that work.
fn merged_with_work_after_it() -> TaskLifecycle {
    let mut task = running();
    apply(&mut task, Event::PostMergeDeclared { operation: operation("release") });
    apply(&mut task, Event::Present { delivery: "delivery".into() });
    apply(&mut task, Event::Accept { delivery: "delivery".into(), merge: Some(operation("merge")) });
    apply(&mut task, Event::MergeObserved { operation: operation("merge") });
    assert!(task.accepted());
    task
}

#[test]
fn a_host_that_ends_during_the_work_after_the_merge_asks_the_user_how_it_goes_on() {
    let mut task = merged_with_work_after_it();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    assert_eq!(task.phase(), TaskPhase::Delivered);
    assert_eq!(task.activity(), ActivityView::Resting);
    assert_eq!(task.cards(), [(&card("session-ended"), WaitReason::Unheard)], "the ended session can no longer say the work ended");

    let mut task = merged_with_work_after_it();
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    assert!(task.cards().is_empty(), "a resting node may still say the work ended");
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    assert_eq!(task.cards(), [(&card("session-ended"), WaitReason::Unheard)], "once its session ends it cannot");
}

#[test]
fn a_resumed_session_goes_on_with_the_work_after_the_merge_without_a_card() {
    let mut task = merged_with_work_after_it();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: Some(input("resumed")) });
    assert!(task.cards().is_empty());
    assert_eq!(task.pending_inputs(), [&input("resumed")]);
    assert_eq!(task.phase(), TaskPhase::Delivered);
}

#[test]
fn ending_the_work_after_the_merge_or_telling_the_node_settles_the_card_of_its_ended_session() {
    let mut task = merged_with_work_after_it();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut task, Event::PostMergeEnded { operation: operation("release"), fact: PostMergeFact::UserDecision });
    assert!(task.cards().is_empty());
    assert_eq!(task.post_merge(), None);

    let mut task = merged_with_work_after_it();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    apply(&mut task, Event::Tell { input: input("told") });
    assert!(task.cards().is_empty());
    assert_eq!(task.pending_inputs(), [&input("told")]);
}

/// A delivery the user answered with a reply: the node works on it in a turn of its own, which
/// left a background shell running when it ended.
fn answering_a_reply_with_background_work() -> TaskLifecycle {
    let mut task = running();
    apply(&mut task, Event::Present { delivery: "delivery".into() });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    assert!(task.cards().is_empty() && task.activity() == ActivityView::Resting, "the delivery waits on the user");
    apply(&mut task, Event::Tell { input: input("reply") });
    fact(&mut task, RuntimeFact::TurnStarted { turn: turn("reply"), trigger: Trigger::Input(input("reply")) });
    fact(&mut task, RuntimeFact::WorkStarted { turn: turn("reply"), work: work("snapshots") });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("reply"), unheard_card: card("reply-card"), wake: None, awaits: None });
    assert_eq!((task.phase(), task.activity()), (TaskPhase::Delivered, ActivityView::OwnWork));
    task
}

#[test]
fn a_host_that_ends_while_the_node_works_on_the_users_reply_resumes_its_session() {
    let mut task = answering_a_reply_with_background_work();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: Some(input("resumed")) });
    assert_eq!(task.phase(), TaskPhase::Delivered);
    assert!(task.cards().is_empty(), "no card asks the user");
    assert_eq!(task.pending_inputs(), [&input("resumed")], "the resumed session is told to go on");
    assert_eq!(task.activity(), ActivityView::Starting);
}

#[test]
fn a_host_that_ends_while_the_node_works_on_the_users_reply_without_a_resume_leaves_the_delivery_standing() {
    let mut task = answering_a_reply_with_background_work();
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    assert_eq!((task.phase(), task.activity()), (TaskPhase::Delivered, ActivityView::Resting));
    assert!(task.cards().is_empty() && task.pending_inputs().is_empty(), "the delivery the user decides on is what stands");
}

#[test]
fn a_host_that_ends_on_a_delivery_at_rest_leaves_it_waiting_for_the_user() {
    let mut task = running();
    apply(&mut task, Event::Present { delivery: "delivery".into() });
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: card("turn-card"), wake: None, awaits: None });
    let cursor = task.cursor().unwrap() + 1;
    let stamp = task.execution().unwrap().clone();
    assert_eq!(
        task.apply(task.revision(), Event::Runtime { stamp, cursor, fact: RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: Some(input("resumed")) } }),
        Err(Refusal::InvalidState),
        "nothing was cut: the session has nothing to go on with"
    );
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    assert!(task.cards().is_empty() && task.pending_inputs().is_empty(), "the delivery still waits on the user");
}

#[test]
fn a_host_that_ends_on_a_delivery_that_owes_nothing_after_the_merge_asks_nothing() {
    let mut task = running();
    apply(&mut task, Event::Present { delivery: "delivery".into() });
    apply(&mut task, Event::Accept { delivery: "delivery".into(), merge: None });
    fact(&mut task, RuntimeFact::HostEnded { unheard_card: card("session-ended"), resume: None });
    assert!(task.cards().is_empty());
}

fn delivered_over(cards: &[Event]) -> TaskLifecycle {
    let mut task = running();
    fact(&mut task, RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None });
    for card in cards {
        apply(&mut task, card.clone());
    }
    apply(&mut task, Event::Present { delivery: "result".into() });
    task
}
fn held(task: &TaskLifecycle) -> Vec<InteractionId> {
    task.cards().into_iter().map(|(card, _)| card.clone()).collect()
}

#[test]
fn a_hand_back_tells_the_session_how_it_goes_on_when_its_turn_ended_unheard() {
    let mut task = delivered_over(&[]);
    assert_eq!(held(&task), vec![InteractionId::from("unheard")]);
    apply(&mut task, Event::HandBack { delivery: "result".into(), input: input("changes") });
    assert!(task.cards().is_empty());
    assert_eq!(task.stage(), Some(WorkStage::ChangesRequested));
    assert_eq!(task.pending_inputs(), vec![&input("changes")]);
}

#[test]
fn a_hand_back_keeps_the_questions_that_still_wait_and_queues_the_changes() {
    let mut task = delivered_over(&[Event::Ask { card: "question".into() }]);
    apply(&mut task, Event::HandBack { delivery: "result".into(), input: input("changes") });
    assert_eq!(held(&task), vec![InteractionId::from("question")]);
    assert_eq!(task.phase(), TaskPhase::InProgress);
    assert_eq!(task.stage(), Some(WorkStage::ChangesRequested));
    assert_eq!(task.pending_inputs(), vec![&input("changes")]);
}


#[test]
fn retry_answer_before_native_readiness_preserves_original_mission_and_never_claims_availability(){
    let mut task=TaskLifecycle::planned("task".into(),input("original-mission")).unwrap();admit(&mut task,"instance");apply(&mut task,Event::Ask{card:"retry".into()});apply(&mut task,Event::Answer{card:"retry".into(),input:input("user-retry")});
    assert_eq!(task.resource(),Some(ResourceEvidence::Reserved));assert_eq!(task.pending_inputs().iter().map(|input|input.as_str()).collect::<Vec<_>>(),["original-mission","user-retry"]);assert!(task.turn().is_none());
    let before=format!("{task:?}");let stamp=task.execution().unwrap().clone();assert!(task.apply(task.revision(),Event::Runtime{stamp,cursor:1,fact:RuntimeFact::TurnStarted{turn:turn("invalid"),trigger:Trigger::Input(input("original-mission"))}}).is_err());assert_eq!(format!("{task:?}"),before);
    fact(&mut task,RuntimeFact::ResourceAvailable);fact(&mut task,RuntimeFact::TurnStarted{turn:turn("actual"),trigger:Trigger::Input(input("original-mission"))});assert_eq!(task.pending_inputs().iter().map(|input|input.as_str()).collect::<Vec<_>>(),["user-retry"]);
}
