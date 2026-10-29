use super::*;
use std::time::Instant;

#[test]
fn watchdog_measures_progress_and_fires_once_at_five_minutes() {
    let mut watchdog = Watchdog::default();
    let start = Instant::now();
    assert!(!watchdog.observe(start, true, false, 1));
    assert!(!watchdog.observe(
        start + STALLED_AFTER - Duration::from_millis(1),
        true,
        false,
        1
    ));
    assert!(watchdog.observe(start + STALLED_AFTER, true, false, 1));
    assert!(!watchdog.observe(start + STALLED_AFTER * 2, true, false, 1));
    assert!(!watchdog.observe(start + STALLED_AFTER * 2, true, false, 2));
    assert!(!watchdog.observe(
        start + STALLED_AFTER * 3 - Duration::from_millis(1),
        true,
        false,
        2
    ));
    assert!(watchdog.observe(start + STALLED_AFTER * 3, true, false, 2));
}

#[test]
fn watchdog_waits_for_account_and_usage_and_rearms_after_a_pause() {
    for suspended in [true, false] {
        let mut watchdog = Watchdog::default();
        let start = Instant::now();
        assert!(!watchdog.observe(start, true, false, 0));
        assert!(!watchdog.observe(start + STALLED_AFTER, suspended, suspended, 0));
        assert!(!watchdog.observe(start + STALLED_AFTER * 2, true, false, 0));
        assert!(watchdog.observe(start + STALLED_AFTER * 3, true, false, 0));
    }
}

#[test]
fn diagnostic_chatter_and_unrelated_domain_activity_do_not_mask_a_stall() {
    use crate::domain_orchestrator::testing::fixture;
    use crate::domain_store::batch_effects::BatchReport;
    let f = fixture();
    f.tell("pending", "A request still needs processing");
    let epoch = f.domain().read().state().data_epoch.clone();
    let (_, owner) = f
        .domain()
        .store
        .lock()
        .unwrap()
        .claim_orchestrator_batch(&[], "chatter", "chatter-session", epoch)
        .unwrap();
    drop(owner);
    let progress = f
        .domain()
        .live
        .orchestrator_progress
        .load(Ordering::Relaxed);
    f.domain()
        .publish_batch_report(
            "chatter",
            1,
            b"first diagnostic",
            &BatchReport {
                kind: "diagnostic".into(),
                ..Default::default()
            },
        )
        .unwrap();
    f.tell("unrelated", "Another user request");
    f.domain()
        .publish_batch_report(
            "chatter",
            2,
            b"second diagnostic",
            &BatchReport {
                kind: "diagnostic".into(),
                ..Default::default()
            },
        )
        .unwrap();
    f.domain()
        .publish_batch_report(
            "chatter",
            3,
            b"system keepalive",
            &BatchReport {
                kind: "system".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        f.domain()
            .live
            .orchestrator_progress
            .load(Ordering::Relaxed),
        progress
    );
    let start = Instant::now();
    let mut watchdog = Watchdog::default();
    assert!(!f.executor.watchdog_due(&mut watchdog, start));
    assert!(
        f.executor
            .watchdog_due(&mut watchdog, start + STALLED_AFTER)
    );
    assert!(f.cards(PROBLEM_PREFIX).is_empty() && f.cards(STUCK_PROBLEM_PREFIX).is_empty());
}

type DrainSender = tokio::sync::oneshot::Sender<()>;
static DRAINS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<String, DrainSender>>,
> = std::sync::LazyLock::new(Default::default);

pub(crate) fn draining(run: &str) {
    if let Some(sender) = DRAINS.lock().unwrap().remove(run) {
        let _ = sender.send(());
    }
}

struct DrainNotice(String);
impl Drop for DrainNotice {
    fn drop(&mut self) {
        DRAINS.lock().unwrap().remove(&self.0);
    }
}
fn on_drain(run: &str) -> (DrainNotice, tokio::sync::oneshot::Receiver<()>) {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    assert!(DRAINS.lock().unwrap().insert(run.into(), sender).is_none());
    (DrainNotice(run.into()), receiver)
}

struct Clock(tokio::sync::mpsc::UnboundedSender<MonitorCheck>);
impl Clock {
    async fn check(&self, now: Instant) {
        let (observed, done) = tokio::sync::oneshot::channel();
        self.0
            .send(MonitorCheck {
                now,
                observed: Some(observed),
            })
            .unwrap();
        done.await.unwrap();
    }
}
fn monitored() -> (Watchdog, Clock) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    (
        Watchdog {
            checks: Some(receiver),
            ..Default::default()
        },
        Clock(sender),
    )
}
fn executor(f: &crate::domain_orchestrator::testing::Fixture) -> Arc<Executor> {
    Arc::new(Executor {
        domain: Arc::clone(&f.executor.domain),
        home: f.executor.home.clone(),
        claude_home: f.executor.claude_home.clone(),
    })
}

