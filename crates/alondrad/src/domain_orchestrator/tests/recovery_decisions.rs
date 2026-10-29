use super::testing::{Fixture, fixture};
use super::*;

fn legacy_batch_with_confirmed_effect(f: &Fixture) -> Run {
    batch_with_confirmed_effect(f, false)
}

fn batch_with_confirmed_effect(f: &Fixture, selected: bool) -> Run {
    f.tell(
        "legacy-confirmed-request",
        "Keep this confirmed legacy change",
    );
    let epoch = f.domain().read().state().data_epoch.clone();
    let ids: Vec<_> = f
        .executor
        .pending()
        .unwrap()
        .iter()
        .map(|message| message.id)
        .collect();
    let selection = selected.then(|| {
        crate::orchestrator_provider::selection(f.domain(), &f.executor.home, now_millis()).unwrap()
    });
    let (claims, owner) = if let Some(selection) = &selection {
        f.domain()
            .store
            .lock()
            .unwrap()
            .claim_selected_orchestrator_batch(
                &ids,
                "legacy-confirmed",
                "legacy-confirmed-session",
                epoch.clone(),
                Some(selection),
            )
            .unwrap()
    } else {
        f.domain()
            .store
            .lock()
            .unwrap()
            .claim_orchestrator_batch(
                &ids,
                "legacy-confirmed",
                "legacy-confirmed-session",
                epoch.clone(),
            )
            .unwrap()
    };
    drop(owner);
    let bearer = f.domain().bind_orchestrator("legacy-confirmed").unwrap();
    f.domain()
        .execute_bearer(
            bearer.expose_for_child_environment(),
            &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: epoch,
                command_id: "legacy-confirmed-title".into(),
                expected_revision: 0,
                command: Command::TitleRequest {
                    request_id: "legacy-confirmed-request".into(),
                    title: "Confirmed legacy title".into(),
                },
            },
        )
        .unwrap();
    f.domain().end_execution("legacy-confirmed");
    let run = Run {
        id: "legacy-confirmed".into(),
        message_ids: claims.iter().map(|claim| claim.message.id).collect(),
        attempts: claims
            .iter()
            .map(|claim| (claim.attempt_id, claim.version))
            .collect(),
        started_at: 1,
        spawned: true,
    };
    save(
        &f.executor.home,
        &Runtime {
            active_run: Some(run.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        f.domain()
            .store
            .lock()
            .unwrap()
            .batch_selection(&run.id)
            .unwrap()
            .is_some(),
        selected
    );
    if selected {
        f.domain()
            .live
            .orchestrator_runs
            .lock()
            .unwrap()
            .insert(run.id.clone());
    }
    run
}

fn assert_legacy_claims_discarded(executor: &Executor, run: &Run) {
    let store = executor.domain.store.lock().unwrap();
    let attempts = store.delivery_evidence_page(0).unwrap();
    assert!(
        attempts
            .iter()
            .filter(|attempt| run.message_ids.contains(&attempt.message_id))
            .all(|attempt| attempt.state == "discarded" && attempt.current_state == "discarded")
    );
    let epoch = executor.domain.read().state().data_epoch.clone();
    assert!(
        !store
            .unfinished_orchestrator_batches(&epoch, None, None)
            .unwrap()
            .iter()
            .any(|batch| batch.run == run.id)
    );
    assert_eq!(
        store.batch_effects(&run.id).unwrap().len(),
        1,
        "the confirmed command is not replayed"
    );
}

#[tokio::test]
async fn legacy_confirmed_batch_without_selection_resolves_on_startup_and_new_requests_proceed() {
    let f = fixture();
    let run = legacy_batch_with_confirmed_effect(&f);
    let restarted = Executor {
        domain: Arc::new(DomainRuntime::open(&f.executor.home).unwrap()),
        home: f.executor.home.clone(),
        claude_home: f.executor.claude_home.clone(),
    };
    let runtime = restarted.startup_recovery().await.unwrap();
    assert!(
        runtime.active_run.is_none()
            && runtime.last_error.is_none()
            && runtime.batch_handoffs.is_empty()
    );
    assert_eq!(
        runtime.pending_resolutions,
        vec![Resolution {
            run_id: run.id.clone(),
            message_ids: run.message_ids.clone()
        }]
    );
    assert_legacy_claims_discarded(&restarted, &run);
    assert_eq!(
        restarted.domain.read().state().requests[&"legacy-confirmed-request".into()]
            .title
            .as_deref(),
        Some("Confirmed legacy title")
    );
    assert!(f.starts().is_empty());
    let (backend, _) = f.domain().sdk().unwrap();
    restarted.domain.enable_sdk(backend.clone());
    let epoch = restarted.domain.read().state().data_epoch.clone();
    restarted
        .domain
        .execute_fixture(
            &Actor::User {
                user_id: "u".into(),
            },
            CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: epoch,
                command_id: "after-legacy-recovery".into(),
                expected_revision: 0,
                command: Command::CreateRequest {
                    request_id: "after-legacy-recovery".into(),
                    body: "Process the new request after legacy recovery".into(),
                    auto_merge: false,
                    maintenance: false,
                    message: true,
                },
            },
        )
        .unwrap();
    restarted.tick().await.unwrap();
    assert!(restarted.domain.read().state().requests[&"after-legacy-recovery".into()].closed);
    assert_eq!(f.starts().len(), 1);
    assert!(
        f.prompts()
            .last()
            .unwrap()
            .contains("Previous interrupted inbox batches have already been resolved")
    );
    assert!(
        restarted
            .domain
            .read()
            .state()
            .interactions
            .keys()
            .all(|id| !id.0.starts_with(PROBLEM_PREFIX))
    );
}

