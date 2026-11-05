// The Live Activity routes end to end: real signatures and envelopes, a fake APNs.
mod activity_http_tests {
    use super::*;
    use std::sync::Mutex;

    const PHONE_ID: &str = "phone-install-0123456789";
    const T: u64 = 1_790_000_000_000;

    #[derive(Debug, Clone)]
    struct Pushed {
        token: String,
        headers: HeaderMap,
        body: Value,
    }

    struct Fixture {
        url: String,
        server: Arc<Server>,
        epoch: [u8; 16],
        client: reqwest::Client,
        pushed: Arc<Mutex<Vec<Pushed>>>,
        nonce: std::sync::atomic::AtomicU8,
        tasks: Vec<tokio::task::JoinHandle<()>>,
        macs: Vec<(String, SigningKey)>,
        phone_sign: SigningKey,
        phone_ecdh: SecretKey,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for task in &self.tasks {
                task.abort();
            }
        }
    }

    fn start_token() -> String { "aa".repeat(80) }
    fn update_token() -> String { "bb".repeat(80) }
    fn dead_token() -> String { "dd".repeat(80) }

    impl Fixture {
        async fn new() -> Self {
            let pushed = Arc::new(Mutex::new(Vec::<Pushed>::new()));
            let captured = pushed.clone();
            let apns = Router::new().route(
                "/3/device/{token}",
                post(move |Path(token): Path<String>, headers: HeaderMap, Json(body): Json<Value>| {
                    let captured = captured.clone();
                    async move {
                        let dead = token == dead_token();
                        captured.lock().unwrap().push(Pushed { token, headers, body });
                        if dead {
                            (StatusCode::GONE, Json(json!({"reason":"Unregistered"}))).into_response()
                        } else {
                            StatusCode::OK.into_response()
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let apns_url = format!("http://{}", listener.local_addr().unwrap());
            let apns_task = tokio::spawn(async move { axum::serve(listener, apns).await.unwrap() });
            let mac_1 = SigningKey::from_slice(&[31u8; 32]).unwrap();
            let mac_2 = SigningKey::from_slice(&[32u8; 32]).unwrap();
            let phone_sign = SigningKey::from_slice(&[33u8; 32]).unwrap();
            let phone_ecdh = SecretKey::from_slice(&[34u8; 32]).unwrap();
            let (url, server, relay_task) = test_server_with_apns(&mac_1, Some(apns_url)).await;
            server
                .auth
                .enroll_mac(auth::MacDevice {
                    mac_id: "mac-2".into(),
                    owner_sub: "owner".into(),
                    signing_key: key_bytes(&mac_2),
                    agreement_key: key_bytes(&mac_2),
                })
                .unwrap();
            for (mac, phone) in [("mac-1", "client-a"), ("mac-2", "client-b")] {
                server
                    .auth
                    .enroll(
                        &auth::Identity { device_id: mac.into(), owner_sub: "owner".into() },
                        PhoneDevice { device_id: phone.into(), owner_sub: "owner".into(), signing_key: key_bytes(&phone_sign) },
                    )
                    .unwrap();
            }
            server.activity.set_clock(T);
            Fixture {
                url,
                epoch: server.auth.server_epoch(),
                server,
                client: reqwest::Client::new(),
                pushed,
                nonce: std::sync::atomic::AtomicU8::new(1),
                tasks: vec![apns_task, relay_task],
                macs: vec![("mac-1".into(), mac_1), ("mac-2".into(), mac_2)],
                phone_sign,
                phone_ecdh,
            }
        }

        fn nonce(&self) -> u8 {
            self.nonce.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }

        fn mac_key(&self, mac: &str) -> &SigningKey {
            &self.macs.iter().find(|(id, _)| id == mac).unwrap().1
        }

        async fn phone(&self, device: &str, path: &str, body: &Value, key: &SigningKey, sub: &str) -> StatusCode {
            let bytes = serde_json::to_vec(body).unwrap();
            let request = self.client.post(format!("{}{path}", self.url)).bearer_auth(jwt(sub, now_ms())).body(bytes.clone());
            sign(&self.epoch, request, key, device, "POST", path, &bytes, self.nonce()).send().await.unwrap().status()
        }

        async fn register(&self, device: &str, body: Value) -> StatusCode {
            self.phone(device, "/v1/activity/register", &body, &self.phone_sign, "owner").await
        }

        fn envelope(&self, mac: &str, phone: &str, kind: &str, expires_at_ms: u64, signer: &SigningKey) -> Envelope {
            let plaintext = serde_json::to_vec(&json!({
                "push_binding": URL_SAFE_NO_PAD.encode([7u8; 32]),
                "payload": {"inbox": 3, "waiting": 1, "working": 2, "mac_name": "Mac mini", "measured_at_ms": now_ms(), "seq": now_ms()},
            }))
            .unwrap();
            let request_id = URL_SAFE_NO_PAD.encode([self.nonce(); 16]);
            Envelope::seal_random(
                Context { route: mac, sender: mac, recipient: phone, request_id: &request_id, kind, expires_at_ms },
                &plaintext,
                signer,
                &self.phone_ecdh.public_key(),
            )
            .unwrap()
        }

        async fn send(&self, mac: &str, body: &Value) -> (StatusCode, Value) {
            let bytes = serde_json::to_vec(body).unwrap();
            let path = "/v1/mac/activity";
            let request = self.client.post(format!("{}{path}", self.url)).body(bytes.clone());
            let response = sign(&self.epoch, request, self.mac_key(mac), mac, "POST", path, &bytes, self.nonce()).send().await.unwrap();
            let status = response.status();
            (status, response.json().await.unwrap_or(Value::Null))
        }

        async fn report(&self, mac: &str, phone: &str, active: bool, may_start: bool, urgent: bool) -> StatusCode {
            let envelope = self.envelope(mac, phone, "activity", now_ms() + 120_000, self.mac_key(mac));
            self.send(mac, &json!({"device_id": phone, "envelope": envelope, "active": active, "may_start": may_start, "urgent": urgent})).await.0
        }

        fn take_pushed(&self) -> Vec<Pushed> {
            std::mem::take(&mut *self.pushed.lock().unwrap())
        }

        fn at(&self, offset_ms: u64) {
            self.server.activity.set_clock(T + offset_ms);
        }
    }

    fn registration(update: Option<String>) -> Value {
        let mut body = json!({"phone_id": PHONE_ID, "start_token": start_token(), "environment": "sandbox", "frequent_updates": true});
        if let Some(token) = update {
            body["update_token"] = json!(token);
            body["activity_id"] = json!("8E2B5D0C-1F44-4D9A-9B1E-2C3D4E5F6A7B");
        }
        body
    }

    fn header<'a>(push: &'a Pushed, name: &str) -> &'a str {
        push.headers.get(name).unwrap().to_str().unwrap()
    }

    fn macs(push: &Pushed) -> Vec<String> {
        push.body["aps"]["content-state"]["macs"].as_array().unwrap().iter().map(|e| e["sender"].as_str().unwrap().to_owned()).collect()
    }

    #[tokio::test]
    async fn registration_is_signed_validated_grouped_by_phone_and_kept_apart_from_the_device_registry() {
        let f = Fixture::new().await;
        let registry_before = std::fs::read(&f.server.auth.config.device_registry_path).unwrap();
        let body = registration(None);
        assert_eq!(f.phone("client-a", "/v1/activity/register", &body, &f.phone_sign, "intruder").await, StatusCode::UNAUTHORIZED);
        let wrong_key = SigningKey::from_slice(&[35u8; 32]).unwrap();
        assert_eq!(f.phone("client-a", "/v1/activity/register", &body, &wrong_key, "owner").await, StatusCode::UNAUTHORIZED);
        for invalid in [
            json!({"phone_id": PHONE_ID, "start_token": "ab".repeat(31), "environment": "sandbox", "frequent_updates": true}),
            json!({"phone_id": PHONE_ID, "start_token": "ab".repeat(257), "environment": "sandbox", "frequent_updates": true}),
            json!({"phone_id": PHONE_ID, "start_token": start_token(), "environment": "staging", "frequent_updates": true}),
            json!({"phone_id": "short", "environment": "sandbox", "frequent_updates": true}),
        ] {
            assert_eq!(f.register("client-a", invalid.clone()).await, StatusCode::BAD_REQUEST, "{invalid}");
        }
        assert_eq!(f.register("client-a", body.clone()).await, StatusCode::NO_CONTENT);
        assert_eq!(f.register("client-b", body).await, StatusCode::NO_CONTENT);
        let phone = f.server.activity.phone(PHONE_ID).unwrap();
        assert_eq!(
            phone.pairings,
            [("client-a".to_owned(), "mac-1".to_owned()), ("client-b".to_owned(), "mac-2".to_owned())].into(),
            "both pairings of the same iPhone feed one Live Activity"
        );
        assert_eq!(phone.start_token, Some(start_token()));
        assert_eq!(
            std::fs::read(&f.server.auth.config.device_registry_path).unwrap(),
            registry_before,
            "a relay from before live-activity.json starts on the same device registry"
        );
        let reopened = activity::Store::open(&f.server.auth.config.device_registry_path).unwrap();
        assert_eq!(reopened.path(), f.server.activity.path());
        assert!(reopened.path().ends_with("live-activity.json"));
        assert_eq!(reopened.phone(PHONE_ID), Some(phone), "a restarted relay reads the same phone");

        assert_eq!(
            f.phone("client-a", "/v1/activity/unregister", &json!({"phone_id": PHONE_ID}), &wrong_key, "owner").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            f.phone("client-a", "/v1/activity/unregister", &json!({"phone_id": PHONE_ID}), &f.phone_sign, "owner").await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(f.server.activity.phone(PHONE_ID), None);
        let (status, body) = {
            let envelope = f.envelope("mac-2", "client-b", "activity", now_ms() + 120_000, f.mac_key("mac-2"));
            f.send("mac-2", &json!({"device_id": "client-b", "envelope": envelope, "active": true, "may_start": true, "urgent": false})).await
        };
        assert_eq!((status, body), (StatusCode::GONE, json!({"error": "activity_registration_gone"})), "the daemon stops that phone");
        assert_eq!(f.register("client-b", registration(None)).await, StatusCode::NO_CONTENT);
        assert_eq!(f.report("mac-2", "client-b", false, true, false).await, StatusCode::ACCEPTED, "registering again lets it back in");
    }

    #[tokio::test]
    async fn mac_reports_are_checked_and_only_the_latest_of_each_mac_is_kept() {
        let f = Fixture::new().await;
        assert_eq!(f.register("client-a", registration(None)).await, StatusCode::NO_CONTENT);
        let valid = |envelope: Envelope, device: &str| json!({"device_id": device, "envelope": envelope, "active": true, "may_start": false, "urgent": false});
        let mac_1 = f.mac_key("mac-1");
        let forger = SigningKey::from_slice(&[36u8; 32]).unwrap();
        for (body, expected) in [
            (valid(f.envelope("mac-1", "client-a", "push", now_ms() + 120_000, mac_1), "client-a"), "invalid_activity_route"),
            (valid(f.envelope("mac-1", "client-b", "activity", now_ms() + 120_000, mac_1), "client-a"), "invalid_activity_route"),
            (valid(f.envelope("mac-1", "client-a", "activity", now_ms() - 1, mac_1), "client-a"), "invalid_activity_route"),
            (valid(f.envelope("mac-1", "client-a", "activity", now_ms() + MAX_PUSH_LIFETIME_MS + 60_000, mac_1), "client-a"), "invalid_activity_route"),
            (valid(f.envelope("mac-1", "client-a", "activity", now_ms() + 120_000, &forger), "client-a"), "invalid_envelope_signature"),
        ] {
            assert_eq!(f.send("mac-1", &body).await, (StatusCode::BAD_REQUEST, json!({"error": expected})));
        }
        let foreign = valid(f.envelope("mac-1", "client-b", "activity", now_ms() + 120_000, mac_1), "client-b");
        assert_eq!(
            f.send("mac-1", &foreign).await,
            (StatusCode::GONE, json!({"error": "activity_registration_gone"})),
            "a phone paired with another Mac; never a 404, which puts the daemon to sleep"
        );
        let wrong_mac = f.envelope("mac-1", "client-a", "activity", now_ms() + 120_000, mac_1);
        let bytes = serde_json::to_vec(&valid(wrong_mac, "client-a")).unwrap();
        let request = f.client.post(format!("{}/v1/mac/activity", f.url)).body(bytes.clone());
        let status = sign(&f.epoch, request, &forger, "mac-1", "POST", "/v1/mac/activity", &bytes, f.nonce()).send().await.unwrap().status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "the request itself is signed by the Mac");

        assert_eq!(f.report("mac-1", "client-a", true, false, false).await, StatusCode::ACCEPTED);
        let first = f.server.activity.phone(PHONE_ID).unwrap().reports["mac-1"].envelope.clone();
        f.at(5_000);
        assert_eq!(f.report("mac-1", "client-a", true, false, false).await, StatusCode::ACCEPTED);
        let phone = f.server.activity.phone(PHONE_ID).unwrap();
        assert_eq!(phone.reports.len(), 1, "only the latest report of each Mac");
        assert_ne!(phone.reports["mac-1"].envelope, first);
        assert!(f.take_pushed().is_empty(), "quiet hours: nothing starts");

        assert_eq!(f.report("mac-2", "client-b", true, true, false).await, StatusCode::ACCEPTED, "held until its phone registers");
        assert_eq!(f.register("client-b", registration(None)).await, StatusCode::NO_CONTENT);
        assert_eq!(f.server.activity.phone(PHONE_ID).unwrap().reports.len(), 2);
    }

    #[tokio::test]
    async fn the_live_activity_starts_updates_rotates_and_ends_through_apns() {
        let f = Fixture::new().await;
        for device in ["client-a", "client-b"] {
            assert_eq!(f.register(device, registration(None)).await, StatusCode::NO_CONTENT);
        }
        assert_eq!(f.report("mac-1", "client-a", true, false, false).await, StatusCode::ACCEPTED);
        assert!(f.take_pushed().is_empty(), "never in quiet hours");

        f.at(10_000);
        assert_eq!(f.report("mac-1", "client-a", true, true, false).await, StatusCode::ACCEPTED);
        let pushed = f.take_pushed();
        assert_eq!(pushed.len(), 1);
        let start = &pushed[0];
        assert_eq!(start.body["aps"]["content-state"]["omitted"], 0);
        assert_eq!(start.token, start_token());
        assert_eq!(header(start, "apns-push-type"), "liveactivity");
        assert_eq!(header(start, "apns-topic"), "com.bitomule.alondra.push-type.liveactivity");
        assert_eq!(header(start, "apns-priority"), "5");
        assert_eq!(start.body["aps"]["event"], "start");
        assert_eq!(start.body["aps"]["attributes-type"], "AlondraActivityAttributes");
        assert_eq!(start.body["aps"]["attributes"], json!({"started_at_ms": T + 10_000}));
        assert_eq!(start.body["aps"]["timestamp"], (T + 10_000) / 1000);
        assert_eq!(start.body["aps"]["stale-date"], (T + 10_000 + 30 * 60_000) / 1000);
        assert_eq!(start.body["aps"]["alert"], json!({"title": {"loc-key": "Alondra"}, "body": {"loc-key": "Work in progress"}}));
        assert_eq!(macs(start), ["mac-1"]);

        f.at(3 * 60_000);
        assert_eq!(f.report("mac-1", "client-a", true, true, false).await, StatusCode::ACCEPTED);
        assert!(f.take_pushed().is_empty(), "one started already; the app has not registered its token yet");

        f.server.activity.set_clock(T + 4 * 60_000);
        let mut with_token = registration(Some(update_token()));
        with_token["started_at_ms"] = json!(T + 10_000);
        assert_eq!(f.register("client-a", with_token).await, StatusCode::NO_CONTENT);
        assert_eq!(f.report("mac-2", "client-b", true, true, true).await, StatusCode::ACCEPTED);
        let pushed = f.take_pushed();
        assert_eq!(pushed.len(), 1);
        assert_eq!((pushed[0].token.as_str(), pushed[0].body["aps"]["event"].as_str()), (update_token().as_str(), Some("update")));
        assert_eq!(header(&pushed[0], "apns-priority"), "10", "the Inbox grew");
        assert_eq!(macs(&pushed[0]), ["mac-2", "mac-1"], "every Mac's latest envelope, newest first");

        f.at(4 * 60_000 + 30_000);
        assert_eq!(f.report("mac-1", "client-a", true, true, false).await, StatusCode::ACCEPTED);
        assert!(f.take_pushed().is_empty(), "an update waits a minute after the last push");
        f.at(5 * 60_000);
        assert_eq!(f.report("mac-2", "client-b", true, true, true).await, StatusCode::ACCEPTED);
        assert_eq!(header(&f.take_pushed()[0], "apns-priority"), "5", "one priority 10 every five minutes at most");
        f.at(6 * 60_000);
        assert_eq!(f.report("mac-1", "client-a", true, true, false).await, StatusCode::ACCEPTED);
        assert_eq!(header(&f.take_pushed()[0], "apns-priority"), "5");

        let rotate = 10_000 + activity::ROTATE_AFTER_MS;
        f.at(rotate);
        assert_eq!(f.report("mac-1", "client-a", true, true, false).await, StatusCode::ACCEPTED);
        let pushed = f.take_pushed();
        let events: Vec<_> = pushed.iter().map(|p| (p.token.clone(), p.body["aps"]["event"].as_str().unwrap().to_owned())).collect();
        assert_eq!(events, [(update_token(), "end".to_owned()), (start_token(), "start".to_owned())], "rotates at 7 h 45 min");

        let mut dead = registration(Some(dead_token()));
        dead["started_at_ms"] = json!(T + rotate);
        assert_eq!(f.register("client-a", dead).await, StatusCode::NO_CONTENT);
        f.at(rotate + 60_000);
        assert_eq!(f.report("mac-1", "client-a", true, true, false).await, StatusCode::ACCEPTED);
        assert_eq!(f.take_pushed().iter().map(|p| p.token.clone()).collect::<Vec<_>>(), [dead_token()]);
        assert_eq!(f.server.activity.phone(PHONE_ID).unwrap().live, None, "APNs said 410: the token is forgotten");
        f.at(rotate + activity::START_SPACING_MS);
        assert_eq!(f.report("mac-1", "client-a", true, true, false).await, StatusCode::ACCEPTED);
        let pushed = f.take_pushed();
        assert_eq!((pushed[0].token.clone(), pushed[0].body["aps"]["event"].clone()), (start_token(), json!("start")), "the next active report starts a new one");

        let mut fresh = registration(Some(update_token()));
        fresh["started_at_ms"] = json!(T + rotate + activity::START_SPACING_MS);
        assert_eq!(f.register("client-a", fresh).await, StatusCode::NO_CONTENT);
        f.at(rotate + activity::START_SPACING_MS + 60_000);
        let path = "/v1/macs/mac-2";
        let request = f.client.delete(format!("{}{path}", f.url));
        let revoked = sign(&f.epoch, request, f.mac_key("mac-2"), "mac-2", "DELETE", path, &[], f.nonce()).send().await.unwrap();
        assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
        let pushed = f.take_pushed();
        assert_eq!(pushed.len(), 1);
        assert_eq!(macs(&pushed[0]), ["mac-1"], "the revoked Mac leaves the next push");
        assert!(!f.server.activity.phone(PHONE_ID).unwrap().pairings.contains_key("client-b"));

        f.at(rotate + activity::START_SPACING_MS + 2 * 60_000);
        assert_eq!(f.report("mac-1", "client-a", false, true, false).await, StatusCode::ACCEPTED);
        let pushed = f.take_pushed();
        assert_eq!(pushed.len(), 1);
        let end = &pushed[0].body["aps"];
        let now_s = (T + rotate + activity::START_SPACING_MS + 2 * 60_000) / 1000;
        assert_eq!((end["event"].clone(), end["dismissal-date"].clone()), (json!("end"), json!(now_s)), "no Mac is active");
        assert_eq!(header(&pushed[0], "apns-priority"), "5");
        assert_eq!(f.server.activity.phone(PHONE_ID).unwrap().live, None);
    }

    /// Real envelopes are about 800 bytes, so 4 KB holds the newest four, not the eight the
    /// design estimated; the rest are counted in `omitted`.
    #[tokio::test]
    async fn eight_macs_go_out_as_the_newest_that_fit_in_one_apns_push() {
        let f = Fixture::new().await;
        let reports = (0..8u64).map(|i| {
            let mac = format!("mac-{}", uuid_like(i));
            let device = format!("client-{}", uuid_like(i + 100));
            let report = activity::Report {
                envelope: f.envelope(&mac, &device, "activity", now_ms() + 600_000, f.mac_key("mac-1")),
                device_id: device,
                active: true,
                may_start: true,
                urgent: false,
                received_at_ms: T + i,
            };
            (mac, report)
        });
        let phone = activity::Phone {
            owner_sub: "owner".into(),
            environment: "production".into(),
            frequent_updates: true,
            start_token: Some("ab".repeat(80)),
            live: None,
            pairings: Default::default(),
            reports: reports.collect(),
            last_start_ms: None,
            last_urgent_ms: None,
            last_sent_ms: None,
            urgent_pending: false,
            unsent: false,
        };
        for push in [activity::Send::Start, activity::Send::Update { priority: 10 }, activity::Send::End] {
            let size = |state: &Value| serde_json::to_vec(&activity::payload(&push, state, T)).unwrap().len();
            let state = phone.content_state(|state| size(state) <= 4096);
            let sent = state["macs"].as_array().unwrap().len();
            assert!(size(&state) <= 4096, "{push:?} is {} bytes", size(&state));
            assert!(sent >= 4, "{push:?} carries only {sent} Macs");
            assert_eq!(state["omitted"], 8 - sent);
            assert_eq!(state["macs"][0]["sender"], format!("mac-{}", uuid_like(7)), "the newest first");
        }
    }

    fn uuid_like(i: u64) -> String {
        format!("{i:08X}-1F44-4D9A-9B1E-2C3D4E5F6A7B")
    }
}
