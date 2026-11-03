use super::*;
use crate::iroh_endpoint::{self, ALPN, IrohConfig, close};
use alondra_core::views::action_id::{ActionId, Verb};
use iroh::endpoint::{Connection, ConnectionError, PortmapperConfig, VarInt, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode};
use std::net::Ipv4Addr;

const MAC_SIGNING: [u8; 32] = [1u8; 32];

struct Phone {
    id: &'static str,
    signing: SigningKey,
    agreement: SecretKey,
}

impl Phone {
    fn new(id: &'static str, seed: u8) -> Self {
        Self {
            id,
            signing: SigningKey::from_slice(&[seed; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[seed + 1; 32]).unwrap(),
        }
    }

    fn work(&self, service: &Service, id: &str, method: &str, path: &str, body: Option<Value>) -> Envelope {
        let mut value = json!({"method": method, "path": path});
        if let Some(body) = body {
            value["body"] = body;
        }
        Envelope::seal_random(
            EnvelopeContext {
                route: "mac-1",
                sender: self.id,
                recipient: "mac-1",
                request_id: id,
                kind: "request",
                expires_at_ms: now_ms() + 60_000,
            },
            &serde_json::to_vec(&value).unwrap(),
            &self.signing,
            &service.agreement.public_key(),
        )
        .unwrap()
    }

    fn opened(&self, sealed: &Value) -> Value {
        let envelope: Envelope = serde_json::from_value(sealed.clone()).unwrap();
        let mac = SigningKey::from_slice(&MAC_SIGNING).unwrap();
        let body = envelope
            .open(mac.verifying_key(), &self.agreement, "mac-1", self.id, "mac-1", now_ms())
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }
}

struct Fixture {
    service: Arc<Service>,
    domain: Arc<crate::domain_runtime::DomainRuntime>,
    mac: EndpointAddr,
    phone: Phone,
    other: Phone,
    home: crate::test_support::Directory,
    _endpoint: Endpoint,
}

impl Fixture {
    async fn new() -> Self {
        let (phone, other) = (Phone::new("phone-1", 33), Phone::new("phone-2", 43));
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        for (name, bytes) in [("signing.key", MAC_SIGNING), ("agreement.key", [2u8; 32])] {
            let path = home.join(name);
            std::fs::write(&path, URL_SAFE_NO_PAD.encode(bytes)).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let approved: Vec<_> = [&phone, &other]
            .iter()
            .map(|p| json!({"id": p.id, "name": "iPhone", "created_at": 0, "last_seen": null,
                "public_key": STANDARD.encode(p.signing.verifying_key().to_sec1_bytes())}))
            .collect();
        std::fs::write(home.join("devices.json"), serde_json::to_vec(&approved).unwrap()).unwrap();
        let mut config = String::from(
            "mac_id = \"mac-1\"\nsigning_key_path = \"signing.key\"\nagreement_key_path = \"agreement.key\"\n",
        );
        for p in [&phone, &other] {
            config.push_str(&format!(
                "[[devices]]\ndevice_id = \"{}\"\nsigning_key = \"{}\"\nagreement_key = \"{}\"\n",
                p.id,
                URL_SAFE_NO_PAD.encode(p.signing.verifying_key().to_sec1_bytes()),
                URL_SAFE_NO_PAD.encode(p.agreement.public_key().to_sec1_bytes())
            ));
        }
        std::fs::write(home.join("service.toml"), config).unwrap();
        let service = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap())).unwrap().unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Default::default());
        let service = Arc::new(service.with_domain(domain.clone()).with_client(client));
        let config = IrohConfig {
            relay: "off".into(),
            listen: Some(Ipv4Addr::LOCALHOST.into()),
            port: 0,
            portmapper: false,
            ..IrohConfig::default()
        };
        let endpoint = iroh_endpoint::bind(service.iroh_key().clone(), &config).await.unwrap();
        let mac = EndpointAddr::new(endpoint.id()).with_ip_addr(endpoint.bound_sockets()[0]);
        tokio::spawn(iroh_endpoint::serve(endpoint.clone(), service.clone()));
        Self { service, domain, mac, phone, other, home, _endpoint: endpoint }
    }

    async fn connect(&self, seed: u8) -> (Endpoint, Connection) {
        let client = Endpoint::builder(presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&[seed; 32]))
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let connection = client.connect(self.mac.clone(), ALPN).await.unwrap();
        (client, connection)
    }

    fn hello_signature(&self, signer: &SigningKey, device_id: &str, client: &Endpoint) -> String {
        alondra_e2e::sign_iroh_hello(
            signer,
            "mac-1",
            device_id,
            client.id().as_bytes(),
            self.service.iroh_key().public().as_bytes(),
        )
        .unwrap()
    }

    /// A connection whose hello `phone` signed and the Mac accepted.
    async fn paired(&self, phone: &Phone, seed: u8) -> (Endpoint, Connection) {
        let (client, connection) = self.connect(seed).await;
        let signature = self.hello_signature(&phone.signing, phone.id, &client);
        let reply = exchange(&connection, &json!({"device_id": phone.id, "signature": signature})).await;
        assert_eq!(reply, json!({"ok": true}));
        (client, connection)
    }

    fn action(&self, phone: &Phone, request_id: &str, limit: &str, command_id: &str) -> Envelope {
        let state = self.domain.state().unwrap();
        let action = ActionId::new(Verb::SetLimit, "limit", state.revision, Some(limit.into())).unwrap();
        phone.work(&self.service, request_id, "POST", "/actions", Some(json!({"protocol": 1,
            "command_id": command_id, "data_epoch": state.data_epoch, "client": "ios", "lang": "en",
            "action_id": action.to_string(), "option_id": limit, "input": {}})))
    }

    fn limit(&self) -> String {
        std::fs::read_to_string(alondra_core::options::path_in(&self.home)).unwrap_or_default()
    }
}

