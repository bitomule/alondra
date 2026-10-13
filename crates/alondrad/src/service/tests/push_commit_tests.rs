use super::*;
use std::future::IntoFuture;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Instant;

fn http(address: SocketAddr, path: &str) -> std::io::Result<Vec<u8>> {
    use p256::ecdsa::signature::Signer;
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let timestamp = alondra_core::now_millis().to_string();
    let key = SigningKey::from_slice(&[33;32]).unwrap();
    let signature: p256::ecdsa::Signature = key.sign(crate::auth::signed_message("GET", path, &timestamp, b"").as_bytes());
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {address}\r\nx-alondra-device: phone-1\r\nx-alondra-timestamp: {timestamp}\r\nx-alondra-signature: {}\r\nConnection: close\r\n\r\n", STANDARD.encode(signature.to_der().as_bytes()))?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[test]
fn push_commit_retained_writer_keeps_signed_http_reads_serviceable() {
    let mut observations = Vec::new();
    for cohort in 0..3 {
    for workers in [1, 2] {
        for path in ["/v1/service/push-binding", PUSH_BINDING_ROUTE] {
            let (_directory, mut service, phone) = push_fixture();
            activate_push(&service);
            let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&service.home).unwrap());
            let expected_epoch = domain.state().unwrap().data_epoch.0.clone();
            if path == PUSH_BINDING_ROUTE { service = service.with_domain(domain); }
            let state = crate::domain_bootstrap::DomainDaemon::open(&service.home).unwrap();
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(workers).enable_all().build().unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let router = crate::domain_bootstrap::router(state.clone()).layer(
                axum::middleware::from_fn_with_state(state.auth(), crate::auth::guard));
            let server = {
                let _entered = runtime.enter();
                runtime.spawn(axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(),
                    router.into_make_service_with_connect_info::<SocketAddr>()).into_future())
            };
            let (ready, waiting) = mpsc::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let released = Arc::new(Mutex::new(Some(released)));
            service.journal.before_write = Some(Arc::new(move |stage, _| {
                let ready = ready.clone();
                let receiver = if stage == ReceiptWrite::PushPreferences { released.lock().unwrap().take() } else { None };
                Box::pin(async move { if let Some(receiver) = receiver { ready.send(()).unwrap(); receiver.await.unwrap(); } })
            }));
            let (sql, entered) = mpsc::channel();
            service.journal.push_sql_entry = Some(Arc::new(move || { sql.send(()).unwrap(); }));
            let service = Arc::new(service);
            let binding = URL_SAFE_NO_PAD.encode([9;32]);
            let work = changes_work(&service, "blocked-push", json!({"method":"POST","path":path,
                "body":{"push_binding":binding,"quiet_start":"23:00","quiet_end":"08:00","preferences_revision":1}}));
            let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
            let (done, completed) = mpsc::channel();
            let (ack, acknowledged) = mpsc::channel();
            let task = runtime.spawn({
                let service = service.clone();
                let envelope = work.envelope.clone();
                async move {
                    let result = service.execute(&envelope, prepared, Transport::Relay,
                        || async { ack.send(()).unwrap(); Ok(()) }).await;
                    done.send(result.is_ok()).unwrap();
                    result.map(|(bytes, _, guard)| { drop(guard); bytes })
                }
            });
            waiting.recv_timeout(Duration::from_secs(30)).unwrap();
            acknowledged.recv_timeout(Duration::from_secs(30)).unwrap();
            let mut writer = service.journal.db().unwrap();
            let transaction = writer.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
            release.send(()).unwrap();
            let entry = entered.recv_timeout(Duration::from_secs(30));
            if entry.is_err() {
                transaction.rollback().unwrap();
                let outcome = runtime.block_on(task);
                server.abort();
                let _ = runtime.block_on(server);
                panic!("SQL entry watchdog: {entry:?}; mutation={outcome:?}");
            }
            let held_slots = service.journal.accept_jobs.available_permits();
            let held_receipts = service.journal.receipt_admissions.available_permits();
            let held_reservations = service.inflight.lock().unwrap().len();
            let start = Instant::now();
            let responses: Vec<_> = ["/daemon", "/views/home?protocol=1&client=tui&lang=en"].into_iter()
                .map(|path| (path, http(address, path))).collect();
            let elapsed = start.elapsed().as_micros();
            let before: Vec<_> = completed.try_iter().collect();
            transaction.rollback().unwrap();
            drop(writer);
            let result = runtime.block_on(task).unwrap();
            let errors = usize::from(result.is_err());
            let successful = responses.iter().all(|(path, response)| response.as_ref().is_ok_and(|bytes| {
                let text = String::from_utf8_lossy(bytes);
                bytes.starts_with(b"HTTP/1.1 200") && if *path=="/daemon" {
                    text.contains("\"views_protocol\"")
                } else {
                    text.contains("\"protocol\":1") && text.contains(&expected_epoch)
                }
            }));
            let mut exact_replays = 0;
            if let Ok(bytes) = &result {
                let envelope: Envelope = serde_json::from_slice(bytes).unwrap();
                let opened = envelope.open(service.signing.verifying_key(), &phone, "mac-1", "phone-1", "mac-1", now_ms()).unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&opened).unwrap()["status"],204);
                for _ in 0..2 {
                    let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
                    let (replay, _, guard) = runtime.block_on(service.execute(&work.envelope, prepared, Transport::Relay, || async { Ok(()) })).unwrap();
                    assert_eq!(&replay, bytes);
                    drop(guard);
                    exact_replays += 1;
                }
                assert_eq!(service.journal.push_binding("phone-1").unwrap(),Some(binding));
                assert_eq!(service.journal.db().unwrap().query_row("SELECT revision FROM service_push_preference_revisions WHERE device_id='phone-1'",[],|r| r.get::<_,i64>(0)).unwrap(),1);
            }
            server.abort();
            let _ = runtime.block_on(server);
            assert!(service.inflight.lock().unwrap().is_empty());
            assert_eq!(service.journal.accept_jobs.available_permits(),1);
            println!("push_commit cohort={cohort} workers={workers} route={path} held_writer_http_us={elapsed} http_ok={successful} completed_before_release={before:?} mutation_errors={errors} exact_replays={exact_replays} held_slot_available={held_slots} held_receipts_available={held_receipts} held_reservations={held_reservations} mutation_result={result:?}");
            observations.push((successful,before.is_empty(),errors,exact_replays));
        }
    }
    }
    assert!(observations.iter().all(|&(http, before, errors, replays)| http && before && errors==0 && replays==2),"health and signed projection must finish while SQLite still retains the pending preference transaction: {observations:?}");
}

