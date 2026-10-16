use super::*;

/// A relay that answers the work poll with `work` and every other route with 200, records every
/// path it serves, and hands out the epoch byte held in `epoch`.
async fn relay_front(
    work: StatusCode,
    epoch: Arc<std::sync::atomic::AtomicU8>,
) -> (Url, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let relay = Router::new().fallback({
        let seen = seen.clone();
        move |request: axum::http::Request<axum::body::Body>| {
            let (seen, epoch) = (seen.clone(), epoch.clone());
            async move {
                let path = request.uri().path().to_owned();
                seen.lock().unwrap().push(format!("{} {path}", request.method()));
                match path.as_str() {
                    "/v1/bootstrap" => {
                        let epoch = [epoch.load(Ordering::SeqCst); 16];
                        axum::Json(json!({"epoch": URL_SAFE_NO_PAD.encode(epoch)})).into_response()
                    }
                    "/v1/work" => work.into_response(),
                    _ => StatusCode::OK.into_response(),
                }
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, relay).await.unwrap() });
    (url, seen, server)
}

fn count(seen: &Mutex<Vec<String>>, route: &str) -> usize {
    seen.lock().unwrap().iter().filter(|line| *line == route).count()
}

#[tokio::test]
async fn a_relay_without_the_mailbox_leaves_a_heartbeat_that_keeps_enrollment() {
    for missing in [StatusCode::NOT_FOUND, StatusCode::METHOD_NOT_ALLOWED] {
        let (_directory, mut service, _) = push_fixture();
        let (url, seen, server) = relay_front(missing, Arc::new(7.into())).await;
        service.set_relay_url(url);
        let service = Arc::new(service);
        let (mut epoch, mut revoked, mut tasks) = (None, HashSet::new(), tokio::task::JoinSet::new());
        for _ in 0..3 {
            service.poll_work(&mut epoch, &mut revoked, &mut tasks).await
                .unwrap_or_else(|error| panic!("{missing}: a relay without the mailbox is no failure: {error:#}"));
        }
        assert_eq!(count(&seen, "GET /v1/work"), 1, "{missing}: asked once, then only the heartbeat: {seen:?}");
        assert_eq!(count(&seen, "POST /v1/devices"), 1, "{missing}: the approved phone is enrolled: {seen:?}");
        assert!(count(&seen, "GET /v1/bootstrap") >= 3, "{missing}: each heartbeat looks for a new epoch: {seen:?}");
        assert_eq!(epoch, Some([7; 16]));
        server.abort();
    }
}

#[tokio::test]
async fn a_restarted_relay_is_asked_for_the_mailbox_again() {
    let (_directory, mut service, _) = push_fixture();
    let relay_epoch = Arc::new(std::sync::atomic::AtomicU8::new(7));
    let (url, seen, server) = relay_front(StatusCode::NOT_FOUND, relay_epoch.clone()).await;
    service.set_relay_url(url);
    let service = Arc::new(service);
    let (mut epoch, mut revoked, mut tasks) = (None, HashSet::new(), tokio::task::JoinSet::new());
    service.poll_work(&mut epoch, &mut revoked, &mut tasks).await.unwrap();
    relay_epoch.store(8, Ordering::SeqCst);
    service.poll_work(&mut epoch, &mut revoked, &mut tasks).await.unwrap();
    assert_eq!(epoch, None, "the heartbeat saw the new epoch");
    service.poll_work(&mut epoch, &mut revoked, &mut tasks).await.unwrap();
    assert_eq!(epoch, Some([8; 16]));
    assert_eq!(count(&seen, "GET /v1/work"), 2, "the restarted relay is asked again: {seen:?}");
    server.abort();
}

#[tokio::test]
async fn every_request_through_the_mailbox_leaves_one_line_without_its_content() {
    let (_directory, service, _) = push_fixture();
    activate_push(&service);
    let service = Arc::new(service);
    let mut tasks = tokio::task::JoinSet::new();
    let requests = [
        ("read", json!({"method":"GET","path":"/capabilities"})),
        ("action", json!({"method":"POST","path":"/v1/service/push-binding","body":{"push_binding":URL_SAFE_NO_PAD.encode([9;32])}})),
        ("wait", json!({"method":"GET","path":"/v1/changes?after=0&wait=0"})),
    ];
    for (id, request) in requests {
        service.schedule_work(changes_work(&service, id, request), [7; 16], &mut tasks).await.unwrap();
    }
    tasks.abort_all();
    let lines = service.relay.as_ref().unwrap().mailbox_uses.lock().unwrap().clone();
    assert_eq!(lines, vec![
        "mailbox read from phone-1: GET /capabilities",
        "mailbox action from phone-1: POST /v1/service/push-binding",
        "mailbox change wait from phone-1: GET /v1/changes",
    ]);
    assert!(lines.iter().all(|line| !line.contains(&URL_SAFE_NO_PAD.encode([9;32]))));
}