async fn send(connection: &Connection, body: &[u8]) -> Result<Vec<u8>> {
    let (mut send, mut recv) = connection.open_bi().await?;
    send.write_all(body).await?;
    send.finish()?;
    Ok(recv.read_to_end(4 * 1024 * 1024).await?)
}

async fn exchange(connection: &Connection, body: &impl serde::Serialize) -> Value {
    serde_json::from_slice(&send(connection, &serde_json::to_vec(body).unwrap()).await.unwrap()).unwrap()
}

async fn close_code(connection: &Connection) -> Option<VarInt> {
    match tokio::time::timeout(Duration::from_secs(5), connection.closed()).await.expect("connection closed") {
        ConnectionError::ApplicationClosed(close) => Some(close.error_code),
        _ => None,
    }
}

#[tokio::test]
async fn a_paired_device_gets_a_whole_request_answered_on_one_stream() {
    let f = Fixture::new().await;
    let (_client, connection) = f.paired(&f.phone, 50).await;
    let envelope = f.phone.work(&f.service, "op-routes", "GET", crate::direct::routes::PATH, None);
    let reply = exchange(&connection, &envelope).await;
    let answer = f.phone.opened(&reply["response"]);
    assert_eq!(answer["status"], 200, "{answer}");
    assert_eq!(answer["body"]["iroh_endpoint"], f.service.iroh_key().public().to_string());

    let (direct, lease) = f.service.handle_direct(envelope).await.unwrap();
    drop(lease);
    assert_eq!(
        serde_json::from_slice::<Value>(&direct).unwrap(),
        reply["response"],
        "the direct listener replays the very answer iroh got: one journal behind both"
    );
}