fn prepared_push(service: &Service, id: &str) -> (Work, LocalRequest, [u8;32], Arc<ReceiptAdmission>) {
    let work = changes_work(service, id, json!({"method":"POST","path":"/v1/service/push-binding",
        "body":{"push_binding":URL_SAFE_NO_PAD.encode([9;32])}}));
    let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
    service.journal.accept("phone-1",id,&prepared.digest).unwrap();
    service.journal.executing_local("phone-1",id,&prepared.digest).unwrap();
    let receipt = service.journal.reserve_receipt(prepared.guard).unwrap();
    (work,prepared.request,prepared.digest,receipt)
}

#[test]
fn push_commit_queued_work_rechecks_revocation_and_cancels_without_effect() {
    use std::future::Future;
    use std::task::Poll;
    for action in ["none", "approval", "enrollment", "cancel"] {
        let (_directory, service, _) = push_fixture();
        activate_push(&service);
        let service = Arc::new(service);
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let (work, request, digest, receipt) = prepared_push(&service,"queued-push");
        let held = service.journal.accept_jobs.clone().try_acquire_owned().unwrap();
        let (polled, observed) = mpsc::channel();
        let task = runtime.spawn({
            let service = service.clone();
            async move {
                let device = service.devices.get("phone-1").unwrap();
                let operation = service.execute_atomic_local(&work.envelope,&device,&request,&digest,Transport::Relay,receipt);
                let mut operation = std::pin::pin!(operation);
                let initial = std::future::poll_fn(|cx| Poll::Ready(operation.as_mut().poll(cx))).await;
                polled.send(initial.is_pending()).unwrap();
                match initial { Poll::Ready(result) => result, Poll::Pending => operation.await }
            }
        });
        let pending = observed.recv_timeout(Duration::from_secs(30)).unwrap();
        match action {
            "approval" => { Auth::open(&service.home).unwrap().revoke("phone-1").unwrap(); },
            "enrollment" => service.journal.revoke_enrollment("phone-1").unwrap(),
            "cancel" => {
                task.abort();
                runtime.block_on(async { while !task.is_finished() { tokio::task::yield_now().await; } });
            },
            _ => (),
        }
        drop(held);
        let result = runtime.block_on(task);
        let state: String = service.journal.db().unwrap().query_row("SELECT state FROM service_receipts WHERE request_id='queued-push'",[],|r|r.get(0)).unwrap();
        assert!(pending,"push must honor the shared physical SQL slot: {action}");
        if action == "none" {
            assert!(result.unwrap().is_ok());
            assert_eq!(state,"completed");
            assert_eq!(service.journal.push_binding("phone-1").unwrap(),Some(URL_SAFE_NO_PAD.encode([9;32])));
        } else {
            if action == "cancel" { assert!(result.is_err_and(|error|error.is_cancelled())); }
            else { assert!(result.unwrap().is_err()); }
            assert_eq!(state,"executing");
            let binding: Option<String> = service.journal.db().unwrap().query_row("SELECT binding FROM service_push_bindings WHERE device_id='phone-1'",[],|r|r.get(0)).optional().unwrap();
            assert_eq!(binding,if action=="enrollment" { None } else { Some(URL_SAFE_NO_PAD.encode([4;32])) });
        }
        assert!(service.inflight.lock().unwrap().is_empty());
        assert_eq!(service.journal.accept_jobs.available_permits(),1);
        assert_eq!(service.journal.receipt_admissions.available_permits(),MAX_JOURNAL_RECEIPTS);
    }
}