fn legacy_run(f: &crate::domain_orchestrator::testing::Fixture) -> Run {
    f.tell(
        "legacy-pending",
        "Recover this legacy claim without replaying it",
    );
    let message = f.executor.pending().unwrap()[0].id;
    let epoch = f.domain().read().state().data_epoch.clone();
    let claim = {
        let mut store = f.domain().store.lock().unwrap();
        let claim = store.claim_message(message).unwrap().unwrap();
        store.bind_batch_effects("legacy-watchdog", &epoch).unwrap();
        claim
    };
    let run = Run {
        id: "legacy-watchdog".into(),
        message_ids: vec![claim.message.id],
        attempts: vec![(claim.attempt_id, claim.version)],
        started_at: 0,
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
    run
}

#[tokio::test]
async fn watchdog_restart_recovers_a_legacy_run_instead_of_repeating_its_idle_guard() {
    use crate::domain_orchestrator::testing::fixture;
    let f = fixture();
    let run = legacy_run(&f);
    let start = Instant::now();
    let mut watchdog = Watchdog {
        progress: f
            .domain()
            .live
            .orchestrator_progress
            .load(Ordering::Relaxed),
        since: Some(start),
        ..Default::default()
    };
    executor(&f)
        .supervised_tick_at(&mut watchdog, start + STALLED_AFTER)
        .await
        .unwrap();
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_none());
    assert_eq!(
        runtime.pending_resolutions,
        vec![Resolution {
            run_id: run.id,
            message_ids: run.message_ids
        }]
    );
    assert_eq!(f.attempts(), vec!["discarded"]);
    assert!(f.starts().is_empty());
    assert!(f.cards(PROBLEM_PREFIX).is_empty() && f.cards(STUCK_PROBLEM_PREFIX).is_empty());
    f.tell("after-watchdog", "Process the next request");
    f.tick().await;
    assert_eq!(f.starts().len(), 1);
    assert!(f.domain().read().state().requests[&"after-watchdog".into()].closed);
}

#[tokio::test]
async fn a_watchdog_retries_legacy_recovery_after_its_runtime_write_fails() {
    use crate::domain_orchestrator::testing::fixture;
    for occurrence in [1, 2] {
        let f = fixture();
        let run = legacy_run(&f);
        let start = Instant::now();
        let mut watchdog = Watchdog {
            progress: f
                .domain()
                .live
                .orchestrator_progress
                .load(Ordering::Relaxed),
            since: Some(start),
            ..Default::default()
        };
        let executor = executor(&f);
        let fault = faults::Injection::at(&f.executor.home, "runtime_write", occurrence);
        for attempt in 1..=2 {
            let error = executor
                .supervised_tick_at(
                    &mut watchdog,
                    start + STALLED_AFTER + Duration::from_secs((attempt - 1) * 10),
                )
                .await
                .unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<std::io::Error>()
                    .unwrap()
                    .raw_os_error(),
                Some(libc::ENOSPC)
            );
            assert_eq!(fault.hits(), attempt as usize);
            assert!(
                watchdog.restart_pending,
                "a failed recovery remains scheduled"
            );
            assert_eq!(f.attempts(), vec!["discarded"]);
            assert_eq!(
                load(&f.executor.home).unwrap().active_run,
                (occurrence == 1).then(|| run.clone())
            );
            assert!(f.starts().is_empty());
        }
        drop(fault);

        executor
            .supervised_tick_at(
                &mut watchdog,
                start + STALLED_AFTER + Duration::from_secs(20),
            )
            .await
            .unwrap();
        assert!(!watchdog.restart_pending);
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none());
        assert_eq!(
            runtime.pending_resolutions,
            vec![Resolution {
                run_id: run.id,
                message_ids: run.message_ids
            }]
        );
        assert_eq!(f.attempts(), vec!["discarded"]);
        assert!(
            f.starts().is_empty(),
            "pending recovery finishes before a replacement starts"
        );
        assert!(f.cards(PROBLEM_PREFIX).is_empty() && f.cards(STUCK_PROBLEM_PREFIX).is_empty());

        f.tell("after-recovery-write", "Process this once storage returns");
        executor.supervised_tick(&mut watchdog).await.unwrap();
        assert_eq!(f.starts().len(), 1);
        assert!(f.domain().read().state().requests[&"after-recovery-write".into()].closed);
        assert!(f.cards(PROBLEM_PREFIX).is_empty() && f.cards(STUCK_PROBLEM_PREFIX).is_empty());
    }
}

