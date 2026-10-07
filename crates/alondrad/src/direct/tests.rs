use super::*;
use crate::auth::Auth;
use alondra_e2e::Context;
use alondra_e2e::pairing::{self as protocol, Approval, Transcript};
use base64::engine::general_purpose::STANDARD;
use p256::SecretKey;
use p256::ecdsa::SigningKey;
use std::path::PathBuf;
use std::sync::Mutex;

fn wire_signing(key: &SigningKey) -> String {
    URL_SAFE_NO_PAD.encode(key.verifying_key().to_sec1_point(false).as_bytes())
}

fn wire_agreement(key: &SecretKey) -> String {
    URL_SAFE_NO_PAD.encode(key.public_key().to_sec1_point(false).as_bytes())
}

struct Phone {
    id: String,
    signing: SigningKey,
    agreement: SecretKey,
}

impl Phone {
    fn new(id: &str, seed: u8) -> Self {
        Self {
            id: id.into(),
            signing: SigningKey::from_slice(&[seed; 32]).unwrap(),
            agreement: SecretKey::from_slice(&[seed + 1; 32]).unwrap(),
        }
    }

    fn seal(&self, mac_id: &str, request_id: &str, kind: &str, expires_at_ms: u64, plain: &serde_json::Value) -> Envelope {
        Envelope::seal_random(
            Context { route: mac_id, sender: &self.id, recipient: mac_id, request_id, kind, expires_at_ms },
            &serde_json::to_vec(plain).unwrap(),
            &self.signing,
            &SecretKey::from_slice(&[2u8; 32]).unwrap().public_key(),
        )
        .unwrap()
    }

    fn request(&self, request_id: &str, path: &str) -> Envelope {
        self.seal("mac-1", request_id, "request", now_ms() + 60_000, &json!({"method": "GET", "path": path}))
    }

    fn open(&self, bytes: &[u8]) -> serde_json::Value {
        let envelope: Envelope = serde_json::from_slice(bytes).unwrap();
        let mac = SigningKey::from_slice(&[1u8; 32]).unwrap();
        serde_json::from_slice(
            &envelope.open(mac.verifying_key(), &self.agreement, "mac-1", &self.id, "mac-1", now_ms()).unwrap(),
        )
        .unwrap()
    }
}

struct Fixture {
    home: PathBuf,
    direct: Arc<Direct>,
    base: String,
    _server: tokio::task::JoinHandle<()>,
}