#[tokio::test]
async fn a_device_not_paired_here_is_refused_before_any_request() {
    let f = Fixture::new().await;
    let stranger = Phone::new("phone-9", 70);

    let (client, connection) = f.connect(51).await;
    let signature = f.hello_signature(&stranger.signing, stranger.id, &client);
    let reply = exchange(&connection, &json!({"device_id": stranger.id, "signature": signature})).await;
    assert_eq!(reply, json!({"error": "unauthorized"}));
    assert_eq!(close_code(&connection).await, Some(VarInt::from_u32(close::UNAUTHORIZED)));

    let (client, connection) = f.connect(52).await;
    let forged = f.hello_signature(&stranger.signing, f.phone.id, &client);
    let reply = exchange(&connection, &json!({"device_id": f.phone.id, "signature": forged})).await;
    assert_eq!(reply, json!({"error": "unauthorized"}), "a paired ID signed by another key proves nothing");
    assert_eq!(close_code(&connection).await, Some(VarInt::from_u32(close::UNAUTHORIZED)));

    let (elsewhere, _) = f.connect(53).await;
    let replayed = f.hello_signature(&f.phone.signing, f.phone.id, &elsewhere);
    let (_client, connection) = f.connect(54).await;
    let reply = exchange(&connection, &json!({"device_id": f.phone.id, "signature": replayed})).await;
    assert_eq!(reply, json!({"error": "unauthorized"}), "a hello signed for another endpoint cannot be replayed");
    assert_eq!(close_code(&connection).await, Some(VarInt::from_u32(close::UNAUTHORIZED)));
    assert!(send(&connection, b"{}").await.is_err(), "nothing is served after a refused hello");
}

#[tokio::test]
async fn a_connection_that_never_says_hello_is_closed() {
    let f = Fixture::new().await;
    let (_client, connection) = f.connect(55).await;
    assert_eq!(close_code(&connection).await, Some(VarInt::from_u32(close::HELLO_TIMEOUT)));
}

#[tokio::test]
async fn a_stream_from_another_device_than_the_hello_is_refused() {
    let f = Fixture::new().await;
    let (_client, connection) = f.paired(&f.phone, 56).await;
    let theirs = f.other.work(&f.service, "op-other", "GET", crate::direct::routes::PATH, None);
    assert_eq!(exchange(&connection, &theirs).await, json!({"error": "wrong_sender"}));
    let mine = f.phone.work(&f.service, "op-mine", "GET", crate::direct::routes::PATH, None);
    assert_eq!(f.phone.opened(&exchange(&connection, &mine).await["response"])["status"], 200);
}

#[tokio::test]
async fn a_revoked_device_loses_the_open_connection_and_is_told_so_sealed() {
    let f = Fixture::new().await;
    let (_client, connection) = f.paired(&f.phone, 57).await;
    assert!(Auth::open(&f.home).unwrap().revoke("phone-1").unwrap());
    let after = f.phone.work(&f.service, "op-after", "GET", crate::direct::routes::PATH, None);
    assert_eq!(
        exchange(&connection, &after).await,
        json!({"error": "unauthorized"}),
        "a revocation on this Mac cuts the open connection at its next request"
    );

    f.service.remember_revoked("phone-1");
    let (_client, connection) = f.paired(&f.phone, 58).await;
    let request = f.phone.work(&f.service, "op-told", "GET", crate::direct::routes::PATH, None);
    let answer = f.phone.opened(&exchange(&connection, &request).await["revoked"]);
    assert_eq!((answer["status"].as_u64(), answer["error"].as_str()), (Some(403), Some("device_revoked")));
}

#[tokio::test]
async fn a_repeated_mutation_runs_once_over_iroh_and_directly() {
    let f = Fixture::new().await;
    let (_client, connection) = f.paired(&f.phone, 59).await;
    let action = f.action(&f.phone, "op-limit", "4", "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10");
    let first = exchange(&connection, &action).await;
    assert_eq!(f.phone.opened(&first["response"])["body"]["outcome"], "applied");
    assert!(f.limit().contains("max_working_nodes = 4"));

    std::fs::write(alondra_core::options::path_in(&f.home), f.limit().replace("max_working_nodes = 4", "max_working_nodes = 6")).unwrap();
    let again = exchange(&connection, &action).await;
    assert_eq!(again, first, "a retry over iroh gets the stored answer");
    let (direct, lease) = f.service.handle_direct(action.clone()).await.unwrap();
    drop(lease);
    assert_eq!(serde_json::from_slice::<Value>(&direct).unwrap(), first["response"], "and so does one sent directly");
    assert!(f.limit().contains("max_working_nodes = 6"), "the retries never applied the action again");

    let reused = f.action(&f.phone, "op-limit", "5", "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10");
    assert_eq!(exchange(&connection, &reused).await, json!({"error": "idempotency_conflict"}));
}

