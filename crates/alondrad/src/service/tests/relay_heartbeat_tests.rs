use super::*;

/// A relay that answers every route with 200, records every path it serves, and hands out the
/// epoch byte held in `epoch`.
async fn relay_front(epoch: Arc<std::sync::atomic::AtomicU8>) -> (Url, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
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
async fn the_heartbeat_keeps_enrollment_without_asking_for_the_mailbox() {
    let (_directory, mut service, _) = push_fixture();
    let (url, seen, server) = relay_front(Arc::new(7.into())).await;
    service.set_relay_url(url);
    let (mut epoch, mut revoked) = (None, HashSet::new());
    for _ in 0..3 {
        service.keep_registered(&mut epoch, &mut revoked).await.unwrap();
    }
    assert_eq!(count(&seen, "POST /v1/devices"), 1, "the approved phone is enrolled once: {seen:?}");
    assert_eq!(count(&seen, "GET /v1/bootstrap"), 3, "each heartbeat looks for a new epoch: {seen:?}");
    assert!(seen.lock().unwrap().iter().all(|line| !line.contains("/v1/work")), "{seen:?}");
    assert_eq!(epoch, Some([7; 16]));
    server.abort();
}

#[tokio::test]
async fn a_restarted_relay_is_followed_to_its_new_epoch_without_enrolling_again() {
    let (_directory, mut service, _) = push_fixture();
    let relay_epoch = Arc::new(std::sync::atomic::AtomicU8::new(7));
    let (url, seen, server) = relay_front(relay_epoch.clone()).await;
    service.set_relay_url(url);
    let (mut epoch, mut revoked) = (None, HashSet::new());
    service.keep_registered(&mut epoch, &mut revoked).await.unwrap();
    relay_epoch.store(8, Ordering::SeqCst);
    service.keep_registered(&mut epoch, &mut revoked).await.unwrap();
    assert_eq!(epoch, Some([8; 16]));
    assert_eq!(count(&seen, "POST /v1/devices"), 1, "the relay keeps its registry across a restart: {seen:?}");
    server.abort();
}
