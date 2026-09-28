//! V41: the signed loopback reader and the sealed service reader see the same
//! domain state. Everything here runs in a synthetic HOME and loopback relay.
use super::*;
use alondra_core::domain_v2::{
    Actor, Command, CommandEnvelope, PROTOCOL_VERSION, Recipient, Scope,
};
use axum::{Router, body::Bytes, routing::post};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature, signature::Signer};
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    sync::atomic::{AtomicI64, Ordering},
};

fn execute(hub: &crate::hub::Hub, actor: Actor, id: &str, command: Command) {
    let state = hub.domain.state().unwrap();
    hub.domain
        .execute(
            &actor,
            &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch,
                command_id: id.into(),
                expected_revision: state.revision,
                command,
            },
            None,
            || Ok(()),
        )
        .unwrap();
}

async fn local_get(
    client: &Client,
    base: &str,
    key: &SigningKey,
    device: &str,
    path: &str,
) -> Value {
    static TIMESTAMP: AtomicI64 = AtomicI64::new(0);
    let now = alondra_core::now_millis();
    let timestamp = loop {
        let old = TIMESTAMP.load(Ordering::SeqCst);
        let next = now.max(old + 1);
        if TIMESTAMP
            .compare_exchange(old, next, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            break next;
        }
    };
    let message = crate::auth::signed_message("GET", path, &timestamp.to_string(), b"");
    let signature: Signature = key.sign(message.as_bytes());
    let response = client
        .get(format!("{base}{path}"))
        .header("x-alondra-device", device)
        .header("x-alondra-timestamp", timestamp.to_string())
        .header(
            "x-alondra-signature",
            STANDARD.encode(signature.to_der().as_bytes()),
        )
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    json!({"status":status,"body":response.json::<Value>().await.unwrap()})
}

fn sealed(service: &Service, key: &SigningKey, id: &str, request: Value) -> Work {
    let sender = service.devices.keys().next().unwrap();
    Work {
        request_id: id.into(),
        envelope: EnvelopeV1::seal_random(
            EnvelopeContext {
                route: "mac-parity",
                sender,
                recipient: "mac-parity",
                request_id: id,
                kind: "request",
                expires_at_ms: now_ms() + 60_000,
            },
            &serde_json::to_vec(&request).unwrap(),
            key,
            &service.agreement.public_key(),
        )
        .unwrap(),
    }
}

