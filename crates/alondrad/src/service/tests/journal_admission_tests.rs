use super::*;
use std::future::IntoFuture;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;

#[test]
fn a_retained_sqlite_writer_does_not_occupy_the_daemon_runtime() {
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    drop(crate::domain_runtime::DomainRuntime::open(&service.home).unwrap());
    let state = crate::domain_bootstrap::DomainDaemon::open(&service.home).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2).enable_all().build().unwrap();

    fn listener() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        (listener, address)
    }
    let (local_listener, local_address) = listener();
    let (daemon_listener, daemon_address) = listener();
    service.local_port = local_address.port();
    let service = Arc::new(service);
    let calls = Arc::new(AtomicUsize::new(0));
    let receipts = Arc::new(AtomicUsize::new(0));
    let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let local = Router::new().route("/v1/options", get({
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { axum::Json(json!({"protocol_version":2})) }
        }
    }));
    let daemon = crate::domain_bootstrap::router(state.clone())
        .layer(axum::middleware::from_fn_with_state(state.auth(), crate::auth::guard));
    let servers = {
        let _entered = runtime.enter();
        [
            runtime.spawn(axum::serve(tokio::net::TcpListener::from_std(local_listener).unwrap(), local).into_future()),
            runtime.spawn(axum::serve(tokio::net::TcpListener::from_std(daemon_listener).unwrap(),
                daemon.into_make_service_with_connect_info::<SocketAddr>()).into_future()),
        ]
    };

    // All preparation/classification precedes contention; the marker follows these steps.
    let mut prepared = Vec::new();
    for id in ["sql-one", "sql-two"] {
        let work = changes_work(&service, id, json!({"method":"GET","path":"/v1/options"}));
        let mut request = service.prepare_work(&work, Transport::Direct).unwrap().unwrap();
        assert!(Service::ordinary_read(&request.request));
        request.execution = ExecutionLane::Read;
        prepared.push((work, request));
    }
    let mut writer = service.journal.db().unwrap();
    let transaction = writer.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    let (started, starting) = mpsc::channel();
    let (completed, completions) = mpsc::channel();
    let entry = Arc::new(std::sync::Barrier::new(3));
    let mut jobs = Vec::new();
    for (index, (work, prepared)) in prepared.into_iter().enumerate() {
        let service = service.clone();
        let started = started.clone();
        let completed = completed.clone();
        let entry = entry.clone();
        let (receipts, responses) = (receipts.clone(), responses.clone());
        jobs.push(runtime.spawn(async move {
            // The productive Read path has no suspension between this marker and accept.
            started.send(index).unwrap();
            entry.wait();
            let executed = service.execute(&work.envelope, prepared, Transport::Direct, || {
                receipts.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            }).await;
            completed.send((index, executed.is_ok())).unwrap();
            executed.map(|(response, _, _)| responses.lock().unwrap().push(response))
        }));
    }
    drop(started);
    drop(completed);
    let starts = [starting.recv().unwrap(), starting.recv().unwrap()];
    entry.wait();

    // This native controller owns the SQL writer and can release/drain despite runtime stalls.
    let daemon_response = (|| -> std::io::Result<Vec<u8>> {
        let mut connection = TcpStream::connect(daemon_address)?;
        write!(connection, "GET /daemon HTTP/1.1\r\nHost: {daemon_address}\r\nConnection: close\r\n\r\n")?;
        let mut response = Vec::new();
        connection.read_to_end(&mut response)?;
        Ok(response)
    })();
    let before_release: Vec<_> = completions.try_iter().collect();
    transaction.rollback().unwrap();
    drop(writer);
    let outcomes = runtime.block_on(async {
        let mut outcomes = Vec::new();
        for job in jobs { outcomes.push(job.await.unwrap()); }
        for server in servers {
            server.abort();
            let _ = server.await;
        }
        outcomes
    });
    let remaining: Vec<_> = completions.try_iter().collect();
    let errors = outcomes.iter().filter(|result| result.is_err()).count();
    println!("journal_runtime_outcomes={outcomes:?}");
    let results = opened_results(&service, &phone, &responses);
    let journaled: i64 = service.journal.db().unwrap().query_row(
        "SELECT count(*) FROM service_receipts", [], |row| row.get(0)).unwrap();
    let daemon_response = daemon_response.unwrap();
    println!("journal_runtime offered=2 starts={starts:?} completed_before_daemon={} before_results={before_release:?} remaining_results={remaining:?} errors={errors} receipts={} local_calls={} replies={} journaled={journaled} daemon_bytes={} reservations={}",
        before_release.len(), receipts.load(Ordering::SeqCst), calls.load(Ordering::SeqCst),
        results.len(), daemon_response.len(), service.inflight.lock().unwrap().len());
    assert!(daemon_response.starts_with(b"HTTP/1.1 200"));
    assert!(String::from_utf8_lossy(&daemon_response).contains("\"views_protocol\""));
    assert!(before_release.is_empty(), "the independent daemon response must precede SQL completion while its writer is retained");
    assert_eq!(errors, 0);
    assert_eq!(receipts.load(Ordering::SeqCst), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(results, vec![json!({"status":200,"body":{"protocol_version":2}});2]);
    assert_eq!(journaled, 2);
    assert!(service.inflight.lock().unwrap().is_empty());
}

