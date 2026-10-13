use super::*;
use axum::body::to_bytes;
use std::io::{BufRead, Write};
use std::sync::atomic::AtomicBool;
use tokio::sync::Notify;

struct Harness {
    hold: AtomicBool,
    release: Notify,
    actions: AtomicU64,
    reads: AtomicU64,
    bootstraps: AtomicU64,
}
fn emit(value: Value) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "ALONDRA_FIXTURE {}", value).unwrap();
    out.flush().unwrap();
}
async fn barrier(
    State(harness): State<Arc<Harness>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if request.method() == Method::GET && path == "/v1/bootstrap" {
        harness.bootstraps.fetch_add(1, Ordering::Relaxed);
    }
    let upload = request.method() == Method::POST
        && path.starts_with("/v1/work/")
        && path.ends_with("/response");
    let request = if request.method() == Method::POST && path == "/v1/requests" {
        let (parts, body) = request.into_parts();
        let bytes = to_bytes(body, state::MAX_RESPONSE_BYTES).await.unwrap();
        let envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        if envelope.request_id.starts_with("op_") {
            harness.actions.fetch_add(1, Ordering::Relaxed);
        } else {
            harness.reads.fetch_add(1, Ordering::Relaxed);
        }
        Request::from_parts(parts, Body::from(bytes))
    } else {
        request
    };
    if upload {
        let id = path
            .strip_prefix("/v1/work/")
            .unwrap()
            .strip_suffix("/response")
            .unwrap();
        if id.starts_with("op_") && harness.hold.swap(false, Ordering::Relaxed) {
            let released = harness.release.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            emit(json!({"event":"applied","command_id":id}));
            released.await;
        }
    }
    next.run(request).await
}

#[ignore = "stdin-controlled real relay for the separate client process E2E target"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_stack_fixture_relay() {
    let directory = crate::test_support::Directory::new();
    std::fs::create_dir_all(&directory).unwrap();
    let mac = SigningKey::from_slice(&[1u8; 32]).unwrap();
    let mac_agreement = SecretKey::from_slice(&[2u8; 32]).unwrap();
    let phone = SigningKey::from_slice(&[33u8; 32]).unwrap();
    let config = Config {
        listen: "127.0.0.1:0".into(),
        issuer: "https://id.example/realms/alondra".into(),
        audience: "alondra-relay".into(),
        jwks_url: "http://127.0.0.1:1/jwks".into(),
        allow_internal_http_jwks: false,
        owner_sub: "owner".into(),
        mac_id: "mac-1".into(),
        mac_signing_key: key_bytes(&mac),
        device_registry_path: directory.join("registry.json"),
        apns: None,
    };
    let auth = Auth::test(config, include_bytes!("../tests/fixtures/jwks.json")).unwrap();
    auth.enroll_mac(auth::MacDevice {
        mac_id: "mac-1".into(),
        owner_sub: "owner".into(),
        signing_key: key_bytes(&mac),
        agreement_key: URL_SAFE_NO_PAD.encode(mac_agreement.public_key().to_sec1_bytes()),
    })
    .unwrap();
    // Enrollment is preapproved fixture setup. The Mac still runs the real connector enrollment protocol.
    auth.enroll(
        &auth::Identity {
            device_id: "mac-1".into(),
            owner_sub: "owner".into(),
        },
        PhoneDevice {
            device_id: "phone-1".into(),
            owner_sub: "owner".into(),
            signing_key: key_bytes(&phone),
        },
    )
    .unwrap();
    let epoch = URL_SAFE_NO_PAD.encode(auth.server_epoch());
    let server = Arc::new(Server {
        lab_waiters: Arc::default(),
        auth,
        management_keycloak: None,
        web_session: None,
        web_assets: None,
        apns: None,
        queue: Queue::new(),
        pairing: pairing::Mailbox::default(),
        work_waiters: Arc::new(Semaphore::new(16)),
        change_waiters: Arc::new(Semaphore::new(64)),
        status_waiters: Arc::new(Semaphore::new(16)),
        requests: Arc::new(Semaphore::new(128)),
        device_gate: Arc::new(RwLock::new(())),
        waitlist: management::waitlist::Limiter::new(None),
        epoch,
        test_directory: Some(directory),
    });
    let harness = Arc::new(Harness {
        hold: AtomicBool::new(false),
        release: Notify::new(),
        actions: AtomicU64::new(0),
        reads: AtomicU64::new(0),
        bootstraps: AtomicU64::new(0),
    });
    let routes =
        router(server.clone()).layer(middleware::from_fn_with_state(harness.clone(), barrier));
    let port = std::env::var("ALONDRA_FIXTURE_RELAY_PORT")
        .map(|value| value.parse::<u16>().unwrap())
        .unwrap_or(0);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(async move {
        axum::serve(
            listener,
            routes.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    emit(
        json!({"event":"ready","base_url":base,"bearer":jwt("owner",now_ms()),"expired_bearer":jwt("owner",0),"mac_id":"mac-1","device_id":"phone-1",
        "signing_seed_base64":base64::engine::general_purpose::STANDARD.encode([33u8;32]),
        "agreement_seed_base64":base64::engine::general_purpose::STANDARD.encode([34u8;32]),
        "mac_signing_seed_base64":base64::engine::general_purpose::STANDARD.encode([1u8;32]),
        "mac_agreement_seed_base64":base64::engine::general_purpose::STANDARD.encode([2u8;32])}),
    );
    let (commands, mut incoming) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let reader = std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let stopped = value["command"] == "stop";
            if commands.send(value).is_err() || stopped {
                break;
            }
        }
    });
    while let Some(command) = incoming.recv().await {
        match command["command"].as_str().unwrap() {
            "hold_next_action" => {
                harness.hold.store(true, Ordering::Relaxed);
                emit(json!({"event":"armed","kind":"action"}));
            }
            "release" => {
                harness.release.notify_waiters();
                emit(json!({"event":"released"}));
            }
            "stats" => emit(
                json!({"event":"stats","action_posts":harness.actions.load(Ordering::Relaxed),"read_posts":harness.reads.load(Ordering::Relaxed),"bootstrap_gets":harness.bootstraps.load(Ordering::Relaxed)}),
            ),
            "expire_action" => {
                let id = command["request_id"].as_str().unwrap();
                let job = server.queue.get("owner", "phone-1", id, now_ms().saturating_add(120_001)).unwrap();
                assert_eq!(job.status, state::JobStatus::Expired);
                emit(json!({"event":"expired","command_id":id}));
            }
            "stop" => break,
            other => panic!("unexpected relay fixture command {other}"),
        }
    }
    harness.release.notify_waiters();
    let _ = stop.send(());
    // Cancel only waits admitted by this fixture before graceful shutdown.
    {
        let _gate=server.device_gate.write().await;
        server.auth.revoke_mac(&auth::Identity {device_id:"mac-1".into(),owner_sub:"owner".into()}).unwrap();
        server.queue.notify_mac_revoked("mac-1");
    }
    server.queue.notify_phone_revoked("phone-1");
    serving.await.unwrap();
    reader.join().unwrap();
    emit(json!({"event":"stopped"}));
    drop(server);
    drop(harness);
}
