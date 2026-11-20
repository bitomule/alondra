//! Owner transactions of SDK hosts on the real domain database, with the whole engine.
use alondra_store_sqlite::AsSqlite as _;
use crate::sdk_owner::SdkOwners as _;
use crate::domain_store::*;
use alondra_domain_rules::*;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use alondra_agent_application::{InputMetadata, ObservationNormalizer, ObservedFact};
use alondra_domain::{RuntimeFact, TaskLifecycle, Trigger};
use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
use alondra_provider_contract::{McpServerReport, McpSignInReport, OrderedObservation, PreparedCommand, ProviderBinding, TurnOutcome};
use alondra_store::{
    AdapterState, AuthorityScope, CommandEvidence, Committed, CommittedCommand, CommittedControl, ControlAttempt,
    ControlPermit, DispatchAttempt, DispatchPermit, DispatchReceipt, LifecycleTransaction,
    ObservationAcceptance, OwnerGrant, UnresolvedCommand, UnresolvedControl,
};
use alondra_engine_ports::journal::owner::ObservationRefusal;
use crate::domain_runtime::DomainRuntime;
use alondra_store_sqlite::tests::{Fixture, seeded};
use alondra_agent_application::{Normalization, ObservationMetadata};
use alondra_foundation::{ContinuationId, ExecutionStamp, InputId, TurnId, WorkId};
use alondra_store::InputPayload;
use alondra_provider_contract::{AdapterError, Capability, Observation, Placement};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc;
use alondra_daemon_engine::wiring::owner_tests::*;

#[test]
fn card_raw_observation_journal_and_checkpoint_commit_together_and_publish_before_acknowledging() {
    super::wired();
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let before = counts(&observer);
    let state = Arc::clone(runtime.read().state());
    let mut changes = runtime.subscribe();
    let ((acceptance, inside), outcomes) = runtime
        .own(scope_of(&state), |owner| {
            let acceptance = owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
            owner.stage_command(&agent(), &envelope(&state, "owner-ask", ask("owner-card")), Attached::None)?;
            Ok((acceptance, counts(&observer)))
        })
        .unwrap();
    assert_eq!(acceptance, ObservationAcceptance::Accepted);
    assert_eq!(inside, before, "another connection sees nothing of the owner transaction before its commit");
    let after = counts(&observer);
    for (index, table) in TABLES.iter().enumerate().filter(|(_, table)| **table != "lifecycle_inputs") {
        assert!(after[index] > before[index], "{table} committed with the rest of the transaction");
    }
    let acknowledged = runtime.take_acknowledgements();
    assert_eq!(acknowledged.len(), 1);
    assert_eq!(acknowledged[0].observations.len(), 1);
    assert!(acknowledged[0].observations[0].matches_evidence(b"raw observation 1"));
    let published = runtime.read();
    assert_eq!(outcomes.len(), 2, "the observation's facts and the card: two domain commands");
    assert!(matches!(outcomes[0].command_id.0.as_str(), id if id.starts_with("observation:")));
    let last = outcomes.last().unwrap();
    assert_eq!(published.state().revision, last.outcome.revision);
    assert!(published.state().interactions.contains_key(&InteractionId::from("owner-card")));
    assert!(published.state().tasks[&task()].started, "the observed resource starts the task through its journal");
    assert!(changes.has_changed().unwrap(), "the change is notified");
    assert_eq!(*changes.borrow_and_update(), last.outcome.revision);
}