fn quiet_provider(f: &crate::domain_orchestrator::testing::Fixture, ignore_term: bool) {
    let native = f.root.join("orchestrator-fake-claude.py");
    let mut source = std::fs::read_to_string(&native).unwrap();
    let before_events = "    for state in ('queued', 'started'):\n";
    assert!(source.contains(before_events));
    source = source.replace(before_events, &format!("    if 'fixture:quiet-hold' in text:\n        hold(os.environ['ALONDRA_DOMAIN_CAPABILITY'])\n{before_events}"));
    if ignore_term {
        assert!(source.contains("import time"));
        source = source.replace(
            "import time",
            "import time\nimport signal\nsignal.signal(signal.SIGTERM, signal.SIG_IGN)",
        );
    }
    std::fs::write(&native, source).unwrap();
    f.mark_once("fixture:quiet-hold");
}

#[tokio::test]
async fn a_watchdog_cancels_a_held_provider_and_the_next_request_runs_without_a_problem() {
    use crate::domain_orchestrator::testing::fixture;
    let f = fixture();
    f.tell("held", "Wait before replying");
    quiet_provider(&f, true);
    let (mut watchdog, clock) = monitored();
    let executor = executor(&f);
    let running = tokio::spawn(async move { executor.supervised_tick(&mut watchdog).await });
    f.held().await;
    let run = load(&f.executor.home).unwrap().active_run.unwrap().id;
    let (_notice, drained) = on_drain(&run);
    let stop = f
        .domain()
        .live
        .orchestrator_host
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .1
        .clone();
    let start = Instant::now();
    clock.check(start).await;
    clock
        .check(start + STALLED_AFTER - Duration::from_millis(1))
        .await;
    assert!(
        stop.process_group().is_some(),
        "a live host remains owned before the stall threshold"
    );
    clock.check(start + STALLED_AFTER).await;
    drained.await.unwrap();
    running.await.unwrap().unwrap();
    assert!(
        stop.process_group().is_none(),
        "the provider worker reaped its host before recovery"
    );
    assert!(
        !f.executor
            .home
            .join("runtime/orchestrator/owner.json")
            .exists()
    );
    assert!(f.domain().live.orchestrator_host.lock().unwrap().is_none());
    assert!(load(&f.executor.home).unwrap().active_run.is_none());
    assert!(f.cards(PROBLEM_PREFIX).is_empty());
    f.tell("fresh", "Process the next request");
    f.tick().await;
    assert_eq!(
        f.starts().len(),
        2,
        "the joined provider is followed by exactly one fresh host"
    );
    assert!(f.domain().read().state().requests[&"fresh".into()].closed);
    assert!(f.cards(PROBLEM_PREFIX).is_empty() && f.cards(STUCK_PROBLEM_PREFIX).is_empty());
}