#[tokio::test]
async fn a_body_over_the_limit_or_not_an_envelope_is_refused_and_the_connection_stays() {
    let f = Fixture::new().await;
    let (_client, connection) = f.paired(&f.phone, 60).await;
    let huge = vec![b' '; iroh_endpoint::MAX_REQUEST + 1];
    let reply: Value = serde_json::from_slice(&send(&connection, &huge).await.unwrap()).unwrap();
    assert_eq!(reply, json!({"error": "too_large"}));
    assert_eq!(serde_json::from_slice::<Value>(&send(&connection, b"[]").await.unwrap()).unwrap(), json!({"error": "invalid_envelope"}));
    let request = f.phone.work(&f.service, "op-still", "GET", crate::direct::routes::PATH, None);
    assert_eq!(f.phone.opened(&exchange(&connection, &request).await["response"])["status"], 200);
}

#[tokio::test]
async fn the_iroh_key_is_created_once_and_kept_across_restarts() {
    let f = Fixture::new().await;
    let path = f.home.join(iroh_endpoint::KEY_FILE);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let again = Service::open(&f.home, 0, Arc::new(Auth::open(&f.home).unwrap())).unwrap().unwrap();
    assert_eq!(again.iroh_key().public(), f.service.iroh_key().public());
}

#[test]
fn the_iroh_table_defaults_to_on_with_the_announced_relay_and_rejects_plain_http() {
    let config: IrohConfig = toml::from_str("").unwrap();
    assert_eq!(config, IrohConfig::default());
    assert!(config.enabled && config.portmapper && config.relay == "auto" && config.listen.is_none());
    assert_eq!(config.port, iroh_endpoint::DEFAULT_PORT);
    assert_eq!(config.fixed_relay().unwrap(), None);
    let fixed: IrohConfig = toml::from_str("relay = \"https://relay.example.com\"").unwrap();
    assert_eq!(fixed.fixed_relay().unwrap().unwrap().to_string(), "https://relay.example.com/");
    let plain: IrohConfig = toml::from_str("relay = \"http://relay.example.com\"").unwrap();
    assert!(plain.fixed_relay().is_err());
    assert!(toml::from_str::<IrohConfig>("relays = \"off\"").is_err());
}

