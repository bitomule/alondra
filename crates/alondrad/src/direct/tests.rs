use super::*;
use crate::auth::Auth;
use alondra_e2e::Context;
use alondra_e2e::pairing::{self as protocol, Approval, Transcript};
use base64::engine::general_purpose::STANDARD;
use p256::SecretKey;
use p256::ecdsa::SigningKey;
use std::path::PathBuf;

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
    _directory: crate::test_support::Directory,
}

impl Drop for Fixture {
    fn drop(&mut self) { self._server.abort(); }
}

fn home_with(devices: &[&Phone], relay: Option<&str>) -> crate::test_support::Directory {
        let home = crate::test_support::Directory::new();
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

async fn serve(home: crate::test_support::Directory, service: Arc<Service>, auth: Arc<Auth>) -> Fixture {
    let direct = Arc::new(Direct::new(
        service.clone(),
        Arc::new(PairingBook::new(&home)),
        "Mac de prueba".into(),
        crate::direct_approval(home.to_path_buf(), auth, service),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(direct.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fixture { home: home.to_path_buf(), direct, base, _server: server, _directory: home }
}

async fn fixture(devices: &[&Phone]) -> Fixture {
    let home = home_with(devices, None);
    let auth = Arc::new(Auth::open(&home).unwrap());
    let service = Service::open(&home, 0, auth.clone()).unwrap().unwrap();
    serve(home, Arc::new(service), auth).await
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
    let (status, body) = send(&f.base, "/direct/request", &expired).await;
    assert_eq!(status, 200, "an expired envelope is refused in an answer only its device opens");
    assert_eq!(phone.open(&body)["body"]["error"], "envelope_expired");

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
async fn an_expired_envelope_tells_its_own_device_the_macs_clock_and_nobody_else() {
    let phone = Phone::new("client-a", 30);
    let stranger = Phone::new("client-b", 40);
    let f = fixture(&[&phone]).await;
    let read = json!({"method": "GET", "path": routes::PATH});
    let expired_at = now_ms() - 10_000;
    let before = now_ms();
    let (status, body) = send(&f.base, "/direct/request", &phone.seal("mac-1", "op-1", "request", expired_at, &read)).await;
    let after = now_ms();
    assert_eq!(status, 200);
    let answer = phone.open(&body);
    assert_eq!((answer["status"].as_u64(), answer["error"].as_str()), (Some(503), Some("envelope_expired")), "{answer}");
    let mac_now = answer["body"]["mac_now_ms"].as_u64().unwrap();
    assert!((before..=after).contains(&mac_now), "the answer carries the Mac's clock: {answer}");
    assert_eq!(answer["body"]["late_ms"].as_u64(), Some(mac_now - expired_at));
    assert_eq!(answer["body"]["kind"], "in_progress", "older terminals seal it again instead of dropping it");

    let mut forged = phone.seal("mac-1", "op-2", "request", expired_at, &read);
    forged.signature = stranger.seal("mac-1", "op-2", "request", expired_at, &read).signature;
    assert_eq!(send(&f.base, "/direct/request", &forged).await.0, 401, "a late envelope someone else signed learns nothing");

    assert_eq!(
        send(&f.base, "/direct/request", &phone.request("op-1", routes::PATH)).await.0,
        200,
        "the same request sealed again in time is served: the late copy left nothing behind"
    );
}

#[tokio::test]
async fn approvals_the_mac_cannot_read_refuse_the_request_as_unavailable_not_unauthorized() {
    let phone = Phone::new("client-a", 30);
    let f = fixture(&[&phone]).await;
    let approvals = std::fs::read(f.home.join("devices.json")).unwrap();
    std::fs::write(f.home.join("devices.json"), b"[{\"id\":").unwrap();
    let (status, body) = send(&f.base, "/direct/request", &phone.request("op-1", routes::PATH)).await;
    assert_eq!(status, 503, "{}", String::from_utf8_lossy(&body));
    std::fs::write(f.home.join("devices.json"), approvals).unwrap();
    assert_eq!(send(&f.base, "/direct/request", &phone.request("op-2", routes::PATH)).await.0, 200);
}

#[tokio::test]
async fn a_revoked_device_is_told_so_in_a_sealed_answer_only_when_it_proves_who_it_is() {
    let phone = Phone::new("client-a", 30);
    let bystander = Phone::new("client-b", 40);
    let f = fixture(&[&phone, &bystander]).await;
    assert_eq!(send(&f.base, "/direct/request", &phone.request("op-1", routes::PATH)).await.0, 200);

    let auth = Auth::open(&f.home).unwrap();
    assert!(auth.revoke("client-a").unwrap());
    assert!(auth.revoke("client-b").unwrap());
    f.direct.service.remember_revoked("client-a");

    let (status, body) = send(&f.base, "/direct/request", &phone.request("op-2", routes::PATH)).await;
    assert_eq!(status, 200, "the revoked device gets an answer it can open");
    let answer = phone.open(&body);
    assert_eq!((answer["status"].as_u64(), answer["error"].as_str()), (Some(403), Some("device_revoked")));

    let mut forged = phone.request("op-3", routes::PATH);
    forged.signature = bystander.request("op-3", routes::PATH).signature;
    assert_eq!(send(&f.base, "/direct/request", &forged).await.0, 401, "someone else's signature proves nothing");

    assert_eq!(
        send(&f.base, "/direct/request", &bystander.request("op-4", routes::PATH)).await.0,
        401,
        "a device nobody remembered gets the plain refusal"
    );
}

#[tokio::test]
async fn the_remembered_revoked_devices_survive_a_restart_and_stay_bounded() {
    let phone = Phone::new("client-a", 30);
    let home = home_with(&[&phone], None);
    let service = Service::open(&home, 0, Arc::new(Auth::open(&home).unwrap())).unwrap().unwrap();
    service.remember_revoked("client-a");
    let again = crate::revoked_devices::RevokedDevices::open(&home);
    assert_eq!(again.len(), 1);
    let many: Vec<_> = (0..100)
        .map(|n| {
            let key = Phone::new(&format!("client-{n}"), 30);
            crate::revoked_devices::Tombstone::new(&key.id, key.signing.verifying_key(), &key.agreement.public_key(), n)
        })
        .collect();
    again.add(many).unwrap();
    assert_eq!(again.len(), 64);
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

#[tokio::test]
async fn a_signed_request_records_when_the_device_was_last_used() {
    let first = Phone::new("client-a", 30);
    let second = Phone::new("client-b", 40);
    let home = home_with(&[&first, &second], None);
    let auth = Arc::new(Auth::open(&home).unwrap());
    let service = Arc::new(Service::open(&home, 0, auth.clone()).unwrap().unwrap());
    let f = serve(home, service, auth.clone()).await;
    let before = alondra_core::now_millis();
    let used = |id: &str| auth.usage().snapshot(0).devices.get(id).and_then(|used| used.last_used_ms);
    assert_eq!((used("client-a"), used("client-b")), (None, None));

    assert_eq!(send(&f.base, "/direct/request", &first.request("op-first", routes::PATH)).await.0, 200);
    assert!(used("client-a").is_some_and(|at| at >= before));
    assert_eq!(used("client-b"), None, "only the device that asked");
    assert_eq!(send(&f.base, "/direct/request", &second.request("op-second", routes::PATH)).await.0, 200);
    assert!(used("client-b").is_some_and(|at| at >= before));

    auth.usage().flush(alondra_core::now_millis()).unwrap();
    let reopened = crate::device_usage::DeviceUsage::at(&f.home).snapshot(0);
    assert!(reopened.devices["client-b"].last_used_ms.is_some(), "the use outlives a restart");
}

#[tokio::test]
async fn an_unused_device_is_unpaired_on_its_own_and_told_so_while_the_one_in_use_stays() {
    let unused = Phone::new("client-a", 30);
    let in_use = Phone::new("client-b", 40);
    let home = home_with(&[&unused, &in_use], None);
    std::fs::write(home.join("device-usage.json"), r#"{"tracking_since_ms": 0}"#).unwrap();
    drop(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
    let daemon = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
    let auth = daemon.auth();
    let service = Arc::new(Service::open(&home, 0, auth.clone()).unwrap().unwrap());
    daemon.attach_service(service.clone());
    let f = serve(home, service, auth.clone()).await;
    assert_eq!(send(&f.base, "/direct/request", &in_use.request("op-1", routes::PATH)).await.0, 200);

    let now = alondra_core::now_millis();
    let gone = crate::device_expiry::sweep(&daemon, &f.home, now, 30.0);
    assert_eq!(gone.iter().map(|expired| expired.id.as_str()).collect::<Vec<_>>(), ["client-a"]);
    assert_eq!(gone[0].reason, crate::device_usage::ExpiryReason::Unused);
    let paired: Vec<_> = Auth::open(&f.home).unwrap().list().into_iter().map(|device| device.id).collect();
    assert_eq!(paired, ["client-b"], "the device in use stays paired");
    assert_eq!(auth.usage().snapshot(now).expired, gone, "the list remembers which went and why");

    let (status, body) = send(&f.base, "/direct/request", &unused.request("op-2", routes::PATH)).await;
    assert_eq!(status, 200, "the unpaired device gets an answer it can open");
    let answer = unused.open(&body);
    assert_eq!((answer["status"].as_u64(), answer["error"].as_str()), (Some(403), Some("device_revoked")));
    assert_eq!(send(&f.base, "/direct/request", &in_use.request("op-3", routes::PATH)).await.0, 200);
}

/// A relay that is `DOWN`, an `OLD` one whose bootstrap knows nothing of removals, or a `CURRENT`
/// one whose bootstrap, when the Mac signs it, lists what the account removed from this Mac.
async fn account_relay(mode: Arc<std::sync::atomic::AtomicU8>, removals: serde_json::Value) -> (String, tokio::task::JoinHandle<()>) {
    let relay = Router::new().fallback(move |request: Request<Body>| {
        let (mode, removals) = (mode.clone(), removals.clone());
        async move {
            match (mode.load(std::sync::atomic::Ordering::SeqCst), request.uri().path()) {
                (DOWN, _) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                (relay, "/v1/bootstrap") => {
                    let mut boot = json!({"epoch": URL_SAFE_NO_PAD.encode([7u8; 16])});
                    if relay == CURRENT && request.headers().contains_key("x-device-signature") {
                        boot["removals"] = removals;
                    }
                    Json(boot).into_response()
                }
                _ => StatusCode::OK.into_response(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    (url, tokio::spawn(async move { axum::serve(listener, relay).await.unwrap() }))
}

const DOWN: u8 = 0;
const OLD: u8 = 1;
const CURRENT: u8 = 2;

#[tokio::test]
async fn a_client_the_account_removed_is_unpaired_once_the_mac_reaches_the_relay_and_the_others_stay() {
    let removed = Phone::new("client-a", 50);
    let kept = Phone::new("client-b", 60);
    let paired_again = Phone::new("client-c", 70);
    let now = alondra_core::now_millis();
    let mode = Arc::new(std::sync::atomic::AtomicU8::new(DOWN));
    let (relay, relay_task) = account_relay(mode.clone(), json!({"mac_removed": false, "clients": [
        {"client_id": "client-a", "removed_at_ms": now},
        {"client_id": "client-c", "removed_at_ms": now - 60_000}]})).await;
    let home = home_with(&[&removed, &kept, &paired_again], Some(&relay));
    let mut approved: serde_json::Value = serde_json::from_slice(&std::fs::read(home.join("devices.json")).unwrap()).unwrap();
    approved[2]["created_at"] = json!(now - 1_000);
    std::fs::write(home.join("devices.json"), serde_json::to_vec(&approved).unwrap()).unwrap();
    let auth = Arc::new(Auth::open(&home).unwrap());
    let service = Arc::new(Service::open(&home, 0, auth.clone()).unwrap().unwrap());
    let f = serve(home, service.clone(), auth.clone()).await;
    let (mut epoch, mut revoked) = (None, std::collections::HashSet::new());
    let paired = || Auth::open(&f.home).unwrap().list().into_iter().map(|device| device.id).collect::<Vec<_>>();

    mode.store(OLD, std::sync::atomic::Ordering::SeqCst);
    for _ in 0..2 {
        service.keep_registered(&mut epoch, &mut revoked).await.unwrap();
    }
    assert_eq!(paired(), ["client-a", "client-b", "client-c"], "a relay that sends no removals changes nothing");
    assert_eq!(send(&f.base, "/direct/request", &removed.request("op-1", routes::PATH)).await.0, 200);

    mode.store(DOWN, std::sync::atomic::Ordering::SeqCst);
    assert!(service.keep_registered(&mut epoch, &mut revoked).await.is_err(), "the Mac cannot reach the relay");
    assert_eq!(paired(), ["client-a", "client-b", "client-c"]);
    mode.store(CURRENT, std::sync::atomic::Ordering::SeqCst);
    service.keep_registered(&mut epoch, &mut revoked).await.unwrap();
    assert_eq!(paired(), ["client-b", "client-c"],
        "back online, the first bootstrap brings the removal: the removed client goes; the one kept and the one paired again after its removal stay");
    let gone = auth.usage().snapshot(now).expired;
    assert_eq!(gone.iter().map(|expired| (expired.id.as_str(), expired.reason)).collect::<Vec<_>>(),
        [("client-a", crate::device_usage::ExpiryReason::AccountRemoved)]);

    let (status, body) = send(&f.base, "/direct/request", &removed.request("op-2", routes::PATH)).await;
    assert_eq!(status, 200, "the removed client gets an answer it can open");
    let answer = removed.open(&body);
    assert_eq!((answer["status"].as_u64(), answer["error"].as_str()), (Some(403), Some("device_revoked")));
    assert_eq!(send(&f.base, "/direct/request", &kept.request("op-3", routes::PATH)).await.0, 200);
    assert_eq!(send(&f.base, "/direct/request", &paired_again.request("op-4", routes::PATH)).await.0, 200);

    service.keep_registered(&mut epoch, &mut revoked).await.unwrap();
    assert_eq!(auth.usage().snapshot(now).expired.len(), 1, "reading the same list again changes nothing");
    relay_task.abort();
}