use std::future::Future;
use std::task::Poll;

fn admission_fixture() -> (crate::test_support::Directory, Arc<Service>, tokio::runtime::Runtime) {
    let (directory, service, _) = push_fixture();
    activate_push(&service);
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    (directory, Arc::new(service), runtime)
}

fn admission_reservation(service: &Service, id: &str) -> (Arc<WorkGuard>, [u8;32]) {
    let work = changes_work(service, id, json!({"method":"GET","path":"/v1/options"}));
    let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
    (prepared.guard, prepared.digest)
}

#[test]
fn accept_admission_bounds_pending_work_and_recovers_the_same_unacknowledged_envelope() {
    let (_directory, service, runtime) = admission_fixture();
    let gate = Arc::new(std::sync::Barrier::new(2));
    let (entered, start) = mpsc::channel();
    let (reservation, digest) = admission_reservation(&service, "occupied-0");
    let first = runtime.spawn({
        let journal = service.journal.clone();
        let gate = gate.clone();
        async move { journal.off_runtime_admitted(reservation, move |journal| {
            entered.send(()).unwrap();
            gate.wait();
            journal.accept("phone-1", "occupied-0", &digest).map(|_| ())
        }).await }
    });
    start.recv().unwrap();
    let (queued, observed) = mpsc::channel();
    let mut jobs = vec![first];
    for index in 1..MAX_JOURNAL_ACCEPTS {
        let id = format!("occupied-{index}");
        let (reservation, digest) = admission_reservation(&service, &id);
        let journal = service.journal.clone();
        let queued = queued.clone();
        jobs.push(runtime.spawn(async move {
            let operation = journal.off_runtime_admitted(reservation,
                move |journal| journal.accept("phone-1", &id, &digest).map(|_| ()));
            let mut operation = std::pin::pin!(operation);
            let initial = std::future::poll_fn(|cx| Poll::Ready(operation.as_mut().poll(cx))).await;
            queued.send(initial.is_pending()).unwrap();
            match initial { Poll::Ready(result) => result, Poll::Pending => operation.await }
        }));
    }
    let observed: Vec<_> = (1..MAX_JOURNAL_ACCEPTS).map(|_| observed.recv().unwrap()).collect();
    let occupied = service.journal.accept_admissions.available_permits();

    let work = changes_work(&service, "overflow", json!({"method":"POST","path":"/v1/service/push-binding",
        "body":{"push_binding":URL_SAFE_NO_PAD.encode([9;32])}}));
    let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
    let acknowledgements = Arc::new(AtomicUsize::new(0));
    let rejected = runtime.block_on(async {
        let ack = acknowledgements.clone();
        let operation = service.execute(&work.envelope, prepared, Transport::Relay, || async move {
            ack.fetch_add(1, Ordering::SeqCst); Ok(())
        });
        let mut operation = std::pin::pin!(operation);
        matches!(std::future::poll_fn(|cx| Poll::Ready(operation.as_mut().poll(cx))).await, Poll::Ready(Err(_)))
    });
    let before_ack = acknowledgements.load(Ordering::SeqCst);
    let before_row: i64 = service.journal.db().unwrap().query_row(
        "SELECT count(*) FROM service_receipts WHERE request_id='overflow'", [], |row| row.get(0)).unwrap();
    let before_binding = service.journal.push_binding("phone-1").unwrap();
    gate.wait();
    let results = runtime.block_on(async {
        let mut results = Vec::new();
        for job in jobs { results.push(job.await.unwrap()); }
        results
    });
    let replay = runtime.block_on(async {
        let mut replies = Vec::new();
        for _ in 0..2 {
            let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
            let ack = acknowledgements.clone();
            let result = service.execute(&work.envelope, prepared, Transport::Relay, || async move {
                ack.fetch_add(1, Ordering::SeqCst); Ok(())
            }).await;
            replies.push(result.map(|(bytes, _, guard)| { drop(guard); bytes }));
        }
        replies
    });
    let binding = service.journal.push_binding("phone-1").unwrap();
    assert!(observed.iter().all(|pending| *pending));
    assert_eq!(occupied, 0);
    assert!(rejected, "an overflow admission must fail before SQL");
    assert_eq!(before_ack, 0);
    assert_eq!(before_row, 0);
    assert_eq!(before_binding, Some(URL_SAFE_NO_PAD.encode([4;32])));
    assert!(results.iter().all(Result::is_ok));
    assert!(replay.iter().all(Result::is_ok));
    assert_eq!(replay[0].as_ref().unwrap(), replay[1].as_ref().unwrap(), "the same envelope must replay its completed ciphertext");
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 2);
    assert_eq!(binding, Some(URL_SAFE_NO_PAD.encode([9;32])));
    assert_eq!(service.journal.accept_admissions.available_permits(), MAX_JOURNAL_ACCEPTS);
    assert_eq!(service.journal.accept_jobs.available_permits(), 1);
    assert!(service.inflight.lock().unwrap().is_empty());
}