#[tokio::test]
async fn the_mac_binds_its_endpoint_at_the_relay_once_per_epoch_and_follows_the_announced_relay() {
    use axum::Json;
    use axum::extract::State as RelayState;
    use axum::routing::put;
    #[derive(Clone, Default)]
    struct Seen {
        bindings: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
        status: Arc<AtomicUsize>,
    }
    let seen = Seen { status: Arc::new(AtomicUsize::new(404)), ..Seen::default() };
    let router = Router::new()
        .route("/v1/bootstrap", get(|| async {
            Json(json!({"epoch": URL_SAFE_NO_PAD.encode([1u8; 16]), "iroh_relay": "https://iroh.example.com"}))
        }))
        .route("/v1/iroh-node", put(|RelayState(seen): RelayState<Seen>, headers: HeaderMap, Json(body): Json<Value>| async move {
            seen.bindings.lock().unwrap().push((headers, body));
            axum::http::StatusCode::from_u16(seen.status.load(Ordering::SeqCst) as u16).unwrap()
        }))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (_directory, mut service, _) = push_fixture();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let announced = service.iroh_relay_announced();
    let epoch = service.bootstrap().await.unwrap();
    assert_eq!(announced.borrow().as_deref(), Some("https://iroh.example.com"));

    service.keep_iroh_endpoint_bound(&epoch).await;
    service.keep_iroh_endpoint_bound(&epoch).await;
    assert_eq!(seen.bindings.lock().unwrap().len(), 1, "a relay without the route is not asked again at once");

    seen.status.store(204, Ordering::SeqCst);
    let next = [2u8; 16];
    service.keep_iroh_endpoint_bound(&next).await;
    service.keep_iroh_endpoint_bound(&next).await;
    let bindings = seen.bindings.lock().unwrap().clone();
    assert_eq!(bindings.len(), 2, "a new epoch binds again, once");
    let (headers, body) = &bindings[1];
    assert_eq!(headers["x-device-id"], "mac-1", "signed as this Mac");
    let endpoint = service.iroh_key().public();
    assert_eq!(body["iroh_node_id"], endpoint.to_string());
    let proof = URL_SAFE_NO_PAD.decode(body["proof"].as_str().unwrap()).unwrap();
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, endpoint.as_bytes())
        .verify(b"alondra-iroh-node-v1\0mac-1", &proof)
        .expect("the proof is the endpoint key's signature over the domain and the Mac ID");
    server.abort();
}

#[test]
fn the_endpoint_id_is_the_ed25519_public_key_of_the_stored_seed() {
    let seed = [7u8; 32];
    let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    let from_ring: String = ring::signature::KeyPair::public_key(&pair).as_ref().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(iroh::SecretKey::from_bytes(&seed).public().to_string(), from_ring);
    assert_eq!(from_ring, "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c");
}

/// A paired Mac on loopback for the Swift client's process test (`//apple/Data/Iroh`). It prints
/// where it listens and the body of one action, answers `limit` with the options file, and stops
/// when stdin closes. Its keys are the fixed seeds above, which the Swift test derives itself.
#[ignore = "stdin-controlled iroh server for the Swift client process test"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn iroh_process_fixture_server() {
    use std::io::BufRead;
    let f = Fixture::new().await;
    let state = f.domain.state().unwrap();
    let action = ActionId::new(Verb::SetLimit, "limit", state.revision, Some("4".into())).unwrap();
    let ready = json!({
        "event": "ready",
        "endpoint": f.service.iroh_key().public().to_string(),
        "address": f._endpoint.bound_sockets()[0].to_string(),
        "action": serde_json::to_string(&json!({"protocol": 1, "command_id": "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10",
            "data_epoch": state.data_epoch, "client": "ios", "lang": "en", "action_id": action.to_string(),
            "option_id": "4", "input": {}})).unwrap(),
    });
    println!("ALONDRA_IROH {ready}");
    let options = alondra_core::options::path_in(&f.home);
    tokio::task::spawn_blocking(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { return };
            if line.trim() == "limit" {
                let text = std::fs::read_to_string(&options).unwrap_or_default();
                println!("ALONDRA_IROH {}", json!({"event": "limit", "limit": text}));
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_connection_is_counted_by_the_path_and_bytes_it_used_once_it_closes() {
    let f = Fixture::new().await;
    let before = crate::reachability::recorded(crate::reachability::PathKind::Lan);
    let (_client, connection) = f.paired(&f.phone, 51).await;
    let followed = tokio::spawn(crate::reachability::follow(connection.clone()));
    let envelope = f.phone.work(&f.service, "op-path", "GET", crate::direct::routes::PATH, None);
    exchange(&connection, &envelope).await;
    connection.close(VarInt::from_u32(0), b"done");
    let followed = tokio::time::timeout(Duration::from_secs(5), followed).await.expect("ends with the connection").unwrap();
    assert_eq!(followed.best, Some(crate::reachability::PathKind::Lan), "loopback is this Mac's own network");
    let sent = serde_json::to_vec(&envelope).unwrap().len() as u64;
    assert!(followed.bytes.lan > sent, "the exchange went over the loopback path: {followed:?}");
    assert_eq!((followed.bytes.internet, followed.bytes.relay), (0, 0), "nothing went elsewhere: {followed:?}");
    let counted = async {
        while crate::reachability::recorded(crate::reachability::PathKind::Lan) == before {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), counted).await.expect("the Mac counted the closed connection");
}

#[test]
fn the_daemon_status_carries_the_direct_connection_diagnosis() {
    crate::reachability::record_session(crate::reachability::PathKind::Internet, crate::reachability::Bytes::default());
    let status = iroh_endpoint::status();
    assert_eq!(status["reachability"]["verdict"], "reachable", "a direct session from outside confirms it: {status}");
    assert_eq!(status["reachability"]["confirmed"], true);
}

#[tokio::test]
async fn the_default_port_falls_back_to_a_free_one_and_says_it_is_not_fixed() {
    let key = iroh::SecretKey::from_bytes(&[61; 32]);
    let loopback = IrohConfig { relay: "off".into(), listen: Some(Ipv4Addr::LOCALHOST.into()), portmapper: false, ..IrohConfig::default() };
    let _taken = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, iroh_endpoint::DEFAULT_PORT));
    let (endpoint, fixed) = iroh_endpoint::bind_preferring_port(&key, &loopback).await.expect("binds a free port");
    assert!(!fixed);
    assert_ne!(endpoint.bound_sockets()[0].port(), iroh_endpoint::DEFAULT_PORT);

    let held = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let chosen = IrohConfig { port: held.local_addr().unwrap().port(), ..loopback.clone() };
    assert!(iroh_endpoint::bind_preferring_port(&key, &chosen).await.is_none(), "a port set in service.toml is not swapped");
    drop(held);
    let (endpoint, fixed) = iroh_endpoint::bind_preferring_port(&key, &chosen).await.expect("binds the chosen port");
    assert!(fixed);
    assert_eq!(endpoint.bound_sockets()[0].port(), chosen.port);
}

