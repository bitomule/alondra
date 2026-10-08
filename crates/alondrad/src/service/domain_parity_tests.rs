//! The retained signed loopback and sealed service resource reads see the same
//! domain state. Everything here runs in a synthetic HOME and loopback relay.
use super::*;
use reqwest::StatusCode;
use alondra_core::domain::{
    Actor, Command, CommandEnvelope, InteractionPayload, PROTOCOL_VERSION, QuestionOption,
    Recipient, Scope,
};
use axum::{Router, body::Bytes, routing::post};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature, signature::Signer};
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    sync::Barrier,
    sync::atomic::{AtomicI64, Ordering},
};

fn execute(hub: &crate::hub::Hub, actor: Actor, id: &str, command: Command) {
    let state = hub.domain.state().unwrap();
    hub.domain
        .execute(
            &actor,
            &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
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
    let sender = &service.devices.snapshot().remove(0).0;
    Work {
        request_id: id.into(),
        envelope: Envelope::seal_random(
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
    let envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
    let recipient = &service.devices.snapshot().remove(0).0;
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
    let scratch_directory = crate::test_support::Directory::new();
    use std::os::unix::fs::PermissionsExt;
    let home = scratch_directory.parent().unwrap().join(format!(
        "alondra-domain-parity-{}",
        crate::auth::random_hex(8)
    ));
    std::fs::create_dir_all(&home).unwrap();
    let config = alondra_core::config::Config::resolve(
        home.clone(),
        &Default::default(),
        |_| None,
        || Some("Fixture".into()),
    );
    let hub = Arc::new(crate::hub::Hub::open(config).unwrap());
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
            body: "Synthetic request".into(), auto_merge: false, maintenance: false, message: false,
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
        Actor::System,
        "info",
        Command::PublishInformation {
            interaction_id: "info-parity".into(),
            scope: Scope::Task {
                task_id: "task-parity".into(),
            },
            recipient: Recipient::User,
            body: "Synthetic information".into(),
            attachments: vec![], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
        },
    );
    execute(
        &hub,
        Actor::Agent {
            agent_id: "agent-parity".into(),
        },
        "question",
        Command::AskQuestion {
            interaction_id: "question-parity".into(),
            scope: Scope::Task {
                task_id: "task-parity".into(),
            },
            recipient: Recipient::User,
            body: "Choose one".into(),
            blocking: true,
            options: ["alpha", "beta"]
                .into_iter()
                .map(|id| QuestionOption {
                    id: id.into(),
                    label: id.into(),
                    description: None,
                    inputs: vec![],
                })
                .collect(),
            attachments: vec![], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
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

    let app = crate::domain_api::routes::<crate::hub::Hub>()
    .with_state(hub.clone())
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
    let domain = hub.domain.clone();
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
    let epoch = domain.state().unwrap().data_epoch.0.clone();
    let paths = [
        "/capabilities".to_owned(),
        "/document-revisions/rev-parity/content?offset=0".to_owned(),
        "/document-revisions/missing/content?offset=0".to_owned(),
        "/pull-requests/missing/diff".to_owned(),
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
    // This fixture intentionally has a v1 local Hub and a v2-only encrypted
    // service. Schedule reads must expose the active authority of each mode.
    let local_schedule = local_get(&client, &base, &phone_key, device_id,
        "/schedules").await;
    assert_eq!(local_schedule["status"], 409);
    let remote_schedule = encrypted(&service, &phone_key, &phone_agreement, &responses,
        "schedule-v2-read", json!({"method":"GET","path":"/schedules"})).await;
    assert_eq!(remote_schedule["status"], 200);
    let legacy_service = Service::open(&home, 0, auth.clone()).unwrap().unwrap().with_hub(hub.clone());
    let before_schedule = domain.state().unwrap();
    let blocked = encrypted(&legacy_service, &phone_key, &phone_agreement, &responses,
        "schedule-mixed-blocked", json!({"method":"POST","path":"/schedules",
            "body":{"title":"Review","text":"Review project","frequency":"daily",
                "hour":9,"minute":0}})).await;
    assert_eq!(blocked["status"], 409, "{blocked}");
    assert_eq!(blocked["body"]["code"], "incompatible_protocol", "{blocked}");
    assert_eq!(domain.state().unwrap(), before_schedule);
    // A legacy daemon must never write its v2 sidecar: a non-empty store blocks
    // both cutover and legacy startup. Reads stay open, writes are typed rejections.
    let legacy_command = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: before_schedule.data_epoch.clone(),
        command_id: "legacy-read-once".into(),
        expected_revision: before_schedule.revision,
        command: Command::MarkRead {
            interaction_id: "info-parity".into(),
        },
    };
    let blocked_command = sealed(&legacy_service, &phone_key, "legacy-command-blocked",
        json!({"method":"POST","path":"/commands","body":legacy_command}));
    assert!(legacy_service.prepare_work(&blocked_command, Transport::Relay).is_err(),
        "generic command paths are rejected before dispatch");
    assert_eq!(domain.state().unwrap(), before_schedule);
    let blocked_resolve = encrypted(&legacy_service, &phone_key, &phone_agreement, &responses,
        "legacy-resolve-blocked", json!({"method":"POST",
            "path":"/delivery-attempts/1/resolve",
            "body":{"protocol_version":2,"data_epoch":before_schedule.data_epoch,
                "expected_version":1,"decision":"retry"}})).await;
    assert_eq!(blocked_resolve["status"], 409);
    assert_eq!(blocked_resolve["body"]["code"], "incompatible_protocol");
    assert_eq!(domain.state().unwrap(), before_schedule);
    assert_eq!(local_get(&client, &base, &phone_key, device_id, "/capabilities").await["status"], 200);

    let state = domain.state().unwrap();
    let command = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "read-parity-once".into(),
        expected_revision: state.revision,
        command: Command::MarkRead {
            interaction_id: "info-parity".into(),
        },
    };
    let request = json!({"method":"PUT","path":"/interactions/info-parity/read-state","body":{
        "data_epoch":command.data_epoch,
        "command_id":command.command_id,
        "expected_revision":command.expected_revision,
        "interaction_id":"info-parity"
    }});
    let work = sealed(&service, &phone_key, "command-once", request.clone());
    let ciphertext_digest = Sha256::digest(serde_json::to_vec(&work.envelope).unwrap()).to_vec();
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
    let receipts = service.journal.db().unwrap();
    let persisted: (String, Vec<u8>, String) = receipts
        .query_row(
            "SELECT sender,digest,state FROM service_receipts WHERE request_id='command-once'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        persisted,
        (device_id.into(), ciphertext_digest, "completed".into())
    );
    let receipt_count: i64 = receipts
        .query_row(
            "SELECT count(*) FROM service_receipts WHERE request_id='command-once'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipt_count, 1);
    drop(receipts);
    let envelope: Envelope = serde_json::from_slice(&first_bytes).unwrap();
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
    let pending_after_read = domain
        .store
        .lock()
        .unwrap()
        .pending_messages()
        .unwrap()
        .len();
    let question_revision = domain.state().unwrap().revision;
    let barrier = Arc::new(Barrier::new(3));
    let decisions = ["alpha", "beta"]
        .into_iter()
        .map(|option| {
            let domain = domain.clone();
            let barrier = barrier.clone();
            let envelope = CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: domain.state().unwrap().data_epoch.clone(),
                command_id: format!("answer-{option}").into(),
                expected_revision: question_revision,
                command: Command::AnswerQuestion {
                    interaction_id: "question-parity".into(),
                    option_id: option.into(),
                    inputs: Default::default(),
                    attachments: vec![],
                },
            };
            tokio::task::spawn_blocking(move || {
                let body = serde_json::to_value(&envelope).unwrap();
                barrier.wait();
                let result = crate::domain_api::encrypted_command_runtime(
                    &domain,
                    "phone-parity",
                    Some(&body),
                    ("phone-parity", option, &[option.len() as u8; 32]),
                    || Ok(()),
                )
                .unwrap();
                (option, envelope, result)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let mut results = Vec::new();
    for decision in decisions {
        results.push(decision.await.unwrap());
    }
    let winners = results
        .iter()
        .filter(|(_, _, value)| value["status"] == 200)
        .collect::<Vec<_>>();
    let losers = results
        .iter()
        .filter(|(_, _, value)| value["status"] == 409)
        .collect::<Vec<_>>();
    assert_eq!(
        winners.len(),
        1,
        "exactly one competing answer must commit: {results:?}"
    );
    assert_eq!(
        losers.len(),
        1,
        "exactly one competing answer must conflict: {results:?}"
    );
    assert_eq!(losers[0].2["body"]["code"], "stale_revision");
    let winner = winners[0];
    let answered_state = domain.state().unwrap();
    let answer = &answered_state.interactions[&"question-parity".into()].payload;
    assert!(
        matches!(answer, InteractionPayload::Question { answer: Some(a), .. } if a.option_id == winner.0)
    );
    assert_eq!(domain.state().unwrap().revision, question_revision + 1);
    assert_eq!(
        domain
            .store
            .lock()
            .unwrap()
            .pending_messages()
            .unwrap()
            .len(),
        pending_after_read + 1
    );
    assert!(
        domain
            .store
            .lock()
            .unwrap()
            .delivery_evidence()
            .unwrap()
            .is_empty()
    );
    let winner_body = serde_json::to_value(&winner.1).unwrap();
    assert_eq!(
        crate::domain_api::encrypted_command_runtime(
            &domain,
            "phone-parity",
            Some(&winner_body),
            ("phone-parity", winner.0, &[winner.0.len() as u8; 32]),
            || Ok(())
        )
        .unwrap(),
        winner.2,
        "retrying the successful decision must return its receipt without a second effect"
    );
    assert_eq!(
        domain
            .store
            .lock()
            .unwrap()
            .pending_messages()
            .unwrap()
            .len(),
        pending_after_read + 1
    );
    let other_sender = crate::domain_api::encrypted_command_runtime(
        &domain,
        "phone-other",
        Some(&winner_body),
        ("phone-other", winner.0, &[winner.0.len() as u8; 32]),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(other_sender["status"], 409);
    assert_eq!(
        other_sender["body"]["code"], "stale_revision",
        "another sender cannot inherit the first sender's semantic receipt"
    );
    assert_eq!(
        domain
            .store
            .lock()
            .unwrap()
            .pending_messages()
            .unwrap()
            .len(),
        pending_after_read + 1
    );
    let updated_path = "/capabilities";
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

    let revision_before_old_clients = domain.state().unwrap().revision;
    let stale = encrypted(&service, &phone_key, &phone_agreement, &responses,
        "stale-epoch", json!({"method":"PUT","path":"/interactions/info-parity/read-state",
            "body":{"data_epoch":"wrong-epoch","command_id":"stale-read",
                "expected_revision":0,"interaction_id":"info-parity"}})).await;
    assert_eq!(stale["status"], 409);
    assert_eq!(stale["body"]["code"], "stale_epoch");
    let old_protocol = encrypted(&service, &phone_key, &phone_agreement, &responses,
        "old-protocol", json!({"method":"PUT","path":"/interactions/info-parity/read-state",
            "body":{"protocol_version":1,"data_epoch":epoch,"command_id":"old-read",
                "expected_revision":0,"interaction_id":"info-parity"}})).await;
    assert_eq!(old_protocol["status"], 422);
    assert_eq!(old_protocol["body"]["code"], "invalid_input");
    assert_eq!(
        domain.state().unwrap().revision,
        revision_before_old_clients
    );
    auth.revoke(device_id).unwrap();
    assert_eq!(
        local_get(&client, &base, &phone_key, device_id, "/capabilities").await["status"],
        401
    );
    assert!(
        service
            .prepare_work(&sealed(
                &service,
                &phone_key,
                "after-revoke",
                json!({"method":"GET","path":"/capabilities"})
            ), Transport::Relay)
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