#[test]
fn queued_accept_cancellation_runs_no_sql_and_releases_its_reservation() {
    let (_directory, service, runtime) = admission_fixture();
    let gate = Arc::new(std::sync::Barrier::new(2));
    let (entered, start) = mpsc::channel();
    let (reservation, digest) = admission_reservation(&service, "held");
    let first = runtime.spawn({
        let journal = service.journal.clone();
        let gate = gate.clone();
        async move { journal.off_runtime_admitted(reservation, move |journal| {
            entered.send(()).unwrap(); gate.wait();
            journal.accept("phone-1", "held", &digest).map(|_| ())
        }).await }
    });
    start.recv().unwrap();
    let (reservation, digest) = admission_reservation(&service, "cancelled");
    let called = Arc::new(AtomicBool::new(false));
    let (polled, poll) = mpsc::channel();
    let queued = runtime.spawn({
        let journal = service.journal.clone();
        let called = called.clone();
        async move {
            let operation = journal.off_runtime_admitted(reservation, move |journal| {
                called.store(true, Ordering::SeqCst);
                journal.accept("phone-1", "cancelled", &digest).map(|_| ())
            });
            let mut operation = std::pin::pin!(operation);
            let initial = std::future::poll_fn(|cx| Poll::Ready(operation.as_mut().poll(cx))).await;
            polled.send(initial.is_pending()).unwrap();
            match initial { Poll::Ready(result) => result, Poll::Pending => operation.await }
        }
    });
    let pending = poll.recv().unwrap();
    queued.abort();
    let cancelled = runtime.block_on(queued);
    let retained = service.inflight.lock().unwrap().len();
    let available = service.journal.accept_admissions.available_permits();
    gate.wait();
    let first_result = runtime.block_on(first).unwrap();
    let row: i64 = service.journal.db().unwrap().query_row(
        "SELECT count(*) FROM service_receipts WHERE request_id='cancelled'", [], |row| row.get(0)).unwrap();
    assert!(pending);
    assert!(cancelled.is_err_and(|error| error.is_cancelled()));
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(row, 0);
    assert_eq!(retained, 1);
    assert_eq!(available, MAX_JOURNAL_ACCEPTS - 1);
    assert!(first_result.is_ok());
    assert_eq!(service.journal.accept_admissions.available_permits(), MAX_JOURNAL_ACCEPTS);
    assert!(service.inflight.lock().unwrap().is_empty());
}

