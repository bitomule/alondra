use super::*;

async fn registration_response(service: &Service, phone: &SecretKey, id: &str, path: &str, body: Value) -> Value {
    let work = changes_work(service, id, json!({"method":"POST","path":path,"body":body}));
    let (bytes, _lease) = service.handle_direct(work.envelope).await.unwrap();
    let envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
    serde_json::from_slice(&envelope.open(service.signing.verifying_key(), phone,
        "mac-1", "phone-1", "mac-1", now_ms()).unwrap()).unwrap()
}

#[tokio::test]
async fn registration_after_a_forgotten_binding_recovers_with_a_newer_revision() {
    let (_directory, service, phone) = push_fixture();
    activate_push(&service);
    let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&service.home).unwrap());
    let service = service.with_domain(domain);
    let binding = URL_SAFE_NO_PAD.encode([4u8; 32]);
    let body = json!({"push_binding":binding,"preferences_revision":1,"quiet_start":null,"quiet_end":null});
    let accepted = registration_response(&service, &phone, "first", PUSH_BINDING_ROUTE, body.clone()).await;
    assert_eq!(accepted["status"], 204);
    service.journal.forget_push_binding("phone-1", &binding).unwrap();
    assert_eq!(service.journal.db().unwrap().query_row("SELECT revision FROM service_push_preference_revisions WHERE device_id='phone-1'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);

    // Foreground registration resends the same snapshot and revision even if APNs dropped it.
    let stale = registration_response(&service, &phone, "return", PUSH_BINDING_ROUTE, body.clone()).await;
    assert_eq!(stale["status"], 409, "{stale}");
    assert_eq!(stale["body"]["kind"], "stale_push_preferences", "{stale}");
    assert_eq!(stale["body"]["current_revision"], 1);
    assert!(service.journal.push_binding("phone-1").unwrap().is_none(), "the stale snapshot must not reactivate a removed binding");

    let mut newer = body;
    newer["preferences_revision"] = json!(2);
    let recovered = registration_response(&service, &phone, "retry-newer", PUSH_BINDING_ROUTE, newer).await;
    assert_eq!(recovered["status"], 204, "{recovered}");
    assert_eq!(service.journal.push_binding("phone-1").unwrap(), Some(binding));
}

#[test]
fn registration_rejections_keep_the_typed_reason_on_the_wire_and_in_the_log() {
    const PROBE: &str = "ALONDRA_REGISTRATION_LOG_PROBE";
    if std::env::var_os(PROBE).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "service::tests::registration_tests::registration_rejections_keep_the_typed_reason_on_the_wire_and_in_the_log", "--nocapture"])
            .env(PROBE, "1").output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{}\n{stderr}", String::from_utf8_lossy(&output.stdout));
        for (operation, status, reason) in [
            ("push_binding", 422, "invalid_push_binding"),
            ("push_binding", 422, "invalid_quiet_hours"),
            ("push_binding", 422, "invalid_preferences_revision"),
            ("push_binding", 422, "unknown_registration_field"),
            ("push_binding", 409, "stale_push_preferences"),
            ("activity_key", 422, "invalid_activity_key"),
            ("activity_key", 409, "device_not_enrolled"),
            ("push_binding", 409, "device_not_enrolled"),
            ("push_binding", 404, "unsupported_registration_route"),
            ("push_binding", 400, "device_approval_revoked"),
        ] {
            let expected = format!("device registration rejected: operation={operation} transport=Direct status={status} reason={reason}");
            assert!(stderr.contains(&expected), "missing {expected:?}: {stderr}");
        }
        for private in ["phone-1", "mac-1", "private-field", "private-value", "not-a-key", "bad-binding"] {
            assert!(!stderr.contains(private), "registration logs leaked {private}: {stderr}");
        }
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let (_directory, service, phone) = push_fixture();
        activate_push(&service);
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&service.home).unwrap());
        let service = service.with_domain(domain);
        let binding = URL_SAFE_NO_PAD.encode([4u8; 32]);
        for (id, body, reason) in [
            ("binding", json!({"push_binding":"bad-binding"}), "invalid_push_binding"),
            ("quiet", json!({"push_binding":binding,"quiet_start":"bad","quiet_end":null}), "invalid_quiet_hours"),
            ("revision", json!({"push_binding":binding,"preferences_revision":0}), "invalid_preferences_revision"),
            ("field", json!({"push_binding":binding,"private-field":"private-value"}), "unknown_registration_field"),
        ] {
            let response = registration_response(&service, &phone, id, PUSH_BINDING_ROUTE, body).await;
            assert_eq!(response["status"], 422, "{response}");
            assert_eq!(response["body"]["kind"], reason, "{response}");
        }
        let accepted = registration_response(&service, &phone, "accepted", PUSH_BINDING_ROUTE,
            json!({"push_binding":binding,"preferences_revision":2,"quiet_start":null,"quiet_end":null})).await;
        assert_eq!(accepted["status"], 204);
        let stale = registration_response(&service, &phone, "stale", PUSH_BINDING_ROUTE,
            json!({"push_binding":binding,"preferences_revision":1,"quiet_start":null,"quiet_end":null})).await;
        assert_eq!(stale["status"], 409);
        assert_eq!(stale["body"]["kind"], "stale_push_preferences");
        assert_eq!(stale["body"]["current_revision"], 2);
        assert_eq!(stale["current_revision"], 2, "legacy response fields remain available");
        let key = registration_response(&service, &phone, "bad-key", ACTIVITY_KEY_ROUTE, json!({"public_key":"not-a-key"})).await;
        assert_eq!(key["body"]["kind"], "invalid_activity_key");

        let (_directory, service, phone) = push_fixture();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&service.home).unwrap());
        let service = service.with_domain(domain);
        let public_key = URL_SAFE_NO_PAD.encode(phone.public_key().to_sec1_bytes());
        for (id, path, body) in [
            ("unenrolled-key", ACTIVITY_KEY_ROUTE, json!({"public_key":public_key})),
            ("unenrolled-binding", PUSH_BINDING_ROUTE, json!({"push_binding":binding})),
        ] {
            let response = registration_response(&service, &phone, id, path, body).await;
            assert_eq!(response["status"], 409, "{response}");
            assert_eq!(response["body"]["kind"], "device_not_enrolled", "{response}");
        }
        assert!(service.journal.push_binding("phone-1").unwrap().is_none());
        assert!(service.journal.activity_key("phone-1").unwrap().is_none());
        activate_push(&service);
        let recovered = registration_response(&service, &phone, "enrolled", PUSH_BINDING_ROUTE, json!({"push_binding":binding})).await;
        assert_eq!(recovered["status"], 204);

        let (_directory, service, phone) = push_fixture();
        let unsupported = registration_response(&service, &phone, "unsupported", PUSH_BINDING_ROUTE, json!({"push_binding":binding})).await;
        assert_eq!(unsupported["status"], 404);
        assert_eq!(unsupported["body"]["kind"], "unsupported_registration_route");

        let (_directory, mut service, _) = push_fixture();
        activate_push(&service);
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&service.home).unwrap());
        service = service.with_domain(domain);
        let home = service.home.clone();
        service.journal.before_write = Some(Arc::new(move |stage, _| {
            let home = home.clone();
            Box::pin(async move {
                if stage == ReceiptWrite::PushPreferences {
                    Auth::open(&home).unwrap().revoke("phone-1").unwrap();
                }
            })
        }));
        let work = changes_work(&service, "queued-revocation", json!({"method":"POST","path":PUSH_BINDING_ROUTE,
            "body":{"push_binding":binding,"preferences_revision":1,"quiet_start":null,"quiet_end":null}}));
        assert!(matches!(service.handle_direct(work.envelope).await, Err(DirectRefusal::Rejected(_))));
        assert_eq!(service.journal.db().unwrap().query_row("SELECT count(*) FROM service_push_preference_revisions", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    });
}
