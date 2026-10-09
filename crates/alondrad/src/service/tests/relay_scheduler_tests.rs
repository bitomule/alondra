use super::*;
use axum::extract::Path;
use std::collections::VecDeque;

#[tokio::test]
async fn a_held_response_upload_does_not_block_the_next_ordinary_relay_request() {
    let mut observations = Vec::new();
    for _ in 0..3 { observations.push(held_response_profile().await); }
    let oversized_rejected = oversized_work_profile().await;
    for (intake_advanced, independent_before_release, peak) in observations {
        assert!(intake_advanced, "the completed request's held upload must not retain intake");
        assert!(independent_before_release, "the independent response must finish before releasing the first upload");
        assert_eq!(peak, 2);
    }
    assert!(oversized_rejected, "relay work must be bounded before JSON allocation and preparation");
}

async fn held_response_profile() -> (bool, bool, usize) {
    let cpu_start = cpu_micros();
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    let local_calls = Arc::new(AtomicUsize::new(0));
    let local = Router::new().route("/v1/options", get({
        let calls = local_calls.clone();
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { axum::Json(json!({"protocol_version":2})) }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.local_port = listener.local_addr().unwrap().port();
    let local_server = tokio::spawn(axum::serve(listener, local).into_future());
    let work = Arc::new(Mutex::new(VecDeque::from([
        changes_work(&service, "held-upload", json!({"method":"GET","path":"/v1/options"})),
        changes_work(&service, "independent", json!({"method":"GET","path":"/v1/options"})),
    ])));
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let replies = Arc::new(Mutex::new(Vec::new()));
    let receipts = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (delivered, mut completions) = tokio::sync::mpsc::unbounded_channel();
    let relay = Router::new()
        .route("/v1/bootstrap", get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([7;16])})) }))
        .route("/v1/work", get({
            let work = work.clone();
            move || {
                let next = work.lock().unwrap().pop_front().unwrap();
                async move { axum::Json(json!({"request_id":next.request_id,"envelope":next.envelope})) }
            }
        }))
        .route("/v1/work/{id}/receipt", post({
            let receipts = receipts.clone();
            move || {
                receipts.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::ACCEPTED }
            }
        }))
        .route("/v1/work/{id}/response", post({
            let entered = entered.clone();
            let release = release.clone();
            let replies = replies.clone();
            let active = active.clone();
            let peak = peak.clone();
            move |Path(id): Path<String>, body: Bytes| {
                let entered = entered.clone();
                let release = release.clone();
                let replies = replies.clone();
                let active = active.clone();
                let peak = peak.clone();
                let delivered = delivered.clone();
                async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    if id == "held-upload" {
                        entered.add_permits(1);
                        release.acquire().await.unwrap().forget();
                    }
                    replies.lock().unwrap().push(body.to_vec());
                    delivered.send(id).unwrap();
                    active.fetch_sub(1, Ordering::SeqCst);
                    StatusCode::ACCEPTED
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let (scheduled, mut admitted) = tokio::sync::mpsc::unbounded_channel();
    let (worker_done, completed_worker) = tokio::sync::oneshot::channel();
    let producer = tokio::spawn({
        let service = service.clone();
        async move {
            let mut epoch = None;
            let mut revoked = HashSet::new();
            let mut tasks = tokio::task::JoinSet::new();
            for id in ["held-upload", "independent"] {
                service.poll_work(&mut epoch, &mut revoked, &mut tasks).await.unwrap();
                scheduled.send(id).unwrap();
            }
            if let Some(result) = tasks.join_next().await {
                result.unwrap().unwrap();
                worker_done.send(()).unwrap();
            }
            while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
        }
    });
    entered.acquire().await.unwrap().forget();
    // On this current-thread runtime, an asynchronous schedule returns to the producer
    // before its spawned handler can reach the response server. Inline handling cannot.
    let intake_advanced = admitted.try_recv() == Ok("held-upload");
    let independent_before_release = if intake_advanced {
        let independent = completions.recv().await.unwrap() == "independent";
        completed_worker.await.unwrap();
        assert_eq!(service.inflight.lock().unwrap().len(), 1, "the completed independent worker releases its reservation while the first delivery retains its own");
        independent
    } else { false };
    release.add_permits(1);
    producer.await.unwrap();
    let results = opened_results(&service, &phone, &replies);
    let peak = peak.load(Ordering::SeqCst);
    let reply_bytes: usize = replies.lock().unwrap().iter().map(Vec::len).sum();
    println!("relay-delivery profile offered=2 completed={} errors={} local_calls={} peak_uploads={peak} reply_bytes={reply_bytes} cpu_us={} independent_before_release={independent_before_release}",
        results.len(), results.iter().filter(|value| value["status"] != 200).count(), local_calls.load(Ordering::SeqCst), cpu_micros() - cpu_start);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|value| value == &json!({"status":200,"body":{"protocol_version":2}})));
    assert_eq!(receipts.load(Ordering::SeqCst), 2);
    assert_eq!(local_calls.load(Ordering::SeqCst), 2);
    assert!(service.inflight.lock().unwrap().is_empty());
    assert!(work.lock().unwrap().is_empty());
    local_server.abort();
    relay_server.abort();
    let _ = local_server.await;
    let _ = relay_server.await;
    (intake_advanced, independent_before_release, peak)
}

fn cpu_micros() -> i64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) }, 0);
    let usage = unsafe { usage.assume_init() };
    usage.ru_utime.tv_sec * 1_000_000 + i64::from(usage.ru_utime.tv_usec)
        + usage.ru_stime.tv_sec * 1_000_000 + i64::from(usage.ru_stime.tv_usec)
}

