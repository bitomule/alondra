mod operator_api {
    use super::iroh_access::IrohKey;
    use super::*;
    use operator::Operator;
    use std::sync::atomic::AtomicU8;

    const TOKEN: &str = "test-operator-token-0123456789abcdef-0123";

    struct Relay {
        url: String,
        operator_url: String,
        server: Arc<Server>,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    }

    impl Drop for Relay {
        fn drop(&mut self) {
            for task in &self.tasks {
                task.abort();
            }
        }
    }

    /// One registry on disk that relays start over and stop, as a restart would.
    struct World {
        directory: crate::test_support::Directory,
        mac_key: SigningKey,
        phone_key: SigningKey,
        http: reqwest::Client,
        nonce: AtomicU8,
    }

    impl World {
        async fn new() -> (Self, Relay) {
            let world = Self {
                directory: crate::test_support::Directory::new(),
                mac_key: SigningKey::from_slice(&[121u8; 32]).unwrap(),
                phone_key: SigningKey::from_slice(&[122u8; 32]).unwrap(),
                http: reqwest::Client::new(),
                nonce: AtomicU8::new(0),
            };
            let relay = world.start(Operator::new(Some(TOKEN.into()), None).unwrap()).await;
            let body = serde_json::to_vec(&PhoneDevice {
                device_id: "phone-1".into(),
                owner_sub: "owner".into(),
                signing_key: key_bytes(&world.phone_key),
            })
            .unwrap();
            assert_eq!(world.as_mac(&relay, "POST", "/v1/devices", body).await, StatusCode::CREATED);
            (world, relay)
        }

        fn config(&self) -> Config {
            Config {
                listen: "127.0.0.1:0".into(),
                issuer: "https://id.example/realms/alondra".into(),
                audience: "alondra-relay".into(),
                jwks_url: "http://127.0.0.1:1/jwks".into(),
                allow_internal_http_jwks: false,
                owner_sub: "owner".into(),
                mac_id: "mac-1".into(),
                mac_signing_key: key_bytes(&self.mac_key),
                device_registry_path: self.directory.join("registry.json"),
                apns: None,
            }
        }

        async fn start(&self, operator: Operator) -> Relay {
            let auth = Auth::test(self.config(), include_bytes!("../tests/fixtures/jwks.json")).unwrap();
            if operator.follows_source() {
                auth.await_removal_source();
            }
            let server = Arc::new(Server {
                activity: activity::Store::open(&auth.config.device_registry_path).unwrap(),
                epoch: URL_SAFE_NO_PAD.encode(auth.server_epoch()),
                auth,
                management_keycloak: None,
                web_session: None,
                web_assets: None,
                apns: None,
                pairing: pairing::Mailbox::default(),
                requests: Arc::new(Semaphore::new(128)),
                device_gate: Arc::new(RwLock::new(())),
                waitlist: management::waitlist::Limiter::new(None),
                iroh_relay: Some(TEST_IROH_RELAY.into()),
                iroh_access_token: Some(TEST_IROH_ACCESS_TOKEN.into()),
                report_repo: None,
                identity: None,
                testflight_url: None,
                operator,
                test_directory: None,
            });
            let (url, public) = serve(router(server.clone())).await;
            let (operator_url, private) = serve(operator::router(server.clone())).await;
            Relay { url, operator_url, server, tasks: vec![public, private] }
        }

        async fn signed(&self, relay: &Relay, key: &SigningKey, id: &str, owner: Option<&str>,
            method: &str, path: &str, body: Vec<u8>) -> StatusCode {
            let nonce = self.nonce.fetch_add(1, Ordering::Relaxed) + 1;
            let mut builder = self.http.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
                format!("{}{path}", relay.url));
            if let Some(owner) = owner {
                builder = builder.bearer_auth(jwt(owner, now_ms()));
            }
            sign(&relay.server.auth.server_epoch(), builder.body(body.clone()), key, id, method,
                path, &body, nonce).send().await.unwrap().status()
        }

        async fn as_mac(&self, relay: &Relay, method: &str, path: &str, body: Vec<u8>) -> StatusCode {
            self.signed(relay, &self.mac_key, "mac-1", None, method, path, body).await
        }

        async fn as_phone(&self, relay: &Relay, method: &str, path: &str, body: Vec<u8>) -> StatusCode {
            self.signed(relay, &self.phone_key, "phone-1", Some("owner"), method, path, body).await
        }

        /// The phone's signed request to bind its iroh endpoint.
        async fn phone_binds(&self, relay: &Relay, node: &IrohKey) -> StatusCode {
            self.as_phone(relay, "PUT", "/v1/iroh-node", node.binding_for("phone-1")).await
        }

        async fn macs_of(&self, relay: &Relay, owner: &str) -> StatusCode {
            self.http.get(format!("{}/v1/macs", relay.url)).bearer_auth(jwt(owner, now_ms()))
                .send().await.unwrap().status()
        }

        async fn ready(&self, relay: &Relay) -> StatusCode {
            self.http.get(format!("{}/readyz", relay.url)).send().await.unwrap().status()
        }

        async fn access(&self, relay: &Relay, node: &IrohKey) -> StatusCode {
            self.http.post(format!("{}/v1/iroh/access", relay.url)).bearer_auth(TEST_IROH_ACCESS_TOKEN)
                .header("X-Iroh-NodeId", node.id()).send().await.unwrap().status()
        }

        async fn remove(&self, relay: &Relay, removals: Value) -> (StatusCode, Value) {
            let response = self.http.post(format!("{}/v1/operator/removals", relay.operator_url))
                .bearer_auth(TOKEN).json(&json!({"removals": removals})).send().await.unwrap();
            let status = response.status();
            (status, response.json().await.unwrap_or(Value::Null))
        }
    }

    async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>())
                .await
                .unwrap();
        });
        (url, task)
    }

    /// An account source that serves `list` to a caller presenting the operator token.
    async fn source(list: Value) -> (String, tokio::task::JoinHandle<()>) {
        let router = Router::new().route("/removals", get(move |headers: HeaderMap| {
            let list = list.clone();
            async move {
                if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
                    != Some(&format!("Bearer {TOKEN}")) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(list).into_response()
            }
        }));
        let (url, task) = serve(router).await;
        (format!("{url}/removals"), task)
    }

    fn phone_removal(seq: u64) -> Value {
        json!({"seq": seq, "kind": "client", "owner": "owner", "client_id": "phone-1", "mac_id": "mac-1"})
    }

    #[tokio::test]
    async fn the_operator_api_is_absent_from_the_public_listener_and_needs_its_token() {
        let (world, relay) = World::new().await;
        let body = json!({"removals": []});
        let public = world.http.post(format!("{}/v1/operator/removals", relay.url))
            .bearer_auth(TOKEN).json(&body).send().await.unwrap();
        assert_eq!(public.status(), StatusCode::NOT_FOUND);
        for presented in [None, Some("test-operator-token-0123456789abcdef-012X")] {
            let mut request = world.http.post(format!("{}/v1/operator/removals", relay.operator_url)).json(&body);
            if let Some(token) = presented {
                request = request.bearer_auth(token);
            }
            assert_eq!(request.send().await.unwrap().status(), StatusCode::UNAUTHORIZED, "{presented:?}");
        }
        assert_eq!(world.remove(&relay, json!([])).await, (StatusCode::OK, json!({"applied": 0})));

        let tokenless = world.start(Operator::default()).await;
        let response = world.http.post(format!("{}/v1/operator/removals", tokenless.operator_url))
            .bearer_auth(TOKEN).json(&body).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_operator_token_and_the_source_url_are_checked_at_start() {
        assert!(Operator::new(Some("short".into()), None).is_err());
        assert!(Operator::new(None, Some("https://cloud.example/removals")).is_err(), "a source needs the token");
        for allowed in ["https://cloud.example/removals", "http://127.0.0.1:9/removals", "http://cloud:18082/removals"] {
            assert!(Operator::new(Some(TOKEN.into()), Some(allowed)).is_ok(), "{allowed}");
        }
        for refused in ["http://cloud.example/removals", "ftp://cloud/removals", "not a url"] {
            assert!(Operator::new(Some(TOKEN.into()), Some(refused)).is_err(), "{refused}");
        }
    }

    #[tokio::test]
    async fn a_removal_is_held_on_disk_refuses_the_device_and_is_idempotent() {
        let (world, relay) = World::new().await;
        let node = IrohKey::new(31);
        assert_eq!(world.phone_binds(&relay, &node).await, StatusCode::NO_CONTENT);
        assert_eq!(world.access(&relay, &node).await, StatusCode::OK);

        assert_eq!(world.remove(&relay, json!([phone_removal(1)])).await, (StatusCode::OK, json!({"applied": 1})));
        assert_eq!(world.access(&relay, &node).await, StatusCode::FORBIDDEN);
        assert_eq!(world.phone_binds(&relay, &node).await, StatusCode::UNAUTHORIZED);
        assert_eq!(world.remove(&relay, json!([phone_removal(1)])).await, (StatusCode::OK, json!({"applied": 1})),
            "a batch delivered again changes nothing");
        drop(relay);

        let restarted = world.start(Operator::new(Some(TOKEN.into()), None).unwrap()).await;
        assert_eq!(restarted.server.auth.removals_seq(), 1);
        assert!(!restarted.server.auth.is_phone_registered("phone-1"));
        assert_eq!(world.phone_binds(&restarted, &node).await, StatusCode::UNAUTHORIZED);
        let body = serde_json::to_vec(&PhoneDevice {
            device_id: "phone-1".into(),
            owner_sub: "owner".into(),
            signing_key: key_bytes(&world.phone_key),
        })
        .unwrap();
        assert_ne!(world.as_mac(&restarted, "POST", "/v1/devices", body).await, StatusCode::CREATED,
            "a removed device cannot be enrolled again");
    }

    #[tokio::test]
    async fn a_batch_that_skips_a_number_applies_nothing_and_asks_for_the_list() {
        let (world, relay) = World::new().await;
        let node = IrohKey::new(32);
        assert_eq!(world.phone_binds(&relay, &node).await, StatusCode::NO_CONTENT);
        assert_eq!(world.remove(&relay, json!([phone_removal(2)])).await,
            (StatusCode::CONFLICT, json!({"error": "removals_gap", "applied": 0})));
        assert_eq!(world.access(&relay, &node).await, StatusCode::OK, "nothing of a refused batch is applied");
        tokio::time::timeout(Duration::from_secs(5), relay.server.operator.gap.notified())
            .await
            .expect("a gap asks the source for its whole list");
    }

    #[tokio::test]
    async fn the_whole_list_fills_a_gap_and_numbering_goes_on_from_it() {
        let (world, relay) = World::new().await;
        let node = IrohKey::new(33);
        assert_eq!(world.phone_binds(&relay, &node).await, StatusCode::NO_CONTENT);
        let (url, _source) = source(json!({"seq": 3, "removals": [
            {"kind": "client", "owner": "owner", "client_id": "phone-1", "mac_id": "mac-1"}]})).await;
        drop(relay);
        let relay = world.start(Operator::new(Some(TOKEN.into()), Some(&url)).unwrap()).await;
        assert_eq!(world.remove(&relay, json!([{"seq": 4, "kind": "owner", "owner": "other"}])).await.0,
            StatusCode::CONFLICT);
        operator::reconcile_from_source(&relay.server).await.unwrap();
        assert_eq!(world.access(&relay, &node).await, StatusCode::FORBIDDEN);
        assert_eq!(world.remove(&relay, json!([{"seq": 4, "kind": "owner", "owner": "other"}])).await,
            (StatusCode::OK, json!({"applied": 4})));
        assert_eq!(world.macs_of(&relay, "other").await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_device_removed_while_the_relay_was_down_is_refused_once_it_is_back() {
        let (world, relay) = World::new().await;
        let node = IrohKey::new(34);
        assert_eq!(world.phone_binds(&relay, &node).await, StatusCode::NO_CONTENT);
        drop(relay);

        let (url, _source) = source(json!({"seq": 1, "removals": [
            {"kind": "client", "owner": "owner", "client_id": "phone-1", "mac_id": "mac-1"}]})).await;
        let back = world.start(Operator::new(Some(TOKEN.into()), Some(&url)).unwrap()).await;
        assert_eq!(world.phone_binds(&back, &node).await, StatusCode::SERVICE_UNAVAILABLE,
            "before the source's list is read the account cannot be checked");
        assert_eq!(world.access(&back, &node).await, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(world.ready(&back).await, StatusCode::SERVICE_UNAVAILABLE);

        operator::reconcile_from_source(&back.server).await.unwrap();
        assert_eq!(world.phone_binds(&back, &node).await, StatusCode::UNAUTHORIZED);
        assert_eq!(world.access(&back, &node).await, StatusCode::FORBIDDEN);
        assert_eq!(world.ready(&back).await, StatusCode::OK);
        assert_eq!(world.macs_of(&back, "owner").await, StatusCode::OK, "the account itself stays open");
    }

    #[tokio::test]
    async fn an_unreachable_source_keeps_account_routes_unavailable_and_never_open() {
        let (world, relay) = World::new().await;
        let node = IrohKey::new(35);
        assert_eq!(world.phone_binds(&relay, &node).await, StatusCode::NO_CONTENT);
        drop(relay);
        let back = world.start(Operator::new(Some(TOKEN.into()), Some("http://127.0.0.1:1/removals")).unwrap()).await;
        assert!(operator::reconcile_from_source(&back.server).await.is_err());
        assert_eq!(world.phone_binds(&back, &node).await, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(world.access(&back, &node).await, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(world.macs_of(&back, "owner").await, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(world.as_mac(&back, "PUT", "/v1/iroh-node", IrohKey::new(36).binding_for("mac-1")).await,
            StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_closed_account_cannot_use_its_token_or_enroll_a_mac() {
        let (world, relay) = World::new().await;
        assert_eq!(world.macs_of(&relay, "owner").await, StatusCode::OK);
        assert_eq!(world.remove(&relay, json!([{"seq": 1, "kind": "owner", "owner": "owner"}])).await.0, StatusCode::OK);
        assert_eq!(world.macs_of(&relay, "owner").await, StatusCode::FORBIDDEN);
        assert_eq!(world.macs_of(&relay, "someone-else").await, StatusCode::OK);
        assert!(!relay.server.auth.is_phone_registered("phone-1"));
        assert!(!relay.server.auth.is_mac_registered("mac-1"));

        let mac = SigningKey::from_slice(&[123u8; 32]).unwrap();
        let body = serde_json::to_vec(&json!({"mac_id": "mac-2", "signing_key": key_bytes(&mac),
            "agreement_key": key_bytes(&mac)})).unwrap();
        let enrolled = world.http.post(format!("{}/v1/macs", relay.url)).bearer_auth(jwt("owner", now_ms()))
            .body(body).header(header::CONTENT_TYPE, "application/json").send().await.unwrap();
        assert_eq!(enrolled.status(), StatusCode::FORBIDDEN);
        assert_eq!(enrolled.json::<Value>().await.unwrap()["error"], "account_access_denied");
    }

    #[tokio::test]
    async fn a_removal_of_another_accounts_device_is_skipped_and_the_rest_apply() {
        let (world, relay) = World::new().await;
        let node = IrohKey::new(37);
        assert_eq!(world.phone_binds(&relay, &node).await, StatusCode::NO_CONTENT);
        let wrong = json!({"seq": 2, "kind": "client", "owner": "intruder", "client_id": "phone-1", "mac_id": "mac-1"});
        let mac_as_client = json!({"seq": 3, "kind": "client", "owner": "owner", "client_id": "mac-1", "mac_id": "mac-1"});
        assert_eq!(world.remove(&relay, json!([{"seq": 1, "kind": "owner", "owner": "x"}, wrong, mac_as_client])).await,
            (StatusCode::OK, json!({"applied": 3})));
        assert_eq!(world.access(&relay, &node).await, StatusCode::OK, "another account's device is never touched");
        assert_eq!(world.as_mac(&relay, "PUT", "/v1/iroh-node", IrohKey::new(38).binding_for("mac-1")).await,
            StatusCode::NO_CONTENT, "a Mac named as a client is never touched");
        assert_eq!(world.macs_of(&relay, "x").await, StatusCode::FORBIDDEN, "the valid removal is applied");
        assert!(Auth::test(world.config(), include_bytes!("../tests/fixtures/jwks.json")).is_ok(),
            "the registry stays loadable");
        assert_eq!(world.remove(&relay, json!([{"seq": 4, "kind": "mystery", "owner": "x"}])).await.0,
            StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn a_client_paired_with_another_mac_than_named_is_still_removed() {
        let (world, relay) = World::new().await;
        let node = IrohKey::new(39);
        assert_eq!(world.phone_binds(&relay, &node).await, StatusCode::NO_CONTENT);
        let elsewhere = json!({"seq": 1, "kind": "client", "owner": "owner", "client_id": "phone-1", "mac_id": "mac-9"});
        assert_eq!(world.remove(&relay, json!([elsewhere])).await, (StatusCode::OK, json!({"applied": 1})));
        assert_eq!(world.access(&relay, &node).await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_full_list_with_a_conflicting_entry_clears_the_wait_and_applies_the_rest() {
        let (world, relay) = World::new().await;
        let node = IrohKey::new(40);
        assert_eq!(world.phone_binds(&relay, &node).await, StatusCode::NO_CONTENT);
        drop(relay);
        let (url, _source) = source(json!({"seq": 3, "removals": [
            {"kind": "client", "owner": "intruder", "client_id": "phone-1", "mac_id": "mac-1"},
            {"kind": "owner", "owner": ""},
            {"kind": "owner", "owner": "y"}]})).await;
        let back = world.start(Operator::new(Some(TOKEN.into()), Some(&url)).unwrap()).await;
        operator::reconcile_from_source(&back.server).await.unwrap();
        assert_eq!(world.ready(&back).await, StatusCode::OK);
        assert_eq!(back.server.auth.removals_seq(), 3);
        assert_eq!(world.macs_of(&back, "y").await, StatusCode::FORBIDDEN);
        assert_eq!(world.access(&back, &node).await, StatusCode::OK);
        assert_eq!(world.remove(&back, json!([{"seq": 4, "kind": "owner", "owner": "z"}])).await,
            (StatusCode::OK, json!({"applied": 4})), "removals go on after the conflicting one");
    }

    #[tokio::test]
    async fn a_batch_below_the_held_number_is_refused_as_a_reset() {
        let (world, relay) = World::new().await;
        let owners = json!([{"seq": 1, "kind": "owner", "owner": "a"}, {"seq": 2, "kind": "owner", "owner": "b"},
            {"seq": 3, "kind": "owner", "owner": "c"}]);
        assert_eq!(world.remove(&relay, owners).await.0, StatusCode::OK);
        assert_eq!(world.remove(&relay, json!([{"seq": 2, "kind": "owner", "owner": "d"}])).await,
            (StatusCode::CONFLICT, json!({"error": "removals_behind", "applied": 3})));
        assert_eq!(world.macs_of(&relay, "d").await, StatusCode::OK, "nothing of it is applied");
        tokio::time::timeout(Duration::from_secs(5), relay.server.operator.gap.notified())
            .await
            .expect("a reset asks the source for its whole list");
        assert_eq!(world.remove(&relay, json!([{"seq": 3, "kind": "owner", "owner": "c"}])).await,
            (StatusCode::OK, json!({"applied": 3})), "the last held batch sent again is acknowledged");
        drop(relay);

        let (url, _source) = source(json!({"seq": 1, "removals": [{"kind": "owner", "owner": "d"}]})).await;
        let reset = world.start(Operator::new(Some(TOKEN.into()), Some(&url)).unwrap()).await;
        operator::reconcile_from_source(&reset.server).await.unwrap();
        assert_eq!(reset.server.auth.removals_seq(), 1, "the source's numbering is followed even when it went back");
        assert_eq!(world.macs_of(&reset, "a").await, StatusCode::FORBIDDEN, "nothing held is restored");
        assert_eq!(world.remove(&reset, json!([{"seq": 2, "kind": "owner", "owner": "e"}])).await,
            (StatusCode::OK, json!({"applied": 2})));
    }

    #[tokio::test]
    async fn reading_an_unchanged_list_writes_nothing() {
        let (world, relay) = World::new().await;
        drop(relay);
        let (url, _source) = source(json!({"seq": 1, "removals": [
            {"kind": "client", "owner": "owner", "client_id": "phone-1", "mac_id": "mac-1"}]})).await;
        let back = world.start(Operator::new(Some(TOKEN.into()), Some(&url)).unwrap()).await;
        operator::reconcile_from_source(&back.server).await.unwrap();
        let writes = back.server.auth.registry_write_count();
        operator::reconcile_from_source(&back.server).await.unwrap();
        assert_eq!(back.server.auth.registry_write_count(), writes);
    }

    #[test]
    fn a_closed_account_cannot_finish_a_mac_enrollment_it_started() {
        let directory = crate::test_support::Directory::new();
        let world = World {
            directory,
            mac_key: SigningKey::from_slice(&[121u8; 32]).unwrap(),
            phone_key: SigningKey::from_slice(&[122u8; 32]).unwrap(),
            http: reqwest::Client::new(),
            nonce: AtomicU8::new(0),
        };
        let auth = Auth::test(world.config(), include_bytes!("../tests/fixtures/jwks.json")).unwrap();
        auth.apply_removals(&[(1, auth::Removal::Owner { owner: "closing".into() })]).unwrap();
        let mac = SigningKey::from_slice(&[124u8; 32]).unwrap();
        let enrolled = auth.enroll_mac(auth::MacDevice {
            mac_id: "mac-late".into(),
            owner_sub: "closing".into(),
            signing_key: key_bytes(&mac),
            agreement_key: key_bytes(&mac),
        });
        assert!(matches!(enrolled.unwrap_err().downcast_ref(), Some(auth::AccountGateError::Denied)));
        assert!(!auth.is_mac_registered("mac-late"));
    }

    #[tokio::test]
    async fn the_operator_reads_an_accounts_devices_and_ends_pushes_of_finished_sessions() {
        let (world, relay) = World::new().await;
        let devices = |owner: &str| world.http.get(format!("{}/v1/operator/owners/{owner}/devices", relay.operator_url))
            .bearer_auth(TOKEN).send();
        assert_eq!(devices("owner").await.unwrap().json::<Value>().await.unwrap(),
            json!({"macs": [{"mac_id": "mac-1", "clients": ["phone-1"]}]}));
        assert_eq!(devices("other").await.unwrap().json::<Value>().await.unwrap(), json!({"macs": []}));

        relay.server.auth.register_push("phone-1", push_for(1), offline_session("s1")).unwrap();
        let sessions: Value = world.http.get(format!("{}/v1/operator/push-sessions", relay.operator_url))
            .bearer_auth(TOKEN).send().await.unwrap().json().await.unwrap();
        assert_eq!(sessions, json!({"sessions": [{"owner": "owner", "client_id": "alondra-ios",
            "registrations": [{"device_id": "phone-1", "sid": "s1"}]}]}));
        let ended = |sid: &str| world.http.post(format!("{}/v1/operator/push-sessions/ended", relay.operator_url))
            .bearer_auth(TOKEN).json(&json!({"ended": [{"device_id": "phone-1", "sid": sid}]})).send();
        assert_eq!(ended("another").await.unwrap().status(), StatusCode::NO_CONTENT);
        assert!(relay.server.auth.push_registration("phone-1").is_some(), "only the session that registered it ends it");
        assert_eq!(ended("s1").await.unwrap().status(), StatusCode::NO_CONTENT);
        assert!(relay.server.auth.push_registration("phone-1").is_none());
        assert!(relay.server.auth.push_ended("phone-1"));
    }
}