#[test]
fn push_commit_started_work_survives_caller_cancellation_with_exact_replay() {
    let (_directory, mut service, _) = push_fixture();
    activate_push(&service);
    let (enter, entered) = mpsc::channel();
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let physical_thread = Arc::new(Mutex::new(None));
    let caller_thread = Arc::new(Mutex::new(None));
    service.journal.push_sql_entry = Some(Arc::new({
        let gate = gate.clone();
        let physical_thread = physical_thread.clone();
        move || { *physical_thread.lock().unwrap()=Some(std::thread::current().id()); enter.send(()).unwrap();
            let mut released=gate.0.lock().unwrap();
            while !*released { released=gate.1.wait(released).unwrap(); } }
    }));
    let service = Arc::new(service);
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let (work, request, digest, receipt) = prepared_push(&service,"started-push");
    let task = runtime.spawn({
        let service = service.clone();
        let envelope = work.envelope.clone();
        let caller_thread = caller_thread.clone();
        async move {
            *caller_thread.lock().unwrap()=Some(std::thread::current().id());
            let device = service.devices.get("phone-1").unwrap();
            service.execute_atomic_local(&envelope,&device,&request,&digest,Transport::Relay,receipt).await
        }
    });
    let entry = entered.recv_timeout(Duration::from_secs(30));
    let on_runtime = *physical_thread.lock().unwrap()==*caller_thread.lock().unwrap();
    if entry.is_err() || on_runtime {
        *gate.0.lock().unwrap()=true;
        gate.1.notify_all();
        if entry.is_err() { task.abort(); }
        let result = runtime.block_on(task);
        panic!("push SQL must run outside caller worker; entry={entry:?} on_runtime={on_runtime} result={result:?}");
    }
    task.abort();
    let cancelled = runtime.block_on(task);
    let held_slot = service.journal.accept_jobs.available_permits();
    let held_receipts = service.journal.receipt_admissions.available_permits();
    let held_work = service.inflight.lock().unwrap().len();
    *gate.0.lock().unwrap()=true;
    gate.1.notify_all();
    // Acquiring the shared slot is a physical completion barrier, not a timing guess.
    let completion = runtime.block_on(service.journal.accept_jobs.clone().acquire_owned()).unwrap();
    drop(completion);
    let bytes: Vec<u8> = service.journal.db().unwrap().query_row("SELECT response FROM service_receipts WHERE request_id='started-push' AND state='completed'",[],|r|r.get(0)).unwrap();
    assert_eq!(service.journal.push_binding("phone-1").unwrap(),Some(URL_SAFE_NO_PAD.encode([9;32])));
    for _ in 0..2 {
        let prepared=service.prepare_work(&work,Transport::Relay).unwrap().unwrap();
        let (replay,_,guard)=runtime.block_on(service.execute(&work.envelope,prepared,Transport::Relay,||async{Ok(())})).unwrap();
        assert_eq!(replay,bytes);
        drop(guard);
    }
    assert!(cancelled.is_err_and(|error|error.is_cancelled()),"caller must finish while SQL stays held");
    assert_eq!(held_slot,0);
    assert_eq!(held_receipts,MAX_JOURNAL_RECEIPTS-1);
    assert_eq!(held_work,1);
    assert!(service.inflight.lock().unwrap().is_empty());
    assert_eq!(service.journal.receipt_admissions.available_permits(),MAX_JOURNAL_RECEIPTS);
}
