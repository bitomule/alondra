use super::*;
use std::future::IntoFuture;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;

fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    (listener, address)
}

fn journal_bytes(path: &Path) -> [u64; 3] {
    ["", "-wal", "-shm"].map(|suffix| {
        std::fs::metadata(format!("{}{suffix}", path.display())).map_or(0, |file| file.len())
    })
}

fn cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    usage.ru_utime.tv_sec as f64
        + usage.ru_utime.tv_usec as f64 / 1_000_000.0
        + usage.ru_stime.tv_sec as f64
        + usage.ru_stime.tv_usec as f64 / 1_000_000.0
}

#[derive(Default)]
struct StageEntry {
    released: Mutex<bool>,
    changed: std::sync::Condvar,
    arrivals: AtomicUsize,
    ready: tokio::sync::Notify,
}
impl StageEntry {
    async fn arrive(&self) {
        if self.arrivals.fetch_add(1, Ordering::SeqCst) == 1 {
            self.ready.notify_waiters();
        }
        loop {
            let notified = self.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let released = *self.released.lock().unwrap();
            if released || self.arrivals.load(Ordering::SeqCst) >= 2 {
                break;
            }
            notified.await;
        }
        tokio::task::yield_now().await;
    }
    fn wait(&self) {
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.changed.wait(released).unwrap();
        }
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
        self.ready.notify_waiters();
    }
}