fn home_with(devices: &[&Phone], relay: Option<&str>) -> PathBuf {
    let home = std::env::temp_dir().join(format!("alondra-direct-{}", crate::auth::random_hex(8)));
    std::fs::create_dir_all(&home).unwrap();
    for (name, bytes) in [("signing.key", [1u8; 32]), ("agreement.key", [2u8; 32])] {
        let path = home.join(name);
        std::fs::write(&path, URL_SAFE_NO_PAD.encode(bytes)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let approved: Vec<_> = devices
        .iter()
        .map(|phone| json!({"id": phone.id, "name": "iPhone", "created_at": 0, "last_seen": null,
            "public_key": STANDARD.encode(phone.signing.verifying_key().to_sec1_bytes())}))
        .collect();
    std::fs::write(home.join("devices.json"), serde_json::to_vec(&approved).unwrap()).unwrap();
    let mut config = String::new();
    if let Some(relay) = relay {
        config.push_str(&format!("relay_url = \"{relay}\"\nowner_sub = \"owner\"\n"));
    }
    config.push_str("mac_id = \"mac-1\"\nsigning_key_path = \"signing.key\"\nagreement_key_path = \"agreement.key\"\n");
    if devices.is_empty() {
        config.push_str("devices = []\n");
    }
    for phone in devices {
        config.push_str(&format!(
            "[[devices]]\ndevice_id = \"{}\"\nsigning_key = \"{}\"\nagreement_key = \"{}\"\n",
            phone.id,
            wire_signing(&phone.signing),
            wire_agreement(&phone.agreement)
        ));
    }
    std::fs::write(home.join("service.toml"), config).unwrap();
    home
}

async fn serve(home: &std::path::Path, service: Arc<Service>, auth: Arc<Auth>) -> Fixture {
    let direct = Arc::new(Direct::new(
        service.clone(),
        Arc::new(PairingBook::new(home)),
        "Mac de prueba".into(),
        crate::direct_approval(home.to_path_buf(), auth, service),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(direct.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fixture { home: home.to_path_buf(), direct, base, _server: server }
}

async fn fixture(devices: &[&Phone]) -> Fixture {
    let home = home_with(devices, None);
    let auth = Arc::new(Auth::open(&home).unwrap());
    let service = Service::open(&home, 0, auth.clone()).unwrap().unwrap();
    serve(&home, Arc::new(service), auth).await
}

async fn send(base: &str, path: &str, body: &impl serde::Serialize) -> (u16, Vec<u8>) {
    let response = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(body).unwrap())
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.bytes().await.unwrap().to_vec())
}

async fn get(base: &str, path: &str) -> (u16, serde_json::Value) {
    let response = reqwest::get(format!("{base}{path}")).await.unwrap();
    (response.status().as_u16(), response.json().await.unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
async fn a_device_paired_directly_is_listed_as_direct_and_this_macs_clients_as_mac() {
    let direct = Phone::new("phone-a", 40);
    let relay = Phone::new("phone-b", 50);
    let f = fixture(&[&direct, &relay]).await;
    remember_direct(&f.home, "phone-a").unwrap();
    remember_direct(&f.home, "phone-a").unwrap();
    assert_eq!(f.direct.service.device_origin("phone-a"), "direct");
    assert_eq!(f.direct.service.device_origin("phone-b"), "relay");
    assert_eq!(f.direct.service.device_origin("mac-app"), "mac");
}

#[tokio::test]
async fn hello_names_the_mac_and_its_public_keys() {
    let f = fixture(&[]).await;
    let (status, hello) = get(&f.base, "/direct/hello").await;
    assert_eq!(status, 200);
    assert_eq!(hello["mac_id"], "mac-1");
    assert_eq!(hello["name"], "Mac de prueba");
    assert_eq!(hello["signing_key"], wire_signing(&SigningKey::from_slice(&[1u8; 32]).unwrap()));
    assert_eq!(hello["agreement_key"], wire_agreement(&SecretKey::from_slice(&[2u8; 32]).unwrap()));
}

#[tokio::test]
async fn an_approved_device_without_relay_enrollment_is_served_and_a_repeat_gets_the_same_bytes() {
    let phone = Phone::new("client-a", 30);
    let f = fixture(&[&phone]).await;
    let envelope = phone.request("op-1", routes::PATH);
    let (status, first) = send(&f.base, "/direct/request", &envelope).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&first));
    let answer = phone.open(&first);
    assert_eq!(answer["status"], 200);
    assert_eq!(answer["body"]["port"], 47810);
    let (status, again) = send(&f.base, "/direct/request", &envelope).await;
    assert_eq!((status, &again), (200, &first), "a retry replays the stored response");

    let other = phone.request("op-1", "/routes?again");
    let (status, _) = send(&f.base, "/direct/request", &other).await;
    assert_eq!(status, 409, "the same request ID with other bytes is a conflict");
}

#[tokio::test]
async fn unknown_revoked_expired_and_misdirected_envelopes_are_refused() {
    let phone = Phone::new("client-a", 30);
    let f = fixture(&[&phone]).await;
    let stranger = Phone::new("client-b", 40);
    assert_eq!(send(&f.base, "/direct/request", &stranger.request("op-1", routes::PATH)).await.0, 401);

    let expired = phone.seal("mac-1", "op-2", "request", now_ms() - 1, &json!({"method": "GET", "path": routes::PATH}));
    assert_eq!(send(&f.base, "/direct/request", &expired).await.0, 401);

    let elsewhere = phone.seal("mac-2", "op-3", "request", now_ms() + 60_000, &json!({"method": "GET", "path": routes::PATH}));
    assert_eq!(send(&f.base, "/direct/request", &elsewhere).await.0, 421);

    let mut forged = phone.request("op-4", routes::PATH);
    forged.signature = stranger.request("op-4", routes::PATH).signature;
    assert_eq!(send(&f.base, "/direct/request", &forged).await.0, 401);

    assert_eq!(send(&f.base, "/direct/request", &phone.request("op-5", routes::PATH)).await.0, 200);
    assert!(Auth::open(&f.home).unwrap().revoke("client-a").unwrap());
    assert_eq!(
        send(&f.base, "/direct/request", &phone.request("op-6", routes::PATH)).await.0,
        401,
        "a revocation on this Mac cuts the direct path at once"
    );
}

#[tokio::test]
async fn the_loopback_api_is_not_reachable_on_the_direct_port() {
    let f = fixture(&[]).await;
    for path in ["/tasks", "/devices", "/devices/pending", "/daemon", "/pairing"] {
        assert_eq!(get(&f.base, path).await.0, 404, "{path}");
    }
    assert_eq!(send(&f.base, "/devices/pending/x/approve", &json!({})).await.0, 404);
    assert_eq!(send(&f.base, "/pair", &json!({})).await.0, 404);
}

#[tokio::test]
async fn browser_requests_and_non_json_bodies_are_refused() {
    let phone = Phone::new("client-a", 30);
    let f = fixture(&[&phone]).await;
    let client = reqwest::Client::new();
    let with_origin = client
        .post(format!("{}/direct/request", f.base))
        .header("content-type", "application/json")
        .header("origin", "https://evil.example")
        .body(serde_json::to_vec(&phone.request("op-1", routes::PATH)).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(with_origin.status().as_u16(), 403);
    let text_plain = client
        .post(format!("{}/direct/pairing", f.base))
        .header("content-type", "text/plain")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(text_plain.status().as_u16(), 415);
    let oversized = client
        .post(format!("{}/direct/request", f.base))
        .header("content-type", "application/json")
        .body(vec![b' '; MAX_REQUEST + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status().as_u16(), 413);
}

#[test]
fn only_private_link_local_loopback_and_tailnet_peers_are_allowed() {
    for allowed in ["127.0.0.1", "10.1.2.3", "172.16.0.9", "192.168.1.20", "169.254.3.4", "100.64.0.1", "100.127.255.254", "::1", "fd7a:115c:a1e0::1", "fe80::1", "::ffff:192.168.1.2"] {
        assert!(allowed_peer(allowed.parse().unwrap()), "{allowed}");
    }
    for refused in ["8.8.8.8", "100.128.0.1", "100.63.255.255", "2001:db8::1", "::ffff:8.8.8.8", "172.32.0.1"] {
        assert!(!allowed_peer(refused.parse().unwrap()), "{refused}");
    }
}

#[tokio::test]
async fn the_same_envelope_through_the_relay_and_directly_gets_the_same_answer() {
    let phone = Phone::new("client-a", 30);
    let responses = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let saved = responses.clone();
    let relay = Router::new()
        .route("/v1/work/{id}/receipt", post(|| async { Json(json!({"status": "received"})) }))
        .route(
            "/v1/work/{id}/response",
            post(move |body: Bytes| {
                let saved = saved.clone();
                async move {
                    saved.lock().unwrap().push(body.to_vec());
                    Json(json!({"status": "completed"}))
                }
            }),
        );
    let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("http://{}/", relay_listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(relay_listener, relay).await.unwrap() });
    let home = home_with(&[&phone], Some(&relay_url));
    let auth = Arc::new(Auth::open(&home).unwrap());
    let service = Service::open(&home, 0, auth.clone()).unwrap().unwrap();
    service.enroll_with_relay_for_tests("client-a");
    let service = Arc::new(service);
    let f = serve(&home, service.clone(), auth).await;

    let envelope = phone.request("op-both", routes::PATH);
    service.handle_relay_for_tests(envelope.clone(), &[9u8; 16]).await.unwrap();
    let through_relay = responses.lock().unwrap().pop().expect("relay delivered a response");
    let (status, direct_bytes) = send(&f.base, "/direct/request", &envelope).await;
    assert_eq!(status, 200);
    assert_eq!(direct_bytes, through_relay, "one journal answers both transports with the same bytes");
    assert_eq!(phone.open(&direct_bytes)["status"], 200);
}

#[tokio::test]
async fn a_direct_only_device_cannot_use_the_relay() {
    let phone = Phone::new("client-a", 30);
    let home = home_with(&[&phone], Some("http://127.0.0.1:1/"));
    let service = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap())).unwrap().unwrap();
    let error = service.handle_relay_for_tests(phone.request("op-1", routes::PATH), &[9u8; 16]).await.unwrap_err();
    assert!(format!("{error:#}").contains("enrolled with the relay"), "{error:#}");
}

struct Pairing<'a> {
    phone: &'a Phone,
    nonce: String,
    signing_key: String,
    agreement_key: String,
}

impl<'a> Pairing<'a> {
    fn new(phone: &'a Phone) -> Self {
        Self {
            phone,
            nonce: protocol::random_nonce().unwrap(),
            signing_key: wire_signing(&phone.signing),
            agreement_key: wire_agreement(&phone.agreement),
        }
    }

    fn start(&self) -> serde_json::Value {
        let commitment = protocol::commitment(&self.nonce, &self.phone.id, &self.signing_key, &self.agreement_key).unwrap();
        let envelope = self.phone.seal(
            "mac-1",
            "pair-start",
            "pairing",
            now_ms() + 60_000,
            &json!({"device_id": self.phone.id, "name": "iPhone de prueba", "signing_key": self.signing_key,
                "agreement_key": self.agreement_key, "commitment": commitment}),
        );
        json!({"signing_key": self.signing_key, "envelope": envelope})
    }

    fn reveal(&self, pairing_id: &str) -> serde_json::Value {
        let envelope = self.phone.seal(
            "mac-1",
            "pair-reveal",
            "pairing",
            now_ms() + 60_000,
            &json!({"pairing_id": pairing_id, "client_nonce": self.nonce}),
        );
        json!({"envelope": envelope})
    }

    fn transcript<'b>(&'b self, hello: &'b serde_json::Value, pairing_id: &'b str, mac_nonce: &'b str) -> Transcript<'b> {
        Transcript {
            pairing_id,
            mac_id: hello["mac_id"].as_str().unwrap(),
            mac_signing_key: hello["signing_key"].as_str().unwrap(),
            mac_agreement_key: hello["agreement_key"].as_str().unwrap(),
            device_id: &self.phone.id,
            client_signing_key: &self.signing_key,
            client_agreement_key: &self.agreement_key,
            client_nonce: &self.nonce,
            mac_nonce,
        }
    }
}

#[tokio::test]
async fn pairing_over_the_network_ends_with_a_device_the_mac_serves() {
    let f = fixture(&[]).await;
    let phone = Phone::new("client-new", 50);
    let pairing = Pairing::new(&phone);
    let (_, hello) = get(&f.base, "/direct/hello").await;

    let (status, started) = send(&f.base, "/direct/pairing", &pairing.start()).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&started));
    let started: serde_json::Value = serde_json::from_slice(&started).unwrap();
    let pairing_id = started["pairing_id"].as_str().unwrap();
    let mac_nonce = started["mac_nonce"].as_str().unwrap();
    assert_eq!(get(&f.base, &format!("/direct/pairing/{pairing_id}")).await.1["state"], "committed");

    let (status, _) = send(&f.base, &format!("/direct/pairing/{pairing_id}/reveal"), &pairing.reveal(pairing_id)).await;
    assert_eq!(status, 200);
    let pending = f.direct.pairings().pending(now_ms());
    let code = pairing.transcript(&hello, pairing_id, mac_nonce).code().unwrap();
    assert_eq!(pending[0].code, code, "the Mac shows the code the phone computed");
    assert_eq!(pending[0].name, "iPhone de prueba");

    assert_eq!(send(&f.base, "/direct/request", &phone.request("op-1", routes::PATH)).await.0, 401);
    f.direct.approve(pairing_id).unwrap();

    let (_, status) = get(&f.base, &format!("/direct/pairing/{pairing_id}")).await;
    assert_eq!(status["state"], "approved");
    let approval: Approval = serde_json::from_value(status["approval"].clone()).unwrap();
    let mac_key = p256::ecdsa::VerifyingKey::from_sec1_bytes(
        &URL_SAFE_NO_PAD.decode(hello["signing_key"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    approval
        .verify(status["signature"].as_str().unwrap(), &mac_key, &pairing.transcript(&hello, pairing_id, mac_nonce), now_ms())
        .unwrap();
    let config: toml::Table = toml::from_str(&std::fs::read_to_string(f.home.join("service.toml")).unwrap()).unwrap();
    assert_eq!(config["devices"][0]["device_id"].as_str(), Some("client-new"));
    let (status, bytes) = send(&f.base, "/direct/request", &phone.request("op-2", routes::PATH)).await;
    assert_eq!(status, 200, "the approved device is served at once");
    assert_eq!(phone.open(&bytes)["status"], 200);
}

#[tokio::test]
async fn a_denied_device_cannot_ask_again_over_the_network() {
    let f = fixture(&[]).await;
    let phone = Phone::new("client-new", 50);
    let pairing = Pairing::new(&phone);
    let (_, started) = send(&f.base, "/direct/pairing", &pairing.start()).await;
    let started: serde_json::Value = serde_json::from_slice(&started).unwrap();
    let pairing_id = started["pairing_id"].as_str().unwrap();
    send(&f.base, &format!("/direct/pairing/{pairing_id}/reveal"), &pairing.reveal(pairing_id)).await;
    f.direct.pairings().deny(pairing_id, now_ms()).unwrap();
    assert_eq!(get(&f.base, &format!("/direct/pairing/{pairing_id}")).await.1["state"], "denied");
    let retry = Pairing { nonce: protocol::random_nonce().unwrap(), ..Pairing::new(&phone) };
    assert_eq!(send(&f.base, "/direct/pairing", &retry.start()).await.0, 403);
}