struct AdmissionExit(mpsc::Sender<()>);
impl Drop for AdmissionExit { fn drop(&mut self) { let _ = self.0.send(()); } }

#[test]
fn started_accept_owns_its_slot_and_reservation_after_cancel_or_runtime_shutdown() {
    for shutdown in [false, true] {
        let (_directory, service, runtime) = admission_fixture();
        let mut runtime = Some(runtime);
        let gate = Arc::new(std::sync::Barrier::new(2));
        let (entered, start) = mpsc::channel();
        let (exited, exit) = mpsc::channel();
        let (caller_done, caller_exit) = mpsc::channel();
        let outcome = Arc::new(Mutex::new(None));
        let (reservation, digest) = admission_reservation(&service, "started");
        let task = runtime.as_ref().unwrap().spawn({
            let journal = service.journal.clone();
            let gate = gate.clone();
            let outcome = outcome.clone();
            async move {
                let _caller_exit = AdmissionExit(caller_done);
                journal.off_runtime_admitted(reservation, move |journal| {
                    entered.send(()).unwrap(); gate.wait();
                    *outcome.lock().unwrap() = Some(journal.accept("phone-1", "started", &digest).map(|_| ()).map_err(|error| format!("{error:#}")));
                    Ok(AdmissionExit(exited))
                }).await
            }
        });
        start.recv().unwrap();
        if shutdown {
            runtime.take().unwrap().shutdown_background();
            caller_exit.recv().unwrap();
            drop(task);
        } else {
            task.abort();
            let _ = runtime.as_ref().unwrap().block_on(task);
            caller_exit.recv().unwrap();

        }
        let held_admissions = service.journal.accept_admissions.available_permits();
        let held_jobs = service.journal.accept_jobs.available_permits();
        let held_reservations = service.inflight.lock().unwrap().len();
        gate.wait();
        exit.recv().unwrap();
        drop(runtime);
        assert_eq!(held_admissions, MAX_JOURNAL_ACCEPTS - 1);
        assert_eq!(held_jobs, 0);
        assert_eq!(held_reservations, 1, "caller cancellation must not retire a still-running SQL job");
        assert!(outcome.lock().unwrap().as_ref().unwrap().is_ok());
        assert_eq!(service.journal.accept_admissions.available_permits(), MAX_JOURNAL_ACCEPTS);
        assert_eq!(service.journal.accept_jobs.available_permits(), 1);
        assert!(service.inflight.lock().unwrap().is_empty());
    }
}

#[test]
fn a_failed_admitted_sql_operation_releases_all_ownership() {
    let (_directory, service, runtime) = admission_fixture();
    let (reservation, digest) = admission_reservation(&service, "error");
    std::fs::remove_file(&service.journal.path).unwrap();
    let result: Result<()> = runtime.block_on(service.journal.off_runtime_admitted(reservation,
        move |journal| journal.accept("phone-1", "error", &digest).map(|_| ())));
    println!("admitted_sql_error={result:?}");
    assert!(result.is_err());
    assert_eq!(service.journal.accept_admissions.available_permits(), MAX_JOURNAL_ACCEPTS);
    assert_eq!(service.journal.accept_jobs.available_permits(), 1);
    assert!(service.inflight.lock().unwrap().is_empty());
}