fn retained_writer_at_transition(stage: ReceiptWrite) {
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    drop(crate::domain_runtime::DomainRuntime::open(&service.home).unwrap());
    let state = crate::domain_bootstrap::DomainDaemon::open(&service.home).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (local_listener, local_address) = listener();
    let (relay_listener, relay_address) = listener();
    let (daemon_listener, daemon_address) = listener();
    service.local_port = local_address.port();
    service.set_relay_url(Url::parse(&format!("http://{relay_address}/")).unwrap());

    let entry = Arc::new(StageEntry::default());
    let (enter, entered) = mpsc::channel();
    let enabled = Arc::new(AtomicBool::new(true));
    service.journal.before_write = Some(Arc::new({
        let entry = entry.clone();
        let enabled = enabled.clone();
        move |observed, id| {
            let (entry, enabled, enter, id) =
                (entry.clone(), enabled.clone(), enter.clone(), id.to_owned());
            Box::pin(async move {
                if observed == stage && enabled.load(Ordering::SeqCst) {
                    entry.arrive().await;
                    enter.send(id).unwrap();
                    entry.wait();
                }
            })
        }
    }));
    let service = Arc::new(service);
    let calls = Arc::new(AtomicUsize::new(0));
    let receipts = Arc::new(AtomicUsize::new(0));
    let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let local = Router::new().route(
        "/v1/options",
        get({
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { axum::Json(json!({"protocol_version":2})) }
            }
        }),
    );
    let relay = Router::new()
        .route(
            "/v1/work/{id}/receipt",
            post({
                let receipts = receipts.clone();
                move || {
                    receipts.fetch_add(1, Ordering::SeqCst);
                    async { StatusCode::ACCEPTED }
                }
            }),
        )
        .route(
            "/v1/work/{id}/response",
            post({
                let responses = responses.clone();
                move |body: Bytes| {
                    responses.lock().unwrap().push(body.to_vec());
                    async { StatusCode::ACCEPTED }
                }
            }),
        );
    let daemon = crate::domain_bootstrap::router(state.clone()).layer(
        axum::middleware::from_fn_with_state(state.auth(), crate::auth::guard),
    );
    let servers = {
        let _entered = runtime.enter();
        [
            runtime.spawn(
                axum::serve(
                    tokio::net::TcpListener::from_std(local_listener).unwrap(),
                    local,
                )
                .into_future(),
            ),
            runtime.spawn(
                axum::serve(
                    tokio::net::TcpListener::from_std(relay_listener).unwrap(),
                    relay,
                )
                .into_future(),
            ),
            runtime.spawn(
                axum::serve(
                    tokio::net::TcpListener::from_std(daemon_listener).unwrap(),
                    daemon.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .into_future(),
            ),
        ]
    };
    let works: Vec<_> = ["transition-one", "transition-two"]
        .into_iter()
        .map(|id| changes_work(&service, id, json!({"method":"GET","path":"/v1/options"})))
        .collect();
    let request_bytes: usize = works
        .iter()
        .map(|work| serde_json::to_vec(&work.envelope).unwrap().len())
        .sum();
    let (complete, completed) = mpsc::channel();
    let mut jobs = Vec::new();
    for work in &works {
        let mut prepared = service
            .prepare_work(work, Transport::Relay)
            .unwrap()
            .unwrap();
        prepared.execution = ExecutionLane::Read;
        let service = service.clone();
        let complete = complete.clone();
        let work = Work {
            request_id: work.request_id.clone(),
            envelope: work.envelope.clone(),
        };
        jobs.push(runtime.spawn(async move {
            let result = service.execute_relay(&work, &[7; 16], prepared).await;
            let error = result.as_ref().err().map(|error| format!("{error:?}"));
            complete.send((work.request_id.clone(), error)).unwrap();
            match result {
                Ok(delivery) => service.deliver_response(work, &[7; 16], delivery).await,
                Err(error) => Err(error),
            }
        }));
    }
    drop(complete);
    let starts = [
        entered.recv_timeout(Duration::from_secs(30)),
        entered.recv_timeout(Duration::from_secs(30)),
    ];
    let before_state: Vec<(String, String)> = {
        let connection = service.journal.db().unwrap();
        let mut query = connection
            .prepare("SELECT request_id,state FROM service_receipts ORDER BY request_id")
            .unwrap();
        query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let before_ack = receipts.load(Ordering::SeqCst);
    let before_calls = calls.load(Ordering::SeqCst);
    let before_bytes = journal_bytes(&service.journal.path);
    let before_cpu = cpu_seconds();
    let mut writer = service.journal.db().unwrap();
    let transaction = writer
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    entry.release();
    let daemon_response = (|| -> std::io::Result<Vec<u8>> {
        let mut connection = TcpStream::connect(daemon_address)?;
        connection.set_read_timeout(Some(Duration::from_secs(30)))?;
        connection.set_write_timeout(Some(Duration::from_secs(30)))?;
        write!(
            connection,
            "GET /daemon HTTP/1.1\r\nHost: {daemon_address}\r\nConnection: close\r\n\r\n"
        )?;
        let mut response = Vec::new();
        connection.read_to_end(&mut response)?;
        Ok(response)
    })();
    let before_health: Vec<_> = completed.try_iter().collect();
    enabled.store(false, Ordering::SeqCst);
    transaction.rollback().unwrap();
    drop(writer);
    let outcomes = runtime.block_on(async {
        let mut outcomes = Vec::new();
        for job in jobs {
            outcomes.push(job.await.unwrap());
        }
        for server in servers {
            server.abort();
            let _ = server.await;
        }
        outcomes
    });
    let final_events: Vec<_> = completed.try_iter().collect();
    let errors = outcomes.iter().filter(|result| result.is_err()).count();
    let results = opened_results(&service, &phone, &responses);
    let response_bytes: usize = responses.lock().unwrap().iter().map(Vec::len).sum();
    let final_bytes = journal_bytes(&service.journal.path);
    let cpu = cpu_seconds() - before_cpu;
    println!(
        "journal_transition stage={stage:?} starts={starts:?} before_state={before_state:?} ack_before={before_ack} calls_before={before_calls} completed_before_health={before_health:?} final_events={final_events:?} errors={errors} acknowledgements={} calls={} replies={} request_body_bytes={request_bytes} response_body_bytes={response_bytes} journal_bytes_before={before_bytes:?} journal_bytes_after={final_bytes:?} process_cpu_seconds={cpu} reservations={} outcomes={outcomes:?}",
        receipts.load(Ordering::SeqCst),
        calls.load(Ordering::SeqCst),
        results.len(),
        service.inflight.lock().unwrap().len()
    );
    let daemon_response = daemon_response.unwrap();
    assert!(daemon_response.starts_with(b"HTTP/1.1 200"));
    assert!(String::from_utf8_lossy(&daemon_response).contains("\"views_protocol\""));
    assert!(
        starts.iter().all(Result::is_ok),
        "both productive transition calls must reach the native entry gate"
    );
    assert_eq!(before_ack, 2);
    assert_eq!(
        before_calls,
        if stage == ReceiptWrite::Complete {
            2
        } else {
            0
        }
    );
    assert!(before_state.iter().all(|(_, state)| state
        == if stage == ReceiptWrite::Complete {
            "executing"
        } else {
            "accepted"
        }));
    assert!(
        before_health.is_empty(),
        "health must finish while the real writer retains both transitions"
    );
    assert_eq!(errors, 0);
    assert_eq!(
        results,
        vec![json!({"status":200,"body":{"protocol_version":2}}); 2]
    );
    assert!(service.inflight.lock().unwrap().is_empty());

    runtime.block_on(async {
        for work in &works {
            let expected: Vec<u8> = service.journal.db().unwrap().query_row(
                "SELECT response FROM service_receipts WHERE sender=?1 AND request_id=?2 AND state='completed'",
                params![work.envelope.sender, work.request_id], |row| row.get(0)).unwrap();
            for _ in 0..2 {
                let prepared = service.prepare_work(work, Transport::Relay).unwrap().unwrap();
                let (bytes, _, guard) = service.execute(&work.envelope, prepared, Transport::Relay, || async { Ok(()) }).await.unwrap();
                assert_eq!(bytes, expected, "same-envelope replay must return exact stored ciphertext");
                drop(guard);
            }
        }
    });
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(service.inflight.lock().unwrap().is_empty());
}

#[test]
fn a_retained_writer_during_executing_keeps_health_serviceable() {
    retained_writer_at_transition(ReceiptWrite::Executing);
}

#[test]
fn a_retained_writer_during_completion_keeps_health_serviceable() {
    retained_writer_at_transition(ReceiptWrite::Complete);
}

fn transition_fixture() -> (
    crate::test_support::Directory,
    Arc<Service>,
    tokio::runtime::Runtime,
) {
    let (directory, service, _) = push_fixture();
    activate_push(&service);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    (directory, Arc::new(service), runtime)
}

fn prepared_ticket(service: &Service, id: &str) -> (Work, Arc<ReceiptAdmission>, [u8; 32]) {
    let work = changes_work(service, id, json!({"method":"GET","path":"/v1/options"}));
    let prepared = service
        .prepare_work(&work, Transport::Relay)
        .unwrap()
        .unwrap();
    let receipt = service.journal.reserve_receipt(prepared.guard).unwrap();
    (work, receipt, prepared.digest)
}

fn drain_sql(journal: &Journal, runtime: &tokio::runtime::Runtime, physical: usize) {
    let permit = runtime
        .block_on(
            journal
                .accept_jobs
                .clone()
                .acquire_many_owned(physical as u32),
        )
        .unwrap();
    drop(permit);
}

#[test]
fn bounded_lifecycles_reject_overflow_before_ack_and_recover_the_same_envelope() {
    let (_directory, service, runtime) = transition_fixture();
    let held: Vec<_> = (0..MAX_JOURNAL_RECEIPTS)
        .map(|index| prepared_ticket(&service, &format!("held-{index}")).1)
        .collect();
    let available = service
        .journal
        .clone()
        .receipt_admissions
        .available_permits();
    service.journal.db().unwrap().execute_batch("CREATE TABLE binding_effects(n INTEGER NOT NULL); INSERT INTO binding_effects VALUES(0); CREATE TRIGGER observe_binding AFTER UPDATE ON service_push_bindings BEGIN UPDATE binding_effects SET n=n+1; END;").unwrap();
    let work = changes_work(
        &service,
        "lifecycle-overflow",
        json!({"method":"POST","path":"/v1/service/push-binding","body":{"push_binding":URL_SAFE_NO_PAD.encode([9;32])}}),
    );
    let acknowledgements = AtomicUsize::new(0);
    let prepared = service
        .prepare_work(&work, Transport::Direct)
        .unwrap()
        .unwrap();
    let rejected =
        runtime.block_on(
            service.execute(&work.envelope, prepared, Transport::Direct, || async {
                acknowledgements.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        );
    let rejected = match rejected {
        Ok((_, _, guard)) => {
            drop(guard);
            false
        }
        Err(_) => true,
    };
    let before_ack = acknowledgements.load(Ordering::SeqCst);
    let before_row: i64 = service
        .journal
        .db()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM service_receipts WHERE request_id='lifecycle-overflow'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let before_effect: i64 = service
        .journal
        .db()
        .unwrap()
        .query_row("SELECT n FROM binding_effects", [], |row| row.get(0))
        .unwrap();
    drop(held);
    let replies = runtime.block_on(async {
        let mut replies = Vec::new();
        for _ in 0..3 {
            let prepared = service
                .prepare_work(&work, Transport::Direct)
                .unwrap()
                .unwrap();
            replies.push(
                service
                    .execute(&work.envelope, prepared, Transport::Direct, || async {
                        acknowledgements.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
                    .map(|(bytes, _, guard)| {
                        drop(guard);
                        bytes
                    }),
            );
        }
        replies
    });
    let effects: i64 = service
        .journal
        .db()
        .unwrap()
        .query_row("SELECT n FROM binding_effects", [], |row| row.get(0))
        .unwrap();
    println!(
        "receipt_lifecycles held={} available={available} rejected={} before_ack={before_ack} before_row={before_row} before_effect={before_effect} effects_after={effects} available_after={} reservations={}",
        MAX_JOURNAL_RECEIPTS,
        rejected,
        service.journal.receipt_admissions.available_permits(),
        service.inflight.lock().unwrap().len()
    );
    assert_eq!(available, 0);
    assert!(rejected);
    assert_eq!((before_ack, before_row, before_effect), (0, 0, 0));
    assert!(replies.iter().all(Result::is_ok));
    assert_eq!(replies[0].as_ref().unwrap(), replies[1].as_ref().unwrap());
    assert_eq!(replies[1].as_ref().unwrap(), replies[2].as_ref().unwrap());
    assert_eq!(effects, 1);
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 3);
    assert_eq!(service.journal.receipt_admissions.available_permits(), MAX_JOURNAL_RECEIPTS);
    assert_eq!(service.journal.accept_admissions.available_permits(), MAX_JOURNAL_ACCEPTS);
    assert_eq!(service.journal.accept_jobs.available_permits(), 1);
    assert!(service.inflight.lock().unwrap().is_empty());
}

#[test]
fn queued_transition_cancellation_runs_no_sql_and_releases_only_its_ticket() {
    let (_directory, service, runtime) = transition_fixture();
    let physical = service.journal.accept_jobs.available_permits();
    let (_, first_receipt, first_digest) = prepared_ticket(&service, "first-transition");
    service
        .journal
        .accept("phone-1", "first-transition", &first_digest)
        .unwrap();
    let gate = Arc::new(StageEntry::default());
    let (entered, entry) = mpsc::channel();
    let first = runtime.spawn({
        let journal = service.journal.clone();
        let gate = gate.clone();
        async move {
            journal
                .write_receipt(first_receipt, move |journal| {
                    entered.send(()).unwrap();
                    gate.wait();
                    journal.executing("phone-1", "first-transition", &first_digest)
                })
                .await
        }
    });
    let entered = entry.recv_timeout(Duration::from_secs(30));
    let (_, receipt, digest) = prepared_ticket(&service, "queued-transition");
    service
        .journal
        .accept("phone-1", "queued-transition", &digest)
        .unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let (observed, observation) = mpsc::channel();
    let queued = runtime.spawn({
        let journal = service.journal.clone();
        let called = called.clone();
        async move {
            let future = journal.write_receipt(receipt, move |journal| {
                called.store(true, Ordering::SeqCst);
                journal.executing("phone-1", "queued-transition", &digest)
            });
            let mut future = std::pin::pin!(future);
            let initial = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(std::future::Future::poll(future.as_mut(), cx))
            })
            .await;
            observed.send(initial.is_pending()).unwrap();
            match initial {
                std::task::Poll::Ready(result) => result,
                std::task::Poll::Pending => future.await,
            }
        }
    });
    let pending = observation.recv_timeout(Duration::from_secs(30));
    queued.abort();
    let cancelled = runtime.block_on(queued);
    let retained = service.inflight.lock().unwrap().len();
    let available = service.journal.receipt_admissions.available_permits();
    gate.release();
    let first_result = runtime.block_on(first);
    drain_sql(&service.journal, &runtime, physical);
    let state: String = service
        .journal
        .db()
        .unwrap()
        .query_row(
            "SELECT state FROM service_receipts WHERE request_id='queued-transition'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(entered.is_ok());
    assert_eq!(pending.unwrap(), true);
    assert!(cancelled.is_err_and(|error| error.is_cancelled()));
    assert_eq!(retained, 1);
    assert_eq!(available, MAX_JOURNAL_RECEIPTS - 1);
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(state, "accepted");
    assert!(first_result.unwrap().is_ok());
    assert_eq!(service.journal.receipt_admissions.available_permits(), MAX_JOURNAL_RECEIPTS);
    assert!(service.inflight.lock().unwrap().is_empty());
}

#[test]
fn started_transitions_keep_the_ticket_and_reservation_until_physical_exit() {
    for stage in [ReceiptWrite::Executing, ReceiptWrite::Complete] {
        let (_directory, service, runtime) = transition_fixture();
        let physical = service.journal.accept_jobs.available_permits();
        let (work, receipt, digest) = prepared_ticket(&service, "started-transition");
        service
            .journal
            .accept("phone-1", &work.request_id, &digest)
            .unwrap();
        if stage == ReceiptWrite::Complete {
            service
                .journal
                .executing("phone-1", &work.request_id, &digest)
                .unwrap();
        }
        let device = service.devices.get("phone-1").unwrap();
        let response = service
            .encode_response(
                &work.envelope,
                &device,
                &json!({"status":200,"body":{"ok":true}}),
                &LocalRequest {
                    method: "GET".into(),
                    path: "/v1/options".into(),
                    body: None,
                },
            )
            .unwrap();
        let expected = response.clone();
        let gate = Arc::new(StageEntry::default());
        let (entered, entry) = mpsc::channel();
        let job = runtime.spawn({
            let journal = service.journal.clone();
            let gate = gate.clone();
            async move {
                journal
                    .write_receipt(receipt, move |journal| {
                        entered.send(()).unwrap();
                        gate.wait();
                        if stage == ReceiptWrite::Executing {
                            journal.executing("phone-1", "started-transition", &digest)
                        } else {
                            journal.complete("phone-1", "started-transition", &response)
                        }
                    })
                    .await
            }
        });
        let entered = entry.recv_timeout(Duration::from_secs(30));
        job.abort();
        let cancelled = runtime.block_on(job);
        let retained = service.inflight.lock().unwrap().len();
        let available = service.journal.receipt_admissions.available_permits();
        let busy = service.journal.accept_jobs.available_permits();
        let duplicate = service
            .prepare_work(&work, Transport::Relay)
            .unwrap()
            .is_none();
        gate.release();
        drain_sql(&service.journal, &runtime, physical);
        let final_row: (String, Option<Vec<u8>>) = service
            .journal
            .db()
            .unwrap()
            .query_row(
                "SELECT state,response FROM service_receipts WHERE request_id='started-transition'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        println!(
            "started_receipt stage={stage:?} ticket_available_before_exit={available} physical_available_before_exit={busy} retained={retained} duplicate_held={duplicate}"
        );
        assert!(entered.is_ok());
        assert!(cancelled.is_err_and(|error| error.is_cancelled()));
        assert_eq!((retained, available, busy), (1, MAX_JOURNAL_RECEIPTS - 1, 0));
        assert!(duplicate);
        if stage == ReceiptWrite::Complete {
            assert_eq!(final_row, ("completed".into(), Some(expected)));
        } else {
            assert_eq!(final_row, ("executing".into(), None));
        }
        assert_eq!(service.journal.receipt_admissions.available_permits(), MAX_JOURNAL_RECEIPTS);
        assert!(service.inflight.lock().unwrap().is_empty());
    }
}

#[test]
fn completion_keeps_the_owned_response_allocation_and_errors_release_capacity() {
    let (_directory, service, runtime) = transition_fixture();
    let (work, receipt, digest) = prepared_ticket(&service, "owned-response");
    service
        .journal
        .accept("phone-1", &work.request_id, &digest)
        .unwrap();
    service
        .journal
        .executing("phone-1", &work.request_id, &digest)
        .unwrap();
    let device = service.devices.get("phone-1").unwrap();
    let response = service
        .encode_response(
            &work.envelope,
            &device,
            &json!({"status":200,"body":{"text":"x".repeat(700_000)}}),
            &LocalRequest {
                method: "GET".into(),
                path: "/v1/options".into(),
                body: None,
            },
        )
        .unwrap();
    let allocation = (
        response.as_ptr() as usize,
        response.len(),
        response.capacity(),
    );
    let returned = runtime
        .block_on(
            service
                .journal
                .complete_admitted("phone-1", &work.request_id, response, receipt),
        )
        .unwrap();
    let stored: Vec<u8> = service
        .journal
        .db()
        .unwrap()
        .query_row(
            "SELECT response FROM service_receipts WHERE request_id='owned-response'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        (
            returned.as_ptr() as usize,
            returned.len(),
            returned.capacity()
        ),
        allocation
    );
    assert_eq!(returned, stored);
    assert!(allocation.1 > 900_000 && allocation.1 <= MAX_RELAY_RESPONSE);
    let (_, receipt, digest) = prepared_ticket(&service, "wrong-state");
    service
        .journal
        .accept("phone-1", "wrong-state", &digest)
        .unwrap();
    let rejected = runtime.block_on(service.journal.complete_admitted(
        "phone-1",
        "wrong-state",
        returned,
        receipt,
    ));
    assert!(rejected.is_err());
    assert_eq!(service.journal.receipt_admissions.available_permits(), MAX_JOURNAL_RECEIPTS);
    let (_, receipt, digest) = prepared_ticket(&service, "missing-journal");
    std::fs::remove_file(&service.journal.path).unwrap();
    let missing = runtime.block_on(service.journal.executing_admitted(
        "phone-1",
        "missing-journal",
        &digest,
        false,
        receipt,
    ));
    assert!(missing.is_err());
    assert!(!service.journal.path.exists());
    assert_eq!(service.journal.receipt_admissions.available_permits(), MAX_JOURNAL_RECEIPTS);
    assert_eq!(service.journal.accept_jobs.available_permits(), 1);
    assert!(service.inflight.lock().unwrap().is_empty());
}

#[test]
fn executing_rechecks_approval_after_waiting_for_its_sql_slot() {
    for revoke in ["none", "approval", "enrollment"] {
        let (_directory, mut service, _) = push_fixture();
        activate_push(&service);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (listener, address) = listener();
        service.local_port = address.port();
        let calls = Arc::new(AtomicUsize::new(0));
        let local = Router::new().route(
            "/v1/options",
            get({
                let calls = calls.clone();
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async { axum::Json(json!({"protocol_version":2})) }
                }
            }),
        );
        let server = {
            let _entered = runtime.enter();
            runtime.spawn(
                axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), local)
                    .into_future(),
            )
        };
        let work = changes_work(
            &service,
            "authority-transition",
            json!({"method":"GET","path":"/v1/options"}),
        );
        let prepared = service
            .prepare_work(&work, Transport::Relay)
            .unwrap()
            .unwrap();
        let held = Arc::new(Mutex::new(None));
        let accepted = Arc::new(tokio::sync::Notify::new());
        let (checkpoint, before, result) = runtime.block_on(async {
            let mut operation = std::pin::pin!(service.execute(&work.envelope, prepared, Transport::Relay, || {
                let held = held.clone(); let accepted = accepted.clone(); let journal = service.journal.clone();
                async move {
                    let permit = journal.accept_jobs.clone().acquire_owned().await?;
                    *held.lock().unwrap() = Some(permit);
                    accepted.notify_one();
                    Ok(())
                }
            }));
            let early = tokio::select! { result = operation.as_mut() => Some(result), _ = accepted.notified() => None };
            let checkpoint = early.is_none() && held.lock().unwrap().is_some();
            let before: String = service.journal.db().unwrap().query_row("SELECT state FROM service_receipts WHERE request_id='authority-transition'", [], |row| row.get(0)).unwrap();
            match revoke {
                "approval" => { Auth::open(&service.home).unwrap().revoke("phone-1").unwrap(); }
                "enrollment" => service.journal.revoke_enrollment("phone-1").unwrap(),
                _ => (),
            }
            drop(held.lock().unwrap().take());
            let result = match early { Some(result) => result, None => operation.await };
            (checkpoint, before, result)
        });
        let successful = result.is_ok();
        let bytes = result.map(|(bytes, _, guard)| {
            drop(guard);
            bytes
        });
        if revoke == "none" {
            runtime.block_on(async {
                let prepared = service
                    .prepare_work(&work, Transport::Relay)
                    .unwrap()
                    .unwrap();
                let (replay, _, guard) = service
                    .execute(&work.envelope, prepared, Transport::Relay, || async {
                        Ok(())
                    })
                    .await
                    .unwrap();
                assert_eq!(&replay, bytes.as_ref().unwrap());
                drop(guard);
            });
        }
        runtime.block_on(async {
            server.abort();
            let _ = server.await;
        });
        println!(
            "receipt_authority revoke={revoke} pre_transition_state={before} successful={successful} calls={}",
            calls.load(Ordering::SeqCst)
        );
        assert!(checkpoint);
        assert_eq!(before, "accepted");
        assert_eq!(successful, revoke == "none");
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(revoke == "none"));
        assert_eq!(service.journal.receipt_admissions.available_permits(), MAX_JOURNAL_RECEIPTS);
        assert_eq!(service.journal.accept_jobs.available_permits(), 1);
        assert!(service.inflight.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn changes_ordinary_and_inline_busy_share_real_lifecycle_admission() {
    use axum::extract::Path as RoutePath;
    use std::collections::VecDeque;
    let (_directory, original, _) = push_fixture();
    let home = original.home.clone();
    drop(original);
    let mut configuration = std::fs::read_to_string(home.join("service.toml")).unwrap();
    let mut approvals = Vec::new();
    let mut phones = Vec::new();
    for index in 0..16 {
        let id = format!("phone-{}", index + 1);
        let signing = SigningKey::from_slice(&[33 + index as u8; 32]).unwrap();
        let agreement = SecretKey::from_slice(&[34 + index as u8; 32]).unwrap();
        approvals.push(json!({"id":id,"name":format!("Fixture phone {}",index+1),"created_at":0,"last_seen":null,"public_key":STANDARD.encode(signing.verifying_key().to_sec1_bytes())}));
        if index != 0 {
            configuration.push_str(&format!("\n[[devices]]\ndevice_id = \"{id}\"\nsigning_key = \"{}\"\nagreement_key = \"{}\"\n", URL_SAFE_NO_PAD.encode(signing.verifying_key().to_sec1_bytes()), URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_bytes())));
        }
        phones.push((id, signing, agreement));
    }
    std::fs::write(
        home.join("devices.json"),
        serde_json::to_vec(&approvals).unwrap(),
    )
    .unwrap();
    std::fs::write(home.join("service.toml"), configuration).unwrap();
    let mut service = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap()))
        .unwrap()
        .unwrap();
    for (id, _, _) in &phones {
        service.enroll_with_relay_for_tests(id);
    }
    let release_changes = Arc::new(tokio::sync::Semaphore::new(0));
    let release_post = Arc::new(tokio::sync::Semaphore::new(0));
    let release_read = Arc::new(tokio::sync::Semaphore::new(0));
    let release_busy_ack = Arc::new(tokio::sync::Semaphore::new(0));
    let release_post_upload = Arc::new(tokio::sync::Semaphore::new(0));
    let release_post_sql = Arc::new(tokio::sync::Semaphore::new(0));
    let changes_entered = Arc::new(tokio::sync::Semaphore::new(0));
    let ordinary_entered = Arc::new(tokio::sync::Semaphore::new(0));
    let busy_ack_entered = Arc::new(tokio::sync::Semaphore::new(0));
    let post_upload_entered = Arc::new(tokio::sync::Semaphore::new(0));
    let post_sql_entered = Arc::new(tokio::sync::Semaphore::new(0));
    let post_sql_released = Arc::new(tokio::sync::Semaphore::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let changes_calls = Arc::new(AtomicUsize::new(0));
    let read_calls = Arc::new(AtomicUsize::new(0));
    let ack_requests = Arc::new(AtomicUsize::new(0));
    let ack_returns = Arc::new(AtomicUsize::new(0));
    let responses = Arc::new(Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let local = Router::new()
        .route(
            "/v1/changes",
            get({
                let (entered, release, calls) = (
                    changes_entered.clone(),
                    release_changes.clone(),
                    changes_calls.clone(),
                );
                move || {
                    let (entered, release, calls) =
                        (entered.clone(), release.clone(), calls.clone());
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        entered.add_permits(1);
                        release.acquire().await.unwrap().forget();
                        axum::Json(json!({"tick":42}))
                    }
                }
            }),
        )
        .route(
            "/v1/questions/1/answer",
            post({
                let (entered, release, effects) = (
                    ordinary_entered.clone(),
                    release_post.clone(),
                    effects.clone(),
                );
                move || {
                    let (entered, release, effects) =
                        (entered.clone(), release.clone(), effects.clone());
                    async move {
                        effects.fetch_add(1, Ordering::SeqCst);
                        entered.add_permits(1);
                        release.acquire().await.unwrap().forget();
                        axum::Json(json!({"saved":true}))
                    }
                }
            }),
        )
        .route(
            "/v1/options",
            get({
                let (entered, release, calls) = (
                    ordinary_entered.clone(),
                    release_read.clone(),
                    read_calls.clone(),
                );
                move || {
                    let (entered, release, calls) =
                        (entered.clone(), release.clone(), calls.clone());
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        entered.add_permits(1);
                        release.acquire().await.unwrap().forget();
                        axum::Json(json!({"short":true}))
                    }
                }
            }),
        );
    let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.local_port = local_listener.local_addr().unwrap().port();
    let local_server = tokio::spawn(axum::serve(local_listener, local).into_future());
    let seal = |phone: usize, id: &str, request: Value| Work {
        request_id: id.to_owned(),
        envelope: Envelope::seal_random(
            EnvelopeContext {
                route: "mac-1",
                sender: &phones[phone].0,
                recipient: "mac-1",
                request_id: id,
                kind: "request",
                expires_at_ms: now_ms() + 600_000,
            },
            &serde_json::to_vec(&request).unwrap(),
            &phones[phone].1,
            &service.agreement.public_key(),
        )
        .unwrap(),
    };
    let mut works: Vec<_> = (0..16)
        .map(|index| {
            seal(
                index,
                &format!("cohort-changes-{index}"),
                json!({"method":"GET","path":"/v1/changes?after=0&wait=50"}),
            )
        })
        .collect();
    works.push(seal(
        0,
        "cohort-post",
        json!({"method":"POST","path":"/v1/questions/1/answer","body":{"text":"Fixture answer"}}),
    ));
    works.push(seal(
        1,
        "cohort-read",
        json!({"method":"GET","path":"/v1/options"}),
    ));
    for index in 2..relay::MAX_ORDINARY_DELIVERIES {
        works.push(seal(index, &format!("cohort-read-{index}"), json!({"method":"GET","path":"/v1/options"})));
    }
    works.push(seal(
        0,
        "cohort-busy",
        json!({"method":"GET","path":"/v1/changes?after=0&wait=50"}),
    ));
    let queue = Arc::new(Mutex::new(VecDeque::from(
        works
            .iter()
            .map(|work| json!({"request_id":work.request_id,"envelope":work.envelope}))
            .collect::<Vec<_>>(),
    )));
    let relay = Router::new()
        .route(
            "/v1/bootstrap",
            get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([7;16])})) }),
        )
        .route(
            "/v1/work",
            get({
                let queue = queue.clone();
                move || {
                    let next = queue.lock().unwrap().pop_front().unwrap();
                    async move { axum::Json(next) }
                }
            }),
        )
        .route(
            "/v1/work/{id}/receipt",
            post({
                let (entered, release, requests, returned) = (
                    busy_ack_entered.clone(),
                    release_busy_ack.clone(),
                    ack_requests.clone(),
                    ack_returns.clone(),
                );
                move |RoutePath(id): RoutePath<String>| {
                    let (entered, release, returned) =
                        (entered.clone(), release.clone(), returned.clone());
                    requests.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if id == "cohort-busy" {
                            entered.add_permits(1);
                            release.acquire().await.unwrap().forget();
                        }
                        returned.fetch_add(1, Ordering::SeqCst);
                        StatusCode::ACCEPTED
                    }
                }
            }),
        )
        .route(
            "/v1/work/{id}/response",
            post({
                let (responses, entered, release) = (
                    responses.clone(),
                    post_upload_entered.clone(),
                    release_post_upload.clone(),
                );
                move |RoutePath(id): RoutePath<String>, body: Bytes| {
                    let (responses, entered, release) =
                        (responses.clone(), entered.clone(), release.clone());
                    async move {
                        responses.lock().unwrap().push((id.clone(), body.to_vec()));
                        if id == "cohort-post" {
                            entered.add_permits(1);
                            release.acquire().await.unwrap().forget();
                        }
                        StatusCode::ACCEPTED
                    }
                }
            }),
        );
    let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(
        Url::parse(&format!("http://{}/", relay_listener.local_addr().unwrap())).unwrap(),
    );
    let relay_server = tokio::spawn(axum::serve(relay_listener, relay).into_future());
    service.journal.before_write = Some(Arc::new({
        let (entered, release, released) = (
            post_sql_entered.clone(),
            release_post_sql.clone(),
            post_sql_released.clone(),
        );
        move |stage, id| {
            let target = stage == ReceiptWrite::Complete && id == "cohort-post";
            let (entered, release, released) = (entered.clone(), release.clone(), released.clone());
            Box::pin(async move {
                if target {
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    released.add_permits(1);
                }
            })
        }
    }));
    let service = Arc::new(service);
    let mut tasks = tokio::task::JoinSet::new();
    let mut epoch = None;
    let mut revoked = HashSet::new();
    let mut full_snapshot = None;
    let mut blocked_completion = None;
    let exercise=tokio::time::timeout(Duration::from_secs(30), async {
        for _ in 0..MAX_JOURNAL_ACCEPTS { service.poll_work(&mut epoch,&mut revoked,&mut tasks).await?; }
        tokio::select! {
            permit=changes_entered.acquire_many(16) => permit?.forget(),
            done=tasks.join_next() => anyhow::bail!("changes worker finished before the cohort hold: {done:?}"),
        }
        tokio::select! {
            permit=ordinary_entered.acquire_many(relay::MAX_ORDINARY_DELIVERIES as u32) => permit?.forget(),
            done=tasks.join_next() => anyhow::bail!("ordinary worker finished before the cohort hold: {done:?}"),
        }
        let task_count = tasks.len();
        let busy=service.poll_work(&mut epoch,&mut revoked,&mut tasks);
        let mut busy=std::pin::pin!(busy);
        tokio::select! {
            permit=busy_ack_entered.acquire() => permit?.forget(),
            result=busy.as_mut() => anyhow::bail!("inline saturated intake finished before its ACK hold: {result:?}"),
        }
        let inflight=service.inflight.lock().unwrap();
        full_snapshot=Some((service.journal.receipt_admissions.available_permits(),inflight.len(),inflight.values().filter(|(_,changes)| *changes).count(),task_count,ack_requests.load(Ordering::SeqCst),ack_returns.load(Ordering::SeqCst),effects.load(Ordering::SeqCst),changes_calls.load(Ordering::SeqCst),read_calls.load(Ordering::SeqCst),service.journal.accept_jobs.available_permits()));
        drop(inflight);
        let physical=service.journal.accept_jobs.clone().acquire_owned().await?;
        release_post.add_permits(1);
        post_sql_entered.acquire().await?.forget();
        release_post_sql.add_permits(1);
        post_sql_released.acquire().await?.forget();
        let post_state:String=service.journal.db()?.query_row("SELECT state FROM service_receipts WHERE request_id='cohort-post'",[],|row|row.get(0))?;
        blocked_completion=Some((post_state,service.journal.receipt_admissions.available_permits(),service.journal.accept_jobs.available_permits(),responses.lock().unwrap().len()));
        drop(physical);
        post_upload_entered.acquire().await?.forget();
        release_busy_ack.add_permits(1);
        busy.await?;
        Ok::<_,anyhow::Error>(())
    }).await;
    for gate in [
        &release_changes,
        &release_post,
        &release_read,
        &release_busy_ack,
        &release_post_upload,
        &release_post_sql,
    ] {
        gate.add_permits(64);
    }
    let mut outcomes = Vec::new();
    while let Some(result) = tasks.join_next().await {
        outcomes.push(result);
    }
    let physical = service
        .journal
        .accept_jobs
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    drop(physical);
    let initial = responses.lock().unwrap().clone();
    let mut replay_results = Vec::new();
    if matches!(exercise, Ok(Ok(()))) {
        let mut replay = tokio::task::JoinSet::new();
        for work in &works {
            replay_results.push(
                service
                    .schedule_work(repeat_work(work), [7; 16], &mut replay)
                    .await,
            );
            while let Some(result) = replay.join_next().await {
                outcomes.push(result);
            }
        }
    }
    local_server.abort();
    relay_server.abort();
    let _ = local_server.await;
    let _ = relay_server.await;
    let records: i64 = service
        .journal
        .db()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM service_receipts WHERE state='completed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let active_devices: i64 = service
        .journal
        .db()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM service_enrollments WHERE state='active'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let all = responses.lock().unwrap();
    let mut statuses = Vec::new();
    let mut exact_replays = true;
    for work in &works {
        let copies: Vec<_> = all
            .iter()
            .filter(|(id, _)| id == &work.request_id)
            .collect();
        if copies.len() != 2 {
            exact_replays = false;
            continue;
        }
        exact_replays &= copies[0].1 == copies[1].1;
        let envelope: Envelope = serde_json::from_slice(&copies[0].1).unwrap();
        let agreement = &phones
            .iter()
            .find(|(id, _, _)| id == &envelope.recipient)
            .unwrap()
            .2;
        let opened: Value = serde_json::from_slice(
            &envelope
                .open(
                    service.signing.verifying_key(),
                    agreement,
                    "mac-1",
                    &envelope.recipient,
                    "mac-1",
                    now_ms(),
                )
                .unwrap(),
        )
        .unwrap();
        statuses.push((work.request_id.clone(), opened));
    }
    println!(
        "mixed_receipt_cohort snapshot={full_snapshot:?} blocked_completion={blocked_completion:?} initial_replies={} total_replies={} records={records} active_devices={active_devices} effects={} changes_calls={} read_calls={} lifecycle_available={} physical_available={} reservations={} exact_replays={exact_replays} exercise={exercise:?}",
        initial.len(),
        all.len(),
        effects.load(Ordering::SeqCst),
        changes_calls.load(Ordering::SeqCst),
        read_calls.load(Ordering::SeqCst),
        service.journal.receipt_admissions.available_permits(),
        service.journal.accept_jobs.available_permits(),
        service.inflight.lock().unwrap().len()
    );
    println!("mixed_receipt_outcomes={outcomes:?} replay_scheduling={replay_results:?}");
    assert!(matches!(exercise, Ok(Ok(()))));
    assert_eq!(full_snapshot, Some((0, MAX_JOURNAL_RECEIPTS, 16, MAX_JOURNAL_ACCEPTS, MAX_JOURNAL_RECEIPTS, MAX_JOURNAL_ACCEPTS, 1, 16, relay::MAX_ORDINARY_DELIVERIES - 1, 1)));
    assert_eq!(blocked_completion, Some(("executing".into(), 0, 0, 0)));
    assert_eq!(active_devices, 16);
    assert_eq!(initial.len(), MAX_JOURNAL_RECEIPTS);
    assert_eq!(all.len(), 2 * MAX_JOURNAL_RECEIPTS);
    assert_eq!(records, MAX_JOURNAL_RECEIPTS as i64);
    assert!(outcomes.iter().all(|result| matches!(result, Ok(Ok(())))));
    assert!(replay_results.iter().all(Result::is_ok));
    assert!(exact_replays);
    assert_eq!(
        statuses
            .iter()
            .filter(|(_, value)| value["status"] == 429 && value["error"] == "changes_busy")
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|(_, value)| value["status"] == 200)
            .count(),
        MAX_JOURNAL_ACCEPTS
    );
    for (id, value) in &statuses {
        if id.starts_with("cohort-changes-") {
            assert_eq!(value, &json!({"status":200,"body":{"tick":42}}));
        }
        if id == "cohort-post" {
            assert_eq!(value, &json!({"status":200,"body":{"saved":true}}));
        }
        if id.starts_with("cohort-read") {
            assert_eq!(value, &json!({"status":200,"body":{"short":true}}));
        }
    }
    assert_eq!(
        (
            effects.load(Ordering::SeqCst),
            changes_calls.load(Ordering::SeqCst),
            read_calls.load(Ordering::SeqCst)
        ),
        (1, 16, relay::MAX_ORDINARY_DELIVERIES - 1)
    );
    assert_eq!(service.journal.receipt_admissions.available_permits(), MAX_JOURNAL_RECEIPTS);
    assert_eq!(service.journal.accept_admissions.available_permits(), MAX_JOURNAL_ACCEPTS);
    assert_eq!(service.journal.accept_jobs.available_permits(), 1);
    assert!(service.inflight.lock().unwrap().is_empty());
}