#[tokio::test]
async fn legacy_resolution_save_failure_keeps_claims_and_preserves_an_unrelated_active_run() {
    for unrelated in [false, true] {
        let f = fixture();
        let run = legacy_batch_with_confirmed_effect(&f);
        if unrelated {
            save(
                &f.executor.home,
                &Runtime {
                    active_run: Some(Run {
                        id: "unrelated-current-run".into(),
                        message_ids: vec![],
                        attempts: vec![],
                        started_at: 17,
                        spawned: true,
                    }),
                    last_error: Some("Keep the other run's explicit refusal".into()),
                    failure: Some(Failure::Denied),
                    retry_at: Some(123),
                    note: Some("Keep the unrelated context".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let before = load(&f.executor.home).unwrap();
        let fault = faults::Injection::at(&f.executor.home, "runtime_write", 1);
        assert!(f.executor.repair_incomplete().await.is_err());
        assert_eq!(fault.hits(), 1);
        drop(fault);
        assert_eq!(load(&f.executor.home).unwrap(), before);
        let attempts = f
            .domain()
            .store
            .lock()
            .unwrap()
            .delivery_evidence_page(0)
            .unwrap();
        assert!(
            attempts
                .iter()
                .filter(|attempt| run.message_ids.contains(&attempt.message_id))
                .all(|attempt| attempt.state == "dispatching"
                    && attempt.current_state == "dispatching")
        );
        f.executor.repair_incomplete().await.unwrap();
        f.executor.repair_incomplete().await.unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert_eq!(
            runtime.pending_resolutions,
            vec![Resolution {
                run_id: run.id.clone(),
                message_ids: run.message_ids.clone()
            }]
        );
        assert!(runtime.batch_handoffs.is_empty());
        if unrelated {
            let mut expected = before;
            expected.pending_resolutions = runtime.pending_resolutions.clone();
            assert_eq!(runtime, expected);
        } else {
            assert!(runtime.active_run.is_none() && runtime.last_error.is_none());
        }
        assert_legacy_claims_discarded(&f.executor, &run);
        assert!(f.starts().is_empty());
    }
}

#[tokio::test]
async fn legacy_confirmed_batches_keep_explicit_denial_and_cancellation() {
    for cancelled in [false, true] {
        let f = fixture();
        let run = legacy_batch_with_confirmed_effect(&f);
        let mut runtime = load(&f.executor.home).unwrap();
        if cancelled {
            runtime.cancel_requested = true;
        } else {
            runtime.last_error = Some("The native tool was denied".into());
            runtime.failure = Some(Failure::Denied);
        }
        save(&f.executor.home, &runtime).unwrap();
        f.executor.repair_incomplete().await.unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert_eq!(runtime.active_run.unwrap().id, run.id);
        assert_eq!(
            runtime.failure,
            Some(if cancelled {
                Failure::Cancelled
            } else {
                Failure::Denied
            })
        );
        assert!(runtime.pending_resolutions.is_empty() && runtime.batch_handoffs.is_empty());
        assert!(f.starts().is_empty());
    }
}

#[tokio::test]
async fn selected_continuation_metadata_save_failure_keeps_old_claims_recoverable() {
    for unrelated in [false, true] {
        let f = fixture();
        let run = batch_with_confirmed_effect(&f, true);
        if unrelated {
            save(
                &f.executor.home,
                &Runtime {
                    active_run: Some(Run {
                        id: "unrelated-current-run".into(),
                        message_ids: vec![],
                        attempts: vec![],
                        started_at: 23,
                        spawned: true,
                    }),
                    last_error: Some("Preserve the unrelated stop".into()),
                    failure: Some(Failure::Denied),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let before = load(&f.executor.home).unwrap();
        let fault = faults::Injection::at(&f.executor.home, "runtime_write", 1);
        assert!(f.executor.repair_incomplete().await.is_err());
        assert_eq!(fault.hits(), 1);
        drop(fault);
        assert_eq!(load(&f.executor.home).unwrap(), before);
        let attempts = f
            .domain()
            .store
            .lock()
            .unwrap()
            .delivery_evidence_page(0)
            .unwrap();
        assert!(
            attempts
                .iter()
                .filter(|attempt| run.message_ids.contains(&attempt.message_id))
                .all(|attempt| attempt.state == "dispatching"
                    && attempt.current_state == "dispatching")
        );
        f.executor.repair_incomplete().await.unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert_eq!(runtime.batch_handoffs.len(), 1);
        assert_eq!(
            runtime.pending_resolutions,
            vec![Resolution {
                run_id: run.id.clone(),
                message_ids: run.message_ids.clone()
            }]
        );
        if unrelated {
            assert_eq!(runtime.active_run, before.active_run);
            assert_eq!(runtime.last_error, before.last_error);
            assert_eq!(runtime.failure, before.failure);
        } else {
            assert!(runtime.active_run.is_none());
        }
        assert!(f.starts().is_empty());
    }
}

#[tokio::test]
async fn persisted_handoff_survives_reconcile_failure_and_restart_without_a_cold_stop() {
    for unrelated in [false, true] {
        let f = fixture();
        let run = batch_with_confirmed_effect(&f, true);
        if unrelated {
            save(
                &f.executor.home,
                &Runtime {
                    active_run: Some(Run {
                        id: "unrelated-current-run".into(),
                        message_ids: vec![],
                        attempts: vec![],
                        started_at: 29,
                        spawned: true,
                    }),
                    last_error: Some("Preserve the unrelated refusal".into()),
                    failure: Some(Failure::Denied),
                    note: Some("Keep this unrelated context".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let before = load(&f.executor.home).unwrap();
        let fault = faults::Injection::at(&f.executor.home, "reconcile", 1);
        assert!(f.executor.repair_incomplete().await.is_err());
        assert_eq!(fault.hits(), 1);
        drop(fault);
        let persisted = load(&f.executor.home).unwrap();
        assert_eq!(persisted.batch_handoffs.len(), 1);
        assert_eq!(persisted.batch_handoffs[0].run_id, run.id);
        assert_eq!(
            persisted.pending_resolutions,
            vec![Resolution {
                run_id: run.id.clone(),
                message_ids: run.message_ids.clone()
            }]
        );
        if unrelated {
            let mut expected = before;
            expected.batch_handoffs = persisted.batch_handoffs.clone();
            expected.pending_resolutions = persisted.pending_resolutions.clone();
            assert_eq!(persisted, expected);
        } else {
            assert!(persisted.active_run.is_none());
        }
        let restarted = Executor {
            domain: Arc::new(DomainRuntime::open(&f.executor.home).unwrap()),
            home: f.executor.home.clone(),
            claude_home: f.executor.claude_home.clone(),
        };
        assert!(
            !restarted
                .domain
                .live
                .orchestrator_runs
                .lock()
                .unwrap()
                .contains(&run.id)
        );
        if unrelated {
            restarted.repair_incomplete().await.unwrap();
        } else {
            restarted.startup_recovery().await.unwrap();
        }
        restarted.repair_incomplete().await.unwrap();
        assert_eq!(
            load(&f.executor.home).unwrap(),
            persisted,
            "restart preserves the known safe handoff and any unrelated run"
        );
        let attempts = restarted
            .domain
            .store
            .lock()
            .unwrap()
            .delivery_evidence_page(0)
            .unwrap();
        assert!(attempts.iter().filter(|attempt| run.message_ids.contains(&attempt.message_id))
            .all(|attempt| attempt.state == "discarded" && attempt.current_state == "discarded"));
        assert!(
            restarted
                .domain
                .read()
                .state()
                .interactions
                .keys()
                .all(|id| !id.0.starts_with(PROBLEM_PREFIX))
        );
        assert!(f.starts().is_empty());
    }
}

#[tokio::test]
async fn startup_preserves_a_current_cancelled_batch_without_legacy_recovery() {
    let f = fixture();
    let run = ambiguous_batch(&f, true);
    let mut expected = load(&f.executor.home).unwrap();
    expected.failure = Some(Failure::Cancelled);
    expected.last_error = Some("The user cancelled this batch; reconcile its effects".into());
    expected.cancel_requested = false;
    save(&f.executor.home, &expected).unwrap();
    let restarted = Executor {
        domain: Arc::new(DomainRuntime::open(&f.executor.home).unwrap()),
        home: f.executor.home.clone(),
        claude_home: f.executor.claude_home.clone(),
    };
    let fault = faults::Injection::at(&f.executor.home, "runtime_write", 1);
    let recovered = restarted.startup_recovery().await.unwrap();
    assert_eq!(
        fault.hits(),
        0,
        "an already reconciled stop needs no legacy runtime rewrite"
    );
    assert_eq!(recovered, expected);
    let read = restarted.domain.read();
    let card = &read.state().interactions[&InteractionId(format!("{PROBLEM_PREFIX}{}", run.id))];
    let problem = BatchProblem::from_body(&card.body).unwrap();
    assert_eq!(problem.stop, BatchStop::Cancelled);
    assert_eq!(problem.error, expected.last_error.unwrap());
    assert!(restarted.pending().unwrap().is_empty());
    let attempts = restarted
        .domain
        .store
        .lock()
        .unwrap()
        .delivery_evidence_page(0)
        .unwrap();
    assert!(
        attempts
            .iter()
            .filter(|attempt| run.message_ids.contains(&attempt.message_id))
            .all(|attempt| attempt.state == "uncertain" && attempt.current_state == "uncertain")
    );
}

#[tokio::test]
async fn startup_still_resolves_a_legacy_sessionless_batch() {
    let f = fixture();
    f.tell(
        "legacy-pending",
        "Resolve this old inbox claim without replaying it",
    );
    let message = f.executor.pending().unwrap()[0].id;
    let epoch = f.domain().read().state().data_epoch.clone();
    let claim = {
        let mut store = f.domain().store.lock().unwrap();
        let claim = store.claim_message(message).unwrap().unwrap();
        store
            .bind_batch_effects("legacy-sessionless", &epoch)
            .unwrap();
        claim
    };
    let run = Run {
        id: "legacy-sessionless".into(),
        message_ids: vec![claim.message.id],
        attempts: vec![(claim.attempt_id, claim.version)],
        started_at: 1,
        spawned: true,
    };
    save(
        &f.executor.home,
        &Runtime {
            active_run: Some(run.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let restarted = Executor {
        domain: Arc::new(DomainRuntime::open(&f.executor.home).unwrap()),
        home: f.executor.home.clone(),
        claude_home: f.executor.claude_home.clone(),
    };
    let recovered = restarted.startup_recovery().await.unwrap();
    assert!(recovered.active_run.is_none());
    assert_eq!(
        recovered.pending_resolutions,
        vec![Resolution {
            run_id: run.id,
            message_ids: run.message_ids,
        }]
    );
    assert!(restarted.pending().unwrap().is_empty());
    let attempts = restarted
        .domain
        .store
        .lock()
        .unwrap()
        .delivery_evidence_page(0)
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].state, "discarded");
    assert_eq!(attempts[0].current_state, "discarded");
    assert!(f.starts().is_empty());
}

fn ambiguous_batch(f: &Fixture, messages: bool) -> Run {
    if messages {
        f.tell("decision-first", "First pending message");
        f.tell("decision-second", "Second pending message");
    }
    let epoch = f.domain().read().state().data_epoch.clone();
    let selection =
        crate::orchestrator_provider::selection(f.domain(), &f.executor.home, now_millis())
            .unwrap();
    let ids: Vec<_> = f
        .executor
        .pending()
        .unwrap()
        .iter()
        .map(|message| message.id)
        .collect();
    let mut store = f.domain().store.lock().unwrap();
    let (claims, owner) = store
        .claim_selected_orchestrator_batch(
            &ids,
            "user-decision",
            "decision-session",
            epoch,
            Some(&selection),
        )
        .unwrap();
    drop(owner);
    for claim in &claims {
        store
            .finish_message(
                claim.attempt_id,
                claim.version,
                State::Uncertain,
                Some("interrupted"),
            )
            .unwrap();
    }
    let run = Run {
        id: "user-decision".into(),
        message_ids: claims.iter().map(|claim| claim.message.id).collect(),
        attempts: claims
            .iter()
            .map(|claim| (claim.attempt_id, claim.version))
            .collect(),
        started_at: 0,
        spawned: true,
    };
    drop(store);
    save(
        &f.executor.home,
        &Runtime {
            active_run: Some(run.clone()),
            last_error: Some("The batch stopped after an unconfirmed operation".into()),
            failure: Some(Failure::Interrupted),
            cancel_requested: true,
            pending_resolutions: if messages {
                vec![]
            } else {
                vec![Resolution {
                    run_id: "prior-resolution".into(),
                    message_ids: vec![],
                }]
            },
            ..Default::default()
        },
    )
    .unwrap();
    f.executor.remember_stop(
        &run,
        BatchProblem {
            stop: BatchStop::Interrupted,
            calls: vec![],
            subjects: vec![],
            error: "The pending stop must yield to the user's decision".into(),
        },
    );
    run
}

#[tokio::test]
async fn completed_batch_claims_are_handed_after_restart_before_delivery_commit() {
    let f = fixture();
    f.tell("completed-before-restart", "Complete this message");
    let fault = faults::Injection::at(&f.executor.home, "delivery_commit", 1);
    assert!(f.executor.tick().await.is_err());
    assert_eq!(fault.hits(), 1);
    drop(fault);
    let run = load(&f.executor.home).unwrap().active_run.unwrap();
    let domain = Arc::new(DomainRuntime::open(&f.executor.home).unwrap());
    assert!(
        domain
            .store
            .lock()
            .unwrap()
            .delivery_evidence_page(0)
            .unwrap()
            .iter()
            .any(|attempt| run.message_ids.contains(&attempt.message_id)
                && attempt.state == "uncertain")
    );
    let restarted = Executor {
        domain,
        home: f.executor.home.clone(),
        claude_home: f.executor.claude_home.clone(),
    };
    restarted.repair_incomplete().await.unwrap();
    let attempts = restarted
        .domain
        .store
        .lock()
        .unwrap()
        .delivery_evidence_page(0)
        .unwrap();
    assert!(
        attempts
            .iter()
            .filter(|attempt| run.message_ids.contains(&attempt.message_id))
            .all(|attempt| attempt.state == "handed" && attempt.current_state == "handed")
    );
    let runtime = load(&f.executor.home).unwrap();
    assert!(
        runtime.active_run.is_none()
            && runtime.pending_resolutions.is_empty()
            && runtime.batch_handoffs.is_empty()
    );
    assert_eq!(
        f.starts().len(),
        1,
        "completion repair cannot repeat a completed invocation"
    );
}

#[tokio::test]
async fn explicit_decisions_finish_after_runtime_save_failure_without_a_second_user_action() {
    for decision in [Decision::Acknowledge, Decision::Retry, Decision::Cancel] {
        for messages in [true, false] {
            let f = fixture();
            let run = ambiguous_batch(&f, messages);
            let fault = faults::Injection::at(&f.executor.home, "runtime_write", 1);
            let response = decide(
                &f.executor,
                decision,
                Some(&serde_json::json!({"run_id":run.id})),
            );
            assert_eq!(response["status"], 409, "{decision:?}/{messages}");
            assert_eq!(fault.hits(), 1);
            assert!(
                f.domain().live.orchestrator_stop.lock().unwrap().is_none(),
                "{decision:?}/{messages}"
            );
            drop(fault);
            assert!(load(&f.executor.home).unwrap().active_run.is_some());
            // A fresh runtime proves that the decision survives loss of all in-memory state.
            let restarted = Executor {
                domain: Arc::new(DomainRuntime::open(&f.executor.home).unwrap()),
                home: f.executor.home.clone(),
                claude_home: f.executor.claude_home.clone(),
            };
            restarted.repair_incomplete().await.unwrap();
            let runtime = load(&f.executor.home).unwrap();
            assert!(
                runtime.active_run.is_none()
                    && runtime.last_error.is_none()
                    && !runtime.cancel_requested,
                "{decision:?}/{messages}: {runtime:?}"
            );
            let retry = decision == Decision::Retry;
            let target = if retry { "retryable" } else { "discarded" };
            let attempts = restarted
                .domain
                .store
                .lock()
                .unwrap()
                .delivery_evidence_page(0)
                .unwrap();
            assert!(
                attempts
                    .iter()
                    .filter(|attempt| run.message_ids.contains(&attempt.message_id))
                    .all(|attempt| attempt.state == target && attempt.current_state == target),
                "{decision:?}/{messages}"
            );
            assert_eq!(
                runtime
                    .pending_resolutions
                    .iter()
                    .filter(|resolution| resolution.run_id == run.id)
                    .count(),
                usize::from(!retry)
            );
            assert_eq!(
                restarted.pending().unwrap().len(),
                if retry && messages { 2 } else { 0 }
            );
            assert!(f.starts().is_empty(), "repair must not invoke the provider");
        }
    }
}

#[tokio::test]
async fn failed_explicit_decision_commit_leaves_every_claim_uncertain() {
    let f = fixture();
    let run = ambiguous_batch(&f, true);
    let fault = faults::Injection::at(&f.executor.home, "delivery_commit", 1);
    assert!(f.executor.retry(&run.id).is_err());
    drop(fault);
    assert!(f.domain().live.orchestrator_stop.lock().unwrap().is_some());
    let attempts = f
        .domain()
        .store
        .lock()
        .unwrap()
        .delivery_evidence_page(0)
        .unwrap();
    assert!(
        attempts
            .iter()
            .filter(|attempt| run.message_ids.contains(&attempt.message_id))
            .all(|attempt| attempt.state == "uncertain" && attempt.current_state == "uncertain")
    );
    let epoch = f.domain().read().state().data_epoch.clone();
    let batches = f
        .domain()
        .store
        .lock()
        .unwrap()
        .unfinished_orchestrator_batches(&epoch, Some(&run.id), None)
        .unwrap();
    assert!(
        batches
            .iter()
            .find(|batch| batch.run == run.id)
            .unwrap()
            .decision
            .is_none()
    );
}

#[tokio::test]
async fn settled_launch_and_limit_failures_stay_automatic_after_restart() {
    for marker in ["fixture:exit-before-init", "fixture:limited"] {
        let f = fixture();
        f.tell(
            "automatic",
            "Keep this request eligible for automatic retry",
        );
        f.mark_once(marker);
        f.tick().await;
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none());
        let old_run = runtime.last_run_id.unwrap();
        let domain = Arc::new(DomainRuntime::open(&f.executor.home).unwrap());
        domain.enable_sdk(f.domain().sdk().unwrap().0.clone());
        let restarted = Executor {
            domain,
            home: f.executor.home.clone(),
            claude_home: f.executor.claude_home.clone(),
        };
        restarted.repair_incomplete().await.unwrap();
        assert!(load(&f.executor.home).unwrap().active_run.is_none());
        assert!(restarted.pending().unwrap().len() > 0);
        assert!(f.cards(PROBLEM_PREFIX).is_empty());
        assert!(
            !restarted
                .domain
                .read()
                .state()
                .interactions
                .keys()
                .any(|id| id.0.starts_with(PROBLEM_PREFIX))
        );
        if marker == "fixture:exit-before-init" {
            let mut runtime = load(&f.executor.home).unwrap();
            runtime.retry_at = None;
            save(&f.executor.home, &runtime).unwrap();
            restarted.tick().await.unwrap();
            assert!(restarted.domain.read().state().requests[&"automatic".into()].closed);
            rusqlite::Connection::open(crate::domain_database_migration::active_path(
                &f.executor.home,
            ))
            .unwrap()
            .execute(
                "UPDATE orchestrator_batch_runs SET ended=0 WHERE run=?1",
                [&old_run],
            )
            .unwrap();
            let history = Executor {
                domain: Arc::new(DomainRuntime::open(&f.executor.home).unwrap()),
                home: f.executor.home.clone(),
                claude_home: f.executor.claude_home.clone(),
            };
            history.repair_incomplete().await.unwrap();
            assert!(load(&f.executor.home).unwrap().active_run.is_none());
            assert_eq!(
                f.starts().len(),
                2,
                "settled history cannot resurrect a batch"
            );
        }
    }
}

#[tokio::test]
async fn a_partially_written_launch_failure_keeps_its_automatic_disposition_after_restart() {
    let f = fixture();
    f.tell("first", "Retry the first request automatically");
    f.tell("second", "Retry the second request automatically");
    f.mark_once("fixture:exit-before-init");
    let fault = faults::Injection::at(&f.executor.home, "delivery_finish", 2);
    assert!(f.executor.tick().await.is_err());
    assert!(fault.hits() > 0);
    drop(fault);
    let mut runtime = load(&f.executor.home).unwrap();
    runtime.last_error = Some("A launch error retained by an older daemon".into());
    runtime.failure = Some(Failure::Launch);
    save(&f.executor.home, &runtime).unwrap();
    let restarted = Executor {
        domain: Arc::new(DomainRuntime::open(&f.executor.home).unwrap()),
        home: f.executor.home.clone(),
        claude_home: f.executor.claude_home.clone(),
    };
    restarted.repair_incomplete().await.unwrap();
    assert!(load(&f.executor.home).unwrap().active_run.is_none());
    assert_eq!(restarted.pending().unwrap().len(), 2);
    assert!(
        !restarted
            .domain
            .read()
            .state()
            .interactions
            .keys()
            .any(|id| id.0.starts_with(PROBLEM_PREFIX))
    );
}

#[tokio::test]
async fn decision_routes_cannot_settle_or_reoffer_a_live_provider() {
    let f = fixture();
    f.tell(
        "running",
        "Keep this claimed until the real provider finishes",
    );
    f.mark_once("fixture:quiet-hold");
    let during = async {
        f.held().await;
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        let mut decisions = Vec::new();
        for decision in [Decision::Acknowledge, Decision::Retry] {
            let response = decide(
                &f.executor,
                decision,
                Some(&serde_json::json!({"run_id":run.id})),
            );
            let retained = load(&f.executor.home)
                .unwrap()
                .active_run
                .is_some_and(|active| active.id == run.id);
            decisions.push((response["status"].clone(), retained, f.attempts()));
        }
        f.release();
        decisions
    };
    let (result, decisions) = tokio::join!(f.executor.tick(), during);
    result.unwrap();
    for (status, retained, attempts) in decisions {
        assert_eq!(status, 409);
        assert!(retained && attempts.iter().all(|state| state == "dispatching"));
    }
    assert!(load(&f.executor.home).unwrap().active_run.is_none());
    assert_eq!(f.starts().len(), 1);
}