#[tokio::test]
async fn a_proven_hello_is_remembered_with_its_endpoint_and_a_stranger_or_revoked_device_is_not() {
    let f = Fixture::new().await;
    let (client, _connection) = f.paired(&f.phone, 70).await;
    let hellos = f.service.iroh_hellos();
    let hello = &hellos["phone-1"];
    assert_eq!(hello.node, client.id().to_string());
    alondra_e2e::verify_iroh_hello(
        f.phone.signing.verifying_key(),
        &hello.signature,
        "mac-1",
        "phone-1",
        client.id().as_bytes(),
        f.service.iroh_key().public().as_bytes(),
    )
    .expect("the remembered signature is the device's own hello from that endpoint");

    let (stranger, connection) = f.connect(71).await;
    let forged = f.hello_signature(&SigningKey::from_slice(&[99u8; 32]).unwrap(), "phone-2", &stranger);
    assert_eq!(exchange(&connection, &json!({"device_id": "phone-2", "signature": forged})).await, json!({"error": "unauthorized"}));
    assert!(!f.service.iroh_hellos().contains_key("phone-2"));

    assert!(Auth::open(&f.home).unwrap().revoke("phone-1").unwrap());
    f.service.remember_revoked("phone-1");
    let _revoked = f.paired(&f.phone, 72).await;
    assert!(f.service.iroh_hellos().is_empty(), "a revoked device's endpoint is neither kept nor learned");
}

#[tokio::test]
async fn routes_tell_a_device_where_to_dial_the_endpoint() {
    let f = Fixture::new().await;
    f.service.set_iroh_dial(|dial| {
        dial.addresses = vec!["192.168.1.20:51000".into()];
        dial.relay = Some("https://iroh.example.com/".into());
    });
    let (_client, connection) = f.paired(&f.phone, 73).await;
    let envelope = f.phone.work(&f.service, "op-dial", "GET", crate::direct::routes::PATH, None);
    let answer = f.phone.opened(&exchange(&connection, &envelope).await["response"]);
    assert_eq!(answer["body"]["iroh_addrs"], json!(["192.168.1.20:51000"]), "{answer}");
    assert_eq!(answer["body"]["iroh_relay"], "https://iroh.example.com/");
}

