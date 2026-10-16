use super::*;
use axum::extract::Path;
use std::collections::VecDeque;
use super::relay::UPLOAD_RETRY_DELAYS;

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
    let first_held = Arc::new(tokio::sync::Semaphore::new(0));
    let replies = Arc::new(Mutex::new(Vec::new()));
    let receipts = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (delivered, mut completions) = tokio::sync::mpsc::unbounded_channel();
    let relay = Router::new()
        .route("/v1/bootstrap", get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([7;16])})) }))
        .route("/v1/work", get({
            let work = work.clone();
            let first_held = first_held.clone();
            move || {
                let next = work.lock().unwrap().pop_front().unwrap();
                let first_held = first_held.clone();
                async move {
                    if next.request_id == "independent" { first_held.acquire().await.unwrap().forget(); }
                    axum::Json(json!({"request_id":next.request_id,"envelope":next.envelope}))
                }
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
            let first_held = first_held.clone();
            let release = release.clone();
            let replies = replies.clone();
            let active = active.clone();
            let peak = peak.clone();
            move |Path(id): Path<String>, body: Bytes| {
                let entered = entered.clone();
                let first_held = first_held.clone();
                let release = release.clone();
                let replies = replies.clone();
                let active = active.clone();
                let peak = peak.clone();
                let delivered = delivered.clone();
                async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    if id == "held-upload" {
                        first_held.add_permits(1);
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
async fn relay_delivery_slots_bound_uploads_and_release_on_admission_cancel_and_shutdown() {
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
        for index in 1..relay::MAX_ORDINARY_DELIVERIES {
            service.schedule_work(changes_work(&service, &format!("held-{index}"), request.clone()), [7;16], &mut tasks).await.unwrap();
            entered.acquire().await.unwrap().forget();
        }
        assert_eq!(tasks.len(), relay::MAX_ORDINARY_DELIVERIES);
        assert_eq!(calls.load(Ordering::SeqCst), relay::MAX_ORDINARY_DELIVERIES);
        assert_eq!(receipts.load(Ordering::SeqCst), relay::MAX_ORDINARY_DELIVERIES);
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
        assert_eq!(service.inflight.lock().unwrap().len(), relay::MAX_ORDINARY_DELIVERIES, "dropping an admission waiter releases its reservation");
        service.schedule_work(repeat_work(&first), [7;16], &mut tasks).await.unwrap();
        assert_eq!(tasks.len(), relay::MAX_ORDINARY_DELIVERIES, "redelivery must not allocate a delivery slot or task");
        assert_eq!(calls.load(Ordering::SeqCst), relay::MAX_ORDINARY_DELIVERIES);
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        assert!(service.inflight.lock().unwrap().is_empty());
        release.add_permits(relay::MAX_ORDINARY_DELIVERIES);
        finished.acquire_many(relay::MAX_ORDINARY_DELIVERIES as u32).await.unwrap().forget();
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
    assert!(pending, "an overflow job must await delivery admission before execution or sealing");
    assert_eq!(reservations, relay::MAX_ORDINARY_DELIVERIES + 1);
    assert_eq!(accepted, 0, "an admission waiter must not acknowledge or start a durable receipt");
    assert_eq!(admitted_calls, relay::MAX_ORDINARY_DELIVERIES);
    assert_eq!(admitted_receipts, relay::MAX_ORDINARY_DELIVERIES);
    assert_eq!(calls.load(Ordering::SeqCst), relay::MAX_ORDINARY_DELIVERIES + 1);
    assert_eq!(receipts.load(Ordering::SeqCst), relay::MAX_ORDINARY_DELIVERIES + 1);
}

#[tokio::test]
async fn a_late_upload_failure_requests_resync_and_replays_the_receipt_without_another_effect() {
    const FAILED_UPLOADS: usize = UPLOAD_RETRY_DELAYS.len() + 1;
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
                    if replies.len() <= FAILED_UPLOADS { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::ACCEPTED }
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
    assert_eq!(results, vec![json!({"status":200,"body":{"saved":true}}); FAILED_UPLOADS + 1]);
    let replies = replies.lock().unwrap();
    assert!(replies.iter().all(|reply| *reply == replies[0]), "redelivery must upload the same stored ciphertext");
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
    let prepared = service.prepare_work(&work, Transport::Relay).unwrap().unwrap();
    let delivery = service.execute_relay(&work, &[7;16], prepared).await.unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn({
        let service = service.clone();
        async move { service.deliver_response(work, &[7;16], delivery).await }
    });
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
#[tokio::test]
async fn a_held_external_dispatch_does_not_block_an_independent_short_relay_read() {
    let mut samples = Vec::new();
    for _ in 0..3 { samples.push(held_dispatch_profile(false).await); }
    for (advanced, completed_before_release, _) in samples {
        assert!(advanced, "a held external dispatch must not retain ordinary relay intake");
        assert!(completed_before_release, "the independent short read must complete before external dispatch release");
    }
}

async fn held_dispatch_profile(slow_read: bool) -> (bool, bool, usize) {
    let cpu_start = cpu_micros();
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let first_held = Arc::new(tokio::sync::Semaphore::new(0));
    struct ActiveLocal(Arc<AtomicUsize>);
    impl Drop for ActiveLocal {
        fn drop(&mut self) { self.0.fetch_sub(1, Ordering::SeqCst); }
    }
    let local_active = Arc::new(AtomicUsize::new(0));
    let local_peak = Arc::new(AtomicUsize::new(0));
    let local_calls = Arc::new(AtomicUsize::new(0));
    let local = Router::new().route("/v1/options", get({
        let calls = local_calls.clone();
        let active = local_active.clone();
        let peak = local_peak.clone();
        let entered = entered.clone();
        let first_held = first_held.clone();
        let release = release.clone();
        move || {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            let active = active.clone();
            let peak = peak.clone();
            let entered = entered.clone();
            let first_held = first_held.clone();
            let release = release.clone();
            async move {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                let _active = ActiveLocal(active);
                peak.fetch_max(count, Ordering::SeqCst);
                if slow_read && call == 0 {
                    first_held.add_permits(1);
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                }
                axum::Json(json!({"protocol_version":2}))
            }
        }
    })).route("/v1/questions/1/answer", post({
        let calls = local_calls.clone();
        let entered = entered.clone();
        let first_held = first_held.clone();
        let release = release.clone();
        let active = local_active.clone();
        let peak = local_peak.clone();
        move || {
            let calls = calls.clone();
            let entered = entered.clone();
            let first_held = first_held.clone();
            let release = release.clone();
            let active = active.clone();
            let peak = peak.clone();
            async move {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                let _active = ActiveLocal(active);
                peak.fetch_max(count, Ordering::SeqCst);
                calls.fetch_add(1, Ordering::SeqCst);
                first_held.add_permits(1);
                entered.add_permits(1);
                release.acquire().await.unwrap().forget();
                axum::Json(json!({"saved":true}))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.local_port = listener.local_addr().unwrap().port();
    let local_server = tokio::spawn(axum::serve(listener, local).into_future());
    let work = Arc::new(Mutex::new(VecDeque::from([
        changes_work(&service, "held-dispatch", if slow_read { json!({"method":"GET","path":"/v1/options"}) } else { json!({"method":"POST","path":"/v1/questions/1/answer","body":{"text":"synthetic"}}) }),
        changes_work(&service, "independent", json!({"method":"GET","path":"/v1/options"})),
    ])));
    let replies = Arc::new(Mutex::new(Vec::new()));
    let receipts = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (delivered, mut completions) = tokio::sync::mpsc::unbounded_channel();
    let relay = Router::new()
        .route("/v1/bootstrap", get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([7;16])})) }))
        .route("/v1/work", get({
            let work = work.clone();
            let first_held = first_held.clone();
            move || {
                let next = work.lock().unwrap().pop_front().unwrap();
                let first_held = first_held.clone();
                async move {
                    if next.request_id == "independent" { first_held.acquire().await.unwrap().forget(); }
                    axum::Json(json!({"request_id":next.request_id,"envelope":next.envelope}))
                }
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
            let replies = replies.clone();
            let active = active.clone();
            let peak = peak.clone();
            move |Path(id): Path<String>, body: Bytes| {
                let replies = replies.clone();
                let active = active.clone();
                let peak = peak.clone();
                let delivered = delivered.clone();
                async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
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
            for id in ["held-dispatch", "independent"] {
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
    let intake_advanced = admitted.try_recv() == Ok("held-dispatch");
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
    println!("relay-dispatch profile slow_read={slow_read} offered=2 completed={} errors={} local_calls={} peak_uploads={peak} peak_local_dispatches={} reply_bytes={reply_bytes} cpu_us={} independent_before_release={independent_before_release}",
        results.len(), results.iter().filter(|value| value["status"] != 200).count(), local_calls.load(Ordering::SeqCst), local_peak.load(Ordering::SeqCst), cpu_micros() - cpu_start);
    assert_eq!(results.len(), 2);
    assert!(results.contains(&json!({"status":200,"body":{"protocol_version":2}})));
    if slow_read {
        assert!(results.iter().all(|value| value == &json!({"status":200,"body":{"protocol_version":2}})));
    } else { assert!(results.contains(&json!({"status":200,"body":{"saved":true}}))); }
    assert_eq!(receipts.load(Ordering::SeqCst), 2);
    assert_eq!(local_calls.load(Ordering::SeqCst), 2);
    assert!(service.inflight.lock().unwrap().is_empty());
    assert!(work.lock().unwrap().is_empty());
    local_server.abort();
    relay_server.abort();
    let _ = local_server.await;
    let _ = relay_server.await;
    assert_eq!(local_active.load(Ordering::SeqCst), 0);
    (intake_advanced, independent_before_release, local_peak.load(Ordering::SeqCst))
}


#[tokio::test]
async fn a_held_slow_read_does_not_block_an_independent_short_relay_read() {
    let mut samples = Vec::new();
    for _ in 0..3 { samples.push(held_dispatch_profile(true).await); }
    for (advanced, completed_before_release, peak) in samples {
        assert!(advanced, "a held slow read must not retain ordinary relay intake");
        assert!(completed_before_release, "independent read result must arrive before releasing the slow read");
        assert_eq!(peak, 2);
    }
}

#[tokio::test]
async fn a_queued_mutation_ticket_cancels_without_leaving_a_turn_or_permit() {
    let (scheduled, _) = queued_ticket_profile(true).await;
    assert!(scheduled, "a mutation waiting for its turn must leave intake owned by the scheduler");
}

#[tokio::test]
async fn a_new_lane_waiter_cannot_jump_an_earlier_scheduled_mutation() {
    let (scheduled, jumped) = queued_ticket_profile(false).await;
    assert!(scheduled, "mutation scheduling must return before its blocked turn");
    assert!(!jumped, "a new lane waiter must not jump a mutation reserved before spawn");
}

async fn queued_ticket_profile(cancel: bool) -> (bool, bool) {
    use std::{future::Future, task::Poll};
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    let replies = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let receipts = Arc::new(Mutex::new(Vec::<String>::new()));
    let relay = Router::new()
        .route("/v1/work/{id}/receipt", post({
            let receipts = receipts.clone();
            move |Path(id): Path<String>| {
                receipts.lock().unwrap().push(id);
                async { StatusCode::ACCEPTED }
            }
        }))
        .route("/v1/work/{id}/response", post({
            let replies = replies.clone();
            move |body: Bytes| {
                replies.lock().unwrap().push(body.to_vec());
                async { StatusCode::ACCEPTED }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let mut held = Some(service.lane.lock().await);
    let mut tasks = tokio::task::JoinSet::new();
    let scheduled = {
        let work = changes_work(&service, "queued-turn", json!({"method":"POST","path":"/v1/service/push-binding","body":{"push_binding":URL_SAFE_NO_PAD.encode([7;32])}}));
        let schedule = service.schedule_work(work, [7;16], &mut tasks);
        let mut schedule = std::pin::pin!(schedule);
        match std::future::poll_fn(|cx| Poll::Ready(schedule.as_mut().poll(cx))).await {
            Poll::Ready(result) => { result.unwrap(); true },
            Poll::Pending => false,
        }
    };
    let mut jumped = false;
    if cancel {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        drop(held.take());
    } else if scheduled {
        let waiter = service.lane.lock();
        let mut waiter = std::pin::pin!(waiter);
        let before = std::future::poll_fn(|cx| Poll::Ready(waiter.as_mut().poll(cx))).await;
        assert!(matches!(before, Poll::Pending));
        drop(before);
        drop(held.take());
        let after = std::future::poll_fn(|cx| Poll::Ready(waiter.as_mut().poll(cx))).await;
        jumped = matches!(after, Poll::Ready(_));
        drop(after);
    } else { drop(held.take()); }
    while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
    service.schedule_work(changes_work(&service, "fresh-turn", json!({"method":"POST","path":"/v1/service/push-binding","body":{"push_binding":URL_SAFE_NO_PAD.encode([8;32])}})), [7;16], &mut tasks).await.unwrap();
    while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
    let results = opened_results(&service, &phone, &replies);
    let expected = if scheduled && !cancel { vec!["queued-turn", "fresh-turn"] } else { vec!["fresh-turn"] };
    let received = receipts.lock().unwrap().clone();
    let reserved = service.inflight.lock().unwrap().len();
    let binding = service.journal.push_binding("phone-1").unwrap();
    server.abort();
    let _ = server.await;
    assert_eq!(received, expected);
    assert_eq!(results.len(), expected.len());
    assert!(results.iter().all(|value| value == &json!({"status":204})));
    assert_eq!(reserved, 0);
    assert_eq!(binding, Some(URL_SAFE_NO_PAD.encode([8;32])));
    (scheduled, jumped)
}

#[tokio::test]
async fn a_waiting_preference_mutation_leaves_room_for_an_independent_read() {
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let released = Arc::new(AtomicBool::new(false));
    let effect = Arc::new(AtomicUsize::new(0));
    let early_read = Arc::new(AtomicBool::new(false));
    let read_entered = Arc::new(tokio::sync::Semaphore::new(0));
    let local = Router::new()
        .route("/v1/questions/1/answer", post({
            let entered = entered.clone(); let release = release.clone(); let effect = effect.clone();
            move || {
                let entered = entered.clone(); let release = release.clone(); let effect = effect.clone();
                async move {
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    effect.store(1, Ordering::SeqCst);
                    axum::Json(json!({"saved":true}))
                }
            }
        }))
        .route("/v1/options", get({
            let released = released.clone(); let early_read = early_read.clone(); let read_entered = read_entered.clone();
            move || {
                if !released.load(Ordering::SeqCst) { early_read.store(true, Ordering::SeqCst); }
                read_entered.add_permits(1);
                async { axum::Json(json!({"short":true})) }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.local_port = listener.local_addr().unwrap().port();
    let local_server = tokio::spawn(axum::serve(listener, local).into_future());
    let work = Arc::new(Mutex::new(VecDeque::from([
        changes_work(&service, "budget-held", json!({"method":"POST","path":"/v1/questions/1/answer","body":{"text":"synthetic"}})),
        changes_work(&service, "budget-pref", json!({"method":"POST","path":"/v1/service/push-binding","body":{"push_binding":URL_SAFE_NO_PAD.encode([7;32])}})),
        changes_work(&service, "budget-read", json!({"method":"GET","path":"/v1/options"})),
    ])));
    let third = Arc::new(tokio::sync::Semaphore::new(0));
    let receipts = Arc::new(AtomicUsize::new(0));
    let early_preference = Arc::new(AtomicBool::new(false));
    let replies = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let relay = Router::new()
        .route("/v1/bootstrap", get(|| async { axum::Json(json!({"epoch":URL_SAFE_NO_PAD.encode([7;16])})) }))
        .route("/v1/work", get({
            let work = work.clone(); let third = third.clone();
            move || {
                let next = work.lock().unwrap().pop_front().unwrap();
                if next.request_id == "budget-read" { third.add_permits(1); }
                async move { axum::Json(json!({"request_id":next.request_id,"envelope":next.envelope})) }
            }
        }))
        .route("/v1/work/{id}/receipt", post({
            let receipts = receipts.clone(); let effect = effect.clone(); let early_preference = early_preference.clone();
            move |Path(id): Path<String>| {
                receipts.fetch_add(1, Ordering::SeqCst);
                if id == "budget-pref" && effect.load(Ordering::SeqCst) == 0 { early_preference.store(true, Ordering::SeqCst); }
                async { StatusCode::ACCEPTED }
            }
        }))
        .route("/v1/work/{id}/response", post({
            let replies = replies.clone();
            move |body: Bytes| {
                replies.lock().unwrap().push(body.to_vec());
                async { StatusCode::ACCEPTED }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let (scheduled, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let producer = tokio::spawn({
        let service = service.clone();
        async move {
            let mut tasks = tokio::task::JoinSet::new(); let mut epoch = None; let mut revoked = HashSet::new();
            for id in ["budget-held", "budget-pref", "budget-read"] {
                service.poll_work(&mut epoch, &mut revoked, &mut tasks).await.unwrap();
                scheduled.send(id).unwrap();
            }
            while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
        }
    });
    entered.acquire().await.unwrap().forget();
    let advanced = observed.try_recv() == Ok("budget-held");
    let mut second_scheduled = false;
    let mut receipts_at_budget = None;
    if advanced {
        second_scheduled = observed.recv().await == Some("budget-pref");
        third.acquire().await.unwrap().forget();
        read_entered.acquire().await.unwrap().forget();
        receipts_at_budget = Some(receipts.load(Ordering::SeqCst));
    }
    released.store(true, Ordering::SeqCst);
    release.add_permits(1);
    producer.await.unwrap();
    let results = opened_results(&service, &phone, &replies);
    let binding = service.journal.push_binding("phone-1").unwrap();
    let reserved = service.inflight.lock().unwrap().len();
    local_server.abort(); relay_server.abort();
    let _ = local_server.await; let _ = relay_server.await;
    assert_eq!(results.len(), 3);
    assert!(results.contains(&json!({"status":200,"body":{"saved":true}})));
    assert!(results.contains(&json!({"status":204})));
    assert!(results.contains(&json!({"status":200,"body":{"short":true}})));
    assert_eq!(effect.load(Ordering::SeqCst), 1);
    assert_eq!(binding, Some(URL_SAFE_NO_PAD.encode([7;32])));
    assert_eq!(reserved, 0);
    assert!(!early_preference.load(Ordering::SeqCst), "the queued preference must not jump the held mutation");
    assert!(early_read.load(Ordering::SeqCst), "a read must execute while a mutation runs and a preference waits for its turn");
    assert!(advanced, "held mutation execution must leave intake available");
    assert!(second_scheduled, "the preference waiting for its turn must not retain intake");
    assert_eq!(receipts_at_budget, Some(2));
}

#[tokio::test]
async fn a_saturated_changes_rejection_does_not_wait_for_the_mutation_lane() {
    use std::{future::Future, task::Poll};
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    let replies = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let relay = Router::new()
        .route("/v1/work/{id}/receipt", post(|| async { StatusCode::ACCEPTED }))
        .route("/v1/work/{id}/response", post({
            let replies = replies.clone();
            move |body: Bytes| {
                replies.lock().unwrap().push(body.to_vec());
                async { StatusCode::ACCEPTED }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let request = json!({"method":"GET","path":"/v1/changes?after=0&wait=50"});
    let live = service.prepare_work(&changes_work(&service, "active-changes", request.clone()), Transport::Relay).unwrap().unwrap();
    let mut held = Some(service.lane.lock().await);
    let mut tasks = tokio::task::JoinSet::new();
    let accepted_before_write_release = {
        let busy = service.schedule_work(changes_work(&service, "busy-changes", request), [7;16], &mut tasks);
        let mut busy = std::pin::pin!(busy);
        let pending = std::future::poll_fn(|cx| Poll::Ready(busy.as_mut().poll(cx).is_pending())).await;
        assert!(pending);
        let accepting = service.journal.accept_jobs.available_permits() == 0;
        let mut accepted: i64 = service.journal.db().unwrap().query_row(
            "SELECT count(*) FROM service_receipts WHERE sender='phone-1' AND request_id='busy-changes'", [], |row| row.get(0),
        ).unwrap();
        if accepting || accepted != 0 {
            busy.as_mut().await.unwrap();
            accepted = service.journal.db().unwrap().query_row(
                "SELECT count(*) FROM service_receipts WHERE sender='phone-1' AND request_id='busy-changes'", [], |row| row.get(0),
            ).unwrap();
            drop(held.take());
        } else {
            drop(held.take());
            busy.await.unwrap();
        }
        accepted
    };
    while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
    drop(live);
    let results = opened_results(&service, &phone, &replies);
    let reserved = service.inflight.lock().unwrap().len();
    server.abort(); let _ = server.await;
    assert_eq!(results, vec![json!({"status":429,"error":"changes_busy"})]);
    assert_eq!(reserved, 0);
    assert_eq!(accepted_before_write_release, 1, "a busy rejection must not queue behind a mutation before acknowledging its read");
}


#[tokio::test]
async fn card_action_and_background_uploads_leave_room_for_two_detail_reads() {
    use std::{future::Future, task::Poll};
    let (_directory, mut service, phone) = push_fixture();
    activate_push(&service);
    let effects = Arc::new(AtomicUsize::new(0));
    let local = Router::new()
        .route("/v1/questions/1/answer", post({
            let effects = effects.clone();
            move || {
                effects.fetch_add(1, Ordering::SeqCst);
                async { axum::Json(json!({"saved":true})) }
            }
        }))
        .route("/v1/options", get(|| async { axum::Json(json!({"detail":"independent"})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.local_port = listener.local_addr().unwrap().port();
    let local_server = tokio::spawn(axum::serve(listener, local).into_future());
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let replies = Arc::new(Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let receipts = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let relay = Router::new()
        .route("/v1/work/{id}/receipt", post({
            let receipts = receipts.clone();
            move || { receipts.fetch_add(1, Ordering::SeqCst); async { StatusCode::ACCEPTED } }
        }))
        .route("/v1/work/{id}/response", post({
            let (entered, release, replies, active, peak) = (entered.clone(), release.clone(), replies.clone(), active.clone(), peak.clone());
            move |Path(id): Path<String>, body: Bytes| {
                let (entered, release, replies, active, peak) = (entered.clone(), release.clone(), replies.clone(), active.clone(), peak.clone());
                async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    replies.lock().unwrap().push((id, body.to_vec()));
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    active.fetch_sub(1, Ordering::SeqCst);
                    StatusCode::ACCEPTED
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
    let service = Arc::new(service);
    let mut tasks = tokio::task::JoinSet::new();
    let read = json!({"method":"GET","path":"/v1/options"});
    service.schedule_work(changes_work(&service, "action", json!({"method":"POST","path":"/v1/questions/1/answer","body":{"text":"Fixture answer"}})), [7;16], &mut tasks).await.unwrap();
    service.schedule_work(changes_work(&service, "background", read.clone()), [7;16], &mut tasks).await.unwrap();
    entered.acquire_many(2).await.unwrap().forget();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let mut admitted_details = 0;
    for id in ["detail-one", "detail-two"] {
        let scheduling = service.schedule_work(changes_work(&service, id, read.clone()), [7;16], &mut tasks);
        let mut scheduling = std::pin::pin!(scheduling);
        let ready = std::future::poll_fn(|cx| Poll::Ready(scheduling.as_mut().poll(cx))).await;
        match ready {
            Poll::Ready(result) => { result.unwrap(); admitted_details += 1; }
            Poll::Pending => break,
        }
    }
    if admitted_details == 2 { entered.acquire_many(2).await.unwrap().forget(); }
    let before_release: Vec<_> = replies.lock().unwrap().iter().map(|(id, _)| id.clone()).collect();
    let fifth_waits = {
        let scheduling = service.schedule_work(changes_work(&service, "fifth", read), [7;16], &mut tasks);
        let mut scheduling = std::pin::pin!(scheduling);
        std::future::poll_fn(|cx| Poll::Ready(scheduling.as_mut().poll(cx).is_pending())).await
    };
    release.add_permits(8);
    while let Some(result) = tasks.join_next().await { result.unwrap().unwrap(); }
    let responses = replies.lock().unwrap();
    for (id, bytes) in responses.iter() {
        let envelope: Envelope = serde_json::from_slice(bytes).unwrap();
        assert_eq!(&envelope.request_id, id);
        let opened: Value = serde_json::from_slice(&envelope.open(service.signing.verifying_key(), &phone, "mac-1", "phone-1", "mac-1", now_ms()).unwrap()).unwrap();
        let expected = if id == "action" { json!({"status":200,"body":{"saved":true}}) } else { json!({"status":200,"body":{"detail":"independent"}}) };
        assert_eq!(opened, expected);
    }
    println!("card-action-detail offered=4 completed={} details_before_release={admitted_details} effects={} peak_uploads={} receipts={} response_bytes={} fifth_waits={fifth_waits}", responses.len(), effects.load(Ordering::SeqCst), peak.load(Ordering::SeqCst), receipts.load(Ordering::SeqCst), responses.iter().map(|(_, bytes)| bytes.len()).sum::<usize>());
    drop(responses);
    local_server.abort(); relay_server.abort();
    let _ = local_server.await; let _ = relay_server.await;
    assert!(service.inflight.lock().unwrap().is_empty());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert!(fifth_waits, "the worker adjustment must retain bounded admission");
    assert_eq!(admitted_details, 2, "both details must execute while the already-applied action and background upload are held");
    assert!(before_release.contains(&"detail-one".to_owned()));
    assert!(before_release.contains(&"detail-two".to_owned()));
    assert_eq!(peak.load(Ordering::SeqCst), 4);
}

/// Forwards to `target`, closing a connection instead whenever `cut` says so for the request it
/// carries: a dropped packet or a network that resets the connection mid-request.
async fn lossy_proxy(target: std::net::SocketAddr, cut: Arc<dyn Fn(&[u8]) -> bool + Send + Sync>) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let cut = cut.clone();
            tokio::spawn(async move {
                let Ok(mut upstream) = tokio::net::TcpStream::connect(target).await else { return };
                let (mut client_read, mut client_write) = client.split();
                let (mut upstream_read, mut upstream_write) = upstream.split();
                let forward = async {
                    let mut buffer = vec![0u8; 64 * 1024];
                    loop {
                        let read = client_read.read(&mut buffer).await.unwrap_or(0);
                        if read == 0 || cut(&buffer[..read]) { return; }
                        if upstream_write.write_all(&buffer[..read]).await.is_err() { return; }
                    }
                };
                tokio::select! {
                    () = forward => {}
                    _ = tokio::io::copy(&mut upstream_read, &mut client_write) => {}
                }
            });
        }
    });
    address
}

async fn upload_through(statuses: Vec<StatusCode>, cut_first_upload: bool) -> (bool, bool, usize, usize, bool) {
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
    let statuses = Arc::new(Mutex::new(VecDeque::from(statuses)));
    let relay = Router::new()
        .route("/v1/work/{id}/receipt", post(|| async { StatusCode::ACCEPTED }))
        .route("/v1/work/{id}/response", post({
            let (replies, statuses) = (replies.clone(), statuses.clone());
            move |body: Bytes| {
                let (replies, statuses) = (replies.clone(), statuses.clone());
                async move {
                    replies.lock().unwrap().push(body.to_vec());
                    statuses.lock().unwrap().pop_front().unwrap_or(StatusCode::ACCEPTED)
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_address = listener.local_addr().unwrap();
    let relay_server = tokio::spawn(axum::serve(listener, relay).into_future());
    let cuts = Arc::new(AtomicUsize::new(usize::from(cut_first_upload)));
    let proxy = lossy_proxy(relay_address, Arc::new({
        let cuts = cuts.clone();
        move |bytes: &[u8]| {
            let upload = bytes.starts_with(b"POST /v1/work/") && bytes.windows(9).any(|w| w == b"/response");
            upload && cuts.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1)).is_ok()
        }
    })).await;
    service.set_relay_url(Url::parse(&format!("http://{proxy}/")).unwrap());
    let service = Arc::new(service);
    let work = changes_work(&service, "lossy-upload", json!({"method":"POST","path":"/v1/questions/1/answer","body":{"text":"fixture"}}));
    let mut tasks = tokio::task::JoinSet::new();
    service.schedule_work(repeat_work(&work), [7;16], &mut tasks).await.unwrap();
    let delivered = tasks.join_next().await.unwrap().unwrap().is_ok();
    let resynced = service.relay.as_ref().unwrap().resync_pending();
    let results = opened_results(&service, &phone, &replies);
    let same_bytes = {
        let replies = replies.lock().unwrap();
        replies.iter().all(|reply| *reply == replies[0])
    };
    assert!(results.iter().all(|result| *result == json!({"status":200,"body":{"saved":true}})));
    local_server.abort();
    relay_server.abort();
    let _ = local_server.await;
    let _ = relay_server.await;
    (delivered, resynced, effects.load(Ordering::SeqCst), results.len(), same_bytes && cuts.load(Ordering::SeqCst) == 0)
}

#[tokio::test]
async fn a_response_upload_cut_by_the_network_or_a_busy_relay_is_resent_until_it_lands() {
    let (delivered, resynced, effects, uploads, same_bytes) =
        upload_through(vec![StatusCode::SERVICE_UNAVAILABLE, StatusCode::TOO_MANY_REQUESTS], true).await;
    assert!(delivered, "a passing outage must not fail the delivery");
    assert!(!resynced, "a passing outage must not drop the work poll for a new epoch");
    assert_eq!(effects, 1);
    assert_eq!(uploads, 3, "the cut upload never reached the relay; two busy answers, then it landed");
    assert!(same_bytes, "every attempt resends the same sealed response");
}

#[tokio::test]
async fn a_response_the_relay_refuses_is_not_resent() {
    let (delivered, resynced, effects, uploads, _) = upload_through(vec![StatusCode::CONFLICT], false).await;
    assert!(!delivered);
    assert!(resynced, "a refusal still forces a fresh epoch");
    assert_eq!(effects, 1);
    assert_eq!(uploads, 1);
}

/// An HTTP/2 relay front that, like a proxy which already forwarded the first copy, records the
/// first work poll's nonce as used and then refuses its stream. A nonce it sees twice is a replay,
/// refused with 401 exactly as the relay does.
async fn refusing_relay_front() -> (std::net::SocketAddr, Arc<Mutex<Vec<(String, String)>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let refused = Arc::new(AtomicBool::new(false));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let (log, refused) = (log.clone(), refused.clone());
            tokio::spawn(async move {
                let Ok(mut connection) = h2::server::handshake(socket).await else { return };
                while let Some(Ok((request, mut respond))) = connection.accept().await {
                    let path = request.uri().path().to_owned();
                    let reply = |status: StatusCode| axum::http::Response::builder().status(status).body(()).unwrap();
                    if path == "/v1/bootstrap" {
                        let body = serde_json::to_vec(&json!({"epoch":URL_SAFE_NO_PAD.encode([7; 16])})).unwrap();
                        let mut stream = respond.send_response(reply(StatusCode::OK), false).unwrap();
                        stream.send_data(body.into(), true).unwrap();
                        continue;
                    }
                    let nonce = request.headers().get("x-device-nonce")
                        .and_then(|value| value.to_str().ok()).unwrap_or_default().to_owned();
                    let replayed = {
                        let mut log = log.lock().unwrap();
                        let replayed = log.iter().any(|(_, used)| *used == nonce);
                        log.push((path.clone(), nonce));
                        replayed
                    };
                    if path == "/v1/work" && !refused.swap(true, Ordering::SeqCst) {
                        respond.send_reset(h2::Reason::REFUSED_STREAM);
                    } else if replayed {
                        let _ = respond.send_response(reply(StatusCode::UNAUTHORIZED), true);
                    } else if path == "/v1/work" {
                        let _ = respond.send_response(reply(StatusCode::NO_CONTENT), true);
                    } else {
                        let _ = respond.send_response(reply(StatusCode::OK), true);
                    }
                }
            });
        }
    });
    (address, seen)
}

#[tokio::test]
async fn a_refused_work_poll_is_signed_again_instead_of_replayed_into_a_401() {
    let (_directory, mut service, _phone) = push_fixture();
    let (relay, seen) = refusing_relay_front().await;
    service.set_relay_url(Url::parse(&format!("http://{relay}/")).unwrap());
    service.client = super::http_client().http2_prior_knowledge().build().unwrap();
    let service = Arc::new(service);
    let (mut epoch, mut revoked, mut tasks) = (None, HashSet::new(), tokio::task::JoinSet::new());
    let refused = service.poll_work(&mut epoch, &mut revoked, &mut tasks).await;
    let next = service.poll_work(&mut epoch, &mut revoked, &mut tasks).await;
    let seen = seen.lock().unwrap().clone();
    let polls: Vec<&String> = seen.iter().filter(|(path, _)| path == "/v1/work").map(|(_, nonce)| nonce).collect();
    assert!(polls.windows(2).all(|pair| pair[0] != pair[1]),
        "every work poll that reaches the relay carries a fresh nonce: {polls:?}");
    let refusal = refused.expect_err("the refused stream fails this poll");
    let relay_status = refusal.chain()
        .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .find_map(reqwest::Error::status);
    assert_ne!(relay_status, Some(StatusCode::UNAUTHORIZED),
        "the refused poll must not come back as a relay 401: {refusal:#}");
    next.expect("the next poll, signed again, reaches the relay");
    assert_eq!(polls.len(), 2, "one refused copy and one freshly signed poll: {seen:?}");
}