#[tokio::test]
#[ignore = "opt-in isolated encrypted execution-lane profiling"]
async fn profile_ordinary_execution_behind_a_held_relay_receipt() {
    use std::{future::Future, task::Poll};
    for _ in 0..3 {
        let cpu_start = cpu_micros();
        let (_directory, service, phone) = push_fixture();
        activate_push(&service);
        let mut service = service;
        let local = Router::new().route("/v1/options", get(|| async { axum::Json(json!({"short":true})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.local_port = listener.local_addr().unwrap().port();
        let local_server = tokio::spawn(axum::serve(listener, local).into_future());
        let service = Arc::new(service);
        let entered = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let first = changes_work(&service, "held-receipt", json!({"method":"POST","path":"/v1/service/push-binding","body":{"push_binding":URL_SAFE_NO_PAD.encode([4;32])}}));
        let prepared = service.prepare_work(&first, Transport::Relay).unwrap().unwrap();
        let blocked = tokio::spawn({
            let service = service.clone();
            let entered = entered.clone();
            let release = release.clone();
            async move {
                service.execute(&first.envelope, prepared, Transport::Relay, || async {
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    Ok(())
                }).await.unwrap().0
            }
        });
        entered.acquire().await.unwrap().forget();
        let second = changes_work(&service, "independent", json!({"method":"GET","path":"/v1/options"}));
        let prepared = service.prepare_work(&second, Transport::Relay).unwrap().unwrap();
        let admitted = AtomicBool::new(false);
        let independent = service.execute(&second.envelope, prepared, Transport::Relay, || async {
            admitted.store(true, Ordering::SeqCst);
            Ok(())
        });
        let mut independent = std::pin::pin!(independent);
        let pending = std::future::poll_fn(|cx| Poll::Ready(independent.as_mut().poll(cx).is_pending())).await;
        let before_release = admitted.load(Ordering::SeqCst);
        release.add_permits(1);
        let bytes = vec![blocked.await.unwrap(), independent.await.unwrap().0];
        let responses = Mutex::new(bytes);
        let results = opened_results(&service, &phone, &responses);
        println!("relay-execution profile offered=2 completed={} errors={} second_pending={pending} independent_admitted_before_release={before_release} cpu_us={}",
            results.len(), results.iter().filter(|value| value["status"] != 204 && value["status"] != 200).count(), cpu_micros() - cpu_start);
        assert_eq!(results, vec![json!({"status":204}), json!({"status":200,"body":{"short":true}})]);
        local_server.abort();
        let _ = local_server.await;
        assert!(service.inflight.lock().unwrap().is_empty());
    }
}

async fn oversized_work_profile() -> bool {
    let (_directory, mut service, _) = push_fixture();
    activate_push(&service);
    let work = changes_work(&service, "oversized", json!({"method":"POST","path":"/v1/service/push-binding","body":{"push_binding":URL_SAFE_NO_PAD.encode([7;32])}}));
    let body = json!({"request_id":work.request_id,"envelope":work.envelope,"padding":"x".repeat(512 * 1024)});
    let receipts = Arc::new(AtomicUsize::new(0));
    let responses = Arc::new(AtomicUsize::new(0));
    let relay = Router::new()
        .route("/v1/bootstrap", get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([7;16])})) }))
        .route("/v1/work", get(move || {
            let body = body.clone();
            async move { axum::Json(body) }
        }))
        .route("/v1/work/{id}/receipt", post({
            let receipts = receipts.clone();
            move || {
                receipts.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::ACCEPTED }
            }
        }))
        .route("/v1/work/{id}/response", post({
            let responses = responses.clone();
            move || {
                responses.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::ACCEPTED }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let mut tasks = tokio::task::JoinSet::new();
    let mut epoch = None;
    let result = service.poll_work(&mut epoch, &mut HashSet::new(), &mut tasks).await;
    while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
    let rejected = result.is_err();
    let journaled: i64 = service.journal.db().unwrap().query_row(
        "SELECT count(*) FROM service_receipts WHERE sender='phone-1' AND request_id='oversized'",
        [], |row| row.get(0),
    ).unwrap();
    let binding = service.journal.push_binding("phone-1").unwrap();
    println!("relay-work-bound profile offered=1 rejected={rejected} receipts={} responses={} journaled={journaled}", receipts.load(Ordering::SeqCst), responses.load(Ordering::SeqCst));
    if rejected {
        assert!(epoch.is_none());
        assert_eq!(receipts.load(Ordering::SeqCst), 0);
        assert_eq!(responses.load(Ordering::SeqCst), 0);
        assert_eq!(journaled, 0);
        assert_eq!(binding, Some(URL_SAFE_NO_PAD.encode([4;32])));
    } else {
        assert_eq!(receipts.load(Ordering::SeqCst), 1);
        assert_eq!(responses.load(Ordering::SeqCst), 1);
        assert_eq!(journaled, 1);
        assert_eq!(binding, Some(URL_SAFE_NO_PAD.encode([7;32])));
    }
    assert!(service.inflight.lock().unwrap().is_empty());
    server.abort();
    let _ = server.await;
    rejected
}

#[tokio::test]
async fn relay_delivery_slots_bound_two_uploads_and_release_on_admission_cancel_and_shutdown() {
    use std::{future::Future, task::Poll};
    let (_directory, mut service, _) = push_fixture();
    activate_push(&service);
    let calls = Arc::new(AtomicUsize::new(0));
    let local = Router::new().route("/v1/options", get({
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { axum::Json(json!({"short":true})) }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.local_port = listener.local_addr().unwrap().port();
    let local_server = tokio::spawn(axum::serve(listener, local).into_future());
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    struct HandlerExit(Arc<tokio::sync::Semaphore>);
    impl Drop for HandlerExit {
        fn drop(&mut self) { self.0.add_permits(1); }
    }
    let finished = Arc::new(tokio::sync::Semaphore::new(0));
    let receipts = Arc::new(AtomicUsize::new(0));
    let relay = Router::new()
        .route("/v1/work/{id}/receipt", post({
            let receipts = receipts.clone();
            move || {
                receipts.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::ACCEPTED }
            }
        }))
        .route("/v1/work/{id}/response", post({
            let entered = entered.clone();
            let release = release.clone();
            let finished = finished.clone();
            move |Path(id): Path<String>| {
                let entered = entered.clone();
                let release = release.clone();
                let finished = finished.clone();
                async move {
                    let _exit = HandlerExit(finished);
                    if id != "after-shutdown" {
                        entered.add_permits(1);
                        release.acquire().await.unwrap().forget();
                    }
                    StatusCode::ACCEPTED
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let request = json!({"method":"GET","path":"/v1/options"});
    let first = changes_work(&service, "first", request.clone());
    let mut tasks = tokio::task::JoinSet::new();
    let scheduled_before_upload = {
        let schedule = service.schedule_work(repeat_work(&first), [7;16], &mut tasks);
        let mut schedule = std::pin::pin!(schedule);
        tokio::select! {
            result = &mut schedule => { result.unwrap(); true },
            permit = entered.acquire() => {
                permit.unwrap().forget();
                release.add_permits(1);
                schedule.await.unwrap();
                false
            },
        }
    };
    let mut admission_observation = None;
    if scheduled_before_upload {
        entered.acquire().await.unwrap().forget();
        service.schedule_work(changes_work(&service, "second", request.clone()), [7;16], &mut tasks).await.unwrap();
        entered.acquire().await.unwrap().forget();
        assert_eq!(tasks.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(receipts.load(Ordering::SeqCst), 2);
        {
            let schedule = service.schedule_work(changes_work(&service, "third", request.clone()), [7;16], &mut tasks);
            let mut schedule = std::pin::pin!(schedule);
            let pending = std::future::poll_fn(|cx| Poll::Ready(schedule.as_mut().poll(cx).is_pending())).await;
            let reservations = service.inflight.lock().unwrap().len();
            let accepted: i64 = service.journal.db().unwrap().query_row(
                "SELECT count(*) FROM service_receipts WHERE sender='phone-1' AND request_id='third'",
                [], |row| row.get(0),
            ).unwrap();
            admission_observation = Some((pending, reservations, accepted,
                calls.load(Ordering::SeqCst), receipts.load(Ordering::SeqCst)));
        }
        assert_eq!(service.inflight.lock().unwrap().len(), 2, "dropping an admission waiter releases its reservation");
        service.schedule_work(repeat_work(&first), [7;16], &mut tasks).await.unwrap();
        assert_eq!(tasks.len(), 2, "redelivery must not allocate a delivery slot or task");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        assert!(service.inflight.lock().unwrap().is_empty());
        release.add_permits(2);
        finished.acquire_many(2).await.unwrap().forget();
    } else {
        while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
        finished.acquire().await.unwrap().forget();
    }
    service.schedule_work(changes_work(&service, "after-shutdown", request), [7;16], &mut tasks).await.unwrap();
    while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
    finished.acquire().await.unwrap().forget();
    assert!(service.inflight.lock().unwrap().is_empty());
    local_server.abort();
    relay_server.abort();
    let _ = local_server.await;
    let _ = relay_server.await;
    assert!(scheduled_before_upload, "ordinary delivery must be owned and bounded outside intake");
    let (pending, reservations, accepted, admitted_calls, admitted_receipts) = admission_observation.unwrap();
    assert!(pending, "a third job must await delivery admission before execution or sealing");
    assert_eq!(reservations, 3);
    assert_eq!(accepted, 0, "an admission waiter must not acknowledge or start a durable receipt");
    assert_eq!(admitted_calls, 2);
    assert_eq!(admitted_receipts, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(receipts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_late_upload_failure_requests_resync_and_replays_the_receipt_without_another_effect() {
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    let effects = Arc::new(AtomicUsize::new(0));
    let local = Router::new().route("/v1/questions/1/answer", post({
        let effects = effects.clone();
        move || {
            effects.fetch_add(1, Ordering::SeqCst);
            async { axum::Json(json!({"saved":true})) }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.local_port = listener.local_addr().unwrap().port();
    let local_server = tokio::spawn(axum::serve(listener, local).into_future());
    let replies = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let relay = Router::new()
        .route("/v1/work/{id}/receipt", post(|| async { StatusCode::ACCEPTED }))
        .route("/v1/work/{id}/response", post({
            let replies = replies.clone();
            move |body: Bytes| {
                let replies = replies.clone();
                async move {
                    let mut replies = replies.lock().unwrap();
                    replies.push(body.to_vec());
                    if replies.len() == 1 { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::ACCEPTED }
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let work = changes_work(&service, "lost-upload", json!({"method":"POST","path":"/v1/questions/1/answer","body":{"text":"fixture"}}));
    let mut tasks = tokio::task::JoinSet::new();
    let scheduling = service.schedule_work(repeat_work(&work), [7;16], &mut tasks).await;
    let asynchronous = scheduling.is_ok();
    if asynchronous {
        assert!(tasks.join_next().await.unwrap().unwrap().is_err());
        assert!(tasks.is_empty());
    }
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert!(service.inflight.lock().unwrap().is_empty());
    let requested_resync = service.relay.as_ref().unwrap().resync_pending();
    let digest = Sha256::digest(serde_json::to_vec(&work.envelope).unwrap()).into();
    assert!(matches!(service.journal.accept("phone-1", "lost-upload", &digest).unwrap(), Disposition::Completed(_)));
    service.handle(work, &[7;16]).await.unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let results = opened_results(&service, &phone, &replies);
    assert_eq!(results, vec![json!({"status":200,"body":{"saved":true}});2]);
    let replies = replies.lock().unwrap();
    assert_eq!(replies[0], replies[1], "redelivery must upload the same stored ciphertext");
    drop(replies);
    local_server.abort();
    relay_server.abort();
    let _ = local_server.await;
    let _ = relay_server.await;
    assert!(asynchronous, "the upload failure must be reported by the owned delivery task");
    assert!(requested_resync, "a late upload error must invalidate the enrolled epoch");
}

#[tokio::test]
async fn a_valid_near_limit_envelope_keeps_its_work_wrapper_and_encrypted_reply() {
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    let local = Router::new().route("/v1/questions/1/answer", post(|axum::Json(body): axum::Json<Value>| async move {
        assert_eq!(body["text"].as_str().unwrap().len(), 190 * 1024);
        axum::Json(json!({"saved_bytes":190 * 1024}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.local_port = listener.local_addr().unwrap().port();
    let local_server = tokio::spawn(axum::serve(listener, local).into_future());
    let work = changes_work(&service, "near-limit", json!({"method":"POST","path":"/v1/questions/1/answer","body":{"text":"x".repeat(190 * 1024)}}));
    let envelope_bytes = serde_json::to_vec(&work.envelope).unwrap().len();
    assert!(envelope_bytes <= 256 * 1024);
    assert!(envelope_bytes > 256 * 1024 - 4096);
    let body = json!({"request_id":work.request_id,"envelope":work.envelope});
    let replies = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let relay = Router::new()
        .route("/v1/bootstrap", get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([7;16])})) }))
        .route("/v1/work", get(move || {
            let body = body.clone();
            async move { axum::Json(body) }
        }))
        .route("/v1/work/{id}/receipt", post(|| async { StatusCode::ACCEPTED }))
        .route("/v1/work/{id}/response", post({
            let replies = replies.clone();
            move |body: Bytes| {
                let replies = replies.clone();
                async move {
                    replies.lock().unwrap().push(body.to_vec());
                    StatusCode::ACCEPTED
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let mut tasks = tokio::task::JoinSet::new();
    service.poll_work(&mut None, &mut HashSet::new(), &mut tasks).await.unwrap();
    while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
    assert_eq!(opened_results(&service, &phone, &replies), vec![json!({"status":200,"body":{"saved_bytes":190 * 1024}})]);
    assert!(service.inflight.lock().unwrap().is_empty());
    local_server.abort();
    relay_server.abort();
    let _ = local_server.await;
    let _ = relay_server.await;
}

#[tokio::test]
async fn over_limit_relay_work_is_rejected_before_receipt_or_dispatch() {
    assert!(oversized_work_profile().await, "oversized relay work must be rejected before acknowledgement or execution");
}

#[tokio::test]
async fn a_pending_domain_read_delivery_cannot_cross_an_approval_generation() {
    let (_directory, mut service, _) = push_fixture();
    activate_push(&service);
    let home = service.home.clone();
    let config = alondra_core::config::Config::resolve(
        home.clone(), &Default::default(), |_| None, || Some("Synthetic".into()),
    );
    let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
    service = service.with_hub(hub);
    let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let relay = Router::new()
        .route("/v1/work/{id}/receipt", post(|| async { StatusCode::ACCEPTED }))
        .route("/v1/work/{id}/response", post({
            let responses = responses.clone();
            move |bytes: Bytes| {
                let responses = responses.clone();
                async move {
                    responses.lock().unwrap().push(bytes.to_vec());
                    StatusCode::ACCEPTED
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let work = changes_work(&service, "pending-generation", json!({"method":"GET","path":"/capabilities"}));
    let device = service.devices.get("phone-1").unwrap();
    let binding = service.read_receipt_binding("phone-1", &device, Transport::Relay).unwrap();
    let digest = approval_bound_read_digest(&serde_json::to_vec(&work.envelope).unwrap(), &binding);
    let mut tasks = tokio::task::JoinSet::new();
    service.schedule_work(work, [7;16], &mut tasks).await.unwrap();
    let sent_before = responses.lock().unwrap().len();
    let approvals = home.join("devices.json");
    let mut approved: Value = serde_json::from_slice(&std::fs::read(&approvals).unwrap()).unwrap();
    approved[0]["created_at"] = json!(1);
    std::fs::write(&approvals, serde_json::to_vec(&approved).unwrap()).unwrap();
    while let Some(result) = tasks.join_next().await { let _ = result.unwrap(); }
    let sent_after = responses.lock().unwrap().len();
    let completed = matches!(service.journal.accept("phone-1", "pending-generation", &digest).unwrap(), Disposition::Completed(_));
    let reserved = service.inflight.lock().unwrap().len();
    server.abort();
    let _ = server.await;
    assert!(completed, "the already applied read keeps its durable original receipt");
    assert_eq!(reserved, 0);
    assert_eq!(sent_after, sent_before, "a pending read must not upload old-generation ciphertext after reapproval");
}