#[tokio::test]
async fn the_mac_binds_its_devices_endpoints_at_the_relay_after_its_own_and_again_when_one_changes() {
    use axum::Json;
    use axum::extract::{Path as RelayPath, State as RelayState};
    use axum::routing::put;
    #[derive(Clone, Default)]
    struct Seen {
        devices: Arc<Mutex<Vec<(String, Value)>>>,
    }
    let seen = Seen::default();
    let router = Router::new()
        .route("/v1/bootstrap", get(|| async { Json(json!({"epoch": URL_SAFE_NO_PAD.encode([1u8; 16])})) }))
        .route("/v1/iroh-node", put(|| async { axum::http::StatusCode::NO_CONTENT }))
        .route("/v1/devices/{id}/iroh-node", put(|RelayState(seen): RelayState<Seen>, RelayPath(id): RelayPath<String>, Json(body): Json<Value>| async move {
            seen.devices.lock().unwrap().push((id, body));
            axum::http::StatusCode::NO_CONTENT
        }))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (directory, mut service, _) = push_fixture();
    service.set_relay_url(Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let bound = || seen.devices.lock().unwrap().clone();

    service.remember_iroh_hello("phone-1", "node-a", "signature-a");
    let epoch = service.bootstrap().await.unwrap();
    service.keep_device_iroh_nodes_bound(&epoch).await;
    assert!(bound().is_empty(), "nothing is bound for a device before this Mac's own endpoint");

    service.keep_iroh_endpoint_bound(&epoch).await;
    service.keep_device_iroh_nodes_bound(&epoch).await;
    service.keep_device_iroh_nodes_bound(&epoch).await;
    assert_eq!(bound(), [("phone-1".to_owned(), json!({"iroh_node_id": "node-a", "hello_signature": "signature-a"}))]);

    service.remember_iroh_hello("phone-1", "node-b", "signature-b");
    service.keep_device_iroh_nodes_bound(&epoch).await;
    assert_eq!(bound().len(), 2, "a new endpoint is bound again at once");
    assert_eq!(bound()[1].1["iroh_node_id"], "node-b");

    service.remember_iroh_hello("phone-9", "node-c", "signature-c");
    assert!(Auth::open(&directory).unwrap().revoke("phone-1").unwrap());
    let next = [2u8; 16];
    service.keep_iroh_endpoint_bound(&next).await;
    service.keep_device_iroh_nodes_bound(&next).await;
    assert_eq!(bound().len(), 2, "a revoked device, or one never paired here, is not bound");
    server.abort();
}

/// 15 Oct, iPhone on the home Wi-Fi: every read over iroh queued behind the one before it, so a
/// card took 4 to 17 s. Reads run side by side, as they do from the relay; writers keep their turn.
#[tokio::test]
async fn a_read_never_waits_for_a_writer_but_a_write_does() {
    let f = Fixture::new().await;
    let (_client, connection) = f.paired(&f.phone, 61).await;
    let writer = f.service.lane.clone().lock_owned().await;

    let read = f.phone.work(&f.service, "op-read", "GET", crate::direct::routes::PATH, None);
    let answered = tokio::time::timeout(Duration::from_secs(5), exchange(&connection, &read)).await;
    assert_eq!(f.phone.opened(&answered.expect("a read answers while a writer holds the lane")["response"])["status"], 200);

    let action = f.action(&f.phone, "op-limit", "4", "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10");
    let write = tokio::spawn({
        let connection = connection.clone();
        async move { exchange(&connection, &action).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!write.is_finished(), "a write waits for the writer before it");
    drop(writer);
    let written = tokio::time::timeout(Duration::from_secs(5), write).await.unwrap().unwrap();
    assert_eq!(f.phone.opened(&written["response"])["body"]["outcome"], "applied");
}