#[test]
fn a_deferred_commit_failure_issues_no_seal_publication_or_cache() {
    super::wired();
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    observer.execute_batch("CREATE TABLE composite_parent(id INTEGER PRIMARY KEY); CREATE TABLE composite_child(parent INTEGER REFERENCES composite_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_composite_commit AFTER INSERT ON interactions WHEN NEW.id='owner-card' BEGIN INSERT INTO composite_child VALUES(404); END").unwrap();
    let before = counts(&observer);
    let read_before = runtime.read();
    let state = Arc::clone(read_before.state());
    let error = runtime
        .own(scope_of(&state), |owner| {
            owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
            owner.stage_command(&agent(), &envelope(&state, "owner-ask", ask("owner-card")), Attached::None).map(|_| ())
        })
        .unwrap_err();
    match error {
        StoreError::Storage(error) => assert!(
            matches!(error.downcast_ref::<alondra_store::Error>(),
                Some(alondra_store::Error::Database(rusqlite::Error::SqliteFailure(code, _)))
                    if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
            "{error:?}"
        ),
        other => panic!("expected the deferred foreign key failure, got {other:?}"),
    }
    assert_eq!(counts(&observer), before);
    assert!(Arc::ptr_eq(runtime.read().state(), read_before.state()), "nothing is published");
    assert!(runtime.take_acknowledgements().is_empty(), "no seal leaves a failed commit");
    let store = runtime.store.lock().unwrap();
    assert!(Arc::ptr_eq(store.sqlite().cached_state().unwrap().0, read_before.state()), "the cache keeps the committed state");
    assert!(store.sqlite().conn.is_autocommit());
}

#[test]
fn publication_follows_the_commit_and_the_acknowledgement_follows_publication() {
    super::wired();
    let (fixture, store, execution) = running();
    let runtime = Arc::new(runtime(&fixture, store));
    let observer = observer(&fixture);
    let before = counts(&observer);
    let read_before = runtime.read();
    let state = Arc::clone(read_before.state());
    let revocation = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(fixture.0.join("domain-approval.lock"))
        .unwrap();
    let (at_barrier, reached) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let barrier = at_barrier.clone();
    runtime.before_owner_publish(move || {
        barrier.send(None).unwrap();
        released.recv().unwrap();
    });
    let owning = {
        let runtime = Arc::clone(&runtime);
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let result = runtime.own(scope_of(&state), |owner| {
                owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
                owner.stage_command(&agent(), &envelope(&state, "owner-ask", ask("owner-card")), Attached::None).map(|_| ())
            });
            at_barrier.send(Some(format!("{result:?}"))).unwrap();
            result
        })
    };
    if let Some(finished) = reached.recv().unwrap() {
        panic!("the owner transaction never reached publication: {finished}");
    }
    assert_ne!(counts(&observer), before, "the SQL commit happened before publication");
    assert!(Arc::ptr_eq(runtime.read().state(), read_before.state()), "readers still see the state before the commit");
    assert!(runtime.take_acknowledgements().is_empty(), "no acknowledgement while readers see the old state");
    // SAFETY: the descriptor stays owned by `revocation` for the call.
    let locked = unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&revocation), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(
        (locked, std::io::Error::last_os_error().raw_os_error()),
        (-1, Some(libc::EWOULDBLOCK)),
        "a revocation waits until the owner transaction has published"
    );
    let (waiting, blocked) = mpsc::channel();
    runtime.store.on_wait(move || {
        let _ = waiting.send(());
    });
    let competing = {
        let runtime = Arc::clone(&runtime);
        let command = envelope(&state, "competing", Command::RegisterAgent { agent_id: "agent-2".into(), name: "Second".into(), at: 0 });
        std::thread::spawn(move || runtime.execute_as_system(command))
    };
    blocked.recv().unwrap();
    release.send(()).unwrap();
    let (_, outcomes) = owning.join().unwrap().unwrap();
    let acknowledged = runtime.take_acknowledgements();
    assert_eq!(acknowledged.len(), 1);
    assert!(acknowledged[0].observations[0].matches_evidence(b"raw observation 1"));
    let competing = competing.join().unwrap().unwrap();
    assert!(competing.revision > outcomes[0].outcome.revision, "the competing writer committed after the owner published");
    assert_eq!(runtime.read().state().revision, competing.revision);
}

#[test]
fn a_crash_before_publication_acknowledges_nothing_and_the_replay_acknowledges_once() {
    super::wired();
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let observer = observer(&fixture);
    let read_before = runtime.read();
    let state = Arc::clone(read_before.state());
    let command = envelope(&state, "owner-ask", ask("owner-card"));
    let work = |owner: &mut dyn OwnerJournal| {
        let acceptance = owner.stage_observation(&binding(), 1, &batch(&execution, 1), &Normalizer::Available, no_home(), false)?;
        owner.stage_command(&agent(), &command, Attached::None)?;
        Ok(acceptance)
    };
    runtime.before_owner_publish(|| panic!("publication failed after the commit"));
    let crashed = catch_unwind(AssertUnwindSafe(|| runtime.own(scope_of(&state), work)));
    assert!(crashed.is_err());
    assert!(Arc::ptr_eq(runtime.read().state(), read_before.state()), "nothing was published");
    assert!(runtime.take_acknowledgements().is_empty(), "nothing was acknowledged");
    let committed = counts(&observer);
    assert_eq!(card(&observer, "owner-card"), Some((None, false)), "the transaction itself committed");
    drop(runtime);

    let restarted = crate::engine_tests::open_at(&fixture.0, &fixture.path()).unwrap();
    let published = Arc::clone(restarted.read().state());
    assert!(published.interactions.contains_key(&InteractionId::from("owner-card")));
    let (acceptance, outcomes) = restarted.own(scope_of(&published), work).unwrap();
    assert_eq!(acceptance, ObservationAcceptance::Duplicate);
    assert!(outcomes[0].replayed);
    assert_eq!(counts(&observer), committed, "the replay adds no second card, receipt or message");
    let acknowledged = restarted.take_acknowledgements();
    assert_eq!(acknowledged.len(), 1);
    assert!(acknowledged[0].observations[0].matches_evidence(b"raw observation 1"));
    assert_eq!(restarted.read().state().revision, published.revision);
}