#[tokio::test]
async fn watchdog_keeps_monitoring_until_an_admitted_physical_writer_finishes() {
    use crate::domain_orchestrator::testing::fixture;
    use std::sync::atomic::AtomicUsize;
    let f = fixture();
    f.tell("held", "Wait while an admitted operation completes");
    quiet_provider(&f, false);
    let (mut watchdog, clock) = monitored();
    let executor = executor(&f);
    let running = tokio::spawn(async move { executor.supervised_tick(&mut watchdog).await });
    f.held().await;
    let run = load(&f.executor.home).unwrap().active_run.unwrap().id;
    let (_notice, drained) = on_drain(&run);
    let stop = f
        .domain()
        .live
        .orchestrator_host
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .1
        .clone();
    let domain = Arc::clone(&f.executor.domain);
    let bearer = f.starts().into_iter().next().unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    let effect_count = Arc::clone(&effects);
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, barrier) = tokio::sync::oneshot::channel();
    let physical = tokio::spawn(async move {
        crate::domain_runtime::batch_operations::execute(
            domain,
            &bearer,
            Some("abcd"),
            "launch",
            serde_json::json!({"task":"held"}),
            async move {
                effect_count.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                barrier.await.unwrap();
                Ok(serde_json::json!({"node":"the-one-physical-effect"}))
            },
        )
        .await
    });
    started.await.unwrap();
    let start = Instant::now();
    clock.check(start).await;
    clock.check(start + STALLED_AFTER).await;
    drained.await.unwrap();
    assert!(
        stop.process_group().is_none(),
        "the host is reaped before admitted operations drain"
    );
    assert!(f.domain().live.orchestrator_host.lock().unwrap().is_none());
    f.tell("fresh", "Run after the old writer has finished");
    clock.check(start + STALLED_AFTER * 2).await;
    assert!(
        !running.is_finished(),
        "restarting cannot detach an admitted physical writer"
    );
    assert_eq!(
        f.starts().len(),
        1,
        "no fresh host starts while the old writer is live"
    );
    assert!(load(&f.executor.home).unwrap().active_run.is_some());
    release.send(()).unwrap();
    assert_eq!(
        physical.await.unwrap().unwrap(),
        serde_json::json!({"node":"the-one-physical-effect"})
    );
    running.await.unwrap().unwrap();
    assert!(
        f.domain()
            .confirmed_batch_effects(&run, &["launch".into()])
            .unwrap()
    );
    f.tick().await;
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(f.starts().len(), 2);
    assert!(f.domain().read().state().requests[&"fresh".into()].closed);
    assert!(load(&f.executor.home).unwrap().active_run.is_none());
    assert!(f.cards(PROBLEM_PREFIX).is_empty() && f.cards(STUCK_PROBLEM_PREFIX).is_empty());
}

#[test]
fn account_usage_and_explicit_user_waits_do_not_trigger_the_watchdog() {
    use crate::domain_orchestrator::testing::fixture;
    use alondra_core::domain::{CardKind, CardSpec};
    let f = fixture();
    f.tell("pending", "A request awaiting a legitimate dependency");
    let start = Instant::now();
    let mut watchdog = Watchdog::default();
    assert!(!f.executor.watchdog_due(&mut watchdog, start));
    f.domain()
        .live
        .account_wait
        .wait_for("Sign in to the selected account");
    assert!(
        !f.executor
            .watchdog_due(&mut watchdog, start + STALLED_AFTER * 2)
    );
    f.domain().live.account_wait.end();
    f.domain().live.usage_pause.exhaust(Some(1_000), 0);
    assert!(
        !f.executor
            .watchdog_due(&mut watchdog, start + STALLED_AFTER * 4)
    );
    f.domain().live.usage_pause.resume(1_000);
    f.domain().live.usage_pause.pause_by_user(2_000, 1_000);
    assert!(
        !f.executor
            .watchdog_due(&mut watchdog, start + STALLED_AFTER * 6)
    );
    f.domain().live.usage_pause.resume(2_000);
    let mut runtime = load(&f.executor.home).unwrap();
    runtime.active_run = Some(Run {
        id: "explicit-decision".into(),
        message_ids: vec![],
        attempts: vec![],
        started_at: 0,
        spawned: true,
    });
    runtime.last_error = Some("Reconcile the earlier operation before continuing".into());
    save(&f.executor.home, &runtime).unwrap();
    assert!(
        !f.executor
            .watchdog_due(&mut watchdog, start + STALLED_AFTER * 8)
    );
    runtime.failure = Some(Failure::Launch);
    save(&f.executor.home, &runtime).unwrap();
    let mut automatic = Watchdog::default();
    assert!(!f.executor.watchdog_due(&mut automatic, start));
    assert!(
        f.executor
            .watchdog_due(&mut automatic, start + STALLED_AFTER)
    );
    runtime.active_run = None;
    runtime.last_error = None;
    save(&f.executor.home, &runtime).unwrap();
    f.run(
        Actor::Orchestrator { generation: 0 },
        "question",
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "question".into(),
            scope: Scope::Orchestrator {
                request_id: Some("pending".into()),
            },
            recipient: Recipient::User,
            body: "Choose how to proceed".into(),
            blocking: true,
            options: vec![
                QuestionOption {
                    id: "continue".into(),
                    label: "Continue".into(),
                    description: None,
                },
                QuestionOption {
                    id: "stop".into(),
                    label: "Stop".into(),
                    description: None,
                },
            ],
            card: CardSpec::derived(CardKind::Decide, "Choose how to proceed"),
        },
    );
    assert!(f.executor.waiting_requests().is_empty());
    assert!(
        !f.executor
            .watchdog_due(&mut watchdog, start + STALLED_AFTER * 10)
    );
    assert!(f.cards(PROBLEM_PREFIX).is_empty() && f.cards(STUCK_PROBLEM_PREFIX).is_empty());
}
