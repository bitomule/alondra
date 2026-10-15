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
        Event::Answer {
            card: "permission".into(),
            input: input("unused"),
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
        Event::Answer {
            card: "second".into(),
            input: input("unused"),
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

use proptest::prelude::*;
proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0xa10d_a),
        ..ProptestConfig::default()
    })]
    #[test]
    fn arbitrary_histories_preserve_terminal_exclusion_and_atomic_rejections(actions in prop::collection::vec(0u8..26, 1..160)) {
        let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
        for action in actions {
            let runtime = |fact| task.execution().cloned().map(|stamp| Event::Runtime { stamp, cursor: task.cursor().unwrap() + 1, fact });
            let event = match action {
                0 => Some(Event::Admit { node: "node".into(), conversation: ConversationId::new("conversation").unwrap(), instance: InstanceId::new("instance").unwrap(), host: HostId::new("host").unwrap() }),
                1 => runtime(RuntimeFact::ResourceAvailable),
                2 => runtime(RuntimeFact::TurnStarted { turn: turn("root"), trigger: Trigger::Input(input("initial")) }),
                3 => runtime(RuntimeFact::WorkStarted { turn: turn("root"), work: work("child") }),
                4 => runtime(RuntimeFact::WorkEnded { work: work("child"), continuation: continuation("wake") }),
                5 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into() }),
                6 => runtime(RuntimeFact::Ask { card: "first".into(), callback: CallbackId::new("first").unwrap(), reason: WaitReason::Permission }),
                7 => Some(Event::Answer { card: "first".into(), input: input("reply") }),
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
                21 => Some(Event::Pause { card: "pause".into() }),
                22 => Some(Event::Resume { card: "pause".into(), input: input("resume") }),
                23 => runtime(RuntimeFact::Notification),
                24 => Some(Event::RetireReleasedExecution { problem_card: "recovery".into(), input: input("retry") }),
                _ => Some(Event::HandBack { delivery: "result".into(), input: input("changes") }),
            };
            let Some(event) = event else { continue };
            let before = format!("{task:?}");
            if task.apply(task.revision(), event).is_err() { prop_assert_eq!(format!("{task:?}"), before); }
            if matches!(task.phase(), TaskPhase::Completed | TaskPhase::Cancelled) {
                prop_assert!(task.execution().is_none());
                prop_assert!(!task.holds_slot());
                prop_assert_eq!(task.activity(), ActivityView::None);
                prop_assert_eq!(task.drain().count(), 0);
            }
            if task.phase() == TaskPhase::Finishing { prop_assert!(task.drain().count() > 0); }
            if task.activity() == ActivityView::OwnWork { prop_assert!(task.holds_slot()); }
            if task.phase() == TaskPhase::InProgress && task.activity() == ActivityView::Resting { prop_assert!(!task.cards().is_empty() || !task.dependencies().is_empty()); }
            if let Some(stamp) = task.execution() { prop_assert_eq!(stamp.task(), task.task()); }
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
        },
    );
    apply(
        &mut task,
        Event::Answer {
            card: "permission".into(),
            input: input("unused"),
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
        },
    );
    for id in ["first", "second"] {
        apply(
            &mut task,
            Event::Answer {
                card: id.into(),
                input: input("unused"),
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
        },
    );
    assert_eq!(
        task.apply(
            task.revision(),
            Event::Pause {
                card: "pause-b".into()
            }
        ),
        Err(Refusal::DuplicateObligation)
    );
    fact(
        &mut task,
        RuntimeFact::TurnEnded {
            turn: turn("root"),
            unheard_card: "unused".into(),
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
        },
    );
    apply(
        &mut task,
        Event::Answer {
            card: "permission".into(),
            input: input("unused"),
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