#[test]
fn settling_the_case_of_a_stopped_node_tells_the_task_how_to_go_on_in_one_commit() {
    super::wired();
    let (fixture, mut store, execution) = running();
    let normalizer = woken(&mut store, &execution);
    let state = store.load().unwrap();
    store
        .own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, WOKEN_CURSOR), &normalizer, no_home(), false))
        .unwrap();
    let state = store.load().unwrap();
    let instruct = Command::SettleStall {
        case_id: stall_case_of("turn-2"),
        task_id: task(),
        outcome: alondra_domain_rules::StallOutcome::Instruct { text: "Add the tests first".into() },
    };
    let ordinary = store.execute_filling_revision(&orchestrator(), &envelope(&state, "instruct", instruct)).unwrap();
    let permits = &ordinary.sealed.as_ref().unwrap().permits;
    assert_eq!(permits.len(), 1, "the commit hands the owner the dispatch of what the task is told");
    let prepared: String = store.conn
        .query_row("SELECT d.status FROM lifecycle_dispatches d JOIN lifecycle_inputs i USING(task_id,input_id) WHERE d.task_id=?1 AND d.kind='input' AND i.body = 'Add the tests first'", [TASK], |r| r.get(0))
        .unwrap();
    assert_eq!(prepared, "prepared");
    assert!(card(&store.conn, stall_case_of("turn-2").0.as_str()).unwrap().0.is_some(), "the case is settled");
    let held: i64 = store.conn.query_row("SELECT count(*) FROM lifecycle_cards", [], |r| r.get(0)).unwrap();
    assert_eq!(held, 0, "the lifecycle no longer requires it");
    drop(store);
    let mut reopened = alondra_store_sqlite::DomainStore::open(&fixture.path()).unwrap();
    let (pending, _, cards) = journal_after(&mut reopened);
    assert_eq!((pending.len(), cards), (1, 0), "the task waits to be told, with no card left");
}

/// Evidence of the end of a turn the domain already recorded is left out of what it records: the
/// observation that repeats it commits with nothing recorded and is not discarded.
#[test]
fn a_repeated_turn_end_commits_with_nothing_recorded() {
    super::wired();
    let (fixture, runtime, execution, turn) = resumed_with_a_notification_waiting();
    let end = Normalizer::Facts(vec![Observation::TurnEnded { turn, outcome: alondra_provider_contract::TurnOutcome::Completed }]);
    commit(&runtime, &batch(&execution, 5), &end).unwrap();
    let before = cursor_of(&runtime);
    assert_eq!(commit(&runtime, &batch(&execution, 6), &end).expect("the repeated end commits"), ObservationAcceptance::Accepted);
    assert_eq!(cursor_of(&runtime), before, "nothing new was recorded");
    assert!(discarded(&fixture).is_empty(), "and nothing was discarded");
}

/// Production, 2026-11-07 (t-idle-node-release-processes, observation 1342): the provider's
/// adapter refused an observation, and the owner stopped on it at every takeover. Now it is
/// discarded with why: its checkpoint moves past it, and the next observation settles as usual.
#[test]
fn an_observation_the_adapter_refuses_is_discarded_with_why_and_the_next_one_settles() {
    super::wired();
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    assert_eq!(commit(&runtime, &batch(&execution, 1), &Normalizer::Refuses).expect("the refused observation is discarded"), ObservationAcceptance::Accepted);
    assert_eq!(discarded(&fixture), vec![(1, "adapter_storage".to_owned())]);
    assert_eq!(runtime.read().state().lifecycle_of(&task()).unwrap().cursor(), Some(0), "it recorded nothing");
    assert_eq!(runtime.take_acknowledgements().len(), 1, "it is acknowledged, so the host goes on");
    assert_eq!(commit(&runtime, &batch(&execution, 1), &Normalizer::Refuses).unwrap(), ObservationAcceptance::Duplicate);
    assert_eq!(commit(&runtime, &batch(&execution, 2), &Normalizer::Available).unwrap(), ObservationAcceptance::Accepted);
    assert_eq!(cursor_of(&runtime), 1, "the next observation is recorded");
    assert_eq!(discarded(&fixture).len(), 1, "and nothing else is discarded");
}

