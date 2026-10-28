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

#[tokio::test]
async fn the_live_activity_report_goes_signed_to_its_route_sealed_for_the_phone_with_its_flags() {
    let (_directory, mut service, phone_agreement) = push_fixture();
    activate_push(&service);
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(std::sync::atomic::AtomicU16::new(202));
    let relay = Router::new().fallback({
        let (bodies, answer) = (bodies.clone(), answer.clone());
        move |request: axum::http::Request<axum::body::Body>| {
            let (bodies, answer) = (bodies.clone(), answer.clone());
            async move {
                let (parts, body) = request.into_parts();
                match parts.uri.path() {
                    "/v1/bootstrap" => axum::Json(json!({"epoch": URL_SAFE_NO_PAD.encode([7u8; 16])})).into_response(),
                    "/v1/mac/activity" => {
                        assert!(parts.headers.contains_key("x-device-signature"), "signed by the Mac");
                        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                        bodies.lock().unwrap().push(serde_json::from_slice::<Value>(&bytes).unwrap());
                        let status = StatusCode::from_u16(answer.load(Ordering::SeqCst)).unwrap();
                        (status, axum::Json(json!({"error": "activity_registration_gone"}))).into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, relay).await.unwrap() });

    let phones = service.activity_phones().unwrap();
    assert_eq!(phones, vec![("phone-1".to_owned(), URL_SAFE_NO_PAD.encode([4u8; 32]))]);
    let (phone, binding) = &phones[0];
    let payload = json!({"inbox": 3, "waiting": 1, "working": 2, "mac_name": "Mac mini", "measured_at_ms": 1, "seq": 2});
    let flags = crate::activity_push::Flags { active: true, may_start: true, urgent: false };
    let mut epoch = None;
    let (status, _) = service.send_device_activity(phone, binding, &payload, flags, &mut epoch).await.unwrap();
    assert_eq!(crate::activity_push::sent(status, ""), crate::activity_push::Sent::Accepted);
    answer.store(410, Ordering::SeqCst);
    let (status, code) = service.send_device_activity(phone, binding, &payload, flags, &mut epoch).await.unwrap();
    assert_eq!(crate::activity_push::sent(status, &code), crate::activity_push::Sent::Gone);

    let sent = bodies.lock().unwrap()[0].clone();
    assert_eq!((sent["device_id"].clone(), sent["active"].clone(), sent["may_start"].clone(), sent["urgent"].clone()),
        (json!("phone-1"), json!(true), json!(true), json!(false)));
    let envelope: alondra_e2e::Envelope = serde_json::from_value(sent["envelope"].clone()).unwrap();
    let signing = SigningKey::from_slice(&[1u8; 32]).unwrap();
    let opened: Value = serde_json::from_slice(&envelope.open(signing.verifying_key(), &phone_agreement, "mac-1", "phone-1", "mac-1", envelope.expires_at_ms).unwrap()).unwrap();
    assert_eq!(opened, json!({"push_binding": binding, "payload": payload}));
    server.abort();
}

#[test]
fn a_new_live_activity_never_starts_in_the_phones_quiet_hours() {
    let (_directory, service, _) = push_fixture();
    activate_push(&service);
    let binding = URL_SAFE_NO_PAD.encode([4u8; 32]);
    let quiet = QuietHours::parse_update(Some(&json!("23:00")), Some(&json!("08:00"))).unwrap();
    service.journal.set_push_preferences("phone-1", &binding, quiet).unwrap();
    for (minute, may_start) in [(22 * 60 + 59, true), (23 * 60, false), (3 * 60, false), (8 * 60 - 1, false), (8 * 60, true), (12 * 60, true)] {
        assert_eq!(service.may_start_activity("phone-1", minute).unwrap(), may_start, "at minute {minute}");
    }
}