async fn encrypted(
    service: &Service,
    key: &SigningKey,
    agreement: &SecretKey,
    responses: &Arc<Mutex<Vec<Vec<u8>>>>,
    id: &str,
    request: Value,
) -> Value {
    let work = sealed(service, key, id, request);
    service.handle(work, &[9; 16]).await.unwrap();
    let bytes = responses.lock().unwrap().pop().unwrap();
    let envelope: EnvelopeV1 = serde_json::from_slice(&bytes).unwrap();
    let recipient = service.devices.keys().next().unwrap();
    let body = envelope
        .open(
            service.signing.verifying_key(),
            agreement,
            "mac-parity",
            recipient,
            "mac-parity",
            now_ms(),
        )
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn signed_loopback_and_sealed_service_share_reads_errors_and_command_receipts() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::temp_dir().join(format!(
        "alondra-domain-v2-parity-{}",
        crate::auth::random_hex(8)
    ));
    std::fs::create_dir_all(&home).unwrap();
    let config = alondra_core::config::Config::resolve(
        home.clone(),
        &Default::default(),
        |_| None,
        || Some("Fixture".into()),
    );
    let hub = Arc::new(crate::hub::Hub::open(config, false).unwrap());
    execute(
        &hub,
        Actor::System,
        "agent",
        Command::RegisterAgent {
            agent_id: "agent-parity".into(),
            name: "Synthetic Agent".into(),
        },
    );
    let user = Actor::User {
        user_id: "fixture".into(),
    };
    execute(
        &hub,
        user.clone(),
        "request",
        Command::CreateRequest {
            request_id: "request-parity".into(),
            body: "Synthetic request".into(),
        },
    );
    execute(
        &hub,
        user,
        "task",
        Command::CreateTask {
            task_id: "task-parity".into(),
            request_id: "request-parity".into(),
            title: "Synthetic task".into(),
            agent_id: Some("agent-parity".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: None,
        },
    );
    execute(
        &hub,
        Actor::Agent {
            agent_id: "agent-parity".into(),
        },
        "info",
        Command::PublishInformation {
            interaction_id: "info-parity".into(),
            scope: Scope::Task {
                task_id: "task-parity".into(),
            },
            recipient: Recipient::User,
            body: "Synthetic information".into(),
            attachments: vec![],
        },
    );
    let source = b"# Synthetic design\n";
    std::fs::create_dir_all(home.join("files")).unwrap();
    std::fs::write(home.join("files/design.md"), source).unwrap();
    let digest = Sha256::digest(source)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    execute(
        &hub,
        Actor::Agent {
            agent_id: "agent-parity".into(),
        },
        "document",
        Command::CreateDocument {
            task_id: "task-parity".into(),
            document_id: "doc-parity".into(),
            title: "Synthetic design".into(),
            revision_id: "rev-parity".into(),
            content_digest: digest,
            content_reference: "files/design.md".into(),
            role: "result".into(),
        },
    );

    let phone_key = SigningKey::from_slice(&[31; 32]).unwrap();
    let phone_agreement = SecretKey::from_slice(&[32; 32]).unwrap();
    let mac_signing = SigningKey::from_slice(&[33; 32]).unwrap();
    let auth = Arc::new(Auth::open(&home).unwrap());
    let device_id = "phone-parity";
    let signing_wire = URL_SAFE_NO_PAD.encode(phone_key.verifying_key().to_sec1_bytes());
    let agreement_wire = URL_SAFE_NO_PAD.encode(phone_agreement.public_key().to_sec1_bytes());
    auth.approve_service(device_id, "Synthetic phone", &signing_wire)
        .unwrap();
    for (name, bytes) in [
        ("mac-signing.key", [33u8; 32]),
        ("mac-agreement.key", [34u8; 32]),
    ] {
        let path = home.join(name);
        std::fs::write(&path, URL_SAFE_NO_PAD.encode(bytes)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let app = crate::http::router(
        hub.clone(),
        auth.clone(),
        Arc::new(crate::push::Push::open(&home).unwrap()),
    )
    .layer(axum::middleware::from_fn_with_state(
        auth.clone(),
        crate::auth::guard,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let local = tokio::spawn(
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .into_future(),
    );
    let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let relay = Router::new()
        .route(
            "/v1/work/{id}/receipt",
            post(|| async { StatusCode::ACCEPTED }),
        )
        .route(
            "/v1/work/{id}/response",
            post({
                let responses = responses.clone();
                move |body: Bytes| {
                    let responses = responses.clone();
                    async move {
                        responses.lock().unwrap().push(body.to_vec());
                        StatusCode::ACCEPTED
                    }
                }
            }),
        );
    let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("http://{}/", relay_listener.local_addr().unwrap());
    let relay_task = tokio::spawn(axum::serve(relay_listener, relay).into_future());
    std::fs::write(home.join("service.toml"), format!(
        "relay_url = \"{relay_url}\"\nmac_id = \"mac-parity\"\nowner_sub = \"owner\"\nsigning_key_path = \"mac-signing.key\"\nagreement_key_path = \"mac-agreement.key\"\n[[devices]]\ndevice_id = \"{}\"\nsigning_key = \"{signing_wire}\"\nagreement_key = \"{agreement_wire}\"\n",
        device_id,
    )).unwrap();
    let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
    let service = Service::open(&home, 0, auth.clone())
        .unwrap()
        .unwrap()
        .with_domain(domain.clone());
    service
        .journal
        .reserve_enrollment(device_id, &phone_key.verifying_key().to_sec1_bytes())
        .unwrap();
    service.journal.activate_enrollment(device_id).unwrap();
    let client = Client::new();

    let initial_revision = domain.state().unwrap().revision;
    let epoch = domain.state().unwrap().data_epoch.0;
    let paths = [
        "/v2/domain/capabilities".to_owned(),
        "/v2/domain".to_owned(),
        "/v2/domain/tasks/task-parity/actions".to_owned(),
        "/v2/domain/tasks/missing/actions".to_owned(),
        "/v2/domain/document-revisions/rev-parity/content?offset=0".to_owned(),
        "/v2/domain/document-revisions/missing/content?offset=0".to_owned(),
        format!("/v2/domain/changes?data_epoch={epoch}&after={initial_revision}&wait=0"),
        "/v2/domain/delivery-attempts?after=0".to_owned(),
    ];
    for (index, path) in paths.iter().enumerate() {
        let local = local_get(&client, &base, &phone_key, device_id, path).await;
        let remote = encrypted(
            &service,
            &phone_key,
            &phone_agreement,
            &responses,
            &format!("read-{index}"),
            json!({"method":"GET","path":path}),
        )
        .await;
        assert_eq!(remote, local, "local/encrypted mismatch for {path}");
    }

    let state = domain.state().unwrap();
    let command = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch,
        command_id: "read-parity-once".into(),
        expected_revision: state.revision,
        command: Command::MarkRead {
            interaction_id: "info-parity".into(),
        },
    };
    let request = json!({"method":"POST","path":"/v2/domain/commands","body":command});
    let work = sealed(&service, &phone_key, "command-once", request.clone());
    service
        .handle(
            Work {
                request_id: work.request_id.clone(),
                envelope: work.envelope.clone(),
            },
            &[9; 16],
        )
        .await
        .unwrap();
    let first_bytes = responses.lock().unwrap().pop().unwrap();
    service.handle(work, &[9; 16]).await.unwrap();
    let replay_bytes = responses.lock().unwrap().pop().unwrap();
    assert_eq!(
        first_bytes, replay_bytes,
        "sealed retry must reuse the exact durable receipt"
    );
    let envelope: EnvelopeV1 = serde_json::from_slice(&first_bytes).unwrap();
    let opened = envelope
        .open(
            mac_signing.verifying_key(),
            &phone_agreement,
            "mac-parity",
            device_id,
            "mac-parity",
            now_ms(),
        )
        .unwrap();
    let outcome: Value = serde_json::from_slice(&opened).unwrap();
    assert_eq!(outcome["status"], 200);
    assert_eq!(domain.state().unwrap().revision, initial_revision + 1);
    let updated_path = "/v2/domain";
    assert_eq!(
        local_get(&client, &base, &phone_key, device_id, updated_path).await,
        encrypted(
            &service,
            &phone_key,
            &phone_agreement,
            &responses,
            "updated-projection",
            json!({"method":"GET","path":updated_path})
        )
        .await
    );

    let stale = encrypted(&service, &phone_key, &phone_agreement, &responses,
        "stale-epoch", json!({"method":"POST","path":"/v2/domain/commands",
            "body":{"protocol_version":2,"data_epoch":"wrong-epoch","command_id":"stale",
                "expected_revision":0,"command":{"kind":"mark_read","interaction_id":"info-parity"}}})).await;
    assert_eq!(stale["status"], 409);
    assert_eq!(domain.state().unwrap().revision, initial_revision + 1);
    auth.revoke(device_id).unwrap();
    assert_eq!(
        local_get(&client, &base, &phone_key, device_id, "/v2/domain").await["status"],
        401
    );
    assert!(
        service
            .prepare_work(&sealed(
                &service,
                &phone_key,
                "after-revoke",
                json!({"method":"GET","path":"/v2/domain"})
            ))
            .is_err()
    );
    local.abort();
    relay_task.abort();
    let _ = local.await;
    let _ = relay_task.await;
    drop(service);
    drop(domain);
    drop(hub);
    drop(auth);
    std::fs::remove_dir_all(home).unwrap();
}