/// Production, 2026-11-09 (t-asc-analytics-hang): a host kept reporting work of a task already
/// delivered, and the domain refused it each time. Its provider runs that work all the same: it is
/// recorded once, the delivery stays the user's, and nothing is discarded.
#[test]
fn work_observed_after_the_delivery_is_recorded_once() {
    super::wired();
    let (fixture, mut store, execution) = running();
    let input = pending_input(&mut store);
    let runtime = runtime(&fixture, store);
    commit(&runtime, &batch(&execution, 1), &Normalizer::Turn { input, work: false }).unwrap();
    let state = runtime.state().unwrap();
    runtime.execute_fixture(&Actor::User { user_id: "fixture-user".into() }, envelope(&state, "answer-fixture-question", answer())).unwrap();
    let state = runtime.state().unwrap();
    let delivery = Command::PresentResult {
        attachments: vec![], task_id: task(), interaction_id: "delivery".into(), body: "Done".into(), is_final: true, nothing_to_review: false,
        targets: vec![alondra_domain_rules::DeliveryTarget::Text { text: "Done".into() }], checklist: vec![],
        card: alondra_domain_rules::CardSpec::delivery("Review the result", &[], true),
    };
    runtime.execute(&agent(), &CommandEnvelope { expected_revision: state.revision, ..envelope(&state, "deliver", delivery) }, None, || Ok(())).unwrap();
    assert!(runtime.read().state().lifecycle_of(&task()).unwrap().phase() == alondra_domain::TaskPhase::Delivered);
    let work = Normalizer::Facts(vec![Observation::WorkStarted { turn: TurnId::new("turn-1").unwrap(), work: WorkId::new("subagent").unwrap() }]);
    assert_eq!(commit(&runtime, &batch(&execution, 2), &work).unwrap(), ObservationAcceptance::Accepted);
    assert!(discarded(&fixture).is_empty());
    let lifecycle = runtime.read().state().lifecycle_of(&task()).unwrap();
    assert_eq!((lifecycle.phase(), lifecycle.activity()), (alondra_domain::TaskPhase::Delivered, alondra_domain::ActivityView::Working));
    assert_eq!(commit(&runtime, &batch(&execution, 2), &work).unwrap(), ObservationAcceptance::Duplicate);
    assert!(discarded(&fixture).is_empty(), "and it is not recorded again");
}

/// An observation of an owner another one replaced is not the observation's own failure: it is
/// refused as before, and nothing is discarded.
#[test]
fn an_observation_of_a_stale_owner_is_refused_not_discarded() {
    super::wired();
    let (fixture, store, execution) = running();
    let runtime = runtime(&fixture, store);
    let stale = OrderedObservation { owner_generation: 2, ..batch(&execution, 1) };
    let state = Arc::clone(runtime.read().state());
    let refused = crate::sdk_owner::commit_observation(&runtime, &scope_of(&state), &binding(), 2, &stale, &Normalizer::Refuses, no_home(), None);
    assert!(refused.is_err(), "{refused:?}");
    assert!(discarded(&fixture).is_empty());
}

/// Property: whatever sequence of observations a host reports, and whichever of them its adapter
/// refuses, each one ends settled or discarded with why, the checkpoint reaches the last one, and
/// none the adapter read is discarded.
#[test]
fn every_observation_ends_settled_or_discarded_and_none_settleable_is_lost() {
    super::wired();
    use proptest::prelude::*;
    proptest!(ProptestConfig::with_cases(12), |(refused in proptest::collection::vec(any::<bool>(), 1..8))| {
        let (fixture, store, execution) = running();
        let runtime = runtime(&fixture, store);
        for (cursor, refuses) in (1..).zip(&refused) {
            let normalizer = if *refuses { Normalizer::Refuses } else { Normalizer::Facts(vec![]) };
            prop_assert_eq!(commit(&runtime, &batch(&execution, cursor), &normalizer).unwrap(), ObservationAcceptance::Accepted);
        }
        let expected: Vec<i64> = (1..).zip(&refused).filter(|(_, refuses)| **refuses).map(|(cursor, _)| cursor).collect();
        prop_assert_eq!(discarded(&fixture).into_iter().map(|(cursor, _)| cursor).collect::<Vec<_>>(), expected);
        let state = Arc::clone(runtime.read().state());
        let (checkpoint, _) = runtime.own(scope_of(&state), |owner| Ok(owner.normalization_checkpoint(&execution)?.map(|checkpoint| checkpoint.cursor()))).unwrap();
        prop_assert_eq!(checkpoint, Some(refused.len() as u64));
    });
}

fn commit(runtime: &DomainRuntime, batch: &OrderedObservation, normalizer: &Normalizer) -> std::result::Result<ObservationAcceptance, StoreError> {
    let state = Arc::clone(runtime.read().state());
    crate::sdk_owner::commit_observation(runtime, &scope_of(&state), &binding(), 1, batch, normalizer, no_home(), None).map(|(acceptance, _)| acceptance)
}
